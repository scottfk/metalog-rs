# Gentoo: app-admin/metalog from metalog-rs

`app-admin/metalog/metalog-20260902_p9999.ebuild` is a live ebuild that
installs metalog 20260902 with the daemon replaced by metalog-rs (built from
this repository's `main`). Everything else is the same as Gentoo's
`app-admin/metalog-20260902`:

- `/etc/metalog.conf` (with Gentoo's patch), the man pages and the docs come
  from upstream's release tarball. It's the same distfile as Gentoo's, with
  the same hashes.
- The init script, `conf.d/metalog`, `/sbin/consolelog.sh` and the systemd
  unit are Gentoo's, copied into `files/`.

Notes:

- It keeps the package name `app-admin/metalog`, so `virtual/logger` and
  anything else that depends on metalog is satisfied as before.
- The USE flags are the same as Gentoo's. `unicode` is upstream's
  `--with-unicode`; `zlib` is `--with-compress` (the `compress` cargo
  feature).
- The only crate is `libc`, fetched like a normal distfile (`CRATES`), so no
  `-network-sandbox` is needed. If metalog-rs's `Cargo.lock` changes, update
  `CRATES` and regenerate the Manifest.

It was rehearsed on the test VM (snapshot `metalog-base`) with hydra's
`USE="unicode zlib"`. The rehearsal went:

1. Build C metalog with a binary package.
2. Switch to this ebuild and restart the service.
3. Boot.
4. Roll back to C offline, from the binary package only.
5. Boot.
6. Reinstall this ebuild.

With kernel noise filtered out, the two boots' logs matched. The only
differences were timing: an ssh probe, and whether start-stop-daemon's
"Will stop" line was logged as metalog was stopped (see QUIRKS.md,
"Intentional differences").

## Setup (once)

Put the package in a local overlay (here `localrepo`), then generate the
Manifest:

```sh
cp -r contrib/gentoo/app-admin/metalog /var/db/repos/localrepo/app-admin/
ebuild /var/db/repos/localrepo/app-admin/metalog/metalog-20260902_p9999.ebuild manifest
```

Then, in `/etc/portage`, *add* these lines and keep the ones you already
have. C metalog 20260902 is `~amd64`; if your keywords stop accepting it,
you can't roll back.

```sh
# package.accept_keywords: it's a live ebuild
=app-admin/metalog-20260902_p9999 **
# package.mask: metalog-rs ports exactly 20260902, so nothing newer may replace it
>app-admin/metalog-20260902_p9999
```

## Before switching: a binary package of the C metalog

```sh
quickpkg --include-config=y app-admin/metalog
```

If quickpkg fails with "binpkg appears unsigned", Portage is set to require
signed binary packages (`binpkg-request-signature`, common with Gentoo's
binhost). Run it as `FEATURES=-binpkg-request-signature quickpkg ...`, and
do the same for the `--usepkgonly` emerge below. hydra doesn't set that
feature.

## Switching

```sh
emerge -1 =app-admin/metalog-20260902_p9999
rc-service metalog restart
ls -l /proc/$(pgrep -xo metalog)/exe     # not "(deleted)": the new binary runs
```

`pgrep -a metalog` still shows `metalog [MASTER]` and `metalog [KERNEL]`.
Until the restart, the old C daemon keeps running from its deleted file.
That's harmless, but its logs come from C.

metalog isn't needed to boot, so if something goes wrong, stopping it
(`rc-service metalog stop`) loses only logging.

## Going back

```sh
emerge -1 --usepkgonly =app-admin/metalog-20260902   # or without --usepkgonly, to build it
rc-service metalog restart
```

The differences from C that metalog-rs keeps on purpose (upstream bugs
ported as they are) are in QUIRKS.md, installed in
`/usr/share/doc/metalog-20260902_p9999/metalog-rs/`.
