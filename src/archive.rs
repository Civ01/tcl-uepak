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
    /// Footer recognised, but the version is newer than repak knows.
    Unsupported(u32),
    /// A key was given but does not decrypt the index (SHA-1 mismatch).
    WrongKey,
    /// The user cancelled the key prompt.
    Cancelled,
    Pak(repak::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Io(e) => write!(f, "{e}"),
            OpenError::Encrypted => write!(f, "the pak index is encrypted (AES key needed)"),
            OpenError::Unsupported(v) => write!(f, "pak version {v} is not supported"),
            OpenError::WrongKey => write!(f, "the AES key does not match this pak"),
            OpenError::Cancelled => write!(f, "cancelled by the user"),
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
    /// A folder of the mount point's visible part (`Folder` view only): listed
    /// explicitly so it can carry marker attributes. Has no data.
    pub mount_dir: bool,
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
    /// Key the pak was opened with, if any.
    pub key: Option<crate::keys::AesKey>,
    /// Set once a key prompt for encrypted entries was cancelled on this handle.
    pub key_declined: bool,
    /// Attributes for mount folder entries (from options at open time).
    pub mount_folder_attr: u32,
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
    fn read_pak(
        reader: &mut BufReader<File>,
        key: Option<&crate::keys::AesKey>,
    ) -> Result<PakReader, OpenError> {
        let mut builder = repak::PakBuilder::new();
        if let Some(k) = key {
            builder = builder.key(k.cipher());
        }
        match builder.reader(reader) {
            Ok(pak) => Ok(pak),
            Err(repak::Error::Encrypted) => Err(OpenError::Encrypted),
            Err(repak::Error::WrongKey) => Err(OpenError::WrongKey),
            Err(repak::Error::UnsupportedVersion(v)) => Err(OpenError::Unsupported(v)),
            Err(e) => Err(OpenError::Pak(e)),
        }
    }

    pub fn open(
        path: &Path,
        options: &Options,
        key: Option<&crate::keys::AesKey>,
    ) -> Result<Self, OpenError> {
        let file = File::open(path).map_err(OpenError::Io)?;
        let mtime = file.metadata().and_then(|m| m.modified()).ok();
        let mut reader = BufReader::new(file);
        let pak = Self::read_pak(&mut reader, key)?;

        let prefix = match options.mount_point {
            MountPointMode::Root => String::new(),
            MountPointMode::Folder => mount_prefix(pak.mount_point()),
        };
        // Mount folders first: "TheIsle", "TheIsle\Content".
        let mut items: Vec<Item> = vec![];
        let mut acc = String::new();
        for seg in prefix.split('\\').filter(|s| !s.is_empty()) {
            if !acc.is_empty() {
                acc.push('\\');
            }
            acc.push_str(seg);
            items.push(Item {
                pak_path: String::new(),
                display: acc.clone(),
                info: EntryInfo {
                    offset: 0,
                    compressed_size: 0,
                    uncompressed_size: 0,
                    compression: None,
                    encrypted: false,
                    timestamp: None,
                },
                mount_dir: true,
            });
        }
        let files: Vec<Item> = pak
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
                    mount_dir: false,
                }
            })
            .collect();
        items.extend(files);

        Ok(Archive {
            path: path.to_path_buf(),
            reader,
            pak,
            items,
            next: 0,
            current: None,
            mtime,
            oodle_warned: false,
            key: key.copied(),
            key_declined: false,
            mount_folder_attr: options.mount_folder_attr,
        })
    }

    /// Re-reads the index with `key` (for paks whose index is plain but whose
    /// entries are encrypted). Listing state and position are kept.
    pub fn set_key(&mut self, key: &crate::keys::AesKey) -> Result<(), OpenError> {
        self.pak = Self::read_pak(&mut self.reader, Some(key))?;
        self.key = Some(*key);
        Ok(())
    }

    pub fn pak(&self) -> &PakReader {
        &self.pak
    }

    /// Number of real file entries (excludes mount folders).
    pub fn file_count(&self) -> usize {
        self.items.iter().filter(|i| !i.mount_dir).count()
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

/// Raw footer fields, read independently of repak (for detection and logs).
#[derive(Debug, Default)]
pub struct FooterSniff {
    pub file_size: u64,
    /// Offset of the magic from the end of the file.
    pub magic_from_end: u64,
    pub version: u32,
    pub encrypted_index: Option<bool>,
    pub key_guid: Option<String>,
    pub index_offset: u64,
    pub index_size: u64,
    pub compression: Vec<String>,
}

impl std::fmt::Display for FooterSniff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "size={} version={} encrypted_index={:?} key_guid={} index@{}+{} magic_from_end={} compression=[{}]",
            self.file_size,
            self.version,
            self.encrypted_index,
            self.key_guid.as_deref().unwrap_or("-"),
            self.index_offset,
            self.index_size,
            self.magic_from_end,
            self.compression.join(", ")
        )
    }
}

/// Finds the PAK footer magic in the last 512 bytes and decodes the fields
/// around it. `None` if there is no magic (not a standard pak).
pub fn sniff_footer(path: &Path) -> Option<FooterSniff> {
    let mut f = File::open(path).ok()?;
    let len = f.seek(SeekFrom::End(0)).ok()?;
    let tail = len.min(512);
    if tail < 44 {
        return None;
    }
    f.seek(SeekFrom::Start(len - tail)).ok()?;
    let mut buf = vec![0; tail as usize];
    f.read_exact(&mut buf).ok()?;
    let magic = PAK_MAGIC.to_le_bytes();
    let pos = buf.windows(4).rposition(|w| w == magic)?;
    let u32_at = |o: usize| {
        buf.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let u64_at = |o: usize| {
        buf.get(o..o + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    let version = u32_at(pos + 4)?;
    let mut s = FooterSniff {
        file_size: len,
        magic_from_end: tail - pos as u64,
        version,
        index_offset: u64_at(pos + 8).unwrap_or(0),
        index_size: u64_at(pos + 16).unwrap_or(0),
        ..Default::default()
    };
    if version >= 4 && pos >= 1 {
        s.encrypted_index = Some(buf[pos - 1] != 0);
    }
    if version >= 7 && pos >= 17 {
        s.key_guid = Some(
            buf[pos - 17..pos - 1]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect(),
        );
    }
    if version >= 8 {
        let mut o = pos + 4 + 4 + 8 + 8 + 20 + usize::from(version == 9);
        while let Some(name) = buf.get(o..o + 32) {
            let name: String = name
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as char)
                .collect();
            if !name.is_empty() {
                s.compression.push(name);
            }
            o += 32;
        }
    }
    Some(s)
}

/// Cheap content check: PAK footer magic within the last 512 bytes.
pub fn looks_like_pak(path: &Path) -> bool {
    sniff_footer(path).is_some()
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
