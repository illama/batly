//! Light and dark theme for the windows built from standard controls
//! (Settings, Receivers and devices), following the Windows app theme.
//!
//! * [`init`] lets the process use dark common controls and menus
//!   (uxtheme `SetPreferredAppMode`, Windows 10 1903 and later; the tray
//!   menu follows too).
//! * [`apply`] gives a window a dark or light title bar, rounded corners
//!   and the Mica backdrop (Windows 11), and themes every child control.
//! * [`on_message`], called first by the window procedure, paints the
//!   background and the labels, check boxes and radio buttons (whose text
//!   the dark theme leaves black), and re-applies the theme when the user
//!   switches between light and dark.
//!
//! Group boxes and tab controls are drawn by a subclass: neither has a
//! dark style of its own.

use crate::gfx::Canvas;
use crate::ui::{self, palette, Palette};
use crate::win::wide;
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, GetWindowDC,
    InvalidateRect, RedrawWindow, ReleaseDC, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, DT_CALCRECT, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HFONT, NULL_BRUSH, PAINTSTRUCT, PS_SOLID,
    RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::{
    SetWindowTheme, CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT, CDRF_SKIPDEFAULT, NMCUSTOMDRAW,
    NMHDR, NM_CUSTOMDRAW, WM_MOUSELEAVE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetClientRect, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, LoadCursorW,
    SendMessageW, SetCursor, GWL_STYLE, IDC_HAND, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC,
    WM_DRAWITEM, WM_ENABLE, WM_ERASEBKGND, WM_GETFONT, WM_KILLFOCUS, WM_MEASUREITEM, WM_MOUSEMOVE, WM_NCPAINT, WM_NOTIFY, WM_PAINT,
    WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_THEMECHANGED,
};

const BM_GETCHECK: u32 = 0x00F0;
const BS_TYPEMASK: u32 = 0xF;
const BS_CHECKBOX: u32 = 0x2;
const BS_AUTOCHECKBOX: u32 = 0x3;
const BS_RADIOBUTTON: u32 = 0x4;
const BS_3STATE: u32 = 0x5;
const BS_AUTO3STATE: u32 = 0x6;
const BS_GROUPBOX: u32 = 0x7;
const BS_AUTORADIOBUTTON: u32 = 0x9;
const LVM_GETHEADER: u32 = 0x101F;
const LVM_SETBKCOLOR: u32 = 0x1001;
const LVM_SETTEXTCOLOR: u32 = 0x1024;
const LVM_SETTEXTBKCOLOR: u32 = 0x1026;
const TCM_GETITEMCOUNT: u32 = 0x1304;
const TCM_GETITEMRECT: u32 = 0x130A;
const TCM_GETCURSEL: u32 = 0x130B;
const TCM_GETITEMW: u32 = 0x133C;
const TCIF_TEXT: u32 = 1;
const SUBCLASS_ID: usize = 0xBA71;
const HAND_ID: usize = 0xBA72;
const COMBO_ID: usize = 0xBA73;
const EDIT_ID: usize = 0xBA74;
const CB_GETCURSEL: u32 = 0x0147;
const CB_GETLBTEXT: u32 = 0x0148;
const CB_GETLBTEXTLEN: u32 = 0x0149;
const CB_GETDROPPEDSTATE: u32 = 0x0157;
const CB_GETCOMBOBOXINFO: u32 = 0x0164;
const BS_PUSHBUTTON: u32 = 0x0;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const ODT_COMBOBOX: u32 = 3;
const ODS_SELECTED: u32 = 0x0001;
const ODS_COMBOBOXEDIT: u32 = 0x1000;
/// Style to add to the drop-down lists so that their items are drawn by
/// [`on_message`] (`CBS_OWNERDRAWFIXED | CBS_HASSTRINGS`).
pub const CBS_THEMED: u32 = 0x0010 | 0x0200;

/// Background of the windows and of the input fields, per theme.
struct Brushes {
    light: bool,
    window: HBRUSH,
    field: HBRUSH,
}

thread_local! {
    static BRUSHES: RefCell<Option<Brushes>> = const { RefCell::new(None) };
}

/// Brushes of the current theme (recreated when it changes).
fn brushes(p: &Palette) -> (HBRUSH, HBRUSH) {
    BRUSHES.with(|b| {
        let mut b = b.borrow_mut();
        if b.as_ref().is_none_or(|b| b.light != p.light) {
            // SAFETY: brushes we own; the old ones are no longer selected.
            unsafe {
                if let Some(old) = b.take() {
                    DeleteObject(old.window);
                    DeleteObject(old.field);
                }
                *b = Some(Brushes { light: p.light, window: CreateSolidBrush(p.bg), field: CreateSolidBrush(field_color(p)) });
            }
        }
        b.as_ref().map_or((0 as _, 0 as _), |b| (b.window, b.field))
    })
}

/// Input fields: white on light, a bit lighter than the window on dark.
fn field_color(p: &Palette) -> COLORREF {
    if p.light {
        ui::rgb(0xFF, 0xFF, 0xFF)
    } else {
        p.card
    }
}

// ------------------------------------------------- undocumented uxtheme

/// Windows build number (`RtlGetVersion`, which does not lie about it).
fn build_number() -> u32 {
    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    // SAFETY: RtlGetVersion fills the structure whose size we give.
    unsafe {
        let ntdll = LoadLibraryW(wide("ntdll.dll").as_ptr());
        let Some(f) = GetProcAddress(ntdll, c"RtlGetVersion".as_ptr().cast()) else { return 0 };
        let f: unsafe extern "system" fn(*mut OsVersionInfo) -> i32 = std::mem::transmute(f);
        let mut info: OsVersionInfo = zeroed();
        info.size = size_of::<OsVersionInfo>() as u32;
        if f(&mut info) == 0 {
            info.build
        } else {
            0
        }
    }
}

/// uxtheme function exported by ordinal only (stable since 1903).
fn uxtheme_ordinal(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    if build_number() < 18362 {
        return None;
    }
    // SAFETY: uxtheme is always present; an ordinal is a valid name pointer.
    unsafe { GetProcAddress(LoadLibraryW(wide("uxtheme.dll").as_ptr()), ordinal as *const u8) }
}

/// Lets the process use dark controls and menus when the app theme is
/// dark. Call once, before creating windows.
pub fn init() {
    // 135: SetPreferredAppMode(1 = AllowDark), 136: FlushMenuThemes.
    // SAFETY: signatures documented by the Windows SDK headers of Wine and
    // by every dark mode implementation (Notepad++, win32-darkmode).
    unsafe {
        if let Some(f) = uxtheme_ordinal(135) {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set(1);
        }
        if let Some(f) = uxtheme_ordinal(136) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

fn allow_dark(hwnd: HWND, dark: bool) {
    // 133: AllowDarkModeForWindow(hwnd, allow).
    if let Some(f) = uxtheme_ordinal(133) {
        // SAFETY: see `init`.
        unsafe {
            let allow: unsafe extern "system" fn(HWND, BOOL) -> BOOL = std::mem::transmute(f);
            allow(hwnd, dark as BOOL);
        }
    }
}

// ----------------------------------------------------------- applying

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: the buffer size is given.
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize]).to_ascii_lowercase()
}

fn button_type(hwnd: HWND) -> u32 {
    // SAFETY: plain query.
    unsafe { GetWindowLongW(hwnd, GWL_STYLE) as u32 & BS_TYPEMASK }
}

/// `None` puts back the control's default (light) theme.
fn set_theme(hwnd: HWND, name: Option<&str>) {
    let name = name.map(wide);
    // SAFETY: NUL-terminated name or null; null sub-app list.
    unsafe { SetWindowTheme(hwnd, name.as_ref().map_or(null(), |n| n.as_ptr()), null()) };
}

/// Title bar, corners and backdrop of a top-level window; theme of all its
/// children. Call after creating the controls, and again on a theme change
/// ([`on_message`] does it).
pub fn apply(hwnd: HWND) {
    let p = palette();
    let dark = !p.light;
    frame(hwnd, p.border);
    allow_dark(hwnd, dark);
    unsafe extern "system" fn each(child: HWND, lparam: LPARAM) -> BOOL {
        theme_child(child, lparam != 0);
        1
    }
    // SAFETY: the callback only touches the child it is given.
    unsafe {
        EnumChildWindows(hwnd, Some(each), dark as LPARAM);
        RedrawWindow(hwnd, null(), 0 as _, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
    }
}

/// Dark or light title bar, rounded corners and Mica (Windows 11; the
/// attributes are ignored by older versions).
pub fn frame(hwnd: HWND, border: COLORREF) {
    const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
    const DWMSBT_MAINWINDOW: u32 = 2; // Mica
    ui::modern_frame(hwnd, border);
    // SAFETY: attribute value valid for its size.
    unsafe { DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, (&DWMSBT_MAINWINDOW as *const u32).cast(), 4) };
}

fn theme_child(child: HWND, dark: bool) {
    allow_dark(child, dark);
    let class = class_of(child);
    let explorer = dark.then_some("DarkMode_Explorer");
    match class.as_str() {
        "button" => match button_type(child) {
            BS_GROUPBOX => subclass(child),
            _ => {
                set_theme(child, explorer);
                // SAFETY: the procedure lives as long as the program.
                unsafe { SetWindowSubclass(child, Some(hand_proc), HAND_ID, 0) };
            }
        },
        "edit" => {
            set_theme(child, dark.then_some("DarkMode_CFD"));
            // SAFETY: the procedure lives as long as the program.
            unsafe { SetWindowSubclass(child, Some(edit_proc), EDIT_ID, 0) };
        }
        "combobox" => {
            set_theme(child, dark.then_some("DarkMode_CFD"));
            combo_list_frame(child);
            // SAFETY: the procedure lives as long as the program.
            unsafe { SetWindowSubclass(child, Some(combo_proc), COMBO_ID, 0) };
        }
        "systabcontrol32" => subclass(child),
        "syslistview32" => {
            // SAFETY: the procedure lives as long as the program.
            unsafe { SetWindowSubclass(child, Some(edit_proc), EDIT_ID, 0) };
            let p = palette();
            set_theme(child, Some(explorer.unwrap_or("Explorer")));
            // SAFETY: list view messages with plain values.
            unsafe {
                SendMessageW(child, LVM_SETBKCOLOR, 0, field_color(&p) as LPARAM);
                SendMessageW(child, LVM_SETTEXTBKCOLOR, 0, field_color(&p) as LPARAM);
                SendMessageW(child, LVM_SETTEXTCOLOR, 0, p.fg as LPARAM);
                let header = SendMessageW(child, LVM_GETHEADER, 0, 0) as HWND;
                if !header.is_null() {
                    allow_dark(header, dark);
                    set_theme(header, Some(if dark { "DarkMode_ItemsView" } else { "ItemsView" }));
                }
            }
        }
        "scrollbar" | "msctls_updown32" => set_theme(child, explorer),
        "msctls_trackbar32" => {
            // SAFETY: the procedure lives as long as the program.
            unsafe { SetWindowSubclass(child, Some(hand_proc), HAND_ID, 0) };
        }
        _ => {}
    }
    // SAFETY: plain call.
    unsafe { InvalidateRect(child, null(), 1) };
}

fn subclass(hwnd: HWND) {
    // SAFETY: the subclass procedure lives as long as the program; calling
    // it twice for the same id only updates it.
    unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) };
}

// ----------------------------------------------------------- messages

/// Handles the theme part of a window's messages; `None` = not handled,
/// go on with the window's own handling.
///
/// # Safety
/// The arguments must be those the window procedure received.
pub unsafe fn on_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let p = palette();
    match msg {
        WM_ERASEBKGND => {
            let (window, _) = brushes(&p);
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            FillRect(wparam as HDC, &r, window);
            Some(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORDLG => {
            let dc = wparam as HDC;
            let (window, _) = brushes(&p);
            SetTextColor(dc, p.fg);
            SetBkColor(dc, p.bg);
            Some(window as LRESULT)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let dc = wparam as HDC;
            let (_, field) = brushes(&p);
            SetTextColor(dc, p.fg);
            SetBkColor(dc, field_color(&p));
            Some(field as LRESULT)
        }
        WM_NOTIFY => {
            let hdr = &*(lparam as *const NMHDR);
            if hdr.code != NM_CUSTOMDRAW {
                return None;
            }
            let class = class_of(hdr.hwndFrom);
            if class == "msctls_trackbar32" {
                return Some(draw_trackbar(hdr.hwndFrom, &*(lparam as *const NMCUSTOMDRAW), &p));
            }
            if class != "button" {
                return None;
            }
            let cd = &*(lparam as *const NMCUSTOMDRAW);
            let kind = button_type(hdr.hwndFrom);
            let check = matches!(kind, BS_CHECKBOX | BS_AUTOCHECKBOX | BS_3STATE | BS_AUTO3STATE);
            let radio = matches!(kind, BS_RADIOBUTTON | BS_AUTORADIOBUTTON);
            let push = matches!(kind, BS_PUSHBUTTON | BS_DEFPUSHBUTTON);
            if cd.dwDrawStage != CDDS_PREPAINT || !(check || radio || push) {
                return Some(CDRF_DODEFAULT as LRESULT);
            }
            if push {
                draw_push(hdr.hwndFrom, cd, kind == BS_DEFPUSHBUTTON, &p);
            } else {
                draw_check(hdr.hwndFrom, cd, radio, &p);
            }
            Some(CDRF_SKIPDEFAULT as LRESULT)
        }
        WM_MEASUREITEM => {
            let mi = &mut *(lparam as *mut MeasureItem);
            if mi.ctl_type != ODT_COMBOBOX {
                return None;
            }
            let scale = ui::Scale::of(hwnd);
            mi.item_height = if mi.item_id == u32::MAX { scale.px(18) } else { scale.px(30) } as u32;
            Some(1)
        }
        WM_DRAWITEM => {
            let di = &*(lparam as *const DrawItem);
            if di.ctl_type != ODT_COMBOBOX {
                return None;
            }
            draw_combo_item(di, &p);
            Some(1)
        }
        WM_SETTINGCHANGE if lparam != 0 && wide_eq(lparam as *const u16, "ImmersiveColorSet") => {
            apply(hwnd);
            None
        }
        WM_THEMECHANGED => {
            apply(hwnd);
            None
        }
        _ => None,
    }
}

unsafe fn wide_eq(mut ptr: *const u16, s: &str) -> bool {
    for c in s.encode_utf16() {
        if *ptr != c {
            return false;
        }
        ptr = ptr.add(1);
    }
    *ptr == 0
}

fn window_text(hwnd: HWND) -> Vec<u16> {
    // SAFETY: the buffer is one longer than the text.
    unsafe {
        let n = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
        buf.truncate(got);
        buf
    }
}

/// Check box or radio button in the Batly style: a rounded box (or a
/// circle) filled with the accent colour when checked, the text in our
/// colour.
unsafe fn draw_check(hwnd: HWND, cd: &NMCUSTOMDRAW, radio: bool, p: &Palette) {
    let dc = cd.hdc;
    let r = cd.rc;
    let scale = ui::Scale::of(hwnd);
    let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) == 1;
    let disabled = cd.uItemState & CDIS_DISABLED != 0;
    let pressed = cd.uItemState & CDIS_SELECTED != 0;
    let hot = cd.uItemState & CDIS_HOT != 0;
    let focus = cd.uItemState & CDIS_FOCUS != 0;
    let size = scale.px(18);
    let top = r.top + (r.bottom - r.top - size) / 2;
    let glyph = RECT { left: r.left + scale.px(1), top, right: r.left + scale.px(1) + size, bottom: top + size };
    let Some(mut cv) = Canvas::new(RECT { right: glyph.right + scale.px(2), ..r }, p.bg) else { return };
    let accent = if disabled {
        p.border
    } else if pressed {
        ui::mix(p.accent, p.fg, 0.2)
    } else {
        p.accent
    };
    let radius = if radio { size as f32 / 2.0 } else { scale.px(5) as f32 };
    if checked {
        cv.round_rect(glyph, radius, accent, 1.0, None);
        if radio {
            let c = scale.px(9) / 2;
            cv.circle((glyph.left + glyph.right) / 2, (glyph.top + glyph.bottom) / 2, c as f32, p.on_accent, 1.0);
        } else {
            cv.check(glyph, p.on_accent, scale.px(2) as f32);
        }
    } else {
        let border = if disabled {
            p.border
        } else if hot || focus {
            p.accent
        } else {
            ui::mix(p.border, p.sub, 0.5)
        };
        cv.round_rect(glyph, radius, ui::mix(field_color(p), p.hover, if hot { 1.0 } else { 0.0 }), 1.0, Some(border));
    }
    cv.blit(dc);
    let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
    let old = SelectObject(dc, font as _);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, if disabled { p.sub } else { p.fg });
    let mut text = window_text(hwnd);
    let mut tr = RECT { left: glyph.right + scale.px(8), ..r };
    let (window, _) = brushes(p);
    FillRect(dc, &tr, window);
    DrawTextW(dc, text.as_mut_ptr(), text.len() as i32, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    SelectObject(dc, old);
}

/// Push button: rounded, the default one filled with the accent colour,
/// the others on the card colour; hover, pressed and focus states.
unsafe fn draw_push(hwnd: HWND, cd: &NMCUSTOMDRAW, default: bool, p: &Palette) {
    let dc = cd.hdc;
    let r = cd.rc;
    let scale = ui::Scale::of(hwnd);
    let disabled = cd.uItemState & CDIS_DISABLED != 0;
    let pressed = cd.uItemState & CDIS_SELECTED != 0;
    let hot = cd.uItemState & CDIS_HOT != 0;
    let focus = cd.uItemState & CDIS_FOCUS != 0;
    let Some(mut cv) = Canvas::new(r, p.bg) else { return };
    let radius = scale.px(6) as f32;
    let (fill, border, text) = if disabled {
        (field_color(p), Some(p.border), p.sub)
    } else if default {
        let base = p.accent;
        let fill = if pressed {
            ui::mix(base, p.bg, 0.25)
        } else if hot {
            ui::mix(base, p.fg, 0.12)
        } else {
            base
        };
        (fill, None, p.on_accent)
    } else {
        let fill = if pressed {
            ui::mix(p.hover, p.border, 0.6)
        } else if hot {
            p.hover
        } else {
            field_color(p)
        };
        (fill, Some(if hot { ui::mix(p.border, p.sub, 0.4) } else { p.border }), p.fg)
    };
    let inner = RECT { left: r.left + 1, top: r.top + 1, right: r.right - 1, bottom: r.bottom - 1 };
    cv.round_rect(inner, radius, fill, 1.0, border);
    if focus && !disabled {
        cv.outline(r, radius + 1.0, if default { ui::mix(p.accent, p.fg, 0.4) } else { p.accent }, scale.px(2).max(1) as f32);
    }
    cv.blit(dc);
    let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
    let old = SelectObject(dc, font as _);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, text);
    let mut label = window_text(hwnd);
    let mut tr = r;
    if pressed {
        tr.top += 1;
        tr.bottom += 1;
    }
    DrawTextW(dc, label.as_mut_ptr(), label.len() as i32, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(dc, old);
}

/// Slider: a thin rounded track, filled with the accent colour up to a
/// round thumb; no tick marks.
unsafe fn draw_trackbar(hwnd: HWND, cd: &NMCUSTOMDRAW, p: &Palette) -> LRESULT {
    const CDDS_ITEMPREPAINT: u32 = 0x0001_0001;
    const CDRF_NOTIFYITEMDRAW: u32 = 0x20;
    const TBCD_TICS: usize = 1;
    const TBCD_THUMB: usize = 2;
    const TBCD_CHANNEL: usize = 3;
    const TBM_GETTHUMBRECT: u32 = 0x0419;
    if cd.dwDrawStage == CDDS_PREPAINT {
        return CDRF_NOTIFYITEMDRAW as LRESULT;
    }
    if cd.dwDrawStage != CDDS_ITEMPREPAINT {
        return CDRF_DODEFAULT as LRESULT;
    }
    let scale = ui::Scale::of(hwnd);
    let enabled = IsWindowEnabled(hwnd) != 0;
    let accent = if enabled { p.accent } else { p.border };
    let mut thumb: RECT = zeroed();
    SendMessageW(hwnd, TBM_GETTHUMBRECT, 0, &mut thumb as *mut RECT as LPARAM);
    match cd.dwItemSpec {
        TBCD_TICS => CDRF_SKIPDEFAULT as LRESULT,
        TBCD_CHANNEL => {
            let mut client: RECT = zeroed();
            GetClientRect(hwnd, &mut client);
            // Clear the whole control (the ticks area included).
            let (window, _) = brushes(p);
            FillRect(cd.hdc, &client, window);
            let r = cd.rc;
            let mid = (thumb.top + thumb.bottom) / 2;
            let h = scale.px(4);
            let track = RECT { left: r.left, top: mid - h / 2, right: r.right, bottom: mid - h / 2 + h };
            let area = RECT { left: client.left, top: track.top - 2, right: client.right, bottom: track.bottom + 2 };
            if let Some(mut cv) = Canvas::new(area, p.bg) {
                cv.round_rect(track, h as f32 / 2.0, p.border, 1.0, None);
                let x = (thumb.left + thumb.right) / 2;
                cv.round_rect(RECT { right: x.max(track.left + h), ..track }, h as f32 / 2.0, accent, 1.0, None);
                cv.blit(cd.hdc);
            }
            CDRF_SKIPDEFAULT as LRESULT
        }
        TBCD_THUMB => {
            let d = scale.px(18);
            let (cx, cy) = ((thumb.left + thumb.right) / 2, (thumb.top + thumb.bottom) / 2);
            let area = RECT { left: cx - d / 2 - 2, top: cy - d / 2 - 2, right: cx + d / 2 + 2, bottom: cy + d / 2 + 2 };
            let hot = cd.uItemState & (CDIS_HOT | CDIS_SELECTED) != 0;
            if let Some(mut cv) = Canvas::new(area, p.bg) {
                // Repaint the track under the thumb.
                let h = scale.px(4);
                let track = RECT { left: area.left, top: cy - h / 2, right: area.right, bottom: cy - h / 2 + h };
                cv.fill(RECT { right: cx, ..track }, accent);
                cv.fill(RECT { left: cx, ..track }, p.border);
                cv.circle(cx, cy, d as f32 / 2.0, if p.light { ui::rgb(0xFF, 0xFF, 0xFF) } else { p.card }, 1.0);
                cv.outline(
                    RECT { left: cx - d / 2, top: cy - d / 2, right: cx + d / 2, bottom: cy + d / 2 },
                    d as f32 / 2.0,
                    p.border,
                    1.0,
                );
                let inner = if hot { d as f32 * 0.3 } else { d as f32 * 0.22 };
                cv.circle(cx, cy, inner, accent, 1.0);
                cv.blit(cd.hdc);
            }
            CDRF_SKIPDEFAULT as LRESULT
        }
        _ => CDRF_DODEFAULT as LRESULT,
    }
}

// ------------------------------------------------------- hand cursor

/// Buttons, check boxes and radio buttons: the hand cursor.
unsafe extern "system" fn hand_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if msg == WM_SETCURSOR && IsWindowEnabled(hwnd) != 0 {
        SetCursor(LoadCursorW(null_mut(), IDC_HAND));
        return 1;
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

// ----------------------------------------------------------- text fields

/// Text fields: their frame (the client edge) in our colours, accent when
/// the field has the focus, instead of the sunken 3D edge.
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_NCPAINT => {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            paint_edit_frame(hwnd);
            r
        }
        WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE => {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            RedrawWindow(hwnd, null(), 0 as _, RDW_FRAME | RDW_INVALIDATE);
            r
        }
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint_edit_frame(hwnd: HWND) {
    let mut window: RECT = zeroed();
    let mut client: RECT = zeroed();
    GetWindowRect(hwnd, &mut window);
    GetClientRect(hwnd, &mut client);
    let (w, h) = (window.right - window.left, window.bottom - window.top);
    // Client area position inside the window.
    let mut origin = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    ClientToScreen(hwnd, &mut origin);
    let (cx, cy) = (origin.x - window.left, origin.y - window.top);
    if cx <= 0 && cy <= 0 {
        return; // no frame
    }
    let p = palette();
    let dc = GetWindowDC(hwnd);
    let field = field_color(&p);
    let border = if IsWindowEnabled(hwnd) == 0 {
        p.border
    } else if GetFocus() == hwnd {
        p.accent
    } else {
        ui::mix(p.border, p.sub, 0.35)
    };
    // Frame band in the field colour, then a 1 px border.
    let brush = CreateSolidBrush(field);
    let bands = [
        RECT { left: 0, top: 0, right: w, bottom: cy },
        RECT { left: 0, top: cy + client.bottom, right: w, bottom: h },
        RECT { left: 0, top: 0, right: cx, bottom: h },
        RECT { left: cx + client.right, top: 0, right: w, bottom: h },
    ];
    for b in &bands {
        FillRect(dc, b, brush);
    }
    DeleteObject(brush as _);
    outline(dc, RECT { left: 0, top: 0, right: w, bottom: h }, 0, border);
    ReleaseDC(hwnd, dc);
}

// ------------------------------------------------------ drop-down lists

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

#[repr(C)]
struct ComboInfo {
    size: u32,
    item: RECT,
    button: RECT,
    button_state: u32,
    combo: HWND,
    edit: HWND,
    list: HWND,
}

fn combo_text(hwnd: HWND, index: isize) -> Vec<u16> {
    if index < 0 {
        return Vec::new();
    }
    // SAFETY: the buffer is one longer than the item text.
    unsafe {
        let n = SendMessageW(hwnd, CB_GETLBTEXTLEN, index as usize, 0);
        if n < 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; n as usize + 1];
        let got = SendMessageW(hwnd, CB_GETLBTEXT, index as usize, buf.as_mut_ptr() as LPARAM).max(0) as usize;
        buf.truncate(got);
        buf
    }
}

/// Rounded corners and our border for the list that drops down.
fn combo_list_frame(combo: HWND) {
    // SAFETY: the structure size is set; DWM ignores unknown attributes.
    unsafe {
        let mut info: ComboInfo = zeroed();
        info.size = size_of::<ComboInfo>() as u32;
        if SendMessageW(combo, CB_GETCOMBOBOXINFO, 0, &mut info as *mut ComboInfo as LPARAM) == 0 || info.list.is_null() {
            return;
        }
        let small: u32 = 3; // DWMWCP_ROUNDSMALL
        let border = palette().border;
        DwmSetWindowAttribute(info.list, 33, (&small as *const u32).cast(), 4);
        DwmSetWindowAttribute(info.list, 34, (&border as *const u32).cast(), 4);
    }
}

/// One item of an open drop-down list: the selected one on a tinted pill
/// with an accent mark.
unsafe fn draw_combo_item(di: &DrawItem, p: &Palette) {
    if di.item_state & ODS_COMBOBOXEDIT != 0 {
        // The closed field is painted by `combo_proc`.
        return;
    }
    let r = di.rc_item;
    let scale = ui::Scale::of(di.hwnd_item);
    let field = field_color(p);
    let Some(mut cv) = Canvas::new(r, field) else { return };
    let selected = di.item_state & ODS_SELECTED != 0;
    if selected {
        let pill =
            RECT { left: r.left + scale.px(4), top: r.top + scale.px(2), right: r.right - scale.px(4), bottom: r.bottom - scale.px(2) };
        cv.round_rect(pill, scale.px(5) as f32, ui::mix(field, p.accent, 0.16), 1.0, None);
        let mark_h = (pill.bottom - pill.top) / 2;
        let mark_top = pill.top + mark_h / 2;
        cv.round_rect(
            RECT { left: pill.left, top: mark_top, right: pill.left + scale.px(3), bottom: mark_top + mark_h },
            scale.px(2) as f32,
            p.accent,
            1.0,
            None,
        );
    }
    cv.blit(di.hdc);
    let mut text = combo_text(di.hwnd_item, di.item_id as i32 as isize);
    let font = SendMessageW(di.hwnd_item, WM_GETFONT, 0, 0) as HFONT;
    let old = SelectObject(di.hdc, font as _);
    SetBkMode(di.hdc, TRANSPARENT as i32);
    SetTextColor(di.hdc, p.fg);
    let mut tr = RECT { left: r.left + scale.px(14), right: r.right - scale.px(8), ..r };
    DrawTextW(di.hdc, text.as_mut_ptr(), text.len() as i32, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    SelectObject(di.hdc, old);
}

/// The closed drop-down list: a rounded field with the chosen item and a
/// chevron; the border lights up under the mouse and with the focus.
unsafe extern "system" fn combo_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, hot: usize) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            paint_combo(hwnd, dc, hot != 0);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_SETCURSOR if IsWindowEnabled(hwnd) != 0 => {
            SetCursor(LoadCursorW(null_mut(), IDC_HAND));
            1
        }
        WM_MOUSEMOVE if hot == 0 => {
            let mut tme =
                TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut tme);
            SetWindowSubclass(hwnd, Some(combo_proc), COMBO_ID, 1);
            InvalidateRect(hwnd, null(), 0);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            SetWindowSubclass(hwnd, Some(combo_proc), COMBO_ID, 0);
            InvalidateRect(hwnd, null(), 0);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            InvalidateRect(hwnd, null(), 0);
            r
        }
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint_combo(hwnd: HWND, dc: HDC, hot: bool) {
    let p = palette();
    let scale = ui::Scale::of(hwnd);
    let mut r: RECT = zeroed();
    GetClientRect(hwnd, &mut r);
    let enabled = IsWindowEnabled(hwnd) != 0;
    let open = SendMessageW(hwnd, CB_GETDROPPEDSTATE, 0, 0) != 0;
    let focus = GetFocus() == hwnd;
    let Some(mut cv) = Canvas::new(r, p.bg) else { return };
    let field = field_color(&p);
    let fill = if !enabled {
        p.bg
    } else if hot || open {
        ui::mix(field, p.hover, 0.7)
    } else {
        field
    };
    let border = if !enabled {
        p.border
    } else if focus || open {
        p.accent
    } else if hot {
        ui::mix(p.border, p.sub, 0.5)
    } else {
        p.border
    };
    let inner = RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
    cv.round_rect(inner, scale.px(6) as f32, fill, 1.0, Some(border));
    if focus && !open {
        // Accent line at the bottom, like the Windows 11 fields.
        let line = RECT { left: r.left + scale.px(5), top: r.bottom - scale.px(2), right: r.right - scale.px(5), bottom: r.bottom };
        cv.round_rect(line, 1.0, p.accent, 1.0, None);
    }
    let arrow = RECT { left: r.right - scale.px(28), top: r.top, right: r.right - scale.px(4), bottom: r.bottom };
    cv.chevron(arrow, if enabled { p.sub } else { p.border }, scale.px(2).max(1) as f32 * 0.8);
    cv.blit(dc);
    let mut text = combo_text(hwnd, SendMessageW(hwnd, CB_GETCURSEL, 0, 0));
    let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
    let old = SelectObject(dc, font as _);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, if enabled { p.fg } else { p.sub });
    let mut tr = RECT { left: r.left + scale.px(10), right: arrow.left, ..r };
    DrawTextW(dc, text.as_mut_ptr(), text.len() as i32, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    SelectObject(dc, old);
}

// ------------------------------------------------- group boxes and tabs

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_ERASEBKGND if class_of(hwnd) == "systabcontrol32" => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            if class_of(hwnd) == "systabcontrol32" {
                paint_tabs(hwnd, dc);
            } else {
                paint_group(hwnd, dc);
            }
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

/// Group box: a thin rounded frame with its title on it.
unsafe fn paint_group(hwnd: HWND, dc: HDC) {
    let p = palette();
    let scale = ui::Scale::of(hwnd);
    let mut r: RECT = zeroed();
    GetClientRect(hwnd, &mut r);
    let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
    let old = SelectObject(dc, font as _);
    let mut text = window_text(hwnd);
    let mut measure = RECT { left: 0, top: 0, right: r.right, bottom: r.bottom };
    DrawTextW(dc, text.as_mut_ptr(), text.len() as i32, &mut measure, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
    let text_h = measure.bottom - measure.top;
    let frame = RECT { top: r.top + text_h / 2, ..r };
    // Outline only: the controls inside are siblings, never painted over.
    outline(dc, frame, scale.px(4), p.border);
    let pad = scale.px(8);
    let title = RECT { left: r.left + pad, top: r.top, right: (r.left + pad * 2 + measure.right).min(r.right), bottom: r.top + text_h };
    let (window, _) = brushes(&p);
    FillRect(dc, &title, window);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, p.fg);
    let mut tr = RECT { left: title.left + pad / 2, ..title };
    DrawTextW(dc, text.as_mut_ptr(), text.len() as i32, &mut tr, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);
    SelectObject(dc, old);
}

unsafe fn outline(dc: HDC, r: RECT, radius: i32, color: COLORREF) {
    let pen = CreatePen(PS_SOLID, 1, color);
    let old_pen = SelectObject(dc, pen as _);
    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    RoundRect(dc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);
    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    DeleteObject(pen as _);
}

/// Tabs in the Windows 11 style: plain labels, the selected one bold-ish
/// with an accent underline, over the window background.
unsafe fn paint_tabs(hwnd: HWND, dc: HDC) {
    #[repr(C)]
    struct TcItemW {
        mask: u32,
        state: u32,
        state_mask: u32,
        text: *mut u16,
        text_max: i32,
        image: i32,
        lparam: LPARAM,
    }
    let p = palette();
    let scale = ui::Scale::of(hwnd);
    let mut client: RECT = zeroed();
    GetClientRect(hwnd, &mut client);
    let (window, _) = brushes(&p);
    FillRect(dc, &client, window);
    let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
    let old = SelectObject(dc, font as _);
    SetBkMode(dc, TRANSPARENT as i32);
    let count = SendMessageW(hwnd, TCM_GETITEMCOUNT, 0, 0).max(0) as usize;
    let selected = SendMessageW(hwnd, TCM_GETCURSEL, 0, 0);
    let mut bottom = client.top;
    for i in 0..count {
        let mut r: RECT = zeroed();
        SendMessageW(hwnd, TCM_GETITEMRECT, i, &mut r as *mut RECT as LPARAM);
        bottom = bottom.max(r.bottom);
        let mut buf = [0u16; 128];
        let mut item = TcItemW { mask: TCIF_TEXT, state: 0, state_mask: 0, text: buf.as_mut_ptr(), text_max: 128, image: 0, lparam: 0 };
        SendMessageW(hwnd, TCM_GETITEMW, i, &mut item as *mut TcItemW as LPARAM);
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let is_selected = selected == i as isize;
        SetTextColor(dc, if is_selected { p.fg } else { p.sub });
        let mut tr = r;
        DrawTextW(dc, buf.as_mut_ptr(), len as i32, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        if is_selected {
            let w = (r.right - r.left) / 3;
            let mid = (r.left + r.right) / 2;
            let bar = RECT { left: mid - w / 2, top: r.bottom - scale.px(3), right: mid + w / 2, bottom: r.bottom - scale.px(1) };
            ui::round_rect(dc, bar, scale.px(1), p.accent, None);
        }
    }
    ui::fill(dc, RECT { left: client.left, top: bottom, right: client.right, bottom: bottom + 1 }, p.border);
    SelectObject(dc, old);
}
