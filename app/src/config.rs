//! Settings, stored as TOML. The format is a superset of the original
//! `appsettings.toml`, so an existing file can be dropped in as is.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{fs, io};

pub const FILE_NAME: &str = "appsettings.toml";

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "UI")]
    pub ui: Ui,
    #[serde(rename = "Alerts")]
    pub alerts: Alerts,
    #[serde(rename = "History")]
    pub history: History,
    #[serde(rename = "HTTPServer")]
    pub http: Http,
    #[serde(rename = "GHub")]
    pub ghub: GHub,
    #[serde(rename = "Native")]
    pub native: Native,
    #[serde(rename = "OtherDevices")]
    pub other: OtherDevices,
    #[serde(rename = "MQTT")]
    pub mqtt: Mqtt,
    #[serde(rename = "Webhooks")]
    pub webhooks: Webhooks,
    #[serde(rename = "Updates")]
    pub updates: Updates,
    /// Per device settings, keyed by device id.
    #[serde(rename = "Devices")]
    pub devices: BTreeMap<String, DeviceSettings>,
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
    /// "auto", "en", "fr", "de", "es", "it" or "pt".
    pub language: String,
    /// False until the first device has been picked automatically.
    pub first_run_done: bool,
    /// One single icon showing the device with the lowest battery.
    pub single_icon: bool,
    /// Hide a device icon while its level is above this percentage (0 = never hide).
    pub hide_above: u8,
    /// Global shortcut that opens the battery panel, e.g. "Ctrl+Alt+B" ("" = none).
    pub hotkey: String,
    /// Device ids in display order (devices not listed come after, by name).
    pub device_order: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Alerts {
    /// Warn when the estimated time left drops below `predictiveHours`.
    pub predictive: bool,
    pub predictive_hours: u32,
    /// Notify when a charging device reaches this level (0 = off), e.g. 80
    /// to preserve the battery.
    pub charge_limit: u8,
    /// Notify when a device is fully charged.
    pub charge_complete: bool,
    /// Notify when a device has been offline for this many hours (0 = off).
    pub offline_hours: u32,
    /// Hold notifications while a full screen application runs or
    /// "Do not disturb" is on; they are shown afterwards.
    pub quiet_when_busy: bool,
    /// "Remind me later" delay, in hours.
    pub snooze_hours: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct History {
    /// Record every reading (needed for the time-left estimate and charts).
    pub enabled: bool,
    /// Readings older than this are deleted.
    pub retention_days: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Http {
    pub enabled: bool,
    pub addr: String,
    pub port: u16,
    pub use_ipv6: bool,
    /// Access token required from other computers ("" = none). Requests
    /// from this computer never need it.
    pub token: String,
    /// Other Batly instances to merge into this one, e.g.
    /// ["http://living-room-pc:12321"] (add "#token" after the URL if needed).
    pub peers: Vec<String>,
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct OtherDevices {
    /// Any Bluetooth device that reports its battery to Windows.
    pub bluetooth: bool,
    /// Xbox (XInput) and PlayStation controllers.
    pub gamepads: bool,
    /// Razer, Corsair and SteelSeries wireless devices.
    pub razer: bool,
    pub corsair: bool,
    pub steelseries: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Mqtt {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Topics are `<baseTopic>/<device>/state`.
    pub base_topic: String,
    /// Home Assistant MQTT discovery.
    pub discovery: bool,
    pub discovery_prefix: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Webhooks {
    /// URLs that receive a JSON POST on every alert (low battery, charged…).
    /// Discord and Slack webhook URLs get a message in their own format.
    pub urls: Vec<String>,
    /// Also POST every level change, not only alerts.
    pub all_changes: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Updates {
    /// Look for a new version on GitHub once a day.
    pub check: bool,
    /// Version the user chose to skip.
    pub skipped: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceSettings {
    /// Display name ("" = the device name).
    pub alias: String,
    /// "auto", "mouse", "keyboard", "headset", "gamepad", "earbuds" or "other".
    pub icon: String,
    /// Low battery level for this device (absent = the global one).
    pub low_battery_threshold: Option<u8>,
    /// No alert at all for this device.
    pub muted: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            numeric_display: false,
            selected_devices: Vec::new(),
            low_battery_threshold: 15,
            language: "auto".into(),
            first_run_done: false,
            single_icon: false,
            hide_above: 0,
            hotkey: "Ctrl+Alt+B".into(),
            device_order: Vec::new(),
        }
    }
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            predictive: true,
            predictive_hours: 24,
            charge_limit: 0,
            charge_complete: true,
            offline_hours: 0,
            quiet_when_busy: true,
            snooze_hours: 4,
        }
    }
}

impl Default for History {
    fn default() -> Self {
        Self { enabled: true, retention_days: 730 }
    }
}

impl Default for Http {
    fn default() -> Self {
        Self { enabled: true, addr: "localhost".into(), port: 12321, use_ipv6: true, token: String::new(), peers: Vec::new() }
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

impl Default for OtherDevices {
    fn default() -> Self {
        Self { bluetooth: true, gamepads: true, razer: true, corsair: true, steelseries: true }
    }
}

impl Default for Mqtt {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 1883,
            username: String::new(),
            password: String::new(),
            base_topic: "batly".into(),
            discovery: true,
            discovery_prefix: "homeassistant".into(),
        }
    }
}

impl Default for Updates {
    fn default() -> Self {
        Self { check: true, skipped: String::new() }
    }
}

impl Config {
    /// Settings of one device (defaults when there are none).
    pub fn device(&self, id: &str) -> DeviceSettings {
        self.devices.get(id).cloned().unwrap_or_default()
    }

    /// Mutable settings of one device, created on demand.
    pub fn device_mut(&mut self, id: &str) -> &mut DeviceSettings {
        self.devices.entry(id.to_string()).or_default()
    }

    /// Name to show for a device: its alias, or the name it reports.
    pub fn display_name(&self, id: &str, name: &str) -> String {
        match self.devices.get(id).map(|d| d.alias.trim()) {
            Some(alias) if !alias.is_empty() => alias.to_string(),
            _ => name.to_string(),
        }
    }

    /// Low battery level that applies to a device (0 = no alert).
    pub fn threshold(&self, id: &str) -> u8 {
        match self.devices.get(id) {
            Some(d) if d.muted => 0,
            Some(DeviceSettings { low_battery_threshold: Some(t), .. }) => *t,
            _ => self.ui.low_battery_threshold,
        }
    }

    /// Settings that need the backends / servers to be restarted.
    pub fn services_differ(&self, other: &Config) -> bool {
        self.http != other.http
            || self.ghub != other.ghub
            || self.native != other.native
            || self.other != other.other
            || self.mqtt != other.mqtt
            || self.history != other.history
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
        self.ui.hide_above = self.ui.hide_above.min(100);
        self.ui.selected_devices.dedup();
        self.ui.device_order.dedup();
        self.alerts.predictive_hours = self.alerts.predictive_hours.clamp(1, 24 * 14);
        self.alerts.charge_limit = self.alerts.charge_limit.min(100);
        self.alerts.snooze_hours = self.alerts.snooze_hours.clamp(1, 24 * 7);
        self.history.retention_days = self.history.retention_days.clamp(7, 3650);
        for d in self.devices.values_mut() {
            d.low_battery_threshold = d.low_battery_threshold.map(|t| t.min(100));
            if d.icon.trim().eq_ignore_ascii_case("auto") {
                d.icon.clear();
            }
        }
        // Drop entries that hold nothing but defaults.
        self.devices.retain(|_, d| *d != DeviceSettings::default());
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
        let mut line = |text: String| {
            s.push_str(&text);
            s.push('\n');
        };
        let (ui, al, hi, http, mq, wh) = (&self.ui, &self.alerts, &self.history, &self.http, &self.mqtt, &self.webhooks);
        line("# Batly settings - https://toml.io for the syntax (note the \"\" around strings).".into());
        line("# Changes made here are applied as soon as the file is saved.".into());
        line(String::new());
        line("[UI]".into());
        line("# Show the battery level as a number instead of a gauge".into());
        line(format!("numericDisplay = {}", ui.numeric_display));
        line("# Devices that get their own tray icon (managed from the tray menu)".into());
        line(format!("selectedDevices = {}", array(&ui.selected_devices)));
        line("# Notify when a device drops to this percentage (0 = never)".into());
        line(format!("lowBatteryThreshold = {}", ui.low_battery_threshold));
        line("# \"auto\", \"en\", \"fr\", \"de\", \"es\", \"it\" or \"pt\"".into());
        line(format!("language = {}", quote(&ui.language)));
        line(format!("firstRunDone = {}", ui.first_run_done));
        line("# One single icon showing the device with the lowest battery".into());
        line(format!("singleIcon = {}", ui.single_icon));
        line("# Hide a device icon while its level is above this percentage (0 = never hide)".into());
        line(format!("hideAbove = {}", ui.hide_above));
        line("# Shortcut that opens the battery panel, e.g. \"Ctrl+Alt+B\" or \"Win+Shift+F9\" (\"\" = none)".into());
        line(format!("hotkey = {}", quote(&ui.hotkey)));
        line("# Device ids in display order".into());
        line(format!("deviceOrder = {}", array(&ui.device_order)));
        line(String::new());
        line("[Alerts]".into());
        line("# Warn when the estimated time left drops below predictiveHours".into());
        line(format!("predictive = {}", al.predictive));
        line(format!("predictiveHours = {}", al.predictive_hours));
        line("# Notify when a charging device reaches this level, e.g. 80 to preserve the battery (0 = off)".into());
        line(format!("chargeLimit = {}", al.charge_limit));
        line("# Notify when a device is fully charged".into());
        line(format!("chargeComplete = {}", al.charge_complete));
        line("# Notify when a device has been offline for this many hours (0 = off)".into());
        line(format!("offlineHours = {}", al.offline_hours));
        line("# Hold notifications during full screen games and \"Do not disturb\"".into());
        line(format!("quietWhenBusy = {}", al.quiet_when_busy));
        line("# \"Remind me later\" delay in hours".into());
        line(format!("snoozeHours = {}", al.snooze_hours));
        line(String::new());
        line("[History]".into());
        line("# Record the readings (time left estimate, charts, battery wear)".into());
        line(format!("enabled = {}", hi.enabled));
        line(format!("retentionDays = {}", hi.retention_days));
        line(String::new());
        line("[HTTPServer]".into());
        line(format!("enabled = {}", http.enabled));
        line("# \"localhost\", an IP address / host name of this machine, or \"0.0.0.0\" for all interfaces".into());
        line(format!("addr = {}", quote(&http.addr)));
        line(format!("port = {}", http.port));
        line(format!("useIpv6 = {}", http.use_ipv6));
        line("# Token required from other computers: ?token=... or \"Authorization: Bearer ...\" (\"\" = none)".into());
        line(format!("token = {}", quote(&http.token)));
        line("# Other Batly instances shown here too, e.g. [\"http://living-room-pc:12321\"]".into());
        line("# (append \"#token\" to the URL when that instance requires one)".into());
        line(format!("peers = {}", array(&http.peers)));
        line(String::new());
        line("[GHub]".into());
        line("# Read battery levels from Logitech G HUB (if it is running)".into());
        line(format!("enabled = {}", self.ghub.enabled));
        line(String::new());
        line("[Native]".into());
        line("# Talk to the devices directly (HID++)".into());
        line(format!("enabled = {}", self.native.enabled));
        line(String::new());
        line("# Time in seconds to retry a device poll on failure".into());
        line(format!("retryTime = {}", self.native.retry_time));
        line(String::new());
        line("# Time in seconds to next device poll on success. Keep it high: frequent".into());
        line("# polling can interfere with the power saving modes of the devices.".into());
        line(format!("pollPeriod = {}", self.native.poll_period));
        line(String::new());
        line("# Devices whose name contains one of these strings are never polled,".into());
        line("# e.g. [\"G403\", \"G502\"]".into());
        line(format!("disabledDevices = {}", array(&self.native.disabled_devices)));
        line(String::new());
        line("[OtherDevices]".into());
        line("# Any Bluetooth device that reports its battery to Windows (headphones, earbuds…)".into());
        line(format!("bluetooth = {}", self.other.bluetooth));
        line("# Xbox and PlayStation controllers".into());
        line(format!("gamepads = {}", self.other.gamepads));
        line("# Wireless mice, keyboards and headsets of other brands".into());
        line(format!("razer = {}", self.other.razer));
        line(format!("corsair = {}", self.other.corsair));
        line(format!("steelseries = {}", self.other.steelseries));
        line(String::new());
        line("[MQTT]".into());
        line("# Publish the levels to an MQTT broker (Home Assistant discovery included)".into());
        line(format!("enabled = {}", mq.enabled));
        line(format!("host = {}", quote(&mq.host)));
        line(format!("port = {}", mq.port));
        line(format!("username = {}", quote(&mq.username)));
        line(format!("password = {}", quote(&mq.password)));
        line(format!("baseTopic = {}", quote(&mq.base_topic)));
        line(format!("discovery = {}", mq.discovery));
        line(format!("discoveryPrefix = {}", quote(&mq.discovery_prefix)));
        line(String::new());
        line("[Webhooks]".into());
        line("# URLs that receive a JSON POST on every alert (Discord / Slack webhooks get a message)".into());
        line(format!("urls = {}", array(&wh.urls)));
        line("# Also send every level change".into());
        line(format!("allChanges = {}", wh.all_changes));
        line(String::new());
        line("[Updates]".into());
        line("# Look for a new version on GitHub once a day".into());
        line(format!("check = {}", self.updates.check));
        line(format!("skipped = {}", quote(&self.updates.skipped)));
        for (id, d) in &self.devices {
            line(String::new());
            line(format!("[Devices.{}]", quote(id)));
            line(format!("alias = {}", quote(&d.alias)));
            line(format!("icon = {}", quote(if d.icon.is_empty() { "auto" } else { &d.icon })));
            if let Some(t) = d.low_battery_threshold {
                line(format!("lowBatteryThreshold = {t}"));
            }
            line(format!("muted = {}", d.muted));
        }
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
/// (portable mode), otherwise in `%APPDATA%\Batly`.
pub fn locate(exe_dir: &Path, appdata: Option<PathBuf>) -> PathBuf {
    let portable = exe_dir.join(FILE_NAME);
    match appdata {
        Some(dir) if !portable.exists() => dir.join("Batly").join(FILE_NAME),
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
        cfg.ui.single_icon = true;
        cfg.ui.hotkey = "Win+Shift+F9".into();
        cfg.alerts.charge_limit = 80;
        cfg.http.token = "s3cret".into();
        cfg.http.peers = vec!["http://pc:12321#tok".into()];
        cfg.mqtt.enabled = true;
        cfg.mqtt.password = "p\"w".into();
        cfg.webhooks.urls = vec!["https://discord.com/api/webhooks/1/x".into()];
        cfg.device_mut("dev 1").alias = "Souris bureau".into();
        cfg.device_mut("dev 1").low_battery_threshold = Some(30);
        cfg.device_mut("A.B").icon = "gamepad".into();
        cfg.device_mut("muted").muted = true;
        assert_eq!(cfg.display_name("dev 1", "G502"), "Souris bureau");
        assert_eq!(cfg.display_name("other", "G502"), "G502");
        assert_eq!((cfg.threshold("dev 1"), cfg.threshold("muted"), cfg.threshold("x")), (30, 0, 20));
        assert_eq!(Config::parse(&cfg.to_toml()).unwrap(), cfg);
        assert_eq!(Config::parse(&Config::default().to_toml()).unwrap(), Config::default());
    }

    #[test]
    fn empty_and_invalid_input() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert!(Config::parse("[Native]\npollPeriod = \"soon\"").is_err());
        let cfg = Config::parse("[Native]\npollPeriod = 0\nretryTime = 0").unwrap();
        assert_eq!((cfg.native.poll_period, cfg.native.retry_time), (5, 1));
        // Entries holding only defaults are dropped.
        let cfg = Config::parse("[Devices.x]\nalias = \"\"\n[Devices.y]\nalias = \"Y\"").unwrap();
        assert_eq!(cfg.devices.keys().collect::<Vec<_>>(), ["y"]);
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("batly-test-{}", std::process::id()));
        let path = dir.join("sub").join(FILE_NAME);
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(path.exists(), "defaults are written on first load");
        let mut cfg2 = cfg.clone();
        cfg2.ui.selected_devices.push("x".into());
        cfg2.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg2);
        assert_eq!(locate(path.parent().unwrap(), Some(dir.clone())), path);
        assert_eq!(locate(&dir, Some(dir.clone())), dir.join("Batly").join(FILE_NAME));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
