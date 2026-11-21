//! Device model shared by every backend (native HID++, G HUB) and every
//! consumer (tray, HTTP server).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceKind {
    Keyboard,
    Mouse,
    Headset,
    Gamepad,
    Earbuds,
    Other,
}

impl DeviceKind {
    /// Device type codes of HID++ feature 0x0005 (DeviceNameAndType).
    pub fn from_hidpp(code: u8) -> Self {
        match code {
            0 | 2 => Self::Keyboard,
            3 | 4 | 5 => Self::Mouse,
            8 => Self::Headset,
            _ => Self::Other,
        }
    }

    pub fn from_ghub(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "keyboard" => Self::Keyboard,
            "mouse" => Self::Mouse,
            "headset" | "headphones" | "speaker" => Self::Headset,
            "gamepad" | "controller" | "joystick" => Self::Gamepad,
            "earbuds" | "earphones" => Self::Earbuds,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyboard => "Keyboard",
            Self::Mouse => "Mouse",
            Self::Headset => "Headset",
            Self::Gamepad => "Gamepad",
            Self::Earbuds => "Earbuds",
            Self::Other => "Other",
        }
    }

    /// Inverse of `as_str` (case insensitive), also accepts `from_ghub` names.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => None,
            "other" => Some(Self::Other),
            name => Some(Self::from_ghub(name)).filter(|k| *k != Self::Other),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Discharging,
    Charging,
    Full,
    NotCharging,
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discharging => "discharging",
            Self::Charging => "charging",
            Self::Full => "full",
            Self::NotCharging => "not_charging",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// Logitech HID++ (receivers, Bluetooth, cable).
    Native,
    /// Logitech G HUB.
    GHub,
    /// Standard Bluetooth battery (GATT Battery Service or hands-free profile).
    Bluetooth,
    /// Game controllers (Xbox / XInput, DualSense, DualShock 4…).
    Gamepad,
    /// Other brands spoken to over HID (Razer, Corsair, SteelSeries…).
    Vendor,
    /// Reported by another LGSTray instance on the network.
    Remote,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::GHub => "ghub",
            Self::Bluetooth => "bluetooth",
            Self::Gamepad => "gamepad",
            Self::Vendor => "vendor",
            Self::Remote => "remote",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Battery {
    /// 0..=100
    pub percent: f32,
    pub status: Status,
    pub millivolts: Option<u16>,
    /// G HUB's estimate of remaining battery life (hours).
    pub mileage: Option<f64>,
}

/// Extra, mostly static, information about a device. Every field is
/// optional: backends fill in what they know.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceInfo {
    pub firmware: Option<String>,
    pub serial: Option<String>,
    /// Manufacturer / model when it is not obvious from the name.
    pub model: Option<String>,
    /// Easy-Switch: (current host channel 1-based, number of channels).
    pub host_channel: Option<(u8, u8)>,
    /// Receiver the device is paired to (display name), if any.
    pub receiver: Option<String>,
    /// Link quality 0..=100 when the receiver reports it.
    pub signal: Option<u8>,
    /// Computer the device is attached to (remote devices).
    pub host: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub source: Source,
    pub has_battery: bool,
    pub battery: Option<Battery>,
    pub online: bool,
    pub last_update: Option<SystemTime>,
    pub info: DeviceInfo,
}

#[derive(Clone, Debug)]
pub enum Event {
    Init {
        id: String,
        name: String,
        kind: DeviceKind,
        source: Source,
        has_battery: bool,
    },
    Update {
        id: String,
        battery: Battery,
    },
    Offline {
        id: String,
    },
    /// Merges the `Some` fields into the device information.
    Info {
        id: String,
        info: DeviceInfo,
    },
    /// The device is gone for good (unpaired, remote instance forgotten).
    Remove {
        id: String,
    },
}

impl Event {
    pub fn id(&self) -> &str {
        match self {
            Event::Init { id, .. }
            | Event::Update { id, .. }
            | Event::Offline { id }
            | Event::Info { id, .. }
            | Event::Remove { id } => id,
        }
    }
}

type Listener = Arc<dyn Fn(&Device) + Send + Sync>;

/// Thread-safe device registry. Backends publish events, consumers take
/// snapshots; subscribers are called (outside of any lock, on the
/// publishing thread) with the device after every visible change.
#[derive(Default)]
pub struct Hub {
    devices: Mutex<Vec<Device>>,
    listeners: Mutex<Vec<Listener>>,
    /// Bumped on every visible change, for long-polling consumers (SSE).
    generation: Mutex<u64>,
    changed: Condvar,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called with no argument after every visible change (UI wake-up).
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        self.subscribe(move |_| f());
    }

    /// Called with the changed device after every visible change. A removed
    /// device is passed with `online == false` and `has_battery == false`.
    pub fn subscribe(&self, f: impl Fn(&Device) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Arc::new(f));
    }

    pub fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }

    /// Blocks until the generation differs from `seen` or `timeout` elapses;
    /// returns the current generation.
    pub fn wait_change(&self, seen: u64, timeout: Duration) -> u64 {
        let guard = self.generation.lock().unwrap();
        let (guard, _) = self.changed.wait_timeout_while(guard, timeout, |g| *g == seen).unwrap();
        *guard
    }

    pub fn snapshot(&self) -> Vec<Device> {
        self.devices.lock().unwrap().clone()
    }

    /// Look a device up by id first, then by name (HTTP API semantics).
    pub fn find(&self, key: &str) -> Option<Device> {
        let devices = self.devices.lock().unwrap();
        devices
            .iter()
            .find(|d| d.id == key)
            .or_else(|| devices.iter().find(|d| d.name == key))
            .cloned()
    }

    pub fn publish(&self, event: Event) {
        let id = event.id().to_string();
        let changed = {
            let mut devices = self.devices.lock().unwrap();
            let removed = match &event {
                Event::Remove { id } => devices.iter().find(|d| d.id == *id).cloned(),
                _ => None,
            };
            apply(&mut devices, event).then(|| {
                removed
                    .map(|mut d| {
                        d.online = false;
                        d.has_battery = false;
                        d
                    })
                    .or_else(|| devices.iter().find(|d| d.id == id).cloned())
            })
        };
        if let Some(device) = changed {
            *self.generation.lock().unwrap() += 1;
            self.changed.notify_all();
            let listeners: Vec<Listener> = self.listeners.lock().unwrap().clone();
            if let Some(device) = device {
                for f in &listeners {
                    f(&device);
                }
            }
        }
    }
}

/// Applies an event; returns true when something visible changed.
fn apply(devices: &mut Vec<Device>, event: Event) -> bool {
    match event {
        Event::Init { id, name, kind, source, has_battery } => {
            if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
                let changed = d.name != name || d.kind != kind || d.has_battery != has_battery || !d.online;
                d.name = name;
                d.kind = kind;
                d.source = source;
                d.has_battery = has_battery;
                d.online = true;
                changed
            } else {
                devices.push(Device {
                    id,
                    name,
                    kind,
                    source,
                    has_battery,
                    battery: None,
                    online: true,
                    last_update: None,
                    info: DeviceInfo::default(),
                });
                devices.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
                true
            }
        }
        Event::Update { id, battery } => match devices.iter_mut().find(|d| d.id == id) {
            Some(d) => {
                let changed = d.battery != Some(battery) || !d.online;
                d.battery = Some(battery);
                d.online = true;
                d.last_update = Some(SystemTime::now());
                changed
            }
            None => false,
        },
        Event::Offline { id } => match devices.iter_mut().find(|d| d.id == id) {
            Some(d) if d.online => {
                d.online = false;
                true
            }
            _ => false,
        },
        Event::Info { id, info } => match devices.iter_mut().find(|d| d.id == id) {
            Some(d) => {
                let before = d.info.clone();
                let i = &mut d.info;
                macro_rules! merge {
                    ($($f:ident),*) => { $(if info.$f.is_some() { i.$f = info.$f; })* };
                }
                merge!(firmware, serial, model, host_channel, receiver, signal, host);
                d.info != before
            }
            None => false,
        },
        Event::Remove { id } => {
            let before = devices.len();
            devices.retain(|d| d.id != id);
            devices.len() != before
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn init(id: &str, name: &str) -> Event {
        Event::Init { id: id.into(), name: name.into(), kind: DeviceKind::Mouse, source: Source::Native, has_battery: true }
    }

    #[test]
    fn hub_tracks_devices_and_notifies_only_on_change() {
        let hub = Hub::new();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        hub.on_change(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        });
        hub.publish(init("b", "Zeta"));
        hub.publish(init("a", "alpha"));
        hub.publish(init("a", "alpha")); // no change
        assert_eq!(n.load(Ordering::SeqCst), 2);
        let snap = hub.snapshot();
        assert_eq!(snap.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);

        let bat = Battery { percent: 80.0, status: Status::Discharging, millivolts: None, mileage: None };
        hub.publish(Event::Update { id: "a".into(), battery: bat });
        hub.publish(Event::Update { id: "a".into(), battery: bat });
        hub.publish(Event::Update { id: "missing".into(), battery: bat });
        assert_eq!(n.load(Ordering::SeqCst), 3);
        hub.publish(Event::Offline { id: "a".into() });
        hub.publish(Event::Offline { id: "a".into() });
        assert_eq!(n.load(Ordering::SeqCst), 4);
        assert!(!hub.find("alpha").unwrap().online);
        assert_eq!(hub.find("a").unwrap().battery.unwrap().percent, 80.0);

        let info = DeviceInfo { firmware: Some("RQR39.06".into()), host_channel: Some((2, 3)), ..Default::default() };
        let generation = hub.generation();
        hub.publish(Event::Info { id: "a".into(), info: info.clone() });
        hub.publish(Event::Info { id: "a".into(), info: DeviceInfo { signal: Some(80), ..Default::default() } });
        hub.publish(Event::Info { id: "a".into(), info: info.clone() }); // no change
        let d = hub.find("a").unwrap();
        assert_eq!((d.info.firmware.as_deref(), d.info.host_channel, d.info.signal), (Some("RQR39.06"), Some((2, 3)), Some(80)));
        assert_eq!(n.load(Ordering::SeqCst), 6);
        assert_eq!(hub.wait_change(generation, Duration::from_millis(1)), generation + 2);

        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        hub.subscribe(move |d| seen2.lock().unwrap().push((d.id.clone(), d.has_battery)));
        hub.publish(Event::Remove { id: "a".into() });
        hub.publish(Event::Remove { id: "a".into() });
        assert!(hub.find("a").is_none());
        assert_eq!(*seen.lock().unwrap(), [("a".to_string(), false)]);
    }

    #[test]
    fn kind_names() {
        for k in [DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Headset, DeviceKind::Gamepad, DeviceKind::Earbuds, DeviceKind::Other] {
            assert_eq!(DeviceKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(DeviceKind::parse("auto"), None);
        assert_eq!(DeviceKind::parse(""), None);
    }
}
