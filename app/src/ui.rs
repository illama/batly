//! Small GDI helpers shared by the custom drawn windows (battery panel) and
//! the settings window: theme, DPI scaling, fonts, text and shapes.

use crate::win::{self, wide};
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{COLORREF, HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, RoundRect, SelectObject, SetBkMode,
    SetDIBitsToDevice, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, DIB_RGB_COLORS, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, HDC, HFONT, PS_SOLID, TRANSPARENT,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;

const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

pub fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

pub fn rgb3(c: [u8; 3]) -> COLORREF {
    rgb(c[0], c[1], c[2])
}

/// Applications (not the taskbar) use the light theme.
pub fn apps_light() -> bool {
    win::reg_read_dword(PERSONALIZE_KEY, "AppsUseLightTheme").is_none_or(|v| v != 0)
}

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
    pub warn_bg: COLORREF,
    pub warn_fg: COLORREF,
    pub bg_rgb: [u8; 3],
}

pub fn palette() -> Palette {
    if apps_light() {
        Palette {
            light: true,
            bg: rgb(0xF3, 0xF3, 0xF3),
            fg: rgb(0x1A, 0x1A, 0x1A),
            sub: rgb(0x5F, 0x5F, 0x5F),
            hover: rgb(0xE3, 0xE3, 0xE3),
            card: rgb(0xFB, 0xFB, 0xFB),
            border: rgb(0xDD, 0xDD, 0xDD),
            accent: rgb(0x00, 0x5F, 0xB8),
            warn_bg: rgb(0xFF, 0xF4, 0xCE),
            warn_fg: rgb(0x5C, 0x40, 0x00),
            bg_rgb: [0xF3, 0xF3, 0xF3],
        }
    } else {
        Palette {
            light: false,
            bg: rgb(0x20, 0x20, 0x20),
            fg: rgb(0xFF, 0xFF, 0xFF),
            sub: rgb(0xA8, 0xA8, 0xA8),
            hover: rgb(0x38, 0x38, 0x38),
            card: rgb(0x2B, 0x2B, 0x2B),
            border: rgb(0x3A, 0x3A, 0x3A),
            accent: rgb(0x60, 0xCD, 0xFF),
            warn_bg: rgb(0x43, 0x35, 0x19),
            warn_fg: rgb(0xFC, 0xE1, 0x00),
            bg_rgb: [0x20, 0x20, 0x20],
        }
    }
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

/// Composites straight-alpha RGBA pixels over `bg` and draws them at (x, y).
pub fn draw_rgba(dc: HDC, rgba: &[u8], n: usize, x: i32, y: i32, bg: [u8; 3]) {
    let mut bgra = Vec::with_capacity(n * n * 4);
    for px in rgba.chunks_exact(4) {
        let a = px[3] as u32;
        let mix = |c: u8, b: u8| ((c as u32 * a + b as u32 * (255 - a)) / 255) as u8;
        bgra.extend_from_slice(&[mix(px[2], bg[2]), mix(px[1], bg[1]), mix(px[0], bg[0]), 255]);
    }
    // SAFETY: BITMAPINFO describes exactly `bgra`.
    unsafe {
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = n as i32;
        info.bmiHeader.biHeight = -(n as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        SetDIBitsToDevice(dc, x, y, n as u32, n as u32, 0, 0, 0, n as u32, bgra.as_ptr().cast(), &info, DIB_RGB_COLORS);
    }
}

pub fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub const EMPTY: RECT = RECT { left: 0, top: 0, right: 0, bottom: 0 };

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

/// Dark title bar for normal windows when apps use the dark theme.
pub fn themed_title_bar(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let dark: u32 = (!apps_light()) as u32;
    // SAFETY: attribute value valid for its size.
    unsafe { DwmSetWindowAttribute(hwnd, 20, (&dark as *const u32).cast(), 4) };
}

pub fn null_hwnd() -> HWND {
    null_mut()
}
