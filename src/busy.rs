//! Paks being written right now (SPEC §8.1). With background operations TC
//! can run several `PackFiles`/`DeleteFiles`/`OpenArchive` calls at once, on
//! different threads. Two writers on the same pak would corrupt it, and
//! opening a pak mid-write would see a half-written index (and its undo file,
//! wrongly offering a restore). A writer holds a [`Guard`] for the pak;
//! others are refused while it lives.
//!
//! Across processes (two TC instances) the writer also holds an OS lock on
//! `<pak>.uepak-lock` (`File::try_lock`: LockFileEx on Windows). The OS
//! drops the lock if TC crashes, so a leftover lock file never blocks.

use crate::dlog;
use std::path::Path;
use std::sync::Mutex;

static BUSY: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn key(path: &Path) -> String {
    let p = path.to_string_lossy().replace('/', "\\");
    p.trim_end_matches('\\').to_lowercase()
}

pub fn lock_path(pak: &Path) -> std::path::PathBuf {
    let mut name = pak.file_name().unwrap_or_default().to_os_string();
    name.push(".uepak-lock");
    pak.with_file_name(name)
}

/// Released when dropped (also on error or panic).
#[derive(Debug)]
pub struct Guard {
    key: String,
    lock: Option<(std::fs::File, std::path::PathBuf)>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some((f, p)) = self.lock.take() {
            let _ = f.unlock();
            drop(f);
            let _ = std::fs::remove_file(p);
        }
        let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = b.iter().position(|k| *k == self.key) {
            b.swap_remove(i);
        }
    }
}

/// Tries the cross-process lock. `Err(())` = another process holds it;
/// `Ok(None)` = no lock possible here (e.g. read-only folder: the write
/// itself will report that).
fn os_lock(pak: &Path) -> Result<Option<(std::fs::File, std::path::PathBuf)>, ()> {
    let p = lock_path(pak);
    let f = match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&p)
    {
        Ok(f) => f,
        Err(e) => {
            dlog!("  lock file {} not created: {e}", p.display());
            return Ok(None);
        }
    };
    match f.try_lock() {
        Ok(()) => Ok(Some((f, p))),
        Err(std::fs::TryLockError::WouldBlock) => Err(()),
        Err(std::fs::TryLockError::Error(e)) => {
            dlog!("  lock on {} failed: {e}", p.display());
            Ok(None)
        }
    }
}

/// Claims `path` for writing; `None` if another operation (in this TC or
/// another process) is writing it.
pub fn acquire(path: &Path) -> Option<Guard> {
    let k = key(path);
    {
        let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        if b.contains(&k) {
            return None;
        }
        b.push(k.clone());
    }
    match os_lock(path) {
        Ok(lock) => Some(Guard { key: k, lock }),
        Err(()) => {
            dlog!("  {} is being changed by another process", path.display());
            let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
            b.retain(|x| *x != k);
            None
        }
    }
}

/// Whether another operation (here or in another process) is writing
/// `path` right now.
pub fn is_busy(path: &Path) -> bool {
    let k = key(path);
    if BUSY.lock().unwrap_or_else(|e| e.into_inner()).contains(&k) {
        return true;
    }
    let p = lock_path(path);
    if !p.exists() {
        return false;
    }
    // a lock file: held by a live writer, or left by a crash
    match std::fs::OpenOptions::new().write(true).open(&p) {
        Ok(f) => match f.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => true,
            _ => {
                let _ = f.unlock();
                drop(f);
                let _ = std::fs::remove_file(&p);
                false
            }
        },
        Err(_) => false,
    }
}
