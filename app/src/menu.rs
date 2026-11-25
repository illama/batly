//! The tray menu in the Batly style: owner-drawn items on the panel
//! colours, a rounded highlight under the mouse, accent check marks and the
//! text after a tab (shortcut, level) right-aligned and dimmed.
//!
//! [`style`] turns the items of a menu built with `AppendMenuW` into
//! owner-drawn ones; [`on_message`], called first by the window that owns
//! the menu, measures and draws them.

use crate::gfx::Canvas;
use crate::ui::{self, Font, Scale};
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetDC, GetTextExtentPoint32W, ReleaseDC, SelectObject, DT_LEFT, DT_RIGHT, DT_VCENTER, HBRUSH, HDC,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMenuItemCount, GetMenuItemInfoW, SetMenuInfo, SetMenuItemInfoW, HMENU, MENUINFO, MENUITEMINFOW, MFT_OWNERDRAW, MFT_SEPARATOR,
    MIIM_DATA, MIIM_FTYPE, MIIM_STATE, MIIM_STRING, MIM_BACKGROUND, WM_DRAWITEM, WM_MEASUREITEM,
};

const ODT_MENU: u32 = 1;
const ODS_SELECTED: u32 = 0x0001;
const ODS_GRAYED: u32 = 0x0002;
const ODS_DISABLED: u32 = 0x0004;
const ODS_CHECKED: u32 = 0x0008;

#[derive(Clone, Default)]
struct Item {
    text: String,
    /// Text after the tab, right-aligned.
    right: String,
    separator: bool,
}

struct Menus {
    items: Vec<Item>,
    brush: HBRUSH,
    owner: HWND,
}

thread_local! {
    static MENUS: RefCell<Option<Menus>> = const { RefCell::new(None) };
}

/// Owner-draws every item of `menu`, shown by `owner`.
pub fn style(menu: HMENU, owner: HWND) {
    let p = ui::palette();
    let mut items = Vec::new();
    // SAFETY: menu handle owned by this thread; buffers sized as declared.
    unsafe {
        let count = GetMenuItemCount(menu).max(0) as u32;
        for i in 0..count {
            let mut buf = [0u16; 512];
            let mut mii: MENUITEMINFOW = zeroed();
            mii.cbSize = size_of::<MENUITEMINFOW>() as u32;
            mii.fMask = MIIM_FTYPE | MIIM_STATE | MIIM_STRING;
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
            items.push(Item { text, right, separator });
            let mut set: MENUITEMINFOW = zeroed();
            set.cbSize = size_of::<MENUITEMINFOW>() as u32;
            set.fMask = MIIM_FTYPE | MIIM_DATA;
            set.fType = MFT_OWNERDRAW | if separator { MFT_SEPARATOR } else { 0 };
            set.dwItemData = items.len();
            SetMenuItemInfoW(menu, i, 1, &set);
        }
        let brush = CreateSolidBrush(p.bg);
        let mut info: MENUINFO = zeroed();
        info.cbSize = size_of::<MENUINFO>() as u32;
        info.fMask = MIM_BACKGROUND;
        info.hbrBack = brush;
        SetMenuInfo(menu, &info);
        MENUS.with(|m| {
            if let Some(old) = m.borrow_mut().replace(Menus { items, brush, owner }) {
                windows_sys::Win32::Graphics::Gdi::DeleteObject(old.brush as _);
            }
        });
    }
}

#[repr(C)]
struct MeasureItem {
    ctl_type: u32,
    ctl_id: u32,
    item_id: u32,
    item_width: u32,
    item_height: u32,
    item_data: usize,
}

#[repr(C)]
struct DrawItem {
    ctl_type: u32,
    ctl_id: u32,
    item_id: u32,
    item_action: u32,
    item_state: u32,
    hwnd_item: HWND,
    hdc: HDC,
    rc_item: RECT,
    item_data: usize,
}

fn item(data: usize) -> Option<(Item, HWND)> {
    MENUS.with(|m| m.borrow().as_ref().and_then(|m| data.checked_sub(1).and_then(|i| m.items.get(i)).map(|it| (it.clone(), m.owner))))
}

fn width(dc: HDC, font: &Font, s: &str) -> i32 {
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut size = SIZE { cx: 0, cy: 0 };
    // SAFETY: valid DC and font; the old font is restored.
    unsafe {
        let old = SelectObject(dc, font.0 as _);
        GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32, &mut size);
        SelectObject(dc, old);
    }
    size.cx
}

/// Measures and draws the owner-drawn menu items.
///
/// # Safety
/// The arguments must be those the window procedure received.
pub unsafe fn on_message(_hwnd: HWND, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_MEASUREITEM => {
            let mi = &mut *(lparam as *mut MeasureItem);
            if mi.ctl_type != ODT_MENU {
                return None;
            }
            let (it, owner) = item(mi.item_data)?;
            let s = Scale::of(owner);
            if it.separator {
                mi.item_height = s.px(9) as u32;
                mi.item_width = s.px(40) as u32;
                return Some(1);
            }
            let font = Font::new(s, 13, 400);
            let dc = GetDC(null_mut());
            let w = width(dc, &font, &it.text) + if it.right.is_empty() { 0 } else { s.px(32) + width(dc, &font, &it.right) };
            ReleaseDC(null_mut(), dc);
            mi.item_width = (w + s.px(36 + 18)) as u32;
            mi.item_height = s.px(32) as u32;
            Some(1)
        }
        WM_DRAWITEM => {
            let di = &*(lparam as *const DrawItem);
            if di.ctl_type != ODT_MENU {
                return None;
            }
            let (it, owner) = item(di.item_data)?;
            draw(di, &it, Scale::of(owner));
            Some(1)
        }
        _ => None,
    }
}

unsafe fn draw(di: &DrawItem, it: &Item, s: Scale) {
    let p = ui::palette();
    let r = di.rc_item;
    let Some(mut cv) = Canvas::new(r, p.bg) else { return };
    if it.separator {
        let mid = (r.top + r.bottom) / 2;
        cv.fill(RECT { left: r.left + s.px(12), top: mid, right: r.right - s.px(12), bottom: mid + 1 }, p.border);
        cv.blit(di.hdc);
        return;
    }
    let disabled = di.item_state & (ODS_GRAYED | ODS_DISABLED) != 0;
    let selected = di.item_state & ODS_SELECTED != 0;
    let checked = di.item_state & ODS_CHECKED != 0;
    if selected && !disabled {
        let pill = RECT { left: r.left + s.px(4), top: r.top + s.px(1), right: r.right - s.px(4), bottom: r.bottom - s.px(1) };
        cv.round_rect(pill, s.px(6) as f32, p.hover, 1.0, None);
    }
    if checked {
        let size = s.px(16);
        let top = r.top + (r.bottom - r.top - size) / 2;
        let b = RECT { left: r.left + s.px(12), top, right: r.left + s.px(12) + size, bottom: top + size };
        cv.round_rect(b, s.px(4) as f32, if disabled { p.border } else { p.accent }, 1.0, None);
        cv.check(b, p.on_accent, s.px(2) as f32);
    }
    cv.blit(di.hdc);
    let font = Font::new(s, 13, if checked { 600 } else { 400 });
    let fg = if disabled { p.sub } else { p.fg };
    ui::text(di.hdc, &font, fg, &it.text, RECT { left: r.left + s.px(38), right: r.right - s.px(14), ..r }, DT_LEFT | DT_VCENTER);
    if !it.right.is_empty() {
        let small = Font::new(s, 13, 400);
        ui::text(di.hdc, &small, p.sub, &it.right, RECT { left: r.left + s.px(38), right: r.right - s.px(16), ..r }, DT_RIGHT | DT_VCENTER);
    }
}

/// Frees what [`style`] kept, once the menu is closed.
pub fn release() {
    MENUS.with(|m| {
        if let Some(old) = m.borrow_mut().take() {
            // SAFETY: the menu that used the brush is destroyed.
            unsafe { windows_sys::Win32::Graphics::Gdi::DeleteObject(old.brush as _) };
        }
    });
}
