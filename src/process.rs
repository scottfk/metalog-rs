//! Routing a line through the config blocks (upstream `processLogLine()`),
//! plus what a matching block does: write it, echo it to stdout, send it
//! to remote hosts, run its command (`spawnCommand()`, `sendRemote()`),
//! and metalog's own messages (`doLog()`).

use crate::bcat;
use crate::config::{Sign, SignedRegex, SourceType, log_fac, log_pri};
use crate::daemon::{State, VERBOSE};
use crate::logline::{self, DateBuf, Line, Pid, Prg, build_log_line, cstr_at, rfc_stamp_fmt, stamp};
use crate::output::write_log_line;
use crate::stdio;
use crate::sys;

const MAX_LOG_LENGTH: usize = 8192;
const DEFAULT_DNS_LOOKUP_INTERVAL: i64 = 120;

/// The rule shared by `regex`/`neg_regex` and their `program_` forms: no
/// negative regex may match, and if there are positive regexes, at least
/// one must.  Evaluated in order, stopping at the first negative match.
fn regexes_pass(list: &[SignedRegex], subject: &[u8]) -> bool {
    let mut result = 0;
    for r in list {
        let m = r.regex.is_match(subject);
        match r.sign {
            Sign::Positive => {
                if m {
                    result = 1;
                } else if result != 1 {
                    result = -1;
                }
            }
            Sign::Negative => {
                if m {
                    return false;
                }
            }
        }
    }
    result != -1
}

/// `processLogLine()`.
pub fn process_log_line(
    st: &mut State,
    source: Option<usize>,
    logcode: i32,
    prg: &[u8],
    pid: Option<&[u8]>,
    info: &[u8],
) {
    let facility = log_fac(logcode);
    let priority = log_pri(logcode);
    // Never true (buffers are 4 kB), but it's in the C code.
    let info = &info[..info.len().min(MAX_LOG_LENGTH)];

    for bi in 0..st.cfg.blocks.len() {
        let b = &st.cfg.blocks[bi];
        if !b.facilities.is_empty() && !b.facilities.contains(&facility) {
            continue;
        }
        // QUIRK: "skip if minimum < priority < maximum", so `maximum` makes
        // a block take the *outer* range: minimum = 3, maximum = 6 accepts
        // priorities 0-3 and 6-7.
        if priority > b.minimum && priority < b.maximum {
            continue;
        }
        if b.source.is_some() && source != b.source {
            continue;
        }
        if let Some(p) = &b.program
            && !p.eq_ignore_ascii_case(prg)
        {
            continue;
        }
        if !b.program_regexes.is_empty() && !regexes_pass(&b.program_regexes, prg) {
            continue;
        }
        if info.is_empty() {
            continue;
        }
        if !b.regexes.is_empty() && !regexes_pass(&b.regexes, info) {
            continue;
        }

        let (output, log_format, log_severity, brk) = (b.output, b.log_format, b.log_severity, b.brk);
        let hosts = b.hosts.clone();
        let command = b.command.clone();
        let line = Line { prg, pid, info, priority, facility };

        let mut datebuf: DateBuf = [0; 100];
        if let Some(o) = output {
            let sf = &st.cfg.outputs[o].stamp_fmt;
            if !sf.is_empty() {
                let fmt = rfc_stamp_fmt(log_format).map_or_else(|| sf.clone(), <[u8]>::to_vec);
                stamp(&fmt, &mut datebuf);
            }
        }

        let mut do_break = false;
        if let Some(o) = output {
            let date = cstr_at(&datebuf, 0).to_vec();
            write_log_line(st, o, &date, &line, log_format, log_severity);
            stdio::out(&build_log_line(&line, log_format, &date, false, log_severity));

            if !hosts.is_empty() {
                for hi in 0..st.cfg.remote_hosts.len() {
                    let h = &st.cfg.remote_hosts[hi];
                    if h.severity_level >= priority && hosts.contains(&hi) {
                        let fmt =
                            rfc_stamp_fmt(h.format).map_or_else(|| st.cfg.outputs[o].stamp_fmt.clone(), <[u8]>::to_vec);
                        stamp(&fmt, &mut datebuf);
                        let date = cstr_at(&datebuf, 0).to_vec();
                        send_remote(st, hi, &line, &date, log_severity);
                    }
                }
            }
            if brk != 0 {
                do_break = true;
            }
        }
        if let Some(cmd) = command {
            // QUIRK: after remote hosts were served, the command gets the
            // last remote host's timestamp format, not the block's.
            let date = cstr_at(&datebuf, 0).to_vec();
            spawn_command(st, &cmd, &date, prg, info);
            if brk != 0 {
                do_break = true;
            }
        }
        if do_break {
            break;
        }
    }
}

/// `spawnCommand()`: fork and `execl(command, command, date, prg, info)`.
/// Not while another spawn is in progress (it would recurse via doLog).
pub fn spawn_command(st: &mut State, command: &[u8], date: &[u8], prg: &[u8], info: &[u8]) -> i32 {
    if st.spawn_recursion != 0 {
        return 1;
    }
    st.spawn_recursion += 1;
    let exec = sys::Exec::new(command, date, prg, info);
    // A failed fork returns -1 and is otherwise ignored, as in C.
    let pid = sys::fork().unwrap_or(-1);
    if pid == 0 {
        exec.exec_or_127();
    }
    if VERBOSE.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        do_log(
            st,
            &bcat!(
                "Forked command \"",
                command,
                " \"",
                date,
                "\" \"",
                prg,
                "\" \"",
                info,
                "\" [",
                (pid as u32).to_string(),
                "]."
            ),
        );
    }
    st.spawn_recursion -= 1;
    0
}

/// `doLog()`: metalog's own messages, as syslog.emerg from "metalog".
///
/// QUIRK: `LOG_SYSLOG` is passed as the whole log code, so the priority is
/// 0 (emerg), and metalog's chatter matches `minimum = 1`-style blocks.
pub fn do_log(st: &mut State, msg: &[u8]) {
    // vsnprintf into char infobuf[512].
    let msg = &msg[..msg.len().min(511)];
    let pid = sys::getpid().to_string();
    process_log_line(st, None, libc::LOG_SYSLOG, b"metalog", Some(pid.as_bytes()), msg);
}

/// `log_line()`: sanitize, parse, process one received line.  `buf` is a
/// NUL-terminated buffer the parser may write NULs into.
pub fn log_line(st: &mut State, source: usize, buf: &mut [u8]) -> i32 {
    logline::sanitize(buf);
    let ty = st.cfg.sources[source].ty;
    let Some(p) = logline::parse_log_line(ty, buf) else {
        return -1;
    };
    let prg: &[u8] = match p.prg {
        Prg::Kernel => logline::CF_PROGNAME_KERNEL,
        Prg::At(i) => cstr_at(buf, i),
    };
    let pid: Option<&[u8]> = match p.pid {
        Pid::None => None,
        Pid::Zero => Some(b"0"),
        Pid::At(i) => Some(cstr_at(buf, i)),
    };
    let info = cstr_at(buf, p.info);
    process_log_line(st, Some(source), p.logcode, prg, pid, info);
    0
}

/// `log_kernel()`: process the complete lines in a stream buffer, and
/// return how many bytes are left over after the last newline (0 if there
/// was no newline at all: the whole buffer is then dropped).
pub fn log_kernel(st: &mut State, source: usize, buf: &mut [u8], bsize: usize) -> usize {
    debug_assert_eq!(st.cfg.sources[source].ty, SourceType::Klog);
    let mut start = 0;
    for n in 0..bsize {
        if buf[n] == b'\n' {
            buf[n] = 0;
            log_line(st, source, &mut buf[start..]);
            start = n + 1;
        }
    }
    if start == 0 { 0 } else { bsize - start }
}

/// `sendRemote()`: one UDP datagram to a remote syslog server, resolving
/// (again) when there's no socket or the lookup is over two minutes old.
pub fn send_remote(st: &mut State, hi: usize, l: &Line<'_>, datebuf: &[u8], log_severity: bool) -> i32 {
    let format = st.cfg.remote_hosts[hi].format;
    let text = build_log_line(l, format, datebuf, true, log_severity);
    let h = &mut st.cfg.remote_hosts[hi];

    let (now, _) = sys::clock_monotonic();
    if h.sock >= 0 && h.last_dns + DEFAULT_DNS_LOOKUP_INTERVAL > now {
        // Always to the first resolved address, whichever one connected.
        let Some(first) = h.result.as_ref().and_then(|r| r.entries().first().copied()) else {
            // QUIRK: if a re-lookup failed while the old socket stayed
            // open, C dereferences the NULL result here and crashes.
            sys::raise_segv();
        };
        let sent = sys::sendto(h.sock, &text, &first).is_ok();
        if sent {
            return 0;
        }
        sys::close(h.sock);
        h.sock = -1;
    }

    h.last_dns = sys::clock_monotonic().0;
    h.result = None;
    let Some(res) = sys::AddrInfo::lookup(
        &h.hostname,
        Some(&h.port),
        libc::AF_UNSPEC,
        libc::SOCK_DGRAM,
        libc::AI_ADDRCONFIG | libc::AI_CANONNAME,
    ) else {
        return -1;
    };
    if h.sock >= 0 {
        sys::close(h.sock);
        h.sock = -1;
    }
    let mut ok = false;
    for rp in res.entries() {
        if h.sock < 0 {
            h.sock = sys::socket(rp.family, rp.socktype, rp.protocol).unwrap_or(-1);
        }
        if h.sock < 0 {
            continue;
        }
        if sys::connect(h.sock, &rp).is_err() {
            sys::close(h.sock);
            h.sock = -1;
            continue;
        }
        if sys::sendto(h.sock, &text, &rp).is_err() {
            sys::close(h.sock);
            h.sock = -1;
            continue;
        }
        ok = true;
        break;
    }
    h.result = Some(res);
    if ok { 0 } else { -1 }
}
