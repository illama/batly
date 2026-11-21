//! Alerts produced by the alert engine (`alerts.rs`) and delivered to the
//! user (tray notification) and to the integrations (webhooks, MQTT).

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum NoticeKind {
    /// Level at or below the device's low battery threshold.
    LowBattery,
    /// Estimated time left below the configured horizon ("charge tonight").
    RunningOut,
    /// Charging device reached the configured charge limit (e.g. 80 %).
    ChargeLimit,
    /// Charging finished.
    Charged,
    /// Offline for longer than the configured delay.
    Offline,
    /// A new Batly version is available.
    Update,
}

impl NoticeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowBattery => "low_battery",
            Self::RunningOut => "running_out",
            Self::ChargeLimit => "charge_limit",
            Self::Charged => "charged",
            Self::Offline => "offline",
            Self::Update => "update",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub kind: NoticeKind,
    /// Empty for notices that are not about a device.
    pub device_id: String,
    /// Display name (alias when set).
    pub device_name: String,
    pub percent: Option<f32>,
    /// Estimated hours left, when known.
    pub hours_left: Option<f64>,
    /// Localised title and text, as shown in the notification.
    pub title: String,
    pub text: String,
}
