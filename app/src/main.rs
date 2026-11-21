//! LGSTray – battery levels of Logitech (and other) devices in the Windows
//! notification area.
//!
//! A from-scratch Rust implementation of LGSTrayBattery: a single small
//! native executable, no .NET runtime, no helper process, no hidapi.dll.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod alerts;
mod config;
mod fetch;
mod ghub;
mod hidpp;
mod history;
mod hotkey;
mod http;
mod i18n;
mod icon;
mod log;
mod model;
mod notice;
mod report;
mod sha256;
mod update;

#[cfg(windows)]
mod actions;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod panel;
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
    eprintln!("LGSTray only runs on Windows.");
    std::process::exit(1);
}

/// Language of the Windows user interface.
#[cfg(windows)]
pub fn system_language() -> i18n::Lang {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    // SAFETY: plain FFI call.
    i18n::Lang::from_langid(unsafe { GetUserDefaultUILanguage() } & 0x3FF)
}

/// `lgstray --status [--json]`: prints the devices of the running instance.
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
        let _ = writeln!(out, "{}", i18n::tr([
            "LGSTray is not running (or its web server is disabled).",
            "LGSTray n'est pas lancé (ou son serveur web est désactivé).",
            "LGSTray läuft nicht (oder sein Webserver ist deaktiviert).",
            "LGSTray no se está ejecutando (o su servidor web está desactivado).",
            "LGSTray non è in esecuzione (o il suo server web è disattivato).",
            "O LGSTray não está em execução (ou o servidor web está desativado).",
        ]));
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
            let mutex = CreateMutexW(null(), 0, win::wide("Local\\LGSTray-rs-instance").as_ptr());
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
    if !acquired {
        return;
    }

    if has("debug") {
        log::init(&cfg_path.with_file_name("lgstray.log"));
        log!("LGSTray {} - settings: {}", http::VERSION, cfg_path.display());
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
    let history_dir = cfg.history.enabled.then(|| cfg_path.with_file_name("history"));
    let history = Arc::new(history::History::open(history_dir, cfg.history.retention_days));
    {
        let history = history.clone();
        hub.subscribe(move |d| {
            history.record(d);
        });
    }
    hub.on_change(tray::sync_notifier(hwnd));

    let web_url = if cfg.http.enabled && !http::start(&cfg.http, hub.clone()).is_empty() {
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
    let actions = vec![
        actions::update_action(cfg_path.clone(), known_update),
        actions::report_action(hub.clone(), cfg_path.clone()),
    ];
    let services = tray::Services {
        native: cfg.native.enabled.then(|| native::Native::start(&cfg.native, hub.clone())),
        ghub: cfg.ghub.enabled.then(|| ghub::GHub::start(hub.clone())),
        web_url,
        history,
        refresh: Vec::new(),
        rescan: Vec::new(),
        notice_sinks: Vec::new(),
        actions,
        started_with: cfg.clone(),
    };

    tray::run(hwnd, hub, cfg, cfg_path, services);
}
