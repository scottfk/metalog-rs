# Contributing

Thanks for stopping by. This is a personal project, a Rust port of
[metalog](https://github.com/hvisage/metalog) that I tinker with in my spare time, but
contributions are welcome. Here's how things work.

## What this project is

metalog-rs aims to be a **drop-in replacement** for C metalog. Given the same arguments,
config, environment and input, it should produce the same log files, stdout, stderr,
exit status and side effects, down to the byte. It is not a redesign: if upstream has an
odd edge case, we copy the odd edge case and list it in [QUIRKS.md](QUIRKS.md).

The upstream release we track is pinned in [tools/REFERENCE_TAG](tools/REFERENCE_TAG).
Behaviour is checked against a build of that tag, not against the man pages or against
what seems sensible.

Layout (the modules follow upstream's single `src/metalog.c`):

| Module | What it is |
|--------|------------|
| `src/config.rs` | `configParser()`/`parseLine()` and the data model. |
| `src/logline.rs` | Sanitising and parsing received lines, output formats, timestamps. |
| `src/process.rs` | `processLogLine()`: matching blocks, commands, remote hosts, `doLog()`. |
| `src/output.rs` | `writeLogLine()`: duplicates, rate limits, rotation, compression. |
| `src/daemon.rs` | Signals, the klog child, sockets, the poll loop, pid file. |
| `src/sys.rs` | Every libc, PCRE2 and zlib call. **The only module with `unsafe` code.** |
| `src/stdio.rs`, `getopt.rs`, `num.rs`, `strtod.rs`, `bytes.rs` | glibc behaviour that leaks into output (stdio buffering, getopt messages, `strto*`). |

## What's useful to contribute

- **Behaviour differences.** If metalog-rs does something different from C metalog at
  the pinned tag, that's a bug. Please send the config, the input (the datagrams, or the
  `logger` command), and both outputs. A new differential test case is even better.
- **Quirk fixes, as separate changes.** Fixing something listed in QUIRKS.md is welcome,
  but as its own PR that says which quirk it fixes. Ideally it gets fixed upstream too.
- **Reference bumps.** Moving `tools/REFERENCE_TAG` to a newer upstream release, plus the
  fixes needed so the differential tests pass again.
- **Documentation.** Corrections or additions, especially install notes for different
  distros and init systems.

## What doesn't fit

- **"Improvements" that change behaviour quietly.** New config keywords, changed
  defaults and nicer messages belong upstream in metalog first. Once they ship in a
  tagged release, we'll match them.
- **New dependencies** without a strong reason. The only crate is `libc`, and I'd like to
  keep it that way. PCRE2 and zlib are the same system libraries upstream links, so
  regexes and `.gz` files behave identically. Please don't swap them for the `regex` or
  `flate2` crates.
- **`unsafe` outside `src/sys.rs`.** If you need a new syscall, add a safe wrapper there.

## Building and testing

You need Rust 1.88 or newer (edition 2024), a C compiler, `libpcre2-8` and `zlib`
headers and libraries. For the differential tests you also need the tools to build
upstream metalog (`git`, `autoconf`, `automake`, `autoconf-archive`, `pkg-config`) and
unprivileged user namespaces.

```sh
make                # release build: target/release/metalog
make reference      # build the C reference at the pinned tag into ../metalog
make check          # build the reference if needed and run everything
make clippy         # cargo clippy --all-targets -D warnings
make fmt            # cargo fmt
```

**Before sending a PR, please make sure `make check`, `make clippy` and `make fmt` are
clean.**

### Differential tests

Most testing is done by running the C reference and metalog-rs side by side on the same
input. The harness lives in [tests/common/mod.rs](tests/common/mod.rs):

- Each run is "root" in an unprivileged user namespace, with its own directory under
  `target/difftest/<case>/{c,rust}/`.
- Timestamps come from an LD_PRELOAD clock ([tests/shim/faketime.c](tests/shim/faketime.c)).
- The exit status, stdout, stderr, every file written, UDP datagrams and the process
  title are compared.

When you fix a difference or change behaviour, add cases that cover it, including error
paths and odd input. Unit tests are fine for internal helpers, but a differential test is
what shows the behaviour matches. To check that a case really tests something, look at
what C produced: `METALOG_DIFF_SHOW=1 cargo test --features unicode -- --nocapture`.

The kernel log path (`klogctl()`) can't run in a user namespace; it's tested in a VM.

## Code conventions

- **Port, don't paraphrase.** Each function's doc comment names the upstream function it
  ports (e.g. `` `writeLogLine()` ``). Keep the structure close enough to the C that
  someone can read the two side by side.
- **Bytes, not strings.** Log lines, paths and config values are `Vec<u8>`/`&[u8]`, as
  they are in C. Don't round-trip through `String` where non-UTF-8 input could show up.
- **Output goes through `stdio`**, so buffering and flush order match C stdio.
- **Mark quirks.** Reproduced upstream bugs get a `QUIRK:` comment in the code and a row
  in QUIRKS.md.
- **Every `unsafe` block gets a `// SAFETY:` comment.** The lints enforce this
  (`undocumented_unsafe_blocks`, `unsafe_op_in_unsafe_fn`).
- **Script headers.** Shell scripts, the C shim and the Makefile have a changelog comment
  at the top, one line per change: `YYYYMMDD - <description> - <author>`. Please add a
  line when you change one.

## How to submit changes

The repo lives at <https://codeberg.org/scottfk/metalog-rs>.

1. Fork the repo and create a branch.
2. Make your change, with tests.
3. In the PR description, explain **what** changed and **why**. For a behaviour fix,
   include the input that showed the difference.
4. If you bumped or tested against a different upstream tag, say which one.

There's no CLA. Contributions are accepted under the project's
[GPL-2.0 license](LICENSE), the same license as upstream metalog.

## Issues

You can open issues for questions, not just bugs. If something about the build or the
reference setup is confusing, ask. I can't promise a fast response, but I do read them.

## Code of conduct

Be straightforward and respectful. That's the whole policy. See
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
