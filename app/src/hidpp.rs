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
//!
//! On top of the battery it also handles, on explicit request only:
//! receiver description and pairing / unpairing (Unifying-style register
//! 0xB2, Bolt registers 0xC0 / 0xC1), and a few hardware settings (DPI,
//! report rate, keyboard backlight, SmartShift). Static device details
//! (firmware, serial, Easy-Switch channel) are read when a device is first
//! seen and on every reconnection, never polled.

use crate::log;
use crate::model::{Battery, DeviceInfo, DeviceKind, Event, Source, Status};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
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
const FEAT_CHANGE_HOST: u16 = 0x1814;
const FEAT_BACKLIGHT2: u16 = 0x1982;
const FEAT_SMART_SHIFT: u16 = 0x2110;
const FEAT_SMART_SHIFT_ENHANCED: u16 = 0x2111;
const FEAT_ADJUSTABLE_DPI: u16 = 0x2201;
const FEAT_REPORT_RATE: u16 = 0x8060;

// HID++ 1.0 register access sub ids.
const SET_SHORT: u8 = 0x80;
const GET_SHORT: u8 = 0x81;
const SET_LONG: u8 = 0x82;
const GET_LONG: u8 = 0x83;

// Receiver registers.
const REG_PAIRING: u8 = 0xB2;
const REG_INFO: u8 = 0xB5;
const REG_BOLT_DISCOVERY: u8 = 0xC0;
const REG_BOLT_PAIRING: u8 = 0xC1;
const REG_BOLT_UNIQUE_ID: u8 = 0xFB;
const REG_FIRMWARE: u8 = 0xF1;

// Receiver notifications.
const NOTIF_UNPAIRED: u8 = 0x40;
const NOTIF_CONNECTION: u8 = 0x41;
const NOTIF_PAIRING_LOCK: u8 = 0x4A;
const NOTIF_PASSKEY_REQUEST: u8 = 0x4D;
const NOTIF_PASSKEY_PRESSED: u8 = 0x4E;
const NOTIF_DEVICE_DISCOVERY: u8 = 0x4F;
const NOTIF_DISCOVERY_STATUS: u8 = 0x53;
const NOTIF_PAIRING_STATUS: u8 = 0x54;

/// How long the receiver keeps its pairing lock / discovery open (seconds).
const PAIRING_WINDOW: u8 = 30;
/// Our own safety net on top of the receiver's timeout.
const PAIRING_DEADLINE: Duration = Duration::from_secs(PAIRING_WINDOW as u64 + 10);
/// After the lock closed without error, how long to wait for the
/// connection notification of the new device.
const PAIRING_GRACE: Duration = Duration::from_secs(3);

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

// ------------------------------------------------------------ public API

/// Family of a receiver, from its USB product id (same table as Solaar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverKind {
    Unifying,
    Bolt,
    Lightspeed,
    Nano,
    /// A receiver we do not know: treated like a Unifying one for reading,
    /// but neither pairing nor unpairing is offered.
    Unknown,
    /// Not a receiver: a device connected by cable or Bluetooth.
    Direct,
}

impl ReceiverKind {
    pub fn from_product_id(pid: u16) -> Self {
        match pid {
            0xC548 => Self::Bolt,
            0xC52B | 0xC532 => Self::Unifying,
            0xC539 | 0xC53A | 0xC53D | 0xC53F | 0xC541 | 0xC545 | 0xC547 | 0xC54D => Self::Lightspeed,
            0xC518 | 0xC51A | 0xC51B | 0xC521 | 0xC525 | 0xC526 | 0xC52E | 0xC52F | 0xC531 | 0xC534 | 0xC535
            | 0xC537 => Self::Nano,
            _ => Self::Unknown,
        }
    }

    /// Product name (not translated: these are Logitech product names).
    pub fn name(self) -> &'static str {
        match self {
            Self::Unifying => "Unifying Receiver",
            Self::Bolt => "Bolt Receiver",
            Self::Lightspeed => "Lightspeed Receiver",
            Self::Nano => "Nano Receiver",
            Self::Unknown => "Logitech Receiver",
            Self::Direct => "USB / Bluetooth",
        }
    }

    /// Opening the pairing lock is supported.
    pub fn can_pair(self) -> bool {
        matches!(self, Self::Unifying | Self::Bolt | Self::Lightspeed | Self::Nano)
    }

    /// Nano and Lightspeed receivers do not unpair: a new pairing replaces
    /// the old one (Solaar's `may_unpair = False`).
    pub fn can_unpair(self) -> bool {
        matches!(self, Self::Unifying | Self::Bolt)
    }

    fn default_slots(self) -> u8 {
        match self {
            Self::Unifying | Self::Bolt | Self::Unknown => 6,
            Self::Direct | Self::Lightspeed | Self::Nano => 1,
        }
    }
}

/// One paired slot of a receiver (or the device of a direct connection).
#[derive(Clone, Debug, PartialEq)]
pub struct PairedDevice {
    /// Device index on the receiver, 1-based (0xFF for a direct connection).
    pub slot: u8,
    /// Device id as published in the hub, once the device has been
    /// identified (it may be asleep since Batly started).
    pub id: Option<String>,
    /// Device name, or the receiver's short code name when not identified.
    pub name: String,
    pub kind: Option<DeviceKind>,
    pub online: bool,
    /// Wireless product id from the receiver's pairing register.
    pub wpid: Option<u16>,
    /// Firmware, serial, Easy-Switch channel… as published in the hub.
    pub info: DeviceInfo,
}

/// Snapshot of a receiver and its paired devices.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverInfo {
    /// Stable key of the endpoint, filled in by the platform layer.
    pub id: String,
    pub name: String,
    pub kind: ReceiverKind,
    pub product_id: u16,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    /// Number of pairing slots.
    pub slots: u8,
    pub devices: Vec<PairedDevice>,
    /// A pairing is in progress on this receiver.
    pub pairing: bool,
}

/// Progress of a pairing, in order: `Waiting`, then for Bolt `Found` and
/// `Passkey`, and finally exactly one of `Paired`, `Failed`, `Timeout` or
/// `Cancelled`. The channel is closed afterwards (or if the receiver goes
/// away, without a final event).
#[derive(Clone, Debug, PartialEq)]
pub enum PairingEvent {
    /// The receiver is listening: switch the device on / press its
    /// Easy-Switch or connect button.
    Waiting,
    /// Bolt: a device has been discovered and is being paired.
    Found { name: String },
    /// Bolt: the user must authenticate. On a keyboard, type `code` then
    /// Enter (`keyboard == true`); on a mouse, click the sequence given by
    /// [`passkey_clicks`] then both buttons together.
    Passkey { name: String, code: String, keyboard: bool },
    Paired { slot: u8, name: String },
    Failed(PairingError),
    Timeout,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingError {
    /// The receiver cannot pair (unknown type, direct connection).
    NotSupported,
    /// A pairing is already in progress on this receiver.
    Busy,
    /// The receiver refused to open its pairing lock / discovery.
    LockFailed,
    DeviceNotSupported,
    TooManyDevices,
    /// Any other failure reported by the receiver.
    Failed,
}

/// Mouse authentication sequence of a Bolt passkey: the decimal code as a
/// 10-bit number, most significant bit first; `true` = right button.
pub fn passkey_clicks(code: &str) -> Option<Vec<bool>> {
    let n: u32 = code.trim().parse().ok()?;
    Some((0..10).rev().map(|bit| n >> bit & 1 == 1).collect())
}

/// Failure of an unpairing or settings request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionError {
    /// Not supported by this receiver / device.
    NotSupported,
    /// The device is not connected right now.
    Offline,
    /// A pairing is in progress.
    Busy,
    /// The device or receiver rejected the request or did not answer.
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DpiSettings {
    pub choices: Vec<u16>,
    pub current: u16,
    pub default: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportRateSettings {
    /// Supported report intervals, in milliseconds (1 ms = 1000 Hz).
    pub choices: Vec<u8>,
    pub current: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacklightMode {
    Off,
    /// Enabled, on a device without selectable modes.
    On,
    /// Ambient light sensor / hand proximity.
    Auto,
    /// Fixed level (`level`).
    Manual,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BacklightSettings {
    pub mode: BacklightMode,
    /// Modes the keyboard accepts.
    pub modes: Vec<BacklightMode>,
    /// Current level, meaningful in `Manual` mode.
    pub level: u8,
    /// Number of levels (0..levels); 0 when the level cannot be set.
    pub levels: u8,
}

/// Scroll wheel mode (SmartShift).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelMode {
    FreeSpin,
    /// Always ratcheted.
    Ratchet,
    /// Ratcheted, switching to free spin above this speed (1..=254).
    Smart(u8),
}

/// Hardware settings of one device; `None` = feature not present.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceSettings {
    pub dpi: Option<DpiSettings>,
    pub report_rate: Option<ReportRateSettings>,
    pub backlight: Option<BacklightSettings>,
    pub smart_shift: Option<WheelMode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Dpi(u16),
    /// Report interval in milliseconds.
    ReportRate(u8),
    Backlight { mode: BacklightMode, level: u8 },
    SmartShift(WheelMode),
}

pub type SettingsReply = Sender<Result<DeviceSettings, ActionError>>;

pub enum Command {
    Refresh,
    Stop,
    /// Describe the receiver and its paired devices.
    Describe(Sender<ReceiverInfo>),
    /// Open the pairing lock; progress is streamed on the channel.
    Pair(Sender<PairingEvent>),
    CancelPairing,
    Unpair { slot: u8, reply: Sender<Result<(), ActionError>> },
    GetSettings { device: String, reply: SettingsReply },
    /// Applies a setting, then answers with the settings read back.
    SetSetting { device: String, setting: Setting, reply: SettingsReply },
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

// ------------------------------------------------------- payload parsing

/// Feature 0x0003 getFwInfo: main application firmware only, formatted
/// like Solaar (`RQM68.01.B0010`).
fn parse_firmware(p: &[u8]) -> Option<String> {
    if p.len() < 8 || p[0] & 0x0F != 0 {
        return None;
    }
    let prefix: String = p[1..4].iter().filter(|b| b.is_ascii_alphanumeric()).map(|&b| b as char).collect();
    let build = u16::from_be_bytes([p[6], p[7]]);
    let mut version = format!("{prefix}{:02X}.{:02X}", p[4], p[5]);
    if build != 0 {
        version.push_str(&format!(".B{build:04X}"));
    }
    Some(version)
}

/// Receiver register 0xF1: `[sub-register, major, minor]` in BCD.
fn format_register_firmware(main: [u8; 3], build: Option<[u8; 3]>) -> String {
    let mut s = format!("{:02X}.{:02X}", main[1], main[2]);
    if let Some(b) = build {
        s.push_str(&format!(".B{:02X}{:02X}", b[1], b[2]));
    }
    s
}

/// Serial number of feature 0x0003 (ASCII on recent devices), else the
/// unit id in hex.
fn format_serial(serial: Option<&[u8]>, unit_id: &[u8]) -> String {
    if let Some(s) = serial {
        let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
        let s = &s[..end];
        if !s.is_empty() && s.iter().all(|b| b.is_ascii_alphanumeric()) {
            return String::from_utf8_lossy(s).into_owned();
        }
    }
    hex_upper(unit_id)
}

/// Feature 0x2201 getSensorDpiList, concatenated pages without the sensor
/// index: big-endian values ending with 0; `0xE000 | step` announces a
/// range from the previous value to the next one.
pub fn parse_dpi_list(bytes: &[u8]) -> Vec<u16> {
    let mut list: Vec<u16> = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let val = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        if val == 0 {
            break;
        }
        if val >> 13 == 0b111 {
            let step = val & 0x1FFF;
            let Some(last) = bytes.get(i + 2..i + 4).map(|b| u16::from_be_bytes([b[0], b[1]])) else { break };
            if let Some(&first) = list.last() {
                if step > 0 && last > first {
                    list.extend((first as u32 + step as u32..=last as u32).step_by(step as usize).map(|v| v as u16));
                }
            }
            i += 4;
        } else {
            list.push(val);
            i += 2;
        }
    }
    list
}

/// Feature 0x8060 getReportRateList: bit n set = (n + 1) ms supported.
fn parse_rate_flags(flags: u8) -> Vec<u8> {
    (0..8).filter(|i| flags >> i & 1 == 1).map(|i| i + 1).collect()
}

/// Feature 0x2110 / 0x2111 getRatchetControlMode.
fn parse_wheel(p: &[u8]) -> Option<WheelMode> {
    match p {
        [1, ..] => Some(WheelMode::FreeSpin),
        [_, 0xFF, ..] => Some(WheelMode::Ratchet),
        [_, t, ..] => Some(WheelMode::Smart((*t).max(1))),
        _ => None,
    }
}

/// setRatchetControlMode parameters: wheel mode, auto-disengage, default
/// auto-disengage (0 = unchanged).
fn encode_wheel(mode: WheelMode) -> [u8; 3] {
    match mode {
        WheelMode::FreeSpin => [1, 0, 0],
        WheelMode::Ratchet => [2, 0xFF, 0],
        WheelMode::Smart(t) => [2, t.clamp(1, 0xFE), 0],
    }
}

/// Feature 0x1982 getBacklightConfig (little-endian, as in Solaar).
#[derive(Clone, Copy, Debug, PartialEq)]
struct BacklightConfig {
    enabled: bool,
    options: u8,
    supported: u8,
    level: u8,
    durations: [u16; 3],
}

impl BacklightConfig {
    fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < 12 {
            return None;
        }
        let le = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        Some(Self { enabled: p[0] != 0, options: p[1], supported: p[2], level: p[5], durations: [le(6), le(8), le(10)] })
    }

    fn mode(&self) -> BacklightMode {
        if !self.enabled {
            return BacklightMode::Off;
        }
        match (self.options >> 3) & 0x03 {
            1 => BacklightMode::Auto,
            3 => BacklightMode::Manual,
            _ => BacklightMode::On,
        }
    }

    fn modes(&self) -> Vec<BacklightMode> {
        let mut modes = vec![BacklightMode::Off];
        if self.supported & 0x38 == 0 {
            modes.push(BacklightMode::On);
        }
        if self.supported & 0x08 != 0 {
            modes.push(BacklightMode::Auto);
        }
        if self.supported & 0x20 != 0 {
            modes.push(BacklightMode::Manual);
        }
        modes
    }

    /// setBacklightConfig parameters.
    fn encode(&self, mode: BacklightMode, level: u8) -> [u8; 10] {
        let bits = match mode {
            BacklightMode::Off => (self.options >> 3) & 0x03,
            BacklightMode::On => 0,
            BacklightMode::Auto => 1,
            BacklightMode::Manual => 3,
        };
        let options = (self.options & 0x07) | (bits << 3);
        let level = if bits == 3 { level } else { 0 };
        let [a, b] = self.durations[0].to_le_bytes();
        let [c, d] = self.durations[1].to_le_bytes();
        let [e, f] = self.durations[2].to_le_bytes();
        [(mode != BacklightMode::Off) as u8, options, 0xFF, level, a, b, c, d, e, f]
    }
}

/// HID++ 1.0 device type (pairing registers and notifications).
fn kind_from_hidpp10(code: u8) -> Option<DeviceKind> {
    match code & 0x0F {
        0x01 | 0x03 => Some(DeviceKind::Keyboard),
        0x02 | 0x08 | 0x09 => Some(DeviceKind::Mouse),
        0x0B | 0x0C => Some(DeviceKind::Gamepad),
        0x0D => Some(DeviceKind::Headset),
        0x00 => None,
        _ => Some(DeviceKind::Other),
    }
}

// ---------------------------------------------------------------- session

struct Info {
    id: String,
    name: String,
    kind: DeviceKind,
    battery: Option<BatteryFeature>,
    /// Feature indexes resolved so far (only positive answers are cached).
    features: BTreeMap<u16, u8>,
    /// Last details published with `Event::Info`.
    details: DeviceInfo,
}

impl Info {
    fn new(id: String, name: String, kind: DeviceKind, battery: Option<BatteryFeature>) -> Self {
        Self { id, name, kind, battery, features: BTreeMap::new(), details: DeviceInfo::default() }
    }
}

#[derive(Default)]
struct Slot {
    info: Option<Info>,
    /// Next time this device needs attention; `None` = wait for an event.
    due: Option<Instant>,
    fails: u32,
    /// Last known link state.
    online: bool,
    /// Re-read the static details (reconnection).
    reread: bool,
}

/// What a receiver's pairing registers say about one slot.
#[derive(Clone, Debug, PartialEq)]
struct PairingRecord {
    slot: u8,
    wpid: Option<u16>,
    kind: Option<DeviceKind>,
    codename: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ReceiverDetails {
    serial: Option<String>,
    firmware: Option<String>,
    slots: u8,
}

struct Pairing {
    events: Sender<PairingEvent>,
    bolt: bool,
    deadline: Instant,
    /// Slots that were already announced before the lock opened.
    before: Vec<u8>,
    /// Unifying: device connected while the lock was open.
    new_slot: Option<u8>,
    /// Unifying: the lock closed without error, waiting for the device.
    closed: bool,
    /// Success established; the name is looked up before reporting.
    paired_slot: Option<u8>,
    // Bolt discovery.
    counter: Option<u16>,
    address: Option<[u8; 6]>,
    authentication: u8,
    device_kind: u8,
    name: Option<String>,
    pair_sent: bool,
}

pub struct Session<T: Transport> {
    transport: T,
    cfg: Settings,
    sink: Box<dyn Fn(Event) + Send>,
    commands: Receiver<Command>,
    devices: BTreeMap<u8, Slot>,
    backlog: VecDeque<Vec<u8>>,
    /// Commands that need I/O, run from the event loop.
    pending: VecDeque<Command>,
    /// Directly connected device (no receiver in between).
    direct: bool,
    product_id: u16,
    ping_seq: u8,
    stopping: bool,
    tag: String,
    pairing: Option<Pairing>,
    receiver_details: Option<ReceiverDetails>,
    /// Pairing registers, read on demand and dropped when they may change.
    paired: Option<Vec<PairingRecord>>,
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
            pending: VecDeque::new(),
            direct: false,
            product_id: 0,
            ping_seq: 0x55,
            stopping: false,
            tag,
            pairing: None,
            receiver_details: None,
            paired: None,
        }
    }

    /// USB product id of the endpoint, used to tell receiver families apart.
    pub fn with_product_id(mut self, product_id: u16) -> Self {
        self.product_id = product_id;
        self
    }

    fn receiver_kind(&self) -> ReceiverKind {
        if self.direct {
            ReceiverKind::Direct
        } else {
            ReceiverKind::from_product_id(self.product_id)
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
            if let Some(command) = self.pending.pop_front() {
                self.execute(command)?;
                continue;
            }
            self.pairing_step()?;

            let now = Instant::now();
            let due: Vec<u8> =
                self.devices.iter().filter(|(_, s)| s.due.is_some_and(|d| d <= now)).map(|(&i, _)| i).collect();
            if !due.is_empty() {
                for idx in due {
                    self.service(idx)?;
                }
                continue;
            }

            let next = self.devices.values().filter_map(|s| s.due).chain(self.pairing.as_ref().map(|p| p.deadline)).min();
            let wait = next.map_or(IDLE, |d| d.saturating_duration_since(now));
            if let Incoming::Report(report) = self.transport.read(wait)? {
                self.on_unsolicited(&report);
            }
        }
    }

    /// Returns true when the session must stop. Commands that need I/O
    /// are queued: this is also called from inside requests.
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
                Ok(command) => self.pending.push_back(command),
                Err(TryRecvError::Empty) => break,
            }
            if self.stopping {
                break;
            }
        }
        self.stopping
    }

    fn execute(&mut self, command: Command) -> io::Result<()> {
        match command {
            Command::Refresh | Command::Stop => {}
            Command::Describe(reply) => {
                let info = self.describe()?;
                let _ = reply.send(info);
            }
            Command::Pair(events) => self.start_pairing(events)?,
            Command::CancelPairing => self.cancel_pairing()?,
            Command::Unpair { slot, reply } => {
                let result = self.unpair(slot)?;
                let _ = reply.send(result);
            }
            Command::GetSettings { device, reply } => {
                let result = match self.settings_target(&device) {
                    Ok(idx) => self.read_settings(idx)?,
                    Err(e) => Err(e),
                };
                let _ = reply.send(result);
            }
            Command::SetSetting { device, setting, reply } => {
                let result = match self.settings_target(&device) {
                    Ok(idx) => match self.write_setting(idx, setting)? {
                        true => self.read_settings(idx)?,
                        false => Err(ActionError::Failed),
                    },
                    Err(e) => Err(e),
                };
                let _ = reply.send(result);
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- I/O

    fn send(&mut self, idx: u8, b2: u8, b3: u8, params: &[u8], long: bool) -> io::Result<()> {
        let mut buf = [0u8; LONG_LEN];
        let len = if !long && params.len() <= 3 && self.transport.has_short() {
            buf[0] = REPORT_SHORT;
            SHORT_LEN
        } else {
            buf[0] = REPORT_LONG;
            LONG_LEN
        };
        let n = params.len().min(LONG_LEN - 4);
        buf[1] = idx;
        buf[2] = b2;
        buf[3] = b3;
        buf[4..4 + n].copy_from_slice(&params[..n]);
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
        self.transact_ex(idx, b2, b3, params, timeout, false, None)
    }

    /// `transact` with a forced long report and / or a check that the
    /// first answer parameter echoes the first request parameter.
    #[allow(clippy::too_many_arguments)]
    fn transact_ex(
        &mut self,
        idx: u8,
        b2: u8,
        b3: u8,
        params: &[u8],
        timeout: Duration,
        long: bool,
        echo: Option<u8>,
    ) -> io::Result<Option<Vec<u8>>> {
        self.send(idx, b2, b3, params, long)?;
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
                    // Only abort for a stop request; anything else can wait.
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
                if report[2] == b2 && report[3] == b3 && echo.is_none_or(|e| report[4] == e) {
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

    /// Short register read whose answer must echo the sub-register.
    fn read_short_register(&mut self, address: u8, sub: u8) -> io::Result<Option<[u8; 3]>> {
        let reply = self.transact_ex(RECEIVER_IDX, GET_SHORT, address, &[sub, 0, 0], REQUEST_TIMEOUT, false, Some(sub))?;
        Ok(reply.map(|r| [r[4], r[5], r[6]]))
    }

    /// Long register read; returns the 16 data bytes (starting with the
    /// echoed sub-register when `echo` is set).
    fn read_long_register(&mut self, address: u8, params: [u8; 3], echo: bool) -> io::Result<Option<Vec<u8>>> {
        let echo = echo.then_some(params[0]);
        let reply = self.transact_ex(RECEIVER_IDX, GET_LONG, address, &params, REQUEST_TIMEOUT * 2, false, echo)?;
        Ok(reply.filter(|r| r.len() >= LONG_LEN).map(|r| r[4..].to_vec()))
    }

    /// Long register write (always sent as a long report, like Solaar).
    fn write_long_register(&mut self, address: u8, params: &[u8]) -> io::Result<bool> {
        let reply = self.transact_ex(RECEIVER_IDX, SET_LONG, address, params, REQUEST_TIMEOUT * 2, true, None)?;
        Ok(reply.is_some())
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
        let is_receiver = self.register(GET_SHORT, 0x02, [0; 3])?.is_some();
        log!("[{}] receiver: {}", self.tag, is_receiver);

        if is_receiver {
            // Enable "wireless" + "software present" notifications, keeping
            // whatever other flags are already set.
            if let Some(flags) = self.register(GET_SHORT, 0x00, [0; 3])? {
                if flags[1] & 0x09 != 0x09 {
                    self.register(SET_SHORT, 0x00, [flags[0], flags[1] | 0x09, flags[2]])?;
                }
            }
            // Ask the receiver to announce every paired device (0x41).
            self.register(SET_SHORT, 0x02, [0x02, 0, 0])?;
            self.pump(Duration::from_millis(500))?;
            while let Some(report) = self.backlog.pop_front() {
                self.on_unsolicited(&report);
            }
            if self.devices.is_empty() {
                for idx in 1..=6 {
                    if self.ping(idx)? {
                        self.devices.insert(idx, Slot { due: Some(Instant::now()), online: true, ..Slot::default() });
                    }
                }
            }
        } else {
            self.direct = true;
            if self.ping(RECEIVER_IDX)? {
                self.devices.insert(RECEIVER_IDX, Slot { due: Some(Instant::now()), online: true, ..Slot::default() });
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

        // Receiver notifications about pairing.
        if !self.direct && idx == RECEIVER_IDX {
            if matches!(
                r[2],
                NOTIF_PAIRING_LOCK
                    | NOTIF_PASSKEY_REQUEST
                    | NOTIF_PASSKEY_PRESSED
                    | NOTIF_DEVICE_DISCOVERY
                    | NOTIF_DISCOVERY_STATUS
                    | NOTIF_PAIRING_STATUS
            ) {
                self.on_pairing_notification(r);
            }
            return;
        }

        // Device unpaired (by us or by other software).
        if !self.direct && r[2] == NOTIF_UNPAIRED && r[3] == 0x02 && (1..=6).contains(&idx) {
            log!("[{}] device {} unpaired", self.tag, idx);
            self.forget(idx);
            return;
        }

        // HID++ 1.0 device connection notification from a receiver.
        if !self.direct && r[2] == NOTIF_CONNECTION && (1..=6).contains(&idx) {
            let link_up = r[4] & 0x40 == 0;
            log!("[{}] device {} link {}", self.tag, idx, if link_up { "up" } else { "down" });
            if let Some(p) = self.pairing.as_mut().filter(|p| !p.bolt && link_up) {
                if p.closed {
                    p.paired_slot = Some(idx);
                } else if p.new_slot.is_none() || !p.before.contains(&idx) {
                    p.new_slot = Some(idx);
                }
            }
            if !self.devices.contains_key(&idx) {
                self.paired = None;
            }
            let slot = self.devices.entry(idx).or_default();
            slot.fails = 0;
            slot.online = link_up;
            if link_up {
                slot.due = Some(Instant::now() + self.cfg.settle);
                slot.reread = slot.info.is_some();
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
                slot.online = true;
                slot.due = Some(Instant::now() + self.cfg.poll);
                (self.sink)(Event::Update { id: info.id.clone(), battery });
            }
        }
    }

    /// A slot was unpaired: the device is gone for good.
    fn forget(&mut self, idx: u8) {
        self.paired = None;
        if let Some(slot) = self.devices.remove(&idx) {
            if let Some(info) = slot.info {
                (self.sink)(Event::Remove { id: info.id });
            }
        }
    }

    // ------------------------------------------------------------ devices

    fn service(&mut self, idx: u8) -> io::Result<()> {
        if self.devices.get(&idx).is_some_and(|s| s.reread && s.info.is_some()) {
            self.devices.entry(idx).or_default().reread = false;
            self.read_details(idx)?;
        }
        let known = self.devices.get(&idx).and_then(|s| s.info.as_ref()).map(|i| i.battery);
        let ok = match known {
            None => match self.init_device(idx)? {
                Some(info) => {
                    let has_battery = info.battery.is_some();
                    let slot = self.devices.entry(idx).or_default();
                    slot.info = Some(info);
                    slot.fails = 0;
                    slot.online = true;
                    slot.reread = false;
                    self.read_details(idx)?;
                    // Read the battery right away, or go dormant.
                    self.devices.entry(idx).or_default().due = has_battery.then(Instant::now);
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
            slot.online = true;
            slot.due = Some(Instant::now() + self.cfg.poll);
        } else {
            // Exponential back-off, capped: the device is most likely
            // asleep and will announce itself when it wakes up.
            let factor = 1u32 << slot.fails.min(5);
            let cap = self.cfg.poll.clamp(self.cfg.retry, Duration::from_secs(120).max(self.cfg.retry));
            slot.due = Some(Instant::now() + (self.cfg.retry * factor).min(cap));
            slot.fails += 1;
            if self.direct {
                slot.online = false;
            }
        }
        Ok(())
    }

    fn feature_index(&mut self, idx: u8, feature: u16) -> io::Result<Option<u8>> {
        let [hi, lo] = feature.to_be_bytes();
        Ok(self.call(idx, FEATURE_ROOT, 0, &[hi, lo])?.map(|p| p[0]).filter(|&i| i != 0))
    }

    /// Feature index through the per-device cache (positive answers only:
    /// a timeout must not be mistaken for a missing feature).
    fn feature(&mut self, idx: u8, feature: u16) -> io::Result<Option<u8>> {
        let cached = self.devices.get(&idx).and_then(|s| s.info.as_ref()).and_then(|i| i.features.get(&feature).copied());
        if cached.is_some() {
            return Ok(cached);
        }
        let found = self.feature_index(idx, feature)?;
        if let (Some(i), Some(info)) = (found, self.devices.get_mut(&idx).and_then(|s| s.info.as_mut())) {
            info.features.insert(feature, i);
        }
        Ok(found)
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
        let mut serial_text = None;
        let f_info = self.feature_index(idx, FEAT_DEVICE_INFO)?;
        if let Some(f_info) = f_info {
            if let Some(p) = self.call(idx, f_info, 0, &[])? {
                if p.len() >= 15 {
                    let mut serial_raw = None;
                    if p[14] & 0x01 != 0 {
                        if let Some(serial) = self.call(idx, f_info, 2, &[])? {
                            if serial.len() >= 11 {
                                id = Some(hex_upper(&serial[..11]));
                                serial_raw = Some(serial[..12.min(serial.len())].to_vec());
                            }
                        }
                    }
                    if id.is_none() {
                        id = Some(format!("{}-{}", hex_upper(&p[1..5]), hex_upper(&p[7..12])));
                    }
                    serial_text = Some(format_serial(serial_raw.as_deref(), &p[1..5]));
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
        (self.sink)(Event::Init {
            id: id.clone(),
            name: name.clone(),
            kind,
            source: Source::Native,
            has_battery: battery.is_some(),
        });
        let mut info = Info::new(id, name, kind, battery);
        info.features.insert(FEAT_DEVICE_NAME, f_name);
        if let Some(f) = f_info {
            info.features.insert(FEAT_DEVICE_INFO, f);
        }
        info.details.serial = serial_text;
        Ok(Some(info))
    }

    /// Reads and publishes the static details of an identified device:
    /// firmware, Easy-Switch channel (and the serial found at init).
    fn read_details(&mut self, idx: u8) -> io::Result<()> {
        let mut details = DeviceInfo::default();
        if let Some(f) = self.feature(idx, FEAT_DEVICE_INFO)? {
            if let Some(p) = self.call(idx, f, 0, &[])? {
                for entity in 0..p[0].min(8) {
                    let Some(fw) = self.call(idx, f, 1, &[entity])? else { break };
                    if let Some(version) = parse_firmware(&fw) {
                        details.firmware = Some(version);
                        break;
                    }
                }
            }
        }
        if let Some(f) = self.feature(idx, FEAT_CHANGE_HOST)? {
            if let Some(p) = self.call(idx, f, 0, &[])? {
                if p[0] > 0 && p[1] < p[0] {
                    details.host_channel = Some((p[1] + 1, p[0]));
                }
            }
        }
        if !self.direct {
            details.receiver = Some(self.receiver_kind().name().to_string());
        }
        let Some(info) = self.devices.get_mut(&idx).and_then(|s| s.info.as_mut()) else { return Ok(()) };
        details.serial = info.details.serial.clone();
        // Keep what a failed read could not refresh.
        details.firmware = details.firmware.or(info.details.firmware.take());
        details.host_channel = details.host_channel.or(info.details.host_channel);
        info.details = details.clone();
        let id = info.id.clone();
        if details != DeviceInfo::default() {
            (self.sink)(Event::Info { id, info: details });
        }
        Ok(())
    }

    /// Marks a slot as permanently uninteresting.
    fn ignored(&mut self, idx: u8) -> Option<Info> {
        let online = self.devices.get(&idx).is_some_and(|s| s.online);
        self.devices.insert(idx, Slot { online, ..Slot::default() });
        None
    }

    // --------------------------------------------------------- receivers

    fn receiver_details(&mut self) -> io::Result<ReceiverDetails> {
        if let Some(d) = &self.receiver_details {
            return Ok(d.clone());
        }
        let kind = self.receiver_kind();
        let mut details = ReceiverDetails { serial: None, firmware: None, slots: kind.default_slots() };
        if kind == ReceiverKind::Bolt {
            if let Some(d) = self.read_long_register(REG_BOLT_UNIQUE_ID, [0; 3], false)? {
                let end = d.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
                if end > 0 {
                    details.serial = Some(hex_upper(&d[..end]));
                }
            }
        } else if let Some(d) = self.read_long_register(REG_INFO, [0x03, 0, 0], true)? {
            details.serial = Some(hex_upper(&d[1..5]));
            if (1..=6).contains(&d[6]) {
                details.slots = d[6];
            }
        }
        if let Some(main) = self.read_short_register(REG_FIRMWARE, 0x01)? {
            let build = self.read_short_register(REG_FIRMWARE, 0x02)?;
            details.firmware = Some(format_register_firmware(main, build));
        }
        self.receiver_details = Some(details.clone());
        Ok(details)
    }

    /// Reads the receiver's pairing registers (no request reaches the
    /// devices themselves, so sleeping devices are left alone).
    fn pairing_records(&mut self, slots: u8) -> io::Result<Vec<PairingRecord>> {
        if let Some(p) = &self.paired {
            return Ok(p.clone());
        }
        let bolt = self.receiver_kind() == ReceiverKind::Bolt;
        // Register 0x02 tells how many devices are paired: stop once they
        // have all been found instead of waiting on every empty slot.
        let count = self.register(GET_SHORT, 0x02, [0; 3])?.map_or(0, |p| p[1] as usize);
        let mut records = Vec::new();
        for n in 1..=slots {
            if count > 0 && records.len() >= count {
                break;
            }
            let record = if bolt {
                let Some(d) = self.read_long_register(REG_INFO, [0x50 + n, 0, 0], true)? else { continue };
                let wpid = u16::from_be_bytes([d[3], d[2]]);
                if wpid == 0 {
                    continue;
                }
                let codename = self.read_long_register(REG_INFO, [0x60 + n, 0x01, 0], true)?.and_then(|c| {
                    let len = (c[2] as usize).min(14);
                    let name = String::from_utf8_lossy(&c[3..3 + len]).trim().to_string();
                    (!name.is_empty()).then_some(name)
                });
                PairingRecord { slot: n, wpid: Some(wpid), kind: kind_from_hidpp10(d[1]), codename }
            } else {
                let Some(d) = self.read_long_register(REG_INFO, [0x20 + n - 1, 0, 0], true)? else { continue };
                let wpid = u16::from_be_bytes([d[3], d[4]]);
                if wpid == 0 {
                    continue;
                }
                let codename = self.read_codename(n)?;
                PairingRecord { slot: n, wpid: Some(wpid), kind: kind_from_hidpp10(d[7]), codename }
            };
            records.push(record);
        }
        self.paired = Some(records.clone());
        Ok(records)
    }

    /// Unifying-style short device name (register 0xB5, 0x40 + n - 1).
    fn read_codename(&mut self, n: u8) -> io::Result<Option<String>> {
        Ok(self.read_long_register(REG_INFO, [0x40 + n - 1, 0, 0], true)?.and_then(|c| {
            let len = (c[1] as usize).min(14);
            let name = String::from_utf8_lossy(&c[2..2 + len]).trim().to_string();
            (!name.is_empty()).then_some(name)
        }))
    }

    fn describe(&mut self) -> io::Result<ReceiverInfo> {
        let kind = self.receiver_kind();
        let to_paired = |slot: u8, s: &Slot, record: Option<&PairingRecord>| PairedDevice {
            slot,
            id: s.info.as_ref().map(|i| i.id.clone()),
            name: s
                .info
                .as_ref()
                .map(|i| i.name.clone())
                .or_else(|| record.and_then(|r| r.codename.clone()))
                .unwrap_or_else(|| format!("#{slot}")),
            kind: s.info.as_ref().map(|i| i.kind).or_else(|| record.and_then(|r| r.kind)),
            online: s.online,
            wpid: record.and_then(|r| r.wpid),
            info: s.info.as_ref().map(|i| i.details.clone()).unwrap_or_default(),
        };

        if self.direct {
            let devices: Vec<PairedDevice> = self
                .devices
                .iter()
                .filter(|(_, s)| s.info.is_some())
                .map(|(&slot, s)| to_paired(slot, s, None))
                .collect();
            let name = devices.first().map_or_else(|| kind.name().to_string(), |d| d.name.clone());
            return Ok(ReceiverInfo {
                id: String::new(),
                name,
                kind,
                product_id: self.product_id,
                serial: None,
                firmware: None,
                slots: 1,
                devices,
                pairing: false,
            });
        }

        let details = self.receiver_details()?;
        let records = self.pairing_records(details.slots)?;
        let mut slots: Vec<u8> = records.iter().map(|r| r.slot).collect();
        slots.extend(self.devices.iter().filter(|(_, s)| s.info.is_some()).map(|(&i, _)| i));
        slots.sort_unstable();
        slots.dedup();
        let empty = Slot::default();
        let devices = slots
            .into_iter()
            .map(|slot| {
                let record = records.iter().find(|r| r.slot == slot);
                to_paired(slot, self.devices.get(&slot).unwrap_or(&empty), record)
            })
            .collect();
        Ok(ReceiverInfo {
            id: String::new(),
            name: kind.name().to_string(),
            kind,
            product_id: self.product_id,
            serial: details.serial,
            firmware: details.firmware,
            slots: details.slots.max(1),
            devices,
            pairing: self.pairing.is_some(),
        })
    }

    // ------------------------------------------------------------ pairing

    fn start_pairing(&mut self, events: Sender<PairingEvent>) -> io::Result<()> {
        let kind = self.receiver_kind();
        if !kind.can_pair() {
            let _ = events.send(PairingEvent::Failed(PairingError::NotSupported));
            return Ok(());
        }
        if self.pairing.is_some() {
            let _ = events.send(PairingEvent::Failed(PairingError::Busy));
            return Ok(());
        }
        let bolt = kind == ReceiverKind::Bolt;
        let opened = if bolt {
            self.register(SET_SHORT, REG_BOLT_DISCOVERY, [PAIRING_WINDOW, 0x01, 0])?
        } else {
            self.register(SET_SHORT, REG_PAIRING, [0x01, 0x00, PAIRING_WINDOW])?
        };
        if opened.is_none() {
            let _ = events.send(PairingEvent::Failed(PairingError::LockFailed));
            return Ok(());
        }
        log!("[{}] pairing started", self.tag);
        let _ = events.send(PairingEvent::Waiting);
        self.pairing = Some(Pairing {
            events,
            bolt,
            deadline: Instant::now() + PAIRING_DEADLINE,
            before: self.devices.keys().copied().collect(),
            new_slot: None,
            closed: false,
            paired_slot: None,
            counter: None,
            address: None,
            authentication: 0,
            device_kind: 0,
            name: None,
            pair_sent: false,
        });
        Ok(())
    }

    fn finish_pairing(&mut self, event: PairingEvent) {
        log!("[{}] pairing finished: {:?}", self.tag, event);
        if let Some(p) = self.pairing.take() {
            let _ = p.events.send(event);
        }
        self.paired = None;
    }

    fn on_pairing_notification(&mut self, r: &[u8]) {
        let Some(p) = self.pairing.as_mut() else { return };
        if p.paired_slot.is_some() {
            return; // already settled, the name lookup is pending
        }
        let (address, error) = (r[3], r[4]);
        let outcome = match (r[2], p.bolt) {
            (NOTIF_PAIRING_LOCK, false) => {
                if address & 0x01 != 0 {
                    p.closed = false;
                    None
                } else if error != 0 {
                    Some(unifying_error(error))
                } else if let Some(slot) = p.new_slot {
                    p.paired_slot = Some(slot);
                    None
                } else {
                    // The connection notification may come a bit later.
                    p.closed = true;
                    p.deadline = p.deadline.min(Instant::now() + PAIRING_GRACE);
                    None
                }
            }
            (NOTIF_DISCOVERY_STATUS, true) => match (address, error) {
                (0x00, _) => None,
                (_, 0) if p.pair_sent => None,
                (_, 0) => Some(PairingEvent::Timeout),
                (_, e) => Some(bolt_error(e)),
            },
            (NOTIF_DEVICE_DISCOVERY, true) if r.len() >= LONG_LEN && !p.pair_sent => {
                let counter = u16::from_le_bytes([r[3], r[4]]);
                if *p.counter.get_or_insert(counter) == counter {
                    match r[5] {
                        0 => {
                            p.device_kind = r[7];
                            p.address = Some([r[10], r[11], r[12], r[13], r[14], r[15]]);
                            p.authentication = r[18];
                        }
                        1 => {
                            let len = (r[6] as usize).min(LONG_LEN - 7);
                            p.name = Some(String::from_utf8_lossy(&r[7..7 + len]).trim().to_string());
                        }
                        _ => {}
                    }
                }
                None
            }
            (NOTIF_PASSKEY_REQUEST, true) => {
                let code: String = r[4..r.len().min(10)].iter().filter(|b| b.is_ascii_digit()).map(|&b| b as char).collect();
                let _ = p.events.send(PairingEvent::Passkey {
                    name: p.name.clone().unwrap_or_default(),
                    code,
                    keyboard: p.authentication & 0x01 != 0,
                });
                None
            }
            (NOTIF_PAIRING_STATUS, true) => match (address, error) {
                (0x00, 0) => None,
                (0x02, 0) if r.len() > 11 => {
                    p.paired_slot = Some(r[11]);
                    None
                }
                (_, 0) => Some(PairingEvent::Failed(PairingError::Failed)),
                (_, e) => Some(bolt_error(e)),
            },
            _ => None,
        };
        if let Some(event) = outcome {
            self.finish_pairing(event);
        }
    }

    /// Pairing work that needs I/O, run from the event loop.
    fn pairing_step(&mut self) -> io::Result<()> {
        let Some(p) = self.pairing.as_mut() else { return Ok(()) };

        // Bolt: pair the discovered device.
        if p.bolt && !p.pair_sent && p.paired_slot.is_none() {
            if let (Some(address), Some(name)) = (p.address, p.name.clone()) {
                p.pair_sent = true;
                let entropy = if kind_from_hidpp10(p.device_kind) == Some(DeviceKind::Keyboard) { 20 } else { 10 };
                let auth = p.authentication;
                let _ = p.events.send(PairingEvent::Found { name });
                let mut params = vec![0x01, 0x00];
                params.extend_from_slice(&address);
                params.extend_from_slice(&[auth, entropy]);
                if !self.write_long_register(REG_BOLT_PAIRING, &params)? {
                    self.finish_pairing(PairingEvent::Failed(PairingError::Failed));
                }
                return Ok(());
            }
        }

        if let Some(slot) = p.paired_slot {
            let known = p.name.clone();
            let name = match known {
                Some(n) if !n.is_empty() => Some(n),
                _ if !self.direct && self.receiver_kind() != ReceiverKind::Bolt => self.read_codename(slot)?,
                _ => None,
            };
            self.finish_pairing(PairingEvent::Paired { slot, name: name.unwrap_or_else(|| format!("#{slot}")) });
            return Ok(());
        }

        if Instant::now() >= p.deadline {
            if let Some(slot) = p.new_slot.filter(|_| !p.bolt) {
                p.paired_slot = Some(slot);
                return self.pairing_step();
            }
            self.close_pairing()?;
            self.finish_pairing(PairingEvent::Timeout);
        }
        Ok(())
    }

    /// Closes the receiver's pairing lock / discovery.
    fn close_pairing(&mut self) -> io::Result<()> {
        let Some(p) = self.pairing.as_ref() else { return Ok(()) };
        if p.bolt {
            if p.pair_sent {
                let mut params = [0u8; 10];
                params[0] = 0x02;
                self.write_long_register(REG_BOLT_PAIRING, &params)?;
            }
            self.register(SET_SHORT, REG_BOLT_DISCOVERY, [PAIRING_WINDOW, 0x02, 0])?;
        } else if !p.closed {
            self.register(SET_SHORT, REG_PAIRING, [0x02, 0x00, 0x00])?;
        }
        Ok(())
    }

    fn cancel_pairing(&mut self) -> io::Result<()> {
        if self.pairing.is_none() {
            return Ok(());
        }
        self.close_pairing()?;
        self.finish_pairing(PairingEvent::Cancelled);
        Ok(())
    }

    fn unpair(&mut self, slot: u8) -> io::Result<Result<(), ActionError>> {
        let kind = self.receiver_kind();
        if !kind.can_unpair() || !(1..=6).contains(&slot) {
            return Ok(Err(ActionError::NotSupported));
        }
        if self.pairing.is_some() {
            return Ok(Err(ActionError::Busy));
        }
        let done = if kind == ReceiverKind::Bolt {
            self.write_long_register(REG_BOLT_PAIRING, &[0x03, slot])?
        } else {
            self.register(SET_SHORT, REG_PAIRING, [0x03, slot, 0])?.is_some()
        };
        if !done {
            return Ok(Err(ActionError::Failed));
        }
        log!("[{}] unpaired slot {}", self.tag, slot);
        self.forget(slot);
        Ok(Ok(()))
    }

    // ----------------------------------------------------------- settings

    fn settings_target(&self, device: &str) -> Result<u8, ActionError> {
        let (&idx, slot) = self
            .devices
            .iter()
            .find(|(_, s)| s.info.as_ref().is_some_and(|i| i.id == device))
            .ok_or(ActionError::NotSupported)?;
        if !slot.online {
            return Err(ActionError::Offline);
        }
        if self.pairing.is_some() {
            return Err(ActionError::Busy);
        }
        Ok(idx)
    }

    fn read_settings(&mut self, idx: u8) -> io::Result<Result<DeviceSettings, ActionError>> {
        if !self.ping(idx)? && !self.ping(idx)? {
            return Ok(Err(ActionError::Offline));
        }
        let mut s = DeviceSettings::default();

        if let Some(f) = self.feature(idx, FEAT_ADJUSTABLE_DPI)? {
            let mut bytes = Vec::new();
            for page in 0..8u8 {
                let Some(p) = self.call(idx, f, 1, &[0, 0, page])? else { break };
                bytes.extend_from_slice(&p[1.min(p.len())..]);
                if bytes.ends_with(&[0, 0]) {
                    break;
                }
            }
            let choices = parse_dpi_list(&bytes);
            if let Some(p) = self.call(idx, f, 2, &[0])?.filter(|p| p.len() >= 5) {
                let default = u16::from_be_bytes([p[3], p[4]]);
                let current = match u16::from_be_bytes([p[1], p[2]]) {
                    0 => default,
                    dpi => dpi,
                };
                if !choices.is_empty() {
                    s.dpi = Some(DpiSettings { choices, current, default });
                }
            }
        }

        if let Some(f) = self.feature(idx, FEAT_REPORT_RATE)? {
            if let (Some(list), Some(cur)) = (self.call(idx, f, 0, &[])?, self.call(idx, f, 1, &[])?) {
                let choices = parse_rate_flags(list[0]);
                if !choices.is_empty() {
                    s.report_rate = Some(ReportRateSettings { choices, current: cur[0] });
                }
            }
        }

        if let Some(f) = self.feature(idx, FEAT_BACKLIGHT2)? {
            if let Some(cfg) = self.call(idx, f, 0, &[])?.as_deref().and_then(BacklightConfig::parse) {
                let levels = self.call(idx, f, 2, &[])?.map_or(0, |p| p[0]);
                s.backlight = Some(BacklightSettings {
                    mode: cfg.mode(),
                    modes: cfg.modes(),
                    level: cfg.level,
                    levels: if levels > 1 { levels } else { 0 },
                });
            }
        }

        if let Some((f, read, _)) = self.smart_shift(idx)? {
            s.smart_shift = self.call(idx, f, read, &[])?.as_deref().and_then(parse_wheel);
        }
        Ok(Ok(s))
    }

    /// SmartShift feature index with its read / write functions.
    fn smart_shift(&mut self, idx: u8) -> io::Result<Option<(u8, u8, u8)>> {
        if let Some(f) = self.feature(idx, FEAT_SMART_SHIFT_ENHANCED)? {
            return Ok(Some((f, 1, 2)));
        }
        Ok(self.feature(idx, FEAT_SMART_SHIFT)?.map(|f| (f, 0, 1)))
    }

    /// Writes one setting; `false` = not supported or rejected.
    fn write_setting(&mut self, idx: u8, setting: Setting) -> io::Result<bool> {
        log!("[{}] device {}: set {:?}", self.tag, idx, setting);
        let reply = match setting {
            Setting::Dpi(dpi) => match self.feature(idx, FEAT_ADJUSTABLE_DPI)? {
                Some(f) => {
                    let [hi, lo] = dpi.to_be_bytes();
                    self.call(idx, f, 3, &[0, hi, lo])?
                }
                None => None,
            },
            Setting::ReportRate(ms) => match self.feature(idx, FEAT_REPORT_RATE)? {
                Some(f) if (1..=8).contains(&ms) => self.call(idx, f, 2, &[ms])?,
                _ => None,
            },
            Setting::Backlight { mode, level } => match self.feature(idx, FEAT_BACKLIGHT2)? {
                Some(f) => match self.call(idx, f, 0, &[])?.as_deref().and_then(BacklightConfig::parse) {
                    Some(cfg) => self.call(idx, f, 1, &cfg.encode(mode, level))?,
                    None => None,
                },
                None => None,
            },
            Setting::SmartShift(mode) => match self.smart_shift(idx)? {
                Some((f, _, write)) => self.call(idx, f, write, &encode_wheel(mode))?,
                None => None,
            },
        };
        Ok(reply.is_some())
    }
}

fn unifying_error(code: u8) -> PairingEvent {
    match code {
        0x01 | 0x06 => PairingEvent::Timeout,
        0x02 => PairingEvent::Failed(PairingError::DeviceNotSupported),
        0x03 => PairingEvent::Failed(PairingError::TooManyDevices),
        _ => PairingEvent::Failed(PairingError::Failed),
    }
}

fn bolt_error(code: u8) -> PairingEvent {
    match code {
        0x01 => PairingEvent::Timeout,
        _ => PairingEvent::Failed(PairingError::Failed),
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

    #[test]
    fn detail_payloads() {
        assert_eq!(parse_firmware(&[0, b'R', b'Q', b'M', 0x68, 0x01, 0x00, 0x10, 0]).as_deref(), Some("RQM68.01.B0010"));
        assert_eq!(parse_firmware(&[0, b'U', b'0', b'1', 0x12, 0x03, 0, 0]).as_deref(), Some("U0112.03"));
        assert_eq!(parse_firmware(&[1, b'B', b'O', b'T', 1, 2, 0, 0]), None);
        assert_eq!(format_register_firmware([1, 0x12, 0x10], Some([2, 0x00, 0x32])), "12.10.B0032");
        assert_eq!(format_serial(Some(b"2049LZ0ABCD8\0"), &[1, 2, 3, 4]), "2049LZ0ABCD8");
        assert_eq!(format_serial(Some(&[0xFF, 0x01]), &[0xDE, 0xAD, 0xBE, 0xEF]), "DEADBEEF");
        assert_eq!(format_serial(None, &[0xDE, 0xAD, 0xBE, 0xEF]), "DEADBEEF");

        // 400, 800, then 900..=1600 by 100.
        let list = parse_dpi_list(&[0x01, 0x90, 0x03, 0x20, 0xE0, 0x64, 0x06, 0x40, 0, 0, 0x12, 0x34]);
        assert_eq!(list, [400, 800, 900, 1000, 1100, 1200, 1300, 1400, 1500, 1600]);
        assert_eq!(parse_dpi_list(&[0x03, 0x20, 0x06, 0x40, 0, 0]), [800, 1600]);
        assert!(parse_dpi_list(&[0xE0, 0x64, 0x06, 0x40]).is_empty()); // range without a start
        assert_eq!(parse_rate_flags(0b1000_1011), [1, 2, 4, 8]);

        assert_eq!(parse_wheel(&[1, 10, 0]), Some(WheelMode::FreeSpin));
        assert_eq!(parse_wheel(&[2, 0xFF, 0]), Some(WheelMode::Ratchet));
        assert_eq!(parse_wheel(&[2, 12, 0]), Some(WheelMode::Smart(12)));
        assert_eq!(encode_wheel(WheelMode::Smart(0)), [2, 1, 0]);
        assert_eq!(encode_wheel(WheelMode::Ratchet), [2, 0xFF, 0]);

        let cfg = BacklightConfig::parse(&[1, 0x1A, 0x28, 0, 0, 4, 60, 0, 30, 0, 0x2C, 1, 0, 0, 0, 0]).unwrap();
        assert_eq!(cfg.mode(), BacklightMode::Manual);
        assert_eq!(cfg.modes(), [BacklightMode::Off, BacklightMode::Auto, BacklightMode::Manual]);
        assert_eq!(cfg.level, 4);
        assert_eq!(cfg.encode(BacklightMode::Auto, 6), [1, 0x0A, 0xFF, 0, 60, 0, 30, 0, 0x2C, 1]);
        assert_eq!(cfg.encode(BacklightMode::Manual, 6), [1, 0x1A, 0xFF, 6, 60, 0, 30, 0, 0x2C, 1]);
        assert_eq!(cfg.encode(BacklightMode::Off, 6)[..4], [0, 0x1A, 0xFF, 6]);
        let plain = BacklightConfig::parse(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!((plain.mode(), plain.modes()), (BacklightMode::Off, vec![BacklightMode::Off, BacklightMode::On]));

        assert_eq!(passkey_clicks("000005"), Some(vec![false, false, false, false, false, false, false, true, false, true]));
        assert_eq!(passkey_clicks("1023").unwrap().iter().filter(|&&b| b).count(), 10);
        assert_eq!(passkey_clicks("abc"), None);

        assert_eq!(ReceiverKind::from_product_id(0xC548), ReceiverKind::Bolt);
        assert_eq!(ReceiverKind::from_product_id(0xC52B), ReceiverKind::Unifying);
        assert_eq!(ReceiverKind::from_product_id(0xC539), ReceiverKind::Lightspeed);
        assert_eq!(ReceiverKind::from_product_id(0xC534), ReceiverKind::Nano);
        assert!(!ReceiverKind::Nano.can_unpair() && ReceiverKind::Nano.can_pair());
        assert!(!ReceiverKind::Unknown.can_pair() && !ReceiverKind::Direct.can_pair());
    }

    #[derive(Clone, Copy, PartialEq)]
    enum PairOutcome {
        Success,
        Timeout,
    }

    /// Simulated endpoint: optionally a receiver (Unifying-style or Bolt)
    /// with one mouse on index 1, or the same mouse connected directly
    /// (long reports only).
    struct Fake {
        receiver: bool,
        bolt: bool,
        asleep: bool,
        pair_outcome: PairOutcome,
        out: VecDeque<Vec<u8>>,
        writes: usize,
        idle_reads: u32,
        battery_requests: u32,
        /// Every report written to the receiver itself (index 0xFF).
        register_writes: Vec<Vec<u8>>,
        unpaired: Vec<u8>,
        dpi: u16,
        rate: u8,
        backlight: [u8; 12],
        wheel: [u8; 3],
    }

    const NAME: &[u8] = b"G Pro Wireless Gaming Mouse";

    impl Fake {
        fn new(receiver: bool) -> Self {
            Self {
                receiver,
                bolt: false,
                asleep: false,
                pair_outcome: PairOutcome::Success,
                out: VecDeque::new(),
                writes: 0,
                idle_reads: 0,
                battery_requests: 0,
                register_writes: Vec::new(),
                unpaired: Vec::new(),
                dpi: 800,
                rate: 1,
                backlight: [1, 0x1A, 0x28, 0, 0, 4, 60, 0, 30, 0, 0x2C, 1],
                wheel: [2, 12, 12],
            }
        }

        fn reply(&mut self, req: &[u8], params: &[u8]) {
            let mut r = vec![0u8; LONG_LEN];
            r[0] = REPORT_LONG;
            r[1..4].copy_from_slice(&req[1..4]);
            r[4..4 + params.len()].copy_from_slice(params);
            self.out.push_back(r);
        }

        fn short(&mut self, bytes: [u8; 7]) {
            self.out.push_back(bytes.to_vec());
        }

        fn long(&mut self, head: &[u8]) {
            let mut r = vec![0u8; LONG_LEN];
            r[..head.len()].copy_from_slice(head);
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
                        0x1814 => 7,
                        0x2201 => 8,
                        0x8060 => 9,
                        0x1982 => 10,
                        0x2111 => 11,
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
                    p[0] = 2; // firmware entities
                    p[1..5].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
                    p[7..12].copy_from_slice(&[1, 2, 3, 4, 5]);
                    self.reply(req, &p);
                }
                (3, 1) if req[4] == 0 => self.reply(req, &[0, b'R', b'Q', b'M', 0x68, 0x01, 0x00, 0x10, 0, 0x40, 0x93]),
                (3, 1) => self.reply(req, &[1, b'B', b'O', b'T', 0x01, 0x02, 0, 0]),
                (6, 0) => {
                    self.battery_requests += 1;
                    self.reply(req, &[0x0F, 0x50, 0x00]);
                }
                // Easy-Switch: channel 2 of 3.
                (7, 0) => self.reply(req, &[3, 1, 0]),
                // DPI list (one page): 400, 800, 900..=1600 by 100.
                (8, 1) => self.reply(req, &[0, 0x01, 0x90, 0x03, 0x20, 0xE0, 0x64, 0x06, 0x40, 0, 0]),
                (8, 2) => {
                    let [hi, lo] = self.dpi.to_be_bytes();
                    self.reply(req, &[0, hi, lo, 0x03, 0x20]);
                }
                (8, 3) => {
                    self.dpi = u16::from_be_bytes([req[5], req[6]]);
                    self.reply(req, &req[4..7]);
                }
                (9, 0) => self.reply(req, &[0b1000_1011]),
                (9, 1) => self.reply(req, &[self.rate]),
                (9, 2) => {
                    self.rate = req[4];
                    self.reply(req, &[]);
                }
                (10, 0) => self.reply(req, &self.backlight.clone()),
                (10, 1) => {
                    assert_eq!(req[0], REPORT_LONG);
                    assert_eq!(req[6], 0xFF, "effect byte");
                    let b = &mut self.backlight;
                    b[0] = req[4];
                    b[1] = req[5];
                    b[5] = req[7];
                    b[6..12].copy_from_slice(&req[8..14]);
                    self.reply(req, &[]);
                }
                (10, 2) => self.reply(req, &[8]),
                (11, 1) => self.reply(req, &self.wheel.clone()),
                (11, 2) => {
                    for (i, v) in req[4..7].iter().enumerate() {
                        if *v != 0 {
                            self.wheel[i] = *v;
                        }
                    }
                    self.reply(req, &[]);
                }
                _ => self.out.push_back(vec![REPORT_LONG, req[1], 0xFF, req[2], req[3], 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            }
        }

        /// Answers to the receiver's own registers.
        fn receiver(&mut self, req: &[u8]) {
            match (req[2], req[3], req[4]) {
                (0x81, 0x02, _) => self.short([0x10, 0xFF, 0x81, 0x02, 0, 1, 0]),
                (0x81, 0x00, _) => self.short([0x10, 0xFF, 0x81, 0x00, 0, 0, 0]),
                (0x80, 0x00, _) => {
                    assert_eq!(&req[4..7], &[0, 0x09, 0]);
                    self.short([0x10, 0xFF, 0x80, 0x00, 0, 0, 0]);
                }
                (0x80, 0x02, _) => {
                    self.short([0x10, 0xFF, 0x80, 0x02, 0, 0, 0]);
                    let flags = if self.asleep { 0x40 } else { 0x00 };
                    self.short([0x10, 0x01, 0x41, 0x04, flags | 0x02, 0x88, 0x40]);
                }
                // Firmware register.
                (0x81, 0xF1, 1) => self.short([0x10, 0xFF, 0x81, 0xF1, 1, 0x12, 0x10]),
                (0x81, 0xF1, 2) => self.short([0x10, 0xFF, 0x81, 0xF1, 2, 0x00, 0x32]),
                // Receiver information.
                (0x83, 0xB5, 0x03) if !self.bolt => self.long(&[0x11, 0xFF, 0x83, 0xB5, 0x03, 0x12, 0x34, 0x56, 0x78, 0, 6]),
                (0x83, 0xFB, _) if self.bolt => self.long(&[0x11, 0xFF, 0x83, 0xFB, 0xAB, 0xCD, 0xEF, 0x01]),
                // Pairing information of slot 1 (Unifying, then Bolt layout).
                (0x83, 0xB5, 0x20) if !self.bolt && !self.unpaired.contains(&1) => {
                    self.long(&[0x11, 0xFF, 0x83, 0xB5, 0x20, 0x01, 8, 0x40, 0x93, 0, 0, 0x02])
                }
                (0x83, 0xB5, 0x40) if !self.bolt => self.long(&[0x11, 0xFF, 0x83, 0xB5, 0x40, 5, b'G', b' ', b'P', b'r', b'o']),
                (0x83, 0xB5, 0x51) if self.bolt && !self.unpaired.contains(&1) => self.long(&[0x11, 0xFF, 0x83, 0xB5, 0x51, 0x02, 0x93, 0x40, 1, 2, 3, 4]),
                (0x83, 0xB5, 0x61) if self.bolt => {
                    self.long(&[0x11, 0xFF, 0x83, 0xB5, 0x61, 0x01, 5, b'G', b' ', b'P', b'r', b'o'])
                }
                (0x83, 0xB5, 0x41) if !self.bolt => self.long(&[0x11, 0xFF, 0x83, 0xB5, 0x41, 4, b'K', b'4', b'0', b'0']),
                // Unifying pairing lock.
                (0x80, 0xB2, action) => {
                    self.short([0x10, 0xFF, 0x80, 0xB2, 0, 0, 0]);
                    match action {
                        0x01 => {
                            assert_eq!(req[6], PAIRING_WINDOW);
                            self.short([0x10, 0xFF, 0x4A, 0x01, 0, 0, 0]);
                            match self.pair_outcome {
                                PairOutcome::Success => {
                                    self.short([0x10, 0x02, 0x41, 0x04, 0x01, 0x23, 0x40]);
                                    self.short([0x10, 0xFF, 0x4A, 0x00, 0, 0, 0]);
                                }
                                PairOutcome::Timeout => self.short([0x10, 0xFF, 0x4A, 0x00, 0x01, 0, 0]),
                            }
                        }
                        0x02 => self.short([0x10, 0xFF, 0x4A, 0x00, 0, 0, 0]),
                        0x03 => {
                            self.unpaired.push(req[5]);
                            self.short([0x10, req[5], 0x40, 0x02, 0, 0, 0]);
                        }
                        _ => panic!("unexpected 0xB2 action {action}"),
                    }
                }
                // Bolt discovery.
                (0x80, 0xC0, timeout) if self.bolt => {
                    self.short([0x10, 0xFF, 0x80, 0xC0, 0, 0, 0]);
                    if req[5] == 0x01 {
                        assert_eq!(timeout, PAIRING_WINDOW);
                        self.short([0x10, 0xFF, 0x53, 0x00, 0, 0, 0]);
                        // Discovered keyboard: kind, address, authentication.
                        self.long(&[
                            0x11, 0xFF, 0x4F, 0x07, 0x00, 0x00, 0, 0x01, 0, 0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0, 0,
                            0x01,
                        ]);
                        // Its name.
                        self.long(&[0x11, 0xFF, 0x4F, 0x07, 0x00, 0x01, 7, b'M', b'X', b' ', b'K', b'e', b'y', b's']);
                        // A notification from another discovery round is ignored.
                        self.long(&[0x11, 0xFF, 0x4F, 0x08, 0x00, 0x01, 3, b'B', b'a', b'd']);
                    }
                }
                // Bolt pairing.
                (0x82, 0xC1, action) if self.bolt => {
                    assert_eq!(req[0], REPORT_LONG, "0x82 must be a long report");
                    self.short([0x10, 0xFF, 0x82, 0xC1, 0, 0, 0]);
                    match action {
                        0x01 => {
                            assert_eq!(&req[6..12], &[0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6]);
                            assert_eq!((req[12], req[13]), (0x01, 20), "authentication, keyboard entropy");
                            self.short([0x10, 0xFF, 0x53, 0x01, 0, 0, 0]);
                            self.short([0x10, 0xFF, 0x54, 0x00, 0, 0, 0]);
                            self.long(&[0x11, 0xFF, 0x4D, 0x00, b'0', b'4', b'2', b'1', b'3', b'7']);
                            self.short([0x10, 0xFF, 0x4E, 0x00, 0x01, 0, 0]);
                            self.long(&[0x11, 0xFF, 0x54, 0x02, 0, 0, 0, 0, 0, 0, 0, 3]);
                            self.short([0x10, 0xFF, 0x54, 0x01, 0, 0, 0]);
                        }
                        0x03 => {
                            self.unpaired.push(req[5]);
                            self.short([0x10, req[5], 0x40, 0x02, 0, 0, 0]);
                        }
                        _ => {}
                    }
                }
                _ => self.error10(req, 0x02),
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
            match req[1] {
                0xFF => {
                    if req[2] == 0x80 || req[2] == 0x82 {
                        self.register_writes.push(req.to_vec());
                    }
                    self.receiver(req)
                }
                1 if self.asleep || self.unpaired.contains(&1) => self.error10(req, 0x09),
                1 => self.device(req),
                _ => self.error10(req, 0x09),
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

    fn assert_full_flow(events: &[Event], receiver: Option<&str>) {
        assert_eq!(events.len(), 4, "{events:?}");
        match &events[0] {
            Event::Init { id, name, kind, source, has_battery } => {
                assert_eq!(id, "DEADBEEF-0102030405");
                assert_eq!(name, "G Pro Wireless Gaming Mouse");
                assert_eq!((*kind, *source, *has_battery), (DeviceKind::Mouse, Source::Native, true));
            }
            e => panic!("unexpected {e:?}"),
        }
        match &events[1] {
            Event::Info { id, info } => {
                assert_eq!(id, "DEADBEEF-0102030405");
                assert_eq!(info.firmware.as_deref(), Some("RQM68.01.B0010"));
                assert_eq!(info.serial.as_deref(), Some("DEADBEEF"));
                assert_eq!(info.host_channel, Some((2, 3)));
                assert_eq!(info.receiver.as_deref(), receiver);
                assert_eq!(info.signal, None);
            }
            e => panic!("unexpected {e:?}"),
        }
        match &events[2] {
            Event::Update { id, battery } => {
                assert_eq!(id, "DEADBEEF-0102030405");
                assert_eq!(battery.millivolts, Some(3920));
                assert_eq!(battery.status, Status::Discharging);
            }
            e => panic!("unexpected {e:?}"),
        }
        assert!(matches!(&events[3], Event::Offline { .. }));
    }

    #[test]
    fn receiver_with_one_mouse() {
        assert_full_flow(&run(Fake::new(true), settings()), Some("Logitech Receiver"));
    }

    #[test]
    fn direct_long_only_device() {
        assert_full_flow(&run(Fake::new(false), settings()), None);
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
        let info = Info::new("X".into(), "Mouse".into(), DeviceKind::Mouse, Some(BatteryFeature::Unified(7)));
        s.devices.insert(1, Slot { info: Some(info), due: None, fails: 3, online: false, reread: false });
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
        assert!(!s.devices[&1].online);
    }

    // ------------------------------------------- receivers, pairing, settings

    type Events = mpsc::Receiver<Event>;

    /// A session that has discovered its receiver and identified the mouse
    /// on slot 1, driven step by step.
    fn ready(mut fake: Fake, product_id: u16) -> (Session<Fake>, Events) {
        fake.bolt = product_id == 0xC548;
        let (tx, rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        std::mem::forget(cmd_tx); // keep the command channel open
        let mut s = Session::new(fake, settings(), cmd_rx, move |e| tx.send(e).unwrap(), "t".into())
            .with_product_id(product_id);
        s.discover().unwrap();
        s.service(1).unwrap();
        (s, rx)
    }

    /// Runs the event loop body until nothing is left to do.
    fn settle(s: &mut Session<Fake>) {
        for _ in 0..100 {
            while let Some(r) = s.backlog.pop_front() {
                s.on_unsolicited(&r);
            }
            while let Some(c) = s.pending.pop_front() {
                s.execute(c).unwrap();
            }
            s.pairing_step().unwrap();
            match s.transport.read(Duration::from_millis(1)).unwrap() {
                Incoming::Report(r) => s.on_unsolicited(&r),
                _ if s.backlog.is_empty() && s.pending.is_empty() => return,
                _ => {}
            }
        }
    }

    #[test]
    fn describe_unifying_receiver() {
        let (mut s, _events) = ready(Fake::new(true), 0xC52B);
        let info = s.describe().unwrap();
        assert_eq!((info.kind, info.name.as_str(), info.slots), (ReceiverKind::Unifying, "Unifying Receiver", 6));
        assert_eq!(info.serial.as_deref(), Some("12345678"));
        assert_eq!(info.firmware.as_deref(), Some("12.10.B0032"));
        assert_eq!(info.devices.len(), 1, "{info:?}");
        let d = &info.devices[0];
        assert_eq!((d.slot, d.id.as_deref(), d.name.as_str()), (1, Some("DEADBEEF-0102030405"), "G Pro Wireless Gaming Mouse"));
        assert_eq!((d.kind, d.online, d.wpid), (Some(DeviceKind::Mouse), true, Some(0x4093)));
        assert_eq!(d.info.firmware.as_deref(), Some("RQM68.01.B0010"));
        assert_eq!(d.info.host_channel, Some((2, 3)));
        // Cached: describing again does not touch the receiver.
        let writes = s.transport.writes;
        assert_eq!(s.describe().unwrap(), info);
        assert_eq!(s.transport.writes, writes);
    }

    #[test]
    fn describe_lists_sleeping_devices_by_code_name() {
        let mut fake = Fake::new(true);
        fake.asleep = true;
        let (mut s, events) = ready(fake, 0xC532);
        let info = s.describe().unwrap();
        assert_eq!(info.devices.len(), 1);
        let d = &info.devices[0];
        assert_eq!((d.id.as_ref(), d.name.as_str(), d.online), (None, "G Pro", false));
        assert!(events.try_iter().next().is_none());
    }

    #[test]
    fn describe_bolt_receiver() {
        let (mut s, _events) = ready(Fake::new(true), 0xC548);
        let info = s.describe().unwrap();
        assert_eq!((info.kind, info.slots), (ReceiverKind::Bolt, 6));
        assert_eq!(info.serial.as_deref(), Some("ABCDEF01"));
        assert_eq!(info.devices.len(), 1);
        assert_eq!(info.devices[0].wpid, Some(0x4093));
    }

    #[test]
    fn reconnection_rereads_details() {
        let (mut s, events) = ready(Fake::new(true), 0xC52B);
        let _: Vec<Event> = events.try_iter().collect();
        s.on_unsolicited(&[0x10, 0x01, 0x41, 0x04, 0x42, 0x88, 0x40]); // down
        s.on_unsolicited(&[0x10, 0x01, 0x41, 0x04, 0x02, 0x88, 0x40]); // up
        assert!(s.devices[&1].reread);
        s.service(1).unwrap();
        let events: Vec<Event> = events.try_iter().collect();
        assert!(matches!(&events[0], Event::Offline { .. }), "{events:?}");
        assert!(matches!(&events[1], Event::Info { info, .. } if info.host_channel == Some((2, 3))), "{events:?}");
        assert!(matches!(&events[2], Event::Update { .. }), "{events:?}");
    }

    #[test]
    fn unifying_pairing_succeeds() {
        let (mut s, _events) = ready(Fake::new(true), 0xC52B);
        let (tx, progress) = mpsc::channel();
        s.pending.push_back(Command::Pair(tx));
        settle(&mut s);
        let steps: Vec<PairingEvent> = progress.try_iter().collect();
        assert_eq!(steps, [PairingEvent::Waiting, PairingEvent::Paired { slot: 2, name: "K400".into() }]);
        assert!(s.pairing.is_none());
        assert!(s.devices.contains_key(&2), "the new device is serviced like any other");
        assert!(s.transport.register_writes.iter().any(|w| w[2..7] == [0x80, 0xB2, 0x01, 0x00, PAIRING_WINDOW]));
    }

    #[test]
    fn unifying_pairing_times_out() {
        let mut fake = Fake::new(true);
        fake.pair_outcome = PairOutcome::Timeout;
        let (mut s, _events) = ready(fake, 0xC52B);
        let (tx, progress) = mpsc::channel();
        s.pending.push_back(Command::Pair(tx));
        settle(&mut s);
        assert_eq!(progress.try_iter().collect::<Vec<_>>(), [PairingEvent::Waiting, PairingEvent::Timeout]);
    }

    #[test]
    fn pairing_cancel_and_busy() {
        let (mut s, _events) = ready(Fake::new(true), 0xC52B);
        let (tx, progress) = mpsc::channel();
        s.start_pairing(tx).unwrap();
        s.transport.out.clear(); // the device never shows up
        s.backlog.clear();
        let (tx2, second) = mpsc::channel();
        s.start_pairing(tx2).unwrap();
        assert_eq!(second.try_iter().collect::<Vec<_>>(), [PairingEvent::Failed(PairingError::Busy)]);
        let (reply, unpaired) = mpsc::channel();
        s.pending.push_back(Command::Unpair { slot: 1, reply });
        s.pending.push_back(Command::CancelPairing);
        settle(&mut s);
        assert_eq!(unpaired.try_recv(), Ok(Err(ActionError::Busy)));
        assert_eq!(progress.try_iter().collect::<Vec<_>>(), [PairingEvent::Waiting, PairingEvent::Cancelled]);
        assert!(s.transport.register_writes.iter().any(|w| w[2..7] == [0x80, 0xB2, 0x02, 0, 0]));
    }

    #[test]
    fn pairing_deadline() {
        let (mut s, _events) = ready(Fake::new(true), 0xC52B);
        let (tx, progress) = mpsc::channel();
        s.start_pairing(tx).unwrap();
        s.transport.out.clear();
        s.backlog.clear();
        s.pairing.as_mut().unwrap().deadline = Instant::now();
        s.pairing_step().unwrap();
        assert_eq!(progress.try_iter().collect::<Vec<_>>(), [PairingEvent::Waiting, PairingEvent::Timeout]);
        assert!(s.transport.register_writes.iter().any(|w| w[2..5] == [0x80, 0xB2, 0x02]));
    }

    #[test]
    fn pairing_not_supported() {
        for pid in [0x0000u16, 0xC077] {
            let (mut s, _events) = ready(Fake::new(true), pid);
            let (tx, progress) = mpsc::channel();
            s.start_pairing(tx).unwrap();
            assert_eq!(progress.try_iter().collect::<Vec<_>>(), [PairingEvent::Failed(PairingError::NotSupported)]);
            assert!(s.transport.register_writes.iter().all(|w| w[3] != 0xB2), "no write on an unknown receiver");
        }
    }

    #[test]
    fn bolt_pairing_with_passkey() {
        let (mut s, _events) = ready(Fake::new(true), 0xC548);
        let (tx, progress) = mpsc::channel();
        s.pending.push_back(Command::Pair(tx));
        settle(&mut s);
        let steps: Vec<PairingEvent> = progress.try_iter().collect();
        assert_eq!(
            steps,
            [
                PairingEvent::Waiting,
                PairingEvent::Found { name: "MX Keys".into() },
                PairingEvent::Passkey { name: "MX Keys".into(), code: "042137".into(), keyboard: true },
                PairingEvent::Paired { slot: 3, name: "MX Keys".into() },
            ]
        );
        assert!(s.transport.register_writes.iter().any(|w| w[2..6] == [0x80, 0xC0, PAIRING_WINDOW, 0x01]));
    }

    #[test]
    fn unpair_publishes_remove() {
        for pid in [0xC52Bu16, 0xC548] {
            let (mut s, events) = ready(Fake::new(true), pid);
            let _: Vec<Event> = events.try_iter().collect();
            let (reply, result) = mpsc::channel();
            s.pending.push_back(Command::Unpair { slot: 1, reply });
            settle(&mut s);
            assert_eq!(result.try_recv(), Ok(Ok(())));
            let events: Vec<Event> = events.try_iter().collect();
            assert_eq!(events.len(), 1, "{events:?}");
            assert!(matches!(&events[0], Event::Remove { id } if id == "DEADBEEF-0102030405"));
            assert!(!s.devices.contains_key(&1));
            let expected: &[u8] = if pid == 0xC548 { &[0x82, 0xC1, 0x03, 0x01] } else { &[0x80, 0xB2, 0x03, 0x01] };
            assert!(s.transport.register_writes.iter().any(|w| &w[2..6] == expected));
            assert!(s.describe().unwrap().devices.is_empty());
        }
    }

    #[test]
    fn unpair_refused_on_nano() {
        let (mut s, events) = ready(Fake::new(true), 0xC534);
        let _: Vec<Event> = events.try_iter().collect();
        assert_eq!(s.unpair(1).unwrap(), Err(ActionError::NotSupported));
        assert!(s.transport.register_writes.iter().all(|w| w[3] != 0xB2));
        assert!(events.try_iter().next().is_none());
    }

    #[test]
    fn settings_read_and_write() {
        let (mut s, _events) = ready(Fake::new(true), 0xC52B);
        let id = "DEADBEEF-0102030405".to_string();
        let get = |s: &mut Session<Fake>| {
            let (reply, rx) = mpsc::channel();
            s.pending.push_back(Command::GetSettings { device: id.clone(), reply });
            settle(s);
            rx.try_recv().unwrap()
        };
        let set = |s: &mut Session<Fake>, setting| {
            let (reply, rx) = mpsc::channel();
            s.pending.push_back(Command::SetSetting { device: id.clone(), setting, reply });
            settle(s);
            rx.try_recv().unwrap()
        };

        let st = get(&mut s).unwrap();
        let dpi = st.dpi.clone().unwrap();
        assert_eq!((dpi.current, dpi.default, dpi.choices.len()), (800, 800, 10));
        assert_eq!(st.report_rate, Some(ReportRateSettings { choices: vec![1, 2, 4, 8], current: 1 }));
        let bl = st.backlight.clone().unwrap();
        assert_eq!((bl.mode, bl.level, bl.levels), (BacklightMode::Manual, 4, 8));
        assert_eq!(st.smart_shift, Some(WheelMode::Smart(12)));

        let st = set(&mut s, Setting::Dpi(1200)).unwrap();
        assert_eq!((s.transport.dpi, st.dpi.unwrap().current), (1200, 1200));
        let st = set(&mut s, Setting::ReportRate(4)).unwrap();
        assert_eq!(st.report_rate.unwrap().current, 4);
        let st = set(&mut s, Setting::Backlight { mode: BacklightMode::Manual, level: 6 }).unwrap();
        assert_eq!(st.backlight.as_ref().map(|b| (b.mode, b.level)), Some((BacklightMode::Manual, 6)));
        assert_eq!(&s.transport.backlight[6..12], &[60, 0, 30, 0, 0x2C, 1], "durations preserved");
        let st = set(&mut s, Setting::Backlight { mode: BacklightMode::Off, level: 0 }).unwrap();
        assert_eq!(st.backlight.unwrap().mode, BacklightMode::Off);
        let st = set(&mut s, Setting::SmartShift(WheelMode::FreeSpin)).unwrap();
        assert_eq!(st.smart_shift, Some(WheelMode::FreeSpin));
        let st = set(&mut s, Setting::SmartShift(WheelMode::Ratchet)).unwrap();
        assert_eq!(st.smart_shift, Some(WheelMode::Ratchet));
        assert_eq!(set(&mut s, Setting::ReportRate(9)), Err(ActionError::Failed));

        // Unknown device, then offline device.
        let (reply, rx) = mpsc::channel();
        s.pending.push_back(Command::GetSettings { device: "nope".into(), reply });
        settle(&mut s);
        assert_eq!(rx.try_recv().unwrap(), Err(ActionError::NotSupported));
        s.on_unsolicited(&[0x10, 0x01, 0x41, 0x04, 0x42, 0x88, 0x40]);
        let writes = s.transport.writes;
        assert_eq!(get(&mut s), Err(ActionError::Offline));
        assert_eq!(s.transport.writes, writes, "an offline device is not woken up");
    }

    #[test]
    fn settings_absent_features() {
        // Only positive feature lookups are cached.
        let (mut s, _events) = ready(Fake::new(true), 0xC52B);
        let idx = 1;
        for f in [FEAT_ADJUSTABLE_DPI, FEAT_REPORT_RATE, FEAT_BACKLIGHT2, FEAT_SMART_SHIFT_ENHANCED] {
            assert!(s.feature(idx, f).unwrap().is_some());
        }
        assert_eq!(s.feature(idx, FEAT_SMART_SHIFT).unwrap(), None);
        assert!(!s.devices[&idx].info.as_ref().unwrap().features.contains_key(&FEAT_SMART_SHIFT));
    }
}
