//! Plugin locations and options.
//!
//! Options live in `uepak.ini`. The settings directory is the plugin's own
//! directory when it is writable (portable installs, user-owned TC folders),
//! otherwise the directory of TC's suggested ini (next to wincmd.ini).

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

pub const INI_NAME: &str = "uepak.ini";
pub const DEPS_DIR: &str = "deps";

/// How the pak's mount point appears in the listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MountPointMode {
    /// Paths are shown relative to the mount point (mount point is the root).
    Root,
    /// The mount point (minus leading `../`) is shown as top-level folders.
    #[default]
    Folder,
}

/// Windows attribute bits for mount folder entries (`MountFolderAttr`).
pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;

/// Compression for new entries (`[Pack] Compression`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackCompression {
    None,
    Zlib,
    Gzip,
    Oodle,
    Zstd,
    Lz4,
}

impl std::fmt::Display for PackCompression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "None",
            Self::Zlib => "Zlib",
            Self::Gzip => "Gzip",
            Self::Oodle => "Oodle",
            Self::Zstd => "Zstd",
            Self::Lz4 => "LZ4",
        })
    }
}

impl PackCompression {
    pub fn parse(v: &str) -> Option<Self> {
        Some(match v.trim().to_ascii_lowercase().as_str() {
            "" | "none" | "store" => Self::None,
            "zlib" => Self::Zlib,
            "gzip" => Self::Gzip,
            "oodle" => Self::Oodle,
            "zstd" => Self::Zstd,
            "lz4" => Self::Lz4,
            _ => return None,
        })
    }

    pub fn repak(self) -> Option<repak::Compression> {
        Some(match self {
            Self::None => return None,
            Self::Zlib => repak::Compression::Zlib,
            Self::Gzip => repak::Compression::Gzip,
            Self::Oodle => repak::Compression::Oodle,
            Self::Zstd => repak::Compression::Zstd,
            Self::Lz4 => repak::Compression::LZ4,
        })
    }
}

/// `[Pack]` section: settings for new paks (the Configure dialog will edit these).
#[derive(Debug, Clone)]
pub struct PackOptions {
    pub mount_point: String,
    pub version: repak::Version,
    pub compression: PackCompression,
    /// Uncompressed bytes per compression block.
    pub block_size: u32,
    /// Level for the chosen method; `None` = the method's default.
    pub level: Option<i32>,
    pub oodle_compressor: repak::OodleCompressor,
    /// Largest undo copy made without asking (SPEC §4.4); `u64::MAX` = no limit.
    pub undo_limit: u64,
}

pub const DEFAULT_UNDO_LIMIT: u64 = 1 << 30;

/// `1G`, `512M`, `4096K`, `1073741824`, `0`, `unlimited` -> bytes.
pub fn parse_bytes(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_uppercase();
    if v == "UNLIMITED" || v == "NONE" || v == "OFF" {
        return Some(u64::MAX);
    }
    let v = v.strip_suffix('B').unwrap_or(&v);
    let (num, mul) = match v.chars().last() {
        Some('K') => (&v[..v.len() - 1], 1u64 << 10),
        Some('M') => (&v[..v.len() - 1], 1 << 20),
        Some('G') => (&v[..v.len() - 1], 1 << 30),
        Some('T') => (&v[..v.len() - 1], 1 << 40),
        _ => (v, 1),
    };
    let n: f64 = num.trim().parse().ok()?;
    (n >= 0.0 && n.is_finite()).then_some((n * mul as f64) as u64)
}

/// Human-readable size: `1.5 GB`, `640 KB`.
pub fn human_size(b: u64) -> String {
    if b == u64::MAX {
        return "unlimited".into();
    }
    const U: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} bytes")
    } else if v >= 100.0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Oodle level names (Oodle's numeric values).
pub const OODLE_LEVELS: [(&str, i32); 14] = [
    ("HyperFast4", -4),
    ("HyperFast3", -3),
    ("HyperFast2", -2),
    ("HyperFast1", -1),
    ("None", 0),
    ("SuperFast", 1),
    ("VeryFast", 2),
    ("Fast", 3),
    ("Normal", 4),
    ("Optimal1", 5),
    ("Optimal2", 6),
    ("Optimal3", 7),
    ("Optimal4", 8),
    ("Optimal5", 9),
];

pub const OODLE_COMPRESSORS: [(&str, repak::OodleCompressor); 5] = [
    ("Kraken", repak::OodleCompressor::Kraken),
    ("Mermaid", repak::OodleCompressor::Mermaid),
    ("Selkie", repak::OodleCompressor::Selkie),
    ("Leviathan", repak::OodleCompressor::Leviathan),
    ("Hydra", repak::OodleCompressor::Hydra),
];

/// Valid level range and default for a method (`None` = no levels).
pub fn level_range(c: PackCompression) -> Option<(i32, i32, i32)> {
    match c {
        PackCompression::Zlib | PackCompression::Gzip => Some((0, 9, 1)),
        PackCompression::Zstd => Some((1, 22, 3)),
        PackCompression::Oodle => Some((-4, 9, 4)),
        PackCompression::None | PackCompression::Lz4 => None,
    }
}

/// `6`, `-2`, or an Oodle level name (`Normal`, `Optimal3`, …).
pub fn parse_level(v: &str) -> Option<i32> {
    let v = v.trim();
    v.parse().ok().or_else(|| {
        OODLE_LEVELS
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(v))
            .map(|(_, l)| *l)
    })
}

impl PackOptions {
    /// Codec settings for repak; the level applies to the chosen method.
    pub fn compression_options(&self) -> repak::CompressionOptions {
        let mut o = repak::CompressionOptions {
            oodle_compressor: self.oodle_compressor,
            ..Default::default()
        };
        if let Some(l) = self.level {
            match self.compression {
                PackCompression::Zlib => o.zlib_level = l.max(0) as u32,
                PackCompression::Gzip => o.gzip_level = l.max(0) as u32,
                PackCompression::Zstd => o.zstd_level = l,
                PackCompression::Oodle => o.oodle_level = l,
                PackCompression::None | PackCompression::Lz4 => {}
            }
        }
        o
    }
}

pub const DEFAULT_PACK_MOUNT: &str = "../../../";
pub const DEFAULT_BLOCK_SIZE: u32 = 65536;

impl Default for PackOptions {
    fn default() -> Self {
        PackOptions {
            mount_point: DEFAULT_PACK_MOUNT.into(),
            version: repak::Version::V11,
            compression: PackCompression::None,
            block_size: DEFAULT_BLOCK_SIZE,
            level: None,
            oodle_compressor: repak::OodleCompressor::Mermaid,
            undo_limit: DEFAULT_UNDO_LIMIT,
        }
    }
}

/// `V11`, `v11`, `11`, `V8B`, `8b` -> version (V1 and V6 are not writable).
pub fn parse_version(v: &str) -> Option<repak::Version> {
    use repak::Version::*;
    let v = v.trim().to_ascii_uppercase();
    let v = v.strip_prefix('V').unwrap_or(&v);
    Some(match v {
        "2" => V2,
        "3" => V3,
        "4" => V4,
        "5" => V5,
        "7" => V7,
        "8A" => V8A,
        "8B" | "8" => V8B,
        "9" => V9,
        "10" => V10,
        "11" => V11,
        "12" => V12,
        _ => return None,
    })
}

/// `65536`, `64K`, `64KB`, `1M` -> bytes (at least 1 KB, at most 64 MB).
pub fn parse_size(v: &str) -> Option<u32> {
    let v = v.trim().to_ascii_uppercase();
    let v = v.strip_suffix('B').unwrap_or(&v);
    let (num, mul) = if let Some(n) = v.strip_suffix('K') {
        (n, 1024)
    } else if let Some(n) = v.strip_suffix('M') {
        (n, 1024 * 1024)
    } else {
        (v, 1)
    };
    let n: u64 = num.trim().parse().ok()?;
    let bytes = n.checked_mul(mul)?;
    (1024..=64 * 1024 * 1024)
        .contains(&bytes)
        .then_some(bytes as u32)
}

/// Normalises a mount point: `\\` -> `/`, empty -> `../../../`, otherwise as
/// typed (no trailing `/` added: Unreal adds it when mounting, and keeping
/// the text as typed makes output byte-identical to repak).
pub fn normalize_mount(v: &str) -> String {
    let v = v.trim().replace('\\', "/");
    if v.is_empty() {
        DEFAULT_PACK_MOUNT.into()
    } else {
        v
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mount_point: MountPointMode,
    /// Extra attributes on mount folders in `Folder` view, so TC can show them
    /// differently: `Hidden` (default: TC dims hidden items), `ReadOnly`,
    /// `System`, combinations (`ReadOnly,Hidden`) or `None`.
    pub mount_folder_attr: u32,
    /// `DebugLog=1`: write a diagnostic log (see `log.rs`).
    pub debug_log: bool,
    pub pack: PackOptions,
    /// Problems found while parsing (logged; invalid values keep defaults).
    pub warnings: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mount_point: MountPointMode::default(),
            mount_folder_attr: ATTR_HIDDEN,
            debug_log: false,
            pack: PackOptions::default(),
            warnings: vec![],
        }
    }
}

pub fn parse_attr_list(value: &str) -> u32 {
    value
        .split([',', '|', '+', ' '])
        .map(|v| match v.trim() {
            "readonly" | "r" => ATTR_READONLY,
            "hidden" | "h" => ATTR_HIDDEN,
            "system" | "s" => ATTR_SYSTEM,
            _ => 0,
        })
        .fold(0, |a, b| a | b)
}

static PLUGIN_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static TC_INI_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

pub fn plugin_dir() -> Option<&'static Path> {
    PLUGIN_DIR
        .get_or_init(|| crate::win::module_path().and_then(|p| p.parent().map(Path::to_path_buf)))
        .as_deref()
}

pub fn deps_dir() -> Option<PathBuf> {
    plugin_dir().map(|d| d.join(DEPS_DIR))
}

pub fn set_tc_ini_name(ini: &Path) {
    if let Some(dir) = ini.parent() {
        *TC_INI_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
    }
}

fn tc_ini_dir() -> Option<PathBuf> {
    TC_INI_DIR.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Can we create files in `dir`? (Shipping a file does not make its
/// directory writable: e.g. Program Files denies writes to non-admins.)
fn is_writable_dir(dir: &Path) -> bool {
    let probe = dir.join(".uepak-write-test");
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Directory for writable plugin data (options, remembered AES keys).
pub fn settings_dir() -> Option<PathBuf> {
    match plugin_dir() {
        Some(dir) if is_writable_dir(dir) => Some(dir.to_path_buf()),
        _ => tc_ini_dir().or_else(|| plugin_dir().map(Path::to_path_buf)),
    }
}

/// Loads options; checks the settings dir first, then the plugin dir
/// (the shipped default). Missing file or keys fall back to defaults.
pub fn load_options() -> Options {
    let candidates = [settings_dir(), plugin_dir().map(Path::to_path_buf)];
    for dir in candidates.into_iter().flatten() {
        if let Ok(text) = std::fs::read_to_string(dir.join(INI_NAME)) {
            return parse_options(&text);
        }
    }
    Options::default()
}

pub fn parse_options(text: &str) -> Options {
    let mut opts = Options::default();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // inline comments: "Key=Value   ; comment"
        let value = match value.find([';', '#']) {
            Some(i) if i > 0 && value[..i].ends_with([' ', '\t']) => &value[..i],
            _ => value,
        };
        if section == "pack" {
            let key = key.trim().to_ascii_lowercase().replace('_', "");
            let raw = value.trim();
            let mut bad = || {
                opts.warnings
                    .push(format!("[Pack] {key}={raw}: invalid, default kept"))
            };
            match key.as_str() {
                "mountpoint" => opts.pack.mount_point = normalize_mount(raw),
                "version" => match parse_version(raw) {
                    Some(v) => opts.pack.version = v,
                    None => bad(),
                },
                "compression" => match PackCompression::parse(raw) {
                    Some(c) => opts.pack.compression = c,
                    None => bad(),
                },
                "blocksize" => match parse_size(raw) {
                    Some(b) => opts.pack.block_size = b,
                    None => bad(),
                },
                "level" if raw.is_empty() || raw.eq_ignore_ascii_case("default") => {
                    opts.pack.level = None
                }
                "level" => match parse_level(raw) {
                    Some(l) => opts.pack.level = Some(l),
                    None => bad(),
                },
                "undolimit" => match parse_bytes(raw) {
                    Some(b) => opts.pack.undo_limit = b,
                    None => bad(),
                },
                "oodlecompressor" => match OODLE_COMPRESSORS
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(raw))
                {
                    Some((_, c)) => opts.pack.oodle_compressor = *c,
                    None => bad(),
                },
                _ => {}
            }
            continue;
        }
        if section != "options" {
            continue;
        }
        // Keys are case-insensitive and ignore '_' (DebugLog == debug_log).
        let key = key.trim().to_ascii_lowercase().replace('_', "");
        let value = value.trim().to_ascii_lowercase();
        match key.as_str() {
            "mountpoint" => {
                opts.mount_point = match value.as_str() {
                    "root" | "0" => MountPointMode::Root,
                    _ => MountPointMode::Folder,
                }
            }
            "mountfolderattr" => opts.mount_folder_attr = parse_attr_list(&value),
            "debuglog" => opts.debug_log = matches!(value.as_str(), "1" | "true" | "yes" | "on"),
            _ => {}
        }
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mount_point() {
        let o = parse_options("[Options]\nMountPoint = Folder\n");
        assert_eq!(o.mount_point, MountPointMode::Folder);
        let o = parse_options("; c\n[other]\nMountPoint=Root\n[options]\nmountpoint=root");
        assert_eq!(o.mount_point, MountPointMode::Root);
        assert_eq!(parse_options("").mount_point, MountPointMode::Folder);
    }

    #[test]
    fn parses_mount_folder_attr() {
        assert_eq!(Options::default().mount_folder_attr, ATTR_HIDDEN);
        let o = parse_options("[Options]\nMountFolderAttr=Hidden");
        assert_eq!(o.mount_folder_attr, ATTR_HIDDEN);
        let o = parse_options("[Options]\nmount_folder_attr = ReadOnly, System");
        assert_eq!(o.mount_folder_attr, ATTR_READONLY | ATTR_SYSTEM);
        assert_eq!(
            parse_options("[Options]\nMountFolderAttr=None").mount_folder_attr,
            0
        );
    }

    #[test]
    fn parses_pack_section() {
        let o = parse_options(
            "[Pack]\nMountPoint=..\\..\\..\\Townfall\\Config\nVersion=v12\nCompression=Oodle\nBlockSize=128K",
        );
        assert_eq!(o.pack.mount_point, "../../../Townfall/Config");
        assert_eq!(o.pack.version, repak::Version::V12);
        assert_eq!(o.pack.compression, PackCompression::Oodle);
        assert_eq!(o.pack.block_size, 131072);
        let d = parse_options("");
        assert_eq!(d.pack.mount_point, "../../../");
        assert_eq!(d.pack.version, repak::Version::V11);
        assert_eq!(d.pack.compression, PackCompression::None);
        assert_eq!(d.pack.block_size, 65536);
        let bad = parse_options("[Pack]\nVersion=V99\nCompression=brotli\nBlockSize=1");
        assert_eq!(bad.pack.version, repak::Version::V11);
        assert_eq!(bad.warnings.len(), 3);
        assert_eq!(
            parse_options("[Pack]\nMountPoint=").pack.mount_point,
            "../../../"
        );
        assert_eq!(parse_version("8a"), Some(repak::Version::V8A));
        let o = parse_options("[Pack]\nCompression=Oodle\nLevel=Optimal3\nOodleCompressor=kraken");
        assert_eq!(o.pack.level, Some(7));
        let c = o.pack.compression_options();
        assert_eq!(
            (c.oodle_level, c.oodle_compressor),
            (7, repak::OodleCompressor::Kraken)
        );
        let o = parse_options("[Pack]\nCompression=Zlib\nLevel=9");
        assert_eq!(o.pack.compression_options().zlib_level, 9);
        assert_eq!(parse_options("[Pack]\nLevel=").pack.level, None);
        assert_eq!(parse_options("[Pack]\nLevel=fast!").warnings.len(), 1);
        assert_eq!(d.pack.undo_limit, 1 << 30);
        assert_eq!(
            parse_options("[Pack]\nUndoLimit=4G").pack.undo_limit,
            4 << 30
        );
        assert_eq!(parse_options("[Pack]\nUndoLimit=0").pack.undo_limit, 0);
        assert_eq!(
            parse_options("[Pack]\nUndoLimit=unlimited").pack.undo_limit,
            u64::MAX
        );
        assert_eq!(parse_bytes("1.5G"), Some(3 << 29));
        assert_eq!(human_size(3 << 29), "1.5 GB");
        assert_eq!(human_size(640 << 10), "640 KB");
        assert_eq!(parse_size("1M"), Some(1 << 20));
    }

    #[test]
    fn parses_debug_log() {
        assert!(parse_options("[Options]\ndebug_log=1").debug_log);
        assert!(parse_options("[options]\nDebugLog = true").debug_log);
        assert!(!parse_options("[Options]\nDebugLog=0").debug_log);
        assert!(!parse_options("").debug_log);
    }
}
