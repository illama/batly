//! Batly – battery levels of Logitech (and other) devices in the Windows
//! notification area.
//!
//! A from-scratch Rust implementation of LGSTrayBattery: a single small
//! native executable, no .NET runtime, no helper process, no hidapi.dll.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod alerts;
mod bluetooth;
mod config;
mod fetch;
mod gamepad;
mod ghub;
mod hidpp;
mod history;
mod hotkey;
mod http;
mod i18n;
mod icon;
mod lighting;
mod log;
mod model;
mod mqtt;
mod notice;
mod peers;
mod report;
mod sha256;
mod tracker;
mod update;
mod vendors;
mod webhooks;

#[cfg(windows)]
mod actions;
#[cfg(windows)]
mod hidio;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod panel;
#[cfg(any(windows, test))]
mod presence;
#[cfg(windows)]
mod receivers;
#[cfg(windows)]
mod settings_ui;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod uninstall;
#[cfg(windows)]
mod win;

#[cfg(not(windows))]
fn main() {
    eprintln!("Batly only runs on Windows.");
    std::process::exit(1);
}

/// Language of the Windows user interface.
#[cfg(windows)]
pub fn system_language() -> i18n::Lang {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    // SAFETY: plain FFI call.
    i18n::Lang::from_langid(unsafe { GetUserDefaultUILanguage() } & 0x3FF)
}

/// `batly --status [--json]`: prints the devices of the running instance.
#[cfg(windows)]
fn print_status(cfg: &config::Config, as_json: bool) -> i32 {
    use std::io::Write;
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain FFI call; fails harmlessly without a parent console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    let mut out = std::io::stdout();
    let url = format!("http://127.0.0.1:{}/api/devices", cfg.http.port);
    let reply = if cfg.http.enabled { fetch::get(&url, &[], std::time::Duration::from_secs(3)).ok() } else { None };
    let Some(reply) = reply.filter(|r| r.ok()) else {
        let _ = writeln!(
            out,
            "{}",
            i18n::tr([
                "Batly is not running (or its web server is disabled).",
                "Batly n'est pas lancé (ou son serveur web est désactivé).",
                "Batly läuft nicht (oder sein Webserver ist deaktiviert).",
                "Batly no se está ejecutando (o su servidor web está desactivado).",
                "Batly non è in esecuzione (o il suo server web è disattivato).",
                "O Batly não está em execução (ou o servidor web está desativado).",
            ])
        );
        return 2;
    };
    if as_json {
        let _ = writeln!(out, "{}", reply.text());
        return 0;
    }
    let list: Vec<serde_json::Value> = serde_json::from_slice(&reply.body).unwrap_or_default();
    for d in &list {
        let name = d["alias"].as_str().filter(|a| !a.is_empty()).or(d["name"].as_str()).unwrap_or("?");
        let level = d["battery_percent"].as_f64().map_or("–".to_string(), |p| i18n::percent(p as f32));
        let mut line = format!("{name:<32} {level:>6}  {}", d["status"].as_str().unwrap_or(""));
        if let Some(h) = d["time_left_hours"].as_f64() {
            line.push_str(&format!("  {}", i18n::time_left(h)));
        }
        if d["online"] == false {
            line.push_str(&format!("  ({})", i18n::offline()));
        }
        let _ = writeln!(out, "{line}");
    }
    if list.is_empty() {
        let _ = writeln!(out, "{}", i18n::no_device());
    }
    0
}

/// `--demo`: fake devices with some history, for screenshots and tests
/// without hardware.
#[cfg(windows)]
fn demo(hub: &model::Hub, history: &history::History) {
    use model::{Battery, DeviceInfo, DeviceKind, Event, Source, Status};
    let now = history::now();
    let devices = [
        ("demo-mx", "MX Master 3S", DeviceKind::Mouse, Source::Native, 62.0, Status::Discharging, 1.1),
        ("demo-kb", "MX Keys S", DeviceKind::Keyboard, Source::Native, 14.0, Status::Discharging, 0.4),
        ("demo-hs", "PRO X Wireless", DeviceKind::Headset, Source::GHub, 48.0, Status::Charging, -30.0),
        ("demo-pad", "Xbox Wireless Controller", DeviceKind::Gamepad, Source::Gamepad, 85.0, Status::Discharging, 3.0),
        ("demo-buds", "Galaxy Buds2", DeviceKind::Earbuds, Source::Bluetooth, 100.0, Status::Full, 0.0),
    ];
    for (id, name, kind, source, percent, status, rate) in devices {
        hub.publish(Event::Init { id: id.into(), name: name.into(), kind, source, has_battery: true });
        // Readings of the last hours at `rate` %/h, so estimates show up.
        let mut d = hub.find(id).unwrap();
        for h in (1..=20).rev() {
            let p = (percent + rate * h as f32).clamp(0.0, 100.0);
            let s = if rate < 0.0 { Status::Charging } else { status };
            d.battery = Some(Battery { percent: p, status: s, millivolts: None, mileage: None });
            history.record_at(&d, now - h * 3600 / if rate < 0.0 { 20 } else { 1 });
        }
        hub.publish(Event::Update { id: id.into(), battery: Battery { percent, status, millivolts: Some(3900), mileage: None } });
    }
    hub.publish(Event::Info {
        id: "demo-mx".into(),
        info: DeviceInfo { host_channel: Some((1, 3)), firmware: Some("RBM21.01".into()), ..Default::default() },
    });
}

#[cfg(windows)]
fn main() {
    use config::{Config, LoadError};
    use std::path::PathBuf;
    use std::ptr::null;
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_ascii_lowercase()).collect();
    let has = |flag: &str| args.iter().any(|a| a.trim_start_matches(['-', '/']) == flag);

    // SAFETY: plain FFI call without pointers.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let system_lang = system_language();
    i18n::init("auto", system_lang);

    if has("uninstall") {
        uninstall::run();
        return;
    }

    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default();
    let cfg_path = config::locate(&exe_dir, win::appdata());

    if has("status") {
        let cfg = Config::load(&cfg_path).unwrap_or_default();
        i18n::init(&cfg.ui.language, system_lang);
        std::process::exit(print_status(&cfg, has("json")));
    }

    // Single instance. The mutex handle is intentionally leaked: it lives
    // as long as the process. After a restart (new settings) the previous
    // instance may still be closing: wait for it a little.
    let tries = if has("restart") { 50 } else { 1 };
    let mut acquired = false;
    for attempt in 0..tries {
        // SAFETY: valid NUL-terminated name.
        unsafe {
            let mutex = CreateMutexW(null(), 0, win::wide("Local\\Batly-instance").as_ptr());
            if !mutex.is_null() && GetLastError() != ERROR_ALREADY_EXISTS {
                acquired = true;
                break;
            }
            if !mutex.is_null() {
                CloseHandle(mutex);
            }
        }
        if attempt + 1 < tries {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    // `--panel` / `--settings`: open that window, in the running instance
    // when there is one.
    let open_command = if has("settings") {
        Some(tray::ID_SETTINGS)
    } else if has("panel") {
        Some(tray::ID_PANEL)
    } else if has("receivers") {
        Some(tray::ID_RECEIVERS)
    } else {
        None
    };
    if !acquired {
        if let Some(command) = open_command {
            use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_COMMAND};
            // SAFETY: valid class name; posting to another process's window.
            unsafe {
                let other = FindWindowW(win::wide(win::WINDOW_CLASS).as_ptr(), null());
                if !other.is_null() {
                    windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(u32::MAX);
                    PostMessageW(other, WM_COMMAND, command, 0);
                }
            }
        }
        return;
    }

    if has("debug") {
        log::init(&cfg_path.with_file_name("batly.log"));
        log!("Batly {} - settings: {}", http::VERSION, cfg_path.display());
    }

    let cfg = match Config::load(&cfg_path) {
        Ok(cfg) => cfg,
        Err(LoadError::Parse(error)) => {
            let text = i18n::settings_invalid(&cfg_path.display().to_string(), &error);
            if win::confirm(i18n::settings_invalid_title(), &text) {
                let _ = std::fs::copy(&cfg_path, cfg_path.with_extension("toml.bak"));
                let _ = Config::default().save(&cfg_path);
            }
            Config::default()
        }
        Err(LoadError::Io(error)) => {
            log!("cannot read settings: {error}");
            Config::default()
        }
    };
    i18n::init(&cfg.ui.language, system_lang);

    let Some(hwnd) = tray::create_window() else { return };
    let hub = Arc::new(model::Hub::new());

    // History first, so the statistics include the reading that woke the UI.
    // The demo keeps its fake readings in memory only.
    let history_dir = (cfg.history.enabled && !has("demo")).then(|| cfg_path.with_file_name("history"));
    let history = Arc::new(history::History::open(history_dir, cfg.history.retention_days));
    {
        let history = history.clone();
        hub.subscribe(move |d| {
            history.record(d);
        });
    }
    hub.on_change(tray::sync_notifier(hwnd));

    if has("demo") {
        demo(&hub, &history);
    }

    // Settings shared with the servers (aliases), kept current by the tray.
    let shared_cfg = Arc::new(std::sync::Mutex::new(cfg.clone()));
    let names = shared_cfg.clone();
    let ctx =
        Arc::new(http::Context::new(hub.clone(), history.clone()).with_names(move |id, name| names.lock().unwrap().display_name(id, name)));
    let web_url = if cfg.http.enabled && !http::start(&cfg.http, ctx.clone()).is_empty() {
        let host = match cfg.http.addr.trim() {
            "" | "localhost" | "0.0.0.0" | "+" | "*" => "localhost",
            host => host,
        };
        Some(format!("http://{host}:{}/", cfg.http.port))
    } else {
        None
    };
    let known_update = Arc::new(std::sync::Mutex::new(None));
    actions::start_auto_check(&cfg, known_update.clone(), Arc::new(tray::notice_poster(hwnd)));
    let actions = vec![actions::update_action(cfg_path.clone(), known_update), actions::report_action(hub.clone(), cfg_path.clone())];
    // Integrations. They stop when dropped: the tray keeps them alive.
    let mut keep_alive: Vec<Box<dyn std::any::Any>> = Vec::new();
    keep_alive.push(Box::new(peers::start(&cfg.http.peers, &ctx)));
    let mut notice_sinks: Vec<Box<dyn Fn(&notice::Notice)>> = Vec::new();
    if cfg.mqtt.enabled && !cfg.mqtt.host.trim().is_empty() {
        let mqtt = std::rc::Rc::new(mqtt::Mqtt::start(&cfg.mqtt, ctx.clone()));
        let sink = mqtt.clone();
        notice_sinks.push(Box::new(move |n| sink.notice(n)));
        keep_alive.push(Box::new(mqtt));
    }
    if !cfg.webhooks.urls.is_empty() {
        let webhooks = webhooks::Webhooks::start(&cfg.webhooks);
        let changes = webhooks.clone();
        hub.subscribe(move |d| changes.device_changed(d));
        notice_sinks.push(Box::new(move |n| webhooks.send(n)));
    }

    // Other brands and standard devices.
    let mut refresh: Vec<Box<dyn Fn()>> = Vec::new();
    let mut rescan: Vec<Box<dyn Fn()>> = Vec::new();
    if cfg.other.bluetooth {
        let b = std::rc::Rc::new(bluetooth::Bluetooth::start(&cfg.other, hub.clone()));
        let (b1, b2) = (b.clone(), b);
        refresh.push(Box::new(move || b1.refresh()));
        rescan.push(Box::new(move || b2.rescan()));
    }
    if cfg.other.gamepads {
        let g = std::rc::Rc::new(gamepad::Gamepads::start(&cfg.other, hub.clone()));
        let (g1, g2) = (g.clone(), g);
        refresh.push(Box::new(move || g1.refresh()));
        rescan.push(Box::new(move || g2.rescan()));
    }
    if cfg.other.razer || cfg.other.corsair || cfg.other.steelseries {
        let v = std::rc::Rc::new(vendors::Vendors::start(&cfg.other, hub.clone()));
        let (v1, v2) = (v.clone(), v);
        refresh.push(Box::new(move || v1.refresh()));
        rescan.push(Box::new(move || v2.rescan()));
    }

    let services = tray::Services {
        native: cfg.native.enabled.then(|| native::Native::start(&cfg.native, hub.clone(), Some(cfg_path.with_file_name("backups")))),
        ghub: cfg.ghub.enabled.then(|| ghub::GHub::start(hub.clone())),
        web_url,
        history,
        refresh,
        rescan,
        notice_sinks,
        actions,
        shared_cfg,
        _keep_alive: keep_alive,
        started_with: cfg.clone(),
    };

    if let Some(command) = open_command {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_COMMAND};
        // SAFETY: our own window; handled once the message loop runs.
        unsafe { PostMessageW(hwnd, WM_COMMAND, command, 0) };
    }
    tray::run(hwnd, hub, cfg, cfg_path, services);
}
