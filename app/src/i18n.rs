//! User interface strings: English, French, German, Spanish, Italian and
//! Portuguese.
//!
//! Fixed strings are declared with `strings!`; modules that need their own
//! strings call `tr(["en", "fr", "de", "es", "it", "pt"])`.

use crate::model::{Device, Status};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En = 0,
    Fr = 1,
    De = 2,
    Es = 3,
    It = 4,
    Pt = 5,
}

pub const LANGUAGES: [(&str, &str); 7] = [
    ("auto", "Auto"),
    ("en", "English"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("pt", "Português"),
];

static LANG: AtomicU8 = AtomicU8::new(0);

impl Lang {
    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim().to_ascii_lowercase().get(..2)? {
            "en" => Some(Self::En),
            "fr" => Some(Self::Fr),
            "de" => Some(Self::De),
            "es" => Some(Self::Es),
            "it" => Some(Self::It),
            "pt" => Some(Self::Pt),
            _ => None,
        }
    }

    /// Windows primary language id (LANGID & 0x3FF).
    pub fn from_langid(primary: u16) -> Self {
        match primary {
            0x0C => Self::Fr,
            0x07 => Self::De,
            0x0A => Self::Es,
            0x10 => Self::It,
            0x16 => Self::Pt,
            _ => Self::En,
        }
    }

    pub fn code(self) -> &'static str {
        ["en", "fr", "de", "es", "it", "pt"][self as usize]
    }
}

/// `setting` is "auto" or a language code; `system` is used for "auto".
pub fn init(setting: &str, system: Lang) {
    let lang = Lang::from_code(setting).unwrap_or(system);
    LANG.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::Fr,
        2 => Lang::De,
        3 => Lang::Es,
        4 => Lang::It,
        5 => Lang::Pt,
        _ => Lang::En,
    }
}

/// Picks the translation for the current language: [en, fr, de, es, it, pt].
pub fn tr<T: Copy>(t: [T; 6]) -> T {
    t[lang() as usize]
}

macro_rules! strings {
    ($($name:ident => $en:expr, $fr:expr, $de:expr, $es:expr, $it:expr, $pt:expr;)*) => {
        $(pub fn $name() -> &'static str { tr([$en, $fr, $de, $es, $it, $pt]) })*
    };
}

strings! {
    no_device => "No device found", "Aucun périphérique détecté", "Kein Gerät gefunden", "Ningún dispositivo encontrado", "Nessun dispositivo trovato", "Nenhum dispositivo encontrado";
    searching => "Batly – looking for devices…", "Batly – recherche de périphériques…", "Batly – Geräte werden gesucht…", "Batly – buscando dispositivos…", "Batly – ricerca dispositivi…", "Batly – procurando dispositivos…";
    numeric_display => "Show level as a number", "Afficher le niveau en chiffres", "Ladestand als Zahl anzeigen", "Mostrar el nivel como número", "Mostra il livello come numero", "Mostrar o nível como número";
    low_battery_alerts => "Low battery notifications", "Notifications de batterie faible", "Benachrichtigung bei niedrigem Akku", "Avisos de batería baja", "Notifiche di batteria scarica", "Avisos de bateria fraca";
    autostart => "Start with Windows", "Lancer au démarrage de Windows", "Mit Windows starten", "Iniciar con Windows", "Avvia con Windows", "Iniciar com o Windows";
    refresh => "Refresh now", "Actualiser maintenant", "Jetzt aktualisieren", "Actualizar ahora", "Aggiorna ora", "Atualizar agora";
    open_settings => "Open settings file", "Ouvrir le fichier de réglages", "Einstellungsdatei öffnen", "Abrir el archivo de ajustes", "Apri il file delle impostazioni", "Abrir o arquivo de configurações";
    open_web => "Open web page", "Ouvrir la page web", "Webseite öffnen", "Abrir la página web", "Apri la pagina web", "Abrir a página web";
    quit => "Quit", "Quitter", "Beenden", "Salir", "Esci", "Sair";
    charging => "charging", "en charge", "lädt", "cargando", "in carica", "carregando";
    full => "charged", "chargé", "geladen", "cargado", "carico", "carregado";
    offline => "offline", "hors ligne", "offline", "desconectado", "non connesso", "desconectado";
    unknown_level => "level unknown", "niveau inconnu", "Ladestand unbekannt", "nivel desconocido", "livello sconosciuto", "nível desconhecido";
    no_battery => "no battery", "sans batterie", "kein Akku", "sin batería", "senza batteria", "sem bateria";
    low_battery_title => "Low battery", "Batterie faible", "Akku schwach", "Batería baja", "Batteria scarica", "Bateria fraca";
    settings_invalid_title => "Batly – invalid settings", "Batly – réglages invalides", "Batly – ungültige Einstellungen", "Batly – ajustes no válidos", "Batly – impostazioni non valide", "Batly – configurações inválidas";
    uninstall_title => "Uninstall Batly", "Désinstaller Batly", "Batly deinstallieren", "Desinstalar Batly", "Disinstalla Batly", "Desinstalar o Batly";
    uninstall_confirm => "Remove Batly from this computer?", "Supprimer Batly de cet ordinateur ?", "Batly von diesem Computer entfernen?", "¿Quitar Batly de este equipo?", "Rimuovere Batly da questo computer?", "Remover o Batly deste computador?";
    uninstall_settings => "Also delete your settings?", "Supprimer aussi vos réglages ?", "Auch Ihre Einstellungen löschen?", "¿Borrar también sus ajustes?", "Eliminare anche le impostazioni?", "Apagar também as suas configurações?";
    uninstall_done => "Batly has been removed.", "Batly a été désinstallé.", "Batly wurde entfernt.", "Batly se ha desinstalado.", "Batly è stato rimosso.", "O Batly foi removido.";
}

pub fn settings_invalid(path: &str, error: &str) -> String {
    let (head, question) = tr([
        ("The settings file is invalid:", "Replace it with the default settings? (a .bak copy is kept)"),
        ("Le fichier de réglages est invalide :", "Le remplacer par les réglages par défaut ? (une copie .bak est conservée)"),
        ("Die Einstellungsdatei ist ungültig:", "Durch die Standardeinstellungen ersetzen? (eine .bak-Kopie bleibt erhalten)"),
        ("El archivo de ajustes no es válido:", "¿Sustituirlo por los ajustes predeterminados? (se guarda una copia .bak)"),
        ("Il file delle impostazioni non è valido:", "Sostituirlo con le impostazioni predefinite? (viene conservata una copia .bak)"),
        ("O arquivo de configurações é inválido:", "Substituí-lo pelas configurações padrão? (uma cópia .bak é mantida)"),
    ]);
    format!("{head}\n{path}\n\n{error}\n\n{question}")
}

pub fn low_battery_text(name: &str, percent_left: f32) -> String {
    let p = percent(percent_left);
    match lang() {
        Lang::En => format!("{name}: {p} remaining"),
        Lang::Fr => format!("{name} : {p} restants"),
        Lang::De => format!("{name}: noch {p}"),
        Lang::Es => format!("{name}: queda un {p}"),
        Lang::It => format!("{name}: {p} rimanente"),
        Lang::Pt => format!("{name}: {p} restantes"),
    }
}

/// `62%`, or `62 %` where the language puts a space before the sign.
pub fn percent(p: f32) -> String {
    match lang() {
        Lang::Fr | Lang::De | Lang::Es => format!("{p:.0}\u{a0}%"),
        _ => format!("{p:.0}%"),
    }
}

/// Compact duration: `45 min`, `5 h 20`, `4 d 6 h` (localised units).
pub fn duration(hours: f64) -> String {
    let minutes = (hours * 60.0).round().max(0.0) as u64;
    let day = tr(["d", "j", "T", "d", "g", "d"]);
    let (d, h, m) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if d >= 10 {
        format!("{d}\u{a0}{day}")
    } else if d > 0 {
        format!("{d}\u{a0}{day} {h}\u{a0}h")
    } else if h > 0 {
        if m >= 5 { format!("{h}\u{a0}h\u{a0}{m:02}") } else { format!("{h}\u{a0}h") }
    } else {
        format!("{}\u{a0}min", m.max(1))
    }
}

/// `≈ 4 d 6 h left`
pub fn time_left(hours: f64) -> String {
    let d = duration(hours);
    match lang() {
        Lang::En => format!("≈ {d} left"),
        Lang::Fr => format!("≈ {d} restants"),
        Lang::De => format!("≈ noch {d}"),
        Lang::Es => format!("≈ quedan {d}"),
        Lang::It => format!("≈ {d} rimanenti"),
        Lang::Pt => format!("≈ {d} restantes"),
    }
}

/// `full in ≈ 40 min`
pub fn time_to_full(hours: f64) -> String {
    let d = duration(hours);
    let prefix = tr(["full in", "plein dans", "voll in", "lleno en", "carico tra", "cheio em"]);
    format!("{prefix} ≈ {d}")
}

/// State without the level, for places that show the level next to it:
/// `en charge`, `chargé`, `hors ligne`… (empty while discharging).
pub fn state(d: &Device) -> String {
    if !d.has_battery {
        return no_battery().into();
    }
    let Some(b) = d.battery else {
        return if d.online { unknown_level().into() } else { offline().into() };
    };
    let mut parts = Vec::new();
    match b.status {
        Status::Charging => parts.push(charging()),
        Status::Full => parts.push(full()),
        _ => {}
    }
    if !d.online {
        parts.push(offline());
    }
    parts.join(" · ")
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
/// `estimate` is the time left (discharging) or to full (charging), in hours.
pub fn tooltip(name: &str, d: &Device, estimate: Option<f64>) -> String {
    let mut s = format!("{name}\n{}", describe(d));
    if let Some(b) = d.battery.filter(|_| d.online) {
        let mileage = b.mileage.filter(|h| h.is_finite() && *h > 0.0);
        match (b.status, estimate.or(mileage)) {
            (Status::Charging, Some(h)) => s.push_str(&format!("\n{}", time_to_full(h))),
            (Status::Discharging | Status::Unknown, Some(h)) => s.push_str(&format!("\n{}", time_left(h))),
            _ => {}
        }
        if let Some(mv) = b.millivolts {
            s.push_str(&format!(" · {:.2}\u{a0}V", mv as f32 / 1000.0));
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
        init("en", Lang::Fr);
        let mut d = Device {
            id: "1".into(),
            name: "G502".into(),
            kind: DeviceKind::Mouse,
            source: Source::Native,
            has_battery: true,
            battery: None,
            online: true,
            last_update: None,
            info: Default::default(),
        };
        assert_eq!(describe(&d), "level unknown");
        d.battery = Some(Battery { percent: 84.6, status: Status::Charging, millivolts: Some(4012), mileage: None });
        assert_eq!(describe(&d), "85% · charging");
        assert_eq!(tooltip("G502", &d, Some(0.5)), "G502\n85% · charging\nfull in ≈ 30\u{a0}min · 4.01\u{a0}V");
        d.online = false;
        d.battery.as_mut().unwrap().status = Status::Discharging;
        assert_eq!(describe(&d), "85% · offline");

        init("auto", Lang::Fr);
        d.online = true;
        assert_eq!(tooltip("Souris", &d, Some(102.3)), "Souris\n85\u{a0}%\n≈ 4\u{a0}j 6\u{a0}h restants · 4.01\u{a0}V");
        init("de", Lang::Fr);
        assert_eq!(duration(5.34), "5\u{a0}h\u{a0}20");
        assert_eq!(duration(0.01), "1\u{a0}min");
        assert_eq!(duration(24.0 * 12.0), "12\u{a0}T");
        assert_eq!(Lang::from_code("pt-BR"), Some(Lang::Pt));
        assert_eq!(Lang::from_langid(0x0A), Lang::Es);
        init("en", Lang::Fr);
    }
}
