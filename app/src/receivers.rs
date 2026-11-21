//! "Receivers and devices" window: lists the receivers (and directly
//! connected devices) with their paired devices, pairs / unpairs devices
//! and edits a few hardware settings of the selected device.
//!
//! Standard Win32 controls only (common controls v6 through the manifest),
//! system message font, per-monitor DPI aware. Every HID request runs on
//! the session threads of [`Native`]; answers come back as posted messages
//! carrying a boxed payload, so the UI thread never blocks on a device.
//! The window lives on the UI thread; `open` is called from there.

use crate::hidpp::{
    passkey_clicks, ActionError, BacklightMode, DeviceSettings, PairedDevice, PairingError, PairingEvent, ReceiverInfo, ReceiverKind,
    Setting, WheelMode,
};
use crate::i18n::{lang, tr, Lang};
use crate::model::DeviceKind;
use crate::native::Native;
use crate::win::wide;
use std::cell::{Cell, RefCell};
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::mpsc::Receiver;
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, GetSysColorBrush, COLOR_BTNFACE, HFONT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, LVCOLUMNW,
    LVITEMW, LVN_ITEMCHANGED, NMHDR, NMLISTVIEW,
};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, IsChild, IsDialogMessageW, IsIconic, KillTimer, LoadCursorW, LoadIconW,
    MessageBoxW, PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW,
    SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx, CW_USEDEFAULT, HHOOK, IDC_ARROW, IDYES, MB_ICONWARNING, MB_YESNO, MSG,
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_RESTORE, SW_SHOW, WH_GETMESSAGE, WM_APP,
    WNDCLASSW,
};

const CLASS: &str = "BatlyReceivers";

// Posted messages; `lParam` owns a `Box<Option<T>>` (see `post`).
const WM_APP_LIST: u32 = WM_APP + 40; // Option<Vec<ReceiverInfo>>
const WM_APP_SETTINGS: u32 = WM_APP + 41; // Option<Result<DeviceSettings, ActionError>>, wParam = seq << 1 | is_set
const WM_APP_PAIRING: u32 = WM_APP + 42; // Option<PairingEvent>, None = end of stream
const WM_APP_UNPAIR: u32 = WM_APP + 43; // Option<Result<(), ActionError>>
const WM_APP_CHANGED: u32 = WM_APP + 44; // no payload

const TIMER_REFRESH: usize = 1;

// Window messages, styles and notifications (plain values: the
// windows-sys constants come with a variety of integer types).
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_SETFONT: u32 = 0x0030;
const WM_NOTIFY: u32 = 0x004E;
const WM_KEYFIRST: u32 = 0x0100;
const WM_KEYLAST: u32 = 0x0109;
const WM_COMMAND: u32 = 0x0111;
const WM_TIMER: u32 = 0x0113;
const WM_HSCROLL: u32 = 0x0114;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_NULL: u32 = 0x0000;
const PM_REMOVE: usize = 0x0001;
const IDCANCEL: u16 = 2;

const WS_OVERLAPPED: u32 = 0x0000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
const WS_EX_CONTROLPARENT: u32 = 0x0001_0000;

const BS_PUSHBUTTON: u32 = 0x0;
const BS_GROUPBOX: u32 = 0x7;
const BN_CLICKED: u32 = 0;
const SS_LEFT: u32 = 0x0;
const SS_CENTER: u32 = 0x1;
const SS_NOPREFIX: u32 = 0x80;
const SS_CENTERIMAGE: u32 = 0x200;
const SS_ENDELLIPSIS: u32 = 0x4000;
const CBS_DROPDOWNLIST: u32 = 0x3;
const CBN_SELCHANGE: u32 = 1;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_SETCURSEL: u32 = 0x014E;
const CB_GETITEMDATA: u32 = 0x0150;
const CB_SETITEMDATA: u32 = 0x0151;
const CB_SETMINVISIBLE: u32 = 0x1701;
const TBS_AUTOTICKS: u32 = 0x1;
const TBM_GETPOS: u32 = 0x0400;
const TBM_SETPOS: u32 = 0x0405;
const TBM_SETRANGEMIN: u32 = 0x0407;
const TBM_SETRANGEMAX: u32 = 0x0408;
const TBM_SETPAGESIZE: u32 = 0x0415;
const TBM_SETTICFREQ: u32 = 0x0414;
const TB_ENDTRACK: usize = 8;
const LVS_REPORT: u32 = 0x1;
const LVS_SINGLESEL: u32 = 0x4;
const LVS_SHOWSELALWAYS: u32 = 0x8;
const LVS_NOSORTHEADER: u32 = 0x8000;
const LVS_EX_FULLROWSELECT: usize = 0x20;
const LVS_EX_DOUBLEBUFFER: usize = 0x10000;
const LVM_DELETEALLITEMS: u32 = 0x1009;
const LVM_GETNEXTITEM: u32 = 0x100C;
const LVM_ENSUREVISIBLE: u32 = 0x1013;
const LVM_SETCOLUMNWIDTH: u32 = 0x101E;
const LVM_SETITEMSTATE: u32 = 0x102B;
const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
const LVM_INSERTITEMW: u32 = 0x104D;
const LVM_INSERTCOLUMNW: u32 = 0x1061;
const LVM_SETITEMTEXTW: u32 = 0x1074;
const LVCF_WIDTH: u32 = 0x2;
const LVCF_TEXT: u32 = 0x4;
const LVIF_TEXT: u32 = 0x1;
const LVIF_PARAM: u32 = 0x4;
const LVIF_STATE: u32 = 0x8;
const LVIS_FOCUSED: u32 = 0x1;
const LVIS_SELECTED: u32 = 0x2;
const LVNI_SELECTED: usize = 0x2;

// Control ids.
const ID_RECEIVER: u16 = 100;
const ID_LIST: u16 = 101;
const ID_PAIR: u16 = 102;
const ID_UNPAIR: u16 = 103;
const ID_DPI: u16 = 110;
const ID_RATE: u16 = 111;
const ID_BL_MODE: u16 = 112;
const ID_BL_LEVEL: u16 = 113;
const ID_WHEEL: u16 = 114;
const ID_WHEEL_SPEED: u16 = 115;

/// Client area in pixels at 96 DPI.
const CLIENT: (i32, i32) = (640, 604);
/// List view columns: width at 96 DPI.
const COLUMNS: [i32; 5] = [196, 56, 128, 130, 88];
/// SmartShift speed range shown on the slider (Solaar uses the same).
const WHEEL_MAX: u8 = 50;

// ------------------------------------------------------------ strings

macro_rules! text {
    ($($name:ident => $en:expr, $fr:expr, $de:expr, $es:expr, $it:expr, $pt:expr;)*) => {
        $(fn $name() -> &'static str { tr([$en, $fr, $de, $es, $it, $pt]) })*
    };
}

text! {
    s_title => "Receivers and devices", "Récepteurs et appareils", "Empfänger und Geräte", "Receptores y dispositivos", "Ricevitori e dispositivi", "Receptores e dispositivos";
    s_receiver => "Receiver:", "Récepteur :", "Empfänger:", "Receptor:", "Ricevitore:", "Receptor:";
    s_searching => "Looking for receivers…", "Recherche des récepteurs…", "Empfänger werden gesucht…", "Buscando receptores…", "Ricerca dei ricevitori…", "Procurando receptores…";
    s_none => "No receiver or device found", "Aucun récepteur ni appareil détecté", "Kein Empfänger oder Gerät gefunden", "No se encontró ningún receptor ni dispositivo", "Nessun ricevitore o dispositivo trovato", "Nenhum receptor ou dispositivo encontrado";
    s_col_device => "Device", "Appareil", "Gerät", "Dispositivo", "Dispositivo", "Dispositivo";
    s_col_slot => "Slot", "Empl.", "Platz", "Ranura", "Slot", "Slot";
    s_col_status => "Status", "État", "Status", "Estado", "Stato", "Estado";
    s_col_firmware => "Firmware", "Micrologiciel", "Firmware", "Firmware", "Firmware", "Firmware";
    s_col_channel => "Easy-Switch", "Easy-Switch", "Easy-Switch", "Easy-Switch", "Easy-Switch", "Easy-Switch";
    s_connected => "connected", "connecté", "verbunden", "conectado", "connesso", "conectado";
    s_asleep => "offline / asleep", "hors ligne / en veille", "offline / Ruhezustand", "desconectado / en reposo", "non connesso / in standby", "desconectado / em repouso";
    s_pair => "Pair a new device…", "Appairer un nouvel appareil…", "Neues Gerät koppeln…", "Emparejar un dispositivo nuevo…", "Associa un nuovo dispositivo…", "Emparelhar um novo dispositivo…";
    s_cancel_pair => "Cancel pairing", "Annuler l'appairage", "Kopplung abbrechen", "Cancelar el emparejamiento", "Annulla l'associazione", "Cancelar o emparelhamento";
    s_unpair => "Unpair", "Désappairer", "Entkoppeln", "Desemparejar", "Dissocia", "Desemparelhar";
    s_group => "Settings of the selected device", "Réglages de l'appareil sélectionné", "Einstellungen des ausgewählten Geräts", "Ajustes del dispositivo seleccionado", "Impostazioni del dispositivo selezionato", "Configurações do dispositivo selecionado";
    s_dpi => "Sensitivity (DPI)", "Sensibilité (DPI)", "Empfindlichkeit (DPI)", "Sensibilidad (DPI)", "Sensibilità (DPI)", "Sensibilidade (DPI)";
    s_rate => "Report rate", "Taux de rapport", "Abfragerate", "Frecuencia de sondeo", "Frequenza di polling", "Taxa de relatório";
    s_backlight => "Keyboard backlight", "Rétroéclairage", "Tastaturbeleuchtung", "Retroiluminación", "Retroilluminazione", "Retroiluminação";
    s_wheel => "Scroll wheel", "Molette", "Mausrad", "Rueda de desplazamiento", "Rotellina", "Roda de rolagem";
    s_bl_off => "Off", "Éteint", "Aus", "Apagada", "Spenta", "Desligada";
    s_bl_on => "On", "Allumé", "An", "Encendida", "Accesa", "Ligada";
    s_bl_auto => "Automatic", "Automatique", "Automatisch", "Automática", "Automatica", "Automática";
    s_bl_manual => "Manual level", "Niveau fixe", "Feste Stufe", "Nivel fijo", "Livello fisso", "Nível fixo";
    s_wheel_free => "Free spin", "Roue libre", "Freilauf", "Giro libre", "Rotazione libera", "Rotação livre";
    s_wheel_ratchet => "Ratchet", "Crantée", "Raster", "Por pasos", "A scatti", "Com cliques";
    s_wheel_smart => "SmartShift (automatic)", "SmartShift (automatique)", "SmartShift (automatisch)", "SmartShift (automático)", "SmartShift (automatico)", "SmartShift (automático)";
    s_reading => "Reading the device settings…", "Lecture des réglages de l'appareil…", "Geräteeinstellungen werden gelesen…", "Leyendo los ajustes del dispositivo…", "Lettura delle impostazioni del dispositivo…", "Lendo as configurações do dispositivo…";
    s_applying => "Applying…", "Application du réglage…", "Wird angewendet…", "Aplicando…", "Applicazione in corso…", "Aplicando…";
    s_applied => "Setting applied.", "Réglage appliqué.", "Einstellung übernommen.", "Ajuste aplicado.", "Impostazione applicata.", "Configuração aplicada.";
    s_no_settings => "This device has no adjustable setting.", "Cet appareil n'a aucun réglage modifiable.", "Dieses Gerät hat keine einstellbaren Optionen.", "Este dispositivo no tiene ajustes modificables.", "Questo dispositivo non ha impostazioni regolabili.", "Este dispositivo não tem configurações ajustáveis.";
    s_not_identified => "This device has not been identified yet: wake it up (move it or press a key).", "Cet appareil n'a pas encore été identifié : réveillez-le (bougez-le ou appuyez sur une touche).", "Dieses Gerät wurde noch nicht erkannt: Wecken Sie es auf (bewegen oder eine Taste drücken).", "Este dispositivo aún no se ha identificado: actívelo (muévalo o pulse una tecla).", "Questo dispositivo non è ancora stato identificato: riattivalo (muovilo o premi un tasto).", "Este dispositivo ainda não foi identificado: ative-o (mova-o ou pressione uma tecla).";
    s_err_offline => "The device is offline or asleep: move it or press a key, then select it again.", "L'appareil est hors ligne ou en veille : bougez-le ou appuyez sur une touche, puis sélectionnez-le à nouveau.", "Das Gerät ist offline oder im Ruhezustand: Bewegen Sie es oder drücken Sie eine Taste und wählen Sie es dann erneut aus.", "El dispositivo está desconectado o en reposo: muévalo o pulse una tecla y vuelva a seleccionarlo.", "Il dispositivo non è connesso o è in standby: muovilo o premi un tasto, poi selezionalo di nuovo.", "O dispositivo está desconectado ou em repouso: mova-o ou pressione uma tecla e selecione-o novamente.";
    s_err_unsupported => "Not supported by this device or receiver.", "Non pris en charge par cet appareil ou ce récepteur.", "Von diesem Gerät oder Empfänger nicht unterstützt.", "No compatible con este dispositivo o receptor.", "Non supportato da questo dispositivo o ricevitore.", "Não suportado por este dispositivo ou receptor.";
    s_err_busy => "A pairing is in progress.", "Un appairage est en cours.", "Eine Kopplung läuft gerade.", "Hay un emparejamiento en curso.", "È in corso un'associazione.", "Há um emparelhamento em andamento.";
    s_err_failed => "The device or receiver rejected the request.", "L'appareil ou le récepteur a refusé la demande.", "Das Gerät oder der Empfänger hat die Anfrage abgelehnt.", "El dispositivo o el receptor rechazó la solicitud.", "Il dispositivo o il ricevitore ha rifiutato la richiesta.", "O dispositivo ou o receptor recusou a solicitação.";
    s_err_gone => "The device is no longer available.", "L'appareil n'est plus disponible.", "Das Gerät ist nicht mehr verfügbar.", "El dispositivo ya no está disponible.", "Il dispositivo non è più disponibile.", "O dispositivo não está mais disponível.";
    s_pair_waiting => "Switch on the device to pair (or switch it off and on again). On a device with several channels, first select a free channel with its Easy-Switch button, then hold the button until the light blinks quickly.", "Allumez l'appareil à appairer (ou éteignez-le puis rallumez-le). Sur un appareil à plusieurs canaux, choisissez d'abord un canal libre avec son bouton Easy-Switch, puis maintenez ce bouton jusqu'à ce que le voyant clignote rapidement.", "Schalten Sie das zu koppelnde Gerät ein (oder aus und wieder ein). Bei einem Gerät mit mehreren Kanälen wählen Sie zuerst mit der Easy-Switch-Taste einen freien Kanal und halten die Taste dann gedrückt, bis die Leuchte schnell blinkt.", "Encienda el dispositivo que desea emparejar (o apáguelo y vuelva a encenderlo). En un dispositivo con varios canales, elija primero un canal libre con su botón Easy-Switch y mantenga pulsado el botón hasta que la luz parpadee rápidamente.", "Accendi il dispositivo da associare (o spegnilo e riaccendilo). Su un dispositivo con più canali, scegli prima un canale libero con il tasto Easy-Switch, poi tieni premuto il tasto finché la spia lampeggia rapidamente.", "Ligue o dispositivo a emparelhar (ou desligue-o e ligue-o novamente). Num dispositivo com vários canais, escolha primeiro um canal livre com o botão Easy-Switch e mantenha o botão pressionado até a luz piscar rapidamente.";
    s_pair_code_hint => "The code to enter will appear below.", "Le code à saisir s'affichera ci-dessous.", "Der einzugebende Code erscheint unten.", "El código que debe introducir aparecerá abajo.", "Il codice da inserire apparirà qui sotto.", "O código a introduzir aparecerá abaixo.";
    s_pair_replaces => "This receiver keeps a single device per slot: the new device replaces the current one.", "Ce récepteur ne garde qu'un appareil par emplacement : le nouvel appareil remplace l'actuel.", "Dieser Empfänger speichert nur ein Gerät pro Platz: Das neue Gerät ersetzt das aktuelle.", "Este receptor guarda un solo dispositivo por ranura: el nuevo sustituye al actual.", "Questo ricevitore memorizza un solo dispositivo per slot: il nuovo sostituisce quello attuale.", "Este receptor guarda um único dispositivo por slot: o novo substitui o atual.";
    s_pair_found => "“{name}” found, connecting…", "« {name} » détecté, connexion…", "„{name}“ gefunden, Verbindung wird hergestellt…", "Se encontró «{name}», conectando…", "Trovato “{name}”, connessione in corso…", "“{name}” encontrado, conectando…";
    s_pair_keyboard => "On “{name}”, type this code, then press Enter:", "Sur « {name} », tapez ce code puis appuyez sur Entrée :", "Geben Sie auf „{name}“ diesen Code ein und drücken Sie dann die Eingabetaste:", "En «{name}», escriba este código y pulse Intro:", "Su “{name}”, digita questo codice e premi Invio:", "Em “{name}”, digite este código e pressione Enter:";
    s_pair_mouse => "On “{name}”, click in this order ({l} = left button, {r} = right button), then press both buttons together:", "Sur « {name} », cliquez dans cet ordre ({l} = bouton gauche, {r} = bouton droit), puis appuyez sur les deux boutons ensemble :", "Klicken Sie auf „{name}“ in dieser Reihenfolge ({l} = linke Taste, {r} = rechte Taste) und drücken Sie dann beide Tasten gleichzeitig:", "En «{name}», haga clic en este orden ({l} = botón izquierdo, {r} = botón derecho) y luego pulse ambos botones a la vez:", "Su “{name}”, fai clic in quest'ordine ({l} = tasto sinistro, {r} = tasto destro), poi premi entrambi i tasti insieme:", "Em “{name}”, clique nesta ordem ({l} = botão esquerdo, {r} = botão direito) e depois pressione os dois botões juntos:";
    s_left => "L", "G", "L", "I", "S", "E";
    s_right => "R", "D", "R", "D", "D", "D";
    s_pair_done => "“{name}” is paired (slot {slot}).", "« {name} » est appairé (emplacement {slot}).", "„{name}“ ist gekoppelt (Platz {slot}).", "«{name}» está emparejado (ranura {slot}).", "“{name}” è associato (slot {slot}).", "“{name}” está emparelhado (slot {slot}).";
    s_pair_timeout => "Time is up: no device was detected. Try again.", "Délai dépassé : aucun appareil n'a été détecté. Réessayez.", "Zeit abgelaufen: Es wurde kein Gerät erkannt. Versuchen Sie es erneut.", "Tiempo agotado: no se detectó ningún dispositivo. Inténtelo de nuevo.", "Tempo scaduto: nessun dispositivo rilevato. Riprova.", "Tempo esgotado: nenhum dispositivo foi detetado. Tente novamente.";
    s_pair_cancelled => "Pairing cancelled.", "Appairage annulé.", "Kopplung abgebrochen.", "Emparejamiento cancelado.", "Associazione annullata.", "Emparelhamento cancelado.";
    s_pair_failed => "Pairing failed: {reason}.", "Échec de l'appairage : {reason}.", "Kopplung fehlgeschlagen: {reason}.", "Error de emparejamiento: {reason}.", "Associazione non riuscita: {reason}.", "Falha no emparelhamento: {reason}.";
    s_pair_lost => "The receiver no longer answers.", "Le récepteur ne répond plus.", "Der Empfänger antwortet nicht mehr.", "El receptor ya no responde.", "Il ricevitore non risponde più.", "O receptor não responde mais.";
    s_why_not_supported => "this receiver cannot pair devices", "ce récepteur ne peut pas appairer d'appareil", "dieser Empfänger kann keine Geräte koppeln", "este receptor no puede emparejar dispositivos", "questo ricevitore non può associare dispositivi", "este receptor não pode emparelhar dispositivos";
    s_why_busy => "a pairing is already in progress", "un appairage est déjà en cours", "eine Kopplung läuft bereits", "ya hay un emparejamiento en curso", "è già in corso un'associazione", "já há um emparelhamento em andamento";
    s_why_lock => "the receiver did not open its pairing window", "le récepteur n'a pas ouvert sa fenêtre d'appairage", "der Empfänger hat das Kopplungsfenster nicht geöffnet", "el receptor no abrió su ventana de emparejamiento", "il ricevitore non ha aperto la finestra di associazione", "o receptor não abriu a janela de emparelhamento";
    s_why_device => "this device is not compatible with this receiver", "cet appareil n'est pas compatible avec ce récepteur", "dieses Gerät ist mit diesem Empfänger nicht kompatibel", "este dispositivo no es compatible con este receptor", "questo dispositivo non è compatibile con questo ricevitore", "este dispositivo não é compatível com este receptor";
    s_why_full => "the receiver has no free slot (unpair a device first)", "le récepteur n'a plus d'emplacement libre (désappairez d'abord un appareil)", "der Empfänger hat keinen freien Platz (entkoppeln Sie zuerst ein Gerät)", "el receptor no tiene ranuras libres (desempareje antes un dispositivo)", "il ricevitore non ha slot liberi (dissocia prima un dispositivo)", "o receptor não tem slots livres (desemparelhe primeiro um dispositivo)";
    s_why_failed => "the receiver reported an error", "le récepteur a signalé une erreur", "der Empfänger hat einen Fehler gemeldet", "el receptor informó de un error", "il ricevitore ha segnalato un errore", "o receptor relatou um erro";
    s_unpair_confirm => "Unpair “{name}” from this receiver?\n\nThe device will stop working with this receiver until it is paired again.", "Désappairer « {name} » de ce récepteur ?\n\nL'appareil ne fonctionnera plus avec ce récepteur tant qu'il n'aura pas été appairé à nouveau.", "„{name}“ von diesem Empfänger entkoppeln?\n\nDas Gerät funktioniert mit diesem Empfänger erst wieder, wenn es erneut gekoppelt wird.", "¿Desemparejar «{name}» de este receptor?\n\nEl dispositivo dejará de funcionar con este receptor hasta que se vuelva a emparejar.", "Dissociare “{name}” da questo ricevitore?\n\nIl dispositivo non funzionerà più con questo ricevitore finché non verrà associato di nuovo.", "Desemparelhar “{name}” deste receptor?\n\nO dispositivo deixará de funcionar com este receptor até ser emparelhado novamente.";
    s_unpaired => "“{name}” has been unpaired.", "« {name} » a été désappairé.", "„{name}“ wurde entkoppelt.", "«{name}» se ha desemparejado.", "“{name}” è stato dissociato.", "“{name}” foi desemparelhado.";
    s_slots => "{used}/{total} slots used", "{used}/{total} emplacements utilisés", "{used}/{total} Plätze belegt", "{used}/{total} ranuras en uso", "{used}/{total} slot occupati", "{used}/{total} slots em uso";
    s_receiver_fw => "firmware {fw}", "micrologiciel {fw}", "Firmware {fw}", "firmware {fw}", "firmware {fw}", "firmware {fw}";
    s_serial => "serial {sn}", "n° de série {sn}", "Seriennr. {sn}", "n.º de serie {sn}", "n. di serie {sn}", "n.º de série {sn}";
    s_direct => "Device connected directly (USB cable or Bluetooth)", "Appareil connecté directement (câble USB ou Bluetooth)", "Direkt verbundenes Gerät (USB-Kabel oder Bluetooth)", "Dispositivo conectado directamente (cable USB o Bluetooth)", "Dispositivo collegato direttamente (cavo USB o Bluetooth)", "Dispositivo ligado diretamente (cabo USB ou Bluetooth)";
    s_kind_keyboard => "Keyboard", "Clavier", "Tastatur", "Teclado", "Tastiera", "Teclado";
    s_kind_mouse => "Mouse", "Souris", "Maus", "Ratón", "Mouse", "Rato";
    s_kind_headset => "Headset", "Casque", "Headset", "Auriculares", "Cuffie", "Headset";
    s_kind_gamepad => "Game controller", "Manette", "Controller", "Mando", "Controller", "Comando";
    s_kind_earbuds => "Earbuds", "Écouteurs", "Ohrhörer", "Auriculares", "Auricolari", "Auriculares";
    s_kind_other => "Device", "Appareil", "Gerät", "Dispositivo", "Dispositivo", "Dispositivo";
}

fn kind_name(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Keyboard => s_kind_keyboard(),
        DeviceKind::Mouse => s_kind_mouse(),
        DeviceKind::Headset => s_kind_headset(),
        DeviceKind::Gamepad => s_kind_gamepad(),
        DeviceKind::Earbuds => s_kind_earbuds(),
        DeviceKind::Other => s_kind_other(),
    }
}

fn action_error(e: ActionError) -> &'static str {
    match e {
        ActionError::NotSupported => s_err_unsupported(),
        ActionError::Offline => s_err_offline(),
        ActionError::Busy => s_err_busy(),
        ActionError::Failed => s_err_failed(),
    }
}

fn pairing_reason(e: PairingError) -> &'static str {
    match e {
        PairingError::NotSupported => s_why_not_supported(),
        PairingError::Busy => s_why_busy(),
        PairingError::LockFailed => s_why_lock(),
        PairingError::DeviceNotSupported => s_why_device(),
        PairingError::TooManyDevices => s_why_full(),
        PairingError::Failed => s_why_failed(),
    }
}

fn backlight_name(mode: BacklightMode) -> &'static str {
    match mode {
        BacklightMode::Off => s_bl_off(),
        BacklightMode::On => s_bl_on(),
        BacklightMode::Auto => s_bl_auto(),
        BacklightMode::Manual => s_bl_manual(),
    }
}

fn rate_label(ms: u8) -> String {
    let hz = 1000 / ms.max(1) as u32;
    match lang() {
        Lang::Fr | Lang::De | Lang::Es | Lang::It | Lang::Pt => format!("{ms} ms ({hz}\u{a0}Hz)"),
        Lang::En => format!("{ms} ms ({hz} Hz)"),
    }
}

// --------------------------------------------------------- async plumbing

/// Posts `value` to the window; the receiver of the message owns the box.
fn post<T: Send + 'static>(hwnd: isize, msg: u32, wparam: usize, value: Option<T>) -> bool {
    let boxed = Box::into_raw(Box::new(value));
    // SAFETY: plain FFI call; on failure the box is reclaimed below.
    let ok = unsafe { PostMessageW(hwnd as HWND, msg, wparam, boxed as LPARAM) } != 0;
    if !ok {
        // SAFETY: the message was not queued, we still own the box.
        drop(unsafe { Box::from_raw(boxed) });
    }
    ok
}

/// Waits (on a helper thread) for one answer and posts it; `None` when
/// the channel closed or nothing came within 20 s.
fn forward_once<T: Send + 'static>(hwnd: HWND, msg: u32, wparam: usize, rx: Receiver<T>) {
    let raw = hwnd as isize;
    let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
        post(raw, msg, wparam, rx.recv_timeout(Duration::from_secs(20)).ok());
    });
}

/// Posts every value of a stream, then `None` when it closes.
fn forward_stream<T: Send + 'static>(hwnd: HWND, msg: u32, rx: Receiver<T>) {
    let raw = hwnd as isize;
    let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
        for value in rx.iter() {
            if !post(raw, msg, 0, Some(value)) {
                return;
            }
        }
        post::<T>(raw, msg, 0, None);
    });
}

/// Takes back a payload posted with `post`.
///
/// # Safety
/// `lparam` must come from `post::<T>` for this very message.
unsafe fn take<T>(lparam: LPARAM) -> Option<T> {
    *Box::from_raw(lparam as *mut Option<T>)
}

// ------------------------------------------------------------- the window

struct Controls {
    receiver: HWND,
    info: HWND,
    list: HWND,
    pair: HWND,
    unpair: HWND,
    pair_text: HWND,
    pair_code: HWND,
    dpi: HWND,
    rate: HWND,
    bl_mode: HWND,
    bl_level: HWND,
    wheel: HWND,
    wheel_speed: HWND,
    status: HWND,
}

struct Ui {
    hwnd: HWND,
    native: Native,
    dpi: u32,
    font: HFONT,
    big_font: HFONT,
    /// Every control with its rectangle at 96 DPI.
    layout: Vec<(HWND, [i32; 4])>,
    c: Controls,
    receivers: Vec<ReceiverInfo>,
    /// Selected receiver id.
    receiver: Option<String>,
    /// Selected row (slot) in the device list.
    slot: Option<u8>,
    /// Device whose settings are shown.
    device: Option<String>,
    settings: Option<DeviceSettings>,
    /// Sequence number of the last settings request.
    seq: usize,
    busy: bool,
    /// Receiver id of the pairing in progress, and whether it ended.
    pairing: Option<String>,
    pairing_final: bool,
    /// Name of the device being unpaired.
    unpairing: String,
    hook: HHOOK,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    /// Window handle, readable without borrowing `UI` (keyboard hook).
    static HWND_CELL: Cell<isize> = const { Cell::new(0) };
}

/// Runs `f` on the window state unless it is already borrowed (re-entrant
/// notification while the state is being updated): returns `None` then.
fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok().and_then(|mut ui| ui.as_mut().map(f)))
}

fn scale(v: i32, dpi: u32) -> i32 {
    (v as i64 * dpi as i64 / 96) as i32
}

/// Opens the window, or brings it to the front if it is already open.
/// Must be called on the UI thread (the one running the message loop).
pub fn open(native: &Native) {
    let existing = HWND_CELL.with(|c| c.get());
    if existing != 0 {
        let hwnd = existing as HWND;
        // SAFETY: our own window, alive while HWND_CELL is set.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
        return;
    }
    // SAFETY: window class registration and creation on this thread; all
    // strings are NUL-terminated buffers that outlive the calls.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc);
        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS);
        let mut class: WNDCLASSW = zeroed();
        class.lpfnWndProc = Some(wnd_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        class.hIcon = LoadIconW(instance, 1 as _);
        class.hbrBackground = GetSysColorBrush(COLOR_BTNFACE);
        // Fails harmlessly when the class is already registered.
        RegisterClassW(&class);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            wide(s_title()).as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CLIENT.0,
            CLIENT.1,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        let (font, big_font) = create_fonts(dpi);
        let (c, layout) = create_controls(hwnd);
        let mut ui = Ui {
            hwnd,
            native: native.clone(),
            dpi,
            font,
            big_font,
            layout,
            c,
            receivers: Vec::new(),
            receiver: None,
            slot: None,
            device: None,
            settings: None,
            seq: 0,
            busy: false,
            pairing: None,
            pairing_final: false,
            unpairing: String::new(),
            hook: null_mut(),
        };
        ui.apply_dpi(dpi, None);
        ui.hook = SetWindowsHookExW(WH_GETMESSAGE, Some(dialog_keys), null_mut(), GetCurrentThreadId());
        HWND_CELL.with(|cell| cell.set(hwnd as isize));

        native.watch(Some(change_notifier(hwnd)));
        ui.set_text(ui.c.info, s_searching());
        ui.show_settings();
        ui.update_buttons();
        ui.request_list();
        UI.with(|cell| *cell.borrow_mut() = Some(ui));

        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        with_ui(|ui| SetFocus(ui.c.list));
    }
}

/// Wakes the window (from any thread) when a native device changed.
fn change_notifier(hwnd: HWND) -> Box<dyn Fn() + Send + Sync> {
    let raw = hwnd as isize;
    Box::new(move || {
        // SAFETY: plain FFI call; a stale handle is harmless.
        unsafe { PostMessageW(raw as HWND, WM_APP_CHANGED, 0, 0) };
    })
}

/// Message font of the system at `dpi`, and a large bold one for codes.
fn create_fonts(dpi: u32) -> (HFONT, HFONT) {
    // SAFETY: `ncm` is a plain structure with its size set; the LOGFONTs
    // passed to CreateFontIndirectW are valid.
    unsafe {
        let mut ncm: NONCLIENTMETRICSW = zeroed();
        ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, ncm.cbSize, (&mut ncm as *mut NONCLIENTMETRICSW).cast(), 0, dpi);
        let mut lf = ncm.lfMessageFont;
        if lf.lfHeight == 0 {
            lf.lfHeight = -scale(12, dpi);
        }
        let font = CreateFontIndirectW(&lf);
        lf.lfHeight = lf.lfHeight.saturating_mul(2);
        lf.lfWeight = 600;
        let big = CreateFontIndirectW(&lf);
        (font, big)
    }
}

/// Creates every child control (positions are set by `Ui::apply_dpi`).
///
/// # Safety
/// `hwnd` must be a window created on this thread.
unsafe fn create_controls(hwnd: HWND) -> (Controls, Vec<(HWND, [i32; 4])>) {
    let instance = GetModuleHandleW(null());
    let mut layout = Vec::new();
    let mut make = |class: &str, text: &str, style: u32, ex: u32, id: u16, rect: [i32; 4]| -> HWND {
        let h = CreateWindowExW(
            ex,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            hwnd,
            id as usize as _,
            instance,
            null(),
        );
        layout.push((h, rect));
        h
    };
    let label = SS_LEFT | SS_NOPREFIX | SS_CENTERIMAGE;
    let combo = CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP;
    let track = TBS_AUTOTICKS | WS_TABSTOP;

    make("STATIC", s_receiver(), label, 0, 0, [12, 12, 96, 24]);
    let receiver = make("COMBOBOX", "", combo | WS_GROUP, 0, ID_RECEIVER, [112, 12, 516, 300]);
    let info = make("STATIC", "", SS_LEFT | SS_NOPREFIX | SS_ENDELLIPSIS, 0, 0, [12, 44, 616, 20]);
    let list = make(
        "SysListView32",
        "",
        LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER | WS_TABSTOP | WS_GROUP,
        WS_EX_CLIENTEDGE,
        ID_LIST,
        [12, 70, 616, 176],
    );
    let pair = make("BUTTON", s_pair(), BS_PUSHBUTTON | WS_TABSTOP | WS_GROUP, 0, ID_PAIR, [12, 254, 240, 28]);
    let unpair = make("BUTTON", s_unpair(), BS_PUSHBUTTON | WS_TABSTOP, 0, ID_UNPAIR, [260, 254, 160, 28]);
    let pair_text = make("STATIC", "", SS_LEFT | SS_NOPREFIX, 0, 0, [12, 290, 616, 64]);
    let pair_code = make("STATIC", "", SS_CENTER | SS_NOPREFIX | SS_CENTERIMAGE, 0, 0, [12, 354, 616, 40]);
    make("BUTTON", s_group(), BS_GROUPBOX, 0, 0, [12, 400, 616, 170]);
    make("STATIC", s_dpi(), label, 0, 0, [28, 424, 180, 24]);
    let dpi = make("COMBOBOX", "", combo | WS_GROUP, 0, ID_DPI, [212, 424, 150, 300]);
    make("STATIC", s_rate(), label, 0, 0, [28, 458, 180, 24]);
    let rate = make("COMBOBOX", "", combo | WS_GROUP, 0, ID_RATE, [212, 458, 150, 300]);
    make("STATIC", s_backlight(), label, 0, 0, [28, 492, 180, 24]);
    let bl_mode = make("COMBOBOX", "", combo | WS_GROUP, 0, ID_BL_MODE, [212, 492, 150, 300]);
    let bl_level = make("msctls_trackbar32", "", track, 0, ID_BL_LEVEL, [370, 490, 246, 28]);
    make("STATIC", s_wheel(), label, 0, 0, [28, 526, 180, 24]);
    let wheel = make("COMBOBOX", "", combo | WS_GROUP, 0, ID_WHEEL, [212, 526, 150, 300]);
    let wheel_speed = make("msctls_trackbar32", "", track, 0, ID_WHEEL_SPEED, [370, 524, 246, 28]);
    let status = make("STATIC", "", SS_LEFT | SS_NOPREFIX | SS_ENDELLIPSIS, 0, 0, [12, 578, 616, 20]);

    SetWindowTheme(list, wide("Explorer").as_ptr(), null());
    SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as LPARAM);
    let titles = [s_col_device(), s_col_slot(), s_col_status(), s_col_firmware(), s_col_channel()];
    for (i, (title, width)) in titles.iter().zip(COLUMNS).enumerate() {
        let mut text = wide(title);
        let mut col: LVCOLUMNW = zeroed();
        col.mask = LVCF_TEXT | LVCF_WIDTH;
        col.cx = width;
        col.pszText = text.as_mut_ptr();
        SendMessageW(list, LVM_INSERTCOLUMNW, i, &col as *const LVCOLUMNW as LPARAM);
    }
    for combo in [receiver, dpi, rate, bl_mode, wheel] {
        SendMessageW(combo, CB_SETMINVISIBLE, 12, 0);
    }
    SendMessageW(wheel_speed, TBM_SETRANGEMIN, 0, 1);
    SendMessageW(wheel_speed, TBM_SETRANGEMAX, 1, WHEEL_MAX as LPARAM);
    SendMessageW(wheel_speed, TBM_SETTICFREQ, 7, 0);
    SendMessageW(wheel_speed, TBM_SETPAGESIZE, 0, 5);

    let c = Controls { receiver, info, list, pair, unpair, pair_text, pair_code, dpi, rate, bl_mode, bl_level, wheel, wheel_speed, status };
    (c, layout)
}

impl Ui {
    fn set_text(&self, hwnd: HWND, text: &str) {
        // SAFETY: valid window and NUL-terminated string.
        unsafe { SetWindowTextW(hwnd, wide(text).as_ptr()) };
    }

    fn enable(&self, hwnd: HWND, on: bool) {
        // SAFETY: valid window handle.
        unsafe { EnableWindow(hwnd, on as i32) };
    }

    /// Fonts, positions and column widths for `dpi`; `suggested` is the
    /// window rectangle proposed by WM_DPICHANGED.
    fn apply_dpi(&mut self, dpi: u32, suggested: Option<RECT>) {
        if dpi != self.dpi {
            let (font, big) = create_fonts(dpi);
            // SAFETY: fonts we created; the controls get the new ones
            // through WM_SETFONT below before they draw again.
            unsafe {
                DeleteObject(self.font as _);
                DeleteObject(self.big_font as _);
            }
            self.font = font;
            self.big_font = big;
            self.dpi = dpi;
        }
        // SAFETY: child windows of our window; RECT pointers are valid.
        unsafe {
            for &(h, [x, y, w, hgt]) in &self.layout {
                SetWindowPos(h, null_mut(), scale(x, dpi), scale(y, dpi), scale(w, dpi), scale(hgt, dpi), SWP_NOZORDER | SWP_NOACTIVATE);
                let font = if h == self.c.pair_code { self.big_font } else { self.font };
                SendMessageW(h, WM_SETFONT, font as WPARAM, 1);
            }
            for (i, width) in COLUMNS.iter().enumerate() {
                SendMessageW(self.c.list, LVM_SETCOLUMNWIDTH, i, scale(*width, dpi) as LPARAM);
            }
            match suggested {
                Some(r) => {
                    SetWindowPos(self.hwnd, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                None => {
                    let mut r = RECT { left: 0, top: 0, right: scale(CLIENT.0, dpi), bottom: scale(CLIENT.1, dpi) };
                    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
                    AdjustWindowRectExForDpi(&mut r, style, 0, WS_EX_CONTROLPARENT, dpi);
                    SetWindowPos(
                        self.hwnd,
                        null_mut(),
                        0,
                        0,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE,
                    );
                }
            }
        }
    }

    // ---------------------------------------------------------- receivers

    fn request_list(&self) {
        forward_once(self.hwnd, WM_APP_LIST, 0, self.native.list_receivers());
    }

    fn current(&self) -> Option<&ReceiverInfo> {
        let id = self.receiver.as_ref()?;
        self.receivers.iter().find(|r| &r.id == id)
    }

    fn selected_device(&self) -> Option<&PairedDevice> {
        let slot = self.slot?;
        self.current()?.devices.iter().find(|d| d.slot == slot)
    }

    fn on_list(&mut self, receivers: Vec<ReceiverInfo>) {
        self.receivers = receivers;
        if self.current().is_none() {
            self.receiver = self.receivers.first().map(|r| r.id.clone());
            self.slot = None;
        }
        // SAFETY: combo box owned by our window; strings outlive the calls.
        unsafe {
            SendMessageW(self.c.receiver, CB_RESETCONTENT, 0, 0);
            for (i, r) in self.receivers.iter().enumerate() {
                let label = match r.kind {
                    ReceiverKind::Direct => format!("{} (USB / Bluetooth)", r.name),
                    _ => r.name.clone(),
                };
                let dup = self.receivers[..i].iter().filter(|o| o.name == r.name).count();
                let label = if dup > 0 { format!("{label} #{}", dup + 1) } else { label };
                SendMessageW(self.c.receiver, CB_ADDSTRING, 0, wide(&label).as_ptr() as LPARAM);
                if Some(&r.id) == self.receiver.as_ref() {
                    SendMessageW(self.c.receiver, CB_SETCURSEL, i, 0);
                }
            }
        }
        self.fill_devices();
    }

    fn on_receiver_changed(&mut self) {
        // SAFETY: combo box owned by our window.
        let sel = unsafe { SendMessageW(self.c.receiver, CB_GETCURSEL, 0, 0) };
        let id = usize::try_from(sel).ok().and_then(|i| self.receivers.get(i)).map(|r| r.id.clone());
        if id != self.receiver {
            self.receiver = id;
            self.slot = None;
            self.fill_devices();
        }
    }

    /// Receiver summary, device list, then the selection-dependent parts.
    fn fill_devices(&mut self) {
        let info = match self.current() {
            None => s_none().to_string(),
            Some(r) if r.kind == ReceiverKind::Direct => s_direct().to_string(),
            Some(r) => {
                let mut parts = vec![s_slots().replace("{used}", &r.devices.len().to_string()).replace("{total}", &r.slots.to_string())];
                if let Some(fw) = &r.firmware {
                    parts.push(s_receiver_fw().replace("{fw}", fw));
                }
                if let Some(sn) = &r.serial {
                    parts.push(s_serial().replace("{sn}", sn));
                }
                format!("{} · {}", r.name, parts.join(" · "))
            }
        };
        self.set_text(self.c.info, &info);

        let devices: Vec<PairedDevice> = self.current().map(|r| r.devices.clone()).unwrap_or_default();
        if self.slot.is_some_and(|s| !devices.iter().any(|d| d.slot == s)) {
            self.slot = None;
        }
        if self.slot.is_none() {
            self.slot = devices.first().map(|d| d.slot);
        }
        // SAFETY: list view owned by our window; every LVITEMW and string
        // buffer outlives the SendMessageW call that uses it.
        unsafe {
            SendMessageW(self.c.list, LVM_DELETEALLITEMS, 0, 0);
            for (row, d) in devices.iter().enumerate() {
                let cells = [
                    d.name.clone(),
                    if d.slot == 0xFF { "—".to_string() } else { d.slot.to_string() },
                    if d.online { s_connected() } else { s_asleep() }.to_string(),
                    d.info.firmware.clone().unwrap_or_else(|| "—".into()),
                    d.info.host_channel.map_or_else(|| "—".into(), |(cur, n)| format!("{cur} / {n}")),
                ];
                for (col, text) in cells.iter().enumerate() {
                    let mut buf = wide(text);
                    let mut item: LVITEMW = zeroed();
                    item.iItem = row as i32;
                    item.iSubItem = col as i32;
                    item.pszText = buf.as_mut_ptr();
                    if col == 0 {
                        item.mask = LVIF_TEXT | LVIF_PARAM;
                        item.lParam = d.slot as LPARAM;
                        SendMessageW(self.c.list, LVM_INSERTITEMW, 0, &item as *const LVITEMW as LPARAM);
                    } else {
                        item.mask = LVIF_TEXT;
                        SendMessageW(self.c.list, LVM_SETITEMTEXTW, row, &item as *const LVITEMW as LPARAM);
                    }
                }
                if Some(d.slot) == self.slot {
                    let mut item: LVITEMW = zeroed();
                    item.mask = LVIF_STATE;
                    item.state = LVIS_SELECTED | LVIS_FOCUSED;
                    item.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
                    SendMessageW(self.c.list, LVM_SETITEMSTATE, row, &item as *const LVITEMW as LPARAM);
                    SendMessageW(self.c.list, LVM_ENSUREVISIBLE, row, 0);
                }
            }
        }
        self.device_changed(false);
        self.update_buttons();
    }

    fn on_select(&mut self) {
        // SAFETY: list view owned by our window.
        let row = unsafe { SendMessageW(self.c.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as LPARAM) };
        let devices: Vec<u8> = self.current().map(|r| r.devices.iter().map(|d| d.slot).collect()).unwrap_or_default();
        let slot = usize::try_from(row).ok().and_then(|i| devices.get(i).copied());
        if slot != self.slot {
            self.slot = slot;
            self.device_changed(true);
            self.update_buttons();
        }
    }

    /// Reloads the settings when the selected device changed, or when it
    /// just came back online. `explicit` = selected by the user.
    fn device_changed(&mut self, explicit: bool) {
        let selected = self.selected_device().cloned();
        let id = selected.as_ref().and_then(|d| d.id.clone());
        let online = selected.as_ref().is_some_and(|d| d.online);
        let same = id == self.device;
        if same && !explicit && (self.settings.is_some() || self.busy || !online) {
            if !self.busy {
                self.show_status_for(selected.as_ref());
            }
            return;
        }
        self.device = id.clone();
        self.settings = None;
        self.busy = false;
        self.seq += 1;
        match (&selected, id) {
            (Some(d), Some(id)) if d.online => {
                self.busy = true;
                self.set_text(self.c.status, s_reading());
                forward_once(self.hwnd, WM_APP_SETTINGS, self.seq << 1, self.native.get_settings(&id));
            }
            (Some(_), Some(_)) => self.set_text(self.c.status, s_err_offline()),
            (Some(_), None) => self.set_text(self.c.status, s_not_identified()),
            (None, _) => self.set_text(self.c.status, ""),
        }
        self.show_settings();
    }

    /// Short description of the selected device in the status line.
    fn show_status_for(&self, device: Option<&PairedDevice>) {
        let Some(d) = device else {
            self.set_text(self.c.status, "");
            return;
        };
        let mut parts = Vec::new();
        if let Some(kind) = d.kind {
            parts.push(kind_name(kind).to_string());
        }
        if let Some(sn) = &d.info.serial {
            parts.push(s_serial().replace("{sn}", sn));
        }
        if let Some(wpid) = d.wpid {
            parts.push(format!("WPID {wpid:04X}"));
        }
        if self.settings.as_ref().is_some_and(|s| *s == DeviceSettings::default()) {
            parts.push(s_no_settings().to_string());
        }
        self.set_text(self.c.status, &parts.join(" · "));
    }

    fn update_buttons(&self) {
        let r = self.current();
        let pairing_here = self.pairing.is_some() && self.pairing == self.receiver;
        let can_pair = r.is_some_and(|r| r.kind.can_pair()) && self.pairing.is_none();
        self.enable(self.c.pair, can_pair || pairing_here);
        self.set_text(self.c.pair, if pairing_here { s_cancel_pair() } else { s_pair() });
        let can_unpair = r.is_some_and(|r| r.kind.can_unpair()) && self.selected_device().is_some() && self.pairing.is_none();
        self.enable(self.c.unpair, can_unpair);
        self.enable(self.c.receiver, self.pairing.is_none());
    }

    // ----------------------------------------------------------- settings

    /// Fills the settings controls from `self.settings`.
    fn show_settings(&self) {
        let s = self.settings.as_ref();
        let usable = !self.busy;
        // SAFETY: combo boxes / trackbars owned by our window; strings
        // outlive the calls.
        unsafe {
            let fill = |combo: HWND, items: &[(String, isize)], current: Option<isize>, enabled: bool| {
                SendMessageW(combo, CB_RESETCONTENT, 0, 0);
                if items.is_empty() {
                    SendMessageW(combo, CB_ADDSTRING, 0, wide("—").as_ptr() as LPARAM);
                    SendMessageW(combo, CB_SETCURSEL, 0, 0);
                }
                for (i, (text, data)) in items.iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, 0, wide(text).as_ptr() as LPARAM);
                    SendMessageW(combo, CB_SETITEMDATA, i, *data);
                    if Some(*data) == current {
                        SendMessageW(combo, CB_SETCURSEL, i, 0);
                    }
                }
                EnableWindow(combo, (enabled && !items.is_empty()) as i32);
            };

            let dpi = s.and_then(|s| s.dpi.as_ref());
            let mut items: Vec<(String, isize)> =
                dpi.map(|d| d.choices.iter().map(|&v| (v.to_string(), v as isize)).collect()).unwrap_or_default();
            if let Some(d) = dpi.filter(|d| !d.choices.contains(&d.current)) {
                items.push((d.current.to_string(), d.current as isize));
            }
            fill(self.c.dpi, &items, dpi.map(|d| d.current as isize), usable);

            let rate = s.and_then(|s| s.report_rate.as_ref());
            let items: Vec<(String, isize)> =
                rate.map(|r| r.choices.iter().map(|&ms| (rate_label(ms), ms as isize)).collect()).unwrap_or_default();
            fill(self.c.rate, &items, rate.map(|r| r.current as isize), usable);

            let bl = s.and_then(|s| s.backlight.as_ref());
            let mut modes: Vec<BacklightMode> = bl.map(|b| b.modes.clone()).unwrap_or_default();
            if let Some(b) = bl.filter(|b| !b.modes.contains(&b.mode)) {
                modes.push(b.mode);
            }
            let items: Vec<(String, isize)> = modes.iter().map(|&m| (backlight_name(m).to_string(), m as isize)).collect();
            fill(self.c.bl_mode, &items, bl.map(|b| b.mode as isize), usable);
            let levels = bl.map_or(0, |b| b.levels);
            SendMessageW(self.c.bl_level, TBM_SETRANGEMIN, 0, 0);
            SendMessageW(self.c.bl_level, TBM_SETRANGEMAX, 1, levels.saturating_sub(1).max(1) as LPARAM);
            SendMessageW(self.c.bl_level, TBM_SETPOS, 1, bl.map_or(0, |b| b.level) as LPARAM);
            let manual = bl.is_some_and(|b| b.mode == BacklightMode::Manual && b.levels > 1);
            EnableWindow(self.c.bl_level, (usable && manual) as i32);

            let wheel = s.and_then(|s| s.smart_shift);
            let items: Vec<(String, isize)> = if wheel.is_some() {
                vec![(s_wheel_free().into(), 0), (s_wheel_ratchet().into(), 1), (s_wheel_smart().into(), 2)]
            } else {
                Vec::new()
            };
            let (current, speed) = match wheel {
                Some(WheelMode::FreeSpin) => (Some(0), 16),
                Some(WheelMode::Ratchet) => (Some(1), 16),
                Some(WheelMode::Smart(t)) => (Some(2), t.min(WHEEL_MAX)),
                None => (None, 16),
            };
            fill(self.c.wheel, &items, current, usable);
            SendMessageW(self.c.wheel_speed, TBM_SETPOS, 1, speed as LPARAM);
            EnableWindow(self.c.wheel_speed, (usable && current == Some(2)) as i32);
        }
    }

    fn combo_value(&self, combo: HWND) -> Option<isize> {
        // SAFETY: combo box owned by our window.
        unsafe {
            let sel = SendMessageW(combo, CB_GETCURSEL, 0, 0);
            (sel >= 0).then(|| SendMessageW(combo, CB_GETITEMDATA, sel as usize, 0))
        }
    }

    fn track_value(&self, track: HWND) -> u8 {
        // SAFETY: trackbar owned by our window.
        unsafe { SendMessageW(track, TBM_GETPOS, 0, 0) }.clamp(0, 255) as u8
    }

    /// A settings control changed: build the setting and send it.
    fn on_setting_control(&mut self, id: u16) {
        let Some(current) = self.settings.clone() else { return };
        if self.busy {
            return;
        }
        let setting = match id {
            ID_DPI => self.combo_value(self.c.dpi).map(|v| Setting::Dpi(v as u16)).filter(|_| current.dpi.is_some()),
            ID_RATE => self.combo_value(self.c.rate).map(|v| Setting::ReportRate(v as u8)).filter(|_| current.report_rate.is_some()),
            ID_BL_MODE | ID_BL_LEVEL => current.backlight.as_ref().and_then(|b| {
                let mode = match self.combo_value(self.c.bl_mode)? {
                    v if v == BacklightMode::On as isize => BacklightMode::On,
                    v if v == BacklightMode::Auto as isize => BacklightMode::Auto,
                    v if v == BacklightMode::Manual as isize => BacklightMode::Manual,
                    _ => BacklightMode::Off,
                };
                let level = if id == ID_BL_LEVEL { self.track_value(self.c.bl_level) } else { b.level };
                Some(Setting::Backlight { mode, level })
            }),
            ID_WHEEL | ID_WHEEL_SPEED => current.smart_shift.and_then(|_| {
                Some(Setting::SmartShift(match self.combo_value(self.c.wheel)? {
                    0 => WheelMode::FreeSpin,
                    1 => WheelMode::Ratchet,
                    _ => WheelMode::Smart(self.track_value(self.c.wheel_speed).max(1)),
                }))
            }),
            _ => None,
        };
        let (Some(setting), Some(device)) = (setting, self.device.clone()) else { return };
        // Nothing to write when the value did not change.
        let unchanged = match setting {
            Setting::Dpi(v) => current.dpi.as_ref().is_some_and(|d| d.current == v),
            Setting::ReportRate(v) => current.report_rate.as_ref().is_some_and(|r| r.current == v),
            Setting::Backlight { mode, level } => {
                current.backlight.as_ref().is_some_and(|b| b.mode == mode && (mode != BacklightMode::Manual || b.level == level))
            }
            Setting::SmartShift(m) => current.smart_shift == Some(m),
        };
        if unchanged {
            return;
        }
        self.seq += 1;
        self.busy = true;
        self.set_text(self.c.status, s_applying());
        self.show_settings();
        forward_once(self.hwnd, WM_APP_SETTINGS, self.seq << 1 | 1, self.native.set_setting(&device, setting));
    }

    fn on_settings(&mut self, tag: usize, result: Option<Result<DeviceSettings, ActionError>>) {
        if tag >> 1 != self.seq {
            return; // stale answer (another device was selected meanwhile)
        }
        self.busy = false;
        let was_set = tag & 1 == 1;
        match result {
            Some(Ok(s)) => {
                self.settings = Some(s);
                if was_set {
                    self.set_text(self.c.status, s_applied());
                } else {
                    self.show_status_for(self.selected_device());
                }
            }
            Some(Err(e)) => self.set_text(self.c.status, action_error(e)),
            None => self.set_text(self.c.status, s_err_gone()),
        }
        self.show_settings();
    }

    // ------------------------------------------------------------ pairing

    fn start_or_cancel_pairing(&mut self) {
        if let Some(id) = &self.pairing {
            self.native.cancel_pairing(id);
            return;
        }
        let Some(r) = self.current().filter(|r| r.kind.can_pair()) else { return };
        let (id, kind) = (r.id.clone(), r.kind);
        let mut text = s_pair_waiting().to_string();
        match kind {
            ReceiverKind::Bolt => text = format!("{text}\n{}", s_pair_code_hint()),
            ReceiverKind::Lightspeed | ReceiverKind::Nano => text = format!("{text}\n{}", s_pair_replaces()),
            _ => {}
        }
        self.set_text(self.c.pair_text, &text);
        self.set_text(self.c.pair_code, "");
        self.pairing = Some(id.clone());
        self.pairing_final = false;
        self.update_buttons();
        forward_stream(self.hwnd, WM_APP_PAIRING, self.native.pair(&id));
    }

    fn on_pairing(&mut self, event: Option<PairingEvent>) {
        let quote = |s: &str, name: &str| s.replace("{name}", name);
        match event {
            Some(PairingEvent::Waiting) => {}
            Some(PairingEvent::Found { name }) => {
                self.set_text(self.c.pair_text, &quote(s_pair_found(), &name));
            }
            Some(PairingEvent::Passkey { name, code, keyboard }) => {
                if keyboard {
                    self.set_text(self.c.pair_text, &quote(s_pair_keyboard(), &name));
                    let spaced: Vec<String> = code.chars().map(String::from).collect();
                    self.set_text(self.c.pair_code, &spaced.join(" "));
                } else {
                    let text = quote(s_pair_mouse(), &name).replace("{l}", s_left()).replace("{r}", s_right());
                    self.set_text(self.c.pair_text, &text);
                    let clicks = passkey_clicks(&code).unwrap_or_default();
                    let seq: Vec<&str> = clicks.iter().map(|&right| if right { s_right() } else { s_left() }).collect();
                    self.set_text(self.c.pair_code, &seq.join(" "));
                }
            }
            Some(final_event) => {
                self.pairing_final = true;
                let text = match final_event {
                    PairingEvent::Paired { slot, name } => s_pair_done().replace("{name}", &name).replace("{slot}", &slot.to_string()),
                    PairingEvent::Failed(e) => s_pair_failed().replace("{reason}", pairing_reason(e)),
                    PairingEvent::Timeout => s_pair_timeout().to_string(),
                    _ => s_pair_cancelled().to_string(),
                };
                self.set_text(self.c.pair_text, &text);
                self.set_text(self.c.pair_code, "");
            }
            None => {
                if !self.pairing_final {
                    self.set_text(self.c.pair_text, s_pair_lost());
                    self.set_text(self.c.pair_code, "");
                }
                self.pairing = None;
                self.update_buttons();
                self.request_list();
            }
        }
    }

    // ---------------------------------------------------------- unpairing

    /// (receiver id, slot, device name) of the selected device.
    fn unpair_target(&self) -> Option<(String, u8, String)> {
        let r = self.current().filter(|r| r.kind.can_unpair())?;
        let d = self.selected_device()?;
        Some((r.id.clone(), d.slot, d.name.clone()))
    }

    fn unpair(&mut self, receiver: &str, slot: u8, name: &str) {
        self.set_text(self.c.status, "");
        self.enable(self.c.unpair, false);
        forward_once(self.hwnd, WM_APP_UNPAIR, 0, self.native.unpair(receiver, slot));
        self.set_text(self.c.pair_text, "");
        self.set_text(self.c.pair_code, "");
        self.unpairing = name.to_string();
    }

    fn on_unpaired(&mut self, result: Option<Result<(), ActionError>>) {
        let text = match result {
            Some(Ok(())) => s_unpaired().replace("{name}", &self.unpairing),
            Some(Err(e)) => action_error(e).to_string(),
            None => s_err_gone().to_string(),
        };
        self.set_text(self.c.pair_text, &text);
        self.slot = None;
        self.request_list();
        self.update_buttons();
    }

    fn destroy(&mut self) {
        if let Some(id) = self.pairing.take() {
            self.native.cancel_pairing(&id);
        }
        self.native.watch(None);
        // SAFETY: hook and fonts created by us; the controls using the
        // fonts are being destroyed with the window.
        unsafe {
            if !self.hook.is_null() {
                UnhookWindowsHookEx(self.hook);
            }
            DeleteObject(self.font as _);
            DeleteObject(self.big_font as _);
        }
    }
}

/// Keyboard navigation (Tab, arrows, Esc) without access to the message
/// loop: messages for our window go through IsDialogMessage first.
///
/// # Safety
/// Called by Windows as a WH_GETMESSAGE hook: `lparam` points to the MSG
/// being retrieved, which may be modified.
unsafe extern "system" fn dialog_keys(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam == PM_REMOVE {
        let msg = &mut *(lparam as *mut MSG);
        let main = HWND_CELL.with(|c| c.get()) as HWND;
        if !main.is_null()
            && (WM_KEYFIRST..=WM_KEYLAST).contains(&msg.message)
            && (msg.hwnd == main || IsChild(main, msg.hwnd) != 0)
            && IsDialogMessageW(main, msg) != 0
        {
            msg.message = WM_NULL;
            msg.wParam = 0;
            msg.lParam = 0;
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

/// Re-posts a payload message that arrived while the state was borrowed
/// (the payload stays owned by the message).
///
/// # Safety
/// Same contract as `PostMessageW`.
unsafe fn repost(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    PostMessageW(hwnd, msg, wparam, lparam);
    0
}

/// # Safety
/// Called by Windows with the parameters of a message for our window;
/// WM_NOTIFY / WM_DPICHANGED `lparam` pointers are valid for the call and
/// WM_APP_* payloads come from `post` with the matching type.
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let busy = || UI.with(|cell| cell.try_borrow_mut().is_err());
    match msg {
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as u16;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            match (id, code) {
                (ID_RECEIVER, CBN_SELCHANGE) => {
                    with_ui(|ui| ui.on_receiver_changed());
                }
                (ID_DPI | ID_RATE | ID_BL_MODE | ID_WHEEL, CBN_SELCHANGE) => {
                    with_ui(|ui| ui.on_setting_control(id));
                }
                (ID_PAIR, BN_CLICKED) => {
                    with_ui(|ui| ui.start_or_cancel_pairing());
                }
                (ID_UNPAIR, BN_CLICKED) => {
                    // The confirmation runs a modal loop: not under borrow.
                    if let Some(Some((receiver, slot, name))) = with_ui(|ui| ui.unpair_target()) {
                        let text = s_unpair_confirm().replace("{name}", &name);
                        let answer = MessageBoxW(hwnd, wide(&text).as_ptr(), wide(s_unpair()).as_ptr(), MB_YESNO | MB_ICONWARNING);
                        if answer == IDYES {
                            with_ui(|ui| ui.unpair(&receiver, slot, &name));
                        }
                    }
                }
                (IDCANCEL, _) => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_NOTIFY => {
            let hdr = &*(lparam as *const NMHDR);
            if hdr.idFrom == ID_LIST as usize && hdr.code == LVN_ITEMCHANGED {
                let nm = &*(lparam as *const NMLISTVIEW);
                if nm.uChanged & LVIF_STATE != 0 && (nm.uNewState ^ nm.uOldState) & LVIS_SELECTED != 0 {
                    with_ui(|ui| ui.on_select());
                }
            }
            0
        }
        WM_HSCROLL => {
            if wparam & 0xFFFF == TB_ENDTRACK {
                let from = lparam as HWND;
                with_ui(|ui| {
                    if from == ui.c.bl_level {
                        ui.on_setting_control(ID_BL_LEVEL);
                    } else if from == ui.c.wheel_speed {
                        ui.on_setting_control(ID_WHEEL_SPEED);
                    }
                });
            }
            0
        }
        WM_APP_LIST if busy() => repost(hwnd, msg, wparam, lparam),
        WM_APP_LIST => {
            let list = take::<Vec<ReceiverInfo>>(lparam);
            with_ui(|ui| ui.on_list(list.unwrap_or_default()));
            0
        }
        WM_APP_SETTINGS if busy() => repost(hwnd, msg, wparam, lparam),
        WM_APP_SETTINGS => {
            let result = take::<Result<DeviceSettings, ActionError>>(lparam);
            with_ui(|ui| ui.on_settings(wparam, result));
            0
        }
        WM_APP_PAIRING if busy() => repost(hwnd, msg, wparam, lparam),
        WM_APP_PAIRING => {
            let event = take::<PairingEvent>(lparam);
            with_ui(|ui| ui.on_pairing(event));
            0
        }
        WM_APP_UNPAIR if busy() => repost(hwnd, msg, wparam, lparam),
        WM_APP_UNPAIR => {
            let result = take::<Result<(), ActionError>>(lparam);
            with_ui(|ui| ui.on_unpaired(result));
            0
        }
        WM_APP_CHANGED => {
            // Device events come in bursts: debounce.
            SetTimer(hwnd, TIMER_REFRESH, 700, None);
            0
        }
        WM_TIMER => {
            KillTimer(hwnd, wparam);
            if wparam == TIMER_REFRESH {
                with_ui(|ui| ui.request_list());
            }
            0
        }
        WM_DPICHANGED => {
            let dpi = (wparam & 0xFFFF) as u32;
            let suggested = *(lparam as *const RECT);
            with_ui(|ui| ui.apply_dpi(dpi, Some(suggested)));
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            HWND_CELL.with(|c| c.set(0));
            if let Some(mut ui) = UI.with(|cell| cell.try_borrow_mut().ok().and_then(|mut u| u.take())) {
                ui.destroy();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
