//! User interface strings (English / French).

use crate::model::{Device, Status};
use std::sync::atomic::{AtomicBool, Ordering};

static FRENCH: AtomicBool = AtomicBool::new(false);

/// `setting` is "auto", "en" or "fr"; `system_is_french` is used for "auto".
pub fn init(setting: &str, system_is_french: bool) {
    let fr = match setting.to_ascii_lowercase().as_str() {
        "fr" => true,
        "en" => false,
        _ => system_is_french,
    };
    FRENCH.store(fr, Ordering::Relaxed);
}

fn fr() -> bool {
    FRENCH.load(Ordering::Relaxed)
}

macro_rules! strings {
    ($($name:ident => $en:expr, $fr:expr;)*) => {
        $(pub fn $name() -> &'static str { if fr() { $fr } else { $en } })*
    };
}

strings! {
    no_device => "No device found", "Aucun périphérique détecté";
    searching => "LGSTray – looking for devices…", "LGSTray – recherche de périphériques…";
    numeric_display => "Show level as a number", "Afficher le niveau en chiffres";
    low_battery_alerts => "Low battery notifications", "Notifications de batterie faible";
    autostart => "Start with Windows", "Lancer au démarrage de Windows";
    refresh => "Refresh now", "Actualiser maintenant";
    open_settings => "Open settings file", "Ouvrir le fichier de réglages";
    open_web => "Open web page", "Ouvrir la page web";
    quit => "Quit", "Quitter";
    charging => "charging", "en charge";
    full => "charged", "chargé";
    offline => "offline", "hors ligne";
    unknown_level => "level unknown", "niveau inconnu";
    no_battery => "no battery", "sans batterie";
    low_battery_title => "Low battery", "Batterie faible";
    settings_invalid_title => "LGSTray – invalid settings", "LGSTray – réglages invalides";
    uninstall_title => "Uninstall LGSTray", "Désinstaller LGSTray";
    uninstall_confirm => "Remove LGSTray from this computer?", "Supprimer LGSTray de cet ordinateur ?";
    uninstall_settings => "Also delete your settings?", "Supprimer aussi vos réglages ?";
    uninstall_done => "LGSTray has been removed.", "LGSTray a été désinstallé.";
}

pub fn settings_invalid(path: &str, error: &str) -> String {
    if fr() {
        format!("Le fichier de réglages est invalide :\n{path}\n\n{error}\n\nLe remplacer par les réglages par défaut ? (une copie .bak est conservée)")
    } else {
        format!("The settings file is invalid:\n{path}\n\n{error}\n\nReplace it with the default settings? (a .bak copy is kept)")
    }
}

pub fn low_battery_text(name: &str, percent: f32) -> String {
    if fr() {
        format!("{name} : {percent:.0} % restants")
    } else {
        format!("{name}: {percent:.0}% remaining")
    }
}

fn percent(p: f32) -> String {
    if fr() {
        format!("{p:.0} %")
    } else {
        format!("{p:.0}%")
    }
}

/// Short state used in the menu and tooltips, e.g. `85% · charging`.
pub fn describe(d: &Device) -> String {
    if !d.has_battery {
        return no_battery().into();
    }
    let Some(b) = d.battery else {
        return if d.online { unknown_level().into() } else { offline().into() };
    };
    let mut s = percent(b.percent);
    match b.status {
        Status::Charging => s = format!("{s} · {}", charging()),
        Status::Full => s = format!("{s} · {}", full()),
        _ => {}
    }
    if !d.online {
        s = format!("{s} · {}", offline());
    }
    s
}

/// Tooltip of a device icon (Windows limits it to 127 characters).
pub fn tooltip(d: &Device) -> String {
    let mut s = format!("{}\n{}", d.name, describe(d));
    if let Some(b) = d.battery {
        if let Some(mv) = b.millivolts {
            s.push_str(&format!(" · {:.2} V", mv as f32 / 1000.0));
        }
        if let Some(h) = b.mileage.filter(|h| h.is_finite() && *h > 0.0) {
            s.push_str(&format!(" · ~{h:.0} h"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DeviceKind, Source};

    #[test]
    fn descriptions() {
        init("en", true);
        let mut d = Device {
            id: "1".into(),
            name: "G502".into(),
            kind: DeviceKind::Mouse,
            source: Source::Native,
            has_battery: true,
            battery: None,
            online: true,
            last_update: None,
        };
        assert_eq!(describe(&d), "level unknown");
        d.battery = Some(Battery { percent: 84.6, status: Status::Charging, millivolts: Some(4012), mileage: None });
        assert_eq!(describe(&d), "85% · charging");
        assert_eq!(tooltip(&d), "G502\n85% · charging · 4.01 V");
        d.online = false;
        d.battery.as_mut().unwrap().status = Status::Discharging;
        assert_eq!(describe(&d), "85% · offline");
    }
}
