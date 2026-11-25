//! Game controllers.
//!
//! - Xbox controllers (and anything else XInput knows) through
//!   `XInputGetBatteryInformation`, loaded at run time from xinput1_4.dll
//!   (or xinput1_3.dll) so that the program still starts without it. XInput
//!   only gives four levels (empty / low / medium / full); wired
//!   controllers have no battery. Ids are per XInput slot (`xinput-1`…).
//! - Sony DualShock 4 (054C:05C4, 054C:09CC, wireless adapter 054C:0BA0),
//!   DualSense (054C:0CE6) and DualSense Edge (054C:0DF2) through their HID
//!   input reports, over USB and Bluetooth. Offsets and level encoding
//!   follow the Linux hid-playstation driver. Nothing is ever sent to the
//!   controllers: over Bluetooth they only report their battery once in
//!   "extended" report mode (0x11 / 0x31), which a game or Steam switches
//!   on; in the basic mode (report 0x01) the level is unknown and the
//!   controller is not listed.
//!
//! Both are polled every minute (XInput has no change notification; the
//! HID reads take a few milliseconds) and on demand.

use crate::model::{Battery, Status};

/// Result of `XInputGetBatteryInformation`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum XBattery {
    Disconnected,
    Wired,
    Level(f32),
}

pub const BATTERY_TYPE_DISCONNECTED: u8 = 0;
pub const BATTERY_TYPE_WIRED: u8 = 1;

/// XInput battery type / level (BATTERY_LEVEL_EMPTY..FULL) to a percentage.
pub fn xinput_battery(battery_type: u8, level: u8) -> XBattery {
    match battery_type {
        BATTERY_TYPE_DISCONNECTED => XBattery::Disconnected,
        BATTERY_TYPE_WIRED => XBattery::Wired,
        // Alkaline, NiMH, unknown (0xFF): all report a level.
        _ => XBattery::Level(match level {
            0 => 5.0,
            1 => 30.0,
            2 => 65.0,
            _ => 100.0,
        }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PsModel {
    pub name: &'static str,
    pub dualsense: bool,
    /// DualShock 4 USB wireless adapter: reports even without a controller.
    pub dongle: bool,
}

pub const SONY_VID: u16 = 0x054C;

pub fn ps_model(pid: u16) -> Option<PsModel> {
    let (name, dualsense) = match pid {
        0x05C4 | 0x09CC | 0x0BA0 => ("DualShock 4", false),
        0x0CE6 => ("DualSense", true),
        0x0DF2 => ("DualSense Edge", true),
        _ => return None,
    };
    Some(PsModel { name, dualsense, dongle: pid == 0x0BA0 })
}

/// HID collections of Bluetooth devices carry the HID service UUID
/// (classic 0x1124, LE 0x1812) in their path.
pub fn is_bluetooth_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("{00001124-0000-1000-8000-00805f9b34fb}") || p.contains("{00001812-0000-1000-8000-00805f9b34fb}")
}

fn battery(percent: u8, status: Status) -> Battery {
    Battery { percent: percent.min(100) as f32, status, millivolts: None, mileage: None }
}

/// DualShock 4 status byte: level 0..=10 in the low nibble, bit 4 = cable.
pub fn ds4_status(b: u8) -> Option<Battery> {
    let level = b & 0x0F;
    if b & 0x10 != 0 {
        match level {
            0..=9 => Some(battery(level * 10 + 5, Status::Charging)),
            10 => Some(battery(100, Status::Charging)),
            11 => Some(battery(100, Status::Full)),
            _ => None,
        }
    } else {
        Some(battery((level * 10 + 5).min(100), Status::Discharging))
    }
}

/// DualSense status byte: level 0..=10 in the low nibble, charging state
/// in the high one.
pub fn dualsense_status(b: u8) -> Option<Battery> {
    let level = ((b & 0x0F) * 10 + 5).min(100);
    match b >> 4 {
        0x0 => Some(battery(level, Status::Discharging)),
        0x1 => Some(battery(level, Status::Charging)),
        0x2 => Some(battery(100, Status::Full)),
        0xA | 0xB => Some(battery(0, Status::NotCharging)),
        _ => None,
    }
}

/// Battery of a PlayStation controller input report (first byte = report
/// id), `None` for reports that do not carry it.
pub fn parse_ps(model: PsModel, bluetooth: bool, report: &[u8]) -> Option<Battery> {
    let index = match (model.dualsense, bluetooth, *report.first()?) {
        (true, false, 0x01) => 53,
        (true, true, 0x31) => 54,
        (false, false, 0x01) => 30,
        (false, true, 0x11) => 32,
        _ => return None,
    };
    // Basic Bluetooth reports also use id 0x01 but are only ~10 bytes long.
    if report.len() < 40 {
        return None;
    }
    // A wireless adapter without its controller sends all-zero motion
    // data (gyroscope and accelerometer, bytes 13..25).
    if model.dongle && report.get(13..25)?.iter().all(|&b| b == 0) {
        return None;
    }
    let status = *report.get(index)?;
    if model.dualsense {
        dualsense_status(status)
    } else {
        ds4_status(status)
    }
}

#[cfg(windows)]
pub use imp::Gamepads;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::config;
    use crate::hidio::{self, Hid};
    use crate::i18n::tr;
    use crate::log;
    use crate::model::{DeviceInfo, DeviceKind, Hub, Source};
    use crate::tracker::{id_part, Seen, Tracker};
    use crate::win::wide;
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

    const POLL: Duration = Duration::from_secs(60);
    const BATTERY_DEVTYPE_GAMEPAD: u8 = 0;

    #[repr(C)]
    #[derive(Default)]
    struct XInputBatteryInformation {
        battery_type: u8,
        battery_level: u8,
    }

    type GetBatteryInformation = unsafe extern "system" fn(u32, u8, *mut XInputBatteryInformation) -> u32;

    fn load_xinput() -> Option<GetBatteryInformation> {
        for dll in ["xinput1_4.dll", "xinput1_3.dll"] {
            // SAFETY: valid NUL-terminated names; the module stays loaded
            // for the life of the process (never freed).
            unsafe {
                let module = LoadLibraryExW(wide(dll).as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
                if module.is_null() {
                    continue;
                }
                if let Some(f) = GetProcAddress(module, c"XInputGetBatteryInformation".as_ptr().cast()) {
                    // SAFETY: documented signature of the export.
                    return Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, GetBatteryInformation>(f));
                }
            }
        }
        None
    }

    enum Msg {
        Scan,
    }

    /// Handle to the game controller backend thread.
    pub struct Gamepads {
        tx: Sender<Msg>,
    }

    impl Gamepads {
        /// Starts the backend; it does nothing when `cfg.gamepads` is off.
        pub fn start(cfg: &config::OtherDevices, hub: Arc<Hub>) -> Self {
            let (tx, rx) = mpsc::channel();
            if cfg.gamepads {
                let _ = thread::Builder::new().name("gamepads".into()).stack_size(256 * 1024).spawn(move || {
                    let xinput = load_xinput();
                    log!("gamepads: XInput {}", if xinput.is_some() { "available" } else { "missing" });
                    let mut tracker = Tracker::new(Source::Gamepad);
                    loop {
                        let mut seen = xinput.map(xinput_devices).unwrap_or_default();
                        seen.extend(playstation_devices());
                        for e in tracker.scan(seen) {
                            hub.publish(e);
                        }
                        match rx.recv_timeout(POLL) {
                            Ok(Msg::Scan) | Err(RecvTimeoutError::Timeout) => while rx.try_recv().is_ok() {},
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                });
            }
            Self { tx }
        }

        /// Call after a device arrival / removal.
        pub fn rescan(&self) {
            let _ = self.tx.send(Msg::Scan);
        }

        /// Re-reads every battery now.
        pub fn refresh(&self) {
            let _ = self.tx.send(Msg::Scan);
        }
    }

    /// XInput controllers. Headsets (`BATTERY_DEVTYPE_HEADSET`) are not
    /// asked: a headset plugged into a controller has no battery of its own,
    /// and drivers answer that query with the controller's level or a full
    /// "unknown" battery, even for empty slots (phantom "Xbox headsets").
    fn xinput_devices(get: GetBatteryInformation) -> Vec<Seen> {
        let mut out = Vec::new();
        for slot in 0..4u32 {
            let mut info = XInputBatteryInformation::default();
            // SAFETY: valid out pointer of the documented layout.
            if unsafe { get(slot, BATTERY_DEVTYPE_GAMEPAD, &mut info) } != 0 {
                continue;
            }
            let (has_battery, battery) = match xinput_battery(info.battery_type, info.battery_level) {
                XBattery::Disconnected => continue,
                // A wired controller is listed, without battery.
                XBattery::Wired => (false, None),
                XBattery::Level(p) => (true, Some(Battery { percent: p, status: Status::Discharging, millivolts: None, mileage: None })),
            };
            let base = tr(["Xbox controller", "Manette Xbox", "Xbox-Controller", "Mando Xbox", "Controller Xbox", "Comando Xbox"]);
            out.push(Seen {
                id: format!("xinput-{}", slot + 1),
                name: format!("{base} {}", slot + 1),
                kind: DeviceKind::Gamepad,
                has_battery,
                battery,
                info: DeviceInfo::default(),
            });
        }
        out
    }

    fn playstation_devices() -> Vec<Seen> {
        let mut out: Vec<Seen> = Vec::new();
        for info in hidio::hid_devices(|vid| vid == SONY_VID) {
            let Some(model) = ps_model(info.pid) else { continue };
            // The gamepad collection (generic desktop / game pad).
            if info.usage_page != 0x01 || info.usage != 0x05 {
                continue;
            }
            let bluetooth = is_bluetooth_path(&info.path);
            let Some(hid) = Hid::open(&info, false) else { continue };
            let deadline = Instant::now() + Duration::from_millis(600);
            let mut battery = None;
            while battery.is_none() && Instant::now() < deadline {
                match hid.read(250) {
                    Some(report) => battery = parse_ps(model, bluetooth, &report),
                    None => break,
                }
            }
            let Some(battery) = battery else { continue };
            let key = info.serial.clone().or_else(|| hidio::parent_instance_id(&info.path)).unwrap_or_else(|| info.path.clone());
            let id = format!("ps-{}", id_part(&key));
            if out.iter().any(|s| s.id == id) {
                continue;
            }
            out.push(Seen {
                id,
                name: info
                    .product
                    .clone()
                    .filter(|p| !p.eq_ignore_ascii_case("wireless controller"))
                    .unwrap_or_else(|| model.name.to_string()),
                kind: DeviceKind::Gamepad,
                has_battery: true,
                battery: Some(battery),
                info: DeviceInfo { model: Some(format!("Sony {}", model.name)), serial: info.serial.clone(), ..Default::default() },
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xinput_levels() {
        assert_eq!(xinput_battery(0, 3), XBattery::Disconnected);
        assert_eq!(xinput_battery(1, 3), XBattery::Wired);
        assert_eq!(xinput_battery(2, 0), XBattery::Level(5.0));
        assert_eq!(xinput_battery(3, 1), XBattery::Level(30.0));
        assert_eq!(xinput_battery(0xFF, 2), XBattery::Level(65.0));
        assert_eq!(xinput_battery(2, 3), XBattery::Level(100.0));
    }

    #[test]
    fn status_bytes() {
        let p = |b: Option<Battery>| b.map(|b| (b.percent, b.status));
        assert_eq!(p(ds4_status(0x08)), Some((85.0, Status::Discharging)));
        assert_eq!(p(ds4_status(0x0A)), Some((100.0, Status::Discharging)));
        assert_eq!(p(ds4_status(0x15)), Some((55.0, Status::Charging)));
        assert_eq!(p(ds4_status(0x1B)), Some((100.0, Status::Full)));
        assert_eq!(p(ds4_status(0x1F)), None);
        assert_eq!(p(dualsense_status(0x04)), Some((45.0, Status::Discharging)));
        assert_eq!(p(dualsense_status(0x1A)), Some((100.0, Status::Charging)));
        assert_eq!(p(dualsense_status(0x20)), Some((100.0, Status::Full)));
        assert_eq!(p(dualsense_status(0xB0)), Some((0.0, Status::NotCharging)));
        assert_eq!(p(dualsense_status(0xF0)), None);
    }

    #[test]
    fn reports() {
        let ds = ps_model(0x0CE6).unwrap();
        let ds4 = ps_model(0x09CC).unwrap();
        assert!(ds.dualsense && !ds4.dualsense);
        assert_eq!(ps_model(0x1234), None);

        let mut usb = vec![0u8; 64];
        usb[0] = 0x01;
        usb[53] = 0x07;
        assert_eq!(parse_ps(ds, false, &usb).unwrap().percent, 75.0);
        let mut bt = vec![0u8; 78];
        bt[0] = 0x31;
        bt[54] = 0x13;
        assert_eq!(parse_ps(ds, true, &bt).unwrap().status, Status::Charging);
        // Basic Bluetooth report: no battery.
        assert_eq!(parse_ps(ds, true, &[0x01, 0x80, 0x80, 0x80, 0x80, 0, 0, 0, 0, 0]), None);
        assert_eq!(parse_ps(ds, true, &usb), None);

        let mut usb4 = vec![0u8; 64];
        usb4[0] = 0x01;
        usb4[30] = 0x1B;
        assert_eq!(parse_ps(ds4, false, &usb4).unwrap().status, Status::Full);
        let mut bt4 = vec![0u8; 78];
        bt4[0] = 0x11;
        bt4[32] = 0x03;
        assert_eq!(parse_ps(ds4, true, &bt4).unwrap().percent, 35.0);
        assert_eq!(parse_ps(ds4, false, &[0x01]), None);
        let dongle = ps_model(0x0BA0).unwrap();
        assert_eq!(parse_ps(dongle, false, &usb4), None);
        usb4[14] = 0x12;
        assert_eq!(parse_ps(dongle, false, &usb4).unwrap().status, Status::Full);
    }

    #[test]
    fn bluetooth_paths() {
        assert!(is_bluetooth_path(
            r"\\?\hid#{00001124-0000-1000-8000-00805f9b34fb}_vid&0002054c_pid&0ce6#8&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"
        ));
        assert!(!is_bluetooth_path(r"\\?\hid#vid_054c&pid_0ce6&mi_03#8&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"));
    }
}
