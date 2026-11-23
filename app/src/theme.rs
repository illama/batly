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

use crate::ui::{self, palette, Palette};
use crate::win::wide;
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::null;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect, RedrawWindow,
    RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, DT_CALCRECT, DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    HBRUSH, HDC, HFONT, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::{
    DrawThemeBackground, GetThemePartSize, GetWindowTheme, SetWindowTheme, BP_CHECKBOX, BP_RADIOBUTTON, CDDS_PREPAINT, CDIS_DISABLED,
    CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT, CDRF_SKIPDEFAULT, NMCUSTOMDRAW, NMHDR, NM_CUSTOMDRAW, TS_DRAW,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetClientRect, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, SendMessageW, GWL_STYLE,
    WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_GETFONT, WM_NOTIFY, WM_PAINT,
    WM_SETTINGCHANGE, WM_THEMECHANGED,
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
            _ => set_theme(child, explorer),
        },
        "edit" | "combobox" => set_theme(child, dark.then_some("DarkMode_CFD")),
        "systabcontrol32" => subclass(child),
        "syslistview32" => {
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
        WM_NOTIFY if !p.light => {
            let hdr = &*(lparam as *const NMHDR);
            if hdr.code != NM_CUSTOMDRAW || class_of(hdr.hwndFrom) != "button" {
                return None;
            }
            let cd = &*(lparam as *const NMCUSTOMDRAW);
            let kind = button_type(hdr.hwndFrom);
            let check = matches!(kind, BS_CHECKBOX | BS_AUTOCHECKBOX | BS_3STATE | BS_AUTO3STATE);
            let radio = matches!(kind, BS_RADIOBUTTON | BS_AUTORADIOBUTTON);
            if cd.dwDrawStage != CDDS_PREPAINT || !(check || radio) {
                return Some(CDRF_DODEFAULT as LRESULT);
            }
            draw_check(hdr.hwndFrom, cd, radio, &p);
            Some(CDRF_SKIPDEFAULT as LRESULT)
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

/// Check box or radio button with readable text on the dark background:
/// the glyph from the control's (dark) theme, the text in our color.
unsafe fn draw_check(hwnd: HWND, cd: &NMCUSTOMDRAW, radio: bool, p: &Palette) {
    let dc = cd.hdc;
    let r = cd.rc;
    let (window, _) = brushes(p);
    FillRect(dc, &r, window);
    let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) == 1;
    let disabled = cd.uItemState & CDIS_DISABLED != 0;
    let pressed = cd.uItemState & CDIS_SELECTED != 0;
    let hot = cd.uItemState & CDIS_HOT != 0;
    // CBS_/RBS_ states: 1-4 unchecked, 5-8 checked (normal, hot, pressed, disabled).
    let offset = if disabled {
        3
    } else if pressed {
        2
    } else if hot {
        1
    } else {
        0
    };
    let state = if checked { 5 } else { 1 } + offset;
    let part = if radio { BP_RADIOBUTTON } else { BP_CHECKBOX };
    let theme = GetWindowTheme(hwnd);
    let mut size = SIZE { cx: ui::Scale::of(hwnd).px(13), cy: ui::Scale::of(hwnd).px(13) };
    if theme != 0 {
        GetThemePartSize(theme, dc, part, state, null(), TS_DRAW, &mut size);
    }
    let top = r.top + (r.bottom - r.top - size.cy) / 2;
    let glyph = RECT { left: r.left, top, right: r.left + size.cx, bottom: top + size.cy };
    if theme != 0 {
        DrawThemeBackground(theme, dc, part, state, &glyph, null());
    }
    let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
    let old = SelectObject(dc, font as _);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, if disabled { p.sub } else { p.fg });
    let mut text = window_text(hwnd);
    let mut tr = RECT { left: glyph.right + ui::Scale::of(hwnd).px(6), ..r };
    DrawTextW(dc, text.as_mut_ptr(), text.len() as i32, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
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
