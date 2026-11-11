//! Rotation, retention, compression and rate limiting.  C vs Rust.

mod common;
use common::{Case, DEFAULT_TIME, Step, check, msg};

fn at(offset: i64) -> Step {
    Step::Time(DEFAULT_TIME.0 + offset, 0)
}

#[test]
fn rotate_by_size() {
    // Tiny maxsize: every line after the first rotates; one rotation per
    // second (the file names have 1 s resolution, so the clock moves on).
    let conf = "All:\n  maxsize = 60\n  maxfiles = 3\n  logdir = \"@T@/all\"\n";
    let mut steps = Vec::new();
    for i in 0..8 {
        steps.push(at(i * 2));
        steps.push(msg(13, "p", None, &format!("message number {i}")));
    }
    check(Case::new("rotate_size", conf).steps(steps));
    // Several rotations within one second overwrite each other.
    let steps: Vec<Step> = (0..5).map(|i| msg(13, "p", None, &format!("same second {i}"))).collect();
    check(Case::new("rotate_same_second", conf).steps(steps));
}

#[test]
fn rotate_by_time() {
    // maxtime aligns the period to a multiple of maxtime since the epoch.
    let conf = "All:\n  maxtime = 3600\n  logdir = \"@T@/all\"\n";
    check(Case::new("rotate_time", conf).steps([
        msg(13, "p", None, "first"),
        at(1800),
        msg(13, "p", None, "same period"),
        at(3601),
        msg(13, "p", None, "next period"),
        at(3 * 3600 + 5),
        msg(13, "p", None, "later"),
    ]));
    // maxsize = 0 or maxtime = 0 turns rotation off entirely.
    for (name, conf) in [
        ("rotate_off_size0", "All:\n  maxsize = 0\n  maxtime = 1\n  logdir = \"@T@/all\"\n"),
        ("rotate_off_time0", "All:\n  maxsize = 10\n  maxtime = 0\n  logdir = \"@T@/all\"\n"),
    ] {
        check(Case::new(name, conf).steps([
            msg(13, "p", None, "a"),
            at(100),
            msg(13, "p", None, "b"),
            msg(13, "p", None, "c"),
        ]));
    }
}

#[test]
fn retention_and_compression() {
    let pre = |c: Case| {
        c.file("all/log-2025-11-01-00:00:00", b"old 1\n", 0o644)
            .file("all/log-2025-11-02-00:00:00.gz", b"not really gzip\n", 0o644)
            .file("all/log-2025-11-03-00:00:00", b"old 3\n", 0o644)
            .file("all/log-2025-11-04-00:00:00junk.xz", b"odd name\n", 0o644)
            .file("all/log-2025-11-05-00:00:00.txt", b"not a log\n", 0o644)
            .file("all/log-2025-11-06-00:00:00", b"", 0o644)
            .file("all/unrelated", b"keep\n", 0o644)
    };
    let steps = || {
        let mut v = Vec::new();
        for i in 0..4 {
            v.push(at(i * 2));
            v.push(msg(13, "p", None, &format!("rotating line {i}")));
        }
        v
    };
    for (name, extra) in [
        ("retain", "maxfiles = 4\n"),
        ("retain_compress", "maxfiles = 6\n  compress = 1\n"),
        ("retain_compress_delay", "maxfiles = 10\n  compress = 1\n  compress_delay = 2\n"),
        ("retain_compress_two", "maxfiles = 10\n  compress = 2\n"),
        ("retain_negative", "maxfiles = -1\n"),
        ("retain_zero", "maxfiles = 0\n  compress = 1\n"),
    ] {
        let conf = format!("All:\n  maxsize = 30\n  {extra}  logdir = \"@T@/all\"\n");
        check(pre(Case::new(name, &conf)).steps(steps()));
    }
}

#[test]
fn postrotate() {
    let script = b"#!/bin/sh\nprintf '%s|' \"$@\" >> @T@/post.out\necho >> @T@/post.out\n";
    let conf = "All:\n  maxsize = 30\n  postrotate_cmd = \"@T@/post.sh\"\n  logdir = \"@T@/all\"\n";
    check(Case::new("postrotate", conf).file("post.sh", script, 0o755).steps([
        msg(13, "p", None, "one one one one"),
        at(2),
        msg(13, "p", None, "two two two two"),
        Step::Settle,
        at(4),
        msg(13, "p", None, "three three three"),
        Step::Settle,
    ]));
    // Verbose: the fork is logged *during* the rotation, re-entering
    // writeLogLine while `current` is closed.  With time-based rotation
    // the inner call rotates again and its empty `current` replaces the
    // outer's rotated file.  (With a maxsize smaller than the "Forked
    // command" line, C loops forever instead: see QUIRKS.md.)
    let conf = "All:\n  maxtime = 10\n  postrotate_cmd = \"@T@/post.sh\"\n  logdir = \"@T@/all\"\n";
    check(Case::new("postrotate_verbose", conf).extra_args(&["-v"]).file("post.sh", script, 0o755).steps([
        msg(13, "p", None, "one"),
        at(11),
        msg(13, "p", None, "two"),
        Step::Settle,
        at(25),
        msg(13, "p", None, "three"),
        Step::Settle,
    ]));
}

#[test]
fn ratelimit() {
    let conf = "All:\n  showrepeats = 1\n  ratelimit = \"2/s\"\n  ratelimit_burst = 3\n  logdir = \"@T@/all\"\n";
    let mut steps = Vec::new();
    for i in 0..6 {
        steps.push(msg(13, "p", None, &format!("burst {i}")));
    }
    steps.push(Step::Time(DEFAULT_TIME.0 + 1, 0));
    for i in 0..4 {
        steps.push(msg(13, "p", None, &format!("after 1s {i}")));
    }
    steps.push(Step::Time(DEFAULT_TIME.0 + 60, 0));
    for i in 0..5 {
        steps.push(msg(13, "p", None, &format!("after 60s {i}")));
    }
    check(Case::new("ratelimit", conf).steps(steps));
    // Set after logdir: still reaches the output.
    let conf = "All:\n  logdir = \"@T@/all\"\n  showrepeats = 1\n  ratelimit = \"1/m\"\n";
    check(Case::new("ratelimit_after_logdir", conf).steps((0..3).map(|i| msg(13, "p", None, &format!("m{i}")))));
}
