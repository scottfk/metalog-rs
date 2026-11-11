//! Thin wrappers over libc, PCRE2 and zlib.
//!
//! This is the only module with `unsafe` code.  Everything here mirrors one
//! C call in upstream metalog, with the same flags and the same failure
//! reporting (errno), so the rest of the port can stay in safe Rust and
//! still make exactly the syscalls the C program makes.
//!
//! errno: upstream's `warnp()` prints `strerror(errno)` from whatever call
//! failed last, including ones it never checks.  The wrappers record the
//! errno of every failing call in [`cerrno`], which is what `warnp` reads,
//! so stray Rust-std syscalls can't change those messages.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicUsize, Ordering};

use crate::bytes::to_cstring;

// ---------------------------------------------------------------------------
// errno

static C_ERRNO: AtomicI32 = AtomicI32::new(0);

fn last_os_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Record the current OS errno as "C's errno" and return it.
fn fail() -> i32 {
    let e = last_os_errno();
    C_ERRNO.store(e, Ordering::Relaxed);
    e
}

/// The errno C metalog would see now.
pub fn cerrno() -> i32 {
    C_ERRNO.load(Ordering::Relaxed)
}

/// Set "C's errno" (for calls like `getgrnam` whose caller sets errno = 0).
pub fn set_cerrno(e: i32) {
    C_ERRNO.store(e, Ordering::Relaxed);
}

/// `strerror(e)`.
pub fn strerror(e: i32) -> Vec<u8> {
    // SAFETY: strerror returns a valid NUL-terminated string (possibly a
    // static "Unknown error N" buffer); it is copied out immediately and the
    // program is single-threaded.
    unsafe { CStr::from_ptr(libc::strerror(e)) }.to_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Plain file and fd operations

pub type Fd = c_int;

pub fn write(fd: Fd, data: &[u8]) -> isize {
    // SAFETY: data is a valid buffer of the given length.
    let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
    if n < 0 {
        fail();
    }
    n
}

/// Write everything, retrying on EINTR, giving up on other errors (stdio's
/// flush behaviour; metalog ignores output errors).
pub fn write_all(fd: Fd, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = write(fd, data);
        if n < 0 {
            if cerrno() == libc::EINTR {
                continue;
            }
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

pub fn read(fd: Fd, buf: &mut [u8]) -> isize {
    // SAFETY: buf is a valid writable buffer of the given length.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        fail();
    }
    n
}

pub fn open(path: &[u8], flags: c_int, mode: libc::mode_t) -> Result<Fd, i32> {
    let p = to_cstring(path);
    // SAFETY: p is a valid C string; open is variadic in the mode argument.
    let fd = unsafe { libc::open(p.as_ptr(), flags, mode as c_uint) };
    if fd < 0 { Err(fail()) } else { Ok(fd) }
}

#[cfg_attr(not(feature = "compress"), allow(dead_code))]
pub fn openat(dirfd: Fd, name: &[u8], flags: c_int, mode: libc::mode_t) -> Result<Fd, i32> {
    let p = to_cstring(name);
    // SAFETY: p is a valid C string.
    let fd = unsafe { libc::openat(dirfd, p.as_ptr(), flags, mode as c_uint) };
    if fd < 0 { Err(fail()) } else { Ok(fd) }
}

pub fn close(fd: Fd) -> i32 {
    // SAFETY: closing an integer fd has no memory-safety implications.
    let r = unsafe { libc::close(fd) };
    if r < 0 {
        fail();
    }
    r
}

/// The parts of `struct stat` metalog looks at.
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub mode: libc::mode_t,
    #[cfg_attr(not(feature = "compress"), allow(dead_code))]
    pub size: i64,
    pub blksize: i64,
}

fn conv_stat(st: &libc::stat) -> Stat {
    Stat { mode: st.st_mode, size: st.st_size, blksize: st.st_blksize }
}

pub fn stat(path: &[u8]) -> Result<Stat, i32> {
    let p = to_cstring(path);
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: p is a valid C string, st has room for a struct stat.
    if unsafe { libc::stat(p.as_ptr(), st.as_mut_ptr()) } < 0 {
        return Err(fail());
    }
    // SAFETY: initialised by the successful stat.
    Ok(conv_stat(unsafe { st.assume_init_ref() }))
}

pub fn fstat(fd: Fd) -> Result<Stat, i32> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: st has room for a struct stat.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } < 0 {
        return Err(fail());
    }
    // SAFETY: initialised by the successful fstat.
    Ok(conv_stat(unsafe { st.assume_init_ref() }))
}

pub fn is_dir(mode: libc::mode_t) -> bool {
    mode & libc::S_IFMT == libc::S_IFDIR
}

pub fn is_chr(mode: libc::mode_t) -> bool {
    mode & libc::S_IFMT == libc::S_IFCHR
}

fn path_call(path: &[u8], f: impl FnOnce(*const c_char) -> c_int) -> Result<(), i32> {
    let p = to_cstring(path);
    if f(p.as_ptr()) < 0 { Err(fail()) } else { Ok(()) }
}

pub fn mkdir(path: &[u8], mode: libc::mode_t) -> Result<(), i32> {
    // SAFETY: valid C string.
    path_call(path, |p| unsafe { libc::mkdir(p, mode) })
}

pub fn unlink(path: &[u8]) -> Result<(), i32> {
    // SAFETY: valid C string.
    path_call(path, |p| unsafe { libc::unlink(p) })
}

pub fn chmod(path: &[u8], mode: libc::mode_t) -> Result<(), i32> {
    // SAFETY: valid C string.
    path_call(path, |p| unsafe { libc::chmod(p, mode) })
}

pub fn unlinkat(dirfd: Fd, name: &[u8]) -> Result<(), i32> {
    // SAFETY: valid C string.
    path_call(name, |p| unsafe { libc::unlinkat(dirfd, p, 0) })
}

pub fn rename(from: &[u8], to: &[u8]) -> Result<(), i32> {
    let a = to_cstring(from);
    let b = to_cstring(to);
    // SAFETY: valid C strings.
    if unsafe { libc::rename(a.as_ptr(), b.as_ptr()) } < 0 { Err(fail()) } else { Ok(()) }
}

pub fn fchmod(fd: Fd, mode: libc::mode_t) -> Result<(), i32> {
    // SAFETY: no pointers involved.
    if unsafe { libc::fchmod(fd, mode) } < 0 { Err(fail()) } else { Ok(()) }
}

/// `lseek(fd, 0, SEEK_END)`: what glibc's `ftell` returns for a stream
/// freshly opened in append mode.
pub fn seek_end(fd: Fd) -> i64 {
    // SAFETY: no pointers involved.
    let r = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
    if r < 0 {
        fail();
    }
    r
}

pub fn isatty(fd: Fd) -> bool {
    // SAFETY: no pointers involved.
    unsafe { libc::isatty(fd) == 1 }
}

/// Read a whole file into memory (stdio-style reading; stops quietly at a
/// read error, as `fgets` returning NULL would).
pub fn read_to_end(fd: Fd) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = read(fd, &mut buf);
        if n < 0 && cerrno() == libc::EINTR {
            continue;
        }
        if n <= 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
}

/// Directory entry names (without `.` and `..` filtering: callers filter,
/// like scandir's callbacks do).  `None` if the directory can't be opened.
pub fn read_dir_names(path: &[u8]) -> Option<Vec<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt;
    let rd = match std::fs::read_dir(crate::bytes::as_path(path)) {
        Ok(rd) => rd,
        Err(e) => {
            set_cerrno(e.raw_os_error().unwrap_or(0));
            return None;
        }
    };
    let mut names: Vec<Vec<u8>> = vec![b".".to_vec(), b"..".to_vec()];
    for ent in rd.flatten() {
        names.push(ent.file_name().as_bytes().to_vec());
    }
    Some(names)
}

// ---------------------------------------------------------------------------
// Processes and signals

pub fn getpid() -> i32 {
    // SAFETY: always safe.
    unsafe { libc::getpid() }
}

pub fn geteuid() -> u32 {
    // SAFETY: always safe.
    unsafe { libc::geteuid() }
}

pub fn getpgrp() -> i32 {
    // SAFETY: always safe.
    unsafe { libc::getpgrp() }
}

pub fn setpgid(pid: i32, pgid: i32) {
    // SAFETY: no pointers involved.
    unsafe { libc::setpgid(pid, pgid) };
}

pub fn fork() -> Result<i32, i32> {
    // SAFETY: metalog is single-threaded, so the child is a faithful copy
    // and may keep running Rust code (C metalog's klog child does too).
    let pid = unsafe { libc::fork() };
    if pid < 0 { Err(fail()) } else { Ok(pid) }
}

/// `_exit(code)`: no atexit handlers, no stdio flushing.
pub fn exit_now(code: i32) -> ! {
    // SAFETY: _exit never returns and touches no Rust state.
    unsafe { libc::_exit(code) }
}

/// `exit(code)` at the libc level (our own stdio has been flushed by the caller).
pub fn exit_process(code: i32) -> ! {
    std::process::exit(code)
}

/// Prepared `execl(cmd, cmd, a, b, c, NULL)`, built before forking so the
/// child only makes async-signal-safe calls.
#[derive(Debug)]
pub struct Exec {
    args: Vec<CString>,
}

impl Exec {
    pub fn new(cmd: &[u8], a: &[u8], b: &[u8], c: &[u8]) -> Exec {
        Exec { args: vec![to_cstring(cmd), to_cstring(cmd), to_cstring(a), to_cstring(b), to_cstring(c)] }
    }

    /// In the forked child: exec, or `_exit(127)`.
    pub fn exec_or_127(&self) -> ! {
        let argv: [*const c_char; 5] = [
            self.args[1].as_ptr(),
            self.args[2].as_ptr(),
            self.args[3].as_ptr(),
            self.args[4].as_ptr(),
            std::ptr::null(),
        ];
        // SAFETY: path and argv are valid NUL-terminated arrays of C strings
        // that outlive the call; execv only returns on failure.
        unsafe {
            libc::execv(self.args[0].as_ptr(), argv.as_ptr());
            libc::_exit(127)
        }
    }
}

/// Die of SIGSEGV, as C metalog does where it dereferences NULL.
pub fn raise_segv() -> ! {
    // SAFETY: restoring the default action and raising a signal have no
    // memory-safety implications; the process ends here.
    unsafe {
        libc::signal(libc::SIGSEGV, libc::SIG_DFL);
        libc::raise(libc::SIGSEGV);
        libc::abort()
    }
}

pub fn abort() -> ! {
    // SAFETY: abort never returns.
    unsafe { libc::abort() }
}

unsafe extern "C" {
    static program_invocation_short_name: *const c_char;
}

/// glibc's `program_invocation_short_name`, read live: it points into the
/// argv area, so after `setprogname()` it shows part of the new title.
pub fn program_short_name() -> Vec<u8> {
    // SAFETY: glibc sets this to a valid C string inside argv[0] at
    // startup; the argv area stays NUL-terminated after setprogname().
    unsafe {
        let p = program_invocation_short_name;
        if p.is_null() { Vec::new() } else { CStr::from_ptr(p).to_bytes().to_vec() }
    }
}

/// `unlink()` of a prepared path: no allocation, safe in a signal handler.
pub fn unlink_c(path: &CStr) {
    // SAFETY: path is a valid C string.
    unsafe { libc::unlink(path.as_ptr()) };
}

pub fn kill(pid: i32, sig: c_int) {
    // SAFETY: no pointers involved.
    unsafe { libc::kill(pid, sig) };
}

/// `waitpid(-1, &status, WNOHANG)`: `Some((pid, status))` while children are reaped.
pub fn waitpid_any_nohang() -> Option<(i32, c_int)> {
    let mut status: c_int = 0;
    // SAFETY: status is a valid out-pointer.
    let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    if pid > 0 { Some((pid, status)) } else { None }
}

pub type Handler = extern "C" fn(c_int);

/// glibc `signal()` (BSD semantics: SA_RESTART, handler stays installed).
pub fn signal(sig: c_int, handler: Handler) {
    // SAFETY: handler is a valid extern "C" fn(c_int).
    unsafe { libc::signal(sig, handler as *const () as libc::sighandler_t) };
}

pub fn signal_ignore(sig: c_int) {
    // SAFETY: SIG_IGN is always a valid disposition.
    unsafe { libc::signal(sig, libc::SIG_IGN) };
}

pub fn signal_default(sig: c_int) {
    // SAFETY: SIG_DFL is always a valid disposition.
    unsafe { libc::signal(sig, libc::SIG_DFL) };
}

pub fn daemon() -> Result<(), i32> {
    // SAFETY: daemon() forks and redirects fds; single-threaded program.
    if unsafe { libc::daemon(0, 0) } < 0 { Err(fail()) } else { Ok(()) }
}

/// `getgrnam(name)` with errno cleared first: `Ok(None)` if not found.
pub fn getgrnam_gid(name: &[u8]) -> Result<Option<libc::gid_t>, i32> {
    let n = to_cstring(name);
    // SAFETY: errno is thread-local; clearing it is what the C code does.
    unsafe { *libc::__errno_location() = 0 };
    // SAFETY: n is a valid C string; the result is read immediately.
    let g = unsafe { libc::getgrnam(n.as_ptr()) };
    if g.is_null() {
        let e = last_os_errno();
        set_cerrno(e);
        return if e == 0 { Ok(None) } else { Err(e) };
    }
    // SAFETY: g is a valid struct group pointer.
    Ok(Some(unsafe { (*g).gr_gid }))
}

pub fn setgid(gid: libc::gid_t) -> Result<(), i32> {
    // SAFETY: no pointers involved.
    if unsafe { libc::setgid(gid) } < 0 { Err(fail()) } else { Ok(()) }
}

/// `klogctl(type, buf, len)`.
pub fn klogctl(ty: c_int, buf: Option<&mut [u8]>, len: c_int) -> c_int {
    let (p, l) = match buf {
        Some(b) => (b.as_mut_ptr().cast::<c_char>(), b.len() as c_int),
        None => (std::ptr::null_mut(), len),
    };
    // SAFETY: p/l describe a valid buffer, or are NULL with a plain number.
    let r = unsafe { libc::klogctl(ty, p, l) };
    if r < 0 {
        fail();
    }
    r
}

// ---------------------------------------------------------------------------
// Sockets and poll

/// `socket(PF_UNIX, SOCK_DGRAM, 0)`.
pub fn socket_unix_dgram() -> Result<Fd, i32> {
    // SAFETY: no pointers involved.
    let fd = unsafe { libc::socket(libc::PF_UNIX, libc::SOCK_DGRAM, 0) };
    if fd < 0 { Err(fail()) } else { Ok(fd) }
}

/// `bind(fd, {AF_UNIX, path}, sizeof(struct sockaddr_un))`.  The path is
/// already truncated to fit `sun_path` (as snprintf did in C).
pub fn bind_unix(fd: Fd, path: &[u8]) -> Result<(), i32> {
    // SAFETY: sockaddr_un is plain old data; all-zero is valid.
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (d, s) in sa.sun_path.iter_mut().zip(path.iter()) {
        *d = *s as c_char;
    }
    // SAFETY: sa is a valid sockaddr_un of the given size.
    let r = unsafe {
        libc::bind(
            fd,
            (&sa as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if r < 0 { Err(fail()) } else { Ok(()) }
}

pub const SUN_PATH_LEN: usize = 108;

/// `socketpair(domain, SOCK_STREAM, 0, fds)`.
pub fn socketpair_stream() -> Result<[Fd; 2], i32> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: fds has room for two ints.
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) } < 0 {
        return Err(fail());
    }
    Ok(fds)
}

pub fn pipe() -> Result<[Fd; 2], i32> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: fds has room for two ints.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(fail());
    }
    Ok(fds)
}

pub use libc::pollfd as PollFd;

pub fn poll(fds: &mut [PollFd]) -> c_int {
    // SAFETY: fds is a valid array of pollfd of the given length.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
    if r < 0 {
        fail();
    }
    r
}

// ---------------------------------------------------------------------------
// Names and addresses

pub const HOST_NAME_MAX: usize = 64;

/// `gethostname(buf, HOST_NAME_MAX + 1)`; `None` on failure.
pub fn gethostname() -> Option<Vec<u8>> {
    let mut buf = [0u8; HOST_NAME_MAX + 1];
    // SAFETY: buf is valid for its length.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        fail();
        return None;
    }
    Some(crate::bytes::c_str_prefix(&buf).to_vec())
}

/// An owned `getaddrinfo()` result list.
#[derive(Debug)]
pub struct AddrInfo {
    head: *mut libc::addrinfo,
}

impl AddrInfo {
    /// `getaddrinfo(node, service, hints)`; `None` on any failure.
    pub fn lookup(
        node: &[u8],
        service: Option<&[u8]>,
        family: c_int,
        socktype: c_int,
        flags: c_int,
    ) -> Option<AddrInfo> {
        let n = to_cstring(node);
        let s = service.map(to_cstring);
        // SAFETY: addrinfo is plain old data; all-zero is valid.
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = family;
        hints.ai_socktype = socktype;
        hints.ai_flags = flags;
        let mut res: *mut libc::addrinfo = std::ptr::null_mut();
        // SAFETY: valid strings and hints; res receives an allocated list.
        let r = unsafe {
            libc::getaddrinfo(n.as_ptr(), s.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()), &hints, &mut res)
        };
        if r != 0 || res.is_null() { None } else { Some(AddrInfo { head: res }) }
    }

    /// `ai_canonname` of the first entry.
    pub fn canonname(&self) -> Option<Vec<u8>> {
        // SAFETY: head is a valid addrinfo from getaddrinfo.
        let c = unsafe { (*self.head).ai_canonname };
        if c.is_null() {
            return None;
        }
        // SAFETY: ai_canonname is a valid C string when set.
        Some(unsafe { CStr::from_ptr(c) }.to_bytes().to_vec())
    }

    /// The entries, in order.
    pub fn entries(&self) -> Vec<Addr> {
        let mut out = Vec::new();
        let mut p = self.head;
        while !p.is_null() {
            // SAFETY: p walks a valid list from getaddrinfo.
            let a = unsafe { &*p };
            out.push(Addr {
                family: a.ai_family,
                socktype: a.ai_socktype,
                protocol: a.ai_protocol,
                addr: a.ai_addr,
                len: a.ai_addrlen,
            });
            p = a.ai_next;
        }
        out
    }
}

impl Drop for AddrInfo {
    fn drop(&mut self) {
        // SAFETY: head came from getaddrinfo and is freed once.
        unsafe { libc::freeaddrinfo(self.head) };
    }
}

/// One `getaddrinfo()` entry; valid while its [`AddrInfo`] lives.
#[derive(Debug, Clone, Copy)]
pub struct Addr {
    pub family: c_int,
    pub socktype: c_int,
    pub protocol: c_int,
    addr: *const libc::sockaddr,
    len: libc::socklen_t,
}

pub fn socket(family: c_int, socktype: c_int, protocol: c_int) -> Result<Fd, i32> {
    // SAFETY: no pointers involved.
    let fd = unsafe { libc::socket(family, socktype, protocol) };
    if fd < 0 { Err(fail()) } else { Ok(fd) }
}

/// `connect(fd, addr)`.  `addr` must come from a live [`AddrInfo`].
pub fn connect(fd: Fd, addr: &Addr) -> Result<(), i32> {
    // SAFETY: addr points into a live getaddrinfo list (caller contract).
    if unsafe { libc::connect(fd, addr.addr, addr.len) } < 0 { Err(fail()) } else { Ok(()) }
}

/// `sendto(fd, data, 0, addr)`.  `addr` must come from a live [`AddrInfo`].
pub fn sendto(fd: Fd, data: &[u8], addr: &Addr) -> Result<(), i32> {
    // SAFETY: data is valid; addr points into a live getaddrinfo list.
    let r = unsafe { libc::sendto(fd, data.as_ptr().cast(), data.len(), 0, addr.addr, addr.len) };
    if r < 0 { Err(fail()) } else { Ok(()) }
}

// ---------------------------------------------------------------------------
// Time

pub fn time_now() -> i64 {
    // SAFETY: NULL is a valid argument.
    unsafe { libc::time(std::ptr::null_mut()) }
}

pub fn clock_realtime() -> (i64, i64) {
    clock(libc::CLOCK_REALTIME)
}

pub fn clock_monotonic() -> (i64, i64) {
    clock(libc::CLOCK_MONOTONIC)
}

fn clock(id: libc::clockid_t) -> (i64, i64) {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is a valid out-pointer.
    unsafe { libc::clock_gettime(id, &mut ts) };
    (ts.tv_sec, ts.tv_nsec)
}

/// `gettimeofday()`; `None` if it fails.
pub fn gettimeofday() -> Option<(i64, i64)> {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    // SAFETY: tv is a valid out-pointer, tz may be NULL.
    if unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) } != 0 {
        fail();
        return None;
    }
    Some((tv.tv_sec, tv.tv_usec))
}

pub type Tm = libc::tm;

/// `localtime()` (not `_r`: glibc's plain `localtime` re-reads TZ on every
/// call, which metalog relies on).
pub fn localtime(t: i64) -> Option<Tm> {
    // SAFETY: t is valid; the static result is copied out immediately in
    // this single-threaded program.
    let p = unsafe { libc::localtime(&t) };
    // SAFETY: non-NULL results point to a valid struct tm.
    if p.is_null() { None } else { Some(unsafe { *p }) }
}

pub fn gmtime(t: i64) -> Option<Tm> {
    // SAFETY: as for localtime.
    let p = unsafe { libc::gmtime(&t) };
    // SAFETY: non-NULL results point to a valid struct tm.
    if p.is_null() { None } else { Some(unsafe { *p }) }
}

pub fn timegm(tm: &mut Tm) -> i64 {
    // SAFETY: tm is a valid struct tm.
    unsafe { libc::timegm(tm) }
}

/// `strftime(buf, buf.len(), fmt, tm)` writing straight into `buf`, like C
/// (on overflow glibc returns 0 and leaves whatever it had written).
pub fn strftime(buf: &mut [u8], fmt: &[u8], tm: &Tm) -> usize {
    let f = to_cstring(fmt);
    // SAFETY: buf is valid for its length, f is a C string, tm is valid.
    unsafe { libc::strftime(buf.as_mut_ptr().cast(), buf.len(), f.as_ptr(), tm) }
}

/// `strptime(s, fmt, tm)` on a zeroed `tm`: the parsed time and the offset
/// of the first unparsed byte.
pub fn strptime(s: &[u8], fmt: &CStr) -> Option<(Tm, usize)> {
    let cs = to_cstring(s);
    // SAFETY: struct tm is plain old data; all-zero is valid.
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    // SAFETY: valid C strings and tm.
    let end = unsafe { libc::strptime(cs.as_ptr(), fmt.as_ptr(), &mut tm) };
    if end.is_null() {
        return None;
    }
    // SAFETY: end points into cs.
    let off = unsafe { end.offset_from(cs.as_ptr()) } as usize;
    Some((tm, off))
}

// ---------------------------------------------------------------------------
// argv / environ (the process title)

static ARGC: AtomicUsize = AtomicUsize::new(0);
static ARGV: AtomicPtr<*mut c_char> = AtomicPtr::new(std::ptr::null_mut());
static ARGV_LTH: AtomicUsize = AtomicUsize::new(0);

extern "C" fn capture_args(argc: c_int, argv: *mut *mut c_char, _envp: *mut *mut c_char) {
    ARGC.store(argc as usize, Ordering::Relaxed);
    ARGV.store(argv, Ordering::Relaxed);
}

/// glibc calls `.init_array` entries with (argc, argv, envp): keep the real
/// argv pointer, which `setprogname()` overwrites like C metalog does.
#[used]
#[unsafe(link_section = ".init_array")]
static CAPTURE_ARGS: extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char) = capture_args;

/// The command-line arguments as bytes.
pub fn args() -> Vec<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt;
    std::env::args_os().map(|a| a.into_vec()).collect()
}

unsafe extern "C" {
    static mut environ: *mut *mut c_char;
}

/// `clearargs()`: measure the contiguous argv+environ string area, and move
/// environ to the heap so that area can be overwritten.
pub fn clearargs() {
    let argv = ARGV.load(Ordering::Relaxed);
    let argc = ARGC.load(Ordering::Relaxed);
    if argv.is_null() || argc == 0 {
        return;
    }
    // SAFETY: argv/environ are the process's own arrays, valid and
    // NUL-terminated; this runs single-threaded before anything else reads
    // environ concurrently.  The new environ is leaked on purpose (as in C).
    unsafe {
        let env = environ;
        let mut i = 0;
        while !env.is_null() && !(*env.add(i)).is_null() {
            i += 1;
        }
        let start = *argv as usize;
        let lth = if i > 0 {
            let last = *env.add(i - 1);
            last as usize + libc::strlen(last) - start
        } else {
            let last = *argv.add(argc - 1);
            last as usize + libc::strlen(last) - start
        };
        ARGV_LTH.store(lth, Ordering::Relaxed);
        if !env.is_null() {
            let mut copy: Vec<*mut c_char> = Vec::with_capacity(i + 1);
            for k in 0..i {
                let d = libc::strdup(*env.add(k));
                if d.is_null() {
                    crate::stdio::warn(&crate::bcat!(
                        "error: strdup(\"",
                        CStr::from_ptr(*env.add(k)).to_bytes(),
                        "\") failed: ",
                        strerror(last_os_errno())
                    ));
                    crate::stdio::exit(libc::EXIT_FAILURE);
                }
                copy.push(d);
            }
            copy.push(std::ptr::null_mut());
            environ = Box::leak(copy.into_boxed_slice()).as_mut_ptr();
        }
    }
}

/// `setprogname()`: overwrite the argv area with `title`.
pub fn setprogname(title: &[u8]) {
    let argv = ARGV.load(Ordering::Relaxed);
    let lth = ARGV_LTH.load(Ordering::Relaxed);
    if argv.is_null() || lth == 0 {
        return;
    }
    // SAFETY: [argv[0], argv[0] + lth) is the process's argv/environ string
    // area measured by clearargs(); environ no longer points into it.
    // Nothing in Rust holds references into it (std copies args lazily and
    // we read them before this).
    unsafe {
        let base = *argv;
        std::ptr::write_bytes(base, 0, lth);
        let n = title.len().min(lth.saturating_sub(2));
        std::ptr::copy_nonoverlapping(title.as_ptr().cast::<c_char>(), base, n);
        if ARGC.load(Ordering::Relaxed) > 1 {
            *argv.add(1) = std::ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------------------
// PCRE2 (8-bit library, as metalog uses it)

#[repr(C)]
struct Pcre2Code {
    _p: [u8; 0],
}
#[repr(C)]
struct Pcre2MatchData {
    _p: [u8; 0],
}

#[link(name = "pcre2-8")]
unsafe extern "C" {
    fn pcre2_compile_8(
        pattern: *const u8,
        length: usize,
        options: u32,
        errorcode: *mut c_int,
        erroroffset: *mut usize,
        ccontext: *mut c_void,
    ) -> *mut Pcre2Code;
    fn pcre2_code_free_8(code: *mut Pcre2Code);
    fn pcre2_match_data_create_8(ovecsize: u32, gcontext: *mut c_void) -> *mut Pcre2MatchData;
    fn pcre2_match_data_free_8(md: *mut Pcre2MatchData);
    fn pcre2_match_8(
        code: *const Pcre2Code,
        subject: *const u8,
        length: usize,
        startoffset: usize,
        options: u32,
        md: *mut Pcre2MatchData,
        mcontext: *mut c_void,
    ) -> c_int;
    fn pcre2_get_ovector_pointer_8(md: *mut Pcre2MatchData) -> *mut usize;
    fn pcre2_get_error_message_8(errorcode: c_int, buffer: *mut u8, bufflen: usize) -> c_int;
}

pub const PCRE2_CASELESS: u32 = 0x0000_0008;

/// A compiled PCRE2 pattern.
#[derive(Debug)]
pub struct Regex {
    code: *mut Pcre2Code,
}

impl Regex {
    /// `pcre2_compile(pattern, PCRE2_ZERO_TERMINATED, options)`.
    /// On failure: (error message, error offset).
    pub fn compile(pattern: &[u8], options: u32) -> Result<Regex, (Vec<u8>, usize)> {
        let pat = crate::bytes::c_str_prefix(pattern);
        let mut code: c_int = 0;
        let mut off: usize = 0;
        // SAFETY: pat is a valid buffer of the given length.
        let re =
            unsafe { pcre2_compile_8(pat.as_ptr(), pat.len(), options, &mut code, &mut off, std::ptr::null_mut()) };
        if re.is_null() {
            let mut buf = [0u8; 256];
            // SAFETY: buf is valid for its length.
            unsafe { pcre2_get_error_message_8(code, buf.as_mut_ptr(), buf.len()) };
            return Err((crate::bytes::c_str_prefix(&buf).to_vec(), off));
        }
        Ok(Regex { code: re })
    }

    /// `pcre2_match(...) >= 0`, with a 16-pair match block like metalog's.
    pub fn is_match(&self, subject: &[u8]) -> bool {
        self.captures(subject).0 >= 0
    }

    /// The return code and the ovector pairs (only meaningful when rc > 0).
    pub fn captures(&self, subject: &[u8]) -> (c_int, Vec<(usize, usize)>) {
        // SAFETY: creating a match block has no preconditions.
        let md = unsafe { pcre2_match_data_create_8(16, std::ptr::null_mut()) };
        if md.is_null() {
            return (-1, Vec::new());
        }
        // SAFETY: code and md are valid, subject is valid for its length.
        let rc = unsafe { pcre2_match_8(self.code, subject.as_ptr(), subject.len(), 0, 0, md, std::ptr::null_mut()) };
        let mut pairs = Vec::new();
        if rc > 0 {
            // SAFETY: the ovector has 16 pairs; rc <= 16.
            let ov = unsafe { pcre2_get_ovector_pointer_8(md) };
            for i in 0..rc as usize {
                // SAFETY: within the ovector.
                let (s, e) = unsafe { (*ov.add(2 * i), *ov.add(2 * i + 1)) };
                pairs.push((s, e));
            }
        }
        // SAFETY: md was created above and is freed once.
        unsafe { pcre2_match_data_free_8(md) };
        (rc, pairs)
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        // SAFETY: code came from pcre2_compile and is freed once.
        unsafe { pcre2_code_free_8(self.code) };
    }
}

// ---------------------------------------------------------------------------
// zlib (gzdopen/gzwrite/gzclose, exactly as compressLogFile uses them)

#[cfg(feature = "compress")]
mod zlib {
    use std::ffi::{c_char, c_int, c_uint, c_void};

    #[link(name = "z")]
    unsafe extern "C" {
        pub fn gzdopen(fd: c_int, mode: *const c_char) -> *mut c_void;
        pub fn gzwrite(file: *mut c_void, buf: *const c_void, len: c_uint) -> c_int;
        pub fn gzclose(file: *mut c_void) -> c_int;
    }
}

/// `gz = gzdopen(fd, "w")`, `gzwrite(gz, data)`, `gzclose(gz)`: returns
/// (gzwrite's result, gzclose == Z_OK), or `None` if gzdopen failed (the
/// fd is then still open).
#[cfg(feature = "compress")]
pub fn gzip_fd(fd: Fd, data: &[u8]) -> Option<(c_int, bool)> {
    // SAFETY: "w" is a valid C string; fd is an open writable fd.
    let gz = unsafe { zlib::gzdopen(fd, c"w".as_ptr()) };
    if gz.is_null() {
        return None;
    }
    // SAFETY: gz is valid, data is valid for its length (truncated to
    // unsigned int as the C call does).
    let sz = unsafe { zlib::gzwrite(gz, data.as_ptr().cast(), data.len() as c_uint) };
    // SAFETY: gz is valid and closed once.
    let ok = unsafe { zlib::gzclose(gz) } == 0;
    Some((sz, ok))
}
