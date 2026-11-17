//! Total Commander packer plugin (WCX, x64, Unicode) for Unreal Engine `.pak` files.
//!
//! Read-only skeleton: list, extract, test. Every exported function runs
//! inside [`guard`], so a Rust panic becomes an error popup instead of
//! taking Total Commander down.
#![allow(non_snake_case, clippy::missing_safety_doc)]

pub mod archive;
pub mod config;
pub mod dialog;
pub mod ffi;
pub mod keys;
pub mod log;
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
            dlog!("PANIC in {func}: {msg}");
            show_error(&format!(
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
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if log::enabled() {
                log::write(&format!("panic: {info}"));
            } else if cfg!(test) {
                default_hook(info);
            }
        }));
    });
    reload_options();
}

/// Re-reads `uepak.ini` (so DebugLog can be toggled without restarting TC).
fn reload_options() -> config::Options {
    let options = config::load_options();
    let was_enabled = log::enabled();
    log::configure(options.debug_log);
    if options.debug_log && !was_enabled {
        log_environment(&options);
    }
    options
}

fn log_environment(options: &config::Options) {
    dlog!("plugin: {:?}", win::module_path());
    dlog!("settings dir: {:?}", config::settings_dir());
    dlog!("options: {options:?}");
    let deps = config::deps_dir();
    let listing = deps
        .as_ref()
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "<missing>".into());
    dlog!("deps dir: {deps:?} contains: [{listing}]");
    dlog!("oodle search paths: {:?}", oodle_loader::search_paths());
}

/// Shows a warning, appending where the debug log went (copying it to the
/// clipboard first if it is in memory).
fn show_warning(text: &str) {
    match log::on_error() {
        Some(note) => win::warn(&format!("{text}\n\n{note}")),
        None => win::warn(text),
    }
}

fn show_error(text: &str) {
    match log::on_error() {
        Some(note) => win::error(&format!("{text}\n\n{note}")),
        None => win::error(text),
    }
}

fn code_name(code: c_int) -> &'static str {
    match code {
        0 => "OK",
        E_END_ARCHIVE => "E_END_ARCHIVE",
        E_NO_MEMORY => "E_NO_MEMORY",
        E_BAD_DATA => "E_BAD_DATA",
        E_BAD_ARCHIVE => "E_BAD_ARCHIVE",
        E_UNKNOWN_FORMAT => "E_UNKNOWN_FORMAT",
        E_EOPEN => "E_EOPEN",
        E_ECREATE => "E_ECREATE",
        E_ECLOSE => "E_ECLOSE",
        E_EREAD => "E_EREAD",
        E_EWRITE => "E_EWRITE",
        E_EABORTED => "E_EABORTED",
        E_NOT_SUPPORTED => "E_NOT_SUPPORTED",
        _ => "?",
    }
}

fn op_name(op: c_int) -> &'static str {
    match op {
        PK_SKIP => "SKIP",
        PK_TEST => "TEST",
        PK_EXTRACT => "EXTRACT",
        _ => "?",
    }
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
        show_warning(&oodle_missing_text(archive, detail));
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

/// Paks whose key prompt was cancelled, and when. TC opens a pak several
/// times per operation (list, then extract); don't ask again right away.
static DECLINED: Mutex<Vec<(PathBuf, std::time::Instant)>> = Mutex::new(Vec::new());
const DECLINE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

fn recently_declined(path: &Path) -> bool {
    let mut d = DECLINED.lock().unwrap_or_else(|e| e.into_inner());
    d.retain(|(_, t)| t.elapsed() < DECLINE_GRACE);
    d.iter().any(|(p, _)| p == path)
}

fn mark_declined(path: &Path) {
    let mut d = DECLINED.lock().unwrap_or_else(|e| e.into_inner());
    d.push((path.to_path_buf(), std::time::Instant::now()));
}

fn pak_guid(path: &Path) -> String {
    archive::sniff_footer(path)
        .and_then(|f| f.key_guid)
        .unwrap_or_else(|| keys::ZERO_GUID.to_string())
}

fn save_key(folder: &Path, guid: &str, key: &keys::AesKey, persist: bool) {
    match keys::remember(folder, guid, key, persist) {
        Ok(()) => dlog!(
            "  key remembered for {} ({})",
            folder.display(),
            if persist { "saved" } else { "this session" }
        ),
        Err(e) => {
            dlog!("  could not save key: {e}");
            show_warning(&format!(
                "The key works but could not be saved to {}:\n{e}\n\nIt is kept until Total Commander is closed.",
                keys::KEYS_NAME
            ));
        }
    }
}

/// Opens a pak, resolving an AES key if its index is encrypted: remembered
/// keys first (SPEC §3.5), then the key prompt until a key matches or the
/// user cancels.
fn open_with_keys(path: &Path, options: &config::Options) -> Result<Archive, OpenError> {
    match Archive::open(path, options, None) {
        Err(OpenError::Encrypted) => {}
        other => return other,
    }
    let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let guid = pak_guid(path);
    dlog!("  index encrypted, key guid {guid}; looking for a key");
    for (key, source, from) in keys::candidates(&folder, &guid) {
        match Archive::open(path, options, Some(&key)) {
            Ok(arc) => {
                dlog!("  key from {source:?} record ({from}) matches");
                if source == keys::Source::Guid {
                    save_key(&folder, &guid, &key, true);
                }
                return Ok(arc);
            }
            Err(OpenError::WrongKey) => {
                dlog!("  key from {source:?} record ({from}) does not match")
            }
            Err(e) => return Err(e),
        }
    }
    if recently_declined(path) {
        dlog!("  key prompt was cancelled moments ago; not asking again");
        return Err(OpenError::Cancelled);
    }
    let mut error: Option<String> = None;
    loop {
        let answer = dialog::prompt_key(&dialog::KeyPrompt {
            pak: path,
            guid: &guid,
            error: error.as_deref(),
        });
        let Some(answer) = answer else {
            dlog!("  key prompt cancelled");
            mark_declined(path);
            return Err(OpenError::Cancelled);
        };
        let key = match keys::AesKey::parse(&answer.text) {
            Ok(k) => k,
            Err(msg) => {
                dlog!("  entered key has an invalid format");
                error = Some(msg.to_string());
                continue;
            }
        };
        match Archive::open(path, options, Some(&key)) {
            Ok(arc) => {
                dlog!("  entered key matches");
                save_key(&folder, &guid, &key, answer.remember);
                return Ok(arc);
            }
            Err(OpenError::WrongKey) => {
                dlog!("  entered key does not match");
                error = Some("This key does not match this pak.".into());
            }
            Err(e) => return Err(e),
        }
    }
}

/// For a pak with a plain index but encrypted entries: find a key before
/// extracting one. Entries can't be validated up front, so the first
/// remembered key is used, otherwise the user is asked once per handle.
fn ensure_entry_key(arc: &mut Archive) -> Result<(), c_int> {
    if arc.key.is_some() {
        return Ok(());
    }
    if arc.key_declined {
        return Err(E_EABORTED);
    }
    let path = arc.path.clone();
    let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let guid = pak_guid(&path);
    let key = match keys::candidates(&folder, &guid).into_iter().next() {
        Some((k, source, from)) => {
            dlog!("  encrypted entry: using key from {source:?} record ({from})");
            k
        }
        None => {
            let mut error: Option<String> = None;
            loop {
                let Some(answer) = dialog::prompt_key(&dialog::KeyPrompt {
                    pak: &path,
                    guid: &guid,
                    error: error.as_deref(),
                }) else {
                    arc.key_declined = true;
                    return Err(E_EABORTED);
                };
                match keys::AesKey::parse(&answer.text) {
                    Ok(k) => {
                        save_key(&folder, &guid, &k, answer.remember);
                        break k;
                    }
                    Err(msg) => error = Some(msg.to_string()),
                }
            }
        }
    };
    arc.set_key(&key).map_err(|e| {
        dlog!("  re-reading index with key failed: {e}");
        E_BAD_ARCHIVE
    })
}

fn open_impl(name: Option<String>, mode: c_int, result: &mut c_int) -> Handle {
    init();
    let Some(name) = name else {
        *result = E_EOPEN;
        return std::ptr::null_mut();
    };
    let path = PathBuf::from(name);
    let options = reload_options();
    dlog!(
        "OpenArchive {} mode={}",
        path.display(),
        if mode == PK_OM_LIST {
            "LIST"
        } else {
            "EXTRACT"
        }
    );
    if log::enabled() {
        match archive::sniff_footer(&path) {
            Some(f) => dlog!("  footer: {f}"),
            None => dlog!("  footer: no PAK magic in the last 512 bytes"),
        }
    }
    let started = std::time::Instant::now();
    match open_with_keys(&path, &options) {
        Ok(arc) => {
            dlog!(
                "  opened in {} ms: version={:?} mount={:?} entries={} skipped_unsafe={} compression={:?} encrypted_index={}",
                started.elapsed().as_millis(),
                arc.pak().version(),
                arc.pak().mount_point(),
                arc.items.len(),
                arc.pak().files().len() - arc.items.len(),
                arc.pak().used_compression(),
                arc.pak().encrypted_index(),
            );
            if arc.uses_oodle() {
                match oodle_loader::oodle() {
                    Ok(o) => dlog!("  oodle: loaded {}", o.path().display()),
                    Err(e) => {
                        dlog!("  oodle: NOT available: {e}");
                        if mode == PK_OM_LIST {
                            warn_oodle_once(&path, &e.to_string());
                        }
                    }
                }
            }
            *result = 0;
            Box::into_raw(Box::new(arc)) as Handle
        }
        Err(e) => {
            dlog!("  open failed: {e}");
            *result = match &e {
                OpenError::Io(io) if io.kind() == io::ErrorKind::NotFound => E_EOPEN,
                OpenError::Io(io) if io.kind() == io::ErrorKind::PermissionDenied => E_EOPEN,
                OpenError::Io(_) => E_EREAD,
                OpenError::Encrypted | OpenError::WrongKey => E_NOT_SUPPORTED,
                OpenError::Cancelled => E_EABORTED,
                OpenError::Unsupported(v) => {
                    show_warning(&format!(
                        "{}\n\nPAK version {v} is newer than this plugin supports.",
                        path.display()
                    ));
                    E_UNKNOWN_FORMAT
                }
                OpenError::Pak(repak::Error::Magic(_))
                | OpenError::Pak(repak::Error::UnsupportedOrEncrypted(_)) => E_UNKNOWN_FORMAT,
                OpenError::Pak(_) => E_BAD_ARCHIVE,
            };
            dlog!("  -> {}", code_name(*result));
            if !matches!(e, OpenError::Cancelled | OpenError::Unsupported(_)) {
                log::on_error();
            }
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
    let Some(item) = arc.next_item() else {
        dlog!(
            "ReadHeader: end of archive after {} entries",
            arc.items.len()
        );
        return None;
    };
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
    if let Some(item) = arc.current_item() {
        dlog!(
            "ProcessFile {} {} packed={} unpacked={} compression={:?} encrypted={} dest={:?}",
            op_name(op),
            item.pak_path,
            item.info.compressed_size,
            item.info.uncompressed_size,
            item.info.compression,
            item.info.encrypted,
            dest
        );
    }
    if arc.current_item().is_some_and(|i| i.info.encrypted) {
        if let Err(code) = ensure_entry_key(arc) {
            dlog!("  -> {} (no key for encrypted entry)", code_name(code));
            return code;
        }
    }
    let started = std::time::Instant::now();
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
    if let Err(e) = &result {
        dlog!("  failed after {} ms: {e:?}", started.elapsed().as_millis());
    }
    let code = match result {
        Ok(()) => 0,
        Err(ExtractError::Aborted) => E_EABORTED,
        Err(ExtractError::Create(_)) => E_ECREATE,
        Err(ExtractError::Write(_)) => E_EWRITE,
        Err(ExtractError::Read(repak::Error::OodleFailed(e))) => {
            if !arc.oodle_warned {
                arc.oodle_warned = true;
                show_warning(&oodle_missing_text(&arc.path, &e.to_string()));
            }
            E_NOT_SUPPORTED
        }
        Err(ExtractError::Read(repak::Error::Encrypted | repak::Error::Encryption)) => {
            E_NOT_SUPPORTED
        }
        Err(ExtractError::Read(_)) if arc.current_item().is_some_and(|i| i.info.encrypted) => {
            dlog!("  decrypting failed: the remembered key may not match this pak's entries");
            E_BAD_DATA
        }
        Err(ExtractError::Read(_)) => E_BAD_DATA,
    };
    if code == 0 {
        dlog!("  ok in {} ms", started.elapsed().as_millis());
    } else {
        dlog!("  -> {}", code_name(code));
        if code != E_EABORTED {
            log::on_error();
        }
    }
    code
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
            let arc = Box::from_raw(h as *mut Archive);
            dlog!("CloseArchive {}", arc.path.display());
            drop(arc);
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
    // NEW/MODIFY/DELETE/OPTIONS are advertised so TC routes Alt+F5, F5-into-pak,
    // F8 and "Configure" to the plugin; for now those calls are only recorded
    // (see `report_unimplemented`). TC caches caps: reinstall after changes.
    PK_CAPS_NEW
        | PK_CAPS_MODIFY
        | PK_CAPS_MULTIPLE
        | PK_CAPS_DELETE
        | PK_CAPS_OPTIONS
        | PK_CAPS_BY_CONTENT
}

/// Records a call to a not-yet-implemented operation: always copies the
/// details to the clipboard (so they can be pasted without DebugLog), writes
/// them to the debug log if enabled, and tells the user.
fn report_unimplemented(what: &str, details: &str) {
    dlog!("{what} (not implemented yet):\n{details}");
    let copied = win::copy_to_clipboard(&format!("uepak: {what}\n{details}"));
    let preview: String = details.lines().take(25).collect::<Vec<_>>().join("\n");
    let more = details.lines().count().saturating_sub(25);
    win::warn(&format!(
        "{what} is not implemented yet.\n\nWhat Total Commander passed:\n{preview}{}\n\n{}",
        if more > 0 {
            format!("\n... ({more} more lines)")
        } else {
            String::new()
        },
        if copied {
            "Full details copied to the clipboard."
        } else {
            "Could not copy the details to the clipboard."
        }
    ));
}

fn describe_list(items: &[String]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, s)| format!("  [{i}] {s:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pack_flags(flags: c_int) -> String {
    let mut v = vec![];
    if flags & 1 != 0 {
        v.push("MOVE_FILES");
    }
    if flags & 2 != 0 {
        v.push("SAVE_PATHS");
    }
    if flags & 4 != 0 {
        v.push("ENCRYPT");
    }
    format!("{flags} [{}]", v.join("|"))
}

#[no_mangle]
pub unsafe extern "system" fn PackFilesW(
    packed_file: *const u16,
    sub_path: *const u16,
    src_path: *const u16,
    add_list: *const u16,
    flags: c_int,
) -> c_int {
    guard(E_NOT_SUPPORTED, "PackFilesW", || {
        init();
        let packed = win::from_wide_ptr(packed_file);
        let exists = packed.as_deref().is_some_and(|p| Path::new(p).exists());
        let items = win::from_wide_multi(add_list);
        let details = format!(
            "PackedFile: {packed:?} (exists: {exists})\nSubPath: {:?}\nSrcPath: {:?}\nFlags: {}\nAddList ({} items):\n{}",
            win::from_wide_ptr(sub_path),
            win::from_wide_ptr(src_path),
            pack_flags(flags),
            items.len(),
            describe_list(&items)
        );
        report_unimplemented("PackFiles", &details);
        E_NOT_SUPPORTED
    })
}

#[no_mangle]
pub unsafe extern "system" fn DeleteFilesW(
    packed_file: *const u16,
    delete_list: *const u16,
) -> c_int {
    guard(E_NOT_SUPPORTED, "DeleteFilesW", || {
        init();
        let items = win::from_wide_multi(delete_list);
        let details = format!(
            "PackedFile: {:?}\nDeleteList ({} items):\n{}",
            win::from_wide_ptr(packed_file),
            items.len(),
            describe_list(&items)
        );
        report_unimplemented("DeleteFiles", &details);
        E_NOT_SUPPORTED
    })
}

#[no_mangle]
pub extern "system" fn ConfigurePacker(parent: *mut c_void, dll_instance: *mut c_void) {
    guard((), "ConfigurePacker", || {
        init();
        report_unimplemented(
            "ConfigurePacker (settings dialog)",
            &format!("Parent: {parent:?}\nDllInstance: {dll_instance:?}"),
        );
    })
}

#[no_mangle]
pub extern "system" fn PackFiles(
    _packed_file: *const c_char,
    _sub_path: *const c_char,
    _src_path: *const c_char,
    _add_list: *const c_char,
    _flags: c_int,
) -> c_int {
    dlog!("PackFiles (ANSI) called; only the Unicode version is supported");
    E_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn DeleteFiles(
    _packed_file: *const c_char,
    _delete_list: *const c_char,
) -> c_int {
    dlog!("DeleteFiles (ANSI) called; only the Unicode version is supported");
    E_NOT_SUPPORTED
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
