//! Device model shared by every backend (native HID++, G HUB) and every
//! consumer (tray, HTTP server).

use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceKind {
    Keyboard,
    Mouse,
    Headset,
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
            "headset" => Self::Headset,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyboard => "Keyboard",
            Self::Mouse => "Mouse",
            Self::Headset => "Headset",
            Self::Other => "Other",
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
    Native,
    GHub,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::GHub => "ghub",
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
}

type Listener = Box<dyn Fn() + Send + Sync>;

/// Thread-safe device registry. Backends publish events, consumers take
/// snapshots; `on_change` wakes the UI thread.
#[derive(Default)]
pub struct Hub {
    devices: Mutex<Vec<Device>>,
    listener: Mutex<Option<Listener>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.listener.lock().unwrap() = Some(Box::new(f));
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
        let changed = {
            let mut devices = self.devices.lock().unwrap();
            apply(&mut devices, event)
        };
        if changed {
            if let Some(f) = self.listener.lock().unwrap().as_ref() {
                f();
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
    }
}
