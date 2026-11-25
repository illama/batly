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
//! * `POST /api/device/{id}/settings` – changes a Logitech device of this
//!   computer (JSON body, see [`parse_settings`]): DPI, report rate,
//!   backlight, SmartShift, lighting, Easy-Switch channel
//!
//! `?local=1` leaves out the devices of other Batly instances: peers use
//! it so that two instances merging each other do not loop. When a token is
//! set, requests that do not come from this computer need `?token=` or
//! `Authorization: Bearer`.
//!
//! Changes (`POST`) always need the token, sent as `Authorization: Bearer`:
//! without a token set, the API stays read-only. This also keeps a website
//! from changing a device through a form posted to localhost.
//!
//! Web pages: only pages served from this computer (`localhost`, loopback
//! addresses) get the CORS header, so an ordinary website opened in the
//! browser cannot read the devices. Without a valid token, the `Host` header
//! must name this computer (localhost, an IP address or the computer name):
//! this defeats DNS rebinding, where a website points its own name at
//! 127.0.0.1 to read the API as if it were local.
//!
//! Built on `std::net` only: no HTTP.sys, hence no admin rights needed to
//! listen on every interface.

use crate::config;
use crate::hidpp::{ActionError, BacklightMode, Setting, WheelMode};
use crate::history::{self, History, Sample, Stats};
use crate::i18n::{self, tr};
use crate::lighting::{EffectKind, LedEffect};
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

/// Applies a setting to a Logitech device of this computer (blocking).
pub type Control = Box<dyn Fn(&str, Setting) -> Result<(), ActionError> + Send + Sync>;

/// Reads the hardware settings of a Logitech device of this computer
/// (blocking).
pub type Reader = Box<dyn Fn(&str) -> Result<crate::hidpp::DeviceSettings, ActionError> + Send + Sync>;

/// Largest request body read (`POST`).
const MAX_BODY: usize = 8192;

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
    /// Set once the Logitech support has started (`POST` requests).
    pub control: std::sync::OnceLock<Control>,
    /// Set with `control`: reads a device's settings (Home Assistant).
    pub reader: std::sync::OnceLock<Reader>,
}

impl Context {
    pub fn new(hub: Arc<Hub>, history: Arc<History>) -> Self {
        Self {
            hub,
            history,
            display_name: Box::new(|_, name| name.to_string()),
            instance: new_instance_id(),
            host: hostname(),
            control: Default::default(),
            reader: Default::default(),
        }
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
    let me: Arc<str> = hostname().into();
    let (connections, streams) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut bound = Vec::new();
    for addr in resolve(cfg) {
        match TcpListener::bind(addr) {
            Ok(listener) => {
                bound.push(listener.local_addr().unwrap_or(addr));
                let (ctx, token, me, connections, streams) = (ctx.clone(), token.clone(), me.clone(), connections.clone(), streams.clone());
                let _ = thread::Builder::new().name("http".into()).stack_size(128 * 1024).spawn(move || {
                    for stream in listener.incoming().flatten() {
                        let Some(slot) = Slot::take(&connections, MAX_CONNECTIONS) else { continue };
                        let (ctx, token, me, streams) = (ctx.clone(), token.clone(), me.clone(), streams.clone());
                        let _ = thread::Builder::new().name("http-conn".into()).stack_size(256 * 1024).spawn(move || {
                            let _slot = slot;
                            if let Err(e) = handle(stream, &ctx, &token, &me, &streams) {
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

fn handle(stream: TcpStream, ctx: &Context, token: &str, me: &str, streams: &Arc<AtomicUsize>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::with_capacity(2048, &stream);
    let mut request_line = String::new();
    reader.by_ref().take(4096).read_line(&mut request_line)?;
    // Read (and drain) the headers; only Authorization, Host, Origin and
    // Content-Length matter.
    let (mut auth, mut host, mut origin, mut length) = (None, None, None, 0usize);
    let mut line = String::new();
    for _ in 0..100 {
        line.clear();
        if reader.by_ref().take(8192).read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let (name, value) = (name.trim(), Some(value.trim().to_string()));
            if name.eq_ignore_ascii_case("authorization") {
                auth = value;
            } else if name.eq_ignore_ascii_case("host") {
                host = value;
            } else if name.eq_ignore_ascii_case("origin") {
                origin = value;
            } else if name.eq_ignore_ascii_case("content-length") {
                length = value.and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
            }
        }
    }

    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let loopback = stream.peer_addr().is_ok_and(|a| is_loopback(a.ip()));
    let host_ok = host_allowed(host.as_deref(), me);
    let mut body = String::new();
    if method == "POST" {
        if length > MAX_BODY {
            let mut out = &stream;
            return out.write_all(&Response::new(413, "text/plain", "Request too large.".into()).to_bytes(false, None));
        }
        let mut raw = vec![0; length];
        reader.read_exact(&mut raw)?;
        body = String::from_utf8_lossy(&raw).into_owned();
    }
    let req = Request {
        method,
        target,
        authorized: authorized(token, loopback, host_ok, target, auth.as_deref()),
        body: &body,
        can_write: can_write(token, auth.as_deref()),
    };
    let cors = origin.as_deref().filter(|o| origin_allowed(o));
    let mut out = &stream;
    if method == "GET" && req.authorized && split_target(target).0 == "/api/events" {
        return match Slot::take(streams, MAX_STREAMS) {
            Some(_slot) => event_stream(&stream, ctx, query_flag(target, "local"), cors),
            None => out.write_all(&Response::new(503, "text/plain", "Too many clients.".into()).to_bytes(false, cors)),
        };
    }
    let response = respond(&req, ctx);
    out.write_all(&response.to_bytes(method == "HEAD", cors))?;
    out.flush()
}

pub fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_loopback(),
        IpAddr::V6(a) => a.is_loopback() || a.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    }
}

/// A request that gives the right token (in the query or as a bearer) is
/// let through. Without it, requests from this computer, or any request when
/// no token is set, are let through only when `Host` names this computer
/// (`host_ok`, see [`host_allowed`]).
pub fn authorized(token: &str, loopback: bool, host_ok: bool, target: &str, authorization: Option<&str>) -> bool {
    if !token.is_empty() && token_given(token, target, authorization) {
        return true;
    }
    (token.is_empty() || loopback) && host_ok
}

/// Changes need the token, as a bearer only: a link or a form cannot
/// carry it, and without a token set the API is read-only.
pub fn can_write(token: &str, authorization: Option<&str>) -> bool {
    !token.is_empty() && token_given(token, "", authorization)
}

fn token_given(token: &str, target: &str, authorization: Option<&str>) -> bool {
    let bearer = authorization
        .map(str::trim)
        .filter(|h| h.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("bearer ")))
        .map(|h| h[7..].trim().to_string());
    query(target, "token").or(bearer).is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes()))
}

/// Name part of a `Host` header or of an origin's authority: port and
/// brackets removed, lower case, no trailing dot.
fn host_name(authority: &str) -> String {
    let authority = authority.trim();
    let name = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        match authority.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => authority,
        }
    };
    name.trim_end_matches('.').to_ascii_lowercase()
}

fn is_localhost(name: &str) -> bool {
    name == "localhost" || name.ends_with(".localhost")
}

/// Whether a `Host` header names this computer: localhost, an IP address
/// (no DNS involved, so no rebinding), or the computer name, alone or
/// followed by a local domain (`pc-salon.local`). Old HTTP/1.0 clients
/// without `Host` are accepted.
pub fn host_allowed(host: Option<&str>, me: &str) -> bool {
    let Some(host) = host else { return true };
    let name = host_name(host);
    let me = me.trim().to_ascii_lowercase();
    is_localhost(&name)
        || name.parse::<IpAddr>().is_ok()
        || (!me.is_empty() && (name == me || name.strip_prefix(me.as_str()).is_some_and(|rest| rest.starts_with('.'))))
}

/// Whether a web page from this origin may read the responses (CORS): only
/// pages served from this computer.
pub fn origin_allowed(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    let name = host_name(rest.split('/').next().unwrap_or(""));
    is_localhost(&name) || name.parse::<IpAddr>().is_ok_and(is_loopback)
}

fn cors_headers(origin: Option<&str>) -> String {
    match origin {
        Some(o) => format!(
            "Access-Control-Allow-Origin: {o}\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\n\
             Access-Control-Allow-Methods: GET, POST, OPTIONS\r\nVary: Origin\r\n"
        ),
        None => "Vary: Origin\r\n".into(),
    }
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
    /// Body of a `POST`.
    pub body: &'a str,
    /// The bearer token was given (see [`can_write`]).
    pub can_write: bool,
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

    fn to_bytes(&self, head_only: bool, origin: Option<&str>) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            413 => "Content Too Large",
            422 => "Unprocessable Content",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let mut out = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\n{}{}Connection: close\r\n\r\n",
            self.status,
            reason,
            self.content_type,
            self.body.len(),
            cors_headers(origin),
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
        "POST" => return post(req, ctx),
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
            let routes: [(&str, fn(&Context, &Request, &str) -> Response); 7] = [
                ("/api/device/", |ctx, _, key| found(ctx, key, |d| Response::new(200, "application/json", device_json(ctx, &d)))),
                ("/device/", |ctx, _, key| found(ctx, key, |d| Response::new(200, "text/xml", xml(&d)))),
                ("/api/history/", history_json),
                ("/ui/card/", |ctx, _, key| match ctx.hub.find(key) {
                    Some(d) => Response::new(200, "text/html", page::card(ctx, &d)),
                    None => Response::new(404, "text/html", String::new()),
                }),
                ("/ui/history/", |ctx, req, key| {
                    let days = query(req.target, "days").and_then(|d| d.trim().parse::<u64>().ok()).unwrap_or(7).clamp(1, 365);
                    found(ctx, key, |d| Response::new(200, "text/html", page::history_view(ctx, &d, days)))
                }),
                ("/ui/details/", |ctx, _, key| found(ctx, key, |d| Response::new(200, "text/html", page::details_view(ctx, &d)))),
                ("/ui/overlay/", |ctx, _, key| found(ctx, key, |d| Response::new(200, "text/html", page::overlay_view(ctx, &d)))),
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

fn json_error(status: u16, message: &str) -> Response {
    Response::new(status, "application/json", json!({ "ok": false, "error": message }).to_string())
}

/// `POST /api/device/{id}/settings`.
fn post(req: &Request, ctx: &Context) -> Response {
    if !req.can_write {
        return json_error(
            403,
            "Changes need the API token as \"Authorization: Bearer <token>\"; set a token in Settings > Network to allow them.",
        );
    }
    let (path, _) = split_target(req.target);
    let Some(key) = path.strip_prefix("/api/device/").and_then(|rest| rest.strip_suffix("/settings")) else {
        return json_error(404, "Not found: POST /api/device/{id}/settings");
    };
    let key = percent_decode(key);
    let Some(device) = ctx.hub.find(&key) else { return json_error(404, &format!("{key} not found.")) };
    if device.source != Source::Native {
        return json_error(409, "Only the Logitech devices of this computer can be changed.");
    }
    let settings = match parse_settings(req.body) {
        Ok(s) => s,
        Err(e) => return json_error(400, &e),
    };
    let Some(control) = ctx.control.get() else { return json_error(503, "The Logitech support is turned off.") };
    let mut applied = 0;
    for setting in settings {
        if let Err(e) = control(&device.id, setting) {
            let (status, message) = action_error(e);
            return Response::new(status, "application/json", json!({ "ok": false, "error": message, "applied": applied }).to_string());
        }
        applied += 1;
    }
    Response::new(200, "application/json", json!({ "ok": true, "applied": applied }).to_string())
}

fn action_error(e: ActionError) -> (u16, &'static str) {
    match e {
        ActionError::NotSupported => (422, "Not supported by this device."),
        ActionError::Offline => (409, "The device is not connected."),
        ActionError::Busy => (409, "A pairing is in progress."),
        ActionError::Failed => (502, "The device did not accept the change."),
        ActionError::Onboard(_) => (422, "The lighting cannot be saved in this device's memory (use \"keep\")."),
        ActionError::Backup => (500, "The onboard memory could not be backed up: nothing was written."),
        ActionError::Verify => (502, "The onboard memory did not read back as written: the previous content was put back."),
        ActionError::NoBackup => (409, "No backup to restore from."),
        ActionError::GHub => (409, "Logitech G HUB is running: it manages the lighting of this device."),
    }
}

/// Reads the body of `POST /api/device/{id}/settings`: a JSON object with
/// any of these fields, applied in this order:
///
/// ```json
/// { "dpi": 1600,
///   "report_rate": 1000,
///   "backlight": "off" | "on" | "auto" | 0..n,
///   "smart_shift": "free" | "ratchet" | 1..254,
///   "lighting": { "action": "preview" | "save" | "keep" | "release" | "restore" | "forget",
///                 "zone": 0, "effect": "off" | "static" | "breathe" | "cycle",
///                 "color": "#RRGGBB", "period_ms": 4000, "intensity": 100 },
///   "host": 1..3 }
/// ```
///
/// `report_rate` is in Hz (125 to 1000); a number as `backlight` sets that
/// level; `host` (the Easy-Switch channel) comes last since the device
/// then leaves this computer.
pub fn parse_settings(body: &str) -> Result<Vec<Setting>, String> {
    const KNOWN: [&str; 6] = ["dpi", "report_rate", "backlight", "smart_shift", "lighting", "host"];
    let v: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {e}"))?;
    let Some(obj) = v.as_object() else { return Err("Expected a JSON object.".into()) };
    if let Some(unknown) = obj.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(format!("Unknown field \"{unknown}\" (known: {}).", KNOWN.join(", ")));
    }
    let mut out = Vec::new();
    if let Some(dpi) = whole(obj, "dpi", "", 50, 64_000)? {
        out.push(Setting::Dpi(dpi as u16));
    }
    if let Some(hz) = whole(obj, "report_rate", "", 125, 1000)? {
        if 1000 % hz != 0 || !(1..=8).contains(&(1000 / hz)) {
            return Err("\"report_rate\" must be 125, 250, 500 or 1000 (Hz), or 1000 divided by 1 to 8.".into());
        }
        out.push(Setting::ReportRate((1000 / hz) as u8));
    }
    if let Some(b) = obj.get("backlight") {
        let (mode, level) = match (b.as_str(), b.as_u64()) {
            (Some("off"), _) => (BacklightMode::Off, 0),
            (Some("on"), _) => (BacklightMode::On, 0),
            (Some("auto"), _) => (BacklightMode::Auto, 0),
            (_, Some(level)) if level <= 255 => (BacklightMode::Manual, level as u8),
            _ => return Err("\"backlight\" must be \"off\", \"on\", \"auto\" or a level.".into()),
        };
        out.push(Setting::Backlight { mode, level });
    }
    if let Some(w) = obj.get("smart_shift") {
        out.push(Setting::SmartShift(match (w.as_str(), w.as_u64()) {
            (Some("free"), _) => WheelMode::FreeSpin,
            (Some("ratchet"), _) => WheelMode::Ratchet,
            (_, Some(n)) if (1..=254).contains(&n) => WheelMode::Smart(n as u8),
            _ => return Err("\"smart_shift\" must be \"free\", \"ratchet\" or a speed from 1 to 254.".into()),
        }));
    }
    if let Some(l) = obj.get("lighting") {
        out.push(parse_lighting(l)?);
    }
    if let Some(host) = whole(obj, "host", "", 1, 3)? {
        out.push(Setting::Host(host as u8));
    }
    if out.is_empty() {
        return Err("Nothing to change.".into());
    }
    Ok(out)
}

/// Optional whole number field `name` within `min..=max`.
fn whole(obj: &serde_json::Map<String, Value>, name: &str, prefix: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    match obj.get(name) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .map(Some)
            .ok_or_else(|| format!("\"{prefix}{name}\" must be a whole number from {min} to {max}.")),
    }
}

fn parse_lighting(l: &Value) -> Result<Setting, String> {
    let Some(obj) = l.as_object() else { return Err("\"lighting\" must be an object.".into()) };
    let text = |name: &str| obj.get(name).and_then(Value::as_str);
    let action = text("action").unwrap_or("preview");
    match action {
        "release" => return Ok(Setting::LedRelease),
        "restore" => return Ok(Setting::LedRestore),
        "forget" => return Ok(Setting::LedForget),
        "preview" | "save" | "keep" => {}
        _ => return Err("\"lighting.action\" must be preview, save, keep, release, restore or forget.".into()),
    }
    let kind = match text("effect").unwrap_or("static") {
        "off" => EffectKind::Off,
        "static" => EffectKind::Static,
        "breathe" => EffectKind::Breathe,
        "cycle" => EffectKind::Cycle,
        _ => return Err("\"lighting.effect\" must be off, static, breathe or cycle.".into()),
    };
    let mut effect = LedEffect { kind, ..LedEffect::default() };
    if let Some(c) = text("color") {
        let hex = c.trim().trim_start_matches('#');
        effect.color = match u32::from_str_radix(hex, 16) {
            Ok(rgb) if hex.len() == 6 => rgb,
            _ => return Err("\"lighting.color\" must look like \"#FF8800\".".into()),
        };
    }
    if let Some(ms) = whole(obj, "period_ms", "lighting.", 1000, 20_000)? {
        effect.period_ms = ms as u16;
    }
    if let Some(i) = whole(obj, "intensity", "lighting.", 1, 100)? {
        effect.intensity = i as u8;
    }
    let zone = whole(obj, "zone", "lighting.", 0, 15)?.unwrap_or(0) as u8;
    Ok(match action {
        "keep" => Setting::LedKeep { zone, effect },
        mode => Setting::Led { zone, effect, save: mode == "save" },
    })
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
fn event_stream(stream: &TcpStream, ctx: &Context, local: bool, origin: Option<&str>) -> io::Result<()> {
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut out = stream;
    out.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\n\
             {}X-Accel-Buffering: no\r\nConnection: close\r\n\r\n",
            cors_headers(origin)
        )
        .as_bytes(),
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
    let num = |v: Option<f64>, digits: usize| v.filter(|v| v.is_finite()).map_or_else(|| "null".to_string(), |v| format!("{v:.digits$}"));
    let text = |v: Option<&str>| v.map_or(Value::Null, Value::from);
    let mut s = json(d);
    s.pop();
    s.push_str(&format!(
        ",\"alias\":{},\"time_left_hours\":{},\"time_to_full_hours\":{},\"health_percent\":{},\"cycles\":{},\"host\":{}}}",
        text(ctx.alias(d).as_deref()),
        num(e.time_left_hours, 2),
        num(e.time_to_full_hours, 2),
        num(e.stats.health_percent, 0),
        num((e.stats.samples > 1).then_some(e.stats.cycles), 2),
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
        ("batly_battery_cycles", "Full-charge equivalents recorded.", |_, e| (e.stats.samples > 1).then_some(e.stats.cycles)),
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

    /// Line icon of a device kind (24 × 24, drawn with `currentColor`).
    fn kind_icon(k: DeviceKind) -> &'static str {
        match k {
            DeviceKind::Mouse => r#"<rect x="6" y="2.5" width="12" height="19" rx="6"/><path d="M12 6.5v3.5"/>"#,
            DeviceKind::Keyboard => {
                r#"<rect x="2" y="6" width="20" height="12" rx="2.5"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M8 14h8"/>"#
            }
            DeviceKind::Headset => {
                r#"<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="7" rx="1.5"/><rect x="17" y="14" width="4" height="7" rx="1.5"/>"#
            }
            DeviceKind::Gamepad => {
                r#"<path d="M7 7h10a5 5 0 0 1 4.9 6l-.6 3a2.5 2.5 0 0 1-4.3 1.2L15 15H9l-2 2.2A2.5 2.5 0 0 1 2.7 16l-.6-3A5 5 0 0 1 7 7z"/><path d="M7 10v3M5.5 11.5h3M16 11h.01M18 12.5h.01"/>"#
            }
            DeviceKind::Earbuds => {
                r#"<circle cx="7" cy="8" r="3.5"/><path d="M7 11.5V20"/><circle cx="17" cy="8" r="3.5"/><path d="M17 11.5V20"/>"#
            }
            DeviceKind::Other => r#"<rect x="3" y="7" width="16" height="10" rx="2.5"/><path d="M22 10.5v3"/>"#,
        }
    }

    const STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#f4f5f7;--bg2:#eceef2;--card:#fff;--fg:#14161b;--mute:#5d6573;--line:#e0e3e8;--hover:#eceef2;--accent:#0e9f6e;--on-accent:#fff;--charge:#1d7fe0;--warn:#c98500;--bad:#d64545;--shadow:0 1px 2px rgba(20,22,27,.06),0 8px 24px rgba(20,22,27,.06);--glow:rgba(14,159,110,.18)}
@media(prefers-color-scheme:dark){:root{--bg:#13151a;--bg2:#171a20;--card:#1c1f26;--fg:#f2f4f7;--mute:#9aa3b2;--line:#2c313b;--hover:#262a33;--accent:#3ddc97;--on-accent:#072418;--charge:#5ab8ff;--warn:#f5b541;--bad:#f2645a;--shadow:0 1px 2px rgba(0,0,0,.3),0 12px 32px rgba(0,0,0,.28);--glow:rgba(61,220,151,.16)}}
*{box-sizing:border-box}
html{scrollbar-color:var(--line) transparent}
::selection{background:var(--accent);color:var(--on-accent)}
body{-webkit-user-select:none;user-select:none;-webkit-tap-highlight-color:transparent;margin:0;min-height:100vh;background:radial-gradient(1200px 500px at 15% -10%,var(--glow),transparent 60%),var(--bg);background-attachment:fixed;color:var(--fg);font:15px/1.5 "Segoe UI Variable Text","Segoe UI",system-ui,-apple-system,Roboto,sans-serif;-webkit-font-smoothing:antialiased}
.wrap{max-width:1160px;margin:0 auto;padding:0 20px}
header.top{display:flex;align-items:center;justify-content:space-between;gap:16px;padding:28px 0 8px}
.brand{display:flex;align-items:center;gap:14px}
.logo{width:44px;height:44px;border-radius:12px;display:grid;place-items:center;background:linear-gradient(145deg,var(--accent),color-mix(in srgb,var(--accent) 55%,var(--charge)));color:var(--on-accent);box-shadow:0 6px 18px var(--glow)}
.logo svg{width:24px;height:24px}
h1{font-size:22px;font-weight:700;margin:0;letter-spacing:-.01em}
.brand p{margin:0;color:var(--mute);font-size:13.5px}
#live{display:inline-flex;align-items:center;gap:8px;font-size:13px;color:var(--mute);white-space:nowrap;padding:6px 12px;border:1px solid var(--line);border-radius:999px;background:var(--card)}
#live::before{content:"";width:8px;height:8px;border-radius:50%;background:var(--accent);box-shadow:0 0 0 0 var(--accent);animation:pulse 2s infinite}
#live.off::before{background:var(--bad);animation:none}
@keyframes pulse{0%{box-shadow:0 0 0 0 color-mix(in srgb,var(--accent) 60%,transparent)}70%{box-shadow:0 0 0 8px transparent}100%{box-shadow:0 0 0 0 transparent}}
.bar-tools{display:flex;flex-wrap:wrap;align-items:center;gap:10px;padding:18px 0}
#summary{flex:1;min-width:200px;display:flex;flex-wrap:wrap;gap:8px}
.chip{display:inline-flex;align-items:center;gap:6px;padding:4px 10px;border-radius:999px;font-size:12.5px;font-weight:600;background:var(--hover);color:var(--mute);white-space:nowrap}
.chip.ok{background:color-mix(in srgb,var(--accent) 14%,transparent);color:var(--accent)}
.chip.charge{background:color-mix(in srgb,var(--charge) 14%,transparent);color:var(--charge)}
.chip.low{background:color-mix(in srgb,var(--bad) 14%,transparent);color:var(--bad)}
.field{position:relative}
input[type=search]{-webkit-user-select:text;user-select:text}
input[type=search],.dd-btn{font:inherit;font-size:14px;color:var(--fg);background:var(--card);border:1px solid var(--line);border-radius:10px;height:38px;padding:0 12px;outline:none;transition:border-color .15s,box-shadow .15s,background .15s}
input[type=search]{width:220px;padding-left:34px}
.field svg{position:absolute;left:11px;top:11px;width:16px;height:16px;color:var(--mute);pointer-events:none}
input[type=search]:hover,.dd-btn:hover{background-color:var(--hover)}
input[type=search]:focus,.dd-btn:focus-visible,.dd.open .dd-btn{border-color:var(--accent);box-shadow:0 0 0 3px var(--glow)}
.dd{position:relative;z-index:5}
.dd-btn{display:inline-flex;align-items:center;gap:10px;min-width:200px;justify-content:space-between;cursor:pointer}
.dd-btn svg{width:16px;height:16px;color:var(--mute);transition:transform .25s cubic-bezier(.2,.8,.2,1)}
.dd.open .dd-btn svg{transform:rotate(180deg);color:var(--accent)}
.dd-list{position:absolute;right:0;top:calc(100% + 8px);min-width:100%;margin:0;padding:6px;list-style:none;background:var(--card);border:1px solid var(--line);border-radius:14px;box-shadow:var(--shadow),0 16px 40px rgba(0,0,0,.25);opacity:0;transform:translateY(-8px) scale(.97);transform-origin:top right;pointer-events:none;transition:opacity .18s,transform .22s cubic-bezier(.2,.8,.2,1);outline:none}
.dd.open .dd-list{opacity:1;transform:none;pointer-events:auto}
.dd-list li{position:relative;padding:9px 14px 9px 36px;border-radius:9px;cursor:pointer;white-space:nowrap;font-size:14px;transition:background .12s,color .12s;opacity:0;transform:translateY(-4px)}
.dd.open .dd-list li{opacity:1;transform:none;transition:background .12s,color .12s,opacity .2s,transform .25s cubic-bezier(.2,.8,.2,1)}
.dd.open .dd-list li:nth-child(2){transition-delay:0s,0s,30ms,30ms}.dd.open .dd-list li:nth-child(3){transition-delay:0s,0s,60ms,60ms}.dd.open .dd-list li:nth-child(4){transition-delay:0s,0s,90ms,90ms}
.dd-list li:hover,.dd-list li.act{background:var(--hover)}
.dd-list li[aria-selected=true]{color:var(--accent);font-weight:600}
.dd-list li[aria-selected=true]::before{content:"";position:absolute;left:14px;top:50%;width:10px;height:5px;border:solid currentColor;border-width:0 0 2px 2px;transform:translateY(-75%) rotate(-45deg)}
#grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(320px,1fr));gap:16px;padding-bottom:12px}
.card{position:relative;background:var(--card);border:1px solid var(--line);border-radius:16px;padding:18px;display:flex;flex-direction:column;gap:12px;min-width:0;box-shadow:var(--shadow);transition:transform .2s cubic-bezier(.2,.8,.2,1),box-shadow .2s,border-color .2s;animation:rise .45s cubic-bezier(.2,.8,.2,1) both;animation-delay:calc(var(--i,0)*45ms)}
.card.upd{animation:none}
.card:hover{transform:translateY(-3px);border-color:color-mix(in srgb,var(--accent) 35%,var(--line));box-shadow:var(--shadow),0 0 0 4px var(--glow)}
.card[hidden]{display:none}
@keyframes rise{from{opacity:0;transform:translateY(12px)}}
.card.offline .top{opacity:.55}
.top{display:flex;align-items:center;gap:14px}
.ring{position:relative;width:64px;height:64px;flex:none;color:var(--accent)}
.warn .ring{color:var(--warn)}.low .ring{color:var(--bad)}.charging .ring{color:var(--charge)}.offline .ring,.nobat .ring{color:var(--mute)}
.ring svg{width:64px;height:64px;transform:rotate(-90deg)}
.ring circle{fill:none;stroke-width:6}
.ring .t{stroke:var(--line)}
.ring .v{stroke:currentColor;stroke-linecap:round;transition:stroke-dasharray .8s cubic-bezier(.2,.8,.2,1);animation:fill 1s cubic-bezier(.2,.8,.2,1) both;animation-delay:calc(var(--i,0)*45ms + 120ms)}
.upd .ring .v{animation:none}
@keyframes fill{from{stroke-dasharray:0 100}}
.ring .k{position:absolute;inset:0;display:grid;place-items:center;color:var(--fg)}
.ring .k svg{width:22px;height:22px;transform:none;fill:none;stroke:currentColor;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}
.charging .ring::after{content:"";position:absolute;inset:-4px;border-radius:50%;border:2px solid var(--charge);opacity:0;animation:charge 2.2s ease-out infinite}
@keyframes charge{0%{opacity:.5;transform:scale(.9)}100%{opacity:0;transform:scale(1.15)}}
.who{flex:1;min-width:0}
.name{font-weight:650;font-size:16px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.sub{color:var(--mute);font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.pct{font-size:30px;font-weight:700;line-height:1;font-variant-numeric:tabular-nums;white-space:nowrap;letter-spacing:-.02em}
.warn .pct{color:var(--warn)}.low .pct{color:var(--bad)}.charging .pct{color:var(--charge)}.offline .pct{color:var(--mute)}
.state{display:flex;align-items:center;gap:8px;font-size:14px;min-height:1.5em;color:var(--mute)}
.state b{font-weight:600;color:var(--fg)}
dl{display:grid;grid-template-columns:1fr auto;gap:6px 12px;margin:0;font-size:13px;padding:12px 0 0;border-top:1px solid var(--line)}
dl:empty{display:none}
dt{color:var(--mute)}dd{margin:0;text-align:right;font-variant-numeric:tabular-nums;font-weight:600}
.chart{color:var(--accent);background:var(--bg2);border-radius:12px;padding:10px 10px 6px}
.charging .chart{color:var(--charge)}
.chart svg{display:block;width:100%;height:80px;overflow:visible}
.chart .grid line{stroke:var(--line);stroke-width:1;vector-effect:non-scaling-stroke;stroke-dasharray:3 4}
.chart .line{fill:none;stroke:currentColor;stroke-width:2.2;stroke-linejoin:round;stroke-linecap:round;vector-effect:non-scaling-stroke;stroke-dasharray:1;stroke-dashoffset:1;animation:draw 1.2s .25s cubic-bezier(.4,0,.2,1) forwards}
.upd .chart .line{animation:none;stroke-dashoffset:0}
@keyframes draw{to{stroke-dashoffset:0}}
.chart .cap{display:flex;justify-content:space-between;color:var(--mute);font-size:11.5px;padding-top:4px}
.chart .none{color:var(--mute);font-size:13px;padding:14px 4px}
.actions{display:flex;gap:6px;flex-wrap:wrap}
.act{display:inline-flex;align-items:center;gap:6px;font:inherit;font-size:12.5px;font-weight:600;padding:6px 12px;border-radius:999px;border:1px solid var(--line);background:transparent;color:var(--fg);cursor:pointer;transition:background .15s,border-color .15s,color .15s,transform .1s}
.act svg{width:14px;height:14px}
.act:hover{background:var(--accent);border-color:var(--accent);color:var(--on-accent)}
.act:active{transform:scale(.96)}
.act:focus-visible,.x:focus-visible,.seg button:focus-visible,.btn:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.chart[data-view]{cursor:pointer;transition:background .15s}.chart[data-view]:hover{background:var(--hover)}
#modal{position:fixed;inset:0;z-index:50;display:grid;place-items:center;padding:20px}
#modal[hidden]{display:none}
.scrim{position:absolute;inset:0;background:rgba(5,7,10,.55);-webkit-backdrop-filter:blur(6px);backdrop-filter:blur(6px);opacity:0;transition:opacity .22s}
#modal.open .scrim{opacity:1}
.sheet{position:relative;width:min(720px,100%);max-height:calc(100vh - 40px);overflow:auto;background:var(--card);border:1px solid var(--line);border-radius:20px;box-shadow:0 24px 64px rgba(0,0,0,.35);padding:22px 24px 24px;opacity:0;transform:translateY(18px) scale(.97);transition:transform .3s cubic-bezier(.2,.8,.2,1),opacity .22s}
#modal.open .sheet{opacity:1;transform:none}
.mbody{transition:opacity .15s}.mbody.loading{opacity:.45}
.mh{display:flex;justify-content:space-between;gap:12px;align-items:flex-start;margin-bottom:16px}
.mh h2{margin:0;font-size:20px;letter-spacing:-.01em}.mh p{margin:2px 0 0;color:var(--mute);font-size:13.5px}
.x{flex:none;width:36px;height:36px;border-radius:10px;border:1px solid var(--line);background:transparent;color:var(--mute);cursor:pointer;display:grid;place-items:center;transition:background .15s,color .15s,transform .15s}
.x:hover{background:var(--hover);color:var(--fg);transform:rotate(90deg)}.x svg{width:18px;height:18px}
.seg{display:inline-flex;padding:3px;border:1px solid var(--line);border-radius:12px;background:var(--bg2);gap:2px;margin-bottom:12px}
.seg button{font:inherit;font-size:13px;font-weight:600;border:0;background:transparent;color:var(--mute);padding:6px 14px;border-radius:9px;cursor:pointer;transition:background .18s,color .18s}
.seg button:hover{color:var(--fg)}.seg button[aria-selected=true]{background:var(--accent);color:var(--on-accent)}
.bigchart{color:var(--accent);background:var(--bg2);border-radius:14px;padding:12px 10px 6px}
.sheet.charging .bigchart{color:var(--charge)}
.bigchart svg{width:100%;height:auto;display:block;overflow:visible}
.bigchart .grid line{stroke:var(--line);stroke-dasharray:3 4}.bigchart .grid line.v{stroke-dasharray:1 6}
.bigchart text{fill:var(--mute);font-size:11px}
.bigchart .line{fill:none;stroke:currentColor;stroke-width:2.4;stroke-linejoin:round;stroke-linecap:round;stroke-dasharray:1;stroke-dashoffset:1;animation:draw 1s .1s cubic-bezier(.4,0,.2,1) forwards}
.bigchart .dot{fill:currentColor;stroke:var(--card);stroke-width:2.5;opacity:0;animation:pop .3s 1s forwards}
@keyframes pop{to{opacity:1}}
.tiles{display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:10px;margin-top:14px}
.tile{background:var(--bg2);border:1px solid var(--line);border-radius:12px;padding:10px 12px;animation:rise .35s cubic-bezier(.2,.8,.2,1) both}
.tile:nth-child(2){animation-delay:40ms}.tile:nth-child(3){animation-delay:80ms}.tile:nth-child(4){animation-delay:120ms}.tile:nth-child(n+5){animation-delay:160ms}
.tile span{display:block;color:var(--mute);font-size:12px}.tile b{font-size:17px;font-variant-numeric:tabular-nums}
.sheet .none{color:var(--mute);padding:36px 0;text-align:center}
.facts{grid-template-columns:auto 1fr;border-top:0;padding:0;gap:0;font-size:14px}
.facts dt,.facts dd{padding:10px 0;border-bottom:1px solid var(--line)}.facts dd{text-align:right;word-break:break-all;padding-left:16px}
.raw{margin:16px 0 0;color:var(--mute);font-size:12.5px}
.ovopts{display:flex;gap:10px;flex-wrap:wrap}
.ovprev{border-radius:14px;border:1px solid var(--line);height:110px;overflow:hidden;background:repeating-conic-gradient(var(--bg2) 0 25%,var(--card) 0 50%) 0 0/16px 16px}
.ovprev iframe{width:100%;height:100%;border:0;background:transparent}
.hint{color:var(--mute);font-size:13px;margin:12px 0 8px}
.copy{display:flex;gap:8px}
.copy input{flex:1;min-width:0;font:inherit;font-size:13px;color:var(--fg);background:var(--bg2);border:1px solid var(--line);border-radius:10px;padding:0 12px;height:38px;-webkit-user-select:text;user-select:text}
.btn{font:inherit;font-weight:600;font-size:13.5px;border:0;border-radius:10px;padding:0 18px;height:38px;background:var(--accent);color:var(--on-accent);cursor:pointer;transition:filter .15s,transform .1s}
.btn:hover{filter:brightness(1.08)}.btn:active{transform:scale(.97)}
body.locked{overflow:hidden}
a{color:var(--accent);text-decoration:none;cursor:pointer}a:hover{text-decoration:underline}
a:focus-visible,.card:focus-visible{outline:2px solid var(--accent);outline-offset:2px;border-radius:8px}
#empty{color:var(--mute);padding:56px 0;text-align:center}
#nomatch{color:var(--mute);padding:24px 0;text-align:center}
footer{color:var(--mute);font-size:13px;display:flex;flex-wrap:wrap;align-items:center;gap:8px 14px;padding:24px 0 36px;border-top:1px solid var(--line);margin-top:12px}
footer code{font-size:12px;padding:2px 7px;border-radius:6px;background:var(--hover);color:var(--fg)}
footer a:hover{text-decoration:none}footer a:hover code{background:var(--accent);color:var(--on-accent)}
footer .v{margin-left:auto}
@media(max-width:560px){header.top{flex-wrap:wrap}input[type=search]{width:100%}.field{flex:1}.bar-tools{gap:8px}.dd-btn{min-width:0}}
@media(prefers-reduced-motion:reduce){*,*::before,*::after{animation:none!important;transition:none!important}.chart .line,.bigchart .line{stroke-dashoffset:0}.bigchart .dot{opacity:1}}
"#;

    const SCRIPT: &str = r#"
(function(){
var m=location.search.match(/[?&]token=([^&]*)/),tk=m?'token='+m[1]:'';
function u(p){return tk?p+(p.indexOf('?')<0?'?':'&')+tk:p}
var grid=document.getElementById('grid'),live=document.getElementById('live'),empty=document.getElementById('empty'),
 sum=document.getElementById('summary'),q=document.getElementById('q'),sort=document.getElementById('sort'),nomatch=document.getElementById('nomatch'),pend={};
var lang=document.documentElement.lang;
var dd=sort,btn=dd.querySelector('.dd-btn'),list=dd.querySelector('.dd-list'),opts=Array.prototype.slice.call(list.children),act=-1;
function pick(v,quiet){opts.forEach(function(o){o.setAttribute('aria-selected',o.dataset.v===v?'true':'false');if(o.dataset.v===v)dd.querySelector('.dd-label').textContent=o.textContent});dd.dataset.value=v;if(!quiet){try{localStorage.setItem('batly.sort',v)}catch(x){}apply()}}
function mark(i){act=(i+opts.length)%opts.length;opts.forEach(function(o,k){o.classList.toggle('act',k===act)})}
function open(){dd.classList.add('open');btn.setAttribute('aria-expanded','true');mark(opts.findIndex(function(o){return o.dataset.v===dd.dataset.value}));list.focus()}
function close(f){dd.classList.remove('open');btn.setAttribute('aria-expanded','false');opts.forEach(function(o){o.classList.remove('act')});if(f)btn.focus()}
btn.addEventListener('click',function(){dd.classList.contains('open')?close(true):open()});
btn.addEventListener('keydown',function(e){if(e.key==='ArrowDown'||e.key==='ArrowUp'){e.preventDefault();open()}});
opts.forEach(function(o,i){o.addEventListener('click',function(){pick(o.dataset.v);close(true)});o.addEventListener('mousemove',function(){mark(i)})});
list.addEventListener('keydown',function(e){if(e.key==='ArrowDown'){e.preventDefault();mark(act+1)}else if(e.key==='ArrowUp'){e.preventDefault();mark(act-1)}else if(e.key==='Enter'||e.key===' '){e.preventDefault();if(act>=0)pick(opts[act].dataset.v);close(true)}else if(e.key==='Escape'||e.key==='Tab'){close(e.key==='Escape')}});
document.addEventListener('click',function(e){if(!dd.contains(e.target))close(false)});
function find(id){for(var i=0;i<grid.children.length;i++)if(grid.children[i].dataset.id===id)return grid.children[i]}
function times(root){var t=root.querySelectorAll('time[datetime]');for(var i=0;i<t.length;i++){var d=new Date(t[i].getAttribute('datetime'));if(!isNaN(d))t[i].textContent=d.toLocaleString(lang,{dateStyle:'short',timeStyle:'short'})}}
function count(el){var p=el.querySelector('.pct');if(!p||!window.requestAnimationFrame||(window.matchMedia&&matchMedia('(prefers-reduced-motion: reduce)').matches))return;var to=parseFloat(el.dataset.level);if(isNaN(to)||el.dataset.bat!=='1')return;
 var txt=p.textContent,t0=null,from=0,d=900;function step(ts){if(t0===null)t0=ts;var k=Math.min(1,(ts-t0)/d),e=1-Math.pow(1-k,3);p.textContent=txt.replace(/[\d.,]+/,Math.round(from+(to-from)*e).toLocaleString(lang));if(k<1)requestAnimationFrame(step);else p.textContent=txt}
 requestAnimationFrame(step)}
function cards(){return Array.prototype.slice.call(grid.children)}
function summary(){var c=cards(),n=c.length,ch=0,low=null,lowName='';c.forEach(function(e){if(e.classList.contains('charging'))ch++;var l=parseFloat(e.dataset.level);if(e.dataset.bat==='1'&&!isNaN(l)&&(low===null||l<low)){low=l;lowName=e.dataset.name}});
 var s='<span class="chip">'+sum.dataset.n.replace('{n}',n)+'</span>';if(ch)s+='<span class="chip charge">'+sum.dataset.c.replace('{n}',ch)+'</span>';
 if(low!==null)s+='<span class="chip '+(low<=15?'low':low<=30?'':'ok')+'">'+sum.dataset.l.replace('{n}',Math.round(low)).replace('{d}',lowName.replace(/[<&]/g,''))+'</span>';sum.innerHTML=n?s:''}
function apply(){var term=(q.value||'').trim().toLowerCase(),key=dd.dataset.value,c=cards(),shown=0;
 c.sort(function(a,b){if(key==='level'){return (parseFloat(a.dataset.level)||999)-(parseFloat(b.dataset.level)||999)}if(key==='kind'){return a.dataset.kind.localeCompare(b.dataset.kind)||a.dataset.name.localeCompare(b.dataset.name)}if(key==='name')return a.dataset.name.localeCompare(b.dataset.name,lang);return (+a.dataset.order)-(+b.dataset.order)});
 c.forEach(function(e){grid.appendChild(e);var ok=!term||e.dataset.name.toLowerCase().indexOf(term)>=0||e.dataset.kind.toLowerCase().indexOf(term)>=0;e.hidden=!ok;if(ok)shown++});
 nomatch.hidden=!(c.length&&!shown);empty.hidden=c.length>0;summary()}
try{var saved=localStorage.getItem('batly.sort');if(saved!==null)pick(saved,true)}catch(x){}
q.addEventListener('input',apply);
cards().forEach(function(e,i){e.dataset.order=i;count(e)});
var modal=document.getElementById('modal'),mbody=modal.querySelector('.mbody'),cur=null,back=null,shut=null;
function load(kind,id,q){mbody.classList.add('loading');fetch(u('/ui/'+kind+'/'+encodeURIComponent(id)+(q||''))).then(function(r){return r.ok?r.text():''}).then(function(h){
 if(!cur||cur.id!==id)return;mbody.innerHTML=h;mbody.classList.remove('loading');times(mbody);if(kind==='overlay')ov();var x=mbody.querySelector('.x');if(x&&!q)x.focus()}).catch(function(){mbody.classList.remove('loading')})}
function openv(kind,id){cur={kind:kind,id:id};back=document.activeElement;clearTimeout(shut);mbody.innerHTML='';modal.hidden=false;document.body.classList.add('locked');
 var card=find(id);modal.querySelector('.sheet').className='sheet'+(card&&card.classList.contains('charging')?' charging':'');
 requestAnimationFrame(function(){requestAnimationFrame(function(){modal.classList.add('open')})});load(kind,id,kind==='history'?'?days=7':'')}
function closev(){if(!cur)return;cur=null;modal.classList.remove('open');document.body.classList.remove('locked');shut=setTimeout(function(){modal.hidden=true;mbody.innerHTML=''},260);if(back&&back.focus)back.focus()}
function ov(){var p=mbody.querySelector('.ovprev iframe'),i=mbody.querySelector('.copy input');if(!p||!i)return;
 var sel=function(k){var b=mbody.querySelector('[data-ov="'+k+'"] [aria-selected="true"]');return b?b.dataset.v:''};
 var q='&theme='+sel('theme')+'&size='+sel('size');p.src=u(p.dataset.base+q);i.value=location.origin+u(i.dataset.base+q)}
grid.addEventListener('click',function(e){var b=e.target.closest('[data-view]');if(!b)return;var c=b.closest('.card');if(c)openv(b.dataset.view,c.dataset.id)});
modal.addEventListener('click',function(e){var t=e.target;
 if(t.closest('[data-close]')){closev();return}
 var d=t.closest('[data-days]');if(d&&cur){mbody.querySelectorAll('[data-days]').forEach(function(x){x.setAttribute('aria-selected',x===d)});load('history',cur.id,'?days='+d.dataset.days);return}
 var o=t.closest('[data-ov] button');if(o){o.parentNode.querySelectorAll('button').forEach(function(x){x.setAttribute('aria-selected',x===o)});ov();return}
 var c=t.closest('[data-copy]');if(c){var i=mbody.querySelector('.copy input');var done=function(){var l=c.textContent;c.textContent='✓';setTimeout(function(){c.textContent=l},1400)};
  if(navigator.clipboard&&navigator.clipboard.writeText){navigator.clipboard.writeText(i.value).then(done,function(){i.select();document.execCommand('copy');done()})}else{i.select();document.execCommand('copy');done()}}});
document.addEventListener('keydown',function(e){if(e.key==='Escape'&&cur){e.preventDefault();closev()}});
var hm=location.hash.match(/^#(history|details|overlay)=(.+)$/);if(hm)openv(hm[1],decodeURIComponent(hm[2]));

function refresh(id){if(pend[id])return;pend[id]=setTimeout(function(){delete pend[id];
fetch(u('/ui/card/'+encodeURIComponent(id))).then(function(r){return r.ok?r.text():''}).then(function(h){
var old=find(id);if(!h){if(old)old.remove()}else{var t=document.createElement('template');t.innerHTML=h;var n=t.content.firstElementChild;times(n);
 if(old){n.classList.add('upd');n.dataset.order=old.dataset.order;var ov=old.querySelector('.ring .v'),nv=n.querySelector('.ring .v');old.replaceWith(n);
  if(ov&&nv){var to=nv.getAttribute('stroke-dasharray');nv.setAttribute('stroke-dasharray',ov.getAttribute('stroke-dasharray'));nv.getBoundingClientRect();nv.setAttribute('stroke-dasharray',to)}}
 else{n.dataset.order=grid.children.length;n.style.setProperty('--i',0);grid.appendChild(n);count(n)}}
apply()}).catch(function(){})},250)}
times(document);apply();
if(!window.EventSource)return;
var es=new EventSource(u('/api/events'));
es.onopen=function(){live.className='';live.textContent=live.dataset.on};
es.onerror=function(){live.className='off';live.textContent=live.dataset.off};
es.addEventListener('device',function(e){try{refresh(JSON.parse(e.data).id)}catch(x){}});
})();
"#;

    pub fn index(ctx: &Context) -> String {
        let devices = ctx.devices(false);
        let title = tr([
            "Battery levels",
            "Niveaux de batterie",
            "Akkustände",
            "Niveles de batería",
            "Livelli della batteria",
            "Níveis de bateria",
        ]);
        let mut s = format!(
            "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\">\
             <title>Batly – {title}</title><style>{STYLE}</style></head><body>\
             <div class=\"wrap\"><header class=\"top\"><div class=\"brand\"><div class=\"logo\" aria-hidden=\"true\">\
             <svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
             <rect x=\"2\" y=\"7\" width=\"17\" height=\"10\" rx=\"2.5\"/><path d=\"M22 10.5v3\"/><path d=\"M11.5 9 9 12h3l-2.5 3\"/></svg></div>\
             <div><h1>Batly</h1><p>{title}</p></div></div>\
             <span id=\"live\" class=\"off\" data-on=\"{on}\" data-off=\"{off}\">{off}</span></header>\
             <div class=\"bar-tools\"><div id=\"summary\" data-n=\"{sum_n}\" data-c=\"{sum_c}\" data-l=\"{sum_l}\"></div>\
             <label class=\"field\"><svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\">\
             <circle cx=\"11\" cy=\"11\" r=\"7\"/><path d=\"m20 20-3.5-3.5\"/></svg>\
             <input id=\"q\" type=\"search\" placeholder=\"{search}\" aria-label=\"{search}\"></label>\
             <div class=\"dd\" id=\"sort\" data-value=\"\"><button type=\"button\" class=\"dd-btn\" aria-haspopup=\"listbox\" aria-expanded=\"false\" aria-label=\"{sort}\">\
             <span class=\"dd-label\">{sort_default}</span><svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"m6 9 6 6 6-6\"/></svg></button>\
             <ul class=\"dd-list\" role=\"listbox\" tabindex=\"-1\"><li role=\"option\" data-v=\"\" aria-selected=\"true\">{sort_default}</li>\
             <li role=\"option\" data-v=\"level\">{sort_level}</li><li role=\"option\" data-v=\"name\">{sort_name}</li><li role=\"option\" data-v=\"kind\">{sort_kind}</li></ul></div></div>\
             <main><div id=\"grid\">",
            lang = i18n::lang().code(),
            on = tr(["Live", "En direct", "Live", "En directo", "In diretta", "Ao vivo"]),
            off = tr(["Reconnecting…", "Reconnexion…", "Verbinde…", "Reconectando…", "Riconnessione…", "Reconectando…"]),
            sum_n = tr(["{n} devices", "{n} appareils", "{n} Geräte", "{n} dispositivos", "{n} dispositivi", "{n} dispositivos"]),
            sum_c = tr(["{n} charging", "{n} en charge", "{n} lädt", "{n} cargando", "{n} in carica", "{n} carregando"]),
            sum_l = tr([
                "Lowest: {d} {n} %",
                "Plus bas : {d} {n} %",
                "Niedrigster: {d} {n} %",
                "Más bajo: {d} {n} %",
                "Più basso: {d} {n} %",
                "Mais baixo: {d} {n} %",
            ]),
            search = tr(["Search", "Rechercher", "Suchen", "Buscar", "Cerca", "Pesquisar"]),
            sort = tr(["Sort", "Trier", "Sortieren", "Ordenar", "Ordina", "Ordenar"]),
            sort_default = tr(["Usual order", "Ordre habituel", "Übliche Reihenfolge", "Orden habitual", "Ordine abituale", "Ordem habitual"]),
            sort_level = tr(["Lowest level first", "Niveau le plus bas d'abord", "Niedrigster Stand zuerst", "Nivel más bajo primero", "Livello più basso prima", "Nível mais baixo primeiro"]),
            sort_name = tr(["By name", "Par nom", "Nach Name", "Por nombre", "Per nome", "Por nome"]),
            sort_kind = tr(["By type", "Par type", "Nach Typ", "Por tipo", "Per tipo", "Por tipo"]),
        );
        for (i, d) in devices.iter().enumerate() {
            s.push_str(&card(ctx, d).replacen("<article ", &format!("<article style=\"--i:{}\" ", i.min(12)), 1));
        }
        s.push_str(&format!(
            "</div><p id=\"empty\"{}>{}</p><p id=\"nomatch\" hidden>{}</p></main><footer><span>API</span>\
             <a href=\"/api/devices\"><code>/api/devices</code></a><a href=\"/api/events\"><code>/api/events</code></a>\
             <a href=\"/metrics\"><code>/metrics</code></a><a href=\"/overlay\"><code>/overlay</code></a>\
             <a href=\"/devices\"><code>/device/{{id}}</code></a><span class=\"v\">Batly {VERSION}</span></footer></div>\
             <div id=\"modal\" hidden><div class=\"scrim\" data-close></div><section class=\"sheet\" role=\"dialog\" aria-modal=\"true\"><div class=\"mbody\"></div></section></div>\
             <script>{SCRIPT}</script></body></html>",
            if devices.is_empty() { "" } else { " hidden" },
            i18n::no_device(),
            tr(["No match.", "Aucun résultat.", "Kein Treffer.", "Sin resultados.", "Nessun risultato.", "Nenhum resultado."]),
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
        if !d.has_battery {
            class.push_str(" nobat");
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
            row(
                tr([
                    "Runtime per charge",
                    "Autonomie par charge",
                    "Laufzeit pro Ladung",
                    "Autonomía por carga",
                    "Autonomia per carica",
                    "Autonomia por carga",
                ]),
                i18n::duration(h),
            );
        }
        if let Some(h) = st.avg_charge_hours {
            row(
                tr([
                    "Usual charge time",
                    "Durée de charge habituelle",
                    "Übliche Ladedauer",
                    "Tiempo de carga habitual",
                    "Durata di carica abituale",
                    "Tempo de carga habitual",
                ]),
                i18n::duration(h),
            );
        }
        if let Some(h) = st.health_percent {
            row(
                tr([
                    "Battery health",
                    "Santé de la batterie",
                    "Akkuzustand",
                    "Salud de la batería",
                    "Salute della batteria",
                    "Saúde da bateria",
                ]),
                i18n::percent(h as f32),
            );
        }
        if st.samples > 1 && st.cycles >= 0.1 {
            row(
                tr(["Charge cycles", "Cycles de charge", "Ladezyklen", "Ciclos de carga", "Cicli di carica", "Ciclos de carga"]),
                format!("{:.1}", st.cycles),
            );
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
                    "<div class=\"chart\" data-view=\"history\" title=\"{}\">{svg}<div class=\"cap\"><span>{}</span><span>100\u{a0}%</span></div></div>",
                    tr(["Open the history", "Ouvrir l'historique", "Verlauf öffnen", "Abrir el historial", "Apri la cronologia", "Abrir o histórico"]),
                    tr(["Last 7 days", "7 derniers jours", "Letzte 7 Tage", "Últimos 7 días", "Ultimi 7 giorni", "Últimos 7 dias"])
                ),
                None => format!(
                    "<div class=\"chart\"><div class=\"none\">{}</div></div>",
                    tr([
                        "Not enough readings yet for a chart.",
                        "Pas encore assez de relevés pour un graphique.",
                        "Noch nicht genug Messwerte für ein Diagramm.",
                        "Aún no hay lecturas suficientes para un gráfico.",
                        "Letture ancora insufficienti per un grafico.",
                        "Ainda não há leituras suficientes para um gráfico."
                    ])
                ),
            }
        } else {
            String::new()
        };

        let now = level.filter(|_| d.has_battery).unwrap_or(0.0).clamp(0.0, 100.0);
        format!(
            "<article class=\"{class}\" data-id=\"{id}\" data-name=\"{name}\" data-kind=\"{kind}\" data-level=\"{data_level}\" data-bat=\"{bat}\">\
             <div class=\"top\"><div class=\"ring\" role=\"progressbar\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-valuenow=\"{now:.0}\">\
             <svg viewBox=\"0 0 64 64\"><circle class=\"t\" cx=\"32\" cy=\"32\" r=\"28\" pathLength=\"100\"/>\
             <circle class=\"v\" cx=\"32\" cy=\"32\" r=\"28\" pathLength=\"100\" stroke-dasharray=\"{now:.1} 100\"/></svg>\
             <div class=\"k\"><svg viewBox=\"0 0 24 24\">{icon}</svg></div></div>\
             <div class=\"who\"><div class=\"name\" title=\"{name}\">{name}</div><div class=\"sub\">{sub}</div></div><div class=\"pct\">{pct}</div></div>\
             <div class=\"state\">{state}</div><dl>{rows}</dl>{chart_html}\
             <div class=\"actions\"><button type=\"button\" class=\"act\" data-view=\"history\">\
             <svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M3 3v18h18\"/><path d=\"m7 15 4-4 3 3 5-6\"/></svg>{hist}</button>\
             <button type=\"button\" class=\"act\" data-view=\"details\"><svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\"><circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M12 11v6M12 7.5h.01\"/></svg>{details}</button>\
             <button type=\"button\" class=\"act\" data-view=\"overlay\"><svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><rect x=\"3\" y=\"4\" width=\"18\" height=\"13\" rx=\"2\"/><path d=\"M8 21h8M12 17v4\"/></svg>Overlay</button></div></article>",
            id = escape(&d.id),
            name = escape(&name),
            kind = escape(kind_name(d.kind)),
            data_level = level.filter(|_| d.has_battery).map(|p| format!("{p:.0}")).unwrap_or_default(),
            bat = if d.has_battery { 1 } else { 0 },
            icon = kind_icon(d.kind),
            sub = escape(&sub),
            pct = escape(&pct),
            state = escape(&state),
            hist = tr(["History", "Historique", "Verlauf", "Historial", "Cronologia", "Histórico"]),
            details = tr(["Details", "Détails", "Details", "Detalles", "Dettagli", "Detalhes"]),
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
        static GRADIENTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let g = GRADIENTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut svg = format!(
            "<svg viewBox=\"0 0 {w} {h}\" preserveAspectRatio=\"none\" aria-hidden=\"true\">\
             <defs><linearGradient id=\"fade{g}\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\"><stop offset=\"0\" style=\"stop-color:currentColor;stop-opacity:.34\"/>\
             <stop offset=\"1\" style=\"stop-color:currentColor;stop-opacity:0\"/></linearGradient></defs><g class=\"grid\">\
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
                "<polygon class=\"area\" fill=\"url(#fade{g})\" points=\"{first:.1},{h} {} {last:.1},{h}\"/><polyline class=\"line\" pathLength=\"1\" points=\"{}\"/>",
                pts.join(" "),
                pts.join(" ")
            ));
        }
        Some(svg + "</svg>")
    }

    /// Large chart of the level for the history view: grid with 0–100 %
    /// labels and day marks (SVG keeping its proportions).
    pub fn big_chart(samples: &[Sample], now: u64, days: u64, live: Option<f32>) -> Option<String> {
        if samples.len() < 2 {
            return None;
        }
        let (w, h, left, right, top, bottom) = (640.0, 240.0, 40.0, 630.0, 10.0, 212.0);
        let start = now.saturating_sub(days * 86_400);
        let x = |t: u64| left + (t.saturating_sub(start) as f64 / (now - start).max(1) as f64 * (right - left)).clamp(0.0, right - left);
        let y = |p: f32| top + (100.0 - p.clamp(0.0, 100.0) as f64) / 100.0 * (bottom - top);
        static GRADIENTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let g = GRADIENTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut svg = format!(
            "<svg class=\"big\" viewBox=\"0 0 {w} {h}\" role=\"img\"><defs><linearGradient id=\"bfade{g}\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\
             <stop offset=\"0\" style=\"stop-color:currentColor;stop-opacity:.32\"/><stop offset=\"1\" style=\"stop-color:currentColor;stop-opacity:0\"/>\
             </linearGradient></defs><g class=\"grid\">"
        );
        for level in [0, 25, 50, 75, 100] {
            let gy = y(level as f32);
            svg.push_str(&format!(
                "<line x1=\"{left}\" x2=\"{right}\" y1=\"{gy:.1}\" y2=\"{gy:.1}\"/><text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{level}\u{a0}%</text>",
                left - 6.0,
                gy + 4.0
            ));
        }
        // Day marks: every day over a week, every 5 days beyond.
        let step = if days <= 7 {
            1
        } else if days <= 31 {
            5
        } else {
            30
        };
        let today = now - now % 86_400;
        for k in (0..=days).step_by(step as usize) {
            let t = today.saturating_sub(k * 86_400);
            if t < start {
                break;
            }
            let date = iso8601(UNIX_EPOCH + Duration::from_secs(t));
            let label = format!("{}/{}", &date[8..10], &date[5..7]);
            let gx = x(t);
            svg.push_str(&format!(
                "<line class=\"v\" x1=\"{gx:.1}\" x2=\"{gx:.1}\" y1=\"{top}\" y2=\"{bottom}\"/><text x=\"{gx:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{label}</text>",
                bottom + 20.0
            ));
        }
        svg.push_str("</g>");
        let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut prev: Option<u64> = None;
        for s in samples {
            if prev.is_none_or(|p| s.t.saturating_sub(p) > 3 * 3600) {
                runs.push(Vec::new());
            }
            if let Some(run) = runs.last_mut() {
                run.push((x(s.t), y(s.percent)));
            }
            prev = Some(s.t);
        }
        if let (Some(p), Some(last), Some(run)) = (live, samples.last(), runs.last_mut()) {
            if now.saturating_sub(last.t) <= 3 * 3600 {
                run.push((right, y(p)));
            }
        }
        for run in runs.iter().filter(|r| !r.is_empty()) {
            let pts: Vec<String> = run.iter().map(|(px, py)| format!("{px:.1},{py:.1}")).collect();
            let (first, last) = (run[0].0, run[run.len() - 1].0);
            svg.push_str(&format!(
                "<polygon class=\"area\" fill=\"url(#bfade{g})\" points=\"{first:.1},{bottom} {} {last:.1},{bottom}\"/>\
                 <polyline class=\"line\" pathLength=\"1\" points=\"{}\"/>",
                pts.join(" "),
                pts.join(" ")
            ));
        }
        if let Some(&(lx, ly)) = runs.iter().rev().find_map(|r| r.last()) {
            svg.push_str(&format!("<circle class=\"dot\" cx=\"{lx:.1}\" cy=\"{ly:.1}\" r=\"4.5\"/>"));
        }
        Some(svg + "</svg>")
    }

    fn view_head(title: &str, sub: &str) -> String {
        format!(
            "<header class=\"mh\"><div><h2>{}</h2><p>{}</p></div><button class=\"x\" type=\"button\" data-close aria-label=\"{}\">\
             <svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\"><path d=\"M6 6l12 12M18 6 6 18\"/></svg></button></header>",
            escape(title),
            escape(sub),
            tr(["Close", "Fermer", "Schließen", "Cerrar", "Chiudi", "Fechar"])
        )
    }

    /// History view (`/ui/history/{id}?days=N`): large chart, period
    /// switch and statistics.
    pub fn history_view(ctx: &Context, d: &Device, days: u64) -> String {
        let name = ctx.name(d);
        let now = history::now();
        let samples = ctx.history.samples(&d.id, now.saturating_sub(days * 86_400));
        let live = d.online.then(|| d.battery.map(|b| b.percent)).flatten();
        let st = ctx.history.stats(&d.id);
        let mut s = view_head(
            &name,
            tr([
                "Battery history",
                "Historique de la batterie",
                "Akkuverlauf",
                "Historial de la batería",
                "Cronologia della batteria",
                "Histórico da bateria",
            ]),
        );
        s.push_str("<div class=\"seg\" role=\"tablist\">");
        for (n, label) in [
            (1, tr(["24 h", "24 h", "24 Std.", "24 h", "24 h", "24 h"])),
            (7, tr(["7 days", "7 jours", "7 Tage", "7 días", "7 giorni", "7 dias"])),
            (30, tr(["30 days", "30 jours", "30 Tage", "30 días", "30 giorni", "30 dias"])),
        ] {
            s.push_str(&format!("<button type=\"button\" role=\"tab\" data-days=\"{n}\" aria-selected=\"{}\">{label}</button>", n == days));
        }
        s.push_str("</div>");
        match big_chart(&samples, now, days, live) {
            Some(svg) => s.push_str(&format!("<div class=\"bigchart\">{svg}</div>")),
            None => s.push_str(&format!(
                "<p class=\"none\">{}</p>",
                tr([
                    "Not enough readings yet for this period.",
                    "Pas encore assez de relevés sur cette période.",
                    "Noch nicht genug Messwerte für diesen Zeitraum.",
                    "Aún no hay lecturas suficientes para este periodo.",
                    "Letture ancora insufficienti per questo periodo.",
                    "Ainda não há leituras suficientes para este período."
                ])
            )),
        }
        let mut tiles = Vec::new();
        let mut tile = |label: &str, value: String| {
            tiles.push(format!("<div class=\"tile\"><span>{}</span><b>{}</b></div>", escape(label), escape(&value)))
        };
        if let Some(h) = st.time_left_hours.filter(|_| d.has_battery) {
            tile(
                tr(["Time left", "Autonomie restante", "Restlaufzeit", "Autonomía restante", "Autonomia residua", "Autonomia restante"]),
                i18n::duration(h),
            );
        }
        if let Some(h) = st.full_runtime_hours {
            tile(
                tr([
                    "Runtime per charge",
                    "Autonomie par charge",
                    "Laufzeit pro Ladung",
                    "Autonomía por carga",
                    "Autonomia per carica",
                    "Autonomia por carga",
                ]),
                i18n::duration(h),
            );
        }
        if let Some(h) = st.avg_charge_hours {
            tile(
                tr([
                    "Usual charge time",
                    "Durée de charge habituelle",
                    "Übliche Ladedauer",
                    "Tiempo de carga habitual",
                    "Durata di carica abituale",
                    "Tempo de carga habitual",
                ]),
                i18n::duration(h),
            );
        }
        if let Some(r) = st.drain_per_hour {
            tile(
                tr([
                    "Use per hour",
                    "Consommation par heure",
                    "Verbrauch pro Stunde",
                    "Consumo por hora",
                    "Consumo orario",
                    "Consumo por hora",
                ]),
                format!("{r:.1}\u{a0}%"),
            );
        }
        if let Some(hp) = st.health_percent {
            tile(
                tr([
                    "Battery health",
                    "Santé de la batterie",
                    "Akkuzustand",
                    "Salud de la batería",
                    "Salute della batteria",
                    "Saúde da bateria",
                ]),
                i18n::percent(hp as f32),
            );
        }
        if st.samples > 1 {
            tile(
                tr(["Charge cycles", "Cycles de charge", "Ladezyklen", "Ciclos de carga", "Cicli di carica", "Ciclos de carga"]),
                format!("{:.1}", st.cycles),
            );
        }
        tile(tr(["Readings", "Relevés", "Messwerte", "Lecturas", "Letture", "Leituras"]), samples.len().to_string());
        drop(tile);
        s.push_str(&format!("<div class=\"tiles\">{}</div>", tiles.join("")));
        s
    }

    /// Details view (`/ui/details/{id}`): everything known about a device.
    pub fn details_view(ctx: &Context, d: &Device) -> String {
        let name = ctx.name(d);
        let mut s = view_head(&name, kind_name(d.kind));
        let mut rows = String::new();
        let mut row = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                rows.push_str(&format!("<dt>{}</dt><dd>{}</dd>", escape(k), escape(&v)));
            }
        };
        let b = d.battery;
        row(tr(["Level", "Niveau", "Stand", "Nivel", "Livello", "Nível"]), b.filter(|_| d.has_battery).map(|b| i18n::percent(b.percent)));
        row(
            tr(["State", "État", "Zustand", "Estado", "Stato", "Estado"]),
            Some(capitalize(if !d.online {
                i18n::offline()
            } else {
                match b.map(|b| b.status) {
                    Some(Status::Charging) => i18n::charging(),
                    Some(Status::Full) => i18n::full(),
                    _ if !d.has_battery => i18n::no_battery(),
                    _ => tr(["On battery", "Sur batterie", "Im Akkubetrieb", "Con batería", "A batteria", "Na bateria"]),
                }
            })),
        );
        row(
            tr(["Voltage", "Tension", "Spannung", "Voltaje", "Tensione", "Tensão"]),
            b.and_then(|b| b.millivolts).map(|mv| format!("{:.2}\u{a0}V", mv as f32 / 1000.0)),
        );
        if name != d.name {
            row(
                tr(["Reported name", "Nom annoncé", "Gemeldeter Name", "Nombre informado", "Nome riportato", "Nome informado"]),
                Some(d.name.clone()),
            );
        }
        row(tr(["Model", "Modèle", "Modell", "Modelo", "Modello", "Modelo"]), d.info.model.clone());
        row(tr(["Firmware", "Micrologiciel", "Firmware", "Firmware", "Firmware", "Firmware"]), d.info.firmware.clone());
        row(
            tr(["Serial number", "N° de série", "Seriennummer", "Número de serie", "Numero di serie", "Número de série"]),
            d.info.serial.clone(),
        );
        row(tr(["Receiver", "Récepteur", "Empfänger", "Receptor", "Ricevitore", "Receptor"]), d.info.receiver.clone());
        row(tr(["Computer", "Ordinateur", "Computer", "Equipo", "Computer", "Computador"]), d.info.host.clone());
        row(
            tr([
                "Easy-Switch channel",
                "Canal Easy-Switch",
                "Easy-Switch-Kanal",
                "Canal Easy-Switch",
                "Canale Easy-Switch",
                "Canal Easy-Switch",
            ]),
            d.info.host_channel.map(|(c, n)| format!("{c} / {n}")),
        );
        row(tr(["Signal", "Signal", "Signal", "Señal", "Segnale", "Sinal"]), d.info.signal.map(|v| format!("{v}\u{a0}%")));
        row(
            tr(["Source", "Source", "Quelle", "Fuente", "Origine", "Fonte"]),
            Some(
                match d.source {
                    Source::Native => "Logitech (HID++)",
                    Source::GHub => "Logitech G HUB",
                    other => other.as_str(),
                }
                .to_string(),
            ),
        );
        row(tr(["Identifier", "Identifiant", "Kennung", "Identificador", "Identificativo", "Identificador"]), Some(d.id.clone()));
        drop(row);
        if let Some(t) = d.last_update {
            let iso = iso8601(t);
            rows.push_str(&format!(
                "<dt>{}</dt><dd><time datetime=\"{iso}\">{iso}</time></dd>",
                tr(["Last reading", "Dernier relevé", "Letzte Messung", "Última lectura", "Ultima lettura", "Última leitura"])
            ));
        }
        let id_url = percent_encode(&d.id);
        s.push_str(&format!(
            "<dl class=\"facts\">{rows}</dl><p class=\"raw\">{} : <a href=\"/api/device/{id_url}\" target=\"_blank\">JSON</a> · \
             <a href=\"/device/{id_url}\" target=\"_blank\">XML</a> · <a href=\"/api/history/{id_url}?days=30\" target=\"_blank\">{}</a></p>",
            tr(["Raw data, for developers", "Données brutes, pour les développeurs", "Rohdaten für Entwickler", "Datos sin procesar, para desarrolladores", "Dati grezzi, per sviluppatori", "Dados brutos, para desenvolvedores"]),
            tr(["history (JSON)", "historique (JSON)", "Verlauf (JSON)", "historial (JSON)", "cronologia (JSON)", "histórico (JSON)"]),
        ));
        s
    }

    /// Overlay view (`/ui/overlay/{id}`): live preview for OBS and the
    /// link to copy (made absolute by the page script).
    pub fn overlay_view(ctx: &Context, d: &Device) -> String {
        let name = ctx.name(d);
        let id_url = percent_encode(&d.id);
        let mut s = view_head(
            &name,
            tr([
                "Overlay for OBS / streaming",
                "Overlay pour OBS / le streaming",
                "Overlay für OBS / Streaming",
                "Overlay para OBS / streaming",
                "Overlay per OBS / streaming",
                "Overlay para OBS / streaming",
            ]),
        );
        s.push_str(&format!(
            "<div class=\"ovopts\"><div class=\"seg\" data-ov=\"theme\"><button type=\"button\" data-v=\"dark\" aria-selected=\"true\">{}</button>\
             <button type=\"button\" data-v=\"light\" aria-selected=\"false\">{}</button></div>\
             <div class=\"seg\" data-ov=\"size\"><button type=\"button\" data-v=\"24\" aria-selected=\"false\">S</button>\
             <button type=\"button\" data-v=\"32\" aria-selected=\"true\">M</button><button type=\"button\" data-v=\"48\" aria-selected=\"false\">L</button></div></div>\
             <div class=\"ovprev\"><iframe title=\"Overlay\" data-base=\"/overlay?device={id_url}\" src=\"/overlay?device={id_url}&amp;theme=dark&amp;size=32\"></iframe></div>\
             <p class=\"hint\">{}</p><div class=\"copy\"><input type=\"text\" readonly data-base=\"/overlay?device={id_url}\"><button type=\"button\" class=\"btn\" data-copy>{}</button></div>",
            tr(["Dark", "Sombre", "Dunkel", "Oscuro", "Scuro", "Escuro"]),
            tr(["Light", "Clair", "Hell", "Claro", "Chiaro", "Claro"]),
            tr([
                "In OBS: add a \"Browser\" source with this link (transparent background).",
                "Dans OBS : ajoutez une source « Navigateur » avec ce lien (fond transparent).",
                "In OBS: eine „Browser“-Quelle mit diesem Link hinzufügen (transparenter Hintergrund).",
                "En OBS: añade una fuente «Navegador» con este enlace (fondo transparente).",
                "In OBS: aggiungi una sorgente «Browser» con questo link (sfondo trasparente).",
                "No OBS: adicione uma fonte «Navegador» com este link (fundo transparente)."
            ]),
            tr(["Copy", "Copier", "Kopieren", "Copiar", "Copia", "Copiar"]),
        ));
        s
    }

    /// Transparent page for OBS: `?device=<id or name>&size=32&theme=dark|light&layout=row|column&names=0`.
    pub fn overlay(target: &str) -> String {
        let size = query(target, "size").and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(32).clamp(8, 400);
        let theme = if query(target, "theme").as_deref() == Some("light") { "light" } else { "dark" };
        let layout = if query(target, "layout").as_deref() == Some("column") { "column" } else { "row" };
        let device = Value::from(query(target, "device").unwrap_or_default()).to_string().replace('<', "\\u003c");
        let names = !matches!(query(target, "names").as_deref(), Some("0" | "false" | "no"));
        format!(
            r#"<!doctype html><html><head><meta charset="utf-8"><meta name="color-scheme" content="light dark"><title>Batly overlay</title><style>
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
        respond(&Request { method: "GET", target, authorized: true, body: "", can_write: false }, ctx)
    }

    #[test]
    fn routes() {
        let ctx = ctx_of(hub());
        let r = get(&ctx, "/");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("G &lt;Pro&gt; &amp; Co") && r.body.contains("data-view=\"details\""));
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
        assert_eq!(respond(&Request { method: "PUT", target: "/", authorized: true, body: "", can_write: false }, &ctx).status, 405);
        assert_eq!(
            respond(&Request { method: "OPTIONS", target: "/api/devices", authorized: false, body: "", can_write: false }, &ctx).status,
            204
        );

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
        hub.publish(Event::Init {
            id: "peer:pc:X".into(),
            name: "K".into(),
            kind: DeviceKind::Keyboard,
            source: Source::Remote,
            has_battery: true,
        });
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

    fn post_t(ctx: &Context, target: &str, body: &str, can_write: bool) -> Response {
        respond(&Request { method: "POST", target, authorized: true, body, can_write }, ctx)
    }

    #[test]
    fn settings_body() {
        let s = parse_settings(r##"{"host": 2, "dpi": 1600, "report_rate": 500, "backlight": 3, "smart_shift": "free"}"##).unwrap();
        assert_eq!(
            s,
            vec![
                Setting::Dpi(1600),
                Setting::ReportRate(2),
                Setting::Backlight { mode: BacklightMode::Manual, level: 3 },
                Setting::SmartShift(WheelMode::FreeSpin),
                // Easy-Switch always last.
                Setting::Host(2),
            ]
        );
        assert_eq!(parse_settings(r#"{"backlight": "auto", "smart_shift": 20}"#).unwrap()[1], Setting::SmartShift(WheelMode::Smart(20)));
        let red = LedEffect { kind: EffectKind::Breathe, color: 0xFF0000, period_ms: 3000, intensity: 50 };
        assert_eq!(
            parse_settings(
                r##"{"lighting": {"action": "save", "effect": "breathe", "color": "#ff0000", "period_ms": 3000, "intensity": 50}}"##
            ),
            Ok(vec![Setting::Led { zone: 0, effect: red, save: true }])
        );
        assert_eq!(
            parse_settings(r##"{"lighting": {"color": "00FF00"}}"##),
            Ok(vec![Setting::Led { zone: 0, effect: LedEffect { color: 0x00FF00, ..LedEffect::default() }, save: false }])
        );
        assert!(matches!(parse_settings(r#"{"lighting": {"action": "keep", "zone": 1}}"#).unwrap()[0], Setting::LedKeep { zone: 1, .. }));
        assert_eq!(parse_settings(r#"{"lighting": {"action": "release"}}"#), Ok(vec![Setting::LedRelease]));
        for bad in [
            "",
            "[]",
            "{}",
            r#"{"dpi": "1600"}"#,
            r#"{"dpi": 10}"#,
            r#"{"report_rate": 300}"#,
            r#"{"backlight": "max"}"#,
            r#"{"smart_shift": 0}"#,
            r#"{"host": 4}"#,
            r#"{"lighting": {"effect": "rainbow"}}"#,
            r##"{"lighting": {"color": "#F00"}}"##,
            r#"{"lighting": {"action": "flash"}}"#,
            r#"{"lighting": {"period_ms": 50}}"#,
            r#"{"delete": true}"#,
        ] {
            assert!(parse_settings(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn changes_need_the_bearer_token() {
        let ctx = ctx_of(hub());
        let target = "/api/device/ABC123/settings";
        // No token: read-only, whatever the address.
        let r = post_t(&ctx, target, r#"{"dpi": 800}"#, false);
        assert_eq!(r.status, 403);
        assert!(r.body.contains("Bearer"));
        // Token given but the Logitech support is off.
        assert_eq!(post_t(&ctx, target, r#"{"dpi": 800}"#, true).status, 503);

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let _ = ctx.control.set(Box::new(move |id, setting| {
            log.lock().unwrap().push((id.to_string(), setting));
            match setting {
                Setting::Host(_) => Err(ActionError::NotSupported),
                _ => Ok(()),
            }
        }));
        let r = post_t(&ctx, "/api/device/G%20%3CPro%3E%20%26%20Co/settings", r#"{"dpi": 800, "report_rate": 1000}"#, true);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(serde_json::from_str::<Value>(&r.body).unwrap()["applied"], 2);
        assert_eq!(seen.lock().unwrap()[..], [("ABC123".to_string(), Setting::Dpi(800)), ("ABC123".to_string(), Setting::ReportRate(1))]);

        let r = post_t(&ctx, target, r#"{"dpi": 900, "host": 2}"#, true);
        assert_eq!(r.status, 422);
        assert_eq!(serde_json::from_str::<Value>(&r.body).unwrap()["applied"], 1);
        assert_eq!(post_t(&ctx, target, "{oops", true).status, 400);
        assert_eq!(post_t(&ctx, "/api/device/nope/settings", r#"{"dpi": 800}"#, true).status, 404);
        assert_eq!(post_t(&ctx, "/api/device/ABC123", r#"{"dpi": 800}"#, true).status, 404);

        // Devices of another computer or another brand cannot be changed.
        ctx.hub.publish(Event::Init {
            id: "R1".into(),
            name: "Far".into(),
            kind: DeviceKind::Mouse,
            source: Source::Remote,
            has_battery: true,
        });
        assert_eq!(post_t(&ctx, "/api/device/R1/settings", r#"{"dpi": 800}"#, true).status, 409);

        assert!(can_write("s3cret", Some("Bearer s3cret")));
        assert!(!can_write("s3cret", Some("Bearer nope")));
        assert!(!can_write("", Some("Bearer ")));
        assert!(!can_write("s3cret", None));
    }

    fn authorized_t(token: &str, loopback: bool, target: &str, authorization: Option<&str>) -> bool {
        authorized(token, loopback, true, target, authorization)
    }

    #[test]
    fn dns_rebinding_and_cors() {
        // Host must name this computer unless the token is given.
        for ok in [
            "localhost:12321",
            "LOCALHOST",
            "127.0.0.1:12321",
            "[::1]:12321",
            "192.168.1.2",
            "pc-salon",
            "PC-Salon.local:12321",
            "app.localhost",
        ] {
            assert!(host_allowed(Some(ok), "PC-SALON"), "{ok}");
        }
        for bad in ["evil.example:12321", "pc-salon-evil.com", "localhost.evil.com", "x"] {
            assert!(!host_allowed(Some(bad), "PC-SALON"), "{bad}");
        }
        assert!(host_allowed(None, "") && !host_allowed(Some("pc"), ""));
        assert!(!authorized("", true, false, "/", None));
        assert!(!authorized("s3cret", true, false, "/", None));
        assert!(authorized("s3cret", true, false, "/?token=s3cret", None));
        assert!(authorized("s3cret", false, false, "/", Some("Bearer s3cret")));
        // CORS: pages of this computer only.
        for ok in ["http://localhost:12321", "http://127.0.0.1:8080", "https://[::1]", "http://obs.localhost"] {
            assert!(origin_allowed(ok), "{ok}");
        }
        for bad in ["https://evil.example", "null", "http://192.168.1.2:12321", "file://", "http://localhost.evil.com"] {
            assert!(!origin_allowed(bad), "{bad}");
        }
        let with = String::from_utf8(Response::new(200, "text/plain", "x".into()).to_bytes(false, Some("http://localhost:3000"))).unwrap();
        assert!(with.contains("Access-Control-Allow-Origin: http://localhost:3000\r\n") && with.contains("Vary: Origin"));
        let without = String::from_utf8(Response::new(200, "text/plain", "x".into()).to_bytes(false, None)).unwrap();
        assert!(!without.contains("Access-Control-Allow-Origin"));
    }

    #[test]
    fn token_checks() {
        assert!(authorized_t("", false, "/", None));
        assert!(authorized_t("s3cret", true, "/", None));
        assert!(!authorized_t("s3cret", false, "/", None));
        assert!(authorized_t("s3cret", false, "/api/devices?local=1&token=s3cret", None));
        assert!(!authorized_t("s3cret", false, "/api/devices?token=s3cre", None));
        assert!(authorized_t("s3cret", false, "/", Some("Bearer s3cret")));
        assert!(authorized_t("s3cret", false, "/", Some("bearer  s3cret ")));
        assert!(!authorized_t("s3cret", false, "/", Some("Basic s3cret")));
        assert!(authorized_t("a b", false, "/?token=a%20b", None));
        assert!(constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd") && !constant_time_eq(b"ab", b"abc"));
        assert!(!constant_time_eq(b"", b"\xFF"));
        assert!(is_loopback("::ffff:127.0.0.1".parse().unwrap()) && is_loopback("::1".parse().unwrap()));
        assert!(!is_loopback("192.168.1.2".parse().unwrap()));
        let ctx = ctx_of(hub());
        assert_eq!(
            respond(&Request { method: "GET", target: "/api/devices", authorized: false, body: "", can_write: false }, &ctx).status,
            401
        );
    }

    #[test]
    fn prometheus_format() {
        let hub = hub();
        hub.publish(Event::Init {
            id: "q\"\\\n".into(),
            name: "N".into(),
            kind: DeviceKind::Other,
            source: Source::Vendor,
            has_battery: true,
        });
        let m = metrics(&ctx_of(hub), false);
        assert!(m.contains("# TYPE batly_battery_percent gauge\n"));
        assert!(m.contains("batly_battery_percent{id=\"ABC123\",name=\"G <Pro> & Co\",type=\"Mouse\",source=\"native\",host=\"\"} 69\n"));
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
        s.write_all(b"GET /device/ABC123 HTTP/1.1\r\nHost: localhost\r\nOrigin: https://evil.example\r\n\r\n").unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        // Loopback: no token needed.
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(!text.contains("Access-Control-Allow-Origin"));
        assert!(text.ends_with("</xml>"));
    }

    #[test]
    fn event_stream_over_tcp() {
        let (ctx, addr) = serve("");
        let s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        (&s).write_all(b"GET /api/events HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:3000\r\n\r\n").unwrap();
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
        assert!(head.contains("Access-Control-Allow-Origin: http://localhost:3000"), "{head}");
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
