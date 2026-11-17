# tcl-uepak — Specification

Total Commander 11+ packer plugin (WCX, x64 only) for Unreal Engine `.pak` files.

Status of this document: **draft v1** (2026-11-17). Sections marked **[done]** describe
behaviour that already exists in the skeleton; everything else is planned.

---

## 1. Goals and non-goals

**Goals**

- Browse, extract and test `.pak` files from UE 4.0 to current UE5 inside Total Commander.
- Create new `.pak` files and modify existing ones (add, replace, delete) from TC's
  standard pack (Alt+F5), copy-to-archive (F5 onto an open pak) and delete (F8) flows.
- Never crash or hang Total Commander; every failure ends in a clear message or a TC error code.
- Be easy to diagnose: optional detailed log ([§7](#7-diagnostics)).

**Non-goals**

- IoStore containers (`.utoc` / `.ucas`). Separate project.
- Creating **new** encrypted paks. Existing encrypted paks can be modified (§4.4); the
  AES key is symmetric, so the key that reads a pak can also write it.
- Pak signing (`.sig` files). A modified signed pak will fail signature checks in games
  that enforce them; this is documented, not handled.
- 32-bit Total Commander.
- Game-specific pak variants (custom magic, obfuscated footers; CUE4Parse knows many). Only
  the standard Epic format is supported.

---

## 2. Architecture

| Component | Role |
|---|---|
| `uepak.wcx64` (this repo, Rust `cdylib`) | WCX exports, TC integration, dialogs, settings, logging |
| [`Civ01/repak`](https://github.com/Civ01/repak) fork, branch `oodle-search-paths` | Pak format: read, write, index rewrite, compression, AES decryption |
| `deps\oo2core_N_win64.dll` (user-supplied) | Oodle codec, loaded at runtime |

Plugin folder layout:

```
uepak.wcx64
uepak.ini          shipped defaults (options, see §6)
pluginst.inf       TC auto-install descriptor
deps\              third-party codec DLLs (Oodle), never downloaded
uepak.log          only when DebugLog=1 and the folder is writable
```

Writable data (edited `uepak.ini`, `uepak.keys.json`) lives in the **settings directory**:
the plugin folder if it is writable, otherwise the folder of `wincmd.ini` (from
`PackSetDefaultParams`). **[done]**

### Exported functions

| Export | Mode | Status |
|---|---|---|
| `OpenArchiveW`, `ReadHeaderExW`, `ProcessFileW`, `CloseArchive` | read | done |
| `SetProcessDataProcW`, `SetChangeVolProcW` | both | done (ChangeVol is a no-op) |
| `CanYouHandleThisFileW` | read | done (footer magic in the last 512 bytes) |
| `PackSetDefaultParams` | both | done |
| `GetPackerCaps` | both | done; packing caps added with §4 |
| `PackFilesW` | pack | new paks done; adding to existing paks planned |
| `DeleteFilesW` | pack | planned |
| `ConfigurePacker` | pack | planned (settings dialog, §5) |
| `GetBackgroundFlags` | both | planned, see §8 |
| ANSI variants of the above | both | thin wrappers; TC x64 uses the W versions |

Capabilities once packing ships:
`PK_CAPS_NEW | PK_CAPS_MODIFY | PK_CAPS_MULTIPLE | PK_CAPS_DELETE | PK_CAPS_OPTIONS |
PK_CAPS_BY_CONTENT | PK_CAPS_SEARCHTEXT`. Not set: `PK_CAPS_ENCRYPT`, `PK_CAPS_MEMPACK`,
`PK_CAPS_HIDE`. (TC must re-install the plugin when caps change.)

---

## 3. Reading and unpacking

### 3.1 Supported formats

| Pak version | UE | Read |
|---|---|---|
| 1 | very early UE4 | not supported (no samples, rare) |
| 2–8B | 4.0 – 4.24 | supported |
| 9 (frozen index) | 4.25 | supported, except paks with a frozen index |
| 10–11 | 4.26 – 5.x | supported |
| 12 `Utf8PakDirectory` | 5.3+ | supported (same layout as 11) |
| 13 `SortedDirectoryIndex` | newer UE5 / UE6 | **planned, low priority** (fork work: flat directory index) |
| 14 `PakchunkIndex` | newer | **planned, low priority** (extra footer field) |
| 15 `EncryptionMethod` | newer | **planned, low priority** (encryption method + IVs in footer) |
| unknown / newer | — | refused with "PAK version N is newer than this plugin supports" **[done]** |

The CUE4Parse `EPakFileVersion` and `FPakInfo` sources are the reference for versions 13–15.
No v13+ pak has been seen in the wild so far: games built with UE 5.8.4 still ship v11.

| Compression | Read |
|---|---|
| None, Zlib, Gzip, Zstd, LZ4 | built in **[done]** |
| Oodle | via `deps\oo2core_N_win64.dll`, any N, highest wins **[done]** |

### 3.2 Listing **[done, view modes being revised]**

- Entries are listed from the index; no file data is read until extraction.
- Directories are implied by paths (paks store no directory entries).

#### View modes (`[Options] MountPoint`)

A mount point is split into a hidden **base** (the leading `../`, `./` and `/` segments)
and a **visible part** (the rest). Example: `../../../TheIsle/Content/` → base `../../../`,
visible part `TheIsle/Content`.

| Mode | Panel root shows | Use |
|---|---|---|
| `Folder` (**default**) | the visible part as folders: `TheIsle\Content\…` | see where files land in the game; extraction gives the same layout as `repak unpack` |
| `Root` | the contents of the mount point: `Hiraeth\…`, `TheIsle\…` | work inside the mount point only |

**Mount folders** (`Folder` mode only) **[done]**: every folder of the visible part is
listed as an explicit directory entry with `DIRECTORY` plus the attributes from
`[Options] MountFolderAttr` (default `ReadOnly`; also `Hidden`, `System`, combinations,
`None`). Real folders are only implied by file paths, so they carry no attributes.
Ways to see the difference in TC:
- `ReadOnly`: a colour rule (Configuration → Colours → define colours by file type) whose
  search template requires both *Directory* and *Read only*.
- `Hidden`: TC draws hidden items dimmed when hidden files are shown; no rule needed, but
  the mount folders disappear when hidden files are not shown.
Extracting a mount folder just creates the folder (no attributes are applied). Names are
never decorated (no `<…>`): the shown names travel back into extraction paths and
`DeleteList`, and `<` `>` aren't valid in Windows file names.
*To verify in TC: which marker works best.*

**Mode consistency:** the plugin remembers, per archive path, which mode its last listing
used, and maps `PackFiles` / `DeleteFiles` paths with that mode. Switching the mode in
`uepak.ini` while a pak is open therefore can't make a delete hit the wrong paths.
- Packed size and unpacked size come from the index; >4 GB entries are supported.
- File date/time: the `.pak` file's own modification time (modern paks store none).
- Entries are skipped, and logged, if they contain `..`, are absolute, contain `:`, or have
  empty path components (path-traversal protection). Deleted records (v6+) are skipped.

### 3.3 Extracting and testing **[done]**

- `PK_EXTRACT`: create parent folders, stream the entry to disk, report progress through
  `ProcessDataProcW`; cancel → `E_EABORTED`. A partial file is deleted on any failure.
- `PK_TEST`: decompress to nowhere; verifies decompression.
- Errors: create → `E_ECREATE`, write → `E_EWRITE`, decode → `E_BAD_DATA`, missing Oodle /
  missing key → popup + `E_NOT_SUPPORTED`.
- **Planned:** decompress block by block instead of loading the whole entry into memory, so
  multi-GB compressed entries extract with constant memory (fork change F7).
- **Planned:** check the entry's SHA-1 during `PK_TEST`.

### 3.4 Search inside archives (planned)

Set `PK_CAPS_SEARCHTEXT` so TC's "Find files → search in archives" with text works. TC
extracts each candidate through `ProcessFile`, so this needs no extra code.

### 3.5 AES keys **[done]**

**When a key is needed:** the index is encrypted, or an entry being extracted is encrypted.

**Lookup order:**

1. A key remembered for the pak's **folder or any parent folder** (full paths,
   case-insensitive; the deepest match wins) with the pak's key GUID. A key saved for
   `…\Content\Paks` also covers `…\Paks\~mods` and `…\Paks\LogicMods`.
2. If the key GUID is non-zero: any remembered key with the same GUID, from any folder,
   tried silently. A successful match is remembered for this folder too.
   (A zero GUID identifies nothing: every game's main key has it.)
3. Otherwise, show the key dialog.

**Validation:** the footer stores the SHA-1 of the *decrypted* index (verified on real
paks; Unreal checks the same). After decrypting, the fork compares hashes before parsing
anything, so a wrong key is a clean `WrongKey` error, never garbage parsing. The dialog
then re-opens with "This key does not match this pak". Entry-only encryption (plain index)
can't be validated up front: the first matching remembered key is used, and a decode
failure is logged as "key may not match".

**Key dialog** (modal, parent = TC window):

- Text field for the key: 64 hex digits (optional `0x` prefix, spaces ignored) or base64
  of 32 bytes.
- Shows the pak name, its folder and its key GUID.
- Checkbox **"Remember for this folder"**, on by default.
- OK / Cancel. Cancel means the open fails quietly (`E_EABORTED`, no second message).
- Listing mode and extract mode use the same dialog. Every key that works is kept in a
  session cache (until TC exits) even when "Remember" is off, so the extract pass after a
  listing doesn't ask again. After Cancel, the same pak isn't prompted again for 10 s
  (TC opens a pak several times per operation).
- For a plain index with encrypted entries, listing needs no key; the prompt appears on
  the first extraction of an encrypted entry, at most once per opened archive.
- Built from an in-memory dialog template (no resource compiler); tested under Wine by
  driving the real dialog (bad format → wrong key → right key).

**Storage:** `uepak.keys.json` in the settings directory, separate from `uepak.ini`:

```json
{
  "format": 1,
  "keys": [
    {
      "folder": "D:\\Games\\MyGame\\MyGame\\Content\\Paks",
      "guid": "00000000000000000000000000000000",
      "key": "0x0123…cdef",
      "added": "2026-11-17T15:30:00+03:00"
    }
  ]
}
```

- One record per (folder, GUID), so a Paks folder that mixes keys works.
- Written atomically (write a temp file, then rename). A corrupt file is renamed to
  `uepak.keys.json.bad` and logged; the plugin carries on without remembered keys.
- Keys are stored in plain text. TC's master-password store (`PkSetCryptCallback`) is a
  possible later option.
- Keys are never written to the debug log; it only logs the record and folder.

---

## 4. Packing

### 4.1 User flows

| TC action | Plugin call | Result |
|---|---|---|
| Alt+F5, choose `pak`, new name | `PackFilesW` on a non-existing file | new pak with the current pack settings (§5) |
| Alt+F5 with the name of an existing `.pak` typed in | `PackFilesW` on an existing file | add/replace entries (§4.4), always at the pak's root (`SubPath` is never set) |
| F5 from a panel into an open pak | `PackFilesW` on an existing file | same as above |
| F8 inside an open pak | `DeleteFilesW` | remove entries (§4.5) |
| "Configure" button in the Alt+F5 dialog | `ConfigurePacker` | settings dialog (§5) |

Alt+F5 does **not** target a pak that is open in the other panel: TC proposes a new name
built from the source folder (`<target dir>\<source folder>.pak`). To add to an open pak,
use F5 into it.

TC passes no per-operation settings to `PackFiles`. "Settings for this operation" means
the last values saved in the dialog. They persist and apply to every pack until changed.
The dialog is the same one reached from TC's plugin configuration.

### 4.2 Path mapping

The same model as repak: **the mount point places the files and pak paths are relative to the
selected items.**

- Target path = `SubPath\` (if TC supplies one) + the AddList item, in the panel's view
  (§3.2). Converted to a pak entry path as described in §4.2.1. Paths are relative to
  `SrcPath`, the folder whose contents are being packed.
- Example: mount point `../../../Townfall/Config`, source panel inside the folder that
  contains `Engine.ini`, select `Engine.ini` → entry `Engine.ini`. The game sees it as
  `Townfall/Config/Engine.ini`.
- No extra path prefix setting.
- AddList items ending in `\` (folders) produce no entry; their files are listed by TC
  individually. Empty folders cannot be represented in a pak and are dropped (logged).
- `PK_PACK_SAVE_PATHS` not set → entries use the file name only. TC still sends the full
  relative paths (`deps\oo2core_9_win64.dll`); the plugin strips them. Two files with the
  same name → abort before writing, with the duplicates named in the message.
- `PK_PACK_MOVE_FILES` → delete the source files only after the whole pak is written and
  closed successfully.
- `PK_PACK_ENCRYPT` is never offered (no `PK_CAPS_ENCRYPT`).
- Entry order: sorted by path (repak uses a BTreeMap), so output is deterministic.
- Mount point normalisation: `\` → `/`, a trailing `/` is added, and an empty value becomes
  `../../../`. The value is otherwise written as typed, so `/Game/` style mount points are
  possible.

#### 4.2.1 Existing paks: view path → entry path

New paks are unaffected by the view mode (there is nothing to view yet). For F5 into an
open pak and for F8:

**`Root` mode.** Every view path is relative to the mount point. Entry path = view path.
No guards: you can only work inside the mount point.

**`Folder` mode.** Let *V* be the mount point's visible part (`TheIsle\Content`).

- Target **inside** *V* (equal to *V* or below it): entry path = target minus `V\`.
  The mount point is unchanged. This is the normal case.
- Target **outside** *V*: at the panel root, in a mount folder above the deepest one
  (`TheIsle`), or beside one (`TheIsle\Plugins`). The new visible part becomes the
  deepest common folder of *V* and all targets (`TheIsle`, or empty at the root). Before
  writing, ask:

  > These files go outside the pak's mount point `../../../TheIsle/Content/`.
  > The mount point will change to `../../../TheIsle/`. Files already in the pak keep
  > their game paths (`TheIsle/Content/...`); only the pak's index is rewritten, no file
  > data is repacked. Continue?

  Yes → re-root every existing entry (prefix it with the dropped segments, e.g. `Content/`),
  then add the new files. No → `E_EABORTED`, nothing written.
- Why this is cheap: entry paths exist only in the index. The per-entry record stored
  before each file's data has no name, so changing the mount point is an index rewrite
  (same transactional procedure as §4.4), not a repack.
- The hidden base (`../../../`) never changes, so nothing can be packed above it.
- Comparisons are case-insensitive. The new mount point keeps the original's spelling for
  the segments it keeps.

### 4.3 New paks **[done: basic]**

Written with the dialog's version, mount point, compression method, level, Oodle compressor
and block size.

- **Per-entry fallback:** if compressing an entry doesn't make it smaller, store it
  uncompressed (UnrealPak behaviour). Stops small or already-compressed files from growing.
- Files are streamed from disk block by block. Memory use stays flat for multi-GB inputs
  (fork change F5). *Current build: each file is read into memory whole.*
- Written to `<pak>.uepak-tmp` and renamed into place at the end, so a failed or cancelled
  pack never leaves a partial pak.
- Verified: the primary index, path-hash index and full-directory index SHA-1s match, as
  in real UE paks (UE checks them on mount).
- Progress through `ProcessDataProcW` (bytes read from the source); cancel → delete the
  partial pak, `E_EABORTED`.
- Any failure → delete the partial pak, then return the matching code (`E_EREAD`,
  `E_EWRITE`, `E_ECREATE`, …).
- Oodle selected but no Oodle DLL loadable → popup **before** anything is written,
  `E_NOT_SUPPORTED`.

### 4.4 Modifying existing paks (append + rewrite index)

New and replaced entries' data is written at the position of the old index. Then a new
index and footer are written. Replaced data stays in the file as dead space.

- **Kept from the existing pak:** version, mount point, path-hash seed, existing compression
  slots. The dialog's version and mount point are ignored here; the log notes any mismatch.
- **Compression:** the dialog's method is used for new entries. If the pak's footer doesn't
  list that method yet, it is added to a free compression slot (v8+ has 5). If no slot is
  free, or the version is < 8 and the method isn't Zlib/Gzip/Oodle, abort with a message.
- **Replace:** an entry with the same path is replaced. TC does not ask for confirmation
  for plugins; the log records it.
- **Encrypted paks:** allowed after a confirmation:

  > You are trying to add files to an encrypted pak. The new files and the pak's index
  > will be encrypted with the same key. Continue?

  - The key comes from the same lookup as reading (§3.5); no key → the key prompt.
  - New entries are encrypted when the pak's existing entries are (any encrypted entry
    counts); the index is re-encrypted when it was encrypted. The footer keeps its key
    GUID and stores the SHA-1 of the plain index, as Unreal expects.
  - The same applies to `DeleteFiles` (index re-encrypted) and to compact rebuilds.
- **Refused, read-only message:** v9 paks with a frozen index, versions repak cannot write.
- **Crash safety (transactional append):**
  1. Before writing, copy the original tail (`index_offset … EOF`: index + footer) to
     `<pak>.uepak-undo` next to the pak, and keep the original length.
  2. Append data, write the new index and footer, flush.
  3. Success → delete the undo file. Failure or cancel → truncate to `index_offset`,
     write the saved tail back, delete the undo file.
  4. On open, if `<pak>.uepak-undo` exists (TC or the machine died mid-write), offer to
     restore the pak to its previous state.
- **Dead space:** recorded nowhere; it grows with each replace or delete.
- **Compact when modifying** (dialog checkbox, `[Pack] Compact=0|1`, default off): instead
  of appending, rebuild the pak. Write a new pak next to the original (`<pak>.uepak-new`)
  containing the kept entries (copied as stored: no recompression, compressed blocks are
  reused) plus the new ones, then replace the original with the new file. No dead space;
  costs a full rewrite. The original is untouched until the final rename, so no undo file
  is needed. Also applies to `DeleteFiles`.

### 4.5 Deleting

- `DeleteList` items are view paths, exactly as listed (verified in TC 11 x64):
  - files: `Shaders\StandaloneRenderer\OpenGL\SplashVertexShader.glsl` (`Root`) or
    `Engine\Shaders\…\SplashVertexShader.glsl` (`Folder`);
  - a folder: `Engine\Shaders\StandaloneRenderer\OpenGL\*.*` → everything below it,
    recursively.
- Paths map to entries as in §4.2.1. Deleting never changes the mount point. Deleting a
  mount folder (`Folder` mode) deletes every entry below it.
- Only the index is rewritten (same transactional procedure as §4.4); data becomes dead
  space.
- Deleting every entry leaves a valid, empty pak.
- The same rules as §4.4 apply (confirmation for encrypted paks; frozen and unwritable
  versions refused).

### 4.6 Compression methods for writing

| Method | Levels | Notes |
|---|---|---|
| None | — | |
| Zlib | 1–9, default 6 | Supported by all UE4/UE5 versions |
| Gzip | 1–9, default 6 | Same codec, different framing; rarely used by games |
| Oodle | HyperFast4…Optimal5, default **Normal** | Compressor Kraken / Mermaid / Selkie / Leviathan / Hydra, default **Mermaid**. Needs `deps\` DLL. The UE default for shipped games. |
| Zstd | 1–22, default 3 | Only games built with UE's Zstd plugin can read it; the dialog shows a warning |
| LZ4 | — (no levels) | Only games with LZ4 enabled; the dialog shows a warning |

### 4.7 Block size

- Default **64 KB** (UnrealPak default).
- Choices: 64 KB, 124 KB (126,976 bytes, the largest value with the compact encoding),
  256 KB, 512 KB, 1 MB.
- Values above 126,976 bytes use the extended encoding, which older engines can't read.
  The dialog shows a warning when one is selected. (repak today always writes 126,976.)

---

## 5. Settings dialog (`ConfigurePacker`)

A modal dialog, child of TC's window. It's built from a resource script compiled into the
DLL (`embed-resource`), uses the `MS Shell Dlg 2` font, and scales with DPI through dialog
units.

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
│   ⚠ (warning line for Zstd/LZ4/large blocks/missing Oodle DLL)
│                                                       │
│ Browsing                                              │
│   Mount point     (•) as folders  ( ) as root         │
│   [ ] Debug log (uepak.log)                           │
│                                                       │
│ [Defaults]                          [ OK ] [ Cancel ] │
└───────────────────────────────────────────────────────┘
```

- **PAK version list:** v12 (UE 5.3+), **v11 (UE 4.26 – 5.x, default)**,
  v10 (rare), v9 (4.25), v8B (4.23–4.24), v8A (4.22), v7 (4.21), v5 (4.20), v4 (4.16–4.19),
  v3 (4.3–4.15).
  Each entry is labelled with its UE range.
- **Level and Oodle compressor** are enabled only for methods that use them.
- **The Oodle line** shows which DLL would be loaded, or "not found in deps\".
- **OK** saves to `uepak.ini` in the settings directory and takes effect immediately.
  **Defaults** resets the fields without saving.
- If the settings directory isn't the plugin folder and has no `uepak.ini` yet, the first
  save copies the shipped file there, then applies the changes, so the comments survive.
  Values are written with `WritePrivateProfileStringW`.

---

## 6. Configuration file (`uepak.ini`)

```ini
[Options]
; Browsing: Folder = mount point shown as folders (default), Root = contents of the mount point
MountPoint=Folder
; attributes on mount folders in Folder view: ReadOnly | Hidden | System | None (combinable)
MountFolderAttr=ReadOnly
; 1 = write uepak.log (see SPEC §7)
DebugLog=0

[Pack]
MountPoint=../../../
; V3 … V12
Version=V11
; None | Zlib | Gzip | Oodle | Zstd | LZ4
Compression=None
; method-specific; empty = method default
Level=
; Kraken | Mermaid | Selkie | Leviathan | Hydra
OodleCompressor=Mermaid
; bytes
BlockSize=65536
; 1 = rebuild the whole pak when adding/replacing/deleting (no dead space)
Compact=0
```

- Keys are case-insensitive and ignore `_` (`DebugLog` = `debug_log`). **[done]**
- Unknown or invalid values fall back to defaults and are logged.
- The file is re-read on every `OpenArchive`, `PackFiles` and `DeleteFiles` call, so no TC
  restart is needed.
- Read order: settings directory, then the shipped file in the plugin folder. **[done]**

---

## 7. Diagnostics

**[done]** for reading; packing adds its own events.

- `DebugLog=1` → `uepak.log` in the plugin folder, appended to, one line per event with a
  local timestamp and thread id.
- If the plugin folder isn't writable, the log is kept in memory (last 512 KB) and copied
  to the clipboard on every failed operation. Popups end with where the log is.
- **Logged:** environment (paths, `deps\` contents, Oodle search paths, options); raw footer
  of each opened pak (from an independent parser, so it works when repak fails); open
  result; per-entry operation, sizes, compression, destination, time and error; panics.
- **Packing adds:** the resolved settings, each added, replaced or deleted entry (path, sizes,
  method), the undo/restore steps, and total time.
- **Never logged:** AES keys.

---

## 8. Robustness rules

- Every export runs inside a panic guard → error popup + safe return value. **[done]**
- No modal popups while TC is running a background operation (§8.1); errors go to the log
  and the return code instead.
- No network access, ever. The Oodle download path is compiled out. **[done]**
- Path traversal: unsafe entry names are never extracted (§3.2). **[done]**
- A modified pak is never left half-written (§4.4).

### 8.1 Background operations

`GetBackgroundFlags` initially returns `0` (TC runs plugin operations in the foreground).
To return `BACKGROUND_UNPACK | BACKGROUND_PACK` later, the global `ProcessDataProc`
pointer has to become per-operation (TC sets it per thread) and popups need a
background-safe path. Tracked as an open item.

---

## 9. Required fork changes (`Civ01/repak`)

| # | Change | Needed for |
|---|---|---|
| F1 | ✅ Oodle search paths, optional download, no unwrap | Oodle via `deps\` |
| F2 | ✅ `entries()` / `EntryInfo` | listing |
| F3 | ✅ v12, `UnsupportedVersion`, clear `Encrypted` error | reading |
| F4 | Compression settings: ✅ block size, ✅ uncompressed fallback when not smaller; level per method, Oodle compressor + level still to do | §4.3, §4.6, §4.7 |
| F5 | Streaming writes from a `Read` (block by block) with a progress / cancel callback | large files, progress |
| F6 | `PakWriter::remove(path)`; adding a compression method to an existing pak's footer slots; expose original `index_offset` for the undo tail; copy an entry's stored bytes into another pak without recompressing (compact) | §4.4, §4.5 |
| F7 | Block-by-block decompression on read (constant memory), with progress | §3.3 |
| F8 | Read versions 13, 14, 15 (flat directory index, pakchunk index, encryption method + IVs) | §3.1 |
| F9 | Entry SHA-1 verification API | `PK_TEST` |
| F10 | AES writing: encrypted index (plain-index SHA-1 in footer, key GUID kept), encrypted entries (each compression block / uncompressed data padded to 16 bytes, AES-256-ECB, as Unreal does); round-trip tests against repak's encrypted test paks and real game paks | §4.4 encrypted paks |

Each change gets tests in the fork (round-trip per version × method), and is offered
upstream where it's generally useful.

---

## 10. Testing

**Automated (CI, Windows):**

- Synthetic round trips for every writable version × every method × an edge-case payload:
  empty file, 1 byte, exact block multiple, block + 1, a >4 GB sparse entry (nightly only).
- Pack with the plugin → read back with repak (and with UnrealPak/FModel by hand for
  releases).
- Modify flows: add, replace, delete all; cancel mid-write → the pak is byte-identical
  to before; a simulated crash (undo file present) → restore.
- Errors: missing Oodle DLL, wrong AES key, corrupt index, truncated file, unwritable
  destination.
- repak's own test paks (v3–v11, plain / compressed / encrypted with the test key).

**Real-world sample matrix** (`UEPAK_SAMPLES`, not committed):

| Sample | Covers | Have |
|---|---|---|
| UE4.27 mod pak | v11, uncompressed | ✅ lists, extracts |
| UE5.1 mod pak | v11, uncompressed | ✅ lists, extracts |
| UE5.8 config pak | v11, tiny | ✅ lists, extracts |
| zlib_4.22.pak (2,959 entries) | v8A, Zlib | ✅ lists, extracts |
| Oodle_unecrypted_5.4.pak | v11, Oodle | ✅ lists; extraction to verify on Windows with the DLL |
| oodle_extra_5.9.2.pak | v11, Oodle | ✅ lists; extraction to verify on Windows with the DLL |
| encrypted_5.6.pak | v11, encrypted index, Oodle | ✅ lists + extracts with key (user-tested in TC) |
| EndOfAbyss-Windows.pak | v12, encrypted index, Oodle | ✅ key validated by index SHA-1; 4,587 entries listed |
| Encrypted entries, plain index | per-entry decryption | needed |
| v8B / v9 game pak | 4.23 – 4.25 | needed (none available yet) |
| v13+ pak | new formats | none exist in the wild yet |
| >4 GB pak | 64-bit sizes | synthetic (nightly CI) |

**In TC, by hand, per release:** install via `pluginst.inf`; browse; F5 extract; Alt+F5 new
pak; F5 into an open pak; F8 delete; Configure dialog; Ctrl+PgDn on a renamed pak; text
search in archive; plugin folder read-only (log to clipboard, settings in the `wincmd.ini`
folder).

---

## 11. Observed TC behaviour (TC 11 x64, captured with the logging stubs)

| Action | What TC passes |
|---|---|
| F8 on files | `DeleteList` = view paths, one per file |
| F8 on a folder | `DeleteList` = `folder\*.*` |
| F5 into an open pak, at its root | `SubPath` = none |
| F5 into an open pak, in a subfolder | `SubPath` = that view path (`Engine\Programs\CrashReportClient`) |
| Any pack, a folder selected | `AddList` = folder entry with trailing `\` (`deps\`), then each file with its relative path |
| "Also pack path names" off | `Flags` = 0, but `AddList` still has relative paths |
| "Move to archive" | `Flags` includes 1 (`MOVE_FILES`) |
| Alt+F5 with a pak open in the other panel | new file name `<dir>\<source folder>.pak`, `SubPath` none (the open pak isn't targeted) |
| After any `PackFiles` | TC re-opens the pak in list mode to refresh the panel |
| Configure | `Parent` = TC window handle, `DllInstance` = the plugin's module |

## 12. Open items

1. **Mount folder marker** (§3.2): verify in TC whether `ReadOnly` + colour rule or
   `Hidden` works better, and make that the default.

Resolved:
- Compact = checkbox (§4.4).
- `pluginst.inf` with `file=uepak.wcx64` installs in TC 11 x64.
- Key lookup = folder or parent folder, then same non-zero GUID (§3.5).
- View modes: both supported, `Folder` is the default, mount-point guard in `Folder`
  mode (§3.2, §4.2.1).
- TC argument formats captured (§11).
- Background operations: after packing is stable (§8.1).
- Encrypted paks: modifiable after a confirmation; no new encrypted paks (§4.4).
