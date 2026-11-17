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
}

fn list(path: &Path) -> Result<Vec<Listed>, i32> {
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
            let l = listed
                .iter()
                .find(|l| l.name == name.replace('/', "\\"))
                .unwrap();
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
            let got = out
                .iter()
                .find(|(n, _)| *n == name.replace('/', "\\"))
                .unwrap();
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
fn pack_and_delete_stubs_record_and_refuse() {
    let pak = wide("C:\\x\\new.pak");
    let src = wide("C:\\src\\");
    // "a.txt\0dir\\b.txt\0\0"
    let list: Vec<u16> = "a.txt\0dir\\b.txt\0\0".encode_utf16().collect();
    assert_eq!(
        unsafe { win::from_wide_multi(list.as_ptr()) },
        vec!["a.txt", "dir\\b.txt"]
    );
    let r = unsafe {
        PackFilesW(
            pak.as_ptr(),
            std::ptr::null(),
            src.as_ptr(),
            list.as_ptr(),
            2,
        )
    };
    assert_eq!(r, E_NOT_SUPPORTED);
    assert_eq!(
        unsafe { DeleteFilesW(pak.as_ptr(), list.as_ptr()) },
        E_NOT_SUPPORTED
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
            assert_eq!(list(&p).err(), Some(E_NOT_SUPPORTED), "{}", p.display());
            println!(
                "{}: encrypted index -> E_NOT_SUPPORTED (expected)",
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
fn encrypted_index_is_reported_as_not_supported() {
    // Flip the "encrypted index" byte that precedes the magic.
    let pak = patched_pak("enc", |b, pos| b[pos - 1] = 1);
    let sniff = archive::sniff_footer(&pak).unwrap();
    assert_eq!(sniff.encrypted_index, Some(true));
    assert_eq!(list(&pak).err(), Some(E_NOT_SUPPORTED));
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
