//! Settings window: every option of `appsettings.toml`, in pages, drawn
//! with egui in the Batly style (see [`crate::look`]). Saving writes the
//! file; the tray picks the change up like any other edit of the file (hot
//! reload).

use crate::config::Config;
use crate::i18n::{self, tr};
use crate::look;
use crate::win;
use crate::{hotkey, log};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Check,
    Number(i64, i64),
    Text,
    Secret,
    Lines,
    Choice(&'static [(&'static str, &'static str)]),
}

#[derive(Clone, PartialEq, Debug)]
enum Value {
    Bool(bool),
    Num(i64),
    Text(String),
    Lines(Vec<String>),
    Choice(String),
}

/// Whether a field belongs to the global settings or to the selected device.
#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Global,
    Device,
}

struct Field {
    page: usize,
    scope: Scope,
    label: String,
    kind: Kind,
    get: fn(&Config, &str) -> Value,
    set: fn(&mut Config, &str, Value),
}

fn icon_choices() -> &'static [(&'static str, &'static str)] {
    const EN: &[(&str, &str)] = &[
        ("", "Automatic"),
        ("mouse", "Mouse"),
        ("keyboard", "Keyboard"),
        ("headset", "Headset"),
        ("earbuds", "Earbuds"),
        ("gamepad", "Controller"),
        ("other", "Other"),
    ];
    const FR: &[(&str, &str)] = &[
        ("", "Automatique"),
        ("mouse", "Souris"),
        ("keyboard", "Clavier"),
        ("headset", "Casque"),
        ("earbuds", "Écouteurs"),
        ("gamepad", "Manette"),
        ("other", "Autre"),
    ];
    const DE: &[(&str, &str)] = &[
        ("", "Automatisch"),
        ("mouse", "Maus"),
        ("keyboard", "Tastatur"),
        ("headset", "Headset"),
        ("earbuds", "Ohrhörer"),
        ("gamepad", "Controller"),
        ("other", "Andere"),
    ];
    const ES: &[(&str, &str)] = &[
        ("", "Automático"),
        ("mouse", "Ratón"),
        ("keyboard", "Teclado"),
        ("headset", "Auriculares"),
        ("earbuds", "Auriculares in-ear"),
        ("gamepad", "Mando"),
        ("other", "Otro"),
    ];
    const IT: &[(&str, &str)] = &[
        ("", "Automatico"),
        ("mouse", "Mouse"),
        ("keyboard", "Tastiera"),
        ("headset", "Cuffie"),
        ("earbuds", "Auricolari"),
        ("gamepad", "Controller"),
        ("other", "Altro"),
    ];
    const PT: &[(&str, &str)] = &[
        ("", "Automático"),
        ("mouse", "Mouse"),
        ("keyboard", "Teclado"),
        ("headset", "Headset"),
        ("earbuds", "Fones intra"),
        ("gamepad", "Comando"),
        ("other", "Outro"),
    ];
    tr([EN, FR, DE, ES, IT, PT])
}

fn pages() -> Vec<String> {
    vec![
        tr(["General", "Général", "Allgemein", "General", "Generale", "Geral"]).into(),
        tr(["Alerts", "Alertes", "Warnungen", "Alertas", "Avvisi", "Alertas"]).into(),
        tr(["Devices", "Appareils", "Geräte", "Dispositivos", "Dispositivi", "Dispositivos"]).into(),
        tr(["Sources", "Sources", "Quellen", "Fuentes", "Origini", "Fontes"]).into(),
        tr(["Network", "Réseau", "Netzwerk", "Red", "Rete", "Rede"]).into(),
        tr(["Home automation", "Domotique", "Hausautomation", "Domótica", "Domotica", "Domótica"]).into(),
    ]
}

macro_rules! field {
    ($page:expr, $label:expr, $kind:expr, |$c:ident| $get:expr, |$m:ident, $v:ident| $set:expr) => {
        Field { page: $page, scope: Scope::Global, label: $label.into(), kind: $kind, get: |$c, _| $get, set: |$m, _, $v| $set }
    };
}

macro_rules! device_field {
    ($label:expr, $kind:expr, |$c:ident, $id:ident| $get:expr, |$m:ident, $mid:ident, $v:ident| $set:expr) => {
        Field { page: 2, scope: Scope::Device, label: $label.into(), kind: $kind, get: |$c, $id| $get, set: |$m, $mid, $v| $set }
    };
}

fn b(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
}
fn n(v: &Value) -> i64 {
    if let Value::Num(n) = v {
        *n
    } else {
        0
    }
}
fn s(v: Value) -> String {
    match v {
        Value::Text(t) | Value::Choice(t) => t.trim().to_string(),
        _ => String::new(),
    }
}
fn lines(v: Value) -> Vec<String> {
    if let Value::Lines(l) = v {
        l
    } else {
        Vec::new()
    }
}

fn fields() -> Vec<Field> {
    let pct = Kind::Number(0, 100);
    vec![
        // General
        field!(
            0,
            tr(["Language", "Langue", "Sprache", "Idioma", "Lingua", "Idioma"]),
            Kind::Choice(&i18n::LANGUAGES),
            |c| Value::Choice(c.ui.language.clone()),
            |c, v| c.ui.language = s(v)
        ),
        field!(0, i18n::numeric_display(), Kind::Check, |c| Value::Bool(c.ui.numeric_display), |c, v| c.ui.numeric_display = b(&v)),
        field!(
            0,
            tr([
                "One icon for all devices (lowest level)",
                "Une seule icône (le niveau le plus bas)",
                "Ein Symbol für alle (niedrigster Stand)",
                "Un solo icono (nivel más bajo)",
                "Una sola icona (livello più basso)",
                "Um só ícone (nível mais baixo)"
            ]),
            Kind::Check,
            |c| Value::Bool(c.ui.single_icon),
            |c, v| c.ui.single_icon = b(&v)
        ),
        field!(
            0,
            tr([
                "Hide an icon above (%, 0 = never)",
                "Masquer une icône au-dessus de (%, 0 = jamais)",
                "Symbol ausblenden über (%, 0 = nie)",
                "Ocultar un icono por encima de (%, 0 = nunca)",
                "Nascondi un'icona sopra (%, 0 = mai)",
                "Ocultar um ícone acima de (%, 0 = nunca)"
            ]),
            pct,
            |c| Value::Num(c.ui.hide_above as i64),
            |c, v| c.ui.hide_above = n(&v) as u8
        ),
        field!(
            0,
            tr([
                "Panel shortcut (e.g. Ctrl+Alt+B)",
                "Raccourci du panneau (ex. Ctrl+Alt+B)",
                "Tastenkürzel für das Panel (z. B. Strg+Alt+B)",
                "Atajo del panel (p. ej. Ctrl+Alt+B)",
                "Scorciatoia del pannello (es. Ctrl+Alt+B)",
                "Atalho do painel (ex. Ctrl+Alt+B)"
            ]),
            Kind::Text,
            |c| Value::Text(c.ui.hotkey.clone()),
            |c, v| c.ui.hotkey = s(v)
        ),
        field!(
            0,
            tr([
                "Check for updates every day",
                "Rechercher les mises à jour chaque jour",
                "Täglich nach Updates suchen",
                "Buscar actualizaciones cada día",
                "Cerca aggiornamenti ogni giorno",
                "Procurar atualizações todos os dias"
            ]),
            Kind::Check,
            |c| Value::Bool(c.updates.check),
            |c, v| c.updates.check = b(&v)
        ),
        // Alerts
        field!(
            1,
            tr([
                "Low battery level (%, 0 = no alert)",
                "Niveau de batterie faible (%, 0 = aucune alerte)",
                "Niedriger Akkustand (%, 0 = keine Warnung)",
                "Nivel de batería baja (%, 0 = sin aviso)",
                "Livello batteria scarica (%, 0 = nessun avviso)",
                "Nível de bateria fraca (%, 0 = sem alerta)"
            ]),
            pct,
            |c| Value::Num(c.ui.low_battery_threshold as i64),
            |c, v| c.ui.low_battery_threshold = n(&v) as u8
        ),
        field!(
            1,
            tr([
                "Warn before it runs out (estimate)",
                "Prévenir avant la panne (estimation)",
                "Vor dem Leerlaufen warnen (Schätzung)",
                "Avisar antes de que se agote (estimación)",
                "Avvisa prima che si scarichi (stima)",
                "Avisar antes de acabar (estimativa)"
            ]),
            Kind::Check,
            |c| Value::Bool(c.alerts.predictive),
            |c, v| c.alerts.predictive = b(&v)
        ),
        field!(
            1,
            tr([
                "… when less than (hours) is left",
                "… quand il reste moins de (heures)",
                "… wenn weniger als (Stunden) übrig",
                "… cuando queden menos de (horas)",
                "… quando restano meno di (ore)",
                "… quando restar menos de (horas)"
            ]),
            Kind::Number(1, 336),
            |c| Value::Num(c.alerts.predictive_hours as i64),
            |c, v| c.alerts.predictive_hours = n(&v) as u32
        ),
        field!(
            1,
            tr([
                "Charge limit reminder (%, 0 = off)",
                "Rappel de limite de charge (%, 0 = désactivé)",
                "Ladegrenze-Erinnerung (%, 0 = aus)",
                "Aviso de límite de carga (%, 0 = no)",
                "Promemoria limite di carica (%, 0 = no)",
                "Lembrete de limite de carga (%, 0 = não)"
            ]),
            pct,
            |c| Value::Num(c.alerts.charge_limit as i64),
            |c, v| c.alerts.charge_limit = n(&v) as u8
        ),
        field!(
            1,
            tr([
                "Notify when fully charged",
                "Prévenir quand la charge est terminée",
                "Benachrichtigen, wenn voll geladen",
                "Avisar cuando esté cargado",
                "Avvisa a carica completata",
                "Avisar quando estiver carregado"
            ]),
            Kind::Check,
            |c| Value::Bool(c.alerts.charge_complete),
            |c, v| c.alerts.charge_complete = b(&v)
        ),
        field!(
            1,
            tr([
                "Device offline for (hours, 0 = never)",
                "Appareil hors ligne depuis (heures, 0 = jamais)",
                "Gerät offline seit (Stunden, 0 = nie)",
                "Dispositivo desconectado durante (horas, 0 = nunca)",
                "Dispositivo offline da (ore, 0 = mai)",
                "Dispositivo desconectado há (horas, 0 = nunca)"
            ]),
            Kind::Number(0, 720),
            |c| Value::Num(c.alerts.offline_hours as i64),
            |c, v| c.alerts.offline_hours = n(&v) as u32
        ),
        field!(
            1,
            tr([
                "Hold alerts during games and Do not disturb",
                "Retenir les alertes en jeu et en Ne pas déranger",
                "Warnungen bei Spielen und „Nicht stören“ zurückhalten",
                "Retener avisos en juegos y No molestar",
                "Trattieni gli avvisi nei giochi e in Non disturbare",
                "Reter alertas em jogos e Não incomodar"
            ]),
            Kind::Check,
            |c| Value::Bool(c.alerts.quiet_when_busy),
            |c, v| c.alerts.quiet_when_busy = b(&v)
        ),
        field!(
            1,
            tr([
                "\"Later\" means in (hours)",
                "« Plus tard » = dans (heures)",
                "„Später“ bedeutet in (Stunden)",
                "«Más tarde» = dentro de (horas)",
                "«Più tardi» = tra (ore)",
                "«Mais tarde» = daqui a (horas)"
            ]),
            Kind::Number(1, 168),
            |c| Value::Num(c.alerts.snooze_hours as i64),
            |c, v| c.alerts.snooze_hours = n(&v) as u32
        ),
        // Devices (per device, below the list)
        device_field!(
            tr(["Name shown", "Nom affiché", "Angezeigter Name", "Nombre mostrado", "Nome mostrato", "Nome exibido"]),
            Kind::Text,
            |c, id| Value::Text(c.device(id).alias),
            |c, id, v| c.device_mut(id).alias = s(v)
        ),
        device_field!(
            tr(["Icon", "Icône", "Symbol", "Icono", "Icona", "Ícone"]),
            Kind::Choice(icon_choices()),
            |c, id| Value::Choice(c.device(id).icon),
            |c, id, v| c.device_mut(id).icon = s(v)
        ),
        device_field!(
            tr([
                "Show its own tray icon",
                "Afficher sa propre icône",
                "Eigenes Symbol anzeigen",
                "Mostrar su propio icono",
                "Mostra la propria icona",
                "Mostrar o próprio ícone"
            ]),
            Kind::Check,
            |c, id| Value::Bool(c.ui.selected_devices.iter().any(|s| s == id)),
            |c, id, v| {
                c.ui.selected_devices.retain(|s| s != id);
                if b(&v) {
                    c.ui.selected_devices.push(id.to_string());
                    c.ui.first_run_done = true;
                }
            }
        ),
        device_field!(
            tr([
                "Low level for this device (%, -1 = global)",
                "Niveau faible pour cet appareil (%, -1 = global)",
                "Niedriger Stand für dieses Gerät (%, -1 = global)",
                "Nivel bajo para este dispositivo (%, -1 = global)",
                "Livello basso per questo dispositivo (%, -1 = globale)",
                "Nível baixo para este dispositivo (%, -1 = global)"
            ]),
            Kind::Number(-1, 100),
            |c, id| Value::Num(c.device(id).low_battery_threshold.map_or(-1, i64::from)),
            |c, id, v| c.device_mut(id).low_battery_threshold = u8::try_from(n(&v)).ok()
        ),
        device_field!(
            tr([
                "No alert for this device",
                "Aucune alerte pour cet appareil",
                "Keine Warnung für dieses Gerät",
                "Sin avisos para este dispositivo",
                "Nessun avviso per questo dispositivo",
                "Sem alertas para este dispositivo"
            ]),
            Kind::Check,
            |c, id| Value::Bool(c.device(id).muted),
            |c, id, v| c.device_mut(id).muted = b(&v)
        ),
        // Sources
        field!(
            3,
            tr([
                "Logitech devices (direct, HID++)",
                "Appareils Logitech (direct, HID++)",
                "Logitech-Geräte (direkt, HID++)",
                "Dispositivos Logitech (directo, HID++)",
                "Dispositivi Logitech (diretto, HID++)",
                "Dispositivos Logitech (direto, HID++)"
            ]),
            Kind::Check,
            |c| Value::Bool(c.native.enabled),
            |c, v| c.native.enabled = b(&v)
        ),
        field!(
            3,
            tr(["Logitech G HUB", "Logitech G HUB", "Logitech G HUB", "Logitech G HUB", "Logitech G HUB", "Logitech G HUB"]),
            Kind::Check,
            |c| Value::Bool(c.ghub.enabled),
            |c, v| c.ghub.enabled = b(&v)
        ),
        field!(
            3,
            tr([
                "Bluetooth devices (headphones, earbuds…)",
                "Appareils Bluetooth (casques, écouteurs…)",
                "Bluetooth-Geräte (Kopfhörer, Ohrhörer…)",
                "Dispositivos Bluetooth (auriculares…)",
                "Dispositivi Bluetooth (cuffie, auricolari…)",
                "Dispositivos Bluetooth (fones…)"
            ]),
            Kind::Check,
            |c| Value::Bool(c.other.bluetooth),
            |c, v| c.other.bluetooth = b(&v)
        ),
        field!(
            3,
            tr([
                "Xbox and PlayStation controllers",
                "Manettes Xbox et PlayStation",
                "Xbox- und PlayStation-Controller",
                "Mandos Xbox y PlayStation",
                "Controller Xbox e PlayStation",
                "Comandos Xbox e PlayStation"
            ]),
            Kind::Check,
            |c| Value::Bool(c.other.gamepads),
            |c, v| c.other.gamepads = b(&v)
        ),
        field!(3, "Razer", Kind::Check, |c| Value::Bool(c.other.razer), |c, v| c.other.razer = b(&v)),
        field!(3, "Corsair", Kind::Check, |c| Value::Bool(c.other.corsair), |c, v| c.other.corsair = b(&v)),
        field!(3, "SteelSeries", Kind::Check, |c| Value::Bool(c.other.steelseries), |c, v| c.other.steelseries = b(&v)),
        field!(
            3,
            tr([
                "Keep a history (time left, charts, wear)",
                "Garder un historique (autonomie, courbes, usure)",
                "Verlauf speichern (Restzeit, Diagramme, Verschleiß)",
                "Guardar historial (autonomía, gráficos, desgaste)",
                "Conserva la cronologia (autonomia, grafici, usura)",
                "Guardar histórico (autonomia, gráficos, desgaste)"
            ]),
            Kind::Check,
            |c| Value::Bool(c.history.enabled),
            |c, v| c.history.enabled = b(&v)
        ),
        field!(
            3,
            tr([
                "Keep it for (days)",
                "Le conserver (jours)",
                "Aufbewahren (Tage)",
                "Conservarlo (días)",
                "Conservala (giorni)",
                "Guardar por (dias)"
            ]),
            Kind::Number(7, 3650),
            |c| Value::Num(c.history.retention_days as i64),
            |c, v| c.history.retention_days = n(&v) as u32
        ),
        // Network
        field!(
            4,
            tr(["Web page and API", "Page web et API", "Webseite und API", "Página web y API", "Pagina web e API", "Página web e API"]),
            Kind::Check,
            |c| Value::Bool(c.http.enabled),
            |c, v| c.http.enabled = b(&v)
        ),
        field!(
            4,
            tr([
                "Address (localhost, IP or 0.0.0.0)",
                "Adresse (localhost, IP ou 0.0.0.0)",
                "Adresse (localhost, IP oder 0.0.0.0)",
                "Dirección (localhost, IP o 0.0.0.0)",
                "Indirizzo (localhost, IP o 0.0.0.0)",
                "Endereço (localhost, IP ou 0.0.0.0)"
            ]),
            Kind::Text,
            |c| Value::Text(c.http.addr.clone()),
            |c, v| c.http.addr = s(v)
        ),
        field!(4, "Port", Kind::Number(1, 65535), |c| Value::Num(c.http.port as i64), |c, v| c.http.port = n(&v) as u16),
        field!(4, "IPv6", Kind::Check, |c| Value::Bool(c.http.use_ipv6), |c, v| c.http.use_ipv6 = b(&v)),
        field!(
            4,
            tr([
                "Access token for other computers",
                "Jeton d'accès pour les autres PC",
                "Zugriffstoken für andere PCs",
                "Token de acceso para otros PC",
                "Token di accesso per altri PC",
                "Token de acesso para outros PCs"
            ]),
            Kind::Secret,
            |c| Value::Text(c.http.token.clone()),
            |c, v| c.http.token = s(v)
        ),
        field!(
            4,
            tr([
                "Other PCs to show here (one URL per line, url#token)",
                "Autres PC à afficher ici (une URL par ligne, url#jeton)",
                "Andere PCs hier anzeigen (eine URL pro Zeile, url#token)",
                "Otros PC a mostrar (una URL por línea, url#token)",
                "Altri PC da mostrare (un URL per riga, url#token)",
                "Outros PCs a mostrar (um URL por linha, url#token)"
            ]),
            Kind::Lines,
            |c| Value::Lines(c.http.peers.clone()),
            |c, v| c.http.peers = lines(v)
        ),
        // Home automation
        field!(
            5,
            tr([
                "Publish to MQTT (Home Assistant)",
                "Publier en MQTT (Home Assistant)",
                "Per MQTT veröffentlichen (Home Assistant)",
                "Publicar por MQTT (Home Assistant)",
                "Pubblica via MQTT (Home Assistant)",
                "Publicar por MQTT (Home Assistant)"
            ]),
            Kind::Check,
            |c| Value::Bool(c.mqtt.enabled),
            |c, v| c.mqtt.enabled = b(&v)
        ),
        field!(
            5,
            tr(["Broker", "Serveur (broker)", "Broker", "Servidor (broker)", "Broker", "Servidor (broker)"]),
            Kind::Text,
            |c| Value::Text(c.mqtt.host.clone()),
            |c, v| c.mqtt.host = s(v)
        ),
        field!(5, "Port", Kind::Number(1, 65535), |c| Value::Num(c.mqtt.port as i64), |c, v| c.mqtt.port = n(&v) as u16),
        field!(
            5,
            tr(["User", "Utilisateur", "Benutzer", "Usuario", "Utente", "Usuário"]),
            Kind::Text,
            |c| Value::Text(c.mqtt.username.clone()),
            |c, v| c.mqtt.username = s(v)
        ),
        field!(
            5,
            tr(["Password", "Mot de passe", "Passwort", "Contraseña", "Password", "Senha"]),
            Kind::Secret,
            |c| Value::Text(c.mqtt.password.clone()),
            |c, v| c.mqtt.password = s(v)
        ),
        field!(
            5,
            tr(["Base topic", "Sujet de base", "Basis-Topic", "Tema base", "Topic di base", "Tópico base"]),
            Kind::Text,
            |c| Value::Text(c.mqtt.base_topic.clone()),
            |c, v| c.mqtt.base_topic = s(v)
        ),
        field!(
            5,
            tr([
                "Home Assistant discovery",
                "Découverte Home Assistant",
                "Home-Assistant-Erkennung",
                "Descubrimiento de Home Assistant",
                "Rilevamento Home Assistant",
                "Descoberta do Home Assistant"
            ]),
            Kind::Check,
            |c| Value::Bool(c.mqtt.discovery),
            |c, v| c.mqtt.discovery = b(&v)
        ),
        field!(
            5,
            tr([
                "Webhooks (one URL per line; Discord, Slack…)",
                "Webhooks (une URL par ligne ; Discord, Slack…)",
                "Webhooks (eine URL pro Zeile; Discord, Slack…)",
                "Webhooks (una URL por línea; Discord, Slack…)",
                "Webhook (un URL per riga; Discord, Slack…)",
                "Webhooks (um URL por linha; Discord, Slack…)"
            ]),
            Kind::Lines,
            |c| Value::Lines(c.webhooks.urls.clone()),
            |c, v| c.webhooks.urls = lines(v)
        ),
        field!(
            5,
            tr([
                "Send every change, not only alerts",
                "Envoyer chaque changement, pas seulement les alertes",
                "Jede Änderung senden, nicht nur Warnungen",
                "Enviar cada cambio, no solo avisos",
                "Invia ogni modifica, non solo avvisi",
                "Enviar cada alteração, não só alertas"
            ]),
            Kind::Check,
            |c| Value::Bool(c.webhooks.all_changes),
            |c, v| c.webhooks.all_changes = b(&v)
        ),
    ]
}

const CLASS: &str = "BatlySettings";

/// Value being edited in a field.
#[derive(Clone, PartialEq, Debug)]
enum Edit {
    Bool(bool),
    Text(String),
    Choice(usize),
}

fn to_edit(kind: Kind, v: &Value) -> Edit {
    match (kind, v) {
        (Kind::Check, Value::Bool(b)) => Edit::Bool(*b),
        (Kind::Choice(options), Value::Choice(c)) => Edit::Choice(options.iter().position(|(k, _)| k.eq_ignore_ascii_case(c)).unwrap_or(0)),
        (_, Value::Num(n)) => Edit::Text(n.to_string()),
        (_, Value::Text(t)) => Edit::Text(t.clone()),
        (_, Value::Lines(l)) => Edit::Text(l.join("\n")),
        (Kind::Check, _) => Edit::Bool(false),
        (Kind::Choice(_), _) => Edit::Choice(0),
        _ => Edit::Text(String::new()),
    }
}

/// Reads an edited field; `Err` holds a message for an invalid number.
fn from_edit(kind: Kind, e: &Edit, label: &str) -> Result<Value, String> {
    Ok(match (kind, e) {
        (Kind::Check, Edit::Bool(b)) => Value::Bool(*b),
        (Kind::Choice(options), Edit::Choice(i)) => Value::Choice(options.get(*i).map_or("", |o| o.0).to_string()),
        (Kind::Number(min, max), Edit::Text(t)) => {
            let v: i64 = t.trim().parse().map_err(|_| label.to_string())?;
            if v < min || v > max {
                return Err(format!("{label} ({min} – {max})"));
            }
            Value::Num(v)
        }
        (Kind::Lines, Edit::Text(t)) => Value::Lines(t.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()),
        (_, Edit::Text(t)) => Value::Text(t.clone()),
        _ => return Err(label.to_string()),
    })
}

/// A device that can be configured (known now or present in the settings).
#[derive(Clone)]
pub struct KnownDevice {
    pub id: String,
    pub name: String,
}

struct Settings {
    owner: HWND,
    cfg: Config,
    path: PathBuf,
    devices: Vec<KnownDevice>,
    selected: Option<usize>,
    fields: Vec<Field>,
    /// One per field; device fields hold the selected device's values.
    edits: Vec<Edit>,
    autostart: bool,
    page: usize,
    error: Option<String>,
    show_secret: bool,
}

pub fn open(owner: HWND, cfg: &Config, path: PathBuf, mut devices: Vec<KnownDevice>) {
    if crate::egui_host::focus(CLASS) {
        return;
    }
    // Devices only known from the settings file are configurable too.
    for id in cfg.devices.keys().chain(cfg.ui.selected_devices.iter()) {
        if !devices.iter().any(|d| &d.id == id) {
            devices.push(KnownDevice { id: id.clone(), name: id.clone() });
        }
    }
    sort_devices(&mut devices, cfg);
    let fields = fields();
    let edits = fields.iter().map(|f| to_edit(f.kind, &(f.get)(cfg, ""))).collect();
    let mut st = Settings {
        owner,
        cfg: cfg.clone(),
        path,
        devices,
        selected: None,
        fields,
        edits,
        autostart: win::autostart_enabled(),
        page: 0,
        error: None,
        show_secret: false,
    };
    if !st.devices.is_empty() {
        st.select(Some(0));
    }
    let title = format!("Batly – {}", tr(["Settings", "Réglages", "Einstellungen", "Ajustes", "Impostazioni", "Configurações"]));
    crate::egui_host::open(
        crate::egui_host::Options { class: CLASS, title, size: (680.0, 660.0), min_size: (560.0, 460.0), resizable: true },
        Box::new(st),
    );
}

fn sort_devices(devices: &mut [KnownDevice], cfg: &Config) {
    let rank = |id: &str| cfg.ui.device_order.iter().position(|o| o == id).unwrap_or(usize::MAX);
    devices.sort_by(|a, b| rank(&a.id).cmp(&rank(&b.id)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

impl Settings {
    /// Stores the device fields being edited into the working copy.
    fn commit_device(&mut self) -> Result<(), String> {
        let Some(sel) = self.selected else { return Ok(()) };
        let id = self.devices[sel].id.clone();
        for (f, e) in self.fields.iter().zip(&self.edits) {
            if f.scope == Scope::Device {
                let v = from_edit(f.kind, e, &f.label)?;
                (f.set)(&mut self.cfg, &id, v);
            }
        }
        Ok(())
    }

    fn select(&mut self, index: Option<usize>) {
        if let Err(e) = self.commit_device() {
            self.error = Some(e);
            return;
        }
        self.selected = index.filter(|i| *i < self.devices.len());
        if let Some(i) = self.selected {
            let id = self.devices[i].id.clone();
            for (f, e) in self.fields.iter().zip(self.edits.iter_mut()) {
                if f.scope == Scope::Device {
                    *e = to_edit(f.kind, &(f.get)(&self.cfg, &id));
                }
            }
        }
    }

    fn move_device(&mut self, delta: isize) {
        let Some(i) = self.selected else { return };
        let j = i as isize + delta;
        if j < 0 || j as usize >= self.devices.len() {
            return;
        }
        let _ = self.commit_device();
        self.devices.swap(i, j as usize);
        self.selected = Some(j as usize);
        self.cfg.ui.device_order = self.devices.iter().map(|d| d.id.clone()).collect();
    }

    fn save(&mut self) -> bool {
        let mut cfg = self.cfg.clone();
        let mut errors = Vec::new();
        for (f, e) in self.fields.iter().zip(&self.edits) {
            if f.scope != Scope::Global {
                continue;
            }
            match from_edit(f.kind, e, &f.label) {
                Ok(v) => (f.set)(&mut cfg, "", v),
                Err(e) => errors.push(e),
            }
        }
        if let Err(e) = self.commit_device() {
            errors.push(e);
        }
        cfg.devices.clone_from(&self.cfg.devices);
        cfg.ui.selected_devices.clone_from(&self.cfg.ui.selected_devices);
        cfg.ui.device_order.clone_from(&self.cfg.ui.device_order);
        if !cfg.ui.hotkey.trim().is_empty() && hotkey::parse(&cfg.ui.hotkey).is_none() {
            errors.push(format!(
                "{} : {}",
                tr([
                    "Invalid shortcut",
                    "Raccourci invalide",
                    "Ungültiges Tastenkürzel",
                    "Atajo no válido",
                    "Scorciatoia non valida",
                    "Atalho inválido"
                ]),
                cfg.ui.hotkey
            ));
        }
        if !errors.is_empty() {
            let title = tr(["Please check", "À vérifier", "Bitte prüfen", "Revise", "Da verificare", "Verifique"]);
            self.error = Some(format!("{title} : {}", errors.join(" · ")));
            return false;
        }
        // Normalise through the parser (clamps, drops empty device entries).
        let cfg = Config::parse(&cfg.to_toml()).unwrap_or(cfg);
        if self.autostart != win::autostart_enabled() {
            win::set_autostart(self.autostart);
        }
        match cfg.save(&self.path) {
            Ok(()) => {
                // SAFETY: posting to the tray window.
                unsafe { PostMessageW(self.owner, crate::tray::WM_APP_RELOAD, 0, 0) };
                true
            }
            Err(e) => {
                log!("settings: {e}");
                self.error = Some(e.to_string());
                false
            }
        }
    }

    /// The fields of `page` with `scope`, drawn as rows of a card.
    fn field_rows(&mut self, ui: &mut egui::Ui, page: usize, scope: Scope) {
        let indices: Vec<usize> =
            (0..self.fields.len()).filter(|&i| self.fields[i].page == page && self.fields[i].scope == scope).collect();
        let c = look::colours(ui.ctx());
        for (n, &i) in indices.iter().enumerate() {
            if n > 0 {
                look::divider(ui);
            }
            let f = &self.fields[i];
            let label = f.label.clone();
            let kind = f.kind;
            let invalid = from_edit(kind, &self.edits[i], &label).is_err();
            let token_page = page == 4 && matches!(kind, Kind::Secret);
            let show_secret = self.show_secret;
            let mut new_token = false;
            let mut toggle_secret = false;
            match (&mut self.edits[i], kind) {
                (Edit::Bool(b), _) => {
                    look::row(ui, &label, None, |ui| look::switch(ui, b));
                }
                (Edit::Choice(sel), Kind::Choice(options)) => {
                    let names: Vec<&str> = options.iter().map(|o| o.1).collect();
                    look::row(ui, &label, None, |ui| look::choice(ui, ("choice", i), sel, &names, 220.0));
                }
                (Edit::Text(t), Kind::Lines) => {
                    ui.label(&label);
                    ui.add(
                        egui::TextEdit::multiline(t).desired_rows(3).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(10, 8)),
                    );
                }
                (Edit::Text(t), Kind::Number(..)) => {
                    look::row(ui, &label, None, |ui| {
                        let r = look::field(ui, t, 90.0, false);
                        if invalid {
                            ui.painter().rect_stroke(
                                r.rect,
                                egui::CornerRadius::same(8),
                                egui::Stroke::new(1.5, c.bad),
                                egui::StrokeKind::Outside,
                            );
                        }
                    });
                }
                (Edit::Text(t), Kind::Secret) => {
                    look::row(ui, &label, None, |ui| {
                        if token_page
                            && look::button(ui, tr(["New", "Nouveau", "Neu", "Nuevo", "Nuovo", "Novo"]), look::Kind::Secondary).clicked()
                        {
                            new_token = true;
                        }
                        if look::icon_button(
                            ui,
                            if show_secret { '\u{ED1A}' } else { '\u{E7B3}' },
                            tr(["Show", "Afficher", "Anzeigen", "Mostrar", "Mostra", "Mostrar"]),
                        )
                        .clicked()
                        {
                            toggle_secret = true;
                        }
                        look::field(ui, t, 220.0, !show_secret);
                    });
                }
                (Edit::Text(t), _) => {
                    look::row(ui, &label, None, |ui| look::field(ui, t, 240.0, false));
                }
                _ => {}
            }
            if new_token {
                self.edits[i] = Edit::Text(random_token());
                self.show_secret = true;
            }
            if toggle_secret {
                self.show_secret = !self.show_secret;
            }
        }
    }

    fn devices_page(&mut self, ui: &mut egui::Ui) {
        let c = look::colours(ui.ctx());
        look::card(ui, Some(tr(["Devices", "Appareils", "Geräte", "Dispositivos", "Dispositivi", "Dispositivos"])), |ui| {
            if self.devices.is_empty() {
                ui.label(egui::RichText::new(i18n::no_device()).color(c.sub));
                return;
            }
            let mut clicked = None;
            for (i, d) in self.devices.iter().enumerate() {
                let shown = self.cfg.display_name(&d.id, &d.name);
                let active = self.selected == Some(i);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::click());
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                let sel = ui.ctx().animate_bool_with_time(resp.id.with("s"), active, 0.18);
                let fill = look::lerp(look::lerp(egui::Color32::TRANSPARENT, c.hover, hover), look::lerp(c.card, c.accent, 0.14), sel);
                ui.painter().rect_filled(rect, egui::CornerRadius::same(8), fill);
                if sel > 0.0 {
                    let h = 16.0 * sel;
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(egui::pos2(rect.left() + 4.0, rect.center().y), egui::vec2(3.0, h)),
                        egui::CornerRadius::same(2),
                        c.accent,
                    );
                }
                let font = if active { look::strong(14.0) } else { egui::FontId::proportional(14.0) };
                ui.painter().text(egui::pos2(rect.left() + 16.0, rect.center().y), egui::Align2::LEFT_CENTER, &shown, font, c.fg);
                if shown != d.name {
                    ui.painter().text(
                        egui::pos2(rect.right() - 12.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        &d.name,
                        egui::FontId::proportional(12.0),
                        c.sub,
                    );
                }
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
            if let Some(i) = clicked {
                self.select(Some(i));
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let sel = self.selected;
                ui.add_enabled_ui(sel.is_some_and(|i| i > 0), |ui| {
                    if look::button(ui, tr(["Move up", "Monter", "Nach oben", "Subir", "Su", "Subir"]), look::Kind::Secondary).clicked() {
                        self.move_device(-1);
                    }
                });
                ui.add_enabled_ui(sel.is_some_and(|i| i + 1 < self.devices.len()), |ui| {
                    if look::button(ui, tr(["Move down", "Descendre", "Nach unten", "Bajar", "Giù", "Descer"]), look::Kind::Secondary)
                        .clicked()
                    {
                        self.move_device(1);
                    }
                });
            });
        });
        ui.add_space(8.0);
        if let Some(i) = self.selected {
            let name = self.cfg.display_name(&self.devices[i].id, &self.devices[i].name);
            look::page_in(ui, ("device", i), |ui| {
                look::card(ui, Some(&name), |ui| self.field_rows(ui, 2, Scope::Device));
            });
        }
    }
}

impl crate::egui_host::View for Settings {
    fn ui(&mut self, ui: &mut egui::Ui, ctl: &mut crate::egui_host::Ctl) {
        let c = look::colours(ui.ctx());
        let pages = pages();
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ui.ctx()) {
            ctl.close = true;
        }
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) && self.save() {
            ctl.close = true;
        }
        let footer = 68.0;
        egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 18, bottom: 0 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                look::heading(ui, tr(["Settings", "Réglages", "Einstellungen", "Ajustes", "Impostazioni", "Configurações"]), None);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("Batly {}", crate::http::VERSION)).small().color(c.sub));
                });
            });
            let before = self.page;
            if look::tabs(ui, &mut self.page, &pages) {
                look::reset_page(ui.ctx(), ("settings-page", self.page));
                let _ = before;
            }
        });
        let height = (ui.available_height() - footer).max(80.0);
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 10, bottom: 16 }).show(ui, |ui| {
                let page = self.page;
                look::page_in(ui, ("settings-page", page), |ui| {
                    if page == 2 {
                        self.devices_page(ui);
                    } else {
                        look::card(ui, None, |ui| {
                            self.field_rows(ui, page, Scope::Global);
                            if page == 0 {
                                look::divider(ui);
                                look::row(ui, i18n::autostart(), None, |ui| look::switch(ui, &mut self.autostart));
                            }
                        });
                    }
                });
            });
        });
        // Footer.
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, c.border));
        egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 16, bottom: 16 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                if look::button(
                    ui,
                    tr([
                        "Edit the file…",
                        "Modifier le fichier…",
                        "Datei bearbeiten…",
                        "Editar el archivo…",
                        "Modifica il file…",
                        "Editar o arquivo…",
                    ]),
                    look::Kind::Ghost,
                )
                .clicked()
                {
                    win::shell_open(ctl.hwnd, "notepad.exe", Some(&format!("\"{}\"", self.path.display())));
                    ctl.close = true;
                }
                if let Some(e) = &self.error {
                    ui.label(egui::RichText::new(e).color(c.bad).small());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if look::button(ui, tr(["Save", "Enregistrer", "Speichern", "Guardar", "Salva", "Salvar"]), look::Kind::Primary)
                        .clicked()
                        && self.save()
                    {
                        ctl.close = true;
                    }
                    if look::button(ui, tr(["Cancel", "Annuler", "Abbrechen", "Cancelar", "Annulla", "Cancelar"]), look::Kind::Secondary)
                        .clicked()
                    {
                        ctl.close = true;
                    }
                });
            });
        });
    }
}

fn random_token() -> String {
    // std's RandomState is seeded from the OS random generator.
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for _ in 0..4 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        out.push_str(&format!("{:08x}", h.finish() as u32));
    }
    out
}
