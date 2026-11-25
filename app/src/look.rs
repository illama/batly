//! The Batly look for egui windows: the palette of [`crate::ui`], Segoe UI
//! (the Windows font, when present), rounded widgets, and a few animated
//! components: switch, tabs with a sliding underline, cards, buttons,
//! fading pages.

use crate::ui::{self, Palette};
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Rect, Response, Sense, Stroke, TextStyle, Ui, Vec2};
use std::sync::Arc;

/// Whether the Windows icon font was found (Segoe Fluent Icons / MDL2).
static ICONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn has_icons() -> bool {
    ICONS.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn colour(c: u32) -> Color32 {
    Color32::from_rgb((c & 0xFF) as u8, (c >> 8 & 0xFF) as u8, (c >> 16 & 0xFF) as u8)
}

/// Palette in egui colours.
#[derive(Clone, Copy)]
pub struct Colours {
    pub bg: Color32,
    pub fg: Color32,
    pub sub: Color32,
    pub hover: Color32,
    pub card: Color32,
    pub field: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub charge: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

impl Colours {
    pub fn of(p: &Palette) -> Self {
        Self {
            bg: colour(p.bg),
            fg: colour(p.fg),
            sub: colour(p.sub),
            hover: colour(p.hover),
            card: colour(p.card),
            field: if p.light { Color32::WHITE } else { colour(ui::mix(p.card, p.hover, 0.5)) },
            border: colour(p.border),
            accent: colour(p.accent),
            on_accent: colour(p.on_accent),
            charge: colour(p.charge),
            warn: colour(p.warn),
            bad: colour(p.bad),
        }
    }
}

/// Colours of the current theme of `ctx`.
pub fn colours(ctx: &egui::Context) -> Colours {
    Colours::of(if ctx.global_style().visuals.dark_mode { &ui::DARK } else { &ui::LIGHT })
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// Segoe UI (regular and semibold) and the Windows icon font, when found.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
    let load = |names: &[&str]| names.iter().find_map(|n| std::fs::read(dir.join(n)).ok());
    if let Some(regular) = load(&["segoeui.ttf"]) {
        fonts.font_data.insert("segoe".into(), Arc::new(egui::FontData::from_owned(regular)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "segoe".into());
    }
    let semibold = load(&["seguisb.ttf", "segoeuib.ttf"]);
    let strong = FontFamily::Name("strong".into());
    let mut strong_list = Vec::new();
    if let Some(data) = semibold {
        fonts.font_data.insert("segoe-semibold".into(), Arc::new(egui::FontData::from_owned(data)));
        strong_list.push("segoe-semibold".to_string());
    }
    strong_list.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(strong, strong_list);
    let icons = FontFamily::Name("icons".into());
    let mut icon_list = Vec::new();
    if let Some(data) = load(&["SegoeIcons.ttf", "segmdl2.ttf"]) {
        fonts.font_data.insert("icons".into(), Arc::new(egui::FontData::from_owned(data)));
        ICONS.store(true, std::sync::atomic::Ordering::Relaxed);
        icon_list.push("icons".to_string());
    }
    icon_list.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(icons, icon_list);
    ctx.set_fonts(fonts);
}

pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("strong".into()))
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

/// Visuals and spacing for the light or dark theme.
pub fn apply(ctx: &egui::Context, light: bool) {
    let c = Colours::of(if light { &ui::LIGHT } else { &ui::DARK });
    let mut style = (*ctx.global_style()).clone();
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    let r = CornerRadius::same(8);
    v.panel_fill = c.bg;
    v.window_fill = c.card;
    v.extreme_bg_color = c.field;
    v.faint_bg_color = c.card;
    v.override_text_color = Some(c.fg);
    v.hyperlink_color = c.accent;
    v.selection.bg_fill = lerp(c.field, c.accent, 0.35);
    v.selection.stroke = Stroke::new(1.0, c.accent);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0, c.border);
    v.window_shadow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(if light { 40 } else { 110 }) };
    v.popup_shadow = v.window_shadow;
    v.text_cursor.stroke = Stroke::new(2.0, c.accent);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = c.card;
    w.noninteractive.weak_bg_fill = c.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, c.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, c.fg);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = c.field;
    w.inactive.weak_bg_fill = c.field;
    w.inactive.bg_stroke = Stroke::new(1.0, c.border);
    w.inactive.fg_stroke = Stroke::new(1.5, c.fg);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = c.hover;
    w.hovered.weak_bg_fill = c.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, lerp(c.border, c.accent, 0.6));
    w.hovered.fg_stroke = Stroke::new(1.5, c.fg);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = lerp(c.hover, c.accent, 0.2);
    w.active.weak_bg_fill = lerp(c.hover, c.accent, 0.2);
    w.active.bg_stroke = Stroke::new(1.0, c.accent);
    w.active.fg_stroke = Stroke::new(1.5, c.fg);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open.bg_fill = c.hover;
    w.open.weak_bg_fill = c.hover;
    w.open.bg_stroke = Stroke::new(1.0, c.accent);
    w.open.corner_radius = r;
    style.visuals = v;
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.interact_size = Vec2::new(40.0, 32.0);
    style.spacing.combo_height = 320.0;
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.slider_width = 220.0;
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    style.animation_time = 0.18;
    // Labels behave like an application's, not like a document.
    style.interaction.selectable_labels = false;
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Heading, strong(22.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
    ]
    .into();
    ctx.set_global_style(style);
}

// -------------------------------------------------------------- components

/// Animated on/off switch.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let c = colours(ui.ctx());
    let size = Vec2::new(40.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.16);
    let hover = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    let enabled = ui.is_enabled();
    let off_fill = lerp(c.field, c.hover, hover);
    let fill = lerp(off_fill, if enabled { c.accent } else { c.border }, t);
    let stroke = lerp(lerp(c.border, c.sub, 0.4 + 0.3 * hover), fill, t);
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(11), fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let r = 7.0 + 1.5 * hover;
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    let knob = lerp(lerp(c.sub, c.fg, hover), c.on_accent, t);
    p.circle_filled(egui::pos2(x, rect.center().y), r, knob);
    if resp.has_focus() {
        p.rect_stroke(rect.expand(3.0), CornerRadius::same(14), Stroke::new(2.0, c.accent), egui::StrokeKind::Outside);
    }
    resp
}

/// A row: label on the left (with an optional hint under it), the control
/// on the right.
pub fn row<R>(ui: &mut Ui, label: &str, hint: Option<&str>, control: impl FnOnce(&mut Ui) -> R) -> R {
    let c = colours(ui.ctx());
    let height = if hint.is_some() { 44.0 } else { 36.0 };
    let width = ui.available_width();
    ui.allocate_ui_with_layout(Vec2::new(width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(Vec2::new(width, height));
        match hint {
            None => {
                ui.label(egui::RichText::new(label).color(c.fg));
            }
            Some(h) => {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add_space((height - 34.0) / 2.0);
                    ui.label(egui::RichText::new(label).color(c.fg));
                    ui.label(egui::RichText::new(h).small().color(c.sub));
                });
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control).inner
    })
    .inner
}

/// A card: rounded surface with a border; `title` above it in small caps.
pub fn card<R>(ui: &mut Ui, title: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let c = colours(ui.ctx());
    if let Some(t) = title {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(t.to_uppercase()).font(strong(11.5)).color(c.sub));
        ui.add_space(-4.0);
    }
    egui::Frame::new()
        .fill(c.card)
        .stroke(Stroke::new(1.0, c.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Thin separator inside a card.
pub fn divider(ui: &mut Ui) {
    let c = colours(ui.ctx());
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, c.border);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
    Ghost,
}

/// Rounded button with an animated hover and press.
pub fn button(ui: &mut Ui, text: &str, kind: Kind) -> Response {
    let c = colours(ui.ctx());
    let font = strong(13.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, c.fg);
    let size = Vec2::new(galley.size().x + 32.0, 34.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let enabled = ui.is_enabled();
    let hover = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered() && enabled, 0.12);
    let press = ui.ctx().animate_bool_with_time(resp.id.with("p"), resp.is_pointer_button_down_on() && enabled, 0.06);
    let (base, text_col, stroke) = match kind {
        Kind::Primary => (c.accent, c.on_accent, None),
        Kind::Danger => (c.bad, Color32::WHITE, None),
        Kind::Secondary => (c.field, c.fg, Some(c.border)),
        Kind::Ghost => (Color32::TRANSPARENT, c.accent, None),
    };
    let hover_fill = match kind {
        Kind::Primary | Kind::Danger => lerp(base, c.fg, 0.12),
        _ => c.hover,
    };
    let mut fill = lerp(base, hover_fill, hover);
    fill = lerp(fill, lerp(base, c.bg, 0.3), press);
    let (fill, text_col, stroke) = if enabled { (fill, text_col, stroke) } else { (c.field, lerp(c.sub, c.field, 0.25), Some(c.border)) };
    let r = rect.shrink(press * 1.0);
    let p = ui.painter();
    p.rect(
        r,
        CornerRadius::same(8),
        fill,
        stroke.map_or(Stroke::NONE, |s| Stroke::new(1.0, lerp(s, c.sub, hover * 0.5))),
        egui::StrokeKind::Inside,
    );
    if resp.has_focus() {
        p.rect_stroke(rect.expand(2.0), CornerRadius::same(10), Stroke::new(2.0, c.accent), egui::StrokeKind::Outside);
    }
    let pos = r.center() - galley.size() / 2.0;
    p.galley_with_override_text_color(pos, galley, text_col);
    resp
}

/// Small round icon button (a Segoe MDL2 glyph).
pub fn icon_button(ui: &mut Ui, glyph: char, tooltip: &str) -> Response {
    let c = colours(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tooltip);
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() && ui.is_enabled(), 0.12);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(8), lerp(Color32::TRANSPARENT, c.hover, hover));
    let col = if ui.is_enabled() { lerp(c.sub, c.accent, hover) } else { c.border };
    if has_icons() {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, glyph, icon_font(15.0), col);
    } else {
        p.circle_filled(rect.center(), 3.0, col);
    }
    resp
}

/// Tabs with an underline that slides to the selected one.
pub fn tabs(ui: &mut Ui, selected: &mut usize, labels: &[String]) -> bool {
    let c = colours(ui.ctx());
    let mut changed = false;
    let mut rects = Vec::new();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in labels.iter().enumerate() {
            let active = *selected == i;
            let font = if active { strong(14.0) } else { FontId::proportional(14.0) };
            let galley = ui.painter().layout_no_wrap(label.clone(), font, c.fg);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 24.0, 36.0), Sense::click());
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() && !active {
                *selected = i;
                changed = true;
            }
            let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
            if hover > 0.0 && !active {
                ui.painter().rect_filled(
                    rect.shrink2(Vec2::new(0.0, 3.0)),
                    CornerRadius::same(8),
                    lerp(Color32::TRANSPARENT, c.hover, hover),
                );
            }
            let col = if active { c.fg } else { lerp(c.sub, c.fg, hover) };
            ui.painter().galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, col);
            rects.push(rect);
        }
    });
    // Sliding underline.
    if let Some(target) = rects.get(*selected) {
        let id = ui.scope_id().with("tab-underline");
        let x = ui.ctx().animate_value_with_time(id.with("x"), target.center().x, 0.22);
        let w = ui.ctx().animate_value_with_time(id.with("w"), target.width() - 28.0, 0.22);
        let y = target.bottom() - 2.0;
        ui.painter().rect_filled(Rect::from_center_size(egui::pos2(x, y), Vec2::new(w.max(8.0), 3.0)), CornerRadius::same(2), c.accent);
    }
    let full = ui.available_rect_before_wrap();
    ui.painter().hline(full.x_range(), ui.min_rect().bottom() + 0.5, Stroke::new(1.0, c.border));
    ui.add_space(6.0);
    changed
}

/// Fades and slides in the content of a page that just appeared; `key`
/// changes when the page changes.
pub fn page_in<R>(ui: &mut Ui, key: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui) -> R) -> R {
    let id = egui::Id::unique(("page-in", key));
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let start = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    let t = ((now - start) as f32 / 0.24).clamp(0.0, 1.0);
    let e = 1.0 - (1.0 - t).powi(3);
    if t < 1.0 {
        ctx.request_repaint();
    }
    ui.scope(|ui| {
        ui.multiply_opacity(e);
        ui.add_space((1.0 - e) * 10.0);
        add(ui)
    })
    .inner
}

/// Forgets the start time of the page animations of `ui`, so the next page
/// shown animates again.
pub fn reset_page(ctx: &egui::Context, key: impl std::hash::Hash + std::fmt::Debug) {
    ctx.data_mut(|d| d.remove::<f64>(egui::Id::unique(("page-in", key))));
}

/// Drop-down list of `options`; returns whether the choice changed.
pub fn choice(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, selected: &mut usize, options: &[&str], width: f32) -> bool {
    let before = *selected;
    let text = options.get(*selected).copied().unwrap_or("");
    egui::ComboBox::from_id_salt(id).width(width).selected_text(text).icon(chevron).show_ui(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for (i, o) in options.iter().enumerate() {
            ui.selectable_value(selected, i, *o);
        }
    });
    *selected != before
}

/// Chevron of the drop-down lists; turns over while the list is open.
fn chevron(ui: &Ui, rect: Rect, visuals: &egui::style::WidgetVisuals, open: bool) {
    let t = ui.ctx().animate_bool_with_time(ui.next_auto_id().with("chevron"), open, 0.2);
    let c = rect.center();
    let s = 4.0;
    let dir = 1.0 - 2.0 * t;
    let pts =
        vec![egui::pos2(c.x - s, c.y - s * 0.5 * dir), egui::pos2(c.x, c.y + s * 0.5 * dir), egui::pos2(c.x + s, c.y - s * 0.5 * dir)];
    ui.painter().add(egui::Shape::line(pts, Stroke::new(1.6, visuals.fg_stroke.color)));
}

/// Text field of a given width.
pub fn field(ui: &mut Ui, text: &mut String, width: f32, password: bool) -> Response {
    ui.add(egui::TextEdit::singleline(text).desired_width(width).password(password).margin(Margin::symmetric(10, 7)))
}

/// Section title of a page.
pub fn heading(ui: &mut Ui, text: &str, sub: Option<&str>) {
    let c = colours(ui.ctx());
    ui.label(egui::RichText::new(text).font(strong(20.0)).color(c.fg));
    if let Some(s) = sub {
        ui.label(egui::RichText::new(s).color(c.sub));
    }
    ui.add_space(4.0);
}
