//! In-place modification of existing paks with crash safety (SPEC §4.4).
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
    /// Index or entries encrypted (modifying encrypted paks: M4).
    Encrypted,
    /// An interrupted earlier change must be resolved first.
    PendingUndo,
    Io(io::Error),
    Pak(repak::Error),
    Aborted,
    /// A source file could not be read.
    Read(PathBuf, io::Error),
    /// Message for the user.
    User(String),
}

impl std::fmt::Display for ModifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModifyError::Encrypted => f.write_str("the pak is encrypted"),
            ModifyError::PendingUndo => f.write_str("an interrupted earlier change is pending"),
            ModifyError::Io(e) => write!(f, "{e}"),
            ModifyError::Pak(e) => write!(f, "{e}"),
            ModifyError::Aborted => f.write_str("cancelled"),
            ModifyError::Read(p, e) => write!(f, "could not read {}: {e}", p.display()),
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
}

pub fn inspect(pak: &Path) -> Result<Inspect, ModifyError> {
    let mut r = BufReader::new(File::open(pak)?);
    let p = repak::PakBuilder::new().reader(&mut r)?;
    if p.encrypted_index() || p.entries().any(|(_, e)| e.encrypted) {
        return Err(ModifyError::Encrypted);
    }
    Ok(Inspect {
        mount: p.mount_point().to_string(),
        version: p.version(),
        entries: p.files(),
        slots: crate::archive::sniff_footer(pak)
            .map(|f| f.compression)
            .unwrap_or_default(),
    })
}

fn rollback(file: &mut File, index_offset: u64, tail: &[u8], len: u64) -> io::Result<()> {
    file.set_len(index_offset)?;
    file.seek(SeekFrom::Start(index_offset))?;
    file.write_all(tail)?;
    file.set_len(len)?;
    file.sync_all()
}

/// Opens `pak` for writing, runs `edit` on a writer positioned at the old
/// index, writes the new index, truncates. Restores the original on any
/// error, cancel or panic. Returns the new file size.
pub fn modify<F>(pak: &Path, edit: F) -> Result<u64, ModifyError>
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
        repak::PakBuilder::new().reader(&mut r)?
    };
    if reader.encrypted_index() || reader.entries().any(|(_, e)| e.encrypted) {
        return Err(ModifyError::Encrypted);
    }
    let index_offset = reader
        .index_offset()
        .ok_or_else(|| ModifyError::User("pak has no index offset".into()))?;

    // 1. save the tail
    let mut tail = vec![0; (len - index_offset) as usize];
    file.seek(SeekFrom::Start(index_offset))?;
    file.read_exact(&mut tail)?;
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
        let mut w = reader.into_pakwriter(BufWriter::new(&mut file))?;
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
