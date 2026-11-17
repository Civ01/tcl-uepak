//! Modal AES key prompt, built from an in-memory dialog template
//! (no resource compiler needed).

use std::path::Path;

/// What the user entered.
pub struct KeyAnswer {
    pub text: String,
    pub remember: bool,
}

pub struct KeyPrompt<'a> {
    pub pak: &'a Path,
    pub guid: &'a str,
    /// Shown above the buttons, e.g. "Key does not match this pak".
    pub error: Option<&'a str>,
}

#[cfg(test)]
pub static TEST_ANSWERS: std::sync::Mutex<Vec<Option<(String, bool)>>> =
    std::sync::Mutex::new(Vec::new());

/// Shows the dialog. `None` = cancelled.
pub fn prompt_key(p: &KeyPrompt) -> Option<KeyAnswer> {
    #[cfg(test)]
    {
        let _ = p;
        let mut q = TEST_ANSWERS.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        q.remove(0)
            .map(|(text, remember)| KeyAnswer { text, remember })
    }
    #[cfg(not(test))]
    imp::prompt(p)
}

#[cfg(all(windows, not(test)))]
mod imp {
    use super::*;
    use crate::win::wide;
    use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const ID_INFO: u16 = 100;
    const ID_EDIT: u16 = 101;
    const ID_REMEMBER: u16 = 102;
    const ID_ERROR: u16 = 103;
    const ID_LABEL: u16 = 104;
    const BM_GETCHECK: u32 = 0x00F0;
    const BM_SETCHECK: u32 = 0x00F1;
    const EM_LIMITTEXT: u32 = 0x00C5;

    struct State {
        info: Vec<u16>,
        error: Vec<u16>,
        answer: Option<KeyAnswer>,
    }

    /// DLGTEMPLATE builder (all fields are WORD/DWORD sequences).
    struct Tpl(Vec<u16>);
    impl Tpl {
        fn u32(&mut self, v: u32) {
            self.0.push(v as u16);
            self.0.push((v >> 16) as u16);
        }
        fn u16(&mut self, v: u16) {
            self.0.push(v);
        }
        fn str(&mut self, s: &str) {
            self.0.extend(s.encode_utf16());
            self.0.push(0);
        }
        fn align4(&mut self) {
            if self.0.len() % 2 == 1 {
                self.0.push(0);
            }
        }
        #[allow(clippy::too_many_arguments)]
        fn item(
            &mut self,
            class: u16,
            style: u32,
            x: i16,
            y: i16,
            cx: i16,
            cy: i16,
            id: u16,
            text: &str,
        ) {
            self.align4();
            self.u32(style | WS_CHILD | WS_VISIBLE);
            self.u32(0);
            for v in [x, y, cx, cy] {
                self.u16(v as u16);
            }
            self.u16(id);
            self.u16(0xFFFF);
            self.u16(class);
            self.str(text);
            self.u16(0);
        }
    }

    /// The template as DWORDs: DialogBoxIndirect requires DWORD alignment,
    /// which a `Vec<u16>` doesn't guarantee.
    fn template() -> Vec<u32> {
        const DS_SETFONT: u32 = 0x40;
        const DS_MODALFRAME: u32 = 0x80;
        const DS_FIXEDSYS: u32 = 0x8;
        const DS_CENTER: u32 = 0x800;
        const BUTTON: u16 = 0x80;
        const EDIT: u16 = 0x81;
        const STATIC: u16 = 0x82;
        const SS_NOPREFIX: u32 = 0x80;
        const ES_AUTOHSCROLL: u32 = 0x80;
        const BS_DEFPUSHBUTTON: u32 = 1;
        const BS_AUTOCHECKBOX: u32 = 3;
        let mut t = Tpl(vec![]);
        t.u32(
            DS_SETFONT
                | DS_FIXEDSYS
                | DS_MODALFRAME
                | DS_CENTER
                | WS_POPUP
                | WS_CAPTION
                | WS_SYSMENU,
        );
        t.u32(0);
        t.u16(7); // controls
        for v in [0i16, 0, 310, 132] {
            t.u16(v as u16);
        }
        t.u16(0); // menu
        t.u16(0); // class
        t.str("UE PAK plugin: AES key needed");
        t.u16(8);
        t.str("MS Shell Dlg 2");
        t.item(STATIC, SS_NOPREFIX, 7, 7, 296, 34, ID_INFO, "");
        t.item(
            STATIC,
            0,
            7,
            44,
            296,
            9,
            ID_LABEL,
            "AES key (64 hex digits, optional 0x, or base64):",
        );
        t.item(
            EDIT,
            ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP,
            7,
            55,
            296,
            14,
            ID_EDIT,
            "",
        );
        t.item(
            BUTTON,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            7,
            74,
            296,
            10,
            ID_REMEMBER,
            "&Remember for this folder and its subfolders",
        );
        t.item(STATIC, SS_NOPREFIX, 7, 88, 296, 18, ID_ERROR, "");
        t.item(
            BUTTON,
            BS_DEFPUSHBUTTON | WS_TABSTOP,
            199,
            111,
            50,
            14,
            1,
            "OK",
        );
        t.item(BUTTON, WS_TABSTOP, 253, 111, 50, 14, 2, "Cancel");
        if t.0.len() % 2 == 1 {
            t.0.push(0);
        }
        t.0.chunks(2)
            .map(|w| w[0] as u32 | (w[1] as u32) << 16)
            .collect()
    }

    unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let st = &*(lp as *const State);
                SetDlgItemTextW(hwnd, ID_INFO as i32, st.info.as_ptr());
                SetDlgItemTextW(hwnd, ID_ERROR as i32, st.error.as_ptr());
                SendDlgItemMessageW(hwnd, ID_REMEMBER as i32, BM_SETCHECK, 1, 0);
                SendDlgItemMessageW(hwnd, ID_EDIT as i32, EM_LIMITTEXT, 200, 0);
                1 // focus goes to the first tab stop: the edit box
            }
            WM_COMMAND => {
                let id = (wp & 0xFFFF) as i32;
                let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if id == 1 && !st.is_null() {
                    let mut buf = [0u16; 256];
                    let n =
                        GetDlgItemTextW(hwnd, ID_EDIT as i32, buf.as_mut_ptr(), buf.len() as i32);
                    let remember =
                        SendDlgItemMessageW(hwnd, ID_REMEMBER as i32, BM_GETCHECK, 0, 0) == 1;
                    (*st).answer = Some(KeyAnswer {
                        text: String::from_utf16_lossy(&buf[..n as usize]),
                        remember,
                    });
                    EndDialog(hwnd, 1);
                    return 1;
                }
                if id == 2 {
                    EndDialog(hwnd, 0);
                    return 1;
                }
                0
            }
            _ => 0,
        }
    }

    pub fn prompt(p: &KeyPrompt) -> Option<KeyAnswer> {
        let name = p
            .pak
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folder = p
            .pak
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let guid = if p.guid == crate::keys::ZERO_GUID {
            format!("{} (main key)", p.guid)
        } else {
            p.guid.to_string()
        };
        let mut st = State {
            info: wide(&format!(
                "The file list of this pak is encrypted.\nPak: {name}\nFolder: {folder}\nKey GUID: {guid}"
            )),
            error: wide(&p.error.map(|e| format!("\u{26A0} {e}")).unwrap_or_default()),
            answer: None,
        };
        let tpl = template();
        let r = unsafe {
            DialogBoxIndirectParamW(
                crate::win::module_handle(),
                tpl.as_ptr() as *const DLGTEMPLATE,
                GetActiveWindow(),
                Some(proc_),
                &mut st as *mut State as LPARAM,
            )
        };
        if r == -1 {
            let err = unsafe { GetLastError() };
            crate::dlog!("key dialog could not be created (error {err})");
            crate::win::error(&format!(
                "The AES key dialog could not be shown (Windows error {err})."
            ));
        }
        st.answer
    }
}

#[cfg(all(not(windows), not(test)))]
mod imp {
    use super::*;
    pub fn prompt(_p: &KeyPrompt) -> Option<KeyAnswer> {
        None
    }
}
