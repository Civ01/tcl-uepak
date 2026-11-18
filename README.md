# tcl-uepak

Total Commander 11+ packer plugin (WCX, **x64 only**) for Unreal Engine `.pak` files.
Built in Rust on a fork of [repak](https://github.com/Civ01/repak) (branch
`oodle-search-paths`). Full design and status: [SPEC.md](SPEC.md).

Features:
- **Reading:** list, extract (F5), test (incl. per-file checksums); pak versions 2–12
  (UE 4.0 – current UE5); AES-encrypted paks.
- **Packing:** new paks (Alt+F5), adding/replacing in existing paks (F5 into an open pak),
  deleting (F8). No compression, Zlib, Gzip, Oodle, Zstd or LZ4, with levels.
- **Safe changes:** existing paks are changed in place with an undo file (restore offered
  on the next open after a crash), a free-space check, and reuse of space at the end of
  the pak (`UndoLimit`, default 1 GB, caps the undo copy made without asking).
- **Compact:** `Compact=1` (or the dialog checkbox) rebuilds the pak on every change
  instead: no dead space from replaced or deleted files.
- **Encrypted paks:** F5/F8 work after a confirmation, with the key from the key prompt;
  new files and the index are encrypted like the pak's own (same format as UnrealPak).
  New encrypted paks aren't created.
- **Large files:** everything streams block by block (about 12 MB of memory even for
  multi-GB files).
- **Total Commander integration:** Find Files searches names (index only) and text inside
  paks; unpacking and packing can run in the background.
- **Settings:** the Configure button in TC's pack dialog (or the plugin settings) opens a
  dialog that writes `uepak.ini`.

## Install

Open the release zip in Total Commander (it contains `pluginst.inf`) and accept the
install prompt, or add `uepak.wcx64` manually under *Configuration → Plugins → Packer
plugins* for the `pak` extension. When updating from a build older than M5, reinstall
once: TC caches what a plugin supports.

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
base64). A wrong key is detected and the dialog asks again. The key is saved in
`uepak.keys.json` (settings folder) for that folder and its subfolders, unless you tick
"Use for this session only": then it's kept only until TC exits. A key with a non-zero key
GUID is also tried, without saving it again, for paks with the same GUID elsewhere. Keys are
never written to the log.

## Options (`uepak.ini`)

`[Options]`:
- `MountPoint=Folder|Root` — show the mount point (without leading `../`) as top-level
  folders (default), or show contents relative to the mount point.
- `MountFolderAttr=Hidden|ReadOnly|System|None` (default Hidden; combinations allowed) —
  attributes on the mount point's folders in Folder view, to tell them apart from real
  folders.
- `DebugLog=0|1` — detailed diagnostic log in `uepak.log` in the plugin folder. If that
  folder is not writable the log is kept in memory and copied to the clipboard whenever
  an operation fails.

`[Pack]` (also set by the settings dialog): `MountPoint`, `Version`, `Compression`,
`Level`, `OodleCompressor`, `BlockSize`, `UndoLimit`, `Compact` — see the comments in
`uepak.ini` and SPEC.md §6.

Options are re-read on every operation (no restart needed). The plugin uses `uepak.ini`
from its own folder when that folder is writable, otherwise from the folder of
`wincmd.ini`.

## Build

```
cargo build --release          # Windows, MSVC toolchain
copy target\release\uepak.dll uepak.wcx64
```

Cross-build from Linux: `cargo build --release --target x86_64-pc-windows-gnu`
(needs `mingw-w64`). CI tests, builds and packages the plugin on every push
(`.github/workflows/build.yml`; the zip is a workflow artifact).

Tests: `cargo test`. Optional, from the environment: `UEPAK_SAMPLES=<folder with .pak
files>` (real paks), `UEPAK_KEYED_PAK` + `UEPAK_KEYED_KEY` (a real encrypted pak and its
key; never commit keys), `UEPAK_BIG_MB` (size of the big-file streaming test).
Diagnose one file: `UEPAK_DEBUG_PAK=<file.pak> cargo test debug_open_env_pak -- --nocapture`
(log goes to `target/debug/deps/uepak.log`).

Note: the crate is `cdylib` only on purpose — adding `rlib` disables LTO and roughly
triples the DLL size, so tests live inside the crate (`src/tests.rs`).
