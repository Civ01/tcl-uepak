//! Platform-independent archive state behind a TC archive handle.

use crate::config::{MountPointMode, Options};
use repak::{Compression, EntryInfo, PakReader};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const PAK_MAGIC: u32 = 0x5A6F12E1;

#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    /// Index is AES-encrypted and no key is available.
    Encrypted,
    Pak(repak::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Io(e) => write!(f, "{e}"),
            OpenError::Encrypted => write!(f, "the pak index is encrypted (AES key needed)"),
            OpenError::Pak(e) => write!(f, "{e}"),
        }
    }
}

pub struct Item {
    /// Path as stored in the pak index (relative to the mount point, `/`-separated).
    pub pak_path: String,
    /// Path shown to TC (`\`-separated, optional mount point prefix).
    pub display: String,
    pub info: EntryInfo,
}

pub struct Archive {
    pub path: PathBuf,
    reader: BufReader<File>,
    pak: PakReader,
    pub items: Vec<Item>,
    /// Index of the next item ReadHeader returns.
    next: usize,
    /// Item last returned by ReadHeader, target of ProcessFile.
    current: Option<usize>,
    /// Archive's own modification time, used for every entry.
    pub mtime: Option<std::time::SystemTime>,
    /// Set once the "Oodle missing" warning was shown for this handle.
    pub oodle_warned: bool,
}

/// Turns `../../../Game/Content/` into `Game\Content` (empty if nothing left).
pub fn mount_prefix(mount_point: &str) -> String {
    mount_point
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".." && *s != ".")
        .collect::<Vec<_>>()
        .join("\\")
}

/// Rejects entry names that would escape the destination folder.
fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['/', '\\'])
        && !path.contains(':')
        && path.split(['/', '\\']).all(|c| c != ".." && !c.is_empty())
}

impl Archive {
    pub fn open(path: &Path, options: &Options) -> Result<Self, OpenError> {
        let file = File::open(path).map_err(OpenError::Io)?;
        let mtime = file.metadata().and_then(|m| m.modified()).ok();
        let mut reader = BufReader::new(file);
        let pak = match repak::PakBuilder::new().reader(&mut reader) {
            Ok(pak) => pak,
            Err(repak::Error::Encrypted) => return Err(OpenError::Encrypted),
            Err(e) => return Err(OpenError::Pak(e)),
        };

        let prefix = match options.mount_point {
            MountPointMode::Root => String::new(),
            MountPointMode::Folder => mount_prefix(pak.mount_point()),
        };
        let items = pak
            .entries()
            .filter(|(p, _)| is_safe_relative(p))
            .map(|(p, info)| {
                let rel = p.replace('/', "\\");
                let display = if prefix.is_empty() {
                    rel
                } else {
                    format!("{prefix}\\{rel}")
                };
                Item {
                    pak_path: p.to_string(),
                    display,
                    info,
                }
            })
            .collect();

        Ok(Archive {
            path: path.to_path_buf(),
            reader,
            pak,
            items,
            next: 0,
            current: None,
            mtime,
            oodle_warned: false,
        })
    }

    pub fn uses_oodle(&self) -> bool {
        self.pak.used_compression().contains(&Compression::Oodle)
    }

    /// Advances to the next entry (ReadHeader). `None` at end of archive.
    pub fn next_item(&mut self) -> Option<&Item> {
        let idx = self.next;
        let item = self.items.get(idx)?;
        self.current = Some(idx);
        self.next += 1;
        Some(item)
    }

    pub fn current_item(&self) -> Option<&Item> {
        self.current.and_then(|i| self.items.get(i))
    }

    /// Decompresses the current entry into `out`.
    pub fn read_current<W: Write>(&mut self, out: &mut W) -> Result<(), repak::Error> {
        let item = self
            .current
            .and_then(|i| self.items.get(i))
            .ok_or_else(|| repak::Error::MissingEntry(String::new()))?;
        self.pak.read_file(&item.pak_path, &mut self.reader, out)
    }

    /// Extracts the current entry to `dest`, removing a partial file on failure.
    pub fn extract_current<W, F>(&mut self, dest: &Path, wrap: F) -> Result<(), ExtractError>
    where
        F: FnOnce(BufWriter<File>) -> W,
        W: Write + Finish,
    {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(ExtractError::Create)?;
        }
        let file = File::create(dest).map_err(ExtractError::Create)?;
        let mut out = wrap(BufWriter::new(file));
        let result = self
            .read_current(&mut out)
            .map_err(ExtractError::from_repak)
            .and_then(|_| out.finish().map_err(ExtractError::from_write));
        drop(out);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }
}

/// Final flush for extraction writers, so write errors are not lost in Drop.
pub trait Finish {
    fn finish(&mut self) -> io::Result<()>;
}

impl Finish for BufWriter<File> {
    fn finish(&mut self) -> io::Result<()> {
        self.flush()?;
        self.get_ref().sync_all().or(Ok(()))
    }
}

#[derive(Debug)]
pub enum ExtractError {
    Create(io::Error),
    Write(io::Error),
    Aborted,
    Read(repak::Error),
}

/// Error payload used by progress writers when the user cancels.
/// (Not `ErrorKind::Interrupted`: `io::copy` would silently retry that.)
#[derive(Debug)]
pub struct UserAborted;

impl std::fmt::Display for UserAborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("aborted by user")
    }
}

impl std::error::Error for UserAborted {}

pub fn is_user_abort(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<UserAborted>())
}

impl ExtractError {
    fn from_write(e: io::Error) -> Self {
        if is_user_abort(&e) {
            ExtractError::Aborted
        } else {
            ExtractError::Write(e)
        }
    }

    pub fn from_repak(e: repak::Error) -> Self {
        match e {
            repak::Error::Io(io) => Self::from_write(io),
            other => ExtractError::Read(other),
        }
    }
}

/// Cheap content check: PAK footer magic within the last 512 bytes.
pub fn looks_like_pak(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let Ok(len) = f.seek(SeekFrom::End(0)) else {
        return false;
    };
    let tail = len.min(512);
    if tail < 44 || f.seek(SeekFrom::Start(len - tail)).is_err() {
        return false;
    }
    let mut buf = vec![0; tail as usize];
    if f.read_exact(&mut buf).is_err() {
        return false;
    }
    let magic = PAK_MAGIC.to_le_bytes();
    buf.windows(4).any(|w| w == magic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_prefix_strips_relative_parts() {
        assert_eq!(mount_prefix("../../../Moadra/Config/"), "Moadra\\Config");
        assert_eq!(mount_prefix("../../../"), "");
        assert_eq!(mount_prefix("/Game/"), "Game");
    }

    #[test]
    fn rejects_unsafe_names() {
        assert!(is_safe_relative("Content/a.uasset"));
        assert!(!is_safe_relative("../evil.dll"));
        assert!(!is_safe_relative("a/../../b"));
        assert!(!is_safe_relative("C:/x"));
        assert!(!is_safe_relative("/abs"));
        assert!(!is_safe_relative("a//b"));
    }
}
