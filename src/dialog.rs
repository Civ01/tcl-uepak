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

/// Error line of the last key prompt shown (tests).
#[cfg(test)]
pub static TEST_LAST_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub static TEST_CONFIRMS: std::sync::Mutex<Vec<bool>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
pub static TEST_CONFIRM_TEXTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Yes/No question. Tests answer from `TEST_CONFIRMS` (empty = No).
pub fn confirm(text: &str) -> bool {
    #[cfg(test)]
    {
        TEST_CONFIRM_TEXTS.lock().unwrap().push(text.to_string());
        let mut q = TEST_CONFIRMS.lock().unwrap();
        if q.is_empty() {
            false
        } else {
            q.remove(0)
        }
    }
    #[cfg(not(test))]
    crate::win::ask_yes_no(text)
}

/// Shows the dialog. `None` = cancelled.
pub fn prompt_key(p: &KeyPrompt) -> Option<KeyAnswer> {
    #[cfg(test)]
    {
        *TEST_LAST_ERROR.lock().unwrap() = p.error.map(String::from);
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

    /// DLGTEMPLATE builder (all fields are WORD/DWORD sequences). Counts the
    /// items itself: a wrong count in the header makes Windows read past the
    /// template.
    pub(super) struct Tpl {
        buf: Vec<u16>,
        count_at: usize,
        items: u16,
    }
    impl Tpl {
        /// Dialog header: modal frame, caption, centered, MS Shell Dlg 2 8pt.
        pub(super) fn dialog(title: &str, cx: i16, cy: i16) -> Self {
            const DS_SETFONT: u32 = 0x40;
            const DS_MODALFRAME: u32 = 0x80;
            const DS_FIXEDSYS: u32 = 0x8;
            const DS_CENTER: u32 = 0x800;
            let mut t = Tpl {
                buf: vec![],
                count_at: 0,
                items: 0,
            };
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
            t.count_at = t.buf.len();
            t.u16(0); // item count, patched in into_dwords
            for v in [0i16, 0, cx, cy] {
                t.u16(v as u16);
            }
            t.u16(0); // menu
            t.u16(0); // class
            t.str(title);
            t.u16(8);
            t.str("MS Shell Dlg 2");
            t
        }
        fn u32(&mut self, v: u32) {
            self.buf.push(v as u16);
            self.buf.push((v >> 16) as u16);
        }
        fn u16(&mut self, v: u16) {
            self.buf.push(v);
        }
        fn str(&mut self, s: &str) {
            self.buf.extend(s.encode_utf16());
            self.buf.push(0);
        }
        fn align4(&mut self) {
            if self.buf.len() % 2 == 1 {
                self.buf.push(0);
            }
        }
        #[allow(clippy::too_many_arguments)]
        pub(super) fn item(
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
            self.items += 1;
        }
        /// DWORD-aligned template for DialogBoxIndirect.
        pub(super) fn into_dwords(mut self) -> Vec<u32> {
            self.buf[self.count_at] = self.items;
            if self.buf.len() % 2 == 1 {
                self.buf.push(0);
            }
            self.buf
                .chunks(2)
                .map(|w| w[0] as u32 | (w[1] as u32) << 16)
                .collect()
        }
    }

    fn template() -> Vec<u32> {
        const BUTTON: u16 = 0x80;
        const EDIT: u16 = 0x81;
        const STATIC: u16 = 0x82;
        const SS_NOPREFIX: u32 = 0x80;
        const ES_AUTOHSCROLL: u32 = 0x80;
        const BS_DEFPUSHBUTTON: u32 = 1;
        const BS_AUTOCHECKBOX: u32 = 3;
        let mut t = Tpl::dialog("UE PAK plugin: AES key needed", 310, 132);
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
        t.into_dwords()
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

/// Shows the settings dialog (ConfigurePacker). Returns true if saved.
pub fn configure(parent: *mut std::ffi::c_void) -> bool {
    #[cfg(any(test, not(windows)))]
    {
        let _ = parent;
        false
    }
    #[cfg(all(windows, not(test)))]
    settings_ui::show(parent)
}

#[cfg(all(windows, not(test)))]
mod settings_ui {
    use super::imp::Tpl;
    use crate::settings::{self, Model, COMPRESSIONS, OODLE_COMPRESSORS_UI, VERSIONS};
    use crate::win::wide;
    use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const ID_MOUNT: u16 = 201;
    const ID_VERSION: u16 = 202;
    const ID_COMP: u16 = 203;
    const ID_LEVEL: u16 = 204;
    const ID_OODLE: u16 = 205;
    const ID_BLOCK: u16 = 206;
    const ID_WARN: u16 = 207;
    const ID_VIEW_FOLDER: u16 = 208;
    const ID_VIEW_ROOT: u16 = 209;
    const ID_ATTR: u16 = 210;
    const ID_DEBUG: u16 = 211;
    const ID_DEFAULTS: u16 = 212;
    const ID_INIPATH: u16 = 213;
    const ID_UNDO: u16 = 214;
    const ID_COMPACT: u16 = 215;

    const BUTTON: u16 = 0x80;
    const EDIT: u16 = 0x81;
    const STATIC: u16 = 0x82;
    const COMBO: u16 = 0x85;
    const BS_AUTOCHECKBOX: u32 = 3;
    const BS_AUTORADIOBUTTON: u32 = 9;
    const BS_GROUPBOX: u32 = 7;
    const BS_DEFPUSHBUTTON: u32 = 1;
    const ES_AUTOHSCROLL: u32 = 0x80;
    const CBS_DROPDOWNLIST: u32 = 3;
    const SS_NOPREFIX: u32 = 0x80;
    const CB_ADDSTRING: u32 = 0x0143;
    const CB_RESETCONTENT: u32 = 0x014B;
    const CB_SETCURSEL: u32 = 0x014E;
    const CB_GETCURSEL: u32 = 0x0147;
    const CBN_SELCHANGE: u32 = 1;
    const EN_CHANGE: u32 = 0x0300;
    const BM_GETCHECK: u32 = 0x00F0;
    const BM_SETCHECK: u32 = 0x00F1;

    struct State {
        model: Model,
        levels: Vec<(Option<i32>, String)>,
        blocks: Vec<(u32, String)>,
        attrs: Vec<(u32, String)>,
        undos: Vec<(u64, String)>,
        oodle: Result<std::path::PathBuf, String>,
        ini: Option<std::path::PathBuf>,
        filling: bool,
        saved: bool,
    }

    fn template() -> Vec<u32> {
        let mut t = Tpl::dialog("UE PAK plugin settings", 320, 300);
        let label =
            |t: &mut Tpl, y: i16, text: &str| t.item(STATIC, 0, 14, y + 2, 84, 9, 0xFFFF, text);
        let combo = CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP;
        t.item(
            BUTTON,
            BS_GROUPBOX,
            6,
            4,
            308,
            182,
            0xFFFF,
            "Packing (new paks; existing paks keep their version and mount point)",
        );
        label(&mut t, 18, "Mount point:");
        t.item(
            EDIT,
            ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP,
            100,
            18,
            206,
            13,
            ID_MOUNT,
            "",
        );
        label(&mut t, 35, "PAK version:");
        t.item(COMBO, combo, 100, 35, 206, 160, ID_VERSION, "");
        label(&mut t, 52, "Compression:");
        t.item(COMBO, combo, 100, 52, 206, 120, ID_COMP, "");
        label(&mut t, 69, "Level:");
        t.item(COMBO, combo, 100, 69, 206, 160, ID_LEVEL, "");
        label(&mut t, 86, "Oodle compressor:");
        t.item(COMBO, combo, 100, 86, 206, 100, ID_OODLE, "");
        label(&mut t, 103, "Block size:");
        t.item(COMBO, combo, 100, 103, 206, 100, ID_BLOCK, "");
        label(&mut t, 120, "Undo copy limit:");
        t.item(COMBO, combo, 100, 120, 206, 100, ID_UNDO, "");
        t.item(
            BUTTON,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            14,
            138,
            290,
            10,
            ID_COMPACT,
            "&Compact when changing a pak (rebuild it: no dead space, no undo copy)",
        );
        t.item(STATIC, SS_NOPREFIX, 14, 152, 292, 32, ID_WARN, "");
        t.item(BUTTON, BS_GROUPBOX, 6, 190, 308, 72, 0xFFFF, "Browsing");
        label(&mut t, 204, "Mount point:");
        t.item(
            BUTTON,
            BS_AUTORADIOBUTTON | WS_GROUP | WS_TABSTOP,
            100,
            205,
            80,
            10,
            ID_VIEW_FOLDER,
            "as &folders",
        );
        t.item(
            BUTTON,
            BS_AUTORADIOBUTTON,
            184,
            205,
            80,
            10,
            ID_VIEW_ROOT,
            "as &root",
        );
        label(&mut t, 221, "Mount folders:");
        t.item(COMBO, combo | WS_GROUP, 100, 221, 206, 100, ID_ATTR, "");
        t.item(
            BUTTON,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            14,
            240,
            290,
            10,
            ID_DEBUG,
            "Write a &debug log (uepak.log)",
        );
        t.item(STATIC, SS_NOPREFIX, 6, 266, 308, 9, ID_INIPATH, "");
        t.item(BUTTON, WS_TABSTOP, 6, 280, 60, 14, ID_DEFAULTS, "&Defaults");
        t.item(
            BUTTON,
            BS_DEFPUSHBUTTON | WS_TABSTOP,
            200,
            280,
            54,
            14,
            1,
            "OK",
        );
        t.item(BUTTON, WS_TABSTOP, 260, 280, 54, 14, 2, "Cancel");
        t.into_dwords()
    }

    fn names<T>(v: &[(T, &str)]) -> Vec<String> {
        v.iter().map(|(_, s)| s.to_string()).collect()
    }

    unsafe fn fill(hwnd: HWND, id: u16, items: &[String], sel: usize) {
        SendDlgItemMessageW(hwnd, id as i32, CB_RESETCONTENT, 0, 0);
        for it in items {
            let w = wide(it);
            SendDlgItemMessageW(hwnd, id as i32, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        SendDlgItemMessageW(hwnd, id as i32, CB_SETCURSEL, sel, 0);
    }

    unsafe fn sel(hwnd: HWND, id: u16) -> usize {
        let r = SendDlgItemMessageW(hwnd, id as i32, CB_GETCURSEL, 0, 0);
        if r < 0 {
            0
        } else {
            r as usize
        }
    }

    unsafe fn checked(hwnd: HWND, id: u16) -> bool {
        SendDlgItemMessageW(hwnd, id as i32, BM_GETCHECK, 0, 0) == 1
    }

    unsafe fn set_check(hwnd: HWND, id: u16, on: bool) {
        SendDlgItemMessageW(hwnd, id as i32, BM_SETCHECK, on as usize, 0);
    }

    unsafe fn fill_levels(hwnd: HWND, st: &mut State) {
        st.levels = st.model.level_choices();
        if !st.levels.iter().any(|(l, _)| *l == st.model.level) {
            st.model.level = None;
        }
        let i = st
            .levels
            .iter()
            .position(|(l, _)| *l == st.model.level)
            .unwrap_or(0);
        let labels: Vec<String> = st.levels.iter().map(|(_, s)| s.clone()).collect();
        fill(hwnd, ID_LEVEL, &labels, i);
        EnableWindow(
            GetDlgItem(hwnd, ID_LEVEL as i32),
            st.model.uses_levels() as i32,
        );
        EnableWindow(
            GetDlgItem(hwnd, ID_OODLE as i32),
            st.model.uses_oodle_compressor() as i32,
        );
    }

    unsafe fn show_warnings(hwnd: HWND, st: &State) {
        let w = st.model.warnings(&st.oodle);
        let text = w
            .iter()
            .map(|s| format!("\u{26A0} {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        SetDlgItemTextW(hwnd, ID_WARN as i32, wide(&text).as_ptr());
    }

    unsafe fn model_to_controls(hwnd: HWND, st: &mut State) {
        st.filling = true;
        let m = st.model.clone();
        SetDlgItemTextW(hwnd, ID_MOUNT as i32, wide(&m.pack_mount).as_ptr());
        // (generic helper: closures can't be generic)
        let vi = VERSIONS
            .iter()
            .position(|(v, _)| *v == m.version)
            .unwrap_or(1);
        fill(hwnd, ID_VERSION, &names(&VERSIONS[..]), vi);
        let ci = COMPRESSIONS
            .iter()
            .position(|(c, _)| *c == m.compression)
            .unwrap_or(0);
        fill(hwnd, ID_COMP, &names(&COMPRESSIONS[..]), ci);
        let oi = OODLE_COMPRESSORS_UI
            .iter()
            .position(|(c, _)| *c == m.oodle_compressor)
            .unwrap_or(1);
        fill(hwnd, ID_OODLE, &names(&OODLE_COMPRESSORS_UI[..]), oi);
        st.blocks = m.block_choices();
        let bi = st
            .blocks
            .iter()
            .position(|(b, _)| *b == m.block_size)
            .unwrap_or(0);
        fill(
            hwnd,
            ID_BLOCK,
            &st.blocks.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
            bi,
        );
        st.undos = m.undo_choices();
        let ui = st
            .undos
            .iter()
            .position(|(u, _)| *u == m.undo_limit)
            .unwrap_or(2);
        fill(
            hwnd,
            ID_UNDO,
            &st.undos.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
            ui,
        );
        st.attrs = m.attr_choices();
        let ai = st
            .attrs
            .iter()
            .position(|(a, _)| *a == m.mount_folder_attr)
            .unwrap_or(0);
        fill(
            hwnd,
            ID_ATTR,
            &st.attrs.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
            ai,
        );
        set_check(
            hwnd,
            ID_VIEW_FOLDER,
            m.view == crate::config::MountPointMode::Folder,
        );
        set_check(
            hwnd,
            ID_VIEW_ROOT,
            m.view == crate::config::MountPointMode::Root,
        );
        set_check(hwnd, ID_DEBUG, m.debug_log);
        set_check(hwnd, ID_COMPACT, m.compact);
        fill_levels(hwnd, st);
        show_warnings(hwnd, st);
        st.filling = false;
    }

    unsafe fn controls_to_model(hwnd: HWND, st: &mut State) {
        let mut buf = [0u16; 1024];
        let n = GetDlgItemTextW(hwnd, ID_MOUNT as i32, buf.as_mut_ptr(), buf.len() as i32);
        st.model.pack_mount = String::from_utf16_lossy(&buf[..n as usize]);
        st.model.version = VERSIONS[sel(hwnd, ID_VERSION).min(VERSIONS.len() - 1)].0;
        st.model.compression = COMPRESSIONS[sel(hwnd, ID_COMP).min(COMPRESSIONS.len() - 1)].0;
        st.model.oodle_compressor = OODLE_COMPRESSORS_UI[sel(hwnd, ID_OODLE).min(4)].0;
        if let Some((l, _)) = st.levels.get(sel(hwnd, ID_LEVEL)) {
            st.model.level = *l;
        }
        if let Some((b, _)) = st.blocks.get(sel(hwnd, ID_BLOCK)) {
            st.model.block_size = *b;
        }
        if let Some((u, _)) = st.undos.get(sel(hwnd, ID_UNDO)) {
            st.model.undo_limit = *u;
        }
        if let Some((a, _)) = st.attrs.get(sel(hwnd, ID_ATTR)) {
            st.model.mount_folder_attr = *a;
        }
        st.model.view = if checked(hwnd, ID_VIEW_ROOT) {
            crate::config::MountPointMode::Root
        } else {
            crate::config::MountPointMode::Folder
        };
        st.model.debug_log = checked(hwnd, ID_DEBUG);
        st.model.compact = checked(hwnd, ID_COMPACT);
    }

    unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let st = &mut *(lp as *mut State);
                let where_ = match &st.ini {
                    Some(p) => format!("Saved to {}", p.display()),
                    None => "No writable settings folder found".into(),
                };
                SetDlgItemTextW(hwnd, ID_INIPATH as i32, wide(&where_).as_ptr());
                model_to_controls(hwnd, st);
                1
            }
            WM_COMMAND => {
                let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if st.is_null() {
                    return 0;
                }
                let st = &mut *st;
                let id = (wp & 0xFFFF) as u16;
                let code = ((wp >> 16) & 0xFFFF) as u32;
                match id {
                    1 => {
                        controls_to_model(hwnd, st);
                        match &st.ini {
                            Some(p) => match settings::save(&st.model, p) {
                                Ok(()) => {
                                    crate::dlog!(
                                        "settings saved to {}: {:?}",
                                        p.display(),
                                        st.model
                                    );
                                    st.saved = true;
                                    EndDialog(hwnd, 1);
                                }
                                Err(e) => crate::win::error(&format!(
                                    "The settings could not be saved to {}:\n{e}",
                                    p.display()
                                )),
                            },
                            None => {
                                crate::win::error("No writable folder for uepak.ini was found.")
                            }
                        }
                        1
                    }
                    2 => {
                        EndDialog(hwnd, 0);
                        1
                    }
                    ID_DEFAULTS => {
                        st.model = Model::defaults();
                        model_to_controls(hwnd, st);
                        1
                    }
                    _ if st.filling => 0,
                    ID_COMP if code == CBN_SELCHANGE => {
                        controls_to_model(hwnd, st);
                        fill_levels(hwnd, st);
                        show_warnings(hwnd, st);
                        1
                    }
                    _ if code == CBN_SELCHANGE || code == EN_CHANGE || code == 0 => {
                        controls_to_model(hwnd, st);
                        show_warnings(hwnd, st);
                        0
                    }
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    pub fn show(parent: *mut std::ffi::c_void) -> bool {
        let options = crate::config::load_options();
        let mut st = State {
            model: Model::from_options(&options),
            levels: vec![],
            blocks: vec![],
            attrs: vec![],
            undos: vec![],
            oodle: settings::oodle_status(),
            ini: settings::ini_path(),
            filling: false,
            saved: false,
        };
        let tpl = template();
        let r = unsafe {
            DialogBoxIndirectParamW(
                crate::win::module_handle(),
                tpl.as_ptr() as *const DLGTEMPLATE,
                parent as HWND,
                Some(proc_),
                &mut st as *mut State as LPARAM,
            )
        };
        if r == -1 {
            let err = unsafe { GetLastError() };
            crate::dlog!("settings dialog could not be created (error {err})");
            crate::win::error(&format!(
                "The settings dialog could not be shown (Windows error {err})."
            ));
        }
        st.saved
    }
}

#[cfg(all(not(windows), not(test)))]
mod imp {
    use super::*;
    pub fn prompt(_p: &KeyPrompt) -> Option<KeyAnswer> {
        None
    }
}
