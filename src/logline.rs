//! One log line: sanitising and parsing what arrives on a socket, building
//! the text that gets written, and the timestamps (upstream `sanitize()`,
//! `parseLogLine()`, `build_log_line()`, `get_stamp_fmt_timestamp()`).
//!
//! Received lines live in a NUL-terminated byte buffer, and parsing works
//! on it the way the C code does: it writes NULs to end the program name
//! and pid, and hands out offsets into the buffer.

use crate::bcat;
use crate::bytes::c_str_prefix;
use crate::config::{LogFormat, SourceType};
use crate::num::is_c_space;
use crate::sys;

pub const CF_PROGNAME_KERNEL: &[u8] = b"kernel";
pub const NILVALUE: &[u8] = b"-";
pub const BOM_UTF8: &[u8] = b"\xEF\xBB\xBF";
pub const NONPRINTABLE_SUBSTITUTE_CHAR: u8 = b'_';

/// `ISCTRLCODE()`.  With `--with-unicode` only C0 and DEL count, so UTF-8
/// sequences survive; without it, C1 bytes (0x80-0x9f) are replaced too.
pub fn is_ctrl_code(b: u8) -> bool {
    if cfg!(feature = "unicode") { b == 0x7f || b < 0x20 } else { b == 0x7f || (b & 0x60) == 0 }
}

/// `sanitize()`: control characters become `_`, except that the very last
/// character of the line is dropped instead if it is one.
///
/// QUIRK: only one trailing control character is dropped, so "msg\n\n"
/// becomes "msg_".
pub fn sanitize(buf: &mut [u8]) {
    let mut i = 0;
    while i < buf.len() && buf[i] != 0 {
        if is_ctrl_code(buf[i]) {
            if buf.get(i + 1).copied().unwrap_or(0) == 0 {
                buf[i] = 0;
            } else {
                buf[i] = NONPRINTABLE_SUBSTITUTE_CHAR;
            }
        }
        i += 1;
    }
}

/// Where the parts of a parsed line are.
#[derive(Debug, Clone, Copy)]
pub enum Prg {
    Kernel,
    At(usize),
}

#[derive(Debug, Clone, Copy)]
pub enum Pid {
    None,
    Zero,
    At(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct Parsed {
    pub logcode: i32,
    pub prg: Prg,
    pub pid: Pid,
    pub info: usize,
}

/// The C string starting at `at` in `buf`.
pub fn cstr_at(buf: &[u8], at: usize) -> &[u8] {
    c_str_prefix(&buf[at.min(buf.len())..])
}

/// `parseLogLine()`: `<PRI>...: prog[pid]: message`.
///
/// QUIRK: the program name is found by skipping to the first `:` (which
/// in a normal syslog line is in the timestamp, "Nov 11 14:58:00"), then to
/// the next space.  Without a timestamp, everything up to the first `:` is
/// lost, the program name is taken from the message text, and a line with
/// only one `:` is dropped.
///
/// QUIRK: `<0>` (kern.emerg) is rejected, because the priority must be > 0.
pub fn parse_log_line(ty: SourceType, buf: &mut [u8]) -> Option<Parsed> {
    let at = |buf: &[u8], i: usize| buf.get(i).copied().unwrap_or(0);
    let mut i = 0;
    if at(buf, i) != b'<' {
        return None;
    }
    i += 1;
    let mut logcode = crate::num::atoi(cstr_at(buf, i));
    if logcode <= 0 {
        return None;
    }
    while at(buf, i) != b'>' {
        if at(buf, i) == 0 {
            return None;
        }
        i += 1;
    }
    i += 1;

    if ty == SourceType::Klog {
        logcode |= libc::LOG_KERN;
        return Some(Parsed { logcode, prg: Prg::Kernel, pid: Pid::Zero, info: i });
    }
    while at(buf, i) != b':' {
        if at(buf, i) == 0 {
            return None;
        }
        i += 1;
    }
    i += 1;
    while !is_c_space(at(buf, i)) {
        if at(buf, i) == 0 {
            return None;
        }
        i += 1;
    }
    buf[i] = 0;
    i += 1;
    let prg = i;
    while at(buf, i) != b'[' && at(buf, i) != b':' {
        if at(buf, i) == 0 {
            return None;
        }
        i += 1;
    }
    let mut pid = Pid::None;
    if at(buf, i) == b'[' {
        buf[i] = 0;
        i += 1;
        pid = Pid::At(i);
        while at(buf, i) != b']' {
            if at(buf, i) == 0 {
                return None;
            }
            i += 1;
        }
        buf[i] = 0;
        i += 1;
        while at(buf, i) != b':' {
            if at(buf, i) == 0 {
                return None;
            }
            i += 1;
        }
    } else {
        buf[i] = 0;
    }
    i += 1;
    while is_c_space(at(buf, i)) {
        i += 1;
    }
    Some(Parsed { logcode, prg: Prg::At(prg), pid, info: i })
}

/// `build_priority_str()`.
fn priority_str(severity: i32) -> &'static [u8] {
    match severity {
        libc::LOG_EMERG => b"EMERG",
        libc::LOG_ALERT => b"ALERT",
        libc::LOG_CRIT => b"CRIT",
        libc::LOG_ERR => b"ERR",
        libc::LOG_WARNING => b"WARNING",
        libc::LOG_NOTICE => b"NOTICE",
        libc::LOG_INFO => b"INFO",
        libc::LOG_DEBUG => b"DEBUG",
        _ => b"UNKNOWN",
    }
}

/// The fields of one line, as `build_log_line()` takes them.
#[derive(Debug, Clone, Copy)]
pub struct Line<'a> {
    pub prg: &'a [u8],
    pub pid: Option<&'a [u8]>,
    pub info: &'a [u8],
    pub priority: i32,
    pub facility: i32,
}

/// `build_log_line()`.
pub fn build_log_line(
    l: &Line<'_>,
    format: LogFormat,
    datebuf: &[u8],
    print_hostname: bool,
    log_severity: bool,
) -> Vec<u8> {
    let pri = (l.facility * 8 + l.priority).to_string();
    let pid = l.pid.unwrap_or(NILVALUE);
    let mut hostname =
        if print_hostname { sys::gethostname().unwrap_or_else(|| NILVALUE.to_vec()) } else { Vec::new() };
    match format {
        LogFormat::Legacy | LogFormat::LegacyTimestamp => {
            let ts = format == LogFormat::LegacyTimestamp;
            match (log_severity, ts) {
                (true, false) => bcat!("[", l.prg, "][", priority_str(l.priority), "] ", l.info, "\n"),
                (true, true) => bcat!(datebuf, " [", l.prg, "][", priority_str(l.priority), "] ", l.info, "\n"),
                (false, false) => bcat!("[", l.prg, "] ", l.info, "\n"),
                (false, true) => bcat!(datebuf, " [", l.prg, "] ", l.info, "\n"),
            }
        }
        LogFormat::Rfc3164 => {
            let kernel = l.prg == CF_PROGNAME_KERNEL;
            match (kernel, print_hostname) {
                (true, true) => bcat!("<", pri, ">", datebuf, " ", hostname, " ", l.prg, ": ", l.info, "\n"),
                (true, false) => bcat!("<", pri, ">", datebuf, " ", l.prg, ": ", l.info, "\n"),
                (false, true) => {
                    bcat!("<", pri, ">", datebuf, " ", hostname, " ", l.prg, "[", pid, "]: ", l.info, "\n")
                }
                (false, false) => bcat!("<", pri, ">", datebuf, " ", l.prg, "[", pid, "]: ", l.info, "\n"),
            }
        }
        LogFormat::Rfc5424 => {
            if print_hostname {
                hostname = match sys::AddrInfo::lookup(
                    &hostname,
                    None,
                    libc::AF_UNSPEC,
                    libc::SOCK_STREAM,
                    libc::AI_CANONNAME,
                ) {
                    // snprintf("%s", NULL) prints "(null)" in glibc.
                    Some(ai) => ai.canonname().unwrap_or_else(|| b"(null)".to_vec()),
                    None => NILVALUE.to_vec(),
                };
                hostname.truncate(sys::HOST_NAME_MAX);
            }
            let kernel = l.prg == CF_PROGNAME_KERNEL;
            match (kernel, print_hostname) {
                (true, true) => {
                    bcat!("<", pri, ">1 ", datebuf, " ", hostname, " ", l.prg, " - - - ", BOM_UTF8, l.info, "\n")
                }
                (true, false) => bcat!("<", pri, ">1 ", datebuf, " ", l.prg, " - - - ", BOM_UTF8, l.info, "\n"),
                (false, true) => {
                    bcat!(
                        "<", pri, ">1 ", datebuf, " ", hostname, " ", l.prg, " ", pid, " - - ", BOM_UTF8, l.info, "\n"
                    )
                }
                (false, false) => {
                    bcat!("<", pri, ">1 ", datebuf, " ", l.prg, " ", pid, " - - ", BOM_UTF8, l.info, "\n")
                }
            }
        }
    }
}

/// `get_stamp_fmt_for_rfc_format()`.
pub fn rfc_stamp_fmt(format: LogFormat) -> Option<&'static [u8]> {
    match format {
        LogFormat::Rfc3164 => Some(b"%b %d %H:%M:%S"),
        LogFormat::Rfc5424 => Some(b"%Y-%m-%dT%H:%M:%S.%3N%:z"),
        _ => None,
    }
}

/// printf `%d` with flags `+`/`0` and a width.
fn fmt_int(v: i64, width: usize, plus: bool, zero: bool) -> Vec<u8> {
    let digits = v.unsigned_abs().to_string();
    let sign = if v < 0 {
        "-"
    } else if plus {
        "+"
    } else {
        ""
    };
    let len = sign.len() + digits.len();
    let pad = width.saturating_sub(len);
    if zero { bcat!(sign, "0".repeat(pad), digits) } else { bcat!(" ".repeat(pad), sign, digits) }
}

/// The datebuf: 100 bytes, as in `processLogLine()`, written in place by
/// `strftime`.  It is reused for the remote hosts, so stale bytes matter.
pub type DateBuf = [u8; 100];

/// `get_stamp_fmt_timestamp()`: format "now" into `datebuf`.
///
/// Adds two conversions to strftime's: `%:z` (+hh:mm) and `%[digits]N`
/// (fractional seconds, default 9 digits).
pub fn stamp(stamp_fmt: &[u8], datebuf: &mut DateBuf) {
    let (sec, nsec) = sys::clock_realtime();
    let Some(tm) = sys::localtime(sec) else {
        return;
    };
    let mut fmt = stamp_fmt.to_vec();

    // QUIRK: only the first "%:z" is replaced, even one that is really an
    // escaped "%%:z"; and for negative offsets with minutes both parts carry
    // the sign ("-03:-30").
    if let Some(p) = crate::bytes::find(&fmt, b"%:z") {
        let off = tm.tm_gmtoff as i32;
        let h = i64::from(off / 3600);
        let m = i64::from((off % 3600) / 60);
        fmt = bcat!(&fmt[..p], fmt_int(h, 3, true, true), ":", fmt_int(m, 2, false, true), &fmt[p + 3..]);
    }

    // "%N" processing, step for step like the C loop.
    let mut p = 0;
    while let Some(off) = fmt[p..].iter().position(|&b| b == b'%') {
        p += off + 1;
        if fmt.get(p) == Some(&b'%') {
            p += 1;
            continue;
        }
        let n = fmt[p..].iter().take_while(|b| b.is_ascii_digit()).count();
        if fmt.get(p + n) != Some(&b'N') {
            p += n;
            continue;
        }
        let mut scale: u32 = 1;
        let mut precision: u64 = 9;
        let mut erange = false;
        if n > 0 {
            let parsed = crate::num::strtoul(&fmt[p..p + n], 10);
            precision = parsed.value;
            if !parsed.erange {
                if precision == 0 || precision > 9 {
                    precision = 9;
                }
                for _ in 0..(9 - precision) {
                    scale *= 10;
                }
            } else {
                erange = true;
            }
        }
        let m = p;
        let value = (nsec as u32) / scale;
        // "%0*u": a width that doesn't fit an int is negative, which means
        // "left-justify" and pads with nothing a number could need.
        let digits = if erange {
            value.to_string().into_bytes()
        } else {
            fmt_int(i64::from(value), precision as usize, false, true)
        };
        fmt = bcat!(&fmt[..p - 1], digits, &fmt[p + n + 1..]);
        p = m;
    }
    sys::strftime(datebuf, &fmt, &tm);
}

#[cfg(test)]
mod tests {
    use super::*;

    type Fields = (i32, Vec<u8>, Option<Vec<u8>>, Vec<u8>);

    fn parse(s: &[u8]) -> Option<Fields> {
        let mut buf = bcat!(s, [0u8]);
        sanitize(&mut buf);
        let p = parse_log_line(SourceType::Syslog, &mut buf)?;
        let prg = match p.prg {
            Prg::Kernel => CF_PROGNAME_KERNEL.to_vec(),
            Prg::At(i) => cstr_at(&buf, i).to_vec(),
        };
        let pid = match p.pid {
            Pid::None => None,
            Pid::Zero => Some(b"0".to_vec()),
            Pid::At(i) => Some(cstr_at(&buf, i).to_vec()),
        };
        Some((p.logcode, prg, pid, cstr_at(&buf, p.info).to_vec()))
    }

    #[test]
    fn typical_line() {
        let (code, prg, pid, info) = parse(b"<13>Nov 11 14:58:00 myprog[42]: hello world").unwrap();
        assert_eq!(code, 13);
        assert_eq!(prg, b"myprog");
        assert_eq!(pid.as_deref(), Some(&b"42"[..]));
        assert_eq!(info, b"hello world");
    }

    #[test]
    fn no_pid_and_odd_lines() {
        let (_, prg, pid, info) = parse(b"<13>Nov 11 14:58:00 myprog: x").unwrap();
        assert_eq!((prg.as_slice(), pid, info.as_slice()), (&b"myprog"[..], None, &b"x"[..]));
        assert!(parse(b"<0>Nov 11 14:58:00 p: x").is_none());
        assert!(parse(b"13>x").is_none());
        assert!(parse(b"<13>no colon here").is_none());
        // No timestamp: the text up to the first ':' is skipped, so the
        // "program" is whatever follows it.
        let (_, prg, _, info) = parse(b"<13>p: hello there: world").unwrap();
        assert_eq!((prg.as_slice(), info.as_slice()), (&b"hello there"[..], &b"world"[..]));
        // ...and with no second ':' the line is dropped.
        assert!(parse(b"<13>no timestamp: first word eaten").is_none());
    }

    #[test]
    fn sanitize_quirk() {
        let mut b = bcat!("a\tb\n\n", [0u8]);
        sanitize(&mut b);
        assert_eq!(c_str_prefix(&b), b"a_b_");
    }

    #[test]
    fn ints() {
        assert_eq!(fmt_int(-3, 3, true, true), b"-03");
        assert_eq!(fmt_int(5, 3, true, true), b"+05");
        assert_eq!(fmt_int(-30, 2, false, true), b"-30");
        assert_eq!(fmt_int(7, 9, false, true), b"000000007");
    }
}
