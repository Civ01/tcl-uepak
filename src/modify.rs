//! Modification of existing paks with crash safety (SPEC §4.4): in place
//! (`modify`) or as a full rebuild (`rebuild`, "Compact when modifying").
//!
//! New data is written where the old index was, then a new index and footer;
//! the file is truncated to the new end. Before touching the file, the old
//! tail (index + footer) is saved to `<pak>.uepak-undo`; any failure, cancel
//! or panic restores it. If TC or Windows dies mid-write, the undo file is
//! found on the next open and the user is offered a restore.

use crate::dlog;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const UNDO_MAGIC: &[u8; 10] = b"UEPAKUNDO1";

#[derive(Debug)]
pub enum ModifyError {
    /// Encrypted and no key was given.
    Encrypted,
    /// An interrupted earlier change must be resolved first.
    PendingUndo,
    Io(io::Error),
    Pak(repak::Error),
    Aborted,
    /// Message for the user.
    User(String),
}

impl std::fmt::Display for ModifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModifyError::Encrypted => f.write_str("the pak is encrypted and no key was given"),
            ModifyError::PendingUndo => f.write_str("an interrupted earlier change is pending"),
            ModifyError::Io(e) => write!(f, "{e}"),
            ModifyError::Pak(e) => write!(f, "{e}"),
            ModifyError::Aborted => f.write_str("cancelled"),
            ModifyError::User(m) => f.write_str(m),
        }
    }
}

impl From<io::Error> for ModifyError {
    fn from(e: io::Error) -> Self {
        ModifyError::Io(e)
    }
}

impl From<repak::Error> for ModifyError {
    fn from(e: repak::Error) -> Self {
        match e {
            repak::Error::Encrypted | repak::Error::WrongKey => ModifyError::Encrypted,
            other => ModifyError::Pak(other),
        }
    }
}

pub fn undo_path(pak: &Path) -> PathBuf {
    let mut name = pak.file_name().unwrap_or_default().to_os_string();
    name.push(".uepak-undo");
    pak.with_file_name(name)
}

pub fn has_undo(pak: &Path) -> bool {
    undo_path(pak).exists()
}

/// What we need to know about a pak before changing it.
#[derive(Debug)]
pub struct Inspect {
    pub mount: String,
    pub version: repak::Version,
    pub entries: Vec<String>,
    /// Compression methods named in the footer slots (v8+).
    pub slots: Vec<String>,
    /// Where each entry lies in the file (same order as `entries`).
    pub spans: Vec<(u64, u64)>,
    pub index_offset: u64,
    pub file_len: u64,
    pub encrypted_index: bool,
    /// At least one entry is encrypted: new entries are encrypted too.
    pub encrypted_entries: bool,
}

impl Inspect {
    /// First byte after the last entry that is kept when `removed` go away:
    /// new data can start here instead of at the index (reusing the space of
    /// removed entries and earlier dead space at the end of the data area).
    pub fn reuse_offset(&self, removed: &[String]) -> u64 {
        self.entries
            .iter()
            .zip(&self.spans)
            .filter(|(p, _)| !removed.contains(p))
            .map(|(_, (_, end))| *end)
            .max()
            .unwrap_or(0)
            .min(self.index_offset)
    }
}

fn builder(key: Option<&crate::keys::AesKey>) -> repak::PakBuilder {
    match key {
        Some(k) => repak::PakBuilder::new().key(k.cipher()),
        None => repak::PakBuilder::new(),
    }
}

/// Bytes of stored data of the entries kept when `removed` go away.
impl Inspect {
    pub fn kept_bytes(&self, removed: &[String]) -> u64 {
        self.entries
            .iter()
            .zip(&self.spans)
            .filter(|(p, _)| !removed.contains(p))
            .map(|(_, (start, end))| end - start)
            .sum()
    }
}

pub fn inspect(pak: &Path, key: Option<&crate::keys::AesKey>) -> Result<Inspect, ModifyError> {
    let mut r = BufReader::new(File::open(pak)?);
    let p = builder(key).reader(&mut r)?;
    let encrypted_entries = p.entries().any(|(_, e)| e.encrypted);
    let (entries, spans): (Vec<String>, Vec<(u64, u64)>) = p
        .entries()
        .map(|(path, e)| (path.to_string(), (e.offset, e.data_end)))
        .unzip();
    Ok(Inspect {
        mount: p.mount_point().to_string(),
        version: p.version(),
        entries,
        slots: crate::archive::sniff_footer(pak)
            .map(|f| f.compression)
            .unwrap_or_default(),
        spans,
        index_offset: p.index_offset().unwrap_or(0),
        file_len: std::fs::metadata(pak)?.len(),
        encrypted_index: p.encrypted_index(),
        encrypted_entries,
    })
}

fn rollback(file: &mut File, index_offset: u64, tail: &[u8], len: u64) -> io::Result<()> {
    file.set_len(index_offset)?;
    file.seek(SeekFrom::Start(index_offset))?;
    file.write_all(tail)?;
    file.set_len(len)?;
    file.sync_all()
}

/// Opens `pak` for writing, removes `removals`, runs `edit` on a writer
/// positioned at `write_at` (default: the old index), writes the new index,
/// truncates. Restores the original on any error, cancel or panic. Returns
/// the new file size.
///
/// `write_at` before the index reuses space: every entry still in the pak
/// after the removals must end at or before it, which is checked before
/// anything is written.
pub fn modify<F>(
    pak: &Path,
    key: Option<&crate::keys::AesKey>,
    removals: &[String],
    write_at: Option<u64>,
    edit: F,
) -> Result<u64, ModifyError>
where
    F: FnOnce(&mut repak::PakWriter<BufWriter<&mut File>>) -> Result<(), ModifyError>,
{
    if has_undo(pak) {
        return Err(ModifyError::PendingUndo);
    }
    let mut file = OpenOptions::new().read(true).write(true).open(pak)?;
    let len = file.metadata()?.len();
    let reader = {
        let mut r = BufReader::new(&mut file);
        builder(key).reader(&mut r)?
    };
    // new entries are encrypted like the existing ones (the caller makes
    // sure there is a key when adding)
    let encrypt_new = key.is_some() && reader.entries().any(|(_, e)| e.encrypted);
    let index_offset = reader
        .index_offset()
        .ok_or_else(|| ModifyError::User("pak has no index offset".into()))?;
    let start = write_at.unwrap_or(index_offset);
    if start > index_offset {
        return Err(ModifyError::User(format!(
            "write offset {start} lies after the index"
        )));
    }
    if let Some((path, e)) = reader
        .entries()
        .find(|(p, e)| !removals.iter().any(|r| r == p) && e.data_end > start)
    {
        return Err(ModifyError::User(format!(
            "internal check failed: {path} ({}..{}) would be overwritten at {start}",
            e.offset, e.data_end
        )));
    }

    // 1. save the tail (from where writing starts: index + footer, plus any
    //    reused space)
    let index_offset = start;
    let mut tail = Vec::new();
    file.seek(SeekFrom::Start(index_offset))?;
    (&mut file)
        .take(len - index_offset)
        .read_to_end(&mut tail)?;
    {
        let mut undo = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(undo_path(pak))?;
        undo.write_all(UNDO_MAGIC)?;
        undo.write_all(&len.to_le_bytes())?;
        undo.write_all(&index_offset.to_le_bytes())?;
        undo.write_all(&tail)?;
        undo.sync_all()?;
    }
    dlog!(
        "  undo saved: index at {index_offset}, {} tail bytes",
        tail.len()
    );

    // 2. edit + new index + truncate
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut w = reader.into_pakwriter_at(BufWriter::new(&mut file), index_offset)?;
        // the index keeps its encryption (and key GUID) when reopened
        w.set_encrypt_entries(encrypt_new);
        for r in removals {
            w.remove(r);
        }
        edit(&mut w)?;
        let out = w.write_index()?;
        let f = out
            .into_inner()
            .map_err(|e| ModifyError::Io(e.into_error()))?;
        let end = f.stream_position()?;
        f.set_len(end)?;
        f.sync_all()?;
        Ok::<u64, ModifyError>(end)
    }));

    // 3. commit or roll back
    match result {
        Ok(Ok(end)) => {
            let _ = std::fs::remove_file(undo_path(pak));
            dlog!("  committed: {len} -> {end} bytes");
            Ok(end)
        }
        Ok(Err(e)) => {
            dlog!("  failed ({e}); rolling back");
            match rollback(&mut file, index_offset, &tail, len) {
                Ok(()) => {
                    let _ = std::fs::remove_file(undo_path(pak));
                }
                Err(re) => dlog!("  ROLLBACK FAILED ({re}); undo file kept for recovery"),
            }
            Err(e)
        }
        Err(panic) => {
            let _ = rollback(&mut file, index_offset, &tail, len)
                .map(|_| std::fs::remove_file(undo_path(pak)));
            std::panic::resume_unwind(panic)
        }
    }
}

pub fn rebuild_path(pak: &Path) -> PathBuf {
    let mut name = pak.file_name().unwrap_or_default().to_os_string();
    name.push(".uepak-new");
    pak.with_file_name(name)
}

/// "Compact when modifying" (SPEC §4.4): builds a complete new pak in
/// `<pak>.uepak-new` from the entries kept after `removals` (stored bytes
/// copied in file order: no recompression, no re-encryption), then runs
/// `edit` on it (rename, new files), writes the index and replaces the
/// original. No dead space and no undo file: until the final rename the
/// original is untouched, and any failure only deletes the new file.
/// `progress(copied, total)` reports the copy; `false` cancels. Returns the
/// new file size.
/// UnrealPak can start entries on a boundary (1–2 MB in a UE 5.6 pak), with
/// zero padding before them. An entry counts as aligned when there is a gap
/// before it and its offset is a multiple of at least 4 KB; the pak's
/// alignment is the smallest such boundary. Returns it with the aligned
/// entries' paths.
pub fn detect_alignment(entries: &[(u64, u64, String)]) -> Option<(u64, Vec<String>)> {
    let mut sorted: Vec<&(u64, u64, String)> = entries.iter().collect();
    sorted.sort();
    let mut prev_end = 0u64;
    let mut found: Vec<(u64, &str)> = vec![];
    for (offset, end, path) in sorted {
        if *offset > prev_end && *offset > 0 && offset.trailing_zeros() >= 12 {
            found.push((*offset, path));
        }
        prev_end = prev_end.max(*end);
    }
    let align = found
        .iter()
        .map(|(o, _)| 1u64 << o.trailing_zeros().min(30))
        .min()?;
    let paths = found
        .into_iter()
        .filter(|(o, _)| o % align == 0)
        .map(|(_, p)| p.to_string())
        .collect();
    Some((align, paths))
}

pub fn rebuild<F>(
    pak: &Path,
    key: Option<&crate::keys::AesKey>,
    keep_alignment: bool,
    removals: &[String],
    progress: &mut dyn FnMut(u64, u64) -> bool,
    edit: F,
) -> Result<u64, ModifyError>
where
    F: FnOnce(&mut repak::PakWriter<BufWriter<&mut File>>) -> Result<(), ModifyError>,
{
    if has_undo(pak) {
        return Err(ModifyError::PendingUndo);
    }
    let mut src = BufReader::new(File::open(pak)?);
    let reader = builder(key).reader(&mut src)?;
    let encrypt_new = key.is_some() && reader.entries().any(|(_, e)| e.encrypted);
    let all: Vec<(u64, u64, String)> = reader
        .entries()
        .map(|(p, e)| (e.offset, e.data_end, p.to_string()))
        .collect();
    let alignment = if keep_alignment {
        detect_alignment(&all)
    } else {
        None
    };
    if let Some((a, paths)) = &alignment {
        dlog!(
            "  keeping alignment: {} entries on {a}-byte boundaries",
            paths.len()
        );
    }
    let mut kept: Vec<(u64, u64, String)> = reader
        .entries()
        .filter(|(p, _)| !removals.iter().any(|r| r == p))
        .map(|(p, e)| (e.offset, e.data_end - e.offset, p.to_string()))
        .collect();
    kept.sort();
    let total: u64 = kept.iter().map(|k| k.1).sum();
    let tmp = rebuild_path(pak);
    let mut out = File::create(&tmp)?;
    dlog!(
        "  compacting: {} entries ({total} bytes) -> {}",
        kept.len(),
        tmp.display()
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut w = reader.new_pakwriter_like(BufWriter::new(&mut out));
        w.set_encrypt_entries(encrypt_new);
        let mut copied = 0u64;
        for (_, _, path) in &kept {
            if let Some((a, paths)) = &alignment {
                if paths.iter().any(|p| p == path) {
                    w.align_to(*a)?;
                }
            }
            w.copy_entry(&reader, path, path, &mut src, &mut |n| {
                copied += n;
                progress(copied, total)
            })
            .map_err(|e| match e {
                repak::Error::Cancelled => ModifyError::Aborted,
                e => ModifyError::Pak(e),
            })?;
        }
        edit(&mut w)?;
        let w = w.write_index()?;
        let f = w
            .into_inner()
            .map_err(|e| ModifyError::Io(e.into_error()))?;
        let end = f.stream_position()?;
        f.sync_all()?;
        Ok::<u64, ModifyError>(end)
    }));
    drop(src);
    drop(out);
    match result {
        Ok(Ok(end)) => match std::fs::rename(&tmp, pak) {
            Ok(()) => {
                dlog!("  compacted: replaced the pak ({end} bytes)");
                Ok(end)
            }
            Err(e) => {
                dlog!("  could not replace the pak with the rebuilt one: {e}");
                let _ = std::fs::remove_file(&tmp);
                Err(ModifyError::User(format!(
                    "The rebuilt pak could not replace the original ({e}). The original is unchanged."
                )))
            }
        },
        Ok(Err(e)) => {
            dlog!("  rebuild failed ({e}); original unchanged");
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
        Err(panic) => {
            let _ = std::fs::remove_file(&tmp);
            std::panic::resume_unwind(panic)
        }
    }
}

/// Restores a pak from its undo file (after a crash mid-write).
pub fn restore(pak: &Path) -> io::Result<()> {
    let data = std::fs::read(undo_path(pak))?;
    if data.len() < 26 || &data[..10] != UNDO_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "undo file is invalid",
        ));
    }
    let len = u64::from_le_bytes(data[10..18].try_into().unwrap());
    let index_offset = u64::from_le_bytes(data[18..26].try_into().unwrap());
    let tail = &data[26..];
    if index_offset + tail.len() as u64 != len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "undo file is inconsistent",
        ));
    }
    let mut file = OpenOptions::new().read(true).write(true).open(pak)?;
    rollback(&mut file, index_offset, tail, len)?;
    drop(file);
    std::fs::remove_file(undo_path(pak))
}

/// Drops a pending undo file (user chose to keep the current state).
pub fn discard_undo(pak: &Path) -> io::Result<()> {
    std::fs::remove_file(undo_path(pak))
}
