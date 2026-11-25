//! Settings window: every option of `appsettings.toml` with standard Win32
//! controls, in tabs. Saving writes the file; the tray picks the change up
//! like any other edit of the file (hot reload).

use crate::config::Config;
use crate::i18n::{self, tr};
use crate::win::{self, wide};
use crate::{hotkey, log};
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, COLOR_WINDOW, HFONT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX, NMHDR, TCIF_TEXT, TCITEMW, TCM_GETCURSEL,
    TCM_INSERTITEMW, TCN_SELCHANGE, WC_TABCONTROLW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "BatlySettings";
const ID_SAVE: usize = 1;
const ID_CANCEL: usize = 2;
const ID_RAW: usize = 3;
const ID_TOKEN: usize = 4;
const ID_DEV_LIST: usize = 5;
const ID_DEV_UP: usize = 6;
const ID_DEV_DOWN: usize = 7;
const ID_FIRST_FIELD: usize = 100;

const BS_AUTOCHECKBOX_: u32 = 0x0003;
const BS_PUSHBUTTON_: u32 = 0x0000;
const BS_DEFPUSHBUTTON_: u32 = 0x0001;
const ES_AUTOHSCROLL_: u32 = 0x0080;
const ES_MULTILINE_: u32 = 0x0004;
const ES_WANTRETURN_: u32 = 0x1000;
const ES_NUMBER_: u32 = 0x2000;
const ES_PASSWORD_: u32 = 0x0020;
const CBS_DROPDOWNLIST_: u32 = 0x0003;
const LBS_NOTIFY_: u32 = 0x0001;
const BM_GETCHECK_: u32 = 0x00F0;
const BM_SETCHECK_: u32 = 0x00F1;
const CB_ADDSTRING_: u32 = 0x0143;
const CB_GETCURSEL_: u32 = 0x0147;
const CB_SETCURSEL_: u32 = 0x014E;
const LB_ADDSTRING_: u32 = 0x0180;
const LB_RESETCONTENT_: u32 = 0x0184;
const LB_SETCURSEL_: u32 = 0x0186;
const LB_GETCURSEL_: u32 = 0x0188;
const LBN_SELCHANGE_: u32 = 1;

#[derive(Clone, Copy, PartialEq)]
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

/// A device that can be configured (known now or present in the settings).
#[derive(Clone)]
pub struct KnownDevice {
    pub id: String,
    pub name: String,
}

struct Ctl {
    field: usize,
    hwnd: HWND,
    label: HWND,
}

struct State {
    hwnd: HWND,
    owner: HWND,
    cfg: Config,
    path: PathBuf,
    devices: Vec<KnownDevice>,
    selected: Option<usize>,
    fields: Vec<Field>,
    ctls: Vec<Ctl>,
    /// Controls of each page that are not fields (device list, buttons…).
    extras: Vec<(usize, HWND)>,
    tab: HWND,
    dev_list: HWND,
    font: HFONT,
    autostart: HWND,
    page: usize,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub fn hwnd() -> Option<HWND> {
    STATE.with(|s| s.borrow().as_ref().map(|st| st.hwnd))
}

/// Lets the window handle Tab / Enter / Esc; call from the message loop.
pub fn pre_translate(msg: &MSG) -> bool {
    match hwnd() {
        // SAFETY: valid window and message.
        Some(h) => unsafe { IsDialogMessageW(h, msg) != 0 },
        None => false,
    }
}

pub fn open(owner: HWND, cfg: &Config, path: PathBuf, mut devices: Vec<KnownDevice>) {
    if let Some(h) = hwnd() {
        // SAFETY: our own window.
        unsafe {
            ShowWindow(h, SW_RESTORE);
            SetForegroundWindow(h);
        }
        return;
    }
    // Devices only known from the settings file are configurable too.
    for id in cfg.devices.keys().chain(cfg.ui.selected_devices.iter()) {
        if !devices.iter().any(|d| &d.id == id) {
            devices.push(KnownDevice { id: id.clone(), name: id.clone() });
        }
    }
    sort_devices(&mut devices, cfg);

    // SAFETY: standard window creation; children are created in `build`.
    unsafe {
        let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_TAB_CLASSES | ICC_STANDARD_CLASSES };
        InitCommonControlsEx(&icc);
        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS);
        let mut class: WNDCLASSW = zeroed();
        class.lpfnWndProc = Some(wnd_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        class.hbrBackground = (COLOR_WINDOW + 1) as _;
        class.hIcon = LoadIconW(instance, 1 as _);
        RegisterClassW(&class);
        let title = format!("Batly – {}", tr(["Settings", "Réglages", "Einstellungen", "Ajustes", "Impostazioni", "Configurações"]));
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            wide(&title).as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                owner,
                cfg: cfg.clone(),
                path,
                devices,
                selected: None,
                fields: fields(),
                ctls: Vec::new(),
                extras: Vec::new(),
                tab: null_mut(),
                dev_list: null_mut(),
                font: null_mut(),
                autostart: null_mut(),
                page: 0,
            })
        });
        build(hwnd);
        crate::theme::apply(hwnd);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

fn sort_devices(devices: &mut [KnownDevice], cfg: &Config) {
    let rank = |id: &str| cfg.ui.device_order.iter().position(|o| o == id).unwrap_or(usize::MAX);
    devices.sort_by(|a, b| rank(&a.id).cmp(&rank(&b.id)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

fn message_font(hwnd: HWND) -> HFONT {
    // SAFETY: NONCLIENTMETRICSW is plain data sized correctly.
    unsafe {
        let mut m: NONCLIENTMETRICSW = zeroed();
        m.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, m.cbSize, (&mut m as *mut NONCLIENTMETRICSW).cast(), 0, GetDpiForWindow(hwnd));
        CreateFontIndirectW(&m.lfMessageFont)
    }
}

/// Creates every control. Positions are in logical pixels, scaled.
fn build(hwnd: HWND) {
    let scale = crate::ui::Scale::of(hwnd);
    let px = |v: i32| scale.px(v);
    let font = message_font(hwnd);
    // SAFETY: child controls of our own window; strings outlive the calls.
    unsafe {
        let instance = GetModuleHandleW(null());
        let child = |class: &str, text: &str, style: u32, ex: u32, x: i32, y: i32, w: i32, h: i32, id: usize| -> HWND {
            let c = CreateWindowExW(
                ex,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | style,
                px(x),
                px(y),
                px(w),
                px(h),
                hwnd,
                id as _,
                instance,
                null(),
            );
            SendMessageW(c, WM_SETFONT, font as usize, 1);
            c
        };

        let (width, label_w, ctl_x, ctl_w) = (600, 300, 330, 250);
        let tab = CreateWindowExW(
            0,
            WC_TABCONTROLW,
            null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            px(10),
            px(10),
            px(width - 20),
            px(30),
            hwnd,
            null_mut(),
            instance,
            null(),
        );
        SendMessageW(tab, WM_SETFONT, font as usize, 1);
        for (i, title) in pages().iter().enumerate() {
            let mut w = wide(title);
            let mut item: TCITEMW = zeroed();
            item.mask = TCIF_TEXT;
            item.pszText = w.as_mut_ptr();
            SendMessageW(tab, TCM_INSERTITEMW, i, (&item as *const TCITEMW) as LPARAM);
        }

        let mut ctls = Vec::new();
        let mut extras = Vec::new();
        let mut max_y = 0;
        let (fields_list, dev_count) = STATE.with(|s| {
            let s = s.borrow();
            let st = s.as_ref().unwrap();
            (st.fields.iter().map(|f| (f.page, f.scope, f.label.clone(), f.kind)).collect::<Vec<_>>(), st.devices.len())
        });
        let mut y_by_page = [56; 6];

        // Devices page: list and order buttons above the per-device fields.
        let list = child("LISTBOX", "", WS_TABSTOP | WS_VSCROLL | WS_BORDER | LBS_NOTIFY_, 0, 20, 56, width - 40 - 110, 150, ID_DEV_LIST);
        let up = child(
            "BUTTON",
            tr(["Move up", "Monter", "Nach oben", "Subir", "Su", "Subir"]),
            WS_TABSTOP | BS_PUSHBUTTON_,
            0,
            width - 120,
            56,
            100,
            28,
            ID_DEV_UP,
        );
        let down = child(
            "BUTTON",
            tr(["Move down", "Descendre", "Nach unten", "Bajar", "Giù", "Descer"]),
            WS_TABSTOP | BS_PUSHBUTTON_,
            0,
            width - 120,
            90,
            100,
            28,
            ID_DEV_DOWN,
        );
        extras.extend([(2, list), (2, up), (2, down)]);
        if dev_count == 0 {
            let none = child("STATIC", i18n::no_device(), 0, 0, 20, 216, width - 40, 20, 0);
            extras.push((2, none));
        }
        y_by_page[2] = 220;

        // Start with Windows (registry, not a settings field).
        let autostart = child("BUTTON", i18n::autostart(), WS_TABSTOP | BS_AUTOCHECKBOX_, 0, 20, 0, width - 40, 24, 0);
        extras.push((0, autostart));

        for (i, (page, _scope, label, kind)) in fields_list.iter().enumerate() {
            let y = y_by_page[*page];
            let id = ID_FIRST_FIELD + i;
            let (ctl, lbl, h) = match kind {
                Kind::Check => (child("BUTTON", label, WS_TABSTOP | BS_AUTOCHECKBOX_, 0, 20, y, width - 40, 24, id), null_mut(), 30),
                Kind::Lines => {
                    let l = child("STATIC", label, 0, 0, 20, y, width - 40, 20, 0);
                    let c = child(
                        "EDIT",
                        "",
                        WS_TABSTOP | WS_VSCROLL | ES_MULTILINE_ | ES_WANTRETURN_ | ES_AUTOHSCROLL_,
                        WS_EX_CLIENTEDGE,
                        20,
                        y + 22,
                        width - 40,
                        64,
                        id,
                    );
                    (c, l, 94)
                }
                _ => {
                    let l = child("STATIC", label, 0, 0, 20, y + 4, label_w, 20, 0);
                    let c = match kind {
                        Kind::Choice(options) => {
                            let c = child(
                                "COMBOBOX",
                                "",
                                WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST_ | crate::theme::CBS_THEMED,
                                0,
                                ctl_x,
                                y,
                                ctl_w,
                                200,
                                id,
                            );
                            for (_, name) in options.iter() {
                                SendMessageW(c, CB_ADDSTRING_, 0, wide(name).as_ptr() as LPARAM);
                            }
                            c
                        }
                        Kind::Number(min, _) => {
                            let style = WS_TABSTOP | ES_AUTOHSCROLL_ | if *min >= 0 { ES_NUMBER_ } else { 0 };
                            child("EDIT", "", style, WS_EX_CLIENTEDGE, ctl_x, y, 90, 24, id)
                        }
                        Kind::Secret if *page == 4 => {
                            // Room for the "New" button.
                            let gen = child(
                                "BUTTON",
                                tr(["New", "Nouveau", "Neu", "Nuevo", "Nuovo", "Novo"]),
                                WS_TABSTOP | BS_PUSHBUTTON_,
                                0,
                                ctl_x + ctl_w - 80,
                                y,
                                80,
                                24,
                                ID_TOKEN,
                            );
                            extras.push((4, gen));
                            child("EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL_ | ES_PASSWORD_, WS_EX_CLIENTEDGE, ctl_x, y, ctl_w - 86, 24, id)
                        }
                        Kind::Secret => {
                            child("EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL_ | ES_PASSWORD_, WS_EX_CLIENTEDGE, ctl_x, y, ctl_w, 24, id)
                        }
                        _ => child("EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL_, WS_EX_CLIENTEDGE, ctl_x, y, ctl_w, 24, id),
                    };
                    (c, l, 32)
                }
            };
            ctls.push(Ctl { field: i, hwnd: ctl, label: lbl });
            y_by_page[*page] = y + h;
        }
        // The autostart box goes after the general fields.
        SetWindowPos(autostart, null_mut(), px(20), px(y_by_page[0]), 0, 0, SWP_NOSIZE | SWP_NOZORDER);
        y_by_page[0] += 30;
        for y in y_by_page {
            max_y = max_y.max(y);
        }

        let bottom = max_y + 16;
        let raw = child(
            "BUTTON",
            tr([
                "Edit the file…",
                "Modifier le fichier…",
                "Datei bearbeiten…",
                "Editar el archivo…",
                "Modifica il file…",
                "Editar o arquivo…",
            ]),
            WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON_,
            0,
            20,
            bottom,
            150,
            30,
            ID_RAW,
        );
        let _ = raw;
        child(
            "BUTTON",
            tr(["Save", "Enregistrer", "Speichern", "Guardar", "Salva", "Salvar"]),
            WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON_,
            0,
            width - 240,
            bottom,
            105,
            30,
            ID_SAVE,
        );
        child(
            "BUTTON",
            tr(["Cancel", "Annuler", "Abbrechen", "Cancelar", "Annulla", "Cancelar"]),
            WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON_,
            0,
            width - 125,
            bottom,
            105,
            30,
            ID_CANCEL,
        );

        // Size the window around the client area.
        let mut r = RECT { left: 0, top: 0, right: px(width), bottom: px(bottom + 46) };
        windows_sys::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
            &mut r,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
            WS_EX_CONTROLPARENT,
            GetDpiForWindow(hwnd),
        );
        SetWindowPos(hwnd, null_mut(), 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER);
        SetWindowPos(tab, null_mut(), 0, 0, px(width - 20), px(bottom - 16), SWP_NOMOVE | SWP_NOZORDER);

        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.as_mut().unwrap();
            st.ctls = ctls;
            st.extras = extras;
            st.tab = tab;
            st.dev_list = list;
            st.font = font;
            st.autostart = autostart;
            SendMessageW(autostart, BM_SETCHECK_, win::autostart_enabled() as usize, 0);
            for c in &st.ctls {
                let f = &st.fields[c.field];
                if f.scope == Scope::Global {
                    write_value(c.hwnd, f.kind, &(f.get)(&st.cfg, ""));
                }
            }
            fill_device_list(st);
        });
        if dev_count > 0 {
            select_device(Some(0));
        }
        show_page(0);
    }
}

fn fill_device_list(st: &State) {
    // SAFETY: our own list box.
    unsafe {
        SendMessageW(st.dev_list, LB_RESETCONTENT_, 0, 0);
        for d in &st.devices {
            let shown = st.cfg.display_name(&d.id, &d.name);
            let text = if shown != d.name { format!("{shown}  ({})", d.name) } else { shown };
            SendMessageW(st.dev_list, LB_ADDSTRING_, 0, wide(&text).as_ptr() as LPARAM);
        }
        if let Some(i) = st.selected {
            SendMessageW(st.dev_list, LB_SETCURSEL_, i, 0);
        }
    }
}

fn write_value(h: HWND, kind: Kind, v: &Value) {
    // SAFETY: valid control handles; strings outlive the calls.
    unsafe {
        match (kind, v) {
            (Kind::Check, Value::Bool(b)) => {
                SendMessageW(h, BM_SETCHECK_, *b as usize, 0);
            }
            (Kind::Choice(options), Value::Choice(c)) => {
                let i = options.iter().position(|(k, _)| k.eq_ignore_ascii_case(c)).unwrap_or(0);
                SendMessageW(h, CB_SETCURSEL_, i, 0);
            }
            (_, Value::Num(n)) => {
                SetWindowTextW(h, wide(&n.to_string()).as_ptr());
            }
            (_, Value::Text(t)) => {
                SetWindowTextW(h, wide(t).as_ptr());
            }
            (_, Value::Lines(l)) => {
                SetWindowTextW(h, wide(&l.join("\r\n")).as_ptr());
            }
            _ => {}
        }
    }
}

fn window_text(h: HWND) -> String {
    // SAFETY: buffer sized from GetWindowTextLengthW.
    unsafe {
        let len = GetWindowTextLengthW(h).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

/// Reads a control; `Err` holds a message for an invalid number.
fn read_value(h: HWND, kind: Kind, label: &str) -> Result<Value, String> {
    // SAFETY: valid control handles.
    unsafe {
        Ok(match kind {
            Kind::Check => Value::Bool(SendMessageW(h, BM_GETCHECK_, 0, 0) == 1),
            Kind::Choice(options) => {
                let i = SendMessageW(h, CB_GETCURSEL_, 0, 0).max(0) as usize;
                Value::Choice(options.get(i).map_or("", |o| o.0).to_string())
            }
            Kind::Number(min, max) => {
                let t = window_text(h);
                let v: i64 = t.trim().parse().map_err(|_| label.to_string())?;
                if v < min || v > max {
                    return Err(format!("{label} ({min} – {max})"));
                }
                Value::Num(v)
            }
            Kind::Lines => Value::Lines(window_text(h).lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()),
            Kind::Text | Kind::Secret => Value::Text(window_text(h)),
        })
    }
}

/// Stores the device fields of the selected device into the working copy.
fn commit_device(st: &mut State) -> Result<(), String> {
    let Some(sel) = st.selected else { return Ok(()) };
    let id = st.devices[sel].id.clone();
    for c in &st.ctls {
        let f = &st.fields[c.field];
        if f.scope == Scope::Device {
            let v = read_value(c.hwnd, f.kind, &f.label)?;
            (f.set)(&mut st.cfg, &id, v);
        }
    }
    Ok(())
}

fn select_device(index: Option<usize>) {
    let page = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        if let Err(e) = commit_device(st) {
            win::message_box("Batly", &e);
        }
        st.selected = index.filter(|i| *i < st.devices.len());
        let id = st.selected.map(|i| st.devices[i].id.clone());
        for c in &st.ctls {
            let f = &st.fields[c.field];
            if f.scope == Scope::Device {
                if let Some(id) = &id {
                    write_value(c.hwnd, f.kind, &(f.get)(&st.cfg, id));
                }
                // SAFETY: our own controls.
                unsafe {
                    EnableWindow(c.hwnd, id.is_some() as i32);
                }
            }
        }
        // SAFETY: our own list box.
        unsafe { SendMessageW(st.dev_list, LB_SETCURSEL_, st.selected.unwrap_or(usize::MAX), 0) };
        Some(st.page)
    });
    let _ = page;
}

fn move_device(delta: isize) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return };
        let Some(i) = st.selected else { return };
        let j = i as isize + delta;
        if j < 0 || j as usize >= st.devices.len() {
            return;
        }
        let _ = commit_device(st);
        st.devices.swap(i, j as usize);
        st.selected = Some(j as usize);
        st.cfg.ui.device_order = st.devices.iter().map(|d| d.id.clone()).collect();
        fill_device_list(st);
    });
}

fn show_page(page: usize) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return };
        st.page = page;
        // SAFETY: our own controls.
        unsafe {
            for c in &st.ctls {
                let visible = st.fields[c.field].page == page;
                let cmd = if visible { SW_SHOW } else { SW_HIDE };
                ShowWindow(c.hwnd, cmd);
                if !c.label.is_null() {
                    ShowWindow(c.label, cmd);
                }
            }
            for (p, h) in &st.extras {
                ShowWindow(*h, if *p == page { SW_SHOW } else { SW_HIDE });
            }
        }
    });
}

fn save() -> bool {
    let result = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        let mut cfg = st.cfg.clone();
        let mut errors = Vec::new();
        for c in &st.ctls {
            let f = &st.fields[c.field];
            if f.scope != Scope::Global {
                continue;
            }
            match read_value(c.hwnd, f.kind, &f.label) {
                Ok(v) => (f.set)(&mut cfg, "", v),
                Err(e) => errors.push(e),
            }
        }
        st.cfg.clone_from(&cfg);
        if let Err(e) = commit_device(st) {
            errors.push(e);
        }
        cfg.devices.clone_from(&st.cfg.devices);
        cfg.ui.selected_devices.clone_from(&st.cfg.ui.selected_devices);
        cfg.ui.device_order.clone_from(&st.cfg.ui.device_order);
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
            return Some(Err(format!("{title}:\n\n{}", errors.join("\n"))));
        }
        // Normalise through the parser (clamps, drops empty device entries).
        let cfg = Config::parse(&cfg.to_toml()).unwrap_or(cfg);
        // SAFETY: our own check box.
        let autostart = unsafe { SendMessageW(st.autostart, BM_GETCHECK_, 0, 0) == 1 };
        if autostart != win::autostart_enabled() {
            win::set_autostart(autostart);
        }
        Some(cfg.save(&st.path).map_err(|e| e.to_string()).map(|_| st.owner))
    });
    match result {
        Some(Ok(owner)) => {
            // SAFETY: posting to the tray window.
            unsafe { PostMessageW(owner, crate::tray::WM_APP_RELOAD, 0, 0) };
            true
        }
        Some(Err(e)) => {
            log!("settings: {e}");
            win::message_box("Batly", &e);
            false
        }
        None => false,
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

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Theme: background, labels, check boxes, light/dark switch.
    if let Some(r) = crate::theme::on_message(hwnd, msg, wparam, lparam) {
        return r;
    }
    match msg {
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            let code = (wparam >> 16) as u32;
            match id {
                ID_SAVE => {
                    if save() {
                        DestroyWindow(hwnd);
                    }
                }
                ID_CANCEL => {
                    DestroyWindow(hwnd);
                }
                ID_RAW => {
                    let path = STATE.with(|s| s.borrow().as_ref().map(|st| st.path.clone()));
                    if let Some(p) = path {
                        win::shell_open(hwnd, "notepad.exe", Some(&format!("\"{}\"", p.display())));
                    }
                    DestroyWindow(hwnd);
                }
                ID_TOKEN => {
                    let field = STATE.with(|s| {
                        let s = s.borrow();
                        let st = s.as_ref()?;
                        st.ctls.iter().find(|c| st.fields[c.field].page == 4 && st.fields[c.field].kind == Kind::Secret).map(|c| c.hwnd)
                    });
                    if let Some(h) = field {
                        SetWindowTextW(h, wide(&random_token()).as_ptr());
                    }
                }
                ID_DEV_LIST if code == LBN_SELCHANGE_ => {
                    let list = STATE.with(|s| s.borrow().as_ref().map(|st| st.dev_list));
                    if let Some(l) = list {
                        let i = SendMessageW(l, LB_GETCURSEL_, 0, 0);
                        select_device((i >= 0).then_some(i as usize));
                    }
                }
                ID_DEV_UP => move_device(-1),
                ID_DEV_DOWN => move_device(1),
                _ => {}
            }
            0
        }
        WM_NOTIFY => {
            let hdr = &*(lparam as *const NMHDR);
            if hdr.code == TCN_SELCHANGE {
                let page = SendMessageW(hdr.hwndFrom, TCM_GETCURSEL, 0, 0).max(0) as usize;
                show_page(page);
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            let font = STATE.with(|s| s.borrow_mut().take().map(|st| st.font));
            if let Some(f) = font {
                DeleteObject(f);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
