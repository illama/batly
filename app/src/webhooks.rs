//! Webhooks: a JSON POST to every configured URL on each alert and, with
//! `allChanges`, on each change of level, charging state or connection.
//!
//! Discord (`discord.com/api/webhooks/…`) and Slack (`hooks.slack.com`)
//! URLs get a message in their own format; other URLs get a generic object:
//! `{"event": "low_battery", "title", "text", "device": {...}, "percent",
//! "hours_left", "timestamp", "source": "lgstray", "version"}`.
//!
//! Sending never blocks the caller: messages go through a bounded queue to
//! a worker thread, and are dropped when the queue is full.

use crate::config;
use crate::fetch::{self, Url};
use crate::http;
use crate::i18n;
use crate::log;
use crate::model::{Device, Status};
use crate::notice::Notice;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

const QUEUE: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Generic,
    Discord,
    Slack,
}

impl Format {
    pub fn of(url: &str) -> Self {
        let Some(u) = Url::parse(url) else { return Self::Generic };
        let host = u.host.to_ascii_lowercase();
        let discord = ["discord.com", "discordapp.com", "ptb.discord.com", "canary.discord.com"].contains(&host.as_str());
        if discord && u.path.starts_with("/api/webhooks/") {
            Self::Discord
        } else if host == "hooks.slack.com" {
            Self::Slack
        } else {
            Self::Generic
        }
    }
}

/// What is sent: an alert, or a device change.
#[derive(Clone, Debug)]
pub enum Message {
    Notice(Notice),
    Change(Device),
}

/// Request body for one URL format.
pub fn body(format: Format, msg: &Message) -> String {
    let (title, text) = match msg {
        Message::Notice(n) => (n.title.clone(), n.text.clone()),
        Message::Change(d) => (d.name.clone(), i18n::describe(d)),
    };
    match format {
        Format::Discord => json!({ "content": format!("**{}**\n{}", title, text), "username": "LGSTray" }).to_string(),
        Format::Slack => json!({ "text": format!("*{}*\n{}", title, text) }).to_string(),
        Format::Generic => {
            let timestamp = http::iso8601(SystemTime::now());
            let v = match msg {
                Message::Notice(n) => json!({
                    "event": n.kind.as_str(),
                    "title": title,
                    "text": text,
                    "device": if n.device_id.is_empty() { Value::Null } else { json!({ "id": n.device_id, "name": n.device_name }) },
                    "percent": n.percent,
                    "hours_left": n.hours_left,
                    "timestamp": timestamp,
                    "source": "lgstray",
                    "version": http::VERSION,
                }),
                Message::Change(d) => json!({
                    "event": "change",
                    "title": title,
                    "text": text,
                    "device": serde_json::from_str::<Value>(&http::json(d)).unwrap_or(Value::Null),
                    "percent": d.battery.map(|b| b.percent.round()),
                    "hours_left": Value::Null,
                    "timestamp": timestamp,
                    "source": "lgstray",
                    "version": http::VERSION,
                }),
            };
            v.to_string()
        }
    }
}

/// Handle to the sender thread; cheap to clone (e.g. into a hub listener).
#[derive(Clone)]
pub struct Webhooks {
    tx: Option<SyncSender<Message>>,
    all_changes: bool,
    /// Last (level, status, online) sent per device, to skip non-changes.
    last: Arc<Mutex<HashMap<String, (i32, Status, bool)>>>,
}

impl Webhooks {
    pub fn start(cfg: &config::Webhooks) -> Self {
        let urls: Vec<String> = cfg.urls.iter().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect();
        let tx = (!urls.is_empty()).then(|| {
            let (tx, rx) = mpsc::sync_channel::<Message>(QUEUE);
            let _ = thread::Builder::new().name("webhooks".into()).stack_size(256 * 1024).spawn(move || {
                // Ends when every handle is dropped.
                for msg in rx {
                    for url in &urls {
                        let format = Format::of(url);
                        match fetch::post_json(url, &body(format, &msg), Duration::from_secs(15)) {
                            Ok(r) if r.ok() => {}
                            Ok(r) => log!("webhook {url}: HTTP {}", r.status),
                            Err(e) => log!("webhook {url}: {e}"),
                        }
                    }
                }
            });
            tx
        });
        Self { tx, all_changes: cfg.all_changes, last: Arc::default() }
    }

    fn queue(&self, msg: Message) {
        if let Some(tx) = &self.tx {
            if let Err(TrySendError::Full(_)) = tx.try_send(msg) {
                log!("webhooks: queue full, message dropped");
            }
        }
    }

    /// Sends an alert to every URL.
    pub fn send(&self, notice: &Notice) {
        self.queue(Message::Notice(notice.clone()));
    }

    /// With `allChanges`, sends the device when its level (whole percent),
    /// charging state or connection changed. Meant for `Hub::subscribe`.
    pub fn device_changed(&self, d: &Device) {
        if !self.all_changes || self.tx.is_none() || !d.has_battery {
            return;
        }
        let Some(b) = d.battery else { return };
        let key = (b.percent.round() as i32, b.status, d.online);
        if self.last.lock().unwrap().insert(d.id.clone(), key) != Some(key) {
            self.queue(Message::Change(d.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DeviceKind, Source};
    use crate::notice::NoticeKind;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn notice() -> Notice {
        Notice {
            kind: NoticeKind::LowBattery,
            device_id: "abc".into(),
            device_name: "Souris".into(),
            percent: Some(12.0),
            hours_left: Some(3.5),
            title: "Batterie faible".into(),
            text: "Souris : 12 % restants".into(),
        }
    }

    fn device(percent: f32) -> Device {
        Device {
            id: "abc".into(),
            name: "G502".into(),
            kind: DeviceKind::Mouse,
            source: Source::Native,
            has_battery: true,
            battery: Some(Battery { percent, status: Status::Discharging, millivolts: None, mileage: None }),
            online: true,
            last_update: None,
            info: Default::default(),
        }
    }

    #[test]
    fn formats() {
        assert_eq!(Format::of("https://discord.com/api/webhooks/1/xyz"), Format::Discord);
        assert_eq!(Format::of("https://discordapp.com/api/webhooks/1/xyz"), Format::Discord);
        assert_eq!(Format::of("https://discord.com/other"), Format::Generic);
        assert_eq!(Format::of("https://hooks.slack.com/services/T/B/X"), Format::Slack);
        assert_eq!(Format::of("http://homeassistant.local:8123/api/webhook/lgs"), Format::Generic);

        let msg = Message::Notice(notice());
        let v: Value = serde_json::from_str(&body(Format::Discord, &msg)).unwrap();
        assert_eq!(v["content"], "**Batterie faible**\nSouris : 12 % restants");
        let v: Value = serde_json::from_str(&body(Format::Slack, &msg)).unwrap();
        assert_eq!(v["text"], "*Batterie faible*\nSouris : 12 % restants");
        let v: Value = serde_json::from_str(&body(Format::Generic, &msg)).unwrap();
        assert_eq!((v["event"].as_str(), v["device"]["id"].as_str(), v["percent"].as_f64()), (Some("low_battery"), Some("abc"), Some(12.0)));
        assert_eq!(v["hours_left"], 3.5);
        let v: Value = serde_json::from_str(&body(Format::Generic, &Message::Change(device(55.4)))).unwrap();
        assert_eq!((v["event"].as_str(), v["device"]["battery_percent"].as_i64(), v["percent"].as_f64()), (Some("change"), Some(55), Some(55.0)));
    }

    #[test]
    fn posts_in_the_background() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                // Headers, then Content-Length bytes.
                loop {
                    let n = s.read(&mut buf).unwrap();
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let len: usize = text
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if req.len() >= i + 4 + len {
                            let _ = tx.send(text[i + 4..].to_string());
                            break;
                        }
                    }
                }
                let _ = s.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let w = Webhooks::start(&config::Webhooks { urls: vec![format!("http://127.0.0.1:{port}/hook"), " ".into()], all_changes: true });
        w.send(&notice());
        let v: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!(v["event"], "low_battery");
        w.device_changed(&device(50.0));
        w.device_changed(&device(50.2)); // same whole percent: skipped
        w.device_changed(&device(49.0));
        let a: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        let b: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!((a["percent"].as_f64(), b["percent"].as_f64()), (Some(50.0), Some(49.0)));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());

        // Without allChanges, changes are not sent.
        let quiet = Webhooks::start(&config::Webhooks { urls: vec![format!("http://127.0.0.1:{port}/hook")], all_changes: false });
        quiet.device_changed(&device(10.0));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
