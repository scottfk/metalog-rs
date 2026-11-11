//! metalog-rs: a drop-in Rust port of metalog (upstream tag 20260902,
//! "metalog version 4"), the syslogd/klogd replacement.
//!
//! The goal is byte-for-byte compatible behavior with the C program,
//! upstream quirks included (see QUIRKS.md); `make check` diffs the two.
//! Module map, following upstream's single metalog.c:
//!
//! * [`config`]: `configParser()`/`parseLine()` and the data model
//! * [`logline`]: sanitizing, parsing and formatting lines, timestamps
//! * [`process`]: `processLogLine()`, commands, remote hosts, `doLog()`
//! * [`output`]: `writeLogLine()`, rotation, compression, rate limiting
//! * [`daemon`]: signals, data sources, the poll loop, pid file
//! * [`sys`]: every libc/PCRE2/zlib call (the only `unsafe` code)
//! * [`stdio`], [`getopt`], [`num`], [`strtod`], [`bytes`]: glibc
//!   behavior the output depends on

// Copied from openrc-rs and kept in sync there, so unused helpers stay.
#[allow(dead_code)]
mod bytes;
mod config;
mod daemon;
#[allow(dead_code)]
mod getopt;
mod logline;
#[allow(dead_code)]
mod num;
mod output;
mod process;
mod stdio;
mod strtod;
mod sys;

use crate::config::{Config, SourceType, wpcre2_compile};
use crate::daemon::{State, VERBOSE};
use crate::getopt::{Getopt, HasArg, LongOpt};
use crate::stdio::{die, out};

const PACKAGE: &str = "metalog";
const VERSION: &str = "4";
const DEFAULT_SOCK_NAME: &[u8] = b"/dev/log";
const DEFAULT_PID_FILE: &[u8] = b"/var/run/metalog.pid";
const DEFAULT_CONFIG_FILE: &[u8] = b"/etc/metalog.conf";
const DEFAULT_CONSOLE_LEVEL: i32 = 7;
const COMPRESSED_REGEX: &[u8] = b"\\.(?:Z|gz|bz2|xz)$";
const CONFFILE_REGEX: &[u8] = b"\\.conf$";
const GETOPT_OPTIONS: &str = "c:aBC:g:hp:S:stVvN";

const LONG_OPTIONS: &[LongOpt] = &[
    LongOpt::new("async", HasArg::No, b'a'),
    LongOpt::new("daemonize", HasArg::No, b'B'),
    LongOpt::new("consolelevel", HasArg::Required, b'c'),
    LongOpt::new("configfile", HasArg::Required, b'C'),
    LongOpt::new("group", HasArg::Required, b'g'),
    LongOpt::new("help", HasArg::No, b'h'),
    LongOpt::new("no-kernel", HasArg::No, b'N'),
    LongOpt::new("pidfile", HasArg::Required, b'p'),
    LongOpt::new("socket", HasArg::Required, b'S'),
    LongOpt::new("synchronous", HasArg::No, b's'),
    LongOpt::new("sync", HasArg::No, b's'),
    LongOpt::new("test-config", HasArg::No, b't'),
    LongOpt::new("verbose", HasArg::No, b'v'),
    LongOpt::new("version", HasArg::No, b'V'),
];

#[derive(Debug)]
struct Options {
    console_level: i32,
    daemonize: bool,
    test_config: bool,
    do_kernel_log: bool,
    sock_name: Vec<u8>,
    pid_file: Vec<u8>,
    config_file: Vec<u8>,
    group_name: Option<Vec<u8>>,
}

/// `help()`.
fn help() -> ! {
    out(format!("{PACKAGE} version {VERSION}\n\n").as_bytes());
    out(b"Options:\n");
    for o in LONG_OPTIONS {
        let arg = if o.has_arg != HasArg::No { " <opt>" } else { "" };
        out(&bcat!("   -", [o.val as u8], ", --", o.name, arg, "\n"));
    }
    stdio::exit(libc::EXIT_SUCCESS)
}

/// `parseOptions()`.
fn parse_options() -> Options {
    let mut o = Options {
        console_level: DEFAULT_CONSOLE_LEVEL,
        daemonize: false,
        test_config: false,
        do_kernel_log: true,
        sock_name: DEFAULT_SOCK_NAME.to_vec(),
        pid_file: DEFAULT_PID_FILE.to_vec(),
        config_file: DEFAULT_CONFIG_FILE.to_vec(),
        group_name: None,
    };
    let mut g = Getopt::new(sys::args(), GETOPT_OPTIONS);
    while let Some(c) = g.next_long(LONG_OPTIONS) {
        let arg = || g.optarg.clone().unwrap_or_default();
        match u8::try_from(c).unwrap_or(0) {
            b'a' => daemon::SYNCHRONOUS.store(false, std::sync::atomic::Ordering::Relaxed),
            b'B' => o.daemonize = true,
            b'c' => {
                o.console_level = num::atoi(&arg());
                if !(0..=7).contains(&o.console_level) {
                    die(b"Invalid console level");
                }
            }
            b'C' => o.config_file = arg(),
            b'g' => o.group_name = Some(arg()),
            b'v' => {
                VERBOSE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            b'N' => o.do_kernel_log = false,
            b'V' => {
                out(format!("{PACKAGE} version {VERSION}\n").as_bytes());
                stdio::exit(libc::EXIT_SUCCESS);
            }
            b'h' => help(),
            b'p' => o.pid_file = arg(),
            b'S' => o.sock_name = arg(),
            b's' => {}
            b't' => o.test_config = true,
            // getopt_long never returns ':' without a leading ':' in the
            // optstring; '?' covers both unknown options and missing args.
            // QUIRK: for long options optopt is 0, printed as a NUL byte.
            _ => die(&bcat!("Unknown option '", [g.optopt as u8], "' or argument missing")),
        }
    }
    o
}

fn main() {
    // Rust ignores SIGPIPE by default; C programs start with SIG_DFL.
    sys::signal_default(libc::SIGPIPE);

    // compile_regexes()
    let (Some(re_compressed), Some(re_conffile)) =
        (wpcre2_compile(COMPRESSED_REGEX, 0), wpcre2_compile(CONFFILE_REGEX, 0))
    else {
        die(b"Compiling regexes");
    };
    let opts = parse_options();
    daemon::set_pid_file(&opts.pid_file);

    let mut cfg = Config::default();
    if opts.do_kernel_log {
        cfg.add_data_source(SourceType::Klog, None, false);
    }
    cfg.add_data_source(SourceType::Syslog, Some(&opts.sock_name), true);
    let ret = config::parse_config(&mut cfg, &opts.config_file, &re_conffile);
    if opts.test_config {
        stdio::exit(ret);
    }
    if ret < 0 {
        die(b"Bad configuration file");
    }
    daemon::install_state(State {
        cfg,
        spawn_recursion: 0,
        re_compressed,
        console_level: opts.console_level,
        leaked: Vec::new(),
    });

    daemon::check_root();
    daemon::setgroup(opts.group_name.as_deref());
    daemon::dodaemonize(opts.daemonize);
    daemon::setsignals();
    if daemon::update_pid_file(&opts.pid_file) != 0 {
        stdio::exit(-1);
    }
    sys::clearargs();
    sys::setprogname(daemon::PROGNAME_MASTER);
    let num_sockets = daemon::get_data_sources(daemon::state());
    if num_sockets < 0 {
        die(b"Unable to bind sockets");
    }
    let ret = daemon::process(num_sockets);
    stdio::exit(ret)
}
