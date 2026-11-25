//! Logitech G HUB backend: G HUB's agent exposes a local WebSocket
//! (`ws://localhost:9010`) that reports the devices it manages and their
//! battery state.
//!
//! The thread blocks on the socket (no polling); when G HUB is not running
//! it retries at a slow pace. A manual refresh simply drops the connection,
//! which makes the loop reconnect and re-list everything.

use crate::log;
use crate::model::{Battery, DeviceKind, Event, Hub, Source, Status};
use serde_json::{json, Value};
use std::io;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::{Message, WebSocket};

const ENDPOINT: &str = "ws://localhost:9010";
const RECONNECT: Duration = Duration::from_secs(30);

/// Set while Batly is connected to the G HUB agent: G HUB runs (checked
/// with its processes by the native backend, see `hidpp::GHUB_RUNNING`).
pub static LINKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct GHub {
    wake: Sender<()>,
    socket: Arc<Mutex<Option<TcpStream>>>,
}

impl GHub {
    pub fn start(hub: Arc<Hub>) -> Self {
        let (wake, wake_rx) = mpsc::channel();
        let socket = Arc::new(Mutex::new(None));
        let shared = socket.clone();
        let _ = thread::Builder::new().name("ghub".into()).stack_size(256 * 1024).spawn(move || {
            let mut known: Vec<String> = Vec::new();
            loop {
                let result = session(&hub, &shared, &mut known);
                LINKED.store(false, std::sync::atomic::Ordering::Relaxed);
                log!("ghub: disconnected ({:?})", result.err());
                *shared.lock().unwrap() = None;
                for id in known.drain(..) {
                    hub.publish(Event::Offline { id });
                }
                // Sleep until the next attempt, or until a refresh is requested.
                match wake_rx.recv_timeout(RECONNECT) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => while wake_rx.try_recv().is_ok() {},
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Self { wake, socket }
    }

    /// Reconnects and re-reads every device.
    pub fn refresh(&self) {
        if let Some(stream) = self.socket.lock().unwrap().as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = self.wake.send(());
    }
}

fn connect() -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    for addr in [SocketAddr::from(([127, 0, 0, 1], 9010)), SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 9010))] {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

type Error = Box<dyn std::error::Error>;

fn session(hub: &Hub, shared: &Mutex<Option<TcpStream>>, known: &mut Vec<String>) -> Result<(), Error> {
    let stream = connect()?;
    stream.set_nodelay(true)?;
    // Re-list the devices every minute even when nothing is said (a device
    // marked offline elsewhere comes back).
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    *shared.lock().unwrap() = Some(stream.try_clone()?);

    let mut request = ENDPOINT.into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("Origin", HeaderValue::from_static("file://"));
    headers.insert("Pragma", HeaderValue::from_static("no-cache"));
    headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
    headers.insert("Sec-WebSocket-Protocol", HeaderValue::from_static("json"));
    let (mut ws, _) = tungstenite::client(request, stream).map_err(|e| e.to_string())?;
    log!("ghub: connected");
    LINKED.store(true, std::sync::atomic::Ordering::Relaxed);
    // G HUB id -> id shown (the Logitech twin's, see `model::twin_of`).
    let mut ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    send(&mut ws, "SUBSCRIBE", "/devices/state/changed")?;
    send(&mut ws, "SUBSCRIBE", "/battery/state/changed")?;
    send(&mut ws, "GET", "/devices/list")?;

    loop {
        let text = match ws.read() {
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                send(&mut ws, "GET", "/devices/list")?;
                continue;
            }
            Err(e) => return Err(e.into()),
            Ok(Message::Text(t)) => t.to_string(),
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => continue,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&text) else { continue };
        let path = msg["path"].as_str().unwrap_or("");
        let payload = &msg["payload"];
        log!("ghub: < {path}");

        if path == "/devices/list" {
            for (event, id) in parse_device_list(payload) {
                // A device Batly read itself keeps the id Batly knows.
                let event = match event {
                    Event::Init { name, kind, source, has_battery, .. } => {
                        let shown = crate::model::twin_of(&name).unwrap_or_else(|| id.clone());
                        ids.insert(id.clone(), shown.clone());
                        Event::Init { id: shown, name, kind, source, has_battery }
                    }
                    other => other,
                };
                let shown = event.id().to_string();
                if !known.contains(&shown) {
                    known.push(shown);
                }
                hub.publish(event);
                send(&mut ws, "GET", &format!("/battery/{id}/state"))?;
            }
        } else if path == "/devices/state/changed" {
            // A device appeared, disappeared or changed: re-list.
            send(&mut ws, "GET", "/devices/list")?;
        } else if is_battery_path(path) {
            if let Some(Event::Update { id, battery }) = parse_battery(payload) {
                let id = ids.get(&id).cloned().unwrap_or(id);
                hub.publish(Event::Update { id, battery });
            }
        }
    }
}

fn send(ws: &mut WebSocket<TcpStream>, verb: &str, path: &str) -> Result<(), Error> {
    let msg = json!({ "msgId": "", "verb": verb, "path": path });
    ws.send(Message::text(msg.to_string()))?;
    Ok(())
}

/// `/battery/state/changed` or `/battery/dev<id>/state`.
fn is_battery_path(path: &str) -> bool {
    if path == "/battery/state/changed" {
        return true;
    }
    path.strip_prefix("/battery/dev")
        .and_then(|rest| rest.strip_suffix("/state"))
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

pub fn parse_device_list(payload: &Value) -> Vec<(Event, String)> {
    let Some(infos) = payload["deviceInfos"].as_array() else { return Vec::new() };
    infos
        .iter()
        .filter_map(|d| {
            let id = d["id"].as_str()?.to_string();
            let name = d["extendedDisplayName"].as_str().or_else(|| d["displayName"].as_str())?.to_string();
            let event = Event::Init {
                id: id.clone(),
                name,
                kind: DeviceKind::from_ghub(d["deviceType"].as_str().unwrap_or("")),
                source: Source::GHub,
                has_battery: d["capabilities"]["hasBatteryStatus"].as_bool().unwrap_or(false),
            };
            Some((event, id))
        })
        .collect()
}

pub fn parse_battery(payload: &Value) -> Option<Event> {
    let id = payload["deviceId"].as_str()?.to_string();
    let percent = payload["percentage"].as_f64()?.clamp(0.0, 100.0) as f32;
    let charging = payload["charging"].as_bool().unwrap_or(false);
    let full = payload["fullyCharged"].as_bool().unwrap_or(false);
    let status = match (charging, full) {
        (_, true) => Status::Full,
        (true, _) => Status::Charging,
        _ => Status::Discharging,
    };
    Some(Event::Update { id, battery: Battery { percent, status, millivolts: None, mileage: payload["mileage"].as_f64() } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_paths() {
        assert!(is_battery_path("/battery/state/changed"));
        assert!(is_battery_path("/battery/dev00000001/state"));
        assert!(!is_battery_path("/battery/dev/state"));
        assert!(!is_battery_path("/battery/dev00/01/state"));
        assert!(!is_battery_path("/devices/list"));
    }

    #[test]
    fn device_list() {
        let payload = json!({ "deviceInfos": [
            { "id": "dev00000001", "extendedDisplayName": "PRO X Wireless", "deviceType": "HEADSET",
              "capabilities": { "hasBatteryStatus": true } },
            { "id": "dev00000002", "displayName": "G915", "deviceType": "KEYBOARD", "capabilities": {} },
            { "extendedDisplayName": "no id" }
        ]});
        let list = parse_device_list(&payload);
        assert_eq!(list.len(), 2);
        assert!(
            matches!(&list[0].0, Event::Init { name, kind: DeviceKind::Headset, has_battery: true, source: Source::GHub, .. } if name == "PRO X Wireless")
        );
        assert!(matches!(&list[1].0, Event::Init { kind: DeviceKind::Keyboard, has_battery: false, .. }));
        assert!(parse_device_list(&json!(null)).is_empty());
    }

    #[test]
    fn battery_update() {
        let e = parse_battery(&json!({ "deviceId": "dev1", "percentage": 87, "charging": true, "mileage": 41.5 })).unwrap();
        assert!(matches!(e, Event::Update { id, battery } if id == "dev1" && battery.percent == 87.0
            && battery.status == Status::Charging && battery.mileage == Some(41.5)));
        assert!(parse_battery(&json!({ "percentage": 87 })).is_none());
    }
}
