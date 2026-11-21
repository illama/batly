//! HID++ 1.0 / 2.0 protocol engine, independent of the operating system.
//!
//! One [`Session`] drives one physical endpoint (a Unifying / Bolt /
//! Lightspeed receiver, or a device connected directly by cable or
//! Bluetooth). It runs on its own thread, is fully event driven (it blocks
//! in `Transport::read` until the next poll is due, a report arrives or a
//! command is posted) and publishes [`Event`]s.
//!
//! Compared with the original C# implementation:
//! * features are resolved with targeted `Root.GetFeature` calls instead of
//!   walking the whole feature table (5 requests instead of 30+),
//! * battery broadcast events are decoded, so charging / level changes show
//!   up immediately instead of at the next poll,
//! * receivers get wireless notifications enabled, so a device waking up is
//!   polled right away and a sleeping one is not hammered,
//! * devices exposing only the long report (Bluetooth LE) are supported.

use crate::log;
use crate::model::{Battery, DeviceKind, Event, Source, Status};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

pub const REPORT_SHORT: u8 = 0x10;
pub const REPORT_LONG: u8 = 0x11;
pub const SHORT_LEN: usize = 7;
pub const LONG_LEN: usize = 20;

/// Software id put in the low nibble of the function byte, used to tell our
/// answers apart from the ones triggered by G HUB / Options+.
const SW_ID: u8 = 0x0A;
const RECEIVER_IDX: u8 = 0xFF;

const FEATURE_ROOT: u8 = 0x00;
const FEAT_DEVICE_INFO: u16 = 0x0003;
const FEAT_DEVICE_NAME: u16 = 0x0005;
const FEAT_BATTERY_LEVEL: u16 = 0x1000;
const FEAT_BATTERY_VOLTAGE: u16 = 0x1001;
const FEAT_UNIFIED_BATTERY: u16 = 0x1004;

const REQUEST_TIMEOUT: Duration = Duration::from_millis(400);
const PING_TIMEOUT: Duration = Duration::from_millis(250);
const IDLE: Duration = Duration::from_secs(3600);

pub enum Incoming {
    Report(Vec<u8>),
    Timeout,
    /// The wait was interrupted because a command is pending.
    Interrupted,
}

/// Raw report channel to one physical endpoint.
pub trait Transport {
    fn has_short(&self) -> bool;
    /// `report` starts with the report id and is exactly 7 or 20 bytes.
    fn write(&mut self, report: &[u8]) -> io::Result<()>;
    fn read(&mut self, timeout: Duration) -> io::Result<Incoming>;
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// Delay before re-trying a device that did not answer.
    pub retry: Duration,
    /// Delay between two successful battery polls.
    pub poll: Duration,
    /// Devices whose name contains one of these strings are ignored.
    pub disabled: Vec<String>,
    /// Grace period after a "device connected" notification.
    pub settle: Duration,
}

pub enum Command {
    Refresh,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BatteryFeature {
    Unified(u8),
    Level(u8),
    Voltage(u8),
}

impl BatteryFeature {
    fn index(self) -> u8 {
        match self {
            Self::Unified(i) | Self::Level(i) | Self::Voltage(i) => i,
        }
    }

    /// Function used to query the current state.
    fn getter(self) -> u8 {
        match self {
            Self::Unified(_) => 1,
            Self::Level(_) | Self::Voltage(_) => 0,
        }
    }

    pub fn parse(self, params: &[u8]) -> Option<Battery> {
        match self {
            Self::Unified(_) => parse_unified(params),
            Self::Level(_) => parse_level(params),
            Self::Voltage(_) => parse_voltage(params),
        }
    }
}

/// Feature 0x1000 – Battery Unified Level Status.
pub fn parse_level(p: &[u8]) -> Option<Battery> {
    if p.len() < 3 {
        return None;
    }
    let status = match p[2] {
        0 => Status::Discharging,
        1 | 2 | 4 => Status::Charging,
        3 => Status::Full,
        _ => Status::NotCharging,
    };
    Some(Battery { percent: p[0].min(100) as f32, status, millivolts: None, mileage: None })
}

/// Feature 0x1004 – Unified Battery.
pub fn parse_unified(p: &[u8]) -> Option<Battery> {
    if p.len() < 3 {
        return None;
    }
    let status = match p[2] {
        0 => Status::Discharging,
        1 | 2 => Status::Charging,
        3 => Status::Full,
        _ => Status::NotCharging,
    };
    // Some devices only report a coarse level bitmask (critical / low /
    // good / full) and leave the percentage at zero.
    let percent = match (p[0], p[1]) {
        (0, l) if l & 0x08 != 0 => 90,
        (0, l) if l & 0x04 != 0 => 50,
        (0, l) if l & 0x02 != 0 => 20,
        (0, l) if l & 0x01 != 0 => 5,
        (pct, _) => pct.min(100),
    };
    Some(Battery { percent: percent as f32, status, millivolts: None, mileage: None })
}

/// Feature 0x1001 – Battery Voltage.
pub fn parse_voltage(p: &[u8]) -> Option<Battery> {
    if p.len() < 3 {
        return None;
    }
    let mv = u16::from_be_bytes([p[0], p[1]]);
    let flags = p[2];
    let status = if flags & 0x80 != 0 {
        match flags & 0x07 {
            0 => Status::Charging,
            1 => Status::Full,
            2 => Status::NotCharging,
            _ => Status::Unknown,
        }
    } else {
        Status::Discharging
    };
    Some(Battery { percent: percent_from_millivolts(mv) as f32, status, millivolts: Some(mv), mileage: None })
}

/// Average discharge curve of a 3.7 V LiPo cell, 100 % down to 1 %.
const MV_LUT: [u16; 100] = [
    4186, 4156, 4143, 4133, 4122, 4113, 4103, 4094, 4086, 4075, 4067, 4059, 4051, 4043, 4035, 4027, 4019, 4011, 4003,
    3997, 3989, 3983, 3976, 3969, 3961, 3955, 3949, 3942, 3935, 3929, 3922, 3916, 3909, 3902, 3896, 3890, 3883, 3877,
    3870, 3865, 3859, 3853, 3848, 3842, 3837, 3833, 3828, 3824, 3819, 3815, 3811, 3808, 3804, 3800, 3797, 3793, 3790,
    3787, 3784, 3781, 3778, 3775, 3772, 3770, 3767, 3764, 3762, 3759, 3757, 3754, 3751, 3748, 3744, 3741, 3737, 3734,
    3730, 3726, 3724, 3720, 3717, 3714, 3710, 3706, 3702, 3697, 3693, 3688, 3683, 3677, 3671, 3666, 3662, 3658, 3654,
    3646, 3633, 3612, 3579, 3537,
];

pub fn percent_from_millivolts(mv: u16) -> u8 {
    // The table is sorted in descending order: binary search the first
    // entry the voltage exceeds.
    let below = MV_LUT.partition_point(|&v| v >= mv);
    (MV_LUT.len() - below) as u8
}

struct Info {
    id: String,
    battery: Option<BatteryFeature>,
}

#[derive(Default)]
struct Slot {
    info: Option<Info>,
    /// Next time this device needs attention; `None` = wait for an event.
    due: Option<Instant>,
    fails: u32,
}

pub struct Session<T: Transport> {
    transport: T,
    cfg: Settings,
    sink: Box<dyn Fn(Event) + Send>,
    commands: Receiver<Command>,
    devices: BTreeMap<u8, Slot>,
    backlog: VecDeque<Vec<u8>>,
    /// Directly connected device (no receiver in between).
    direct: bool,
    ping_seq: u8,
    stopping: bool,
    tag: String,
}

impl<T: Transport> Session<T> {
    pub fn new(
        transport: T,
        cfg: Settings,
        commands: Receiver<Command>,
        sink: impl Fn(Event) + Send + 'static,
        tag: String,
    ) -> Self {
        Self {
            transport,
            cfg,
            sink: Box::new(sink),
            commands,
            devices: BTreeMap::new(),
            backlog: VecDeque::new(),
            direct: false,
            ping_seq: 0x55,
            stopping: false,
            tag,
        }
    }

    /// Runs until the endpoint disappears or a `Stop` command is received.
    pub fn run(mut self) {
        let result = self.event_loop();
        log!("[{}] session ended: {:?}", self.tag, result.err());
        for slot in self.devices.values() {
            if let Some(info) = &slot.info {
                (self.sink)(Event::Offline { id: info.id.clone() });
            }
        }
    }

    fn event_loop(&mut self) -> io::Result<()> {
        self.discover()?;
        loop {
            while let Some(report) = self.backlog.pop_front() {
                self.on_unsolicited(&report);
            }
            if self.drain_commands() {
                return Ok(());
            }

            let now = Instant::now();
            let due: Vec<u8> =
                self.devices.iter().filter(|(_, s)| s.due.is_some_and(|d| d <= now)).map(|(&i, _)| i).collect();
            if !due.is_empty() {
                for idx in due {
                    self.service(idx)?;
                }
                continue;
            }

            let wait = self.devices.values().filter_map(|s| s.due).min().map_or(IDLE, |d| d.saturating_duration_since(now));
            if let Incoming::Report(report) = self.transport.read(wait)? {
                self.on_unsolicited(&report);
            }
        }
    }

    /// Returns true when the session must stop.
    fn drain_commands(&mut self) -> bool {
        loop {
            match self.commands.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => self.stopping = true,
                Ok(Command::Refresh) => {
                    let now = Instant::now();
                    for slot in self.devices.values_mut() {
                        slot.due = Some(now);
                        slot.fails = 0;
                    }
                }
                Err(TryRecvError::Empty) => break,
            }
            if self.stopping {
                break;
            }
        }
        self.stopping
    }

    // ---------------------------------------------------------------- I/O

    fn send(&mut self, idx: u8, b2: u8, b3: u8, params: &[u8]) -> io::Result<()> {
        let mut buf = [0u8; LONG_LEN];
        let len = if params.len() <= 3 && self.transport.has_short() {
            buf[0] = REPORT_SHORT;
            SHORT_LEN
        } else {
            buf[0] = REPORT_LONG;
            LONG_LEN
        };
        buf[1] = idx;
        buf[2] = b2;
        buf[3] = b3;
        buf[4..4 + params.len()].copy_from_slice(params);
        log!("[{}] > {}", self.tag, log::hex(&buf[..len]));
        self.transport.write(&buf[..len])
    }

    fn idx_matches(&self, report: &[u8], idx: u8) -> bool {
        // Devices connected by Bluetooth answer with index 0x00 or 0xFF.
        self.direct || report[1] == idx
    }

    /// Sends a request and waits for its answer; unrelated reports received
    /// meanwhile are queued. `Ok(None)` = error reply or timeout.
    fn transact(&mut self, idx: u8, b2: u8, b3: u8, params: &[u8], timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        self.send(idx, b2, b3, params)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                log!("[{}] timeout", self.tag);
                return Ok(None);
            }
            let report = match self.transport.read(left)? {
                Incoming::Report(r) => r,
                Incoming::Timeout => continue,
                Incoming::Interrupted => {
                    // Only abort for a stop request; a refresh can wait.
                    if self.drain_commands() {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
                    }
                    continue;
                }
            };
            if report.len() < SHORT_LEN {
                continue;
            }
            log!("[{}] < {}", self.tag, log::hex(&report));
            if self.idx_matches(&report, idx) {
                if report[2] == b2 && report[3] == b3 {
                    return Ok(Some(report));
                }
                // HID++ 1.0 error (0x8F) or HID++ 2.0 error (0xFF).
                if (report[2] == 0x8F || report[2] == 0xFF) && report[3] == b2 && report[4] == b3 {
                    return Ok(None);
                }
            }
            if self.backlog.len() < 32 {
                self.backlog.push_back(report);
            }
        }
    }

    /// HID++ 2.0 feature call; returns the parameters of the answer.
    fn call(&mut self, idx: u8, feature: u8, function: u8, params: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let reply = self.transact(idx, feature, (function << 4) | SW_ID, params, REQUEST_TIMEOUT)?;
        Ok(reply.map(|r| r[4..].to_vec()))
    }

    /// HID++ 1.0 register access on the receiver.
    fn register(&mut self, sub_id: u8, address: u8, params: [u8; 3]) -> io::Result<Option<[u8; 3]>> {
        let reply = self.transact(RECEIVER_IDX, sub_id, address, &params, REQUEST_TIMEOUT)?;
        Ok(reply.map(|r| [r[4], r[5], r[6]]))
    }

    fn ping(&mut self, idx: u8) -> io::Result<bool> {
        self.ping_seq = self.ping_seq.wrapping_add(1).max(1);
        let seq = self.ping_seq;
        let reply = self.transact(idx, FEATURE_ROOT, 0x10 | SW_ID, &[0, 0, seq], PING_TIMEOUT)?;
        Ok(reply.is_some_and(|r| r[6] == seq))
    }

    /// Collects unsolicited reports for `duration`.
    fn pump(&mut self, duration: Duration) -> io::Result<()> {
        let deadline = Instant::now() + duration;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            match self.transport.read(left)? {
                Incoming::Report(r) => {
                    log!("[{}] < {}", self.tag, log::hex(&r));
                    self.backlog.push_back(r)
                }
                Incoming::Timeout => {}
                Incoming::Interrupted => {
                    if self.drain_commands() {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------- discovery

    fn discover(&mut self) -> io::Result<()> {
        // Register 0x02 (connection state) only exists on receivers.
        let is_receiver = self.register(0x81, 0x02, [0; 3])?.is_some();
        log!("[{}] receiver: {}", self.tag, is_receiver);

        if is_receiver {
            // Enable "wireless" + "software present" notifications, keeping
            // whatever other flags are already set.
            if let Some(flags) = self.register(0x81, 0x00, [0; 3])? {
                if flags[1] & 0x09 != 0x09 {
                    self.register(0x80, 0x00, [flags[0], flags[1] | 0x09, flags[2]])?;
                }
            }
            // Ask the receiver to announce every paired device (0x41).
            self.register(0x80, 0x02, [0x02, 0, 0])?;
            self.pump(Duration::from_millis(500))?;
            while let Some(report) = self.backlog.pop_front() {
                self.on_unsolicited(&report);
            }
            if self.devices.is_empty() {
                for idx in 1..=6 {
                    if self.ping(idx)? {
                        self.devices.insert(idx, Slot { due: Some(Instant::now()), ..Slot::default() });
                    }
                }
            }
        } else {
            self.direct = true;
            if self.ping(RECEIVER_IDX)? {
                self.devices.insert(RECEIVER_IDX, Slot { due: Some(Instant::now()), ..Slot::default() });
            } else {
                // Asleep or slow to boot: keep trying in the background.
                self.devices.insert(RECEIVER_IDX, Slot { due: Some(Instant::now() + self.cfg.retry), ..Slot::default() });
            }
        }
        Ok(())
    }

    fn on_unsolicited(&mut self, r: &[u8]) {
        if r.len() < SHORT_LEN {
            return;
        }
        let idx = r[1];

        // HID++ 1.0 device connection notification from a receiver.
        if !self.direct && r[2] == 0x41 && (1..=6).contains(&idx) {
            let link_up = r[4] & 0x40 == 0;
            log!("[{}] device {} link {}", self.tag, idx, if link_up { "up" } else { "down" });
            let slot = self.devices.entry(idx).or_default();
            slot.fails = 0;
            if link_up {
                slot.due = Some(Instant::now() + self.cfg.settle);
            } else {
                slot.due = None;
                if let Some(info) = &slot.info {
                    (self.sink)(Event::Offline { id: info.id.clone() });
                }
            }
            return;
        }

        // Battery broadcast event (function 0, software id 0).
        let key = if self.direct { self.devices.keys().next().copied() } else { Some(idx) };
        let Some(slot) = key.and_then(|k| self.devices.get_mut(&k)) else { return };
        let Some(info) = &slot.info else { return };
        let Some(feature) = info.battery else { return };
        if r[2] == feature.index() && r[3] == 0x00 {
            if let Some(battery) = feature.parse(&r[4..]) {
                slot.fails = 0;
                slot.due = Some(Instant::now() + self.cfg.poll);
                (self.sink)(Event::Update { id: info.id.clone(), battery });
            }
        }
    }

    // ------------------------------------------------------------ devices

    fn service(&mut self, idx: u8) -> io::Result<()> {
        let known = self.devices.get(&idx).and_then(|s| s.info.as_ref()).map(|i| i.battery);
        let ok = match known {
            None => match self.init_device(idx)? {
                Some(info) => {
                    let has_battery = info.battery.is_some();
                    let slot = self.devices.entry(idx).or_default();
                    slot.info = Some(info);
                    slot.fails = 0;
                    // Read the battery right away, or go dormant.
                    slot.due = has_battery.then(Instant::now);
                    return Ok(());
                }
                None => false,
            },
            Some(None) => {
                self.devices.entry(idx).or_default().due = None;
                return Ok(());
            }
            Some(Some(feature)) => match self.call(idx, feature.index(), feature.getter(), &[])? {
                Some(params) => match feature.parse(&params) {
                    Some(battery) => {
                        let id = self.devices[&idx].info.as_ref().map(|i| i.id.clone()).unwrap_or_default();
                        (self.sink)(Event::Update { id, battery });
                        true
                    }
                    None => false,
                },
                None => false,
            },
        };

        let slot = self.devices.entry(idx).or_default();
        if ok {
            slot.fails = 0;
            slot.due = Some(Instant::now() + self.cfg.poll);
        } else {
            // Exponential back-off, capped: the device is most likely
            // asleep and will announce itself when it wakes up.
            let factor = 1u32 << slot.fails.min(5);
            let cap = self.cfg.poll.clamp(self.cfg.retry, Duration::from_secs(120).max(self.cfg.retry));
            slot.due = Some(Instant::now() + (self.cfg.retry * factor).min(cap));
            slot.fails += 1;
        }
        Ok(())
    }

    fn feature_index(&mut self, idx: u8, feature: u16) -> io::Result<Option<u8>> {
        let [hi, lo] = feature.to_be_bytes();
        Ok(self.call(idx, FEATURE_ROOT, 0, &[hi, lo])?.map(|p| p[0]).filter(|&i| i != 0))
    }

    /// Identifies a device. `Ok(None)` = not reachable yet.
    fn init_device(&mut self, idx: u8) -> io::Result<Option<Info>> {
        if !self.ping(idx)? && !self.ping(idx)? {
            return Ok(None);
        }

        // -- name & type (0x0005) --
        let Some(f_name) = self.feature_index(idx, FEAT_DEVICE_NAME)? else {
            // Not a HID++ 2.0 peripheral we can describe: ignore it.
            return Ok(self.ignored(idx));
        };
        let Some(count) = self.call(idx, f_name, 0, &[])? else { return Ok(None) };
        let total = count[0] as usize;
        let mut raw = Vec::with_capacity(total);
        while raw.len() < total {
            let Some(chunk) = self.call(idx, f_name, 1, &[raw.len() as u8])? else { return Ok(None) };
            let take = chunk.len().min(total - raw.len());
            if take == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..take]);
        }
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let name = String::from_utf8_lossy(&raw[..end]).trim().to_string();
        if name.is_empty() {
            return Ok(None);
        }
        if self.cfg.disabled.iter().any(|tag| !tag.is_empty() && name.contains(tag.as_str())) {
            log!("[{}] {} is disabled in the settings", self.tag, name);
            return Ok(self.ignored(idx));
        }
        let kind = match self.call(idx, f_name, 2, &[])? {
            Some(p) => DeviceKind::from_hidpp(p[0]),
            None => return Ok(None),
        };

        // -- stable identifier (0x0003) --
        let mut id = None;
        if let Some(f_info) = self.feature_index(idx, FEAT_DEVICE_INFO)? {
            if let Some(p) = self.call(idx, f_info, 0, &[])? {
                if p.len() >= 15 {
                    if p[14] & 0x01 != 0 {
                        if let Some(serial) = self.call(idx, f_info, 2, &[])? {
                            if serial.len() >= 11 {
                                id = Some(hex_upper(&serial[..11]));
                            }
                        }
                    }
                    if id.is_none() {
                        id = Some(format!("{}-{}", hex_upper(&p[1..5]), hex_upper(&p[7..12])));
                    }
                }
            }
        }
        let id = id.unwrap_or_else(|| format!("{:08X}", fnv1a(name.as_bytes())));

        // -- battery feature --
        let battery = if let Some(i) = self.feature_index(idx, FEAT_UNIFIED_BATTERY)? {
            Some(BatteryFeature::Unified(i))
        } else if let Some(i) = self.feature_index(idx, FEAT_BATTERY_LEVEL)? {
            Some(BatteryFeature::Level(i))
        } else {
            self.feature_index(idx, FEAT_BATTERY_VOLTAGE)?.map(BatteryFeature::Voltage)
        };

        log!("[{}] device {}: {:?} id={} battery={:?}", self.tag, idx, name, id, battery);
        (self.sink)(Event::Init { id: id.clone(), name, kind, source: Source::Native, has_battery: battery.is_some() });
        Ok(Some(Info { id, battery }))
    }

    /// Marks a slot as permanently uninteresting.
    fn ignored(&mut self, idx: u8) -> Option<Info> {
        self.devices.remove(&idx);
        self.devices.insert(idx, Slot { info: None, due: None, fails: 0 });
        None
    }
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811C_9DC5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn voltage_lookup_matches_reference_table() {
        assert_eq!(percent_from_millivolts(4200), 100);
        assert_eq!(percent_from_millivolts(4187), 100);
        assert_eq!(percent_from_millivolts(4186), 99);
        assert_eq!(percent_from_millivolts(3812), 50);
        assert_eq!(percent_from_millivolts(3811), 49);
        assert_eq!(percent_from_millivolts(3538), 1);
        assert_eq!(percent_from_millivolts(3537), 0);
        assert_eq!(percent_from_millivolts(0), 0);
        // Same result as the linear scan of the original implementation.
        for mv in 3400..4300u16 {
            let reference = MV_LUT.iter().position(|&v| mv > v).map_or(0, |i| 100 - i) as u8;
            assert_eq!(percent_from_millivolts(mv), reference, "{mv} mV");
        }
    }

    #[test]
    fn battery_payloads() {
        let b = parse_level(&[72, 60, 0]).unwrap();
        assert_eq!((b.percent, b.status), (72.0, Status::Discharging));
        assert_eq!(parse_level(&[100, 0, 3]).unwrap().status, Status::Full);
        assert_eq!(parse_level(&[30, 0, 4]).unwrap().status, Status::Charging);

        let b = parse_unified(&[55, 4, 1, 1]).unwrap();
        assert_eq!((b.percent, b.status), (55.0, Status::Charging));
        assert_eq!(parse_unified(&[0, 2, 0, 0]).unwrap().percent, 20.0);
        assert_eq!(parse_unified(&[0, 8, 3, 1]).unwrap().status, Status::Full);

        let b = parse_voltage(&[0x0F, 0x50, 0x00]).unwrap(); // 3920 mV
        assert_eq!((b.millivolts, b.status), (Some(3920), Status::Discharging));
        assert_eq!(b.percent, 69.0);
        assert_eq!(parse_voltage(&[0x10, 0x00, 0x80]).unwrap().status, Status::Charging);
        assert_eq!(parse_voltage(&[0x10, 0x00, 0x81]).unwrap().status, Status::Full);
        assert!(parse_voltage(&[1, 2]).is_none());
    }

    /// Simulated endpoint: optionally a receiver with one mouse on index 1,
    /// or the same mouse connected directly (long reports only).
    struct Fake {
        receiver: bool,
        asleep: bool,
        out: VecDeque<Vec<u8>>,
        writes: usize,
        idle_reads: u32,
        battery_requests: u32,
    }

    const NAME: &[u8] = b"G Pro Wireless Gaming Mouse";

    impl Fake {
        fn new(receiver: bool) -> Self {
            Self { receiver, asleep: false, out: VecDeque::new(), writes: 0, idle_reads: 0, battery_requests: 0 }
        }

        fn reply(&mut self, req: &[u8], params: &[u8]) {
            let mut r = vec![0u8; LONG_LEN];
            r[0] = REPORT_LONG;
            r[1..4].copy_from_slice(&req[1..4]);
            r[4..4 + params.len()].copy_from_slice(params);
            self.out.push_back(r);
        }

        fn error10(&mut self, req: &[u8], code: u8) {
            self.out.push_back(vec![REPORT_SHORT, req[1], 0x8F, req[2], req[3], code, 0]);
        }

        fn device(&mut self, req: &[u8]) {
            let (feature, function) = (req[2], req[3] >> 4);
            match (feature, function) {
                (0, 0) => {
                    let index = match u16::from_be_bytes([req[4], req[5]]) {
                        0x0005 => 2,
                        0x0003 => 3,
                        0x1001 => 6,
                        _ => 0,
                    };
                    self.reply(req, &[index, 0, 0]);
                }
                (0, 1) => self.reply(req, &[4, 2, req[6]]),
                (2, 0) => self.reply(req, &[NAME.len() as u8]),
                (2, 1) => {
                    let off = req[4] as usize;
                    let end = (off + 16).min(NAME.len());
                    self.reply(req, &NAME[off..end]);
                }
                (2, 2) => self.reply(req, &[3]),
                (3, 0) => {
                    let mut p = [0u8; 16];
                    p[1..5].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
                    p[7..12].copy_from_slice(&[1, 2, 3, 4, 5]);
                    self.reply(req, &p);
                }
                (6, 0) => {
                    self.battery_requests += 1;
                    self.reply(req, &[0x0F, 0x50, 0x00]);
                }
                _ => self.out.push_back(vec![REPORT_LONG, req[1], 0xFF, req[2], req[3], 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            }
        }
    }

    impl Transport for Fake {
        fn has_short(&self) -> bool {
            self.receiver
        }

        fn write(&mut self, req: &[u8]) -> io::Result<()> {
            self.writes += 1;
            assert_eq!(req.len(), if req[0] == REPORT_SHORT { SHORT_LEN } else { LONG_LEN });
            if !self.receiver {
                assert_eq!(req[0], REPORT_LONG, "long-only endpoint must not get short reports");
                if req[2] == 0x81 || req[2] == 0x80 {
                    return Ok(()); // not a receiver: no answer to register access
                }
                self.device(req);
                return Ok(());
            }
            match (req[1], req[2], req[3]) {
                (0xFF, 0x81, 0x02) => self.out.push_back(vec![0x10, 0xFF, 0x81, 0x02, 0, 1, 0]),
                (0xFF, 0x81, 0x00) => self.out.push_back(vec![0x10, 0xFF, 0x81, 0x00, 0, 0, 0]),
                (0xFF, 0x80, 0x00) => {
                    assert_eq!(&req[4..7], &[0, 0x09, 0]);
                    self.out.push_back(vec![0x10, 0xFF, 0x80, 0x00, 0, 0, 0]);
                }
                (0xFF, 0x80, 0x02) => {
                    self.out.push_back(vec![0x10, 0xFF, 0x80, 0x02, 0, 0, 0]);
                    let flags = if self.asleep { 0x40 } else { 0x00 };
                    self.out.push_back(vec![0x10, 0x01, 0x41, 0x04, flags | 0x02, 0x88, 0x40]);
                }
                (1, _, _) if self.asleep => self.error10(req, 0x09),
                (1, _, _) => self.device(req),
                _ => self.error10(req, 0x01),
            }
            Ok(())
        }

        fn read(&mut self, timeout: Duration) -> io::Result<Incoming> {
            if let Some(r) = self.out.pop_front() {
                return Ok(Incoming::Report(r));
            }
            self.idle_reads += 1;
            // Once the session has gone quiet, unplug the endpoint.
            if timeout > Duration::from_millis(500) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "unplugged"));
            }
            std::thread::sleep(timeout.min(Duration::from_millis(1)));
            Ok(Incoming::Timeout)
        }
    }

    fn settings() -> Settings {
        Settings {
            retry: Duration::from_secs(10),
            poll: Duration::from_secs(600),
            disabled: vec![],
            settle: Duration::from_millis(20),
        }
    }

    fn run(fake: Fake, cfg: Settings) -> Vec<Event> {
        let (tx, rx) = mpsc::channel();
        let (_cmd_tx, cmd_rx) = mpsc::channel();
        Session::new(fake, cfg, cmd_rx, move |e| tx.send(e).unwrap(), "test".into()).run();
        rx.try_iter().collect()
    }

    fn assert_full_flow(events: &[Event]) {
        assert_eq!(events.len(), 3, "{events:?}");
        match &events[0] {
            Event::Init { id, name, kind, source, has_battery } => {
                assert_eq!(id, "DEADBEEF-0102030405");
                assert_eq!(name, "G Pro Wireless Gaming Mouse");
                assert_eq!((*kind, *source, *has_battery), (DeviceKind::Mouse, Source::Native, true));
            }
            e => panic!("unexpected {e:?}"),
        }
        match &events[1] {
            Event::Update { id, battery } => {
                assert_eq!(id, "DEADBEEF-0102030405");
                assert_eq!(battery.millivolts, Some(3920));
                assert_eq!(battery.status, Status::Discharging);
            }
            e => panic!("unexpected {e:?}"),
        }
        assert!(matches!(&events[2], Event::Offline { .. }));
    }

    #[test]
    fn receiver_with_one_mouse() {
        assert_full_flow(&run(Fake::new(true), settings()));
    }

    #[test]
    fn direct_long_only_device() {
        assert_full_flow(&run(Fake::new(false), settings()));
    }

    #[test]
    fn sleeping_device_is_left_alone() {
        let mut fake = Fake::new(true);
        fake.asleep = true;
        // Link is down: nothing must be published and no request sent to it.
        assert!(run(fake, settings()).is_empty());
    }

    #[test]
    fn disabled_device_is_ignored() {
        let mut cfg = settings();
        cfg.disabled = vec!["G Pro".into()];
        assert!(run(Fake::new(true), cfg).is_empty());
    }

    #[test]
    fn broadcast_event_updates_battery() {
        let (tx, rx) = mpsc::channel();
        let (_cmd_tx, cmd_rx) = mpsc::channel();
        let mut s = Session::new(Fake::new(true), settings(), cmd_rx, move |e| tx.send(e).unwrap(), "t".into());
        s.devices.insert(1, Slot { info: Some(Info { id: "X".into(), battery: Some(BatteryFeature::Unified(7)) }), due: None, fails: 3 });
        let mut event = vec![0u8; LONG_LEN];
        event[..7].copy_from_slice(&[0x11, 0x01, 0x07, 0x00, 42, 4, 1]);
        s.on_unsolicited(&event);
        // Answers meant for other software (sw id != 0) are not events.
        event[3] = 0x15;
        event[4] = 99;
        s.on_unsolicited(&event);
        // Link down notification.
        s.on_unsolicited(&[0x10, 0x01, 0x41, 0x04, 0x42, 0x88, 0x40]);
        let events: Vec<Event> = rx.try_iter().collect();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(&events[0], Event::Update { id, battery } if id == "X" && battery.percent == 42.0 && battery.status == Status::Charging));
        assert!(matches!(&events[1], Event::Offline { id } if id == "X"));
        assert!(s.devices[&1].due.is_none());
    }
}
