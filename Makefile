# Convenience wrapper around cargo for this workspace. Cargo stays
# authoritative; every target here is the command CLAUDE.md already documents.
#
#   make config     fetch dependencies (needs the private registry token)
#   make cli        build just the groundhog CLI
#   make node       run a local node against a hog-devnode binary
#   make help       list every target
#
# The local devnode path needs two repos: this one supplies `groundhog`, which
# serves the public JSON-RPC surface on 9545, and hogd supplies the hog-devnode
# process it supervises.

SHELL := /bin/bash
.DEFAULT_GOAL := help

# Cargo manages its own parallelism.
.NOTPARALLEL:

CARGO ?= cargo
PROFILE ?= release

ifeq ($(PROFILE),release)
CARGO_PROFILE_FLAG := --release
TARGET_DIR := target/release
else ifeq ($(PROFILE),debug)
CARGO_PROFILE_FLAG :=
TARGET_DIR := target/debug
else
CARGO_PROFILE_FLAG := --profile $(PROFILE)
TARGET_DIR := target/$(PROFILE)
endif

# Binaries `make install` copies. The workspace builds more (groundhog-bench,
# rv32-lower, groundhog-open-rpc-spec, groundhog-faucet); add them here when a
# deployment needs them rather than installing the whole target directory.
BINS ?= groundhog hogsim hog-gateway

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
	@echo "  config        cargo fetch --locked; needs the groundhog registry token"
	@echo ""
	@echo "Build"
	@echo "  build         the whole workspace ($(PROFILE))"
	@echo "  cli           just the groundhog binary, all a local node needs"
	@echo "  node          run groundhog node against DEVNODE_BINARY"
	@echo ""
	@echo "Verify"
	@echo "  check         fmt-check, lint, and test together"
	@echo "  test          cargo test --workspace"
	@echo "  lint          clippy with -D warnings"
	@echo "  fmt           rewrite formatting; fmt-check reports instead"
	@echo "  version       workspace, toolchain, and built binary versions"
	@echo ""
	@echo "Install"
	@echo "  install       copy $(BINS) to $(BINDIR)"
	@echo ""
	@echo "Clean"
	@echo "  clean         cargo clean"
	@echo ""
	@echo "Variables: PROFILE BINS PREFIX BINDIR DESTDIR DEVNODE_BINARY NODE_ARGS"

# The SDK and several workspace crates come from the private Groundhog registry,
# so this fails without a token in ~/.cargo/credentials.toml. That token needs
# the "Bearer " prefix, and ~/.cargo/config.toml must set
# global-credential-providers = ["cargo:token"].
.PHONY: config
config:
	$(CARGO) fetch --locked

.PHONY: build
build:
	$(CARGO) build --workspace $(CARGO_PROFILE_FLAG)

.PHONY: cli
cli:
	$(CARGO) build -p groundhog-cli $(CARGO_PROFILE_FLAG)

# Runs the built binary rather than `cargo run` so an unbuilt tree says so
# instead of starting a long compile with a node command tacked on the end.
.PHONY: node
node:
	@test -x $(TARGET_DIR)/groundhog || { \
		echo "no groundhog binary in $(TARGET_DIR); run 'make cli' first" >&2; exit 1; }
	@test -x "$(DEVNODE_BINARY)" || { \
		echo "no hog-devnode at $(DEVNODE_BINARY); build it in the hogd repo or set DEVNODE_BINARY" >&2; exit 1; }
	GROUNDHOG_DEVNODE_BINARY="$(DEVNODE_BINARY)" $(TARGET_DIR)/groundhog node $(NODE_ARGS)

# The pre-PR checklist in CLAUDE.md, in one target.
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

# cargo install --path crates/tools/cli is the other way to get the CLI on PATH;
# it rebuilds into ~/.cargo/bin. This copies what is already built, so the
# binary installed is the one just tested.
.PHONY: install
install:
	@mkdir -p "$(DESTDIR)$(BINDIR)"
	@for b in $(BINS); do \
		if [ -f $(TARGET_DIR)/$$b ]; then \
			install -m 755 $(TARGET_DIR)/$$b "$(DESTDIR)$(BINDIR)/" || exit 1; \
			echo "  $(DESTDIR)$(BINDIR)/$$b"; \
		else \
			echo "  $$b: not built, skipped" >&2; \
		fi; \
	done

.PHONY: clean
clean:
	$(CARGO) clean
