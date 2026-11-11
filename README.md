# metalog-rs

A drop-in port/reimplementation of metalog in rust. Currently just something fun for me to tinker with in Claude.

## Goals

- **Drop-in compatible with metalog 20260902** ("metalog version 4", the
  version in Gentoo's `app-admin/metalog`): same options and messages, the
  same `metalog.conf` language, and the same files written to the same log
  directories, byte for byte. Upstream quirks are reproduced on purpose and
  listed in [QUIRKS.md](QUIRKS.md). Improvements come later, as deliberate
  changes.
- **Linux only.**
- **Small and boring:** one binary, `libc` as the only crate dependency,
  and the same C libraries upstream links (libpcre2-8, and zlib for
  `compress`), so regexes and `.gz` files behave identically. No async,
  and `unsafe` only in `src/sys.rs`.

## Layout

The modules follow upstream's single `metalog.c`:

| Path | What |
|---|---|
| `src/config.rs` | `metalog.conf` parser and the data model (blocks, outputs, remote hosts, sources) |
| `src/logline.rs` | Sanitizing and parsing received lines, the four output formats, timestamps (`%N`, `%:z`) |
| `src/process.rs` | Routing a line through the blocks, commands, remote UDP hosts, metalog's own messages |
| `src/output.rs` | Writing log directories: duplicates, rate limiting, rotation, retention, compression |
| `src/daemon.rs` | Signals, the klog child, sockets, the poll loop, pid file, daemonizing |
| `src/sys.rs` | Every libc, PCRE2 and zlib call: the only `unsafe` code |
| `src/stdio.rs`, `getopt.rs`, `num.rs`, `strtod.rs`, `bytes.rs` | glibc behavior that the output depends on: stdio buffering, `getopt_long` messages, `strto*`. `getopt.rs`, `num.rs` and `bytes.rs` are copied from openrc-rs. |
| `tests/` | Differential tests (C vs Rust) and the harness ([tests/common/mod.rs](tests/common/mod.rs)) |
| `tests/shim/faketime.c` | LD_PRELOAD clock that makes timestamps and rotation testable |
| `tools/build-reference.sh` | Builds upstream C metalog at [tools/REFERENCE_TAG](tools/REFERENCE_TAG) |

## Building

```sh
make                 # target/release/metalog; USE="unicode zlib" defaults
make UNICODE=no      # upstream's default (no --with-unicode)
make COMPRESS=no     # --without-compress
make install DESTDIR=/some/stage
```

## Testing

```sh
make check
```

This builds the C reference into `../metalog` if it isn't there, then runs
the unit tests and the differential suite. Each differential case runs C
metalog and metalog-rs on the same input and compares the exit status,
stdout, stderr, every file in the log directories (with permissions; `.gz`
files byte for byte), UDP datagrams sent to remote hosts, and the process
title.

How the harness works, in short:

- Each run is "root" in a private user namespace, which metalog requires
  but which needs no privileges. The run uses its own directory under
  `target/difftest/<case>/{c,rust}/` (kept afterwards, for inspection).
- The clock is the faketime shim, so timestamps, `maxtime` rotation and
  rate limits are deterministic.
- A step counts as done when the datagram has left the socket queue and
  metalog is back in `poll()` with no child processes.

`METALOG_DIFF_SHOW=1 cargo test --features unicode -- --nocapture` prints
every outcome, which is useful for checking that a case tests something.

### In the test VM

The kernel log path needs the real root, because a user namespace can't
use `klogctl()`. The cases in [tests/diff_klog.rs](tests/diff_klog.rs) skip
anywhere else. They run in the shared test VM (`hydra-openrc-rs-test`,
snapshot `metalog-base`), which has Gentoo's own `app-admin/metalog` as the
reference:

```sh
METALOG_REFERENCE=/usr/sbin/metalog cargo test --features unicode
```

As real root, the whole suite runs there, and the kernel cases also check:

- records injected through `/dev/kmsg` and real kernel lines (SysRq help);
- the startup backlog;
- `--consolelevel`, and the console level left behind;
- the klog child's death and signals.

They drain the kernel log before each run and run one at a time.

[tools/vm-boot-capture.sh](tools/vm-boot-capture.sh) compares real boots,
with metalog in the default runlevel and Gentoo's stock config. On
2026-11-11, two C boots and one Rust boot were compared. With kernel stack
traces and timestamps filtered out, C vs Rust differed only in ephemeral
ssh ports, exactly as C vs C did. That included `rc-service metalog
buffer`/`unbuffer`, the routing into `pwdfail`/`critical`, and the process
titles.

## Roadmap

1. **Core port (done, branch `phase1-core`).** Everything in `metalog.c`,
   with the differential suite covering the CLI, config parsing, matching,
   formats, timestamps, duplicates, rotation, compression, rate limits,
   commands, signals, remote hosts, daemon mode and startup failures.
2. **Kernel logging in a VM (done, branch `phase2-klog`):** the klog child,
   `--consolelevel` and the startup backlog, diffed against Gentoo's binary.
   Also real boots, with metalog-rs swapped in as the system logger.
3. **Packaging (done, branch `phase3-ebuild`):** a live Gentoo ebuild,
   `app-admin/metalog-20260902_p9999`, a drop-in for `app-admin/metalog`.
   See [contrib/gentoo/README.md](contrib/gentoo/README.md).
4. **Later:** fix quirks as deliberate, separate changes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md) and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## License

GPL-2.0, like upstream metalog (this is a derived work). The files copied
from openrc-rs are BSD-2-Clause-licensed by the same author and are
included here under the GPL.
