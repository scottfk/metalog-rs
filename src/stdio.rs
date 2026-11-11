//! C stdio emulation: buffered `FILE` streams, `stdout`, `stderr`, `exit()`.
//!
//! metalog writes its log files and its stdout copy through stdio, and
//! *when* bytes reach the file is observable: in synchronous mode every
//! line is `fflush`ed, in asynchronous mode (`-a`, SIGUSR2) the data sits in
//! a buffer of `st_blksize` bytes, and a SIGTERM `_exit`s without flushing
//! stdout.  [`CFile`] reproduces glibc's buffer sizing and its
//! `_IO_new_file_xsputn` flushing rule.  (The only intended difference: a
//! single `fprintf` that overflows the buffer may be split into `write(2)`s
//! at different offsets.  The bytes and the flush points are the same.)

use std::cell::RefCell;

use crate::sys;

/// glibc's `BUFSIZ`.
const BUFSIZ: usize = 8192;

/// A fully- or line-buffered output stream on an fd.
#[derive(Debug)]
pub struct CFile {
    fd: sys::Fd,
    buf: Vec<u8>,
    /// Buffer size once allocated (glibc allocates on first write).
    cap: Option<usize>,
    line_buffered: bool,
    error: bool,
}

impl CFile {
    pub fn from_fd(fd: sys::Fd) -> CFile {
        CFile { fd, buf: Vec::new(), cap: None, line_buffered: false, error: false }
    }

    pub fn fd(&self) -> sys::Fd {
        self.fd
    }

    /// `_IO_file_doallocate`: BUFSIZ, or st_blksize if smaller; character
    /// devices that are terminals become line-buffered.
    fn allocate(&mut self) -> usize {
        if let Some(c) = self.cap {
            return c;
        }
        let mut size = BUFSIZ;
        if let Ok(st) = sys::fstat(self.fd) {
            if sys::is_chr(st.mode) && sys::isatty(self.fd) {
                self.line_buffered = true;
            }
            if st.blksize > 0 && (st.blksize as usize) < BUFSIZ {
                size = st.blksize as usize;
            }
        }
        self.cap = Some(size);
        self.buf.reserve_exact(size);
        size
    }

    fn write_direct(&mut self, data: &[u8]) -> bool {
        if !sys::write_all(self.fd, data) {
            self.error = true;
            return false;
        }
        true
    }

    /// `fputs`/`fprintf`: glibc `_IO_new_file_xsputn`.
    pub fn write(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let first = self.cap.is_none();
        let cap = self.allocate();
        // A fresh stream has no buffer yet, hence no room: glibc goes
        // straight to the overflow path on the first write.
        let space = if first { 0 } else { cap - self.buf.len() };
        let mut count = space;
        let mut must_flush = false;
        if self.line_buffered
            && !first
            && count >= data.len()
            && let Some(nl) = data.iter().rposition(|&b| b == b'\n')
        {
            count = nl + 1;
            must_flush = true;
        }
        let mut rest = data;
        if count > 0 {
            let n = count.min(rest.len());
            self.buf.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
        }
        if !rest.is_empty() || must_flush {
            if !self.flush() {
                return;
            }
            let block = cap;
            let do_write = rest.len() - if block >= 128 { rest.len() % block } else { 0 };
            if do_write > 0 {
                if !self.write_direct(&rest[..do_write]) {
                    return;
                }
                rest = &rest[do_write..];
            }
            if !rest.is_empty() {
                // _IO_default_xsputn into the now-empty buffer; a
                // line-buffered stream still flushes at a newline.
                self.buf.extend_from_slice(rest);
                if self.line_buffered && rest.contains(&b'\n') {
                    self.flush();
                }
            }
        }
    }

    /// `fflush`.
    pub fn flush(&mut self) -> bool {
        if self.buf.is_empty() {
            return true;
        }
        let data = std::mem::take(&mut self.buf);
        let ok = self.write_direct(&data);
        self.buf = data;
        self.buf.clear();
        ok
    }

    pub fn has_error(&self) -> bool {
        self.error
    }

    /// `fclose`: flush and close; `false` if either failed.
    pub fn close(mut self) -> bool {
        let ok = self.flush();
        let c = sys::close(self.fd) == 0;
        ok && c && !self.error
    }
}

thread_local! {
    static STDOUT: RefCell<CFile> = RefCell::new(CFile::from_fd(1));
}

/// `fputs(data, stdout)`.
pub fn out(data: &[u8]) {
    STDOUT.with(|s| s.borrow_mut().write(data));
}

/// `fflush(stdout)`.
pub fn flush_stdout() {
    STDOUT.with(|s| {
        s.borrow_mut().flush();
    });
}

/// Unbuffered `fputs(data, stderr)` (glibc issues one write per call).
pub fn err(data: &[u8]) {
    sys::write_all(2, data);
}

/// `warn()`: `metalog: <msg>\n` on stderr.
pub fn warn(msg: &[u8]) {
    err(&crate::bcat!("metalog: ", msg, "\n"));
}

/// `warnp()`: `warn()` plus `: strerror(errno)` for the last failed call.
pub fn warnp(msg: &[u8]) {
    warn(&crate::bcat!(msg, ": ", sys::strerror(sys::cerrno())));
}

/// `err()`: `metalog: error: <msg>`, then `exit(EXIT_FAILURE)`.
pub fn die(msg: &[u8]) -> ! {
    warn(&crate::bcat!("error: ", msg));
    exit(libc::EXIT_FAILURE)
}

/// `errp()`: `err()` with `: strerror(errno)`.
pub fn diep(msg: &[u8]) -> ! {
    warnp(&crate::bcat!("error: ", msg));
    exit(libc::EXIT_FAILURE)
}

/// `exit()`: atexit handlers (metalog registers `exit_hook` once its
/// signals are set up), then every stdio stream is flushed.
pub fn exit(code: i32) -> ! {
    crate::daemon::run_atexit();
    crate::daemon::flush_all_outputs();
    crate::daemon::flush_leaked();
    flush_stdout();
    sys::exit_process(code)
}
