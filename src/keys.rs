//! AES keys: parsing, remembered keys (`uepak.keys.json`) and lookup order.
//!
//! Lookup (SPEC §3.5): a key remembered for the pak's folder or any parent
//! folder (deepest first) with the pak's key GUID; then, for a non-zero
//! GUID, any remembered key with that GUID. Keys also live in a session
//! cache so a key entered without "remember" isn't asked for again while
//! TC is running.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const KEYS_NAME: &str = "uepak.keys.json";
pub const ZERO_GUID: &str = "00000000000000000000000000000000";

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AesKey(pub [u8; 32]);

impl std::fmt::Debug for AesKey {
    // never print key material
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AesKey(..)")
    }
}

impl AesKey {
    /// Accepts 64 hex digits (optional `0x`, spaces/dashes ignored) or
    /// base64 of 32 bytes.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let compact: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect();
        let hex = compact
            .strip_prefix("0x")
            .or_else(|| compact.strip_prefix("0X"))
            .unwrap_or(&compact);
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let mut out = [0u8; 32];
            for (i, b) in out.iter_mut().enumerate() {
                *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
            }
            return Ok(AesKey(out));
        }
        if let Some(bytes) = base64_decode(&compact) {
            if let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) {
                return Ok(AesKey(arr));
            }
        }
        Err("Expected 64 hex digits (optionally starting with 0x) or base64 of 32 bytes.")
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::from("0x");
        for b in self.0 {
            s.push_str(&format!("{b:02X}"));
        }
        s
    }

    pub fn cipher(&self) -> aes::Aes256 {
        use aes::cipher::KeyInit;
        aes::Aes256::new_from_slice(&self.0).expect("32-byte key")
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyRecord {
    pub folder: String,
    pub guid: String,
    pub key: String,
    #[serde(default)]
    pub added: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KeyFile {
    format: u32,
    keys: Vec<KeyRecord>,
}

/// Keys entered this TC session (remembered or not).
static SESSION: Mutex<Vec<KeyRecord>> = Mutex::new(Vec::new());
/// Serialises read-modify-write of the keys file (background operations can
/// save keys from several threads at once).
static FILE_LOCK: Mutex<()> = Mutex::new(());

fn store_path() -> Option<PathBuf> {
    crate::config::settings_dir().map(|d| d.join(KEYS_NAME))
}

fn load_file(path: &Path) -> Vec<KeyRecord> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    match serde_json::from_str::<KeyFile>(&text) {
        Ok(f) => f.keys,
        Err(e) => {
            let bad = path.with_extension("json.bad");
            let _ = std::fs::rename(path, &bad);
            crate::dlog!(
                "keys: {} is invalid ({e}); moved to {}",
                path.display(),
                bad.display()
            );
            vec![]
        }
    }
}

fn save_file(path: &Path, keys: &[KeyRecord]) -> std::io::Result<()> {
    let file = KeyFile {
        format: 1,
        keys: keys.to_vec(),
    };
    let text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path) // replaces atomically on Windows (MoveFileEx)
}

/// Normalised folder for comparisons: lowercase, `\`, no trailing separator.
pub fn norm_folder(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Is `folder` equal to `ancestor` or below it?
fn is_within(folder: &str, ancestor: &str) -> bool {
    folder == ancestor
        || (folder.starts_with(ancestor) && folder.as_bytes().get(ancestor.len()) == Some(&b'\\'))
}

/// Where a candidate key came from (for logging and for remembering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Remembered for this folder or a parent.
    Folder,
    /// Same non-zero GUID, another folder.
    Guid,
}

/// Ordered, de-duplicated candidate keys for a pak in `folder` with `guid`.
pub fn candidates(folder: &Path, guid: &str) -> Vec<(AesKey, Source, String)> {
    let folder = norm_folder(&folder.to_string_lossy());
    let guid = guid.to_ascii_uppercase();
    let mut records = SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(path) = store_path() {
        records.extend(load_file(&path));
    }
    let mut by_folder: Vec<&KeyRecord> = records
        .iter()
        .filter(|r| {
            r.guid.eq_ignore_ascii_case(&guid) && is_within(&folder, &norm_folder(&r.folder))
        })
        .collect();
    by_folder.sort_by_key(|r| std::cmp::Reverse(norm_folder(&r.folder).len()));
    let mut out: Vec<(AesKey, Source, String)> = vec![];
    let mut push = |r: &KeyRecord, src| {
        if let Ok(k) = AesKey::parse(&r.key) {
            if !out.iter().any(|(o, _, _)| *o == k) {
                out.push((k, src, r.folder.clone()));
            }
        }
    };
    for r in by_folder {
        push(r, Source::Folder);
    }
    if guid != ZERO_GUID {
        for r in records
            .iter()
            .filter(|r| r.guid.eq_ignore_ascii_case(&guid))
        {
            push(r, Source::Guid);
        }
    }
    out
}

/// Records a working key for `folder` + `guid`: always in the session cache,
/// and in `uepak.keys.json` when `persist`.
pub fn remember(folder: &Path, guid: &str, key: &AesKey, persist: bool) -> std::io::Result<()> {
    let record = KeyRecord {
        folder: folder
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string(),
        guid: guid.to_ascii_uppercase(),
        key: key.to_hex(),
        added: crate::win::local_timestamp(),
    };
    let same = |r: &KeyRecord| {
        norm_folder(&r.folder) == norm_folder(&record.folder)
            && r.guid.eq_ignore_ascii_case(&record.guid)
    };
    {
        let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        session.retain(|r| !same(r));
        session.push(record.clone());
    }
    if !persist {
        return Ok(());
    }
    let path = store_path().ok_or_else(|| std::io::Error::other("no settings directory"))?;
    let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut keys = load_file(&path);
    keys.retain(|r| !same(r));
    keys.push(record);
    save_file(&path, &keys)
}

#[cfg(test)]
pub fn clear_session() {
    SESSION.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "9575FC2B9E612ADAB80906DB3A176591ECB47181DDA631000BD7FC4C875282E6";

    #[test]
    fn parses_hex_and_base64() {
        let a = AesKey::parse(&format!("0x{HEX}")).unwrap();
        assert_eq!(AesKey::parse(HEX).unwrap(), a);
        assert_eq!(AesKey::parse(&HEX.to_lowercase()).unwrap(), a);
        assert_eq!(
            AesKey::parse(&format!(" 0x{} {} ", &HEX[..32], &HEX[32..])).unwrap(),
            a
        );
        assert_eq!(a.to_hex(), format!("0x{HEX}"));
        // repak's test key
        let b = AesKey::parse("lNJbw660IOC+kU7cnVQ1oeqrXyhk4J6UAZrCBbcnp94=").unwrap();
        assert_eq!(b.0[0], 0x94);
        assert!(AesKey::parse("1234").is_err());
        assert!(AesKey::parse(&HEX[..62]).is_err());
        assert_eq!(format!("{a:?}"), "AesKey(..)");
    }

    #[test]
    fn folder_matching() {
        assert!(is_within("d:\\g\\paks\\~mods", "d:\\g\\paks"));
        assert!(is_within("d:\\g\\paks", "d:\\g\\paks"));
        assert!(!is_within("d:\\g\\paks2", "d:\\g\\paks"));
        assert!(!is_within("d:\\g", "d:\\g\\paks"));
        assert_eq!(norm_folder("D:/G/Paks/"), "d:\\g\\paks");
    }
}
