# tcl-uepak

Total Commander 11+ packer plugin (WCX, **x64 only**) for Unreal Engine `.pak` files.
Built in Rust on a fork of [repak](https://github.com/Civ01/repak).

Status:
- **Reading:** list, extract (F5), test, AES-encrypted paks (key prompt, remembered keys).
- **Packing:** new paks (Alt+F5), adding/replacing in existing paks (F5 into an open pak),
  deleting (F8). No compression, Zlib, Gzip, Oodle, Zstd or LZ4, with levels. Changes to
  existing paks are crash-safe (undo file, restore offered on next open).
- **Settings:** the Configure button in TC's pack dialog (or the plugin settings) opens a
  dialog that writes `uepak.ini`.

- **Large files:** everything streams block by block (memory stays around 12 MB even for
  multi-GB files); free-space check; replacing/removing the last files reuses their space,
  with an undo-size limit (`UndoLimit`, default 1 GB) above which it asks.

- **Encrypted paks:** F5/F8 work on encrypted paks after a confirmation, with the key from
  the key prompt; new files and the index are encrypted like the pak's own (same format
  as UnrealPak). New encrypted paks aren't created.
- **Compact:** `Compact=1` (or the dialog checkbox) rebuilds the pak on every change: no
  dead space from replaced or deleted files.

Next: text search in archives and background operations (see SPEC.md §0).

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

## AES keys

Opening a pak with an encrypted file list asks for its key (64 hex digits, optional `0x`, or
base64). A wrong key is detected and the dialog asks again. With "Remember" ticked the key
is stored in `uepak.keys.json` (settings folder) for that folder and its subfolders; a key
for a non-zero key GUID is also tried for paks with the same GUID elsewhere. Keys are never
written to the log.

## Options (`uepak.ini`)

- `MountPoint=Folder|Root` — show the mount point (without leading `../`) as top-level
  folders (default), or show contents relative to the mount point.
- `MountFolderAttr=Hidden|ReadOnly|System|None` (default Hidden; combinations allowed) — attributes on the
  mount point's folders in Folder view, to tell them apart from real folders.
- `DebugLog=0|1` (also `debug_log`) — detailed diagnostic log in `uepak.log` in the plugin
  folder. If that folder is not writable the log is kept in memory and copied to the
  clipboard whenever an operation fails. Re-read on every archive open, no restart needed.

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
Diagnose one file: `UEPAK_DEBUG_PAK=<file.pak> cargo test debug_open_env_pak -- --nocapture`
(log goes to `target/debug/deps/uepak.log`).

Supported pak versions: 2–12 (UE 4.0 – current UE5). Newer versions are reported as
unsupported rather than guessed.

Note: the crate is `cdylib` only on purpose — adding `rlib` disables LTO and roughly
triples the DLL size, so tests live inside the crate (`src/tests.rs`).
