//! Total Commander packer plugin (WCX, x64, Unicode) for Unreal Engine `.pak` files.
//!
//! Module map: `read` (open/list/extract + AES key resolution), `pack`
//! (PackFiles), `progress` (TC progress callback), `archive` (pak model),
//! `keys` + `dialog` (AES keys), `config`, `log`, `win` (Win32 helpers).
//! Every exported function runs inside [`guard`], so a Rust panic becomes an
//! error popup instead of taking Total Commander down.
#![allow(non_snake_case, clippy::missing_safety_doc)]

pub mod archive;
pub mod config;
pub mod dialog;
pub mod ffi;
pub mod keys;
pub mod log;
mod modify;
mod pack;
mod progress;
mod read;
pub mod view;
pub mod win;

#[cfg(test)]
mod tests;

use archive::Archive;
use ffi::*;
use read::*;
use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void};
use std::io;
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
    for w in &options.warnings {
        dlog!("options: {w}");
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

static PROCESS_DATA_A: AtomicUsize = AtomicUsize::new(0);

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
        hd.FileAttr = out.attr;
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
    progress::set_callback(proc_);
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
        let options = reload_options();
        let packed = win::from_wide_ptr(packed_file);
        let sub = win::from_wide_ptr(sub_path);
        let src = win::from_wide_ptr(src_path);
        let items = win::from_wide_multi(add_list);
        let exists = packed.as_deref().is_some_and(|p| Path::new(p).exists());
        let details = format!(
            "PackedFile: {packed:?} (exists: {exists})\nSubPath: {sub:?}\nSrcPath: {src:?}\nFlags: {}\nAddList ({} items):\n{}",
            pack_flags(flags),
            items.len(),
            describe_list(&items)
        );
        dlog!("PackFiles\n{details}");
        let (Some(packed), Some(src)) = (packed, src) else {
            return E_ECREATE;
        };
        pack_files(
            Path::new(&packed),
            sub.as_deref(),
            Path::new(&src),
            &items,
            flags,
            &options.pack,
        )
    })
}

fn pack_files(
    packed: &Path,
    sub: Option<&str>,
    src: &Path,
    items: &[String],
    flags: c_int,
    opts: &config::PackOptions,
) -> c_int {
    let plan = match pack::plan(src, sub, items, flags) {
        Ok(p) => p,
        Err(e) => {
            let msg = match e {
                pack::PlanError::Duplicates(d) => format!(
                    "Several files would get the same name in the pak (\"Also pack path names\" is off):\n{}",
                    d.join("\n")
                ),
                pack::PlanError::BadPath(p) => format!("Invalid path for a pak entry: {p}"),
                pack::PlanError::Empty => "No files to pack (empty folders can't be stored in a pak).".into(),
            };
            dlog!("  -> refused: {msg}");
            show_warning(&msg);
            return E_EABORTED;
        }
    };
    if packed.exists() {
        let mode = read::view_mode_for(packed, options_view_mode());
        dlog!(
            "  existing pak: view={mode:?} compression={:?} level={:?} files={}",
            opts.compression,
            opts.level,
            plan.files.len()
        );
        let started = std::time::Instant::now();
        return match pack::add_to_existing(packed, &plan, mode, opts) {
            Ok(st) => {
                dlog!(
                    "  -> OK in {} ms: {} added ({} replaced), {} bytes in, pak now {} bytes{}",
                    started.elapsed().as_millis(),
                    st.added,
                    st.replaced,
                    st.input_bytes,
                    st.pak_bytes,
                    st.new_mount
                        .as_ref()
                        .map(|m| format!(", mount point now {m:?}"))
                        .unwrap_or_default()
                );
                if flags & pack::PK_PACK_MOVE_FILES != 0 {
                    pack::delete_sources(&plan);
                }
                0
            }
            Err(e) => report_pack_error(packed, e, "The pak was left unchanged."),
        };
    }
    dlog!(
        "  new pak: version={} mount={:?} compression={:?} block_size={} files={}",
        opts.version,
        opts.mount_point,
        opts.compression,
        opts.block_size,
        plan.files.len()
    );
    let started = std::time::Instant::now();
    match pack::write_new_pak(packed, &plan, opts) {
        Ok(stats) => {
            dlog!(
                "  -> OK in {} ms: {} files, {} -> {} bytes, {} compressed",
                started.elapsed().as_millis(),
                stats.files,
                stats.input_bytes,
                stats.pak_bytes,
                stats.compressed_entries
            );
            if flags & pack::PK_PACK_MOVE_FILES != 0 {
                pack::delete_sources(&plan);
            }
            0
        }
        Err(e) => report_pack_error(packed, e, "No pak was created."),
    }
}

/// Current `[Options] MountPoint` (view mode used for paks not listed yet).
fn options_view_mode() -> config::MountPointMode {
    config::load_options().mount_point
}

/// Logs and shows a packing error; returns the TC code.
fn report_pack_error(packed: &Path, e: pack::PackError, outcome: &str) -> c_int {
    let code = e.code();
    dlog!("  -> {} ({e})", code_name(code));
    match e {
        pack::PackError::Aborted => code,
        pack::PackError::User(msg, _) => {
            show_warning(&msg);
            code
        }
        other => {
            // Our message is more precise than TC's generic one:
            // show it and keep TC quiet.
            show_error(&format!(
                "{}\n\nThe operation failed: {other}\n{outcome}",
                packed.display()
            ));
            E_EABORTED
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn DeleteFilesW(
    packed_file: *const u16,
    delete_list: *const u16,
) -> c_int {
    guard(E_NOT_SUPPORTED, "DeleteFilesW", || {
        init();
        let options = reload_options();
        let items = win::from_wide_multi(delete_list);
        let Some(packed) = win::from_wide_ptr(packed_file) else {
            return E_EOPEN;
        };
        let packed = PathBuf::from(packed);
        let mode = read::view_mode_for(&packed, options.mount_point);
        dlog!(
            "DeleteFiles {} view={mode:?}\n{}",
            packed.display(),
            describe_list(&items)
        );
        let started = std::time::Instant::now();
        match pack::delete_entries(&packed, &items, mode) {
            Ok(st) => {
                dlog!(
                    "  -> OK in {} ms: {} entries removed, pak now {} bytes",
                    started.elapsed().as_millis(),
                    st.removed,
                    st.pak_bytes
                );
                0
            }
            Err(e) => report_pack_error(&packed, e, "The pak was left unchanged."),
        }
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
        hd.FileAttr = out.attr;
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
        hd.FileAttr = out.attr;
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
