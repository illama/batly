//! "Receivers and devices" window: lists the receivers (and directly
//! connected devices) with their paired devices, pairs / unpairs devices
//! and edits a few hardware settings of the selected device.
//!
//! Drawn with egui in the Batly style (see [`crate::look`]), hosted by
//! [`crate::egui_host`]. Every HID request runs on the session threads of
//! [`Native`]; the answers come back on channels that the window polls at
//! each frame (a redraw is requested while one is awaited), so the UI
//! thread never blocks on a device. `open` is called on the UI thread.

use crate::hidpp::{
    passkey_clicks, ActionError, BacklightMode, DeviceSettings, PairedDevice, PairingError, PairingEvent, ReceiverInfo, ReceiverKind,
    Setting, WheelMode,
};
use crate::i18n::{lang, tr, Lang};
use crate::lighting::{self, EffectKind, LedEffect, Lighting, OnboardStatus, PERIOD_MAX_MS, PERIOD_MIN_MS};
use crate::look;
use crate::model::DeviceKind;
use crate::native::Native;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::InvalidateRect;

const CLASS: &str = "BatlyReceivers";
/// SmartShift speed range shown on the slider (Solaar uses the same).
const WHEEL_MAX: u8 = 50;
/// How long an answer of the device is awaited.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// Device events come in bursts: the list is read again this long after
/// the last one.
const REFRESH_DELAY: Duration = Duration::from_millis(700);
/// Redraw interval while an answer is awaited.
const POLL: Duration = Duration::from_millis(100);

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
    s_ghub => "Logitech G HUB is running: Batly leaves the Logitech devices to it (batteries come from G HUB). Close G HUB to change their settings here.", "Logitech G HUB est ouvert : Batly lui laisse les périphériques Logitech (les batteries viennent de G HUB). Fermez G HUB pour régler les appareils ici.", "Logitech G HUB läuft: Batly überlässt ihm die Logitech-Geräte (die Akkustände kommen von G HUB). G HUB schließen, um sie hier einzustellen.", "Logitech G HUB está abierto: Batly le deja los dispositivos Logitech (las baterías vienen de G HUB). Cierre G HUB para ajustarlos aquí.", "Logitech G HUB è aperto: Batly gli lascia i dispositivi Logitech (le batterie arrivano da G HUB). Chiudi G HUB per regolarli qui.", "O Logitech G HUB está aberto: o Batly deixa os dispositivos Logitech com ele (as baterias vêm do G HUB). Feche o G HUB para ajustá-los aqui.";
    s_none => "No receiver or device found", "Aucun récepteur ni appareil détecté", "Kein Empfänger oder Gerät gefunden", "No se encontró ningún receptor ni dispositivo", "Nessun ricevitore o dispositivo trovato", "Nenhum receptor ou dispositivo encontrado";
    s_col_slot => "Slot", "Empl.", "Platz", "Ranura", "Slot", "Slot";
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
    s_led => "Lighting", "Éclairage", "Beleuchtung", "Iluminación", "Illuminazione", "Iluminação";
    s_led_speed => "Effect speed", "Vitesse de l'effet", "Effektgeschwindigkeit", "Velocidad del efecto", "Velocità dell'effetto", "Velocidade do efeito";
    s_led_preview => "Preview", "Aperçu", "Vorschau", "Vista previa", "Anteprima", "Pré-visualizar";
    s_led_save => "Save on the device…", "Enregistrer dans l'appareil…", "Im Gerät speichern…", "Guardar en el dispositivo…", "Salva nel dispositivo…", "Guardar no dispositivo…";
    s_led_release => "Give back", "Rendre la main", "Zurückgeben", "Devolver", "Restituisci", "Devolver";
    s_led_diag => "Diagnostic", "Diagnostic", "Diagnose", "Diagnóstico", "Diagnostica", "Diagnóstico";
    s_fx_off => "Off", "Éteint", "Aus", "Apagado", "Spento", "Desligado";
    s_fx_static => "Fixed color", "Couleur fixe", "Feste Farbe", "Color fijo", "Colore fisso", "Cor fixa";
    s_fx_breathe => "Breathing", "Respiration", "Atmen", "Respiración", "Respiro", "Respiração";
    s_fx_cycle => "Color cycle", "Cycle de couleurs", "Farbwechsel", "Ciclo de colores", "Ciclo di colori", "Ciclo de cores";
    s_zone_primary => "Main", "Principale", "Haupt", "Principal", "Principale", "Principal";
    s_zone_logo => "Logo", "Logo", "Logo", "Logotipo", "Logo", "Logótipo";
    s_zone_left => "Left side", "Côté gauche", "Linke Seite", "Lado izquierdo", "Lato sinistro", "Lado esquerdo";
    s_zone_right => "Right side", "Côté droit", "Rechte Seite", "Lado derecho", "Lato destro", "Lado direito";
    s_zone_combined => "All", "Tout", "Alle", "Todo", "Tutto", "Tudo";
    s_zone_other => "Zone {n}", "Zone {n}", "Zone {n}", "Zona {n}", "Zona {n}", "Zona {n}";
    s_led_previewed => "Preview shown. Batly controls the LEDs until you click “Give back” or close this window.", "Aperçu affiché. Batly pilote les LEDs jusqu'à « Rendre la main » ou la fermeture de cette fenêtre.", "Vorschau aktiv. Batly steuert die LEDs, bis Sie auf „Zurückgeben“ klicken oder dieses Fenster schließen.", "Vista previa activa. Batly controla los LED hasta que pulse «Devolver» o cierre esta ventana.", "Anteprima attiva. Batly controlla i LED finché non fai clic su “Restituisci” o chiudi questa finestra.", "Pré-visualização ativa. O Batly controla os LEDs até clicar em “Devolver” ou fechar esta janela.";
    s_led_released => "The device manages its LEDs again.", "L'appareil gère de nouveau ses LEDs.", "Das Gerät steuert seine LEDs wieder selbst.", "El dispositivo vuelve a gestionar sus LED.", "Il dispositivo gestisce di nuovo i suoi LED.", "O dispositivo voltou a gerir os seus LEDs.";
    s_led_saved => "Lighting saved in the device's memory (active profile).", "Éclairage enregistré dans la mémoire de l'appareil (profil actif).", "Beleuchtung im Gerätespeicher gespeichert (aktives Profil).", "Iluminación guardada en la memoria del dispositivo (perfil activo).", "Illuminazione salvata nella memoria del dispositivo (profilo attivo).", "Iluminação guardada na memória do dispositivo (perfil ativo).";
    s_led_save_confirm => "Write this lighting into the active profile stored in “{name}”?\n\nThe device will keep it without any software. The current memory content is saved first in the “backups” folder next to Batly's settings, then read back after writing.\n\nExperimental feature: only devices whose memory matches the known layout are written.", "Écrire cet éclairage dans le profil actif enregistré dans « {name} » ?\n\nL'appareil le gardera sans aucun logiciel. Le contenu actuel de la mémoire est d'abord sauvegardé dans le dossier « backups » à côté des réglages de Batly, puis relu après l'écriture.\n\nFonction expérimentale : seuls les appareils dont la mémoire correspond au format connu sont modifiés.", "Diese Beleuchtung in das aktive Profil von „{name}“ schreiben?\n\nDas Gerät behält sie ohne Software. Der aktuelle Speicherinhalt wird zuvor im Ordner „backups“ neben den Batly-Einstellungen gesichert und nach dem Schreiben erneut gelesen.\n\nExperimentelle Funktion: Nur Geräte, deren Speicher dem bekannten Format entspricht, werden beschrieben.", "¿Escribir esta iluminación en el perfil activo guardado en «{name}»?\n\nEl dispositivo la conservará sin ningún software. El contenido actual de la memoria se guarda antes en la carpeta «backups» junto a los ajustes de Batly y se vuelve a leer después de escribir.\n\nFunción experimental: solo se escriben los dispositivos cuya memoria coincide con el formato conocido.", "Scrivere questa illuminazione nel profilo attivo memorizzato in “{name}”?\n\nIl dispositivo la conserverà senza alcun software. Il contenuto attuale della memoria viene prima salvato nella cartella “backups” accanto alle impostazioni di Batly e riletto dopo la scrittura.\n\nFunzione sperimentale: vengono scritti solo i dispositivi la cui memoria corrisponde al formato noto.", "Gravar esta iluminação no perfil ativo guardado em “{name}”?\n\nO dispositivo vai mantê-la sem qualquer software. O conteúdo atual da memória é primeiro guardado na pasta “backups” junto das configurações do Batly e relido depois da gravação.\n\nFunção experimental: só são gravados os dispositivos cuja memória corresponde ao formato conhecido.";
    s_onb_absent => "No onboard memory: “Save” makes Batly keep this lighting, applied again at every connection while Batly runs.", "Pas de mémoire embarquée : « Enregistrer » fait garder cet éclairage par Batly, réappliqué à chaque connexion tant que Batly tourne.", "Kein interner Speicher: „Speichern“ lässt Batly diese Beleuchtung behalten und bei jeder Verbindung neu anwenden, solange Batly läuft.", "Sin memoria integrada: «Guardar» hace que Batly conserve esta iluminación y la vuelva a aplicar en cada conexión mientras Batly esté abierto.", "Nessuna memoria integrata: “Salva” fa mantenere a Batly questa illuminazione, riapplicata a ogni connessione finché Batly è in esecuzione.", "Sem memória interna: “Guardar” faz o Batly manter esta iluminação, reaplicada a cada ligação enquanto o Batly estiver a correr.";
    s_onb_host => "Onboard profiles are disabled on this device (another software controls it): saving is not possible.", "Les profils embarqués sont désactivés sur cet appareil (un autre logiciel le pilote) : impossible d'enregistrer.", "Die internen Profile sind auf diesem Gerät deaktiviert (eine andere Software steuert es): Speichern nicht möglich.", "Los perfiles integrados están desactivados en este dispositivo (otro software lo controla): no se puede guardar.", "I profili integrati sono disattivati su questo dispositivo (un altro software lo controlla): impossibile salvare.", "Os perfis internos estão desativados neste dispositivo (outro software controla-o): não é possível guardar.";
    s_onb_readonly => "The active profile is a factory one (read only): choose a custom profile on the device to save.", "Le profil actif est un profil d'usine (lecture seule) : choisissez un profil personnalisé sur l'appareil pour enregistrer.", "Das aktive Profil ist ein Werksprofil (schreibgeschützt): Wählen Sie am Gerät ein eigenes Profil zum Speichern.", "El perfil activo es de fábrica (solo lectura): elija un perfil personalizado en el dispositivo para guardar.", "Il profilo attivo è di fabbrica (sola lettura): scegli un profilo personalizzato sul dispositivo per salvare.", "O perfil ativo é de fábrica (só de leitura): escolha um perfil personalizado no dispositivo para guardar.";
    s_onb_unknown => "This device's memory does not match the known layout: Batly does not write to it. “Diagnostic” creates a report to help support it.", "La mémoire de cet appareil ne correspond pas au format connu : Batly n'y écrit pas. « Diagnostic » crée un rapport pour aider à la prendre en charge.", "Der Speicher dieses Geräts entspricht nicht dem bekannten Format: Batly schreibt nicht hinein. „Diagnose“ erstellt einen Bericht für die Unterstützung.", "La memoria de este dispositivo no coincide con el formato conocido: Batly no escribe en ella. «Diagnóstico» crea un informe para ayudar a admitirla.", "La memoria di questo dispositivo non corrisponde al formato noto: Batly non vi scrive. “Diagnostica” crea un rapporto per aiutare a supportarla.", "A memória deste dispositivo não corresponde ao formato conhecido: o Batly não grava nela. “Diagnóstico” cria um relatório para ajudar a suportá-la.";
    s_onb_zone => "Only the first two zones can be saved in a profile.", "Seules les deux premières zones peuvent être enregistrées dans un profil.", "Nur die ersten beiden Zonen können in einem Profil gespeichert werden.", "Solo las dos primeras zonas pueden guardarse en un perfil.", "Solo le prime due zone possono essere salvate in un profilo.", "Só as duas primeiras zonas podem ser guardadas num perfil.";
    s_err_backup => "The memory could not be backed up: nothing was written.", "Impossible de sauvegarder la mémoire : rien n'a été écrit.", "Der Speicher konnte nicht gesichert werden: Es wurde nichts geschrieben.", "No se pudo hacer una copia de la memoria: no se escribió nada.", "Impossibile salvare la memoria: non è stato scritto nulla.", "Não foi possível guardar a memória: nada foi gravado.";
    s_err_verify => "The memory did not read back as written: its previous content was restored.", "La mémoire relue ne correspond pas à l'écriture : son contenu précédent a été restauré.", "Der Speicher entsprach nach dem Schreiben nicht den Daten: Der vorherige Inhalt wurde wiederhergestellt.", "La memoria no coincidía con lo escrito: se restauró su contenido anterior.", "La memoria riletta non corrisponde alla scrittura: il contenuto precedente è stato ripristinato.", "A memória relida não corresponde à gravação: o conteúdo anterior foi restaurado.";
    s_led_restore => "Restore…", "Restaurer…", "Wiederherstellen…", "Restaurar…", "Ripristina…", "Restaurar…";
    s_led_restore_confirm => "Put back the lighting “{name}” had before Batly first changed it?\n\nOnly the lighting is restored, from the oldest backup; the other settings stored in the device stay as they are.", "Remettre l'éclairage qu'avait « {name} » avant la première modification par Batly ?\n\nSeul l'éclairage est restauré, depuis la plus ancienne sauvegarde ; les autres réglages enregistrés dans l'appareil restent tels quels.", "Die Beleuchtung wiederherstellen, die „{name}“ vor der ersten Änderung durch Batly hatte?\n\nNur die Beleuchtung wird aus der ältesten Sicherung wiederhergestellt; die übrigen im Gerät gespeicherten Einstellungen bleiben unverändert.", "¿Volver a la iluminación que tenía «{name}» antes de que Batly la cambiara por primera vez?\n\nSolo se restaura la iluminación, desde la copia más antigua; los demás ajustes guardados en el dispositivo no cambian.", "Ripristinare l'illuminazione che “{name}” aveva prima della prima modifica di Batly?\n\nViene ripristinata solo l'illuminazione, dal backup più vecchio; le altre impostazioni memorizzate nel dispositivo restano invariate.", "Repor a iluminação que “{name}” tinha antes da primeira alteração do Batly?\n\nSó a iluminação é restaurada, a partir da cópia mais antiga; as outras configurações guardadas no dispositivo mantêm-se.";
    s_led_keep_confirm => "Keep this lighting on “{name}”?\n\nThis device cannot store it: Batly applies it now and again at every connection or wake-up, as long as Batly runs on this PC. The LEDs still turn off when the device is idle. “Restore…” stops it.", "Garder cet éclairage sur « {name} » ?\n\nCet appareil ne peut pas l'enregistrer lui-même : Batly l'applique maintenant puis à chaque connexion ou réveil, tant que Batly tourne sur ce PC. Les LEDs s'éteignent toujours quand l'appareil est inactif. « Restaurer… » l'arrête.", "Diese Beleuchtung auf „{name}“ behalten?\n\nDieses Gerät kann sie nicht speichern: Batly wendet sie jetzt und bei jeder Verbindung oder jedem Aufwachen an, solange Batly auf diesem PC läuft. Die LEDs gehen bei Inaktivität weiterhin aus. „Wiederherstellen…“ beendet es.", "¿Mantener esta iluminación en «{name}»?\n\nEste dispositivo no puede guardarla: Batly la aplica ahora y en cada conexión o reactivación, mientras Batly esté abierto en este PC. Los LED se siguen apagando cuando el dispositivo está inactivo. «Restaurar…» lo detiene.", "Mantenere questa illuminazione su “{name}”?\n\nQuesto dispositivo non può memorizzarla: Batly la applica ora e a ogni connessione o risveglio, finché Batly è in esecuzione su questo PC. I LED si spengono comunque quando il dispositivo è inattivo. “Ripristina…” la interrompe.", "Manter esta iluminação em “{name}”?\n\nEste dispositivo não a consegue guardar: o Batly aplica-a agora e a cada ligação ou despertar, enquanto o Batly estiver a correr neste PC. Os LEDs continuam a apagar-se quando o dispositivo está inativo. “Restaurar…” termina-o.";
    s_led_kept => "Lighting kept by Batly: applied again at every connection.", "Éclairage gardé par Batly : réappliqué à chaque connexion.", "Beleuchtung von Batly behalten: bei jeder Verbindung neu angewendet.", "Iluminación mantenida por Batly: se vuelve a aplicar en cada conexión.", "Illuminazione mantenuta da Batly: riapplicata a ogni connessione.", "Iluminação mantida pelo Batly: reaplicada a cada ligação.";
    s_led_forget_confirm => "Stop keeping the lighting of “{name}”?\n\nThe device goes back to its own lighting.", "Ne plus garder l'éclairage de « {name} » ?\n\nL'appareil reprend son propre éclairage.", "Die Beleuchtung von „{name}“ nicht mehr behalten?\n\nDas Gerät kehrt zu seiner eigenen Beleuchtung zurück.", "¿Dejar de mantener la iluminación de «{name}»?\n\nEl dispositivo vuelve a su propia iluminación.", "Smettere di mantenere l'illuminazione di “{name}”?\n\nIl dispositivo torna alla propria illuminazione.", "Deixar de manter a iluminação de “{name}”?\n\nO dispositivo volta à sua própria iluminação.";
    s_led_forgotten => "The device shows its own lighting again.", "L'appareil reprend son propre éclairage.", "Das Gerät zeigt wieder seine eigene Beleuchtung.", "El dispositivo vuelve a mostrar su propia iluminación.", "Il dispositivo mostra di nuovo la propria illuminazione.", "O dispositivo volta a mostrar a sua própria iluminação.";
    s_led_restored => "Original lighting restored.", "Éclairage d'origine restauré.", "Ursprüngliche Beleuchtung wiederhergestellt.", "Iluminación original restaurada.", "Illuminazione originale ripristinata.", "Iluminação original restaurada.";
    s_err_no_backup => "No backup of this device's memory was found.", "Aucune sauvegarde de la mémoire de cet appareil n'a été trouvée.", "Es wurde keine Sicherung des Gerätespeichers gefunden.", "No se encontró ninguna copia de la memoria de este dispositivo.", "Nessun backup della memoria di questo dispositivo trovato.", "Não foi encontrada nenhuma cópia da memória deste dispositivo.";
    s_diag_done => "Diagnostic opened in a text file.", "Diagnostic ouvert dans un fichier texte.", "Diagnose in einer Textdatei geöffnet.", "Diagnóstico abierto en un archivo de texto.", "Diagnostica aperta in un file di testo.", "Diagnóstico aberto num ficheiro de texto.";
}

fn effect_name(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Off => s_fx_off(),
        EffectKind::Static => s_fx_static(),
        EffectKind::Breathe => s_fx_breathe(),
        EffectKind::Cycle => s_fx_cycle(),
    }
}

fn zone_name(location: u16, index: u8) -> String {
    match lighting::location_key(location) {
        "primary" => s_zone_primary().to_string(),
        "logo" => s_zone_logo().to_string(),
        "left" => s_zone_left().to_string(),
        "right" => s_zone_right().to_string(),
        "combined" => s_zone_combined().to_string(),
        _ => s_zone_other().replace("{n}", &(index + 1).to_string()),
    }
}

/// Why the lighting cannot be saved on the device, if it cannot.
fn onboard_reason(status: OnboardStatus) -> Option<&'static str> {
    match status {
        OnboardStatus::Writable { .. } => None,
        OnboardStatus::Absent => Some(s_onb_absent()),
        OnboardStatus::HostMode => Some(s_onb_host()),
        OnboardStatus::ReadOnly => Some(s_onb_readonly()),
        OnboardStatus::UnknownFormat => Some(s_onb_unknown()),
    }
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
        ActionError::Onboard(status) => onboard_reason(status).unwrap_or(s_err_failed()),
        ActionError::Backup => s_err_backup(),
        ActionError::Verify => s_err_verify(),
        ActionError::NoBackup => s_err_no_backup(),
        ActionError::GHub => crate::i18n::tr([
            "Logitech G HUB is running: it manages the lighting of this device. Close G HUB to change it here.",
            "Logitech G HUB est ouvert : c'est lui qui gère l'éclairage de cet appareil. Fermez G HUB pour le régler ici.",
            "Logitech G HUB läuft: Es steuert die Beleuchtung dieses Geräts. G HUB schließen, um sie hier einzustellen.",
            "Logitech G HUB está abierto: gestiona la iluminación de este dispositivo. Cierre G HUB para cambiarla aquí.",
            "Logitech G HUB è aperto: gestisce l'illuminazione di questo dispositivo. Chiudi G HUB per modificarla qui.",
            "O Logitech G HUB está aberto: ele gerencia a iluminação deste dispositivo. Feche o G HUB para alterá-la aqui.",
        ]),
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

text! {
    s_devices => "Devices", "Appareils", "Geräte", "Dispositivos", "Dispositivi", "Dispositivos";
    s_bl_level => "Backlight level", "Niveau du rétroéclairage", "Beleuchtungsstufe", "Nivel de retroiluminación", "Livello di retroilluminazione", "Nível de retroiluminação";
    s_wheel_speed => "SmartShift threshold", "Seuil SmartShift", "SmartShift-Schwelle", "Umbral de SmartShift", "Soglia SmartShift", "Limiar do SmartShift";
    s_zone => "Zone", "Zone", "Zone", "Zona", "Zona", "Zona";
    s_effect => "Effect", "Effet", "Effekt", "Efecto", "Effetto", "Efeito";
    s_color => "Color", "Couleur", "Farbe", "Color", "Colore", "Cor";
    s_cancel => "Cancel", "Annuler", "Abbrechen", "Cancelar", "Annulla", "Cancelar";
}

// --------------------------------------------------------- async plumbing

/// One awaited answer: `poll` gives `Some(None)` when the channel closed or
/// nothing came within [`ANSWER_TIMEOUT`].
struct Pending<T> {
    rx: Receiver<T>,
    deadline: Instant,
}

impl<T> Pending<T> {
    fn new(rx: Receiver<T>) -> Self {
        Self { rx, deadline: Instant::now() + ANSWER_TIMEOUT }
    }

    fn poll(&self) -> Option<Option<T>> {
        match self.rx.try_recv() {
            Ok(v) => Some(Some(v)),
            Err(TryRecvError::Disconnected) => Some(None),
            Err(TryRecvError::Empty) if Instant::now() >= self.deadline => Some(None),
            Err(TryRecvError::Empty) => None,
        }
    }
}

/// Wakes the window (from any thread) when a native device changed.
fn change_notifier(hwnd: HWND, changed: Arc<AtomicBool>) -> Box<dyn Fn() + Send + Sync> {
    let raw = hwnd as isize;
    Box::new(move || {
        changed.store(true, Ordering::Relaxed);
        // SAFETY: plain FFI call; a stale handle is harmless.
        unsafe { InvalidateRect(raw as HWND, null(), 0) };
    })
}

// ------------------------------------------------------------- the window

/// Colour of a message.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Normal,
    Good,
    Warn,
}

/// What a confirmed dialog does.
enum Action {
    /// Receiver id, slot, device name.
    Unpair(String, u8, String),
    Led(Setting),
}

/// A question shown over the window before an action.
struct Confirm {
    title: &'static str,
    text: String,
    danger: bool,
    action: Action,
}

struct Window {
    native: Native,
    hwnd: HWND,
    /// Set by the change notifier of [`Native::watch`].
    changed: Arc<AtomicBool>,
    /// When to read the list again (debounced device changes).
    refresh_at: Option<Instant>,
    /// Whether a first list arrived.
    loaded: bool,
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
    pair_text: String,
    pair_tone: Tone,
    pair_code: String,
    /// Name of the device being unpaired, and whether the answer is awaited.
    unpairing: String,
    unpair_pending: bool,
    /// Lighting being edited: zone index and effect.
    led: Option<(u8, LedEffect)>,
    /// Lighting setting waiting for its answer.
    led_pending: Option<Setting>,
    /// Device whose LEDs Batly controls (preview shown).
    led_preview: Option<String>,
    /// Slider positions moved but not applied yet.
    bl_level: Option<u8>,
    wheel_speed: Option<u8>,
    status: String,
    tone: Tone,
    confirm: Option<Confirm>,
    list_rx: Option<Pending<Vec<ReceiverInfo>>>,
    /// Tag (`seq << 1 | is_set`) and answer of the last settings request.
    settings_rx: Option<(usize, Pending<Result<DeviceSettings, ActionError>>)>,
    pairing_rx: Option<Receiver<PairingEvent>>,
    unpair_rx: Option<Pending<Result<(), ActionError>>>,
    diag_rx: Option<Pending<Result<String, ActionError>>>,
}

/// Opens the window, or brings it to the front if it is already open.
/// Must be called on the UI thread (the one running the message loop).
pub fn open(native: &Native) {
    if crate::egui_host::focus(CLASS) {
        return;
    }
    let changed = Arc::new(AtomicBool::new(false));
    let mut w = Window {
        native: native.clone(),
        hwnd: std::ptr::null_mut(),
        changed: changed.clone(),
        refresh_at: None,
        loaded: false,
        receivers: Vec::new(),
        receiver: None,
        slot: None,
        device: None,
        settings: None,
        seq: 0,
        busy: false,
        pairing: None,
        pairing_final: false,
        pair_text: String::new(),
        pair_tone: Tone::Normal,
        pair_code: String::new(),
        unpairing: String::new(),
        unpair_pending: false,
        led: None,
        led_pending: None,
        led_preview: None,
        bl_level: None,
        wheel_speed: None,
        status: String::new(),
        tone: Tone::Normal,
        confirm: None,
        list_rx: None,
        settings_rx: None,
        pairing_rx: None,
        unpair_rx: None,
        diag_rx: None,
    };
    w.request_list();
    let title = format!("Batly – {}", s_title());
    let opts = crate::egui_host::Options { class: CLASS, title, size: (680.0, 760.0), min_size: (560.0, 480.0), resizable: true };
    if let Some(hwnd) = crate::egui_host::open(opts, Box::new(w)) {
        native.watch(Some(change_notifier(hwnd, changed)));
    }
}

impl Window {
    fn set_status(&mut self, text: &str, tone: Tone) {
        self.status = text.to_string();
        self.tone = tone;
    }

    /// Reads every answer that came in; true while one is still awaited.
    fn poll(&mut self) -> bool {
        if self.changed.swap(false, Ordering::Relaxed) {
            self.refresh_at = Some(Instant::now() + REFRESH_DELAY);
        }
        if self.refresh_at.is_some_and(|at| Instant::now() >= at) {
            self.refresh_at = None;
            self.request_list();
        }
        if let Some(list) = self.list_rx.as_ref().and_then(Pending::poll) {
            self.list_rx = None;
            self.on_list(list.unwrap_or_default());
        }
        if let Some((tag, result)) = self.settings_rx.as_ref().and_then(|(tag, p)| p.poll().map(|r| (*tag, r))) {
            self.settings_rx = None;
            self.on_settings(tag, result);
        }
        while let Some(rx) = &self.pairing_rx {
            match rx.try_recv() {
                Ok(event) => self.on_pairing(Some(event)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.pairing_rx = None;
                    self.on_pairing(None);
                }
            }
        }
        if let Some(result) = self.unpair_rx.as_ref().and_then(Pending::poll) {
            self.unpair_rx = None;
            self.on_unpaired(result);
        }
        if let Some(result) = self.diag_rx.as_ref().and_then(Pending::poll) {
            self.diag_rx = None;
            self.on_diagnose(result);
        }
        self.list_rx.is_some()
            || self.settings_rx.is_some()
            || self.pairing_rx.is_some()
            || self.unpair_rx.is_some()
            || self.diag_rx.is_some()
    }

    // ---------------------------------------------------------- receivers

    fn request_list(&mut self) {
        self.list_rx = Some(Pending::new(self.native.list_receivers()));
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
        self.loaded = true;
        self.receivers = receivers;
        if self.current().is_none() {
            self.receiver = self.receivers.first().map(|r| r.id.clone());
            self.slot = None;
        }
        self.fill_devices();
    }

    fn on_receiver_changed(&mut self, index: usize) {
        let id = self.receivers.get(index).map(|r| r.id.clone());
        if id != self.receiver {
            self.receiver = id;
            self.slot = None;
            self.fill_devices();
        }
    }

    /// Labels of the receivers in the drop-down list.
    fn receiver_labels(&self) -> Vec<String> {
        self.receivers
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let label = match r.kind {
                    ReceiverKind::Direct => format!("{} (USB / Bluetooth)", r.name),
                    _ => r.name.clone(),
                };
                let dup = self.receivers[..i].iter().filter(|o| o.name == r.name).count();
                if dup > 0 {
                    format!("{label} #{}", dup + 1)
                } else {
                    label
                }
            })
            .collect()
    }

    /// Summary of the selected receiver.
    fn receiver_info(&self) -> String {
        if !self.loaded {
            return s_searching().to_string();
        }
        match self.current() {
            None if crate::hidpp::GHUB_RUNNING.load(std::sync::atomic::Ordering::Relaxed) => s_ghub().to_string(),
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
        }
    }

    /// Keeps the selection valid, then reloads what depends on it.
    fn fill_devices(&mut self) {
        let devices: Vec<u8> = self.current().map(|r| r.devices.iter().map(|d| d.slot).collect()).unwrap_or_default();
        if self.slot.is_some_and(|s| !devices.contains(&s)) {
            self.slot = None;
        }
        if self.slot.is_none() {
            self.slot = devices.first().copied();
        }
        self.device_changed(false);
    }

    fn on_select(&mut self, slot: Option<u8>) {
        if slot != self.slot {
            self.slot = slot;
            self.device_changed(true);
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
        self.release_preview();
        self.device = id.clone();
        self.settings = None;
        self.led = None;
        self.bl_level = None;
        self.wheel_speed = None;
        self.busy = false;
        self.seq += 1;
        match (&selected, id) {
            (Some(d), Some(id)) if d.online => {
                self.busy = true;
                self.set_status(s_reading(), Tone::Normal);
                self.settings_rx = Some((self.seq << 1, Pending::new(self.native.get_settings(&id))));
            }
            (Some(_), Some(_)) => self.set_status(s_err_offline(), Tone::Warn),
            (Some(_), None) => self.set_status(s_not_identified(), Tone::Warn),
            (None, _) => self.set_status("", Tone::Normal),
        }
    }

    /// Short description of the selected device in the status line.
    fn show_status_for(&mut self, device: Option<&PairedDevice>) {
        let Some(d) = device else {
            self.set_status("", Tone::Normal);
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
        if self.lighting().is_some_and(|l| l.kept.is_some()) {
            parts.push(s_led_kept().to_string());
        } else if let Some(reason) = self.lighting().and_then(|l| onboard_reason(l.onboard)) {
            parts.push(reason.to_string());
        }
        self.set_status(&parts.join(" · "), Tone::Normal);
    }

    fn pairing_here(&self) -> bool {
        self.pairing.is_some() && self.pairing == self.receiver
    }

    fn can_pair(&self) -> bool {
        (self.current().is_some_and(|r| r.kind.can_pair()) && self.pairing.is_none()) || self.pairing_here()
    }

    fn can_unpair(&self) -> bool {
        self.current().is_some_and(|r| r.kind.can_unpair())
            && self.selected_device().is_some()
            && self.pairing.is_none()
            && !self.unpair_pending
    }

    // ----------------------------------------------------------- settings

    /// Sends a setting of the settings card, unless nothing changes.
    fn send_setting(&mut self, setting: Setting) {
        let Some(current) = self.settings.clone() else { return };
        if self.busy {
            return;
        }
        let Some(device) = self.device.clone() else { return };
        // Nothing to write when the value did not change.
        let unchanged = match setting {
            Setting::Dpi(v) => current.dpi.as_ref().is_some_and(|d| d.current == v),
            Setting::ReportRate(v) => current.report_rate.as_ref().is_some_and(|r| r.current == v),
            Setting::Backlight { mode, level } => {
                current.backlight.as_ref().is_some_and(|b| b.mode == mode && (mode != BacklightMode::Manual || b.level == level))
            }
            Setting::SmartShift(m) => current.smart_shift == Some(m),
            Setting::Led { .. }
            | Setting::LedRelease
            | Setting::LedRestore
            | Setting::LedKeep { .. }
            | Setting::LedForget
            | Setting::Host(_) => false,
        };
        if unchanged {
            self.bl_level = None;
            self.wheel_speed = None;
            return;
        }
        self.seq += 1;
        self.busy = true;
        self.set_status(s_applying(), Tone::Normal);
        self.settings_rx = Some((self.seq << 1 | 1, Pending::new(self.native.set_setting(&device, setting))));
    }

    fn on_settings(&mut self, tag: usize, result: Option<Result<DeviceSettings, ActionError>>) {
        if tag >> 1 != self.seq {
            return; // stale answer (another device was selected meanwhile)
        }
        self.busy = false;
        self.bl_level = None;
        self.wheel_speed = None;
        let was_set = tag & 1 == 1;
        let led = self.led_pending.take();
        if result.as_ref().is_some_and(|r| r.is_err()) && matches!(led, Some(Setting::Led { save: false, .. })) {
            self.led_preview = None;
        }
        match result {
            Some(Ok(s)) => {
                self.settings = Some(s);
                if let Some(setting) = led {
                    if matches!(
                        setting,
                        Setting::Led { save: true, .. } | Setting::LedRestore | Setting::LedKeep { .. } | Setting::LedForget
                    ) {
                        self.led = None; // show what the device now holds
                    }
                    let text = match setting {
                        Setting::Led { save: true, .. } => s_led_saved(),
                        Setting::LedRelease => s_led_released(),
                        Setting::LedRestore => s_led_restored(),
                        Setting::LedKeep { .. } => s_led_kept(),
                        Setting::LedForget => s_led_forgotten(),
                        _ => s_led_previewed(),
                    };
                    self.set_status(text, Tone::Good);
                } else if was_set {
                    self.set_status(s_applied(), Tone::Good);
                } else {
                    let device = self.selected_device().cloned();
                    self.show_status_for(device.as_ref());
                }
            }
            Some(Err(e)) => self.set_status(action_error(e), Tone::Warn),
            None => self.set_status(s_err_gone(), Tone::Warn),
        }
    }

    // ----------------------------------------------------------- lighting

    fn lighting(&self) -> Option<&Lighting> {
        self.settings.as_ref().and_then(|s| s.lighting.as_ref())
    }

    /// Zone and effect shown: the user's edit, or what the device holds.
    fn led_state(&self) -> Option<(u8, LedEffect)> {
        if self.led.is_some() {
            return self.led;
        }
        let light = self.lighting()?;
        if light.kept.is_some() {
            return light.kept;
        }
        let zone = light.zones.first()?;
        Some((zone.index, zone.current.unwrap_or_default()))
    }

    fn writable(&self) -> bool {
        self.lighting().is_some_and(|l| matches!(l.onboard, OnboardStatus::Writable { .. }))
    }

    /// Another zone was chosen: start from what that zone holds.
    fn set_led_zone(&mut self, zone: u8) {
        let Some((_, mut effect)) = self.led_state() else { return };
        if let Some(current) = self.lighting().and_then(|l| l.zones.iter().find(|x| x.index == zone)).and_then(|x| x.current) {
            effect = current;
        }
        let kinds = self.lighting().and_then(|l| l.zones.iter().find(|x| x.index == zone)).map(|x| x.kinds()).unwrap_or_default();
        if !kinds.contains(&effect.kind) {
            effect.kind = kinds.first().copied().unwrap_or(EffectKind::Static);
        }
        self.led = Some((zone, effect));
        if self.writable() && zone as usize >= lighting::PROFILE_ZONES {
            self.set_status(s_onb_zone(), Tone::Warn);
        }
    }

    /// Changes the edited effect of the zone shown.
    fn edit_led(&mut self, f: impl FnOnce(&mut LedEffect)) {
        if let Some((zone, mut effect)) = self.led_state() {
            f(&mut effect);
            self.led = Some((zone, effect));
        }
    }

    /// Sends a lighting setting (preview, save or release).
    fn send_led(&mut self, setting: Setting) {
        let Some(device) = self.device.clone() else { return };
        if self.busy || self.settings.is_none() {
            return;
        }
        match setting {
            Setting::Led { save: false, .. } => self.led_preview = Some(device.clone()),
            Setting::LedRelease | Setting::LedRestore | Setting::LedForget | Setting::LedKeep { .. } | Setting::Led { save: true, .. } => {
                self.led_preview = None
            }
            _ => {}
        }
        self.seq += 1;
        self.busy = true;
        self.led_pending = Some(setting);
        self.set_status(s_applying(), Tone::Normal);
        self.settings_rx = Some((self.seq << 1 | 1, Pending::new(self.native.set_setting(&device, setting))));
    }

    fn led_preview_now(&mut self) {
        if let Some((zone, effect)) = self.led_state() {
            self.send_led(Setting::Led { zone, effect, save: false });
        }
    }

    /// Zone, effect, device name and whether its memory is writable (else
    /// Batly keeps the lighting) and a lighting is kept, for the dialogs.
    fn led_save_target(&self) -> Option<(u8, LedEffect, String, bool, bool)> {
        let (zone, effect) = self.led_state()?;
        let name = self.selected_device().map(|d| d.name.clone()).unwrap_or_default();
        let light = self.lighting()?;
        Some((zone, effect, name, matches!(light.onboard, OnboardStatus::Writable { .. }), light.kept.is_some()))
    }

    /// Asks before saving the lighting (in the device, or kept by Batly).
    fn ask_save(&mut self) {
        let Some((zone, effect, name, writable, _)) = self.led_save_target() else { return };
        let (text, setting) = if writable {
            (s_led_save_confirm(), Setting::Led { zone, effect, save: true })
        } else {
            (s_led_keep_confirm(), Setting::LedKeep { zone, effect })
        };
        self.confirm =
            Some(Confirm { title: s_led_save(), text: text.replace("{name}", &name), danger: false, action: Action::Led(setting) });
    }

    /// Asks before restoring the original lighting (or forgetting the kept one).
    fn ask_restore(&mut self) {
        let Some((_, _, name, writable, kept)) = self.led_save_target() else { return };
        let (text, setting) = if writable {
            (s_led_restore_confirm(), Setting::LedRestore)
        } else if kept {
            (s_led_forget_confirm(), Setting::LedForget)
        } else {
            return;
        };
        self.confirm =
            Some(Confirm { title: s_led_restore(), text: text.replace("{name}", &name), danger: false, action: Action::Led(setting) });
    }

    /// Gives the LEDs of a previewed device back (selection change, close).
    fn release_preview(&mut self) {
        if let Some(device) = self.led_preview.take() {
            let _ = self.native.set_setting(&device, Setting::LedRelease);
        }
    }

    fn diagnose(&mut self) {
        let Some(device) = self.device.clone() else { return };
        if self.busy {
            return;
        }
        self.set_status(s_reading(), Tone::Normal);
        self.diag_rx = Some(Pending::new(self.native.diagnose(&device)));
    }

    fn on_diagnose(&mut self, result: Option<Result<String, ActionError>>) {
        match result {
            Some(Ok(text)) => {
                let path = std::env::temp_dir().join("batly-lighting.txt");
                match std::fs::write(&path, text) {
                    Ok(()) => {
                        crate::win::shell_open(self.hwnd, &path.to_string_lossy(), None);
                        self.set_status(s_diag_done(), Tone::Good);
                    }
                    Err(e) => self.set_status(&e.to_string(), Tone::Warn),
                }
            }
            Some(Err(e)) => self.set_status(action_error(e), Tone::Warn),
            None => self.set_status(s_err_gone(), Tone::Warn),
        }
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
        self.pair_text = text;
        self.pair_tone = Tone::Normal;
        self.pair_code.clear();
        self.pairing = Some(id.clone());
        self.pairing_final = false;
        self.pairing_rx = Some(self.native.pair(&id));
    }

    fn on_pairing(&mut self, event: Option<PairingEvent>) {
        let quote = |s: &str, name: &str| s.replace("{name}", name);
        match event {
            Some(PairingEvent::Waiting) => {}
            Some(PairingEvent::Found { name }) => {
                self.pair_text = quote(s_pair_found(), &name);
                self.pair_tone = Tone::Normal;
            }
            Some(PairingEvent::Passkey { name, code, keyboard }) => {
                self.pair_tone = Tone::Normal;
                if keyboard {
                    self.pair_text = quote(s_pair_keyboard(), &name);
                    let spaced: Vec<String> = code.chars().map(String::from).collect();
                    self.pair_code = spaced.join(" ");
                } else {
                    self.pair_text = quote(s_pair_mouse(), &name).replace("{l}", s_left()).replace("{r}", s_right());
                    let clicks = passkey_clicks(&code).unwrap_or_default();
                    let seq: Vec<&str> = clicks.iter().map(|&right| if right { s_right() } else { s_left() }).collect();
                    self.pair_code = seq.join(" ");
                }
            }
            Some(final_event) => {
                self.pairing_final = true;
                let (text, tone) = match final_event {
                    PairingEvent::Paired { slot, name } => {
                        (s_pair_done().replace("{name}", &name).replace("{slot}", &slot.to_string()), Tone::Good)
                    }
                    PairingEvent::Failed(e) => (s_pair_failed().replace("{reason}", pairing_reason(e)), Tone::Warn),
                    PairingEvent::Timeout => (s_pair_timeout().to_string(), Tone::Warn),
                    _ => (s_pair_cancelled().to_string(), Tone::Normal),
                };
                self.pair_text = text;
                self.pair_tone = tone;
                self.pair_code.clear();
            }
            None => {
                if !self.pairing_final {
                    self.pair_text = s_pair_lost().to_string();
                    self.pair_tone = Tone::Warn;
                    self.pair_code.clear();
                }
                self.pairing = None;
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

    fn ask_unpair(&mut self) {
        let Some((receiver, slot, name)) = self.unpair_target() else { return };
        self.confirm = Some(Confirm {
            title: s_unpair(),
            text: s_unpair_confirm().replace("{name}", &name),
            danger: true,
            action: Action::Unpair(receiver, slot, name),
        });
    }

    fn unpair(&mut self, receiver: &str, slot: u8, name: &str) {
        self.set_status("", Tone::Normal);
        self.unpair_pending = true;
        self.unpair_rx = Some(Pending::new(self.native.unpair(receiver, slot)));
        self.pair_text.clear();
        self.pair_code.clear();
        self.unpairing = name.to_string();
    }

    fn on_unpaired(&mut self, result: Option<Result<(), ActionError>>) {
        let (text, tone) = match result {
            Some(Ok(())) => (s_unpaired().replace("{name}", &self.unpairing), Tone::Good),
            Some(Err(e)) => (action_error(e).to_string(), Tone::Warn),
            None => (s_err_gone().to_string(), Tone::Warn),
        };
        self.pair_text = text;
        self.pair_tone = tone;
        self.unpair_pending = false;
        self.slot = None;
        self.request_list();
    }

    fn run(&mut self, action: Action) {
        match action {
            Action::Unpair(receiver, slot, name) => self.unpair(&receiver, slot, &name),
            Action::Led(setting) => self.send_led(setting),
        }
    }

    // ------------------------------------------------------------ drawing

    fn header(&mut self, ui: &mut egui::Ui) {
        let c = look::colours(ui.ctx());
        look::heading(ui, s_title(), None);
        let labels = self.receiver_labels();
        if !labels.is_empty() {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let mut sel = self.receivers.iter().position(|r| Some(&r.id) == self.receiver.as_ref()).unwrap_or(usize::MAX);
            let enabled = self.pairing.is_none();
            let changed = ui
                .horizontal(|ui| {
                    ui.label(egui::RichText::new(s_receiver()).color(c.sub));
                    ui.add_enabled_ui(enabled, |ui| look::choice(ui, "receiver", &mut sel, &refs, (ui.available_width() - 12.0).min(420.0)))
                        .inner
                })
                .inner;
            if changed {
                self.on_receiver_changed(sel);
            }
        }
        let info = self.receiver_info();
        ui.horizontal(|ui| {
            if !self.loaded {
                ui.spinner();
            }
            ui.label(egui::RichText::new(info).small().color(c.sub));
        });
    }

    fn devices_card(&mut self, ui: &mut egui::Ui) {
        let c = look::colours(ui.ctx());
        let devices: Vec<PairedDevice> = self.current().map(|r| r.devices.clone()).unwrap_or_default();
        let mut clicked = None;
        let mut pair = false;
        let mut unpair = false;
        let (can_pair, can_unpair, pairing_here) = (self.can_pair(), self.can_unpair(), self.pairing_here());
        look::card(ui, Some(s_devices()), |ui| {
            if devices.is_empty() {
                ui.label(egui::RichText::new("—").color(c.sub));
            }
            for d in &devices {
                let active = self.slot == Some(d.slot);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 50.0), egui::Sense::click());
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                let sel = ui.ctx().animate_bool_with_time(resp.id.with("s"), active, 0.18);
                let fill = look::lerp(look::lerp(egui::Color32::TRANSPARENT, c.hover, hover), look::lerp(c.card, c.accent, 0.14), sel);
                let p = ui.painter();
                p.rect_filled(rect, egui::CornerRadius::same(8), fill);
                if sel > 0.0 {
                    let h = 18.0 * sel;
                    p.rect_filled(
                        egui::Rect::from_center_size(egui::pos2(rect.left() + 4.0, rect.center().y), egui::vec2(3.0, h)),
                        egui::CornerRadius::same(2),
                        c.accent,
                    );
                }
                // State on the right: a dot and a word.
                let (state, dot) = if d.online { (s_connected(), c.charge) } else { (s_asleep(), c.sub) };
                let galley = p.layout_no_wrap(state.to_string(), egui::FontId::proportional(12.0), c.sub);
                let state_w = galley.size().x;
                let state_pos = egui::pos2(rect.right() - 12.0 - state_w, rect.center().y - galley.size().y / 2.0);
                p.galley(state_pos, galley, c.sub);
                p.circle_filled(egui::pos2(state_pos.x - 9.0, rect.center().y), 4.0, dot);
                // Name and details on the left, clipped before the state.
                let text_clip = egui::Rect::from_min_max(rect.min, egui::pos2(state_pos.x - 20.0, rect.max.y));
                let p = p.with_clip_rect(text_clip.intersect(p.clip_rect()));
                let font = if active { look::strong(14.0) } else { egui::FontId::proportional(14.0) };
                p.text(egui::pos2(rect.left() + 16.0, rect.center().y - 9.0), egui::Align2::LEFT_CENTER, &d.name, font, c.fg);
                let mut details = Vec::new();
                if d.slot != 0xFF {
                    details.push(format!("{} {}", s_col_slot(), d.slot));
                }
                if let Some(fw) = &d.info.firmware {
                    details.push(format!("{} {fw}", s_col_firmware()));
                }
                if let Some((cur, n)) = d.info.host_channel {
                    details.push(format!("{} {cur} / {n}", s_col_channel()));
                }
                if let Some(kind) = d.kind {
                    details.insert(0, kind_name(kind).to_string());
                }
                p.text(
                    egui::pos2(rect.left() + 16.0, rect.center().y + 10.0),
                    egui::Align2::LEFT_CENTER,
                    details.join(" · "),
                    egui::FontId::proportional(12.0),
                    c.sub,
                );
                if resp.clicked() {
                    clicked = Some(d.slot);
                }
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(can_pair, |ui| {
                    let (text, kind) =
                        if pairing_here { (s_cancel_pair(), look::Kind::Secondary) } else { (s_pair(), look::Kind::Primary) };
                    if look::button(ui, text, kind).clicked() {
                        pair = true;
                    }
                });
                ui.add_enabled_ui(can_unpair, |ui| {
                    if look::button(ui, s_unpair(), look::Kind::Danger).clicked() {
                        unpair = true;
                    }
                });
            });
        });
        if let Some(slot) = clicked {
            self.on_select(Some(slot));
        }
        if pair {
            self.start_or_cancel_pairing();
        }
        if unpair {
            self.ask_unpair();
        }
    }

    fn pairing_card(&mut self, ui: &mut egui::Ui) {
        if self.pair_text.is_empty() {
            return;
        }
        let c = look::colours(ui.ctx());
        let colour = tone_colour(&c, self.pair_tone);
        ui.add_space(8.0);
        look::card(ui, None, |ui| {
            ui.horizontal(|ui| {
                if self.pairing.is_some() && !self.pairing_final {
                    ui.spinner();
                }
                ui.add(egui::Label::new(egui::RichText::new(&self.pair_text).color(colour)).wrap());
            });
            if !self.pair_code.is_empty() {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(look::lerp(c.card, c.accent, 0.12))
                    .stroke(egui::Stroke::new(1.0, look::lerp(c.border, c.accent, 0.5)))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(&self.pair_code).font(look::strong(30.0)).color(c.fg));
                        });
                    });
            }
        });
    }

    fn settings_card(&mut self, ui: &mut egui::Ui) {
        let c = look::colours(ui.ctx());
        let Some(s) = self.settings.clone() else {
            if self.busy {
                ui.add_space(8.0);
                look::card(ui, Some(s_group()), |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new(s_reading()).color(c.sub));
                    });
                });
            }
            return;
        };
        if s.dpi.is_none() && s.report_rate.is_none() && s.backlight.is_none() && s.smart_shift.is_none() {
            return;
        }
        let usable = !self.busy;
        let mut send = None;
        ui.add_space(8.0);
        look::card(ui, Some(s_group()), |ui| {
            ui.add_enabled_ui(usable, |ui| {
                let mut first = true;
                let mut sep = |ui: &mut egui::Ui| {
                    if !first {
                        look::divider(ui);
                    }
                    first = false;
                };
                if let Some(d) = &s.dpi {
                    sep(ui);
                    let mut values = d.choices.clone();
                    if !values.contains(&d.current) {
                        values.push(d.current);
                    }
                    let names: Vec<String> = values.iter().map(u16::to_string).collect();
                    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    let mut sel = values.iter().position(|v| *v == d.current).unwrap_or(usize::MAX);
                    if look::row(ui, s_dpi(), None, |ui| look::choice(ui, "dpi", &mut sel, &refs, 180.0)) {
                        send = values.get(sel).map(|&v| Setting::Dpi(v));
                    }
                }
                if let Some(r) = &s.report_rate {
                    sep(ui);
                    let names: Vec<String> = r.choices.iter().map(|&ms| rate_label(ms)).collect();
                    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    let mut sel = r.choices.iter().position(|v| *v == r.current).unwrap_or(usize::MAX);
                    if look::row(ui, s_rate(), None, |ui| look::choice(ui, "rate", &mut sel, &refs, 180.0)) {
                        send = r.choices.get(sel).map(|&v| Setting::ReportRate(v));
                    }
                }
                if let Some(b) = &s.backlight {
                    sep(ui);
                    let mut modes = b.modes.clone();
                    if !modes.contains(&b.mode) {
                        modes.push(b.mode);
                    }
                    let refs: Vec<&str> = modes.iter().map(|&m| backlight_name(m)).collect();
                    let mut sel = modes.iter().position(|m| *m == b.mode).unwrap_or(usize::MAX);
                    if look::row(ui, s_backlight(), None, |ui| look::choice(ui, "backlight", &mut sel, &refs, 180.0)) {
                        send = modes.get(sel).map(|&mode| Setting::Backlight { mode, level: b.level });
                    }
                    if b.levels > 1 {
                        let manual = b.mode == BacklightMode::Manual;
                        let mut v = self.bl_level.unwrap_or(b.level).min(b.levels - 1);
                        let resp = look::row(ui, s_bl_level(), None, |ui| {
                            ui.add_enabled(manual, egui::Slider::new(&mut v, 0..=b.levels - 1).integer())
                        });
                        if resp.changed() {
                            self.bl_level = Some(v);
                        }
                        if committed(&resp) {
                            send = Some(Setting::Backlight { mode: b.mode, level: v });
                        }
                    }
                }
                if let Some(m) = s.smart_shift {
                    sep(ui);
                    let (current, speed) = match m {
                        WheelMode::FreeSpin => (0, 16),
                        WheelMode::Ratchet => (1, 16),
                        WheelMode::Smart(t) => (2, t.min(WHEEL_MAX)),
                    };
                    let speed = self.wheel_speed.unwrap_or(speed);
                    let mode = |i: usize, speed: u8| match i {
                        0 => WheelMode::FreeSpin,
                        1 => WheelMode::Ratchet,
                        _ => WheelMode::Smart(speed.max(1)),
                    };
                    let mut sel = current;
                    let names = [s_wheel_free(), s_wheel_ratchet(), s_wheel_smart()];
                    if look::row(ui, s_wheel(), None, |ui| look::choice(ui, "wheel", &mut sel, &names, 180.0)) {
                        send = Some(Setting::SmartShift(mode(sel, speed)));
                    }
                    let mut v = speed;
                    let resp = look::row(ui, s_wheel_speed(), None, |ui| {
                        ui.add_enabled(current == 2, egui::Slider::new(&mut v, 1..=WHEEL_MAX).integer())
                    });
                    if resp.changed() {
                        self.wheel_speed = Some(v);
                    }
                    if committed(&resp) {
                        send = Some(Setting::SmartShift(mode(current, v)));
                    }
                }
            });
        });
        if let Some(setting) = send {
            self.send_setting(setting);
        }
    }

    fn lighting_card(&mut self, ui: &mut egui::Ui) {
        let Some(light) = self.lighting().cloned() else { return };
        let usable = !self.busy;
        let (zone, effect) = self.led_state().unwrap_or((0, LedEffect::default()));
        let writable = matches!(light.onboard, OnboardStatus::Writable { .. });
        let kept = light.kept.is_some();
        // Writable memory: saved in the device; otherwise kept by Batly.
        let can_save = !writable || (zone as usize) < lighting::PROFILE_ZONES;
        let previewing = self.led_preview.is_some() && self.led_preview == self.device;
        let mut new_zone = None;
        let mut new_effect = effect;
        let (mut preview, mut save, mut restore, mut release, mut diag) = (false, false, false, false, false);
        ui.add_space(8.0);
        look::card(ui, Some(s_led()), |ui| {
            ui.add_enabled_ui(usable, |ui| {
                let names: Vec<String> = light.zones.iter().map(|z| zone_name(z.location, z.index)).collect();
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let mut sel = light.zones.iter().position(|z| z.index == zone).unwrap_or(usize::MAX);
                if look::row(ui, s_zone(), None, |ui| look::choice(ui, "led-zone", &mut sel, &refs, 180.0)) {
                    new_zone = light.zones.get(sel).map(|z| z.index);
                }
                look::divider(ui);
                let kinds = light.zones.iter().find(|z| z.index == zone).map(|z| z.kinds()).unwrap_or_default();
                let refs: Vec<&str> = kinds.iter().map(|&k| effect_name(k)).collect();
                let mut sel = kinds.iter().position(|k| *k == effect.kind).unwrap_or(usize::MAX);
                if look::row(ui, s_effect(), None, |ui| look::choice(ui, "led-effect", &mut sel, &refs, 180.0)) {
                    if let Some(&k) = kinds.get(sel) {
                        new_effect.kind = k;
                    }
                }
                look::divider(ui);
                let rgb = effect.color & 0xFF_FFFF;
                let mut srgb = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
                look::row(ui, s_color(), None, |ui| {
                    ui.add_enabled_ui(effect.kind.has_color(), |ui| {
                        egui::widgets::color_picker::color_edit_button_srgb(ui, &mut srgb);
                        ui.label(egui::RichText::new(format!("#{rgb:06X}")).monospace());
                    });
                });
                let picked = (srgb[0] as u32) << 16 | (srgb[1] as u32) << 8 | srgb[2] as u32;
                if picked != rgb {
                    new_effect.color = picked;
                }
                look::divider(ui);
                let (min, max) = (PERIOD_MIN_MS / 1000, PERIOD_MAX_MS / 1000);
                let mut secs = effect.period_ms / 1000;
                let resp = look::row(ui, s_led_speed(), None, |ui| {
                    ui.add_enabled(effect.kind.has_speed(), egui::Slider::new(&mut secs, min..=max).integer().suffix(" s"))
                });
                if resp.changed() {
                    new_effect.period_ms = secs.clamp(1, 20) * 1000;
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    preview = look::button(ui, s_led_preview(), look::Kind::Primary).clicked();
                    ui.add_enabled_ui(can_save, |ui| save = look::button(ui, s_led_save(), look::Kind::Secondary).clicked());
                    ui.add_enabled_ui(writable || kept, |ui| restore = look::button(ui, s_led_restore(), look::Kind::Secondary).clicked());
                    ui.add_enabled_ui(previewing, |ui| release = look::button(ui, s_led_release(), look::Kind::Secondary).clicked());
                    diag = look::button(ui, s_led_diag(), look::Kind::Ghost).clicked();
                });
            });
        });
        if let Some(z) = new_zone {
            self.set_led_zone(z);
        } else if new_effect != effect {
            self.edit_led(|e| *e = new_effect);
        }
        if preview {
            self.led_preview_now();
        }
        if save {
            self.ask_save();
        }
        if restore {
            self.ask_restore();
        }
        if release {
            self.send_led(Setting::LedRelease);
        }
        if diag {
            self.diagnose();
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else { return };
        let c = look::colours(ctx);
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("receivers-confirm")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(egui::RichText::new(confirm.title.trim_end_matches('…')).font(look::strong(16.0)).color(c.fg));
            ui.add_space(2.0);
            ui.add(egui::Label::new(egui::RichText::new(&confirm.text).color(c.fg)).wrap());
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let kind = if confirm.danger { look::Kind::Danger } else { look::Kind::Primary };
                if look::button(ui, confirm.title.trim_end_matches('…'), kind).clicked() {
                    answer = Some(true);
                }
                if look::button(ui, s_cancel(), look::Kind::Secondary).clicked() {
                    answer = Some(false);
                }
            });
        });
        if answer.is_none() && modal.should_close() {
            answer = Some(false);
        }
        if let Some(yes) = answer {
            if let Some(confirm) = self.confirm.take() {
                if yes {
                    self.run(confirm.action);
                }
            }
        }
    }

    /// Up / down arrows move the selection in the device list.
    fn arrow_keys(&mut self, ui: &egui::Ui) {
        if self.confirm.is_some() || egui::Popup::is_any_open(ui.ctx()) || ui.ctx().memory(|m| m.focused().is_some()) {
            return;
        }
        let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
        if !up && !down {
            return;
        }
        let slots: Vec<u8> = self.current().map(|r| r.devices.iter().map(|d| d.slot).collect()).unwrap_or_default();
        let at = self.slot.and_then(|s| slots.iter().position(|x| *x == s));
        let next = match (at, up) {
            (Some(i), true) => i.checked_sub(1),
            (Some(i), false) => Some(i + 1),
            (None, _) => Some(0),
        };
        if let Some(&slot) = next.and_then(|i| slots.get(i)) {
            self.on_select(Some(slot));
        }
    }
}

/// Whether a slider was released (mouse) or moved with the keyboard.
fn committed(resp: &egui::Response) -> bool {
    resp.drag_stopped() || (resp.changed() && !resp.dragged())
}

fn tone_colour(c: &look::Colours, tone: Tone) -> egui::Color32 {
    match tone {
        Tone::Normal => c.sub,
        Tone::Good => c.charge,
        Tone::Warn => c.warn,
    }
}

impl crate::egui_host::View for Window {
    fn ui(&mut self, ui: &mut egui::Ui, ctl: &mut crate::egui_host::Ctl) {
        self.hwnd = ctl.hwnd;
        let waiting = self.poll();
        if waiting {
            ui.ctx().request_repaint_after(POLL);
        }
        if let Some(at) = self.refresh_at {
            ui.ctx().request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
        let c = look::colours(ui.ctx());
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.confirm.is_none() && !egui::Popup::is_any_open(ui.ctx()) {
            ctl.close = true;
        }
        self.arrow_keys(ui);
        let footer = 46.0;
        egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 18, bottom: 6 }).show(ui, |ui| self.header(ui));
        let height = (ui.available_height() - footer).max(80.0);
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 4, bottom: 16 }).show(ui, |ui| {
                self.devices_card(ui);
                self.pairing_card(ui);
                let key = (self.receiver.clone(), self.device.clone());
                look::page_in(ui, ("receivers-device", key), |ui| {
                    self.settings_card(ui);
                    self.lighting_card(ui);
                });
            });
        });
        // Status line.
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, c.border));
        egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 12, bottom: 12 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.busy || self.diag_rx.is_some() || self.unpair_rx.is_some() {
                    ui.spinner();
                }
                ui.add(egui::Label::new(egui::RichText::new(&self.status).small().color(tone_colour(&c, self.tone))).truncate());
            });
        });
        self.confirm_dialog(ui.ctx());
    }

    fn closed(&mut self) {
        self.release_preview();
        if let Some(id) = self.pairing.take() {
            self.native.cancel_pairing(&id);
        }
        self.native.watch(None);
    }
}
