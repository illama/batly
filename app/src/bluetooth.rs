//! Battery of any Bluetooth device that reports it to Windows.
//!
//! Two sources, merged per Bluetooth address:
//! - the device node property `DEVPKEY_Bluetooth_Battery`
//!   ({104EA319-6EE2-4701-BD47-8DDBF425BBE5}, 2), filled in by Windows for
//!   classic devices that report their level over the hands-free profile
//!   (headsets, earbuds) and for many Bluetooth LE devices;
//! - the GATT Battery Service (0x180F, Battery Level 0x2A19) of Bluetooth
//!   LE devices, read through the Win32 BluetoothLE API, with a
//!   value-changed registration so that notifications arrive without
//!   polling.
//!
//! Device node properties have no change notification: they are re-read
//! every minute (local registry reads, no radio traffic) and on demand.
//! Logitech devices are skipped (the HID++ backend already reports them),
//! and so are Xbox / PlayStation controllers when the game controller
//! backend is enabled.

use crate::model::{Battery, DeviceInfo, DeviceKind, Status};
use crate::tracker::Seen;

/// Bluetooth SIG company id of Logitech (vendor id source 1).
const LOGITECH_SIG: u16 = 0x01DA;
const LOGITECH_USB: u16 = 0x046D;
const MICROSOFT_USB: u16 = 0x045E;
const SONY_USB: u16 = 0x054C;

/// Bluetooth address embedded in a device instance id, e.g.
/// `BTHENUM\DEV_A0E9DB123456\…`, `BTHLE\DEV_…`,
/// `BTHLEDEVICE\{0000180F-…}_DEV_VID&02046D_PID&B023_REV&0004_D1D2D3D4D5D6\…`
/// or `BTHENUM\{0000111E-…}_VID&0001005D_PID&223B\7&2E5E3C7A&0&A0E9DB123456_C00000000`.
pub fn bt_address(instance_id: &str) -> Option<u64> {
    let id = instance_id.to_ascii_uppercase();
    let mut segs = id.split('\\');
    if !matches!(segs.next()?, "BTHENUM" | "BTHLE" | "BTHLEDEVICE") {
        return None;
    }
    let hex12 = |s: &str| -> Option<u64> {
        (s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| u64::from_str_radix(s, 16).ok()).flatten().filter(|&a| a != 0)
    };
    let dev = segs.next()?;
    if let Some(rest) = dev.strip_prefix("DEV_") {
        return hex12(rest.get(..12)?);
    }
    if let Some(a) = dev.rsplit('_').next().and_then(hex12) {
        return Some(a);
    }
    let inst = segs.next()?;
    inst.split('_').next()?.rsplit('&').next().and_then(hex12)
}

/// (vendor id source, vendor id, product id) embedded in an instance id:
/// `_VID&0001005D_PID&223B` (classic: 4 digit source) or
/// `_DEV_VID&02046D_PID&B023` (LE: 2 digit source).
pub fn bt_vendor(instance_id: &str) -> Option<(u16, u16, Option<u16>)> {
    let id = instance_id.to_ascii_uppercase();
    let hex_after = |tag: &str| -> Option<&str> {
        let start = id.find(tag)? + tag.len();
        let rest = &id[start..];
        let len = rest.bytes().take_while(|b| b.is_ascii_hexdigit()).count();
        Some(&rest[..len])
    };
    let vid = hex_after("VID&")?;
    if vid.len() < 4 {
        return None;
    }
    let (source, vid) = vid.split_at(vid.len() - 4);
    let source = if source.is_empty() { 0 } else { u16::from_str_radix(source, 16).ok()? };
    let vid = u16::from_str_radix(vid, 16).ok()?;
    let pid = hex_after("PID&").filter(|p| p.len() == 4).and_then(|p| u16::from_str_radix(p, 16).ok());
    Some((source, vid, pid))
}

/// Vendor id in USB-IF terms (`None` for Bluetooth SIG company ids other
/// than the few we care about).
fn usb_vid(source: u16, vid: u16) -> Option<u16> {
    match (source, vid) {
        (1, LOGITECH_SIG) => Some(LOGITECH_USB),
        (1, _) => None,
        _ => Some(vid),
    }
}

/// Devices another backend already reports.
pub fn handled_elsewhere(vendor: Option<(u16, u16)>, kind: DeviceKind, gamepads: bool) -> bool {
    match vendor.and_then(|(s, v)| usb_vid(s, v)) {
        Some(LOGITECH_USB) => true,
        Some(MICROSOFT_USB | SONY_USB) => gamepads && kind == DeviceKind::Gamepad,
        _ => false,
    }
}

/// Kind from the classic Class of Device (major / minor device class).
pub fn kind_from_cod(cod: u32) -> Option<DeviceKind> {
    let major = (cod >> 8) & 0x1F;
    let minor = (cod >> 2) & 0x3F;
    match major {
        // Audio / video: wearable headset, hands-free, headphones.
        0x04 => matches!(minor, 1 | 2 | 6).then_some(DeviceKind::Headset),
        // Peripheral: keyboard / pointing bits, then the sub-type.
        0x05 => match (minor & 0x30, minor & 0x0F) {
            (0x10 | 0x30, _) => Some(DeviceKind::Keyboard),
            (0x20, _) => Some(DeviceKind::Mouse),
            (_, 1 | 2) => Some(DeviceKind::Gamepad),
            _ => None,
        },
        _ => None,
    }
}

/// Kind from the GAP appearance of a Bluetooth LE device.
pub fn kind_from_appearance(appearance: u16) -> Option<DeviceKind> {
    match (appearance >> 6, appearance & 0x3F) {
        (0x0F, 1) => Some(DeviceKind::Keyboard),
        (0x0F, 2) => Some(DeviceKind::Mouse),
        (0x0F, 3 | 4) => Some(DeviceKind::Gamepad),
        // Wearable audio device: earbud, headset, headphones, neckband.
        (0x25, 1) => Some(DeviceKind::Earbuds),
        (0x25, 2..=4) => Some(DeviceKind::Headset),
        _ => None,
    }
}

/// Kind guessed from the device name (the only way to tell earbuds from a
/// headset on classic Bluetooth).
pub fn kind_from_name(name: &str) -> Option<DeviceKind> {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["airpods", "buds", "earbud", "earphone", "écouteur", "ecouteur", "wf-", "tws", "pods"]) {
        Some(DeviceKind::Earbuds)
    } else if has(&["headset", "headphone", "casque", "wh-", "arctis", "hyperx cloud"]) {
        Some(DeviceKind::Headset)
    } else if has(&["controller", "gamepad", "manette", "dualsense", "dualshock"]) {
        Some(DeviceKind::Gamepad)
    } else if has(&["keyboard", "clavier", "keys"]) {
        Some(DeviceKind::Keyboard)
    } else if has(&["mouse", "souris", "trackpad"]) {
        Some(DeviceKind::Mouse)
    } else {
        None
    }
}

pub fn classify(cod: Option<u32>, appearance: Option<u16>, name: &str) -> DeviceKind {
    let by_name = kind_from_name(name);
    if let Some(k) = appearance.and_then(kind_from_appearance) {
        return k;
    }
    if by_name == Some(DeviceKind::Earbuds) {
        return DeviceKind::Earbuds;
    }
    cod.and_then(kind_from_cod).or(by_name).unwrap_or(DeviceKind::Other)
}

/// 16-bit form of a GATT UUID (short form, or a long UUID built on the
/// Bluetooth base UUID 0000xxxx-0000-1000-8000-00805F9B34FB).
pub fn uuid16(is_short: bool, short: u16, data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Option<u16> {
    if is_short {
        return Some(short);
    }
    (data1 <= 0xFFFF && data2 == 0 && data3 == 0x1000 && data4 == [0x80, 0, 0, 0x80, 0x5F, 0x9B, 0x34, 0xFB]).then_some(data1 as u16)
}

/// Battery Level characteristic value (one byte, 0..=100).
pub fn gatt_level(value: &[u8]) -> Option<u8> {
    value.first().copied().filter(|&v| v <= 100)
}

pub fn format_address(address: u64) -> String {
    let b = address.to_be_bytes();
    format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", b[2], b[3], b[4], b[5], b[6], b[7])
}

/// Formats a GUID stored in its in-memory (mixed endian) layout as
/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`.
pub fn guid_string(b: &[u8; 16]) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]),
        u16::from_le_bytes([b[6], b[7]]),
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

pub fn device_id(address: u64) -> String {
    format!("bt-{:012x}", address & 0xFFFF_FFFF_FFFF)
}

/// Everything known about one Bluetooth address after a scan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub address: u64,
    pub le: bool,
    pub name: Option<String>,
    pub cod: Option<u32>,
    pub appearance: Option<u16>,
    /// (vendor id source, vendor id)
    pub vendor: Option<(u16, u16)>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    /// `DEVPKEY_Bluetooth_Battery`.
    pub property_level: Option<u8>,
    /// GATT Battery Level (fresher: notified).
    pub gatt_level: Option<u8>,
    /// `None` when the connection state is unknown.
    pub connected: Option<bool>,
}

impl Node {
    /// The device as published, or `None` when it is not to be shown
    /// (no battery, disconnected, or reported by another backend).
    pub fn to_seen(&self, gamepads: bool) -> Option<Seen> {
        let level = self.gatt_level.or(self.property_level)?;
        if self.connected == Some(false) {
            return None;
        }
        let name = self.name.clone().unwrap_or_else(|| format_address(self.address));
        let kind = classify(self.cod, self.appearance, &name);
        if handled_elsewhere(self.vendor, kind, gamepads) {
            return None;
        }
        let model = match (&self.manufacturer, &self.model) {
            (Some(m), Some(n)) if !n.starts_with(m.as_str()) => Some(format!("{m} {n}")),
            (_, Some(n)) => Some(n.clone()),
            (Some(m), None) => Some(m.clone()),
            (None, None) => None,
        };
        Some(Seen {
            id: device_id(self.address),
            name,
            kind,
            has_battery: true,
            battery: Some(Battery { percent: level as f32, status: Status::Unknown, millivolts: None, mileage: None }),
            info: DeviceInfo { model, serial: Some(format_address(self.address)), ..Default::default() },
        })
    }
}

#[cfg(windows)]
pub use imp::Bluetooth;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::config;
    use crate::hidio::{self, OwnedHandle};
    use crate::log;
    use crate::model::{Event, Hub, Source};
    use crate::tracker::Tracker;
    use crate::win::wide;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use windows_sys::core::GUID;
    use windows_sys::Win32::Devices::Bluetooth::{
        BluetoothFindDeviceClose, BluetoothFindFirstDevice, BluetoothFindNextDevice, BluetoothGATTGetCharacteristicValue,
        BluetoothGATTGetCharacteristics, BluetoothGATTGetServices, BluetoothGATTRegisterEvent, BluetoothGATTUnregisterEvent,
        CharacteristicValueChangedEvent, BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS, BLUETOOTH_GATT_FLAG_FORCE_READ_FROM_DEVICE,
        BLUETOOTH_GATT_FLAG_NONE, BLUETOOTH_GATT_VALUE_CHANGED_EVENT, BLUETOOTH_GATT_VALUE_CHANGED_EVENT_REGISTRATION,
        BTH_LE_GATT_CHARACTERISTIC, BTH_LE_GATT_CHARACTERISTIC_VALUE, BTH_LE_GATT_EVENT_TYPE, BTH_LE_GATT_SERVICE,
    };
    use windows_sys::Win32::Devices::DeviceQuery::{
        DevFreeObjectProperties, DevGetObjectProperties, DevObjectTypeDeviceContainer, DevQueryFlagNone,
    };
    use windows_sys::Win32::Devices::Properties::{
        DEVPKEY_DeviceContainer_IsConnected, DEVPKEY_Device_ContainerId, DEVPKEY_Device_FriendlyName, DEVPKEY_NAME, DEVPROPCOMPKEY,
        DEVPROPERTY, DEVPROP_STORE_SYSTEM, DEVPROP_TYPE_BOOLEAN,
    };
    use windows_sys::Win32::Foundation::{DEVPROPKEY, GENERIC_READ, GENERIC_WRITE};

    const POLL: Duration = Duration::from_secs(60);
    const BLUETOOTH_KEY: GUID = GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a);
    const KEY_BATTERY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5), pid: 2 };
    const KEY_COD: DEVPROPKEY = DEVPROPKEY { fmtid: BLUETOOTH_KEY, pid: 10 };
    const KEY_MANUFACTURER: DEVPROPKEY = DEVPROPKEY { fmtid: BLUETOOTH_KEY, pid: 4 };
    const KEY_MODEL: DEVPROPKEY = DEVPROPKEY { fmtid: BLUETOOTH_KEY, pid: 5 };
    const KEY_VID_SOURCE: DEVPROPKEY = DEVPROPKEY { fmtid: BLUETOOTH_KEY, pid: 6 };
    const KEY_VID: DEVPROPKEY = DEVPROPKEY { fmtid: BLUETOOTH_KEY, pid: 7 };
    const BATTERY_SERVICE: GUID = GUID::from_u128(0x0000180f_0000_1000_8000_00805f9b34fb);
    const GAP_SERVICE: GUID = GUID::from_u128(0x00001800_0000_1000_8000_00805f9b34fb);
    const BATTERY_LEVEL: u16 = 0x2A19;
    const APPEARANCE: u16 = 0x2A01;

    enum Msg {
        Rescan,
        Refresh,
        /// GATT notification: (address, level).
        Level(u64, u8),
    }

    /// Handle to the Bluetooth backend thread.
    pub struct Bluetooth {
        tx: Sender<Msg>,
    }

    impl Bluetooth {
        /// Starts the backend; it does nothing when `cfg.bluetooth` is off.
        pub fn start(cfg: &config::OtherDevices, hub: Arc<Hub>) -> Self {
            let (tx, rx) = mpsc::channel();
            if cfg.bluetooth {
                let gamepads = cfg.gamepads;
                let notify = tx.clone();
                let _ = thread::Builder::new().name("bluetooth".into()).stack_size(256 * 1024).spawn(move || {
                    let mut state = State {
                        tracker: Tracker::new(Source::Bluetooth),
                        gatt: HashMap::new(),
                        appearance: HashMap::new(),
                        nodes: HashMap::new(),
                        gamepads,
                        notify,
                    };
                    state.scan(&hub, false);
                    loop {
                        match rx.recv_timeout(POLL) {
                            Ok(Msg::Level(address, level)) => state.notified(&hub, address, level),
                            Ok(Msg::Refresh) => state.scan(&hub, true),
                            Ok(Msg::Rescan) | Err(RecvTimeoutError::Timeout) => state.scan(&hub, false),
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                });
            }
            Self { tx }
        }

        /// Call after a device arrival / removal.
        pub fn rescan(&self) {
            let _ = self.tx.send(Msg::Rescan);
        }

        /// Re-reads every battery now (GATT values straight from the devices).
        pub fn refresh(&self) {
            let _ = self.tx.send(Msg::Refresh);
        }
    }

    struct State {
        tracker: Tracker,
        /// Open Battery Service handles, by interface path.
        gatt: HashMap<String, Gatt>,
        /// GAP appearance, read once per address.
        appearance: HashMap<u64, Option<u16>>,
        nodes: HashMap<u64, Node>,
        gamepads: bool,
        notify: Sender<Msg>,
    }

    impl State {
        fn publish(hub: &Hub, events: Vec<Event>) {
            for e in events {
                hub.publish(e);
            }
        }

        fn notified(&mut self, hub: &Hub, address: u64, level: u8) {
            let Some(node) = self.nodes.get_mut(&address) else { return };
            node.gatt_level = Some(level);
            if let Some(seen) = node.to_seen(self.gamepads) {
                let events = self.tracker.one(seen);
                Self::publish(hub, events);
            }
        }

        fn scan(&mut self, hub: &Hub, force: bool) {
            let mut nodes: HashMap<u64, Node> = HashMap::new();
            let mut containers: HashMap<u64, String> = HashMap::new();

            // 1. Device node properties.
            for id in hidio::present_device_ids() {
                let Some(address) = bt_address(&id) else { continue };
                let Some(dn) = hidio::locate(&id) else { continue };
                let upper = id.to_ascii_uppercase();
                let node = nodes.entry(address).or_insert_with(|| Node { address, ..Default::default() });
                node.le |= upper.starts_with("BTHLE");
                if let Some(level) = hidio::prop_u8(dn, &KEY_BATTERY).filter(|&l| l <= 100) {
                    node.property_level = node.property_level.max(Some(level));
                }
                if node.vendor.is_none() {
                    node.vendor = bt_vendor(&id).map(|(s, v, _)| (s, v));
                }
                // The root node (`…\DEV_<address>`) carries the device details.
                if upper.split('\\').nth(1).is_some_and(|s| s.starts_with("DEV_")) {
                    node.name = hidio::prop_string(dn, &DEVPKEY_NAME).or_else(|| hidio::prop_string(dn, &DEVPKEY_Device_FriendlyName));
                    node.cod = hidio::prop_u32(dn, &KEY_COD).or(node.cod);
                    node.manufacturer = hidio::prop_string(dn, &KEY_MANUFACTURER);
                    node.model = hidio::prop_string(dn, &KEY_MODEL);
                    if let Some(vid) = hidio::prop_u16(dn, &KEY_VID) {
                        let source = hidio::prop_u8(dn, &KEY_VID_SOURCE).map(u16::from).unwrap_or(0);
                        node.vendor = Some((source, vid));
                    }
                    if let Some(container) = hidio::prop_guid(dn, &DEVPKEY_Device_ContainerId) {
                        containers.insert(address, guid_string(&container));
                    }
                }
            }

            // 2. Connection state: classic devices from the Bluetooth API,
            //    LE devices from their device container.
            let classic = classic_devices();
            for (address, node) in nodes.iter_mut() {
                if let Some(&(connected, cod)) = classic.get(address) {
                    node.connected = Some(connected);
                    node.cod = node.cod.or(Some(cod));
                } else if let Some(container) = containers.get(address) {
                    node.connected = container_connected(container);
                }
            }

            // 3. GATT Battery Service of LE devices.
            let paths = hidio::interface_paths(&BATTERY_SERVICE);
            self.gatt.retain(|path, _| paths.contains(path));
            for path in paths {
                let Some(address) = hidio::interface_instance_id(&path).and_then(|id| bt_address(&id)) else { continue };
                let node = nodes.entry(address).or_insert_with(|| Node { address, le: true, ..Default::default() });
                if node.vendor.is_none() {
                    node.vendor = hidio::interface_instance_id(&path).and_then(|id| bt_vendor(&id)).map(|(s, v, _)| (s, v));
                }
                // Never even open the services of devices reported elsewhere.
                if handled_elsewhere(node.vendor, DeviceKind::Other, false) || node.connected == Some(false) {
                    continue;
                }
                if !self.gatt.contains_key(&path) {
                    match Gatt::open(&path, address, self.notify.clone()) {
                        Some(g) => {
                            log!("bluetooth: battery service {} ({})", format_address(address), path);
                            self.gatt.insert(path.clone(), g);
                        }
                        None => continue,
                    }
                }
                if let Some(g) = self.gatt.get_mut(&path) {
                    if let Some(level) = g.read(force) {
                        g.level = Some(level);
                    }
                    node.gatt_level = g.level.or(self.nodes.get(&address).and_then(|n| n.gatt_level));
                }
                self.appearance.entry(address).or_insert_with(|| read_appearance(address));
                node.appearance = self.appearance.get(&address).copied().flatten();
            }

            let seen: Vec<Seen> = {
                let mut v: Vec<&Node> = nodes.values().collect();
                v.sort_by_key(|n| n.address);
                v.into_iter().filter_map(|n| n.to_seen(self.gamepads)).collect()
            };
            self.nodes = nodes;
            let events = self.tracker.scan(seen);
            Self::publish(hub, events);
        }
    }

    /// Paired classic devices: address → (connected, class of device).
    fn classic_devices() -> HashMap<u64, (bool, u32)> {
        let mut out = HashMap::new();
        // SAFETY: plain data structures with their size set; the search
        // handle is closed below.
        unsafe {
            let mut params: BLUETOOTH_DEVICE_SEARCH_PARAMS = zeroed();
            params.dwSize = size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32;
            params.fReturnAuthenticated = 1;
            params.fReturnRemembered = 1;
            params.fReturnConnected = 1;
            let mut info: BLUETOOTH_DEVICE_INFO = zeroed();
            info.dwSize = size_of::<BLUETOOTH_DEVICE_INFO>() as u32;
            let find = BluetoothFindFirstDevice(&params, &mut info);
            if find.is_null() {
                return out;
            }
            loop {
                out.insert(info.Address.Anonymous.ullLong & 0xFFFF_FFFF_FFFF, (info.fConnected != 0, info.ulClassofDevice));
                info = zeroed();
                info.dwSize = size_of::<BLUETOOTH_DEVICE_INFO>() as u32;
                if BluetoothFindNextDevice(find, &mut info) == 0 {
                    break;
                }
            }
            BluetoothFindDeviceClose(find);
        }
        out
    }

    /// `DEVPKEY_DeviceContainer_IsConnected` of a device container.
    fn container_connected(container: &str) -> Option<bool> {
        let key = DEVPROPCOMPKEY { Key: DEVPKEY_DeviceContainer_IsConnected, Store: DEVPROP_STORE_SYSTEM, LocaleName: null() };
        let id = wide(container);
        let mut count = 0u32;
        let mut props: *mut DEVPROPERTY = null_mut();
        // SAFETY: valid key / id; the returned array is freed below.
        unsafe {
            let hr =
                DevGetObjectProperties(DevObjectTypeDeviceContainer, id.as_ptr(), DevQueryFlagNone as u32, 1, &key, &mut count, &mut props);
            if hr < 0 || props.is_null() {
                return None;
            }
            let result = std::slice::from_raw_parts(props, count as usize)
                .iter()
                .find(|p| p.Type == DEVPROP_TYPE_BOOLEAN && p.BufferSize >= 1 && !p.Buffer.is_null())
                .map(|p| *(p.Buffer as *const u8) != 0);
            DevFreeObjectProperties(count, props);
            result
        }
    }

    fn open_service(path: &str) -> Option<OwnedHandle> {
        hidio::open(path, GENERIC_READ | GENERIC_WRITE, 0).or_else(|| hidio::open(path, GENERIC_READ, 0))
    }

    /// The characteristic `uuid` of the (single) service behind `handle`.
    fn find_characteristic(handle: &OwnedHandle, uuid: u16) -> Option<BTH_LE_GATT_CHARACTERISTIC> {
        // SAFETY: the buffers are valid for the counts given; all
        // structures are plain data.
        unsafe {
            let mut services: [BTH_LE_GATT_SERVICE; 4] = zeroed();
            let mut n = 0u16;
            if BluetoothGATTGetServices(handle.0, services.len() as u16, services.as_mut_ptr(), &mut n, BLUETOOTH_GATT_FLAG_NONE) < 0 {
                return None;
            }
            for service in services.iter().take(n as usize) {
                let mut chars: [BTH_LE_GATT_CHARACTERISTIC; 16] = zeroed();
                let mut m = 0u16;
                if BluetoothGATTGetCharacteristics(
                    handle.0,
                    service,
                    chars.len() as u16,
                    chars.as_mut_ptr(),
                    &mut m,
                    BLUETOOTH_GATT_FLAG_NONE,
                ) < 0
                {
                    continue;
                }
                for c in chars.iter().take(m as usize) {
                    let u = &c.CharacteristicUuid;
                    let l = u.Value.LongUuid;
                    if uuid16(u.IsShortUuid, u.Value.ShortUuid, l.data1, l.data2, l.data3, l.data4) == Some(uuid) {
                        return Some(*c);
                    }
                }
            }
        }
        None
    }

    fn read_value(handle: &OwnedHandle, c: &BTH_LE_GATT_CHARACTERISTIC, force: bool) -> Option<Vec<u8>> {
        // u32 storage keeps the structure aligned.
        let mut buf = [0u32; 16];
        let mut needed = 0u16;
        let flags = if force { BLUETOOTH_GATT_FLAG_FORCE_READ_FROM_DEVICE } else { BLUETOOTH_GATT_FLAG_NONE };
        // SAFETY: the buffer is valid and aligned for the size given.
        unsafe {
            let value = buf.as_mut_ptr() as *mut BTH_LE_GATT_CHARACTERISTIC_VALUE;
            if BluetoothGATTGetCharacteristicValue(handle.0, c, (buf.len() * 4) as u32, value, &mut needed, flags) < 0 {
                return None;
            }
            let size = ((*value).DataSize as usize).min(buf.len() * 4 - 4);
            Some(std::slice::from_raw_parts((*value).Data.as_ptr(), size).to_vec())
        }
    }

    fn read_appearance(address: u64) -> Option<u16> {
        for path in hidio::interface_paths(&GAP_SERVICE) {
            if hidio::interface_instance_id(&path).and_then(|id| bt_address(&id)) != Some(address) {
                continue;
            }
            let handle = open_service(&path)?;
            let c = find_characteristic(&handle, APPEARANCE)?;
            let v = read_value(&handle, &c, false)?;
            return Some(u16::from_le_bytes([*v.first()?, *v.get(1)?]));
        }
        None
    }

    /// Context of a value-changed registration (read on a system thread).
    struct Context {
        address: u64,
        tx: Mutex<Sender<Msg>>,
    }

    unsafe extern "system" fn on_changed(kind: BTH_LE_GATT_EVENT_TYPE, param: *const c_void, context: *const c_void) {
        if kind != CharacteristicValueChangedEvent || param.is_null() || context.is_null() {
            return;
        }
        // SAFETY: Windows passes a BLUETOOTH_GATT_VALUE_CHANGED_EVENT for
        // this event type; `context` is the `Context` registered with it,
        // freed only after the registration is removed.
        unsafe {
            let event = &*(param as *const BLUETOOTH_GATT_VALUE_CHANGED_EVENT);
            let value = event.CharacteristicValue;
            if value.is_null() {
                return;
            }
            let size = ((*value).DataSize as usize).min(event.CharacteristicValueDataSize.saturating_sub(4));
            let data = std::slice::from_raw_parts((*value).Data.as_ptr(), size);
            let context = &*(context as *const Context);
            if let (Some(level), Ok(tx)) = (gatt_level(data), context.tx.lock()) {
                let _ = tx.send(Msg::Level(context.address, level));
            }
        }
    }

    /// An open Battery Service with its Battery Level characteristic.
    struct Gatt {
        handle: OwnedHandle,
        level_char: BTH_LE_GATT_CHARACTERISTIC,
        registration: isize,
        context: *mut Context,
        level: Option<u8>,
    }

    impl Gatt {
        fn open(path: &str, address: u64, tx: Sender<Msg>) -> Option<Self> {
            let handle = open_service(path)?;
            let level_char = find_characteristic(&handle, BATTERY_LEVEL)?;
            let mut gatt = Self { handle, level_char, registration: 0, context: null_mut(), level: None };
            if level_char.IsNotifiable {
                let context = Box::into_raw(Box::new(Context { address, tx: Mutex::new(tx) }));
                let mut reg: BLUETOOTH_GATT_VALUE_CHANGED_EVENT_REGISTRATION =
                    BLUETOOTH_GATT_VALUE_CHANGED_EVENT_REGISTRATION { NumCharacteristics: 1, Characteristics: [level_char] };
                let mut registration = 0isize;
                // SAFETY: `reg` and `context` are valid; the context lives
                // until the registration is removed (`Drop`).
                let hr = unsafe {
                    BluetoothGATTRegisterEvent(
                        gatt.handle.0,
                        CharacteristicValueChangedEvent,
                        (&mut reg as *mut BLUETOOTH_GATT_VALUE_CHANGED_EVENT_REGISTRATION).cast(),
                        Some(on_changed),
                        context as *const c_void,
                        &mut registration,
                        BLUETOOTH_GATT_FLAG_NONE,
                    )
                };
                if hr >= 0 {
                    gatt.registration = registration;
                    gatt.context = context;
                } else {
                    // SAFETY: never handed out.
                    drop(unsafe { Box::from_raw(context) });
                }
            }
            Some(gatt)
        }

        fn read(&self, force: bool) -> Option<u8> {
            gatt_level(&read_value(&self.handle, &self.level_char, force)?)
        }
    }

    // SAFETY: the context pointer is only dereferenced by the callback and
    // freed in `Drop`, after the registration is gone.
    unsafe impl Send for Gatt {}

    impl Drop for Gatt {
        fn drop(&mut self) {
            if self.registration != 0 {
                // SAFETY: the registration is ours; once removed, no
                // callback uses the context any more.
                unsafe { BluetoothGATTUnregisterEvent(self.registration, BLUETOOTH_GATT_FLAG_NONE) };
            }
            if !self.context.is_null() {
                // SAFETY: allocated with Box::into_raw in `open`.
                drop(unsafe { Box::from_raw(self.context) });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let a = 0xA0E9DB123456u64;
        assert_eq!(bt_address(r"BTHENUM\DEV_A0E9DB123456\7&1B2C3D4E&0&BLUETOOTHDEVICE_A0E9DB123456"), Some(a));
        assert_eq!(
            bt_address(r"BTHENUM\{0000111E-0000-1000-8000-00805F9B34FB}_VID&0001005D_PID&223B\7&2E5E3C7A&0&A0E9DB123456_C00000000"),
            Some(a)
        );
        assert_eq!(bt_address(r"bthle\dev_a0e9db123456\7&2a5d3d5e&0&a0e9db123456"), Some(a));
        assert_eq!(
            bt_address(
                r"BTHLEDEVICE\{0000180F-0000-1000-8000-00805F9B34FB}_DEV_VID&02046D_PID&B023_REV&0004_A0E9DB123456\8&36F5B0A6&0&000F"
            ),
            Some(a)
        );
        assert_eq!(bt_address(r"BTHLEDEVICE\{0000180F-0000-1000-8000-00805F9B34FB}_A0E9DB123456\8&1&0&000F"), Some(a));
        assert_eq!(bt_address(r"BTHENUM\{00001124-0000-1000-8000-00805F9B34FB}_LOCALMFG&0002\7&1&0&000000000000_00000000"), None);
        assert_eq!(bt_address(r"USB\VID_046D&PID_C52B\5&1"), None);
        assert_eq!(format_address(a), "A0:E9:DB:12:34:56");
        assert_eq!(device_id(a), "bt-a0e9db123456");
        let g = [0x8b, 0x7d, 0xd6, 0x2b, 0xeb, 0x8b, 0xd5, 0x48, 0x87, 0xe0, 0x6c, 0xda, 0x34, 0x28, 0x04, 0x0a];
        assert_eq!(guid_string(&g), "{2bd67d8b-8beb-48d5-87e0-6cda3428040a}");
    }

    #[test]
    fn vendors() {
        assert_eq!(bt_vendor(r"BTHENUM\{0000111E-0000-1000-8000-00805F9B34FB}_VID&0001005D_PID&223B\7&1"), Some((1, 0x005D, Some(0x223B))));
        assert_eq!(
            bt_vendor(r"BTHLEDEVICE\{0000180F-0000-1000-8000-00805F9B34FB}_DEV_VID&02046D_PID&B023_REV&0004_A0E9DB123456\8"),
            Some((2, 0x046D, Some(0xB023)))
        );
        assert_eq!(bt_vendor(r"BTHLE\DEV_A0E9DB123456\7&1"), None);
        assert!(handled_elsewhere(Some((2, 0x046D)), DeviceKind::Mouse, false));
        assert!(handled_elsewhere(Some((1, 0x01DA)), DeviceKind::Headset, false));
        assert!(!handled_elsewhere(Some((1, 0x046D)), DeviceKind::Headset, false));
        assert!(handled_elsewhere(Some((2, 0x045E)), DeviceKind::Gamepad, true));
        assert!(!handled_elsewhere(Some((2, 0x045E)), DeviceKind::Gamepad, false));
        assert!(!handled_elsewhere(Some((2, 0x045E)), DeviceKind::Mouse, true));
        assert!(!handled_elsewhere(None, DeviceKind::Other, true));
    }

    #[test]
    fn kinds() {
        // Major 4 (audio), minor 1 (wearable headset): 0x240404.
        assert_eq!(kind_from_cod(0x240404), Some(DeviceKind::Headset));
        // Headphones (minor 6).
        assert_eq!(kind_from_cod(0x240418), Some(DeviceKind::Headset));
        assert_eq!(kind_from_cod(0x002540), Some(DeviceKind::Keyboard));
        assert_eq!(kind_from_cod(0x002580), Some(DeviceKind::Mouse));
        assert_eq!(kind_from_cod(0x002508), Some(DeviceKind::Gamepad));
        assert_eq!(kind_from_cod(0x5A020C), None); // phone
        assert_eq!(kind_from_appearance(0x03C1), Some(DeviceKind::Keyboard));
        assert_eq!(kind_from_appearance(0x03C2), Some(DeviceKind::Mouse));
        assert_eq!(kind_from_appearance(0x03C4), Some(DeviceKind::Gamepad));
        assert_eq!(kind_from_appearance(0x0941), Some(DeviceKind::Earbuds));
        assert_eq!(kind_from_appearance(0x0943), Some(DeviceKind::Headset));
        assert_eq!(classify(Some(0x240418), None, "Galaxy Buds2 Pro"), DeviceKind::Earbuds);
        assert_eq!(classify(Some(0x240418), None, "WH-1000XM4"), DeviceKind::Headset);
        assert_eq!(classify(None, Some(0x03C2), "Buds mouse"), DeviceKind::Mouse);
        assert_eq!(classify(None, None, "Xbox Wireless Controller"), DeviceKind::Gamepad);
        assert_eq!(classify(None, None, "Speaker"), DeviceKind::Other);
    }

    #[test]
    fn gatt() {
        let base = [0x80, 0, 0, 0x80, 0x5F, 0x9B, 0x34, 0xFB];
        assert_eq!(uuid16(true, 0x2A19, 0, 0, 0, [0; 8]), Some(0x2A19));
        assert_eq!(uuid16(false, 0, 0x2A19, 0, 0x1000, base), Some(0x2A19));
        assert_eq!(uuid16(false, 0, 0x12342A19, 0, 0x1000, base), None);
        assert_eq!(gatt_level(&[57]), Some(57));
        assert_eq!(gatt_level(&[101]), None);
        assert_eq!(gatt_level(&[]), None);
    }

    #[test]
    fn nodes() {
        let mut n = Node { address: 1, name: Some("Buds".into()), property_level: Some(40), ..Default::default() };
        let s = n.to_seen(true).unwrap();
        assert_eq!((s.id.as_str(), s.kind, s.battery.unwrap().percent), ("bt-000000000001", DeviceKind::Earbuds, 40.0));
        n.gatt_level = Some(35);
        assert_eq!(n.to_seen(true).unwrap().battery.unwrap().percent, 35.0);
        n.manufacturer = Some("Samsung".into());
        n.model = Some("SM-R510".into());
        assert_eq!(n.to_seen(true).unwrap().info.model.as_deref(), Some("Samsung SM-R510"));
        n.connected = Some(false);
        assert!(n.to_seen(true).is_none());
        n.connected = None;
        n.vendor = Some((2, 0x046D));
        assert!(n.to_seen(true).is_none());
        let none = Node { address: 2, name: Some("Phone".into()), ..Default::default() };
        assert!(none.to_seen(true).is_none());
    }
}
