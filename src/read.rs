//! Reading: open (with AES key resolution), list, extract, test.

use super::*;
use crate::archive::{Archive, ExtractError, OpenError};

/// Paths we already warned about (missing Oodle), to avoid repeat popups.
pub(crate) static OODLE_WARNED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

pub(crate) fn oodle_missing_text(archive: &Path, detail: &str) -> String {
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

pub(crate) fn warn_oodle_once(archive: &Path, detail: &str) {
    let mut guard = OODLE_WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if guard
        .get_or_insert_with(HashSet::new)
        .insert(archive.to_path_buf())
    {
        drop(guard);
        show_warning(&oodle_missing_text(archive, detail));
    }
}

pub(crate) unsafe fn archive<'a>(h: Handle) -> Option<&'a mut Archive> {
    (h as *mut Archive).as_mut()
}

/// Paks whose key prompt was cancelled, and when. TC opens a pak several
/// times per operation (list, then extract); don't ask again right away.
pub(crate) static DECLINED: Mutex<Vec<(PathBuf, std::time::Instant)>> = Mutex::new(Vec::new());
pub(crate) const DECLINE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) fn recently_declined(path: &Path) -> bool {
    let mut d = DECLINED.lock().unwrap_or_else(|e| e.into_inner());
    d.retain(|(_, t)| t.elapsed() < DECLINE_GRACE);
    d.iter().any(|(p, _)| p == path)
}

pub(crate) fn mark_declined(path: &Path) {
    let mut d = DECLINED.lock().unwrap_or_else(|e| e.into_inner());
    d.push((path.to_path_buf(), std::time::Instant::now()));
}

pub(crate) fn pak_guid(path: &Path) -> String {
    archive::sniff_footer(path)
        .and_then(|f| f.key_guid)
        .unwrap_or_else(|| keys::ZERO_GUID.to_string())
}

pub(crate) fn save_key(folder: &Path, guid: &str, key: &keys::AesKey, persist: bool) {
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
/// One key dialog at a time: background operations on several paks of the
/// same game would otherwise each ask. Whoever waits re-checks the remembered
/// keys afterwards and usually finds the key just entered.
static KEY_PROMPT: Mutex<()> = Mutex::new(());

pub(crate) fn open_with_keys(path: &Path, options: &config::Options) -> Result<Archive, OpenError> {
    match Archive::open(path, options, None) {
        Err(OpenError::Encrypted) => {}
        other => return other,
    }
    let _prompt = KEY_PROMPT.lock().unwrap_or_else(|e| e.into_inner());
    let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let guid = pak_guid(path);
    dlog!("  index encrypted, key guid {guid}; looking for a key");
    // a remembered key with this exact non-zero GUID that no longer fits
    // means the game most likely re-encrypted its paks with a new key
    let mut stale: Option<String> = None;
    for (key, source, from) in keys::candidates(&folder, &guid) {
        match Archive::open(path, options, Some(&key)) {
            Ok(arc) => {
                dlog!("  key from {source:?} record ({from}) matches");
                if source == keys::Source::Guid {
                    // session only: the next lookup finds the saved record by
                    // its GUID anyway, so the keys file isn't touched
                    save_key(&folder, &guid, &key, false);
                }
                return Ok(arc);
            }
            Err(OpenError::WrongKey) => {
                dlog!("  key from {source:?} record ({from}) does not match");
                if stale.is_none() {
                    stale = Some(from.clone());
                }
            }
            Err(e) => return Err(e),
        }
    }
    if recently_declined(path) {
        dlog!("  key prompt was cancelled moments ago; not asking again");
        return Err(OpenError::Cancelled);
    }
    let mut error: Option<String> = stale.map(|from| stale_key_message(&guid, &from));
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

/// Shown in the key dialog when a remembered key matched this pak's key GUID
/// (or folder) but doesn't decrypt it (SPEC §3.4).
pub(crate) fn stale_key_message(guid: &str, from: &str) -> String {
    if guid != keys::ZERO_GUID {
        format!(
            "The key saved for this key GUID doesn't match this pak. The GUID is the same, so \
             the pak was most likely re-encrypted with a new key by a game update. Enter the \
             new key. (Saved for: {from})"
        )
    } else {
        format!(
            "The key saved for this folder doesn't match this pak; the game may have been \
             updated with a new key. Enter the new key. (Saved for: {from})"
        )
    }
}

/// For a pak with a plain index but encrypted entries: find a key before
/// extracting one. Entries can't be validated up front, so the first
/// remembered key is used, otherwise the user is asked once per handle.
pub(crate) fn ensure_entry_key(arc: &mut Archive) -> Result<(), c_int> {
    if arc.key.is_some() {
        return Ok(());
    }
    let _prompt = KEY_PROMPT.lock().unwrap_or_else(|e| e.into_inner());
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

/// The AES key for changing an encrypted pak (SPEC §4.4).
pub(crate) struct WriteKey {
    pub key: Option<keys::AesKey>,
    /// The key was confirmed: by the index hash (encrypted index) or by
    /// decompressing an encrypted entry. Paks whose only encrypted entries
    /// are stored uncompressed can't be checked.
    pub checked: bool,
}

/// Finds the key for changing `path`: `key: None` for unencrypted paks.
/// Uses the same lookup and dialog as opening (§3.4). With `entries` (adding
/// files) a plain index with encrypted entries needs the key too; it is
/// checked by decompressing the smallest encrypted compressed entry, when
/// there is one. Deleting from such a pak needs no key.
pub(crate) fn key_for_writing(path: &Path, entries: bool) -> Result<WriteKey, OpenError> {
    let options = config::load_options();
    let mut arc = open_with_keys(path, &options)?;
    if let Some(key) = arc.key {
        return Ok(WriteKey {
            key: Some(key),
            checked: true,
        });
    }
    if !entries || !arc.pak().entries().any(|(_, e)| e.encrypted) {
        return Ok(WriteKey {
            key: None,
            checked: true,
        });
    }
    dlog!("  plain index, encrypted entries: looking for a key");
    if ensure_entry_key(&mut arc).is_err() {
        return Err(OpenError::Cancelled);
    }
    let key = arc.key;
    let probe = arc
        .pak()
        .entries()
        .filter(|(_, e)| e.encrypted && e.compression.is_some())
        .min_by_key(|(_, e)| e.compressed_size)
        .map(|(p, _)| p.to_string());
    let Some(probe) = probe else {
        dlog!("  no encrypted compressed entry: the key can't be checked");
        return Ok(WriteKey {
            key,
            checked: false,
        });
    };
    let file = std::fs::File::open(path).map_err(OpenError::Io)?;
    let mut r = std::io::BufReader::new(file);
    match arc.pak().read_file(&probe, &mut r, &mut std::io::sink()) {
        Ok(()) => {
            dlog!("  key checked by decompressing {probe}");
            Ok(WriteKey { key, checked: true })
        }
        Err(e @ (repak::Error::Oodle | repak::Error::OodleFailed(_))) => {
            dlog!("  key can't be checked on {probe}: {e}");
            Ok(WriteKey {
                key,
                checked: false,
            })
        }
        Err(e) => {
            dlog!("  key check failed on {probe}: {e}");
            Err(OpenError::WrongKey)
        }
    }
}

/// View mode each pak was last listed with (lowercased path -> mode), so
/// PackFiles/DeleteFiles map TC's view paths the same way (SPEC §3.2).
pub(crate) static VIEW_MODES: Mutex<Vec<(String, config::MountPointMode)>> = Mutex::new(Vec::new());

fn view_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

pub(crate) fn remember_view_mode(path: &Path, mode: config::MountPointMode) {
    let key = view_key(path);
    let mut v = VIEW_MODES.lock().unwrap_or_else(|e| e.into_inner());
    v.retain(|(k, _)| *k != key);
    v.push((key, mode));
    if v.len() > 256 {
        v.remove(0);
    }
}

/// The mode `path` was listed with, else `fallback` (current options).
pub(crate) fn view_mode_for(
    path: &Path,
    fallback: config::MountPointMode,
) -> config::MountPointMode {
    let key = view_key(path);
    VIEW_MODES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .find(|(k, _)| *k == key)
        .map(|(_, m)| *m)
        .unwrap_or(fallback)
}

/// An interrupted modification left `<pak>.uepak-undo`: offer to restore.
fn offer_undo_restore(path: &Path) {
    if !crate::modify::has_undo(path) {
        return;
    }
    let text = format!(
        "{}\n\nAn earlier change to this pak was interrupted (Total Commander or Windows \
         stopped while writing).\n\nYes: restore the pak to its state before that change.\n\
         No: keep the pak as it is now and delete the backup.",
        path.display()
    );
    if crate::dialog::confirm(&text) {
        match crate::modify::restore(path) {
            Ok(()) => dlog!("  undo: pak restored from backup"),
            Err(e) => {
                dlog!("  undo: restore failed: {e}");
                show_error(&format!("Restoring {} failed: {e}", path.display()));
            }
        }
    } else {
        let _ = crate::modify::discard_undo(path);
        dlog!("  undo: backup discarded, current state kept");
    }
}

pub(crate) fn open_impl(name: Option<String>, mode: c_int, result: &mut c_int) -> Handle {
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
    if crate::busy::is_busy(&path) {
        // a background operation is writing it: its index and undo file are
        // mid-change (never offer a restore now)
        dlog!("  -> refused: the pak is being changed by another operation");
        *result = E_EOPEN;
        return std::ptr::null_mut();
    }
    offer_undo_restore(&path);
    let started = std::time::Instant::now();
    match open_with_keys(&path, &options) {
        Ok(mut arc) => {
            if mode != PK_OM_LIST {
                // the Hidden/ReadOnly marker is only for the panel. TC still
                // gives the folders it creates the attributes from its panel
                // listing, so they are cleaned up after extraction too.
                arc.extract_marker = arc.mount_folder_attr;
                arc.mount_folder_attr = 0;
            }
            remember_view_mode(&path, options.mount_point);
            dlog!(
                "  opened in {} ms: version={:?} mount={:?} entries={} skipped_unsafe={} compression={:?} encrypted_index={}",
                started.elapsed().as_millis(),
                arc.pak().version(),
                arc.pak().mount_point(),
                arc.file_count(),
                arc.pak().files().len() - arc.file_count(),
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

pub(crate) struct HeaderOut {
    pub name: String,
    pub pack: u64,
    pub unpack: u64,
    pub time: c_int,
    pub attr: c_int,
}

pub(crate) fn next_header(arc: &mut Archive) -> Option<HeaderOut> {
    let time = arc.mtime.map(win::dos_time).unwrap_or(0);
    let mount_attr = arc.mount_folder_attr as c_int;
    let Some(item) = arc.next_item() else {
        let files = arc.file_count();
        dlog!(
            "ReadHeader: end of archive after {} entries ({} files)",
            arc.items.len(),
            files
        );
        return None;
    };
    let attr = if item.mount_dir {
        FILE_ATTR_DIRECTORY | mount_attr
    } else {
        FILE_ATTR_ARCHIVE
    };
    Some(HeaderOut {
        name: item.display.clone(),
        pack: item.info.compressed_size,
        unpack: item.info.uncompressed_size,
        time,
        attr,
    })
}

/// TC creates the mount folders it extracts with the attributes from its
/// panel listing (the Hidden/ReadOnly marker, SPEC §3.2): seen in TC 11 under
/// Wine, where the folder is already Hidden when the first file is written.
/// Removes the marker from those folders, once per extraction, `dest` being
/// the first extracted file. TC may drop leading folders (extracting from
/// inside a mount folder), so the folders are matched from the end of the
/// entry's path; only folders created during this extraction are changed.
fn clear_mount_markers(arc: &mut Archive, dest: &Path) {
    let marker = arc.extract_marker;
    if marker == 0 {
        return;
    }
    arc.extract_marker = 0;
    let Some(display) = arc.current_item().map(|i| i.display.clone()) else {
        return;
    };
    let mounts: Vec<&str> = arc
        .items
        .iter()
        .filter(|i| i.mount_dir)
        .map(|i| i.display.as_str())
        .collect();
    // entry folders, deepest first, paired with dest's ancestors
    let comps: Vec<&str> = display.split('\\').collect();
    let mut folder = dest.parent();
    for n in (1..comps.len()).rev() {
        let Some(f) = folder else { break };
        let prefix = comps[..n].join("\\");
        let same_name = f
            .file_name()
            .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(comps[n - 1]));
        if !same_name {
            break; // above the extraction's target folder
        }
        let created_now = std::fs::metadata(f)
            .and_then(|m| m.created())
            .is_ok_and(|t| t >= arc.opened_at - std::time::Duration::from_secs(2));
        if mounts.iter().any(|m| m.eq_ignore_ascii_case(&prefix))
            && created_now
            && win::clear_attrs(f, marker)
        {
            dlog!("  mount folder marker removed: {}", f.display());
        }
        folder = f.parent();
    }
}

pub(crate) fn process_impl(arc: &mut Archive, op: c_int, dest: Option<PathBuf>) -> c_int {
    if op == PK_SKIP {
        return 0;
    }
    // Mount folders carry no data: just create the folder.
    if let Some(item) = arc.current_item().filter(|i| i.mount_dir) {
        dlog!("ProcessFile {} mount folder {}", op_name(op), item.display);
        if op == PK_EXTRACT {
            if let Some(dest) = &dest {
                if let Err(e) = std::fs::create_dir_all(dest) {
                    dlog!("  create folder failed: {e}");
                    return E_ECREATE;
                }
            }
        }
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
            let mut out = progress::Progress {
                inner: io::sink(),
                name: win::wide(&display),
            };
            // PK_TEST also checks each entry's SHA-1 (SPEC §3.3)
            arc.read_current(&mut out, true)
                .map_err(ExtractError::from_repak)
        }
        PK_EXTRACT => {
            let Some(dest) = dest else { return E_ECREATE };
            let name = win::wide(&dest.to_string_lossy());
            let r = arc.extract_current(&dest, |w| progress::Progress { inner: w, name });
            if r.is_ok() {
                clear_mount_markers(arc, &dest);
            }
            r
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
        Err(ExtractError::Read(repak::Error::HashMismatch)) => {
            dlog!("  SHA-1 mismatch: the entry's data is damaged");
            E_BAD_DATA
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

pub(crate) fn join_dest(path: Option<String>, name: Option<String>) -> Option<PathBuf> {
    let name = name?;
    match path {
        Some(dir) if !dir.is_empty() => Some(Path::new(&dir).join(name)),
        _ => Some(PathBuf::from(name)),
    }
}
