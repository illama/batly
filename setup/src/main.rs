//! Batly installer: a single executable that carries `batly.exe`.
//!
//! Per-user installation (no administrator rights):
//! * copies the application to `%LOCALAPPDATA%\Programs\Batly`,
//! * creates a Start menu shortcut,
//! * registers the uninstaller in "Apps & features",
//! * optionally starts the application with Windows.
//!
//! `Batly-Setup.exe /S` installs silently (keeps the autostart choice of
//! a previous installation, enables it on a first one).
//!
//! `Batly-Setup.exe /S /update` is what the application runs to update
//! itself: the installer first waits for the running Batly to exit
//! (single instance mutex), then replaces it and starts the new version.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("The Batly installer only runs on Windows.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_ascii_lowercase()).collect();
    let silent = args.iter().any(|a| matches!(a.as_str(), "/s" | "-s" | "/silent" | "--silent"));
    let update = args.iter().any(|a| matches!(a.as_str(), "/update" | "-update" | "--update"));
    if let Err(message) = installer::run(silent, update) {
        if !silent {
            installer::error_box(&message);
        }
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod installer {
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use windows::core::{w, Interface, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
    use windows::Win32::Globalization::GetUserDefaultUILanguage;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_SZ,
    };
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, MessageBoxW, PostMessageW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
        MB_SETFOREGROUND, MB_YESNO, MESSAGEBOX_STYLE, WM_CLOSE,
    };

    static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/batly.exe"));
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const TITLE: &str = "Batly Setup";

    // Must match `win.rs` in the application.
    const WINDOW_CLASS: PCWSTR = w!("BatlyWindow");
    // Must match the single instance mutex in the application's `main.rs`.
    const INSTANCE_MUTEX: PCWSTR = w!(r"Local\Batly-instance");
    const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const UNINSTALL_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Batly");

    fn french() -> bool {
        // SAFETY: no arguments.
        unsafe { GetUserDefaultUILanguage() & 0x3FF == 0x0C }
    }

    fn tr(en: &str, fr: &str) -> String {
        if french() { fr.into() } else { en.into() }
    }

    fn message(text: &str, style: MESSAGEBOX_STYLE) -> bool {
        // SAFETY: valid strings, no owner window.
        unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(TITLE), style | MB_SETFOREGROUND) == IDYES }
    }

    pub fn error_box(text: &str) {
        message(text, MB_OK | MB_ICONERROR);
    }

    fn set_string(key: PCWSTR, name: &str, value: &str) -> Result<(), String> {
        let data: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `data` is a NUL-terminated UTF-16 buffer of the given size.
        let rc = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key,
                &HSTRING::from(name),
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                (data.len() * 2) as u32,
            )
        };
        rc.ok().map_err(|e| format!("registry: {e}"))
    }

    fn set_dword(key: PCWSTR, name: &str, value: u32) -> Result<(), String> {
        // SAFETY: `value` is 4 bytes.
        let rc = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, key, &HSTRING::from(name), REG_DWORD.0, Some((&value as *const u32).cast()), 4)
        };
        rc.ok().map_err(|e| format!("registry: {e}"))
    }

    fn string_exists(key: PCWSTR, name: &str) -> bool {
        let mut kind = REG_VALUE_TYPE(0);
        // SAFETY: only the existence / type of the value is queried.
        unsafe { RegGetValueW(HKEY_CURRENT_USER, key, &HSTRING::from(name), RRF_RT_REG_SZ, Some(&mut kind), None, None) }
            .is_ok()
    }

    /// Asks a running Batly to quit and waits until it is gone.
    fn close_running_instance() {
        for attempt in 0..50 {
            // SAFETY: valid class name.
            let Ok(hwnd) = (unsafe { FindWindowW(WINDOW_CLASS, PCWSTR::null()) }) else { return };
            if hwnd.is_invalid() {
                return;
            }
            if attempt == 0 {
                // SAFETY: posting to the window found above.
                let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// True while an Batly process holds its single instance mutex.
    fn instance_running() -> bool {
        // SAFETY: valid name; the handle, if any, is closed right away.
        match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, INSTANCE_MUTEX) } {
            Ok(handle) => {
                // SAFETY: handle opened above.
                let _ = unsafe { CloseHandle(handle) };
                true
            }
            Err(_) => false,
        }
    }

    /// Update mode: the application closes itself after starting us; wait
    /// until its process is really gone (up to 30 s, asking again halfway).
    fn wait_for_exit() {
        for attempt in 0..300 {
            if !instance_running() {
                return;
            }
            if attempt == 150 {
                close_running_instance();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn create_shortcut(target: &Path, link: &Path) -> windows::core::Result<()> {
        // SAFETY: standard COM usage on this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(&HSTRING::from(target))?;
            if let Some(dir) = target.parent() {
                shell_link.SetWorkingDirectory(&HSTRING::from(dir))?;
            }
            shell_link.SetDescription(w!("Logitech battery level in the notification area"))?;
            shell_link.SetIconLocation(&HSTRING::from(target), 0)?;
            shell_link.cast::<IPersistFile>()?.Save(&HSTRING::from(link), true)
        }
    }

    pub fn run(silent: bool, update: bool) -> Result<(), String> {
        let silent = silent || update;
        if PAYLOAD.is_empty() {
            return Err("This installer was built without its payload (BATLY_PAYLOAD).".into());
        }
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).filter(|p| p.is_absolute()).ok_or("LOCALAPPDATA is not set")?;
        let dir = local.join("Programs").join("Batly");
        let exe = dir.join("batly.exe");
        let upgrade = exe.exists();

        let mut autostart = !upgrade || string_exists(RUN_KEY, "Batly");
        if !silent {
            let question = tr(
                &format!("Install Batly {VERSION} for the current user?\n\nFolder: {}", dir.display()),
                &format!("Installer Batly {VERSION} pour l'utilisateur actuel ?\n\nDossier : {}", dir.display()),
            );
            if !message(&question, MB_YESNO | MB_ICONQUESTION) {
                return Ok(());
            }
            autostart = message(
                &tr("Start Batly automatically with Windows?", "Lancer Batly automatiquement au démarrage de Windows ?"),
                MB_YESNO | MB_ICONQUESTION,
            );
        }

        if update {
            wait_for_exit();
        }
        close_running_instance();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // The old executable can stay locked for a moment after its
        // process exits (antivirus scan): retry for a few seconds.
        let mut attempt = 0;
        loop {
            match std::fs::write(&exe, PAYLOAD) {
                Ok(()) => break,
                Err(e) if attempt >= 25 => return Err(format!("{}: {e}", exe.display())),
                Err(_) => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }

        let exe_str = exe.display().to_string();
        let quoted = format!("\"{exe_str}\"");
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from).filter(|p| p.is_absolute()) {
            let programs = appdata.join(r"Microsoft\Windows\Start Menu\Programs");
            let _ = std::fs::create_dir_all(&programs);
            // A missing shortcut is not worth failing the installation.
            let _ = create_shortcut(&exe, &programs.join("Batly.lnk"));
        }

        set_string(UNINSTALL_KEY, "DisplayName", "Batly")?;
        set_string(UNINSTALL_KEY, "DisplayVersion", VERSION)?;
        set_string(UNINSTALL_KEY, "Publisher", "Batly contributors")?;
        set_string(UNINSTALL_KEY, "DisplayIcon", &exe_str)?;
        set_string(UNINSTALL_KEY, "InstallLocation", &dir.display().to_string())?;
        set_string(UNINSTALL_KEY, "UninstallString", &format!("{quoted} --uninstall"))?;
        set_string(UNINSTALL_KEY, "URLInfoAbout", "https://github.com/illama/batly")?;
        set_dword(UNINSTALL_KEY, "NoModify", 1)?;
        set_dword(UNINSTALL_KEY, "NoRepair", 1)?;
        set_dword(UNINSTALL_KEY, "EstimatedSize", (PAYLOAD.len() / 1024) as u32)?;

        if autostart {
            set_string(RUN_KEY, "Batly", &quoted)?;
        } else if !silent {
            // SAFETY: valid key / value names.
            let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Batly")) };
        }

        std::process::Command::new(&exe).current_dir(&dir).spawn().map_err(|e| format!("{exe_str}: {e}"))?;
        if !silent {
            message(
                &tr(
                    "Batly is installed and running.\n\nLook for its icon in the notification area (next to the clock); right-click it to choose the devices to display.",
                    "Batly est installé et lancé.\n\nSon icône se trouve dans la zone de notification (près de l'horloge) ; clic droit pour choisir les périphériques à afficher.",
                ),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        Ok(())
    }
}
