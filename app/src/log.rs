//! Optional log, written next to the settings (`batly.log`).
//!
//! Enabled at startup by `batly.exe --debug`, or at any time with
//! [`enable`] (e.g. from a menu). The file is appended to and rotated once
//! it grows past [`MAX_SIZE`]: the previous content is kept as
//! `batly.log.old`, so the log never takes more than about twice that.

use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// Size after which the log is rotated.
pub const MAX_SIZE: u64 = 1024 * 1024;

struct Sink {
    path: PathBuf,
    file: File,
    size: u64,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

/// Path of the rotated (previous) log.
pub fn old_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

fn open(path: &Path) -> std::io::Result<Sink> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let size = file.metadata().map_or(0, |m| m.len());
    Ok(Sink { path: path.to_path_buf(), file, size })
}

fn rotate(sink: &mut Sink) {
    let old = old_path(&sink.path);
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(&sink.path, &old);
    // If the rename failed (file locked by a viewer), truncating keeps the
    // size bounded anyway.
    if let Ok(file) = OpenOptions::new().create(true).write(true).truncate(true).open(&sink.path) {
        sink.file = file;
        sink.size = 0;
    }
}

/// Starts logging to `path` (appending). Can be called at any time, from
/// any thread; calling it again switches to the new path.
pub fn enable(path: &Path) -> bool {
    START.get_or_init(Instant::now);
    let Ok(mut sink) = open(path) else { return false };
    if sink.size >= MAX_SIZE {
        rotate(&mut sink);
    }
    let _ = writeln!(sink.file, "----- {} Batly {}", crate::http::iso8601(SystemTime::now()), env!("CARGO_PKG_VERSION"));
    if let Ok(mut guard) = SINK.lock() {
        *guard = Some(sink);
        ENABLED.store(true, Ordering::Release);
        return true;
    }
    false
}

/// Same as [`enable`]; kept for `--debug`.
pub fn init(path: &Path) {
    enable(path);
}

/// Stops logging (the file is kept).
pub fn disable() {
    ENABLED.store(false, Ordering::Release);
    if let Ok(mut guard) = SINK.lock() {
        *guard = None;
    }
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// Current log file, if logging is on.
pub fn path() -> Option<PathBuf> {
    SINK.lock().ok()?.as_ref().map(|s| s.path.clone())
}

pub fn write(args: Arguments) {
    let elapsed = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    let line = format!("[{elapsed:>9.3}] {args}\n");
    if let Ok(mut guard) = SINK.lock() {
        if let Some(sink) = guard.as_mut() {
            if sink.size + line.len() as u64 > MAX_SIZE {
                rotate(sink);
            }
            if sink.file.write_all(line.as_bytes()).is_ok() {
                sink.size += line.len() as u64;
            }
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for b in bytes {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        if $crate::log::enabled() {
            $crate::log::write(format_args!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // The logger is global: everything is checked in a single test.
    #[test]
    fn enable_write_rotate() {
        let dir = std::env::temp_dir().join(format!("batly-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("batly.log");

        assert!(!enabled());
        crate::log!("dropped");
        assert!(enable(&path));
        assert!(enabled() && super::path().as_deref() == Some(path.as_path()));
        crate::log!("hello {}", 42);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Batly") && text.contains("] hello 42\n") && !text.contains("dropped"));

        let chunk = "x".repeat(1000);
        for _ in 0..1100 {
            crate::log!("{chunk}");
        }
        let size = std::fs::metadata(&path).unwrap().len();
        let old = std::fs::metadata(old_path(&path)).unwrap().len();
        assert!(size < MAX_SIZE && old <= MAX_SIZE && old > MAX_SIZE / 2, "{size} {old}");

        disable();
        assert!(!enabled() && super::path().is_none());
        let before = std::fs::metadata(&path).unwrap().len();
        crate::log!("after");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
