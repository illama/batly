//! The tray menu, in the Batly style: a flyout drawn with egui that slides
//! in, with animated hover, icons, device levels in colour and check
//! switches. It is built like a classic menu (`AppendMenuW`, which keeps
//! the tray code simple) and read back from the `HMENU`; a click posts
//! [`WM_APP_MENU`] with the command to the tray window.

use crate::icon::{self, IconSpec};
use crate::look;
use crate::model::DeviceKind;
use crate::ui;
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMenuItemCount, GetMenuItemInfoW, PostMessageW, HMENU, MENUITEMINFOW, MFS_CHECKED, MFS_DISABLED, MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID,
    MIIM_STATE, MIIM_STRING, WM_APP,
};

/// Posted to the owner with the command id in `wparam`.
pub const WM_APP_MENU: u32 = WM_APP + 20;
const CLASS: &str = "BatlyMenu";
const WIDTH: f32 = 320.0;
const ITEM_H: f32 = 34.0;
const SEP_H: f32 = 9.0;
const PAD: f32 = 6.0;

/// What is drawn in the left column of an item.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Deco {
    /// A "Segoe MDL2 Assets" icon.
    Glyph(char),
    /// A check box (on or off, from the menu item state).
    Check,
    /// The device icon with its level.
    Device { kind: DeviceKind, percent: Option<f32>, charging: bool, online: bool, has_battery: bool },
}

#[derive(Clone, Debug, Default)]
struct Item {
    id: usize,
    text: String,
    /// Text after the tab, right-aligned.
    right: String,
    separator: bool,
    checked: bool,
    disabled: bool,
    deco: Option<Deco>,
}

thread_local! {
    static DECOS: RefCell<Vec<(usize, Deco)>> = const { RefCell::new(Vec::new()) };
}

/// Gives the item of command `id` an icon; call while building the menu,
/// before [`show`].
pub fn decorate(id: usize, deco: Deco) {
    DECOS.with(|d| d.borrow_mut().push((id, deco)));
}

/// The items of `menu`, with their decorations.
fn items(menu: HMENU) -> Vec<Item> {
    let decos = DECOS.with(|d| std::mem::take(&mut *d.borrow_mut()));
    let mut items = Vec::new();
    // SAFETY: menu handle owned by this thread; buffer sized as declared.
    unsafe {
        let count = GetMenuItemCount(menu).max(0) as u32;
        for i in 0..count {
            let mut buf = [0u16; 512];
            let mut mii: MENUITEMINFOW = zeroed();
            mii.cbSize = size_of::<MENUITEMINFOW>() as u32;
            mii.fMask = MIIM_FTYPE | MIIM_STATE | MIIM_STRING | MIIM_ID;
            mii.dwTypeData = buf.as_mut_ptr();
            mii.cch = buf.len() as u32 - 1;
            if GetMenuItemInfoW(menu, i, 1, &mut mii) == 0 {
                continue;
            }
            let separator = mii.fType & MFT_SEPARATOR != 0;
            let full = String::from_utf16_lossy(&buf[..mii.cch as usize]).replace("&&", "&");
            let (text, right) = match full.split_once('\t') {
                Some((a, b)) => (a.to_string(), b.to_string()),
                None => (full, String::new()),
            };
            let id = mii.wID as usize;
            items.push(Item {
                id,
                text,
                right,
                separator,
                checked: mii.fState & MFS_CHECKED != 0,
                disabled: mii.fState & MFS_DISABLED != 0 || (id == 0 && !separator),
                deco: decos.iter().find(|(i, _)| *i == id && id != 0).map(|(_, d)| *d),
            });
        }
    }
    items
}

struct MenuView {
    owner: HWND,
    items: Vec<Item>,
    /// Keyboard highlight.
    active: Option<usize>,
    icons: Vec<Option<egui::TextureHandle>>,
}

/// Shows `menu` as a flyout at `anchor` (screen point); `owner` receives
/// the chosen command.
pub fn show(owner: HWND, menu: HMENU, anchor: (i32, i32)) {
    let items = items(menu);
    let height = PAD * 2.0 + items.iter().map(|i| if i.separator { SEP_H } else { ITEM_H }).sum::<f32>();
    let icons = vec![None; items.len()];
    crate::egui_host::popup(CLASS, (WIDTH, height), anchor, Box::new(MenuView { owner, items, active: None, icons }));
}

/// Closes the menu flyout if it is open.
pub fn close() {
    if let Some(h) = crate::egui_host::find(CLASS) {
        // SAFETY: our own window.
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(h) };
    }
}

impl MenuView {
    fn icon(&mut self, ctx: &egui::Context, n: usize) -> Option<egui::TextureId> {
        let Some(Deco::Device { kind, percent, charging, online, has_battery }) = self.items[n].deco else { return None };
        if self.icons[n].is_none() {
            let light = !ctx.global_style().visuals.dark_mode;
            let size = 44;
            let spec = IconSpec {
                kind,
                percent: if has_battery { percent.map(f32::round) } else { Some(100.0) },
                charging,
                light_foreground: !light,
                dimmed: !online,
            };
            let rgba = icon::render(&spec, size);
            let image = egui::ColorImage::from_rgba_unmultiplied([size, size], &rgba);
            self.icons[n] = Some(ctx.load_texture(format!("menu-icon-{n}"), image, egui::TextureOptions::LINEAR));
        }
        self.icons[n].as_ref().map(|t| t.id())
    }

    fn choose(&self, n: usize, ctl: &mut crate::egui_host::Ctl) {
        let it = &self.items[n];
        if it.separator || it.disabled {
            return;
        }
        // SAFETY: posting to the tray window.
        unsafe { PostMessageW(self.owner, WM_APP_MENU, it.id, 0) };
        ctl.close = true;
    }
}

impl crate::egui_host::View for MenuView {
    fn ui(&mut self, ui: &mut egui::Ui, ctl: &mut crate::egui_host::Ctl) {
        let c = look::colours(ui.ctx());
        let p = if c.bg == look::colour(ui::LIGHT.bg) { ui::LIGHT } else { ui::DARK };
        // Keyboard: arrows, Enter, Escape.
        let selectable: Vec<usize> = (0..self.items.len()).filter(|&i| !self.items[i].separator && !self.items[i].disabled).collect();
        let (down, up, enter, esc) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if esc {
            ctl.close = true;
            return;
        }
        if (down || up) && !selectable.is_empty() {
            let pos = self.active.and_then(|a| selectable.iter().position(|&s| s == a));
            let next = match (pos, down) {
                (None, true) => 0,
                (None, false) => selectable.len() - 1,
                (Some(p), true) => (p + 1) % selectable.len(),
                (Some(p), false) => (p + selectable.len() - 1) % selectable.len(),
            };
            self.active = Some(selectable[next]);
        }
        if enter {
            if let Some(a) = self.active {
                self.choose(a, ctl);
                return;
            }
        }

        // Opening: fade and slide in.
        let mut chosen = None;
        look::page_in(ui, "menu", |ui| {
            let full = ui.max_rect();
            let mut y = full.top() + PAD;
            let pointer = ui.input(|i| i.pointer.hover_pos());
            // The mouse drives the highlight once it moves (or leaves);
            // the arrows keep it while the mouse stays still.
            let mouse = pointer.is_none() || ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
            if mouse && !(down || up) {
                self.active = None;
            }
            for n in 0..self.items.len() {
                let it = self.items[n].clone();
                if it.separator {
                    let mid = y + SEP_H / 2.0;
                    ui.painter().hline((full.left() + 14.0)..=(full.right() - 14.0), mid, egui::Stroke::new(1.0, c.border));
                    y += SEP_H;
                    continue;
                }
                let rect = egui::Rect::from_min_size(egui::pos2(full.left() + PAD, y), egui::vec2(full.width() - PAD * 2.0, ITEM_H));
                y += ITEM_H;
                let id = egui::Id::unique(("menu-item", n));
                let resp = ui.interact(rect, id, if it.disabled { egui::Sense::hover() } else { egui::Sense::click() });
                if resp.hovered() && !it.disabled && pointer.is_some() {
                    self.active = Some(n);
                }
                let hot = self.active == Some(n) && !it.disabled;
                let h = ui.ctx().animate_bool_with_time(id, hot, 0.1);
                if h > 0.0 {
                    ui.painter().rect_filled(rect, egui::CornerRadius::same(8), look::lerp(egui::Color32::TRANSPARENT, c.hover, h));
                    let bar_h = 14.0 * h;
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(egui::pos2(rect.left() + 2.0, rect.center().y), egui::vec2(3.0, bar_h)),
                        egui::CornerRadius::same(2),
                        c.accent,
                    );
                }
                if !it.disabled {
                    resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    chosen = Some(n);
                }
                // Left column.
                let icon_centre = egui::pos2(rect.left() + 20.0, rect.center().y);
                let fg = if it.disabled { c.sub } else { c.fg };
                match it.deco {
                    Some(Deco::Glyph(g)) if look::has_icons() => {
                        let col = if it.disabled { c.border } else { look::lerp(c.sub, c.accent, h) };
                        ui.painter().text(icon_centre, egui::Align2::CENTER_CENTER, g, look::icon_font(15.0), col);
                    }
                    Some(Deco::Device { .. }) => {
                        if let Some(tex) = self.icon(ui.ctx(), n) {
                            let r = egui::Rect::from_center_size(icon_centre, egui::vec2(22.0, 22.0));
                            ui.painter().image(
                                tex,
                                r,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                        }
                        if it.checked {
                            let dot = icon_centre + egui::vec2(9.0, 8.0);
                            ui.painter().circle_filled(dot, 5.0, look::lerp(c.bg, c.hover, h));
                            ui.painter().circle_filled(dot, 3.2, c.accent);
                        }
                    }
                    Some(Deco::Check) | None if it.checked || it.deco == Some(Deco::Check) => {
                        let r = egui::Rect::from_center_size(icon_centre, egui::vec2(16.0, 16.0));
                        let on = ui.ctx().animate_bool_with_time(id.with("check"), it.checked, 0.15);
                        let border = if it.disabled { c.border } else { look::lerp(look::lerp(c.border, c.sub, 0.6), c.accent, h) };
                        ui.painter().rect(
                            r,
                            egui::CornerRadius::same(4),
                            look::lerp(c.field, c.accent, on),
                            egui::Stroke::new(1.5, look::lerp(border, c.accent, on)),
                            egui::StrokeKind::Inside,
                        );
                        if on > 0.0 {
                            let pts = vec![
                                r.center() + egui::vec2(-4.0, 0.2),
                                r.center() + egui::vec2(-1.2, 3.0),
                                r.center() + egui::vec2(4.0, -2.8),
                            ];
                            ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.8, look::lerp(c.field, c.on_accent, on))));
                        }
                    }
                    _ => {}
                }
                let strong = it.checked && matches!(it.deco, Some(Deco::Device { .. }));
                let font = if strong { look::strong(13.5) } else { egui::FontId::proportional(13.5) };
                let text_pos = egui::pos2(rect.left() + 40.0 + h * 2.0, rect.center().y);
                // Right text: shortcut, or level coloured when low / charging.
                let mut right_w = 0.0;
                if !it.right.is_empty() {
                    let mut col = c.sub;
                    let mut rfont = egui::FontId::proportional(12.5);
                    if let Some(Deco::Device { percent: Some(v), charging, online: true, has_battery: true, .. }) = it.deco {
                        if charging || v <= 30.0 {
                            col = look::colour(p.level(v, charging));
                            rfont = look::strong(12.5);
                        }
                    }
                    let g = ui.painter().layout_no_wrap(it.right.clone(), rfont, col);
                    right_w = g.size().x + 12.0;
                    ui.painter().galley(egui::pos2(rect.right() - 12.0 - g.size().x, rect.center().y - g.size().y / 2.0), g, col);
                }
                let max_w = rect.right() - right_w - text_pos.x - 8.0;
                let mut job = egui::text::LayoutJob::simple_singleline(it.text.clone(), font, fg);
                job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(20.0));
                let g = ui.painter().layout_job(job);
                ui.painter().galley(egui::pos2(text_pos.x, text_pos.y - g.size().y / 2.0), g, fg);
            }
        });
        if let Some(n) = chosen {
            self.choose(n, ctl);
        }
    }
}
