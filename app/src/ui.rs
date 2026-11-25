//! Small GDI helpers shared by the custom drawn windows (battery panel) and
//! the settings window: theme, DPI scaling, fonts, text and shapes.

use crate::win::{self, wide};
use windows_sys::Win32::Foundation::{COLORREF, HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, RoundRect, SelectObject, SetBkMode, SetTextColor,
    CLEARTYPE_QUALITY, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, HDC, HFONT, PS_SOLID, TRANSPARENT,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;

const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// Applications (not the taskbar) use the light theme.
pub fn apps_light() -> bool {
    win::reg_read_dword(PERSONALIZE_KEY, "AppsUseLightTheme").is_none_or(|v| v != 0)
}

/// The Batly colours, shared by every window, the tray menu and the web
/// page (whose CSS repeats these values): graphite surfaces, one green
/// accent, blue for charging, amber and red for low levels.
#[derive(Clone, Copy)]
pub struct Palette {
    pub light: bool,
    pub bg: COLORREF,
    pub fg: COLORREF,
    pub sub: COLORREF,
    pub hover: COLORREF,
    pub card: COLORREF,
    pub border: COLORREF,
    pub accent: COLORREF,
    /// Text on an accent background.
    pub on_accent: COLORREF,
    pub charge: COLORREF,
    pub warn: COLORREF,
    pub bad: COLORREF,
    pub warn_bg: COLORREF,
    pub warn_fg: COLORREF,
}

pub const LIGHT: Palette = Palette {
    light: true,
    bg: rgb(0xF4, 0xF5, 0xF7),
    fg: rgb(0x14, 0x16, 0x1B),
    sub: rgb(0x5D, 0x65, 0x73),
    hover: rgb(0xEC, 0xEE, 0xF2),
    card: rgb(0xFF, 0xFF, 0xFF),
    border: rgb(0xE0, 0xE3, 0xE8),
    accent: rgb(0x0E, 0x9F, 0x6E),
    on_accent: rgb(0xFF, 0xFF, 0xFF),
    charge: rgb(0x1D, 0x7F, 0xE0),
    warn: rgb(0xC9, 0x85, 0x00),
    bad: rgb(0xD6, 0x45, 0x45),
    warn_bg: rgb(0xFF, 0xF4, 0xD6),
    warn_fg: rgb(0x6B, 0x4A, 0x00),
};

pub const DARK: Palette = Palette {
    light: false,
    bg: rgb(0x13, 0x15, 0x1A),
    fg: rgb(0xF2, 0xF4, 0xF7),
    sub: rgb(0x9A, 0xA3, 0xB2),
    hover: rgb(0x26, 0x2A, 0x33),
    card: rgb(0x1C, 0x1F, 0x26),
    border: rgb(0x2C, 0x31, 0x3B),
    accent: rgb(0x3D, 0xDC, 0x97),
    on_accent: rgb(0x07, 0x24, 0x18),
    charge: rgb(0x5A, 0xB8, 0xFF),
    warn: rgb(0xF5, 0xB5, 0x41),
    bad: rgb(0xF2, 0x64, 0x5A),
    warn_bg: rgb(0x33, 0x2A, 0x16),
    warn_fg: rgb(0xF5, 0xC7, 0x6A),
};

pub fn palette() -> Palette {
    if apps_light() {
        LIGHT
    } else {
        DARK
    }
}

impl Palette {
    /// Colour of a battery level: charging, low, warning or fine.
    pub fn level(&self, percent: f32, charging: bool) -> COLORREF {
        if charging {
            self.charge
        } else if percent <= 15.0 {
            self.bad
        } else if percent <= 30.0 {
            self.warn
        } else {
            self.accent
        }
    }
}

/// `a` → `b` by `t` (0..1), per channel.
pub fn mix(a: COLORREF, b: COLORREF, t: f32) -> COLORREF {
    let t = t.clamp(0.0, 1.0);
    let ch = |c: COLORREF, s: u32| (c >> s & 0xFF) as f32;
    let m = |s: u32| ((ch(a, s) + (ch(b, s) - ch(a, s)) * t).round() as u32) << s;
    m(0) | m(8) | m(16)
}

/// Ease out (cubic), for animations: fast start, soft landing.
pub fn ease_out(t: f32) -> f32 {
    let t = 1.0 - t.clamp(0.0, 1.0);
    1.0 - t * t * t
}

/// Logical (96 dpi) pixels → physical pixels for a window.
#[derive(Clone, Copy)]
pub struct Scale(pub u32);

impl Scale {
    pub fn of(hwnd: HWND) -> Self {
        // SAFETY: plain FFI call; 0 for an invalid window.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        Self(if dpi == 0 { 96 } else { dpi })
    }

    pub fn px(self, v: i32) -> i32 {
        (v as i64 * self.0 as i64 / 96) as i32
    }
}

/// Owned GDI font.
pub struct Font(pub HFONT);

impl Font {
    /// `size` in logical pixels, `weight` 400 = regular, 600 = semibold.
    pub fn new(scale: Scale, size: i32, weight: i32) -> Self {
        // SAFETY: valid NUL-terminated face name.
        Self(unsafe {
            CreateFontW(
                -scale.px(size),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                1, // DEFAULT_CHARSET
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                wide("Segoe UI").as_ptr(),
            )
        })
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: we own the font; it is no longer selected when dropped.
        unsafe { DeleteObject(self.0) };
    }
}

pub fn fill(dc: HDC, r: RECT, color: COLORREF) {
    // SAFETY: valid DC; the brush is released right away.
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, &r, brush);
        DeleteObject(brush);
    }
}

/// Filled rounded rectangle (radius in physical pixels).
pub fn round_rect(dc: HDC, r: RECT, radius: i32, color: COLORREF, border: Option<COLORREF>) {
    // SAFETY: valid DC; GDI objects are restored and released.
    unsafe {
        let brush = CreateSolidBrush(color);
        let pen = CreatePen(PS_SOLID, 1, border.unwrap_or(color));
        let old_brush = SelectObject(dc, brush);
        let old_pen = SelectObject(dc, pen);
        RoundRect(dc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(brush);
        DeleteObject(pen);
    }
}

/// Draws `text` in `r` with the DT_* `flags` (single line, ellipsis added).
pub fn text(dc: HDC, font: &Font, color: COLORREF, s: &str, mut r: RECT, flags: u32) {
    // SAFETY: valid DC and font; the old font is restored.
    unsafe {
        let old = SelectObject(dc, font.0);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, color);
        let mut w = wide(s);
        DrawTextW(dc, w.as_mut_ptr(), -1, &mut r, flags | DT_NOPREFIX | DT_SINGLELINE | DT_END_ELLIPSIS);
        SelectObject(dc, old);
    }
}

/// Multi-line text; returns the height used (with DT_CALCRECT when `measure`).
pub fn text_block(dc: HDC, font: &Font, color: COLORREF, s: &str, r: RECT, measure: bool) -> i32 {
    use windows_sys::Win32::Graphics::Gdi::{DT_CALCRECT, DT_WORDBREAK};
    let mut rr = r;
    // SAFETY: valid DC and font; the old font is restored.
    unsafe {
        let old = SelectObject(dc, font.0);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, color);
        let mut w = wide(s);
        let flags = DT_WORDBREAK | DT_NOPREFIX | if measure { DT_CALCRECT } else { 0 };
        DrawTextW(dc, w.as_mut_ptr(), -1, &mut rr, flags);
        SelectObject(dc, old);
    }
    rr.bottom - rr.top
}

pub fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Windows 11 rounded corners and a themed border (ignored on Windows 10).
pub fn modern_frame(hwnd: HWND, border: COLORREF) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_BORDER_COLOR: u32 = 34;
    let round: u32 = 2; // DWMWCP_ROUND
    let dark: u32 = (!apps_light()) as u32;
    // SAFETY: attribute values are valid for their sizes.
    unsafe {
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, (&round as *const u32).cast(), 4);
        DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, (&border as *const u32).cast(), 4);
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const u32).cast(), 4);
    }
}
