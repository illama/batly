//! Minimal MQTT 3.1.1 client (QoS 0, `std::net`) publishing the devices to
//! a broker, with Home Assistant discovery.
//!
//! * `<base>/status`        – `online` (retained), `offline` as last will
//! * `<base>/<id>/state`    – device JSON (same as `/api/device/{id}`), retained
//! * `<base>/alert`         – alerts (`Notice`), not retained
//! * `<prefix>/sensor/batly_<id>/battery/config` … – discovery, retained

use crate::config;
use crate::http::{self, Context};
use crate::log;
use crate::model::Device;
use crate::notice::Notice;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const KEEP_ALIVE: u16 = 60;
const PING_EVERY: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

enum Msg {
    Device(Box<Device>),
    Notice(Box<Notice>),
    Stop,
}

/// Running client; dropping it disconnects cleanly.
pub struct Mqtt {
    tx: mpsc::Sender<Msg>,
}

impl Mqtt {
    /// Connects in the background and follows the hub (it subscribes to it).
    pub fn start(cfg: &config::Mqtt, ctx: Arc<Context>) -> Self {
        let (tx, rx) = mpsc::channel();
        let tx2 = tx.clone();
        ctx.hub.subscribe(move |d| {
            let _ = tx2.send(Msg::Device(Box::new(d.clone())));
        });
        let cfg = cfg.clone();
        let _ = thread::Builder::new().name("mqtt".into()).stack_size(256 * 1024).spawn(move || run(&cfg, &ctx, &rx));
        Self { tx }
    }

    /// Publishes an alert on `<base>/alert`.
    pub fn notice(&self, n: &Notice) {
        let _ = self.tx.send(Msg::Notice(Box::new(n.clone())));
    }
}

impl Drop for Mqtt {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

fn run(cfg: &config::Mqtt, ctx: &Context, rx: &mpsc::Receiver<Msg>) {
    let mut delay = Duration::from_secs(1);
    loop {
        match session(cfg, ctx, rx, &mut delay) {
            Ok(()) => return,
            Err(e) => log!("mqtt: {e}"),
        }
        // Wait, dropping the changes: everything is sent again on connect.
        let until = Instant::now() + delay;
        loop {
            match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        delay = (delay * 2).min(MAX_BACKOFF);
    }
}

/// One connection; Ok when asked to stop.
fn session(cfg: &config::Mqtt, ctx: &Context, rx: &mpsc::Receiver<Msg>, delay: &mut Duration) -> io::Result<()> {
    let base = cfg.base_topic.trim().trim_end_matches('/');
    let base = if base.is_empty() { "batly" } else { base };
    let addr =
        (cfg.host.trim(), cfg.port).to_socket_addrs()?.next().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "broker not found"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    let _ = s.set_nodelay(true);
    let client_id = format!("batly-{}", &ctx.instance[..ctx.instance.len().min(12)]);
    let status = format!("{base}/status");
    s.write_all(&connect_packet(&client_id, &cfg.username, &cfg.password, &status, b"offline", KEEP_ALIVE))?;
    let (kind, body) = read_packet(&mut s)?;
    if kind >> 4 != 2 || body.len() < 2 {
        return Err(io::Error::other("no CONNACK"));
    }
    if body[1] != 0 {
        return Err(io::Error::other(format!("connection refused (code {})", body[1])));
    }
    *delay = Duration::from_secs(1);

    // Reader: answers nothing, only proves the broker is alive.
    let start = Instant::now();
    let last_rx = Arc::new(AtomicU64::new(0));
    let mut reader = s.try_clone()?;
    reader.set_read_timeout(None)?;
    let lr = last_rx.clone();
    thread::Builder::new().name("mqtt-read".into()).stack_size(64 * 1024).spawn(move || {
        while read_packet(&mut reader).is_ok() {
            lr.store(start.elapsed().as_secs(), Ordering::Relaxed);
        }
    })?;

    let result = (|| {
        s.write_all(&publish_packet(&status, b"online", true))?;
        let mut announced: HashMap<String, String> = HashMap::new();
        for d in ctx.hub.snapshot() {
            publish_device(&mut s, cfg, base, ctx, &d, &mut announced)?;
        }
        let mut last_ping = Instant::now();
        loop {
            match rx.recv_timeout(PING_EVERY) {
                Ok(Msg::Device(d)) => publish_device(&mut s, cfg, base, ctx, &d, &mut announced)?,
                Ok(Msg::Notice(n)) => s.write_all(&publish_packet(&format!("{base}/alert"), notice_json(&n).as_bytes(), false))?,
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    let _ = s.write_all(&publish_packet(&status, b"offline", true));
                    let _ = s.write_all(&[0xE0, 0]); // DISCONNECT
                    return Ok(());
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_ping.elapsed() >= PING_EVERY {
                s.write_all(&[0xC0, 0])?; // PINGREQ
                last_ping = Instant::now();
            }
            let silent = start.elapsed().as_secs().saturating_sub(last_rx.load(Ordering::Relaxed));
            if silent > KEEP_ALIVE as u64 * 3 / 2 + PING_EVERY.as_secs() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "broker not answering"));
            }
        }
    })();
    let _ = s.shutdown(Shutdown::Both);
    result
}

/// Topic-safe device id.
pub fn topic_id(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn publish_device(
    s: &mut impl Write,
    cfg: &config::Mqtt,
    base: &str,
    ctx: &Context,
    changed: &Device,
    announced: &mut HashMap<String, String>,
) -> io::Result<()> {
    let tid = topic_id(&changed.id);
    // The hub holds the current state; a device it no longer has is gone.
    let Some(d) = ctx.hub.find(&changed.id).filter(|d| d.id == changed.id) else {
        s.write_all(&publish_packet(&format!("{base}/{tid}/state"), b"", true))?;
        if announced.remove(&changed.id).is_some() && cfg.discovery {
            for (topic, _) in discovery(cfg, base, ctx, changed) {
                s.write_all(&publish_packet(&topic, b"", true))?;
            }
        }
        return Ok(());
    };
    if !d.has_battery {
        return Ok(());
    }
    let name = ctx.name(&d);
    if cfg.discovery && announced.get(&d.id) != Some(&name) {
        for (topic, payload) in discovery(cfg, base, ctx, &d) {
            s.write_all(&publish_packet(&topic, payload.as_bytes(), true))?;
        }
    }
    announced.insert(d.id.clone(), name);
    s.write_all(&publish_packet(&format!("{base}/{tid}/state"), http::device_json(ctx, &d).as_bytes(), true))
}

/// Home Assistant discovery messages of a device: (topic, payload).
pub fn discovery(cfg: &config::Mqtt, base: &str, ctx: &Context, d: &Device) -> Vec<(String, String)> {
    let prefix = cfg.discovery_prefix.trim().trim_end_matches('/');
    let prefix = if prefix.is_empty() { "homeassistant" } else { prefix };
    let tid = topic_id(&d.id);
    let device = json!({
        "identifiers": [format!("batly_{tid}")],
        "name": ctx.name(d),
        "model": d.info.model.clone().unwrap_or_else(|| d.name.clone()),
        "manufacturer": "Batly",
        "sw_version": d.info.firmware,
        "suggested_area": d.info.host,
    });
    let entity = |component: &str, key: &str, name: &str, extra: Value| {
        let mut v = json!({
            "name": name,
            "unique_id": format!("batly_{tid}_{key}"),
            "object_id": format!("batly_{tid}_{key}"),
            "state_topic": format!("{base}/{tid}/state"),
            "availability_topic": format!("{base}/status"),
            "json_attributes_topic": format!("{base}/{tid}/state"),
            "device": device,
        });
        if let (Some(obj), Value::Object(more)) = (v.as_object_mut(), extra) {
            obj.extend(more);
        }
        (format!("{prefix}/{component}/batly_{tid}/{key}/config"), v.to_string())
    };
    vec![
        entity(
            "sensor",
            "battery",
            "Battery",
            json!({
                "device_class": "battery", "unit_of_measurement": "%", "state_class": "measurement",
                "value_template": "{{ value_json.battery_percent }}",
            }),
        ),
        entity(
            "binary_sensor",
            "charging",
            "Charging",
            json!({
                "device_class": "battery_charging",
                "value_template": "{{ 'ON' if value_json.charging else 'OFF' }}",
            }),
        ),
        entity(
            "sensor",
            "voltage",
            "Voltage",
            json!({
                "device_class": "voltage", "unit_of_measurement": "V", "state_class": "measurement",
                "entity_category": "diagnostic", "value_template": "{{ value_json.battery_voltage }}",
            }),
        ),
        entity(
            "sensor",
            "time_left",
            "Time left",
            json!({
                "device_class": "duration", "unit_of_measurement": "h",
                "value_template": "{{ value_json.time_left_hours }}",
            }),
        ),
        entity(
            "binary_sensor",
            "online",
            "Connected",
            json!({
                "device_class": "connectivity", "entity_category": "diagnostic",
                "value_template": "{{ 'ON' if value_json.online else 'OFF' }}",
            }),
        ),
    ]
}

pub fn notice_json(n: &Notice) -> String {
    json!({
        "event": n.kind.as_str(),
        "device_id": Some(&n.device_id).filter(|s| !s.is_empty()),
        "device_name": Some(&n.device_name).filter(|s| !s.is_empty()),
        "percent": n.percent,
        "hours_left": n.hours_left,
        "title": n.title,
        "text": n.text,
    })
    .to_string()
}

// ------------------------------------------------------------ packets

/// MQTT "remaining length": 7 bits per byte, high bit = more.
pub fn encode_len(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
}

/// Decodes a remaining length; returns (value, bytes used).
pub fn decode_len(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for (i, &b) in bytes.iter().enumerate().take(4) {
        value |= ((b & 0x7F) as usize) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    let s = &s[..s.len().min(u16::MAX as usize)];
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
}

fn packet(first: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![first];
    encode_len(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

/// CONNECT with a clean session and a retained QoS 0 last will.
pub fn connect_packet(client_id: &str, user: &str, password: &str, will_topic: &str, will: &[u8], keep_alive: u16) -> Vec<u8> {
    let mut body = Vec::new();
    put_str(&mut body, b"MQTT");
    body.push(4); // protocol level 3.1.1
    let mut flags = 0x02 | 0x04 | 0x20; // clean session, will, will retain
    if !user.is_empty() {
        flags |= 0x80;
        if !password.is_empty() {
            flags |= 0x40; // 3.1.1: no password without a user name
        }
    }
    body.push(flags);
    body.extend_from_slice(&keep_alive.to_be_bytes());
    put_str(&mut body, client_id.as_bytes());
    put_str(&mut body, will_topic.as_bytes());
    put_str(&mut body, will);
    if !user.is_empty() {
        put_str(&mut body, user.as_bytes());
        if !password.is_empty() {
            put_str(&mut body, password.as_bytes());
        }
    }
    packet(0x10, &body)
}

/// PUBLISH, QoS 0.
pub fn publish_packet(topic: &str, payload: &[u8], retain: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(topic.len() + payload.len() + 2);
    put_str(&mut body, topic.as_bytes());
    body.extend_from_slice(payload);
    packet(0x30 | retain as u8, &body)
}

/// Reads one packet: (first byte, body).
pub fn read_packet(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first)?;
    let mut len_bytes = Vec::new();
    loop {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        len_bytes.push(b[0]);
        if b[0] & 0x80 == 0 || len_bytes.len() == 4 {
            break;
        }
    }
    let (len, _) = decode_len(&len_bytes).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad length"))?;
    if len > 1 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "packet too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok((first[0], body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use crate::model::{Battery, DeviceKind, Event, Hub, Source, Status};
    use crate::notice::NoticeKind;
    use std::net::TcpListener;

    #[test]
    fn remaining_length() {
        for (n, bytes) in [
            (0usize, vec![0x00u8]),
            (127, vec![0x7F]),
            (128, vec![0x80, 0x01]),
            (16_383, vec![0xFF, 0x7F]),
            (16_384, vec![0x80, 0x80, 0x01]),
            (2_097_151, vec![0xFF, 0xFF, 0x7F]),
            (2_097_152, vec![0x80, 0x80, 0x80, 0x01]),
            (268_435_455, vec![0xFF, 0xFF, 0xFF, 0x7F]),
        ] {
            let mut out = Vec::new();
            encode_len(n, &mut out);
            assert_eq!(out, bytes, "{n}");
            assert_eq!(decode_len(&bytes), Some((n, bytes.len())));
        }
        assert_eq!(decode_len(&[0x80, 0x80, 0x80, 0x80, 0x01]), None);
        assert_eq!(decode_len(&[0x80]), None);
    }

    #[test]
    fn packets() {
        let p = connect_packet("cid", "u", "p", "t/status", b"offline", 60);
        assert_eq!(
            p,
            [
                &[0x10, 40, 0, 4][..],
                b"MQTT",
                &[4, 0xE6, 0, 60, 0, 3],
                b"cid",
                &[0, 8],
                b"t/status",
                &[0, 7],
                b"offline",
                &[0, 1],
                b"u",
                &[0, 1],
                b"p"
            ]
            .concat()
        );
        assert_eq!(connect_packet("c", "", "pw", "w", b"", 30)[9], 0x26, "no password without user");
        let p = publish_packet("a/b", b"xy", true);
        assert_eq!(p, [0x31, 7, 0, 3, b'a', b'/', b'b', b'x', b'y']);
        let big = publish_packet("t", &[7u8; 300], false);
        assert_eq!(&big[..3], [0x30, 0xAF, 0x02]); // 303
        let (kind, body) = read_packet(&mut &big[..]).unwrap();
        assert_eq!((kind, body.len()), (0x30, 303));
        assert_eq!(topic_id("peer:pc:ab/c d"), "peer_pc_ab_c_d");
    }

    fn ctx() -> Arc<Context> {
        let hub = Arc::new(Hub::new());
        hub.publish(Event::Init {
            id: "dev 1".into(),
            name: "G502".into(),
            kind: DeviceKind::Mouse,
            source: Source::Native,
            has_battery: true,
        });
        hub.publish(Event::Update {
            id: "dev 1".into(),
            battery: Battery { percent: 64.0, status: Status::Discharging, millivolts: Some(3870), mileage: None },
        });
        Arc::new(Context::new(hub, Arc::new(History::open(None, 30))).with_names(|_, _| "Souris".into()))
    }

    #[test]
    fn home_assistant_discovery() {
        let ctx = ctx();
        let cfg = config::Mqtt::default();
        let d = ctx.hub.find("dev 1").unwrap();
        let msgs = discovery(&cfg, "batly", &ctx, &d);
        let topics: Vec<&str> = msgs.iter().map(|(t, _)| t.as_str()).collect();
        assert!(topics.contains(&"homeassistant/sensor/batly_dev_1/battery/config"));
        assert!(topics.contains(&"homeassistant/binary_sensor/batly_dev_1/charging/config"));
        assert!(topics.contains(&"homeassistant/sensor/batly_dev_1/voltage/config"));
        assert!(topics.contains(&"homeassistant/sensor/batly_dev_1/time_left/config"));
        let v: Value = serde_json::from_str(&msgs[0].1).unwrap();
        assert_eq!(v["device_class"], "battery");
        assert_eq!(v["state_topic"], "batly/dev_1/state");
        assert_eq!(v["availability_topic"], "batly/status");
        assert_eq!(v["device"]["name"], "Souris");
        assert_eq!(v["device"]["identifiers"][0], "batly_dev_1");
    }

    /// Fake broker: checks CONNECT, records the PUBLISH packets.
    #[test]
    fn talks_to_a_broker() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<(String, Vec<u8>, bool)>();
        thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let (kind, body) = read_packet(&mut s).unwrap();
            assert_eq!(kind, 0x10);
            assert_eq!(&body[..7], b"\0\x04MQTT\x04");
            assert_eq!(body[7], 0xE6, "user, password, will retain, will, clean");
            assert!(body.windows(7).any(|w| w == b"offline") && body.windows(6).any(|w| w == b"secret"));
            s.write_all(&[0x20, 2, 0, 0]).unwrap();
            while let Ok((kind, body)) = read_packet(&mut s) {
                match kind >> 4 {
                    3 => {
                        let n = u16::from_be_bytes([body[0], body[1]]) as usize;
                        let topic = String::from_utf8(body[2..2 + n].to_vec()).unwrap();
                        let _ = tx.send((topic, body[2 + n..].to_vec(), kind & 1 == 1));
                    }
                    12 => s.write_all(&[0xD0, 0]).unwrap(),
                    14 => break,
                    _ => {}
                }
            }
        });
        let ctx = ctx();
        let cfg = config::Mqtt {
            enabled: true,
            host: "127.0.0.1".into(),
            port,
            username: "user".into(),
            password: "secret".into(),
            base_topic: "home/lgs/".into(),
            ..Default::default()
        };
        let mqtt = Mqtt::start(&cfg, ctx.clone());
        let mut got: HashMap<String, (Vec<u8>, bool)> = HashMap::new();
        let mut wait_topic = |topic: &str, until: &dyn Fn(&[u8]) -> bool| loop {
            if got.get(topic).is_some_and(|(p, _)| until(p)) {
                return got[topic].clone();
            }
            let (t, p, r) = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("no {topic}: {:?}", got.keys()));
            got.insert(t, (p, r));
        };
        assert_eq!(wait_topic("home/lgs/status", &|p| p == b"online"), (b"online".to_vec(), true));
        let (state, retained) = wait_topic("home/lgs/dev_1/state", &|_| true);
        let v: Value = serde_json::from_slice(&state).unwrap();
        assert!(retained && v["battery_percent"] == 64 && v["alias"] == "Souris");
        wait_topic("homeassistant/sensor/batly_dev_1/battery/config", &|_| true);

        ctx.hub.publish(Event::Update {
            id: "dev 1".into(),
            battery: Battery { percent: 63.0, status: Status::Charging, millivolts: Some(3870), mileage: None },
        });
        let (state, _) = wait_topic("home/lgs/dev_1/state", &|p| p.windows(23).any(|w| w == b"\"battery_percent\":63,\"b"));
        assert!(serde_json::from_slice::<Value>(&state).unwrap()["charging"] == true);

        mqtt.notice(&Notice {
            kind: NoticeKind::LowBattery,
            device_id: "dev 1".into(),
            device_name: "Souris".into(),
            percent: Some(10.0),
            hours_left: None,
            title: "Batterie faible".into(),
            text: "Souris : 10 % restants".into(),
        });
        let (alert, retained) = wait_topic("home/lgs/alert", &|_| true);
        assert!(!retained && serde_json::from_slice::<Value>(&alert).unwrap()["event"] == "low_battery");

        ctx.hub.publish(Event::Remove { id: "dev 1".into() });
        wait_topic("home/lgs/dev_1/state", &|p| p.is_empty());
        drop(mqtt);
        wait_topic("home/lgs/status", &|p| p == b"offline");
    }
}
