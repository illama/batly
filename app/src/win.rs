//! Small Win32 helpers shared by the Windows-only modules.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_YESNO, SW_SHOWNORMAL,
};

pub const APP_NAME: &str = "Batly";
pub const WINDOW_CLASS: &str = "BatlyWindow";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Batly";

/// NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// Copies `s` into a fixed UTF-16 buffer, truncating on a character
/// boundary and always NUL-terminating.
pub fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for c in s.chars() {
        let mut tmp = [0u16; 2];
        let enc = c.encode_utf16(&mut tmp);
        if n + enc.len() >= dst.len() {
            break;
        }
        dst[n..n + enc.len()].copy_from_slice(enc);
        n += enc.len();
    }
    dst[n] = 0;
}

/// Owned kernel handle of an event object.
pub struct Event(pub HANDLE);

// SAFETY: event handles may be used from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub fn new(manual_reset: bool) -> std::io::Result<Self> {
        // SAFETY: plain FFI call, all pointers optional.
        let h = unsafe { CreateEventW(null(), manual_reset as i32, 0, null()) };
        if h.is_null() {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self(h))
        }
    }

    pub fn set(&self) {
        // SAFETY: the handle is valid for the lifetime of `self`.
        unsafe { SetEvent(self.0) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}

// ------------------------------------------------------------- registry

pub fn reg_read_dword(subkey: &str, value: &str) -> Option<u32> {
    let (mut data, mut size) = (0u32, 4u32);
    // SAFETY: buffers are valid for the sizes given.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(subkey).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (rc == 0).then_some(data)
}

pub fn reg_read_string(subkey: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buffers are valid for the sizes given.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(subkey).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if rc != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..len]))
}

pub fn reg_write_string(subkey: &str, value: &str, data: &str) -> bool {
    let data = wide(data);
    // SAFETY: `data` is a valid NUL-terminated UTF-16 buffer.
    let rc = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            wide(subkey).as_ptr(),
            wide(value).as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    rc == 0
}

pub fn reg_delete_value(subkey: &str, value: &str) {
    // SAFETY: plain FFI call with valid strings.
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(subkey).as_ptr(), wide(value).as_ptr()) };
}

pub fn reg_delete_tree(subkey: &str) {
    // SAFETY: plain FFI call with valid strings.
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(subkey).as_ptr()) };
}

// ------------------------------------------------------------ autostart

pub fn autostart_enabled() -> bool {
    reg_read_string(RUN_KEY, APP_NAME).is_some()
}

pub fn set_autostart(enabled: bool) {
    if enabled {
        if let Ok(exe) = std::env::current_exe() {
            reg_write_string(RUN_KEY, APP_NAME, &format!("\"{}\"", exe.display()));
        }
    } else {
        reg_delete_value(RUN_KEY, APP_NAME);
    }
}

// ----------------------------------------------------------------- misc

pub fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from).filter(|p| p.is_absolute())
}

pub fn start_menu_shortcut() -> Option<PathBuf> {
    appdata().map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs").join("Batly.lnk"))
}

pub fn message_box(title: &str, text: &str) {
    // SAFETY: valid NUL-terminated strings, no owner window.
    unsafe { MessageBoxW(null_mut(), wide(text).as_ptr(), wide(title).as_ptr(), MB_OK | MB_SETFOREGROUND | MB_ICONINFORMATION) };
}

pub fn confirm(title: &str, text: &str) -> bool {
    // SAFETY: valid NUL-terminated strings, no owner window.
    let r = unsafe { MessageBoxW(null_mut(), wide(text).as_ptr(), wide(title).as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND) };
    r == IDYES
}

/// Opens a document / URL, or runs `file` with `args`.
pub fn shell_open(hwnd: HWND, file: &str, args: Option<&str>) {
    let args = args.map(wide);
    // SAFETY: valid NUL-terminated strings.
    unsafe {
        ShellExecuteW(hwnd, wide("open").as_ptr(), wide(file).as_ptr(), args.as_ref().map_or(null(), |a| a.as_ptr()), null(), SW_SHOWNORMAL)
    };
}
