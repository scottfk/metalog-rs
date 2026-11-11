//! Differential test harness: run the C reference metalog and metalog-rs
//! on the same input and compare everything observable.
//!
//! Each run gets a fresh directory `T` under `target/difftest/<case>/<impl>`
//! and runs as "root" in a private user namespace (metalog insists on euid
//! 0), with `-N -S T/sock -C T/metalog.conf -p T/metalog.pid` by default.
//! `@T@` in configs, arguments and files stands for that directory.  The
//! clock is the LD_PRELOAD shim in tests/shim/, stepped by [`Step::Time`].
//!
//! After the steps, metalog gets SIGTERM (unless it already exited) and
//! the harness compares: the exit status, stdout, stderr, and every file
//! under `T` (type, permissions, contents), with `T` and pids normalized.
//!
//! The reference binary comes from `$METALOG_REFERENCE`, default
//! `../metalog/src/metalog` (tools/build-reference.sh builds it there).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::{CommandExt, ExitStatusExt};

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const DEFAULT_TIME: (i64, i64) = (1_762_869_600, 123_456_789); // 2025-11-11 14:00:00 UTC

#[derive(Debug, Clone)]
pub enum Step {
    /// Send one datagram to `T/sock`, then wait until metalog is idle.
    Send(Vec<u8>),
    /// Send a datagram to another socket under T (a `socket =` source).
    SendTo(String, Vec<u8>),
    /// Set the fake clock.
    Time(i64, i64),
    /// Send a signal, then wait until idle.
    Signal(i32),
    /// Wait for idle again (e.g. for a spawned command to finish).
    Settle,
    /// Sleep this many milliseconds (real time).
    Sleep(u64),
}

/// A syslog datagram as glibc's syslog(3) would send it.
pub fn msg(pri: i32, prog: &str, pid: Option<u32>, text: &str) -> Step {
    let tag = match pid {
        Some(p) => format!("{prog}[{p}]"),
        None => prog.to_string(),
    };
    Step::Send(format!("<{pri}>Nov 11 14:00:00 {tag}: {text}").into_bytes())
}

pub fn raw(bytes: &[u8]) -> Step {
    Step::Send(bytes.to_vec())
}

#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    pub config: String,
    /// `None`: the default arguments.
    pub args: Option<Vec<String>>,
    pub extra_args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Extra files to create first: (path under T, contents, mode).
    pub files: Vec<(String, Vec<u8>, u32)>,
    pub steps: Vec<Step>,
    pub start_time: (i64, i64),
    /// Wait for T/sock before running the steps.
    pub wait_for_socket: bool,
    /// Send SIGTERM at the end (otherwise wait for metalog to exit).
    pub terminate: bool,
    /// Listen on a UDP port on 127.0.0.1 (`@PORT@` in the config) and
    /// record the datagrams as the pseudo-file `<udp>`.
    pub udp: bool,
    /// metalog daemonizes (-B): follow the pid file to the real process.
    pub daemon: bool,
}

impl Case {
    pub fn new(name: &str, config: &str) -> Case {
        Case {
            name: name.to_string(),
            config: config.to_string(),
            args: None,
            extra_args: Vec::new(),
            env: vec![("TZ".into(), "UTC".into())],
            files: Vec::new(),
            steps: Vec::new(),
            start_time: DEFAULT_TIME,
            wait_for_socket: true,
            terminate: true,
            udp: false,
            daemon: false,
        }
    }

    pub fn daemon(mut self) -> Case {
        self.daemon = true;
        self
    }

    pub fn udp(mut self) -> Case {
        self.udp = true;
        self
    }

    pub fn steps(mut self, steps: impl IntoIterator<Item = Step>) -> Case {
        self.steps.extend(steps);
        self
    }

    pub fn args(mut self, args: &[&str]) -> Case {
        self.args = Some(args.iter().map(|s| s.to_string()).collect());
        self
    }

    pub fn extra_args(mut self, args: &[&str]) -> Case {
        self.extra_args.extend(args.iter().map(|s| s.to_string()));
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Case {
        self.env.retain(|(kk, _)| kk != k);
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn file(mut self, path: &str, contents: &[u8], mode: u32) -> Case {
        self.files.push((path.into(), contents.to_vec(), mode));
        self
    }

    /// One-shot run (`-t`, `-V`, errors): no socket wait, no SIGTERM.
    pub fn oneshot(mut self) -> Case {
        self.wait_for_socket = false;
        self.terminate = false;
        self
    }

    pub fn time(mut self, sec: i64, nsec: i64) -> Case {
        self.start_time = (sec, nsec);
        self
    }
}

/// What a run left behind, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    pub files: BTreeMap<String, String>,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn reference_binary() -> PathBuf {
    match std::env::var_os("METALOG_REFERENCE") {
        Some(p) => PathBuf::from(p),
        None => manifest_dir().join("../metalog/src/metalog"),
    }
}

pub fn rust_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_metalog"))
}

fn shim() -> &'static Path {
    static SHIM: OnceLock<PathBuf> = OnceLock::new();
    SHIM.get_or_init(|| {
        let dir = manifest_dir().join("target/difftest");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("faketime.so");
        let tmp = dir.join(format!("faketime.so.{}", std::process::id()));
        let src = manifest_dir().join("tests/shim/faketime.c");
        let st = Command::new("cc")
            .args(["-shared", "-fPIC", "-O2", "-o"])
            .arg(&tmp)
            .arg(&src)
            .arg("-ldl")
            .status()
            .expect("run cc");
        assert!(st.success(), "building the faketime shim failed");
        std::fs::rename(&tmp, &out).unwrap();
        out
    })
}

fn subst(s: &str, t: &Path) -> String {
    s.replace("@T@", t.to_str().unwrap())
}

fn write_time(t: &Path, (s, ns): (i64, i64)) {
    let tmp = t.join(".faketime.tmp");
    std::fs::write(&tmp, format!("{s} {ns}\n")).unwrap();
    std::fs::rename(&tmp, t.join(".faketime")).unwrap();
}

/// Enter a new user namespace mapping our uid/gid to root.  Runs between
/// fork and exec, so it only makes raw syscalls on prepared buffers.
fn userns_pre_exec(cmd: &mut Command) {
    // SAFETY: getuid/getgid are always safe.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let files: Vec<(CString, Vec<u8>)> = vec![
        (CString::new("/proc/self/setgroups").unwrap(), b"deny".to_vec()),
        (CString::new("/proc/self/uid_map").unwrap(), format!("0 {uid} 1").into_bytes()),
        (CString::new("/proc/self/gid_map").unwrap(), format!("0 {gid} 1").into_bytes()),
    ];
    // SAFETY: the closure only calls async-signal-safe functions on data
    // prepared before the fork.
    unsafe {
        cmd.pre_exec(move || {
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for (path, data) in &files {
                let fd = libc::open(path.as_ptr(), libc::O_WRONLY);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let n = libc::write(fd, data.as_ptr().cast(), data.len());
                libc::close(fd);
                if n < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// Bytes still queued from `sock` to the receiver (SIOCOUTQ).
fn outq(sock: &UnixDatagram) -> i32 {
    use std::os::fd::AsRawFd;
    let mut n: libc::c_int = 0;
    // SAFETY: TIOCOUTQ (== SIOCOUTQ) writes one int.
    unsafe { libc::ioctl(sock.as_raw_fd(), libc::TIOCOUTQ, &mut n) };
    n
}

/// metalog is blocked in poll() with no children left.
fn idle_once(pid: u32) -> bool {
    let Ok(sc) = std::fs::read_to_string(format!("/proc/{pid}/syscall")) else {
        return true; // gone
    };
    let nr = sc.split_whitespace().next().unwrap_or("");
    let in_poll = nr == "7" || nr == "271";
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    in_poll && children.trim().is_empty()
}

/// The metalog under test: our child, or (with -B) the daemon it forked.
struct Proc {
    child: Child,
    pid: u32,
    daemon: bool,
}

impl Proc {
    fn alive(&mut self) -> bool {
        if self.daemon {
            // Not our child; gone (or a zombie of its new parent) once it exits.
            let st = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).unwrap_or_default();
            !st.is_empty() && !st.contains(") Z ")
        } else {
            matches!(self.child.try_wait(), Ok(None))
        }
    }

    fn kill(&mut self) {
        // SAFETY: signaling the process under test.
        unsafe { libc::kill(self.pid as i32, libc::SIGKILL) };
        let _ = self.child.wait();
    }

    /// Wait for exit; the status as text ("daemon" for -B runs, whose
    /// status nobody here can see).
    fn wait_exit(&mut self, what: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.alive() {
            if Instant::now() > deadline {
                self.kill();
                panic!("{what}: metalog did not exit");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.daemon {
            return "daemon gone".into();
        }
        let st = self.child.wait().unwrap();
        match (st.code(), st.signal()) {
            (Some(c), _) => format!("exit {c}"),
            (None, Some(s)) => format!("signal {s}"),
            _ => "unknown".into(),
        }
    }
}

fn wait_idle(p: &mut Proc, sock: Option<&UnixDatagram>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stable = 0;
    while Instant::now() < deadline {
        if !p.alive() {
            return;
        }
        let queued = sock.map_or(0, outq);
        if queued == 0 && idle_once(p.pid) {
            stable += 1;
            if stable >= 3 {
                return;
            }
        } else {
            stable = 0;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = p.pid;
    p.kill();
    panic!("metalog (pid {pid}) never went idle");
}

/// `/proc/PID/cmdline` (the process title), trailing NULs dropped since
/// their number depends on the length of the environment.
fn cmdline(pid: u32) -> String {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    format!("{:?}", String::from_utf8_lossy(&raw[..end]))
}

fn snapshot(t: &Path, pid: u32) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let skip = ["metalog.conf", ".faketime", ".faketime.tmp", "stdout", "stderr"];
    fn walk(base: &Path, dir: &Path, skip: &[&str], pid: u32, out: &mut BTreeMap<String, String>) {
        let mut ents: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            if dir == base && skip.contains(&rel.as_str()) {
                continue;
            }
            let md = std::fs::symlink_metadata(&p).unwrap();
            use std::os::unix::fs::{FileTypeExt, PermissionsExt};
            let mode = md.permissions().mode() & 0o7777;
            let ft = md.file_type();
            let desc = if ft.is_dir() {
                walk(base, &p, skip, pid, out);
                format!("dir {mode:o}")
            } else if ft.is_socket() {
                format!("socket {mode:o}")
            } else if ft.is_symlink() {
                format!("symlink -> {}", std::fs::read_link(&p).unwrap().display())
            } else {
                let Ok(data) = std::fs::read(&p) else {
                    out.insert(normalize(&rel, base, pid), format!("file {mode:o} (unreadable)"));
                    continue;
                };
                let text = if p.extension().is_some_and(|x| x == "gz") {
                    format!("gzip {} bytes: {:02x?}", data.len(), data)
                } else {
                    normalize(&String::from_utf8_lossy(&data), base, pid)
                };
                format!("file {mode:o}\n{text}")
            };
            out.insert(normalize(&rel, base, pid), desc);
        }
    }
    walk(t, t, &skip, pid, &mut out);
    out
}

/// Replace `T` with `@T@`, the master's pid with `@PID@`, and other pids
/// in metalog's own messages with `@CPID@`.
pub fn normalize(s: &str, t: &Path, pid: u32) -> String {
    let s = s.replace(t.to_str().unwrap(), "@T@");
    let mut out = String::new();
    let pid_s = pid.to_string();
    // Whole-number tokens equal to the pid.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let j = i + bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
            let tok = &s[i..j];
            let after_ok = j == bytes.len() || !bytes[j].is_ascii_alphanumeric();
            if after_ok && tok == pid_s {
                out.push_str("@PID@");
            } else {
                out.push_str(tok);
            }
            i = j;
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    // Heap pointers (%p) differ from run to run.
    let mut res = String::new();
    let mut rest = out.as_str();
    while let Some(k) = rest.find("0x") {
        res.push_str(&rest[..k]);
        let n = rest[k + 2..].bytes().take_while(u8::is_ascii_hexdigit).count();
        res.push_str(if n >= 6 { "0xPTR" } else { &rest[k..k + 2 + n] });
        rest = &rest[k + 2 + n..];
    }
    res.push_str(rest);
    out = res;
    // Pids of spawned children.
    for prefix in ["Child [", "Klog child [", "\" ["] {
        let mut res = String::new();
        let mut rest = out.as_str();
        while let Some(k) = rest.find(prefix) {
            res.push_str(&rest[..k + prefix.len()]);
            rest = &rest[k + prefix.len()..];
            let n = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
            if n > 0 && rest[n..].starts_with(']') {
                res.push_str("@CPID@");
                rest = &rest[n..];
            }
        }
        res.push_str(rest);
        out = res;
    }
    out
}

pub fn run(case: &Case, bin: &Path, label: &str) -> Outcome {
    let t = manifest_dir().join("target/difftest").join(&case.name).join(label);
    let _ = std::fs::remove_dir_all(&t);
    std::fs::create_dir_all(&t).unwrap();
    let udp = case.udp.then(|| {
        let u = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        u.set_nonblocking(true).unwrap();
        u
    });
    let mut config = subst(&case.config, &t);
    if let Some(u) = &udp {
        config = config.replace("@PORT@", &u.local_addr().unwrap().port().to_string());
    }
    std::fs::write(t.join("metalog.conf"), config).unwrap();
    for (path, data, mode) in &case.files {
        let p = t.join(subst(path, &t));
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let data = subst(&String::from_utf8_lossy(data), &t);
        std::fs::write(&p, data).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(*mode)).unwrap();
    }
    write_time(&t, case.start_time);

    let args: Vec<String> = match &case.args {
        Some(a) => a.iter().map(|a| subst(a, &t)).collect(),
        None => ["-N", "-S", "@T@/sock", "-C", "@T@/metalog.conf", "-p", "@T@/metalog.pid"]
            .iter()
            .map(|a| subst(a, &t))
            .collect(),
    };
    let mut cmd = Command::new(bin);
    cmd.arg0("metalog")
        .args(args)
        .args(case.extra_args.iter().map(|a| subst(a, &t)))
        .current_dir(&t)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LD_PRELOAD", shim())
        .env("METALOG_FAKETIME", t.join(".faketime"))
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(t.join("stdout")).unwrap())
        .stderr(std::fs::File::create(t.join("stderr")).unwrap());
    for (k, v) in &case.env {
        cmd.env(k, subst(v, &t));
    }
    userns_pre_exec(&mut cmd);
    let child = cmd.spawn().expect("spawn metalog");
    let mut p = Proc { pid: child.id(), child, daemon: false };
    let mut parent_status = String::new();
    if case.daemon {
        // The parent exits once daemon() forks; the pid file names the daemon.
        parent_status = p.wait_exit(&case.name);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(s) = std::fs::read_to_string(t.join("metalog.pid"))
                && let Ok(n) = s.trim().parse()
            {
                p.pid = n;
                p.daemon = true;
                break;
            }
            assert!(Instant::now() < deadline, "{}: {label}: no pid file", case.name);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let sock_path = t.join("sock");
    let mut title = None;
    if case.wait_for_socket {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !sock_path.exists() && p.alive() {
            if Instant::now() > deadline {
                p.kill();
                panic!("{}: {label}: socket never appeared", case.name);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if sock_path.exists() {
            wait_idle(&mut p, None);
            title = Some(cmdline(p.pid));
        }
    }

    let sender = UnixDatagram::unbound().unwrap();
    for step in &case.steps {
        if !p.alive() {
            break;
        }
        match step {
            Step::Send(data) => {
                let _ = sender.send_to(data, &sock_path);
                wait_idle(&mut p, Some(&sender));
            }
            Step::SendTo(path, data) => {
                let _ = sender.send_to(data, t.join(path));
                wait_idle(&mut p, Some(&sender));
            }
            Step::Time(s, ns) => write_time(&t, (*s, *ns)),
            Step::Signal(sig) => {
                // SAFETY: signaling the process under test.
                unsafe { libc::kill(p.pid as i32, *sig) };
                std::thread::sleep(Duration::from_millis(20));
                wait_idle(&mut p, None);
            }
            Step::Settle => {
                std::thread::sleep(Duration::from_millis(20));
                wait_idle(&mut p, None);
            }
            Step::Sleep(ms) => std::thread::sleep(Duration::from_millis(*ms)),
        }
    }

    if case.terminate && p.alive() {
        // SAFETY: signaling the process under test.
        unsafe { libc::kill(p.pid as i32, libc::SIGTERM) };
    }
    let mut status = p.wait_exit(&format!("{}: {label}", case.name));
    if case.daemon {
        status = format!("parent {parent_status}, {status}");
    }
    // Children (the klog child, commands) may outlive the master briefly.
    std::thread::sleep(Duration::from_millis(30));
    let pid = p.pid;

    let rd = |n: &str| normalize(&String::from_utf8_lossy(&std::fs::read(t.join(n)).unwrap()), &t, pid);
    let mut files = snapshot(&t, pid);
    if let Some(u) = &udp {
        let mut got = String::new();
        let mut buf = [0u8; 65536];
        while let Ok(n) = u.recv(&mut buf) {
            got += &format!("{:?}\n", normalize(&String::from_utf8_lossy(&buf[..n]), &t, pid));
        }
        files.insert("<udp>".into(), got);
    }
    if let Some(title) = title {
        files.insert("<cmdline>".into(), normalize(&title, &t, pid));
    }
    Outcome { status, stdout: rd("stdout"), stderr: rd("stderr"), files }
}

/// Run a case against both implementations and assert identical outcomes.
pub fn check(case: Case) -> Outcome {
    let reference = reference_binary();
    assert!(
        reference.exists(),
        "reference metalog not found at {} (run tools/build-reference.sh or set METALOG_REFERENCE)",
        reference.display()
    );
    let c = run(&case, &reference, "c");
    let r = run(&case, &rust_binary(), "rust");
    if c != r {
        let mut msg = format!("case {}: outcomes differ\n", case.name);
        if c.status != r.status {
            msg += &format!("status: C {:?}, Rust {:?}\n", c.status, r.status);
        }
        if c.stdout != r.stdout {
            msg += &format!("stdout:\n--- C\n{}\n--- Rust\n{}\n", c.stdout, r.stdout);
        }
        if c.stderr != r.stderr {
            msg += &format!("stderr:\n--- C\n{}\n--- Rust\n{}\n", c.stderr, r.stderr);
        }
        for k in c.files.keys().chain(r.files.keys()).collect::<std::collections::BTreeSet<_>>() {
            let (a, b) = (c.files.get(k), r.files.get(k));
            if a != b {
                msg += &format!(
                    "file {k}:\n--- C\n{}\n--- Rust\n{}\n",
                    a.map_or("(missing)", |s| s),
                    b.map_or("(missing)", |s| s)
                );
            }
        }
        panic!("{msg}");
    }
    if std::env::var_os("METALOG_DIFF_SHOW").is_some() {
        eprintln!("== {} ==\n{c:#?}", case.name);
    }
    c
}

/// Paths as bytes for the snapshot (non-UTF-8 names are lossily shown).
pub fn path_bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}
