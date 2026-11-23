//! Welcome window, shown once at the very first launch (no settings file
//! yet), before the device sources start: which devices to watch, one icon
//! or one per device, which alerts, start with Windows. Everything stays
//! changeable later in the settings.

use crate::config::{Config, Welcome};
use crate::i18n::tr;
use crate::ui::Scale;
use crate::win::{self, wide};
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetSystemMetrics, IsDialogMessageW, IsWindow,
    LoadCursorW, LoadIconW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, CW_USEDEFAULT,
    IDC_ARROW, MSG, NONCLIENTMETRICSW, SM_CXSCREEN, SM_CYSCREEN, SPI_GETNONCLIENTMETRICS, SWP_NOZORDER, SW_SHOW, WM_CLOSE, WM_COMMAND,
    WM_DESTROY, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT, WS_GROUP, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};

const CLASS: &str = "BatlyWelcome";
const BM_GETCHECK: u32 = 0x00F0;
const BS_PUSHBUTTON: u32 = 0x0;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_AUTOCHECKBOX: u32 = 0x3;
const BS_AUTORADIOBUTTON: u32 = 0x9;
const BM_SETCHECK: u32 = 0x00F1;
const ID_START: usize = 1;
const ID_DEFAULTS: usize = 2;
/// Client size at 96 DPI.
const SIZE: (i32, i32) = (500, 452);

struct State {
    font: HFONT,
    title_font: HFONT,
    /// Check boxes / radio buttons, in `Choice` order.
    boxes: Vec<HWND>,
    /// Result: `Some` once "Start" was clicked.
    chosen: Option<(Welcome, bool)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Shows the welcome window and waits for it to close. Applies the choices
/// to `cfg` (and to the "start with Windows" entry) and saves them; closing
/// the window keeps the defaults.
pub fn run(cfg: &mut Config, path: &std::path::Path) {
    let initial = (Welcome::from_config(cfg), win::autostart_enabled());
    let Some(hwnd) = open(initial) else { return };
    // SAFETY: standard message loop for our window; IsDialogMessageW gives
    // Tab / Enter / Escape their dialog meaning.
    unsafe {
        let mut msg: MSG = zeroed();
        while IsWindow(hwnd) != 0 && GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    let chosen = STATE.with(|s| s.borrow_mut().take().and_then(|st| st.chosen));
    if let Some((welcome, autostart)) = chosen {
        welcome.apply(cfg);
        if autostart != initial.1 {
            win::set_autostart(autostart);
        }
    }
    if let Err(e) = cfg.save(path) {
        crate::log!("cannot save the welcome choices: {e}");
    }
}

fn message_fonts(dpi: u32) -> (HFONT, HFONT) {
    // SAFETY: plain structure with its size set.
    unsafe {
        let mut m: NONCLIENTMETRICSW = zeroed();
        m.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, m.cbSize, (&mut m as *mut NONCLIENTMETRICSW).cast(), 0, dpi);
        let font = CreateFontIndirectW(&m.lfMessageFont);
        let mut big = m.lfMessageFont;
        big.lfHeight = big.lfHeight * 3 / 2;
        big.lfWeight = 600;
        (font, CreateFontIndirectW(&big))
    }
}

fn open(initial: (Welcome, bool)) -> Option<HWND> {
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
    // SAFETY: standard window creation; the children are created below.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS);
        let mut class: WNDCLASSW = zeroed();
        class.lpfnWndProc = Some(wnd_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        class.hIcon = LoadIconW(instance, 1 as _);
        RegisterClassW(&class);
        let title = tr([
            "Welcome to Batly",
            "Bienvenue dans Batly",
            "Willkommen bei Batly",
            "Bienvenido a Batly",
            "Benvenuto in Batly",
            "Bem-vindo ao Batly",
        ]);
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            wide(title).as_ptr(),
            style,
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
            return None;
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = Scale(dpi);
        let px = |v: i32| scale.px(v);
        // Centered on the main screen.
        let mut r = RECT { left: 0, top: 0, right: px(SIZE.0), bottom: px(SIZE.1) };
        AdjustWindowRectExForDpi(&mut r, style, 0, WS_EX_CONTROLPARENT, dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        SetWindowPos(hwnd, null_mut(), (sw - w) / 2, (sh - h) / 2, w, h, SWP_NOZORDER);

        let (font, title_font) = message_fonts(dpi);
        let make = |class: &str, text: &str, style: u32, x: i32, y: i32, cw: i32, ch: i32, id: usize, f: HFONT| -> HWND {
            let c = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                px(x),
                px(y),
                px(cw),
                px(ch),
                hwnd,
                id as _,
                instance,
                null(),
            );
            SendMessageW(c, WM_SETFONT, f as WPARAM, 1);
            c
        };
        let label = |text: &str, y: i32, h: i32| make("STATIC", text, 0, 20, y, SIZE.0 - 40, h, 0, font);
        let heading = |text: &str, y: i32| make("STATIC", text, 0, 20, y, SIZE.0 - 40, 20, 0, title_font);
        let check = |text: &str, y: i32| make("BUTTON", text, BS_AUTOCHECKBOX | WS_TABSTOP, 32, y, SIZE.0 - 52, 22, 0, font);

        make("STATIC", title, 0, 20, 14, SIZE.0 - 40, 30, 0, title_font);
        label(
            tr([
                "Batly shows the battery of your mice, keyboards, headsets and controllers. A few choices to start (all can be changed later in Settings):",
                "Batly affiche la batterie de vos souris, claviers, casques et manettes. Quelques choix pour commencer (tout se change ensuite dans les Réglages) :",
                "Batly zeigt den Akku Ihrer Mäuse, Tastaturen, Headsets und Controller. Ein paar Entscheidungen zum Start (alles später in den Einstellungen änderbar):",
                "Batly muestra la batería de tus ratones, teclados, auriculares y mandos. Algunas opciones para empezar (todo se puede cambiar luego en Ajustes):",
                "Batly mostra la batteria di mouse, tastiere, cuffie e controller. Alcune scelte per iniziare (tutto si può cambiare poi nelle Impostazioni):",
                "O Batly mostra a bateria dos seus ratos, teclados, auscultadores e comandos. Algumas escolhas para começar (tudo pode ser alterado depois nas Configurações):",
            ]),
            48,
            40,
        );

        heading(
            tr([
                "Devices to watch",
                "Périphériques à surveiller",
                "Zu überwachende Geräte",
                "Dispositivos a vigilar",
                "Dispositivi da monitorare",
                "Dispositivos a monitorizar",
            ]),
            96,
        );
        let mut boxes = vec![
            check("Logitech (Unifying, Bolt, Lightspeed, Bluetooth)", 120),
            check(
                tr([
                    "Logitech G HUB (when installed)",
                    "Logitech G HUB (s'il est installé)",
                    "Logitech G HUB (falls installiert)",
                    "Logitech G HUB (si está instalado)",
                    "Logitech G HUB (se installato)",
                    "Logitech G HUB (se instalado)",
                ]),
                142,
            ),
            check(
                tr([
                    "Bluetooth devices (earbuds, headsets…)",
                    "Appareils Bluetooth (écouteurs, casques…)",
                    "Bluetooth-Geräte (Ohrhörer, Headsets…)",
                    "Dispositivos Bluetooth (auriculares, cascos…)",
                    "Dispositivi Bluetooth (auricolari, cuffie…)",
                    "Dispositivos Bluetooth (auriculares, headsets…)",
                ]),
                164,
            ),
            check(
                tr([
                    "Xbox and PlayStation controllers",
                    "Manettes Xbox et PlayStation",
                    "Xbox- und PlayStation-Controller",
                    "Mandos de Xbox y PlayStation",
                    "Controller Xbox e PlayStation",
                    "Comandos Xbox e PlayStation",
                ]),
                186,
            ),
            check("Razer, Corsair, SteelSeries", 208),
        ];

        heading(tr(["Tray icons", "Icônes", "Symbole", "Iconos", "Icone", "Ícones"]), 240);
        boxes.push(make(
            "BUTTON",
            tr([
                "One icon per device I choose",
                "Une icône par appareil choisi",
                "Ein Symbol pro gewähltem Gerät",
                "Un icono por dispositivo elegido",
                "Un'icona per dispositivo scelto",
                "Um ícone por dispositivo escolhido",
            ]),
            BS_AUTORADIOBUTTON | WS_TABSTOP | WS_GROUP,
            32,
            264,
            SIZE.0 - 52,
            22,
            0,
            font,
        ));
        boxes.push(make(
            "BUTTON",
            tr([
                "A single icon showing the lowest battery",
                "Une seule icône, qui montre la batterie la plus basse",
                "Ein einziges Symbol mit dem niedrigsten Akku",
                "Un solo icono que muestra la batería más baja",
                "Un'unica icona con la batteria più bassa",
                "Um único ícone com a bateria mais baixa",
            ]),
            BS_AUTORADIOBUTTON,
            32,
            286,
            SIZE.0 - 52,
            22,
            0,
            font,
        ));

        heading(tr(["Alerts", "Alertes", "Hinweise", "Alertas", "Avvisi", "Alertas"]), 318);
        let alerts = make(
            "BUTTON",
            tr([
                "Warn me before a battery runs out",
                "Me prévenir avant qu'une batterie ne soit vide",
                "Mich warnen, bevor ein Akku leer ist",
                "Avisarme antes de que se agote una batería",
                "Avvisami prima che una batteria si scarichi",
                "Avisar-me antes de uma bateria acabar",
            ]),
            BS_AUTOCHECKBOX | WS_TABSTOP | WS_GROUP,
            32,
            342,
            SIZE.0 - 52,
            22,
            0,
            font,
        );
        boxes.push(alerts);
        boxes.push(check(
            tr([
                "Tell me when a charging device reaches 80 % (better for the battery)",
                "Me dire quand un appareil en charge atteint 80 % (mieux pour la batterie)",
                "Melden, wenn ein ladendes Gerät 80 % erreicht (besser für den Akku)",
                "Avisarme cuando un dispositivo en carga llegue al 80 % (mejor para la batería)",
                "Avvisami quando un dispositivo in carica raggiunge l'80 % (meglio per la batteria)",
                "Avisar-me quando um dispositivo a carregar chegar a 80 % (melhor para a bateria)",
            ]),
            364,
        ));
        boxes.push(check(
            tr([
                "Start Batly with Windows",
                "Lancer Batly au démarrage de Windows",
                "Batly mit Windows starten",
                "Iniciar Batly con Windows",
                "Avvia Batly con Windows",
                "Iniciar o Batly com o Windows",
            ]),
            386,
        ));

        let (w0, auto) = initial;
        let states = [
            w0.logitech,
            w0.ghub,
            w0.bluetooth,
            w0.gamepads,
            w0.other_brands,
            !w0.single_icon,
            w0.single_icon,
            w0.low_alerts,
            w0.charge_limit,
            auto,
        ];
        for (b, on) in boxes.iter().zip(states) {
            SendMessageW(*b, BM_SETCHECK, on as usize, 0);
        }

        let defaults = tr([
            "Keep the defaults",
            "Garder les réglages par défaut",
            "Standard behalten",
            "Mantener los valores predeterminados",
            "Mantieni le impostazioni predefinite",
            "Manter as predefinições",
        ]);
        make("BUTTON", defaults, BS_PUSHBUTTON | WS_TABSTOP | WS_GROUP, SIZE.0 - 400, SIZE.1 - 44, 230, 30, ID_DEFAULTS, font);
        let start = make(
            "BUTTON",
            tr(["Start", "Commencer", "Los geht's", "Empezar", "Inizia", "Começar"]),
            BS_DEFPUSHBUTTON | WS_TABSTOP,
            SIZE.0 - 160,
            SIZE.1 - 44,
            140,
            30,
            ID_START,
            font,
        );

        STATE.with(|s| *s.borrow_mut() = Some(State { font, title_font, boxes, chosen: None }));
        crate::theme::apply(hwnd);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(start);
        Some(hwnd)
    }
}

/// Reads the boxes back.
fn choices() -> Option<(Welcome, bool)> {
    STATE.with(|s| {
        let s = s.borrow();
        let st = s.as_ref()?;
        // SAFETY: our own check boxes.
        let on = |i: usize| st.boxes.get(i).is_some_and(|&b| unsafe { SendMessageW(b, BM_GETCHECK, 0, 0) } == 1);
        let welcome = Welcome {
            logitech: on(0),
            ghub: on(1),
            bluetooth: on(2),
            gamepads: on(3),
            other_brands: on(4),
            single_icon: on(6),
            low_alerts: on(7),
            charge_limit: on(8),
        };
        Some((welcome, on(9)))
    })
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(r) = crate::theme::on_message(hwnd, msg, wparam, lparam) {
        return r;
    }
    match msg {
        WM_COMMAND => {
            match wparam & 0xFFFF {
                ID_START => {
                    let chosen = choices();
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.chosen = chosen;
                        }
                    });
                    DestroyWindow(hwnd);
                }
                // "Keep the defaults", or Escape (IDCANCEL, same value).
                ID_DEFAULTS => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            STATE.with(|s| {
                if let Some(st) = s.borrow().as_ref() {
                    DeleteObject(st.font as _);
                    DeleteObject(st.title_font as _);
                }
            });
            // `run`'s loop ends as IsWindow turns false; no WM_QUIT, which
            // would also end the application's own loop later.
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
