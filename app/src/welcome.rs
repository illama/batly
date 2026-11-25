//! Welcome window, shown once at the very first launch (no settings file
//! yet), before the device sources start: which devices to watch, one icon
//! or one per device, which alerts, start with Windows. Everything stays
//! changeable later in the settings. Drawn with egui in the Batly style.

use crate::config::{Config, Welcome};
use crate::i18n::tr;
use crate::look;
use crate::win;
use std::cell::RefCell;
use std::mem::zeroed;
use std::ptr::null_mut;
use std::rc::Rc;
use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, IsWindow, TranslateMessage, MSG};

const CLASS: &str = "BatlyWelcome";

const TITLE: [&str; 6] =
    ["Welcome to Batly", "Bienvenue dans Batly", "Willkommen bei Batly", "Bienvenido a Batly", "Benvenuto in Batly", "Bem-vindo ao Batly"];
const INTRO: [&str; 6] = ["Batly shows the battery of your mice, keyboards, headsets and controllers. A few choices to start (all can be changed later in Settings):", "Batly affiche la batterie de vos souris, claviers, casques et manettes. Quelques choix pour commencer (tout se change ensuite dans les Réglages) :", "Batly zeigt den Akku Ihrer Mäuse, Tastaturen, Headsets und Controller. Ein paar Entscheidungen zum Start (alles später in den Einstellungen änderbar):", "Batly muestra la batería de tus ratones, teclados, auriculares y mandos. Algunas opciones para empezar (todo se puede cambiar luego en Ajustes):", "Batly mostra la batteria di mouse, tastiere, cuffie e controller. Alcune scelte per iniziare (tutto si può cambiare poi nelle Impostazioni):", "O Batly mostra a bateria dos seus ratos, teclados, auscultadores e comandos. Algumas escolhas para começar (tudo pode ser alterado depois nas Configurações):"];
const DEVICES: [&str; 6] = [
    "Devices to watch",
    "Périphériques à surveiller",
    "Zu überwachende Geräte",
    "Dispositivos a vigilar",
    "Dispositivi da monitorare",
    "Dispositivos a monitorizar",
];
const GHUB: [&str; 6] = [
    "Logitech G HUB (when installed)",
    "Logitech G HUB (s'il est installé)",
    "Logitech G HUB (falls installiert)",
    "Logitech G HUB (si está instalado)",
    "Logitech G HUB (se installato)",
    "Logitech G HUB (se instalado)",
];
const BLUETOOTH: [&str; 6] = [
    "Bluetooth devices (earbuds, headsets…)",
    "Appareils Bluetooth (écouteurs, casques…)",
    "Bluetooth-Geräte (Ohrhörer, Headsets…)",
    "Dispositivos Bluetooth (auriculares, cascos…)",
    "Dispositivi Bluetooth (auricolari, cuffie…)",
    "Dispositivos Bluetooth (auriculares, headsets…)",
];
const GAMEPADS: [&str; 6] = [
    "Xbox and PlayStation controllers",
    "Manettes Xbox et PlayStation",
    "Xbox- und PlayStation-Controller",
    "Mandos de Xbox y PlayStation",
    "Controller Xbox e PlayStation",
    "Comandos Xbox e PlayStation",
];
const ICONS: [&str; 6] = ["Tray icons", "Icônes", "Symbole", "Iconos", "Icone", "Ícones"];
const PER_DEVICE: [&str; 6] = [
    "One icon per device I choose",
    "Une icône par appareil choisi",
    "Ein Symbol pro gewähltem Gerät",
    "Un icono por dispositivo elegido",
    "Un'icona per dispositivo scelto",
    "Um ícone por dispositivo escolhido",
];
const SINGLE: [&str; 6] = [
    "A single icon showing the lowest battery",
    "Une seule icône, qui montre la batterie la plus basse",
    "Ein einziges Symbol mit dem niedrigsten Akku",
    "Un solo icono que muestra la batería más baja",
    "Un'unica icona con la batteria più bassa",
    "Um único ícone com a bateria mais baixa",
];
const ALERTS: [&str; 6] = ["Alerts", "Alertes", "Hinweise", "Alertas", "Avvisi", "Alertas"];
const LOW: [&str; 6] = [
    "Warn me before a battery runs out",
    "Me prévenir avant qu'une batterie ne soit vide",
    "Mich warnen, bevor ein Akku leer ist",
    "Avisarme antes de que se agote una batería",
    "Avvisami prima che una batteria si scarichi",
    "Avisar-me antes de uma bateria acabar",
];
const CHARGE_LIMIT: [&str; 6] = [
    "Tell me when a charging device reaches 80 % (better for the battery)",
    "Me dire quand un appareil en charge atteint 80 % (mieux pour la batterie)",
    "Melden, wenn ein ladendes Gerät 80 % erreicht (besser für den Akku)",
    "Avisarme cuando un dispositivo en carga llegue al 80 % (mejor para la batería)",
    "Avvisami quando un dispositivo in carica raggiunge l'80 % (meglio per la batteria)",
    "Avisar-me quando um dispositivo a carregar chegar a 80 % (melhor para a bateria)",
];
const AUTOSTART: [&str; 6] = [
    "Start Batly with Windows",
    "Lancer Batly au démarrage de Windows",
    "Batly mit Windows starten",
    "Iniciar Batly con Windows",
    "Avvia Batly con Windows",
    "Iniciar o Batly com o Windows",
];
const DEFAULTS: [&str; 6] = [
    "Keep the defaults",
    "Garder les réglages par défaut",
    "Standard behalten",
    "Mantener los valores predeterminados",
    "Mantieni le impostazioni predefinite",
    "Manter as predefinições",
];
const START: [&str; 6] = ["Start", "Commencer", "Los geht's", "Empezar", "Inizia", "Começar"];

struct WelcomeView {
    w: Welcome,
    autostart: bool,
    result: Rc<RefCell<Option<(Welcome, bool)>>>,
}

/// Shows the welcome window and waits for it to close. Applies the choices
/// to `cfg` (and to the "start with Windows" entry) and saves them; closing
/// the window keeps the defaults.
pub fn run(cfg: &mut Config, path: &std::path::Path) {
    let initial = (Welcome::from_config(cfg), win::autostart_enabled());
    let result = Rc::new(RefCell::new(None));
    let view = WelcomeView { w: initial.0, autostart: initial.1, result: result.clone() };
    let opts = crate::egui_host::Options {
        class: CLASS,
        title: tr(TITLE).into(),
        size: (560.0, 640.0),
        min_size: (480.0, 520.0),
        resizable: false,
    };
    let Some(hwnd) = crate::egui_host::open(opts, Box::new(view)) else { return };
    // SAFETY: standard message loop until our window is gone.
    unsafe {
        let mut msg: MSG = zeroed();
        while IsWindow(hwnd) != 0 && GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let chosen = result.borrow_mut().take();
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

/// One of the two icon modes, as a selectable tile.
fn tile(ui: &mut egui::Ui, w: f32, selected: bool, title: &str, glyphs: usize) -> egui::Response {
    let c = look::colours(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 92.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let sel = ui.ctx().animate_bool_with_time(resp.id.with("s"), selected, 0.2);
    let fill = look::lerp(look::lerp(c.field, c.hover, hover), look::lerp(c.field, c.accent, 0.12), sel);
    let stroke = look::lerp(look::lerp(c.border, c.sub, hover * 0.5), c.accent, sel);
    let p = ui.painter();
    p.rect(rect, egui::CornerRadius::same(12), fill, egui::Stroke::new(1.0 + sel, stroke), egui::StrokeKind::Inside);
    // Small battery icons, like the tray.
    let n = glyphs as f32;
    let start = rect.center().x - (n * 22.0 - 6.0) / 2.0;
    for i in 0..glyphs {
        let x = start + i as f32 * 22.0;
        let body = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 18.0), egui::vec2(14.0, 24.0));
        p.rect_stroke(body, egui::CornerRadius::same(3), egui::Stroke::new(1.5, c.sub), egui::StrokeKind::Inside);
        let level = [0.85, 0.45, 0.2][i % 3];
        let fill_h = (body.height() - 5.0) * level;
        let colour = if level < 0.3 { c.bad } else { c.accent };
        p.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(body.left() + 2.5, body.bottom() - 2.5 - fill_h),
                egui::pos2(body.right() - 2.5, body.bottom() - 2.5),
            ),
            egui::CornerRadius::same(1),
            colour,
        );
        p.rect_filled(
            egui::Rect::from_center_size(egui::pos2(body.center().x, body.top() - 1.5), egui::vec2(6.0, 3.0)),
            egui::CornerRadius::same(1),
            c.sub,
        );
    }
    let mut job = egui::text::LayoutJob::simple(title.to_string(), egui::FontId::proportional(13.0), c.fg, rect.width() - 24.0);
    job.halign = egui::Align::Center;
    let galley = p.layout_job(job);
    p.galley(egui::pos2(rect.center().x, rect.top() + 50.0), galley, c.fg);
    // Check mark in the corner.
    if sel > 0.0 {
        let centre = egui::pos2(rect.right() - 14.0, rect.top() + 14.0);
        p.circle_filled(centre, 8.0 * sel, c.accent);
        let pts = vec![centre + egui::vec2(-3.5, 0.0), centre + egui::vec2(-1.0, 2.5), centre + egui::vec2(3.5, -2.5)];
        p.add(egui::Shape::line(pts, egui::Stroke::new(1.6, look::lerp(c.accent, c.on_accent, sel))));
    }
    resp
}

impl crate::egui_host::View for WelcomeView {
    fn ui(&mut self, ui: &mut egui::Ui, ctl: &mut crate::egui_host::Ctl) {
        let c = look::colours(ui.ctx());
        let footer = 68.0;
        let height = (ui.available_height() - footer).max(80.0);
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 28, top: 24, bottom: 12 }).show(ui, |ui| {
                look::page_in(ui, "welcome", |ui| {
                    // Logo and title.
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
                        let p = ui.painter();
                        p.rect_filled(rect, egui::CornerRadius::same(14), c.accent);
                        let body = egui::Rect::from_center_size(rect.center() - egui::vec2(2.0, 0.0), egui::vec2(24.0, 14.0));
                        p.rect_stroke(body, egui::CornerRadius::same(3), egui::Stroke::new(2.0, c.on_accent), egui::StrokeKind::Inside);
                        p.rect_filled(
                            egui::Rect::from_center_size(egui::pos2(body.right() + 2.5, body.center().y), egui::vec2(3.0, 6.0)),
                            egui::CornerRadius::same(1),
                            c.on_accent,
                        );
                        let bolt = vec![
                            body.center() + egui::vec2(1.5, -5.0),
                            body.center() + egui::vec2(-3.0, 1.0),
                            body.center() + egui::vec2(1.0, 1.0),
                            body.center() + egui::vec2(-1.5, 5.0),
                        ];
                        p.add(egui::Shape::line(bolt, egui::Stroke::new(2.0, c.on_accent)));
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(tr(TITLE)).font(look::strong(22.0)).color(c.fg));
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(tr(INTRO)).color(c.sub));
                    ui.add_space(8.0);

                    look::card(ui, Some(tr(DEVICES)), |ui| {
                        let rows: [(&str, &mut bool); 5] = [
                            ("Logitech (Unifying, Bolt, Lightspeed, Bluetooth)", &mut self.w.logitech),
                            (tr(GHUB), &mut self.w.ghub),
                            (tr(BLUETOOTH), &mut self.w.bluetooth),
                            (tr(GAMEPADS), &mut self.w.gamepads),
                            ("Razer, Corsair, SteelSeries", &mut self.w.other_brands),
                        ];
                        for (i, (label, value)) in rows.into_iter().enumerate() {
                            if i > 0 {
                                look::divider(ui);
                            }
                            look::row(ui, label, None, |ui| look::switch(ui, value));
                        }
                    });
                    ui.add_space(6.0);
                    look::card(ui, Some(tr(ICONS)), |ui| {
                        let w = (ui.available_width() - 10.0) / 2.0;
                        ui.horizontal(|ui| {
                            if tile(ui, w, !self.w.single_icon, tr(PER_DEVICE), 3).clicked() {
                                self.w.single_icon = false;
                            }
                            if tile(ui, w, self.w.single_icon, tr(SINGLE), 1).clicked() {
                                self.w.single_icon = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    look::card(ui, Some(tr(ALERTS)), |ui| {
                        look::row(ui, tr(LOW), None, |ui| look::switch(ui, &mut self.w.low_alerts));
                        look::divider(ui);
                        look::row(ui, tr(CHARGE_LIMIT), None, |ui| look::switch(ui, &mut self.w.charge_limit));
                        look::divider(ui);
                        look::row(ui, tr(AUTOSTART), None, |ui| look::switch(ui, &mut self.autostart));
                    });
                });
            });
        });
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, c.border));
        egui::Frame::new().inner_margin(egui::Margin::symmetric(28, 16)).show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if look::button(ui, tr(START), look::Kind::Primary).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    *self.result.borrow_mut() = Some((self.w, self.autostart));
                    ctl.close = true;
                }
                if look::button(ui, tr(DEFAULTS), look::Kind::Secondary).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    ctl.close = true;
                }
            });
        });
    }
}
