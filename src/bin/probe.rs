//! Outil de diagnostic.
//!   probe list                 -> liste les collections HID Razer
//!   probe listen [secs]        -> écoute passivement les canaux constructeur
//!   probe send <page> <hex..>  -> envoie un rapport de sortie puis écoute 1 s
use blackshark_battery::hid::{self, Device};
use std::time::{Duration, Instant};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

fn vendor_devices() -> Vec<Device> {
    hid::enumerate(0x1532)
        .iter()
        .filter(|d| d.usage_page >= 0xFF00)
        .filter_map(|d| match Device::open(d) {
            Ok(dev) => Some(dev),
            Err(e) => { eprintln!("open {:04X} failed: {e}", d.usage_page); None }
        })
        .collect()
}

fn listen(devs: Vec<Device>, dur: Duration) {
    let start = Instant::now();
    std::thread::scope(|s| {
        for d in &devs {
            s.spawn(move || {
                let mut buf = vec![0u8; d.info.in_len as usize];
                while start.elapsed() < dur {
                    match d.read(&mut buf, 200) {
                        Ok(0) => {}
                        Ok(n) => {
                            let last = buf[..n].iter().rposition(|&x| x != 0).map_or(1, |p| p + 1);
                            println!("{:>7.3}s {:04X} [{n}] {}", start.elapsed().as_secs_f32(), d.info.usage_page, hex(&buf[..last.max(8)]));
                        }
                        Err(e) => { println!("{:04X} read error: {e}", d.info.usage_page); break; }
                    }
                }
            });
        }
    });
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("listen") => {
            let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
            listen(vendor_devices(), Duration::from_secs(secs));
        }
        Some("send") => {
            let page = u16::from_str_radix(args[1].trim_start_matches("0x"), 16).unwrap();
            let bytes: Vec<u8> = args[2..].iter().map(|s| u8::from_str_radix(s, 16).unwrap()).collect();
            let devs = vendor_devices();
            let d = devs.iter().find(|d| d.info.usage_page == page).expect("page introuvable");
            match d.write(&bytes) {
                Ok(n) => println!("sent {n} bytes: {}", hex(&bytes)),
                Err(e) => println!("write error: {e}"),
            }
            listen(devs, Duration::from_millis(1000));
        }
        _ => {
            for d in hid::enumerate(0x1532) {
                println!("{:04X}:{:04X} page={:04X} usage={:04X} in={} out={} feat={}\n   {}",
                    d.vid, d.pid, d.usage_page, d.usage, d.in_len, d.out_len, d.feat_len, d.path_string());
            }
        }
    }
}
