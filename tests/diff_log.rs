//! The logging path: parsing, matching, formats, duplicates, timestamps,
//! commands and signals.  C vs Rust.

mod common;
use common::{Case, Step, check, msg, raw};

const ALL: &str = "All:\n  facility = \"*\"\n  logdir = \"@T@/all\"\n";

#[test]
fn basic_and_duplicates() {
    check(Case::new("basic", ALL).steps([
        msg(13, "myprog", Some(42), "hello world"),
        msg(13, "myprog", None, "hello world"),
        msg(13, "myprog", None, "hello world"),
        msg(13, "myprog", None, "hello world"),
        msg(13, "other", None, "bye"),
    ]));
    // The duplicate tracker's growth quirk: a shorter line matching the
    // start of a longer one counts as a repeat.
    check(Case::new("dup_prefix", ALL).steps([
        msg(13, "p", None, "bb"),
        msg(13, "p", None, "bb"),
        msg(13, "p", None, "cccccc"),
        msg(13, "p", None, "cc"),
        msg(13, "p", None, "cc"),
        msg(13, "p", None, "x"),
        msg(13, "q", None, "x"),
        msg(13, "q", None, "x"),
        msg(13, "q", None, "x"),
        msg(13, "q", None, "x"),
    ]));
    check(Case::new("showrepeats", "All:\n  showrepeats = 1\n  logdir = \"@T@/all\"\n").steps([
        msg(13, "p", None, "same"),
        msg(13, "p", None, "same"),
        msg(13, "p", None, "same"),
    ]));
    let long = "y".repeat(600);
    check(Case::new("dup_long", ALL).steps([
        msg(13, "p", None, &long),
        msg(13, "p", None, &long),
        msg(13, "p", None, &(long.clone() + "different tail beyond 512")),
        msg(13, "p", None, &long),
    ]));
}

#[test]
fn odd_lines() {
    check(Case::new("odd_lines", ALL).steps([
        raw(b"<13>no timestamp: first word eaten"),
        raw(b"<13>p: hello there: world"),
        raw(b"<0>Nov 11 14:00:00 p: emerg is dropped"),
        raw(b"<-5>Nov 11 14:00:00 p: negative"),
        raw(b"13>Nov 11 14:00:00 p: no bracket"),
        raw(b"<13 Nov 11 14:00:00 p: no close"),
        raw(b"<13>Nov 11 14:00:00 p[12 unterminated pid"),
        raw(b"<13>Nov 11 14:00:00 p[12] junk: after pid"),
        raw(b"<13>Nov 11 14:00:00 : empty program"),
        raw(b"<13>Nov 11 14:00:00 p:    "),
        raw(b"<13>Nov 11 14:00:00 tabs\tand\x01ctrl: a\tb\x7fc\n"),
        raw(b"<13>Nov 11 14:00:00 p: two newlines\n\n"),
        raw(b"<13>Nov 11 14:00:00 p: nul\0hidden"),
        raw("<13>Nov 11 14:00:00 p: caf\u{e9} \u{2603} \u{80}".as_bytes()),
        raw(b"<13>Nov 11 14:00:00 p: c1 \x85 \x9f bytes \xff"),
        raw(b"< 14 >Nov 11 14:00:00 p: spaces in pri"),
        raw(b"<99999>Nov 11 14:00:00 p: huge pri"),
        raw(b""),
        Step::Send([b"<13>Nov 11 14:00:00 big: ".as_slice(), &[b'z'; 5000]].concat()),
    ]));
}

#[test]
fn formats() {
    for (i, fmt) in ["legacy", "legacy_timestamp", "rfc3164", "rfc5424", "bogus"].iter().enumerate() {
        for sev in [0, 1] {
            let conf = format!("All:\n  log_format = \"{fmt}\"\n  log_severity = {sev}\n  logdir = \"@T@/all\"\n");
            check(Case::new(&format!("format_{i}_{sev}"), &conf).steps([
                msg(13, "prog", Some(7), "with pid"),
                msg(11, "prog", None, "no pid"),
                msg(6, "kernel", None, "kernel-ish"),
                msg(191, "local7", None, "debug"),
            ]));
        }
    }
}

#[test]
fn stamps() {
    let stamps = [
        "%Y-%m-%d %H:%M:%S.%N",
        "%s.%3N",
        "%6N|%0N|%12N|%1N|%%N|%%%N",
        "%Y-%m-%dT%H:%M:%S%:z",
        "%%:z %:z %:z",
        "",
        "%b %e %T",
        "%99999999999999999999999N",
        &"x".repeat(150),
    ];
    for (i, s) in stamps.iter().enumerate() {
        for (j, tz) in ["UTC", "NST+3:30", "IST-5:30", "EST5EDT"].iter().enumerate() {
            let conf = format!("All:\n  stamp_fmt = \"{s}\"\n  logdir = \"@T@/all\"\n");
            check(Case::new(&format!("stamp_{i}_{j}"), &conf).env("TZ", tz).steps([
                msg(13, "p", None, "one"),
                Step::Time(1_762_869_601, 5_000),
                msg(13, "p", None, "two"),
            ]));
        }
    }
    // rfc formats override stamp_fmt.
    check(
        Case::new(
            "stamp_rfc_override",
            "All:\n  stamp_fmt = \"IGNORED\"\n  log_format = \"rfc5424\"\n  logdir = \"@T@/all\"\n",
        )
        .env("TZ", "NST+3:30")
        .steps([msg(13, "p", Some(1), "x")]),
    );
}

#[test]
fn matching() {
    let conf = r#"
# defaults before the first block apply to all blocks
maxsize = 5000
Kern:
  facility = "kern"
  logdir = "@T@/kern"
  break = 1
Mail and cron:
  facility = "mail"
  facility = "CRON"
  logdir = "@T@/mailcron"
Important:
  minimum = 3
  logdir = "@T@/important"
Outer range:
  minimum = 2
  maximum = 6
  logdir = "@T@/outer"
Program:
  program = "SSHD"
  logdir = "@T@/sshd"
Program regex:
  program_regex = "^cr"
  program_neg_regex = "cron$"
  logdir = "@T@/progre"
Regexes:
  regex = "(password|login)\s+fail"
  regex = "ILLEGAL"
  neg_regex = "test"
  logdir = "@T@/pw"
Only neg:
  neg_regex = "^skip"
  logdir = "@T@/notskip"
  break = 1
Everything else:
  logdir = "@T@/rest"
Shared dir:
  facility = "user"
  logdir = "@T@/rest"
None:
  logdir = "NONE"
"#;
    check(Case::new("matching", conf).steps([
        msg(6, "kernel", None, "kern info"),
        msg(2 * 8 + 6, "postfix", None, "mail"),
        msg(9 * 8 + 6, "crond", None, "cron info"),
        msg(9 * 8 + 6, "cron", None, "cron exact"),
        msg(13, "u", None, "notice"),
        msg(11, "u", None, "err"),
        msg(9, "u", None, "alert"),
        msg(14, "u", None, "info"),
        msg(15, "u", None, "debug"),
        msg(13, "sshd", Some(5), "ssh"),
        msg(13, "crontab", None, "crontab"),
        msg(13, "login", None, "Password  FAIL for x"),
        msg(13, "login", None, "password fail test"),
        msg(13, "login", None, "illegal root login"),
        msg(13, "u", None, "skip me"),
    ]));
}

#[test]
fn sources() {
    let conf =
        "Extra:\n  socket = \"@T@/extra.sock\"\n  logdir = \"@T@/extra\"\n  break = 1\nAll:\n  logdir = \"@T@/all\"\n";
    check(Case::new("sources", conf).steps([
        msg(13, "main", None, "on the main socket"),
        Step::SendTo("extra.sock".into(), b"<13>Nov 11 14:00:00 extra: on the extra socket".to_vec()),
    ]));
    // A config socket that can't be bound is skipped.
    check(Case::new("source_unbindable", "Extra:\n  socket = \"@T@/missing/dir/s\"\n  logdir = \"@T@/x\"\n"));
}

#[test]
fn commands() {
    let script = b"#!/bin/sh\nprintf '%s|' \"$@\" >> @T@/cmd.out\necho >> @T@/cmd.out\n";
    let conf = "Cmd:\n  facility = \"*\"\n  command = \"@T@/cmd.sh\"\n  logdir = \"@T@/all\"\n";
    check(Case::new("command", conf).file("cmd.sh", script, 0o755).steps([
        msg(13, "p", Some(3), "run me"),
        Step::Settle,
        msg(13, "q", None, "and me"),
        Step::Settle,
    ]));
    // Without a logdir the date argument is empty.
    let conf = "Cmd:\n  command = \"@T@/cmd.sh\"\n";
    check(
        Case::new("command_nolog", conf).file("cmd.sh", script, 0o755).steps([msg(13, "p", None, "x"), Step::Settle]),
    );
    // Arguments make the exec fail (exit 127), which metalog then logs.
    let conf = "Cmd:\n  command = \"@T@/cmd.sh --arg\"\n  logdir = \"@T@/all\"\n";
    check(Case::new("command_args", conf).file("cmd.sh", script, 0o755).steps([msg(13, "p", None, "x"), Step::Settle]));
    let fail = b"#!/bin/sh\nexit 3\n";
    let conf = "Cmd:\n  command = \"@T@/fail.sh\"\n  logdir = \"@T@/all\"\n";
    check(Case::new("command_fails", conf).file("fail.sh", fail, 0o755).steps([msg(13, "p", None, "x"), Step::Settle]));
    let killed = b"#!/bin/sh\nkill -9 $$\n";
    let conf = "Cmd:\n  command = \"@T@/k.sh\"\n  logdir = \"@T@/all\"\n";
    check(Case::new("command_killed", conf).file("k.sh", killed, 0o755).steps([msg(13, "p", None, "x"), Step::Settle]));
    // Verbose: the fork is logged, and so are successful exits.
    let conf = "Cmd:\n  command = \"@T@/cmd.sh\"\n  logdir = \"@T@/all\"\n";
    check(Case::new("command_verbose", conf).extra_args(&["-v"]).file("cmd.sh", script, 0o755).steps([
        msg(13, "p", None, "x"),
        Step::Settle,
        msg(13, "p", None, "y"),
        Step::Settle,
    ]));
}

#[test]
fn signals() {
    let conf = "All:\n  logdir = \"@T@/all\"\n";
    // SIGUSR2: buffered (nothing reaches the file); SIGUSR1: flushed.
    check(Case::new("sig_usr", conf).steps([
        msg(13, "p", None, "sync"),
        Step::Signal(libc::SIGUSR2),
        msg(13, "p", None, "buffered 1"),
        msg(13, "p", None, "buffered 2"),
        Step::Signal(libc::SIGUSR1),
        msg(13, "p", None, "sync again"),
    ]));
    // -a starts asynchronous; SIGTERM flushes the files (not stdout).
    check(
        Case::new("sig_async_term", conf).extra_args(&["-a"]).steps([msg(13, "p", None, "a"), msg(13, "p", None, "b")]),
    );
    let conf_flush = "All:\n  flush = 1\n  logdir = \"@T@/all\"\nNever:\n  flush = 0\n  logdir = \"@T@/never\"\n";
    check(Case::new("flush_modes", conf_flush).extra_args(&["-a"]).steps([msg(13, "p", None, "a")]));
    for (name, sig) in
        [("sig_hup", libc::SIGHUP), ("sig_int", libc::SIGINT), ("sig_quit", libc::SIGQUIT), ("sig_pipe", libc::SIGPIPE)]
    {
        check(Case::new(name, conf).steps([msg(13, "p", None, "x"), Step::Signal(sig)]));
    }
}

#[test]
fn stdout_buffer() {
    // Enough output to overflow the 4 kB stdout buffer, so some of it
    // reaches the file before SIGTERM throws the rest away.
    let conf = "All:\n  showrepeats = 1\n  logdir = \"NONE\"\n";
    let steps: Vec<Step> = (0..60).map(|i| msg(13, "p", None, &format!("line {i} {}", "x".repeat(90)))).collect();
    check(Case::new("stdout_overflow", conf).steps(steps));
}

#[test]
fn startup_failures() {
    // The kernel source can't be opened in a user namespace: the klog
    // child fails, returns into main(), and both processes exit.
    check(
        Case::new("klog_child_fails", ALL)
            .args(&["-S", "@T@/sock", "-C", "@T@/metalog.conf", "-p", "@T@/metalog.pid"])
            .oneshot(),
    );
    check(Case::new("bad_config_run", "x:\n  facility = \"nope\"\n").oneshot());
    check(
        Case::new("socket_unbindable", ALL).args(&["-N", "-S", "@T@/no/such/dir", "-C", "@T@/metalog.conf"]).oneshot(),
    );
    check(
        Case::new("pidfile_unwritable", ALL)
            .args(&["-N", "-S", "@T@/sock", "-C", "@T@/metalog.conf", "-p", "@T@/no/dir/pid"])
            .oneshot(),
    );
    // A relative pid file is never written (and the socket comes up).
    check(Case::new("pidfile_relative", ALL).args(&[
        "-N",
        "-S",
        "@T@/sock",
        "-C",
        "@T@/metalog.conf",
        "-p",
        "rel.pid",
    ]));
    check(Case::new("group_unknown", ALL).extra_args(&["-g", "no-such-group-xyz"]).oneshot());
}

#[test]
fn logdir_problems() {
    // logdir is a regular file: it gets replaced by a directory.
    check(Case::new("logdir_is_file", ALL).file("all", b"in the way\n", 0o644).steps([msg(13, "p", None, "x")]));
    check(Case::new("logdir_parent_missing", "All:\n  logdir = \"@T@/a/b/c\"\n").steps([msg(13, "p", None, "x")]));
    check(Case::new("logdir_perms", "All:\n  perms = 0751\n  logdir = \"@T@/all\"\n").steps([msg(13, "p", None, "x")]));
    // An existing .timestamp is honored; a corrupt one is rewritten.
    check(
        Case::new("timestamp_existing", ALL)
            .file("all/.timestamp", b"  1762800000 trailing\n", 0o644)
            .steps([msg(13, "p", None, "x")]),
    );
    check(
        Case::new("timestamp_corrupt", ALL)
            .file("all/.timestamp", b"garbage\n", 0o644)
            .steps([msg(13, "p", None, "x")]),
    );
    check(
        Case::new("current_appended", ALL).file("all/current", b"old line\n", 0o640).steps([msg(13, "p", None, "x")]),
    );
}

#[test]
fn daemon_mode() {
    // As Gentoo's init script starts it: --pidfile=... --daemonize.
    check(
        Case::new("daemonize", ALL)
            .args(&["-N", "-S", "@T@/sock", "-C", "@T@/metalog.conf", "--pidfile=@T@/metalog.pid", "--daemonize"])
            .daemon()
            .steps([
                msg(13, "p", None, "from the daemon"),
                Step::Signal(libc::SIGUSR2),
                msg(13, "p", None, "buffered"),
            ]),
    );
    // -g: setgid to a group that exists in the namespace.
    check(Case::new("group_root", ALL).extra_args(&["-g", "root"]).steps([msg(13, "p", None, "x")]));
}
