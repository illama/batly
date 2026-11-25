//! Hosts an egui interface in a plain Win32 window: mouse, keyboard, text,
//! wheel, clipboard, cursor shapes, DPI and the redraws egui asks for
//! (animations), drawn by the software renderer of [`crate::soft`].
//!
//! The window lives on the tray thread and its message loop, like the
//! other windows of Batly. A [`View`] draws the content and is dropped with
//! the window.

use crate::soft::Raster;
use crate::ui;
use crate::win::wide;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2};
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, KillTimer, LoadCursorW, LoadIconW, RegisterClassW,
    SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, CS_DBLCLKS, CW_USEDEFAULT, HTCLIENT, IDC_ARROW,
    IDC_HAND, IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT, MINMAXINFO, SWP_NOMOVE,
    SWP_NOZORDER, SW_RESTORE, SW_SHOW, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_THEMECHANGED, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_THICKFRAME,
};

/// What a window shows.
pub trait View {
    /// Draws one frame. Set `ctl.close` to close the window.
    fn ui(&mut self, ui: &mut egui::Ui, ctl: &mut Ctl);
    /// Called once the window is gone.
    fn closed(&mut self) {}
}

/// What a frame can ask of its window.
pub struct Ctl {
    pub hwnd: HWND,
    pub close: bool,
    pub title: Option<String>,
}

pub struct Options {
    pub class: &'static str,
    pub title: String,
    /// Client size, logical pixels.
    pub size: (f32, f32),
    pub min_size: (f32, f32),
    pub resizable: bool,
}

struct Host {
    hwnd: HWND,
    ctx: egui::Context,
    view: Box<dyn View>,
    raster: Raster,
    events: Vec<Event>,
    start: Instant,
    cursor: egui::CursorIcon,
    tracking: bool,
    pending_surrogate: Option<u16>,
    min_size: (f32, f32),
    light: bool,
    /// Flyout: closes as soon as it loses the focus.
    popup: bool,
}

thread_local! {
    static HOSTS: RefCell<HashMap<isize, Option<Host>>> = RefCell::new(HashMap::new());
    static CLASSES: RefCell<HashMap<&'static str, isize>> = RefCell::new(HashMap::new());
}

const TIMER: usize = 1;

/// The open window of `class`, if any.
pub fn find(class: &'static str) -> Option<HWND> {
    CLASSES.with(|c| c.borrow().get(class).map(|h| *h as HWND))
}

/// Brings the open window of `class` to the front; false when none is open.
pub fn focus(class: &'static str) -> bool {
    match find(class) {
        Some(h) => {
            // SAFETY: our own window.
            unsafe {
                ShowWindow(h, SW_RESTORE);
                SetForegroundWindow(h);
            }
            true
        }
        None => false,
    }
}

fn style_of(resizable: bool) -> u32 {
    WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | if resizable { WS_THICKFRAME | WS_MAXIMIZEBOX } else { 0 }
}

/// Opens a window showing `view`.
pub fn open(opts: Options, view: Box<dyn View>) -> Option<HWND> {
    if focus(opts.class) {
        return find(opts.class);
    }
    create(opts, None, view)
}

/// Opens a borderless flyout of `size` (logical pixels) next to `anchor`
/// (screen point, e.g. the cursor on the tray icon); it closes when it
/// loses the focus.
pub fn popup(class: &'static str, size: (f32, f32), anchor: (i32, i32), view: Box<dyn View>) -> Option<HWND> {
    if let Some(h) = find(class) {
        // SAFETY: our own window.
        unsafe { DestroyWindow(h) };
    }
    create(Options { class, title: "Batly".into(), size, min_size: size, resizable: false }, Some(anchor), view)
}

fn create(opts: Options, anchor: Option<(i32, i32)>, view: Box<dyn View>) -> Option<HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP};
    // SAFETY: standard class registration and window creation.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide(opts.class);
        let mut class: WNDCLASSW = zeroed();
        class.style = CS_DBLCLKS;
        class.lpfnWndProc = Some(wnd_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        class.hIcon = LoadIconW(instance, 1 as _);
        RegisterClassW(&class);
        let style = if anchor.is_some() { WS_POPUP } else { style_of(opts.resizable) };
        let ex = if anchor.is_some() { WS_EX_TOOLWINDOW | WS_EX_TOPMOST } else { 0 };
        let hwnd = CreateWindowExW(
            ex,
            class_name.as_ptr(),
            wide(&opts.title).as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            200,
            200,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let ctx = egui::Context::default();
        crate::look::install(&ctx);
        let light = ui::apps_light();
        crate::look::apply(&ctx, light);
        HOSTS.with(|h| {
            h.borrow_mut().insert(
                hwnd as isize,
                Some(Host {
                    hwnd,
                    ctx,
                    view,
                    raster: Raster::new(),
                    events: Vec::new(),
                    start: Instant::now(),
                    cursor: egui::CursorIcon::Default,
                    tracking: false,
                    pending_surrogate: None,
                    min_size: opts.min_size,
                    light,
                    popup: anchor.is_some(),
                }),
            )
        });
        CLASSES.with(|c| c.borrow_mut().insert(opts.class, hwnd as isize));
        frame_colours(hwnd, light);
        // Size the window around the wanted client area.
        let scale = ui::Scale::of(hwnd);
        let mut r = RECT { left: 0, top: 0, right: scale.px(opts.size.0 as i32), bottom: scale.px(opts.size.1 as i32) };
        AdjustWindowRectEx(&mut r, style, 0, ex);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        match anchor {
            Some(pt) => place_near(hwnd, pt, w, h),
            None => {
                SetWindowPos(hwnd, null_mut(), 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER);
                center(hwnd);
            }
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        Some(hwnd)
    }
}

/// Puts a flyout of `w` × `h` next to `pt`, inside the work area: above
/// the point when the taskbar is at the bottom, to its left when there is
/// no room on the right.
fn place_near(hwnd: HWND, pt: (i32, i32), w: i32, h: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    // SAFETY: plain queries with valid out-pointers.
    unsafe {
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromPoint(POINT { x: pt.0, y: pt.1 }, MONITOR_DEFAULTTONEAREST), &mut info);
        let work = info.rcWork;
        let x = if pt.0 + w > work.right { pt.0 - w } else { pt.0 };
        let y = if pt.1 - h >= work.top { pt.1 - h } else { pt.1 };
        let x = x.clamp(work.left, (work.right - w).max(work.left));
        let y = y.clamp(work.top, (work.bottom - h).max(work.top));
        SetWindowPos(hwnd, windows_sys::Win32::UI::WindowsAndMessaging::HWND_TOPMOST, x, y, w, h, 0);
    }
}

/// Centres the window on the screen of the cursor.
fn center(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};
    // SAFETY: plain queries with valid out-pointers.
    unsafe {
        let mut pt = zeroed();
        GetCursorPos(&mut pt);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut info);
        let mut r: RECT = zeroed();
        GetWindowRect(hwnd, &mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let work = info.rcWork;
        let x = work.left + ((work.right - work.left) - w).max(0) / 2;
        let y = work.top + ((work.bottom - work.top) - h).max(0) / 2;
        SetWindowPos(hwnd, null_mut(), x, y, 0, 0, windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOSIZE | SWP_NOZORDER);
    }
}

/// Title bar in the window colours (Windows 11), dark or light, rounded.
fn frame_colours(hwnd: HWND, light: bool) {
    let p = if light { ui::LIGHT } else { ui::DARK };
    ui::modern_frame(hwnd, p.border);
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_TEXT_COLOR: u32 = 36;
    // SAFETY: attribute values valid for their sizes; ignored before Windows 11.
    unsafe {
        DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&p.bg as *const u32).cast(), 4);
        DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, (&p.fg as *const u32).cast(), 4);
    }
}

fn ppp(hwnd: HWND) -> f32 {
    // SAFETY: plain query.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut r: RECT = unsafe { zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { GetClientRect(hwnd, &mut r) };
    (r.right.max(1), r.bottom.max(1))
}

fn modifiers() -> Modifiers {
    // SAFETY: plain queries.
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    let ctrl = down(VK_CONTROL);
    Modifiers { alt: down(VK_MENU), ctrl, shift: down(VK_SHIFT), mac_cmd: false, command: ctrl }
}

fn key_of(vk: u16) -> Option<Key> {
    Some(match vk {
        VK_LEFT => Key::ArrowLeft,
        VK_RIGHT => Key::ArrowRight,
        VK_UP => Key::ArrowUp,
        VK_DOWN => Key::ArrowDown,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_TAB => Key::Tab,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_INSERT => Key::Insert,
        VK_SPACE => Key::Space,
        0x30..=0x39 => Key::from_name(&((vk - 0x30) as u8 as char).to_string())?,
        0x41..=0x5A => Key::from_name(&((vk as u8) as char).to_string())?,
        _ if (VK_F1..VK_F1 + 12).contains(&vk) => Key::from_name(&format!("F{}", vk - VK_F1 + 1))?,
        _ => return None,
    })
}

fn pos_of(hwnd: HWND, lparam: LPARAM) -> Pos2 {
    let s = ppp(hwnd);
    let (x, y) = ((lparam & 0xFFFF) as i16 as f32, (lparam >> 16 & 0xFFFF) as i16 as f32);
    Pos2::new(x / s, y / s)
}

// ---------------------------------------------------------------- clipboard

fn clipboard_get() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    // SAFETY: the clipboard is opened and closed here; the locked memory is
    // a NUL-terminated UTF-16 string.
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return None;
        }
        let h = GetClipboardData(CF_UNICODETEXT);
        let mut out = None;
        if !h.is_null() {
            let p = GlobalLock(h as _) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                GlobalUnlock(h as _);
            }
        }
        CloseClipboard();
        out
    }
}

fn clipboard_set(text: &str) {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let w = wide(text);
    // SAFETY: the memory is sized for the string; ownership passes to the
    // clipboard on success.
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        if !h.is_null() {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                GlobalUnlock(h);
                SetClipboardData(CF_UNICODETEXT, h as _);
            }
        }
        CloseClipboard();
    }
}

// ------------------------------------------------------------------ frames

/// Runs `f` with the host of `hwnd` taken out of the table, so that a
/// message box or a nested message during the frame cannot borrow it twice.
fn with_host<R>(hwnd: HWND, f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    let mut host = HOSTS.with(|h| h.borrow_mut().get_mut(&(hwnd as isize)).and_then(Option::take))?;
    let r = f(&mut host);
    HOSTS.with(|h| {
        if let Some(slot) = h.borrow_mut().get_mut(&(hwnd as isize)) {
            *slot = Some(host);
        }
    });
    Some(r)
}

fn push(hwnd: HWND, e: Event) {
    with_host(hwnd, |h| h.events.push(e));
    // SAFETY: our own window.
    unsafe { InvalidateRect(hwnd, null(), 0) };
}

/// Runs one egui frame and paints it; returns whether to close and
/// whether the next frame is wanted right away (animation).
fn frame(h: &mut Host) -> (bool, bool) {
    let s = ppp(h.hwnd);
    let (w, hgt) = client_size(h.hwnd);
    let mut raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(w as f32 / s, hgt as f32 / s))),
        time: Some(h.start.elapsed().as_secs_f64()),
        events: std::mem::take(&mut h.events),
        focused: true,
        ..Default::default()
    };
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(s);
    let mut ctl = Ctl { hwnd: h.hwnd, close: false, title: None };
    let view = &mut h.view;
    let out = h.ctx.run_ui(raw, |ui| view.ui(ui, &mut ctl));

    // Platform side: cursor, clipboard, links.
    h.cursor = out.platform_output.cursor_icon;
    for cmd in &out.platform_output.commands {
        match cmd {
            egui::OutputCommand::CopyText(t) => clipboard_set(t),
            egui::OutputCommand::OpenUrl(u) => crate::win::shell_open(h.hwnd, &u.url, None),
            _ => {}
        }
    }
    if let Some(t) = &ctl.title {
        // SAFETY: our own window; NUL-terminated title.
        unsafe { SetWindowTextW(h.hwnd, wide(t).as_ptr()) };
    }

    let mut delta = out.textures_delta;
    let free = h.raster.set_textures(&mut delta);
    let prims = h.ctx.tessellate(out.shapes, out.pixels_per_point);
    let clear = crate::look::colour(if h.light { ui::LIGHT.bg } else { ui::DARK.bg });
    h.raster.paint(w as usize, hgt as usize, out.pixels_per_point, clear, &prims);
    h.raster.free_textures(&free);

    // Next frame: right away for an animation (paced by the screen
    // refresh, see WM_PAINT), later when egui asks for it, or on the next
    // input.
    let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
    let now = delay < Duration::from_millis(8);
    // SAFETY: our own window.
    unsafe {
        if !now && delay < Duration::from_secs(5) {
            SetTimer(h.hwnd, TIMER, (delay.as_millis() as u32).clamp(10, 5000), None);
        } else {
            KillTimer(h.hwnd, TIMER);
        }
    }
    (ctl.close, now)
}

fn blit(h: &Host, dc: windows_sys::Win32::Graphics::Gdi::HDC) {
    let (w, hgt) = (h.raster.width, h.raster.height);
    if w == 0 || hgt == 0 {
        return;
    }
    // The renderer already writes BGRA, the order of Windows bitmaps.
    let bgra = &h.raster.buf;
    // SAFETY: BITMAPINFO describes exactly `bgra`.
    unsafe {
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w as i32;
        info.bmiHeader.biHeight = -(hgt as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        SetDIBitsToDevice(dc, 0, 0, w as u32, hgt as u32, 0, 0, 0, hgt as u32, bgra.as_ptr().cast(), &info, DIB_RGB_COLORS);
    }
}

fn cursor_of(c: egui::CursorIcon) -> windows_sys::core::PCWSTR {
    use egui::CursorIcon as C;
    match c {
        C::PointingHand => IDC_HAND,
        C::Text | C::VerticalText => IDC_IBEAM,
        C::Wait | C::Progress => IDC_WAIT,
        C::NotAllowed | C::NoDrop => IDC_NO,
        C::Move | C::AllScroll | C::Grab | C::Grabbing => IDC_SIZEALL,
        C::ResizeHorizontal | C::ResizeEast | C::ResizeWest | C::ResizeColumn => IDC_SIZEWE,
        C::ResizeVertical | C::ResizeNorth | C::ResizeSouth | C::ResizeRow => IDC_SIZENS,
        C::ResizeNeSw | C::ResizeNorthEast | C::ResizeSouthWest => IDC_SIZENESW,
        C::ResizeNwSe | C::ResizeNorthWest | C::ResizeSouthEast => IDC_SIZENWSE,
        _ => IDC_ARROW,
    }
}

fn button(hwnd: HWND, lparam: LPARAM, button: PointerButton, pressed: bool) {
    // SAFETY: our own window.
    unsafe {
        if pressed {
            SetCapture(hwnd);
        } else {
            ReleaseCapture();
        }
    }
    push(hwnd, Event::PointerButton { pos: pos_of(hwnd, lparam), button, pressed, modifiers: modifiers() });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let result = with_host(hwnd, |h| {
                let result = frame(h);
                blit(h, dc);
                result
            });
            EndPaint(hwnd, &ps);
            match result {
                Some((true, _)) => {
                    DestroyWindow(hwnd);
                }
                Some((false, true)) => {
                    // Animation: wait for the compositor's next refresh, then
                    // draw again (60 / 120 / 144 Hz, whatever the screen does).
                    if windows_sys::Win32::Graphics::Dwm::DwmFlush() == 0 {
                        InvalidateRect(hwnd, null(), 0);
                    } else {
                        // No compositor: about 60 frames per second.
                        SetTimer(hwnd, TIMER, 16, None);
                    }
                }
                _ => {}
            }
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER if wparam == TIMER => {
            KillTimer(hwnd, TIMER);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_SIZE => {
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_MOUSEMOVE => {
            let start = with_host(hwnd, |h| !std::mem::replace(&mut h.tracking, true)).unwrap_or(false);
            if start {
                let mut tme =
                    TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TrackMouseEvent(&mut tme);
            }
            push(hwnd, Event::PointerMoved(pos_of(hwnd, lparam)));
            0
        }
        WM_MOUSELEAVE => {
            with_host(hwnd, |h| h.tracking = false);
            push(hwnd, Event::PointerGone);
            0
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            button(hwnd, lparam, PointerButton::Primary, true);
            0
        }
        WM_LBUTTONUP => {
            button(hwnd, lparam, PointerButton::Primary, false);
            0
        }
        WM_RBUTTONDOWN => {
            button(hwnd, lparam, PointerButton::Secondary, true);
            0
        }
        WM_RBUTTONUP => {
            button(hwnd, lparam, PointerButton::Secondary, false);
            0
        }
        WM_MBUTTONDOWN => {
            button(hwnd, lparam, PointerButton::Middle, true);
            0
        }
        WM_MBUTTONUP => {
            button(hwnd, lparam, PointerButton::Middle, false);
            0
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let d = (wparam >> 16) as u16 as i16 as f32 / 120.0;
            let delta = if msg == WM_MOUSEWHEEL { Vec2::new(0.0, d) } else { Vec2::new(-d, 0.0) };
            push(
                hwnd,
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: modifiers(),
                },
            );
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let m = modifiers();
            let vk = wparam as u16;
            if pressed && m.ctrl && !m.alt {
                match vk {
                    0x43 => return push_ret(hwnd, Event::Copy),
                    0x58 => return push_ret(hwnd, Event::Cut),
                    0x56 => {
                        if let Some(t) = clipboard_get() {
                            push(hwnd, Event::Paste(t.replace("\r\n", "\n")));
                        }
                        return 0;
                    }
                    _ => {}
                }
            }
            if let Some(key) = key_of(vk) {
                let repeat = pressed && lparam & (1 << 30) != 0;
                push(hwnd, Event::Key { key, physical_key: None, pressed, repeat, modifiers: m });
            }
            if msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            0
        }
        WM_CHAR => {
            let unit = wparam as u16;
            let m = modifiers();
            // Control characters and Ctrl shortcuts are keys, not text
            // (Ctrl+Alt is AltGr: its characters are text).
            if unit < 0x20 || unit == 0x7F || (m.ctrl && !m.alt) {
                return 0;
            }
            let text = if (0xD800..0xDC00).contains(&unit) {
                with_host(hwnd, |h| h.pending_surrogate = Some(unit));
                return 0;
            } else if (0xDC00..0xE000).contains(&unit) {
                let high = with_host(hwnd, |h| h.pending_surrogate.take()).flatten();
                match high {
                    Some(hi) => String::from_utf16_lossy(&[hi, unit]),
                    None => return 0,
                }
            } else {
                String::from_utf16_lossy(&[unit])
            };
            push(hwnd, Event::Text(text));
            0
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            push(hwnd, Event::WindowFocused(msg == WM_SETFOCUS));
            0
        }
        WM_SETCURSOR if (lparam & 0xFFFF) as u32 == HTCLIENT => {
            let c = with_host(hwnd, |h| h.cursor).unwrap_or_default();
            SetCursor(LoadCursorW(null_mut(), cursor_of(c)));
            1
        }
        WM_GETMINMAXINFO => {
            let min = with_host(hwnd, |h| h.min_size);
            if let Some((w, hgt)) = min {
                let s = ppp(hwnd);
                let info = &mut *(lparam as *mut MINMAXINFO);
                let mut r = RECT { left: 0, top: 0, right: (w * s) as i32, bottom: (hgt * s) as i32 };
                AdjustWindowRectEx(&mut r, style_of(true), 0, 0);
                info.ptMinTrackSize.x = r.right - r.left;
                info.ptMinTrackSize.y = r.bottom - r.top;
            }
            0
        }
        WM_DPICHANGED => {
            let r = &*(lparam as *const RECT);
            SetWindowPos(hwnd, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            let light = ui::apps_light();
            let changed = with_host(hwnd, |h| {
                let changed = h.light != light;
                if changed {
                    h.light = light;
                    crate::look::apply(&h.ctx, light);
                }
                changed
            });
            if changed == Some(true) {
                frame_colours(hwnd, light);
                InvalidateRect(hwnd, null(), 0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_ACTIVATE => {
            if wparam & 0xFFFF == 0 && with_host(hwnd, |h| h.popup) == Some(true) {
                // A flyout closes when another window gets the focus.
                DestroyWindow(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER);
            let host = HOSTS.with(|h| h.borrow_mut().remove(&(hwnd as isize))).flatten();
            CLASSES.with(|c| c.borrow_mut().retain(|_, h| *h != hwnd as isize));
            if let Some(mut host) = host {
                host.view.closed();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn push_ret(hwnd: HWND, e: Event) -> LRESULT {
    push(hwnd, e);
    0
}
