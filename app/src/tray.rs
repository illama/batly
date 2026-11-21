//! Notification area icons, context menu and the UI message loop.
//!
//! Everything UI related runs on the main thread. Backends only post a
//! (coalesced) `WM_APP_SYNC` message when the device registry changes.

use crate::config::Config;
use crate::ghub::GHub;
use crate::icon::{self, IconSpec};
use crate::model::{Device, Hub, Source, Status};
use crate::native::Native;
use crate::win::{self, copy_wide, wide};
use crate::{http, i18n, log};
use std::cell::RefCell;
use std::collections::HashSet;
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush, SelectObject,
    SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    DT_CENTER, DT_NOCLIP, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage, HICON,
    HMENU, LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, TPM_BOTTOMALIGN,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DEVICECHANGE,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_NULL, WM_POWERBROADCAST, WM_SETTINGCHANGE, WM_TIMER, WM_USER, WNDCLASSW,
    WS_OVERLAPPED,
};

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_SYNC: u32 = WM_APP + 2;
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const DBT_DEVNODES_CHANGED: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

const TIMER_RESCAN: usize = 1;
const TIMER_RESUME: usize = 2;

const ID_NUMERIC: usize = 1;
const ID_LOW_BATTERY: usize = 2;
const ID_AUTOSTART: usize = 3;
const ID_REFRESH: usize = 4;
const ID_SETTINGS: usize = 5;
const ID_WEB: usize = 6;
const ID_QUIT: usize = 7;
const ID_DEVICE_BASE: usize = 1000;

const PLACEHOLDER_UID: u32 = 1;
const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

static SYNC_PENDING: AtomicBool = AtomicBool::new(false);
static THEME_DIRTY: AtomicBool = AtomicBool::new(false);
static READD_ICONS: AtomicBool = AtomicBool::new(false);
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the application state unless it is already borrowed
/// (re-entrant window message): returns `None` in that case.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut app| app.as_mut().map(f)))
}

/// What an icon currently shows; used to skip redundant updates.
#[derive(Clone, PartialEq)]
struct Shown {
    spec: IconSpec,
    numeric: bool,
    size: i32,
    tip: String,
}

struct TrayIcon {
    uid: u32,
    /// `None` for the placeholder icon.
    device_id: Option<String>,
    hicon: HICON,
    shown: Option<Shown>,
    added: bool,
}

pub struct Services {
    pub native: Option<Native>,
    pub ghub: Option<GHub>,
    pub web_url: Option<String>,
}

struct App {
    hwnd: HWND,
    hub: Arc<Hub>,
    cfg: Config,
    cfg_path: PathBuf,
    services: Services,
    icons: Vec<TrayIcon>,
    next_uid: u32,
    light_taskbar: bool,
    low_notified: HashSet<String>,
}

fn taskbar_is_light() -> bool {
    // Missing value (Windows 8 and earlier): the taskbar is dark.
    win::reg_read_dword(PERSONALIZE_KEY, "SystemUsesLightTheme").is_some_and(|v| v != 0)
}

impl App {
    fn save(&self) {
        if let Err(e) = self.cfg.save(&self.cfg_path) {
            log!("cannot save settings: {e}");
        }
    }

    fn icon_size(&self) -> i32 {
        // SAFETY: plain FFI calls on our own window.
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(self.hwnd)) };
        if size > 0 {
            size
        } else {
            16
        }
    }

    fn notify_data(&self, uid: u32) -> NOTIFYICONDATAW {
        // SAFETY: NOTIFYICONDATAW is plain data.
        let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = uid;
        data
    }

    /// Brings the tray icons in line with the device registry.
    fn sync(&mut self) {
        if THEME_DIRTY.swap(false, Ordering::Relaxed) {
            self.light_taskbar = taskbar_is_light();
        }
        if READD_ICONS.swap(false, Ordering::Relaxed) {
            // Explorer restarted: every icon is gone.
            for icon in &mut self.icons {
                icon.added = false;
                icon.shown = None;
            }
        }
        let devices = self.hub.snapshot();

        // First run: give the first battery powered device an icon.
        if !self.cfg.ui.first_run_done {
            if let Some(d) = devices.iter().find(|d| d.has_battery) {
                self.cfg.ui.selected_devices.push(d.id.clone());
                self.cfg.ui.first_run_done = true;
                self.save();
            }
        }

        let visible: Vec<&Device> =
            devices.iter().filter(|d| self.cfg.ui.selected_devices.iter().any(|s| *s == d.id)).collect();
        let wanted: Vec<Option<&Device>> =
            if visible.is_empty() { vec![None] } else { visible.into_iter().map(Some).collect() };

        // Remove icons that are no longer wanted.
        let mut i = 0;
        while i < self.icons.len() {
            let keep = wanted.iter().any(|w| w.map(|d| &d.id) == self.icons[i].device_id.as_ref());
            if keep {
                i += 1;
            } else {
                let icon = self.icons.remove(i);
                self.remove_icon(icon);
            }
        }

        let size = self.icon_size();
        let numeric = self.cfg.ui.numeric_display;
        for device in wanted {
            let id = device.map(|d| d.id.clone());
            let index = match self.icons.iter().position(|i| i.device_id == id) {
                Some(index) => index,
                None => {
                    let uid = if id.is_none() {
                        PLACEHOLDER_UID
                    } else {
                        self.next_uid += 1;
                        self.next_uid
                    };
                    self.icons.push(TrayIcon { uid, device_id: id, hicon: null_mut(), shown: None, added: false });
                    self.icons.len() - 1
                }
            };
            let shown = Shown {
                spec: match device {
                    Some(d) => IconSpec {
                        kind: d.kind,
                        percent: d.battery.map(|b| b.percent.round()),
                        charging: d.battery.is_some_and(|b| b.status == Status::Charging),
                        light_foreground: !self.light_taskbar,
                        dimmed: !d.online,
                    },
                    None => IconSpec {
                        kind: crate::model::DeviceKind::Mouse,
                        percent: None,
                        charging: false,
                        light_foreground: !self.light_taskbar,
                        dimmed: false,
                    },
                },
                numeric: numeric && device.is_some(),
                size,
                tip: match device {
                    Some(d) => i18n::tooltip(d),
                    None => placeholder_tip(&devices),
                },
            };
            self.update_icon(index, shown);
        }

        self.low_battery_check(&devices);
    }

    fn update_icon(&mut self, index: usize, shown: Shown) {
        let icon = &self.icons[index];
        if icon.added && icon.shown.as_ref() == Some(&shown) {
            return;
        }
        let redraw = icon.hicon.is_null()
            || icon.shown.as_ref().is_none_or(|s| (s.spec, s.numeric, s.size) != (shown.spec, shown.numeric, shown.size));
        let mut hicon = icon.hicon;
        if redraw {
            let n = shown.size as usize;
            let rgba = match (shown.numeric, shown.spec.percent) {
                (true, Some(p)) => render_number(&shown.spec, p, n).unwrap_or_else(|| icon::render(&shown.spec, n)),
                _ => icon::render(&shown.spec, n),
            };
            hicon = hicon_from_rgba(&rgba, n);
        }

        let mut data = self.notify_data(icon.uid);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_APP_TRAY;
        data.hIcon = hicon;
        copy_wide(&mut data.szTip, &shown.tip);
        // SAFETY: `data` is fully initialised.
        let ok = unsafe {
            if icon.added {
                Shell_NotifyIconW(NIM_MODIFY, &data) != 0
            } else {
                let added = Shell_NotifyIconW(NIM_ADD, &data) != 0;
                if added {
                    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                    Shell_NotifyIconW(NIM_SETVERSION, &data);
                }
                added
            }
        };

        let icon = &mut self.icons[index];
        if redraw {
            if !icon.hicon.is_null() {
                // SAFETY: we created this icon and the shell copied it.
                unsafe { DestroyIcon(icon.hicon) };
            }
            icon.hicon = hicon;
        }
        // When the taskbar is not ready yet (early at logon) the add fails;
        // the "TaskbarCreated" broadcast triggers another attempt.
        icon.added = ok;
        icon.shown = ok.then_some(shown);
    }

    fn remove_icon(&self, icon: TrayIcon) {
        let data = self.notify_data(icon.uid);
        // SAFETY: `data` identifies an icon of our window.
        unsafe {
            if icon.added {
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
            if !icon.hicon.is_null() {
                DestroyIcon(icon.hicon);
            }
        }
    }

    fn remove_all_icons(&mut self) {
        for icon in std::mem::take(&mut self.icons) {
            self.remove_icon(icon);
        }
    }

    fn low_battery_check(&mut self, devices: &[Device]) {
        let threshold = self.cfg.ui.low_battery_threshold as f32;
        for d in devices {
            let Some(b) = d.battery else { continue };
            let plugged = matches!(b.status, Status::Charging | Status::Full);
            if threshold > 0.0 && d.online && !plugged && b.percent <= threshold {
                if self.low_notified.insert(d.id.clone()) {
                    self.balloon(d, i18n::low_battery_title(), &i18n::low_battery_text(&d.name, b.percent));
                }
            } else if plugged || b.percent > threshold + 5.0 {
                self.low_notified.remove(&d.id);
            }
        }
    }

    fn balloon(&self, device: &Device, title: &str, text: &str) {
        let Some(icon) = self
            .icons
            .iter()
            .find(|i| i.device_id.as_deref() == Some(device.id.as_str()))
            .or_else(|| self.icons.first())
        else {
            return;
        };
        let mut data = self.notify_data(icon.uid);
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_WARNING;
        copy_wide(&mut data.szInfoTitle, title);
        copy_wide(&mut data.szInfo, text);
        // SAFETY: `data` is fully initialised.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    /// Builds the context menu; returns it with the device ids behind the
    /// `ID_DEVICE_BASE + n` commands.
    fn build_menu(&self) -> (HMENU, Vec<String>) {
        let devices = self.hub.snapshot();
        // SAFETY: menu handles are created and filled on this thread.
        unsafe {
            let menu = CreatePopupMenu();
            let add = |id: usize, text: &str, flags: u32| {
                AppendMenuW(menu, MF_STRING | flags, id, wide(text).as_ptr());
            };
            let check = |on: bool| if on { MF_CHECKED } else { 0 };

            if devices.is_empty() {
                add(0, i18n::no_device(), MF_GRAYED);
            }
            for (n, d) in devices.iter().enumerate() {
                let source = if d.source == Source::GHub { " (G HUB)" } else { "" };
                let text = format!("{}{}\t{}", d.name.replace('&', "&&"), source, i18n::describe(d));
                let selected = self.cfg.ui.selected_devices.iter().any(|s| *s == d.id);
                add(ID_DEVICE_BASE + n, &text, check(selected) | if d.has_battery || selected { 0 } else { MF_GRAYED });
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            add(ID_NUMERIC, i18n::numeric_display(), check(self.cfg.ui.numeric_display));
            add(ID_LOW_BATTERY, i18n::low_battery_alerts(), check(self.cfg.ui.low_battery_threshold > 0));
            add(ID_AUTOSTART, i18n::autostart(), check(win::autostart_enabled()));
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            add(ID_REFRESH, i18n::refresh(), 0);
            add(ID_SETTINGS, i18n::open_settings(), 0);
            if self.services.web_url.is_some() {
                add(ID_WEB, i18n::open_web(), 0);
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            add(0, &format!("LGSTray {}", http::VERSION), MF_GRAYED);
            add(ID_QUIT, i18n::quit(), 0);
            (menu, devices.into_iter().map(|d| d.id).collect())
        }
    }

    /// Handles a menu command; returns true when the application must quit.
    fn command(&mut self, id: usize, device_ids: &[String]) -> bool {
        match id {
            ID_NUMERIC => {
                self.cfg.ui.numeric_display = !self.cfg.ui.numeric_display;
                self.save();
            }
            ID_LOW_BATTERY => {
                self.cfg.ui.low_battery_threshold = if self.cfg.ui.low_battery_threshold > 0 { 0 } else { 15 };
                self.low_notified.clear();
                self.save();
            }
            ID_AUTOSTART => win::set_autostart(!win::autostart_enabled()),
            ID_REFRESH => self.refresh_devices(),
            ID_SETTINGS => {
                win::shell_open(self.hwnd, "notepad.exe", Some(&format!("\"{}\"", self.cfg_path.display())))
            }
            ID_WEB => {
                if let Some(url) = &self.services.web_url {
                    win::shell_open(self.hwnd, url, None);
                }
            }
            ID_QUIT => return true,
            n if n >= ID_DEVICE_BASE => {
                if let Some(device_id) = device_ids.get(n - ID_DEVICE_BASE) {
                    let selected = &mut self.cfg.ui.selected_devices;
                    match selected.iter().position(|s| s == device_id) {
                        Some(pos) => {
                            selected.remove(pos);
                        }
                        None => selected.push(device_id.clone()),
                    }
                    self.cfg.ui.first_run_done = true;
                    self.save();
                }
            }
            _ => return false,
        }
        self.sync();
        false
    }

    fn refresh_devices(&self) {
        if let Some(native) = &self.services.native {
            native.refresh();
        }
        if let Some(ghub) = &self.services.ghub {
            ghub.refresh();
        }
    }
}

fn placeholder_tip(devices: &[Device]) -> String {
    if devices.is_empty() {
        return i18n::searching().into();
    }
    let mut tip = String::from("LGSTray");
    for d in devices.iter().filter(|d| d.has_battery) {
        let line = format!("\n{}: {}", d.name, i18n::describe(d));
        if tip.chars().count() + line.chars().count() > 120 {
            break;
        }
        tip.push_str(&line);
    }
    tip
}

// ------------------------------------------------------------- rendering

/// Builds an icon from straight-alpha RGBA pixels (rows top to bottom).
fn hicon_from_rgba(rgba: &[u8], n: usize) -> HICON {
    // Icon resource layout: BITMAPINFOHEADER (height doubled), bottom-up
    // BGRA colour bitmap, then the 1 bpp AND mask (unused with alpha).
    let mask_stride = n.div_ceil(32) * 4;
    let mut bits = Vec::with_capacity(40 + n * n * 4 + mask_stride * n);
    let header = BITMAPINFOHEADER {
        biSize: 40,
        biWidth: n as i32,
        biHeight: 2 * n as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: 0,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    // SAFETY: BITMAPINFOHEADER is 40 bytes of plain data.
    bits.extend_from_slice(unsafe { std::slice::from_raw_parts((&header as *const BITMAPINFOHEADER).cast::<u8>(), 40) });
    for row in rgba.chunks_exact(n * 4).rev() {
        for px in row.chunks_exact(4) {
            bits.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    bits.resize(bits.len() + mask_stride * n, 0);
    // SAFETY: `bits` is a well formed icon resource of the given length.
    unsafe {
        CreateIconFromResourceEx(bits.as_ptr(), bits.len() as u32, 1, 0x0003_0000, n as i32, n as i32, LR_DEFAULTCOLOR)
    }
}

/// Renders the percentage as text. GDI draws white on black into a DIB and
/// the luminance becomes the alpha channel, which gives properly
/// antialiased text on any taskbar colour.
fn render_number(spec: &IconSpec, percent: f32, n: usize) -> Option<Vec<u8>> {
    let text = format!("{percent:.0}");
    let color = if spec.charging {
        icon::level_color(100.0)
    } else if percent <= 30.0 {
        icon::level_color(percent)
    } else {
        icon::foreground(spec.light_foreground)
    };

    // Draw on a larger canvas, then crop the inked area to the icon.
    let canvas = n * 3;
    let mut font_px = n as f32 * 1.25;
    let mut result = None;
    for _ in 0..3 {
        let coverage = draw_text(&text, canvas, font_px.round() as i32)?;
        let (mut x0, mut y0, mut x1, mut y1) = (canvas, canvas, 0, 0);
        for y in 0..canvas {
            for x in 0..canvas {
                if coverage[y * canvas + x] > 24 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (w, h) = (x1 - x0, y1 - y0);
        result = Some((coverage, x0, y0, w, h));
        if w <= n && h <= n {
            break;
        }
        // Too large ("100"): shrink to fit and try again.
        font_px *= (n as f32 / w as f32).min(n as f32 / h as f32) * 0.98;
    }
    let (coverage, x0, y0, w, h) = result?;

    let mut rgba = vec![0u8; n * n * 4];
    let (off_x, off_y) = ((n as isize - w as isize) / 2, (n as isize - h as isize) / 2);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as isize + off_x, y as isize + off_y);
            if dx < 0 || dy < 0 || dx >= n as isize || dy >= n as isize {
                continue;
            }
            let a = coverage[(y0 + y) * canvas + x0 + x];
            let alpha = if spec.dimmed { (a as f32 * 0.45) as u8 } else { a };
            let o = (dy as usize * n + dx as usize) * 4;
            rgba[o..o + 4].copy_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    Some(rgba)
}

/// Draws `text` centred on a square canvas; returns the coverage (0..=255).
fn draw_text(text: &str, canvas: usize, font_px: i32) -> Option<Vec<u8>> {
    // SAFETY: every GDI object created here is released before returning;
    // the DIB bits are only read while the bitmap is alive.
    unsafe {
        let dc = CreateCompatibleDC(null_mut());
        if dc.is_null() {
            return None;
        }
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = canvas as i32;
        info.bmiHeader.biHeight = -(canvas as i32); // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        let mut bits: *mut core::ffi::c_void = null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(dc);
            return None;
        }
        let font = CreateFontW(-font_px, 0, 0, 0, 700, 0, 0, 0, 0, 0, 0, ANTIALIASED_QUALITY as u32, 0, wide("Segoe UI").as_ptr());
        let old_bitmap = SelectObject(dc, bitmap);
        let old_font = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x00FF_FFFF);
        let mut rect = RECT { left: 0, top: 0, right: canvas as i32, bottom: canvas as i32 };
        let mut wtext = wide(text);
        DrawTextW(dc, wtext.as_mut_ptr(), -1, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP);
        GdiFlush();

        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), canvas * canvas * 4);
        let coverage = pixels.chunks_exact(4).map(|px| px[1]).collect();

        SelectObject(dc, old_font);
        SelectObject(dc, old_bitmap);
        DeleteObject(font);
        DeleteObject(bitmap);
        DeleteDC(dc);
        Some(coverage)
    }
}

// ------------------------------------------------------------ window proc

fn request_sync(hwnd: HWND) {
    if !SYNC_PENDING.swap(true, Ordering::AcqRel) {
        // SAFETY: posting to our own window; harmless if it is gone.
        unsafe { PostMessageW(hwnd, WM_APP_SYNC, 0, 0) };
    }
}

fn show_menu(hwnd: HWND) {
    if MENU_OPEN.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some((menu, device_ids)) = with_app(|app| app.build_menu()) {
        // The state is NOT borrowed while the menu runs its modal loop, so
        // device updates keep flowing.
        // SAFETY: valid menu and window handles owned by this thread.
        let command = unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            SetForegroundWindow(hwnd);
            let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN;
            let command = TrackPopupMenu(menu, flags, pt.x, pt.y, 0, hwnd, null());
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            command
        };
        if command > 0 && with_app(|app| app.command(command as usize, &device_ids)) == Some(true) {
            // SAFETY: our own window; WM_DESTROY cleans up.
            unsafe { DestroyWindow(hwnd) };
        }
    }
    MENU_OPEN.store(false, Ordering::Release);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_SYNC => {
            SYNC_PENDING.store(false, Ordering::Release);
            if with_app(|app| app.sync()).is_none() {
                request_sync(hwnd);
            }
            0
        }
        WM_APP_TRAY => {
            match (lparam as u32) & 0xFFFF {
                WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_SETTINGCHANGE => {
            THEME_DIRTY.store(true, Ordering::Relaxed);
            request_sync(hwnd);
            0
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            request_sync(hwnd);
            0
        }
        WM_DEVICECHANGE => {
            if wparam == DBT_DEVNODES_CHANGED {
                // Device notifications come in bursts: debounce.
                SetTimer(hwnd, TIMER_RESCAN, 1500, None);
            }
            1
        }
        WM_POWERBROADCAST => {
            if wparam == PBT_APMRESUMEAUTOMATIC {
                SetTimer(hwnd, TIMER_RESUME, 5000, None);
            }
            1
        }
        WM_TIMER => {
            KillTimer(hwnd, wparam);
            with_app(|app| match wparam {
                TIMER_RESCAN => {
                    if let Some(native) = &app.services.native {
                        native.rescan();
                    }
                }
                TIMER_RESUME => app.refresh_devices(),
                _ => {}
            });
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            with_app(|app| app.remove_all_icons());
            PostQuitMessage(0);
            0
        }
        _ => {
            if msg != 0 && msg == TASKBAR_CREATED.with(|m| m.get()) {
                READD_ICONS.store(true, Ordering::Relaxed);
                THEME_DIRTY.store(true, Ordering::Relaxed);
                request_sync(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

thread_local! {
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Creates the hidden top-level window that owns the tray icons. It is a
/// real (never shown) top-level window rather than a message-only one so
/// that it receives broadcasts (device changes, theme, taskbar restart).
pub fn create_window() -> Option<HWND> {
    // SAFETY: standard window class registration / creation.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide(win::WINDOW_CLASS);
        let mut class: WNDCLASSW = zeroed();
        class.lpfnWndProc = Some(wnd_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        if RegisterClassW(&class) == 0 {
            return None;
        }
        TASKBAR_CREATED.with(|m| m.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr())));
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide(win::APP_NAME).as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        (!hwnd.is_null()).then_some(hwnd)
    }
}

/// Installs the application state and runs the message loop until quit.
pub fn run(hwnd: HWND, hub: Arc<Hub>, cfg: Config, cfg_path: PathBuf, services: Services) {
    let app = App {
        hwnd,
        hub: hub.clone(),
        cfg,
        cfg_path,
        services,
        icons: Vec::new(),
        next_uid: PLACEHOLDER_UID,
        light_taskbar: taskbar_is_light(),
        low_notified: HashSet::new(),
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(|app| app.sync());

    // SAFETY: standard message loop.
    unsafe {
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|cell| cell.borrow_mut().take());
}

/// Wakes the UI thread from any thread.
pub fn sync_notifier(hwnd: HWND) -> impl Fn() + Send + Sync + 'static {
    let raw = hwnd as isize;
    move || request_sync(raw as HWND)
}
