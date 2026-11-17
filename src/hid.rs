//! Accès HID minimal via hid.dll / setupapi (sans dépendance externe).
use std::{io, mem, ptr};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::HumanInterfaceDevice::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

#[derive(Debug, Clone)]
pub struct DevInfo {
    pub path: Vec<u16>,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
    pub feat_len: u16,
}

impl DevInfo {
    pub fn path_string(&self) -> String {
        String::from_utf16_lossy(&self.path[..self.path.len().saturating_sub(1)])
    }
}

fn open_raw(path: &[u16], access: u32, overlapped: bool) -> io::Result<HANDLE> {
    let flags = if overlapped { FILE_FLAG_OVERLAPPED } else { 0 };
    let h = unsafe {
        CreateFileW(path.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, ptr::null(), OPEN_EXISTING, flags, ptr::null_mut())
    };
    if h == INVALID_HANDLE_VALUE { Err(io::Error::last_os_error()) } else { Ok(h) }
}

/// Liste les collections HID présentes pour un vendor id donné.
pub fn enumerate(vid: u16) -> Vec<DevInfo> {
    let mut out = Vec::new();
    unsafe {
        let mut guid: GUID = mem::zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(&guid, ptr::null(), ptr::null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set as isize == INVALID_HANDLE_VALUE as isize { return out; }
        let mut i = 0;
        loop {
            let mut data: SP_DEVICE_INTERFACE_DATA = mem::zeroed();
            data.cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, ptr::null(), &guid, i, &mut data) == 0 { break; }
            i += 1;
            let mut req = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &data, ptr::null_mut(), 0, &mut req, ptr::null_mut());
            if req < 8 { continue; }
            let mut buf = vec![0u64; (req as usize + 7) / 8];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &data, detail, req, ptr::null_mut(), ptr::null_mut()) == 0 { continue; }
            let p = ptr::addr_of!((*detail).DevicePath) as *const u16;
            let mut len = 0;
            while *p.add(len) != 0 { len += 1; }
            let path: Vec<u16> = std::slice::from_raw_parts(p, len + 1).to_vec();
            if let Some(info) = query(&path) {
                if info.vid == vid { out.push(info); }
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

fn query(path: &[u16]) -> Option<DevInfo> {
    let h = open_raw(path, 0, false).ok()?;
    unsafe {
        let mut attr: HIDD_ATTRIBUTES = mem::zeroed();
        attr.Size = mem::size_of::<HIDD_ATTRIBUTES>() as u32;
        let ok = HidD_GetAttributes(h, &mut attr) != 0;
        let mut pp: PHIDP_PREPARSED_DATA = mem::zeroed();
        let mut caps: HIDP_CAPS = mem::zeroed();
        let ok = ok && HidD_GetPreparsedData(h, &mut pp) != 0;
        if ok {
            HidP_GetCaps(pp, &mut caps);
            HidD_FreePreparsedData(pp);
        }
        CloseHandle(h);
        ok.then(|| DevInfo {
            path: path.to_vec(),
            vid: attr.VendorID,
            pid: attr.ProductID,
            usage_page: caps.UsagePage,
            usage: caps.Usage,
            in_len: caps.InputReportByteLength,
            out_len: caps.OutputReportByteLength,
            feat_len: caps.FeatureReportByteLength,
        })
    }
}

/// Handle HID ouvert en lecture/écriture asynchrone (lectures avec timeout).
pub struct Device {
    h: HANDLE,
    pub info: DevInfo,
}

// Chaque E/S utilise son propre OVERLAPPED/événement : le handle peut être partagé.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    pub fn open(info: &DevInfo) -> io::Result<Self> {
        let h = open_raw(&info.path, GENERIC_READ | GENERIC_WRITE, true)?;
        Ok(Self { h, info: info.clone() })
    }

    fn overlapped_io(&self, write: bool, buf: *mut u8, len: usize, timeout_ms: u32) -> io::Result<usize> {
        let ev = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if ev.is_null() { return Err(io::Error::last_os_error()); }
        let r = self.overlapped_io_ev(ev, write, buf, len, timeout_ms);
        unsafe { CloseHandle(ev) };
        r
    }

    fn overlapped_io_ev(&self, ev: HANDLE, write: bool, buf: *mut u8, len: usize, timeout_ms: u32) -> io::Result<usize> {
        unsafe {
            let mut ov: OVERLAPPED = mem::zeroed();
            ov.hEvent = ev;
            let mut n = 0u32;
            let ok = if write {
                WriteFile(self.h, buf, len as u32, ptr::null_mut(), &mut ov)
            } else {
                ReadFile(self.h, buf, len as u32, ptr::null_mut(), &mut ov)
            };
            if ok == 0 {
                let e = GetLastError();
                if e != ERROR_IO_PENDING { return Err(io::Error::from_raw_os_error(e as i32)); }
                if WaitForSingleObject(ev, timeout_ms) == WAIT_TIMEOUT {
                    CancelIoEx(self.h, &ov);
                    GetOverlappedResult(self.h, &ov, &mut n, 1);
                    return Ok(0);
                }
            }
            if GetOverlappedResult(self.h, &ov, &mut n, 1) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n as usize)
        }
    }

    /// Lit un rapport d'entrée. Retourne Ok(0) si rien n'arrive avant `timeout_ms`.
    /// `buf[0]` contient l'id de rapport.
    pub fn read(&self, buf: &mut [u8], timeout_ms: u32) -> io::Result<usize> {
        let need = self.info.in_len as usize;
        if buf.len() < need {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "buffer trop petit"));
        }
        self.overlapped_io(false, buf.as_mut_ptr(), need, timeout_ms)
    }

    /// Envoie un rapport de sortie (complété par des zéros jusqu'à la taille attendue).
    pub fn write(&self, data: &[u8]) -> io::Result<usize> {
        let mut v = data.to_vec();
        v.resize((self.info.out_len as usize).max(data.len()), 0);
        self.overlapped_io(true, v.as_mut_ptr(), v.len(), 1000)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            CancelIoEx(self.h, ptr::null());
            CloseHandle(self.h);
        }
    }
}
