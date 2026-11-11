# Security

metalog-rs is a reimplementation of a system logger. It runs as root, reads messages
that any local user can send to `/dev/log`, reads the kernel log, and runs commands from
its config. Security bugs here matter, even though this is a hobby project.

## Status and supported versions

metalog-rs is pre-release (0.x). There are no maintained release branches: only the
latest `main` gets fixes.

## What counts as a security issue

These are the kinds of bugs I most want to hear about:

- **Untrusted log input.** Crashes, hangs, memory growth, or log injection driven by what
  an unprivileged user sends to `/dev/log`: malformed priorities, oversized or binary
  datagrams, control characters, floods.
- **Memory safety.** Unsoundness in the `unsafe` code in `src/sys.rs` or in the signal
  handling (`src/daemon.rs`), or anything reachable from safe code that causes undefined
  behavior.
- **Files and commands.** Ways to make metalog write, rotate, compress or delete files
  outside its log directories, or to influence the commands it runs (`command`,
  `postrotate_cmd`) and their arguments.
- **Divergence from upstream that weakens security.** For example, a permission or a
  sanitizing step that C metalog applies and metalog-rs skips.

Some upstream behavior is reproduced on purpose and is already listed in
[QUIRKS.md](QUIRKS.md), including a crash after a failed DNS re-lookup for a remote host.
Reports about those are still welcome, especially if the impact is worse than described.
They will be fixed as deliberate changes.

If the same bug is also present in **upstream C metalog**, please report it to the
[metalog project](https://github.com/hvisage/metalog) as well. Upstream's fix is what
metalog-rs will follow.

## How to report

For anything that could be exploited, **please don't put the details in a public issue.**
Email them to me instead:

    scone-void-baggage (at) duck (dot) com

(Replace `(at)` with `@` and `(dot)` with `.`, and remove the spaces.)

If you don't get a reply, open an issue on <https://codeberg.org/scottfk/metalog-rs/issues>
that only says you've sent a security report, without any details.

For low-risk problems, such as hardening suggestions or issues that need root already
(a malicious `metalog.conf`, for instance), a normal public issue is fine.

Useful things to include:

- The commit you tested.
- The config, and the exact input (datagrams, `logger` command) or steps.
- What happened, and what C metalog does at the pinned tag ([tools/REFERENCE_TAG](tools/REFERENCE_TAG)).

## What to expect

This is a spare-time project, so I can't promise a response time, but I'll acknowledge
security reports as soon as I can, fix confirmed bugs on `main`, and credit you in the
commit unless you'd rather I didn't.
