# Turn — development commands.
#
# The point of this file is that nobody has to remember which flags matter. Two
# of them are not obvious and are easy to get wrong:
#
#   * `--test-threads=4` on the test target. The pty tests open real pseudo-
#     terminals, a finite kernel resource that recycles slowly. Unbounded
#     parallelism exhausts it and you get an opaque `openpty` failure that looks
#     like a bug in the code rather than in the harness.
#   * `--all-targets` on lint. Without it, clippy never looks at test code, and
#     the snapshot tests stop compiling without anyone noticing.
#
# `make verify` is what CI runs. If it passes here it passes there.

SHELL := /bin/bash
.DEFAULT_GOAL := help

CARGO ?= cargo
TEST_THREADS ?= 4
# Everything except the GUI, whose snapshot tests need a GPU.
HEADLESS_CRATES := -p turn-core -p turn-proto -p turn-store -p turn-pty \
                   -p turn-agents -p turn-hook -p turnd

# A short socket path: the kernel caps a unix socket at ~100 bytes and a repo
# checked out somewhere deep will blow past that with the platform default.
TURN_SOCKET ?= /tmp/turn.sock
TURN_DATA_DIR ?= $(HOME)/.local/share/turn-dev
LINUX_IMAGE ?= rust:1-bookworm
LINUX_TARGET_DIR ?= /tmp/turn-linux-target

.PHONY: help
help: ## Show this help
	@echo "Turn — make targets"
	@echo
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[1m%-18s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "  Sockets and state live at TURN_SOCKET=$(TURN_SOCKET)"
	@echo "  and TURN_DATA_DIR=$(TURN_DATA_DIR)."

# --- the loop you actually run -----------------------------------------------

.PHONY: verify
verify: fmt-check lint test ## Everything CI checks, in CI's order
	@echo "verify: ok"

.PHONY: test
test: ## Run the whole test suite
	$(CARGO) test --workspace -- --test-threads=$(TEST_THREADS)

.PHONY: test-headless
test-headless: ## Run every test that needs no GPU (what Linux CI runs)
	$(CARGO) test --workspace --exclude turn-gui -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui --lib --bins --test links --test onboarding --test scrollback

.PHONY: terminal-acceptance
terminal-acceptance: ## Reproduce the complete terminal interaction contract without opening a window
	$(CARGO) test -p turn-proto -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-pty -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui --test links --test scrollback -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd --test cells -- --test-threads=$(TEST_THREADS)

.PHONY: template-acceptance
template-acceptance: ## Reproduce the complete Template lifecycle without opening a window
	$(CARGO) test -p turn-core model::template -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-proto -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-store template -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd template --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui template --lib -- --test-threads=$(TEST_THREADS)

.PHONY: inspector-acceptance
inspector-acceptance: ## Reproduce contextual Workspace, Session, Agent and Process inspection
	$(CARGO) test -p turn-proto -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-store redact::tests -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd inspector --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui inspector --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui --test snapshots inspector -- --test-threads=$(TEST_THREADS)

.PHONY: lifecycle-acceptance
lifecycle-acceptance: ## Reproduce Command Palette and Workspace/Session lifecycle behaviour
	$(CARGO) test -p turn-proto request::tests -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd favourite_and_pin_are_durable --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd ending_a_session --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd outlived --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd lease --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui session_lifecycle_commands --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui close_turn_defaults -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-gui palette_hierarchy_commands --lib -- --test-threads=$(TEST_THREADS)

.PHONY: test-crate
test-crate: ## Test one crate: make test-crate CRATE=turn-core
	@test -n "$(CRATE)" || { echo "set CRATE, e.g. make test-crate CRATE=turn-core"; exit 1; }
	$(CARGO) test -p $(CRATE) -- --test-threads=$(TEST_THREADS)

.PHONY: lint
lint: ## Clippy over everything, warnings are errors
	$(CARGO) clippy --workspace --all-targets -- -D warnings

.PHONY: fmt
fmt: ## Format the workspace
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Fail if anything is unformatted
	$(CARGO) fmt --all -- --check

.PHONY: build
build: ## Debug build
	$(CARGO) build --workspace

.PHONY: release
release: ## Release build of the three binaries
	$(CARGO) build --release --bin turnd --bin turn --bin turn-hook
	@ls -la target/release/turnd target/release/turn target/release/turn-hook

MACOS_APP ?= $(CURDIR)/dist/Turn.app

.PHONY: macos-app
macos-app: ## Build an ad-hoc signed local Turn.app for macOS acceptance
	./scripts/package-macos-app.sh "$(MACOS_APP)"

# --- running it ---------------------------------------------------------------

.PHONY: run run-ready
run: run-ready ## Rebuild, restart the development daemon, and open Turn
	@echo "opening the window with the freshly built turnd…"
	TURN_SOCKET=$(TURN_SOCKET) TURN_DATA_DIR=$(TURN_DATA_DIR) ./target/release/turn

# Keep the ordering explicit even under `make -j`: release must finish before the
# old daemon is stopped, and the GUI must not start until that stop is confirmed.
run-ready: release
	@$(MAKE) --no-print-directory daemon-stop TURN_SOCKET="$(TURN_SOCKET)"

.PHONY: run-reuse
run-reuse: release ## Rebuild binaries and reconnect without restarting the daemon
	@echo "opening the window and reusing the existing turnd…"
	TURN_SOCKET=$(TURN_SOCKET) TURN_DATA_DIR=$(TURN_DATA_DIR) ./target/release/turn

.PHONY: daemon
daemon: ## Start turnd in the background if it is not already up
	@if [ -S "$(TURN_SOCKET)" ] && ./target/release/turnd --socket "$(TURN_SOCKET)" --data-dir "$(TURN_DATA_DIR)" 2>&1 | grep -q "already"; then \
		echo "turnd: already running on $(TURN_SOCKET)"; \
	else \
		mkdir -p "$(TURN_DATA_DIR)"; \
		./target/release/turnd --socket "$(TURN_SOCKET)" --data-dir "$(TURN_DATA_DIR)" \
			> "$(TURN_DATA_DIR)/turnd.log" 2>&1 & \
		for i in $$(seq 1 40); do [ -S "$(TURN_SOCKET)" ] && break; sleep 0.25; done; \
		if [ -S "$(TURN_SOCKET)" ]; then \
			echo "turnd: listening on $(TURN_SOCKET) (log: $(TURN_DATA_DIR)/turnd.log)"; \
		else \
			echo "turnd: failed to start — see $(TURN_DATA_DIR)/turnd.log"; \
			tail -5 "$(TURN_DATA_DIR)/turnd.log"; exit 1; \
		fi; \
	fi

.PHONY: daemon-stop
daemon-stop: ## Stop the background daemon
	@pattern="[t]urnd --socket $(TURN_SOCKET)"; \
	pids="$$(pgrep -f "$$pattern" || true)"; \
	if [ -z "$$pids" ]; then \
		echo "turnd: not running on $(TURN_SOCKET)"; \
		exit 0; \
	fi; \
	echo "turnd: stopping $$pids (active development sessions will end)"; \
	kill -TERM $$pids; \
	for i in $$(seq 1 50); do \
		pgrep -f "$$pattern" >/dev/null || break; \
		sleep 0.1; \
	done; \
	if pgrep -f "$$pattern" >/dev/null; then \
		echo "turnd: did not stop cleanly; refusing to launch against an old daemon"; \
		exit 1; \
	fi; \
	echo "turnd: stopped"

.PHONY: daemon-log
daemon-log: ## Follow the daemon's log
	@tail -f "$(TURN_DATA_DIR)/turnd.log"

.PHONY: gui
gui: run ## Alias for the fresh development run

# --- looking at the interface -------------------------------------------------

.PHONY: snapshots
snapshots: ## Re-record the UI snapshots, then show what changed
	UPDATE_SNAPSHOTS=1 $(CARGO) test -p turn-gui --test snapshots
	@echo
	@echo "Recorded images — open them and look before committing:"
	@ls -la crates/turn-gui/tests/snapshots/*.png | awk '{printf "  %8s  %s\n", $$5, $$9}'

.PHONY: snapshots-check
snapshots-check: ## Fail if the interface no longer matches its recordings
	$(CARGO) test -p turn-gui --test snapshots

# --- Linux parity -------------------------------------------------------------
#
# macOS and Linux parity is a product requirement, so it is a command rather than
# a hope. This runs the headless suite in a container against the working tree,
# mounted read-only so a container cannot rewrite your checkout, with its own
# target directory so it does not fight the host's.

.PHONY: linux-test
linux-test: ## Run the headless suite in a Linux container
	@command -v docker >/dev/null || { echo "docker is not installed"; exit 1; }
	docker run --rm \
		-v "$(CURDIR)":/work:ro \
		-v "$(LINUX_TARGET_DIR)":/target \
		-w /work -e CARGO_TARGET_DIR=/target -e TURN_DATA_DIR=/tmp/turn-data \
		$(LINUX_IMAGE) \
		bash -c 'set -e; uname -srm; \
			cargo test --workspace --exclude turn-gui -- --test-threads=$(TEST_THREADS); \
			cargo test -p turn-gui --lib --bins --test links --test onboarding --test scrollback'

.PHONY: linux-build
linux-build: ## Prove the window links on Linux
	@command -v docker >/dev/null || { echo "docker is not installed"; exit 1; }
	docker run --rm \
		-v "$(CURDIR)":/work:ro \
		-v "$(LINUX_TARGET_DIR)":/target \
		-w /work -e CARGO_TARGET_DIR=/target \
		$(LINUX_IMAGE) \
		bash -c 'set -e; cargo build --release --bin turnd --bin turn --bin turn-hook; \
			ls -la /target/release/turnd /target/release/turn /target/release/turn-hook'

# --- housekeeping -------------------------------------------------------------

.PHONY: clean
clean: ## Remove build artefacts
	$(CARGO) clean

.PHONY: clean-state
clean-state: ## Delete the development database and scratch config
	@echo "removing $(TURN_DATA_DIR)"
	@rm -rf "$(TURN_DATA_DIR)"

.PHONY: doc
doc: ## Build and open the API documentation
	$(CARGO) doc --workspace --no-deps --open

.PHONY: outdated
outdated: ## Show dependencies with newer versions available
	@$(CARGO) outdated --workspace 2>/dev/null \
		|| echo "cargo-outdated is not installed: cargo install cargo-outdated"

.PHONY: loc
loc: ## Lines of Rust, by crate
	@for c in crates/*/; do \
		n=$$(find $$c -name '*.rs' -exec cat {} + 2>/dev/null | wc -l | tr -d ' '); \
		printf "  %-14s %6s\n" "$$(basename $$c)" "$$n"; \
	done
	@printf "  %-14s %6s\n" "TOTAL" \
		"$$(find crates -name '*.rs' -exec cat {} + 2>/dev/null | wc -l | tr -d ' ')"
