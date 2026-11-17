//! Settings model behind the Configure dialog (SPEC §5): choice lists,
//! warnings, and saving to `uepak.ini` without losing comments.

use crate::config::{
    self, level_range, MountPointMode, Options, PackCompression, ATTR_HIDDEN, ATTR_READONLY,
    ATTR_SYSTEM, OODLE_COMPRESSORS, OODLE_LEVELS,
};
use repak::Version;
use std::path::{Path, PathBuf};

pub const VERSIONS: [(Version, &str); 11] = [
    (Version::V12, "V12 (UE 5.3+)"),
    (Version::V11, "V11 (UE 4.26 - 5.x)"),
    (Version::V10, "V10"),
    (Version::V9, "V9 (UE 4.25)"),
    (Version::V8B, "V8B (UE 4.23 - 4.24)"),
    (Version::V8A, "V8A (UE 4.22)"),
    (Version::V7, "V7 (UE 4.21)"),
    (Version::V5, "V5 (UE 4.20)"),
    (Version::V4, "V4 (UE 4.16 - 4.19)"),
    (Version::V3, "V3 (UE 4.3 - 4.15)"),
    (Version::V2, "V2 (UE 4.0 - 4.2)"),
];

pub const COMPRESSIONS: [(PackCompression, &str); 6] = [
    (PackCompression::None, "None"),
    (PackCompression::Oodle, "Oodle"),
    (PackCompression::Zlib, "Zlib"),
    (PackCompression::Gzip, "Gzip"),
    (PackCompression::Zstd, "Zstd"),
    (PackCompression::Lz4, "LZ4"),
];

/// Oodle compressors in dialog order (same as config::OODLE_COMPRESSORS).
pub const OODLE_COMPRESSORS_UI: [(repak::OodleCompressor, &str); 5] = [
    (repak::OodleCompressor::Kraken, "Kraken"),
    (repak::OodleCompressor::Mermaid, "Mermaid (UE default)"),
    (repak::OodleCompressor::Selkie, "Selkie"),
    (repak::OodleCompressor::Leviathan, "Leviathan"),
    (repak::OodleCompressor::Hydra, "Hydra"),
];

pub const BLOCK_SIZES: [(u32, &str); 5] = [
    (65536, "64 KB (UnrealPak default)"),
    (126976, "124 KB (largest for older engines)"),
    (262144, "256 KB"),
    (524288, "512 KB"),
    (1048576, "1 MB"),
];

pub const ATTRS: [(u32, &str); 5] = [
    (ATTR_READONLY, "Read-only"),
    (ATTR_HIDDEN, "Hidden"),
    (ATTR_SYSTEM, "System"),
    (ATTR_READONLY | ATTR_HIDDEN, "Read-only + Hidden"),
    (0, "None"),
];

/// Everything the dialog edits.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub pack_mount: String,
    pub version: Version,
    pub compression: PackCompression,
    pub level: Option<i32>,
    pub oodle_compressor: repak::OodleCompressor,
    pub block_size: u32,
    pub view: MountPointMode,
    pub mount_folder_attr: u32,
    pub debug_log: bool,
}

impl Model {
    pub fn from_options(o: &Options) -> Self {
        Model {
            pack_mount: o.pack.mount_point.clone(),
            version: o.pack.version,
            compression: o.pack.compression,
            level: o.pack.level,
            oodle_compressor: o.pack.oodle_compressor,
            block_size: o.pack.block_size,
            view: o.mount_point,
            mount_folder_attr: o.mount_folder_attr,
            debug_log: o.debug_log,
        }
    }

    pub fn defaults() -> Self {
        Self::from_options(&Options::default())
    }

    /// Level choices for the current method: (value, label); first = default.
    pub fn level_choices(&self) -> Vec<(Option<i32>, String)> {
        let Some((lo, hi, def)) = level_range(self.compression) else {
            return vec![(None, "(not used)".into())];
        };
        let mut v = vec![];
        if self.compression == PackCompression::Oodle {
            let def_name = OODLE_LEVELS
                .iter()
                .find(|(_, l)| *l == def)
                .map(|(n, _)| *n);
            v.push((None, format!("Default ({})", def_name.unwrap_or("Normal"))));
            for (name, l) in OODLE_LEVELS {
                v.push((Some(l), name.to_string()));
            }
        } else {
            v.push((None, format!("Default ({def})")));
            for l in lo..=hi {
                v.push((Some(l), l.to_string()));
            }
        }
        v
    }

    /// Block size choices; a non-standard current value is listed too.
    pub fn block_choices(&self) -> Vec<(u32, String)> {
        let mut v: Vec<(u32, String)> = BLOCK_SIZES
            .iter()
            .map(|(b, l)| (*b, l.to_string()))
            .collect();
        if !v.iter().any(|(b, _)| *b == self.block_size) {
            v.push((
                self.block_size,
                format!("{} bytes (custom)", self.block_size),
            ));
        }
        v
    }

    pub fn attr_choices(&self) -> Vec<(u32, String)> {
        let mut v: Vec<(u32, String)> = ATTRS.iter().map(|(a, l)| (*a, l.to_string())).collect();
        if !v.iter().any(|(a, _)| *a == self.mount_folder_attr) {
            v.push((
                self.mount_folder_attr,
                format!("Custom ({:#x})", self.mount_folder_attr),
            ));
        }
        v
    }

    pub fn uses_levels(&self) -> bool {
        level_range(self.compression).is_some()
    }

    pub fn uses_oodle_compressor(&self) -> bool {
        self.compression == PackCompression::Oodle
    }

    /// Warnings shown under the packing settings (empty = all fine).
    pub fn warnings(&self, oodle_status: &Result<PathBuf, String>) -> Vec<String> {
        let mut w = vec![];
        let opts = config::PackOptions {
            mount_point: self.pack_mount.clone(),
            version: self.version,
            compression: self.compression,
            block_size: self.block_size,
            level: self.level,
            oodle_compressor: self.oodle_compressor,
        };
        if let Err(e) = crate::pack::check_settings(&opts) {
            w.push(e);
        }
        match self.compression {
            PackCompression::Zstd | PackCompression::Lz4 => {
                w.push("Only games built with this compression can read it.".into())
            }
            PackCompression::Oodle => match oodle_status {
                Ok(p) => w.push(format!(
                    "Oodle: {}",
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )),
                Err(e) => w.push(format!("Oodle: {e}. Copy oo2core_N_win64.dll into deps\\.")),
            },
            _ => {}
        }
        if self.compression != PackCompression::None && self.block_size > 126976 {
            w.push("Blocks over 124 KB can't be read by older engine versions.".into());
        }
        w
    }

    /// (section, key, value) for every setting.
    pub fn ini_values(&self) -> Vec<(&'static str, &'static str, String)> {
        let compression = COMPRESSIONS
            .iter()
            .find(|(c, _)| *c == self.compression)
            .map(|(_, n)| n.to_string())
            .unwrap_or_default();
        let level = match (self.level, self.compression) {
            (None, _) => String::new(),
            (Some(l), PackCompression::Oodle) => OODLE_LEVELS
                .iter()
                .find(|(_, v)| *v == l)
                .map(|(n, _)| n.to_string())
                .unwrap_or_else(|| l.to_string()),
            (Some(l), _) => l.to_string(),
        };
        let oodle = OODLE_COMPRESSORS
            .iter()
            .find(|(_, c)| *c == self.oodle_compressor)
            .map(|(n, _)| n.to_string())
            .unwrap_or_default();
        let attr = {
            let mut parts = vec![];
            if self.mount_folder_attr & ATTR_READONLY != 0 {
                parts.push("ReadOnly");
            }
            if self.mount_folder_attr & ATTR_HIDDEN != 0 {
                parts.push("Hidden");
            }
            if self.mount_folder_attr & ATTR_SYSTEM != 0 {
                parts.push("System");
            }
            if parts.is_empty() {
                "None".to_string()
            } else {
                parts.join(",")
            }
        };
        vec![
            (
                "Options",
                "MountPoint",
                match self.view {
                    MountPointMode::Folder => "Folder".into(),
                    MountPointMode::Root => "Root".into(),
                },
            ),
            ("Options", "MountFolderAttr", attr),
            (
                "Options",
                "DebugLog",
                if self.debug_log { "1" } else { "0" }.into(),
            ),
            (
                "Pack",
                "MountPoint",
                config::normalize_mount(&self.pack_mount),
            ),
            ("Pack", "Version", format!("{}", self.version)),
            ("Pack", "Compression", compression),
            ("Pack", "Level", level),
            ("Pack", "OodleCompressor", oodle),
            ("Pack", "BlockSize", self.block_size.to_string()),
        ]
    }
}

fn norm_key(k: &str) -> String {
    k.trim().to_ascii_lowercase().replace('_', "")
}

/// Sets `key=value` in `[section]` of an ini text, keeping everything else
/// (comments, order, other keys). Adds the key or section if missing.
pub fn ini_set(text: &str, section: &str, key: &str, value: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let want_sec = section.to_ascii_lowercase();
    let want_key = norm_key(key);
    let mut in_sec = false;
    let mut sec_found = false;
    let mut last_in_sec: Option<usize> = None;
    for i in 0..lines.len() {
        let t = lines[i].trim();
        if let Some(name) = t.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_sec = name.trim().eq_ignore_ascii_case(&want_sec);
            if in_sec {
                sec_found = true;
                last_in_sec = Some(i);
            }
            continue;
        }
        if !in_sec {
            continue;
        }
        if !t.is_empty() && !t.starts_with(';') && !t.starts_with('#') {
            last_in_sec = Some(i);
        }
        if let Some((k, _)) = t.split_once('=') {
            if norm_key(k) == want_key && !t.starts_with(';') && !t.starts_with('#') {
                let indent = &lines[i][..lines[i].len() - lines[i].trim_start().len()];
                lines[i] = format!("{indent}{}={value}", k.trim());
                return lines.join(nl) + nl;
            }
        }
    }
    let new_line = format!("{key}={value}");
    match (sec_found, last_in_sec) {
        (true, Some(i)) => lines.insert(i + 1, new_line),
        _ => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(new_line);
        }
    }
    lines.join(nl) + nl
}

/// Where the dialog saves: `uepak.ini` in the settings directory.
pub fn ini_path() -> Option<PathBuf> {
    config::settings_dir().map(|d| d.join(config::INI_NAME))
}

/// Saves the model. If the settings directory has no `uepak.ini` yet, the
/// shipped one is copied first so its comments survive.
pub fn save(model: &Model, path: &Path) -> std::io::Result<()> {
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    if text.is_empty() {
        if let Some(shipped) = config::plugin_dir().map(|d| d.join(config::INI_NAME)) {
            if shipped != path {
                text = std::fs::read_to_string(shipped).unwrap_or_default();
            }
        }
    }
    for (sec, key, val) in model.ini_values() {
        text = ini_set(&text, sec, key, &val);
    }
    let tmp = path.with_extension("ini.tmp");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, path)
}

/// Oodle library that would be used (for the dialog's status line).
pub fn oodle_status() -> Result<PathBuf, String> {
    oodle_loader::oodle()
        .map(|o| o.path().to_path_buf())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_set_keeps_comments_and_order() {
        let t = "; top\n[Options]\n; view\nMountPoint=Root\n\n[Pack]\n; v\nVersion=V11\n";
        let t2 = ini_set(t, "Options", "MountPoint", "Folder");
        assert_eq!(
            t2,
            "; top\n[Options]\n; view\nMountPoint=Folder\n\n[Pack]\n; v\nVersion=V11\n"
        );
        let t3 = ini_set(&t2, "Pack", "Level", "9");
        assert!(t3.contains("Version=V11\nLevel=9\n"), "{t3}");
        let t4 = ini_set(&t3, "New", "Key", "x");
        assert!(t4.ends_with("\n[New]\nKey=x\n"), "{t4}");
        // key spelling variants match; commented-out keys are left alone
        let t5 = ini_set(
            "[Options]\n;DebugLog=1\ndebug_log=0\n",
            "Options",
            "DebugLog",
            "1",
        );
        assert_eq!(t5, "[Options]\n;DebugLog=1\ndebug_log=1\n");
        // CRLF preserved
        assert_eq!(ini_set("[A]\r\nk=1\r\n", "A", "k", "2"), "[A]\r\nk=2\r\n");
    }

    #[test]
    fn model_roundtrips_through_ini() {
        let mut m = Model::defaults();
        m.pack_mount = "..\\..\\..\\G1R\\Config".into();
        m.version = Version::V12;
        m.compression = PackCompression::Oodle;
        m.level = Some(7);
        m.oodle_compressor = repak::OodleCompressor::Kraken;
        m.block_size = 262144;
        m.view = MountPointMode::Root;
        m.mount_folder_attr = ATTR_HIDDEN;
        m.debug_log = true;
        let mut text = String::new();
        for (s, k, v) in m.ini_values() {
            text = ini_set(&text, s, k, &v);
        }
        let back = Model::from_options(&config::parse_options(&text));
        assert_eq!(back.pack_mount, "../../../G1R/Config");
        assert_eq!(
            Model {
                pack_mount: back.pack_mount.clone(),
                ..m.clone()
            },
            back,
            "{text}"
        );
        assert!(config::parse_options(&text).warnings.is_empty());
        // shipped ini parses into the defaults
        let shipped = include_str!("../uepak.ini");
        assert_eq!(
            Model::from_options(&config::parse_options(shipped)),
            Model::defaults()
        );
    }

    #[test]
    fn choices_and_warnings() {
        let mut m = Model::defaults();
        assert!(!m.uses_levels());
        assert!(m.warnings(&Err("x".into())).is_empty());
        m.compression = PackCompression::Zlib;
        assert_eq!(m.level_choices().len(), 11); // default + 0..9
        m.compression = PackCompression::Oodle;
        assert_eq!(m.level_choices()[0].1, "Default (Normal)");
        assert!(m.warnings(&Err("missing".into()))[0].contains("deps"));
        m.compression = PackCompression::Lz4;
        m.version = Version::V7;
        let w = m.warnings(&Err("x".into()));
        assert!(w.iter().any(|s| s.contains("V8A")), "{w:?}");
        m.block_size = 12345;
        assert!(m.block_choices().last().unwrap().1.contains("custom"));
    }
}
