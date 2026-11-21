//! The same device can be reached through several endpoints at once: a
//! keyboard on its Lightspeed receiver *and* on its USB cable, or on a
//! receiver and over Bluetooth. Its id comes from its serial number, so
//! every endpoint reports the same id.
//!
//! Without care, the endpoint that loses the device ("link down" from the
//! receiver once the cable takes over) marks it offline although another
//! endpoint still reads it. [`Presence`] remembers which endpoints currently
//! see each device and lets `Offline` / `Remove` through only when the last
//! one lets go.

use crate::model::Event;
use std::collections::HashMap;

#[derive(Default)]
pub struct Presence {
    /// Device id -> endpoints that see it, most recent first.
    seen: HashMap<String, Vec<String>>,
}

impl Presence {
    /// Records an event from `endpoint`; returns whether it should reach
    /// the hub.
    pub fn route(&mut self, endpoint: &str, event: &Event) -> bool {
        match event {
            Event::Init { id, .. } | Event::Update { id, .. } => {
                let list = self.seen.entry(id.clone()).or_default();
                list.retain(|e| e != endpoint);
                list.insert(0, endpoint.to_string());
                true
            }
            Event::Offline { id } | Event::Remove { id } => {
                let Some(list) = self.seen.get_mut(id) else { return true };
                list.retain(|e| e != endpoint);
                if list.is_empty() {
                    self.seen.remove(id);
                    true
                } else {
                    false
                }
            }
            Event::Info { .. } => true,
        }
    }

    /// Endpoint that saw the device most recently (target of its commands).
    pub fn owner(&self, id: &str) -> Option<String> {
        self.seen.get(id).and_then(|list| list.first().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DeviceKind, Source, Status};

    fn init(id: &str) -> Event {
        Event::Init { id: id.into(), name: "PRO X 60".into(), kind: DeviceKind::Keyboard, source: Source::Native, has_battery: true }
    }
    fn update(id: &str) -> Event {
        Event::Update { id: id.into(), battery: Battery { percent: 95.0, status: Status::Charging, millivolts: None, mileage: None } }
    }
    fn offline(id: &str) -> Event {
        Event::Offline { id: id.into() }
    }

    #[test]
    fn link_down_on_one_endpoint_keeps_the_device_online() {
        let mut p = Presence::default();
        assert!(p.route("receiver", &init("K")));
        assert!(p.route("cable", &init("K")));
        // The receiver loses the keyboard once the cable is plugged in.
        assert!(!p.route("receiver", &offline("K")));
        assert_eq!(p.owner("K").as_deref(), Some("cable"));
        assert!(p.route("cable", &update("K")));
        // Cable unplugged and the receiver not back yet: really offline.
        assert!(p.route("cable", &offline("K")));
        assert_eq!(p.owner("K"), None);
        // The receiver finds it again.
        assert!(p.route("receiver", &update("K")));
        assert_eq!(p.owner("K").as_deref(), Some("receiver"));
    }

    #[test]
    fn remove_only_when_no_endpoint_is_left() {
        let mut p = Presence::default();
        p.route("a", &init("K"));
        p.route("b", &init("K"));
        assert!(!p.route("a", &Event::Remove { id: "K".into() }));
        assert!(p.route("b", &Event::Remove { id: "K".into() }));
        // Unknown device, or one already gone: let it through.
        assert!(p.route("a", &offline("X")));
        assert!(p.route("a", &offline("K")));
    }

    #[test]
    fn single_endpoint_behaves_as_before() {
        let mut p = Presence::default();
        assert!(p.route("r", &init("M")));
        assert!(p.route("r", &offline("M")));
        assert!(p.route("r", &update("M")));
        assert!(p.route("r", &offline("M")));
    }
}
