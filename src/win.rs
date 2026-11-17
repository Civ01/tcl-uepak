//! Thin platform layer. Real implementations on Windows, harmless stubs
//! elsewhere so the archive logic can be unit-tested on any host.

use std::path::PathBuf;
use std::time::SystemTime;

pub const CAPTION: &str = "UE PAK plugin";

/// Shows a modal warning box (Windows) or prints to stderr (other hosts).
pub fn warn(text: &str) {
    message(text, false)
}

pub fn error(text: &str) {
    message(text, true)
}

/// Encodes a string as NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 buffer.
pub unsafe fn from_wide_ptr(p: *const u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0;
    while *p.add(len) != 0 {
        len += 1;
    }
    Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)))
}

/// Reads a TC-style list: NUL-separated UTF-16 strings ending with an empty one.
///
/// # Safety
/// `p` must be null or point to a double-NUL-terminated UTF-16 list.
pub unsafe fn from_wide_multi(p: *const u16) -> Vec<String> {
    let mut out = vec![];
    if p.is_null() {
        return out;
    }
    let mut cur = p;
    loop {
        let mut len = 0;
        while *cur.add(len) != 0 {
            len += 1;
        }
        if len == 0 {
            break;
        }
        out.push(String::from_utf16_lossy(std::slice::from_raw_parts(
            cur, len,
        )));
        cur = cur.add(len + 1);
    }
    out
}

/// Copies `s` into a fixed UTF-16 buffer, truncating and NUL-terminating.
pub fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for unit in s.encode_utf16() {
        if n + 1 >= dst.len() {
            break;
        }
        dst[n] = unit;
        n += 1;
    }
    dst[n] = 0;
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_char;
    use windows_sys::Win32::Foundation::{FILETIME, HMODULE};
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, WideCharToMultiByte, CP_ACP};
    use windows_sys::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    use windows_sys::Win32::System::WindowsProgramming::FileTimeToDosDateTime;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL,
    };

    pub fn message(text: &str, is_error: bool) {
        if cfg!(test) {
            // never block a test run (or CI) on a modal box
            eprintln!("[{CAPTION}] {text}");
            return;
        }
        let text = wide(text);
        let caption = wide(CAPTION);
        let icon = if is_error {
            MB_ICONERROR
        } else {
            MB_ICONWARNING
        };
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_OK | icon | MB_TASKMODAL | MB_SETFOREGROUND,
            );
        }
    }

    /// Yes/No question box; true = Yes.
    pub fn ask_yes_no(text: &str) -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
        use windows_sys::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO};
        let text = wide(text);
        let caption = wide(CAPTION);
        unsafe {
            MessageBoxW(
                GetActiveWindow(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_TASKMODAL | MB_SETFOREGROUND,
            ) == IDYES
        }
    }

    /// Handle of this DLL (not of TOTALCMD64.EXE); null on failure.
    pub fn module_handle() -> HMODULE {
        let mut module: HMODULE = std::ptr::null_mut();
        let anchor = module_handle as *const u16;
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                anchor,
                &mut module,
            );
        }
        module
    }

    /// Full path of this DLL (not of TOTALCMD64.EXE).
    pub fn module_path() -> Option<PathBuf> {
        unsafe {
            let module = module_handle();
            if module.is_null() {
                return None;
            }
            let mut buf = vec![0u16; 32768];
            let len = GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
            if len == 0 || len >= buf.len() {
                return None;
            }
            use std::os::windows::ffi::OsStringExt;
            Some(PathBuf::from(std::ffi::OsString::from_wide(&buf[..len])))
        }
    }

    /// SystemTime -> TC/DOS packed local time.
    pub fn dos_time(t: SystemTime) -> i32 {
        const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
        let Ok(d) = t.duration_since(SystemTime::UNIX_EPOCH) else {
            return 0;
        };
        let ticks = d.as_nanos() as u64 / 100 + EPOCH_DIFF_100NS;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut local = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut date, mut time) = (0u16, 0u16);
        unsafe {
            if FileTimeToLocalFileTime(&ft, &mut local) == 0
                || FileTimeToDosDateTime(&local, &mut date, &mut time) == 0
            {
                return 0;
            }
        }
        ((date as u32) << 16 | time as u32) as i32
    }

    /// Local wall-clock time as `YYYY-MM-DD HH:MM:SS.mmm`.
    pub fn local_timestamp() -> String {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut t) };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        )
    }

    pub fn thread_id() -> u32 {
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
    }

    /// Puts `text` on the clipboard as Unicode text. Returns success.
    pub fn copy_to_clipboard(text: &str) -> bool {
        use windows_sys::Win32::Foundation::GlobalFree;
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows_sys::Win32::System::Memory::{
            GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
        };
        const CF_UNICODETEXT: u32 = 13;
        let data = wide(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
        let bytes = data.len() * 2;
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return false;
            }
            let mut ok = false;
            if EmptyClipboard() != 0 {
                let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
                if !mem.is_null() {
                    let dst = GlobalLock(mem) as *mut u16;
                    if !dst.is_null() {
                        std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
                        GlobalUnlock(mem);
                        // On success the clipboard owns the memory.
                        ok = !SetClipboardData(CF_UNICODETEXT, mem as _).is_null();
                    }
                    if !ok {
                        GlobalFree(mem);
                    }
                }
            }
            CloseClipboard();
            ok
        }
    }

    /// ANSI (CP_ACP) C string -> String.
    ///
    /// # Safety
    /// `p` must be null or NUL-terminated.
    pub unsafe fn from_ansi_ptr(p: *const c_char) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let n = MultiByteToWideChar(CP_ACP, 0, p as *const u8, -1, std::ptr::null_mut(), 0);
        if n <= 0 {
            return None;
        }
        let mut buf = vec![0u16; n as usize];
        MultiByteToWideChar(CP_ACP, 0, p as *const u8, -1, buf.as_mut_ptr(), n);
        buf.pop();
        Some(String::from_utf16_lossy(&buf))
    }

    /// Copies `s` into a fixed ANSI buffer (CP_ACP), truncating and NUL-terminating.
    pub fn copy_ansi(dst: &mut [c_char], s: &str) {
        let w: Vec<u16> = s.encode_utf16().collect();
        let n = unsafe {
            WideCharToMultiByte(
                CP_ACP,
                0,
                w.as_ptr(),
                w.len() as i32,
                dst.as_mut_ptr() as *mut u8,
                (dst.len() - 1) as i32,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        dst[n.max(0) as usize] = 0;
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::ffi::{c_char, CStr};

    pub fn message(text: &str, is_error: bool) {
        eprintln!(
            "[{CAPTION}] {}: {text}",
            if is_error { "error" } else { "warning" }
        );
    }

    pub fn module_path() -> Option<PathBuf> {
        std::env::current_exe().ok()
    }

    pub fn ask_yes_no(text: &str) -> bool {
        eprintln!("[{CAPTION}] question (answered No): {text}");
        false
    }

    pub fn dos_time(_t: SystemTime) -> i32 {
        0
    }

    pub fn local_timestamp() -> String {
        let d = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        format!("{}.{:03}", d.as_secs(), d.subsec_millis())
    }

    pub fn thread_id() -> u32 {
        0
    }

    pub fn copy_to_clipboard(_text: &str) -> bool {
        false
    }

    /// # Safety
    /// `p` must be null or NUL-terminated.
    pub unsafe fn from_ansi_ptr(p: *const c_char) -> Option<String> {
        (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
    }

    pub fn copy_ansi(dst: &mut [c_char], s: &str) {
        let n = s.len().min(dst.len() - 1);
        for (d, b) in dst.iter_mut().zip(s.as_bytes()[..n].iter()) {
            *d = *b as c_char;
        }
        dst[n] = 0;
    }
}

pub use imp::*;
