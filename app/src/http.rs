//! Minimal HTTP server exposing the device list.
//!
//! * `GET /` or `/devices`      – HTML index
//! * `GET /device/{id|name}`    – XML, same fields as the original app
//! * `GET /api/devices`         – JSON array
//! * `GET /api/device/{id|name}`– JSON object
//!
//! Built on `std::net` only: no HTTP.sys, hence no admin rights needed to
//! listen on every interface.

use crate::config;
use crate::log;
use crate::model::{Device, Hub, Status};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Starts the listeners; returns the addresses actually bound.
pub fn start(cfg: &config::Http, hub: Arc<Hub>) -> Vec<SocketAddr> {
    let mut bound = Vec::new();
    for addr in resolve(cfg) {
        match TcpListener::bind(addr) {
            Ok(listener) => {
                bound.push(listener.local_addr().unwrap_or(addr));
                let hub = hub.clone();
                let _ = thread::Builder::new().name("http".into()).stack_size(128 * 1024).spawn(move || {
                    for stream in listener.incoming().flatten() {
                        if let Err(e) = handle(stream, &hub) {
                            log!("http: {e}");
                        }
                    }
                });
            }
            Err(e) => log!("http: cannot bind {addr}: {e}"),
        }
    }
    bound
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

fn handle(stream: TcpStream, hub: &Hub) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::with_capacity(2048, &stream);
    let mut request_line = String::new();
    reader.by_ref().take(4096).read_line(&mut request_line)?;
    // Drain the headers so the peer does not see a reset.
    let mut line = String::new();
    for _ in 0..100 {
        line.clear();
        if reader.by_ref().take(8192).read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let response = respond(method, target, hub);
    let mut out = &stream;
    out.write_all(&response.to_bytes(method == "HEAD"))?;
    out.flush()
}

use std::io::Read as _;

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
            404 => "Not Found",
            405 => "Method Not Allowed",
            _ => "Error",
        };
        let mut out = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
            self.status,
            reason,
            self.content_type,
            self.body.len()
        )
        .into_bytes();
        if !head_only {
            out.extend_from_slice(self.body.as_bytes());
        }
        out
    }
}

pub fn respond(method: &str, target: &str, hub: &Hub) -> Response {
    if method != "GET" && method != "HEAD" {
        return Response::new(405, "text/plain", "Method not allowed.".into());
    }
    let path = target.split(['?', '#']).next().unwrap_or("");
    let path = if path.len() > 1 { path.trim_end_matches('/') } else { path };

    match path {
        "/" | "/devices" => Response::new(200, "text/html", index(&hub.snapshot())),
        "/api/devices" => {
            let items: Vec<String> = hub.snapshot().iter().map(json).collect();
            Response::new(200, "application/json", format!("[{}]", items.join(",")))
        }
        _ => {
            let (key, as_json) = if let Some(k) = path.strip_prefix("/api/device/") {
                (k, true)
            } else if let Some(k) = path.strip_prefix("/device/") {
                (k, false)
            } else {
                return Response::new(404, "text/plain", "Not found.".into());
            };
            let key = percent_decode(key);
            match hub.find(&key) {
                Some(d) if as_json => Response::new(200, "application/json", json(&d)),
                Some(d) => Response::new(200, "text/xml", xml(&d)),
                None => Response::new(404, "text/plain", format!("{key} not found.")),
            }
        }
    }
}

fn index(devices: &[Device]) -> String {
    let mut s = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>LGSTray</title>\
         <style>body{font:15px system-ui,sans-serif;margin:2em}td,th{padding:.25em 1em .25em 0;text-align:left}</style>\
         </head><body><h3>LGSTray devices</h3>",
    );
    if devices.is_empty() {
        s.push_str("<p>No device found yet.</p>");
    } else {
        s.push_str("<table><tr><th>Name</th><th>Battery</th><th>By id</th><th>By name</th><th>JSON</th></tr>");
        for d in devices {
            let name = escape(&d.name);
            let id_url = percent_encode(&d.id);
            let battery = d.battery.map_or_else(|| "–".to_string(), |b| format!("{:.0} %", b.percent));
            s.push_str(&format!(
                "<tr><td>{name}</td><td>{battery}</td><td><a href=\"/device/{id_url}\">{id}</a></td>\
                 <td><a href=\"/device/{name_url}\">{name}</a></td><td><a href=\"/api/device/{id_url}\">json</a></td></tr>",
                id = escape(&d.id),
                name_url = percent_encode(&d.name),
            ));
        }
        s.push_str("</table><p><a href=\"/api/devices\">/api/devices</a></p>");
    }
    s.push_str(&format!("<hr><i>LGSTray version: {VERSION}</i></body></html>"));
    s
}

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

pub fn json(d: &Device) -> String {
    let b = d.battery;
    let opt = |v: Option<String>| v.unwrap_or_else(|| "null".into());
    format!(
        "{{\"id\":{},\"name\":{},\"type\":\"{}\",\"source\":\"{}\",\"online\":{},\"has_battery\":{},\
         \"battery_percent\":{},\"battery_voltage\":{},\"mileage\":{},\"status\":\"{}\",\"charging\":{},\"last_update\":{}}}",
        serde_json::Value::from(d.id.as_str()),
        serde_json::Value::from(d.name.as_str()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DeviceKind, Event, Source};

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

    #[test]
    fn routes() {
        let hub = hub();
        let r = respond("GET", "/", &hub);
        assert_eq!(r.status, 200);
        assert!(r.body.contains("G &lt;Pro&gt; &amp; Co") && r.body.contains("/device/ABC123"));
        assert!(r.body.contains("/device/G%20%3CPro%3E%20%26%20Co"));

        let r = respond("GET", "/device/ABC123?x=1", &hub);
        assert_eq!((r.status, r.content_type), (200, "text/xml"));
        assert!(r.body.contains("<device_id>ABC123</device_id>"));
        assert!(r.body.contains("<device_name>G &lt;Pro&gt; &amp; Co</device_name>"));
        assert!(r.body.contains("<device_type>Mouse</device_type>"));
        assert!(r.body.contains("<battery_percent>69.00</battery_percent>"));
        assert!(r.body.contains("<battery_voltage>3.92</battery_voltage>"));
        assert!(r.body.contains("<charging>True</charging>"));

        // Lookup by (percent-encoded) name.
        assert_eq!(respond("GET", "/device/G%20%3CPro%3E%20%26%20Co", &hub).status, 200);
        assert_eq!(respond("GET", "/device/nope", &hub).status, 404);
        assert_eq!(respond("GET", "/nothing", &hub).status, 404);
        assert_eq!(respond("POST", "/", &hub).status, 405);

        let r = respond("GET", "/api/devices/", &hub);
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v[0]["name"], "G <Pro> & Co");
        assert_eq!(v[0]["battery_percent"], 69);
        assert_eq!(v[0]["charging"], true);
        assert_eq!(v[0]["mileage"], serde_json::Value::Null);
        let r = respond("GET", "/api/device/ABC123", &hub);
        assert!(serde_json::from_str::<serde_json::Value>(&r.body).unwrap().is_object());
    }

    #[test]
    fn percent_decoding_edge_cases() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert_eq!(percent_decode("%C3%A9"), "é");
    }

    #[test]
    fn timestamps() {
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601(UNIX_EPOCH + Duration::from_secs(1_795_251_845)), "2026-11-21T09:04:05Z");
    }

    #[test]
    fn serves_over_tcp() {
        let cfg = config::Http { addr: "127.0.0.1".into(), port: 0, use_ipv6: false, ..Default::default() };
        let bound = start(&cfg, Arc::new(hub()));
        assert_eq!(bound.len(), 1);
        let mut s = TcpStream::connect(bound[0]).unwrap();
        s.write_all(b"GET /device/ABC123 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("Access-Control-Allow-Origin: *"));
        assert!(text.ends_with("</xml>"));
    }
}
