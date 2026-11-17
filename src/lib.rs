//! Total Commander packer plugin (WCX, x64, Unicode) for Unreal Engine `.pak` files.
//!
//! Read-only skeleton: list, extract, test. Every exported function runs
//! inside [`guard`], so a Rust panic becomes an error popup instead of
//! taking Total Commander down.
#![allow(non_snake_case, clippy::missing_safety_doc)]

pub mod archive;
pub mod config;
pub mod ffi;
pub mod win;

#[cfg(test)]
mod tests;

use archive::{Archive, ExtractError, Finish, OpenError, UserAborted};
use ffi::*;
use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void};
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, Once};

type Handle = *mut c_void;

// ---------------------------------------------------------------------------
// Infrastructure

/// Runs `f`, turning a panic into an error popup and `fallback`.
fn guard<T>(fallback: T, func: &str, f: impl FnOnce() -> T) -> T {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            win::error(&format!(
                "Internal error in {func}:\n{msg}\n\nThe operation was cancelled."
            ));
            fallback
        }
    }
}

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| {
        if let Some(deps) = config::deps_dir() {
            oodle_loader::set_search_paths([deps]);
        }
    });
}

static PROCESS_DATA_W: AtomicUsize = AtomicUsize::new(0);
static PROCESS_DATA_A: AtomicUsize = AtomicUsize::new(0);

/// Paths we already warned about (missing Oodle), to avoid repeat popups.
static OODLE_WARNED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

fn oodle_missing_text(archive: &Path, detail: &str) -> String {
    let deps = config::deps_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "<plugin folder>\\deps".into());
    format!(
        "{}\n\nThis pak uses Oodle compression, but the Oodle library was not found.\n\
         Copy oo2core_N_win64.dll (usually found in the game's folder, e.g. \
         Engine\\Binaries\\ThirdParty\\Oodle or next to the game .exe) into:\n{deps}\n\n\
         Listing works; extracting Oodle-compressed files will fail until then.\n\n({detail})",
        archive.display()
    )
}

fn warn_oodle_once(archive: &Path, detail: &str) {
    let mut guard = OODLE_WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if guard
        .get_or_insert_with(HashSet::new)
        .insert(archive.to_path_buf())
    {
        drop(guard);
        win::warn(&oodle_missing_text(archive, detail));
    }
}

/// Progress reporting + cancellation via TC's ProcessDataProc.
struct Progress<W> {
    inner: W,
    name: Vec<u16>,
}

impl<W: Write> Write for Progress<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        let cb = PROCESS_DATA_W.load(Ordering::Relaxed);
        if cb != 0 && n > 0 {
            let cb: unsafe extern "system" fn(*mut u16, c_int) -> c_int =
                unsafe { std::mem::transmute(cb) };
            if unsafe { cb(self.name.as_mut_ptr(), n.min(c_int::MAX as usize) as c_int) } == 0 {
                return Err(io::Error::other(UserAborted));
            }
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Finish + Write> Finish for Progress<W> {
    fn finish(&mut self) -> io::Result<()> {
        self.inner.finish()
    }
}

impl Finish for io::Sink {
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

unsafe fn archive<'a>(h: Handle) -> Option<&'a mut Archive> {
    (h as *mut Archive).as_mut()
}

// ---------------------------------------------------------------------------
// Core logic (Unicode)

fn open_impl(name: Option<String>, mode: c_int, result: &mut c_int) -> Handle {
    init();
    let Some(name) = name else {
        *result = E_EOPEN;
        return std::ptr::null_mut();
    };
    let path = PathBuf::from(name);
    let options = config::load_options();
    match Archive::open(&path, &options) {
        Ok(arc) => {
            if mode == PK_OM_LIST && arc.uses_oodle() {
                if let Err(e) = oodle_loader::oodle() {
                    warn_oodle_once(&path, &e.to_string());
                }
            }
            *result = 0;
            Box::into_raw(Box::new(arc)) as Handle
        }
        Err(e) => {
            *result = match &e {
                OpenError::Io(io) if io.kind() == io::ErrorKind::NotFound => E_EOPEN,
                OpenError::Io(io) if io.kind() == io::ErrorKind::PermissionDenied => E_EOPEN,
                OpenError::Io(_) => E_EREAD,
                OpenError::Encrypted => {
                    win::warn(&format!(
                        "{}\n\nThe pak index is AES-encrypted. Key support is not implemented \
                         in this build yet.",
                        path.display()
                    ));
                    E_NOT_SUPPORTED
                }
                OpenError::Pak(repak::Error::Magic(_))
                | OpenError::Pak(repak::Error::UnsupportedOrEncrypted(_)) => E_UNKNOWN_FORMAT,
                OpenError::Pak(_) => E_BAD_ARCHIVE,
            };
            std::ptr::null_mut()
        }
    }
}

struct HeaderOut {
    name: String,
    pack: u64,
    unpack: u64,
    time: c_int,
}

fn next_header(arc: &mut Archive) -> Option<HeaderOut> {
    let time = arc.mtime.map(win::dos_time).unwrap_or(0);
    let item = arc.next_item()?;
    Some(HeaderOut {
        name: item.display.clone(),
        pack: item.info.compressed_size,
        unpack: item.info.uncompressed_size,
        time,
    })
}

fn process_impl(arc: &mut Archive, op: c_int, dest: Option<PathBuf>) -> c_int {
    if op == PK_SKIP {
        return 0;
    }
    let display = arc
        .current_item()
        .map(|i| i.display.clone())
        .unwrap_or_default();
    let result = match op {
        PK_TEST => {
            let mut out = Progress {
                inner: io::sink(),
                name: win::wide(&display),
            };
            arc.read_current(&mut out).map_err(ExtractError::from_repak)
        }
        PK_EXTRACT => {
            let Some(dest) = dest else { return E_ECREATE };
            let name = win::wide(&dest.to_string_lossy());
            arc.extract_current(&dest, |w| Progress { inner: w, name })
        }
        _ => return E_NOT_SUPPORTED,
    };
    match result {
        Ok(()) => 0,
        Err(ExtractError::Aborted) => E_EABORTED,
        Err(ExtractError::Create(_)) => E_ECREATE,
        Err(ExtractError::Write(_)) => E_EWRITE,
        Err(ExtractError::Read(repak::Error::OodleFailed(e))) => {
            if !arc.oodle_warned {
                arc.oodle_warned = true;
                win::warn(&oodle_missing_text(&arc.path, &e.to_string()));
            }
            E_NOT_SUPPORTED
        }
        Err(ExtractError::Read(repak::Error::Encrypted | repak::Error::Encryption)) => {
            E_NOT_SUPPORTED
        }
        Err(ExtractError::Read(_)) => E_BAD_DATA,
    }
}

fn join_dest(path: Option<String>, name: Option<String>) -> Option<PathBuf> {
    let name = name?;
    match path {
        Some(dir) if !dir.is_empty() => Some(Path::new(&dir).join(name)),
        _ => Some(PathBuf::from(name)),
    }
}

// ---------------------------------------------------------------------------
// Exports: Unicode

#[no_mangle]
pub unsafe extern "system" fn OpenArchiveW(data: *mut OpenArchiveDataW) -> Handle {
    guard(std::ptr::null_mut(), "OpenArchiveW", || {
        let Some(data) = data.as_mut() else {
            return std::ptr::null_mut();
        };
        let name = win::from_wide_ptr(data.ArcName);
        open_impl(name, data.OpenMode, &mut data.OpenResult)
    })
}

#[no_mangle]
pub unsafe extern "system" fn ReadHeaderExW(h: Handle, hd: *mut HeaderDataExW) -> c_int {
    guard(E_BAD_ARCHIVE, "ReadHeaderExW", || {
        let (Some(arc), Some(hd)) = (archive(h), hd.as_mut()) else {
            return E_BAD_ARCHIVE;
        };
        let Some(out) = next_header(arc) else {
            return E_END_ARCHIVE;
        };
        std::ptr::write_bytes(hd as *mut HeaderDataExW, 0, 1);
        win::copy_wide(&mut hd.ArcName, &arc.path.to_string_lossy());
        win::copy_wide(&mut hd.FileName, &out.name);
        hd.PackSize = out.pack as u32;
        hd.PackSizeHigh = (out.pack >> 32) as u32;
        hd.UnpSize = out.unpack as u32;
        hd.UnpSizeHigh = (out.unpack >> 32) as u32;
        hd.FileTime = out.time;
        hd.FileAttr = FILE_ATTR_ARCHIVE;
        0
    })
}

#[no_mangle]
pub unsafe extern "system" fn ProcessFileW(
    h: Handle,
    op: c_int,
    dest_path: *const u16,
    dest_name: *const u16,
) -> c_int {
    guard(E_BAD_DATA, "ProcessFileW", || {
        let Some(arc) = archive(h) else {
            return E_BAD_ARCHIVE;
        };
        let dest = dest_path_w(dest_path, dest_name);
        process_impl(arc, op, dest)
    })
}

unsafe fn dest_path_w(dir: *const u16, name: *const u16) -> Option<PathBuf> {
    join_dest(win::from_wide_ptr(dir), win::from_wide_ptr(name))
}

#[no_mangle]
pub unsafe extern "system" fn CloseArchive(h: Handle) -> c_int {
    guard(E_ECLOSE, "CloseArchive", || {
        if !h.is_null() {
            drop(Box::from_raw(h as *mut Archive));
        }
        0
    })
}

#[no_mangle]
pub extern "system" fn SetProcessDataProcW(_h: Handle, proc_: ProcessDataProcW) {
    PROCESS_DATA_W.store(proc_.map_or(0, |f| f as usize), Ordering::Relaxed);
}

#[no_mangle]
pub extern "system" fn SetChangeVolProcW(_h: Handle, _proc: ChangeVolProcW) {}

#[no_mangle]
pub extern "system" fn GetPackerCaps() -> c_int {
    PK_CAPS_MULTIPLE | PK_CAPS_BY_CONTENT
}

#[no_mangle]
pub unsafe extern "system" fn CanYouHandleThisFileW(name: *const u16) -> c_int {
    guard(0, "CanYouHandleThisFileW", || {
        win::from_wide_ptr(name).is_some_and(|n| archive::looks_like_pak(Path::new(&n))) as c_int
    })
}

#[no_mangle]
pub unsafe extern "system" fn PackSetDefaultParams(dps: *const PackDefaultParamStruct) {
    guard((), "PackSetDefaultParams", || {
        if let Some(dps) = dps.as_ref() {
            if let Some(ini) = win::from_ansi_ptr(dps.DefaultIniName.as_ptr()) {
                config::set_tc_ini_name(Path::new(&ini));
            }
        }
        init();
    })
}

// ---------------------------------------------------------------------------
// Exports: ANSI fallbacks (64-bit TC uses the W versions when present)

#[no_mangle]
pub unsafe extern "system" fn OpenArchive(data: *mut OpenArchiveData) -> Handle {
    guard(std::ptr::null_mut(), "OpenArchive", || {
        let Some(data) = data.as_mut() else {
            return std::ptr::null_mut();
        };
        let name = win::from_ansi_ptr(data.ArcName);
        open_impl(name, data.OpenMode, &mut data.OpenResult)
    })
}

#[no_mangle]
pub unsafe extern "system" fn ReadHeaderEx(h: Handle, hd: *mut HeaderDataEx) -> c_int {
    guard(E_BAD_ARCHIVE, "ReadHeaderEx", || {
        let (Some(arc), Some(hd)) = (archive(h), hd.as_mut()) else {
            return E_BAD_ARCHIVE;
        };
        let Some(out) = next_header(arc) else {
            return E_END_ARCHIVE;
        };
        std::ptr::write_bytes(hd as *mut HeaderDataEx, 0, 1);
        win::copy_ansi(&mut hd.ArcName, &arc.path.to_string_lossy());
        win::copy_ansi(&mut hd.FileName, &out.name);
        hd.PackSize = out.pack as u32;
        hd.PackSizeHigh = (out.pack >> 32) as u32;
        hd.UnpSize = out.unpack as u32;
        hd.UnpSizeHigh = (out.unpack >> 32) as u32;
        hd.FileTime = out.time;
        hd.FileAttr = FILE_ATTR_ARCHIVE;
        0
    })
}

#[no_mangle]
pub unsafe extern "system" fn ReadHeader(h: Handle, hd: *mut HeaderData) -> c_int {
    guard(E_BAD_ARCHIVE, "ReadHeader", || {
        let (Some(arc), Some(hd)) = (archive(h), hd.as_mut()) else {
            return E_BAD_ARCHIVE;
        };
        let Some(out) = next_header(arc) else {
            return E_END_ARCHIVE;
        };
        std::ptr::write_bytes(hd as *mut HeaderData, 0, 1);
        win::copy_ansi(&mut hd.ArcName, &arc.path.to_string_lossy());
        win::copy_ansi(&mut hd.FileName, &out.name);
        hd.PackSize = out.pack.min(i32::MAX as u64) as i32;
        hd.UnpSize = out.unpack.min(i32::MAX as u64) as i32;
        hd.FileTime = out.time;
        hd.FileAttr = FILE_ATTR_ARCHIVE;
        0
    })
}

#[no_mangle]
pub unsafe extern "system" fn ProcessFile(
    h: Handle,
    op: c_int,
    dest_path: *const c_char,
    dest_name: *const c_char,
) -> c_int {
    guard(E_BAD_DATA, "ProcessFile", || {
        let Some(arc) = archive(h) else {
            return E_BAD_ARCHIVE;
        };
        let dest = join_dest(win::from_ansi_ptr(dest_path), win::from_ansi_ptr(dest_name));
        // Progress callbacks only go through the W proc; the ANSI one is kept for completeness.
        process_impl(arc, op, dest)
    })
}

#[no_mangle]
pub extern "system" fn SetProcessDataProc(_h: Handle, proc_: ProcessDataProcA) {
    PROCESS_DATA_A.store(proc_.map_or(0, |f| f as usize), Ordering::Relaxed);
}

#[no_mangle]
pub extern "system" fn SetChangeVolProc(_h: Handle, _proc: ChangeVolProcA) {}

#[no_mangle]
pub unsafe extern "system" fn CanYouHandleThisFile(name: *const c_char) -> c_int {
    guard(0, "CanYouHandleThisFile", || {
        win::from_ansi_ptr(name).is_some_and(|n| archive::looks_like_pak(Path::new(&n))) as c_int
    })
}
