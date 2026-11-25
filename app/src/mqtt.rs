//! Minimal MQTT 3.1.1 client (QoS 0, `std::net`) publishing the devices to
//! a broker, with Home Assistant discovery.
//!
//! * `<base>/status`        – `online` (retained), `offline` as last will
//! * `<base>/<id>/state`    – device JSON (same as `/api/device/{id}`), retained
//! * `<base>/alert`         – alerts (`Notice`), not retained
//! * `<prefix>/sensor/batly_<id>/battery/config` … – discovery, retained
//! * `<base>/<id>/controls`, `<base>/<id>/light` – state of the controls of
//!   a Logitech device (lighting, DPI, report rate, backlight, wheel,
//!   Easy-Switch channel), retained; Home Assistant sends its commands on
//!   `<base>/<id>/set/<control>`

use crate::config;
use crate::hidpp::{BacklightMode, DeviceSettings, WheelMode};
use crate::http::{self, Context};
use crate::i18n::tr;
use crate::lighting::{EffectKind, LedEffect};
use crate::log;
use crate::model::{Device, Source};
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
    /// A PUBLISH from the broker: (topic, payload).
    Command(String, Vec<u8>),
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
        let tx3 = tx.clone();
        let _ = thread::Builder::new().name("mqtt".into()).stack_size(256 * 1024).spawn(move || run(&cfg, &ctx, &rx, &tx3));
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

fn run(cfg: &config::Mqtt, ctx: &Context, rx: &mpsc::Receiver<Msg>, tx: &mpsc::Sender<Msg>) {
    let mut delay = Duration::from_secs(1);
    loop {
        match session(cfg, ctx, rx, tx, &mut delay) {
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
fn session(cfg: &config::Mqtt, ctx: &Context, rx: &mpsc::Receiver<Msg>, tx: &mpsc::Sender<Msg>, delay: &mut Duration) -> io::Result<()> {
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

    // Home Assistant commands for the controls.
    s.write_all(&subscribe_packet(1, &format!("{base}/+/set/+")))?;

    // Reader: proves the broker is alive and forwards the commands.
    let start = Instant::now();
    let last_rx = Arc::new(AtomicU64::new(0));
    let mut reader = s.try_clone()?;
    reader.set_read_timeout(None)?;
    let lr = last_rx.clone();
    let commands = tx.clone();
    thread::Builder::new().name("mqtt-read".into()).stack_size(64 * 1024).spawn(move || {
        while let Ok((first, body)) = read_packet(&mut reader) {
            lr.store(start.elapsed().as_secs(), Ordering::Relaxed);
            if let Some((topic, payload)) = parse_publish(first, &body) {
                let _ = commands.send(Msg::Command(topic, payload));
            }
        }
    })?;

    let result = (|| {
        s.write_all(&publish_packet(&status, b"online", true))?;
        let mut announced: HashMap<String, String> = HashMap::new();
        let mut caps: HashMap<String, DeviceSettings> = HashMap::new();
        for d in ctx.hub.snapshot() {
            publish_device(&mut s, cfg, base, ctx, &d, &mut announced)?;
            sync_controls(&mut s, cfg, base, ctx, &d, &mut caps, false)?;
        }
        let mut last_ping = Instant::now();
        loop {
            match rx.recv_timeout(PING_EVERY) {
                Ok(Msg::Device(d)) => {
                    publish_device(&mut s, cfg, base, ctx, &d, &mut announced)?;
                    sync_controls(&mut s, cfg, base, ctx, &d, &mut caps, false)?;
                }
                Ok(Msg::Command(topic, payload)) => command(&mut s, cfg, base, ctx, &topic, &payload, &mut caps)?,
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

/// Announces the controls of a Logitech device of this computer the first
/// time it is seen online (or again with `force`), with their state;
/// removes them when the device is gone.
fn sync_controls(
    s: &mut impl Write,
    cfg: &config::Mqtt,
    base: &str,
    ctx: &Context,
    changed: &Device,
    caps: &mut HashMap<String, DeviceSettings>,
    force: bool,
) -> io::Result<()> {
    let tid = topic_id(&changed.id);
    let Some(d) = ctx.hub.find(&changed.id).filter(|d| d.id == changed.id) else {
        if let Some(st) = caps.remove(&changed.id) {
            for (topic, _) in control_discovery(cfg, base, ctx, changed, &st) {
                s.write_all(&publish_packet(&topic, b"", true))?;
            }
            s.write_all(&publish_packet(&format!("{base}/{tid}/controls"), b"", true))?;
            s.write_all(&publish_packet(&format!("{base}/{tid}/light"), b"", true))?;
        }
        return Ok(());
    };
    if !cfg.discovery || d.source != Source::Native || !d.online || (caps.contains_key(&d.id) && !force) {
        return Ok(());
    }
    let Some(read) = ctx.reader.get() else { return Ok(()) };
    let st = match read(&d.id) {
        Ok(st) => st,
        Err(e) => {
            log!("mqtt: settings of {}: {e:?}", d.id);
            return Ok(());
        }
    };
    for (topic, payload) in control_discovery(cfg, base, ctx, &d, &st) {
        s.write_all(&publish_packet(&topic, payload.as_bytes(), true))?;
    }
    let (controls, light) = control_state(&d, &st);
    s.write_all(&publish_packet(&format!("{base}/{tid}/controls"), controls.as_bytes(), true))?;
    if let Some(light) = light {
        s.write_all(&publish_packet(&format!("{base}/{tid}/light"), light.as_bytes(), true))?;
    }
    caps.insert(d.id.clone(), st);
    Ok(())
}

/// Applies a Home Assistant command (`<base>/<id>/set/<control>`), then
/// publishes the state read back from the device.
fn command(
    s: &mut impl Write,
    cfg: &config::Mqtt,
    base: &str,
    ctx: &Context,
    topic: &str,
    payload: &[u8],
    caps: &mut HashMap<String, DeviceSettings>,
) -> io::Result<()> {
    let Some(rest) = topic.strip_prefix(base).and_then(|t| t.strip_prefix('/')) else { return Ok(()) };
    let Some((tid, control)) = rest.split_once("/set/") else { return Ok(()) };
    let Some(d) = ctx.hub.snapshot().into_iter().find(|d| d.source == Source::Native && topic_id(&d.id) == tid) else { return Ok(()) };
    let Some(st) = caps.get(&d.id) else { return Ok(()) };
    let payload = String::from_utf8_lossy(payload);
    let Some(body) = command_body(control, &payload, st) else {
        log!("mqtt: unknown command {control} = {payload}");
        return Ok(());
    };
    let (Ok(settings), Some(apply)) = (http::parse_settings(&body.to_string()), ctx.control.get()) else { return Ok(()) };
    for setting in settings {
        if let Err(e) = apply(&d.id, setting) {
            log!("mqtt: {} {control}: {e:?}", d.id);
        }
    }
    sync_controls(s, cfg, base, ctx, &d, caps, true)
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

// ------------------------------------------------------- Home Assistant controls

/// The `device` block of every discovery message of `d`.
fn ha_device(ctx: &Context, d: &Device, tid: &str) -> Value {
    json!({
        "identifiers": [format!("batly_{tid}")],
        "name": ctx.name(d),
        "model": d.info.model.clone().unwrap_or_else(|| d.name.clone()),
        "manufacturer": "Batly",
        "sw_version": d.info.firmware,
        "suggested_area": d.info.host,
    })
}

fn discovery_prefix(cfg: &config::Mqtt) -> &str {
    let prefix = cfg.discovery_prefix.trim().trim_end_matches('/');
    if prefix.is_empty() {
        "homeassistant"
    } else {
        prefix
    }
}

fn wheel_name(w: WheelMode) -> &'static str {
    match w {
        WheelMode::FreeSpin => "free",
        WheelMode::Ratchet => "ratchet",
        WheelMode::Smart(_) => "smart",
    }
}

fn backlight_name(m: BacklightMode) -> &'static str {
    match m {
        BacklightMode::Off => "off",
        BacklightMode::On | BacklightMode::Manual => "on",
        BacklightMode::Auto => "auto",
    }
}

/// Zone and effect of the lighting Home Assistant drives: the one Batly
/// keeps, else the first zone.
fn light_of(st: &DeviceSettings) -> Option<(u8, LedEffect)> {
    let l = st.lighting.as_ref()?;
    if let Some(kept) = l.kept {
        return Some(kept);
    }
    let z = l.zones.first()?;
    Some((z.index, z.current.unwrap_or_default()))
}

/// Discovery messages of the controls a device supports: lighting (light),
/// DPI (number), report rate, backlight, wheel and Easy-Switch channel
/// (selects). Commands come on `<base>/<id>/set/<control>`.
pub fn control_discovery(cfg: &config::Mqtt, base: &str, ctx: &Context, d: &Device, st: &DeviceSettings) -> Vec<(String, String)> {
    let prefix = discovery_prefix(cfg);
    let tid = topic_id(&d.id);
    let device = ha_device(ctx, d, &tid);
    let availability = json!([
        { "topic": format!("{base}/status") },
        { "topic": format!("{base}/{tid}/state"), "value_template": "{{ 'online' if value_json.online else 'offline' }}" },
    ]);
    let entity = |component: &str, key: &str, name: &str, extra: Value| {
        let mut v = json!({
            "name": name,
            "unique_id": format!("batly_{tid}_{key}"),
            "object_id": format!("batly_{tid}_{key}"),
            "command_topic": format!("{base}/{tid}/set/{key}"),
            "state_topic": format!("{base}/{tid}/controls"),
            "availability": availability,
            "availability_mode": "all",
            "device": device,
        });
        if let (Some(obj), Value::Object(more)) = (v.as_object_mut(), extra) {
            obj.extend(more);
        }
        (format!("{prefix}/{component}/batly_{tid}/{key}/config"), v.to_string())
    };
    let mut out = Vec::new();
    if light_of(st).is_some() {
        out.push(entity(
            "light",
            "light",
            tr(["Lighting", "Éclairage", "Beleuchtung", "Iluminación", "Illuminazione", "Iluminação"]),
            json!({
                "schema": "json",
                "state_topic": format!("{base}/{tid}/light"),
                "supported_color_modes": ["rgb"],
                "brightness": true,
                "brightness_scale": 100,
            }),
        ));
    }
    if let Some(dpi) = &st.dpi {
        let min = dpi.choices.iter().copied().min().unwrap_or(dpi.current);
        let max = dpi.choices.iter().copied().max().unwrap_or(dpi.current);
        let step = dpi.choices.windows(2).map(|w| w[1].abs_diff(w[0])).filter(|s| *s > 0).min().unwrap_or(50);
        out.push(entity(
            "number",
            "dpi",
            tr(["Sensitivity (DPI)", "Sensibilité (DPI)", "Empfindlichkeit (DPI)", "Sensibilidad (DPI)", "Sensibilità (DPI)", "Sensibilidade (DPI)"]),
            json!({ "min": min, "max": max, "step": step, "mode": "box", "entity_category": "config", "value_template": "{{ value_json.dpi }}" }),
        ));
    }
    if let Some(rr) = st.report_rate.as_ref().filter(|r| r.choices.len() > 1) {
        let options: Vec<String> = rr.choices.iter().map(|ms| format!("{} Hz", 1000 / (*ms).max(1) as u32)).collect();
        out.push(entity(
            "select",
            "report_rate",
            tr(["Report rate", "Taux de rapport", "Abfragerate", "Tasa de informe", "Frequenza di polling", "Taxa de relatório"]),
            json!({ "options": options, "entity_category": "config", "value_template": "{{ value_json.report_rate }}" }),
        ));
    }
    if let Some(bl) = st.backlight.as_ref().filter(|b| b.modes.len() > 1) {
        let mut options: Vec<&str> = bl.modes.iter().map(|m| backlight_name(*m)).collect();
        options.dedup();
        out.push(entity(
            "select",
            "backlight",
            tr(["Backlight", "Rétroéclairage", "Hintergrundbeleuchtung", "Retroiluminación", "Retroilluminazione", "Retroiluminação"]),
            json!({ "options": options, "entity_category": "config", "value_template": "{{ value_json.backlight }}" }),
        ));
    }
    if st.smart_shift.is_some() {
        out.push(entity(
            "select",
            "wheel",
            tr(["Scroll wheel", "Molette", "Scrollrad", "Rueda", "Rotella", "Roda"]),
            json!({ "options": ["ratchet", "free", "smart"], "entity_category": "config", "value_template": "{{ value_json.wheel }}" }),
        ));
    }
    if let Some((_, count)) = d.info.host_channel.filter(|(_, n)| *n > 1) {
        let options: Vec<String> = (1..=count).map(|n| n.to_string()).collect();
        out.push(entity(
            "select",
            "host",
            tr([
                "Easy-Switch channel",
                "Canal Easy-Switch",
                "Easy-Switch-Kanal",
                "Canal Easy-Switch",
                "Canale Easy-Switch",
                "Canal Easy-Switch",
            ]),
            json!({ "options": options, "value_template": "{{ value_json.host }}" }),
        ));
    }
    out
}

/// State of the controls (`<base>/<id>/controls`) and of the light
/// (`<base>/<id>/light`, Home Assistant JSON light schema).
pub fn control_state(d: &Device, st: &DeviceSettings) -> (String, Option<String>) {
    let controls = json!({
        "dpi": st.dpi.as_ref().map(|x| x.current),
        "report_rate": st.report_rate.as_ref().map(|r| format!("{} Hz", 1000 / r.current.max(1) as u32)),
        "backlight": st.backlight.as_ref().map(|b| backlight_name(b.mode)),
        "wheel": st.smart_shift.map(wheel_name),
        "host": d.info.host_channel.map(|(cur, _)| cur.to_string()),
    });
    let light = light_of(st).map(|(_, e)| {
        let on = e.kind != EffectKind::Off;
        json!({
            "state": if on { "ON" } else { "OFF" },
            "color_mode": "rgb",
            "color": { "r": e.color >> 16 & 0xFF, "g": e.color >> 8 & 0xFF, "b": e.color & 0xFF },
            "brightness": if e.intensity == 0 { 100 } else { e.intensity },
        })
        .to_string()
    });
    (controls.to_string(), light)
}

/// Translates a Home Assistant command into the body of
/// `POST /api/device/{id}/settings` (see [`http::parse_settings`]).
pub fn command_body(control: &str, payload: &str, st: &DeviceSettings) -> Option<Value> {
    let p = payload.trim();
    Some(match control {
        "dpi" => json!({ "dpi": p.parse::<f64>().ok()?.round() as u64 }),
        "report_rate" => json!({ "report_rate": p.trim_end_matches("Hz").trim().parse::<u64>().ok()? }),
        "backlight" => json!({ "backlight": p }),
        "wheel" => match p {
            "smart" => json!({ "smart_shift": match st.smart_shift { Some(WheelMode::Smart(n)) => n, _ => 12 } }),
            other => json!({ "smart_shift": other }),
        },
        "host" => json!({ "host": p.parse::<u64>().ok()? }),
        "light" => {
            let cmd: Value = serde_json::from_str(p).ok()?;
            let (zone, current) = light_of(st)?;
            let off = cmd.get("state").and_then(Value::as_str) == Some("OFF");
            let color = match cmd.get("color") {
                Some(c) => {
                    let ch = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0).min(255) as u32;
                    ch("r") << 16 | ch("g") << 8 | ch("b")
                }
                // Turning on from "off": white rather than black.
                None if current.kind == EffectKind::Off || current.color == 0 => 0xFFFFFF,
                None => current.color,
            };
            let intensity = cmd.get("brightness").and_then(Value::as_u64).map(|b| b.clamp(1, 100)).unwrap_or(if current.intensity == 0 {
                100
            } else {
                current.intensity as u64
            });
            json!({ "lighting": {
                "action": "keep",
                "zone": zone,
                "effect": if off { "off" } else { "static" },
                "color": format!("#{color:06X}"),
                "intensity": intensity,
            }})
        }
        _ => return None,
    })
}

/// SUBSCRIBE to one topic filter, QoS 0.
pub fn subscribe_packet(id: u16, filter: &str) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    put_str(&mut body, filter.as_bytes());
    body.push(0);
    packet(0x82, &body)
}

/// Topic and payload of an incoming PUBLISH (QoS 0 or 1).
pub fn parse_publish(first: u8, body: &[u8]) -> Option<(String, Vec<u8>)> {
    if first >> 4 != 3 || body.len() < 2 {
        return None;
    }
    let len = u16::from_be_bytes([body[0], body[1]]) as usize;
    let topic = std::str::from_utf8(body.get(2..2 + len)?).ok()?.to_string();
    let mut rest = 2 + len;
    if first >> 1 & 3 > 0 {
        rest += 2; // packet id
    }
    Some((topic, body.get(rest..)?.to_vec()))
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

    fn settings() -> DeviceSettings {
        use crate::hidpp::{DpiSettings, ReportRateSettings};
        use crate::lighting::{LedApi, LedZone, Lighting, OnboardStatus};
        DeviceSettings {
            dpi: Some(DpiSettings { choices: vec![400, 800, 1600, 3200], current: 1600, default: 800 }),
            report_rate: Some(ReportRateSettings { choices: vec![1, 2, 4, 8], current: 1 }),
            backlight: None,
            smart_shift: Some(WheelMode::Smart(20)),
            lighting: Some(Lighting {
                api: LedApi::Rgb,
                zones: vec![LedZone {
                    index: 0,
                    location: 1,
                    effects: Vec::new(),
                    current: Some(LedEffect { kind: EffectKind::Static, color: 0x00A8FF, period_ms: 0, intensity: 80 }),
                }],
                onboard: OnboardStatus::Absent,
                kept: None,
            }),
        }
    }

    #[test]
    fn home_assistant_controls() {
        let ctx = ctx();
        let cfg = config::Mqtt { discovery: true, ..Default::default() };
        let mut d = ctx.hub.snapshot().remove(0);
        d.info.host_channel = Some((2, 3));
        let st = settings();
        let msgs = control_discovery(&cfg, "batly", &ctx, &d, &st);
        let topics: Vec<&str> = msgs.iter().map(|(t, _)| t.as_str()).collect();
        let tid = topic_id(&d.id);
        for t in
            ["light/batly_{}/light", "number/batly_{}/dpi", "select/batly_{}/report_rate", "select/batly_{}/wheel", "select/batly_{}/host"]
        {
            let want = format!("homeassistant/{}/config", t.replace("{}", &tid));
            assert!(topics.contains(&want.as_str()), "{want} missing in {topics:?}");
        }
        // No backlight on this device: no entity.
        assert!(!topics.iter().any(|t| t.contains("backlight")));
        let dpi: Value = serde_json::from_str(&msgs.iter().find(|(t, _)| t.contains("/dpi/")).unwrap().1).unwrap();
        assert_eq!((dpi["min"].as_u64(), dpi["max"].as_u64(), dpi["step"].as_u64()), (Some(400), Some(3200), Some(400)));
        assert_eq!(dpi["command_topic"], format!("batly/{tid}/set/dpi"));

        let (controls, light) = control_state(&d, &st);
        let c: Value = serde_json::from_str(&controls).unwrap();
        assert_eq!(
            (c["dpi"].as_u64(), c["report_rate"].as_str(), c["wheel"].as_str(), c["host"].as_str()),
            (Some(1600), Some("1000 Hz"), Some("smart"), Some("2"))
        );
        let l: Value = serde_json::from_str(&light.unwrap()).unwrap();
        assert_eq!((l["state"].as_str(), l["color"]["b"].as_u64(), l["brightness"].as_u64()), (Some("ON"), Some(255), Some(80)));
    }

    #[test]
    fn home_assistant_commands() {
        let st = settings();
        let parse = |control: &str, payload: &str| http::parse_settings(&command_body(control, payload, &st).unwrap().to_string()).unwrap();
        use crate::hidpp::Setting;
        assert_eq!(parse("dpi", "3200"), vec![Setting::Dpi(3200)]);
        assert_eq!(parse("dpi", "800.0"), vec![Setting::Dpi(800)]);
        assert_eq!(parse("report_rate", "500 Hz"), vec![Setting::ReportRate(2)]);
        assert_eq!(parse("wheel", "free"), vec![Setting::SmartShift(WheelMode::FreeSpin)]);
        assert_eq!(parse("wheel", "smart"), vec![Setting::SmartShift(WheelMode::Smart(20))]);
        assert_eq!(parse("host", "3"), vec![Setting::Host(3)]);
        // Light: colour and brightness kept by Batly; off; on again keeps the colour.
        let on = parse("light", r#"{"state":"ON","color":{"r":255,"g":0,"b":0},"brightness":40}"#);
        assert!(
            matches!(&on[..], [Setting::LedKeep { zone: 0, effect }] if effect.kind == EffectKind::Static && effect.color == 0xFF0000 && effect.intensity == 40)
        );
        let off = parse("light", r#"{"state":"OFF"}"#);
        assert!(matches!(&off[..], [Setting::LedKeep { effect, .. }] if effect.kind == EffectKind::Off));
        let again = parse("light", r#"{"state":"ON"}"#);
        assert!(matches!(&again[..], [Setting::LedKeep { effect, .. }] if effect.color == 0x00A8FF && effect.intensity == 80));
        assert!(command_body("unknown", "1", &st).is_none());
    }

    #[test]
    fn subscribe_and_incoming_publish() {
        let sub = subscribe_packet(1, "batly/+/set/+");
        assert_eq!(sub[0], 0x82);
        assert!(sub.ends_with(b"batly/+/set/+\0"));
        let p = publish_packet("batly/x/set/dpi", b"800", false);
        let (len, n) = decode_len(&p[1..]).unwrap();
        let body = &p[1 + n..1 + n + len];
        assert_eq!(parse_publish(p[0], body), Some(("batly/x/set/dpi".to_string(), b"800".to_vec())));
        // QoS 1: packet id after the topic.
        let mut q1 = body[..2 + 15].to_vec();
        q1.extend_from_slice(&[0, 7]);
        q1.extend_from_slice(b"on");
        assert_eq!(parse_publish(0x32, &q1), Some(("batly/x/set/dpi".to_string(), b"on".to_vec())));
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
