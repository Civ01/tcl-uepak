//! Unit tests that drive the exported WCX functions the way Total Commander does.
//! Sample paks from real games are not committed; set UEPAK_SAMPLES to a
//! folder with *.pak files to include them.

use crate::ffi::*;
use crate::*;
use std::path::{Path, PathBuf};

fn wide(s: &str) -> Vec<u16> {
    win::wide(s)
}

struct Listed {
    name: String,
    pack: u64,
    unpack: u64,
    attr: i32,
}

/// Files only (mount folder entries filtered out).
fn list(path: &Path) -> Result<Vec<Listed>, i32> {
    list_all(path).map(|v| v.into_iter().filter(|l| l.attr & 0x10 == 0).collect())
}

fn list_all(path: &Path) -> Result<Vec<Listed>, i32> {
    let name = wide(&path.to_string_lossy());
    let mut data = OpenArchiveDataW {
        ArcName: name.as_ptr(),
        OpenMode: PK_OM_LIST,
        OpenResult: 0,
        CmtBuf: std::ptr::null_mut(),
        CmtBufSize: 0,
        CmtSize: 0,
        CmtState: 0,
    };
    let h = unsafe { OpenArchiveW(&mut data) };
    if h.is_null() {
        return Err(data.OpenResult);
    }
    let mut out = vec![];
    loop {
        let mut hd: Box<HeaderDataExW> = unsafe { Box::new(std::mem::zeroed()) };
        let r = unsafe { ReadHeaderExW(h, &mut *hd) };
        if r == E_END_ARCHIVE {
            break;
        }
        assert_eq!(r, 0);
        let end = hd.FileName.iter().position(|&c| c == 0).unwrap();
        out.push(Listed {
            name: String::from_utf16(&hd.FileName[..end]).unwrap(),
            pack: hd.PackSize as u64 | (hd.PackSizeHigh as u64) << 32,
            unpack: hd.UnpSize as u64 | (hd.UnpSizeHigh as u64) << 32,
            attr: hd.FileAttr,
        });
        assert_eq!(
            unsafe { ProcessFileW(h, PK_SKIP, std::ptr::null(), std::ptr::null()) },
            0
        );
    }
    assert_eq!(unsafe { CloseArchive(h) }, 0);
    Ok(out)
}

/// Extracts every entry; returns (name, bytes) pairs.
fn extract_all(path: &Path, dest: &Path) -> Vec<(String, Vec<u8>)> {
    let name = wide(&path.to_string_lossy());
    let mut data = OpenArchiveDataW {
        ArcName: name.as_ptr(),
        OpenMode: PK_OM_EXTRACT,
        OpenResult: 0,
        CmtBuf: std::ptr::null_mut(),
        CmtBufSize: 0,
        CmtSize: 0,
        CmtState: 0,
    };
    let h = unsafe { OpenArchiveW(&mut data) };
    assert!(!h.is_null(), "open failed: {}", data.OpenResult);
    let mut out = vec![];
    loop {
        let mut hd: Box<HeaderDataExW> = unsafe { Box::new(std::mem::zeroed()) };
        if unsafe { ReadHeaderExW(h, &mut *hd) } == E_END_ARCHIVE {
            break;
        }
        let end = hd.FileName.iter().position(|&c| c == 0).unwrap();
        let rel = String::from_utf16(&hd.FileName[..end]).unwrap();
        let target = dest.join(rel.replace('\\', "/"));
        let target_w = wide(&target.to_string_lossy());
        let r = unsafe { ProcessFileW(h, PK_EXTRACT, std::ptr::null(), target_w.as_ptr()) };
        assert_eq!(r, 0, "extract {rel}");
        assert_eq!(
            unsafe { ProcessFileW(h, PK_TEST, std::ptr::null(), std::ptr::null()) },
            0
        );
        if hd.FileAttr & 0x10 != 0 {
            assert!(target.is_dir(), "mount folder {rel} created");
            continue;
        }
        out.push((rel, std::fs::read(&target).unwrap()));
    }
    unsafe { CloseArchive(h) };
    out
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("uepak-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn make_pak(path: &Path, compression: Option<repak::Compression>, files: &[(&str, Vec<u8>)]) {
    let f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut b = repak::PakBuilder::new();
    if let Some(c) = compression {
        b = b.compression([c]);
    }
    let mut w = b.writer(
        f,
        repak::Version::V11,
        "../../../TestGame/Content/".into(),
        None,
    );
    for (name, data) in files {
        w.write_file(name, compression.is_some(), data).unwrap();
    }
    w.write_index().unwrap();
}

fn payload() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("Maps/Test.umap", b"umap".repeat(5000)),
        (
            "Config/DefaultGame.ini",
            b"[/Script/Engine]\nfoo=bar\n".to_vec(),
        ),
        ("Empty.bin", vec![]),
        ("Big/zeros.bin", vec![0u8; 3 * 65536 + 17]),
    ]
}

#[test]
fn roundtrip_synthetic_paks() {
    for comp in [
        None,
        Some(repak::Compression::Zlib),
        Some(repak::Compression::Gzip),
        Some(repak::Compression::Zstd),
        Some(repak::Compression::LZ4),
    ] {
        let dir = tmp(&format!("{comp:?}"));
        let pak = dir.join("test.pak");
        let files = payload();
        make_pak(&pak, comp, &files);
        assert!(archive::looks_like_pak(&pak));

        let listed = list(&pak).unwrap();
        assert_eq!(listed.len(), files.len(), "{comp:?}");
        for (name, data) in &files {
            let shown = format!("TestGame\\Content\\{}", name.replace('/', "\\"));
            let l = listed.iter().find(|l| l.name == shown).unwrap();
            assert_eq!(l.unpack, data.len() as u64);
            if comp.is_some() && !data.is_empty() && data.len() > 1000 {
                assert!(
                    l.pack < l.unpack,
                    "{comp:?} {name} should be smaller packed"
                );
            }
        }

        let out = extract_all(&pak, &dir.join("out"));
        for (name, data) in &files {
            let shown = format!("TestGame\\Content\\{}", name.replace('/', "\\"));
            let got = out.iter().find(|(n, _)| *n == shown).unwrap();
            assert_eq!(&got.1, data, "{comp:?} {name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn rejects_non_pak() {
    let dir = tmp("nonpak");
    let p = dir.join("x.pak");
    std::fs::write(&p, vec![7u8; 4096]).unwrap();
    assert!(!archive::looks_like_pak(&p));
    assert!(list(&p).is_err());
    assert_eq!(list(&dir.join("missing.pak")).err(), Some(E_EOPEN));
}

#[test]
fn parses_tc_multi_string_lists() {
    let list: Vec<u16> = "a.txt\0dir\\b.txt\0\0".encode_utf16().collect();
    assert_eq!(
        unsafe { win::from_wide_multi(list.as_ptr()) },
        vec!["a.txt", "dir\\b.txt"]
    );
    assert!(unsafe { win::from_wide_multi(std::ptr::null()) }.is_empty());
}

#[test]
fn sample_paks() {
    let Ok(dir) = std::env::var("UEPAK_SAMPLES") else {
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pak")))
        .collect();
    paths.sort();
    for p in paths {
        let sniff = archive::sniff_footer(&p).expect("footer");
        if sniff.encrypted_index == Some(true) {
            let _g = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
            crate::dialog::TEST_ANSWERS.lock().unwrap().clear();
            assert_eq!(list(&p).err(), Some(E_EABORTED), "{}", p.display());
            println!(
                "{}: encrypted, no key -> prompt cancelled (expected)",
                p.display()
            );
            continue;
        }
        let listed = list(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let total: u64 = listed.iter().map(|l| l.unpack).sum();
        let oodle = sniff
            .compression
            .iter()
            .any(|c| c.eq_ignore_ascii_case("oodle"));
        if oodle && oodle_loader::oodle().is_err() {
            println!(
                "{}: v{} {} entries, {} bytes (Oodle lib unavailable: listed only)",
                p.display(),
                sniff.version,
                listed.len(),
                total
            );
            continue;
        }
        let out_dir = tmp("sample");
        let out = extract_all(&p, &out_dir);
        assert_eq!(out.len(), listed.len());
        for (l, (_, data)) in listed.iter().zip(&out) {
            assert_eq!(l.unpack, data.len() as u64, "{}", l.name);
        }
        println!(
            "{}: v{} [{}] {} entries, {} bytes extracted + tested",
            p.display(),
            sniff.version,
            sniff.compression.join(","),
            listed.len(),
            total
        );
        let _ = std::fs::remove_dir_all(&out_dir);
    }
}

#[test]
fn guard_turns_panic_into_fallback() {
    let v = crate::guard(42, "test", || -> i32 { panic!("boom") });
    assert_eq!(v, 42);
}

#[test]
fn null_and_bad_handles_do_not_crash() {
    unsafe {
        assert_eq!(
            ReadHeaderExW(std::ptr::null_mut(), std::ptr::null_mut()),
            E_BAD_ARCHIVE
        );
        assert_eq!(
            ProcessFileW(
                std::ptr::null_mut(),
                PK_EXTRACT,
                std::ptr::null(),
                std::ptr::null()
            ),
            E_BAD_ARCHIVE
        );
        assert_eq!(CloseArchive(std::ptr::null_mut()), 0);
        assert!(OpenArchiveW(std::ptr::null_mut()).is_null());
    }
}

/// Patches a freshly written v12 pak: `f(bytes, magic_pos)`.
fn patched_pak(name: &str, f: impl FnOnce(&mut Vec<u8>, usize)) -> PathBuf {
    let dir = tmp(name);
    let pak = dir.join("p.pak");
    make_pak_v(&pak, repak::Version::V12, &payload());
    let mut bytes = std::fs::read(&pak).unwrap();
    let magic = archive::PAK_MAGIC.to_le_bytes();
    let pos = bytes.windows(4).rposition(|w| w == magic).unwrap();
    f(&mut bytes, pos);
    std::fs::write(&pak, bytes).unwrap();
    pak
}

fn make_pak_v(path: &Path, version: repak::Version, files: &[(&str, Vec<u8>)]) {
    let f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut w = repak::PakBuilder::new().writer(f, version, "../../../G/".into(), None);
    for (name, data) in files {
        w.write_file(name, false, data).unwrap();
    }
    w.write_index().unwrap();
}

#[test]
fn v12_pak_lists_and_extracts() {
    let dir = tmp("v12");
    let pak = dir.join("v12.pak");
    make_pak_v(&pak, repak::Version::V12, &payload());
    let sniff = archive::sniff_footer(&pak).unwrap();
    assert_eq!(sniff.version, 12);
    assert_eq!(list(&pak).unwrap().len(), payload().len());
    assert_eq!(extract_all(&pak, &dir.join("out")).len(), payload().len());
}

#[test]
fn encrypted_index_without_key_prompts() {
    let _g = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::dialog::TEST_ANSWERS.lock().unwrap().clear();
    // Flip the "encrypted index" byte that precedes the magic.
    let pak = patched_pak("enc", |b, pos| b[pos - 1] = 1);
    let sniff = archive::sniff_footer(&pak).unwrap();
    assert_eq!(sniff.encrypted_index, Some(true));
    // no answer queued = the user cancels the prompt
    assert_eq!(list(&pak).err(), Some(E_EABORTED));
}

#[test]
fn future_version_is_reported_as_unknown_format() {
    let pak = patched_pak("v99", |b, pos| {
        b[pos + 4..pos + 8].copy_from_slice(&99u32.to_le_bytes())
    });
    assert_eq!(archive::sniff_footer(&pak).unwrap().version, 99);
    assert_eq!(list(&pak).err(), Some(E_UNKNOWN_FORMAT));
}

#[test]
fn debug_log_writes_to_plugin_dir() {
    // pin logging on: other tests reload options (DebugLog=0) concurrently
    crate::log::FORCE.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::log::configure(true);
    crate::dlog!("hello from test {}", 42);
    let path = crate::config::plugin_dir()
        .unwrap()
        .join(crate::log::LOG_NAME);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("hello from test 42"));
    assert!(crate::log::on_error().unwrap().contains("uepak.log"));
}

/// Manual diagnostic: `UEPAK_DEBUG_PAK=<file> cargo test debug_open_env_pak -- --nocapture`
/// opens one pak with debug logging on and prints the result code.
#[test]
fn debug_open_env_pak() {
    let Ok(p) = std::env::var("UEPAK_DEBUG_PAK") else {
        return;
    };
    crate::log::FORCE.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::log::configure(true);
    crate::log_environment(&crate::config::Options::default());
    println!("list result: {:?}", list(Path::new(&p)).map(|l| l.len()));
}

const TEST_KEY_B64: &str = "lNJbw660IOC+kU7cnVQ1oeqrXyhk4J6UAZrCBbcnp94=";

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name)
}

fn answer(a: Option<(&str, bool)>) {
    crate::dialog::TEST_ANSWERS
        .lock()
        .unwrap()
        .push(a.map(|(t, r)| (t.to_string(), r)));
}

/// Serialises tests that use the prompt queue / key stores.
static KEY_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn copy_to_tmp(name: &str, src: &Path) -> PathBuf {
    let dir = tmp(name);
    let p = dir.join(src.file_name().unwrap());
    std::fs::copy(src, &p).unwrap();
    p
}

fn keys_file() -> PathBuf {
    crate::config::settings_dir()
        .unwrap()
        .join(crate::keys::KEYS_NAME)
}

#[test]
fn encrypted_index_prompts_validates_and_remembers() {
    let _g = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::keys::clear_session();
    let _ = std::fs::remove_file(keys_file());
    crate::dialog::TEST_ANSWERS.lock().unwrap().clear();
    let pak = copy_to_tmp("enc-index", &testdata("enc_index.pak"));

    // cancel -> E_EABORTED, and no second prompt right after
    answer(None);
    assert_eq!(list(&pak).err(), Some(E_EABORTED));
    assert_eq!(list(&pak).err(), Some(E_EABORTED));
    assert!(crate::dialog::TEST_ANSWERS.lock().unwrap().is_empty());
    super::DECLINED.lock().unwrap().clear();

    // bad format, then wrong key, then the right one (session only)
    answer(Some(("12345", false)));
    answer(Some((&"11".repeat(32), false)));
    answer(Some((TEST_KEY_B64, false)));
    let listed = list(&pak).unwrap();
    assert_eq!(listed.len(), 4);
    assert!(crate::dialog::TEST_ANSWERS.lock().unwrap().is_empty());
    assert!(!keys_file().exists(), "not persisted without 'remember'");

    // session cache: no prompt for the extract pass
    let out = extract_all(&pak, &pak.parent().unwrap().join("out"));
    assert_eq!(out.len(), 4);

    // remembered key, then a fresh session: found in the file, also for a subfolder
    crate::keys::clear_session();
    answer(Some((TEST_KEY_B64, true)));
    assert_eq!(list(&pak).unwrap().len(), 4);
    let saved = std::fs::read_to_string(keys_file()).unwrap();
    assert!(saved.contains("\"format\": 1"), "{saved}");
    crate::keys::clear_session();
    let sub = pak.parent().unwrap().join("~mods");
    std::fs::create_dir_all(&sub).unwrap();
    let pak2 = sub.join("enc_index.pak");
    std::fs::copy(&pak, &pak2).unwrap();
    assert_eq!(list(&pak2).unwrap().len(), 4, "parent folder's key applies");
    assert!(crate::dialog::TEST_ANSWERS.lock().unwrap().is_empty());

    // a corrupt keys file is set aside, not fatal
    std::fs::write(keys_file(), "{ not json").unwrap();
    crate::keys::clear_session();
    answer(None);
    assert_eq!(list(&pak).err(), Some(E_EABORTED));
    assert!(keys_file().with_extension("json.bad").exists());
    let _ = std::fs::remove_file(keys_file().with_extension("json.bad"));
    let _ = std::fs::remove_file(keys_file());
    super::DECLINED.lock().unwrap().clear();
}

#[test]
fn encrypted_entries_ask_for_key_on_extract() {
    let _g = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::keys::clear_session();
    let _ = std::fs::remove_file(keys_file());
    crate::dialog::TEST_ANSWERS.lock().unwrap().clear();
    let pak = copy_to_tmp("enc-entries", &testdata("enc_entries.pak"));
    // listing needs no key
    assert_eq!(list(&pak).unwrap().len(), 4);
    assert!(crate::dialog::TEST_ANSWERS.lock().unwrap().is_empty());
    // extraction asks once, then all entries come out right
    answer(Some((TEST_KEY_B64, false)));
    let out = extract_all(&pak, &pak.parent().unwrap().join("out"));
    assert_eq!(out.len(), 4);
    let txt = out.iter().find(|(n, _)| n.ends_with("test.txt")).unwrap();
    assert_eq!(
        txt.1,
        std::fs::read(testdata("test.txt")).unwrap(),
        "decrypted"
    );
    crate::keys::clear_session();
}

/// Optional: real encrypted pak + its key, from the environment only
/// (`UEPAK_KEYED_PAK=<file>`, `UEPAK_KEYED_KEY=<key>`); keys never committed.
#[test]
fn real_encrypted_pak_from_env() {
    let (Ok(p), Ok(k)) = (
        std::env::var("UEPAK_KEYED_PAK"),
        std::env::var("UEPAK_KEYED_KEY"),
    ) else {
        return;
    };
    let _g = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::keys::clear_session();
    let pak = copy_to_tmp("real-enc", Path::new(&p));
    answer(Some((&k, false)));
    let listed = list(&pak).unwrap();
    println!("{}: {} entries listed with key", p, listed.len());
    crate::keys::clear_session();
}

#[test]
fn mount_folders_are_listed_with_marker_attribute() {
    let dir = tmp("mountdirs");
    let pak = dir.join("m.pak");
    make_pak(&pak, None, &payload());
    let all = list_all(&pak).unwrap();
    let dirs: Vec<_> = all.iter().filter(|l| l.attr & 0x10 != 0).collect();
    assert_eq!(
        dirs.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["TestGame", "TestGame\\Content"]
    );
    assert!(
        dirs.iter().all(|l| l.attr == 0x10 | 0x2),
        "DIRECTORY|HIDDEN by default"
    );
    assert!(all
        .iter()
        .filter(|l| l.attr & 0x10 == 0)
        .all(|l| l.attr == 0x20));
    // extraction creates them as plain folders
    let out = extract_all(&pak, &dir.join("out"));
    assert_eq!(out.len(), payload().len());
    assert!(dir.join("out/TestGame/Content").is_dir());
}

#[test]
fn root_mount_point_has_no_mount_folders() {
    let dir = tmp("rootmount");
    let pak = dir.join("r.pak");
    make_pak_v(&pak, repak::Version::V11, &payload());
    // mount "../../../G/" -> one folder "G"
    assert_eq!(list_all(&pak).unwrap().len(), payload().len() + 1);
    let pak2 = dir.join("r2.pak");
    let f = std::io::BufWriter::new(std::fs::File::create(&pak2).unwrap());
    let mut w = repak::PakBuilder::new().writer(f, repak::Version::V11, "../../../".into(), None);
    w.write_file("a.txt", false, b"x").unwrap();
    w.write_index().unwrap();
    assert_eq!(list_all(&pak2).unwrap().len(), 1, "nothing visible to show");
}

// ---------------------------------------------------------------------------
// Packing

/// Source tree: a.txt, deps\\big.bin (compressible), deps\\noise.bin
/// (incompressible), deps\\sub\\empty.txt (0 bytes), empty\\ (empty folder).
type Source = (PathBuf, Vec<String>, Vec<(String, Vec<u8>)>);

fn make_source(name: &str) -> Source {
    let root = tmp(name).join("src");
    let mut x: u64 = 0x2545F4914F6CDD1D;
    let noise: Vec<u8> = (0..100_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect();
    let files = vec![
        ("a.txt".to_string(), b"hello pak".to_vec()),
        (
            "deps/big.bin".to_string(),
            b"0123456789abcdef".repeat(20_000),
        ),
        ("deps/noise.bin".to_string(), noise),
        ("deps/sub/empty.txt".to_string(), vec![]),
    ];
    for (rel, data) in &files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    std::fs::create_dir_all(root.join("empty")).unwrap();
    // TC's AddList: folders with trailing backslash, then their files
    let add = [
        "a.txt",
        "deps\\",
        "deps\\big.bin",
        "deps\\noise.bin",
        "deps\\sub\\",
        "deps\\sub\\empty.txt",
        "empty\\",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    (root, add, files)
}

fn read_back(pak: &Path) -> Vec<(String, Vec<u8>)> {
    let mut f = std::io::BufReader::new(std::fs::File::open(pak).unwrap());
    let r = repak::PakBuilder::new().reader(&mut f).unwrap();
    let mut out: Vec<_> = r
        .files()
        .into_iter()
        .map(|p| {
            let d = r.get(&p, &mut f).unwrap();
            (p, d)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn pack_new_pak_every_compression() {
    use crate::config::{PackCompression, PackOptions};
    let (src, add, files) = make_source("pack-all");
    let mut expected = files.clone();
    expected.sort();
    for comp in [
        PackCompression::None,
        PackCompression::Zlib,
        PackCompression::Gzip,
        PackCompression::Zstd,
        PackCompression::Lz4,
    ] {
        let opts = PackOptions {
            compression: comp,
            ..PackOptions::default()
        };
        let pak = src.parent().unwrap().join(format!("{comp:?}.pak"));
        let r = crate::pack_files(&pak, None, &src, &add, pack::PK_PACK_SAVE_PATHS, &opts);
        assert_eq!(r, 0, "{comp:?}");
        assert!(!pak
            .with_file_name(format!("{comp:?}.pak.uepak-tmp"))
            .exists());
        assert_eq!(read_back(&pak), expected, "{comp:?} round trip");
        if let Ok(dir) = std::env::var("UEPAK_KEEP_PACKED") {
            std::fs::copy(&pak, Path::new(&dir).join(pak.file_name().unwrap())).unwrap();
        }
        // through the plugin's own reader too
        let listed = list(&pak).unwrap();
        assert_eq!(listed.len(), files.len());
        let mut f = std::io::BufReader::new(std::fs::File::open(&pak).unwrap());
        let r = repak::PakBuilder::new().reader(&mut f).unwrap();
        assert_eq!(r.version(), repak::Version::V11);
        assert_eq!(r.mount_point(), "../../../");
        let big = r.entry("deps/big.bin").unwrap();
        let noise = r.entry("deps/noise.bin").unwrap();
        if comp == PackCompression::None {
            assert_eq!(big.compression, None);
        } else {
            assert!(big.compression.is_some(), "{comp:?} big.bin compressed");
            assert!(big.compressed_size < big.uncompressed_size / 4);
            assert_eq!(noise.compression, None, "{comp:?} noise stored raw");
        }
    }
}

#[test]
fn pack_through_tc_export_with_defaults() {
    let (src, _add, _files) = make_source("pack-ffi");
    let pak = src.parent().unwrap().join("ffi.pak");
    let pak_w = wide(&pak.to_string_lossy());
    // TC sends SrcPath with a trailing separator
    let src_w = wide(&format!("{}{}", src.display(), std::path::MAIN_SEPARATOR));
    let list: Vec<u16> = "a.txt\0deps\\\0deps\\big.bin\0\0".encode_utf16().collect();
    let r = unsafe {
        PackFilesW(
            pak_w.as_ptr(),
            std::ptr::null(),
            src_w.as_ptr(),
            list.as_ptr(),
            2,
        )
    };
    assert_eq!(r, 0);
    let names: Vec<_> = read_back(&pak).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["a.txt", "deps/big.bin"]);
    // the same files again onto the now existing pak: replaced in place
    let r = unsafe {
        PackFilesW(
            pak_w.as_ptr(),
            std::ptr::null(),
            src_w.as_ptr(),
            list.as_ptr(),
            2,
        )
    };
    assert_eq!(r, 0);
    let names: Vec<_> = read_back(&pak).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["a.txt", "deps/big.bin"]);
}

#[test]
fn pack_mount_point_flatten_and_move() {
    use crate::config::PackOptions;
    let (src, add, _files) = make_source("pack-move");
    let opts = PackOptions {
        mount_point: "../../../Townfall/Config/".into(),
        version: repak::Version::V12,
        ..PackOptions::default()
    };
    // flattening collides? a.txt, big.bin, noise.bin, empty.txt: no collision
    let pak = src.parent().unwrap().join("move.pak");
    let r = crate::pack_files(&pak, None, &src, &add, pack::PK_PACK_MOVE_FILES, &opts);
    assert_eq!(r, 0);
    let names: Vec<_> = read_back(&pak).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["a.txt", "big.bin", "empty.txt", "noise.bin"]);
    assert!(!src.join("a.txt").exists(), "moved");
    assert!(!src.join("deps").exists(), "emptied folders removed");
    assert!(archive::sniff_footer(&pak).unwrap().version == 12);
    let all = list_all(&pak).unwrap();
    assert!(all.iter().any(|l| l.name == "Townfall\\Config\\a.txt"));
}

#[test]
fn pack_refusals_leave_nothing_behind() {
    use crate::config::{PackCompression, PackOptions};
    let (src, add, _files) = make_source("pack-refuse");
    let dir = src.parent().unwrap().to_path_buf();
    // LZ4 needs v8+
    let opts = PackOptions {
        compression: PackCompression::Lz4,
        version: repak::Version::V7,
        ..PackOptions::default()
    };
    let pak = dir.join("v7.pak");
    assert_eq!(
        crate::pack_files(&pak, None, &src, &add, 2, &opts),
        E_NOT_SUPPORTED
    );
    assert!(!pak.exists());
    // Oodle without the library
    if oodle_loader::oodle().is_err() {
        let opts = PackOptions {
            compression: PackCompression::Oodle,
            ..PackOptions::default()
        };
        let pak = dir.join("oodle.pak");
        assert_eq!(
            crate::pack_files(&pak, None, &src, &add, 2, &opts),
            E_NOT_SUPPORTED
        );
        assert!(!pak.exists());
    }
    // name collision when flattening
    std::fs::write(src.join("deps").join("a.txt"), b"other").unwrap();
    let mut add2 = add.clone();
    add2.push("deps\\a.txt".into());
    let pak = dir.join("dup.pak");
    assert_eq!(
        crate::pack_files(&pak, None, &src, &add2, 0, &PackOptions::default()),
        E_EABORTED
    );
    assert!(!pak.exists());
    // unreadable source
    let mut add3 = add.clone();
    add3.push("missing.bin".into());
    let pak = dir.join("missing.pak");
    assert_eq!(
        crate::pack_files(&pak, None, &src, &add3, 2, &PackOptions::default()),
        E_EABORTED
    );
    assert!(!pak.exists());
    assert!(!dir.join("missing.pak.uepak-tmp").exists());
}

unsafe extern "system" fn cancel_cb(_name: *mut u16, _size: i32) -> i32 {
    0
}

#[test]
fn pack_cancel_removes_partial_pak() {
    use crate::config::PackOptions;
    let (src, add, _files) = make_source("pack-cancel");
    let pak = src.parent().unwrap().join("cancel.pak");
    SetProcessDataProcW(std::ptr::null_mut(), Some(cancel_cb));
    let r = crate::pack_files(&pak, None, &src, &add, 2, &PackOptions::default());
    SetProcessDataProcW(std::ptr::null_mut(), None);
    assert_eq!(r, E_EABORTED);
    assert!(!pak.exists());
    assert!(!pak.with_file_name("cancel.pak.uepak-tmp").exists());
    assert!(src.join("a.txt").exists(), "sources untouched");
}

/// Byte-for-byte parity with a reference pak made by repak / UnrealPak:
/// `UEPAK_PARITY_PAK=<ref.pak>` is unpacked, re-packed by the plugin with the
/// same version, mount point and (no) compression, and compared.
#[test]
fn parity_with_reference_pak() {
    use crate::config::PackOptions;
    let Ok(reference) = std::env::var("UEPAK_PARITY_PAK") else {
        return;
    };
    let reference = PathBuf::from(reference);
    let mut f = std::io::BufReader::new(std::fs::File::open(&reference).unwrap());
    let r = repak::PakBuilder::new().reader(&mut f).unwrap();
    let src = tmp("parity").join("src");
    let mut add = vec![];
    for p in r.files() {
        let dest = src.join(&p);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, r.get(&p, &mut f).unwrap()).unwrap();
        add.push(p.replace('/', "\\\\"));
    }
    let opts = PackOptions {
        mount_point: r.mount_point().to_string(),
        version: r.version(),
        ..PackOptions::default()
    };
    let out = src.parent().unwrap().join("plugin.pak");
    assert_eq!(crate::pack_files(&out, None, &src, &add, 2, &opts), 0);
    let a = std::fs::read(&reference).unwrap();
    let b = std::fs::read(&out).unwrap();
    println!(
        "reference {} bytes, plugin {} bytes, identical: {}",
        a.len(),
        b.len(),
        a == b
    );
    assert_eq!(a, b, "byte-identical");
}

// ---------------------------------------------------------------------------
// Modifying existing paks (M1)

/// Serialises tests that queue confirm answers.
static CONFIRM_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn confirm_next(answers: &[bool]) {
    let mut q = crate::dialog::TEST_CONFIRMS.lock().unwrap();
    q.clear();
    q.extend_from_slice(answers);
}

/// Pak with mount ../../../TheIsle/Content/ and two entries.
fn base_pak(name: &str) -> (PathBuf, PathBuf) {
    let dir = tmp(name);
    let pak = dir.join("base.pak");
    let f = std::io::BufWriter::new(std::fs::File::create(&pak).unwrap());
    let mut w = repak::PakBuilder::new()
        .compression([repak::Compression::Zlib])
        .writer(
            f,
            repak::Version::V11,
            "../../../TheIsle/Content/".into(),
            None,
        );
    w.write_file("Maps/A.umap", true, b"map a".repeat(500))
        .unwrap();
    w.write_file("Config/x.ini", false, b"[x]\nk=v\n").unwrap();
    w.write_index().unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("Maps")).unwrap();
    std::fs::write(src.join("new.txt"), b"brand new").unwrap();
    std::fs::write(src.join("Maps").join("A.umap"), b"replaced map").unwrap();
    (pak, src)
}

fn pack_ffi(pak: &Path, sub: Option<&str>, src: &Path, list: &str, flags: i32) -> i32 {
    let pak_w = wide(&pak.to_string_lossy());
    let src_w = wide(&format!("{}{}", src.display(), std::path::MAIN_SEPARATOR));
    let sub_w = sub.map(wide);
    // TC lists: NUL after each item plus a final NUL
    let list_w: Vec<u16> = format!("{list}\0\0").encode_utf16().collect();
    unsafe {
        PackFilesW(
            pak_w.as_ptr(),
            sub_w.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            src_w.as_ptr(),
            list_w.as_ptr(),
            flags,
        )
    }
}

fn delete_ffi(pak: &Path, list: &str) -> i32 {
    let pak_w = wide(&pak.to_string_lossy());
    // TC lists: NUL after each item plus a final NUL
    let list_w: Vec<u16> = format!("{list}\0\0").encode_utf16().collect();
    unsafe { DeleteFilesW(pak_w.as_ptr(), list_w.as_ptr()) }
}

/// Game path of every entry: mount (minus base) + entry, lowercased.
fn game_paths(pak: &Path) -> Vec<(String, Vec<u8>)> {
    let mut f = std::io::BufReader::new(std::fs::File::open(pak).unwrap());
    let r = repak::PakBuilder::new().reader(&mut f).unwrap();
    let mount = crate::view::Mount::parse(r.mount_point()).visible.join("/");
    let mut v: Vec<_> = r
        .files()
        .into_iter()
        .map(|p| {
            let d = r.get(&p, &mut f).unwrap();
            let full = if mount.is_empty() {
                p
            } else {
                format!("{mount}/{p}")
            };
            (full.to_lowercase(), d)
        })
        .collect();
    v.sort();
    v
}

#[test]
fn f5_into_open_pak_adds_and_replaces_inside_mount() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, src) = base_pak("mod-add");
    list(&pak).unwrap(); // TC lists first: remembers Folder view
                         // F5 into TheIsle\Content: new.txt + Maps\A.umap (replace)
    let r = pack_ffi(
        &pak,
        Some("TheIsle\\Content"),
        &src,
        "new.txt\0Maps\\\0Maps\\A.umap",
        2,
    );
    assert_eq!(r, 0);
    assert!(!crate::modify::has_undo(&pak));
    let back = read_back(&pak);
    let names: Vec<_> = back.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Config/x.ini", "Maps/A.umap", "new.txt"]);
    assert_eq!(back[1].1, b"replaced map");
    let mut f = std::io::BufReader::new(std::fs::File::open(&pak).unwrap());
    let r = repak::PakBuilder::new().reader(&mut f).unwrap();
    assert_eq!(
        r.mount_point(),
        "../../../TheIsle/Content/",
        "mount unchanged"
    );
    // and the plugin itself still reads it
    assert_eq!(list(&pak).unwrap().len(), 3);
    if let Ok(dir) = std::env::var("UEPAK_KEEP_PACKED") {
        std::fs::copy(&pak, Path::new(&dir).join("modified_add.pak")).unwrap();
    }
}

#[test]
fn outside_mount_asks_then_reroots_keeping_game_paths() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, src) = base_pak("mod-reroot");
    list(&pak).unwrap();
    let before_bytes = std::fs::read(&pak).unwrap();
    let before = game_paths(&pak);
    // "No" -> nothing written
    confirm_next(&[false]);
    assert_eq!(
        pack_ffi(&pak, Some("TheIsle"), &src, "new.txt", 2),
        E_EABORTED
    );
    assert_eq!(
        std::fs::read(&pak).unwrap(),
        before_bytes,
        "untouched after No"
    );
    // "Yes" -> mount widened to TheIsle, entries re-rooted
    confirm_next(&[true]);
    assert_eq!(pack_ffi(&pak, Some("TheIsle"), &src, "new.txt", 2), 0);
    let mut f = std::io::BufReader::new(std::fs::File::open(&pak).unwrap());
    let r = repak::PakBuilder::new().reader(&mut f).unwrap();
    assert_eq!(r.mount_point(), "../../../TheIsle/");
    let after = game_paths(&pak);
    for old in &before {
        assert!(after.contains(old), "existing game path kept: {}", old.0);
    }
    assert!(after
        .iter()
        .any(|(p, d)| p == "theisle/new.txt" && d == b"brand new"));
    // packing at the panel root widens to the bare base
    confirm_next(&[true]);
    std::fs::write(src.join("root.txt"), b"r").unwrap();
    assert_eq!(pack_ffi(&pak, None, &src, "root.txt", 2), 0);
    let mut f = std::io::BufReader::new(std::fs::File::open(&pak).unwrap());
    let r = repak::PakBuilder::new().reader(&mut f).unwrap();
    assert_eq!(r.mount_point(), "../../../");
    assert!(game_paths(&pak)
        .iter()
        .any(|(p, _)| p == "theisle/content/maps/a.umap"));
    if let Ok(dir) = std::env::var("UEPAK_KEEP_PACKED") {
        std::fs::copy(&pak, Path::new(&dir).join("modified_reroot.pak")).unwrap();
    }
}

#[test]
fn f8_deletes_files_folders_and_mount_folders() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, _src) = base_pak("mod-del");
    list(&pak).unwrap();
    // nothing matched -> unchanged
    let before = std::fs::read(&pak).unwrap();
    assert_eq!(delete_ffi(&pak, "Elsewhere\\x.ini"), 0);
    assert_eq!(std::fs::read(&pak).unwrap(), before);
    // a file (Folder view path)
    assert_eq!(delete_ffi(&pak, "TheIsle\\Content\\Config\\x.ini"), 0);
    let names: Vec<_> = read_back(&pak).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["Maps/A.umap"]);
    assert!(
        std::fs::read(&pak).unwrap().len() < before.len(),
        "truncated"
    );
    // a mount folder -> everything; result is a valid empty pak
    assert_eq!(delete_ffi(&pak, "TheIsle\\*.*"), 0);
    assert!(read_back(&pak).is_empty());
    assert!(list(&pak).unwrap().is_empty());
}

#[test]
fn delete_uses_the_view_mode_the_pak_was_listed_with() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, _src) = base_pak("mod-viewmem");
    crate::read::remember_view_mode(&pak, crate::config::MountPointMode::Root);
    // Root view path (as TC would send after a Root listing)
    assert_eq!(delete_ffi(&pak, "Config\\*.*"), 0);
    let names: Vec<_> = read_back(&pak).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["Maps/A.umap"]);
}

unsafe extern "system" fn cancel_after_first(_n: *mut u16, _s: i32) -> i32 {
    0
}

#[test]
fn cancel_or_failure_leaves_pak_byte_identical() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, src) = base_pak("mod-cancel");
    list(&pak).unwrap();
    let before = std::fs::read(&pak).unwrap();
    SetProcessDataProcW(std::ptr::null_mut(), Some(cancel_after_first));
    let r = pack_ffi(&pak, Some("TheIsle\\Content"), &src, "new.txt", 2);
    SetProcessDataProcW(std::ptr::null_mut(), None);
    assert_eq!(r, E_EABORTED);
    assert_eq!(std::fs::read(&pak).unwrap(), before, "rolled back");
    assert!(!crate::modify::has_undo(&pak));
    // unreadable source mid-way
    let r = pack_ffi(
        &pak,
        Some("TheIsle\\Content"),
        &src,
        "new.txt\0missing.bin",
        2,
    );
    assert_eq!(r, E_EABORTED);
    assert_eq!(
        std::fs::read(&pak).unwrap(),
        before,
        "rolled back after read error"
    );
}

#[test]
fn interrupted_change_is_restored_on_open() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, _src) = base_pak("mod-crash");
    let original = std::fs::read(&pak).unwrap();
    // simulate a crash after the undo file was written and the tail overwritten
    let mut f = std::io::BufReader::new(std::fs::File::open(&pak).unwrap());
    let off = repak::PakBuilder::new()
        .reader(&mut f)
        .unwrap()
        .index_offset()
        .unwrap();
    drop(f);
    let mut undo = b"UEPAKUNDO1".to_vec();
    undo.extend((original.len() as u64).to_le_bytes());
    undo.extend(off.to_le_bytes());
    undo.extend(&original[off as usize..]);
    std::fs::write(crate::modify::undo_path(&pak), undo).unwrap();
    let mut broken = original[..off as usize].to_vec();
    broken.extend(b"half-written garbage");
    std::fs::write(&pak, &broken).unwrap();
    // editing is refused while the undo file exists
    assert_eq!(delete_ffi(&pak, "TheIsle\\*.*"), E_NOT_SUPPORTED);
    // opening offers the restore: Yes
    confirm_next(&[true]);
    assert_eq!(list(&pak).unwrap().len(), 2);
    assert_eq!(
        std::fs::read(&pak).unwrap(),
        original,
        "restored byte-identical"
    );
    assert!(!crate::modify::has_undo(&pak));
}

#[test]
fn modify_refusals() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    // encrypted pak (repak test pak): refused, untouched
    let enc = copy_to_tmp("mod-enc", &testdata("enc_entries.pak"));
    let before = std::fs::read(&enc).unwrap();
    assert_eq!(
        delete_ffi(&enc, "mount\\point\\root\\test.txt"),
        E_NOT_SUPPORTED
    );
    assert_eq!(std::fs::read(&enc).unwrap(), before);
    // adding a new compression method fills a free footer slot
    let (pak, src) = base_pak("mod-slot");
    list(&pak).unwrap();
    let opts = crate::config::PackOptions {
        compression: crate::config::PackCompression::Gzip,
        ..crate::config::PackOptions::default()
    };
    // compressible, so Gzip is actually used (incompressible data is stored raw)
    std::fs::write(src.join("big.txt"), b"gzip me ".repeat(5000)).unwrap();
    let r = crate::pack_files(
        &pak,
        Some("TheIsle\\Content"),
        &src,
        &["big.txt".to_string()],
        2,
        &opts,
    );
    assert_eq!(r, 0);
    let slots = archive::sniff_footer(&pak).unwrap().compression;
    assert!(
        slots.contains(&"Zlib".to_string()) && slots.contains(&"Gzip".to_string()),
        "{slots:?}"
    );
    assert_eq!(read_back(&pak).len(), 3);
}

#[test]
fn reroot_question_explains_the_new_folder() {
    let info = crate::modify::Inspect {
        mount: "../../../G1R/Config".into(),
        version: repak::Version::V11,
        entries: vec!["UserEngine.ini".into()],
        slots: vec![],
    };
    let mount = crate::view::Mount::parse(&info.mount);
    let plan = pack::plan(
        Path::new("C:\\src"),
        Some("G1R"),
        &["Readme.txt".to_string()],
        2,
    )
    .unwrap();
    let views: Vec<_> = plan.files.iter().map(|f| f.view.clone()).collect();
    let mapping = crate::view::map_add(crate::config::MountPointMode::Folder, &mount, &views);
    let q = pack::reroot_question(&info, &mount, &mapping, &plan);
    println!("{q}");
    assert!(q.contains("becomes ../../../G1R"), "{q}");
    assert!(q.contains("\"Config/\" is added"), "{q}");
    assert!(
        q.contains("UserEngine.ini  →  Config/UserEngine.ini"),
        "{q}"
    );
    assert!(
        q.contains("doesn't change: G1R/Config/UserEngine.ini"),
        "{q}"
    );
    assert!(q.contains("New files go to: G1R/Readme.txt"), "{q}");
}
