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

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub mount_point: MountPointMode,
    /// `DebugLog=1`: write a diagnostic log (see `log.rs`).
    pub debug_log: bool,
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
    fn parses_debug_log() {
        assert!(parse_options("[Options]\ndebug_log=1").debug_log);
        assert!(parse_options("[options]\nDebugLog = true").debug_log);
        assert!(!parse_options("[Options]\nDebugLog=0").debug_log);
        assert!(!parse_options("").debug_log);
    }
}
