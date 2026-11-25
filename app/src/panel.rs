//! Battery panel: a Windows 11 style flyout opened by a left click on a tray
//! icon (or the global shortcut). Every device at a glance: level, state,
//! time left / time to full, plus the pending alert and a few actions.
//!
//! The panel is a dumb view: the tray builds a `Content` and the panel posts
//! `WM_COMMAND` messages (`CMD_*`) back to the tray window.

use crate::gfx::Canvas;
use crate::icon::{self, IconSpec};
use crate::model::DeviceKind;
use crate::ui::{self, Font, Palette, Scale};
use crate::win::wide;
use std::cell::RefCell;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::time::Instant;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint, GetMonitorInfoW, InvalidateRect,
    MonitorFromPoint, SelectObject, DT_CENTER, DT_LEFT, DT_RIGHT, DT_VCENTER, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
    SRCCOPY,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, IsWindow, KillTimer, LoadCursorW, PostMessageW, RegisterClassW,
    SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, IDC_ARROW, IDC_HAND, SWP_NOACTIVATE, SW_SHOW,
    WM_ACTIVATE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR,
    WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

pub const CMD_REFRESH: usize = 9001;
pub const CMD_SETTINGS: usize = 9002;
pub const CMD_WEB: usize = 9003;
pub const CMD_SNOOZE: usize = 9004;
pub const CMD_RECEIVERS: usize = 9005;
/// Range of the open chart: 7 or 30 days.
pub const CMD_DAYS_7: usize = 9006;
pub const CMD_DAYS_30: usize = 9007;
/// Opens the settings (from the chart, since a click on a row opens it).
pub const CMD_DEVICE_SETTINGS: usize = 9008;
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
    /// Battery chart unfolded under one row.
    pub chart: Option<Chart>,
}

/// Level over the last days of the device of row `row`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Chart {
    pub row: usize,
    /// 7 or 30.
    pub days: u32,
    /// (unix time, percent), oldest first.
    pub points: Vec<(u64, f32)>,
    pub now: u64,
    /// One line under the chart (statistics, or "no history yet").
    pub summary: String,
    /// Labels: "7 days", "30 days", "Settings".
    pub labels: [String; 3],
}

/// Logical height of an unfolded chart.
const CHART_H: i32 = 186;

struct State {
    hwnd: HWND,
    owner: HWND,
    content: Content,
    hot: Option<usize>,
    /// Clickable areas: (command, rect).
    targets: Vec<(usize, RECT)>,
    anim: Anim,
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
        kick(hwnd);
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
        STATE.with(|s| *s.borrow_mut() = Some(State { hwnd, owner, content, hot: None, targets: Vec::new(), anim: Anim::new() }));
        ui::modern_frame(hwnd, ui::palette().border);
        reposition(hwnd, anchor);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        kick(hwnd);
    }
}

/// Logical layout sizes.
const HEADER: i32 = 54;
const ROW: i32 = 60;
const ROW_STEP: i32 = 66;

/// Height needed for the content, in logical pixels.
fn layout_height(c: &Content, alert_lines: i32) -> i32 {
    let rows = c.rows.len().max(1) as i32;
    let mut h = HEADER + rows * ROW_STEP + 4;
    if c.chart.is_some() {
        h += CHART_H + 6;
    }
    if c.alert.is_some() {
        h += 16 + 20 * alert_lines + 16;
    }
    h + 58
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

/// Hover fade, level bars filling up, rows appearing one after the other
/// and the chart line drawing itself. Driven by a 15 ms timer that only
/// runs while something moves.
struct Anim {
    start: Instant,
    last: Instant,
    /// Hover progress (0..1) per command.
    hover: Vec<(usize, f32)>,
    /// Level shown per row, moving towards the real one.
    levels: Vec<f32>,
    /// (row, days) of the chart being drawn and its progress (0..1).
    chart: Option<((usize, u32), f32)>,
}

impl Anim {
    fn new() -> Self {
        let now = Instant::now();
        Self { start: now, last: now, hover: Vec::new(), levels: Vec::new(), chart: None }
    }

    fn hover_of(&self, cmd: usize) -> f32 {
        self.hover.iter().find(|(c, _)| *c == cmd).map_or(0.0, |(_, v)| *v)
    }

    /// Opacity / slide of row `n` during the opening (0..1).
    fn appear(&self, n: usize) -> f32 {
        let t = (self.start.elapsed().as_secs_f32() - ROW_DELAY * n as f32) / ROW_IN;
        ui::ease_out(t)
    }
}

const TIMER: usize = 1;
const ROW_DELAY: f32 = 0.035;
const ROW_IN: f32 = 0.28;
const HOVER_IN: f32 = 0.12;
const CHART_IN: f32 = 0.6;

/// Advances every animation; returns whether one is still running.
fn tick(st: &mut State) -> bool {
    let now = Instant::now();
    let dt = now.duration_since(st.anim.last).as_secs_f32().min(0.1);
    st.anim.last = now;
    let rows = st.content.rows.len();
    let mut busy = st.anim.start.elapsed().as_secs_f32() < ROW_IN + ROW_DELAY * rows as f32;

    if let Some(hot) = st.hot {
        if !st.anim.hover.iter().any(|(c, _)| *c == hot) {
            st.anim.hover.push((hot, 0.0));
        }
    }
    for (cmd, v) in st.anim.hover.iter_mut() {
        let target = if st.hot == Some(*cmd) { 1.0 } else { 0.0 };
        let step = dt / HOVER_IN;
        *v = if target > *v { (*v + step).min(1.0) } else { (*v - step).max(0.0) };
        busy |= *v != target;
    }
    let hot = st.hot;
    st.anim.hover.retain(|(c, v)| *v > 0.0 || hot == Some(*c));

    st.anim.levels.resize(rows, 0.0);
    for (n, row) in st.content.rows.iter().enumerate() {
        let target = if row.has_battery { row.percent.unwrap_or(0.0).clamp(0.0, 100.0) } else { 0.0 };
        // Bars start once their row has started to appear.
        if st.anim.appear(n) <= 0.0 {
            busy = true;
            continue;
        }
        let v = &mut st.anim.levels[n];
        let diff = target - *v;
        if diff.abs() < 0.3 {
            *v = target;
        } else {
            *v += diff * (dt * 7.0).min(1.0);
            busy = true;
        }
    }

    let key = st.content.chart.as_ref().map(|c| (c.row, c.days));
    match (key, st.anim.chart.as_mut()) {
        (Some(k), Some((old, t))) if *old == k => {
            *t = (*t + dt / CHART_IN).min(1.0);
            busy |= *t < 1.0;
        }
        (Some(k), _) => {
            st.anim.chart = Some((k, 0.0));
            busy = true;
        }
        (None, _) => st.anim.chart = None,
    }
    busy
}

/// Starts the animation timer (no-op when it already runs).
fn kick(hwnd: HWND) {
    // SAFETY: our own window; the same id just resets the timer.
    unsafe { SetTimer(hwnd, TIMER, 15, None) };
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
            s.as_ref().map(|st| draw(mem, scale, &p, client, st)).unwrap_or_default()
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

#[derive(Clone, Copy)]
enum F {
    Title,
    Name,
    Small,
    Big,
    Button,
}

/// Text drawn by GDI once the shapes are on screen.
struct Label {
    font: F,
    color: COLORREF,
    text: String,
    rect: RECT,
    flags: u32,
}

struct Fonts {
    title: Font,
    name: Font,
    small: Font,
    big: Font,
    button: Font,
}

impl Fonts {
    fn new(s: Scale) -> Self {
        Self {
            title: Font::new(s, 16, 600),
            name: Font::new(s, 14, 600),
            small: Font::new(s, 12, 400),
            big: Font::new(s, 20, 600),
            button: Font::new(s, 13, 600),
        }
    }

    fn get(&self, f: F) -> &Font {
        match f {
            F::Title => &self.title,
            F::Name => &self.name,
            F::Small => &self.small,
            F::Big => &self.big,
            F::Button => &self.button,
        }
    }
}

/// Width of `text` in `font`, in physical pixels.
fn text_width(dc: HDC, font: &Font, text: &str) -> i32 {
    use windows_sys::Win32::Graphics::Gdi::GetTextExtentPoint32W;
    let w: Vec<u16> = text.encode_utf16().collect();
    let mut size = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
    // SAFETY: valid DC and font; the old font is restored.
    unsafe {
        let old = SelectObject(dc, font.0 as _);
        GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32, &mut size);
        SelectObject(dc, old);
    }
    size.cx
}

/// Draws the whole panel; returns the clickable areas.
fn draw(dc: HDC, s: Scale, p: &Palette, client: RECT, st: &State) -> Vec<(usize, RECT)> {
    let px = |v: i32| s.px(v);
    let pxf = |v: f32| v * s.0 as f32 / 96.0;
    let w = client.right;
    let c = &st.content;
    let a = &st.anim;
    let fonts = Fonts::new(s);
    let Some(mut cv) = Canvas::new(client, p.bg) else { return Vec::new() };
    let mut labels: Vec<Label> = Vec::new();
    let mut icons: Vec<(Vec<u8>, usize, i32, i32)> = Vec::new();
    let mut targets = Vec::new();
    let mut label = |font: F, color: COLORREF, text: &str, rect: RECT, flags: u32| {
        labels.push(Label { font, color, text: text.to_string(), rect, flags })
    };

    // Header: a small accent mark, the title, the version.
    cv.round_rect(rect(px(18), px(21), px(22), px(35)), pxf(2.0), p.accent, 1.0, None);
    label(F::Title, p.fg, &c.title, rect(px(30), px(14), w - px(18), px(42)), DT_LEFT | DT_VCENTER);
    label(F::Small, p.sub, &format!("Batly {}", crate::http::VERSION), rect(px(18), px(14), w - px(18), px(42)), DT_RIGHT | DT_VCENTER);

    let mut y = px(HEADER);
    if c.rows.is_empty() {
        label(F::Small, p.sub, &c.empty, rect(px(18), y, w - px(18), y + px(ROW)), DT_LEFT | DT_VCENTER);
        y += px(ROW_STEP);
    }
    let icon_size = px(32) as usize;
    for (n, row) in c.rows.iter().enumerate() {
        let t = a.appear(n);
        let fade = |col: COLORREF| ui::mix(p.bg, col, t);
        let dy = ((1.0 - t) * pxf(10.0)) as i32;
        let cmd = CMD_DEVICE + n;
        let open = c.chart.as_ref().is_some_and(|ch| ch.row == n);
        let hv = a.hover_of(cmd).max(if open { 0.6 } else { 0.0 });
        let r = rect(px(10), y + dy, w - px(10), y + dy + px(ROW));
        let card = ui::mix(p.card, p.hover, hv);
        cv.round_rect(r, pxf(10.0), fade(card), 1.0, Some(fade(ui::mix(p.border, p.accent, hv * 0.35))));
        targets.push((cmd, rect(px(10), y, w - px(10), y + px(ROW))));

        let spec = IconSpec {
            kind: row.kind,
            percent: if row.has_battery { row.percent.map(f32::round) } else { Some(100.0) },
            charging: row.charging,
            light_foreground: !p.light,
            dimmed: !row.online || t < 1.0,
        };
        if t > 0.3 {
            icons.push((icon::render(&spec, icon_size), icon_size, px(22), r.top + (px(ROW) - icon_size as i32) / 2));
        }

        let text_left = px(66);
        let right_w = px(84);
        let right = w - px(22);
        let name_color = if row.online { p.fg } else { p.sub };
        label(
            F::Name,
            fade(name_color),
            &row.name,
            rect(text_left, r.top + px(9), right - right_w - px(8), r.top + px(28)),
            DT_LEFT | DT_VCENTER,
        );
        if !row.detail.is_empty() {
            label(
                F::Small,
                fade(p.sub),
                &row.detail,
                rect(text_left, r.top + px(28), right - right_w - px(8), r.top + px(44)),
                DT_LEFT | DT_VCENTER,
            );
        }

        let (value, color) = match (row.has_battery, row.percent) {
            (false, _) | (true, None) => ("–".to_string(), p.sub),
            (true, Some(v)) => {
                let col = if row.charging || v <= 30.0 { p.level(v, row.charging) } else { p.fg };
                (crate::i18n::percent(v), if row.online { col } else { p.sub })
            }
        };
        let value_box = rect(right - right_w, r.top + px(6), right, r.top + px(32));
        label(F::Big, fade(color), &value, value_box, DT_RIGHT | DT_VCENTER);
        if !row.note.is_empty() {
            label(
                F::Small,
                fade(p.sub),
                &row.note,
                rect(right - right_w - px(40), r.top + px(30), right, r.top + px(45)),
                DT_RIGHT | DT_VCENTER,
            );
        }

        // Level bar along the bottom of the card.
        if row.has_battery {
            let bar = rect(text_left, r.top + px(48), right, r.top + px(52));
            cv.round_rect(bar, pxf(2.0), fade(p.border), 1.0, None);
            let level = a.levels.get(n).copied().unwrap_or(0.0);
            let fill_w = ((bar.right - bar.left) as f32 * level / 100.0) as i32;
            if fill_w > 0 {
                let col = if row.online { p.level(row.percent.unwrap_or(0.0), row.charging) } else { p.sub };
                cv.round_rect(rect(bar.left, bar.top, bar.left + fill_w.max(px(4)), bar.bottom), pxf(2.0), fade(col), 1.0, None);
            }
        }
        y += px(ROW_STEP);
        if let Some(chart) = c.chart.as_ref().filter(|ch| ch.row == n) {
            let progress = a.chart.map_or(1.0, |(_, t)| ui::ease_out(t));
            draw_chart(&mut cv, &mut label, dc, &fonts, s, p, w, y, chart, a, progress, &mut targets);
            y += px(CHART_H + 6);
        }
    }
    y += px(4);

    // Pending alert.
    if let Some((text, later)) = &c.alert {
        let bw = text_width(dc, &fonts.button, later) + px(28);
        let inner_w = w - px(10) * 2 - px(26) - bw - px(12);
        let text_h = ui::text_block(dc, &fonts.small, p.warn_fg, text, rect(0, 0, inner_w, px(400)), true).max(px(20));
        let box_r = rect(px(10), y, w - px(10), y + text_h + px(24));
        cv.round_rect(box_r, pxf(10.0), p.warn_bg, 1.0, None);
        cv.round_rect(
            rect(box_r.left + px(8), box_r.top + px(10), box_r.left + px(11), box_r.bottom - px(10)),
            pxf(1.5),
            p.warn,
            1.0,
            None,
        );
        labels.push(Label {
            font: F::Small,
            color: p.warn_fg,
            text: text.clone(),
            rect: rect(px(26), y + px(12), px(26) + inner_w, y + px(12) + text_h),
            flags: u32::MAX,
        });
        let mid = y + (text_h + px(24)) / 2;
        let b = rect(w - px(22) - bw, mid - px(15), w - px(22), mid + px(15));
        let hv = a.hover_of(CMD_SNOOZE);
        cv.round_rect(b, pxf(15.0), ui::mix(p.card, p.hover, hv), 1.0, Some(p.border));
        labels.push(Label { font: F::Button, color: p.fg, text: later.clone(), rect: b, flags: DT_CENTER | DT_VCENTER });
        targets.push((CMD_SNOOZE, b));
        y = box_r.bottom + px(8);
    }

    // Footer: pill buttons.
    cv.fill(rect(0, y, w, y + 1), p.border);
    let mut x = px(10);
    let top = y + px(12);
    for (cmd, label_text) in &c.buttons {
        let bw = text_width(dc, &fonts.button, label_text) + px(28);
        let b = rect(x, top, x + bw, top + px(32));
        let hv = a.hover_of(*cmd);
        if hv > 0.0 {
            cv.round_rect(b, pxf(16.0), ui::mix(p.bg, p.hover, hv), 1.0, None);
        }
        labels.push(Label {
            font: F::Button,
            color: ui::mix(p.accent, p.fg, hv * 0.25),
            text: label_text.clone(),
            rect: b,
            flags: DT_CENTER | DT_VCENTER,
        });
        targets.push((*cmd, b));
        x += bw + px(4);
    }

    for (rgba, n, ix, iy) in &icons {
        cv.image(rgba, *n, *ix, *iy);
    }
    cv.blit(dc);
    for l in &labels {
        if l.flags == u32::MAX {
            ui::text_block(dc, fonts.get(l.font), l.color, &l.text, l.rect, false);
        } else {
            ui::text(dc, fonts.get(l.font), l.color, &l.text, l.rect, l.flags);
        }
    }
    targets
}

/// The unfolded chart: level over 7 or 30 days, the range buttons, a
/// summary line. Gaps of more than 3 hours (device off, PC asleep) break
/// the line. `progress` (0..1) draws the line from left to right.
#[allow(clippy::too_many_arguments)]
fn draw_chart(
    cv: &mut Canvas,
    label: &mut impl FnMut(F, COLORREF, &str, RECT, u32),
    dc: HDC,
    fonts: &Fonts,
    s: Scale,
    p: &Palette,
    w: i32,
    y: i32,
    chart: &Chart,
    a: &Anim,
    progress: f32,
    targets: &mut Vec<(usize, RECT)>,
) {
    let px = |v: i32| s.px(v);
    let pxf = |v: f32| v * s.0 as f32 / 96.0;
    let box_r = rect(px(10), y, w - px(10), y + px(CHART_H));
    cv.round_rect(box_r, pxf(10.0), p.card, 1.0, Some(p.border));

    // Segmented 7 / 30 days control, top right.
    let seg_w: Vec<i32> = chart.labels[..2].iter().map(|l| text_width(dc, &fonts.button, l) + px(22)).collect();
    let total = seg_w[0] + seg_w[1] + px(4);
    let mut x = w - px(20) - total;
    let seg = rect(x, y + px(10), x + total, y + px(38));
    cv.round_rect(seg, pxf(14.0), p.bg, 1.0, Some(p.border));
    x += px(2);
    for (i, (cmd, days)) in [(CMD_DAYS_7, 7), (CMD_DAYS_30, 30)].into_iter().enumerate() {
        let b = rect(x, seg.top + px(2), x + seg_w[i], seg.bottom - px(2));
        let active = chart.days == days;
        if active {
            cv.round_rect(b, pxf(12.0), p.accent, 1.0, None);
        } else {
            let hv = a.hover_of(cmd);
            if hv > 0.0 {
                cv.round_rect(b, pxf(12.0), ui::mix(p.bg, p.hover, hv), 1.0, None);
            }
        }
        label(F::Button, if active { p.on_accent } else { p.sub }, &chart.labels[i], b, DT_CENTER | DT_VCENTER);
        targets.push((cmd, b));
        x += seg_w[i];
    }

    // Settings link, top left.
    let sw = text_width(dc, &fonts.button, &chart.labels[2]) + px(20);
    let b = rect(px(16), y + px(10), px(16) + sw, y + px(38));
    let hv = a.hover_of(CMD_DEVICE_SETTINGS);
    if hv > 0.0 {
        cv.round_rect(b, pxf(14.0), ui::mix(p.card, p.hover, hv), 1.0, None);
    }
    label(F::Button, p.accent, &chart.labels[2], b, DT_CENTER | DT_VCENTER);
    targets.push((CMD_DEVICE_SETTINGS, b));

    // Plot: 0 / 50 / 100 % guides, then the level.
    let plot = rect(px(52), y + px(50), w - px(26), y + px(CHART_H - 34));
    let (pw, ph) = ((plot.right - plot.left) as f32, (plot.bottom - plot.top) as f32);
    for level in [0, 50, 100] {
        let gy = plot.bottom - (ph * level as f32 / 100.0) as i32;
        cv.fill(rect(plot.left, gy, plot.right, gy + 1), ui::mix(p.card, p.border, 0.8));
        label(F::Small, p.sub, &format!("{level} %"), rect(px(14), gy - px(8), plot.left - px(8), gy + px(8)), DT_RIGHT | DT_VCENTER);
    }
    let span = chart.days as u64 * 86_400;
    let start = chart.now.saturating_sub(span);
    let to_x = |t: u64| plot.left as f32 + (t.saturating_sub(start) as f32 / span.max(1) as f32 * pw).clamp(0.0, pw);
    let to_y = |v: f32| plot.bottom as f32 - v.clamp(0.0, 100.0) / 100.0 * ph;
    let limit = plot.left as f32 + pw * progress;
    let mut runs: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut prev: Option<u64> = None;
    for &(t, v) in chart.points.iter().filter(|(t, _)| *t >= start) {
        if prev.is_none_or(|pt| t.saturating_sub(pt) > 3 * 3600) {
            runs.push(Vec::new());
        }
        prev = Some(t);
        let (x, yy) = (to_x(t), to_y(v));
        let Some(run) = runs.last_mut() else { continue };
        if x <= limit {
            run.push((x, yy));
        } else if let Some(&(lx, ly)) = run.last() {
            // Cut the segment where the drawing has got to.
            if lx < limit {
                let k = (limit - lx) / (x - lx).max(0.001);
                run.push((limit, ly + (yy - ly) * k));
            }
        }
    }
    let mut last_point = None;
    for run in runs.iter().filter(|r| !r.is_empty()) {
        cv.area(run, plot.bottom as f32, p.accent, 0.28);
        cv.polyline(run, p.accent, pxf(2.0));
        last_point = run.last().copied();
    }
    if let Some((lx, ly)) = last_point.filter(|_| progress >= 1.0) {
        cv.circle(lx as i32, ly as i32, pxf(5.0), p.accent, 0.25);
        cv.circle(lx as i32, ly as i32, pxf(3.0), p.accent, 1.0);
    }
    label(F::Small, p.sub, &chart.summary, rect(px(16), plot.bottom + px(8), w - px(18), y + px(CHART_H - 6)), DT_LEFT | DT_VCENTER);
}

fn hit(lparam: LPARAM) -> Option<usize> {
    let (x, y) = ((lparam & 0xFFFF) as i16 as i32, (lparam >> 16 & 0xFFFF) as i16 as i32);
    STATE.with(|s| s.borrow().as_ref().and_then(|st| st.targets.iter().find(|(_, r)| ui::inside(r, x, y)).map(|(c, _)| *c)))
}

/// Commands that change what the panel shows: it stays open.
fn keeps_open(cmd: usize) -> bool {
    cmd == CMD_SNOOZE || cmd == CMD_DAYS_7 || cmd == CMD_DAYS_30 || (CMD_DEVICE..CMD_DEVICE + 1000).contains(&cmd)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER if wparam == TIMER => {
            let busy = STATE.with(|s| s.borrow_mut().as_mut().map(tick)).unwrap_or(false);
            InvalidateRect(hwnd, null(), 0);
            if !busy {
                KillTimer(hwnd, TIMER);
            }
            0
        }
        WM_SETCURSOR => {
            let hot = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.hot));
            SetCursor(LoadCursorW(null_mut(), if hot.is_some() { IDC_HAND } else { IDC_ARROW }));
            1
        }
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
            let hot = hit(lparam);
            let changed = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let st = s.as_mut()?;
                (st.hot != hot).then(|| st.hot = hot)
            });
            if changed.is_some() {
                let mut tme =
                    TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TrackMouseEvent(&mut tme);
                SetCursor(LoadCursorW(null_mut(), if hot.is_some() { IDC_HAND } else { IDC_ARROW }));
                kick(hwnd);
            }
            0
        }
        WM_MOUSELEAVE => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.hot = None;
                }
            });
            kick(hwnd);
            0
        }
        WM_LBUTTONUP => {
            if let Some(cmd) = hit(lparam) {
                let owner = STATE.with(|s| s.borrow().as_ref().map(|st| st.owner));
                if let Some(owner) = owner {
                    PostMessageW(owner, WM_COMMAND, cmd, 0);
                }
                if !keeps_open(cmd) {
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
            KillTimer(hwnd, TIMER);
            STATE.with(|s| *s.borrow_mut() = None);
            LAST_CLOSE.with(|c| *c.borrow_mut() = Some(Instant::now()));
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

use std::mem::{size_of, size_of_val};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_clicks_keep_the_panel_open() {
        assert!(keeps_open(CMD_DEVICE));
        assert!(keeps_open(CMD_DEVICE + 3));
        assert!(keeps_open(CMD_DAYS_30));
        assert!(!keeps_open(CMD_DEVICE_SETTINGS));
        assert!(!keeps_open(CMD_SETTINGS));
    }
}
