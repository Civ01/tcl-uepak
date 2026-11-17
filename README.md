# tcl-uepak

Total Commander 11+ packer plugin (WCX, **x64 only**) for Unreal Engine `.pak` files.
Built in Rust on a fork of [repak](https://github.com/Civ01/repak).

Status: **read-only skeleton** — list, extract (F5), test. No packing, no AES keys yet.

## Install

Open the release zip in Total Commander (it contains `pluginst.inf`) and accept the
install prompt, or add `uepak.wcx64` manually under *Configuration → Plugins → Packer
plugins* for the `pak` extension.

Layout in the plugin folder:

```
uepak.wcx64
uepak.ini        options (see comments inside)
deps\            third-party codec DLLs, e.g. oo2core_9_win64.dll for Oodle
```

## Compression

| Method | Status |
|---|---|
| none, Zlib, Gzip, Zstd, LZ4 | built in |
| Oodle | needs `oo2core_N_win64.dll` in `deps\` (any version; highest wins). Missing → warning, listing still works |

## Options (`uepak.ini`)

`[Options] MountPoint=Root|Folder` — show contents relative to the pak's mount point,
or with the mount point (without leading `../`) as top-level folders.

The plugin uses `uepak.ini` from its own folder when that folder is writable,
otherwise from the folder of `wincmd.ini`.

## Build

```
cargo build --release          # Windows, MSVC toolchain
copy target\release\uepak.dll uepak.wcx64
```

Cross-build from Linux: `cargo build --release --target x86_64-pc-windows-gnu`
(needs `mingw-w64`). CI builds and packages on every push (`.github/workflows/build.yml`).

Tests: `cargo test`; set `UEPAK_SAMPLES=<folder with .pak files>` to also run against real paks.

Note: the crate is `cdylib` only on purpose — adding `rlib` disables LTO and roughly
triples the DLL size, so tests live inside the crate (`src/tests.rs`).
