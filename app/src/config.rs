//! Settings, stored as TOML. The format is a superset of the original
//! `appsettings.toml`, so an existing file can be dropped in as is.

use serde::Deserialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{fs, io};

pub const FILE_NAME: &str = "appsettings.toml";

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "UI")]
    pub ui: Ui,
    #[serde(rename = "HTTPServer")]
    pub http: Http,
    #[serde(rename = "GHub")]
    pub ghub: GHub,
    #[serde(rename = "Native")]
    pub native: Native,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Ui {
    /// Show the percentage as a number instead of a battery gauge.
    pub numeric_display: bool,
    /// Ids of the devices that get their own tray icon.
    pub selected_devices: Vec<String>,
    /// Show a notification at or below this level (0 = never).
    pub low_battery_threshold: u8,
    /// "auto", "en" or "fr".
    pub language: String,
    /// False until the first device has been picked automatically.
    pub first_run_done: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Http {
    pub enabled: bool,
    pub addr: String,
    pub port: u16,
    pub use_ipv6: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct GHub {
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Native {
    pub enabled: bool,
    pub retry_time: u32,
    pub poll_period: u32,
    pub disabled_devices: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { ui: Ui::default(), http: Http::default(), ghub: GHub::default(), native: Native::default() }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            numeric_display: false,
            selected_devices: Vec::new(),
            low_battery_threshold: 15,
            language: "auto".into(),
            first_run_done: false,
        }
    }
}

impl Default for Http {
    fn default() -> Self {
        Self { enabled: true, addr: "localhost".into(), port: 12321, use_ipv6: true }
    }
}

impl Default for GHub {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for Native {
    fn default() -> Self {
        Self { enabled: true, retry_time: 10, poll_period: 600, disabled_devices: Vec::new() }
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(String),
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut cfg: Config = toml::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        cfg.sanitize();
        Ok(cfg)
    }

    fn sanitize(&mut self) {
        self.native.retry_time = self.native.retry_time.clamp(1, 3600);
        self.native.poll_period = self.native.poll_period.clamp(5, 86_400);
        self.ui.low_battery_threshold = self.ui.low_battery_threshold.min(100);
        self.ui.selected_devices.dedup();
    }

    /// Loads the file; a missing file yields the defaults (and writes them).
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(LoadError::Parse),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let cfg = Self::default();
                let _ = cfg.save(path);
                Ok(cfg)
            }
            Err(e) => Err(LoadError::Io(e)),
        }
    }

    /// Writes atomically (temp file + rename) so a crash never leaves a
    /// truncated settings file behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, self.to_toml())?;
        fs::rename(&tmp, path)
    }

    pub fn to_toml(&self) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "# LGSTray settings - https://toml.io for the syntax (note the \"\" around strings).\n\
             # Restart LGSTray after editing this file by hand.\n\
             \n\
             [UI]\n\
             # Show the battery level as a number instead of a gauge\n\
             numericDisplay = {}\n\
             # Devices that get their own tray icon (managed from the tray menu)\n\
             selectedDevices = {}\n\
             # Notify when a device drops to this percentage (0 = never)\n\
             lowBatteryThreshold = {}\n\
             # \"auto\", \"en\" or \"fr\"\n\
             language = {}\n\
             firstRunDone = {}\n\
             \n\
             [HTTPServer]\n\
             enabled = {}\n\
             # \"localhost\", an IP address / host name of this machine, or \"0.0.0.0\" for all interfaces\n\
             addr = {}\n\
             port = {}\n\
             useIpv6 = {}\n\
             \n\
             [GHub]\n\
             # Read battery levels from Logitech G HUB (if it is running)\n\
             enabled = {}\n\
             \n\
             [Native]\n\
             # Talk to the devices directly (HID++)\n\
             enabled = {}\n\
             \n\
             # Time in seconds to retry a device poll on failure\n\
             retryTime = {}\n\
             \n\
             # Time in seconds to next device poll on success. Keep it high: frequent\n\
             # polling can interfere with the power saving modes of the devices.\n\
             pollPeriod = {}\n\
             \n\
             # Devices whose name contains one of these strings are never polled,\n\
             # e.g. [\"G403\", \"G502\"]\n\
             disabledDevices = {}\n",
            self.ui.numeric_display,
            array(&self.ui.selected_devices),
            self.ui.low_battery_threshold,
            quote(&self.ui.language),
            self.ui.first_run_done,
            self.http.enabled,
            quote(&self.http.addr),
            self.http.port,
            self.http.use_ipv6,
            self.ghub.enabled,
            self.native.enabled,
            self.native.retry_time,
            self.native.poll_period,
            array(&self.native.disabled_devices),
        );
        s
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    let mut s = String::from("[\n");
    for item in items {
        let _ = writeln!(s, "    {},", quote(item));
    }
    s.push(']');
    s
}

/// Settings live next to the executable when a file is already there
/// (portable mode), otherwise in `%APPDATA%\LGSTray`.
pub fn locate(exe_dir: &Path, appdata: Option<PathBuf>) -> PathBuf {
    let portable = exe_dir.join(FILE_NAME);
    match appdata {
        Some(dir) if !portable.exists() => dir.join("LGSTray").join(FILE_NAME),
        _ => portable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_file_is_accepted() {
        let text = include_str!("../tests/upstream_appsettings.toml");
        let cfg = Config::parse(text).unwrap();
        assert!(cfg.http.enabled && cfg.ghub.enabled && cfg.native.enabled);
        assert_eq!((cfg.http.addr.as_str(), cfg.http.port, cfg.http.use_ipv6), ("localhost", 12321, true));
        assert_eq!((cfg.native.retry_time, cfg.native.poll_period), (10, 600));
        assert_eq!(cfg.ui, Ui::default());
    }

    /// `packaging/appsettings.toml` ships in the portable archive.
    #[test]
    fn packaged_default_file_is_in_sync() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../packaging/appsettings.toml");
        if std::env::var_os("UPDATE_PACKAGING").is_some() {
            std::fs::write(path, Config::default().to_toml()).unwrap();
        }
        assert_eq!(std::fs::read_to_string(path).unwrap().replace("\r\n", "\n"), Config::default().to_toml());
    }

    #[test]
    fn round_trip_with_awkward_strings() {
        let mut cfg = Config::default();
        cfg.ui.numeric_display = true;
        cfg.ui.selected_devices = vec!["dev00000001".into(), "A \"quoted\" \\ id\n".into()];
        cfg.ui.low_battery_threshold = 20;
        cfg.http.addr = "0.0.0.0".into();
        cfg.http.port = 8080;
        cfg.native.disabled_devices = vec!["G403".into(), "Ünïcödé".into()];
        assert_eq!(Config::parse(&cfg.to_toml()).unwrap(), cfg);
        assert_eq!(Config::parse(&Config::default().to_toml()).unwrap(), Config::default());
    }

    #[test]
    fn empty_and_invalid_input() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert!(Config::parse("[Native]\npollPeriod = \"soon\"").is_err());
        let cfg = Config::parse("[Native]\npollPeriod = 0\nretryTime = 0").unwrap();
        assert_eq!((cfg.native.poll_period, cfg.native.retry_time), (5, 1));
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("lgstray-test-{}", std::process::id()));
        let path = dir.join("sub").join(FILE_NAME);
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(path.exists(), "defaults are written on first load");
        let mut cfg2 = cfg.clone();
        cfg2.ui.selected_devices.push("x".into());
        cfg2.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg2);
        assert_eq!(locate(path.parent().unwrap(), Some(dir.clone())), path);
        assert_eq!(locate(&dir, Some(dir.clone())), dir.join("LGSTray").join(FILE_NAME));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
