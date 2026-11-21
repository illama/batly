//! Wireless mice, keyboards and headsets of other brands, spoken to over
//! HID. Only battery queries are ever sent; nothing that changes a
//! setting of the device.
//!
//! Supported (from the open-source references, untested here):
//! - Razer (VID 1532), protocol of openrazer: 90 byte feature reports,
//!   command class 0x07, id 0x80 (battery level, 0..=255) and 0x84
//!   (charging), XOR checksum. Queried on the collections that have
//!   91 byte feature reports (report id + 90), for the wireless products of
//!   [`RAZER_WIRELESS`] and any product whose name says it is wireless.
//!   The transaction id differs between generations: 0x1F, 0x3F and 0xFF
//!   are tried in turn and the one that answers is remembered.
//! - SteelSeries (VID 1038), protocol of HeadsetControl, see
//!   [`steelseries_model`]: Arctis 1 Wireless / 1 Xbox / 7X / 7P
//!   (request 06 12), Arctis 7 2017 / 7 2019 / Pro 2019 (request 06 18),
//!   Arctis Nova 7 / 7X / 7P (request 00 B0, four steps).
//! - Corsair (VID 1B1C) Void, Void Pro, Void Elite and HS70 wireless
//!   headsets, protocol of HeadsetControl (request C9 64, answer 64 …),
//!   see [`corsair_model`].
//!
//! Not supported: Corsair wireless mice and keyboards (the "Bragi"
//! protocol needs the device to be switched to software mode first, which
//! is a configuration change), SteelSeries mice and the Arctis Nova Pro
//! (its base station protocol is not documented well enough).
//!
//! Polled every minute and on demand.

use crate::model::{Battery, Status};

pub const RAZER_VID: u16 = 0x1532;
pub const STEELSERIES_VID: u16 = 0x1038;
pub const CORSAIR_VID: u16 = 0x1B1C;

/// Vendor id embedded in a HID interface path: `vid_1532` (USB) or
/// `vid&021532` / `vid&00021532` (Bluetooth).
pub fn path_vid(path: &str) -> Option<u16> {
    let p = path.to_ascii_lowercase();
    if let Some(i) = p.find("vid_") {
        return u16::from_str_radix(p.get(i + 4..i + 8)?, 16).ok();
    }
    let i = p.find("vid&")? + 4;
    let digits: String = p[i..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if digits.len() < 4 {
        return None;
    }
    u16::from_str_radix(&digits[digits.len() - 4..], 16).ok()
}

/// USB interface number embedded in a HID interface path (`&mi_03`).
pub fn path_interface(path: &str) -> Option<u8> {
    let p = path.to_ascii_lowercase();
    let i = p.find("&mi_")? + 4;
    u8::from_str_radix(p.get(i..i + 2)?, 16).ok()
}

fn battery(percent: u8, status: Status) -> Battery {
    Battery { percent: percent.min(100) as f32, status, millivolts: None, mileage: None }
}

// ------------------------------------------------------------------ Razer

/// Feature report length including the leading report id (0).
pub const RAZER_REPORT_LEN: usize = 91;
pub const RAZER_TRANSACTIONS: [u8; 3] = [0x1F, 0x3F, 0xFF];
pub const RAZER_CLASS_POWER: u8 = 0x07;
pub const RAZER_GET_BATTERY: u8 = 0x80;
pub const RAZER_GET_CHARGING: u8 = 0x84;

/// Wireless Razer products (mouse / keyboard, wired and receiver PIDs of
/// the same product), from openrazer.
pub const RAZER_WIRELESS: &[(u16, &str)] = &[
    (0x0059, "Razer Lancehead"),
    (0x005A, "Razer Lancehead"),
    (0x0062, "Razer Atheris"),
    (0x0072, "Razer Mamba Wireless"),
    (0x0073, "Razer Mamba Wireless"),
    (0x007A, "Razer Viper Ultimate"),
    (0x007B, "Razer Viper Ultimate"),
    (0x007C, "Razer DeathAdder V2 Pro"),
    (0x007D, "Razer DeathAdder V2 Pro"),
    (0x0083, "Razer Basilisk X HyperSpeed"),
    (0x0086, "Razer Basilisk Ultimate"),
    (0x0088, "Razer Basilisk Ultimate"),
    (0x008F, "Razer Naga Pro"),
    (0x0090, "Razer Naga Pro"),
    (0x0094, "Razer Orochi V2"),
    (0x009E, "Razer Viper Mini SE"),
    (0x009F, "Razer Viper Mini SE"),
    (0x00A5, "Razer Viper V2 Pro"),
    (0x00A6, "Razer Viper V2 Pro"),
    (0x00A7, "Razer Naga V2 Pro"),
    (0x00A8, "Razer Naga V2 Pro"),
    (0x00AA, "Razer Basilisk V3 Pro"),
    (0x00AB, "Razer Basilisk V3 Pro"),
    (0x00AF, "Razer Cobra Pro"),
    (0x00B0, "Razer Cobra Pro"),
    (0x00B6, "Razer DeathAdder V3 Pro"),
    (0x00B7, "Razer DeathAdder V3 Pro"),
    (0x025A, "Razer BlackWidow V3 Pro"),
    (0x025C, "Razer BlackWidow V3 Pro"),
];

pub fn razer_known(pid: u16) -> Option<&'static str> {
    RAZER_WIRELESS.iter().find(|(p, _)| *p == pid).map(|(_, n)| *n)
}

/// Whether a Razer product (not in the table) looks wireless.
pub fn razer_wireless_name(product: &str) -> bool {
    let p = product.to_ascii_lowercase();
    ["wireless", "hyperspeed", "ultimate"].iter().any(|w| p.contains(w))
}

/// XOR of bytes 2..88 of the 90 byte report (without report id).
pub fn razer_crc(report: &[u8]) -> u8 {
    report.iter().take(88).skip(2).fold(0, |a, b| a ^ b)
}

/// A "get" request: report id 0 then the 90 byte Razer report.
pub fn razer_request(transaction: u8, class: u8, id: u8, data_size: u8) -> [u8; RAZER_REPORT_LEN] {
    let mut buf = [0u8; RAZER_REPORT_LEN];
    let r = &mut buf[1..];
    r[1] = transaction;
    r[5] = data_size;
    r[6] = class;
    r[7] = id;
    r[88] = razer_crc(r);
    buf
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RazerReply {
    /// Arguments 0 and 1 of a successful answer.
    Ok(u8, u8),
    Busy,
    Failed,
}

/// Answer to a request (with its report id byte first).
pub fn razer_reply(buf: &[u8], class: u8, id: u8) -> RazerReply {
    let Some(r) = buf.get(1..RAZER_REPORT_LEN) else { return RazerReply::Failed };
    if r[6] != class || r[7] != id {
        return RazerReply::Failed;
    }
    match r[0] {
        0x01 => RazerReply::Busy,
        0x02 if r[88] == razer_crc(r) => RazerReply::Ok(r[8], r[9]),
        _ => RazerReply::Failed,
    }
}

/// Battery level argument (0..=255) to a percentage; `None` for 0, which a
/// receiver reports when its device is off.
pub fn razer_percent(level: u8) -> Option<u8> {
    (level > 0).then(|| ((level as u16 * 100 + 127) / 255) as u8)
}

// ------------------------------------------------------------- SteelSeries

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arctis {
    /// Arctis 1 Wireless family: request 06 12, level in byte 3.
    One,
    /// Arctis 7 / Pro: request 06 18, level in byte 2.
    Seven,
    /// Arctis Nova 7 family: request (00) B0, level 0..=4 in byte 2,
    /// state in byte 3.
    Nova7,
}

impl Arctis {
    /// Output report to send (first byte = report id).
    pub fn request(self) -> &'static [u8] {
        match self {
            Arctis::One => &[0x06, 0x12],
            Arctis::Seven => &[0x06, 0x18],
            Arctis::Nova7 => &[0x00, 0xB0],
        }
    }

    pub fn interface(self) -> u8 {
        match self {
            Arctis::One | Arctis::Nova7 => 3,
            Arctis::Seven => 5,
        }
    }
}

pub fn steelseries_model(pid: u16) -> Option<(&'static str, Arctis)> {
    Some(match pid {
        0x12B3 => ("SteelSeries Arctis 1 Wireless", Arctis::One),
        0x12B6 => ("SteelSeries Arctis 1 Xbox", Arctis::One),
        0x12D7 => ("SteelSeries Arctis 7X", Arctis::One),
        0x12D5 => ("SteelSeries Arctis 7P", Arctis::One),
        0x1260 => ("SteelSeries Arctis 7", Arctis::Seven),
        0x12AD => ("SteelSeries Arctis 7", Arctis::Seven),
        0x1252 => ("SteelSeries Arctis Pro Wireless", Arctis::Seven),
        0x2202 => ("SteelSeries Arctis Nova 7", Arctis::Nova7),
        0x2206 => ("SteelSeries Arctis Nova 7X", Arctis::Nova7),
        0x2258 => ("SteelSeries Arctis Nova 7X", Arctis::Nova7),
        0x220A => ("SteelSeries Arctis Nova 7P", Arctis::Nova7),
        _ => return None,
    })
}

/// Strips the 0 report id Windows puts in front of reports of collections
/// without report ids (hidapi does the same).
fn strip_zero_id(report: &[u8]) -> &[u8] {
    match report.split_first() {
        Some((0, rest)) => rest,
        _ => report,
    }
}

/// Answer of an Arctis headset; `Ok(None)` when the headset is off,
/// `Err` for a report that is not the answer.
pub fn arctis_reply(model: Arctis, report: &[u8]) -> Result<Option<Battery>, ()> {
    let r = strip_zero_id(report);
    let expected = strip_zero_id(model.request())[0];
    if r.first() != Some(&expected) || r.len() < 4 {
        return Err(());
    }
    Ok(match model {
        Arctis::One => (r[2] != 0x01).then(|| battery(r[3], Status::Discharging)),
        Arctis::Seven => (r[2] != 0).then(|| battery(r[2], Status::Discharging)),
        Arctis::Nova7 => {
            let percent = r[2].min(4) * 25;
            match r[3] {
                0x01 => Some(battery(percent, Status::Charging)),
                0x03 => Some(battery(percent, Status::Discharging)),
                _ => None,
            }
        }
    })
}

// ----------------------------------------------------------------- Corsair

pub const CORSAIR_REQUEST: [u8; 2] = [0xC9, 0x64];
pub const CORSAIR_USAGE_PAGE: u16 = 0xFFC5;

pub fn corsair_model(pid: u16) -> Option<&'static str> {
    Some(match pid {
        0x1B27 => "Corsair Void Wireless",
        0x0A14 | 0x0A16 | 0x0A17 | 0x0A1A => "Corsair Void Pro Wireless",
        0x0A51 | 0x0A55 => "Corsair Void Elite Wireless",
        0x0A38 => "Corsair HS70 Wireless",
        0x0A4F => "Corsair HS70 Pro Wireless",
        _ => return None,
    })
}

/// Answer `64 xx LL xx SS`: LL = level (bit 7 = microphone up), SS = state
/// (1 / 2 on battery, 0 / 4 / 5 charging). `Ok(None)` = headset off.
pub fn corsair_reply(report: &[u8]) -> Result<Option<Battery>, ()> {
    if report.first() != Some(&0x64) || report.len() < 5 {
        return Err(());
    }
    let level = report[2] & 0x7F;
    Ok(match report[4] {
        _ if level == 0 => None,
        1 | 2 => Some(battery(level, Status::Discharging)),
        0 | 4 => Some(battery(level, Status::Charging)),
        5 => Some(battery(level, if level >= 100 { Status::Full } else { Status::Charging })),
        _ => None,
    })
}

#[cfg(windows)]
pub use imp::Vendors;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::config;
    use crate::hidio::{self, Hid, HidInfo};
    use crate::log;
    use crate::model::{DeviceInfo, DeviceKind, Hub, Source};
    use crate::tracker::{id_part, Seen, Tracker};
    use std::collections::HashMap;
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    const POLL: Duration = Duration::from_secs(60);

    #[derive(Clone, Copy)]
    struct Flags {
        razer: bool,
        corsair: bool,
        steelseries: bool,
    }

    /// Handle to the other-brands backend thread.
    pub struct Vendors {
        tx: Sender<()>,
    }

    impl Vendors {
        /// Starts the backend; it does nothing when the three brands are off.
        pub fn start(cfg: &config::OtherDevices, hub: Arc<Hub>) -> Self {
            let (tx, rx) = mpsc::channel();
            let flags = Flags { razer: cfg.razer, corsair: cfg.corsair, steelseries: cfg.steelseries };
            if flags.razer || flags.corsair || flags.steelseries {
                let _ = thread::Builder::new().name("vendors".into()).stack_size(256 * 1024).spawn(move || {
                    let mut tracker = Tracker::new(Source::Vendor);
                    // Razer transaction id that answered, per product.
                    let mut transactions: HashMap<u16, u8> = HashMap::new();
                    loop {
                        let seen = scan(flags, &mut transactions);
                        for e in tracker.scan(seen) {
                            hub.publish(e);
                        }
                        match rx.recv_timeout(POLL) {
                            Ok(()) | Err(RecvTimeoutError::Timeout) => while rx.try_recv().is_ok() {},
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                });
            }
            Self { tx }
        }

        /// Call after a device arrival / removal.
        pub fn rescan(&self) {
            let _ = self.tx.send(());
        }

        /// Re-reads every battery now.
        pub fn refresh(&self) {
            let _ = self.tx.send(());
        }
    }

    fn scan(flags: Flags, transactions: &mut HashMap<u16, u8>) -> Vec<Seen> {
        let wanted = |vid: u16| {
            (flags.razer && vid == RAZER_VID) || (flags.corsair && vid == CORSAIR_VID) || (flags.steelseries && vid == STEELSERIES_VID)
        };
        let mut out: Vec<Seen> = Vec::new();
        // Physical devices already answered (one collection is enough).
        let mut done: Vec<String> = Vec::new();
        for info in hidio::hid_devices(wanted) {
            let group = hidio::parent_instance_id(&info.path).unwrap_or_else(|| info.path.clone());
            if done.contains(&group) {
                continue;
            }
            let found = match info.vid {
                RAZER_VID => razer(&info, transactions),
                STEELSERIES_VID => steelseries(&info),
                CORSAIR_VID => corsair(&info),
                _ => None,
            };
            let Some((name, kind, battery)) = found else { continue };
            done.push(group.clone());
            let brand = match info.vid {
                RAZER_VID => "razer",
                STEELSERIES_VID => "steelseries",
                _ => "corsair",
            };
            let id = match &info.serial {
                Some(serial) => format!("{brand}-{:04x}-{}", info.pid, id_part(serial)),
                None => format!("{brand}-{:04x}", info.pid),
            };
            if out.iter().any(|s| s.id == id) {
                continue;
            }
            log!("vendors: {id} {name} {:?}", battery);
            out.push(Seen {
                id,
                name: name.clone(),
                kind,
                has_battery: true,
                battery: Some(battery),
                info: DeviceInfo {
                    model: Some(name).filter(|n| info.product.as_deref() != Some(n.as_str())),
                    serial: info.serial.clone(),
                    ..Default::default()
                },
            });
        }
        out
    }

    fn display_name(info: &HidInfo, fallback: &str) -> String {
        info.product.clone().unwrap_or_else(|| fallback.to_string())
    }

    // --------------------------------------------------------------- Razer

    fn razer_query(hid: &Hid, transaction: u8, id: u8) -> Option<(u8, u8)> {
        let request = razer_request(transaction, RAZER_CLASS_POWER, id, 0x02);
        if !hid.set_feature(&request) {
            return None;
        }
        for _ in 0..6 {
            thread::sleep(Duration::from_millis(40));
            let mut buf = [0u8; RAZER_REPORT_LEN];
            if !hid.get_feature(&mut buf) {
                return None;
            }
            match razer_reply(&buf, RAZER_CLASS_POWER, id) {
                RazerReply::Ok(a, b) => return Some((a, b)),
                RazerReply::Busy => continue,
                RazerReply::Failed => return None,
            }
        }
        None
    }

    fn razer(info: &HidInfo, transactions: &mut HashMap<u16, u8>) -> Option<(String, DeviceKind, Battery)> {
        if info.feature_len != RAZER_REPORT_LEN {
            return None;
        }
        let known = razer_known(info.pid);
        if known.is_none() && !info.product.as_deref().is_some_and(razer_wireless_name) {
            return None;
        }
        let hid = Hid::open_features(info)?;
        let order: Vec<u8> = transactions.get(&info.pid).copied().into_iter().chain(RAZER_TRANSACTIONS).collect();
        let (transaction, level) = order.into_iter().find_map(|t| Some((t, razer_query(&hid, t, RAZER_GET_BATTERY)?.1)))?;
        transactions.insert(info.pid, transaction);
        let percent = razer_percent(level)?;
        let charging = razer_query(&hid, transaction, RAZER_GET_CHARGING).is_some_and(|(_, c)| c == 1);
        let status = match (charging, percent) {
            (true, 100) => Status::Full,
            (true, _) => Status::Charging,
            _ => Status::Discharging,
        };
        let name = known.map(str::to_string).unwrap_or_else(|| display_name(info, "Razer"));
        let lower = name.to_ascii_lowercase();
        let kind = if ["blackwidow", "keyboard", "ornata", "huntsman", "deathstalker"].iter().any(|w| lower.contains(w)) {
            DeviceKind::Keyboard
        } else if ["barracuda", "kraken", "nari", "blackshark", "headset"].iter().any(|w| lower.contains(w)) {
            DeviceKind::Headset
        } else {
            DeviceKind::Mouse
        };
        Some((name, kind, Battery { percent: percent as f32, status, millivolts: None, mileage: None }))
    }

    // ---------------------------------------------- headsets (write / read)

    /// Sends `request` and returns the first answer accepted by `parse`.
    fn ask<T>(info: &HidInfo, request: &[u8], parse: impl Fn(&[u8]) -> Result<T, ()>) -> Option<T> {
        if info.output_len < request.len() {
            return None;
        }
        let hid = Hid::open(info, true)?;
        if !hid.write(request) {
            return None;
        }
        let deadline = Instant::now() + Duration::from_millis(1000);
        while Instant::now() < deadline {
            let report = hid.read(500)?;
            if let Ok(v) = parse(&report) {
                return Some(v);
            }
        }
        None
    }

    fn steelseries(info: &HidInfo) -> Option<(String, DeviceKind, Battery)> {
        let (name, model) = steelseries_model(info.pid)?;
        if info.usage_page < 0xFF00 || path_interface(&info.path).is_some_and(|i| i != model.interface()) {
            return None;
        }
        let battery = ask(info, model.request(), |r| arctis_reply(model, r))??;
        Some((name.to_string(), DeviceKind::Headset, battery))
    }

    fn corsair(info: &HidInfo) -> Option<(String, DeviceKind, Battery)> {
        let name = corsair_model(info.pid)?;
        if info.usage_page != CORSAIR_USAGE_PAGE {
            return None;
        }
        let battery = ask(info, &CORSAIR_REQUEST, corsair_reply)??;
        Some((display_name(info, name), DeviceKind::Headset, battery))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(path_vid(r"\\?\hid#vid_1532&pid_007b&mi_00#7&1#{4d1e55b2-f16f-11cf-88cb-001111000030}"), Some(0x1532));
        assert_eq!(path_vid(r"\\?\hid#{00001124-0000-1000-8000-00805f9b34fb}_vid&0002054c_pid&09cc#8&1"), Some(0x054C));
        assert_eq!(path_vid(r"\\?\hid#{00001812-0000-1000-8000-00805f9b34fb}_dev_vid&02046d_pid&b023#9&1"), Some(0x046D));
        assert_eq!(path_vid(r"\\?\hid#acpi0c50#1"), None);
        assert_eq!(path_interface(r"\\?\hid#vid_1038&pid_12ad&mi_05&col01#8&1"), Some(5));
        assert_eq!(path_interface(r"\\?\hid#vid_1038&pid_12ad#8&1"), None);
    }

    #[test]
    fn razer_protocol() {
        let req = razer_request(0x1F, RAZER_CLASS_POWER, RAZER_GET_BATTERY, 0x02);
        assert_eq!(req.len(), 91);
        assert_eq!(&req[..9], &[0x00, 0x00, 0x1F, 0x00, 0x00, 0x00, 0x02, 0x07, 0x80]);
        // CRC: 0x02 ^ 0x07 ^ 0x80.
        assert_eq!(req[89], 0x85);

        let mut reply = req;
        reply[1] = 0x02;
        reply[9] = 0x00;
        reply[10] = 0xFF;
        reply[89] = razer_crc(&reply[1..]);
        assert_eq!(razer_reply(&reply, 0x07, 0x80), RazerReply::Ok(0x00, 0xFF));
        assert_eq!(razer_reply(&reply, 0x07, 0x84), RazerReply::Failed);
        reply[89] ^= 1;
        assert_eq!(razer_reply(&reply, 0x07, 0x80), RazerReply::Failed);
        reply[1] = 0x01;
        assert_eq!(razer_reply(&reply, 0x07, 0x80), RazerReply::Busy);
        assert_eq!(razer_reply(&reply[..10], 0x07, 0x80), RazerReply::Failed);

        assert_eq!(razer_percent(255), Some(100));
        assert_eq!(razer_percent(128), Some(50));
        assert_eq!(razer_percent(0), None);
        assert_eq!(razer_known(0x007B), Some("Razer Viper Ultimate"));
        assert!(razer_wireless_name("Razer Viper V3 HyperSpeed"));
        assert!(!razer_wireless_name("Razer Huntsman"));
    }

    #[test]
    fn steelseries_protocol() {
        let p = |r: Result<Option<Battery>, ()>| r.map(|b| b.map(|b| (b.percent, b.status)));
        assert_eq!(steelseries_model(0x12AD).map(|m| m.1), Some(Arctis::Seven));
        assert_eq!(steelseries_model(0x0000), None);
        assert_eq!(p(arctis_reply(Arctis::Seven, &[0x06, 0x18, 0x4B, 0, 0, 0, 0, 0])), Ok(Some((75.0, Status::Discharging))));
        assert_eq!(p(arctis_reply(Arctis::Seven, &[0x06, 0x18, 0x00, 0])), Ok(None));
        assert_eq!(p(arctis_reply(Arctis::Seven, &[0x07, 0x18, 0x4B, 0])), Err(()));
        assert_eq!(p(arctis_reply(Arctis::One, &[0x06, 0x12, 0x00, 0x96])), Ok(Some((100.0, Status::Discharging))));
        assert_eq!(p(arctis_reply(Arctis::One, &[0x06, 0x12, 0x01, 0x50])), Ok(None));
        // Nova 7: Windows prefixes the report with id 0.
        assert_eq!(p(arctis_reply(Arctis::Nova7, &[0x00, 0xB0, 0x00, 0x03, 0x03, 0, 0, 0, 0])), Ok(Some((75.0, Status::Discharging))));
        assert_eq!(p(arctis_reply(Arctis::Nova7, &[0xB0, 0x00, 0x02, 0x01])), Ok(Some((50.0, Status::Charging))));
        assert_eq!(p(arctis_reply(Arctis::Nova7, &[0xB0, 0x00, 0x02, 0x00])), Ok(None));
        assert_eq!(p(arctis_reply(Arctis::Nova7, &[0xB1, 0x00, 0x02, 0x03])), Err(()));
    }

    #[test]
    fn corsair_protocol() {
        let p = |r: Result<Option<Battery>, ()>| r.map(|b| b.map(|b| (b.percent, b.status)));
        assert_eq!(corsair_model(0x0A55), Some("Corsair Void Elite Wireless"));
        assert_eq!(p(corsair_reply(&[0x64, 0x00, 0xB7, 0xB1, 0x01])), Ok(Some((55.0, Status::Discharging))));
        assert_eq!(p(corsair_reply(&[0x64, 0x00, 0x37, 0xB1, 0x04])), Ok(Some((55.0, Status::Charging))));
        assert_eq!(p(corsair_reply(&[0x64, 0x00, 0x64, 0xB1, 0x05])), Ok(Some((100.0, Status::Full))));
        assert_eq!(p(corsair_reply(&[0x64, 0x00, 0x00, 0xB1, 0x01])), Ok(None));
        assert_eq!(p(corsair_reply(&[0x65, 0x00, 0x37, 0xB1, 0x01])), Err(()));
        assert_eq!(p(corsair_reply(&[0x64, 0x00])), Err(()));
    }
}
