//! Process-level machinery: the global state, signal handlers and their
//! queue, data sources (the syslog socket and the klog child), the poll
//! loop, the pid file and daemonising.
//!
//! # Signals
//!
//! Upstream's handlers do real work: SIGUSR1 flushes every log file, and
//! SIGTERM & co. flush, kill the klog child, remove the pid file and
//! `_exit()` right inside the handler.  That's only safe while the main
//! loop is blocked in `poll()`, which is where metalog spends its time.
//! So the handlers here look at [`MODE`]:
//!
//! * `IDLE` (inside `poll()`): do exactly what C does, immediately.
//! * `BUSY` (handling an event): record the work, and the loop does it as
//!   soon as the current event is done.  (C would act mid-message, which is
//!   undefined behaviour.)
//! * `STARTUP` and `CHILD` (before the loop, and in the klog child): act
//!   immediately; there's nothing buffered to flush yet.
//!
//! Everything a handler touches outside the state is an atomic, and the
//! main loop never holds a reference into the state across `poll()`.

use std::cell::UnsafeCell;
use std::ffi::{CString, c_int};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};

use crate::bcat;
use crate::config::{Config, SourceType};
use crate::process::{do_log, log_kernel, log_line};
use crate::stdio::{self, CFile, die, diep, warn, warnp};
use crate::sys::{self, Regex};

pub const PROGNAME_MASTER: &[u8] = b"metalog [MASTER]";
pub const PROGNAME_KERNEL: &[u8] = b"metalog [KERNEL]";

/// Everything the C code keeps in globals that isn't touched by handlers.
#[derive(Debug)]
pub struct State {
    pub cfg: Config,
    pub spawn_recursion: i32,
    pub re_compressed: Regex,
    pub console_level: c_int,
    /// FILE streams C lost track of (see `output::open_output`); exit()
    /// still flushes them.
    pub leaked: Vec<CFile>,
}

struct StateCell(UnsafeCell<Option<State>>);
// SAFETY: metalog is single-threaded; the only concurrent access is from
// signal handlers on the same thread, governed by MODE (module docs).
unsafe impl Sync for StateCell {}

static STATE: StateCell = StateCell(UnsafeCell::new(None));

pub fn install_state(st: State) {
    // SAFETY: called once from main before any handler can touch the state.
    unsafe { *STATE.0.get() = Some(st) };
}

/// The global state.  Callers must not keep the reference across a point
/// where a handler may run in IDLE mode (i.e. across `poll()`).
#[allow(clippy::mut_from_ref)]
pub fn state() -> &'static mut State {
    // SAFETY: see the module docs; the state is installed before use.
    unsafe { (*STATE.0.get()).as_mut().expect("state installed") }
}

fn state_if_installed() -> Option<&'static mut State> {
    // SAFETY: as for state(); used by exit paths that never return.
    unsafe { (*STATE.0.get()).as_mut() }
}

pub static SYNCHRONOUS: AtomicBool = AtomicBool::new(true);
pub static VERBOSE: AtomicI32 = AtomicI32::new(0);
static KEEP_RUNNING: AtomicBool = AtomicBool::new(true);
static CHILD: AtomicI32 = AtomicI32::new(0);
/// `dolog_queue[2]`: 0 until created, as for C's zero-initialised statics.
static DOLOG_Q: [AtomicI32; 2] = [AtomicI32::new(0), AtomicI32::new(0)];
static ATEXIT_REGISTERED: AtomicBool = AtomicBool::new(false);
static PID_FILE: OnceLock<CString> = OnceLock::new();

const MODE_STARTUP: u8 = 0;
const MODE_CHILD: u8 = 1;
const MODE_BUSY: u8 = 2;
const MODE_IDLE: u8 = 3;
static MODE: AtomicU8 = AtomicU8::new(MODE_STARTUP);
static PENDING_EXIT: AtomicI32 = AtomicI32::new(-1);
static PENDING_FLUSH: AtomicBool = AtomicBool::new(false);

pub fn set_pid_file(path: &[u8]) {
    let _ = PID_FILE.set(crate::bytes::to_cstring(path));
}

/// glibc's `__assert_fail()` message, then `abort()`.
pub fn assert_fail(expr: &[u8], line: u32, func: &[u8]) -> ! {
    stdio::err(&bcat!(
        sys::program_short_name(),
        ": metalog.c:",
        line.to_string(),
        ": ",
        func,
        ": Assertion `",
        expr,
        "' failed.\n"
    ));
    sys::abort()
}

/// `flushAll()`: flush every log file.
pub fn flush_all_outputs() {
    if let Some(st) = state_if_installed() {
        for o in &mut st.cfg.outputs {
            if let Some(fp) = o.fp.as_mut() {
                fp.flush();
            }
        }
    }
}

/// What `exit()` flushes beyond flushAll: streams C leaked.
pub fn flush_leaked() {
    if let Some(st) = state_if_installed() {
        for fp in &mut st.leaked {
            fp.flush();
        }
    }
}

fn delete_pid_file() {
    if let Some(p) = PID_FILE.get() {
        sys::unlink_c(p);
    }
}

/// `exit_hook()`.  `may_flush` is false where the log buffers can't be
/// touched (or are known to be empty).
fn exit_hook(may_flush: bool) {
    SYNCHRONOUS.store(true, Ordering::Relaxed);
    if may_flush {
        flush_all_outputs();
    }
    let child = CHILD.load(Ordering::Relaxed);
    if child > 0 {
        sys::kill(child, libc::SIGTERM);
    }
    delete_pid_file();
    KEEP_RUNNING.store(false, Ordering::Relaxed);
}

/// The atexit() part of `exit()`.
pub fn run_atexit() {
    if ATEXIT_REGISTERED.load(Ordering::Relaxed) {
        exit_hook(MODE.load(Ordering::Relaxed) != MODE_CHILD);
    }
}

/// `metalog_signal_exit()` from a handler.
///
/// QUIRK: termination by signal flushes the log files but `_exit()`s
/// without flushing stdout, and the exit status is always 1.
fn signal_exit(code: i32) {
    match MODE.load(Ordering::Relaxed) {
        MODE_BUSY => {
            PENDING_EXIT.store(code, Ordering::Relaxed);
        }
        m => {
            exit_hook(m == MODE_IDLE);
            sys::exit_now(code);
        }
    }
}

/// Work a handler deferred while the loop was busy.
fn run_pending() {
    if PENDING_FLUSH.swap(false, Ordering::Relaxed) {
        flush_all_outputs();
    }
    let code = PENDING_EXIT.swap(-1, Ordering::Relaxed);
    if code >= 0 {
        exit_hook(true);
        sys::exit_now(code);
    }
}

/// `signal_doLog_queue()`: async-signal-safe; formatted when dequeued.
fn queue(fmt: &'static [u8], pid: u32, status: c_int) {
    let mut hdr = [0u8; 12];
    hdr[..4].copy_from_slice(&pid.to_ne_bytes());
    hdr[4..8].copy_from_slice(&status.to_ne_bytes());
    hdr[8..].copy_from_slice(&(fmt.len() as u32).to_ne_bytes());
    let fd = DOLOG_Q[1].load(Ordering::Relaxed);
    if sys::write(fd, &hdr) != 12 {
        assert_fail(b"ret == sizeof(buf)", 2046, b"signal_doLog_queue");
    }
    if sys::write(fd, fmt) != fmt.len() as isize {
        assert_fail(b"ret == (ssize_t) fmt_len", 2048, b"signal_doLog_queue");
    }
}

/// vsnprintf(fmt, pid, status) for the queue's fixed formats: the first
/// conversion takes the pid, the second the status; `%u` prints unsigned.
fn format_queued(fmt: &[u8], pid: u32, status: c_int) -> Vec<u8> {
    let args = [pid, status as u32];
    let mut next = 0;
    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] == b'%' && matches!(fmt.get(i + 1), Some(b'u' | b'd')) {
            let v = args.get(next).copied().unwrap_or(0);
            next += 1;
            let text = if fmt[i + 1] == b'd' { (v as i32).to_string() } else { v.to_string() };
            out.extend_from_slice(text.as_bytes());
            i += 2;
        } else {
            out.push(fmt[i]);
            i += 1;
        }
    }
    out
}

/// `signal_doLog_dequeue()`.
fn dequeue(st: &mut State) {
    let fd = DOLOG_Q[0].load(Ordering::Relaxed);
    let mut w = [0u8; 4];
    if sys::read(fd, &mut w) != 4 {
        assert_fail(b"ret == sizeof(pid)", 2059, b"signal_doLog_dequeue");
    }
    let pid = u32::from_ne_bytes(w);
    if sys::read(fd, &mut w) != 4 {
        assert_fail(b"ret == sizeof(status)", 2061, b"signal_doLog_dequeue");
    }
    let status = i32::from_ne_bytes(w);
    if sys::read(fd, &mut w) != 4 {
        assert_fail(b"ret == sizeof(fmt_len)", 2063, b"signal_doLog_dequeue");
    }
    let len = u32::from_ne_bytes(w) as usize;
    let mut fmt = vec![0u8; len];
    if sys::read(fd, &mut fmt) != len as isize {
        assert_fail(b"ret == (ssize_t) fmt_len", 2068, b"signal_doLog_dequeue");
    }
    st.spawn_recursion += 1;
    do_log(st, &format_queued(&fmt, pid, status));
    st.spawn_recursion -= 1;
}

extern "C" fn sigkchld(sig: c_int) {
    queue(b"Process [%u] died with signal [%d]\n", sys::getpid() as u32, sig);
    signal_exit(libc::EXIT_FAILURE);
}

extern "C" fn sigchld(_sig: c_int) {
    let mut should_exit = false;
    while let Some((pid, status)) = sys::waitpid_any_nohang() {
        if pid == CHILD.load(Ordering::Relaxed) {
            queue(b"Klog child [%u] died.", pid as u32, 0);
            should_exit = true;
        } else if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) != 0 {
            queue(b"Child [%u] exited with return code %u.", pid as u32, libc::WEXITSTATUS(status));
        } else if !libc::WIFEXITED(status) {
            queue(b"Child [%u] caught signal %u.", pid as u32, libc::WTERMSIG(status));
        } else if VERBOSE.load(Ordering::Relaxed) != 0 {
            queue(b"Child [%u] exited successfully.", pid as u32, 0);
        }
    }
    if should_exit {
        CHILD.store(0, Ordering::Relaxed);
        KEEP_RUNNING.store(false, Ordering::Relaxed);
        signal_exit(libc::EXIT_FAILURE);
    }
}

extern "C" fn sigusr1(_sig: c_int) {
    SYNCHRONOUS.store(true, Ordering::Relaxed);
    queue(b"Got SIGUSR1 - enabling synchronous mode.", 0, 0);
    match MODE.load(Ordering::Relaxed) {
        MODE_IDLE => flush_all_outputs(),
        MODE_BUSY => PENDING_FLUSH.store(true, Ordering::Relaxed),
        _ => {}
    }
}

extern "C" fn sigusr2(_sig: c_int) {
    SYNCHRONOUS.store(false, Ordering::Relaxed);
    queue(b"Got SIGUSR2 - disabling synchronous mode.", 0, 0);
}

/// `setsignals()`.
pub fn setsignals() {
    ATEXIT_REGISTERED.store(true, Ordering::Relaxed);
    sys::signal(libc::SIGCHLD, sigchld);
    for s in [libc::SIGPIPE, libc::SIGHUP, libc::SIGTERM, libc::SIGQUIT, libc::SIGINT] {
        sys::signal(s, sigkchld);
    }
    sys::signal(libc::SIGUSR1, sigusr1);
    sys::signal(libc::SIGUSR2, sigusr2);
}

/// `update_pid_file()`.
///
/// QUIRK: a relative pid file is silently not written, but it is still
/// unlinked at exit (relative to `/` after --daemonize).
pub fn update_pid_file(path: &[u8]) -> i32 {
    if path.first() != Some(&b'/') {
        return 0;
    }
    let fail = || {
        warnp(&bcat!("pid file creation failed: ", path));
        -1
    };
    let Ok(fd) = sys::open(path, libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC | libc::O_NOFOLLOW, 0o644) else {
        return fail();
    };
    if sys::fchmod(fd, 0o644).is_err() {
        return fail();
    }
    let mut fp = CFile::from_fd(fd);
    fp.write(format!("{}\n", sys::getpid()).as_bytes());
    if fp.has_error() || !fp.close() {
        return fail();
    }
    0
}

/// `getSyslogDataSource()`.
fn syslog_source(st: &mut State, si: usize) -> i32 {
    st.cfg.sources[si].stream = false;
    let Ok(fd) = sys::socket_unix_dgram() else {
        warnp(b"Unable to create a local socket");
        return -1;
    };
    let full = st.cfg.sources[si].path.clone().unwrap_or_default();
    // QUIRK: snprintf into sun_path silently truncates long paths.
    let path = &full[..full.len().min(sys::SUN_PATH_LEN - 1)];
    let _ = sys::unlink(path);
    if sys::bind_unix(fd, path).is_err() {
        warnp(&bcat!("Unable to bind local socket: ", full));
        sys::close(fd);
        return if st.cfg.sources[si].required { -1 } else { 0 };
    }
    let _ = sys::chmod(path, 0o666);
    st.cfg.sources[si].fd = fd;
    0
}

/// `getKernelDataSource()`: fork the "metalog [KERNEL]" child, which reads
/// klogctl() and passes the bytes on through a socketpair.
fn kernel_source(st: &mut State, si: usize) -> i32 {
    let Ok(pair) = sys::socketpair_stream() else {
        warnp(b"Unable to create a pipe");
        sys::close(st.cfg.sources[si].fd);
        return -3;
    };
    let pgid = sys::getpgrp();
    let child = match sys::fork() {
        Ok(c) => c,
        Err(_) => {
            warnp(b"Unable to create the klogd child");
            sys::close(st.cfg.sources[si].fd);
            return -3;
        }
    };
    CHILD.store(child, Ordering::Relaxed);
    if child == 0 {
        MODE.store(MODE_CHILD, Ordering::Relaxed);
        sys::signal_ignore(libc::SIGUSR1);
        sys::signal_ignore(libc::SIGUSR2);
        sys::setpgid(0, pgid);
        sys::setprogname(PROGNAME_KERNEL);
        if sys::klogctl(1, None, 0) < 0 || sys::klogctl(8, None, st.console_level) < 0 || sys::klogctl(6, None, 0) < 0 {
            warnp(b"Unable to control the kernel logging device");
            // QUIRK: the child returns into main()'s startup code, which
            // reports "Unable to bind sockets" and exits, removing the
            // master's pid file on the way out.
            return -4;
        }
        let mut line = [0u8; 2048];
        'outer: loop {
            let s = loop {
                let r = sys::klogctl(2, Some(&mut line), 0);
                if r < 0 && sys::cerrno() == libc::EINTR {
                    continue;
                }
                break r;
            };
            if s == -1 {
                break;
            }
            let s = s as isize;
            let mut out: isize = 0;
            while out < s {
                let w = sys::write(pair[1], &line[out as usize..s as usize]);
                if w == -1 && sys::cerrno() != libc::EINTR {
                    break 'outer;
                }
                // QUIRK: an EINTR'd write adds -1, re-sending one byte (C
                // can even index before the buffer; clamped here).
                out = (out + w).max(0);
            }
        }
        sys::klogctl(7, None, 0);
        sys::klogctl(0, None, 0);
        // QUIRK: the child then carries on with the master's startup:
        // it binds the remaining sources and runs a second main loop.
        return 0;
    }
    st.cfg.sources[si].fd = pair[0];
    st.cfg.sources[si].stream = true;
    0
}

/// `getDataSources()`: the signal queue, then every source.  Returns the
/// number of open sources, or a negative error.
pub fn get_data_sources(st: &mut State) -> i32 {
    let q = match sys::socketpair_stream() {
        Ok(q) => q,
        Err(_) => match sys::pipe() {
            Ok(q) => q,
            Err(_) => {
                warnp(b"Unable to create a pipe");
                return -4;
            }
        },
    };
    DOLOG_Q[0].store(q[0], Ordering::Relaxed);
    DOLOG_Q[1].store(q[1], Ordering::Relaxed);

    let mut ret = 0;
    let mut count = 0;
    for si in st.cfg.sources_c_order() {
        if ret != 0 {
            break;
        }
        ret = match st.cfg.sources[si].ty {
            SourceType::Klog => kernel_source(st, si),
            SourceType::Syslog => syslog_source(st, si),
        };
        if st.cfg.sources[si].fd != -1 {
            count += 1;
        }
    }
    if ret > 0 {
        assert_fail(b"ret <= 0", 958, b"getDataSources");
    }
    if ret == 0 { count } else { ret }
}

const BUF: usize = 4096;
const POLL_EVENTS: i16 = libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;

/// `process()`: the main loop.
pub fn process(num_sockets: i32) -> i32 {
    let num = num_sockets.max(0) as usize;
    let mut fds: Vec<sys::PollFd> = Vec::with_capacity(num + 1);
    let mut source_map: Vec<usize> = Vec::with_capacity(num);
    let mut buffers: Vec<[u8; BUF]> = vec![[0u8; BUF]; num];
    {
        let st = state();
        fds.push(sys::PollFd { fd: DOLOG_Q[0].load(Ordering::Relaxed), events: POLL_EVENTS, revents: 0 });
        for si in st.cfg.sources_c_order() {
            let s = &mut st.cfg.sources[si];
            if s.fd != -1 {
                if fds.len() > num {
                    assert_fail(b"nfds <= num_sockets", 1898, b"process");
                }
                source_map.push(si);
                s.bpos = 0;
                fds.push(sys::PollFd { fd: s.fd, events: POLL_EVENTS, revents: 0 });
            }
        }
    }
    let nfds = fds.len();

    MODE.store(MODE_BUSY, Ordering::Relaxed);
    while KEEP_RUNNING.load(Ordering::Relaxed) {
        loop {
            MODE.store(MODE_IDLE, Ordering::SeqCst);
            let r = sys::poll(&mut fds[..nfds]);
            MODE.store(MODE_BUSY, Ordering::SeqCst);
            if !(r < 0 && sys::cerrno() == libc::EINTR && KEEP_RUNNING.load(Ordering::Relaxed)) {
                break;
            }
        }
        run_pending();

        if fds[0].revents != 0 {
            let ev = fds[0].revents;
            fds[0].revents = 0;
            if ev != libc::POLLIN {
                warn(b"Signal queue socket error - aborting");
                sys::close(fds[0].fd);
                return -1;
            }
            dequeue(state());
            run_pending();
        }

        for si in 0..source_map.len() {
            let ev = fds[1 + si].revents;
            if ev == 0 {
                continue;
            }
            let s = source_map[si];
            fds[1 + si].revents = 0;
            let st = state();
            if ev != libc::POLLIN {
                let kind = if st.cfg.sources[s].ty == SourceType::Klog { "Klog" } else { "Syslog" };
                warn(&bcat!(kind, " socket error - aborting"));
                sys::close(fds[1 + si].fd);
                return -1;
            }
            let fd = fds[1 + si].fd;
            let buf = &mut buffers[si];
            if !st.cfg.sources[s].stream {
                let rd = loop {
                    let r = sys::read(fd, &mut buf[..BUF - 1]);
                    if r < 0 && sys::cerrno() == libc::EINTR {
                        continue;
                    }
                    break r;
                };
                if rd == -1 {
                    return -1;
                }
                buf[rd as usize] = 0;
                log_line(st, s, &mut buf[..]);
            } else {
                let bpos = st.cfg.sources[s].bpos;
                let rd = loop {
                    let r = sys::read(fd, &mut buf[bpos..]);
                    if r < 0 && sys::cerrno() == libc::EINTR {
                        continue;
                    }
                    break r;
                };
                if rd == -1 {
                    return -1;
                }
                let rd = rd as usize + bpos;
                let ld = log_kernel(st, s, &mut buf[..], rd);
                if ld > 0 {
                    // QUIRK: C moves the wrong bytes here (it treats the
                    // leftover length as an offset), so a kernel line split
                    // across reads comes out garbled or is dropped.
                    let nb = rd - ld;
                    buf.copy_within(ld..ld + nb, 0);
                    st.cfg.sources[s].bpos = nb;
                } else {
                    st.cfg.sources[s].bpos = 0;
                }
            }
            run_pending();
        }
    }
    0
}

/// `checkRoot()`.
pub fn check_root() {
    if sys::geteuid() != 0 {
        die(b"Sorry, you must be root to launch this program");
    }
}

/// `setgroup()`.
pub fn setgroup(group: Option<&[u8]>) {
    let Some(g) = group else { return };
    match sys::getgrnam_gid(g) {
        Ok(None) => die(&bcat!("Failed to set group: group '", g, "' not found")),
        Err(_) => diep(b"Failed to set group"),
        Ok(Some(gid)) => {
            if sys::setgid(gid).is_err() {
                diep(b"Failed to set group");
            }
        }
    }
}

/// `dodaemonize()` (with daemon(3), as HAVE_DAEMON builds do).
pub fn dodaemonize(daemonize: bool) {
    if daemonize && sys::daemon().is_err() {
        diep(b"Daemonization failed");
    }
}
