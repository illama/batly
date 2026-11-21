//! Alert engine: decides when to notify, independently of how the notice is
//! delivered (tray notification, webhook, MQTT). Pure logic, driven by the
//! device snapshot, the history statistics and the clock.

use crate::config::Config;
use crate::history::Stats;
use crate::i18n::{self, tr};
use crate::model::{Device, Status};
use crate::notice::{Notice, NoticeKind};
use std::collections::HashMap;

#[derive(Default, Debug)]
struct DeviceState {
    low: bool,
    running_out: bool,
    limit: bool,
    charged: bool,
    offline: bool,
    offline_since: Option<u64>,
    last_status: Option<Status>,
}

#[derive(Default)]
pub struct Engine {
    devices: HashMap<String, DeviceState>,
    /// (device id, kind) → unix time until which the alert stays quiet.
    snoozed: HashMap<(String, NoticeKind), u64>,
    /// Notices held back while the user is busy (full screen, presentation).
    held: Vec<Notice>,
}

fn plugged(s: Status) -> bool {
    matches!(s, Status::Charging | Status::Full)
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluates every device; returns the notices to deliver now.
    pub fn evaluate(&mut self, cfg: &Config, devices: &[Device], stats: impl Fn(&str) -> Stats, now: u64) -> Vec<Notice> {
        let mut out = Vec::new();
        self.snoozed.retain(|_, until| *until > now);
        for d in devices.iter().filter(|d| d.has_battery) {
            let name = cfg.display_name(&d.id, &d.name);
            let muted = cfg.device(&d.id).muted;
            let threshold = cfg.threshold(&d.id) as f32;
            let st = self.devices.entry(d.id.clone()).or_default();
            let snoozed = |kind: NoticeKind| self.snoozed.contains_key(&(d.id.clone(), kind));

            // Offline for too long.
            if d.online {
                st.offline_since = None;
                st.offline = false;
            } else {
                let since = *st.offline_since.get_or_insert(now);
                let limit = cfg.alerts.offline_hours as u64 * 3600;
                if limit > 0 && !muted && !st.offline && now.saturating_sub(since) >= limit && !snoozed(NoticeKind::Offline) {
                    st.offline = true;
                    out.push(notice(NoticeKind::Offline, d, &name, None, None));
                }
                continue;
            }

            let Some(b) = d.battery else { continue };
            let previous = st.last_status.replace(b.status);

            if plugged(b.status) {
                st.low = false;
                st.running_out = false;
            } else {
                if b.percent > threshold + 5.0 {
                    st.low = false;
                }
                st.limit = false;
                st.charged = false;
            }

            if muted {
                continue;
            }

            // Low battery.
            if threshold > 0.0 && !plugged(b.status) && b.percent <= threshold && !st.low && !snoozed(NoticeKind::LowBattery) {
                st.low = true;
                out.push(notice(NoticeKind::LowBattery, d, &name, Some(b.percent), None));
            }

            // Running out soon (estimate), before the low threshold is hit.
            let left = stats(&d.id).time_left_hours;
            let below_threshold = threshold > 0.0 && b.percent <= threshold;
            if cfg.alerts.predictive && !plugged(b.status) && !st.running_out && !below_threshold && !snoozed(NoticeKind::RunningOut) {
                if let Some(h) = left.filter(|h| *h <= cfg.alerts.predictive_hours as f64) {
                    st.running_out = true;
                    out.push(notice(NoticeKind::RunningOut, d, &name, Some(b.percent), Some(h)));
                }
            }

            // Charge limit (battery care).
            let limit = cfg.alerts.charge_limit as f32;
            if limit > 0.0
                && limit < 100.0
                && b.status == Status::Charging
                && b.percent >= limit
                && !st.limit
                && !snoozed(NoticeKind::ChargeLimit)
            {
                st.limit = true;
                out.push(notice(NoticeKind::ChargeLimit, d, &name, Some(b.percent), None));
            }

            // Charge complete: Charging → Full (or 100 % while charging).
            let complete = b.status == Status::Full || (b.status == Status::Charging && b.percent >= 100.0);
            if cfg.alerts.charge_complete && complete && !st.charged && previous == Some(Status::Charging) {
                st.charged = true;
                out.push(notice(NoticeKind::Charged, d, &name, Some(b.percent), None));
            } else if complete {
                // Already full when first seen: nothing to announce.
                st.charged = true;
            }
        }
        out
    }

    /// "Remind me later": the same alert comes back after `hours` if the
    /// condition still holds.
    pub fn snooze(&mut self, notice: &Notice, now: u64, hours: u32) {
        self.snoozed.insert((notice.device_id.clone(), notice.kind), now + hours as u64 * 3600);
        if let Some(st) = self.devices.get_mut(&notice.device_id) {
            match notice.kind {
                NoticeKind::LowBattery => st.low = false,
                NoticeKind::RunningOut => st.running_out = false,
                NoticeKind::ChargeLimit => st.limit = false,
                NoticeKind::Offline => st.offline = false,
                NoticeKind::Charged | NoticeKind::Update => {}
            }
        }
    }

    /// Keeps a notice for later (user busy). Only the latest notice per
    /// device and kind is kept.
    pub fn hold(&mut self, n: Notice) {
        self.held.retain(|h| !(h.device_id == n.device_id && h.kind == n.kind));
        self.held.push(n);
    }

    #[cfg(test)]
    pub fn has_held(&self) -> bool {
        !self.held.is_empty()
    }

    pub fn take_held(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.held)
    }
}

fn notice(kind: NoticeKind, d: &Device, name: &str, percent: Option<f32>, hours_left: Option<f64>) -> Notice {
    let p = percent.map(i18n::percent).unwrap_or_default();
    let (title, text) = match kind {
        NoticeKind::LowBattery => (i18n::low_battery_title().to_string(), i18n::low_battery_text(name, percent.unwrap_or(0.0))),
        NoticeKind::RunningOut => {
            let left = i18n::duration(hours_left.unwrap_or(0.0));
            (
                tr([
                    "Charge it soon",
                    "Pensez à la recharger",
                    "Bald aufladen",
                    "Cárgalo pronto",
                    "Ricaricalo presto",
                    "Carregue em breve",
                ])
                .into(),
                tr([
                    format!("{name}: {p}, about {left} left at the usual pace."),
                    format!("{name} : {p}, environ {left} au rythme habituel."),
                    format!("{name}: {p}, bei normaler Nutzung noch etwa {left}."),
                    format!("{name}: {p}, quedan unas {left} al ritmo habitual."),
                    format!("{name}: {p}, circa {left} al ritmo abituale."),
                    format!("{name}: {p}, cerca de {left} no ritmo habitual."),
                ]
                .each_ref())
                .clone(),
            )
        }
        NoticeKind::ChargeLimit => (
            tr([
                "You can unplug it",
                "Vous pouvez la débrancher",
                "Sie können es abstecken",
                "Ya puedes desconectarlo",
                "Puoi scollegarlo",
                "Pode desconectar",
            ])
            .into(),
            tr([
                format!("{name} is at {p}. Stopping here preserves the battery."),
                format!("{name} est à {p}. S'arrêter là préserve la batterie."),
                format!("{name} ist bei {p}. Hier aufzuhören schont den Akku."),
                format!("{name} está al {p}. Parar aquí cuida la batería."),
                format!("{name} è al {p}. Fermarsi qui preserva la batteria."),
                format!("{name} está em {p}. Parar aqui preserva a bateria."),
            ]
            .each_ref())
            .clone(),
        ),
        NoticeKind::Charged => (
            tr(["Fully charged", "Charge terminée", "Vollständig geladen", "Carga completa", "Carica completata", "Carga completa"]).into(),
            tr([
                format!("{name} is fully charged."),
                format!("{name} est complètement chargé."),
                format!("{name} ist vollständig geladen."),
                format!("{name} está completamente cargado."),
                format!("{name} è completamente carico."),
                format!("{name} está totalmente carregado."),
            ]
            .each_ref())
            .clone(),
        ),
        NoticeKind::Offline => (
            tr([
                "Device not seen",
                "Appareil introuvable",
                "Gerät nicht gefunden",
                "Dispositivo no visto",
                "Dispositivo non visto",
                "Dispositivo não encontrado",
            ])
            .into(),
            tr([
                format!("{name} has been offline for a while. Is its battery empty?"),
                format!("{name} est hors ligne depuis un moment. Batterie vide ?"),
                format!("{name} ist seit einer Weile offline. Akku leer?"),
                format!("{name} lleva un rato desconectado. ¿Batería agotada?"),
                format!("{name} è offline da un po'. Batteria scarica?"),
                format!("{name} está desconectado há algum tempo. Bateria vazia?"),
            ]
            .each_ref())
            .clone(),
        ),
        NoticeKind::Update => (String::new(), String::new()),
    };
    Notice { kind, device_id: d.id.clone(), device_name: name.to_string(), percent, hours_left, title, text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DeviceKind, Source};

    fn dev(percent: f32, status: Status, online: bool) -> Device {
        Device {
            id: "m".into(),
            name: "MX".into(),
            kind: DeviceKind::Mouse,
            source: Source::Native,
            has_battery: true,
            battery: Some(Battery { percent, status, millivolts: None, mileage: None }),
            online,
            last_update: None,
            info: Default::default(),
        }
    }

    fn kinds(v: &[Notice]) -> Vec<NoticeKind> {
        v.iter().map(|n| n.kind).collect()
    }

    fn no_stats(_: &str) -> Stats {
        Stats::default()
    }

    #[test]
    fn low_battery_once_then_rearms() {
        let cfg = Config::default();
        let mut e = Engine::new();
        assert!(e.evaluate(&cfg, &[dev(50.0, Status::Discharging, true)], no_stats, 0).is_empty());
        assert_eq!(kinds(&e.evaluate(&cfg, &[dev(15.0, Status::Discharging, true)], no_stats, 1)), [NoticeKind::LowBattery]);
        assert!(e.evaluate(&cfg, &[dev(12.0, Status::Discharging, true)], no_stats, 2).is_empty());
        // Hysteresis: 18 % is not enough to re-arm, plugging in is.
        assert!(e.evaluate(&cfg, &[dev(18.0, Status::Discharging, true)], no_stats, 3).is_empty());
        assert!(e.evaluate(&cfg, &[dev(14.0, Status::Discharging, true)], no_stats, 4).is_empty());
        e.evaluate(&cfg, &[dev(14.0, Status::Charging, true)], no_stats, 5);
        assert_eq!(kinds(&e.evaluate(&cfg, &[dev(14.0, Status::Discharging, true)], no_stats, 6)), [NoticeKind::LowBattery]);
    }

    #[test]
    fn per_device_threshold_alias_and_mute() {
        let mut cfg = Config::default();
        cfg.device_mut("m").alias = "Souris bureau".into();
        cfg.device_mut("m").low_battery_threshold = Some(40);
        let mut e = Engine::new();
        let n = e.evaluate(&cfg, &[dev(39.0, Status::Discharging, true)], no_stats, 0);
        assert_eq!(n[0].device_name, "Souris bureau");
        assert!(n[0].text.contains("Souris bureau"));
        cfg.device_mut("m").muted = true;
        let mut e = Engine::new();
        assert!(e.evaluate(&cfg, &[dev(5.0, Status::Discharging, true)], no_stats, 0).is_empty());
    }

    #[test]
    fn predictive_before_threshold() {
        let cfg = Config::default();
        let mut e = Engine::new();
        let stats = |_: &str| Stats { time_left_hours: Some(10.0), ..Default::default() };
        let n = e.evaluate(&cfg, &[dev(40.0, Status::Discharging, true)], stats, 0);
        assert_eq!(kinds(&n), [NoticeKind::RunningOut]);
        assert_eq!(n[0].hours_left, Some(10.0));
        assert!(e.evaluate(&cfg, &[dev(38.0, Status::Discharging, true)], stats, 1).is_empty());
        let far = |_: &str| Stats { time_left_hours: Some(100.0), ..Default::default() };
        let mut e = Engine::new();
        assert!(e.evaluate(&cfg, &[dev(40.0, Status::Discharging, true)], far, 0).is_empty());
    }

    #[test]
    fn charging_alerts() {
        let mut cfg = Config::default();
        cfg.alerts.charge_limit = 80;
        let mut e = Engine::new();
        // Already full at start-up: no "charged" notice.
        assert!(e.evaluate(&cfg, &[dev(100.0, Status::Full, true)], no_stats, 0).is_empty());
        e.evaluate(&cfg, &[dev(60.0, Status::Discharging, true)], no_stats, 1);
        assert!(e.evaluate(&cfg, &[dev(70.0, Status::Charging, true)], no_stats, 2).is_empty());
        assert_eq!(kinds(&e.evaluate(&cfg, &[dev(81.0, Status::Charging, true)], no_stats, 3)), [NoticeKind::ChargeLimit]);
        assert!(e.evaluate(&cfg, &[dev(90.0, Status::Charging, true)], no_stats, 4).is_empty());
        assert_eq!(kinds(&e.evaluate(&cfg, &[dev(100.0, Status::Full, true)], no_stats, 5)), [NoticeKind::Charged]);
        assert!(e.evaluate(&cfg, &[dev(100.0, Status::Full, true)], no_stats, 6).is_empty());
    }

    #[test]
    fn offline_and_snooze() {
        let mut cfg = Config::default();
        cfg.alerts.offline_hours = 2;
        let mut e = Engine::new();
        e.evaluate(&cfg, &[dev(50.0, Status::Discharging, false)], no_stats, 0);
        assert!(e.evaluate(&cfg, &[dev(50.0, Status::Discharging, false)], no_stats, 3600).is_empty());
        let n = e.evaluate(&cfg, &[dev(50.0, Status::Discharging, false)], no_stats, 7200);
        assert_eq!(kinds(&n), [NoticeKind::Offline]);
        assert!(e.evaluate(&cfg, &[dev(50.0, Status::Discharging, false)], no_stats, 7300).is_empty());

        // Snooze: quiet for 4 h, then back if still offline.
        e.snooze(&n[0], 7300, 4);
        assert!(e.evaluate(&cfg, &[dev(50.0, Status::Discharging, false)], no_stats, 7400).is_empty());
        let again = e.evaluate(&cfg, &[dev(50.0, Status::Discharging, false)], no_stats, 7300 + 4 * 3600 + 1);
        assert_eq!(kinds(&again), [NoticeKind::Offline]);
    }

    #[test]
    fn held_notices_are_deduplicated() {
        let cfg = Config::default();
        let mut e = Engine::new();
        let n = e.evaluate(&cfg, &[dev(10.0, Status::Discharging, true)], no_stats, 0);
        e.hold(n[0].clone());
        e.hold(n[0].clone());
        assert!(e.has_held());
        assert_eq!(e.take_held().len(), 1);
        assert!(!e.has_held());
    }
}
