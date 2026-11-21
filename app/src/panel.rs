//! Battery panel: a Windows 11 style flyout opened by a left click on a tray
//! icon (or the global shortcut). Every device at a glance: level, state,
//! time left / time to full, plus the pending alert and a few actions.
//!
//! The panel is a dumb view: the tray builds a `Content` and the panel posts
//! `WM_COMMAND` messages (`CMD_*`) back to the tray window.

use crate::icon::{self, IconSpec};
use crate::model::DeviceKind;
use crate::ui::{self, Font, Palette, Scale};
use crate::win::wide;
use std::cell::RefCell;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::time::Instant;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint, GetMonitorInfoW,
    InvalidateRect, MonitorFromPoint, SelectObject, DT_LEFT, DT_RIGHT, DT_VCENTER, DT_CENTER, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, IsWindow, LoadCursorW, PostMessageW, RegisterClassW,
    SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SW_SHOW, WM_ACTIVATE,
    WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

pub const CMD_REFRESH: usize = 9001;
pub const CMD_SETTINGS: usize = 9002;
pub const CMD_WEB: usize = 9003;
pub const CMD_SNOOZE: usize = 9004;
pub const CMD_RECEIVERS: usize = 9005;
/// `CMD_DEVICE + n`: click on the n-th row.
pub const CMD_DEVICE: usize = 9100;

const CLASS: &str = "BatlyPanel";
const WIDTH: i32 = 380;

#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    pub name: String,
    pub kind: DeviceKind,
    pub percent: Option<f32>,
    pub charging: bool,
    pub online: bool,
    pub has_battery: bool,
    /// Second line: state and estimate, e.g. "en charge · plein dans ≈ 40 min".
    pub detail: String,
    /// Right-aligned small text under the percentage (host, signal…).
    pub note: String,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Content {
    pub title: String,
    pub empty: String,
    pub rows: Vec<Row>,
    /// Pending alert text, with its "later" button label.
    pub alert: Option<(String, String)>,
    /// (command, label) buttons in the footer.
    pub buttons: Vec<(usize, String)>,
}

struct State {
    hwnd: HWND,
    owner: HWND,
    content: Content,
    hot: Option<usize>,
    /// Clickable areas: (command, rect).
    targets: Vec<(usize, RECT)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static LAST_CLOSE: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

pub fn is_open() -> bool {
    STATE.with(|s| s.borrow().as_ref().is_some_and(|st| unsafe { IsWindow(st.hwnd) } != 0))
}

/// Shows the panel, or closes it when it is open. `anchor` is the point the
/// panel should sit next to (tray icon / cursor).
pub fn toggle(owner: HWND, content: Content, anchor: Option<POINT>) {
    if is_open() {
        close();
        return;
    }
    // The click that made the panel lose focus also lands here: do not
    // reopen it immediately.
    if LAST_CLOSE.with(|c| c.borrow().is_some_and(|t| t.elapsed().as_millis() < 250)) {
        return;
    }
    open(owner, content, anchor);
}

pub fn update(content: Content) {
    let hwnd = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        if st.content == content {
            return None;
        }
        st.content = content;
        Some(st.hwnd)
    });
    if let Some(hwnd) = hwnd {
        reposition(hwnd, None);
        // SAFETY: our own window.
        unsafe { InvalidateRect(hwnd, null(), 0) };
    }
}

pub fn close() {
    let hwnd = STATE.with(|s| s.borrow().as_ref().map(|st| st.hwnd));
    if let Some(hwnd) = hwnd {
        // SAFETY: our own window; WM_DESTROY clears the state.
        unsafe { DestroyWindow(hwnd) };
    }
}

fn open(owner: HWND, content: Content, anchor: Option<POINT>) {
    // SAFETY: standard window class registration / creation.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS);
        let mut class: WNDCLASSW = zeroed();
        class.lpfnWndProc = Some(wnd_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        RegisterClassW(&class); // fails harmlessly when already registered
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name.as_ptr(),
            wide("Batly").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            owner,
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State { hwnd, owner, content, hot: None, targets: Vec::new() })
        });
        ui::modern_frame(hwnd, ui::palette().border);
        reposition(hwnd, anchor);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

/// Height needed for the content, in logical pixels.
fn layout_height(c: &Content, alert_lines: i32) -> i32 {
    let rows = c.rows.len().max(1) as i32;
    let mut h = 52 + rows * 62 + 8;
    if c.alert.is_some() {
        h += 16 + 20 * alert_lines + 24;
    }
    h + 56
}

fn reposition(hwnd: HWND, anchor: Option<POINT>) {
    let scale = Scale::of(hwnd);
    let (lines, anchor_point) = STATE.with(|s| {
        let s = s.borrow();
        let c = s.as_ref().map(|st| &st.content);
        let lines = c.and_then(|c| c.alert.as_ref()).map_or(0, |(t, _)| (t.chars().count() as i32 / 44 + 1).min(4));
        (lines, anchor)
    });
    let height = STATE.with(|s| s.borrow().as_ref().map(|st| layout_height(&st.content, lines))).unwrap_or(200);
    let (w, h) = (scale.px(WIDTH), scale.px(height));
    // SAFETY: plain FFI calls with valid out-pointers.
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        match anchor_point {
            Some(p) => pt = p,
            None => {
                let mut r: RECT = zeroed();
                windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r);
                if r.right - r.left > 20 {
                    // Already placed: keep the edge next to the taskbar.
                    pt = POINT { x: (r.left + r.right) / 2, y: r.bottom + 100 };
                } else {
                    GetCursorPos(&mut pt);
                }
            }
        }
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of_val(&info) as u32;
        GetMonitorInfoW(monitor, &mut info);
        let work = info.rcWork;
        let gap = scale.px(12);
        let x = if pt.x >= work.right {
            work.right - w - gap
        } else if pt.x <= work.left {
            work.left + gap
        } else {
            (pt.x - w / 2).clamp(work.left + gap, work.right - w - gap)
        };
        let y = if pt.y >= work.bottom {
            work.bottom - h - gap
        } else if pt.y <= work.top {
            work.top + gap
        } else {
            (work.bottom - h - gap).max(work.top + gap)
        };
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    }
}

fn paint(hwnd: HWND) {
    let scale = Scale::of(hwnd);
    let p = ui::palette();
    // SAFETY: standard double-buffered WM_PAINT; every GDI object created
    // here is released before returning.
    unsafe {
        let mut ps: PAINTSTRUCT = zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut client: RECT = zeroed();
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        let (w, h) = (client.right, client.bottom);
        let mem = CreateCompatibleDC(dc);
        let bmp = CreateCompatibleBitmap(dc, w, h);
        let old = SelectObject(mem, bmp);

        let targets = STATE.with(|s| {
            let s = s.borrow();
            s.as_ref().map(|st| draw(mem, scale, &p, w, &st.content, st.hot)).unwrap_or_default()
        });
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                st.targets = targets;
            }
        });

        BitBlt(dc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        EndPaint(hwnd, &ps);
    }
}

fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT { left: l, top: t, right: r, bottom: b }
}

/// Draws the whole panel; returns the clickable areas.
fn draw(dc: windows_sys::Win32::Graphics::Gdi::HDC, s: Scale, p: &Palette, w: i32, c: &Content, hot: Option<usize>) -> Vec<(usize, RECT)> {
    let px = |v: i32| s.px(v);
    let mut targets = Vec::new();
    let total_h = px(4000);
    ui::fill(dc, rect(0, 0, w, total_h), p.bg);

    let title = Font::new(s, 15, 600);
    let name_font = Font::new(s, 14, 600);
    let small = Font::new(s, 12, 400);
    let big = Font::new(s, 20, 600);
    let button = Font::new(s, 13, 400);

    // Header.
    ui::text(dc, &title, p.fg, &c.title, rect(px(18), px(14), w - px(18), px(40)), DT_LEFT | DT_VCENTER);
    ui::text(dc, &small, p.sub, &format!("Batly {}", crate::http::VERSION), rect(px(18), px(14), w - px(18), px(40)), DT_RIGHT | DT_VCENTER);

    // Device rows.
    let mut y = px(52);
    if c.rows.is_empty() {
        ui::text(dc, &small, p.sub, &c.empty, rect(px(18), y, w - px(18), y + px(62)), DT_LEFT | DT_VCENTER);
        y += px(62);
    }
    let icon_size = px(32) as usize;
    for (n, row) in c.rows.iter().enumerate() {
        let r = rect(px(8), y, w - px(8), y + px(58));
        let cmd = CMD_DEVICE + n;
        let bg = if hot == Some(cmd) { p.hover } else { p.card };
        ui::round_rect(dc, r, px(6), bg, Some(p.border));
        targets.push((cmd, r));
        let bg_rgb = if hot == Some(cmd) {
            [(p.hover & 0xFF) as u8, (p.hover >> 8 & 0xFF) as u8, (p.hover >> 16 & 0xFF) as u8]
        } else {
            [(p.card & 0xFF) as u8, (p.card >> 8 & 0xFF) as u8, (p.card >> 16 & 0xFF) as u8]
        };
        let spec = IconSpec {
            kind: row.kind,
            percent: if row.has_battery { row.percent.map(f32::round) } else { Some(100.0) },
            charging: row.charging,
            light_foreground: !p.light,
            dimmed: !row.online,
        };
        let rgba = icon::render(&spec, icon_size);
        ui::draw_rgba(dc, &rgba, icon_size, px(20), y + (px(58) - icon_size as i32) / 2, bg_rgb);

        let text_left = px(64);
        let right_w = px(86);
        ui::text(dc, &name_font, if row.online { p.fg } else { p.sub }, &row.name, rect(text_left, y + px(9), w - px(16) - right_w, y + px(29)), DT_LEFT | DT_VCENTER);
        ui::text(dc, &small, p.sub, &row.detail, rect(text_left, y + px(30), w - px(16) - right_w, y + px(48)), DT_LEFT | DT_VCENTER);

        let (value, color) = match (row.has_battery, row.percent) {
            (false, _) | (true, None) => ("–".to_string(), p.sub),
            (true, Some(v)) => {
                let color = if row.charging {
                    ui::rgb3(icon::level_color(100.0))
                } else if v <= 30.0 {
                    ui::rgb3(icon::level_color(v))
                } else {
                    p.fg
                };
                (crate::i18n::percent(v), if row.online { color } else { p.sub })
            }
        };
        let has_note = !row.note.is_empty();
        let value_box = if has_note { rect(w - px(16) - right_w, y + px(6), w - px(20), y + px(34)) } else { rect(w - px(16) - right_w, y, w - px(20), y + px(58)) };
        ui::text(dc, &big, color, &value, value_box, DT_RIGHT | DT_VCENTER);
        if has_note {
            ui::text(dc, &small, p.sub, &row.note, rect(w - px(16) - right_w - px(40), y + px(34), w - px(20), y + px(52)), DT_RIGHT | DT_VCENTER);
        }
        y += px(62);
    }
    y += px(8);

    // Pending alert.
    if let Some((text, later)) = &c.alert {
        let inner_w = w - px(16) - px(24) - px(90);
        let text_h = ui::text_block(dc, &small, p.warn_fg, text, rect(0, 0, inner_w, px(400)), true).max(px(20));
        let box_r = rect(px(8), y, w - px(8), y + text_h + px(24));
        ui::round_rect(dc, box_r, px(6), p.warn_bg, None);
        ui::text_block(dc, &small, p.warn_fg, text, rect(px(20), y + px(12), px(20) + inner_w, y + px(12) + text_h), false);
        let b = rect(w - px(16) - px(84), y + (text_h + px(24)) / 2 - px(15), w - px(18), y + (text_h + px(24)) / 2 + px(15));
        ui::round_rect(dc, b, px(4), if hot == Some(CMD_SNOOZE) { p.hover } else { p.card }, Some(p.border));
        ui::text(dc, &button, p.fg, later, b, DT_CENTER | DT_VCENTER);
        targets.push((CMD_SNOOZE, b));
        y = box_r.bottom + px(8);
    }

    // Footer buttons.
    ui::fill(dc, rect(0, y, w, y + 1), p.border);
    let mut x = px(10);
    let bottom = y + px(48);
    for (cmd, label) in &c.buttons {
        // Rough width: characters × average glyph width.
        let bw = px(24 + 7 * label.chars().count() as i32);
        let b = rect(x, y + px(10), x + bw, bottom - px(8));
        if hot == Some(*cmd) {
            ui::round_rect(dc, b, px(4), p.hover, None);
        }
        ui::text(dc, &button, p.accent, label, b, DT_CENTER | DT_VCENTER);
        targets.push((*cmd, b));
        x += bw + px(4);
    }
    targets
}

fn hit(hwnd: HWND, lparam: LPARAM) -> Option<usize> {
    let (x, y) = ((lparam & 0xFFFF) as i16 as i32, (lparam >> 16 & 0xFFFF) as i16 as i32);
    let _ = hwnd;
    STATE.with(|s| s.borrow().as_ref().and_then(|st| st.targets.iter().find(|(_, r)| ui::inside(r, x, y)).map(|(c, _)| *c)))
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_ACTIVATE => {
            if wparam & 0xFFFF == 0 {
                // Lost the focus: behave like a flyout.
                DestroyWindow(hwnd);
            }
            0
        }
        WM_KEYDOWN => {
            if wparam == VK_ESCAPE as usize {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_MOUSEMOVE => {
            let hot = hit(hwnd, lparam);
            let changed = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let st = s.as_mut()?;
                (st.hot != hot).then(|| st.hot = hot)
            });
            if changed.is_some() {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TrackMouseEvent(&mut tme);
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_MOUSELEAVE => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.hot = None;
                }
            });
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_LBUTTONUP => {
            if let Some(cmd) = hit(hwnd, lparam) {
                let owner = STATE.with(|s| s.borrow().as_ref().map(|st| st.owner));
                if let Some(owner) = owner {
                    PostMessageW(owner, WM_COMMAND, cmd, 0);
                }
                if cmd != CMD_SNOOZE {
                    DestroyWindow(hwnd);
                }
            }
            0
        }
        WM_DPICHANGED => {
            reposition(hwnd, None);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_DESTROY => {
            STATE.with(|s| *s.borrow_mut() = None);
            LAST_CLOSE.with(|c| *c.borrow_mut() = Some(Instant::now()));
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

use std::mem::{size_of, size_of_val};
