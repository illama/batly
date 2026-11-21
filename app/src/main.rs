//! LGSTray – Logitech battery levels in the Windows notification area.
//!
//! A from-scratch Rust implementation of LGSTrayBattery: a single small
//! native executable, no .NET runtime, no helper process, no hidapi.dll.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod fetch;
mod ghub;
mod history;
mod hidpp;
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
mod native;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod uninstall;
#[cfg(windows)]
mod win;

#[cfg(not(windows))]
fn main() {
    eprintln!("LGSTray only runs on Windows.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    use config::{Config, LoadError};
    use std::path::PathBuf;
    use std::ptr::null;
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_ascii_lowercase()).collect();
    let has = |flag: &str| args.iter().any(|a| a.trim_start_matches(['-', '/']) == flag);

    // SAFETY: plain FFI calls without pointers.
    let system_lang = unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        i18n::Lang::from_langid(GetUserDefaultUILanguage() & 0x3FF)
    };
    i18n::init("auto", system_lang);

    if has("uninstall") {
        uninstall::run();
        return;
    }

    // Single instance. The mutex handle is intentionally leaked: it lives
    // as long as the process.
    // SAFETY: valid NUL-terminated name.
    unsafe {
        let mutex = CreateMutexW(null(), 0, win::wide("Local\\LGSTray-rs-instance").as_ptr());
        if mutex.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
    }

    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default();
    let cfg_path = config::locate(&exe_dir, win::appdata());
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
    let services = tray::Services {
        native: cfg.native.enabled.then(|| native::Native::start(&cfg.native, hub.clone())),
        ghub: cfg.ghub.enabled.then(|| ghub::GHub::start(hub.clone())),
        web_url,
    };

    tray::run(hwnd, hub, cfg, cfg_path, services);
}
