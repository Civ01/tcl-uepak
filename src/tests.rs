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
    // (encrypted paks are no longer refused: see the M4 tests)
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
        spans: vec![(0, 100)],
        index_offset: 100,
        file_len: 300,
        encrypted_index: false,
        encrypted_entries: false,
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

// ---------------------------------------------------------------------------
// M3: streaming, space reuse, undo limit, free space, verification

fn pak_len(p: &Path) -> u64 {
    std::fs::metadata(p).unwrap().len()
}

#[test]
fn replacing_the_last_file_reuses_its_space() {
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, src) = base_pak("m3-reuse");
    list(&pak).unwrap();
    std::fs::create_dir_all(src.join("Config")).unwrap();
    std::fs::write(src.join("Config").join("x.ini"), b"[x]\nk=w\n").unwrap();
    let before = pak_len(&pak);
    for i in 0..5 {
        std::fs::write(src.join("Config").join("x.ini"), format!("[x]\nk={i}\n")).unwrap();
        let r = pack_ffi(&pak, Some("TheIsle\\Content"), &src, "Config\\x.ini", 2);
        assert_eq!(r, 0);
    }
    assert_eq!(
        pak_len(&pak),
        before,
        "no growth: the last entry's space is reused"
    );
    let back = read_back(&pak);
    assert_eq!(
        back.iter().find(|(n, _)| n == "Config/x.ini").unwrap().1,
        b"[x]\nk=4\n"
    );
    // deleting it truncates the pak to where it started
    assert_eq!(delete_ffi(&pak, "TheIsle\\Content\\Config\\x.ini"), 0);
    assert!(pak_len(&pak) < before);
    assert_eq!(read_back(&pak).len(), 1);
}

#[test]
fn undo_limit_asks_and_falls_back_to_appending() {
    use crate::config::PackOptions;
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, src) = base_pak("m3-undo");
    list(&pak).unwrap();
    std::fs::create_dir_all(src.join("Config")).unwrap();
    std::fs::write(src.join("Config").join("x.ini"), b"new").unwrap();
    let opts = PackOptions {
        undo_limit: 0,
        ..PackOptions::default()
    };
    let items = ["Config\\x.ini".to_string()];
    let before = pak_len(&pak);
    // reuse question: No -> append; then the plain undo is over the limit too: Yes
    confirm_next(&[false, true]);
    let r = crate::pack_files(&pak, Some("TheIsle\\Content"), &src, &items, 2, &opts);
    assert_eq!(r, 0);
    assert!(pak_len(&pak) > before, "appended: the pak grew");
    assert!(
        crate::dialog::TEST_CONFIRMS.lock().unwrap().is_empty(),
        "both questions asked"
    );
    // reuse question: Yes -> reuses the (now dead) tail
    let grown = pak_len(&pak);
    confirm_next(&[true]);
    assert_eq!(
        crate::pack_files(&pak, Some("TheIsle\\Content"), &src, &items, 2, &opts),
        0
    );
    assert!(pak_len(&pak) < grown, "reused the space at the end");
    // both answered No -> nothing changes
    let now = std::fs::read(&pak).unwrap();
    confirm_next(&[false, false]);
    assert_eq!(
        crate::pack_files(&pak, Some("TheIsle\\Content"), &src, &items, 2, &opts),
        E_EABORTED
    );
    assert_eq!(std::fs::read(&pak).unwrap(), now);
    // a generous limit asks nothing
    confirm_next(&[]);
    let big = PackOptions::default();
    assert_eq!(
        crate::pack_files(&pak, Some("TheIsle\\Content"), &src, &items, 2, &big),
        0
    );
}

#[test]
fn low_free_space_asks_before_writing() {
    use crate::config::PackOptions;
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (src, add, _files) = make_source("m3-space");
    let pak = src.parent().unwrap().join("space.pak");
    pack::TEST_FREE_SPACE.with(|c| c.set(Some(1000)));
    confirm_next(&[false]);
    let r = crate::pack_files(&pak, None, &src, &add, 2, &PackOptions::default());
    assert_eq!(r, E_EABORTED);
    assert!(!pak.exists(), "nothing written after No");
    confirm_next(&[true]);
    let r = crate::pack_files(&pak, None, &src, &add, 2, &PackOptions::default());
    pack::TEST_FREE_SPACE.with(|c| c.set(None));
    assert_eq!(r, 0, "Yes continues (compression may make it fit)");
}

#[test]
fn test_mode_detects_damaged_entries() {
    let dir = tmp("m3-verify");
    let pak = dir.join("v.pak");
    make_pak_v(
        &pak,
        repak::Version::V11,
        &[("a.txt", b"original content".to_vec())],
    );
    let mut bytes = std::fs::read(&pak).unwrap();
    let pos = bytes.windows(8).position(|w| w == b"original").unwrap();
    bytes[pos] = b'O';
    std::fs::write(&pak, &bytes).unwrap();
    let name = wide(&pak.to_string_lossy());
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
    assert!(!h.is_null());
    let mut seen_bad = false;
    loop {
        let mut hd: Box<HeaderDataExW> = unsafe { Box::new(std::mem::zeroed()) };
        if unsafe { ReadHeaderExW(h, &mut *hd) } == E_END_ARCHIVE {
            break;
        }
        if hd.FileAttr & 0x10 != 0 {
            continue;
        }
        let r = unsafe { ProcessFileW(h, PK_TEST, std::ptr::null(), std::ptr::null()) };
        seen_bad |= r == E_BAD_DATA;
    }
    unsafe { CloseArchive(h) };
    assert!(seen_bad, "PK_TEST reports the damaged entry");
}

#[test]
fn big_file_streams_through_pack_and_extract() {
    use crate::config::{PackCompression, PackOptions};
    let dir = tmp("m3-big");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    // 40 MB by default (UEPAK_BIG_MB to change), compressible, many blocks
    let blocks: usize = std::env::var("UEPAK_BIG_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(640, |mb| mb * 16);
    eprintln!("big file: {} MB", blocks / 16);
    let block: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(src.join("big.bin")).unwrap());
        for _ in 0..blocks {
            std::io::Write::write_all(&mut f, &block).unwrap();
        }
    }
    for comp in [
        PackCompression::None,
        PackCompression::Zlib,
        PackCompression::Lz4,
    ] {
        let pak = dir.join(format!("{comp:?}.pak"));
        let opts = PackOptions {
            compression: comp,
            ..PackOptions::default()
        };
        assert_eq!(
            crate::pack_files(&pak, None, &src, &["big.bin".into()], 2, &opts),
            0
        );
        // extract through the plugin; compare by streaming (no whole-file read)
        let name = wide(&pak.to_string_lossy());
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
        let mut hd: Box<HeaderDataExW> = unsafe { Box::new(std::mem::zeroed()) };
        assert_eq!(unsafe { ReadHeaderExW(h, &mut *hd) }, 0);
        let target = dir.join(format!("out-{comp:?}.bin"));
        let tw = wide(&target.to_string_lossy());
        assert_eq!(
            unsafe { ProcessFileW(h, PK_EXTRACT, std::ptr::null(), tw.as_ptr()) },
            0
        );
        assert_eq!(
            unsafe { ProcessFileW(h, PK_TEST, std::ptr::null(), std::ptr::null()) },
            0
        );
        unsafe { CloseArchive(h) };
        assert_eq!(
            std::fs::metadata(&target).unwrap().len(),
            (blocks * 65536) as u64
        );
        let mut f = std::io::BufReader::new(std::fs::File::open(&target).unwrap());
        let mut buf = vec![0u8; 65536];
        for _ in 0..blocks {
            std::io::Read::read_exact(&mut f, &mut buf).unwrap();
            assert!(buf == block, "{comp:?} content");
        }
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&pak);
    }
}

#[test]
fn stale_key_for_same_guid_says_it_probably_changed() {
    let _g = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::keys::clear_session();
    crate::dialog::TEST_ANSWERS.lock().unwrap().clear();
    // repak's test pak with a non-zero key GUID patched into the footer
    // (the GUID isn't covered by the index hash)
    let pak = copy_to_tmp("stale-key", &testdata("enc_index.pak"));
    let mut bytes = std::fs::read(&pak).unwrap();
    let magic = archive::PAK_MAGIC.to_le_bytes();
    let pos = bytes.windows(4).rposition(|w| w == magic).unwrap();
    bytes[pos - 17..pos - 1].copy_from_slice(&[0xAB; 16]);
    std::fs::write(&pak, &bytes).unwrap();
    let guid = archive::sniff_footer(&pak).unwrap().key_guid.unwrap();
    assert_eq!(guid, "AB".repeat(16));
    // an old key remembered for that GUID in another folder
    let other = tmp("stale-key-other");
    crate::keys::remember(&other, &guid, &crate::keys::AesKey([0x11; 32]), false).unwrap();
    answer(Some((TEST_KEY_B64, false)));
    assert_eq!(list(&pak).unwrap().len(), 4);
    let shown = crate::dialog::TEST_LAST_ERROR
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    assert!(shown.contains("re-encrypted with a new key"), "{shown}");
    crate::keys::clear_session();
}

// ---------------------------------------------------------------------------
// M4: changing encrypted paks, compact rebuild

/// Locks for tests that use both the key prompt and confirm queues (always
/// in this order), with clean key state.
fn m4_locks() -> (
    std::sync::MutexGuard<'static, ()>,
    std::sync::MutexGuard<'static, ()>,
) {
    let k = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let c = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::keys::clear_session();
    let _ = std::fs::remove_file(keys_file());
    crate::dialog::TEST_ANSWERS.lock().unwrap().clear();
    crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clear();
    super::DECLINED.lock().unwrap().clear();
    (k, c)
}

fn test_key() -> crate::keys::AesKey {
    crate::keys::AesKey::parse(TEST_KEY_B64).unwrap()
}

/// (entry, data, encrypted) read with `key`, every hash checked.
fn keyed_read_back(pak: &Path, key: &crate::keys::AesKey) -> Vec<(String, Vec<u8>, bool)> {
    let mut f = std::io::BufReader::new(std::fs::File::open(pak).unwrap());
    let r = repak::PakBuilder::new()
        .key(key.cipher())
        .reader(&mut f)
        .unwrap();
    let oodle = oodle_loader::oodle().is_ok();
    let mut out: Vec<_> = r
        .entries()
        .map(|(p, e)| (p.to_string(), e))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(p, e)| {
            if e.compression == Some(repak::Compression::Oodle) && !oodle {
                // can't decompress here: compare sizes instead
                let d = format!("oodle {} {}", e.compressed_size, e.uncompressed_size);
                return (p, d.into_bytes(), e.encrypted);
            }
            let mut d = vec![];
            r.read_file_verified(&p, &mut f, &mut d)
                .unwrap_or_else(|e| panic!("{p}: {e}"));
            (p, d, e.encrypted)
        })
        .collect();
    out.sort();
    out
}

fn one_file_plan(name: &str, data: &[u8]) -> pack::Plan {
    let src = tmp(name).join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("added.txt"), data).unwrap();
    pack::plan(&src, None, &["added.txt".to_string()], 2).unwrap()
}

fn confirm_texts() -> Vec<String> {
    std::mem::take(&mut *crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap())
}

#[test]
fn adding_to_encrypted_paks_asks_and_encrypts_like_the_pak() {
    use crate::config::MountPointMode::Root;
    let key = test_key();
    // (test pak, index encrypted, entries encrypted, key can be checked)
    for (file, enc_index, enc_entries, checked) in [
        ("enc_index.pak", true, true, true),
        ("enc_entries.pak", false, true, false),
        ("enc_index_only.pak", true, false, true),
    ] {
        for compact in [false, true] {
            let _l = m4_locks();
            let pak = copy_to_tmp(&format!("m4-add-{file}-{compact}"), &testdata(file));
            let before = std::fs::read(&pak).unwrap();
            let old = keyed_read_back(&pak, &key);
            let data = b"added to an encrypted pak ".repeat(3000);
            let plan = one_file_plan(&format!("m4-add-src-{file}-{compact}"), &data);
            let opts = crate::config::PackOptions {
                compact,
                ..Default::default()
            };
            // key from the prompt, then "No" to the encryption question
            answer(Some((TEST_KEY_B64, false)));
            confirm_next(&[false]);
            let r = pack::add_to_existing(&pak, &plan, Root, &opts);
            assert!(matches!(r, Err(pack::PackError::Aborted)), "{file}: {r:?}");
            assert_eq!(
                std::fs::read(&pak).unwrap(),
                before,
                "{file}: unchanged after No"
            );
            let q = confirm_texts();
            assert_eq!(q.len(), 1, "{file}: {q:?}");
            assert!(
                q[0].starts_with("You are trying to add files to an encrypted pak"),
                "{}",
                q[0]
            );
            assert_eq!(q[0].contains("can't be checked"), !checked, "{}", q[0]);

            // "Yes": the key is remembered for the session, no second prompt
            confirm_next(&[true]);
            let st = pack::add_to_existing(&pak, &plan, Root, &opts).unwrap();
            assert!(crate::dialog::TEST_ANSWERS.lock().unwrap().is_empty());
            assert_eq!(st.added, 1);
            assert_eq!(st.compacted, compact);
            assert!(!crate::modify::has_undo(&pak));
            assert!(!crate::modify::rebuild_path(&pak).exists());
            let sniff = archive::sniff_footer(&pak).unwrap();
            assert_eq!(sniff.encrypted_index, Some(enc_index), "{file}");
            if enc_index {
                let mut f = std::io::BufReader::new(std::fs::File::open(&pak).unwrap());
                assert!(matches!(
                    repak::PakBuilder::new().reader(&mut f),
                    Err(repak::Error::Encrypted)
                ));
            }
            let now = keyed_read_back(&pak, &key);
            assert_eq!(now.len(), old.len() + 1);
            let added = now.iter().find(|(p, _, _)| p == "added.txt").unwrap();
            assert_eq!(added.1, data);
            assert_eq!(
                added.2, enc_entries,
                "{file}: new entry encrypted like the pak's"
            );
            for o in &old {
                assert!(now.contains(o), "{file}: {} kept", o.0);
            }
            // TC's own list + extract + test pass, with the remembered key
            let out = extract_all(&pak, &tmp(&format!("m4-add-out-{file}-{compact}")));
            assert!(out
                .iter()
                .any(|(n, d)| n.ends_with("added.txt") && *d == data));
            crate::keys::clear_session();
        }
    }
}

#[test]
fn deleting_from_encrypted_paks() {
    use crate::config::MountPointMode::Root;
    let key = test_key();
    for compact in [false, true] {
        let opts = crate::config::PackOptions {
            compact,
            ..Default::default()
        };
        // encrypted index: key + confirmation
        let _l = m4_locks();
        let pak = copy_to_tmp(&format!("m4-del-idx-{compact}"), &testdata("enc_index.pak"));
        let old = keyed_read_back(&pak, &key);
        let victim = old[0].0.clone();
        answer(Some((TEST_KEY_B64, false)));
        confirm_next(&[true]);
        let st = pack::delete_entries(&pak, &[victim.replace('/', "\\")], Root, &opts).unwrap();
        assert_eq!(st.removed, 1);
        let q = confirm_texts();
        assert!(
            q[0].starts_with("You are trying to delete files from an encrypted pak"),
            "{q:?}"
        );
        let now = keyed_read_back(&pak, &key);
        assert_eq!(now, old[1..].to_vec());
        assert_eq!(
            archive::sniff_footer(&pak).unwrap().encrypted_index,
            Some(true)
        );
        drop(_l);

        // plain index, encrypted entries: index-only change, no key, no question
        let _l = m4_locks();
        let pak = copy_to_tmp(
            &format!("m4-del-ent-{compact}"),
            &testdata("enc_entries.pak"),
        );
        let old = keyed_read_back(&pak, &key);
        let victim = old.last().unwrap().0.clone();
        pack::delete_entries(&pak, &[victim.replace('/', "\\")], Root, &opts).unwrap();
        assert!(confirm_texts().is_empty());
        assert!(crate::dialog::TEST_ANSWERS.lock().unwrap().is_empty());
        let now = keyed_read_back(&pak, &key);
        assert_eq!(now, old[..old.len() - 1].to_vec());
    }
}

/// Entries lie back to back from the start, and the index follows the last.
fn assert_no_dead_space(pak: &Path) {
    let mut f = std::io::BufReader::new(std::fs::File::open(pak).unwrap());
    let r = repak::PakBuilder::new().reader(&mut f).unwrap();
    let mut spans: Vec<(u64, u64)> = r.entries().map(|(_, e)| (e.offset, e.data_end)).collect();
    spans.sort();
    let mut at = 0;
    for (start, end) in spans {
        assert_eq!(start, at, "gap before {start}");
        at = end;
    }
    assert_eq!(r.index_offset(), Some(at), "index right after the data");
}

#[test]
fn compact_rebuild_leaves_no_dead_space() {
    use crate::config::MountPointMode::Root;
    let _l = m4_locks();
    let dir = tmp("m4-compact");
    let pak = dir.join("c.pak");
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("a/first.txt", b"first ".repeat(20_000)),
        ("b/second.txt", b"second ".repeat(20_000)),
        ("c/third.txt", b"third ".repeat(20_000)),
    ];
    make_pak(&pak, Some(repak::Compression::Zlib), &files);
    let fresh_size = std::fs::metadata(&pak).unwrap().len();
    assert_no_dead_space(&pak);
    // in-place replace of a file that isn't last leaves dead space
    let src = dir.join("src").join("a");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("first.txt"), b"first ".repeat(20_000)).unwrap();
    let plan = pack::plan(&dir.join("src"), None, &["a\\first.txt".to_string()], 2).unwrap();
    let plain = crate::config::PackOptions::default();
    pack::add_to_existing(&pak, &plan, Root, &plain).unwrap();
    let grown = std::fs::metadata(&pak).unwrap().len();
    assert!(grown > fresh_size + 100, "{grown} vs {fresh_size}");
    // compacting: the same replace again gives exactly a fresh pak's size
    let compact = crate::config::PackOptions {
        compact: true,
        ..Default::default()
    };
    let st = pack::add_to_existing(&pak, &plan, Root, &compact).unwrap();
    assert!(st.compacted);
    let size = std::fs::metadata(&pak).unwrap().len();
    assert!(size < grown, "{size} vs {grown}");
    assert_no_dead_space(&pak);
    let mut expected: Vec<_> = files
        .iter()
        .map(|(n, d)| (n.to_string(), d.clone()))
        .collect();
    expected.sort();
    assert_eq!(read_back(&pak), expected);
    // compact delete: smaller than an in-place delete would leave it
    pack::delete_entries(&pak, &["b\\second.txt".to_string()], Root, &compact).unwrap();
    let after_delete = std::fs::metadata(&pak).unwrap().len();
    assert!(after_delete < size - 1000, "{after_delete} vs {size}");
    assert_no_dead_space(&pak);
    assert_eq!(read_back(&pak).len(), 2);
    // low space: asked, No leaves the pak alone and no temp file behind
    let before = std::fs::read(&pak).unwrap();
    pack::TEST_FREE_SPACE.with(|c| c.set(Some(10)));
    confirm_next(&[false]);
    let r = pack::add_to_existing(&pak, &plan, Root, &compact);
    pack::TEST_FREE_SPACE.with(|c| c.set(None));
    assert!(matches!(r, Err(pack::PackError::Aborted)));
    assert!(confirm_texts()[0].contains("Rebuilding this pak"));
    assert_eq!(std::fs::read(&pak).unwrap(), before);
    assert!(!crate::modify::rebuild_path(&pak).exists());
}

#[test]
fn failed_compact_keeps_the_original() {
    use crate::config::MountPointMode::Root;
    let _l = m4_locks();
    let dir = tmp("m4-compact-fail");
    let pak = dir.join("f.pak");
    make_pak(&pak, None, &payload());
    let before = std::fs::read(&pak).unwrap();
    // a source that disappears between planning and packing
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("gone.txt"), b"x").unwrap();
    let plan = pack::plan(&src, None, &["gone.txt".to_string()], 2).unwrap();
    std::fs::remove_file(src.join("gone.txt")).unwrap();
    let compact = crate::config::PackOptions {
        compact: true,
        ..Default::default()
    };
    let r = pack::add_to_existing(&pak, &plan, Root, &compact);
    assert!(matches!(r, Err(pack::PackError::Read(..))), "{r:?}");
    assert_eq!(std::fs::read(&pak).unwrap(), before);
    assert!(!crate::modify::rebuild_path(&pak).exists());
}

/// Optional: add to / compact a copy of a real encrypted pak
/// (`UEPAK_KEYED_PAK`, `UEPAK_KEYED_KEY`; keys never committed).
#[test]
fn real_encrypted_pak_add_and_compact_from_env() {
    use crate::config::MountPointMode::Root;
    let (Ok(p), Ok(k)) = (
        std::env::var("UEPAK_KEYED_PAK"),
        std::env::var("UEPAK_KEYED_KEY"),
    ) else {
        return;
    };
    let key = crate::keys::AesKey::parse(&k).unwrap();
    for compact in [false, true] {
        let _l = m4_locks();
        let pak = copy_to_tmp(&format!("m4-real-{compact}"), Path::new(&p));
        let old = keyed_read_back(&pak, &key);
        let data = b"real pak addition ".repeat(10_000);
        let plan = one_file_plan(&format!("m4-real-src-{compact}"), &data);
        let opts = crate::config::PackOptions {
            compact,
            compression: crate::config::PackCompression::Zlib,
            ..Default::default()
        };
        answer(Some((&k, false)));
        confirm_next(&[true]);
        pack::add_to_existing(&pak, &plan, Root, &opts).unwrap();
        let now = keyed_read_back(&pak, &key);
        assert_eq!(now.len(), old.len() + 1);
        let added = now.iter().find(|(p, _, _)| p == "added.txt").unwrap();
        assert_eq!(added.1, data);
        let any_enc = old.iter().any(|o| o.2);
        assert_eq!(added.2, any_enc);
        for o in &old {
            assert!(now.contains(o));
        }
        println!(
            "real pak, compact={compact}: {} -> {} bytes",
            std::fs::metadata(Path::new(&p)).unwrap().len(),
            std::fs::metadata(&pak).unwrap().len()
        );
        crate::keys::clear_session();
    }
}

// ---------------------------------------------------------------------------
// M5: background operations, text search

#[test]
fn capabilities_and_background_flags() {
    let caps = GetPackerCaps();
    assert_ne!(caps & PK_CAPS_SEARCHTEXT, 0);
    assert_eq!(caps & (PK_CAPS_MEMPACK | PK_CAPS_HIDE | PK_CAPS_ENCRYPT), 0);
    assert_eq!(GetBackgroundFlags(), BACKGROUND_UNPACK | BACKGROUND_PACK);
}

thread_local! {
    static PROGRESS_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

unsafe extern "system" fn count_cb(_name: *mut u16, size: i32) -> i32 {
    if size > 0 {
        PROGRESS_BYTES.with(|c| c.set(c.get() + size as u64));
    }
    1
}

/// TC's background unpacking: several threads each open, list, extract and
/// close their own pak at the same time, with their own progress callback.
#[test]
fn parallel_extraction_with_per_thread_progress() {
    let dir = tmp("bg-extract");
    let paks: Vec<(PathBuf, u64)> = (0..6)
        .map(|i| {
            let pak = dir.join(format!("p{i}.pak"));
            let data = vec![b'a' + i as u8; 200_000 * (i + 1)];
            let files = [
                ("Data/file.bin", data),
                ("Data/small.txt", b"x".repeat(i + 1)),
            ];
            make_pak(&pak, Some(repak::Compression::Zlib), &files);
            (pak, (200_000 * (i + 1) + i + 1) as u64)
        })
        .collect();
    let handles: Vec<_> = paks
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, (pak, total))| {
            let out = dir.join(format!("out{i}"));
            std::thread::spawn(move || {
                PROGRESS_BYTES.with(|c| c.set(0));
                SetProcessDataProcW(std::ptr::null_mut(), Some(count_cb));
                let got = extract_all(&pak, &out);
                SetProcessDataProcW(std::ptr::null_mut(), None);
                let files: Vec<_> = got.iter().filter(|(_, d)| !d.is_empty()).collect();
                assert_eq!(files.len(), 2, "{i}");
                assert!(files.iter().any(
                    |(n, d)| n.ends_with("file.bin") && d.iter().all(|b| *b == b'a' + i as u8)
                ));
                // extract + test report every byte: twice the pak's contents
                assert_eq!(
                    PROGRESS_BYTES.with(|c| c.get()),
                    2 * total,
                    "{i}: own progress only"
                );
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

/// While one operation writes a pak, others on the same pak are refused (and
/// opening it never offers to restore the in-flight undo file); different
/// paks are fine.
#[test]
fn busy_pak_is_refused_until_released() {
    use crate::config::MountPointMode::Root;
    let _g = CONFIRM_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (pak, src) = base_pak("bg-busy");
    let before = std::fs::read(&pak).unwrap();
    let plan = pack::plan(&src, None, &["new.txt".to_string()], 2).unwrap();
    let opts = crate::config::PackOptions::default();
    {
        let _held = crate::busy::acquire(&pak).unwrap();
        assert!(crate::busy::acquire(&pak).is_none());
        // same pak, other spelling
        let upper = PathBuf::from(pak.to_string_lossy().to_uppercase());
        if cfg!(windows) {
            assert!(crate::busy::is_busy(&upper));
        }
        let r = pack::add_to_existing(&pak, &plan, Root, &opts);
        assert!(matches!(r, Err(pack::PackError::User(_, E_EOPEN))), "{r:?}");
        let r = pack::delete_entries(&pak, &["Config\\x.ini".to_string()], Root, &opts);
        assert!(matches!(r, Err(pack::PackError::User(_, E_EOPEN))), "{r:?}");
        let r = pack::write_new_pak(&pak, &plan, &opts);
        assert!(matches!(r, Err(pack::PackError::User(_, E_EOPEN))), "{r:?}");
        // an undo file mid-write: opening is refused, nothing is restored
        let undo = crate::modify::undo_path(&pak);
        std::fs::write(&undo, b"in flight").unwrap();
        confirm_next(&[true]);
        assert_eq!(list(&pak).err(), Some(E_EOPEN));
        assert_eq!(
            crate::dialog::TEST_CONFIRMS.lock().unwrap().len(),
            1,
            "no restore question"
        );
        std::fs::remove_file(&undo).unwrap();
        confirm_next(&[]);
        assert_eq!(std::fs::read(&pak).unwrap(), before);
        // another pak isn't affected
        let (other, other_src) = base_pak("bg-busy-other");
        let p2 = pack::plan(&other_src, None, &["new.txt".to_string()], 2).unwrap();
        pack::add_to_existing(&other, &p2, Root, &opts).unwrap();
    }
    assert!(!crate::busy::is_busy(&pak));
    pack::add_to_existing(&pak, &plan, Root, &opts).unwrap();
    assert_eq!(read_back(&pak).len(), 3);
}

/// Many threads adding to the same pak at once: each either succeeds or is
/// refused as busy; the pak stays valid and holds every file that was added.
#[test]
fn parallel_packing_into_one_pak_never_corrupts_it() {
    use crate::config::MountPointMode::Root;
    let dir = tmp("bg-same");
    let pak = dir.join("same.pak");
    make_pak(&pak, None, &payload());
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let (pak, src) = (pak.clone(), src.clone());
            std::thread::spawn(move || {
                let name = format!("t{i}.bin");
                std::fs::write(src.join(&name), vec![i as u8; 300_000]).unwrap();
                let plan = pack::plan(&src, None, std::slice::from_ref(&name), 2).unwrap();
                let opts = crate::config::PackOptions {
                    compression: crate::config::PackCompression::Zlib,
                    ..Default::default()
                };
                match pack::add_to_existing(&pak, &plan, Root, &opts) {
                    Ok(_) => Some(name),
                    Err(pack::PackError::User(_, E_EOPEN)) => None,
                    Err(e) => panic!("{i}: {e}"),
                }
            })
        })
        .collect();
    let added: Vec<String> = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap())
        .collect();
    assert!(!added.is_empty());
    let back = read_back(&pak);
    assert_eq!(back.len(), payload().len() + added.len());
    for name in &added {
        let i: u8 = name[1..name.len() - 4].parse().unwrap();
        assert!(back
            .iter()
            .any(|(n, d)| n == name && *d == vec![i; 300_000]));
    }
    assert!(!crate::modify::has_undo(&pak));
}

/// Keys saved from many threads at once all end up in the keys file.
#[test]
fn parallel_key_saves_are_not_lost() {
    let _g = KEY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::keys::clear_session();
    let _ = std::fs::remove_file(keys_file());
    let key = crate::keys::AesKey::parse(TEST_KEY_B64).unwrap();
    let handles: Vec<_> = (0..16)
        .map(|i| {
            std::thread::spawn(move || {
                let folder = PathBuf::from(format!("C:\\Games\\G{i}\\Paks"));
                crate::keys::remember(&folder, crate::keys::ZERO_GUID, &key, true).unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let text = std::fs::read_to_string(keys_file()).unwrap();
    for i in 0..16 {
        assert!(text.contains(&format!("G{i}")), "G{i} saved");
    }
    let _ = std::fs::remove_file(keys_file());
    crate::keys::clear_session();
}
