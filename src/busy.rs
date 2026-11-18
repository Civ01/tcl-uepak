//! Paks being written right now (SPEC §8.1). With background operations TC
//! can run several `PackFiles`/`DeleteFiles`/`OpenArchive` calls at once, on
//! different threads. Two writers on the same pak would corrupt it, and
//! opening a pak mid-write would see a half-written index (and its undo file,
//! wrongly offering a restore). A writer holds a [`Guard`] for the pak;
//! others are refused while it lives.

use std::path::Path;
use std::sync::Mutex;

static BUSY: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn key(path: &Path) -> String {
    let p = path.to_string_lossy().replace('/', "\\");
    p.trim_end_matches('\\').to_lowercase()
}

/// Released when dropped (also on error or panic).
#[derive(Debug)]
pub struct Guard(String);

impl Drop for Guard {
    fn drop(&mut self) {
        let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = b.iter().position(|k| *k == self.0) {
            b.swap_remove(i);
        }
    }
}

/// Claims `path` for writing; `None` if another operation is writing it.
pub fn acquire(path: &Path) -> Option<Guard> {
    let k = key(path);
    let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    if b.contains(&k) {
        return None;
    }
    b.push(k.clone());
    Some(Guard(k))
}

/// Whether another operation is writing `path` right now.
pub fn is_busy(path: &Path) -> bool {
    let k = key(path);
    BUSY.lock().unwrap_or_else(|e| e.into_inner()).contains(&k)
}
