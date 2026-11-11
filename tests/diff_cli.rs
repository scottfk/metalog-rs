//! Command line and `-t` (config checking): C vs Rust.

mod common;
use common::{Case, check};

const GOOD: &str = "All:\n  facility = \"*\"\n  logdir = \"@T@/all\"\n";

fn t(name: &str, config: &str) -> Case {
    Case::new(name, config).args(&["-t", "-C", "@T@/metalog.conf"]).oneshot()
}

#[test]
fn version_and_help() {
    check(Case::new("version", GOOD).args(&["-V"]).oneshot());
    check(Case::new("version_long", GOOD).args(&["--version"]).oneshot());
    check(Case::new("help", GOOD).args(&["-h"]).oneshot());
}

#[test]
fn bad_options() {
    for (name, args) in [
        ("opt_unknown", &["-Q"][..]),
        ("opt_missing_arg", &["-C"][..]),
        ("opt_long_unknown", &["--bogus"][..]),
        ("opt_long_missing", &["--configfile"][..]),
        ("opt_long_ambiguous", &["--co=x"][..]),
        ("opt_long_noarg", &["--sync=1"][..]),
        ("opt_consolelevel_bad", &["-c", "9"][..]),
        ("opt_consolelevel_neg", &["--consolelevel=-1"][..]),
    ] {
        check(Case::new(name, GOOD).args(args).oneshot());
    }
}

#[test]
fn test_config_results() {
    check(t("t_good", GOOD));
    check(Case::new("t_missing", GOOD).args(&["-t", "-C", "@T@/nope.conf"]).oneshot());
    check(Case::new("t_abbrev", GOOD).args(&["--test", "--config=@T@/metalog.conf", "--sy"]).oneshot());
    check(t("t_unknown_keyword", "x:\n  bogus = 1\n"));
    check(t("t_unknown_facility", "x:\n  facility = \"nope\"\n"));
    check(t("t_facility_case", "x:\n  facility = \"LOCAL3\"\n  facility = \"Mark\"\n"));
    check(t("t_bad_regex", "x:\n  regex = \"(unclosed\"\n"));
    check(t("t_bad_program_regex", "x:\n  program_regex = \"a{2,1}\"\n"));
    check(t("t_command_missing", "x:\n  command = \"/nonexistent/cmd --flag\"\n"));
    check(t("t_log_format_bad", "x:\n  log_format = \"RFC5424\"\n"));
    check(t("t_garbage_line", "x:\n  this is not a statement\n  facility = \"*\"\n"));
    check(t("t_newblock_unquoted", "x:\n  regex = foo:\n  bogus = 1\n"));
    check(t("t_long_line", &format!("x:\n  regex = \"{}\"\n", "a".repeat(3000))));
    check(t("t_nul_in_line", "x:\n  facility = \"kern\"\0garbage\n  bogus = 2\n"));
    check(t("t_no_newline_at_end", "x:\n  bogus = 3"));
    check(t("t_repeated_configdir", "configdir = \"@T@\"\nconfigdir = \"@T@/b\"\n"));
}

#[test]
fn ratelimit_parsing() {
    for (i, v) in
        ["10/s", "2.5/m", "0", "0/x", "1/y", "abc", "-1/s", "5", "1e-300/s", "nan/s", "inf/s", "0x10/h", " 3/d"]
            .iter()
            .enumerate()
    {
        check(t(&format!("t_ratelimit_{i}"), &format!("x:\n  ratelimit = \"{v}\"\n")));
    }
    check(t("t_burst_bad", "x:\n  ratelimit_burst = \"0\"\n"));
}

#[test]
fn configdir() {
    let base = "configdir = \"@T@/conf.d\"\nMain:\n  facility = \"*\"\n";
    check(
        t("t_configdir_ok", base)
            .file("conf.d/b.conf", b"B:\n  logdir = \"@T@/b\"\n", 0o644)
            .file("conf.d/a.conf", b"A:\n  logdir = \"@T@/a\"\n", 0o644)
            .file("conf.d/ignored.txt", b"bogus = 1\n", 0o644),
    );
    // An error in a.conf is forgotten because b.conf parses.
    check(t("t_configdir_error_forgotten", base).file("conf.d/a.conf", b"A:\n  facility = \"nope\"\n", 0o644).file(
        "conf.d/b.conf",
        b"B:\n  logdir = \"@T@/b\"\n",
        0o644,
    ));
    check(t("t_configdir_error_last", base).file("conf.d/z.conf", b"Z:\n  facility = \"nope\"\n", 0o644));
    check(t("t_configdir_missing", "configdir = \"@T@/nowhere\"\n"));
    check(t("t_configdir_unreadable", base).file("conf.d/a.conf", b"x\n", 0o000));
}
