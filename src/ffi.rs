//! Definitions from `wcxhead.h` (TC packer plugin interface 2.x).
#![allow(dead_code, non_snake_case)]

use std::ffi::{c_char, c_int, c_uint};

pub const E_END_ARCHIVE: c_int = 10;
pub const E_NO_MEMORY: c_int = 11;
pub const E_BAD_DATA: c_int = 12;
pub const E_BAD_ARCHIVE: c_int = 13;
pub const E_UNKNOWN_FORMAT: c_int = 14;
pub const E_EOPEN: c_int = 15;
pub const E_ECREATE: c_int = 16;
pub const E_ECLOSE: c_int = 17;
pub const E_EREAD: c_int = 18;
pub const E_EWRITE: c_int = 19;
pub const E_SMALL_BUF: c_int = 20;
pub const E_EABORTED: c_int = 21;
pub const E_NO_FILES: c_int = 22;
pub const E_TOO_MANY_FILES: c_int = 23;
pub const E_NOT_SUPPORTED: c_int = 24;

pub const PK_OM_LIST: c_int = 0;
pub const PK_OM_EXTRACT: c_int = 1;

pub const PK_SKIP: c_int = 0;
pub const PK_TEST: c_int = 1;
pub const PK_EXTRACT: c_int = 2;

pub const PK_CAPS_NEW: c_int = 1;
pub const PK_CAPS_MODIFY: c_int = 2;
pub const PK_CAPS_MULTIPLE: c_int = 4;
pub const PK_CAPS_DELETE: c_int = 8;
pub const PK_CAPS_OPTIONS: c_int = 16;
pub const PK_CAPS_MEMPACK: c_int = 32;
pub const PK_CAPS_BY_CONTENT: c_int = 64;
pub const PK_CAPS_SEARCHTEXT: c_int = 128;
pub const PK_CAPS_HIDE: c_int = 256;
pub const PK_CAPS_ENCRYPT: c_int = 512;

pub const FILE_ATTR_ARCHIVE: c_int = 0x20;
pub const FILE_ATTR_DIRECTORY: c_int = 0x10;

pub type ProcessDataProcW = Option<unsafe extern "system" fn(*mut u16, c_int) -> c_int>;
pub type ProcessDataProcA = Option<unsafe extern "system" fn(*mut c_char, c_int) -> c_int>;
pub type ChangeVolProcW = Option<unsafe extern "system" fn(*mut u16, c_int) -> c_int>;
pub type ChangeVolProcA = Option<unsafe extern "system" fn(*mut c_char, c_int) -> c_int>;

#[repr(C)]
pub struct OpenArchiveDataW {
    pub ArcName: *const u16,
    pub OpenMode: c_int,
    pub OpenResult: c_int,
    pub CmtBuf: *mut u16,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
}

#[repr(C)]
pub struct OpenArchiveData {
    pub ArcName: *const c_char,
    pub OpenMode: c_int,
    pub OpenResult: c_int,
    pub CmtBuf: *mut c_char,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
}

#[repr(C)]
pub struct HeaderDataExW {
    pub ArcName: [u16; 1024],
    pub FileName: [u16; 1024],
    pub Flags: c_int,
    pub PackSize: c_uint,
    pub PackSizeHigh: c_uint,
    pub UnpSize: c_uint,
    pub UnpSizeHigh: c_uint,
    pub HostOS: c_int,
    pub FileCRC: c_int,
    pub FileTime: c_int,
    pub UnpVer: c_int,
    pub Method: c_int,
    pub FileAttr: c_int,
    pub CmtBuf: *mut c_char,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
    pub Reserved: [c_char; 1024],
}

#[repr(C)]
pub struct HeaderDataEx {
    pub ArcName: [c_char; 1024],
    pub FileName: [c_char; 1024],
    pub Flags: c_int,
    pub PackSize: c_uint,
    pub PackSizeHigh: c_uint,
    pub UnpSize: c_uint,
    pub UnpSizeHigh: c_uint,
    pub HostOS: c_int,
    pub FileCRC: c_int,
    pub FileTime: c_int,
    pub UnpVer: c_int,
    pub Method: c_int,
    pub FileAttr: c_int,
    pub CmtBuf: *mut c_char,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
    pub Reserved: [c_char; 1024],
}

#[repr(C)]
pub struct HeaderData {
    pub ArcName: [c_char; 260],
    pub FileName: [c_char; 260],
    pub Flags: c_int,
    pub PackSize: c_int,
    pub UnpSize: c_int,
    pub HostOS: c_int,
    pub FileCRC: c_int,
    pub FileTime: c_int,
    pub UnpVer: c_int,
    pub Method: c_int,
    pub FileAttr: c_int,
    pub CmtBuf: *mut c_char,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
}

#[repr(C)]
pub struct PackDefaultParamStruct {
    pub size: c_int,
    pub PluginInterfaceVersionLow: u32,
    pub PluginInterfaceVersionHi: u32,
    pub DefaultIniName: [c_char; 260],
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn struct_sizes_match_wcxhead() {
        if cfg!(target_pointer_width = "64") {
            // 2*2048 + 11*4 + pad(4) + ptr(8) + 3*4 + 1024 + pad(4)
            assert_eq!(std::mem::size_of::<HeaderDataExW>(), 5192);
            assert_eq!(std::mem::size_of::<OpenArchiveDataW>(), 40);
        }
        assert_eq!(std::mem::size_of::<PackDefaultParamStruct>(), 272);
    }
}
