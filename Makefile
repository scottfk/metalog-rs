# Makefile - front door for building, testing and installing metalog-rs
#
# 20261111 - Created by Claude (claude.ai, Anthropic) - scottfk
#
#   make                  release build of target/release/metalog
#   make check            build the C reference if needed, run every test
#                         (unit + differential); fails if the reference is missing
#   make install DESTDIR=/some/stage   install like Gentoo's app-admin/metalog
#   make UNICODE=no ...   build without --with-unicode semantics
#   make COMPRESS=no ...  build without zlib (upstream's --without-compress)
#
# Defaults match hydra's Gentoo build (USE="unicode zlib").  The tests need
# the same features as the reference, so `make check` passes them through;
# the reference is built with --with-unicode --with-compress.

CARGO    ?= cargo
PREFIX   ?= /usr
SBINDIR  ?= $(PREFIX)/sbin
SYSCONFDIR ?= /etc
DESTDIR  ?=
UNICODE  ?= yes
COMPRESS ?= yes
REFERENCE ?= ../metalog

FEATURES := $(strip $(if $(filter yes,$(UNICODE)),unicode) $(if $(filter yes,$(COMPRESS)),compress))
CARGO_FEATURES := --no-default-features $(if $(FEATURES),--features "$(FEATURES)")

.PHONY: all build reference check test clippy fmt install clean

all: build

build:
	$(CARGO) build --release $(CARGO_FEATURES)

reference:
	@if [ ! -x $(REFERENCE)/src/metalog ]; then tools/build-reference.sh $(REFERENCE); fi

check: reference
	METALOG_REFERENCE=$(abspath $(REFERENCE))/src/metalog $(CARGO) test $(CARGO_FEATURES)

test: check

clippy:
	$(CARGO) clippy --all-targets $(CARGO_FEATURES)

fmt:
	$(CARGO) fmt

install: build
	install -d $(DESTDIR)$(SBINDIR)
	install -m 755 target/release/metalog $(DESTDIR)$(SBINDIR)/metalog

clean:
	$(CARGO) clean
