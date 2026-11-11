//! The configuration: blocks, outputs, remote hosts, data sources, and the
//! parser for `metalog.conf` (upstream `configParser()` / `parseLine()`).
//!
//! The C code keeps linked lists of malloc'ed structs that point at each
//! other.  Here they are vectors, and the pointers are indices; the lists'
//! orders are preserved where they're observable.

use std::rc::Rc;

use crate::bytes::c_str_prefix;
use crate::num::{atoi, strtoul};
use crate::stdio::{die, warn, warnp};
use crate::sys::{self, PCRE2_CASELESS, Regex};
use crate::{bcat, strtod};

pub const DEFAULT_MINIMUM: i32 = libc::LOG_DEBUG;
pub const DEFAULT_MAXIMUM: i32 = i32::MAX;
pub const DEFAULT_MAXSIZE: i64 = 1_000_000;
pub const DEFAULT_MAXFILES: i32 = 5;
pub const DEFAULT_MAXTIME: i64 = 60 * 60 * 24;
pub const DEFAULT_PERMS: libc::mode_t = 0o700;
pub const DEFAULT_STAMP_FMT: &[u8] = b"%b %d %T";
pub const DEFAULT_UDP_PORT: &[u8] = b"514";
pub const DEFAULT_REMOTE_HOST_NAME: &[u8] = b"default";
pub const DEFAULT_COMPRESS_DELAY: i32 = 0;
/// `LINE_MAX`, the size of the `fgets` buffer for config lines.
const LINE_MAX: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushMode {
    Default,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Legacy,
    LegacyTimestamp,
    Rfc3164,
    Rfc5424,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Positive,
    Negative,
}

#[derive(Debug, Clone)]
pub struct SignedRegex {
    pub regex: Rc<Regex>,
    pub sign: Sign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Syslog,
    Klog,
}

#[derive(Debug)]
pub struct DataSource {
    pub ty: SourceType,
    pub path: Option<Vec<u8>>,
    pub required: bool,
    pub fd: sys::Fd,
    pub stream: bool,
    pub bpos: usize,
}

#[derive(Debug, Default)]
pub struct DuplicateTracker {
    /// The `previous_prg` allocation (its length is `sizeof_previous_prg`).
    pub prev_prg: Vec<u8>,
    pub prev_info: Vec<u8>,
    /// `previous_sizeof_*`: the length the duplicate check compares with.
    pub prev_prg_len: usize,
    pub prev_info_len: usize,
    pub same_counter: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RateLimiter {
    pub usec_between_msgs: i64,
    pub bucket_size: i32,
    pub token_bucket: i32,
    pub last_tick: i64,
}

#[derive(Debug)]
pub struct Output {
    pub directory: Option<Vec<u8>>,
    pub perms: libc::mode_t,
    pub fp: Option<crate::stdio::CFile>,
    pub size: i64,
    pub maxsize: i64,
    pub maxfiles: i32,
    pub maxtime: i64,
    pub creatime: i64,
    pub dt: DuplicateTracker,
    pub postrotate_cmd: Option<Vec<u8>>,
    pub showrepeats: i32,
    pub stamp_fmt: Vec<u8>,
    pub flush: FlushMode,
    pub rate: RateLimiter,
    pub compress: bool,
    pub compress_delay: i32,
}

#[derive(Debug)]
pub struct RemoteHost {
    pub name: Vec<u8>,
    pub hostname: Vec<u8>,
    pub port: Vec<u8>,
    pub sock: sys::Fd,
    pub last_dns: i64,
    pub result: Option<sys::AddrInfo>,
    pub format: LogFormat,
    pub severity_level: i32,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub minimum: i32,
    pub maximum: i32,
    /// Empty means "any facility" (C: `facilities == NULL`).
    pub facilities: Vec<i32>,
    pub regexes: Vec<SignedRegex>,
    pub maxsize: i64,
    pub maxfiles: i32,
    pub maxtime: i64,
    pub perms: libc::mode_t,
    pub output: Option<usize>,
    pub command: Option<Vec<u8>>,
    pub program: Option<Vec<u8>>,
    pub brk: i32,
    pub program_regexes: Vec<SignedRegex>,
    pub postrotate_cmd: Option<Vec<u8>>,
    pub showrepeats: i32,
    pub stamp_fmt: Vec<u8>,
    pub flush: FlushMode,
    pub usec_between_msgs: i64,
    pub burst_length: i32,
    /// Indices into `Config::remote_hosts` (may repeat, as in C).
    pub hosts: Vec<usize>,
    pub log_format: LogFormat,
    pub log_severity: bool,
    /// Index into `Config::sources`.
    pub source: Option<usize>,
    pub compress: bool,
    pub compress_delay: i32,
}

impl Block {
    fn default_block() -> Block {
        Block {
            minimum: DEFAULT_MINIMUM,
            maximum: DEFAULT_MAXIMUM,
            facilities: Vec::new(),
            regexes: Vec::new(),
            maxsize: DEFAULT_MAXSIZE,
            maxfiles: DEFAULT_MAXFILES,
            maxtime: DEFAULT_MAXTIME,
            perms: DEFAULT_PERMS,
            output: None,
            command: None,
            program: None,
            brk: 0,
            program_regexes: Vec::new(),
            postrotate_cmd: None,
            showrepeats: 0,
            stamp_fmt: DEFAULT_STAMP_FMT.to_vec(),
            flush: FlushMode::Default,
            usec_between_msgs: 0,
            burst_length: 1,
            hosts: Vec::new(),
            log_format: LogFormat::LegacyTimestamp,
            log_severity: false,
            source: None,
            compress: false,
            compress_delay: DEFAULT_COMPRESS_DELAY,
        }
    }
}

/// Everything `configParser()` builds, plus the data sources `main()` adds.
#[derive(Debug, Default)]
pub struct Config {
    pub blocks: Vec<Block>,
    pub outputs: Vec<Output>,
    pub remote_hosts: Vec<RemoteHost>,
    /// In creation order.  C prepends to its list, so C's iteration order is
    /// the reverse of this; see [`Config::sources_c_order`].
    pub sources: Vec<DataSource>,
    pub config_dir: Option<Vec<u8>>,
}

impl Config {
    /// `add_data_source()`: reuse a source of the same type and path.
    pub fn add_data_source(&mut self, ty: SourceType, path: Option<&[u8]>, required: bool) -> usize {
        if let Some(i) = self.sources.iter().position(|s| s.ty == ty && s.path.as_deref() == path) {
            return i;
        }
        self.sources.push(DataSource { ty, path: path.map(<[u8]>::to_vec), required, fd: -1, stream: false, bpos: 0 });
        self.sources.len() - 1
    }

    /// Source indices in the order C walks its `data_sources` list.
    pub fn sources_c_order(&self) -> impl Iterator<Item = usize> + use<> {
        (0..self.sources.len()).rev()
    }

    fn find_remote_host(&self, name: &[u8]) -> Option<usize> {
        self.remote_hosts.iter().position(|h| h.name == name)
    }
}

/// glibc's `facilitynames[]` (with `SYSLOG_NAMES`), in table order.
const FACILITY_NAMES: &[(&[u8], i32)] = &[
    (b"auth", libc::LOG_AUTH),
    (b"authpriv", libc::LOG_AUTHPRIV),
    (b"cron", libc::LOG_CRON),
    (b"daemon", libc::LOG_DAEMON),
    (b"ftp", libc::LOG_FTP),
    (b"kern", libc::LOG_KERN),
    (b"lpr", libc::LOG_LPR),
    (b"mail", libc::LOG_MAIL),
    (b"mark", 24 << 3), // INTERNAL_MARK
    (b"news", libc::LOG_NEWS),
    (b"security", libc::LOG_AUTH),
    (b"syslog", libc::LOG_SYSLOG),
    (b"user", libc::LOG_USER),
    (b"uucp", libc::LOG_UUCP),
    (b"local0", libc::LOG_LOCAL0),
    (b"local1", libc::LOG_LOCAL1),
    (b"local2", libc::LOG_LOCAL2),
    (b"local3", libc::LOG_LOCAL3),
    (b"local4", libc::LOG_LOCAL4),
    (b"local5", libc::LOG_LOCAL5),
    (b"local6", libc::LOG_LOCAL6),
    (b"local7", libc::LOG_LOCAL7),
];

/// `LOG_FAC()`.
pub fn log_fac(p: i32) -> i32 {
    (p & libc::LOG_FACMASK) >> 3
}

/// `LOG_PRI()`.
pub fn log_pri(p: i32) -> i32 {
    p & libc::LOG_PRIMASK
}

/// `wpcre2_compile()`: compile, warning on failure.
pub fn wpcre2_compile(pattern: &[u8], options: u32) -> Option<Regex> {
    match Regex::compile(pattern, options) {
        Ok(r) => Some(r),
        Err((msg, off)) => {
            warn(&bcat!("Invalid regex [", pattern, "] at ", off.to_string(), ": ", msg));
            None
        }
    }
}

/// `translate_log_format()`.
pub fn translate_log_format(s: &[u8]) -> LogFormat {
    match s {
        b"legacy" => LogFormat::Legacy,
        b"legacy_timestamp" => LogFormat::LegacyTimestamp,
        b"rfc3164" => LogFormat::Rfc3164,
        b"rfc5424" => LogFormat::Rfc5424,
        _ => {
            // QUIRK: the message has its own "\n" on top of warn()'s, and
            // "the default" it falls back to is RFC 5424, not the real
            // default (legacy_timestamp for files, legacy for remotes).
            warn(&bcat!("Invalid log_format: ", s, " - set default format\n"));
            LogFormat::Rfc5424
        }
    }
}

/// `extract_remote_host_name()`: `remote_host[NAME]` -> NAME, else "default".
fn extract_remote_host_name(keyword: &[u8], pattern: &[u8]) -> Vec<u8> {
    if keyword.len() == pattern.len() {
        return DEFAULT_REMOTE_HOST_NAME.to_vec();
    }
    let start = keyword.iter().position(|&b| b == b'[');
    let end = keyword.iter().position(|&b| b == b']');
    match (start, end) {
        (Some(s), Some(e)) if e > s => keyword[s + 1..e].to_vec(),
        _ => DEFAULT_REMOTE_HOST_NAME.to_vec(),
    }
}

/// Which block statements go to: before the first `name:` line they set
/// the defaults that later blocks start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cur {
    Default,
    Block(usize),
}

/// The parse state shared by the main file and the `configdir` files.
struct Parser {
    re_newblock: Regex,
    re_newstmt: Regex,
    re_comment: Regex,
    re_null: Regex,
    default_block: Block,
    cur: Cur,
}

impl Parser {
    /// `parseLine()`.  `line` is what `fgets` stored, cut at the first NUL.
    fn parse_line(&mut self, cfg: &mut Config, line: &[u8]) -> i32 {
        let mut line = line;
        if line.is_empty() {
            return 0;
        }
        if line[line.len() - 1] == b'\n' {
            if line.len() < 2 {
                return 0;
            }
            line = &line[..line.len() - 1];
        }
        if self.re_null.is_match(line) || self.re_comment.is_match(line) {
            return 0;
        }
        if self.re_newblock.is_match(line) {
            // QUIRK: the new block is a struct copy of the default block, so
            // it shares the default's facility and regex arrays.  Upstream
            // then realloc()s or free()s those shared arrays in place, which
            // is undefined behavior; this port copies them instead, which
            // is what happens in practice when realloc doesn't move them.
            let b = self.default_block.clone();
            cfg.blocks.push(b);
            self.cur = Cur::Block(cfg.blocks.len() - 1);
            return 0;
        }
        let (rc, caps) = self.re_newstmt.captures(line);
        if rc < 3 {
            // QUIRK: a line that is neither a block header, a comment nor a
            // `key = value` statement is silently ignored.
            return 0;
        }
        let keyword = line[caps[1].0..caps[1].1].to_vec();
        let value = line[caps[2].0..caps[2].1].to_vec();
        self.statement(cfg, line, &keyword, &value)
    }

    fn statement(&mut self, cfg: &mut Config, line: &[u8], keyword: &[u8], value: &[u8]) -> i32 {
        let kw = |k: &[u8]| keyword.eq_ignore_ascii_case(k);
        let cur = self.cur;
        macro_rules! blk {
            () => {
                match cur {
                    Cur::Default => &mut self.default_block,
                    Cur::Block(i) => &mut cfg.blocks[i],
                }
            };
        }
        // Propagate a setting to the block's output, if it already has one
        // (settings made after `logdir` still reach the output).
        macro_rules! out {
            ($f:expr) => {{
                if let Some(o) = blk!().output {
                    let o = &mut cfg.outputs[o];
                    ($f)(o);
                }
            }};
        }

        if kw(b"minimum") {
            blk!().minimum = atoi(value);
        } else if kw(b"maximum") {
            blk!().maximum = atoi(value);
        } else if kw(b"facility") {
            if value == b"*" {
                blk!().facilities.clear();
                return 0;
            }
            let Some(&(_, val)) = FACILITY_NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(value)) else {
                warn(&bcat!("Unknown facility : [", value, "]"));
                return -4;
            };
            blk!().facilities.push(log_fac(val));
        } else if kw(b"regex") || kw(b"neg_regex") {
            let Some(re) = wpcre2_compile(value, PCRE2_CASELESS) else {
                return -5;
            };
            let sign = if kw(b"neg_regex") { Sign::Negative } else { Sign::Positive };
            blk!().regexes.push(SignedRegex { regex: Rc::new(re), sign });
        } else if kw(b"program_regex") || kw(b"program_neg_regex") {
            let Some(re) = wpcre2_compile(value, PCRE2_CASELESS) else {
                return -5;
            };
            let sign = if kw(b"program_neg_regex") { Sign::Negative } else { Sign::Positive };
            blk!().program_regexes.push(SignedRegex { regex: Rc::new(re), sign });
        } else if kw(b"maxsize") {
            let v = strtoul(value, 0).value as i64;
            blk!().maxsize = v;
            out!(|o: &mut Output| o.maxsize = v);
        } else if kw(b"maxfiles") {
            let v = atoi(value);
            blk!().maxfiles = v;
            out!(|o: &mut Output| o.maxfiles = v);
        } else if kw(b"maxtime") {
            let v = strtoul(value, 0).value as i64;
            blk!().maxtime = v;
            out!(|o: &mut Output| o.maxtime = v);
        } else if kw(b"showrepeats") {
            let v = atoi(value);
            blk!().showrepeats = v;
            out!(|o: &mut Output| o.showrepeats = v);
        } else if kw(b"perms") {
            let v = strtoul(value, 8).value as libc::mode_t;
            blk!().perms = v;
            out!(|o: &mut Output| o.perms = v);
        } else if kw(b"configdir") {
            // Only once, to avoid recursion.
            if cfg.config_dir.is_some() {
                // QUIRK: warnp() appends strerror(errno) for no failed call.
                warnp(b"Ignore repeated \"configdir\" statement");
            } else {
                cfg.config_dir = Some(value.to_vec());
            }
        } else if kw(b"logdir") {
            if let Some(i) = cfg.outputs.iter().position(|o| o.directory.as_deref() == Some(value)) {
                blk!().output = Some(i);
                return 0;
            }
            let b = blk!();
            let directory = if value.eq_ignore_ascii_case(b"NONE") { None } else { Some(value.to_vec()) };
            let out = Output {
                directory,
                perms: b.perms,
                fp: None,
                size: 0,
                maxsize: b.maxsize,
                maxfiles: b.maxfiles,
                maxtime: b.maxtime,
                creatime: 0,
                dt: DuplicateTracker::default(),
                postrotate_cmd: b.postrotate_cmd.clone(),
                showrepeats: b.showrepeats,
                stamp_fmt: b.stamp_fmt.clone(),
                flush: b.flush,
                rate: RateLimiter {
                    usec_between_msgs: b.usec_between_msgs,
                    bucket_size: b.burst_length,
                    token_bucket: b.burst_length,
                    last_tick: 0,
                },
                compress: b.compress,
                compress_delay: b.compress_delay,
            };
            cfg.outputs.push(out);
            blk!().output = Some(cfg.outputs.len() - 1);
        } else if kw(b"command") {
            let first = match value.iter().position(|&b| b == b' ') {
                Some(i) => &value[..i],
                None => value,
            };
            if sys::stat(first).is_err() {
                warnp(&bcat!("Ignoring command \"", first, "\""));
                return 0;
            }
            // QUIRK: only the first word must exist, but the whole value,
            // arguments and all, is later exec'ed as the program path, so a
            // command with arguments always fails (exit status 127).
            blk!().command = Some(value.to_vec());
        } else if kw(b"program") {
            blk!().program = Some(value.to_vec());
        } else if kw(b"postrotate_cmd") {
            blk!().postrotate_cmd = Some(value.to_vec());
            let v = value.to_vec();
            out!(|o: &mut Output| o.postrotate_cmd = Some(v));
        } else if keyword.starts_with(b"remote_host") {
            let name = extract_remote_host_name(keyword, b"remote_host");
            let idx = match cfg.find_remote_host(&name) {
                Some(i) => i,
                None => {
                    cfg.remote_hosts.push(RemoteHost {
                        name,
                        hostname: value.to_vec(),
                        port: DEFAULT_UDP_PORT.to_vec(),
                        sock: -1,
                        last_dns: 0,
                        result: None,
                        format: LogFormat::Legacy,
                        severity_level: libc::LOG_DEBUG,
                    });
                    cfg.remote_hosts.len() - 1
                }
            };
            cfg.remote_hosts[idx].hostname = value.to_vec();
        } else if keyword.starts_with(b"remote_port") {
            let name = extract_remote_host_name(keyword, b"remote_port");
            if let Some(i) = cfg.find_remote_host(&name) {
                cfg.remote_hosts[i].port = value.to_vec();
            }
        } else if keyword.starts_with(b"remote_format") {
            let name = extract_remote_host_name(keyword, b"remote_format");
            if let Some(i) = cfg.find_remote_host(&name) {
                cfg.remote_hosts[i].format = translate_log_format(value);
            }
        } else if keyword.starts_with(b"remote_severity_level") {
            let name = extract_remote_host_name(keyword, b"remote_severity_level");
            if let Some(i) = cfg.find_remote_host(&name) {
                cfg.remote_hosts[i].severity_level = atoi(value);
            }
        } else if keyword.starts_with(b"remote_log") {
            let v = atoi(value);
            let name = extract_remote_host_name(keyword, b"remote_log");
            if let Some(h) = cfg.find_remote_host(&name) {
                let b = blk!();
                if v == 1 {
                    b.hosts.push(h);
                } else if v == 0
                    && let Some(pos) = b.hosts.iter().position(|&x| x == h)
                {
                    b.hosts.remove(pos);
                    if b.hosts.is_empty() {
                        // QUIRK: shrinking to zero is realloc(p, 0),
                        // which frees and returns NULL, so wrealloc()
                        // warns "failed" (harmlessly: the count is 0).
                        warnp(&bcat!("realloc(", format!("{:p}", b.hosts.as_ptr()), ", 0) failed"));
                    }
                }
            }
        } else if kw(b"break") {
            blk!().brk = atoi(value);
        } else if kw(b"stamp_fmt") {
            blk!().stamp_fmt = value.to_vec();
            let v = value.to_vec();
            out!(|o: &mut Output| o.stamp_fmt = v);
        } else if kw(b"flush") {
            let f = if atoi(value) != 0 { FlushMode::Always } else { FlushMode::Never };
            blk!().flush = f;
            out!(|o: &mut Output| o.flush = f);
        } else if kw(b"ratelimit") {
            let (base_rate, end) = strtod::strtod(value);
            if end == 0 {
                warnp(&bcat!("Missing number : [", value, "]"));
                return -6;
            }
            if base_rate < 0.0 {
                warn(&bcat!("Negative number : [", value, "]"));
                return -6;
            }
            let usec_between_msgs: i64 = if base_rate != 0.0 {
                let rest = &value[end..];
                if rest.first() != Some(&b'/') {
                    warn(&bcat!("Missing unit of time : [", value, "]"));
                    return -6;
                }
                let usec = match rest.get(1) {
                    Some(b's') => 1_000_000.0,
                    Some(b'm') => 1_000_000.0 * 60.0,
                    Some(b'h') => 1_000_000.0 * 60.0 * 60.0,
                    Some(b'd') => 1_000_000.0 * 60.0 * 60.0 * 24.0,
                    _ => {
                        warn(&bcat!("Invalid unit of time : [", &rest[1..], "]"));
                        return -6;
                    }
                };
                strtod::cvttsd2si(usec / base_rate)
            } else {
                strtod::cvttsd2si(base_rate)
            };
            if usec_between_msgs < 0 {
                // QUIRK: a rate too small to express (or NaN) overflows the
                // int64 conversion and trips upstream's assert().
                crate::daemon::assert_fail(b"usec_between_msgs >= 0", 573, b"parseLine");
            }
            blk!().usec_between_msgs = usec_between_msgs;
            out!(|o: &mut Output| o.rate.usec_between_msgs = usec_between_msgs);
        } else if kw(b"ratelimit_burst") {
            let burst = atoi(value);
            if burst < 1 {
                warn(&bcat!("Non-positive bust length : [", value, "]"));
                return -6;
            }
            blk!().burst_length = burst;
            out!(|o: &mut Output| {
                o.rate.bucket_size = burst;
                o.rate.token_bucket = burst;
            });
        } else if kw(b"log_format") {
            blk!().log_format = translate_log_format(value);
        } else if kw(b"log_severity") {
            blk!().log_severity = atoi(value) != 0;
        } else if kw(b"socket") {
            let s = cfg.add_data_source(SourceType::Syslog, Some(value), false);
            blk!().source = Some(s);
        } else if kw(b"compress") {
            let c = atoi(value) == 1;
            blk!().compress = c;
            #[cfg(not(feature = "compress"))]
            if c {
                warn(b"Log compression configuration ignored as support not compiled in");
            }
            out!(|o: &mut Output| o.compress = c);
        } else if kw(b"compress_delay") {
            let d = atoi(value);
            blk!().compress_delay = d;
            out!(|o: &mut Output| o.compress_delay = d);
        } else {
            die(&bcat!("Unknown keyword '", keyword, "'! line: ", line));
        }
        0
    }

    /// Feed a file through `parseLine()` the way the `fgets` loop does,
    /// stopping at the first nonzero result.
    fn parse_file(&mut self, cfg: &mut Config, data: &[u8]) -> i32 {
        let mut retcode = 0;
        let mut rest = data;
        while !rest.is_empty() {
            // fgets(line, LINE_MAX, fp): up to LINE_MAX-1 bytes, or through '\n'.
            let take = match rest[..rest.len().min(LINE_MAX - 1)].iter().position(|&b| b == b'\n') {
                Some(i) => i + 1,
                None => rest.len().min(LINE_MAX - 1),
            };
            let chunk = c_str_prefix(&rest[..take]);
            rest = &rest[take..];
            retcode = self.parse_line(cfg, chunk);
            if retcode != 0 {
                break;
            }
        }
        retcode
    }
}

/// `configParser()`.  Returns 0 on success, negative on errors (the value
/// is also `-t`'s exit status).
pub fn parse_config(cfg: &mut Config, file: &[u8], re_conffile: &Regex) -> i32 {
    let fd = match sys::open(file, libc::O_RDONLY, 0) {
        Ok(fd) => fd,
        Err(_) => {
            warnp(b"Can't open the configuration file");
            return -2;
        }
    };
    let compile = |p: &[u8]| wpcre2_compile(p, 0);
    let (Some(re_newblock), Some(re_newstmt), Some(re_comment), Some(re_null)) = (
        compile(b":\\s*$"),
        compile(b"^\\s*(.+?)\\s*=\\s*\"?([^\"]*)\"?\\s*$"),
        compile(b"^\\s*#"),
        compile(b"^\\s*$"),
    ) else {
        sys::close(fd);
        return -1;
    };
    let mut p = Parser {
        re_newblock,
        re_newstmt,
        re_comment,
        re_null,
        default_block: Block::default_block(),
        cur: Cur::Default,
    };

    let data = sys::read_to_end(fd);
    let mut retcode = p.parse_file(cfg, &data);

    if retcode == 0
        && let Some(dir) = cfg.config_dir.clone()
    {
        // scandir(config_dir, ..., conffile_filter, alphasort): metalog
        // never calls setlocale(), so alphasort's strcoll is strcmp.
        let Some(names) = sys::read_dir_names(&dir) else {
            warnp(&bcat!("Can't scan the config dir ", dir));
            sys::close(fd);
            return -1;
        };
        let mut names: Vec<Vec<u8>> = names.into_iter().filter(|n| re_conffile.is_match(n)).collect();
        names.sort();
        for name in names {
            let path = bcat!(dir, "/", name);
            let Ok(fd2) = sys::open(&path, libc::O_RDONLY, 0) else {
                warnp(&bcat!("Can't open the config file ", path));
                sys::close(fd);
                return -1;
            };
            let data = sys::read_to_end(fd2);
            // QUIRK: each file overwrites retcode, so an error in one
            // configdir file (which stops parsing that file) is
            // forgotten if a later file parses cleanly.
            retcode = p.parse_file(cfg, &data);
            sys::close(fd2);
        }
    }
    sys::close(fd);
    retcode
}
