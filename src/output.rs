//! Writing to a log directory: duplicate suppression, rate limiting,
//! opening `current` and `.timestamp`, rotation and compression (upstream
//! `writeLogLine()`, `rateLimit()`, `rotateLogFiles()`, `compressLogFile()`).

use crate::bcat;
use crate::config::{FlushMode, LogFormat, RateLimiter};
use crate::daemon::{SYNCHRONOUS, State};
use crate::logline::{Line, build_log_line};
use crate::stdio::{CFile, warn, warnp};
use crate::sys;

pub const OUTPUT_DIR_TIMESTAMP: &[u8] = b".timestamp";
pub const OUTPUT_DIR_CURRENT: &[u8] = b"current";
pub const OUTPUT_DIR_LOGFILES_PREFIX: &[u8] = b"log-";
pub const OUTPUT_DIR_LOGFILES_FMT: &[u8] = b"log-%Y-%m-%d-%H:%M:%S";
const LAST_OUTPUT_TWICE: &[u8] = b"                - Last output repeated twice -\n";
const MAX_SIGNIFICANT_LENGTH: usize = 512;
const PATH_MAX: usize = 4096;

/// `snprintf(path, PATH_MAX, ...)`: silently truncated (upstream only
/// checks for a negative return, which never happens).
fn path_join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = bcat!(dir, "/", name);
    p.truncate(PATH_MAX - 1);
    p
}

/// `rateLimit()`: a token bucket; true means "drop this message".
pub fn rate_limit(rl: &mut RateLimiter) -> bool {
    if rl.usec_between_msgs == 0 {
        return false;
    }
    let Some((sec, usec)) = sys::gettimeofday() else {
        return false;
    };
    let current_tick = sec.wrapping_mul(1_000_000).wrapping_add(usec) / rl.usec_between_msgs;
    let mut diff = current_tick.wrapping_sub(rl.last_tick);
    if diff > 0 {
        diff = diff.wrapping_add(i64::from(rl.token_bucket));
        rl.token_bucket = if diff < i64::from(rl.bucket_size) { diff as i32 } else { rl.bucket_size };
    }
    rl.last_tick = current_tick;
    if rl.token_bucket <= 0 {
        return true;
    }
    rl.token_bucket -= 1;
    false
}

/// `fscanf(fp, "%llu", &v) > 0` on the contents of `.timestamp`.
fn scan_llu(data: &[u8]) -> Option<u64> {
    let start = data.iter().position(|&b| !crate::num::is_c_space(b))?;
    let p = crate::num::strtoul(&data[start..], 10);
    (p.end > 0).then_some(p.value)
}

/// Open `current` and read (or create) `.timestamp`: the `testdir:` part
/// of `writeLogLine()`.
fn open_output(st: &mut State, oi: usize) -> Result<(), i32> {
    let dir = st.cfg.outputs[oi].directory.clone().expect("checked by caller");
    let perms = st.cfg.outputs[oi].perms;
    loop {
        match sys::stat(&dir) {
            Err(_) => {
                if sys::mkdir(&dir, perms).is_err() {
                    warnp(&bcat!("Can't create [", dir, "]"));
                    return Err(-1);
                }
            }
            Ok(s) if !sys::is_dir(s.mode) => {
                if sys::unlink(&dir).is_err() {
                    warnp(&bcat!("Can't unlink [", dir, "]"));
                    return Err(-1);
                }
                continue;
            }
            Ok(_) => {}
        }
        break;
    }
    let path = path_join(&dir, OUTPUT_DIR_CURRENT);
    let fd = match sys::open(&path, libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND, 0o666) {
        Ok(fd) => fd,
        Err(_) => {
            warnp(&bcat!("Unable to access [", path, "]"));
            return Err(-3);
        }
    };
    let fp = CFile::from_fd(fd);
    let tspath = path_join(&dir, OUTPUT_DIR_TIMESTAMP);
    let existing = match sys::open(&tspath, libc::O_RDONLY, 0) {
        Ok(tfd) => {
            let v = scan_llu(&sys::read_to_end(tfd));
            if v.is_none() {
                sys::close(tfd);
                None
            } else {
                Some((tfd, v.unwrap_or(0)))
            }
        }
        Err(_) => None,
    };
    let (tsfp, creatime) = match existing {
        Some((tfd, v)) => (CFile::from_fd(tfd), v as i64),
        None => {
            let creatime = sys::time_now();
            let Ok(tfd) = sys::open(&tspath, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666) else {
                warnp(&bcat!("Unable to write the timestamp [", tspath, "]"));
                fp.close();
                return Err(-3);
            };
            let mut t = CFile::from_fd(tfd);
            t.write(format!("{}\n", creatime as u64).as_bytes());
            (t, creatime)
        }
    };
    if !tsfp.close() {
        // QUIRK: `current` stays open, but unrecorded (the fd leaks).
        warnp(&bcat!("Unable to close [", tspath, "]"));
        std::mem::forget(fp);
        return Err(-3);
    }
    let o = &mut st.cfg.outputs[oi];
    o.creatime = creatime;
    if o.maxtime != 0 {
        o.creatime = o.creatime.wrapping_sub(creatime.wrapping_rem(o.maxtime));
    }
    o.size = sys::seek_end(fp.fd());
    if let Some(old) = o.fp.replace(fp) {
        // Only reachable through re-entry (a verbose postrotate_cmd logs
        // while the rotation is in progress).  C overwrites the FILE
        // pointer and leaks the stream, which exit() still flushes.
        st.leaked.push(old);
    }
    Ok(())
}

/// `writeLogLine()`.
pub fn write_log_line(
    st: &mut State,
    oi: usize,
    date: &[u8],
    l: &Line<'_>,
    log_format: LogFormat,
    log_severity: bool,
) -> i32 {
    {
        let o = &mut st.cfg.outputs[oi];
        if o.directory.is_none() || l.prg.is_empty() || l.info.is_empty() {
            return 0;
        }
    }
    let sizeof_info = l.info.len().min(MAX_SIGNIFICANT_LENGTH);
    let sizeof_prg = l.prg.len().min(MAX_SIGNIFICANT_LENGTH);
    {
        let o = &mut st.cfg.outputs[oi];
        if rate_limit(&mut o.rate) {
            return 0;
        }
        let dt = &mut o.dt;
        if o.showrepeats == 0
            && sizeof_info == dt.prev_info_len
            && sizeof_prg == dt.prev_prg_len
            && dt.prev_info.get(..sizeof_info) == Some(&l.info[..sizeof_info])
            && dt.prev_prg.get(..sizeof_prg) == Some(&l.prg[..sizeof_prg])
        {
            dt.same_counter = dt.same_counter.saturating_add(1);
            return 0;
        }
        // QUIRK: when the buffer has to grow, the new text is stored but
        // its length isn't, so the next identical line isn't recognized as
        // a repeat (repeats are only folded from the third copy on), and a
        // later shorter line can match the start of the stored text.
        if sizeof_info > dt.prev_info.len() {
            dt.prev_info = l.info[..sizeof_info].to_vec();
        } else {
            dt.prev_info[..sizeof_info].copy_from_slice(&l.info[..sizeof_info]);
            dt.prev_info_len = sizeof_info;
        }
        if sizeof_prg > dt.prev_prg.len() {
            dt.prev_prg = l.prg[..sizeof_prg].to_vec();
        } else {
            dt.prev_prg[..sizeof_prg].copy_from_slice(&l.prg[..sizeof_prg]);
            dt.prev_prg_len = sizeof_prg;
        }
    }
    let now = sys::time_now();
    let mut need_open = st.cfg.outputs[oi].fp.is_none();
    loop {
        if need_open && let Err(e) = open_output(st, oi) {
            return e;
        }
        let o = &mut st.cfg.outputs[oi];
        if o.maxsize != 0 && o.maxtime != 0 && (o.size >= o.maxsize || now > o.creatime.wrapping_add(o.maxtime)) {
            let dir = o.directory.clone().expect("checked above");
            if o.fp.is_none() {
                warn(b"writeLogLine():1366: Internal inconsistency");
                return -6;
            }
            let Some(tm) = sys::gmtime(now) else {
                warnp(b"Unable to find the current date");
                return -4;
            };
            let mut newpath = path_join(&dir, OUTPUT_DIR_LOGFILES_PREFIX);
            let mut stampbuf = vec![0u8; PATH_MAX - newpath.len()];
            let n = sys::strftime(&mut stampbuf, &OUTPUT_DIR_LOGFILES_FMT[OUTPUT_DIR_LOGFILES_PREFIX.len()..], &tm);
            if n == 0 {
                warnp(&bcat!("Path name too long for new path in [", dir, "]"));
                return -2;
            }
            newpath.extend_from_slice(&stampbuf[..n]);
            let path = path_join(&dir, OUTPUT_DIR_CURRENT);
            if let Some(fp) = o.fp.take() {
                fp.close();
            }
            if sys::rename(&path, &newpath).is_err() && sys::unlink(&path).is_err() {
                warnp(&bcat!("Unable to rename [", path, "] to [", newpath, "]"));
                return -5;
            }
            let (post, maxfiles, compress, delay) =
                (o.postrotate_cmd.clone(), o.maxfiles, o.compress, o.compress_delay);
            if let Some(cmd) = post {
                // QUIRK: with -v, spawnCommand() logs "Forked command" right
                // here, re-entering writeLogLine() for this output while
                // `current` is closed.  The inner call reopens it, may rotate
                // again (renaming an empty `current` over the file just
                // rotated: its contents are lost), and the outer call then
                // reopens over it.  If the "Forked command" line alone
                // exceeds maxsize, this repeats forever, forking each time.
                crate::process::spawn_command(st, &cmd, date, l.prg, &newpath);
            }
            rotate_log_files(st, &dir, maxfiles, compress, delay);
            let _ = sys::unlink(&path_join(&dir, OUTPUT_DIR_TIMESTAMP));
            need_open = true;
            continue;
        }
        break;
    }
    let o = &mut st.cfg.outputs[oi];
    let fp = o.fp.as_mut().expect("opened above");
    if o.dt.same_counter > 0 {
        if o.dt.same_counter == 1 {
            fp.write(LAST_OUTPUT_TWICE);
            o.size += LAST_OUTPUT_TWICE.len() as i64;
        } else {
            let text = format!("                - Last output repeated {} times -\n", o.dt.same_counter);
            fp.write(text.as_bytes());
            o.size += text.len() as i64;
        }
        o.dt.same_counter = 0;
    }
    let line = build_log_line(l, log_format, date, false, log_severity);
    fp.write(&line);
    // QUIRK: 5 bytes too many are counted per line, so files rotate
    // early (at maxsize minus 5 bytes per line written).
    o.size += line.len() as i64 + 5;
    if o.flush == FlushMode::Always
        || (o.flush == FlushMode::Default && SYNCHRONOUS.load(std::sync::atomic::Ordering::Relaxed))
    {
        fp.flush();
    }
    0
}

/// `oldlog_identify()`: `log-YYYY-mm-dd-HH:MM:SS`, optionally followed by
/// a compression suffix.  Returns the timestamp.
///
/// QUIRK: anything ending in a compression suffix is accepted after the
/// date ("log-2026-01-01-00:00:00junk.gz" counts as a rotated log).
fn oldlog_identify(st: &State, name: &[u8]) -> Option<i64> {
    let (mut tm, off) = sys::strptime(name, c"log-%Y-%m-%d-%H:%M:%S")?;
    let rest = &name[off..];
    if rest.is_empty() || st.re_compressed.is_match(rest) { Some(sys::timegm(&mut tm)) } else { None }
}

/// `rotateLogFiles()`: delete all but the newest `maxfiles` rotated logs,
/// and compress all but the newest `compress_delay`.
fn rotate_log_files(st: &mut State, dir: &[u8], maxfiles: i32, compress: bool, compress_delay: i32) -> i32 {
    let dirfd = match sys::open(dir, libc::O_DIRECTORY | libc::O_RDONLY, 0) {
        Ok(fd) => fd,
        Err(_) => {
            warnp(&bcat!("Unable to rotate [", dir, "]"));
            return -1;
        }
    };
    let mut ret = 0;
    if let Some(names) = sys::read_dir_names(dir) {
        let mut logs: Vec<(i64, Vec<u8>)> =
            names.into_iter().filter_map(|n| oldlog_identify(st, &n).map(|t| (t, n))).collect();
        // scandir's qsort is glibc's (stable) mergesort.
        logs.sort_by_key(|(t, _)| *t);
        let found = logs.len() as i64;
        for (i, (_, name)) in logs.iter().enumerate() {
            let i = i as i64;
            if i < found - i64::from(maxfiles) {
                if sys::unlinkat(dirfd, name).is_err() {
                    ret = -2;
                }
            } else if compress && i < found - i64::from(compress_delay) && !st.re_compressed.is_match(name) {
                compress_log_file(dirfd, name);
            }
        }
    }
    sys::close(dirfd);
    ret
}

/// `compressLogFile()`: `name` -> `name.gz`, removing `name` on success.
#[cfg(feature = "compress")]
fn compress_log_file(dirfd: sys::Fd, name: &[u8]) {
    let Ok(in_fd) = sys::openat(dirfd, name, libc::O_RDONLY, 0) else {
        return;
    };
    // `sz` as in C: it holds asprintf's length until gzwrite runs, so
    // failures between the two go unreported.
    let mut sz: i64 = 0;
    'work: {
        let Ok(stbuf) = sys::fstat(in_fd) else { break 'work };
        if stbuf.size == 0 {
            // mmap() of an empty file fails.
            break 'work;
        }
        let data = sys::read_to_end(in_fd);
        let out_name = bcat!(name, ".gz");
        sz = out_name.len() as i64;
        let Ok(out_fd) = sys::openat(dirfd, &out_name, libc::O_CREAT | libc::O_RDWR | libc::O_TRUNC, stbuf.mode) else {
            break 'work;
        };
        match sys::gzip_fd(out_fd, &data) {
            None => {
                sys::close(out_fd);
            }
            Some((n, ok)) => {
                sz = i64::from(n);
                if ok && n > 0 {
                    let _ = sys::unlinkat(dirfd, name);
                } else {
                    sz = 0;
                }
            }
        }
    }
    sys::close(in_fd);
    if sz <= 0 {
        warn(&bcat!("Unable to compress [", name, "]"));
    }
}

#[cfg(not(feature = "compress"))]
fn compress_log_file(_dirfd: sys::Fd, _name: &[u8]) {}
