//! `lgstray.exe --uninstall`: removes everything the installer created.
//! Registered as the uninstall command in "Apps & features".

use crate::i18n;
use crate::win::{self, wide};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::null;
use std::time::Duration;
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Asks a running instance to quit and waits for it to do so.
pub fn close_running_instance() {
    let class = wide(win::WINDOW_CLASS);
    for attempt in 0..30 {
        // SAFETY: valid NUL-terminated class name.
        let hwnd = unsafe { FindWindowW(class.as_ptr(), null()) };
        if hwnd.is_null() {
            return;
        }
        if attempt == 0 {
            // SAFETY: posting a close request to the window found above.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn run() {
    let title = i18n::uninstall_title();
    if !win::confirm(title, i18n::uninstall_confirm()) {
        return;
    }
    close_running_instance();

    let settings_dir = win::appdata().map(|p| p.join("LGSTray"));
    if settings_dir.as_ref().is_some_and(|d| d.exists()) && win::confirm(title, i18n::uninstall_settings()) {
        if let Some(dir) = &settings_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    win::reg_delete_value(win::RUN_KEY, win::APP_NAME);
    win::reg_delete_tree(win::UNINSTALL_KEY);
    if let Some(shortcut) = win::start_menu_shortcut() {
        let _ = std::fs::remove_file(shortcut);
    }
    win::message_box(title, i18n::uninstall_done());

    // A running executable cannot delete itself: let cmd.exe do it once we
    // are gone. Only our own file is deleted; the folder is removed only if
    // that leaves it empty (rmdir without /s), so running this from a
    // portable copy can never take other files with it.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let script = format!(
                "/d /c ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
                exe.display(),
                dir.display()
            );
            let _ = Command::new("cmd.exe")
                .raw_arg(script)
                .current_dir(std::env::temp_dir())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }
}
