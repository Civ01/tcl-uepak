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
| Packing | New paks (Alt+F5), adding/replacing in existing paks (F5 into a pak), deleting (F8); every compression method with levels and Oodle compressor; crash-safe in-place changes with undo/recovery; changing encrypted paks (after a confirmation); compact rebuild |
| Settings | Configure dialog (TC's pack dialog / plugin settings) writing `uepak.ini` |
| Robustness | Panic guard on every export, allocation guards in the fork, path-traversal checks, no partial output files |
| Diagnostics | `DebugLog`, clipboard fallback, independent footer parser |
| Large files | Streaming reads and writes (peak memory ~12 MB for 3 GB files), per-block progress/Cancel, free-space check, reuse of space at the end of the pak with an undo-size limit, SHA-1 check in "test" |
| Tests | 60 automated tests (Windows CI) + 168 in the fork; Wine-driven key and settings dialogs; index-hash verification; byte parity with repak and (encryption, compact copy) with UnrealPak's test paks |

### Roadmap

| # | Milestone | Contents | Fork work |
|---|---|---|---|
| M1 | ✅ Modify existing paks | F5 into an open pak (add/replace), F8 delete, view-path mapping with the `Folder`-mode mount guard, per-archive mode memory, transactional append with undo file, crash recovery | F6 ✅ |
| M2 | ✅ Settings dialog | `ConfigurePacker` dialog (§5); compression levels and Oodle compressor | F4 ✅ |
| M3 | ✅ Large files | Streaming writes and block-by-block reads (constant memory, per-block progress), entry SHA-1 check in `PK_TEST`, free-space check, reuse of end-of-pak space with `UndoLimit` | F5, F7, F9 ✅ |
| M4 | ✅ Encrypted modify + compact | Adding to/deleting from encrypted paks (§4.4), "Compact when modifying" rebuild | F10, F6 ✅ |
| **M5** | **Polish** (next) | Text search in archives, background operations, v13–15 reading (no samples exist yet) | F8 |

Next: M5 (text search, background operations, v13–15 reading).

### Backburner (no milestone)

- **Alt+F5 onto an existing pak → "Replace the pak":** TC's model is to add to it (current
  behaviour). An optional third answer in the question could rebuild the pak from only
  the selected files instead. Revisit after M4 (shares the compact-rebuild code).

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
- Creating **new** encrypted paks. Existing encrypted paks can be changed (§4.4); the AES
  key is symmetric, so the key that reads a pak can also write it.
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
| `lib.rs` | Exports, panic guard, init/options reload, popups, pack/delete dispatch |
| `read.rs` | Open (incl. AES key resolution, undo recovery), list, extract, test, view-mode memory |
| `pack.rs` | Pack planning (AddList → entries), new paks, add to / delete from existing paks |
| `modify.rs` | In-place transaction: undo file, rewrite, truncate, rollback, restore |
| `view.rs` | Mount point split, view path ↔ entry path mapping |
| `settings.rs` | Settings model, choice lists, warnings, comment-preserving ini writer |
| `archive.rs` | Opened-pak model, view paths, footer sniffer |
| `keys.rs`, `dialog.rs` | AES key parsing/storage/lookup; key prompt, settings dialog, confirmations |
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
| `PackFilesW` | **[done]** new and existing paks |
| `DeleteFilesW` | **[done]** |
| `ConfigurePacker` | **[done]** settings dialog |
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
directory entry with `DIRECTORY` plus `[Options] MountFolderAttr` (default `Hidden`; also
`ReadOnly`, `System`, combinations, `None`). Real folders are only implied by file paths and
carry no attributes. In TC:
- `Hidden` (default, chosen after testing in TC): TC dims hidden items when hidden files are
  shown (no rule needed), but the mount folders disappear when hidden files are not shown.
- `ReadOnly`: meant for a colour rule; in TC 11 testing the colour rule didn't pick it up.

Extracting a mount folder creates a plain folder. Names are never decorated (`<…>`): shown
names travel back into extraction paths and `DeleteList`, and `<` `>` are invalid in file
names.

**Mode consistency [done]:** the plugin remembers, per archive path, which mode its last
listing used, and maps `PackFiles`/`DeleteFiles` paths with that mode, so changing the mode
while a pak is open can't make a delete hit the wrong entries. Listing and editing use the
same mount-point split (`view.rs`).

### 3.3 Extracting and testing **[done]**

- `PK_EXTRACT`: create parent folders, write the entry, report progress; cancel →
  `E_EABORTED`. A partial file is deleted on any failure.
- `PK_TEST`: decompress to nowhere.
- Errors: create → `E_ECREATE`, write → `E_EWRITE`, decode → `E_BAD_DATA`, missing Oodle →
  warning + `E_NOT_SUPPORTED`, key prompt cancelled → `E_EABORTED`.
- **[done]** Streaming: entries are read and decompressed one compression block at a time
  (1 MiB chunks for stored entries; encrypted data decrypted per 16-byte-aligned span),
  so memory doesn't grow with entry size; progress and Cancel per block.
- **[done]** `PK_TEST` also checks each entry's SHA-1 (`E_BAD_DATA` if damaged). The hash
  comes from the record stored before the data (v10+ index entries carry none). Rules,
  verified on real Unreal paks (UE 4.22 – 5.9, incl. an encrypted one) and UnrealPak's
  test paks: the hash covers the stored bytes; for encrypted compressed entries, the
  encrypted bytes including AES padding; for encrypted stored entries, the encrypted bytes
  without the padding (found in M4 with UnrealPak's test paks).

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

**Changed key:** if a remembered key matched the pak by its exact non-zero key GUID (step 1
or 2) but doesn't decrypt it, the key was most likely changed: the game re-encrypted its
paks with a new key in an update. The dialog then doesn't just say "wrong key"; it opens
with: *"The key saved for this key GUID doesn't match this pak. The GUID is the same, so
the pak was most likely re-encrypted with a new key by a game update. Enter the new key.
(Saved for: <folder>)"*. For a zero GUID (folder match only) it says the game may have been
updated. **[done]**

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
| Alt+F5 with an existing pak's name typed in | `PackFilesW`, file exists | **[done]**, always at the pak's root |
| F5 into an open pak | `PackFilesW`, file exists, `SubPath` = current folder | **[done]** |
| F8 inside an open pak | `DeleteFilesW` | **[done]** |
| "Configure" in the Alt+F5 dialog | `ConfigurePacker` | **[done]** |

Alt+F5 does **not** target a pak open in the other panel: TC proposes
`<target dir>\<source folder>.pak`. To add to an open pak, use F5 into it.

Alt+F5 to a name that already exists **adds to** that pak (TC calls `PackFiles` on the
existing file, as for its own archivers; it passes nothing that says "overwrite"). So the
mount-point question can appear there too, when the new files fall outside that pak's mount
point. To get a fresh pak instead, delete or rename the old one first.

TC passes no per-operation settings to `PackFiles`; the current settings (`uepak.ini
[Pack]`, edited by the Configure dialog) apply to every pack.

### 4.2 Path mapping **[done]**

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
- Mount point normalisation: `\` → `/`, empty → `../../../`; otherwise written exactly as
  typed, with no trailing `/` added (Unreal adds it when mounting; real UE paks exist both
  ways). This keeps output byte-identical to repak: verified against a repak-made pak
  (`UEPAK_PARITY_PAK` test).

#### 4.2.1 Existing paks: view path → entry path **[done]**

**`Root` mode:** entry path = view path. No guards.

**`Folder` mode** (*V* = visible part of the mount point, e.g. `TheIsle\Content`):

- Target **inside** *V*: entry path = target minus `V\`. Mount point unchanged.
- Target **outside** *V* (panel root, an upper mount folder, or beside one such as
  `TheIsle\Plugins`): the visible part becomes the deepest common folder of *V* and all
  targets. Ask first, with concrete examples (first existing entry, first new file):

  > These files go outside the pak's mount point ../../../G1R/Config.
  >
  > To fit them, the mount point becomes ../../../G1R, and "Config/" is added to the path
  > of every file already in the pak (it becomes a real folder inside the pak).
  >
  >   UserEngine.ini  →  Config/UserEngine.ini
  >
  > Where the game finds them doesn't change: G1R/Config/UserEngine.ini
  > New files go to: G1R/Readme.txt
  >
  > Only the pak's index is rewritten; no file data is repacked. Continue?

  Yes → prefix every existing entry with the dropped segments, then add the new files.
  No → `E_EABORTED`, nothing written. Cheap because entry paths exist only in the index.
- The hidden base never changes. Comparisons are case-insensitive.

### 4.3 New paks **[done]** (streaming: M3)

Settings: `[Pack]` mount point, version, compression, level, Oodle compressor, block
size (§6).

- **[done]** Per-entry fallback: an entry that doesn't shrink is stored uncompressed.
- **[done]** Checked before writing: version/compression compatibility (V2 has no
  compression; LZ4/Zstd need V8A+), Oodle library loadable.
- **[done]** Written to `<pak>.uepak-tmp`, renamed into place at the end: errors and
  Cancel never leave a partial pak.
- **[done]** Cancel → `E_EABORTED`.
- **[done]** Read/write failures → error popup naming the file, then `E_EABORTED` (so TC
  doesn't add a second, vaguer message). Settings problems → warning + `E_NOT_SUPPORTED`.
- **[done]** Verified: primary index, path-hash index and full-directory index SHA-1s are
  correct (UE checks them on mount), same layout as real UE paks.
- **[done]** Files are streamed from disk one block at a time (memory flat for multi-GB
  inputs); progress and Cancel per block. Output is byte-identical to the in-memory writer.
- **[done]** Free-space check before writing: if the sources (an upper bound; compression
  usually shrinks them) plus the index may not fit, ask before starting. If the disk fills
  anyway, nothing is left behind.
- **[done]** Byte-identical to repak for the same settings (verified with a repak-made pak;
  `UEPAK_PARITY_PAK` test). Default levels match repak's (Zlib/Gzip 1, Zstd 3, Oodle
  Mermaid/Normal).

### 4.4 Modifying existing paks **[done]**

New/replaced data is written where the old index was (or earlier, see *space reuse*), then a
new index and footer; the file is truncated to the new end. Existing data is never moved.

- **Kept:** version, mount point (except §4.2.1), path-hash seed, compression slots. The
  `[Pack]` version and mount point are ignored; mismatches are logged.
- **Compression:** the `[Pack]` method for new entries; added to a free footer slot (v8+
  has 5) if missing. No free slot, or version < 8 with a method other than
  Zlib/Gzip/Oodle → refused with a message.
- **Replace:** same path (case-insensitive) → replaced (TC doesn't confirm for plugins;
  logged).
- Verified: modified paks pass the UE index-hash checks; data before the old index is
  never touched.
- **Refused:** v9 paks with a frozen index; versions repak can't write.
- **Crash safety (transactional append):**
  1. Save the original tail (index + footer) to `<pak>.uepak-undo` and remember the length.
  2. Append data, write the new index and footer, flush.
  3. Success → delete the undo file. Failure/cancel → truncate, restore the tail, delete
     the undo file.
  4. On open, if `<pak>.uepak-undo` exists, ask: Yes = restore the previous state,
     No = keep the current state and delete the backup. Changes to the pak are refused
     while an undo file is pending.
  - The file is truncated to the new end after every rewrite (a delete-only change writes
    a smaller index than before).
- **[done] Space reuse:** replaced and deleted entries are removed before anything is
  written. If no remaining entry lies after some point before the index (the removed
  entries were the last ones, or earlier dead space sits there), new data starts at that
  point instead of at the index. Replacing the file you added last, again and again,
  doesn't grow the pak; deleting the last files shrinks it. A check refuses to start if
  any remaining entry would be overwritten.
- **[done] Undo size limit:** the undo copy covers everything from where writing starts:
  normally just the index, but with space reuse also the reused data. `[Pack] UndoLimit`
  (default **1 GB**; `0` = always ask, `unlimited`) caps what is copied without asking.
  Over the limit, ask: *Yes* = reuse the space (make the big undo copy), *No* = add at the
  end instead (small undo copy; the pak grows). If even the plain index copy exceeds the
  limit, ask once more; *No* cancels.
- **[done] Free-space check:** undo copy + growth of the file (upper bound); ask if it may
  not fit.
- **Dead space** elsewhere (not at the end) is not tracked; it stays until a compact
  rebuild.
- **[done] Encrypted paks:** changed after a confirmation (asked before anything else is
  written; *No* leaves the pak untouched):

  > You are trying to add files to an encrypted pak. The new files and the pak's index
  > will be encrypted with the same key. Continue?

  The wording follows what is encrypted: index only ("its index will be encrypted again …;
  its files aren't encrypted, so the new ones won't be either"), entries only ("the new
  files will be encrypted with the same key as the files already in it"), or deleting
  ("You are trying to delete files from an encrypted pak. Its index will be encrypted
  again with the same key.").
  - **Key:** the read lookup and dialog (§3.4), so a remembered key is used silently.
    An encrypted index validates the key (index SHA-1). With a plain index and encrypted
    entries, the key is checked by decompressing the smallest encrypted compressed entry;
    if there is none (or it is Oodle without the library), the question says the key
    can't be checked. Deleting from a pak whose index is plain needs no key and no
    question (only the index changes).
  - **What gets encrypted:** new entries if any existing entry is encrypted (empty files
    never are, as UnrealPak does); the index again if it was; the key GUID is kept.
  - **Format (fork F10), identical to UnrealPak:** each index part (primary, path hash,
    full directory) is padded to 16 bytes by repeating its first bytes, hashed as padded
    plaintext and encrypted; each part follows the padded previous one. Compressed
    blocks are padded with the entry's first bytes, stored data with its own first bytes.
    Checked against a real UE 5.6 pak (decrypted locally) and UnrealPak's test paks,
    which our writer reproduces byte for byte (v7–v11; v5's UnrealPak filled padding with
    arbitrary bytes).
  - **Checked against UE source** (`PakFileUtilities.cpp`, `UnrealPak.cpp` before 4.21;
    branches 4.14, 4.16, 4.20–4.22, 4.25–4.27, 5.0, 5.3, 5.6, 5.8):
    | Rule | Source | Since |
    |---|---|---|
    | Compressed blocks: pad `[i] = buf[i % total]` (buf starts at the entry's first block), size = padded total, hash = SHA-1 of the encrypted padded buffer | `CompressFileToWorkingBuffer`, `PrepareCopyCompressedFileToPak` | 4.16 (4.14: random padding bytes) |
    | Stored data: pad `[i] = buf[(i - size) % size]`, encrypt, hash = SHA-1 of the first `size` encrypted bytes | `PrepareCopyFileToPak` | 4.21 (≤ 4.20: random padding; hash rule unchanged since 4.14) |
    | Index parts: pad `[i] = buf[i % len]`, SHA-1 of the padded plaintext, then encrypt; path hash index at `IndexOffset + padded primary`, full directory index after the padded path hash index | `FinalizeIndexBlockSize` / `FinalizeIndexBlock` | primary: 4.16; secondary indexes: 4.26 (v10) |
    | Frozen index (v9, 4.25): zero padding | `MemoryImage` path | 4.25 only; writing v9 frozen paks is refused anyway |
    Random padding in old UnrealPaks only changes padding bytes; readers ignore them.
- **[done] Compact when modifying** (`[Pack] Compact=1`, dialog checkbox): builds a
  complete new pak `<pak>.uepak-new` from the kept entries (record + stored bytes copied
  in file order: no recompression, no re-encryption, Oodle entries need no library),
  then the new files, then replaces the original. No dead space and no undo file: the
  original is untouched until the final rename, and any failure or Cancel only deletes
  the new file. Progress: the first bar shows the copy percentage. Free-space check: the
  whole new pak. Applies to F5 into a pak and F8.
  - Compacting also drops alignment padding UnrealPak may have added (the UE 5.6 sample
    shrank from 9.16 to 8.19 MB: some entries started at 1–2 MB boundaries). Offsets in the
    index are what games use, so this is safe on PC.

### 4.5 Deleting **[done]**

- `DeleteList` items are view paths exactly as listed (§11): files one per line, a folder
  as `folder\*.*` (everything below it).
- Mapped as in §4.2.1; deleting never changes the mount point; deleting a mount folder
  deletes everything below it.
- Index-only rewrite with the §4.4 transaction; deleting everything leaves a valid empty pak.
- Items matching nothing (e.g. outside the visible mount part) leave the pak unchanged.
- Same refusals as §4.4.

### 4.6 Compression methods for writing

| Method | Status | Levels | Notes |
|---|---|---|---|
| None | **[done]** | — | |
| Zlib | **[done]** | 0–9, default 1 | Read by every UE4/UE5 version |
| Gzip | **[done]** | 0–9, default 1 | Rarely used by games |
| Oodle | **[done]**, to verify in TC | HyperFast4…Optimal5, default Normal; compressor Kraken/Mermaid/Selkie/Leviathan/Hydra, default Mermaid | Needs the `deps\` DLL. UE's default for shipped games |
| Zstd | **[done]** | 1–22, default 3 | Not built into stock UE; only games that enable it can read it |
| LZ4 | **[done]** | — | Only games that enable it |

### 4.7 Block size **[done]**

- `[Pack] BlockSize`, default **64 KB** (UnrealPak default); accepts bytes or `64K`/`1M`,
  1 KB – 64 MB.
- Up to 126,976 bytes (124 KB) uses the compact encoding every engine reads. Larger
  values use the extended encoding, which older engines can't read (the dialog warns).

---

## 5. Settings dialog (`ConfigurePacker`) **[done]**

Modal, child of TC's window, built from an in-memory dialog template like the key dialog
(no resource compiler; the template builder counts its items), `MS Shell Dlg 2`, DPI
scaling through dialog units. Tested under Wine by driving the real controls.

```
┌ UE PAK plugin settings ───────────────────────────────┐
│ Packing                                               │
│   Mount point     [../../../                       ]  │
│   PAK version     [v11 (UE 4.26 – 5.x)          ▼]   │
│   Compression     [Oodle                         ▼]   │
│   Level           [Normal                        ▼]   │
│   Oodle compressor[Mermaid                       ▼]   │
│   Block size      [64 KB                         ▼]   │
│   Undo copy limit [1 GB (default)                ▼]   │
│   [ ] Compact when changing a pak (rebuild it: no     │
│       dead space, no undo copy)                       │
│   ⚠ (warnings: settings conflicts, Zstd/LZ4, large    │
│      blocks, Oodle library found / not found)          │
│                                                       │
│ Browsing                                              │
│   Mount point     (•) as folders  ( ) as root         │
│   Mount folders   [ReadOnly                      ▼]   │
│   [ ] Debug log (uepak.log)                           │
│                                                       │
│ Saved to <path of uepak.ini>                          │
│ [Defaults]                          [ OK ] [ Cancel ] │
└───────────────────────────────────────────────────────┘
```

- PAK versions labelled with their UE range: v12 (5.3+), **v11 (4.26–5.x, default)**,
  v10, v9 (4.25), v8B (4.23–4.24), v8A (4.22), v7 (4.21), v5 (4.20), v4 (4.16–4.19),
  v3 (4.3–4.15), v2 (4.0–4.2).
- Level choices follow the method (Zlib/Gzip 0–9, Zstd 1–22, Oodle level names, each with
  "Default"); Level / Oodle compressor are disabled for methods that don't use them.
- Block sizes: 64 KB, 124 KB, 256 KB, 512 KB, 1 MB, plus the current value if it's custom.
- Undo copy limits: always ask, 256 MB, 1 GB (default), 4 GB, 16 GB, no limit, plus a custom
  value from the ini.
- Warnings update live; the Oodle line names the DLL that would be used.
- OK saves to `uepak.ini` in the settings directory with a small writer that keeps
  comments, order and line endings (instead of `WritePrivateProfileStringW`: same effect,
  but UTF-8 and testable on any OS). If that directory has no `uepak.ini` yet, the shipped
  one is copied first. Defaults resets the fields without saving.

---

## 6. Configuration file (`uepak.ini`)

```ini
[Options]
MountPoint=Folder          ; Folder (default) | Root                      [done]
MountFolderAttr=Hidden     ; Hidden | ReadOnly | System | None, combinable [done]
DebugLog=0                 ; 1 = uepak.log (§7)                           [done]

[Pack]
MountPoint=../../../       ;                                              [done]
Version=V11                ; V2 … V12 (V1, V6 not writable)               [done]
Compression=None           ; None | Zlib | Gzip | Oodle | Zstd | LZ4      [done]
BlockSize=64K              ; bytes, or 64K / 1M                            [done]
Level=                     ; method-specific, empty = default              [done]
OodleCompressor=Mermaid    ; Kraken | Mermaid | Selkie | Leviathan | Hydra [done]
UndoLimit=1G               ; largest undo copy made without asking         [done]
Compact=0                  ; 1 = full rebuild when modifying               [done]
```

- Keys are case-insensitive and ignore `_` (`DebugLog` = `debug_log`). **[done]**
- Invalid values keep the default and are logged. **[done]**
- Inline comments after values (`Key=Value ; comment`) are allowed. **[done]**
- Re-read on every `OpenArchive`, `PackFiles` and `DeleteFiles` call (no TC restart). **[done]**
- Read order: settings directory, then the shipped file in the plugin folder. **[done]**

---

## 7. Diagnostics **[done]**

- `DebugLog=1` → `uepak.log` in the plugin folder, appended to, one line per event with a
  local timestamp and thread id.
- Plugin folder not writable → the log is kept in memory (last 512 KB) and copied to the
  clipboard on every failed operation; popups say where the log is.
- Logged: environment (paths, `deps\` contents, Oodle search paths, options and option
  warnings); raw footer of each opened pak (independent parser); open result and key
  lookup steps; per-entry extract details and timing; pack/delete arguments, view mode,
  resolved settings, each added/replaced/removed entry, mount-point changes, undo save /
  commit / rollback, result and size; settings dialog saves; panics.
- Never logged: AES keys.

---

## 8. Robustness rules

- **[done]** Every export runs inside a panic guard → error popup + safe return value.
- **[done]** No network access. The Oodle download path is compiled out.
- **[done]** Unsafe entry names are never extracted or packed.
- **[done]** Untrusted lengths from pak files never drive allocations (fork guards); a wrong
  key is detected before parsing.
- **[done]** New paks and extracted files are never left half-written.
- **[done]** A modified pak is never left half-written (§4.4); a panic during a change
  rolls back too.
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
| F4 | Writer: configurable block size, store entries that don't shrink, `CompressionOptions` (levels, Oodle compressor/level) | ✅ |
| F5 | `PakWriter::write_file_from_reader`: streaming writes from `Read + Seek`, progress/cancel; byte-identical to `write_file` (42 version × method combinations); >4 GB entries | ✅ |
| F6 | `PakWriter` editing: `remove`, `rename_all`, `set_mount_point`, `set_compression*`, `paths`; `PakReader::index_offset`; new methods fill free footer slots; `PakReader::new_pakwriter_like` + `PakWriter::copy_entry` (compact: stored bytes copied as-is, absolute block offsets moved; rebuilds all 48 UnrealPak test paks byte for byte) | ✅ |
| F7 | Block-by-block reads and decompression; `EntryInfo::data_end`; `PakReader::into_pakwriter_at` (space reuse) | ✅ |
| F8 | Read v13–15 | M5 |
| F9 | `PakReader::read_file_verified` (entry SHA-1, rules verified on real paks); `HashMismatch`, `Cancelled` errors | ✅ |
| F10 | AES writing: `set_encrypt_entries` / `set_encrypt_index` / `set_encryption_guid`; padding and hashes as UnrealPak (byte-identical on its test paks); footer keeps the GUID; `read_file_verified` checks encrypted stored entries | ✅ |

Each change gets fork tests; generally useful ones are offered upstream.

---

## 10. Testing

**Automated (60 tests, run on Windows CI on every push):**

- Reading: synthetic round trips for None/Zlib/Gzip/Zstd/LZ4, v12, mount folders and
  attributes, future version refused, non-pak refused, null/bad handles, panic guard,
  FFI struct layout.
- AES: repak's public encrypted test paks (`testdata/`): cancel, bad format, wrong key,
  right key, session cache, remembered key + parent folder, corrupt keys file, entry-only
  encryption.
- Packing: every compression round trip (incl. raw fallback), TC-style AddList with folders,
  SubPath, flattening + duplicate refusal, move-to-archive, unreadable source, version/
  compression refusal, missing Oodle, cancel (no partial pak), existing pak refused.
- Modifying: add/replace inside the mount, outside the mount (No → byte-identical, Yes →
  re-rooted with game paths unchanged, panel root → bare base), F8 files / folders / mount
  folders / no match, view-mode memory, cancel and read error → byte-identical rollback,
  simulated crash → restore, pending undo refused, new compression slot.
- Large files / M3: replacing the last file 5× doesn't grow the pak, deleting it shrinks
  it; undo limit (No → append, Yes → reuse, No/No → unchanged); low free space asks
  (No → nothing written); `PK_TEST` catches a damaged entry; 40 MB file streams through
  pack + extract + test (`UEPAK_BIG_MB` for bigger: 3 GB measured at 12 MB peak memory);
  changed-key message for a stale same-GUID key.
- Encrypted paks / M4: adding to each kind of encrypted test pak (index + entries,
  entries only, index only), in place and compacted: No → byte-identical, the question's
  wording, new entry encrypted like the pak, index still encrypted, key remembered,
  extract + test with TC's calls; deleting (index encrypted: question; entries only: no
  key, no question).
- Compact: no gaps and the index right after the data, smaller than in place, low space
  asks (No → unchanged, no temp file), a failing source leaves the original untouched.
- Settings: ini writer (comments, order, CRLF, key variants), model ↔ ini round trip,
  shipped ini = defaults, choices and warnings.
- Options parsing; key parsing (hex/base64); pack planning; view mapping (incl. Unicode).
- Optional env-driven tests (not in CI): `UEPAK_SAMPLES` (real paks), `UEPAK_KEYED_PAK` +
  `UEPAK_KEYED_KEY` (real encrypted pak; keys never committed), `UEPAK_DEBUG_PAK`
  (diagnostic log for one file), `UEPAK_PARITY_PAK` (re-pack a reference pak and compare
  bytes), `UEPAK_KEEP_PACKED` (keep written paks for inspection).

**Manual / tooling:** Wine harnesses that drive the real key dialog and settings dialog;
Python script checking the three index SHA-1s of new and modified paks.

**Speed, in-memory vs streaming writes** (V11, 64 KB blocks, release build, Linux,
average of 2 runs; ±10 % is run-to-run noise):

| Files | None | Zlib | LZ4 |
|---|---|---|---|
| 2000 × 50 KB | +8 % | −7 % | +5 % |
| 100 × 1 MB | −4 % | −1 % | −14 % |
| 5 × 20 MB | −16 % | −6 % | −16 % |

No difference for small files, so only the streaming path is kept (no size threshold).

Fork tests additionally cover streaming parity at block boundaries (empty, 1 byte, exact
block, block + 1, incompressible) and a >4 GB entry.

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
| SML.pak (made by repak) | packing parity | ✅ plugin output byte-identical (806 bytes) |
| G1R.pak (made by the plugin) | packing | ✅ valid; differs from SML.pak only by the chosen mount point |
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

1. **Packing in the field:** Oodle packing on Windows, and a plugin-packed mod pak loaded
   by a game.
2. **Encrypted paks in the field:** a game loading a pak the plugin added files to (or
   compacted) with its key; and the settings dialog's new Compact checkbox in TC (the Wine
   harness no longer runs in the build sandbox).

Resolved: Alt+F5 "replace the pak" moved to the backburner (§0); no size threshold
between in-memory and streaming code paths (measured: no speed difference for small
files, see §10); mount folder marker = `Hidden` (tested in TC); F5/F8 refresh the panel
correctly (tested in TC); plugin output byte-identical to repak with the same mount point
(tested in TC with the dialog); compact = checkbox; `pluginst.inf` with `file=uepak.wcx64` works; key lookup =
folder/parent then same non-zero GUID; both view modes with `Folder` default and the mount
guard; TC argument formats captured; background operations after packing is stable;
encrypted paks modifiable after a confirmation, no new encrypted paks.
