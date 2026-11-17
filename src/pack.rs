//! Packing (PackFiles): creating new paks.
//!
//! Path model (SPEC §4.2): entry paths are the TC AddList items (relative to
//! SrcPath), optionally under SubPath; the mount point places them in the game.
//! Adding to existing paks (§4.4) is the next step and is refused for now.

use crate::config::{MountPointMode, PackCompression, PackOptions};
use crate::ffi::*;
use crate::modify::{self, ModifyError};
use crate::view;
use crate::{dlog, progress, win};
use std::ffi::c_int;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PK_PACK_MOVE_FILES: c_int = 1;
pub const PK_PACK_SAVE_PATHS: c_int = 2;

/// One file to pack.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    pub source: PathBuf,
    /// Target as a TC view path, split into segments (SubPath + item).
    pub view: Vec<String>,
    /// Entry path for a new pak (`/`-separated, relative to the mount point).
    pub entry: String,
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub files: Vec<PlanItem>,
    /// AddList folder items (trailing `\`); only used for "move to archive".
    pub dirs: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum PlanError {
    /// Several files would get the same entry path (case-insensitive).
    Duplicates(Vec<String>),
    /// An item would produce an unsafe entry path.
    BadPath(String),
    Empty,
}

/// `src` + a TC relative path (`\\`-separated), split into components.
fn join_rel(src: &Path, rel: &str) -> PathBuf {
    let mut p = src.to_path_buf();
    for c in rel.split(['\\', '/']).filter(|c| !c.is_empty()) {
        p.push(c);
    }
    p
}

/// Turns TC's PackFiles arguments into a list of (source, entry path).
pub fn plan(
    src: &Path,
    sub_path: Option<&str>,
    items: &[String],
    flags: c_int,
) -> Result<Plan, PlanError> {
    let save_paths = flags & PK_PACK_SAVE_PATHS != 0;
    let sub = sub_path
        .map(|s| s.replace('\\', "/").trim_matches('/').to_string())
        .filter(|s| !s.is_empty());
    let mut files = vec![];
    let mut dirs = vec![];
    for item in items {
        if item.ends_with('\\') || item.ends_with('/') {
            dirs.push(join_rel(src, item));
            continue;
        }
        let rel = item.replace('\\', "/");
        let name = if save_paths {
            rel.clone()
        } else {
            rel.rsplit('/').next().unwrap_or(&rel).to_string()
        };
        let entry = match &sub {
            Some(sub) => format!("{sub}/{name}"),
            None => name,
        };
        if !crate::archive::is_safe_relative(&entry) {
            return Err(PlanError::BadPath(entry));
        }
        files.push(PlanItem {
            source: join_rel(src, item),
            view: crate::view::segments(&entry),
            entry,
        });
    }
    if files.is_empty() {
        return Err(PlanError::Empty);
    }
    let mut seen = std::collections::HashMap::new();
    let mut dups = vec![];
    for f in &files {
        let key = f.entry.to_lowercase();
        if let Some(prev) = seen.insert(key, f.source.clone()) {
            dups.push(format!(
                "{} ({} and {})",
                f.entry,
                prev.display(),
                f.source.display()
            ));
        }
    }
    if !dups.is_empty() {
        return Err(PlanError::Duplicates(dups));
    }
    Ok(Plan { files, dirs })
}

/// Why packing failed; mapped to a TC error code (+ message where useful).
#[derive(Debug)]
pub enum PackError {
    Aborted,
    Read(PathBuf, std::io::Error),
    Write(std::io::Error),
    Pak(repak::Error),
    /// A message for the user; returned as `code`.
    User(String, c_int),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::Aborted => write!(f, "cancelled"),
            PackError::Read(p, e) => write!(f, "could not read {}: {e}", p.display()),
            PackError::Write(e) => write!(f, "could not write the pak: {e}"),
            PackError::Pak(e) => write!(f, "could not write the pak: {e}"),
            PackError::User(m, _) => f.write_str(m),
        }
    }
}

impl PackError {
    pub fn code(&self) -> c_int {
        match self {
            PackError::Aborted => E_EABORTED,
            PackError::Read(..) => E_EREAD,
            PackError::Write(_) => E_EWRITE,
            PackError::Pak(_) => E_EWRITE,
            PackError::User(_, c) => *c,
        }
    }
}

/// Settings checks that need no I/O: version/compression compatibility.
pub fn check_settings(opts: &PackOptions) -> Result<(), String> {
    use repak::Version;
    let c = opts.compression;
    if c == PackCompression::None {
        return Ok(());
    }
    if let (Some(l), Some((lo, hi, _))) = (opts.level, crate::config::level_range(c)) {
        if !(lo..=hi).contains(&l) {
            return Err(format!(
                "Level {l} is out of range for {c:?} ({lo} to {hi})."
            ));
        }
    }
    if opts.version < Version::V3 {
        return Err(format!(
            "PAK version {} has no compression support. Choose Compression=None or version V3 or newer.",
            opts.version
        ));
    }
    if opts.version < Version::V8A
        && !matches!(
            c,
            PackCompression::Zlib | PackCompression::Gzip | PackCompression::Oodle
        )
    {
        return Err(format!(
            "{c:?} compression needs PAK version V8A (UE 4.22) or newer; {} only supports Zlib, Gzip and Oodle.",
            opts.version
        ));
    }
    Ok(())
}

/// Writes a new pak at `target` from `plan`. Writes to a temporary file
/// first and renames it into place, so a failed or cancelled pack never
/// leaves a partial pak behind.
pub fn write_new_pak(
    target: &Path,
    plan: &Plan,
    opts: &PackOptions,
) -> Result<PackStats, PackError> {
    check_settings(opts).map_err(|m| PackError::User(m, E_NOT_SUPPORTED))?;
    check_oodle(opts)?;
    let tmp = tmp_path(target);
    let result = write_to(&tmp, plan, opts);
    match result {
        Ok(stats) => {
            std::fs::rename(&tmp, target).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                PackError::Write(e)
            })?;
            Ok(stats)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn tmp_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".uepak-tmp");
    target.with_file_name(name)
}

#[derive(Debug, Default)]
pub struct PackStats {
    pub files: usize,
    pub input_bytes: u64,
    pub pak_bytes: u64,
    pub compressed_entries: usize,
}

fn write_to(path: &Path, plan: &Plan, opts: &PackOptions) -> Result<PackStats, PackError> {
    let file = std::fs::File::create(path).map_err(PackError::Write)?;
    let mut builder = repak::PakBuilder::new()
        .compression_block_size(opts.block_size)
        .compression_options(opts.compression_options());
    if let Some(c) = opts.compression.repak() {
        builder = builder.compression([c]);
    }
    let mut writer = builder.writer(
        std::io::BufWriter::new(file),
        opts.version,
        opts.mount_point.clone(),
        None,
    );
    let compress = opts.compression != PackCompression::None;
    let mut stats = PackStats::default();
    for item in &plan.files {
        // Whole-file read; streaming writes are fork item F5 (SPEC §9).
        let data =
            std::fs::read(&item.source).map_err(|e| PackError::Read(item.source.clone(), e))?;
        writer
            .write_file(&item.entry, compress, &data)
            .map_err(PackError::Pak)?;
        stats.files += 1;
        stats.input_bytes += data.len() as u64;
        dlog!(
            "  + {} ({} bytes) <- {}",
            item.entry,
            data.len(),
            item.source.display()
        );
        let mut name = win::wide(&item.source.to_string_lossy());
        if !progress::report(&mut name, data.len().max(1) as u64) {
            return Err(PackError::Aborted);
        }
    }
    let mut out = writer.write_index().map_err(PackError::Pak)?;
    out.flush().map_err(PackError::Write)?;
    let file = out
        .into_inner()
        .map_err(|e| PackError::Write(e.into_error()))?;
    file.sync_all().map_err(PackError::Write)?;
    stats.pak_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    drop(file);
    // count entries actually stored compressed (fallback stores some raw)
    if compress {
        if let Ok(f) = std::fs::File::open(path) {
            let mut r = std::io::BufReader::new(f);
            if let Ok(pak) = repak::PakBuilder::new().reader(&mut r) {
                stats.compressed_entries = pak
                    .entries()
                    .filter(|(_, e)| e.compression.is_some())
                    .count();
            }
        }
    }
    Ok(stats)
}

/// "Move to archive": delete packed sources, then the listed folders if empty
/// (deepest first). Failures are logged, not fatal: the pak is already written.
pub fn delete_sources(plan: &Plan) {
    for f in &plan.files {
        if let Err(e) = std::fs::remove_file(&f.source) {
            dlog!("  move: could not delete {}: {e}", f.source.display());
        }
    }
    let mut dirs = plan.dirs.clone();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(&d); // only succeeds when empty
    }
}

impl From<ModifyError> for PackError {
    fn from(e: ModifyError) -> Self {
        match e {
            ModifyError::Encrypted => PackError::User(
                "This pak is encrypted. Changing encrypted paks is not supported yet \
                 (planned: SPEC milestone M4)."
                    .into(),
                E_NOT_SUPPORTED,
            ),
            ModifyError::PendingUndo => PackError::User(
                "An earlier change to this pak was interrupted. Open the pak once to \
                 restore or keep it, then try again."
                    .into(),
                E_NOT_SUPPORTED,
            ),
            ModifyError::Io(e) => PackError::Write(e),
            ModifyError::Pak(e) => PackError::Pak(e),
            ModifyError::Aborted => PackError::Aborted,
            ModifyError::Read(p, e) => PackError::Read(p, e),
            ModifyError::User(m) => PackError::User(m, E_NOT_SUPPORTED),
        }
    }
}

fn footer_name(c: repak::Compression) -> &'static str {
    match c {
        repak::Compression::Zlib => "Zlib",
        repak::Compression::Gzip => "Gzip",
        repak::Compression::Oodle => "Oodle",
        repak::Compression::Zstd => "Zstd",
        repak::Compression::LZ4 => "LZ4",
    }
}

fn check_oodle(opts: &PackOptions) -> Result<(), PackError> {
    if opts.compression == PackCompression::Oodle {
        if let Err(e) = oodle_loader::oodle() {
            return Err(PackError::User(
                format!(
                    "Oodle compression needs the Oodle library: copy oo2core_N_win64.dll into the \
                     plugin's deps folder, or choose another compression.\n\n({e})"
                ),
                E_NOT_SUPPORTED,
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct ModifyStats {
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
    pub input_bytes: u64,
    pub new_mount: Option<String>,
    pub pak_bytes: u64,
}

/// Adds/replaces files in an existing pak (F5 into a pak, Alt+F5 onto one).
pub fn add_to_existing(
    target: &Path,
    plan: &Plan,
    mode: MountPointMode,
    opts: &PackOptions,
) -> Result<ModifyStats, PackError> {
    if modify::has_undo(target) {
        return Err(ModifyError::PendingUndo.into());
    }
    let info = modify::inspect(target)?;
    let mount = view::Mount::parse(&info.mount);
    let views: Vec<Vec<String>> = plan.files.iter().map(|f| f.view.clone()).collect();
    let mapping = view::map_add(mode, &mount, &views);
    for e in &mapping.entries {
        if !crate::archive::is_safe_relative(e) {
            return Err(PackError::User(
                format!("Invalid path for a pak entry: {e}"),
                E_EABORTED,
            ));
        }
    }
    let mut dups = std::collections::HashSet::new();
    for e in &mapping.entries {
        if !dups.insert(e.to_lowercase()) {
            return Err(PackError::User(
                format!("Several files would get the same path in the pak: {e}"),
                E_EABORTED,
            ));
        }
    }
    if let Some(new_mount) = &mapping.new_mount {
        let text = format!(
            "These files go outside the pak's mount point {}.\n\n\
             The mount point will change to {new_mount}. Files already in the pak keep their \
             game paths; only the pak's index is rewritten, no file data is repacked.\n\nContinue?",
            info.mount
        );
        if !crate::dialog::confirm(&text) {
            return Err(PackError::Aborted);
        }
    }
    // settings vs. the existing pak (its version wins)
    let eff = PackOptions {
        version: info.version,
        ..opts.clone()
    };
    check_settings(&eff).map_err(|m| PackError::User(m, E_NOT_SUPPORTED))?;
    if let Some(c) = opts.compression.repak() {
        let name = footer_name(c);
        let capacity = match info.version {
            v if v < repak::Version::V8A => usize::MAX, // fixed Zlib/Gzip/Oodle slots
            repak::Version::V8A => 4,
            _ => 5,
        };
        if !info.slots.iter().any(|s| s.eq_ignore_ascii_case(name)) && info.slots.len() >= capacity
        {
            return Err(PackError::User(
                format!(
                    "This pak already uses {} compression methods ({}); {name} can't be added. \
                     Choose one of those methods or None.",
                    info.slots.len(),
                    info.slots.join(", ")
                ),
                E_NOT_SUPPORTED,
            ));
        }
    }
    check_oodle(opts)?;
    if info.version != opts.version || info.mount != opts.mount_point {
        dlog!(
            "  existing pak keeps version {} and mount {:?} ([Pack] has {} / {:?})",
            info.version,
            info.mount,
            opts.version,
            opts.mount_point
        );
    }

    let compress = opts.compression != PackCompression::None;
    let mut stats = ModifyStats {
        new_mount: mapping.new_mount.clone(),
        ..Default::default()
    };
    let pak_bytes = modify::modify(target, |w| {
        if let Some(c) = opts.compression.repak() {
            w.set_compression([c]);
        } else {
            w.set_compression([]);
        }
        w.set_compression_block_size(opts.block_size);
        w.set_compression_options(opts.compression_options());
        if let Some(new_mount) = &mapping.new_mount {
            let prefix = mapping.reroot_prefix.clone();
            w.rename_all(|p| format!("{prefix}{p}"))?;
            w.set_mount_point(new_mount.clone());
            dlog!("  mount point -> {new_mount:?}, existing entries prefixed with {prefix:?}");
        }
        let current = w.paths();
        for (item, entry) in plan.files.iter().zip(&mapping.entries) {
            for old in current.iter().filter(|p| {
                p.eq_ignore_ascii_case(entry) || p.to_lowercase() == entry.to_lowercase()
            }) {
                if w.remove(old) {
                    stats.replaced += 1;
                    dlog!("  replaced {old}");
                }
            }
            let data = std::fs::read(&item.source)
                .map_err(|e| ModifyError::Read(item.source.clone(), e))?;
            w.write_file(entry, compress, &data)?;
            stats.added += 1;
            stats.input_bytes += data.len() as u64;
            dlog!(
                "  + {entry} ({} bytes) <- {}",
                data.len(),
                item.source.display()
            );
            let mut name = win::wide(&item.source.to_string_lossy());
            if !progress::report(&mut name, data.len().max(1) as u64) {
                return Err(ModifyError::Aborted);
            }
        }
        Ok(())
    })?;
    stats.pak_bytes = pak_bytes;
    Ok(stats)
}

/// Deletes the entries selected by TC's `DeleteList` (F8 inside a pak).
pub fn delete_entries(
    target: &Path,
    items: &[String],
    mode: MountPointMode,
) -> Result<ModifyStats, PackError> {
    if modify::has_undo(target) {
        return Err(ModifyError::PendingUndo.into());
    }
    let info = modify::inspect(target)?;
    let mount = view::Mount::parse(&info.mount);
    let mut selected: Vec<String> = vec![];
    for item in items {
        let t = view::map_delete(mode, &mount, item);
        let hits = view::select(&t, &info.entries);
        dlog!("  {item:?} -> {t:?}: {} entries", hits.len());
        for h in hits {
            if !selected.contains(h) {
                selected.push(h.clone());
            }
        }
    }
    let mut stats = ModifyStats::default();
    if selected.is_empty() {
        return Ok(stats);
    }
    stats.pak_bytes = modify::modify(target, |w| {
        for e in &selected {
            if w.remove(e) {
                stats.removed += 1;
                dlog!("  - {e}");
            }
        }
        Ok(())
    })?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plan_matches_observed_tc_arguments() {
        let src = Path::new("C:\\src");
        let list = items(&[
            "uepak.ini",
            "deps\\",
            "deps\\oo2core_9_win64.dll",
            "deps\\README.txt",
        ]);
        let p = plan(src, None, &list, PK_PACK_SAVE_PATHS).unwrap();
        let entries: Vec<_> = p.files.iter().map(|f| f.entry.as_str()).collect();
        assert_eq!(
            entries,
            ["uepak.ini", "deps/oo2core_9_win64.dll", "deps/README.txt"]
        );
        assert_eq!(p.dirs, [src.join("deps")]);
        // without "pack path names": file names only
        let p = plan(src, None, &list, 0).unwrap();
        let entries: Vec<_> = p.files.iter().map(|f| f.entry.as_str()).collect();
        assert_eq!(entries, ["uepak.ini", "oo2core_9_win64.dll", "README.txt"]);
        // SubPath (F5 into a subfolder)
        let p = plan(
            src,
            Some("Engine\\Programs"),
            &items(&["a.txt"]),
            PK_PACK_SAVE_PATHS,
        )
        .unwrap();
        assert_eq!(p.files[0].entry, "Engine/Programs/a.txt");
    }

    #[test]
    fn plan_rejects_duplicates_and_bad_paths() {
        let src = Path::new("C:\\src");
        let list = items(&["a\\x.txt", "b\\X.TXT"]);
        assert!(matches!(plan(src, None, &list, 0), Err(PlanError::Duplicates(d)) if d.len() == 1));
        assert!(plan(src, None, &list, PK_PACK_SAVE_PATHS).is_ok());
        assert!(matches!(
            plan(src, None, &items(&["..\\evil"]), PK_PACK_SAVE_PATHS),
            Err(PlanError::BadPath(_))
        ));
        assert_eq!(
            plan(src, None, &items(&["dir\\"]), 2),
            Err(PlanError::Empty)
        );
    }

    #[test]
    fn settings_checks() {
        let mut o = PackOptions::default();
        assert!(check_settings(&o).is_ok());
        o.compression = PackCompression::Lz4;
        o.version = repak::Version::V7;
        assert!(check_settings(&o).is_err());
        o.compression = PackCompression::Zlib;
        assert!(check_settings(&o).is_ok());
        o.version = repak::Version::V2;
        assert!(check_settings(&o).is_err());
    }
}
