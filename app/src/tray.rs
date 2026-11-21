//! Notification area icons, context menu, battery panel, alerts and the UI
//! message loop.
//!
//! Everything UI related runs on the main thread. Backends only post a
//! (coalesced) `WM_APP_SYNC` message when the device registry changes.

use crate::alerts::Engine;
use crate::config::Config;
use crate::ghub::GHub;
use crate::history::{self, History, Stats};
use crate::i18n::{self, tr};
use crate::icon::{self, IconSpec};
use crate::model::{Device, DeviceKind, Hub, Source, Status};
use crate::native::Native;
use crate::notice::{Notice, NoticeKind};
use crate::panel;
use crate::win::{self, copy_wide, wide};
use crate::{hotkey, http, log, settings_ui};
use std::cell::RefCell;
use std::collections::HashMap;
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
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::Shell::{
    SHQueryUserNotificationState, Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE,
    NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER, NOTIFYICON_VERSION_4, QUNS_BUSY, QUNS_PRESENTATION_MODE,
    QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage, HICON,
    HMENU, LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, TPM_BOTTOMALIGN,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
    WM_DEVICECHANGE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_HOTKEY, WM_NULL, WM_POWERBROADCAST, WM_SETTINGCHANGE,
    WM_TIMER, WM_USER, WNDCLASSW, WS_OVERLAPPED,
};

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_SYNC: u32 = WM_APP + 2;
/// The settings file changed (watcher thread or settings window).
pub const WM_APP_RELOAD: u32 = WM_APP + 3;
/// A background task asks the UI thread to show a notice (`lparam` =
/// `Box<Notice>` raw pointer).
pub const WM_APP_NOTICE: u32 = WM_APP + 4;
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const DBT_DEVNODES_CHANGED: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

const TIMER_RESCAN: usize = 1;
const TIMER_RESUME: usize = 2;
/// Re-evaluates the time based alerts (estimate, offline) as time passes.
const TIMER_ALERTS: usize = 3;
/// Retries held notices while the user is busy.
const TIMER_HELD: usize = 4;
const TIMER_RELOAD: usize = 5;

const ID_NUMERIC: usize = 1;
const ID_LOW_BATTERY: usize = 2;
const ID_AUTOSTART: usize = 3;
const ID_REFRESH: usize = 4;
pub const ID_SETTINGS: usize = 5;
const ID_WEB: usize = 6;
const ID_QUIT: usize = 7;
pub const ID_PANEL: usize = 8;
const ID_SETTINGS_FILE: usize = 9;
const ID_SNOOZE: usize = 10;
const ID_SINGLE_ICON: usize = 11;
pub const ID_RECEIVERS: usize = 12;
/// `ID_ACTION_BASE + n`: extra actions registered in `Services::actions`.
const ID_ACTION_BASE: usize = 500;
const ID_DEVICE_BASE: usize = 1000;
const HOTKEY_ID: i32 = 1;

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
    /// `None` for the placeholder / summary icon.
    device_id: Option<String>,
    hicon: HICON,
    shown: Option<Shown>,
    added: bool,
}

/// An extra menu entry provided by an optional module (receivers window,
/// update check, problem report…). Runs on the UI thread.
pub struct Action {
    pub label: String,
    pub run: Box<dyn Fn(HWND)>,
}

pub struct Services {
    pub native: Option<Native>,
    pub ghub: Option<GHub>,
    pub web_url: Option<String>,
    pub history: Arc<History>,
    /// Other backends: called on "Refresh now" / after resume.
    pub refresh: Vec<Box<dyn Fn()>>,
    /// Other backends: called after a device arrival / removal.
    pub rescan: Vec<Box<dyn Fn()>>,
    /// Integrations that receive every alert (webhooks, MQTT…).
    pub notice_sinks: Vec<Box<dyn Fn(&Notice)>>,
    pub actions: Vec<Action>,
    /// The settings in effect when the services were started.
    pub started_with: Config,
    /// Copy of the settings read by other threads (HTTP aliases).
    pub shared_cfg: Arc<std::sync::Mutex<Config>>,
    /// Integrations that stop when dropped (peers, MQTT…).
    pub keep_alive: Vec<Box<dyn std::any::Any>>,
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
    alerts: Engine,
    /// Last alert shown, for "remind me later".
    last_notice: Option<Notice>,
    hotkey_registered: bool,
    /// Statistics cache: device id → (history sample count, minute, stats).
    stats_cache: HashMap<String, (usize, u64, Stats)>,
}

fn taskbar_is_light() -> bool {
    // Missing value (Windows 8 and earlier): the taskbar is dark.
    win::reg_read_dword(PERSONALIZE_KEY, "SystemUsesLightTheme").is_some_and(|v| v != 0)
}

/// Full screen game, presentation, "Do not disturb"…
fn user_busy() -> bool {
    let mut state = 0;
    // SAFETY: valid out-pointer.
    let ok = unsafe { SHQueryUserNotificationState(&mut state) } >= 0;
    ok && matches!(state, QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_QUIET_TIME)
}

fn plugged(d: &Device) -> bool {
    d.battery.is_some_and(|b| matches!(b.status, Status::Charging | Status::Full))
}

impl App {
    fn save(&self) {
        *self.services.shared_cfg.lock().unwrap() = self.cfg.clone();
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

    fn name(&self, d: &Device) -> String {
        self.cfg.display_name(&d.id, &d.name)
    }

    fn kind(&self, d: &Device) -> DeviceKind {
        DeviceKind::parse(&self.cfg.device(&d.id).icon).unwrap_or(d.kind)
    }

    /// Time left (discharging) or to full (charging), from the history,
    /// cached per device for a minute or until a new reading arrives.
    fn estimate(&mut self, d: &Device) -> Option<f64> {
        let stats = self.stats(&d.id);
        match d.battery?.status {
            Status::Charging => stats.time_to_full_hours,
            Status::Full | Status::NotCharging => None,
            _ => stats.time_left_hours,
        }
    }

    fn stats(&mut self, id: &str) -> Stats {
        let minute = history::now() / 60;
        let count = self.services.history.samples(id, history::now().saturating_sub(60)).len();
        if let Some((c, m, s)) = self.stats_cache.get(id) {
            if *c == count && *m == minute {
                return s.clone();
            }
        }
        let s = self.services.history.stats(id);
        self.stats_cache.insert(id.to_string(), (count, minute, s.clone()));
        s
    }

    /// Devices in display order: the user's order first, then by name.
    fn ordered(&self, mut devices: Vec<Device>) -> Vec<Device> {
        let order = &self.cfg.ui.device_order;
        let rank = |d: &Device| order.iter().position(|o| *o == d.id).unwrap_or(usize::MAX);
        devices.sort_by(|a, b| {
            rank(a).cmp(&rank(b)).then_with(|| self.name(a).to_lowercase().cmp(&self.name(b).to_lowercase()))
        });
        devices
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
        let devices = self.ordered(self.hub.snapshot());

        // First run: give the first battery powered device an icon.
        if !self.cfg.ui.first_run_done {
            if let Some(d) = devices.iter().find(|d| d.has_battery && d.source != Source::Remote) {
                self.cfg.ui.selected_devices.push(d.id.clone());
                self.cfg.ui.first_run_done = true;
                self.save();
            }
        }

        // Which device gets an icon. `None` = summary icon.
        let with_battery: Vec<&Device> = devices.iter().filter(|d| d.has_battery).collect();
        let lowest = with_battery
            .iter()
            .filter(|d| d.battery.is_some())
            .min_by(|a, b| {
                // Online devices first, then the lowest level.
                (!a.online, a.battery.map_or(101.0, |x| x.percent))
                    .partial_cmp(&(!b.online, b.battery.map_or(101.0, |x| x.percent)))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
        let hide_above = self.cfg.ui.hide_above as f32;
        let wanted: Vec<(Option<String>, Option<&Device>)> = if self.cfg.ui.single_icon {
            vec![(None, lowest)]
        } else {
            let visible: Vec<&Device> = devices
                .iter()
                .filter(|d| self.cfg.ui.selected_devices.iter().any(|s| *s == d.id))
                .filter(|d| {
                    hide_above <= 0.0 || plugged(d) || !d.online || d.battery.is_none_or(|b| b.percent <= hide_above)
                })
                .collect();
            if visible.is_empty() {
                // Keep one icon to click on: the lowest device, as a summary.
                vec![(None, lowest)]
            } else {
                visible.into_iter().map(|d| (Some(d.id.clone()), Some(d))).collect()
            }
        };

        // Remove icons that are no longer wanted.
        let mut i = 0;
        while i < self.icons.len() {
            if wanted.iter().any(|(id, _)| *id == self.icons[i].device_id) {
                i += 1;
            } else {
                let icon = self.icons.remove(i);
                self.remove_icon(icon);
            }
        }

        let size = self.icon_size();
        let numeric = self.cfg.ui.numeric_display;
        let summary_tip = self.summary_tip(&devices);
        for (id, device) in wanted {
            let index = match self.icons.iter().position(|i| i.device_id == id) {
                Some(index) => index,
                None => {
                    let uid = if id.is_none() {
                        PLACEHOLDER_UID
                    } else {
                        self.next_uid += 1;
                        self.next_uid
                    };
                    self.icons.push(TrayIcon { uid, device_id: id.clone(), hicon: null_mut(), shown: None, added: false });
                    self.icons.len() - 1
                }
            };
            let tip = match (&id, device) {
                (Some(_), Some(d)) => {
                    let estimate = self.estimate(d);
                    i18n::tooltip(&self.name(d), d, estimate)
                }
                _ => summary_tip.clone(),
            };
            let shown = Shown {
                spec: match device {
                    Some(d) => IconSpec {
                        kind: self.kind(d),
                        percent: d.battery.map(|b| b.percent.round()),
                        charging: d.battery.is_some_and(|b| b.status == Status::Charging),
                        light_foreground: !self.light_taskbar,
                        dimmed: !d.online,
                    },
                    None => IconSpec {
                        kind: DeviceKind::Mouse,
                        percent: None,
                        charging: false,
                        light_foreground: !self.light_taskbar,
                        dimmed: false,
                    },
                },
                numeric: numeric && device.is_some_and(|d| d.battery.is_some()),
                size,
                tip,
            };
            self.update_icon(index, shown);
        }

        self.check_alerts(&devices);
        if panel::is_open() {
            panel::update(self.panel_content(&devices));
        }
    }

    fn summary_tip(&mut self, devices: &[Device]) -> String {
        if devices.is_empty() {
            return i18n::searching().into();
        }
        let mut tip = String::from("LGSTray");
        for d in devices.iter().filter(|d| d.has_battery) {
            let line = format!("\n{}: {}", self.name(d), i18n::describe(d));
            if tip.chars().count() + line.chars().count() > 120 {
                break;
            }
            tip.push_str(&line);
        }
        tip
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

    // ---------------------------------------------------------------- alerts

    fn check_alerts(&mut self, devices: &[Device]) {
        let history = self.services.history.clone();
        let notices = self.alerts.evaluate(&self.cfg, devices, |id| history.stats(id), history::now());
        for n in notices {
            self.deliver(n);
        }
    }

    fn deliver(&mut self, n: Notice) {
        for sink in &self.services.notice_sinks {
            sink(&n);
        }
        if self.cfg.alerts.quiet_when_busy && user_busy() {
            self.alerts.hold(n);
            // SAFETY: timer on our own window.
            unsafe { SetTimer(self.hwnd, TIMER_HELD, 60_000, None) };
            return;
        }
        self.show_notice(&n);
    }

    fn show_notice(&mut self, n: &Notice) {
        let icon = self
            .icons
            .iter()
            .find(|i| i.device_id.as_deref() == Some(n.device_id.as_str()))
            .or_else(|| self.icons.first());
        let Some(icon) = icon else { return };
        let mut data = self.notify_data(icon.uid);
        data.uFlags = NIF_INFO;
        let warning = matches!(n.kind, NoticeKind::LowBattery | NoticeKind::RunningOut | NoticeKind::Offline);
        data.dwInfoFlags = (if warning { NIIF_WARNING } else { NIIF_INFO }) | NIIF_RESPECT_QUIET_TIME;
        copy_wide(&mut data.szInfoTitle, &n.title);
        copy_wide(&mut data.szInfo, &n.text);
        // SAFETY: `data` is fully initialised.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
        if n.kind != NoticeKind::Update {
            self.last_notice = Some(n.clone());
        }
    }

    fn flush_held(&mut self) {
        if user_busy() {
            return;
        }
        // SAFETY: our own timer.
        unsafe { KillTimer(self.hwnd, TIMER_HELD) };
        for n in self.alerts.take_held() {
            self.show_notice(&n);
        }
    }

    fn snooze(&mut self) {
        if let Some(n) = self.last_notice.take() {
            self.alerts.snooze(&n, history::now(), self.cfg.alerts.snooze_hours);
            if panel::is_open() {
                let devices = self.ordered(self.hub.snapshot());
                panel::update(self.panel_content(&devices));
            }
        }
    }

    // ----------------------------------------------------------------- panel

    fn panel_content(&mut self, devices: &[Device]) -> panel::Content {
        let mut rows = Vec::new();
        for d in devices {
            let mut detail = vec![i18n::state(d)];
            if d.online {
                if let Some(h) = self.estimate(d) {
                    detail.push(match d.battery.map(|b| b.status) {
                        Some(Status::Charging) => i18n::time_to_full(h),
                        _ => i18n::time_left(h),
                    });
                }
            } else if let Some(t) = d.last_update {
                let ago = t.elapsed().map_or(0.0, |e| e.as_secs_f64() / 3600.0);
                detail.push(ago_text(ago));
            }
            detail.retain(|s| !s.is_empty());
            let detail = detail.join(" · ");
            let mut note = Vec::new();
            if let Some(host) = &d.info.host {
                note.push(host.clone());
            }
            if let Some((current, count)) = d.info.host_channel {
                note.push(format!("{current}/{count}"));
            }
            if d.source == Source::GHub {
                note.push("G HUB".into());
            }
            rows.push(panel::Row {
                name: self.name(d),
                kind: self.kind(d),
                percent: d.battery.map(|b| b.percent),
                charging: d.battery.is_some_and(|b| b.status == Status::Charging),
                online: d.online,
                has_battery: d.has_battery,
                detail,
                note: note.join(" · "),
            });
        }
        let mut buttons = vec![
            (panel::CMD_REFRESH, i18n::refresh().to_string()),
            (panel::CMD_SETTINGS, tr(["Settings", "Réglages", "Einstellungen", "Ajustes", "Impostazioni", "Configurações"]).to_string()),
        ];
        if self.services.web_url.is_some() {
            buttons.push((panel::CMD_WEB, tr(["Web page", "Page web", "Webseite", "Página web", "Pagina web", "Página web"]).to_string()));
        }
        panel::Content {
            title: tr(["Batteries", "Batteries", "Akkus", "Baterías", "Batterie", "Baterias"]).into(),
            empty: i18n::no_device().into(),
            rows,
            alert: self.last_notice.as_ref().map(|n| {
                let later = tr(["Later", "Plus tard", "Später", "Más tarde", "Più tardi", "Mais tarde"]);
                (format!("{}\n{}", n.title, n.text), later.to_string())
            }),
            buttons,
        }
    }

    fn open_panel(&mut self, anchor: Option<POINT>) {
        let devices = self.ordered(self.hub.snapshot());
        let content = self.panel_content(&devices);
        let anchor = anchor.or_else(|| self.icons.first().and_then(|i| icon_center(self.hwnd, i.uid)));
        panel::toggle(self.hwnd, content, anchor);
    }

    fn open_settings(&self) {
        let devices = self
            .hub
            .snapshot()
            .into_iter()
            .filter(|d| d.has_battery || self.cfg.devices.contains_key(&d.id))
            .map(|d| settings_ui::KnownDevice { id: d.id, name: d.name, kind: d.kind })
            .collect();
        settings_ui::open(self.hwnd, &self.cfg, self.cfg_path.clone(), devices);
    }

    // ------------------------------------------------------------ settings

    fn register_hotkey(&mut self) {
        // SAFETY: hot key of our own window.
        unsafe {
            if self.hotkey_registered {
                UnregisterHotKey(self.hwnd, HOTKEY_ID);
                self.hotkey_registered = false;
            }
            if let Some(k) = hotkey::parse(&self.cfg.ui.hotkey) {
                self.hotkey_registered = RegisterHotKey(self.hwnd, HOTKEY_ID, k.modifiers, k.vk) != 0;
                if !self.hotkey_registered {
                    log!("hotkey {} is already taken", self.cfg.ui.hotkey);
                }
            }
        }
    }

    /// Re-reads the settings file after a change (by hand or from the
    /// settings window). Returns true when the application must restart to
    /// apply it (servers / backends).
    fn reload(&mut self) -> bool {
        let text = match std::fs::read_to_string(&self.cfg_path) {
            Ok(t) => t,
            Err(_) => return false,
        };
        let cfg = match Config::parse(&text) {
            Ok(c) => c,
            Err(e) => {
                let title = i18n::settings_invalid_title();
                let n = Notice {
                    kind: NoticeKind::Update,
                    device_id: String::new(),
                    device_name: String::new(),
                    percent: None,
                    hours_left: None,
                    title: title.into(),
                    text: e.lines().next().unwrap_or("").chars().take(200).collect(),
                };
                self.show_notice(&n);
                return false;
            }
        };
        if cfg == self.cfg {
            return false;
        }
        log!("settings reloaded");
        if cfg.services_differ(&self.services.started_with) {
            self.cfg = cfg;
            return true;
        }
        self.cfg = cfg;
        *self.services.shared_cfg.lock().unwrap() = self.cfg.clone();
        i18n::init(&self.cfg.ui.language, crate::system_language());
        self.register_hotkey();
        self.stats_cache.clear();
        self.sync();
        false
    }

    // ------------------------------------------------------------------ menu

    /// Builds the context menu; returns it with the device ids behind the
    /// `ID_DEVICE_BASE + n` commands.
    fn build_menu(&self) -> (HMENU, Vec<String>) {
        let devices = self.ordered(self.hub.snapshot());
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
                let source = match (&d.info.host, d.source) {
                    (Some(host), _) => format!(" ({host})"),
                    (None, Source::GHub) => " (G HUB)".into(),
                    _ => String::new(),
                };
                let text = format!("{}{}\t{}", self.name(d).replace('&', "&&"), source, i18n::describe(d));
                let selected = self.cfg.ui.selected_devices.iter().any(|s| *s == d.id);
                let usable = (d.has_battery || selected) && !self.cfg.ui.single_icon;
                add(ID_DEVICE_BASE + n, &text, check(selected && !self.cfg.ui.single_icon) | if usable { 0 } else { MF_GRAYED });
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            let panel_label = tr(["Battery panel", "Panneau des batteries", "Akku-Übersicht", "Panel de baterías", "Pannello batterie", "Painel de baterias"]);
            let shortcut = if self.hotkey_registered { format!("\t{}", self.cfg.ui.hotkey) } else { String::new() };
            add(ID_PANEL, &format!("{panel_label}{shortcut}"), 0);
            if let Some(n) = &self.last_notice {
                add(ID_SNOOZE, &format!("{} – {}", later_label(), n.device_name.replace('&', "&&")), 0);
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            add(ID_NUMERIC, i18n::numeric_display(), check(self.cfg.ui.numeric_display));
            add(
                ID_SINGLE_ICON,
                tr(["One icon for all devices", "Une seule icône pour tout", "Ein Symbol für alle", "Un solo icono para todo", "Una sola icona per tutto", "Um só ícone para tudo"]),
                check(self.cfg.ui.single_icon),
            );
            add(ID_LOW_BATTERY, i18n::low_battery_alerts(), check(self.cfg.ui.low_battery_threshold > 0));
            add(ID_AUTOSTART, i18n::autostart(), check(win::autostart_enabled()));
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            add(ID_REFRESH, i18n::refresh(), 0);
            add(ID_SETTINGS, &format!("{}…", tr(["Settings", "Réglages", "Einstellungen", "Ajustes", "Impostazioni", "Configurações"])), 0);
            if self.services.web_url.is_some() {
                add(ID_WEB, i18n::open_web(), 0);
            }
            if self.services.native.is_some() {
                add(ID_RECEIVERS, tr(["Receivers and devices…", "Récepteurs et appareils…", "Empfänger und Geräte…", "Receptores y dispositivos…", "Ricevitori e dispositivi…", "Receptores e dispositivos…"]), 0);
            }
            for (n, action) in self.services.actions.iter().enumerate() {
                add(ID_ACTION_BASE + n, &action.label, 0);
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
            ID_SINGLE_ICON => {
                self.cfg.ui.single_icon = !self.cfg.ui.single_icon;
                self.save();
            }
            ID_LOW_BATTERY => {
                self.cfg.ui.low_battery_threshold = if self.cfg.ui.low_battery_threshold > 0 { 0 } else { 15 };
                self.save();
            }
            ID_AUTOSTART => win::set_autostart(!win::autostart_enabled()),
            ID_REFRESH | panel::CMD_REFRESH => self.refresh_devices(),
            ID_SETTINGS | panel::CMD_SETTINGS => self.open_settings(),
            ID_SETTINGS_FILE => {
                win::shell_open(self.hwnd, "notepad.exe", Some(&format!("\"{}\"", self.cfg_path.display())))
            }
            ID_WEB | panel::CMD_WEB => {
                if let Some(url) = &self.services.web_url {
                    win::shell_open(self.hwnd, url, None);
                }
            }
            ID_PANEL => self.open_panel(None),
            ID_RECEIVERS | panel::CMD_RECEIVERS => {
                if let Some(native) = &self.services.native {
                    crate::receivers::open(native);
                }
            }
            ID_SNOOZE | panel::CMD_SNOOZE => self.snooze(),
            ID_QUIT => return true,
            n if (panel::CMD_DEVICE..panel::CMD_DEVICE + 1000).contains(&n) => self.open_settings(),
            n if (ID_ACTION_BASE..ID_DEVICE_BASE).contains(&n) => {
                if let Some(action) = self.services.actions.get(n - ID_ACTION_BASE) {
                    (action.run)(self.hwnd);
                }
            }
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
        for f in &self.services.refresh {
            f();
        }
    }

    fn rescan_devices(&self) {
        if let Some(native) = &self.services.native {
            native.rescan();
        }
        for f in &self.services.rescan {
            f();
        }
    }
}

fn later_label() -> String {
    tr(["Remind me later", "Me le rappeler plus tard", "Später erinnern", "Recordármelo más tarde", "Ricordamelo più tardi", "Lembrar mais tarde"]).into()
}

fn ago_text(hours: f64) -> String {
    let d = i18n::duration(hours);
    tr([format!("seen {d} ago"), format!("vu il y a {d}"), format!("vor {d} gesehen"), format!("visto hace {d}"), format!("visto {d} fa"), format!("visto há {d}")].each_ref()).clone()
}

/// Centre of a tray icon on screen (Windows 7+).
fn icon_center(hwnd: HWND, uid: u32) -> Option<POINT> {
    // SAFETY: plain data, valid pointers.
    unsafe {
        let mut id: NOTIFYICONIDENTIFIER = zeroed();
        id.cbSize = size_of::<NOTIFYICONIDENTIFIER>() as u32;
        id.hWnd = hwnd;
        id.uID = uid;
        let mut r: RECT = zeroed();
        (Shell_NotifyIconGetRect(&id, &mut r) >= 0).then(|| POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 })
    }
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

    // Number on top, device glyph underneath so several icons can be told
    // apart. At least 5 px for the glyph or it is unreadable.
    let glyph_h = ((n as f32 * 0.38).round() as usize).max(5).min(n / 2);
    let text_h = n - glyph_h;

    // Draw on a larger canvas, then crop the inked area to the icon.
    let canvas = n * 3;
    let mut font_px = text_h as f32 * 1.25;
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
        if w <= n && h <= text_h {
            break;
        }
        // Too large ("100"): shrink to fit and try again.
        font_px *= (n as f32 / w as f32).min(text_h as f32 / h as f32) * 0.98;
    }
    let (coverage, x0, y0, w, h) = result?;

    let opacity = if spec.dimmed { 0.45 } else { 1.0 };
    let mut rgba = vec![0u8; n * n * 4];
    let (off_x, off_y) = ((n as isize - w as isize) / 2, (text_h as isize - h as isize) / 2);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as isize + off_x, y as isize + off_y);
            if dx < 0 || dy < 0 || dx >= n as isize || dy >= text_h as isize {
                continue;
            }
            let a = coverage[(y0 + y) * canvas + x0 + x];
            let alpha = (a as f32 * opacity) as u8;
            let o = (dy as usize * n + dx as usize) * 4;
            rgba[o..o + 4].copy_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }

    // Device glyph in the bottom band, in the taskbar foreground colour.
    let fg = icon::foreground(spec.light_foreground);
    let glyph = icon::glyph_coverage(spec.kind, n, glyph_h);
    for (i, &c) in glyph.iter().enumerate() {
        let alpha = (c * 255.0 * opacity).round() as u8;
        if alpha > 0 {
            let o = ((text_h + i / n) * n + i % n) * 4;
            rgba[o..o + 4].copy_from_slice(&[fg[0], fg[1], fg[2], alpha]);
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
    panel::close();
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

/// Starts a new instance (which waits for this one to exit) and quits:
/// used when changed settings need the servers / backends restarted.
fn restart(hwnd: HWND) {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--restart");
        if log::enabled() {
            cmd.arg("--debug");
        }
        if cmd.spawn().is_ok() {
            log!("restarting to apply the new settings");
            // SAFETY: our own window; WM_DESTROY cleans up.
            unsafe { DestroyWindow(hwnd) };
        }
    }
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
            // NOTIFYICON_VERSION_4: wparam holds the anchor point.
            let anchor = POINT { x: (wparam & 0xFFFF) as i16 as i32, y: (wparam >> 16 & 0xFFFF) as i16 as i32 };
            match (lparam as u32) & 0xFFFF {
                WM_CONTEXTMENU => show_menu(hwnd),
                NIN_SELECT | NIN_KEYSELECT => {
                    with_app(|app| app.open_panel(Some(anchor)));
                }
                NIN_BALLOONUSERCLICK => {
                    with_app(|app| app.open_panel(None));
                }
                _ => {}
            }
            0
        }
        WM_HOTKEY => {
            with_app(|app| app.open_panel(None));
            0
        }
        WM_COMMAND => {
            if with_app(|app| app.command(wparam & 0xFFFF, &[])) == Some(true) {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_APP_RELOAD => {
            // Editors write in several steps: debounce.
            SetTimer(hwnd, TIMER_RELOAD, 400, None);
            0
        }
        WM_APP_NOTICE => {
            if lparam != 0 {
                let notice = Box::from_raw(lparam as *mut Notice);
                with_app(|app| app.deliver(*notice));
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
            if wparam != TIMER_ALERTS && wparam != TIMER_HELD {
                KillTimer(hwnd, wparam);
            }
            let restart_needed = with_app(|app| match wparam {
                TIMER_RESCAN => {
                    app.rescan_devices();
                    false
                }
                TIMER_RESUME => {
                    app.refresh_devices();
                    false
                }
                TIMER_ALERTS => {
                    app.sync();
                    false
                }
                TIMER_HELD => {
                    app.flush_held();
                    false
                }
                TIMER_RELOAD => app.reload(),
                _ => false,
            });
            if restart_needed == Some(true) {
                restart(hwnd);
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            panel::close();
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

/// Watches the settings folder and posts `WM_APP_RELOAD` on every change.
/// Event driven: the thread sleeps in the kernel until something happens.
fn watch_settings(hwnd: HWND, path: &std::path::Path) {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_FILE_NAME,
        FILE_NOTIFY_CHANGE_LAST_WRITE,
    };
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    let Some(dir) = path.parent().map(|d| d.to_path_buf()) else { return };
    let _ = std::fs::create_dir_all(&dir);
    let raw = hwnd as isize;
    let _ = std::thread::Builder::new().name("settings-watch".into()).stack_size(64 * 1024).spawn(move || {
        // SAFETY: valid NUL-terminated path; the handle lives for the process.
        unsafe {
            let h = FindFirstChangeNotificationW(
                wide(&dir.display().to_string()).as_ptr(),
                0,
                FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
            );
            if h == INVALID_HANDLE_VALUE || h.is_null() {
                log!("cannot watch {}", dir.display());
                return;
            }
            while WaitForSingleObject(h, INFINITE) == 0 {
                PostMessageW(raw as HWND, WM_APP_RELOAD, 0, 0);
                if FindNextChangeNotification(h) == 0 {
                    break;
                }
            }
        }
    });
}

/// Installs the application state and runs the message loop until quit.
pub fn run(hwnd: HWND, hub: Arc<Hub>, cfg: Config, cfg_path: PathBuf, services: Services) {
    watch_settings(hwnd, &cfg_path);
    let app = App {
        hwnd,
        hub: hub.clone(),
        cfg,
        cfg_path,
        services,
        icons: Vec::new(),
        next_uid: PLACEHOLDER_UID,
        light_taskbar: taskbar_is_light(),
        alerts: Engine::new(),
        last_notice: None,
        hotkey_registered: false,
        stats_cache: HashMap::new(),
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(|app| {
        app.register_hotkey();
        app.sync();
    });
    // SAFETY: timer on our own window.
    unsafe { SetTimer(hwnd, TIMER_ALERTS, 10 * 60 * 1000, None) };

    // SAFETY: standard message loop.
    unsafe {
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if settings_ui::pre_translate(&msg) {
                continue;
            }
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

/// Shows a notice from any thread (update available…).
pub fn notice_poster(hwnd: HWND) -> impl Fn(Notice) + Send + Sync + 'static {
    let raw = hwnd as isize;
    move |n| {
        let ptr = Box::into_raw(Box::new(n));
        // SAFETY: the UI thread takes ownership back in WM_APP_NOTICE; if the
        // post fails the box is reclaimed here.
        unsafe {
            if PostMessageW(raw as HWND, WM_APP_NOTICE, 0, ptr as LPARAM) == 0 {
                drop(Box::from_raw(ptr));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DeviceKind;

    /// `PREVIEW_DIR=/some/dir cargo test numeric_previews` writes a sheet of
    /// numeric icons (number over device glyph) at several sizes.
    #[test]
    fn numeric_previews() {
        let cases = [
            (DeviceKind::Mouse, 100.0, false),
            (DeviceKind::Keyboard, 55.0, false),
            (DeviceKind::Headset, 25.0, false),
            (DeviceKind::Gamepad, 8.0, false),
            (DeviceKind::Earbuds, 70.0, true),
        ];
        for n in [16usize, 20, 24, 32, 48] {
            for &(kind, p, charging) in &cases {
                let spec = IconSpec { kind, percent: Some(p), charging, light_foreground: true, dimmed: false };
                let img = render_number(&spec, p, n).expect("gdi text");
                assert_eq!(img.len(), n * n * 4);
                // Something is drawn in the glyph band.
                let band = &img[(n - 4) * n * 4..];
                assert!(band.chunks(4).any(|px| px[3] > 0), "{kind:?} at {n}px has a glyph");
            }
        }
        let Ok(dir) = std::env::var("PREVIEW_DIR") else { return };
        let (n, scale) = (24usize, 4usize);
        let cell = n * scale;
        let width = cell * cases.len();
        let mut sheet = vec![0u8; width * cell * 4];
        for (k, &(kind, p, charging)) in cases.iter().enumerate() {
            let spec = IconSpec { kind, percent: Some(p), charging, light_foreground: true, dimmed: false };
            let img = render_number(&spec, p, n).unwrap();
            for y in 0..cell {
                for x in 0..cell {
                    let s = &img[((y / scale) * n + x / scale) * 4..][..4];
                    let a = s[3] as f32 / 255.0;
                    let d = &mut sheet[(y * width + k * cell + x) * 4..][..4];
                    for c in 0..3 {
                        d[c] = (s[c] as f32 * a + 32.0 * (1.0 - a)) as u8;
                    }
                    d[3] = 255;
                }
            }
        }
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, width as u32, cell as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&sheet).unwrap();
        std::fs::write(format!("{dir}/numeric.png"), out).unwrap();
    }
}
