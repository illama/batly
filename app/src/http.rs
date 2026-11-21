//! Minimal HTTP server exposing the devices.
//!
//! * `GET /` or `/devices`          – live dashboard (HTML)
//! * `GET /overlay`                 – transparent overlay for OBS (`?device=&size=&theme=&layout=`)
//! * `GET /device/{id|name}`        – XML, same fields as the original app
//! * `GET /api/devices`             – JSON array
//! * `GET /api/device/{id|name}`    – JSON object
//! * `GET /api/events`              – Server-Sent Events: `device` on every change, `: ping` every 25 s
//! * `GET /api/history/{id}?days=N` – readings and statistics (JSON)
//! * `GET /metrics`                 – Prometheus text format
//!
//! `?local=1` leaves out the devices of other Batly instances: peers use
//! it so that two instances merging each other do not loop. When a token is
//! set, requests that do not come from this computer need `?token=` or
//! `Authorization: Bearer`.
//!
//! Built on `std::net` only: no HTTP.sys, hence no admin rights needed to
//! listen on every interface.

use crate::config;
use crate::history::{self, History, Sample, Stats};
use crate::i18n::{self, tr};
use crate::log;
use crate::model::{Device, DeviceKind, Hub, Source, Status};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read as _, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Simultaneous connections, and among them live event streams.
const MAX_CONNECTIONS: usize = 64;
const MAX_STREAMS: usize = 32;
const PING: Duration = Duration::from_secs(25);

/// Display name of a device (its alias), from its id and reported name.
pub type DisplayName = Box<dyn Fn(&str, &str) -> String + Send + Sync>;

/// Everything the server (and the integrations built on its JSON) needs.
pub struct Context {
    pub hub: Arc<Hub>,
    pub history: Arc<History>,
    pub display_name: DisplayName,
    /// Random id of this process, announced to peers to detect a loop
    /// back to ourselves.
    pub instance: String,
    /// Computer name, announced to peers.
    pub host: String,
}

impl Context {
    pub fn new(hub: Arc<Hub>, history: Arc<History>) -> Self {
        Self { hub, history, display_name: Box::new(|_, name| name.to_string()), instance: new_instance_id(), host: hostname() }
    }

    pub fn with_names(mut self, f: impl Fn(&str, &str) -> String + Send + Sync + 'static) -> Self {
        self.display_name = Box::new(f);
        self
    }

    pub fn name(&self, d: &Device) -> String {
        (self.display_name)(&d.id, &d.name)
    }

    fn alias(&self, d: &Device) -> Option<String> {
        Some(self.name(d)).filter(|n| *n != d.name)
    }

    /// Current devices; `local` leaves out the ones of other instances.
    pub fn devices(&self, local: bool) -> Vec<Device> {
        let mut list = self.hub.snapshot();
        if local {
            list.retain(|d| d.source != Source::Remote);
        }
        list
    }

    /// Estimates from the history, or G HUB's (or the peer's) own.
    pub fn estimate(&self, d: &Device) -> Estimate {
        let stats = if d.has_battery { self.history.stats(&d.id) } else { Stats::default() };
        let b = d.battery.filter(|_| d.online);
        let mileage = b.and_then(|b| b.mileage).filter(|h| h.is_finite() && *h > 0.0);
        let (left, full) = match b.map(|b| b.status) {
            Some(Status::Charging) => (None, stats.time_to_full_hours),
            Some(Status::Discharging | Status::Unknown) => (stats.time_left_hours.or(mileage), None),
            _ => (None, None),
        };
        Estimate { time_left_hours: left, time_to_full_hours: full, stats }
    }
}

pub struct Estimate {
    pub time_left_hours: Option<f64>,
    pub time_to_full_hours: Option<f64>,
    pub stats: Stats,
}

fn new_instance_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    format!("{:016x}", h.finish())
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default()
}

/// Starts the listeners; returns the addresses actually bound.
pub fn start(cfg: &config::Http, ctx: Arc<Context>) -> Vec<SocketAddr> {
    let token: Arc<str> = cfg.token.trim().into();
    let (connections, streams) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut bound = Vec::new();
    for addr in resolve(cfg) {
        match TcpListener::bind(addr) {
            Ok(listener) => {
                bound.push(listener.local_addr().unwrap_or(addr));
                let (ctx, token, connections, streams) = (ctx.clone(), token.clone(), connections.clone(), streams.clone());
                let _ = thread::Builder::new().name("http".into()).stack_size(128 * 1024).spawn(move || {
                    for stream in listener.incoming().flatten() {
                        let Some(slot) = Slot::take(&connections, MAX_CONNECTIONS) else { continue };
                        let (ctx, token, streams) = (ctx.clone(), token.clone(), streams.clone());
                        let _ = thread::Builder::new().name("http-conn".into()).stack_size(256 * 1024).spawn(move || {
                            let _slot = slot;
                            if let Err(e) = handle(stream, &ctx, &token, &streams) {
                                log!("http: {e}");
                            }
                        });
                    }
                });
            }
            Err(e) => log!("http: cannot bind {addr}: {e}"),
        }
    }
    bound
}

/// One unit of a bounded counter, given back on drop.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(counter: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        let slot = Self(counter.clone());
        (counter.fetch_add(1, Ordering::SeqCst) < max).then_some(slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn resolve(cfg: &config::Http) -> Vec<SocketAddr> {
    let port = cfg.port;
    let mut addrs: Vec<SocketAddr> = match cfg.addr.trim() {
        "localhost" | "" => vec![(Ipv4Addr::LOCALHOST, port).into(), (Ipv6Addr::LOCALHOST, port).into()],
        "0.0.0.0" | "+" | "*" => vec![(Ipv4Addr::UNSPECIFIED, port).into()],
        host => match host.parse::<IpAddr>() {
            Ok(ip) => vec![(ip, port).into()],
            Err(_) => (host, port).to_socket_addrs().map(Iterator::collect).unwrap_or_default(),
        },
    };
    if !cfg.use_ipv6 {
        addrs.retain(SocketAddr::is_ipv4);
    }
    addrs.dedup();
    addrs
}

fn handle(stream: TcpStream, ctx: &Context, token: &str, streams: &Arc<AtomicUsize>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::with_capacity(2048, &stream);
    let mut request_line = String::new();
    reader.by_ref().take(4096).read_line(&mut request_line)?;
    // Read (and drain) the headers; only Authorization matters.
    let mut auth = None;
    let mut line = String::new();
    for _ in 0..100 {
        line.clear();
        if reader.by_ref().take(8192).read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("authorization") {
                auth = Some(value.trim().to_string());
            }
        }
    }

    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let loopback = stream.peer_addr().is_ok_and(|a| is_loopback(a.ip()));
    let req = Request { method, target, authorized: authorized(token, loopback, target, auth.as_deref()) };
    let mut out = &stream;
    if method == "GET" && req.authorized && split_target(target).0 == "/api/events" {
        return match Slot::take(streams, MAX_STREAMS) {
            Some(_slot) => event_stream(&stream, ctx, query_flag(target, "local")),
            None => out.write_all(&Response::new(503, "text/plain", "Too many clients.".into()).to_bytes(false)),
        };
    }
    let response = respond(&req, ctx);
    out.write_all(&response.to_bytes(method == "HEAD"))?;
    out.flush()
}

pub fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_loopback(),
        IpAddr::V6(a) => a.is_loopback() || a.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    }
}

/// Requests from this computer, or any request when no token is set, are
/// let through; others must give the token in the query or as a bearer.
pub fn authorized(token: &str, loopback: bool, target: &str, authorization: Option<&str>) -> bool {
    if token.is_empty() || loopback {
        return true;
    }
    let bearer = authorization
        .map(str::trim)
        .filter(|h| h.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("bearer ")))
        .map(|h| h[7..].trim().to_string());
    query(target, "token").or(bearer).is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes()))
}

/// Compares without stopping at the first difference.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0xFF)) as usize;
    }
    diff == 0
}

pub struct Request<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub authorized: bool,
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Response {
    fn new(status: u16, content_type: &'static str, body: String) -> Self {
        Self { status, content_type, body }
    }

    fn to_bytes(&self, head_only: bool) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            204 => "No Content",
            401 => "Unauthorized",
            404 => "Not Found",
            405 => "Method Not Allowed",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let mut out = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Headers: Authorization\r\n{}Connection: close\r\n\r\n",
            self.status,
            reason,
            self.content_type,
            self.body.len(),
            if self.status == 401 { "WWW-Authenticate: Bearer\r\n" } else { "" },
        )
        .into_bytes();
        if !head_only {
            out.extend_from_slice(self.body.as_bytes());
        }
        out
    }
}

/// Path without the query, trailing '/' removed; and the query.
fn split_target(target: &str) -> (&str, &str) {
    let target = target.split('#').next().unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (if path.len() > 1 { path.trim_end_matches('/') } else { path }, query)
}

/// Decoded value of a query parameter.
pub fn query(target: &str, key: &str) -> Option<String> {
    split_target(target)
        .1
        .split('&')
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(&v.replace('+', " ")))
}

fn query_flag(target: &str, key: &str) -> bool {
    query(target, key).is_some_and(|v| matches!(v.as_str(), "" | "1" | "true" | "yes"))
}

pub fn respond(req: &Request, ctx: &Context) -> Response {
    match req.method {
        "GET" | "HEAD" => {}
        "OPTIONS" => return Response::new(204, "text/plain", String::new()),
        _ => return Response::new(405, "text/plain", "Method not allowed.".into()),
    }
    if !req.authorized {
        return Response::new(401, "text/plain", "Token required.".into());
    }
    let (path, _) = split_target(req.target);
    let local = query_flag(req.target, "local");

    match path {
        "/" | "/devices" => Response::new(200, "text/html", page::index(ctx)),
        "/overlay" => Response::new(200, "text/html", page::overlay(req.target)),
        "/metrics" => Response::new(200, "text/plain; version=0.0.4", metrics(ctx, local)),
        "/api/devices" => {
            let items: Vec<String> = ctx.devices(local).iter().map(|d| device_json(ctx, d)).collect();
            Response::new(200, "application/json", format!("[{}]", items.join(",")))
        }
        // Without a live connection (tests): the initial burst only.
        "/api/events" => Response::new(200, "text/event-stream", {
            let mut s = hello(ctx);
            for d in ctx.devices(local) {
                s.push_str(&sse("device", &device_json(ctx, &d)));
            }
            s + &sse("synced", "{}")
        }),
        _ => {
            let routes: [(&str, fn(&Context, &Request, &str) -> Response); 4] = [
                ("/api/device/", |ctx, _, key| found(ctx, key, |d| Response::new(200, "application/json", device_json(ctx, &d)))),
                ("/device/", |ctx, _, key| found(ctx, key, |d| Response::new(200, "text/xml", xml(&d)))),
                ("/api/history/", history_json),
                ("/ui/card/", |ctx, _, key| match ctx.hub.find(key) {
                    Some(d) => Response::new(200, "text/html", page::card(ctx, &d)),
                    None => Response::new(404, "text/html", String::new()),
                }),
            ];
            for (prefix, route) in routes {
                if let Some(key) = path.strip_prefix(prefix) {
                    return route(ctx, req, &percent_decode(key));
                }
            }
            Response::new(404, "text/plain", "Not found.".into())
        }
    }
}

fn found(ctx: &Context, key: &str, f: impl FnOnce(Device) -> Response) -> Response {
    match ctx.hub.find(key) {
        Some(d) => f(d),
        None => Response::new(404, "text/plain", format!("{key} not found.")),
    }
}

fn history_json(ctx: &Context, req: &Request, key: &str) -> Response {
    let days = query(req.target, "days").and_then(|d| d.trim().parse::<u64>().ok()).unwrap_or(7).clamp(1, 3650);
    let id = ctx.hub.find(key).map_or_else(|| key.to_string(), |d| d.id);
    let samples = ctx.history.samples(&id, history::now().saturating_sub(days * 86_400));
    let st = ctx.history.stats(&id);
    let list: Vec<Value> = samples
        .iter()
        .map(|s| {
            json!({
                "t": s.t,
                "time": iso8601(UNIX_EPOCH + Duration::from_secs(s.t)),
                "percent": s.percent.round(),
                "status": s.status.as_str(),
                "millivolts": s.millivolts,
            })
        })
        .collect();
    let body = json!({
        "id": id,
        "days": days,
        "samples": list,
        "stats": {
            "drain_per_hour": st.drain_per_hour,
            "time_left_hours": st.time_left_hours,
            "time_to_full_hours": st.time_to_full_hours,
            "full_runtime_hours": st.full_runtime_hours,
            "avg_charge_hours": st.avg_charge_hours,
            "cycles": st.cycles,
            "health_percent": st.health_percent,
            "full_mv_early": st.full_mv_early,
            "full_mv_recent": st.full_mv_recent,
            "since": st.since,
            "samples": st.samples,
        },
    });
    Response::new(200, "application/json", body.to_string())
}

// ------------------------------------------------------------ live events

fn sse(event: &str, data: &str) -> String {
    // Our JSON never contains a newline; split anyway to stay valid.
    let mut s = format!("event: {event}\n");
    for line in data.lines() {
        s.push_str("data: ");
        s.push_str(line);
        s.push('\n');
    }
    s + "\n"
}

fn hello(ctx: &Context) -> String {
    let data = json!({ "instance": ctx.instance, "host": ctx.host, "version": VERSION });
    format!("retry: 5000\n\n{}", sse("hello", &data.to_string()))
}

/// Streams `device` events until the client goes away: every device on
/// connection, then each device whose JSON changed. A removed device is
/// sent as `{"id": …, "removed": true}`.
fn event_stream(stream: &TcpStream, ctx: &Context, local: bool) -> io::Result<()> {
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut out = stream;
    out.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\n\
          Access-Control-Allow-Origin: *\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n",
    )?;
    out.write_all(hello(ctx).as_bytes())?;
    let mut sent: HashMap<String, String> = HashMap::new();
    let mut first = true;
    loop {
        let generation = ctx.hub.generation();
        let mut chunk = String::new();
        let devices = ctx.devices(local);
        for d in &devices {
            let j = device_json(ctx, d);
            if sent.get(&d.id) != Some(&j) {
                chunk.push_str(&sse("device", &j));
                sent.insert(d.id.clone(), j);
            }
        }
        sent.retain(|id, _| {
            let keep = devices.iter().any(|d| d.id == *id);
            if !keep {
                chunk.push_str(&sse("device", &json!({ "id": id, "removed": true }).to_string()));
            }
            keep
        });
        if std::mem::take(&mut first) {
            chunk.push_str(&sse("synced", "{}"));
        }
        if !chunk.is_empty() {
            out.write_all(chunk.as_bytes())?;
            out.flush()?;
        }
        while ctx.hub.wait_change(generation, PING) == generation {
            out.write_all(b": ping\n\n")?;
            out.flush()?;
        }
    }
}

// ------------------------------------------------------------ formats

/// Same element names as the original application so existing consumers
/// (Rainmeter skins, Stream Deck plugins, …) keep working; `status`,
/// `online` and `source` are additions.
pub fn xml(d: &Device) -> String {
    let b = d.battery;
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><xml>\
         <device_id>{}</device_id><device_name>{}</device_name><device_type>{}</device_type>\
         <battery_percent>{:.2}</battery_percent><battery_voltage>{:.2}</battery_voltage>\
         <mileage>{:.2}</mileage><charging>{}</charging><last_update>{}</last_update>\
         <status>{}</status><online>{}</online><source>{}</source></xml>",
        escape(&d.id),
        escape(&d.name),
        d.kind.as_str(),
        b.map_or(-1.0, |b| b.percent),
        b.and_then(|b| b.millivolts).map_or(0.0, |mv| mv as f32 / 1000.0),
        b.and_then(|b| b.mileage).unwrap_or(-1.0),
        if b.is_some_and(|b| b.status == Status::Charging) { "True" } else { "False" },
        d.last_update.map(iso8601).unwrap_or_default(),
        b.map_or(Status::Unknown, |b| b.status).as_str(),
        if d.online { "True" } else { "False" },
        d.source.as_str(),
    )
}

/// Base JSON object of a device (fields of LGSTray 4.0).
pub fn json(d: &Device) -> String {
    let b = d.battery;
    let opt = |v: Option<String>| v.unwrap_or_else(|| "null".into());
    format!(
        "{{\"id\":{},\"name\":{},\"type\":\"{}\",\"source\":\"{}\",\"online\":{},\"has_battery\":{},\
         \"battery_percent\":{},\"battery_voltage\":{},\"mileage\":{},\"status\":\"{}\",\"charging\":{},\"last_update\":{}}}",
        Value::from(d.id.as_str()),
        Value::from(d.name.as_str()),
        d.kind.as_str(),
        d.source.as_str(),
        d.online,
        d.has_battery,
        opt(b.map(|b| format!("{:.0}", b.percent))),
        opt(b.and_then(|b| b.millivolts).map(|mv| format!("{:.3}", mv as f32 / 1000.0))),
        opt(b.and_then(|b| b.mileage).filter(|m| m.is_finite()).map(|m| format!("{m:.2}"))),
        b.map_or(Status::Unknown, |b| b.status).as_str(),
        b.is_some_and(|b| b.status == Status::Charging),
        opt(d.last_update.map(|t| format!("\"{}\"", iso8601(t)))),
    )
}

/// `json` plus `alias`, `time_left_hours`, `time_to_full_hours`,
/// `health_percent`, `cycles` and `host`.
pub fn device_json(ctx: &Context, d: &Device) -> String {
    let e = ctx.estimate(d);
    let num = |v: Option<f64>, digits: usize| {
        v.filter(|v| v.is_finite()).map_or_else(|| "null".to_string(), |v| format!("{v:.digits$}"))
    };
    let text = |v: Option<&str>| v.map_or(Value::Null, Value::from);
    let mut s = json(d);
    s.pop();
    s.push_str(&format!(
        ",\"alias\":{},\"time_left_hours\":{},\"time_to_full_hours\":{},\"health_percent\":{},\"cycles\":{},\"host\":{}}}",
        text(ctx.alias(d).as_deref()),
        num(e.time_left_hours, 2),
        num(e.time_to_full_hours, 2),
        num(e.stats.health_percent, 0),
        num(Some(e.stats.cycles).filter(|_| e.stats.samples > 1), 2),
        text(d.info.host.as_deref()),
    ));
    s
}

/// Prometheus label value: backslash, double quote and newline escaped.
pub fn prom_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

pub fn metrics(ctx: &Context, local: bool) -> String {
    type Metric = fn(&Device, &Estimate) -> Option<f64>;
    let families: [(&str, &str, Metric); 9] = [
        ("batly_battery_percent", "Battery level (0-100).", |d, _| d.battery.map(|b| b.percent as f64)),
        ("batly_battery_voltage_volts", "Battery voltage.", |d, _| d.battery?.millivolts.map(|mv| mv as f64 / 1000.0)),
        ("batly_charging", "1 while charging.", |d, _| d.battery.map(|b| (b.status == Status::Charging) as u8 as f64)),
        ("batly_online", "1 when the device answers.", |d, _| Some(d.online as u8 as f64)),
        ("batly_time_left_hours", "Estimated time until empty.", |_, e| e.time_left_hours),
        ("batly_time_to_full_hours", "Estimated time until full.", |_, e| e.time_to_full_hours),
        ("batly_battery_health_percent", "Full-charge runtime compared with the first weeks.", |_, e| e.stats.health_percent),
        ("batly_battery_cycles", "Full-charge equivalents recorded.", |_, e| Some(e.stats.cycles).filter(|_| e.stats.samples > 1)),
        ("batly_last_update_timestamp_seconds", "Time of the last reading.", |d, _| {
            d.last_update.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|t| t.as_secs() as f64)
        }),
    ];
    let devices: Vec<(Device, Estimate, String)> = ctx
        .devices(local)
        .into_iter()
        .filter(|d| d.has_battery)
        .map(|d| {
            let labels = format!(
                "id=\"{}\",name=\"{}\",type=\"{}\",source=\"{}\",host=\"{}\"",
                prom_escape(&d.id),
                prom_escape(&ctx.name(&d)),
                d.kind.as_str(),
                d.source.as_str(),
                prom_escape(d.info.host.as_deref().unwrap_or("")),
            );
            (ctx.estimate(&d), labels, d)
        })
        .map(|(e, labels, d)| (d, e, labels))
        .collect();
    let mut s = format!("# HELP batly_info Batly version.\n# TYPE batly_info gauge\nbatly_info{{version=\"{VERSION}\"}} 1\n");
    for (name, help, value) in families {
        s.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n"));
        for (d, e, labels) in &devices {
            if let Some(v) = value(d, e).filter(|v| v.is_finite()) {
                s.push_str(&format!("{name}{{{labels}}} {}\n", (v * 1000.0).round() / 1000.0));
            }
        }
    }
    s
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// UTC timestamp, e.g. `2026-01-31T18:04:05Z`.
pub fn iso8601(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

// ------------------------------------------------------------ pages

mod page {
    use super::*;

    fn kind_name(k: DeviceKind) -> &'static str {
        match k {
            DeviceKind::Mouse => tr(["Mouse", "Souris", "Maus", "Ratón", "Mouse", "Mouse"]),
            DeviceKind::Keyboard => tr(["Keyboard", "Clavier", "Tastatur", "Teclado", "Tastiera", "Teclado"]),
            DeviceKind::Headset => tr(["Headset", "Casque", "Headset", "Auriculares", "Cuffie", "Headset"]),
            DeviceKind::Gamepad => tr(["Controller", "Manette", "Controller", "Mando", "Controller", "Controle"]),
            DeviceKind::Earbuds => tr(["Earbuds", "Écouteurs", "Ohrhörer", "Auriculares inalámbricos", "Auricolari", "Fones"]),
            DeviceKind::Other => tr(["Device", "Périphérique", "Gerät", "Dispositivo", "Dispositivo", "Dispositivo"]),
        }
    }

    fn capitalize(s: &str) -> String {
        let mut c = s.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
    }

    const STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#f5f6f8;--card:#fff;--fg:#1c1f24;--mute:#6b7280;--line:#e4e7eb;--ok:#1f9d55;--warn:#d48a00;--bad:#d64545;--accent:#2f6fde}
@media(prefers-color-scheme:dark){:root{--bg:#111316;--card:#1a1d22;--fg:#e7e9ec;--mute:#9aa1ab;--line:#2b2f36;--ok:#3cc47a;--warn:#f0a830;--bad:#ef6161;--accent:#6ea2ff}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
header,main,footer{max-width:1120px;margin:0 auto;padding:20px 16px}
header{display:flex;align-items:center;justify-content:space-between;gap:12px;padding-bottom:4px}
h1{font-size:20px;font-weight:600;margin:0}
#live{font-size:13px;color:var(--mute);white-space:nowrap}
#live::before{content:"";display:inline-block;width:8px;height:8px;border-radius:50%;background:var(--ok);margin-right:6px;vertical-align:1px}
#live.off::before{background:var(--bad)}
#grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:16px}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:16px;display:flex;flex-direction:column;gap:10px;min-width:0}
.card.offline .top,.card.offline .bar{opacity:.55}
.top{display:flex;justify-content:space-between;align-items:flex-start;gap:12px}
.name{font-weight:600;font-size:16px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.sub{color:var(--mute);font-size:13px}
.pct{font-size:28px;font-weight:600;line-height:1.1;font-variant-numeric:tabular-nums;white-space:nowrap}
.bar{height:8px;border-radius:4px;background:var(--line);overflow:hidden}
.bar>i{display:block;height:100%;border-radius:4px;background:var(--ok);transition:width .4s}
.warn .bar>i{background:var(--warn)}.low .bar>i{background:var(--bad)}.charging .bar>i{background:var(--accent)}
.state{font-size:14px;min-height:1.45em}
dl{display:grid;grid-template-columns:1fr auto;gap:2px 12px;margin:0;font-size:13px}
dt{color:var(--mute)}dd{margin:0;text-align:right;font-variant-numeric:tabular-nums}
.chart{color:var(--accent)}.chart svg{display:block;width:100%;height:72px}
.chart .grid line{stroke:var(--line);stroke-width:1;vector-effect:non-scaling-stroke}
.chart .area{fill:currentColor;opacity:.14}.chart .line{fill:none;stroke:currentColor;stroke-width:1.6;vector-effect:non-scaling-stroke}
.chart .cap{display:flex;justify-content:space-between;color:var(--mute);font-size:12px}
.chart .none{color:var(--mute);font-size:13px;padding:8px 0}
.links{display:flex;flex-wrap:wrap;gap:4px 12px;font-size:12px}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
#empty{color:var(--mute);padding:32px 0;text-align:center}
footer{color:var(--mute);font-size:13px;display:flex;flex-wrap:wrap;gap:4px 14px}
footer code{font-size:12px}
"#;

    const SCRIPT: &str = r#"
(function(){
var m=location.search.match(/[?&]token=([^&]*)/),tk=m?'token='+m[1]:'';
function u(p){return tk?p+(p.indexOf('?')<0?'?':'&')+tk:p}
var grid=document.getElementById('grid'),live=document.getElementById('live'),empty=document.getElementById('empty'),pend={};
function find(id){for(var i=0;i<grid.children.length;i++)if(grid.children[i].dataset.id===id)return grid.children[i]}
function times(root){var t=root.querySelectorAll('time[datetime]');for(var i=0;i<t.length;i++){var d=new Date(t[i].getAttribute('datetime'));if(!isNaN(d))t[i].textContent=d.toLocaleString(document.documentElement.lang,{dateStyle:'short',timeStyle:'short'})}}
function refresh(id){if(pend[id])return;pend[id]=setTimeout(function(){delete pend[id];
fetch(u('/ui/card/'+encodeURIComponent(id))).then(function(r){return r.ok?r.text():''}).then(function(h){
var old=find(id);if(!h){if(old)old.remove()}else{var t=document.createElement('template');t.innerHTML=h;var n=t.content.firstElementChild;times(n);if(old)old.replaceWith(n);else grid.appendChild(n)}
empty.hidden=grid.children.length>0}).catch(function(){})},250)}
times(document);
if(!window.EventSource)return;
var es=new EventSource(u('/api/events'));
es.onopen=function(){live.className='';live.textContent=live.dataset.on};
es.onerror=function(){live.className='off';live.textContent=live.dataset.off};
es.addEventListener('device',function(e){try{refresh(JSON.parse(e.data).id)}catch(x){}});
})();
"#;

    pub fn index(ctx: &Context) -> String {
        let devices = ctx.devices(false);
        let title = tr(["Battery levels", "Niveaux de batterie", "Akkustände", "Niveles de batería", "Livelli della batteria", "Níveis de bateria"]);
        let mut s = format!(
            "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Batly – {title}</title>\
             <style>{STYLE}</style></head><body><header><h1>{title}</h1>\
             <span id=\"live\" class=\"off\" data-on=\"{on}\" data-off=\"{off}\">{off}</span></header><main><div id=\"grid\">",
            lang = i18n::lang().code(),
            on = tr(["Live", "En direct", "Live", "En directo", "In diretta", "Ao vivo"]),
            off = tr(["Reconnecting…", "Reconnexion…", "Verbinde…", "Reconectando…", "Riconnessione…", "Reconectando…"]),
        );
        for d in &devices {
            s.push_str(&card(ctx, d));
        }
        s.push_str(&format!(
            "</div><p id=\"empty\"{}>{}</p></main><footer><span>API :</span>\
             <a href=\"/api/devices\"><code>/api/devices</code></a><a href=\"/api/events\"><code>/api/events</code></a>\
             <a href=\"/metrics\"><code>/metrics</code></a><a href=\"/overlay\"><code>/overlay</code></a>\
             <a href=\"/devices\"><code>/device/{{id}}</code></a><span>Batly {VERSION}</span></footer>\
             <script>{SCRIPT}</script></body></html>",
            if devices.is_empty() { "" } else { " hidden" },
            i18n::no_device(),
        ));
        s
    }

    /// One device card; also served alone by `/ui/card/{id}` for live updates.
    pub fn card(ctx: &Context, d: &Device) -> String {
        let name = ctx.name(d);
        let e = ctx.estimate(d);
        let b = d.battery;
        let level = b.map(|b| b.percent);
        let mut class = String::from("card");
        if !d.online {
            class.push_str(" offline");
        }
        match (b.map(|b| b.status), level) {
            (Some(Status::Charging), _) => class.push_str(" charging"),
            (_, Some(p)) if p <= 15.0 => class.push_str(" low"),
            (_, Some(p)) if p <= 30.0 => class.push_str(" warn"),
            _ => {}
        }
        let mut sub = kind_name(d.kind).to_string();
        if let Some(host) = &d.info.host {
            sub.push_str(&format!(" · {} {}", tr(["on", "sur", "auf", "en", "su", "em"]), host));
        }
        if name != d.name {
            sub.push_str(&format!(" · {}", d.name));
        }
        let pct = match level {
            Some(p) if d.has_battery => i18n::percent(p),
            _ => "–".into(),
        };
        let state = if !d.has_battery {
            capitalize(i18n::no_battery())
        } else if !d.online {
            capitalize(i18n::offline())
        } else {
            match b.map(|b| b.status) {
                Some(Status::Charging) => match e.time_to_full_hours {
                    Some(h) => format!("{} · {}", capitalize(i18n::charging()), i18n::time_to_full(h)),
                    None => capitalize(i18n::charging()),
                },
                Some(Status::Full) => capitalize(i18n::full()),
                None => capitalize(i18n::unknown_level()),
                _ => e.time_left_hours.map(i18n::time_left).unwrap_or_default(),
            }
        };

        let mut rows = String::new();
        let mut row = |k: &str, v: String| rows.push_str(&format!("<dt>{k}</dt><dd>{}</dd>", escape(&v)));
        if let Some(mv) = b.and_then(|b| b.millivolts) {
            row(tr(["Voltage", "Tension", "Spannung", "Voltaje", "Tensione", "Tensão"]), format!("{:.2}\u{a0}V", mv as f32 / 1000.0));
        }
        let st = &e.stats;
        if let Some(h) = st.full_runtime_hours {
            row(tr(["Runtime per charge", "Autonomie par charge", "Laufzeit pro Ladung", "Autonomía por carga", "Autonomia per carica", "Autonomia por carga"]), i18n::duration(h));
        }
        if let Some(h) = st.avg_charge_hours {
            row(tr(["Usual charge time", "Durée de charge habituelle", "Übliche Ladedauer", "Tiempo de carga habitual", "Durata di carica abituale", "Tempo de carga habitual"]), i18n::duration(h));
        }
        if let Some(h) = st.health_percent {
            row(tr(["Battery health", "Santé de la batterie", "Akkuzustand", "Salud de la batería", "Salute della batteria", "Saúde da bateria"]), i18n::percent(h as f32));
        }
        if st.samples > 1 && st.cycles >= 0.1 {
            row(tr(["Charge cycles", "Cycles de charge", "Ladezyklen", "Ciclos de carga", "Cicli di carica", "Ciclos de carga"]), format!("{:.1}", st.cycles));
        }
        drop(row);
        if let Some(t) = d.last_update {
            let iso = iso8601(t);
            rows.push_str(&format!(
                "<dt>{}</dt><dd><time datetime=\"{iso}\">{iso}</time></dd>",
                tr(["Last reading", "Dernier relevé", "Letzte Messung", "Última lectura", "Ultima lettura", "Última leitura"])
            ));
        }

        let chart_html = if d.has_battery {
            let now = history::now();
            let samples = ctx.history.samples(&d.id, now.saturating_sub(7 * 86_400));
            let live = d.online.then_some(level).flatten();
            match chart(&samples, now, 7, live) {
                Some(svg) => format!(
                    "<div class=\"chart\">{svg}<div class=\"cap\"><span>{}</span><span>100\u{a0}%</span></div></div>",
                    tr(["Last 7 days", "7 derniers jours", "Letzte 7 Tage", "Últimos 7 días", "Ultimi 7 giorni", "Últimos 7 dias"])
                ),
                None => format!(
                    "<div class=\"chart\"><div class=\"none\">{}</div></div>",
                    tr(["Not enough readings yet for a chart.", "Pas encore assez de relevés pour un graphique.", "Noch nicht genug Messwerte für ein Diagramm.", "Aún no hay lecturas suficientes para un gráfico.", "Letture ancora insufficienti per un grafico.", "Ainda não há leituras suficientes para um gráfico."])
                ),
            }
        } else {
            String::new()
        };

        let id_url = percent_encode(&d.id);
        format!(
            "<article class=\"{class}\" data-id=\"{id}\"><div class=\"top\"><div style=\"min-width:0\">\
             <div class=\"name\" title=\"{name}\">{name}</div><div class=\"sub\">{sub}</div></div><div class=\"pct\">{pct}</div></div>\
             <div class=\"bar\" role=\"progressbar\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-valuenow=\"{now:.0}\"><i style=\"width:{now:.0}%\"></i></div>\
             <div class=\"state\">{state}</div><dl>{rows}</dl>{chart_html}\
             <div class=\"links\"><a href=\"/device/{id_url}\">XML</a><a href=\"/api/device/{id_url}\">JSON</a>\
             <a href=\"/api/history/{id_url}?days=30\">{hist}</a><a href=\"/overlay?device={id_url}\">Overlay</a></div></article>",
            id = escape(&d.id),
            name = escape(&name),
            sub = escape(&sub),
            pct = escape(&pct),
            now = level.filter(|_| d.has_battery).unwrap_or(0.0).clamp(0.0, 100.0),
            state = escape(&state),
            hist = tr(["History", "Historique", "Verlauf", "Historial", "Cronologia", "Histórico"]),
        )
    }

    /// Area chart of the level over the last `days` (SVG, no text so it can
    /// stretch). Gaps of more than 3 hours break the line. `live` extends
    /// the last segment up to now.
    pub fn chart(samples: &[Sample], now: u64, days: u64, live: Option<f32>) -> Option<String> {
        if samples.len() < 2 {
            return None;
        }
        let (w, h) = (300.0, 72.0);
        let start = now.saturating_sub(days * 86_400);
        let x = |t: u64| (t.saturating_sub(start) as f64 / (now - start).max(1) as f64 * w).clamp(0.0, w);
        let y = |p: f32| 2.0 + (100.0 - p.clamp(0.0, 100.0) as f64) / 100.0 * (h - 4.0);
        let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut prev: Option<u64> = None;
        for s in samples {
            if prev.is_none_or(|p| s.t.saturating_sub(p) > 3 * 3600) {
                runs.push(Vec::new());
            }
            runs.last_mut().unwrap().push((x(s.t), y(s.percent)));
            prev = Some(s.t);
        }
        if let (Some(p), Some(last)) = (live, samples.last()) {
            if now.saturating_sub(last.t) <= 3 * 3600 {
                runs.last_mut().unwrap().push((w, y(p)));
            }
        }
        let mut svg = format!(
            "<svg viewBox=\"0 0 {w} {h}\" preserveAspectRatio=\"none\" aria-hidden=\"true\"><g class=\"grid\">\
             <line x1=\"0\" x2=\"{w}\" y1=\"{a:.1}\" y2=\"{a:.1}\"/><line x1=\"0\" x2=\"{w}\" y1=\"{b:.1}\" y2=\"{b:.1}\"/>\
             <line x1=\"0\" x2=\"{w}\" y1=\"{c:.1}\" y2=\"{c:.1}\"/></g>",
            a = y(100.0),
            b = y(50.0),
            c = y(0.0)
        );
        for run in runs.iter().filter(|r| !r.is_empty()) {
            let pts: Vec<String> = run.iter().map(|(px, py)| format!("{px:.1},{py:.1}")).collect();
            let (first, last) = (run[0].0, run[run.len() - 1].0);
            svg.push_str(&format!(
                "<polygon class=\"area\" points=\"{first:.1},{h} {} {last:.1},{h}\"/><polyline class=\"line\" points=\"{}\"/>",
                pts.join(" "),
                pts.join(" ")
            ));
        }
        Some(svg + "</svg>")
    }

    /// Transparent page for OBS: `?device=<id or name>&size=32&theme=dark|light&layout=row|column&names=0`.
    pub fn overlay(target: &str) -> String {
        let size = query(target, "size").and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(32).clamp(8, 400);
        let theme = if query(target, "theme").as_deref() == Some("light") { "light" } else { "dark" };
        let layout = if query(target, "layout").as_deref() == Some("column") { "column" } else { "row" };
        let device = Value::from(query(target, "device").unwrap_or_default()).to_string().replace('<', "\\u003c");
        let names = !matches!(query(target, "names").as_deref(), Some("0" | "false" | "no"));
        format!(
            r#"<!doctype html><html><head><meta charset="utf-8"><title>Batly overlay</title><style>
html,body{{margin:0;background:transparent;overflow:hidden}}
body{{font:600 {size}px/1.2 system-ui,"Segoe UI",Roboto,sans-serif;color:var(--fg)}}
.dark{{--fg:#fff;--track:rgba(255,255,255,.22);--sh:0 1px 3px rgba(0,0,0,.85)}}
.light{{--fg:#16181c;--track:rgba(0,0,0,.14);--sh:0 1px 2px rgba(255,255,255,.7)}}
#list{{display:flex;gap:.3em .9em;padding:.25em .4em;flex-wrap:wrap}}.column #list{{flex-direction:column}}
.item{{display:flex;align-items:center;gap:.35em;text-shadow:var(--sh);white-space:nowrap}}
.bat{{position:relative;width:1.9em;height:.9em;border:.09em solid var(--fg);border-radius:.18em;padding:.08em;box-shadow:var(--sh)}}
.bat::after{{content:"";position:absolute;right:-.22em;top:.24em;width:.12em;height:.32em;background:var(--fg);border-radius:0 .08em .08em 0}}
.bat i{{display:block;height:100%;border-radius:.07em;background:#3cc47a}}
.warn .bat i{{background:#f0a830}}.low .bat i{{background:#ef6161}}.chg .bat i{{background:#5aa0ff}}
.name{{font-weight:500;opacity:.92}}.off{{opacity:.45}}
.bolt{{width:.7em;height:.9em;fill:var(--fg)}}
</style></head><body class="{theme} {layout}"><div id="list"></div><script>
(function(){{
var want={device},names={names},m=location.search.match(/[?&]token=([^&]*)/),tk=m?'?token='+m[1]:'',all={{}},list=document.getElementById('list');
function esc(s){{return String(s).replace(/[&<>"]/g,function(c){{return{{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}}[c]}})}}
function match(d){{return want?(d.id===want||d.name===want||d.alias===want):d.has_battery}}
function draw(){{var h='';Object.keys(all).sort().forEach(function(k){{var d=all[k];if(!match(d))return;
var p=d.battery_percent,c=d.charging?'chg':p==null?'':p<=15?'low':p<=30?'warn':'';
h+='<div class="item '+c+(d.online?'':' off')+'">'+(names?'<span class="name">'+esc(d.alias||d.name)+'</span>':'')+
'<span class="bat"><i style="width:'+(p==null?0:p)+'%"></i></span>'+(d.charging?'<svg class="bolt" viewBox="0 0 10 14"><path d="M6 0 0 8h4l-1 6 7-9H6z"/></svg>':'')+
'<span>'+(p==null?'–':Math.round(p)+'%')+'</span></div>'}});list.innerHTML=h}}
var es=new EventSource('/api/events'+tk);
es.addEventListener('device',function(e){{var d=JSON.parse(e.data);if(d.removed)delete all[d.id];else all[d.id]=d;draw()}});
}})();
</script></body></html>"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DeviceInfo, Event};

    fn hub() -> Hub {
        let hub = Hub::new();
        hub.publish(Event::Init {
            id: "ABC123".into(),
            name: "G <Pro> & Co".into(),
            kind: DeviceKind::Mouse,
            source: Source::Native,
            has_battery: true,
        });
        hub.publish(Event::Update {
            id: "ABC123".into(),
            battery: Battery { percent: 69.0, status: Status::Charging, millivolts: Some(3920), mileage: None },
        });
        hub
    }

    fn ctx_of(hub: Hub) -> Context {
        Context::new(Arc::new(hub), Arc::new(History::open(None, 30)))
    }

    fn get(ctx: &Context, target: &str) -> Response {
        respond(&Request { method: "GET", target, authorized: true }, ctx)
    }

    #[test]
    fn routes() {
        let ctx = ctx_of(hub());
        let r = get(&ctx, "/");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("G &lt;Pro&gt; &amp; Co") && r.body.contains("/device/ABC123"));
        assert!(r.body.contains("/api/events") && !r.body.contains("http://") && !r.body.contains("https://"));
        assert_eq!(get(&ctx, "/devices").status, 200);

        let r = get(&ctx, "/device/ABC123?x=1");
        assert_eq!((r.status, r.content_type), (200, "text/xml"));
        assert!(r.body.contains("<device_id>ABC123</device_id>"));
        assert!(r.body.contains("<device_name>G &lt;Pro&gt; &amp; Co</device_name>"));
        assert!(r.body.contains("<device_type>Mouse</device_type>"));
        assert!(r.body.contains("<battery_percent>69.00</battery_percent>"));
        assert!(r.body.contains("<battery_voltage>3.92</battery_voltage>"));
        assert!(r.body.contains("<charging>True</charging>"));

        // Lookup by (percent-encoded) name.
        assert_eq!(get(&ctx, "/device/G%20%3CPro%3E%20%26%20Co").status, 200);
        assert_eq!(get(&ctx, "/device/nope").status, 404);
        assert_eq!(get(&ctx, "/nothing").status, 404);
        assert_eq!(respond(&Request { method: "POST", target: "/", authorized: true }, &ctx).status, 405);
        assert_eq!(respond(&Request { method: "OPTIONS", target: "/api/devices", authorized: false }, &ctx).status, 204);

        let r = get(&ctx, "/api/devices/");
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v[0]["name"], "G <Pro> & Co");
        assert_eq!(v[0]["battery_percent"], 69);
        assert_eq!(v[0]["charging"], true);
        assert_eq!(v[0]["mileage"], Value::Null);
        assert_eq!(v[0]["alias"], Value::Null);
        assert_eq!(v[0]["host"], Value::Null);
        assert!(v[0].get("time_left_hours").is_some());
        let r = get(&ctx, "/api/device/ABC123");
        assert!(serde_json::from_str::<Value>(&r.body).unwrap().is_object());

        let r = get(&ctx, "/ui/card/ABC123");
        assert!(r.body.starts_with("<article") && r.body.contains("data-id=\"ABC123\""));
        assert_eq!(get(&ctx, "/ui/card/nope").status, 404);
    }

    #[test]
    fn alias_time_left_and_remote_filter() {
        let hub = hub();
        hub.publish(Event::Init { id: "peer:pc:X".into(), name: "K".into(), kind: DeviceKind::Keyboard, source: Source::Remote, has_battery: true });
        hub.publish(Event::Update {
            id: "peer:pc:X".into(),
            battery: Battery { percent: 40.0, status: Status::Discharging, millivolts: None, mileage: Some(12.5) },
        });
        hub.publish(Event::Info { id: "peer:pc:X".into(), info: DeviceInfo { host: Some("pc".into()), ..Default::default() } });
        let ctx = ctx_of(hub).with_names(|id, name| if id == "ABC123" { "Ma souris".into() } else { name.into() });
        let all: Value = serde_json::from_str(&get(&ctx, "/api/devices").body).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert_eq!(all[0]["alias"], "Ma souris");
        assert_eq!(all[1]["host"], "pc");
        assert_eq!(all[1]["time_left_hours"], 12.5);
        let local: Value = serde_json::from_str(&get(&ctx, "/api/devices?local=1").body).unwrap();
        assert_eq!(local.as_array().unwrap().len(), 1);
        let ev = get(&ctx, "/api/events?local=1").body;
        assert!(ev.contains("event: hello") && ev.contains("ABC123") && !ev.contains("peer:pc:X") && ev.contains("event: synced"));
        assert!(get(&ctx, "/").body.contains("Ma souris"));
    }

    #[test]
    fn token_checks() {
        assert!(authorized("", false, "/", None));
        assert!(authorized("s3cret", true, "/", None));
        assert!(!authorized("s3cret", false, "/", None));
        assert!(authorized("s3cret", false, "/api/devices?local=1&token=s3cret", None));
        assert!(!authorized("s3cret", false, "/api/devices?token=s3cre", None));
        assert!(authorized("s3cret", false, "/", Some("Bearer s3cret")));
        assert!(authorized("s3cret", false, "/", Some("bearer  s3cret ")));
        assert!(!authorized("s3cret", false, "/", Some("Basic s3cret")));
        assert!(authorized("a b", false, "/?token=a%20b", None));
        assert!(constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd") && !constant_time_eq(b"ab", b"abc"));
        assert!(!constant_time_eq(b"", b"\xFF"));
        assert!(is_loopback("::ffff:127.0.0.1".parse().unwrap()) && is_loopback("::1".parse().unwrap()));
        assert!(!is_loopback("192.168.1.2".parse().unwrap()));
        let ctx = ctx_of(hub());
        assert_eq!(respond(&Request { method: "GET", target: "/api/devices", authorized: false }, &ctx).status, 401);
    }

    #[test]
    fn prometheus_format() {
        let hub = hub();
        hub.publish(Event::Init { id: "q\"\\\n".into(), name: "N".into(), kind: DeviceKind::Other, source: Source::Vendor, has_battery: true });
        let m = metrics(&ctx_of(hub), false);
        assert!(m.contains("# TYPE batly_battery_percent gauge\n"));
        assert!(m.contains(
            "batly_battery_percent{id=\"ABC123\",name=\"G <Pro> & Co\",type=\"Mouse\",source=\"native\",host=\"\"} 69\n"
        ));
        assert!(m.contains("batly_battery_voltage_volts{id=\"ABC123\"") && m.contains("} 3.92\n"));
        assert!(m.contains("batly_charging{id=\"ABC123\"") && m.contains("batly_online{id=\"q\\\"\\\\\\n\""));
        assert_eq!(prom_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        for line in m.lines().filter(|l| !l.starts_with('#')) {
            let (_, value) = line.rsplit_once(' ').unwrap();
            assert!(value.parse::<f64>().is_ok(), "{line}");
        }
    }

    #[test]
    fn history_route_and_chart() {
        let ctx = ctx_of(hub());
        let mut d = ctx.hub.find("ABC123").unwrap();
        let t0 = history::now() - 10 * 3600;
        for h in 0..10u64 {
            d.battery.as_mut().unwrap().percent = 90.0 - h as f32 * 3.0;
            d.battery.as_mut().unwrap().status = Status::Discharging;
            ctx.history.record_at(&d, t0 + h * 3600);
        }
        let v: Value = serde_json::from_str(&get(&ctx, "/api/history/ABC123?days=2").body).unwrap();
        assert_eq!(v["samples"].as_array().unwrap().len(), 10);
        assert_eq!(v["samples"][0]["percent"], 90.0);
        assert!(v["stats"]["samples"] == 10 && v["days"] == 2);
        let svg = page::chart(&ctx.history.samples("ABC123", 0), history::now(), 7, Some(60.0)).unwrap();
        assert!(svg.starts_with("<svg") && svg.contains("polyline") && svg.ends_with("</svg>"));
        assert!(page::chart(&[], 0, 7, None).is_none());
    }

    #[test]
    fn overlay_parameters() {
        let r = page::overlay("/overlay?device=%3C/script%3E&size=9999&theme=light&layout=column&names=0");
        assert!(r.contains("class=\"light column\"") && r.contains("font:600 400px"));
        assert!(!r.contains("</script>\"") && r.contains("\\u003c/script>"));
        assert!(page::overlay("/overlay?theme=<x>").contains("class=\"dark row\""));
    }

    #[test]
    fn percent_decoding_edge_cases() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert_eq!(percent_decode("%C3%A9"), "é");
        assert_eq!(query("/x?a=1&b=c+d&e", "b").as_deref(), Some("c d"));
        assert_eq!(query("/x?a=1&e", "e").as_deref(), Some(""));
        assert_eq!(query("/x", "a"), None);
    }

    #[test]
    fn timestamps() {
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601(UNIX_EPOCH + Duration::from_secs(1_795_251_845)), "2026-11-21T09:04:05Z");
    }

    fn serve(token: &str) -> (Arc<Context>, SocketAddr) {
        let cfg = config::Http { addr: "127.0.0.1".into(), port: 0, use_ipv6: false, token: token.into(), ..Default::default() };
        let ctx = Arc::new(ctx_of(hub()));
        let bound = start(&cfg, ctx.clone());
        assert_eq!(bound.len(), 1);
        (ctx, bound[0])
    }

    #[test]
    fn serves_over_tcp() {
        let (_, addr) = serve("tok");
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /device/ABC123 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        // Loopback: no token needed.
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("Access-Control-Allow-Origin: *"));
        assert!(text.ends_with("</xml>"));
    }

    #[test]
    fn event_stream_over_tcp() {
        let (ctx, addr) = serve("");
        let s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        (&s).write_all(b"GET /api/events HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut r = BufReader::new(&s);
        let mut read_until = |needle: &str| {
            let mut all = String::new();
            let mut line = String::new();
            while !all.contains(needle) {
                line.clear();
                assert!(r.read_line(&mut line).unwrap() > 0, "{all}");
                all.push_str(&line);
            }
            all
        };
        let head = read_until("event: synced");
        assert!(head.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream"), "{head}");
        assert!(head.contains("event: hello") && head.contains(&ctx.instance) && head.contains("\"battery_percent\":69"));
        ctx.hub.publish(Event::Update {
            id: "ABC123".into(),
            battery: Battery { percent: 70.0, status: Status::Charging, millivolts: Some(3920), mileage: None },
        });
        assert!(read_until("\"battery_percent\":70").contains("event: device"));
        ctx.hub.publish(Event::Remove { id: "ABC123".into() });
        assert!(read_until("\"removed\":true").contains("ABC123"));
    }
}
