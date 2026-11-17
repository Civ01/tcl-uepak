//! Optional diagnostic log, enabled with `DebugLog=1` in `uepak.ini`.
//!
//! Written to `uepak.log` in the plugin folder. If that folder is not
//! writable, the log is kept in memory (last ~512 KB) and copied to the
//! clipboard whenever an operation fails.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const LOG_NAME: &str = "uepak.log";
const MEMORY_CAP: usize = 512 * 1024;

enum Sink {
    File(File, PathBuf),
    Memory(String),
}

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Tests can pin logging on regardless of `uepak.ini`.
#[cfg(test)]
pub static FORCE: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Logs a formatted line when debug logging is on; free otherwise.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        if $crate::log::enabled() {
            $crate::log::write(&format!($($arg)*));
        }
    };
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turns logging on/off. Opening the sink happens once per enable.
pub fn configure(enable: bool) {
    #[cfg(test)]
    let enable = enable || FORCE.load(Ordering::Relaxed);
    let mut sink = SINK.lock().unwrap_or_else(|e| e.into_inner());
    if !enable {
        ENABLED.store(false, Ordering::Relaxed);
        *sink = None;
        return;
    }
    if sink.is_some() {
        ENABLED.store(true, Ordering::Relaxed);
        return;
    }
    let file = crate::config::plugin_dir().and_then(|dir| {
        let path = dir.join(LOG_NAME);
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(|f| Sink::File(f, path))
    });
    *sink = Some(file.unwrap_or(Sink::Memory(String::new())));
    ENABLED.store(true, Ordering::Relaxed);
    drop(sink);
    write(&format!(
        "===== uepak {} log started ({}) =====",
        env!("CARGO_PKG_VERSION"),
        location()
    ));
}

/// Human-readable description of where the log goes.
pub fn location() -> String {
    match &*SINK.lock().unwrap_or_else(|e| e.into_inner()) {
        Some(Sink::File(_, path)) => format!("file {}", path.display()),
        Some(Sink::Memory(_)) => {
            "memory: plugin folder not writable, copied to clipboard on errors".into()
        }
        None => "off".into(),
    }
}

pub fn write(msg: &str) {
    let mut sink = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let prefix = format!(
        "{} [{:5}] ",
        crate::win::local_timestamp(),
        crate::win::thread_id()
    );
    let mut text = String::with_capacity(msg.len() + prefix.len() + 2);
    for line in msg.lines() {
        text.push_str(&prefix);
        text.push_str(line);
        text.push_str("\r\n");
    }
    match sink.as_mut() {
        Some(Sink::File(f, _)) => {
            let _ = f.write_all(text.as_bytes());
        }
        Some(Sink::Memory(buf)) => {
            buf.push_str(&text);
            if buf.len() > MEMORY_CAP {
                let mut cut = buf.len() - MEMORY_CAP / 2;
                while !buf.is_char_boundary(cut) {
                    cut += 1;
                }
                let cut = buf[cut..].find('\n').map_or(cut, |i| cut + i + 1);
                buf.replace_range(..cut, "... (older lines dropped)\r\n");
            }
        }
        None => {}
    }
}

/// Call when an operation fails. With an in-memory log, copies it to the
/// clipboard and returns a note to append to any message shown to the user.
pub fn on_error() -> Option<String> {
    if !enabled() {
        return None;
    }
    let sink = SINK.lock().unwrap_or_else(|e| e.into_inner());
    match sink.as_ref() {
        Some(Sink::Memory(buf)) => {
            let text = buf.clone();
            drop(sink);
            if crate::win::copy_to_clipboard(&text) {
                Some("Debug log copied to the clipboard.".into())
            } else {
                Some("Debug log could not be copied to the clipboard.".into())
            }
        }
        Some(Sink::File(_, path)) => Some(format!("Debug log: {}", path.display())),
        None => None,
    }
}
