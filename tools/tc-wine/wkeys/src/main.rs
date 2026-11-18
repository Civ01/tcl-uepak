// Drives Windows apps under Wine: keys by virtual-key code (independent of the X keymap),
// window/control dumps. Usage: wkeys TOKEN... where TOKEN is
//   down|up|enter|esc|tab|f1..f12|space|ins|del|home|end|pgup|pgdn|bksp, a single char,
//   mods joined with '+': ctrl+pgdn, alt+f5, shift+tab
//   text:SOME TEXT   (types unicode text)    wait:MS
//   focus:TITLE-SUBSTRING   dump (top-level windows + child controls)
use std::ffi::c_void;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn vk(name: &str) -> Option<u16> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        "down" => VK_DOWN, "up" => VK_UP, "left" => VK_LEFT, "right" => VK_RIGHT,
        "enter" => VK_RETURN, "esc" => VK_ESCAPE, "tab" => VK_TAB, "space" => VK_SPACE,
        "ins" => VK_INSERT, "del" => VK_DELETE, "home" => VK_HOME, "end" => VK_END,
        "pgup" => VK_PRIOR, "pgdn" => VK_NEXT, "bksp" => VK_BACK,
        "ctrl" => VK_CONTROL, "alt" => VK_MENU, "shift" => VK_SHIFT,
        _ if n.starts_with('f') && n.len() > 1 && n[1..].parse::<u16>().is_ok() => VK_F1 + n[1..].parse::<u16>().unwrap() - 1,
        _ if n.len() == 1 => { let c = n.as_bytes()[0]; if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() as u16 } else { return None } }
        _ => return None,
    })
}

fn key(v: u16, up: bool) -> INPUT {
    let mut i: INPUT = unsafe { std::mem::zeroed() };
    i.r#type = INPUT_KEYBOARD;
    i.Anonymous.ki = KEYBDINPUT { wVk: v, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 };
    i
}
fn uni(c: u16, up: bool) -> INPUT {
    let mut i: INPUT = unsafe { std::mem::zeroed() };
    i.r#type = INPUT_KEYBOARD;
    i.Anonymous.ki = KEYBDINPUT { wVk: 0, wScan: c, dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 };
    i
}
fn send(v: &[INPUT]) { unsafe { SendInput(v.len() as u32, v.as_ptr(), std::mem::size_of::<INPUT>() as i32); } }

fn text_of(h: HWND) -> String {
    let mut b = [0u16; 1024];
    let n = unsafe { GetWindowTextW(h, b.as_mut_ptr(), 1024) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}
fn class_of(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetClassNameW(h, b.as_mut_ptr(), 256) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}
unsafe extern "system" fn child(h: HWND, _: LPARAM) -> i32 {
    if IsWindowVisible(h) != 0 {
        let t = text_of(h);
        let c = class_of(h);
        if !t.is_empty() || c.contains("Edit") || c.contains("Combo") {
            let checked = if c == "Button" { SendMessageW(h, BM_GETCHECK, 0, 0) } else { -1 };
            println!("    [{c}] id={} {:?}{}", GetDlgCtrlID(h), t, if checked >= 0 { format!(" check={checked}") } else { String::new() });
        }
    }
    1
}
unsafe extern "system" fn top(h: HWND, _: LPARAM) -> i32 {
    if IsWindowVisible(h) != 0 {
        let t = text_of(h);
        if !t.is_empty() {
            println!("{:?} [{}] {:?}", h, class_of(h), t);
            if !t.starts_with("Total Commander (x64)") { EnumChildWindows(h, Some(child), 0); }
        }
    }
    1
}
unsafe extern "system" fn find(h: HWND, l: LPARAM) -> i32 {
    let want = &*(l as *const (String, std::cell::Cell<HWND>));
    if IsWindowVisible(h) != 0 && text_of(h).contains(&want.0) { want.1.set(h); return 0; }
    1
}

fn main() {
    for tok in std::env::args().skip(1) {
        if let Some(ms) = tok.strip_prefix("wait:") { std::thread::sleep(std::time::Duration::from_millis(ms.parse().unwrap())); continue; }
        if tok == "dump" { unsafe { EnumWindows(Some(top), 0); } continue; }
        if let Some(t) = tok.strip_prefix("focus:") {
            let want = (t.to_string(), std::cell::Cell::new(std::ptr::null_mut::<c_void>()));
            unsafe { EnumWindows(Some(find), &want as *const _ as LPARAM); }
            let h = want.1.get();
            if h.is_null() { eprintln!("focus: no window {t:?}"); std::process::exit(2); }
            unsafe { SetForegroundWindow(h); }
            std::thread::sleep(std::time::Duration::from_millis(300));
            continue;
        }
        if let Some(t) = tok.strip_prefix("text:") {
            for c in t.encode_utf16() { send(&[uni(c, false), uni(c, true)]); }
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        let parts: Vec<&str> = tok.split('+').collect();
        let codes: Vec<u16> = parts.iter().map(|p| vk(p).unwrap_or_else(|| panic!("unknown key {p}"))).collect();
        let mut v: Vec<INPUT> = codes.iter().map(|c| key(*c, false)).collect();
        v.extend(codes.iter().rev().map(|c| key(*c, true)));
        send(&v);
        std::thread::sleep(std::time::Duration::from_millis(350));
    }
}
