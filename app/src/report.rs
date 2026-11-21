//! "Report a problem": gathers what is needed to investigate an issue in a
//! zip file and opens a prefilled GitHub issue.
//!
//! The zip holds the log (`batly.log` and its rotated `.old`), the
//! settings with every secret masked, the device list (JSON from the hub)
//! and the application / Windows versions. It is written with the "store"
//! method, so no compression code is needed.

use crate::i18n::tr;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const NEW_ISSUE_URL: &str = "https://github.com/illama/batly/issues/new";
const MASK: &str = "***";

/// Builds the report zip on the Desktop (or in %TEMP% if the Desktop
/// cannot be written) and returns its path.
///
/// * `devices_json`: the device list as served by the HTTP API.
/// * `settings_text`: the settings file content (secrets are masked here).
/// * `log_path`: `batly.log` (missing files are simply left out).
pub fn create(devices_json: &str, settings_text: &str, log_path: &Path) -> io::Result<PathBuf> {
    let now = SystemTime::now();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    entries.push(("system.txt".into(), system_info(log_path).into_bytes()));
    entries.push(("devices.json".into(), devices_json.as_bytes().to_vec()));
    entries.push(("appsettings.toml".into(), mask_secrets(settings_text).into_bytes()));
    for path in [crate::log::old_path(log_path), log_path.to_path_buf()] {
        if let Ok(data) = std::fs::read(&path) {
            let name = path.file_name().map_or_else(|| "batly.log".into(), |n| n.to_string_lossy().into_owned());
            entries.push((name, data));
        }
    }
    let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let zip = zip_store(&refs, now);

    let stamp: String = crate::http::iso8601(now).chars().filter(char::is_ascii_digit).collect();
    let name = format!("Batly-report-{}-{}.zip", &stamp[..8], &stamp[8..]);
    let mut last_error = None;
    for dir in [desktop(), Some(std::env::temp_dir())].into_iter().flatten() {
        let path = dir.join(&name);
        match std::fs::write(&path, &zip) {
            Ok(()) => return Ok(path),
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no folder to write the report to")))
}

/// Opens the prefilled "New issue" page and selects `zip` in Explorer so
/// that it can be dragged into the issue.
pub fn open_issue(zip: &Path) {
    let name = zip.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let url = issue_url(env!("CARGO_PKG_VERSION"), &windows_version(), &name);
    crate::update::open_url(&url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `/select,"path"` must reach Explorer unquoted as a whole.
        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", zip.display())).spawn();
    }
}

/// [`create`] then [`open_issue`].
pub fn run(devices_json: &str, settings_text: &str, log_path: &Path) -> io::Result<PathBuf> {
    let zip = create(devices_json, settings_text, log_path)?;
    open_issue(&zip);
    Ok(zip)
}

fn system_info(log_path: &Path) -> String {
    format!(
        "Batly {}\nWindows: {}\nLanguage: {}\nPortable: {}\nLog enabled: {}\nLog file: {}\nCreated: {}\n",
        env!("CARGO_PKG_VERSION"),
        windows_version(),
        crate::i18n::lang().code(),
        crate::update::portable(),
        crate::log::enabled(),
        log_path.display(),
        crate::http::iso8601(SystemTime::now()),
    )
}

// ------------------------------------------------------------ issue page

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// URL of a new issue with title and body prefilled.
pub fn issue_url(app_version: &str, windows: &str, zip_name: &str) -> String {
    let title = format!(
        "[Bug] {}",
        tr([
            "<short description>",
            "<description courte>",
            "<kurze Beschreibung>",
            "<descripción breve>",
            "<breve descrizione>",
            "<descrição curta>",
        ])
    );
    let what =
        tr(["What happened?", "Que s'est-il passé ?", "Was ist passiert?", "¿Qué ha pasado?", "Che cosa è successo?", "O que aconteceu?"]);
    let steps = tr([
        "Steps to reproduce",
        "Étapes pour reproduire",
        "Schritte zum Reproduzieren",
        "Pasos para reproducirlo",
        "Passaggi per riprodurre",
        "Passos para reproduzir",
    ]);
    let devices = tr([
        "Devices concerned (model, receiver / Bluetooth / cable)",
        "Périphériques concernés (modèle, récepteur / Bluetooth / câble)",
        "Betroffene Geräte (Modell, Empfänger / Bluetooth / Kabel)",
        "Dispositivos afectados (modelo, receptor / Bluetooth / cable)",
        "Dispositivi interessati (modello, ricevitore / Bluetooth / cavo)",
        "Dispositivos afetados (modelo, recetor / Bluetooth / cabo)",
    ]);
    let attach = tr([
        "Please attach the report file {file}: it is on your Desktop (or in %TEMP%) and already selected in Explorer, just drag it into this box. Passwords, tokens and webhook URLs have been masked.",
        "Merci de joindre le fichier de rapport {file} : il est sur votre Bureau (ou dans %TEMP%) et déjà sélectionné dans l'Explorateur, il suffit de le glisser dans cette zone. Mots de passe, jetons et URL de webhooks ont été masqués.",
        "Bitte hängen Sie die Berichtsdatei {file} an: Sie liegt auf Ihrem Desktop (oder in %TEMP%) und ist im Explorer bereits markiert, ziehen Sie sie einfach in dieses Feld. Passwörter, Token und Webhook-URLs wurden unkenntlich gemacht.",
        "Adjunte el archivo de informe {file}: está en su Escritorio (o en %TEMP%) y ya seleccionado en el Explorador, solo tiene que arrastrarlo a este cuadro. Las contraseñas, los tokens y las URL de webhooks se han ocultado.",
        "Allega il file di report {file}: si trova sul Desktop (o in %TEMP%) ed è già selezionato in Esplora file, basta trascinarlo in questo riquadro. Password, token e URL dei webhook sono stati oscurati.",
        "Anexe o ficheiro de relatório {file}: está no Ambiente de Trabalho (ou em %TEMP%) e já selecionado no Explorador, basta arrastá-lo para esta caixa. Palavras-passe, tokens e URLs de webhooks foram ocultados.",
    ])
    .replace("{file}", &format!("`{zip_name}`"));
    let body = format!(
        "**Batly**: {app_version}\n**Windows**: {windows}\n\n### {what}\n\n\n### {steps}\n1. \n\n### {devices}\n\n\n---\n📎 {attach}\n"
    );
    format!("{NEW_ISSUE_URL}?title={}&body={}", percent_encode(&title), percent_encode(&body))
}

/// e.g. "Windows 11 Pro 24H2 (26100.2314)".
pub fn windows_version() -> String {
    #[cfg(windows)]
    {
        const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let mut product = hklm_string(KEY, "ProductName").unwrap_or_else(|| "Windows".into());
        let build = hklm_string(KEY, "CurrentBuildNumber").unwrap_or_default();
        // ProductName still says "Windows 10" on Windows 11.
        if build.parse::<u32>().is_ok_and(|b| b >= 22000) {
            product = product.replacen("Windows 10", "Windows 11", 1);
        }
        let display = hklm_string(KEY, "DisplayVersion").unwrap_or_default();
        let ubr = hklm_dword(KEY, "UBR").map(|u| format!(".{u}")).unwrap_or_default();
        let mut text = product;
        if !display.is_empty() {
            text.push(' ');
            text.push_str(&display);
        }
        text.push_str(&format!(" ({build}{ubr})"));
        text
    }
    #[cfg(not(windows))]
    {
        format!("not Windows ({})", std::env::consts::OS)
    }
}

#[cfg(windows)]
fn hklm_string(subkey: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let (k, v) = (crate::win::wide(subkey), crate::win::wide(value));
    let mut buf = [0u16; 256];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf` is writable for `size` bytes; names are NUL-terminated.
    let rc = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size)
    };
    if rc != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

#[cfg(windows)]
fn hklm_dword(subkey: &str, value: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
    let (k, v) = (crate::win::wide(subkey), crate::win::wide(value));
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: `data` is 4 writable bytes; names are NUL-terminated.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            k.as_ptr(),
            v.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (rc == 0).then_some(data)
}

/// The user's Desktop folder (follows OneDrive redirection).
fn desktop() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Com::CoTaskMemFree;
        use windows_sys::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath};
        let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: `raw` receives a CoTaskMemAlloc'ed NUL-terminated string,
        // read once and freed below (CoTaskMemFree accepts null on error).
        unsafe {
            let hr = SHGetKnownFolderPath(&FOLDERID_Desktop, 0, std::ptr::null_mut(), &mut raw);
            let path = if hr >= 0 && !raw.is_null() {
                let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
                Some(PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(raw, len))))
            } else {
                None
            };
            CoTaskMemFree(raw as *const core::ffi::c_void);
            path.filter(|p| p.is_dir())
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// --------------------------------------------------------- secret masking

/// Settings keys whose values are masked, as "section.key" (lowercase).
const SECRETS: [&str; 3] = ["httpserver.token", "mqtt.password", "webhooks.urls"];
/// Peer URLs: only their "#token" part is masked.
const PEERS: &str = "httpserver.peers";

#[derive(Clone, Copy, PartialEq)]
enum Mask {
    None,
    Whole,
    AfterHash,
}

fn mask_kind(section: &str, key: &str) -> Mask {
    let full = if section.is_empty() { key.to_string() } else { format!("{section}.{key}") };
    if SECRETS.contains(&full.as_str()) {
        Mask::Whole
    } else if full == PEERS {
        Mask::AfterHash
    } else {
        Mask::None
    }
}

fn mask_string(content: &str, mask: Mask) -> String {
    match mask {
        _ if content.is_empty() => String::new(),
        Mask::None => content.to_string(),
        Mask::Whole => MASK.to_string(),
        Mask::AfterHash => match content.split_once('#') {
            Some((url, token)) if !token.is_empty() => format!("{url}#{MASK}"),
            _ => content.to_string(),
        },
    }
}

/// Rewrites the string literals of a value (`segment`), keeping comments.
/// `depth` counts the open `[` of an array spanning several lines.
fn mask_value(segment: &str, mask: Mask, depth: &mut i32) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut chars = segment.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '#' => {
                out.push_str(&segment[i..]);
                break;
            }
            '[' => {
                *depth += 1;
                out.push(c);
            }
            ']' => {
                *depth -= 1;
                out.push(c);
            }
            '"' | '\'' => {
                let mut content = String::new();
                let mut closed = false;
                while let Some((_, d)) = chars.next() {
                    if d == c {
                        closed = true;
                        break;
                    }
                    if d == '\\' && c == '"' {
                        content.push(d);
                        if let Some((_, e)) = chars.next() {
                            content.push(e);
                        }
                        continue;
                    }
                    content.push(d);
                }
                out.push(c);
                out.push_str(&mask_string(&content, mask));
                if closed {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Replaces the secrets of a settings file (`[HTTPServer] token`,
/// `[MQTT] password`, `[Webhooks] urls`, peer tokens) with "***". Empty
/// values stay empty, so the report still tells whether one is set.
pub fn mask_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut section = String::new();
    // Mask of a multi-line array still open, with its bracket depth.
    let mut open: Option<(Mask, i32)> = None;
    for line in text.split_inclusive('\n') {
        if let Some((mask, mut depth)) = open.take() {
            out.push_str(&mask_value(line, mask, &mut depth));
            if depth > 0 {
                open = Some((mask, depth));
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            let header = trimmed.trim_start_matches('[');
            let end = header.find(']').unwrap_or(header.len());
            section = header[..end].trim().trim_matches(['"', '\'']).to_ascii_lowercase();
            out.push_str(line);
            continue;
        }
        let Some(eq) = line.find('=').filter(|&i| !line[..i].contains('#')) else {
            out.push_str(line);
            continue;
        };
        let key: String = line[..eq].trim().split('.').map(|p| p.trim().trim_matches(['"', '\''])).collect::<Vec<_>>().join(".");
        let mask = mask_kind(&section, &key.to_ascii_lowercase());
        out.push_str(&line[..=eq]);
        let mut depth = 0;
        out.push_str(&mask_value(&line[eq + 1..], mask, &mut depth));
        if depth > 0 {
            open = Some((mask, depth));
        }
    }
    out
}

// -------------------------------------------------------------------- zip

/// CRC-32 (IEEE 802.3, as used by zip).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// MS-DOS date and time (UTC) of a zip entry.
fn dos_time(t: SystemTime) -> (u16, u16) {
    let iso = crate::http::iso8601(t); // YYYY-MM-DDTHH:MM:SSZ
    let n = |r: std::ops::Range<usize>| iso.get(r).and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    let year = n(0..4).clamp(1980, 2107);
    let time = (n(11..13) << 11) | (n(14..16) << 5) | (n(17..19) / 2);
    let date = ((year - 1980) << 9) | (n(5..7) << 5) | n(8..10);
    (time, date)
}

/// Builds a zip archive of `entries` (name, data) with the "store"
/// method. Names are UTF-8 (general purpose flag bit 11).
pub fn zip_store(entries: &[(&str, &[u8])], mtime: SystemTime) -> Vec<u8> {
    const FLAGS: u16 = 0x0800;
    let mtime = if mtime < UNIX_EPOCH { UNIX_EPOCH } else { mtime };
    let (time, date) = dos_time(mtime);
    let mut out = Vec::new();
    let mut central = Vec::new();
    let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data) in entries {
        let crc = crc32(data);
        let offset = out.len() as u32;
        let (name_len, size) = (name.len() as u16, data.len() as u32);

        le32(&mut out, 0x0403_4b50);
        le16(&mut out, 20); // version needed
        le16(&mut out, FLAGS);
        le16(&mut out, 0); // stored
        le16(&mut out, time);
        le16(&mut out, date);
        le32(&mut out, crc);
        le32(&mut out, size);
        le32(&mut out, size);
        le16(&mut out, name_len);
        le16(&mut out, 0); // extra
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        le32(&mut central, 0x0201_4b50);
        le16(&mut central, 20); // made by (MS-DOS, 2.0)
        le16(&mut central, 20);
        le16(&mut central, FLAGS);
        le16(&mut central, 0);
        le16(&mut central, time);
        le16(&mut central, date);
        le32(&mut central, crc);
        le32(&mut central, size);
        le32(&mut central, size);
        le16(&mut central, name_len);
        le16(&mut central, 0); // extra
        le16(&mut central, 0); // comment
        le16(&mut central, 0); // disk
        le16(&mut central, 0); // internal attributes
        le32(&mut central, 0); // external attributes
        le32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, entries.len() as u16);
    le16(&mut out, entries.len() as u16);
    le32(&mut out, central_size);
    le32(&mut out, central_offset);
    le16(&mut out, 0); // comment
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn crc() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn dos_timestamps() {
        // 2026-01-31T18:04:05Z
        let t = UNIX_EPOCH + Duration::from_secs(1_769_882_645);
        assert_eq!(crate::http::iso8601(t), "2026-01-31T18:04:05Z");
        assert_eq!(dos_time(t), ((18 << 11) | (4 << 5) | 2, (46 << 9) | (1 << 5) | 31));
    }

    #[test]
    fn masks_secrets() {
        let text = "\
[HTTPServer]
enabled = true
token = \"s3cr3t\" # access token
peers = [\"http://pc:12321#t0k\", \"http://other:1\"]

[MQTT]
username = \"me\"
password = 'p#ss'
baseTopic = \"batly\"

[Webhooks]
urls = [
  \"https://discord.com/api/webhooks/1/abc\", # main
  \"https://hooks.slack.com/x\",
]
allChanges = false

[Updates]
token = \"not a secret here\"
";
        let masked = mask_secrets(text);
        for secret in ["s3cr3t", "t0k", "p#ss", "discord.com", "slack.com"] {
            assert!(!masked.contains(secret), "{secret} leaked:\n{masked}");
        }
        assert!(masked.contains("token = \"***\" # access token\n"));
        assert!(masked.contains("peers = [\"http://pc:12321#***\", \"http://other:1\"]"));
        assert!(masked.contains("password = '***'"));
        assert!(masked.contains("  \"***\", # main\n  \"***\",\n]\nallChanges = false"));
        assert!(masked.contains("username = \"me\"") && masked.contains("baseTopic = \"batly\""));
        assert!(masked.contains("token = \"not a secret here\""));
        assert_eq!(masked.lines().count(), text.lines().count());

        // Empty values stay visible as empty; other spellings are caught.
        assert_eq!(mask_secrets("[HTTPServer]\ntoken = \"\"\n"), "[HTTPServer]\ntoken = \"\"\n");
        assert_eq!(mask_secrets("[ mqtt ]\n\"password\"=\"a\\\"b\"\n"), "[ mqtt ]\n\"password\"=\"***\"\n");
        assert_eq!(mask_secrets("HTTPServer.token = \"x\"\n"), "HTTPServer.token = \"***\"\n");
        assert_eq!(
            mask_secrets("[Webhooks]\nurls = [\"a\", [\"b\"]]\nx = \"c\"\n"),
            "[Webhooks]\nurls = [\"***\", [\"***\"]]\nx = \"c\"\n"
        );
        // The packaged default settings come out unchanged (no secret set).
        let defaults = include_str!("../../packaging/appsettings.toml");
        assert_eq!(mask_secrets(defaults), defaults);
    }

    #[test]
    fn issue_page() {
        let url = issue_url("4.1.0", "Windows 11 Pro 24H2 (26100.1)", "Batly-report-20260131-180405.zip");
        assert!(url.starts_with("https://github.com/illama/batly/issues/new?title=%5BBug%5D%20"));
        assert!(url.contains("&body=%2A%2ABatly%2A%2A%3A%204.1.0%0A"));
        assert!(url.contains("Windows%2011%20Pro%2024H2%20%2826100.1%29"));
        assert!(url.contains("Batly-report-20260131-180405.zip"));
        assert!(!url[url.find('?').unwrap() + 1..].contains([' ', '\n', '#', '?']));
    }

    fn python() -> Option<&'static str> {
        std::process::Command::new("python3").arg("--version").output().ok().filter(|o| o.status.success()).map(|_| "python3")
    }

    #[test]
    fn zip_is_valid() {
        let log: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let entries: [(&str, &[u8]); 4] =
            [("system.txt", b"Batly 4.1.0\n"), ("empty.txt", b""), ("réglages.toml", b"a = 1\n"), ("batly.log", &log)];
        let zip = zip_store(&entries, UNIX_EPOCH + Duration::from_secs(1_769_882_645));
        assert_eq!(&zip[..4], b"PK\x03\x04");
        assert_eq!(&zip[zip.len() - 22..zip.len() - 18], b"PK\x05\x06");

        let Some(py) = python() else { return };
        let dir = std::env::temp_dir().join(format!("batly-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.zip");
        std::fs::write(&path, &zip).unwrap();
        let test = std::process::Command::new(py).args(["-m", "zipfile", "-t"]).arg(&path).output().unwrap();
        assert!(test.status.success(), "{}", String::from_utf8_lossy(&test.stderr));
        let script = "import sys, zipfile, hashlib\n\
            z = zipfile.ZipFile(sys.argv[1])\n\
            for i in z.infolist():\n    print(i.filename, i.file_size, i.date_time, hashlib.sha256(z.read(i)).hexdigest())\n";
        let listing = std::process::Command::new(py).env("PYTHONIOENCODING", "utf-8").args(["-c", script]).arg(&path).output().unwrap();
        let listing = String::from_utf8(listing.stdout).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(lines.len(), 4, "{listing}");
        for ((name, data), line) in entries.iter().zip(&lines) {
            let expected = format!("{name} {} (2026, 1, 31, 18, 4, 4) {}", data.len(), crate::sha256::hex_digest(data));
            assert_eq!(*line, expected);
        }
    }

    #[test]
    fn report_file() {
        let dir = std::env::temp_dir().join(format!("batly-report-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("batly.log");
        std::fs::write(&log, "line\n").unwrap();
        let path = create("[{\"id\":\"1\"}]", "[MQTT]\npassword = \"pw\"\n", &log).unwrap();
        let data = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("Batly-report-") && name.ends_with(".zip") && name.len() == "Batly-report-20260131-180405.zip".len());
        let has = |needle: &[u8]| data.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"devices.json") && has(b"appsettings.toml") && has(b"system.txt") && has(b"batly.log"));
        assert!(has(b"password = \"***\"") && !has(b"\"pw\"") && has(b"line\n"));
    }
}
