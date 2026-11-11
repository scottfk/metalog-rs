# Upstream quirks, kept on purpose

metalog-rs reproduces metalog **20260902** ("metalog version 4") exactly,
including behavior that looks like a bug. Each item here is marked `QUIRK`
in the source. Fixing one is a deliberate, separate change (ideally upstream
as well), not something to slip into the port.

Severity: **high** means it can lose logs or take the logger down, **medium**
means it's surprising and user-visible, **low** means it's cosmetic or hard to
hit.

## Rotation and output

| Severity | Quirk | Where |
|---|---|---|
| **high** | With `-v` and a `postrotate_cmd`, the "Forked command" message is logged *during* the rotation, re-entering `writeLogLine()` for the same output while `current` is closed. The inner call can rotate again, renaming an empty `current` over the file that was just rotated, so **that file's contents are lost**. If the "Forked command" line alone is longer than `maxsize`, it loops forever instead: rotating, forking the command and leaking an fd each time, until the SIGCHLD handler blocks on the full signal queue and metalog deadlocks. | `output.rs` (`write_log_line`) |
| medium | Duplicate folding ("Last output repeated N times") misses the second copy. When the buffer that stores the previous line grows, its length isn't recorded, so folding starts at the third copy. Also, a later *shorter* line that matches the start of the stored text counts as a repeat of it: "cccccc" followed by "cc", "cc" is logged as "cccccc" + "repeated 2 times". | `output.rs` (`write_log_line`) |
| medium | Every line adds 5 bytes too many to the size count, so files rotate before `maxsize` (by 5 bytes per line). | `output.rs` |
| medium | Rotated logs are named to the second. Several rotations in one second overwrite each other (`rename()` replaces the target), and the earlier rotated contents are lost. | `output.rs` |
| low | `log-<date>` followed by *anything* ending in `.gz`/`.xz`/`.bz2`/`.Z` counts as a rotated log (so `log-2026-01-01-00:00:00junk.xz` is subject to `maxfiles`). | `output.rs` (`oldlog_identify`) |
| low | An empty rotated log can't be compressed (`mmap()` of an empty file fails). Each rotation warns "Unable to compress" again. Failures after the output file is named (`openat`, `gzdopen`) are not reported at all. | `output.rs` (`compress_log_file`) |
| low | If closing `.timestamp` fails, `current` stays open but forgotten (the fd leaks), and the line is dropped. | `output.rs` (`open_output`) |
| low | Any number other than 1 for `compress` means "off" (`compress = 2` doesn't compress). | `config.rs` |

## Parsing log lines

| Severity | Quirk | Where |
|---|---|---|
| medium | The program name is found by skipping to the first `:`, then to the next space. In a normal line the first `:` is in the timestamp ("Nov 11 14:58:00"). In a line without a timestamp, the text up to the first `:` is lost and the program name comes from the message, so `<13>p: hello there: world` logs as program "hello there", message "world". A line with only one `:` is dropped. | `logline.rs` (`parse_log_line`) |
| medium | `<0>` (kern.emerg) and any non-positive priority are rejected, so the most severe kernel messages are dropped. | `logline.rs` |
| low | Control characters become `_`, except that the last character of the line is *dropped* if it is one. Only one, so `"msg\n\n"` becomes `"msg_"`. | `logline.rs` (`sanitize`) |
| medium | Kernel lines that span two reads from the klog child are garbled or dropped: the leftover bytes are moved from the wrong offset (the leftover *length* is used as the start). Verified in the VM: a 7 kB kernel backlog at startup loses 1-5 lines around the 4 kB mark. Which ones depends on how the klog child's writes coalesce in the socket. | `daemon.rs` (`process`) |

## Matching

| Severity | Quirk | Where |
|---|---|---|
| medium | `maximum` doesn't cap the range: a block is skipped when `minimum < priority < maximum`. So `minimum = 3`, `maximum = 6` accepts priorities 0-3 and 6-7. | `process.rs` |
| medium | metalog's own messages ("Got SIGUSR1...", "Child ... exited") are logged with `LOG_SYSLOG` as the whole log code, which makes their priority 0 (emerg). They match `minimum = 1`-style "critical" blocks. | `process.rs` (`do_log`) |
| low | A block's `command` gets the timestamp of the last remote host served, not the block's own `stamp_fmt` (the date buffer is reused). | `process.rs` |

## Configuration

| Severity | Quirk | Where |
|---|---|---|
| medium | `command` checks only that the first word exists, but then executes the whole value, arguments and all, as the program path. A command with arguments never runs, and it logs "exited with return code 127". | `config.rs` |
| medium | In a `configdir`, each file overwrites the result of the previous one, so an error in one file (which stops parsing that file) is ignored if a later file parses cleanly. Only an error in the last file fails the config. | `config.rs` (`parse_config`) |
| low | A line that is not a block header, comment or `key = value` is silently ignored, as is an unquoted value ending in `:` (that line is taken as a new block header). | `config.rs` |
| low | Statements before the first block set defaults for all later blocks. Upstream copies the default block's facility and regex arrays by pointer and then `realloc()`s or `free()`s them in place, which is undefined behavior. This port copies them, which is what happens when `realloc` doesn't move them. | `config.rs` |
| low | An invalid `log_format` warns with an extra blank line and falls back to RFC 5424, not to the real default. | `config.rs` (`translate_log_format`) |
| low | Removing the last remote host from a block (`remote_log = 0`) warns `realloc(0x..., 0) failed: Success`. | `config.rs` |
| low | `ratelimit` values whose interval doesn't fit in 64 bits (`1e-300/s`, `nan/s`) trip an `assert()`, and metalog aborts. | `config.rs` |
| low | A repeated `configdir` warns with `strerror(errno)` appended, even though no call failed (usually ": Success"). The same applies to "Missing number" for `ratelimit`. | `config.rs` |

## Startup, signals, remote hosts

| Severity | Quirk | Where |
|---|---|---|
| **high** | If a remote host's DNS re-lookup fails (every 120 s) while its socket is still open, the next message dereferences the NULL lookup result and metalog crashes (SIGSEGV). | `process.rs` (`send_remote`) |
| medium | If the klog child can't set up `klogctl()` (e.g. no CAP_SYSLOG), it returns into `main()`'s startup code. It reports "Unable to bind sockets", exits and removes the master's pid file. The master then sees the klog child die and exits too. If the klog child's read loop ends, it carries on with the master's startup instead: it binds the remaining sockets (unlinking the master's) and runs a second main loop. | `daemon.rs` (`kernel_source`) |
| medium | The klog child sets the console log level (`-c`, default 7) and then turns the console off, which drops `console_loglevel` to its minimum. The "console on" call only runs if the child's read loop ends. Normally the master's SIGTERM kills the child first, so **after metalog stops, kernel messages stay off the console** (e.g. for the rest of a shutdown). Verified in the VM: `/proc/sys/kernel/printk` goes from `4 4 1 7` to `1 4 1 7` and stays there. | `daemon.rs` (`kernel_source`) |
| low | A klog-child write interrupted by a signal subtracts 1 from the position and re-sends a byte. C can index before the buffer here; the port clamps at 0. | `daemon.rs` |
| low | Termination by signal flushes the log files but not stdout (`_exit()`), and the exit status is always 1. | `daemon.rs` |
| low | A relative `--pidfile` is silently not written, but it is unlinked at exit (relative to `/` after `--daemonize`). | `daemon.rs` (`update_pid_file`) |
| low | A socket path longer than 107 bytes is silently truncated. | `daemon.rs` (`syslog_source`) |
| low | For an unknown or ambiguous *long* option, the error message contains a NUL byte where the option letter should be (`optopt` is 0). | `main.rs` |
| low | `%:z` in `stamp_fmt`: only the first occurrence is expanded, even one that is really an escaped `%%:z`. For negative offsets that aren't whole hours, both parts are signed (`-03:-30`). | `logline.rs` (`stamp`) |

## Intentional differences

These are the only places where metalog-rs knowingly behaves differently.
Each one replaces undefined behavior in C:

- **Signals while busy.** Upstream's handlers flush files and `_exit()`
  from inside the handler, which is only safe while the main loop is
  blocked in `poll()`. metalog-rs does exactly that when it is in `poll()`.
  If a signal arrives mid-message, the work is deferred until that message
  is finished. See `daemon.rs`. In practice this shows when metalog is
  stopped: start-stop-daemon logs "Will stop ..." and sends SIGTERM straight
  away. Over 20 stops in the VM, C logged that line 15 times and
  metalog-rs 19 times.
- **Write boundaries.** One `fprintf` that overflows the stdio buffer may be
  split into `write(2)`s at different offsets. The bytes in the file, and
  when they are flushed, are the same. See `stdio.rs`.
- **Shared default arrays** (above): copied, not aliased.
