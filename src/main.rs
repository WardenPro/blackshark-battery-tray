//! Icône de barre des tâches affichant la batterie d'un Razer BlackShark V3.
#![windows_subsystem = "windows"]

use blackshark_battery::{config::Config, hid, protocol};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::{mem, ptr};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_STATUS: u32 = WM_APP + 2;
const ID_REFRESH: usize = 1;
const ID_QUIT: usize = 2;
const ID_RESYNC: usize = 3;
const TIMER_ID: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Status {
    /// Dongle absent (débranché ou PID non reconnu).
    NoDongle,
    /// Dongle présent, aucune mesure reçue depuis (casque éteint / hors de portée ?).
    Waiting,
    /// Dernier niveau reçu et son horodatage.
    Level(u8, Instant),
    /// Le dongle existe mais ne peut pas être ouvert.
    Error(u32),
}

static STATUS: Mutex<Status> = Mutex::new(Status::NoDongle);
static CONFIG: OnceLock<Config> = OnceLock::new();
static HWND_MAIN: AtomicUsize = AtomicUsize::new(0);
static REFRESH: AtomicBool = AtomicBool::new(false);
static RESYNC: AtomicBool = AtomicBool::new(false);
static LOW_WARNED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

fn cfg() -> &'static Config {
    CONFIG.get().unwrap()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let v: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..v.len()].copy_from_slice(&v);
    dst[v.len()] = 0;
}

/// Dossier de travail : cache du niveau et journal.
fn data_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("BlackSharkBattery");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Dernier niveau connu, conservé entre deux lancements (le casque ne notifie qu'aux changements).
fn cache_path() -> Option<std::path::PathBuf> {
    Some(data_dir()?.join("last_level.txt"))
}

/// Journal minimal (`app.log`, à côté du cache) : démarrages, arrêts, pertes du dongle.
/// Sans lui, une fermeture inexpliquée ne laisse aucune trace.
fn log(msg: &str) {
    let Some(dir) = data_dir() else { return };
    let path = dir.join("app.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 200_000) {
        let _ = std::fs::remove_file(&path);
    }
    let mut st: SYSTEMTIME = unsafe { mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} [{}] {msg}\n",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, std::process::id()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn save_level(p: u8) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Some(path) = cache_path() { let _ = std::fs::write(path, format!("{p} {now}")); }
}

fn load_level() -> Option<Status> {
    let text = std::fs::read_to_string(cache_path()?).ok()?;
    let mut it = text.split_whitespace();
    let p: u8 = it.next()?.parse().ok().filter(|p| *p <= 100)?;
    let ts: u64 = it.next()?.parse().ok()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let at = Instant::now().checked_sub(Duration::from_secs(now.saturating_sub(ts)))?;
    Some(Status::Level(p, at))
}

fn set_status(s: Status) {
    if let Status::Level(p, _) = s { save_level(p); }
    let changed = {
        let mut cur = STATUS.lock().unwrap();
        let changed = *cur != s;
        *cur = s;
        changed
    };
    let hwnd = HWND_MAIN.load(Ordering::Relaxed);
    if changed && hwnd != 0 {
        unsafe { PostMessageW(hwnd as HWND, WM_STATUS, 0, 0) };
    }
}

// ---------------------------------------------------------------------------
// Thread de lecture HID
// ---------------------------------------------------------------------------

fn find_device() -> Result<Option<hid::Device>, u32> {
    let c = cfg();
    let Some(info) = hid::enumerate(0x1532)
        .into_iter()
        .find(|d| c.product_ids.contains(&d.pid) && d.usage_page == protocol::USAGE_PAGE)
    else {
        return Ok(None);
    };
    hid::Device::open(&info).map(Some).map_err(|e| e.raw_os_error().unwrap_or(0) as u32)
}

fn worker() {
    let c = cfg();
    let poll = Duration::from_millis(c.poll_ms as u64);
    let mut absent = false;
    let mut last_err: Option<u32> = None;
    loop {
        let dev = match find_device() {
            Ok(Some(d)) => {
                log("canal 0xFF14 ouvert");
                absent = false;
                last_err = None;
                d
            }
            Ok(None) => {
                if !absent { log("dongle absent"); absent = true; }
                set_status(Status::NoDongle);
                std::thread::sleep(poll);
                continue;
            }
            Err(code) => {
                if last_err != Some(code) { log(&format!("ouverture du dongle impossible (code {code})")); last_err = Some(code); }
                set_status(Status::Error(code));
                std::thread::sleep(poll.max(Duration::from_secs(5)));
                continue;
            }
        };
        // On garde le dernier niveau connu s'il existe, sinon on attend la première mesure.
        {
            let cur = *STATUS.lock().unwrap();
            if !matches!(cur, Status::Level(..)) { set_status(Status::Waiting); }
        }
        let mut buf = vec![0u8; dev.info.in_len.max(64) as usize];
        let mut last_query: Option<Instant> = None;
        // Poignée de main : le dongle acquitte sans délai, et répond parfois le niveau.
        let _ = dev.write(&protocol::level_query());
        loop {
            if RESYNC.swap(false, Ordering::Relaxed) {
                break; // réouverture du canal
            }
            if REFRESH.swap(false, Ordering::Relaxed) {
                let _ = dev.write(&protocol::level_query());
                last_query = None;
            }
            if !c.query.is_empty() && last_query.is_none_or(|t| t.elapsed() >= poll) {
                let _ = dev.write(&c.query);
                last_query = Some(Instant::now());
            }
            match dev.read(&mut buf, c.poll_ms) {
                Ok(0) => {}
                Ok(n) => {
                    let frame = &buf[..n];
                    if let Some(b) = protocol::parse_battery(frame) {
                        set_status(Status::Level(b.percent, Instant::now()));
                    } else if let Some(Some(level)) = protocol::parse_reply(frame) {
                        // Réponse à `level_query` : rare, mais c'est une vraie mesure.
                        set_status(Status::Level(level, Instant::now()));
                    }
                }
                Err(e) => {
                    // Dongle retiré ou canal invalidé : on journalise puis on repasse en détection.
                    log(&format!("canal perdu ({:?})", e.raw_os_error()));
                    break;
                }
            }
        }
        drop(dev);
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------
// Rendu de l'icône
// ---------------------------------------------------------------------------

/// Dessine `text` dans une icône carrée de la taille système, en couleur `rgb` (0xRRGGBB).
unsafe fn make_icon(text: &str, rgb: u32) -> HICON {
    let size = GetSystemMetrics(SM_CXSMICON).max(16);
    let hdc = CreateCompatibleDC(ptr::null_mut());
    let mut bi: BITMAPINFO = mem::zeroed();
    bi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = size;
    bi.bmiHeader.biHeight = -size;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    let mut bits: *mut core::ffi::c_void = ptr::null_mut();
    let color = CreateDIBSection(hdc, &bi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
    let old_bmp = SelectObject(hdc, color);

    // Texte blanc sur noir, puis on transforme l'intensité en alpha.
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
    px.fill(0);
    let w = wide(text);
    let mut height = size + size / 6;
    let font = loop {
        let f = CreateFontW(-height, 0, 0, 0, FW_BOLD as i32, 0, 0, 0, DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32, CLIP_DEFAULT_PRECIS as u32, NONANTIALIASED_QUALITY as u32,
            (VARIABLE_PITCH | FF_SWISS) as u32, wide("Segoe UI").as_ptr());
        let prev = SelectObject(hdc, f);
        let mut ext: SIZE = mem::zeroed();
        GetTextExtentPoint32W(hdc, w.as_ptr(), (w.len() - 1) as i32, &mut ext);
        SelectObject(hdc, prev);
        if ext.cx <= size + 1 || height <= 8 { break f; }
        DeleteObject(f);
        height -= 1;
    };
    let prev_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, 0x00FF_FFFF);
    let mut rc = RECT { left: -2, top: -1, right: size + 2, bottom: size + 1 };
    DrawTextW(hdc, w.as_ptr(), -1, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP);
    GdiFlush();
    for p in px.iter_mut() {
        let a = *p & 0xFF;
        *p = if a > 0 { 0xFF00_0000 | (rgb & 0x00FF_FFFF) } else { 0 };
    }
    SelectObject(hdc, prev_font);
    DeleteObject(font);
    SelectObject(hdc, old_bmp);

    let mask = CreateBitmap(size, size, 1, 1, ptr::null());
    let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&ii);
    DeleteObject(mask);
    DeleteObject(color);
    DeleteDC(hdc);
    icon
}

fn describe(s: Status) -> (String, String, u32) {
    let stale = cfg().stale_s;
    match s {
        Status::NoDongle => ("×".into(), "BlackShark V3 : dongle non détecté".into(), 0x9E9E9E),
        Status::Waiting => ("…".into(), "BlackShark V3 : en attente d'une mesure\n(casque allumé et à portée ?)".into(), 0xBDBDBD),
        Status::Error(code) => ("!".into(), format!("BlackShark V3 : accès au dongle impossible (erreur {code})"), 0xFF5252),
        Status::Level(p, at) => {
            let age = at.elapsed().as_secs();
            let txt = p.to_string();
            if age > stale {
                // Le casque ne pousse son niveau qu'aux changements : une mesure
                // ancienne est normale, mais il faut le dire clairement.
                let mins = (age / 60).max(1);
                (
                    txt,
                    format!("BlackShark V3 : {p} %\nDernière mesure il y a {mins} min\n(le casque ne signale son niveau qu'aux changements)"),
                    0x9E9E9E,
                )
            } else {
                let tip = if age >= 60 {
                    format!("BlackShark V3 : {p} %\nMesure il y a {} min", age / 60)
                } else {
                    format!("BlackShark V3 : {p} %")
                };
                let color = if p <= cfg().low_battery.max(10) { 0xFF5252 } else if p <= 30 { 0xFFB300 } else { 0x4CE06A };
                (txt, tip, color)
            }
        }
    }
}

unsafe fn update_tray(hwnd: HWND, add: bool) {
    let status = *STATUS.lock().unwrap();
    let (txt, tip, color) = describe(status);
    let mut nid: NOTIFYICONDATAW = mem::zeroed();
    nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = make_icon(&txt, color);
    copy_wide(&mut nid.szTip, &tip);

    // Alerte batterie faible (une fois par passage sous le seuil).
    let low = cfg().low_battery;
    if let Status::Level(p, _) = status {
        if low > 0 && p <= low && !LOW_WARNED.swap(true, Ordering::Relaxed) {
            nid.uFlags |= NIF_INFO;
            nid.dwInfoFlags = NIIF_WARNING;
            copy_wide(&mut nid.szInfoTitle, "Batterie faible");
            copy_wide(&mut nid.szInfo, &format!("Casque BlackShark V3 : {p} %"));
        } else if p > low + 5 {
            LOW_WARNED.store(false, Ordering::Relaxed);
        }
    }

    if !add || Shell_NotifyIconW(NIM_MODIFY, &nid) == 0 {
        Shell_NotifyIconW(NIM_ADD, &nid);
    }
    DestroyIcon(nid.hIcon);
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, ID_REFRESH, wide("Rafraîchir").as_ptr());
    AppendMenuW(menu, MF_STRING, ID_RESYNC, wide("Redétecter le dongle").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quitter").as_ptr());
    let mut pt: POINT = mem::zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, ptr::null());
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_STATUS | WM_TIMER => { update_tray(hwnd, true); 0 }
        WM_TRAY => {
            match lp as u32 {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                WM_LBUTTONDBLCLK => { REFRESH.store(true, Ordering::Relaxed); }
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            match wp & 0xFFFF {
                ID_REFRESH => REFRESH.store(true, Ordering::Relaxed),
                ID_RESYNC => RESYNC.store(true, Ordering::Relaxed),
                ID_QUIT => { log("quitter via le menu"); DestroyWindow(hwnd); }
                _ => {}
            }
            0
        }
        WM_ENDSESSION => {
            if wp != 0 { log("fin de session Windows"); }
            0
        }
        WM_DESTROY => {
            log("arrêt (WM_DESTROY)");
            let mut nid: NOTIFYICONDATAW = mem::zeroed();
            nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            Shell_NotifyIconW(NIM_DELETE, &nid);
            PostQuitMessage(0);
            0
        }
        m if Some(&m) == TASKBAR_CREATED.get() => { update_tray(hwnd, false); 0 }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    // Journalise les paniques : sans ça, l'app disparaît sans laisser de trace.
    std::panic::set_hook(Box::new(|info| log(&format!("PANIQUE : {info}"))));
    log("démarrage");
    unsafe {
        // Une seule instance à la fois.
        CreateMutexW(ptr::null(), 1, wide("Local\\BlackSharkBatteryTray").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log("une autre instance tourne déjà, arrêt");
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    CONFIG.set(Config::load()).unwrap();
    let initial = load_level();

    let _hwnd = unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let class = wide("BlackSharkBatteryTray");
        let mut wc: WNDCLASSW = mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        let _ = TASKBAR_CREATED.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
        let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0,
            ptr::null_mut(), ptr::null_mut(), hinst, ptr::null());
        HWND_MAIN.store(hwnd as usize, Ordering::Relaxed);
        update_tray(hwnd, false);
        // Rafraîchit l'infobulle (âge de la mesure) toutes les 30 s.
        SetTimer(hwnd, TIMER_ID, 30_000, None);
        hwnd
    };

    // Le canal HID n'est ouvert qu'une fois la fenêtre créée : le premier niveau affiché
    // (issu du cache) déclenche ainsi un vrai rafraîchissement de l'icône au démarrage.
    std::thread::spawn(move || {
        if let (Some(s), Ok(Some(_))) = (initial, find_device()) { set_status(s); }
        worker()
    });

    unsafe {
        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        log("fin du programme");
    }
}
