# Convenience wrapper around cargo for this workspace. Cargo stays
# authoritative; run `make help` for the available targets.
#
#   make config     fetch dependencies from crates.io
#   make cli        build just the thruput CLI
#   make node       run a local node against a hog-devnode binary
#   make help       list every target
#
# The local devnode path needs two repos: this one supplies `thruput`, which
# serves the public JSON-RPC surface on 9545, and hogd supplies the hog-devnode
# process it supervises.

SHELL := /bin/bash
.DEFAULT_GOAL := help

# Cargo manages its own parallelism.
.NOTPARALLEL:

CARGO ?= cargo
PROFILE ?= release

# Where cargo puts the artifacts, which CARGO_TARGET_DIR relocates. The targets
# below read binaries out of this tree, so it has to track cargo rather than
# assume ./target.
TARGET_ROOT := $(or $(CARGO_TARGET_DIR),target)

# Cargo's reserved profiles share output directories: `dev` and `test` land in
# debug, while `release` and `bench` land in release. `debug` is our alias for
# `dev`, so profile names cannot always be pasted into the directory as-is.
ifeq ($(PROFILE),release)
CARGO_PROFILE_FLAG := --release
TARGET_DIR := $(TARGET_ROOT)/release
else ifeq ($(PROFILE),debug)
CARGO_PROFILE_FLAG :=
TARGET_DIR := $(TARGET_ROOT)/debug
else ifeq ($(PROFILE),dev)
CARGO_PROFILE_FLAG :=
TARGET_DIR := $(TARGET_ROOT)/debug
else ifeq ($(PROFILE),bench)
CARGO_PROFILE_FLAG := --profile $(PROFILE)
TARGET_DIR := $(TARGET_ROOT)/release
else ifeq ($(PROFILE),test)
CARGO_PROFILE_FLAG := --profile $(PROFILE)
TARGET_DIR := $(TARGET_ROOT)/debug
else
CARGO_PROFILE_FLAG := --profile $(PROFILE)
TARGET_DIR := $(TARGET_ROOT)/$(PROFILE)
endif

# Binaries `make install` copies. The workspace builds more (thruput-bench,
# rv32-lower, thruput-open-rpc-spec, thruput-faucet); add them here when a
# deployment needs them rather than installing the whole target directory.
BINS ?= thruput hog-gateway

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DESTDIR ?=

# hog-devnode is built in the hogd repo, which is normally a sibling checkout.
DEVNODE_BINARY ?= $(abspath ../hogd/build/hogd/devnode/hog-devnode)
NODE_ARGS ?=

WORKSPACE_VERSION := $(shell grep -m1 '^version' Cargo.toml | tr -d '"' | awk '{print $$3}')
TOOLCHAIN := $(shell grep -m1 '^channel' rust-toolchain.toml | tr -d '"' | awk '{print $$3}')

.PHONY: help
help:
	@echo "Setup"
	@echo "  config        cargo fetch --locked"
	@echo ""
	@echo "Build"
	@echo "  build         the whole workspace ($(PROFILE))"
	@echo "  build-native  local-only host-tuned workspace; NEVER use for release artifacts"
	@echo "  cli           just the thruput binary, all a local node needs"
	@echo "  node          run thruput node against DEVNODE_BINARY"
	@echo ""
	@echo "Verify"
	@echo "  check         fmt-check, lint, and test together"
	@echo "  test          cargo test --workspace"
	@echo "  lint          clippy with -D warnings"
	@echo "  fmt           rewrite formatting; fmt-check reports instead"
	@echo "  version       workspace, toolchain, and built binary versions"
	@echo "  check-native  count hardware SHA-256 instructions in portable/native BINS"
	@echo ""
	@echo "Install"
	@echo "  install       copy $(BINS) to $(BINDIR)"
	@echo ""
	@echo "Clean"
	@echo "  clean         cargo clean"
	@echo ""
	@echo "Variables: PROFILE BINS PREFIX BINDIR DESTDIR DEVNODE_BINARY NODE_ARGS"

# Every dependency comes from crates.io, so this needs no credentials.
.PHONY: config
config:
	$(CARGO) fetch --locked

.PHONY: build
build:
	$(CARGO) build --workspace $(CARGO_PROFILE_FLAG)

# Local-only: target-cpu=native can fault on other cores; NEVER use for release artifacts.
# A separate target directory keeps the portable build cache valid.
.PHONY: build-native
build-native:
	RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes" $(CARGO) build --workspace $(CARGO_PROFILE_FLAG) --target-dir "$(TARGET_ROOT)/native"

# Advisory only: instruction presence does not prove runtime dispatch uses it.
# Apple objdump adds a vector suffix (sha256h.4s); do not also count sha256h2.
.PHONY: check-native
check-native:
	@set -o pipefail; \
	case "$$(uname -m)" in \
		aarch64|arm64) instruction=sha256h ;; \
		x86_64|amd64) instruction=sha256rnds2 ;; \
		*) echo "hardware SHA-256 check unavailable for this architecture"; exit 0 ;; \
	esac; \
	disassembler=""; \
	for tool in llvm-objdump objdump; do \
		if command -v "$$tool" >/dev/null 2>&1; then disassembler=$$tool; break; fi; \
	done; \
	if [ -z "$$disassembler" ]; then \
		echo "hardware SHA-256 check unavailable: install llvm-objdump or objdump"; exit 0; \
	fi; \
	for dir in "$(TARGET_DIR)" "$(TARGET_ROOT)/native/$(notdir $(TARGET_DIR))"; do \
		for b in $(BINS); do \
			binary="$$dir/$$b"; \
			if [ ! -f "$$binary" ]; then \
				echo "$$binary: not built"; \
			elif count=$$("$$disassembler" -d "$$binary" 2>/dev/null | \
				awk -v op="$$instruction" '$$0 ~ "[[:space:]]" op "([.][[:alnum:]]+)?[[:space:]]" { n++ } END { print n+0 }'); then \
				echo "$$binary: $$count $$instruction instructions"; \
			else \
				echo "$$binary: disassembly unavailable with $$disassembler"; \
			fi; \
		done; \
	done

.PHONY: cli
cli:
	$(CARGO) build -p thruput $(CARGO_PROFILE_FLAG)

# Runs the built binary rather than `cargo run` so an unbuilt tree says so
# instead of starting a long compile with a node command tacked on the end.
.PHONY: node
node:
	@test -x $(TARGET_DIR)/thruput || { \
		echo "no thruput binary in $(TARGET_DIR); run 'make cli' first" >&2; exit 1; }
	@test -x "$(DEVNODE_BINARY)" || { \
		echo "no hog-devnode at $(DEVNODE_BINARY); build it in the hogd repo or set DEVNODE_BINARY" >&2; exit 1; }
	THRUPUT_DEVNODE_BINARY="$(DEVNODE_BINARY)" $(TARGET_DIR)/thruput node $(NODE_ARGS)

# Core workspace format, lint, and test checks.
.PHONY: check
check: fmt-check lint test

.PHONY: test
test:
	$(CARGO) test --workspace

.PHONY: lint
lint:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

.PHONY: fmt
fmt:
	$(CARGO) fmt

.PHONY: fmt-check
fmt-check:
	$(CARGO) fmt --check

.PHONY: version
version:
	@echo "source tree"
	@echo "  workspace version  $(WORKSPACE_VERSION)"
	@echo "  rust toolchain     $(TOOLCHAIN)"
	@echo "  commit             $$(git rev-parse --short=12 HEAD)$$(git update-index --refresh >/dev/null 2>&1; git diff-index --quiet HEAD -- 2>/dev/null || echo '+dirty')"
	@echo "binaries in $(TARGET_DIR)"
	@for b in $(BINS); do \
		if out=$$($(TARGET_DIR)/$$b --version 2>/dev/null); then \
			echo "  $$out"; \
		else \
			echo "  $$b: not built"; \
		fi; \
	done

# cargo install --path crates/cli is the other way to get the CLI on PATH;
# it rebuilds into ~/.cargo/bin. This copies what is already built, so the
# binary installed is the one just tested.
# Every binary in BINS has to be there. Skipping the missing ones and still
# exiting 0 would let a caller that checks the status treat a clean or
# half-built tree as installed. Narrow BINS to install a subset on purpose.
.PHONY: install
install:
	@missing=""; \
	for b in $(BINS); do \
		test -f $(TARGET_DIR)/$$b || missing="$$missing $$b"; \
	done; \
	if [ -n "$$missing" ]; then \
		echo "not built in $(TARGET_DIR):$$missing; run 'make build' first" >&2; \
		exit 1; \
	fi; \
	mkdir -p "$(DESTDIR)$(BINDIR)"; \
	for b in $(BINS); do \
		install -m 755 $(TARGET_DIR)/$$b "$(DESTDIR)$(BINDIR)/" || exit 1; \
		echo "  $(DESTDIR)$(BINDIR)/$$b"; \
	done

.PHONY: clean
clean:
	$(CARGO) clean
