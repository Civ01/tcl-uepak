//! TC progress reporting and cancellation (`ProcessDataProc`).
//!
//! TC calls `SetProcessDataProc` on the thread that then runs the operation,
//! so the callback is stored per thread: concurrent operations (background
//! mode, SPEC §8.1) each report to their own progress dialog.

use crate::archive::{Finish, UserAborted};
use crate::ffi::ProcessDataProcW;
use std::cell::Cell;
use std::ffi::c_int;
use std::io::{self, Write};

thread_local! {
    static CALLBACK: Cell<ProcessDataProcW> = const { Cell::new(None) };
}

pub fn set_callback(cb: ProcessDataProcW) {
    CALLBACK.with(|c| c.set(cb));
}

/// Reports `bytes` processed for `name` (NUL-terminated UTF-16).
/// Returns `false` if the user pressed Cancel.
pub fn report(name: &mut [u16], bytes: u64) -> bool {
    let Some(cb) = CALLBACK.with(|c| c.get()) else {
        return true;
    };
    let mut left = bytes;
    loop {
        let chunk = left.min(c_int::MAX as u64);
        if unsafe { cb(name.as_mut_ptr(), chunk as c_int) } == 0 {
            return false;
        }
        left -= chunk;
        if left == 0 {
            return true;
        }
    }
}

/// A writer that reports every write and turns Cancel into an error.
pub struct Progress<W> {
    pub inner: W,
    pub name: Vec<u16>,
}

impl<W: Write> Write for Progress<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        if n > 0 && !report(&mut self.name, n as u64) {
            return Err(io::Error::other(UserAborted));
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Finish + Write> Finish for Progress<W> {
    fn finish(&mut self) -> io::Result<()> {
        self.inner.finish()
    }
}

impl Finish for io::Sink {
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}
