//! Optional debug log (`lgstray.exe --debug`), written next to the settings.

use std::fmt::Arguments;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static LOG: OnceLock<(Mutex<File>, Instant)> = OnceLock::new();

pub fn init(path: &Path) {
    if let Ok(file) = File::create(path) {
        let _ = LOG.set((Mutex::new(file), Instant::now()));
    }
}

pub fn enabled() -> bool {
    LOG.get().is_some()
}

pub fn write(args: Arguments) {
    if let Some((file, start)) = LOG.get() {
        if let Ok(mut f) = file.lock() {
            let _ = writeln!(f, "[{:>9.3}] {}", start.elapsed().as_secs_f64(), args);
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
