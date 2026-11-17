# tcl-uepak — Specification

Total Commander 11+ packer plugin (WCX, x64 only) for Unreal Engine `.pak` files.

Document status: **v2** (2026-11-17). Markers: **[done]** implemented and tested,
**[partial]** partly implemented (the gap is named), everything else is planned.

---

## 0. Status and roadmap

### Implemented

| Area | State |
|---|---|
| Reading | List, extract, test; pak versions 2–12; None/Zlib/Gzip/Zstd/LZ4/Oodle; >4 GB entries |
| AES (read) | Key prompt, wrong-key detection, remembered keys (folder/parent, same GUID), session cache |
| Views | `Folder` (default) with marked mount folders, `Root` |
| Packing | New paks via Alt+F5 with every compression method; settings from `uepak.ini [Pack]` |
| Robustness | Panic guard on every export, allocation guards in the fork, path-traversal checks, no partial output files |
| Diagnostics | `DebugLog`, clipboard fallback, independent footer parser |
| Tests | 33 automated tests (Windows CI); Wine-driven key dialog test; index-hash verification script |

### Roadmap

| # | Milestone | Contents | Fork work |
|---|---|---|---|
| **M1** | **Modify existing paks** (next) | F5 into an open pak (add/replace), F8 delete, view-path → entry-path mapping incl. the `Folder`-mode mount guard and per-archive mode memory, transactional append with undo file, crash recovery | F6 |
| M2 | Settings dialog | `ConfigurePacker` dialog (§5); compression levels and Oodle compressor | F4 (rest) |
| M3 | Large files | Streaming writes and block-by-block reads (constant memory, per-block progress), entry SHA-1 check in `PK_TEST` | F5, F7, F9 |
| M4 | Encrypted modify + compact | Adding to/deleting from encrypted paks (§4.4), "Compact when modifying" rebuild | F10, F6 (copy stored entries) |
| M5 | Polish | Text search in archives, background operations, v13–15 reading (no samples exist yet) | F8 |

Rationale for the order: M1 completes the core TC workflows (the capabilities are already
advertised, so F5/F8 currently show "not implemented"). M2 replaces hand-editing the ini.
M3 matters only for very large files. M4 depends on M1's modify machinery.

---

## 1. Goals and non-goals

**Goals**

- Browse, extract and test `.pak` files from UE 4.0 to current UE5 inside Total Commander.
- Create new `.pak` files and modify existing ones (add, replace, delete) from TC's
  standard pack (Alt+F5), copy-to-archive (F5 into an open pak) and delete (F8) flows.
- Never crash or hang Total Commander; every failure ends in a clear message or a TC error code.
- Be easy to diagnose: optional detailed log (§7).

**Non-goals**

- IoStore containers (`.utoc` / `.ucas`). Separate project.
- Creating **new** encrypted paks. Existing encrypted paks will be modifiable (§4.4); the
  AES key is symmetric, so the key that reads a pak can also write it.
- Pak signing (`.sig` files). A modified signed pak fails signature checks in games that
  enforce them; documented, not handled.
- 32-bit Total Commander.
- Game-specific pak variants (custom magic, obfuscated footers). Only the standard Epic
  format is supported.

---

## 2. Architecture

| Component | Role |
|---|---|
| `uepak.wcx64` (this repo, Rust `cdylib`) | WCX exports, TC integration, dialogs, settings, logging |
| [`Civ01/repak`](https://github.com/Civ01/repak) fork, branch `oodle-search-paths` | Pak format: read, write, compression, AES decryption, Oodle loading |
| `deps\oo2core_N_win64.dll` (user-supplied) | Oodle codec, loaded at runtime, never downloaded |

Plugin modules:

| Module | Contents |
|---|---|
| `lib.rs` | Exports, panic guard, init/options reload, popups, `PackFilesW` dispatch |
| `read.rs` | Open (incl. AES key resolution), list, extract, test |
| `pack.rs` | Pack planning (AddList → entries), new-pak writer |
| `archive.rs` | Opened-pak model, view paths, footer sniffer |
| `keys.rs`, `dialog.rs` | AES key parsing/storage/lookup, key prompt |
| `progress.rs` | Per-thread TC progress callback |
| `config.rs`, `log.rs`, `win.rs`, `ffi.rs` | `uepak.ini`, debug log, Win32 helpers, WCX definitions |

The crate is `cdylib` only: adding `rlib` silently disables LTO and roughly triples the
DLL size, so tests live inside the crate (`src/tests.rs`).

Plugin folder layout:

```
uepak.wcx64
uepak.ini          shipped defaults (§6)
pluginst.inf       TC auto-install descriptor (verified with file=uepak.wcx64)
deps\              third-party codec DLLs (Oodle)
uepak.log          only with DebugLog=1 and a writable folder
```

Writable data (edited `uepak.ini`, `uepak.keys.json`) lives in the **settings directory**:
the plugin folder if it is writable, otherwise the folder of `wincmd.ini` (from
`PackSetDefaultParams`). Shipping a file there does not make it writable (e.g. Program
Files ACLs), hence the write probe. **[done]**

### Exported functions

| Export | Status |
|---|---|
| `OpenArchiveW`, `ReadHeaderExW`, `ProcessFileW`, `CloseArchive` | **[done]** |
| `SetProcessDataProcW` | **[done]** (per-thread callback) |
| `SetChangeVolProcW` | no-op |
| `CanYouHandleThisFileW` | **[done]** (footer magic in the last 512 bytes) |
| `PackSetDefaultParams` | **[done]** |
| `GetPackerCaps` | **[done]** `NEW \| MODIFY \| MULTIPLE \| DELETE \| OPTIONS \| BY_CONTENT` |
| `PackFilesW` | **[partial]** new paks; existing paks refused (M1) |
| `DeleteFilesW` | stub: records the arguments (popup, clipboard, log) and refuses (M1) |
| `ConfigurePacker` | stub: records and refuses (M2) |
| `GetBackgroundFlags` | not exported (M5, §8.1) |
| ANSI variants | read functions convert and forward; `PackFiles`/`DeleteFiles` ANSI refuse (TC x64 uses W) |

`PK_CAPS_SEARCHTEXT` is added in M5. Not planned: `PK_CAPS_ENCRYPT`, `PK_CAPS_MEMPACK`,
`PK_CAPS_HIDE`. TC caches caps: reinstall the plugin when they change.

---

## 3. Reading and unpacking

### 3.1 Supported formats **[done]**

| Pak version | UE | Read |
|---|---|---|
| 1 | very early UE4 | not supported (no samples) |
| 2–8B | 4.0 – 4.24 | supported |
| 9 | 4.25 | supported, except paks with a frozen index |
| 10–11 | 4.26 – 5.x | supported |
| 12 `Utf8PakDirectory` | 5.3+ | supported (same layout as 11) |
| 13 `SortedDirectoryIndex`, 14 `PakchunkIndex`, 15 `EncryptionMethod` | newest | M5, low priority: none seen in the wild (UE 5.8.4 games still ship v11). Reference: CUE4Parse `EPakFileVersion` / `FPakInfo` |
| unknown / newer | — | refused: "PAK version N is newer than this plugin supports" |

| Compression | Read |
|---|---|
| None, Zlib, Gzip, Zstd, LZ4 | built in |
| Oodle | `deps\oo2core_N_win64.dll`, any N, highest wins; a DLL added later is picked up without restarting TC |

### 3.2 Listing **[done]**

- Entries come from the index; no file data is read until extraction.
- Packed and unpacked sizes come from the index; >4 GB entries are supported.
- File date/time: the `.pak` file's own modification time (modern paks store none).
- Skipped and logged: entries containing `..`, absolute paths, `:`, or empty path
  components (path-traversal protection); deleted records (v6+).

#### View modes (`[Options] MountPoint`)

A mount point splits into a hidden **base** (leading `../`, `./`, `/` segments) and a
**visible part**: `../../../TheIsle/Content/` → base `../../../`, visible `TheIsle/Content`.

| Mode | Panel root shows | Use |
|---|---|---|
| `Folder` (**default**) | the visible part as folders: `TheIsle\Content\…` | see where files land in the game; extraction gives the `repak unpack` layout |
| `Root` | the contents of the mount point | work inside the mount point only |

**Mount folders** (`Folder` mode): each folder of the visible part is listed as an explicit
directory entry with `DIRECTORY` plus `[Options] MountFolderAttr` (default `ReadOnly`; also
`Hidden`, `System`, combinations, `None`). Real folders are only implied by file paths and
carry no attributes. In TC:
- `ReadOnly`: a colour rule whose search template requires both *Directory* and *Read only*.
- `Hidden`: TC dims hidden items when hidden files are shown (no rule), but the mount
  folders disappear when hidden files are not shown.

Extracting a mount folder creates a plain folder. Names are never decorated (`<…>`): shown
names travel back into extraction paths and `DeleteList`, and `<` `>` are invalid in file
names.

**Mode consistency (M1):** the plugin will remember, per archive path, which mode its last
listing used, and map `PackFiles`/`DeleteFiles` paths with that mode, so changing the mode
while a pak is open can't make a delete hit the wrong entries.

### 3.3 Extracting and testing **[done]**

- `PK_EXTRACT`: create parent folders, write the entry, report progress; cancel →
  `E_EABORTED`. A partial file is deleted on any failure.
- `PK_TEST`: decompress to nowhere.
- Errors: create → `E_ECREATE`, write → `E_EWRITE`, decode → `E_BAD_DATA`, missing Oodle →
  warning + `E_NOT_SUPPORTED`, key prompt cancelled → `E_EABORTED`.
- M3: decompress block by block (constant memory, F7); verify the entry SHA-1 in `PK_TEST` (F9).

### 3.4 AES keys (reading) **[done]**

**When a key is needed:** the index is encrypted (asked on open), or an entry being
extracted is encrypted (asked on first such extraction).

**Lookup order:**

1. A remembered key for the pak's **folder or any parent folder** (case-insensitive, deepest
   match first) with the pak's key GUID. A key for `…\Content\Paks` also covers
   `…\Paks\~mods`.
2. For a non-zero key GUID: any remembered key with that GUID, from any folder; a match is
   saved for this folder too. (A zero GUID identifies nothing: every game's main key has it.)
3. Otherwise the key dialog.

**Validation:** the footer stores the SHA-1 of the *decrypted* index (verified on real paks;
Unreal checks the same). The fork compares it right after decrypting, before any parsing,
so a wrong key is a clean `WrongKey` error. Entry-only encryption can't be validated up
front: the first remembered key is used; a decode failure is logged as "key may not match".

**Key dialog:** modal, built from an in-memory dialog template (no resource compiler).
Shows pak name, folder and key GUID; accepts 64 hex digits (optional `0x`, spaces ignored)
or base64 of 32 bytes; repeats with an error line on a bad format or wrong key;
"Remember for this folder and its subfolders" ticked by default. Cancel → `E_EABORTED`
and no new prompt for the same pak for 10 s (TC opens a pak several times per operation).
Every working key is kept in a session cache until TC exits, even when not remembered.

**Storage:** `uepak.keys.json` in the settings directory:

```json
{
  "format": 1,
  "keys": [
    { "folder": "D:\\Games\\MyGame\\MyGame\\Content\\Paks",
      "guid": "00000000000000000000000000000000",
      "key": "0x0123…CDEF",
      "added": "2026-11-17 15:30:00.000" }
  ]
}
```

One record per (folder, GUID). Written atomically (temp file + rename); a corrupt file is
moved to `uepak.keys.json.bad`. Keys are plain text (TC's master-password store via
`PkSetCryptCallback` is a possible later option) and are never written to the log.

---

## 4. Packing

### 4.1 User flows

| TC action | Plugin call | Status |
|---|---|---|
| Alt+F5 to a new `.pak` | `PackFilesW`, file doesn't exist | **[done]** |
| Alt+F5 with an existing pak's name typed in | `PackFilesW`, file exists | M1 (refused for now); always at the pak's root |
| F5 into an open pak | `PackFilesW`, file exists, `SubPath` = current folder | M1 (refused for now) |
| F8 inside an open pak | `DeleteFilesW` | M1 (stub) |
| "Configure" in the Alt+F5 dialog | `ConfigurePacker` | M2 (stub) |

Alt+F5 does **not** target a pak open in the other panel: TC proposes
`<target dir>\<source folder>.pak`. To add to an open pak, use F5 into it.

TC passes no per-operation settings to `PackFiles`; the current settings (`uepak.ini
[Pack]`, edited by the dialog from M2) apply to every pack.

### 4.2 Path mapping **[done for new paks]**

**The mount point places the files; entry paths are relative to the selected items.**

- Entry path = `SubPath/` (if given) + AddList item, `\` → `/`. Sources are `SrcPath` + item.
- Example: mount point `../../../Townfall/Config/`, select `Engine.ini` in its folder →
  entry `Engine.ini` → game path `Townfall/Config/Engine.ini`.
- AddList folder items (trailing `\`) produce no entry; empty folders can't be stored.
- "Also pack path names" off (`PK_PACK_SAVE_PATHS` unset): TC still sends relative paths;
  the plugin keeps file names only. Names that collide (case-insensitive) → refused with
  the duplicates listed, nothing written.
- "Move to archive" (`PK_PACK_MOVE_FILES`): sources deleted only after the pak is complete;
  then the listed folders are removed if empty (deepest first).
- Unsafe entry paths (`..`, absolute, `:`) are refused.
- Entries are written sorted by path (deterministic output).
- Mount point normalisation: `\` → `/`, trailing `/` added, empty → `../../../`; otherwise
  written as typed (`/Game/` style works).

#### 4.2.1 Existing paks: view path → entry path (M1)

**`Root` mode:** entry path = view path. No guards.

**`Folder` mode** (*V* = visible part of the mount point, e.g. `TheIsle\Content`):

- Target **inside** *V*: entry path = target minus `V\`. Mount point unchanged.
- Target **outside** *V* (panel root, an upper mount folder, or beside one such as
  `TheIsle\Plugins`): the visible part becomes the deepest common folder of *V* and all
  targets. Ask first:

  > These files go outside the pak's mount point `../../../TheIsle/Content/`.
  > The mount point will change to `../../../TheIsle/`. Files already in the pak keep
  > their game paths (`TheIsle/Content/...`); only the pak's index is rewritten, no file
  > data is repacked. Continue?

  Yes → prefix every existing entry with the dropped segments, then add the new files.
  No → `E_EABORTED`, nothing written. Cheap because entry paths exist only in the index.
- The hidden base never changes. Comparisons are case-insensitive.

### 4.3 New paks **[partial]**

Settings: `[Pack]` mount point, version, compression, block size (§6).

- **[done]** Per-entry fallback: an entry that doesn't shrink is stored uncompressed.
- **[done]** Checked before writing: version/compression compatibility (V2 has no
  compression; LZ4/Zstd need V8A+), Oodle library loadable.
- **[done]** Written to `<pak>.uepak-tmp`, renamed into place at the end: errors and
  Cancel never leave a partial pak.
- **[done]** Progress reported per file; Cancel → `E_EABORTED`.
- **[done]** Read/write failures → error popup naming the file, then `E_EABORTED` (so TC
  doesn't add a second, vaguer message). Settings problems → warning + `E_NOT_SUPPORTED`.
- **[done]** Verified: primary index, path-hash index and full-directory index SHA-1s are
  correct (UE checks them on mount), same layout as real UE paks.
- M3: files are currently read into memory whole; streaming writes (F5) will keep memory
  flat and report progress per block.
- M2: compression level and Oodle compressor (currently the codec defaults; Oodle uses
  Mermaid / Normal).

### 4.4 Modifying existing paks (M1; encryption and compact: M4)

New/replaced data is written where the old index was, then a new index and footer.

- **Kept:** version, mount point (except §4.2.1), path-hash seed, compression slots. The
  `[Pack]` version and mount point are ignored; mismatches are logged.
- **Compression:** the `[Pack]` method for new entries; added to a free footer slot (v8+
  has 5) if missing. No free slot, or version < 8 with a method other than
  Zlib/Gzip/Oodle → refused with a message.
- **Replace:** same path → replaced (TC doesn't confirm for plugins; logged).
- **Refused:** v9 paks with a frozen index; versions repak can't write; (until M4)
  encrypted paks.
- **Crash safety (transactional append):**
  1. Save the original tail (index + footer) to `<pak>.uepak-undo` and remember the length.
  2. Append data, write the new index and footer, flush.
  3. Success → delete the undo file. Failure/cancel → truncate, restore the tail, delete
     the undo file.
  4. On open, if `<pak>.uepak-undo` exists, offer to restore the previous state.
- **Dead space** from replaced/deleted data is not tracked; it grows until a compact rebuild.
- **M4, encrypted paks:** allowed after a confirmation:

  > You are trying to add files to an encrypted pak. The new files and the pak's index
  > will be encrypted with the same key. Continue?

  Key from the read lookup (§3.4). New entries encrypted if existing ones are; index
  re-encrypted if it was; key GUID kept; footer hash = SHA-1 of the plain index. Applies
  to delete and compact too.
- **M4, compact when modifying** (`[Pack] Compact=1`, dialog checkbox): build a complete
  new pak (`<pak>.uepak-new`) from the kept entries (stored bytes copied, no
  recompression) plus the new ones, then replace the original. No dead space, no undo file.

### 4.5 Deleting (M1)

- `DeleteList` items are view paths exactly as listed (§11): files one per line, a folder
  as `folder\*.*` (everything below it).
- Mapped as in §4.2.1; deleting never changes the mount point; deleting a mount folder
  deletes everything below it.
- Index-only rewrite with the §4.4 transaction; deleting everything leaves a valid empty pak.
- Same refusals as §4.4.

### 4.6 Compression methods for writing

| Method | Status | Levels (M2) | Notes |
|---|---|---|---|
| None | **[done]** | — | |
| Zlib | **[done]** | 1–9 | Read by every UE4/UE5 version |
| Gzip | **[done]** | 1–9 | Rarely used by games |
| Oodle | **[done]**, to verify in TC | HyperFast4…Optimal5, default Normal; compressor Kraken/Mermaid/Selkie/Leviathan/Hydra, default Mermaid | Needs the `deps\` DLL. UE's default for shipped games |
| Zstd | **[done]** | 1–22 | Not built into stock UE; only games that enable it can read it |
| LZ4 | **[done]** | — | Only games that enable it |

### 4.7 Block size **[done]**

- `[Pack] BlockSize`, default **64 KB** (UnrealPak default); accepts bytes or `64K`/`1M`,
  1 KB – 64 MB.
- Up to 126,976 bytes (124 KB) uses the compact encoding every engine reads. Larger
  values use the extended encoding, which older engines can't read (the dialog will warn).

---

## 5. Settings dialog (`ConfigurePacker`) (M2)

Modal, child of TC's window, built from an in-memory dialog template like the key dialog
(no resource compiler), `MS Shell Dlg 2`, DPI scaling through dialog units.

```
┌ UE PAK plugin settings ───────────────────────────────┐
│ Packing                                               │
│   Mount point     [../../../                       ]  │
│   PAK version     [v11 (UE 4.26 – 5.x)          ▼]   │
│   Compression     [Oodle                         ▼]   │
│   Level           [Normal                        ▼]   │
│   Oodle compressor[Mermaid                       ▼]   │
│   Block size      [64 KB                         ▼]   │
│   [ ] Compact when modifying (rewrite whole pak)      │
│   ⚠ (warning line: Zstd/LZ4, large blocks, Oodle DLL) │
│                                                       │
│ Browsing                                              │
│   Mount point     (•) as folders  ( ) as root         │
│   Mount folders   [ReadOnly                      ▼]   │
│   [ ] Debug log (uepak.log)                           │
│                                                       │
│ [Defaults]                          [ OK ] [ Cancel ] │
└───────────────────────────────────────────────────────┘
```

- PAK versions labelled with their UE range: v12 (5.3+), **v11 (4.26–5.x, default)**,
  v10, v9 (4.25), v8B (4.23–4.24), v8A (4.22), v7 (4.21), v5 (4.20), v4 (4.16–4.19),
  v3 (4.3–4.15), v2 (4.0–4.2).
- Level / Oodle compressor enabled only for methods that use them.
- The Oodle line shows which DLL would be loaded, or "not found in deps\".
- OK saves to `uepak.ini` in the settings directory (`WritePrivateProfileStringW`, so
  comments survive); if that directory has no `uepak.ini` yet, the shipped one is copied
  first. Defaults resets the fields without saving.

---

## 6. Configuration file (`uepak.ini`)

```ini
[Options]
MountPoint=Folder          ; Folder (default) | Root                      [done]
MountFolderAttr=ReadOnly   ; ReadOnly | Hidden | System | None, combinable [done]
DebugLog=0                 ; 1 = uepak.log (§7)                           [done]

[Pack]
MountPoint=../../../       ;                                              [done]
Version=V11                ; V2 … V12 (V1, V6 not writable)               [done]
Compression=None           ; None | Zlib | Gzip | Oodle | Zstd | LZ4      [done]
BlockSize=64K              ; bytes, or 64K / 1M                            [done]
Level=                     ; method-specific, empty = default              (M2)
OodleCompressor=Mermaid    ; Kraken | Mermaid | Selkie | Leviathan | Hydra (M2)
Compact=0                  ; 1 = full rebuild when modifying               (M4)
```

- Keys are case-insensitive and ignore `_` (`DebugLog` = `debug_log`). **[done]**
- Invalid values keep the default and are logged. **[done]**
- Re-read on every `OpenArchive` and `PackFiles` call (no TC restart). **[done]**
- Read order: settings directory, then the shipped file in the plugin folder. **[done]**

---

## 7. Diagnostics **[done]**

- `DebugLog=1` → `uepak.log` in the plugin folder, appended to, one line per event with a
  local timestamp and thread id.
- Plugin folder not writable → the log is kept in memory (last 512 KB) and copied to the
  clipboard on every failed operation; popups say where the log is.
- Logged: environment (paths, `deps\` contents, Oodle search paths, options and option
  warnings); raw footer of each opened pak (independent parser); open result and key
  lookup steps; per-entry extract details and timing; pack arguments, resolved settings,
  each packed entry, result and size; panics.
- Never logged: AES keys.

---

## 8. Robustness rules

- **[done]** Every export runs inside a panic guard → error popup + safe return value.
- **[done]** No network access. The Oodle download path is compiled out.
- **[done]** Unsafe entry names are never extracted or packed.
- **[done]** Untrusted lengths from pak files never drive allocations (fork guards); a wrong
  key is detected before parsing.
- **[done]** New paks and extracted files are never left half-written.
- M1: a modified pak is never left half-written (§4.4).
- M5: no modal popups during background operations (§8.1).

### 8.1 Background operations (M5)

`GetBackgroundFlags` is not exported yet, so TC runs everything in the foreground. The
progress callback is already per-thread. Remaining for `BACKGROUND_UNPACK |
BACKGROUND_PACK`: background-safe popups (key prompt, warnings) and locking around
`uepak.keys.json` writes.

---

## 9. Fork changes (`Civ01/repak`, branch `oodle-search-paths`)

| # | Change | State |
|---|---|---|
| F1 | Oodle: search paths, any `oo2core_N`, no hash pin, failures not cached, download behind a feature, no unwrap, loaded-path reporting | ✅ |
| F2 | `entries()` / `entry()` → `EntryInfo` | ✅ |
| F3 | v12; `UnsupportedVersion`; probing reports `WrongKey` > `Encrypted` > `UnsupportedVersion`; wrong-key detection by index SHA-1; allocation guards; invalid UTF-16 names don't panic | ✅ |
| F4 | Writer: ✅ configurable block size, ✅ store entries that don't shrink; ⬜ levels per method, Oodle compressor/level | partial (M2) |
| F5 | Streaming writes from a `Read`, with progress/cancel callback | M3 |
| F6 | `PakWriter::remove`, add a compression method to an existing pak's slots, expose the original index offset for the undo tail, copy stored entries between paks | M1 (M4 for copy) |
| F7 | Block-by-block decompression on read | M3 |
| F8 | Read v13–15 | M5 |
| F9 | Entry SHA-1 verification | M3 |
| F10 | AES writing: encrypted index and entries (16-byte padded blocks, AES-256-ECB), key GUID kept | M4 |

Each change gets fork tests; generally useful ones are offered upstream.

---

## 10. Testing

**Automated (33 tests, run on Windows CI on every push):**

- Reading: synthetic round trips for None/Zlib/Gzip/Zstd/LZ4, v12, mount folders and
  attributes, future version refused, non-pak refused, null/bad handles, panic guard,
  FFI struct layout.
- AES: repak's public encrypted test paks (`testdata/`): cancel, bad format, wrong key,
  right key, session cache, remembered key + parent folder, corrupt keys file, entry-only
  encryption.
- Packing: every compression round trip (incl. raw fallback), TC-style AddList with folders,
  SubPath, flattening + duplicate refusal, move-to-archive, unreadable source, version/
  compression refusal, missing Oodle, cancel (no partial pak), existing pak refused.
- Options parsing; key parsing (hex/base64); pack planning.
- Optional env-driven tests (not in CI): `UEPAK_SAMPLES` (real paks), `UEPAK_KEYED_PAK` +
  `UEPAK_KEYED_KEY` (real encrypted pak; keys never committed), `UEPAK_DEBUG_PAK`
  (diagnostic log for one file).

**Manual / tooling:** Wine harness that drives the real key dialog; Python script checking
the three index SHA-1s of written paks.

**Planned:** M1 modify flows (add, replace, delete all, cancel mid-write → byte-identical
pak, undo-file recovery); M3 >4 GB and block-boundary cases.

**Real-world samples:**

| Sample | Covers | Result |
|---|---|---|
| UE4.27 / UE5.1 / UE5.8 mod paks | v11, uncompressed | ✅ list + extract |
| zlib_4.22.pak (2,959 entries) | v8A, Zlib | ✅ list + extract |
| Oodle_unecrypted_5.4.pak, oodle_extra_5.9.2.pak | v11, Oodle | ✅ list; Oodle extraction covered by the encrypted_5.6 test in TC |
| encrypted_5.6.pak | v11, encrypted index, Oodle | ✅ list + extract with key (user-tested in TC) |
| EndOfAbyss-Windows.pak | v12, encrypted index, Oodle | ✅ key validated by index SHA-1, 4,587 entries listed |
| Encrypted entries with a plain index | per-entry decryption | synthetic only (repak test pak) |
| v8B / v9 game pak | UE 4.23 – 4.25 | none available |
| Plugin-packed pak loaded by a game | packing end to end | to do (user) |
| Oodle packing | Oodle compressor via DLL | to do (user, Windows) |

**In TC, by hand, per release:** install via `pluginst.inf`; browse; F5 extract; Alt+F5 new
pak; F5 into an open pak; F8 delete; Configure; Ctrl+PgDn on a renamed pak; key dialog;
read-only plugin folder (log to clipboard, settings in the `wincmd.ini` folder).

---

## 11. Observed TC behaviour (TC 11 x64)

| Action | What TC passes |
|---|---|
| F8 on files | `DeleteList` = view paths, one per file |
| F8 on a folder | `DeleteList` = `folder\*.*` |
| F5 into an open pak, at its root | `SubPath` = none |
| F5 into an open pak, in a subfolder | `SubPath` = that view path (`Engine\Programs\CrashReportClient`) |
| Any pack with a folder selected | `AddList` = folder item with trailing `\` (`deps\`), then each file with its relative path |
| "Also pack path names" off | `Flags` = 0, but `AddList` still has relative paths |
| "Move to archive" | `Flags` includes 1 (`MOVE_FILES`) |
| Alt+F5 with a pak open in the other panel | new name `<dir>\<source folder>.pak`, `SubPath` none |
| After any `PackFiles` | TC re-opens the pak in list mode to refresh the panel |
| Configure | `Parent` = TC window handle, `DllInstance` = the plugin's module |

---

## 12. Open items

1. **Mount folder marker** (§3.2): which works better in TC, `ReadOnly` + colour rule or
   `Hidden`? The winner becomes the default.
2. **Packing in the field:** Oodle packing on Windows, and a plugin-packed mod pak loaded
   by a game.

Resolved: compact = checkbox; `pluginst.inf` with `file=uepak.wcx64` works; key lookup =
folder/parent then same non-zero GUID; both view modes with `Folder` default and the mount
guard; TC argument formats captured; background operations after packing is stable;
encrypted paks modifiable after a confirmation, no new encrypted paks.
