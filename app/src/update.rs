//! Update check and installation.
//!
//! * [`check`] asks the GitHub API for the latest release (at most once a
//!   day unless forced; the time of the last check is stored in
//!   `update-check.txt` next to the settings). Pre-releases and drafts are
//!   ignored.
//! * [`install`] downloads the installer to `%TEMP%`, verifies it against
//!   the release's `SHA256SUMS.txt`, starts it with `/S /update` and closes
//!   the application; the installer waits for it to exit, replaces the
//!   executable and starts the new version. A portable copy is never
//!   replaced: the release page is opened instead.

use crate::{fetch, i18n::tr, sha256};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const REPO: &str = "illama/batly";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST_URL: &str = "https://api.github.com/repos/illama/batly/releases/latest";
const STAMP_FILE: &str = "update-check.txt";
const SUMS_FILE: &str = "SHA256SUMS.txt";
/// Minimum delay between two automatic checks.
pub const INTERVAL: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// e.g. "4.1.0" (without the leading "v").
    pub version: String,
    /// Release notes (Markdown).
    pub notes: String,
    /// `Batly-Setup-<version>.exe` download URL.
    pub setup_url: String,
    /// `SHA256SUMS.txt` download URL ("" if the release has none).
    pub sums_url: String,
    /// Release page on GitHub.
    pub page_url: String,
}

// ------------------------------------------------------------- versions

/// A release version `major.minor.patch`; pre-releases are not versions
/// we ever offer, so they do not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Accepts "4.1.0", "v4.1", "4.1.0+build"; rejects "4.1.0-beta.1".
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split('+').next()?;
        if text.contains('-') {
            return None;
        }
        let mut parts = text.split('.');
        let mut next = |required: bool| -> Option<u64> {
            match parts.next() {
                Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok(),
                None if !required => Some(0),
                _ => None,
            }
        };
        let v = Self(next(true)?, next(false)?, next(false)?);
        parts.next().is_none().then_some(v)
    }
}

/// True if `candidate` is a release strictly newer than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(c), Some(cur)) => c.cmp(&cur) == Ordering::Greater,
        // A pre-release build of ours: offer the matching or a newer release.
        (Some(c), None) => {
            let base = current.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next().unwrap_or("");
            Version::parse(base).is_some_and(|b| c >= b)
        }
        _ => false,
    }
}

// ------------------------------------------------------------ GitHub API

/// Reads a `GET /repos/{repo}/releases/latest` (or `/releases/tags/…`)
/// answer. `None` for drafts, pre-releases and releases without installer.
pub fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = v["tag_name"].as_str()?;
    let version = Version::parse(tag)?;
    let version = format!("{}.{}.{}", version.0, version.1, version.2);
    let mut setup_url = String::new();
    let mut sums_url = String::new();
    for asset in v["assets"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let (Some(name), Some(url)) = (asset["name"].as_str(), asset["browser_download_url"].as_str()) else { continue };
        if name.eq_ignore_ascii_case(SUMS_FILE) {
            sums_url = url.into();
        } else if name.starts_with("Batly-Setup") && name.to_ascii_lowercase().ends_with(".exe") {
            setup_url = url.into();
        }
    }
    if !setup_url.starts_with("https://") {
        return None;
    }
    let page_url = match v["html_url"].as_str() {
        Some(u) if u.starts_with("https://") => u.into(),
        _ => format!("https://github.com/{REPO}/releases/tag/{tag}"),
    };
    Some(Release { version, notes: v["body"].as_str().unwrap_or_default().replace("\r\n", "\n"), setup_url, sums_url, page_url })
}

/// Finds the checksum of `file_name` in a `sha256sum` style listing
/// (`<hex>  <name>` or `<hex> *<name>`). Returned in lowercase.
pub fn parse_sums(text: &str, file_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*').trim();
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        (name == file_name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}

/// Checks `data` (the file `file_name`) against a `SHA256SUMS.txt` listing.
pub fn verify(data: &[u8], sums: &str, file_name: &str) -> Result<(), String> {
    let expected = parse_sums(sums, file_name).ok_or_else(|| {
        format!(
            "{} ({file_name})",
            tr([
                "No checksum for the installer in SHA256SUMS.txt",
                "Aucune somme de contrôle pour l'installateur dans SHA256SUMS.txt",
                "Keine Prüfsumme für das Installationsprogramm in SHA256SUMS.txt",
                "No hay suma de comprobación del instalador en SHA256SUMS.txt",
                "Nessun checksum per il programma di installazione in SHA256SUMS.txt",
                "Nenhuma soma de verificação do instalador em SHA256SUMS.txt",
            ])
        )
    })?;
    let actual = sha256::hex_digest(data);
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{}\n{file_name}\n{expected} ≠ {actual}",
            tr([
                "The downloaded installer does not match its published checksum: it was not run.",
                "L'installateur téléchargé ne correspond pas à sa somme de contrôle publiée : il n'a pas été lancé.",
                "Das heruntergeladene Installationsprogramm stimmt nicht mit der veröffentlichten Prüfsumme überein und wurde nicht gestartet.",
                "El instalador descargado no coincide con su suma de comprobación publicada: no se ha ejecutado.",
                "Il programma di installazione scaricato non corrisponde al checksum pubblicato: non è stato avviato.",
                "O instalador transferido não corresponde à soma de verificação publicada: não foi executado.",
            ])
        ))
    }
}

// ------------------------------------------------------------ locations

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default()
}

/// True when the settings live next to the executable (portable copy):
/// such a copy is never replaced automatically.
pub fn portable() -> bool {
    portable_in(&exe_dir())
}

fn portable_in(exe_dir: &Path) -> bool {
    exe_dir.join(crate::config::FILE_NAME).exists()
}

fn stamp_path() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).filter(|p| p.is_absolute());
    crate::config::locate(&exe_dir(), appdata).with_file_name(STAMP_FILE)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// True if an automatic check is due (`last`: time of the previous one).
fn due(now: u64, last: Option<u64>) -> bool {
    match last {
        // A time in the future (clock changed) does not block checks forever.
        Some(last) if last <= now => now - last >= INTERVAL.as_secs(),
        _ => true,
    }
}

fn read_stamp(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_stamp(path: &Path, secs: u64) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{secs}\n"));
}

// --------------------------------------------------------------- checking

/// Looks for a newer release. Blocking (network): call it from a
/// background thread, or use [`check_in_background`].
///
/// Without `force`, returns `None` right away if the last check is less
/// than a day old. The caller filters out the version the user skipped
/// (`[Updates] skipped`) for automatic checks.
pub fn check(force: bool) -> Option<Release> {
    let stamp = stamp_path();
    let now = now_secs();
    if !force && !due(now, read_stamp(&stamp)) {
        return None;
    }
    // Recorded before asking, so a failing network is retried tomorrow,
    // not every few minutes.
    write_stamp(&stamp, now);
    remove_old_installers();
    let headers = [("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")];
    let reply = match fetch::get(LATEST_URL, &headers, Duration::from_secs(20)) {
        Ok(r) => r,
        Err(e) => {
            crate::log!("update check failed: {e}");
            return None;
        }
    };
    if !reply.ok() {
        crate::log!("update check: HTTP {}", reply.status);
        return None;
    }
    let release = parse_release(&reply.text())?;
    crate::log!("update check: latest release {}, running {CURRENT}", release.version);
    is_newer(&release.version, CURRENT).then_some(release)
}

/// Runs [`check`] on a new thread and hands the result to `done` (on that
/// thread: post a message to the UI thread from there).
pub fn check_in_background<F>(force: bool, done: F)
where
    F: FnOnce(Option<Release>) + Send + 'static,
{
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || done(check(force)));
}

/// Installers left in %TEMP% by previous updates.
fn remove_old_installers() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("Batly-Setup-") && name.to_ascii_lowercase().ends_with(".exe") {
            // Fails harmlessly while that installer is still running.
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ------------------------------------------------------------- installing

fn download(url: &str) -> Result<Vec<u8>, String> {
    let failed = tr([
        "Download failed",
        "Échec du téléchargement",
        "Download fehlgeschlagen",
        "Error en la descarga",
        "Download non riuscito",
        "Falha na transferência",
    ]);
    match fetch::get(url, &[], Duration::from_secs(120)) {
        Ok(r) if r.ok() && !r.body.is_empty() => Ok(r.body),
        Ok(r) => Err(format!("{failed}: HTTP {} ({url})", r.status)),
        Err(e) => Err(format!("{failed}: {e} ({url})")),
    }
}

/// File name at the end of a download URL.
fn file_name(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or_default().to_string()
}

/// Opens a URL in the default browser.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    crate::win::shell_open(std::ptr::null_mut(), url, None);
    #[cfg(not(windows))]
    let _ = url;
}

/// Installs `release`, reporting progress from 0 to 100. Blocking: run it
/// on a background thread. On success the installer is running and the
/// application has been asked to close (WM_CLOSE to its window).
///
/// A portable copy is not replaced: the release page is opened instead
/// and `Ok(())` is returned (see [`portable`]).
pub fn install(release: &Release, progress: impl Fn(u8)) -> Result<(), String> {
    if portable() {
        open_url(&release.page_url);
        return Ok(());
    }
    if Version::parse(&release.version).is_none() {
        return Err(format!("invalid version {}", release.version));
    }
    let name = file_name(&release.setup_url);
    if !release.setup_url.starts_with("https://") || !name.starts_with("Batly-Setup") || !name.to_ascii_lowercase().ends_with(".exe") {
        return Err(format!("unexpected installer URL: {}", release.setup_url));
    }
    if release.sums_url.is_empty() {
        return Err(tr([
            "This release publishes no SHA256SUMS.txt: the installer cannot be verified. Download it from the release page.",
            "Cette version ne publie pas de SHA256SUMS.txt : l'installateur ne peut pas être vérifié. Téléchargez-le depuis la page de la version.",
            "Diese Version veröffentlicht keine SHA256SUMS.txt: Das Installationsprogramm kann nicht geprüft werden. Laden Sie es von der Versionsseite herunter.",
            "Esta versión no publica SHA256SUMS.txt: no se puede verificar el instalador. Descárguelo desde la página de la versión.",
            "Questa versione non pubblica SHA256SUMS.txt: il programma di installazione non può essere verificato. Scaricalo dalla pagina della versione.",
            "Esta versão não publica SHA256SUMS.txt: o instalador não pode ser verificado. Transfira-o a partir da página da versão.",
        ])
        .into());
    }

    progress(0);
    let data = download(&release.setup_url)?;
    progress(70);
    let sums = download(&release.sums_url)?;
    progress(80);
    verify(&data, &String::from_utf8_lossy(&sums), &name)?;
    progress(90);

    let path = std::env::temp_dir().join(&name);
    std::fs::write(&path, &data).map_err(|e| format!("{}: {e}", path.display()))?;
    crate::log!("update: starting {} /S /update", path.display());
    std::process::Command::new(&path)
        .args(["/S", "/update"])
        .current_dir(std::env::temp_dir())
        .spawn()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    progress(100);
    close_app();
    Ok(())
}

/// Asks our own window to close (the normal "Quit" path).
fn close_app() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
        let class = crate::win::wide(crate::win::WINDOW_CLASS);
        // SAFETY: valid NUL-terminated class name; posting WM_CLOSE to the
        // window found (ours: single instance) is always allowed.
        unsafe {
            let hwnd = FindWindowW(class.as_ptr(), std::ptr::null());
            if !hwnd.is_null() {
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(Version::parse("4.1.0"), Some(Version(4, 1, 0)));
        assert_eq!(Version::parse("v4.1"), Some(Version(4, 1, 0)));
        assert_eq!(Version::parse("V10"), Some(Version(10, 0, 0)));
        assert_eq!(Version::parse("4.1.0+abc"), Some(Version(4, 1, 0)));
        for bad in ["4.1.0-beta.1", "", "v", "4..1", "4.1.0.0", "4.x", "4.1.-1"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version(4, 10, 0) > Version(4, 9, 9));
        assert!(is_newer("4.1.0", "4.0.0"));
        assert!(is_newer("v4.0.10", "4.0.9"));
        assert!(is_newer("5.0", "4.99.99"));
        assert!(!is_newer("4.0.0", "4.0.0"));
        assert!(!is_newer("3.9.9", "4.0.0"));
        assert!(!is_newer("4.2.0-rc.1", "4.0.0"));
        // Running a pre-release: the final release is newer.
        assert!(is_newer("4.1.0", "4.1.0-beta.2"));
        assert!(!is_newer("4.0.9", "4.1.0-beta.2"));
    }

    const RELEASE: &str = r###"{
        "tag_name": "v4.1.0", "draft": false, "prerelease": false,
        "html_url": "https://github.com/illama/batly/releases/tag/v4.1.0",
        "body": "## Nouveautés\r\n- tout",
        "assets": [
            {"name": "Batly-4.1.0-portable.zip", "browser_download_url": "https://github.com/x/y/releases/download/v4.1.0/Batly-4.1.0-portable.zip"},
            {"name": "Batly-Setup-4.1.0.exe", "browser_download_url": "https://github.com/x/y/releases/download/v4.1.0/Batly-Setup-4.1.0.exe"},
            {"name": "SHA256SUMS.txt", "browser_download_url": "https://github.com/x/y/releases/download/v4.1.0/SHA256SUMS.txt"}
        ]
    }"###;

    #[test]
    fn github_release() {
        let r = parse_release(RELEASE).unwrap();
        assert_eq!(r.version, "4.1.0");
        assert_eq!(r.notes, "## Nouveautés\n- tout");
        assert!(r.setup_url.ends_with("/Batly-Setup-4.1.0.exe") && r.sums_url.ends_with("/SHA256SUMS.txt"));
        assert!(r.page_url.ends_with("/tag/v4.1.0"));
        assert_eq!(file_name(&r.setup_url), "Batly-Setup-4.1.0.exe");

        assert!(parse_release(&RELEASE.replace("\"prerelease\": false", "\"prerelease\": true")).is_none());
        assert!(parse_release(&RELEASE.replace("\"draft\": false", "\"draft\": true")).is_none());
        assert!(parse_release(&RELEASE.replace("v4.1.0\"", "v4.1.0-rc.1\"")).is_none());
        assert!(parse_release(&RELEASE.replace("Batly-Setup-4.1.0.exe\"", "other.exe\"")).is_none());
        assert!(parse_release("{\"message\": \"API rate limit exceeded\"}").is_none());
        assert!(parse_release("not json").is_none());
    }

    #[test]
    fn checksums() {
        let data = b"installer bytes";
        let hash = sha256::hex_digest(data);
        let sums = format!("{}  Batly-4.1.0-portable.zip\n{}  Batly-Setup-4.1.0.exe\r\n", "0".repeat(64), hash.to_uppercase());
        assert_eq!(parse_sums(&sums, "Batly-Setup-4.1.0.exe"), Some(hash.clone()));
        assert_eq!(parse_sums(&format!("{hash} *dist/Batly-Setup-4.1.0.exe"), "Batly-Setup-4.1.0.exe"), Some(hash.clone()));
        assert_eq!(parse_sums(&sums, "Batly-Setup-4.0.0.exe"), None);
        assert_eq!(parse_sums("abc  Batly-Setup-4.1.0.exe", "Batly-Setup-4.1.0.exe"), None);
        assert!(verify(data, &sums, "Batly-Setup-4.1.0.exe").is_ok());
        assert!(verify(b"tampered", &sums, "Batly-Setup-4.1.0.exe").is_err());
        assert!(verify(data, &sums, "missing.exe").is_err());
    }

    #[test]
    fn daily_schedule() {
        let day = INTERVAL.as_secs();
        assert!(due(1_000_000, None));
        assert!(!due(1_000_000, Some(1_000_000 - day + 1)));
        assert!(due(1_000_000, Some(1_000_000 - day)));
        assert!(due(1_000_000, Some(2_000_000)));

        let dir = std::env::temp_dir().join(format!("batly-upd-{}", std::process::id()));
        let path = dir.join(STAMP_FILE);
        write_stamp(&path, 1234);
        assert_eq!(read_stamp(&path), Some(1234));
        assert!(!portable_in(&dir));
        std::fs::write(dir.join(crate::config::FILE_NAME), "").unwrap();
        assert!(portable_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
