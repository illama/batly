//! Other LGSTray instances on the network, merged into this one.
//!
//! One thread per peer (`peers` setting, `url#token` syntax) follows the
//! peer's `/api/events?local=1` stream (falling back to polling
//! `/api/devices?local=1` every minute when the peer has no event stream or
//! is reached over https) and mirrors its devices into the hub as
//! `Source::Remote`, with the id `peer:<host>:<id>` and `DeviceInfo.host`.
//!
//! `local=1` keeps the peer from sending back the devices it got from other
//! instances, so two instances merging each other do not loop; the instance
//! id announced in the `hello` event catches a peer that is ourselves.
//! When a peer cannot be reached, its devices keep their last known state
//! with `online == false`.

use crate::fetch::{self, Url};
use crate::http::Context;
use crate::log;
use crate::model::{Battery, Device, DeviceInfo, DeviceKind, Event, Hub, Source, Status};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read as _, Write as _};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const DEFAULT_PORT: u16 = 12321;
const POLL: Duration = Duration::from_secs(60);
/// The server pings every 25 s: silence for this long means a dead link.
const READ_TIMEOUT: Duration = Duration::from_secs(70);
const MAX_BACKOFF: Duration = Duration::from_secs(120);

/// Running peer threads; dropping it stops them and removes their devices.
pub struct Peers {
    workers: Vec<(mpsc::Sender<()>, Arc<Mutex<Option<TcpStream>>>)>,
}

impl Drop for Peers {
    fn drop(&mut self) {
        for (_stop, conn) in self.workers.drain(..) {
            if let Some(s) = conn.lock().unwrap().as_ref() {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

pub fn start(urls: &[String], ctx: &Arc<Context>) -> Peers {
    let mut workers = Vec::new();
    for raw in urls {
        let Some(peer) = Peer::parse(raw) else {
            log!("peers: invalid URL {raw}");
            continue;
        };
        let (tx, rx) = mpsc::channel();
        let conn = Arc::new(Mutex::new(None));
        let (mirror, instance, conn2) = (Mirror::new(ctx.hub.clone(), &peer), ctx.instance.clone(), conn.clone());
        if thread::Builder::new()
            .name(format!("peer {}", peer.label))
            .stack_size(256 * 1024)
            .spawn(move || run(peer, mirror, &instance, &rx, &conn2))
            .is_ok()
        {
            workers.push((tx, conn));
        }
    }
    Peers { workers }
}

#[derive(Debug, PartialEq)]
pub struct Peer {
    /// Base URL without the token or a trailing '/'.
    pub base: String,
    pub token: Option<String>,
    pub url: Url,
    /// `host`, or `host:port` when the port is not the default one.
    pub label: String,
}

impl Peer {
    pub fn parse(raw: &str) -> Option<Self> {
        let (base, token) = fetch::split_token(raw.trim());
        let base = base.trim_end_matches('/').to_string();
        let url = Url::parse(&base)?;
        let label = if url.port == DEFAULT_PORT { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
        Some(Self { token: token.map(str::to_string), base, url, label })
    }

    fn path(&self, route: &str) -> String {
        format!("{}{route}", self.url.path.split('?').next().unwrap_or("").trim_end_matches('/'))
    }
}

/// What a session with the peer ended with.
#[derive(Debug, PartialEq)]
enum End {
    Stopped,
    /// No event stream there (older LGSTray, https…): poll instead.
    Unsupported,
    /// The peer is this very instance.
    Myself,
    /// Connection lost; `synced` when it had worked.
    Lost { synced: bool },
}

fn run(peer: Peer, mut mirror: Mirror, instance: &str, stop: &mpsc::Receiver<()>, conn: &Mutex<Option<TcpStream>>) {
    // Returns false when asked to stop.
    let wait = |d: Duration| matches!(stop.recv_timeout(d), Err(RecvTimeoutError::Timeout));
    let mut delay = Duration::from_secs(1);
    let mut polling = peer.url.https;
    let mut polls = 0;
    loop {
        if !polling {
            match sse_session(&peer, &mut mirror, instance, stop, conn) {
                End::Stopped => break,
                End::Unsupported => {
                    polling = true;
                    polls = 0;
                    continue;
                }
                End::Myself => {
                    log!("peers: {} is this instance, ignored", peer.base);
                    while wait(Duration::from_secs(3600)) {}
                    break;
                }
                End::Lost { synced } => {
                    mirror.all_offline();
                    if synced {
                        delay = Duration::from_secs(1);
                    }
                    if !wait(delay) {
                        break;
                    }
                    delay = (delay * 2).min(MAX_BACKOFF);
                }
            }
        } else {
            if let Err(e) = poll(&peer, &mut mirror) {
                log!("peers: {}: {e}", peer.base);
                mirror.all_offline();
            }
            if !wait(POLL) {
                break;
            }
            // Try the event stream again now and then (the peer may have been updated).
            polls += 1;
            if polls >= 10 && !peer.url.https {
                polling = false;
            }
        }
    }
    mirror.remove_all();
}

fn auth_header(peer: &Peer) -> Option<String> {
    peer.token.as_ref().map(|t| format!("Bearer {t}"))
}

fn poll(peer: &Peer, mirror: &mut Mirror) -> io::Result<()> {
    let auth = auth_header(peer);
    let headers: Vec<(&str, &str)> = auth.as_deref().map(|a| ("Authorization", a)).into_iter().collect();
    let reply = fetch::get(&format!("{}/api/devices?local=1", peer.base), &headers, Duration::from_secs(10))?;
    if !reply.ok() {
        return Err(io::Error::other(format!("HTTP {}", reply.status)));
    }
    let list: Value = serde_json::from_slice(&reply.body).map_err(io::Error::other)?;
    let list = list.as_array().ok_or_else(|| io::Error::other("not a list"))?;
    let mut seen = HashSet::new();
    for v in list {
        if let Some(id) = mirror.apply(v) {
            seen.insert(id);
        }
    }
    mirror.offline_except(&seen);
    Ok(())
}

fn sse_session(peer: &Peer, mirror: &mut Mirror, instance: &str, stop: &mpsc::Receiver<()>, conn: &Mutex<Option<TcpStream>>) -> End {
    let lost = End::Lost { synced: false };
    let addr = match (peer.url.host.as_str(), peer.url.port).to_socket_addrs().map(|mut a| a.next()) {
        Ok(Some(a)) => a,
        _ => return lost,
    };
    let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(10)) else { return lost };
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    *conn.lock().unwrap() = stream.try_clone().ok();
    // Asked to stop while connecting.
    if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        return End::Stopped;
    }
    let host = if peer.url.host.contains(':') { format!("[{}]", peer.url.host) } else { peer.url.host.clone() };
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: {host}:{}\r\nUser-Agent: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n",
        peer.path("/api/events?local=1"),
        peer.url.port,
        fetch::USER_AGENT
    );
    if let Some(a) = auth_header(peer) {
        req.push_str(&format!("Authorization: {a}\r\n"));
    }
    req.push_str("\r\n");
    if (&stream).write_all(req.as_bytes()).is_err() {
        return lost;
    }

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    let mut read = |line: &mut String| {
        line.clear();
        matches!(reader.by_ref().take(1 << 20).read_line(line), Ok(n) if n > 0)
    };
    if !read(&mut line) {
        return lost;
    }
    let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut event_stream = false;
    loop {
        if !read(&mut line) {
            return lost;
        }
        if line.trim().is_empty() {
            break;
        }
        let l = line.to_ascii_lowercase();
        if l.starts_with("content-type:") && l.contains("text/event-stream") {
            event_stream = true;
        }
    }
    match status {
        200 if event_stream => {}
        404 | 405 | 501 => return End::Unsupported,
        200 => return End::Unsupported,
        _ => {
            log!("peers: {}: HTTP {status}", peer.base);
            return lost;
        }
    }

    let mut parser = SseParser::default();
    let mut synced = false;
    let mut seen = HashSet::new();
    while read(&mut line) {
        let Some((event, data)) = parser.line(&line) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
        match event.as_str() {
            "hello" => {
                if v["instance"].as_str() == Some(instance) {
                    return End::Myself;
                }
                if let Some(h) = v["host"].as_str().filter(|h| !h.is_empty()) {
                    mirror.host = h.to_string();
                }
            }
            "device" => {
                if let Some(id) = mirror.apply(&v) {
                    seen.insert(id);
                }
            }
            "synced" => {
                synced = true;
                mirror.offline_except(&std::mem::take(&mut seen));
            }
            _ => {}
        }
    }
    *conn.lock().unwrap() = None;
    if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        return End::Stopped;
    }
    End::Lost { synced }
}

/// Incremental Server-Sent Events parser: feed it lines, get `(event, data)`
/// when an event is complete.
#[derive(Default)]
pub struct SseParser {
    event: String,
    data: String,
}

impl SseParser {
    pub fn line(&mut self, line: &str) -> Option<(String, String)> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if self.data.is_empty() {
                self.event.clear();
                return None;
            }
            let mut data = std::mem::take(&mut self.data);
            data.pop();
            let event = std::mem::take(&mut self.event);
            return Some((if event.is_empty() { "message".into() } else { event }, data));
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            _ => {}
        }
        None
    }
}

/// A device as described by a peer's JSON.
#[derive(Debug, PartialEq)]
pub struct Remote {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub has_battery: bool,
    pub online: bool,
    pub battery: Option<Battery>,
    pub removed: bool,
}

impl Remote {
    /// None for invalid entries and for devices the peer itself got from
    /// another instance (never mirrored, whatever the peer sends).
    pub fn parse(v: &Value) -> Option<Self> {
        let id = v["id"].as_str()?.to_string();
        if v["removed"].as_bool() == Some(true) {
            return Some(Self { id, name: String::new(), kind: DeviceKind::Other, has_battery: false, online: false, battery: None, removed: true });
        }
        if v["source"].as_str() == Some("remote") || id.starts_with("peer:") {
            return None;
        }
        let status = match v["status"].as_str().unwrap_or("") {
            "discharging" => Status::Discharging,
            "charging" => Status::Charging,
            "full" => Status::Full,
            "not_charging" => Status::NotCharging,
            _ => Status::Unknown,
        };
        let battery = v["battery_percent"].as_f64().map(|p| Battery {
            percent: p.clamp(0.0, 100.0) as f32,
            status,
            millivolts: v["battery_voltage"].as_f64().filter(|v| *v > 0.0).map(|v| (v * 1000.0).round() as u16),
            // The peer's own estimate shows up in our tooltips and JSON.
            mileage: v["time_left_hours"].as_f64().or(v["mileage"].as_f64()).filter(|h| h.is_finite() && *h > 0.0),
        });
        Some(Self {
            name: v["alias"].as_str().or(v["name"].as_str()).unwrap_or(&id).to_string(),
            kind: DeviceKind::parse(v["type"].as_str().unwrap_or("")).unwrap_or(DeviceKind::Other),
            has_battery: v["has_battery"].as_bool().unwrap_or(battery.is_some()),
            online: v["online"].as_bool().unwrap_or(true),
            battery,
            removed: false,
            id,
        })
    }
}

/// Mirrors one peer's devices into the hub, publishing only what changed.
pub struct Mirror {
    hub: Arc<Hub>,
    /// Computer name announced by the peer (else the URL host).
    pub host: String,
    url_host: String,
    label: String,
    owned: HashSet<String>,
}

impl Mirror {
    pub fn new(hub: Arc<Hub>, peer: &Peer) -> Self {
        Self { hub, host: String::new(), url_host: peer.url.host.clone(), label: peer.label.clone(), owned: HashSet::new() }
    }

    pub fn local_id(&self, remote_id: &str) -> String {
        format!("peer:{}:{remote_id}", self.label)
    }

    /// Applies a device object; returns its local id.
    pub fn apply(&mut self, v: &Value) -> Option<String> {
        let r = Remote::parse(v)?;
        let id = self.local_id(&r.id);
        if r.removed {
            self.owned.remove(&id);
            self.hub.publish(Event::Remove { id });
            return None;
        }
        let current: Option<Device> = self.hub.find(&id).filter(|d| d.id == id);
        let same_meta = current.as_ref().is_some_and(|d| d.name == r.name && d.kind == r.kind && d.has_battery == r.has_battery);
        let init = Event::Init { id: id.clone(), name: r.name.clone(), kind: r.kind, source: Source::Remote, has_battery: r.has_battery };
        let battery_changed = r.battery.is_some() && current.as_ref().is_none_or(|d| d.battery != r.battery);
        if !same_meta || (r.online && !current.as_ref().is_some_and(|d| d.online)) {
            self.hub.publish(init);
        }
        if let Some(b) = r.battery.filter(|_| battery_changed || r.online) {
            self.hub.publish(Event::Update { id: id.clone(), battery: b });
        }
        if !r.online {
            self.hub.publish(Event::Offline { id: id.clone() });
        }
        let host = if self.host.is_empty() { self.url_host.clone() } else { self.host.clone() };
        if current.as_ref().is_none_or(|d| d.info.host.as_deref() != Some(host.as_str())) {
            self.hub.publish(Event::Info { id: id.clone(), info: DeviceInfo { host: Some(host), ..Default::default() } });
        }
        self.owned.insert(id.clone());
        Some(id)
    }

    pub fn offline_except(&self, keep: &HashSet<String>) {
        for id in self.owned.iter().filter(|id| !keep.contains(*id)) {
            self.hub.publish(Event::Offline { id: id.clone() });
        }
    }

    pub fn all_offline(&self) {
        self.offline_except(&HashSet::new());
    }

    pub fn remove_all(&mut self) {
        for id in self.owned.drain() {
            self.hub.publish(Event::Remove { id });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::history::History;
    use crate::http;
    use serde_json::json;
    use std::net::TcpListener;
    use std::time::Instant;

    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out: {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn parses_peer_urls() {
        let p = Peer::parse("http://salon:12321/#tok").unwrap();
        assert_eq!((p.base.as_str(), p.token.as_deref(), p.label.as_str()), ("http://salon:12321", Some("tok"), "salon"));
        assert_eq!(p.path("/api/events?local=1"), "/api/events?local=1");
        let p = Peer::parse("http://10.0.0.2:8080/lgs").unwrap();
        assert_eq!((p.label.as_str(), p.token.as_deref(), p.path("/x").as_str()), ("10.0.0.2:8080", None, "/lgs/x"));
        assert!(Peer::parse("salon").is_none());
    }

    #[test]
    fn sse_parsing() {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        for l in ["retry: 5000\n", "\n", ": ping\n", "\n", "event: device\r\n", "data: {\"a\":\r\n", "data:1}\n", "\n", "data: x\n", "\n"] {
            out.extend(p.line(l));
        }
        assert_eq!(out, [("device".to_string(), "{\"a\":\n1}".to_string()), ("message".to_string(), "x".to_string())]);
    }

    #[test]
    fn remote_devices_and_loops() {
        let v = json!({"id":"A1","name":"G502","alias":"Souris","type":"Mouse","source":"native","online":true,"has_battery":true,
            "battery_percent":55,"battery_voltage":3.8,"status":"discharging","mileage":null,"time_left_hours":20.5});
        let r = Remote::parse(&v).unwrap();
        assert_eq!((r.name.as_str(), r.kind, r.online), ("Souris", DeviceKind::Mouse, true));
        assert_eq!(r.battery, Some(Battery { percent: 55.0, status: Status::Discharging, millivolts: Some(3800), mileage: Some(20.5) }));
        // Never mirror what the peer itself mirrors.
        assert!(Remote::parse(&json!({"id":"peer:x:A1","source":"native"})).is_none());
        assert!(Remote::parse(&json!({"id":"B","source":"remote"})).is_none());
        assert!(Remote::parse(&json!({"id":"B","removed":true})).unwrap().removed);

        let hub = Arc::new(Hub::new());
        let mut m = Mirror::new(hub.clone(), &Peer::parse("http://pc:12321").unwrap());
        let changes = Arc::new(Mutex::new(0));
        let c2 = changes.clone();
        hub.on_change(move || *c2.lock().unwrap() += 1);
        assert_eq!(m.apply(&v).as_deref(), Some("peer:pc:A1"));
        let d = hub.find("peer:pc:A1").unwrap();
        assert_eq!((d.source, d.info.host.as_deref(), d.battery.unwrap().percent), (Source::Remote, Some("pc"), 55.0));
        let n = *changes.lock().unwrap();
        m.apply(&v);
        assert_eq!(*changes.lock().unwrap(), n, "same state: no change");
        m.all_offline();
        let d = hub.find("peer:pc:A1").unwrap();
        assert!(!d.online && d.battery.unwrap().percent == 55.0, "last known state kept");
        m.apply(&json!({"id":"A1","removed":true}));
        assert!(hub.find("peer:pc:A1").is_none());
    }

    fn instance(port: u16) -> (Arc<Context>, String) {
        let hub = Arc::new(Hub::new());
        let ctx = Arc::new(Context::new(hub, Arc::new(History::open(None, 30))));
        let cfg = config::Http { addr: "127.0.0.1".into(), port, use_ipv6: false, ..Default::default() };
        let addr = http::start(&cfg, ctx.clone())[0];
        (ctx, format!("http://{addr}"))
    }

    fn local(ctx: &Context, id: &str, percent: f32) {
        ctx.hub.publish(Event::Init { id: id.into(), name: id.into(), kind: DeviceKind::Mouse, source: Source::Native, has_battery: true });
        ctx.hub.publish(Event::Update { id: id.into(), battery: Battery { percent, status: Status::Discharging, millivolts: None, mileage: None } });
    }

    #[test]
    fn two_instances_merge_each_other_without_loop() {
        let (a, url_a) = instance(0);
        let (b, url_b) = instance(0);
        local(&a, "a1", 50.0);
        local(&b, "b1", 60.0);
        let _pa = start(&[url_b.clone()], &a);
        let _pb = start(&[url_a.clone()], &b);
        let label_b = url_b.trim_start_matches("http://").to_string();
        let remote_b = format!("peer:{label_b}:b1");
        wait_for("b1 on a", || a.hub.find(&remote_b).is_some_and(|d| d.battery.is_some()));
        wait_for("a1 on b", || b.hub.snapshot().len() == 2);
        local(&b, "b1", 42.0);
        wait_for("update", || a.hub.find(&remote_b).unwrap().battery.unwrap().percent == 42.0);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(a.hub.snapshot().len(), 2, "{:?}", a.hub.snapshot());
        assert_eq!(b.hub.snapshot().len(), 2);
        assert!(a.hub.snapshot().iter().all(|d| !d.id.starts_with("peer:") || !d.id[5..].contains("peer:")));
        // A peer that is ourselves is ignored.
        let _self_peer = start(&[url_a], &a);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(a.hub.snapshot().len(), 2);
        drop(_pa);
        wait_for("removed on stop", || a.hub.find(&remote_b).is_none());
    }

    /// A peer that answers once, then disappears: the device stays, offline.
    #[test]
    fn unreachable_peer_keeps_last_state() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("GET /api/events?local=1 ") && req.contains("Authorization: Bearer tok"), "{req}");
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: hello\ndata: {\"instance\":\"other\",\"host\":\"SALON\"}\n\n").unwrap();
            s.write_all(b"event: device\ndata: {\"id\":\"k\",\"name\":\"K\",\"type\":\"Keyboard\",\"online\":true,\"has_battery\":true,\"battery_percent\":30,\"status\":\"discharging\"}\n\nevent: synced\ndata: {}\n\n").unwrap();
            thread::sleep(Duration::from_millis(300));
            // Listener and stream dropped: refused from now on.
        });
        let (ctx, _) = instance(0);
        let _p = start(&[format!("http://{addr}#tok")], &ctx);
        let id = format!("peer:{addr}:k");
        wait_for("device", || ctx.hub.find(&id).is_some_and(|d| d.online));
        assert_eq!(ctx.hub.find(&id).unwrap().info.host.as_deref(), Some("SALON"));
        wait_for("offline", || ctx.hub.find(&id).is_some_and(|d| !d.online));
        let d = ctx.hub.find(&id).unwrap();
        assert_eq!((d.battery.unwrap().percent, d.kind), (30.0, DeviceKind::Keyboard));
    }

    /// An instance without `/api/events` (LGSTray 4.0) is polled.
    #[test]
    fn falls_back_to_polling() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 1024];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /api/devices?local=1 ") {
                    r#"[{"id":"m","name":"M","type":"Mouse","online":true,"has_battery":true,"battery_percent":77,"status":"charging"}]"#
                } else {
                    ""
                };
                let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let (ctx, _) = instance(0);
        let _p = start(&[format!("http://{addr}")], &ctx);
        let id = format!("peer:{addr}:m");
        wait_for("polled device", || ctx.hub.find(&id).is_some());
        let d = ctx.hub.find(&id).unwrap();
        assert_eq!((d.battery.unwrap().status, d.info.host.as_deref()), (Status::Charging, Some("127.0.0.1")));
    }
}
