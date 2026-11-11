//! The kernel log path: the klog child, klogctl(), --consolelevel.  C vs
//! Rust.  These need the real root (klogctl() is refused in a user
//! namespace) and change kernel log state, so they only run in the test VM
//! and skip elsewhere.  They run one at a time: the kernel log is global.

mod common;
use std::sync::Mutex;

use common::{Case, Step, check, check_racy};

static KERNEL_LOG: Mutex<()> = Mutex::new(());

fn klog_with(case: Case, attempts: usize) {
    if !common::real_root_available() {
        eprintln!("skipping {}: needs the real root (run in the test VM)", case.name);
        return;
    }
    let _guard = KERNEL_LOG.lock().unwrap_or_else(|e| e.into_inner());
    let case = case.real_root().args(&["-S", "@T@/sock", "-C", "@T@/metalog.conf", "-p", "@T@/metalog.pid"]);
    if attempts > 1 {
        check_racy(case, attempts);
    } else {
        check(case);
    }
}

fn klog(case: Case) {
    klog_with(case, 1);
}

const CONF: &str = r#"
Kernel:
  facility = "kern"
  logdir = "@T@/kern"
  break = 1
User:
  facility = "user"
  logdir = "@T@/user"
Critical:
  minimum = 2
  logdir = "@T@/crit"
"#;

fn kmsg(s: &[u8]) -> Step {
    Step::Kmsg(s.to_vec())
}

#[test]
fn kernel_lines() {
    klog(Case::new("klog_basic", CONF).steps([
        kmsg(b"<6>info from userspace"),
        kmsg(b"<0>emerg from userspace"),
        kmsg(b"no prefix at all"),
        kmsg(b"<3>tabs\tand\x01control"),
        kmsg(b"<6>trailing newline\n"),
        kmsg("<6>utf-8 caf\u{e9} \u{2603}".as_bytes()),
        kmsg(b"<6>repeat"),
        kmsg(b"<6>repeat"),
        kmsg(b"<6>repeat"),
        kmsg(b"<6>repeat"),
        kmsg(&[b"<6>".as_slice(), &[b'k'; 1000]].concat()),
        // Real kernel-facility lines: the SysRq help text.
        Step::Sysrq(b'h'),
    ]));
}

#[test]
fn kernel_formats() {
    for fmt in ["rfc3164", "rfc5424", "legacy"] {
        let conf = format!("All:\n  log_format = \"{fmt}\"\n  log_severity = 1\n  logdir = \"@T@/all\"\n");
        klog(Case::new(&format!("klog_format_{fmt}"), &conf).steps([kmsg(b"<5>notice"), Step::Sysrq(b'h')]));
    }
}

#[test]
fn backlog() {
    // Records waiting before metalog starts are read in chunks by the klog
    // child.  Over 4 kB of them: the master's read ends mid-line, and the
    // wrong-offset memmove (QUIRKS.md) loses lines.  Where that happens
    // depends on how the child's writes coalesce, hence the retries.
    let small: Vec<Vec<u8>> = (0..10).map(|i| format!("<6>backlog line {i}").into_bytes()).collect();
    klog(Case::new("klog_backlog_small", CONF).backlog(small));
    let big: Vec<Vec<u8>> =
        (0..40).map(|i| format!("<6>big backlog {i:03} {}", "b".repeat(150)).into_bytes()).collect();
    klog_with(Case::new("klog_backlog_big", CONF).backlog(big), 3);
}

#[test]
fn console_level_and_exit() {
    // --consolelevel, and what the console level is after metalog stops.
    klog(Case::new("klog_consolelevel", CONF).extra_args(&["-c", "3"]).steps([kmsg(b"<6>x")]));
    klog(Case::new("klog_default_level", CONF).steps([kmsg(b"<6>x")]));
    // SIGHUP and SIGINT end both processes too.
    klog(Case::new("klog_sighup", CONF).steps([kmsg(b"<6>x"), Step::Signal(libc::SIGHUP)]));
    // The klog child dying takes the master down.
    klog(Case::new("klog_child_killed", CONF).steps([kmsg(b"<6>before"), Step::KillKlog]));
    // SIGUSR1/2 are ignored by the klog child, handled by the master.
    klog(Case::new("klog_usr", CONF).steps([
        Step::Signal(libc::SIGUSR2),
        kmsg(b"<6>buffered"),
        Step::Signal(libc::SIGUSR1),
        kmsg(b"<6>synced"),
    ]));
}
