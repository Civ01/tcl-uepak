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
            "{c} compression needs PAK version V8A (UE 4.22) or newer; {} only supports Zlib, Gzip and Oodle.",
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
    let need = source_bytes(plan) + index_estimate(plan.files.iter().map(|f| f.entry.len()));
    check_space(
        target.parent().unwrap_or(Path::new(".")),
        need,
        "Creating this pak",
    )?;
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

/// Sum of the source file sizes (missing files count as 0; they fail later).
fn source_bytes(plan: &Plan) -> u64 {
    plan.files
        .iter()
        .map(|f| std::fs::metadata(&f.source).map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn tmp_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".uepak-tmp");
    target.with_file_name(name)
}

/// Marks I/O errors that come from reading a *source* file (vs. writing the pak).
#[derive(Debug)]
struct SourceError(PathBuf, std::io::Error);
impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0.display(), self.1)
    }
}
impl std::error::Error for SourceError {}

struct Source {
    file: std::fs::File,
    path: PathBuf,
}
impl std::io::Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf).map_err(|e| {
            let kind = e.kind();
            std::io::Error::new(kind, SourceError(self.path.clone(), e))
        })
    }
}
impl std::io::Seek for Source {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }
}

/// repak error -> PackError, telling source-read failures and Cancel apart.
fn pak_error(e: repak::Error) -> PackError {
    match e {
        repak::Error::Cancelled => PackError::Aborted,
        repak::Error::Io(io) if io.get_ref().is_some_and(|i| i.is::<SourceError>()) => {
            let inner = io.into_inner().unwrap().downcast::<SourceError>().unwrap();
            PackError::Read(inner.0, inner.1)
        }
        other => PackError::Pak(other),
    }
}

/// Streams one source file into `w` block by block, reporting progress to TC
/// (Cancel works between blocks). Returns the source size.
fn stream_file<W: Write + std::io::Seek>(
    w: &mut repak::PakWriter<W>,
    entry: &str,
    compress: bool,
    source: &Path,
) -> Result<u64, PackError> {
    let file = std::fs::File::open(source).map_err(|e| PackError::Read(source.to_path_buf(), e))?;
    let size = file
        .metadata()
        .map_err(|e| PackError::Read(source.to_path_buf(), e))?
        .len();
    let mut src = Source {
        file,
        path: source.to_path_buf(),
    };
    let mut name = win::wide(&source.to_string_lossy());
    let mut reported_any = false;
    w.write_file_from_reader(entry, compress, &mut src, size, &mut |n| {
        reported_any = true;
        progress::report(&mut name, n)
    })
    .map_err(pak_error)?;
    if !reported_any && !progress::report(&mut name, 1) {
        // empty files: still give TC a chance to cancel
        return Err(PackError::Aborted);
    }
    Ok(size)
}

/// Free bytes on the volume of `dir` (tests can pin a value).
fn free_space(dir: &Path) -> Option<u64> {
    #[cfg(test)]
    if let Some(v) = TEST_FREE_SPACE.with(|c| c.get()) {
        return Some(v);
    }
    win::free_space(dir)
}

#[cfg(test)]
thread_local! {
    /// Per test thread, so concurrent tests don't see each other's value.
    pub static TEST_FREE_SPACE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Rough size of a new index for `entries` paths (for space estimates).
fn index_estimate(paths: impl Iterator<Item = usize>) -> u64 {
    4096 + paths.map(|len| len as u64 * 2 + 128).sum::<u64>()
}

/// Asks before starting if the operation may need more than the free space.
/// `need` is an upper bound (compression usually makes it smaller).
fn check_space(dir: &Path, need: u64, what: &str) -> Result<(), PackError> {
    let Some(free) = free_space(dir) else {
        return Ok(());
    };
    dlog!("  space: needs up to {need} bytes, {free} free");
    if need <= free {
        return Ok(());
    }
    let text = format!(
        "{what} may need up to {} of free disk space, but only {} is free on that drive.\n\n\
         Compression can make it smaller, so it may still fit. If the disk fills up, the \
         operation is undone and nothing is damaged.\n\nContinue anyway?",
        crate::config::human_size(need),
        crate::config::human_size(free)
    );
    if crate::dialog::confirm(&text) {
        Ok(())
    } else {
        Err(PackError::Aborted)
    }
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
        let size = stream_file(&mut writer, &item.entry, compress, &item.source)?;
        stats.files += 1;
        stats.input_bytes += size;
        dlog!(
            "  + {} ({size} bytes) <- {}",
            item.entry,
            item.source.display()
        );
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
    /// Bytes of space at the end of the data area that were reused.
    pub reused: u64,
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
    if mapping.new_mount.is_some() {
        let text = reroot_question(&info, &mount, &mapping, plan);
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

    // existing entries replaced by the new files (compared after re-rooting)
    let removals: Vec<String> = info
        .entries
        .iter()
        .filter(|p| {
            let renamed = format!("{}{p}", mapping.reroot_prefix).to_lowercase();
            mapping.entries.iter().any(|e| e.to_lowercase() == renamed)
        })
        .cloned()
        .collect();
    let write_at = choose_write_at(&info, &removals, opts.undo_limit)?;
    let start = write_at.unwrap_or(info.index_offset);
    let new_end = start
        + source_bytes(plan)
        + index_estimate(
            info.entries
                .iter()
                .map(|p| p.len() + mapping.reroot_prefix.len())
                .chain(mapping.entries.iter().map(|e| e.len())),
        );
    let need = (info.file_len - start) + new_end.saturating_sub(info.file_len);
    check_space(
        target.parent().unwrap_or(Path::new(".")),
        need,
        "Changing this pak",
    )?;

    let compress = opts.compression != PackCompression::None;
    let mut stats = ModifyStats {
        new_mount: mapping.new_mount.clone(),
        replaced: removals.len(),
        reused: info.index_offset - start,
        ..Default::default()
    };
    for r in &removals {
        dlog!("  replacing {r}");
    }
    let mut failure: Option<PackError> = None;
    let result = modify::modify(target, &removals, write_at, |w| {
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
        for (item, entry) in plan.files.iter().zip(&mapping.entries) {
            match stream_file(w, entry, compress, &item.source) {
                Ok(size) => {
                    stats.added += 1;
                    stats.input_bytes += size;
                    dlog!("  + {entry} ({size} bytes) <- {}", item.source.display());
                }
                Err(e) => {
                    failure = Some(e);
                    return Err(ModifyError::Aborted);
                }
            }
        }
        Ok(())
    });
    match (result, failure) {
        (Ok(bytes), _) => {
            stats.pak_bytes = bytes;
            Ok(stats)
        }
        (Err(_), Some(e)) => Err(e),
        (Err(e), None) => Err(e.into()),
    }
}

/// Where new data starts (SPEC §4.4): reuse space at the end of the data
/// area (removed entries, earlier dead space) unless the undo copy that
/// needs would exceed `UndoLimit`, in which case the user decides.
fn choose_write_at(
    info: &modify::Inspect,
    removals: &[String],
    limit: u64,
) -> Result<Option<u64>, PackError> {
    use crate::config::human_size;
    let reuse = info.reuse_offset(removals);
    if reuse < info.index_offset {
        let undo = info.file_len - reuse;
        let gain = info.index_offset - reuse;
        dlog!("  reusable space at the end: {gain} bytes (undo copy {undo}, limit {limit})");
        if undo <= limit {
            return Ok(Some(reuse));
        }
        let text = format!(
            "Reusing {} at the end of this pak (space of replaced or removed files) keeps it \
             from growing, but the undo copy made before changing the pak would then be {}, \
             over your limit of {} (UndoLimit).\n\n\
             Yes: reuse the space (make the {} undo copy).\n\
             No: add the files at the end instead (small undo copy; the pak grows by up to {} more).",
            human_size(gain),
            human_size(undo),
            human_size(limit),
            human_size(undo),
            human_size(gain)
        );
        if crate::dialog::confirm(&text) {
            return Ok(Some(reuse));
        }
    }
    let plain_undo = info.file_len - info.index_offset;
    if plain_undo > limit {
        let text = format!(
            "The undo copy made before changing this pak (its index) would be {}, over your \
             limit of {} (UndoLimit).\n\nMake it anyway and continue?",
            human_size(plain_undo),
            human_size(limit)
        );
        if !crate::dialog::confirm(&text) {
            return Err(PackError::Aborted);
        }
    }
    Ok(None)
}

/// The confirmation shown before widening a pak's mount point: says which
/// folders get added to the existing entries' paths, with examples.
pub fn reroot_question(
    info: &modify::Inspect,
    mount: &view::Mount,
    mapping: &view::AddMapping,
    plan: &Plan,
) -> String {
    let new_mount = mapping.new_mount.as_deref().unwrap_or_default();
    let added = mapping.reroot_prefix.trim_end_matches('/');
    let game = |visible_len: usize, entry: &str| {
        let mut parts: Vec<&str> = mount.visible[..visible_len]
            .iter()
            .map(|s| s.as_str())
            .collect();
        parts.push(entry);
        parts.join("/")
    };
    let new_len = view::Mount::parse(new_mount).visible.len();
    let mut text = format!(
        "These files go outside the pak's mount point {}.\n\n\
         To fit them, the mount point becomes {new_mount}, and \"{added}/\" is added to the \
         path of every file already in the pak (it becomes a real folder inside the pak).",
        info.mount
    );
    if let Some(first) = info.entries.first() {
        text.push_str(&format!(
            "\n\n  {first}  →  {}{first}",
            mapping.reroot_prefix
        ));
        if info.entries.len() > 1 {
            text.push_str(&format!("\n  (and {} more)", info.entries.len() - 1));
        }
        text.push_str(&format!(
            "\n\nWhere the game finds them doesn't change: {}",
            game(mount.visible.len(), first)
        ));
    }
    if let (Some(f), Some(e)) = (plan.files.first(), mapping.entries.first()) {
        let name = f
            .source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        text.push_str(&format!(
            "\nNew files go to: {}{}",
            game(new_len, e),
            if name.is_empty() || plan.files.len() == 1 {
                String::new()
            } else {
                format!(" (and {} more)", plan.files.len() - 1)
            }
        ));
    }
    text.push_str("\n\nOnly the pak's index is rewritten; no file data is repacked. Continue?");
    text
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
    let write_at = choose_write_at(
        &info,
        &selected,
        crate::config::load_options().pack.undo_limit,
    )?;
    stats.reused = info.index_offset - write_at.unwrap_or(info.index_offset);
    for e in &selected {
        dlog!("  - {e}");
    }
    stats.removed = selected.len();
    stats.pak_bytes = modify::modify(target, &selected, write_at, |_| Ok(()))?;
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
