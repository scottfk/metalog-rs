# Copyright 1999-2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2
# 20261111 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261115 - Fetch from GitHub; the repo moved off Codeberg - Claude - scottfk

# metalog 20260902 with the daemon from metalog-rs (a strict-compatibility
# Rust port, built live from git).  Upstream's release supplies the
# configuration, man pages and docs; Gentoo's files (init script, conf.d,
# consolelog.sh, systemd unit, the metalog.conf patch) are the same as
# app-admin/metalog-20260902's.  Pin the release with a package.mask of
# >app-admin/metalog-20260902_p9999: metalog-rs ports exactly 20260902.

EAPI=8

METALOG_PV="20260902"
# metalog-rs's Cargo.lock: update together.
CRATES="libc@0.2.191"
RUST_MIN_VER="1.88.0"

inherit cargo git-r3 systemd

DESCRIPTION="Highly configurable replacement for syslogd/klogd (metalog-rs daemon)"
HOMEPAGE="https://github.com/scottfk/metalog-rs https://github.com/hvisage/metalog"
EGIT_REPO_URI="https://github.com/scottfk/metalog-rs.git"
SRC_URI="
	https://github.com/hvisage/metalog/archive/refs/tags/${METALOG_PV}.tar.gz -> metalog-${METALOG_PV}.gh.tar.gz
	${CARGO_CRATE_URIS}
"
# cargo builds in ${S}: the metalog-rs checkout.
S="${WORKDIR}/metalog-rs"
EGIT_CHECKOUT_DIR="${S}"
# Upstream's release.
UP="${WORKDIR}/metalog-${METALOG_PV}"

LICENSE="GPL-2"
# Dependent crate licenses
LICENSE+=" || ( Apache-2.0 MIT )"
SLOT="0"
IUSE="unicode zlib"
# The differential tests need upstream's C build and user namespaces.
RESTRICT="test"

RDEPEND="
	dev-libs/libpcre2:=
	zlib? ( virtual/zlib:= )
"
DEPEND="${RDEPEND}"

src_unpack() {
	cargo_src_unpack
	git-r3_src_unpack
}

src_prepare() {
	default
	# Gentoo's metalog.conf patch (a comment pointing at conf.d).
	pushd "${UP}" >/dev/null || die
	eapply "${FILESDIR}"/metalog-20230719-metalog-conf.patch
	popd >/dev/null || die
}

src_configure() {
	# The same switches as upstream's --with-unicode / --with-compress.
	local myfeatures=(
		$(usev unicode)
		$(usev zlib compress)
	)
	cargo_src_configure --no-default-features
}

src_install() {
	dosbin "$(cargo_target_dir)"/metalog

	# What upstream's `make install` adds: the config and the man pages
	# (configure only substitutes @VERSION@ in them).
	insinto /etc
	doins "${UP}"/metalog.conf
	local m
	for m in metalog.8 metalog.conf.5; do
		sed -e 's/@VERSION@/4/' "${UP}/man/${m}.in" > "${T}/${m}" || die
		doman "${T}/${m}"
	done

	dodoc "${UP}"/{AUTHORS,ChangeLog,README,NEWS,metalog.conf}
	docinto metalog-rs
	dodoc README.md QUIRKS.md

	into /
	newsbin "${FILESDIR}"/consolelog.sh-r1 consolelog.sh

	newinitd "${FILESDIR}"/metalog.initd-r1 metalog
	newconfd "${FILESDIR}"/metalog.confd metalog
	systemd_newunit "${FILESDIR}"/metalog.service-r1 metalog.service
}

pkg_preinst() {
	if [[ -d "${ROOT}"/etc/metalog && ! -e "${ROOT}"/etc/metalog.conf ]] ; then
		mv -f "${ROOT}"/etc/metalog/metalog.conf "${ROOT}"/etc/metalog.conf
		rmdir "${ROOT}"/etc/metalog
		export MOVED_METALOG_CONF=true
	else
		export MOVED_METALOG_CONF=false
	fi
}

pkg_postinst() {
	if ${MOVED_METALOG_CONF} ; then
		ewarn "The default metalog.conf file has been moved"
		ewarn "from ${EROOT}/etc/metalog/metalog.conf to just"
		ewarn "${EROOT}/etc/metalog.conf.  If you had a standard"
		ewarn "setup, the file has been moved for you."
	fi

	elog "metalog is now metalog-rs's (${EROOT}/usr/sbin/metalog)."
	elog "Its known differences from C are in ${EROOT}/usr/share/doc/${PF}/metalog-rs/QUIRKS.md*."
	elog "The running daemon is still the old one: rc-service metalog restart"
	elog "To go back, emerge the C release: emerge -1 =app-admin/metalog-${METALOG_PV}"
}
