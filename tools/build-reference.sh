#!/bin/bash
# build-reference.sh - build upstream C metalog at the pinned tag, for the differential tests
#
# 20261111 - Created by Claude (claude.ai, Anthropic) - scottfk
#
# Usage: tools/build-reference.sh [DIR]
#
# DIR defaults to ../metalog (next to this checkout), which is where the
# tests look (override with METALOG_REFERENCE=/path/to/metalog).  Builds
# the way Gentoo's app-admin/metalog does on hydra: in-tree (so assert()
# messages say "metalog.c"), --with-unicode --with-compress.  Gentoo's only
# patch touches a comment in metalog.conf, so the binary is upstream's.

set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
dir=${1:-$here/../metalog}
tag=$(cat "$here/tools/REFERENCE_TAG")
url=https://github.com/hvisage/metalog.git

if [[ ! -d $dir/.git ]]; then
	git clone "$url" "$dir"
fi
cd "$dir"
if [[ $(git describe --tags --exact-match 2>/dev/null || true) != "$tag" ]]; then
	git fetch --tags origin
	git checkout -q "$tag"
fi
if [[ ! -x configure ]]; then
	autoreconf -fi
fi
if [[ ! -f Makefile ]]; then
	./configure -q --prefix=/usr --sysconfdir=/etc --with-unicode --with-compress
fi
make -s
echo "reference: $dir/src/metalog ($(./src/metalog -V))"
