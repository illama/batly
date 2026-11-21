//! Turns the periodic readings of a polling backend (Bluetooth, game
//! controllers, other brands) into hub events: `Init` the first time a
//! device is seen (or when its name / kind changes), `Info` and `Update`
//! with what was read, `Offline` once it is no longer reported.

use crate::model::{Battery, DeviceInfo, DeviceKind, Event, Source};
use std::collections::HashMap;

/// One device as seen by a scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    /// Stable identifier (survives restarts), unique across backends.
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    /// False for devices without a battery (wired controller…).
    pub has_battery: bool,
    /// Last reading; `None` when the level could not be read this time.
    pub battery: Option<Battery>,
    pub info: DeviceInfo,
}

#[derive(Clone, Debug, PartialEq)]
struct Known {
    name: String,
    kind: DeviceKind,
    has_battery: bool,
    online: bool,
    info: DeviceInfo,
}

pub struct Tracker {
    source: Source,
    known: HashMap<String, Known>,
}

impl Tracker {
    pub fn new(source: Source) -> Self {
        Self { source, known: HashMap::new() }
    }

    /// Events for a complete scan: every device in `seen` is online, every
    /// other known device goes offline.
    pub fn scan(&mut self, seen: Vec<Seen>) -> Vec<Event> {
        let mut events = Vec::new();
        let ids: Vec<String> = seen.iter().map(|s| s.id.clone()).collect();
        for s in seen {
            self.seen(s, &mut events);
        }
        let mut gone: Vec<&String> = self.known.keys().filter(|id| !ids.contains(id)).collect();
        gone.sort();
        let gone: Vec<String> = gone.into_iter().cloned().collect();
        for id in gone {
            events.extend(self.offline(&id));
        }
        events
    }

    /// Events for a single device that just reported (partial update).
    pub fn one(&mut self, seen: Seen) -> Vec<Event> {
        let mut events = Vec::new();
        self.seen(seen, &mut events);
        events
    }

    fn offline(&mut self, id: &str) -> Option<Event> {
        let known = self.known.get_mut(id)?;
        if !known.online {
            return None;
        }
        known.online = false;
        Some(Event::Offline { id: id.to_string() })
    }

    fn seen(&mut self, s: Seen, events: &mut Vec<Event>) {
        let fresh = Known { name: s.name.clone(), kind: s.kind, has_battery: s.has_battery, online: true, info: s.info.clone() };
        let previous = self.known.insert(s.id.clone(), fresh);
        let reinit = previous.as_ref().is_none_or(|k| {
            !k.online || k.name != s.name || k.kind != s.kind || k.has_battery != s.has_battery
        });
        if reinit {
            events.push(Event::Init {
                id: s.id.clone(),
                name: s.name,
                kind: s.kind,
                source: self.source,
                has_battery: s.has_battery,
            });
        }
        if s.info != DeviceInfo::default() && (reinit || previous.is_none_or(|k| k.info != s.info)) {
            events.push(Event::Info { id: s.id.clone(), info: s.info });
        }
        if let (true, Some(battery)) = (s.has_battery, s.battery) {
            events.push(Event::Update { id: s.id, battery });
        }
    }
}

/// Keeps the characters that are safe in an id (`[a-z0-9-]`), lowercased;
/// runs of anything else become a single `-`.
pub fn id_part(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    fn bat(p: f32) -> Battery {
        Battery { percent: p, status: Status::Discharging, millivolts: None, mileage: None }
    }

    fn seen(id: &str, p: Option<f32>) -> Seen {
        Seen { id: id.into(), name: "Pad".into(), kind: DeviceKind::Gamepad, has_battery: true, battery: p.map(bat), info: DeviceInfo::default() }
    }

    #[test]
    fn scan_events() {
        let mut t = Tracker::new(Source::Gamepad);
        let ev = t.scan(vec![seen("a", Some(50.0))]);
        assert!(matches!(&ev[..], [Event::Init { source: Source::Gamepad, .. }, Event::Update { .. }]));
        // Second scan: no new Init.
        let ev = t.scan(vec![seen("a", Some(40.0))]);
        assert!(matches!(&ev[..], [Event::Update { .. }]));
        // Unreadable level: nothing at all.
        assert!(t.scan(vec![seen("a", None)]).is_empty());
        // Gone: offline once.
        assert!(matches!(&t.scan(vec![])[..], [Event::Offline { .. }]));
        assert!(t.scan(vec![]).is_empty());
        // Back: Init again.
        let mut s = seen("a", None);
        s.info.model = Some("X".into());
        let ev = t.scan(vec![s.clone()]);
        assert!(matches!(&ev[..], [Event::Init { .. }, Event::Info { .. }]));
        assert!(t.one(s).is_empty());
        assert!(matches!(&t.one(seen("a", Some(1.0)))[..], [Event::Update { .. }]));
    }

    #[test]
    fn ids() {
        assert_eq!(id_part(r"USB\VID_054C&PID_0CE6\5&2B"), "usb-vid-054c-pid-0ce6-5-2b");
        assert_eq!(id_part("--A:b--"), "a-b");
    }
}
