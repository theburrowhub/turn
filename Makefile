# Turn — development commands.
#
# The point of this file is that nobody has to remember which flags matter. Two
# of them are not obvious and are easy to get wrong:
#
#   * `--test-threads=1` on the test target. The pty tests open real pseudo-
#     terminals and schedule real reader threads. Running them serially keeps one
#     load-sensitive integration test from starving another and makes the local
#     gate identical to the release audit.
#   * `--all-targets` on lint. Without it, clippy never looks at test code, and
#     the snapshot tests stop compiling without anyone noticing.
#
# `make verify` is the local umbrella. CI runs the same gates with platform-specific
# test selection for macOS GPU snapshots and Linux headless coverage.

SHELL := /bin/bash
.DEFAULT_GOAL := help

CARGO ?= cargo
TEST_THREADS ?= 1
# Everything except the GUI, whose snapshot tests need a GPU.
HEADLESS_CRATES := -p turn-core -p turn-proto -p turn-store -p turn-pty \
                   -p turn-agents -p turn-hook -p turnd

# A short socket path: the kernel caps a unix socket at ~100 bytes and a repo
# checked out somewhere deep will blow past that with the platform default.
TURN_SOCKET ?= /tmp/turn.sock
TURN_DATA_DIR ?= $(HOME)/.local/share/turn-dev
LINUX_IMAGE ?= rust:1-bookworm
LINUX_TARGET_DIR ?= /tmp/turn-linux-target
CAPABILITY_SOURCE_REPOSITORY ?=

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
verify: product-spec-acceptance fmt-check lint test ## Everything CI checks, in CI's order
	@echo "verify: ok"

.PHONY: product-spec-acceptance
product-spec-acceptance: ## Verify the frozen semantic inventory, proof mapping and mutation resistance
	bash -n scripts/verify-product-capability-source.sh
	@if [ -n "$${TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256:-}" ] || [ "$${CI:-}" = true ]; then \
	  ./scripts/verify-product-spec.sh verify; \
	else \
	  ./scripts/verify-product-spec.sh --verify-local; \
	fi
	./scripts/test-product-spec-gate.sh

.PHONY: product-capability-source-acceptance
product-capability-source-acceptance: ## Verify every frozen capability locator/hash against an audited source clone
	@test -n "$(CAPABILITY_SOURCE_REPOSITORY)" || { \
		echo "set CAPABILITY_SOURCE_REPOSITORY=/path/to/audited-source-repository" >&2; exit 2; \
	}
	./scripts/verify-product-capability-source.sh "$(CAPABILITY_SOURCE_REPOSITORY)"

.PHONY: product-completion-acceptance
product-completion-acceptance: verify ## Require every product requirement to be implemented with evidence
	./scripts/verify-product-completion.sh

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

.PHONY: adapter-acceptance
adapter-acceptance: ## Reproduce dedicated Agent adapters and external-app discovery
	$(CARGO) test -p turn-agents --test contract_gemini --test contract_opencode -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-agents registry::tests --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turn-pty supervisor::tests --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd a_discovered_graphical_app_stays_under_its_parent_without_a_pane --lib -- --test-threads=$(TEST_THREADS)
	$(CARGO) test -p turnd an_authenticated_callback_promotes_inference_without_resetting_the_turn --lib -- --test-threads=$(TEST_THREADS)

.PHONY: performance-acceptance
performance-acceptance: ## Measure the 30-Session/100-Process envelope without opening a window
	$(CARGO) test -p turn-gui both_gui_boundary_queues_have_hard_capacity --lib -- --test-threads=1
	$(CARGO) test -p turn-gui a_large_hierarchy_builds_only_viewport_rows_and_the_reveal_target --lib -- --test-threads=1
	$(CARGO) test -p turnd thirty_sessions_keep_active_and_background_preview_cadence_bounded --lib -- --test-threads=1
	$(CARGO) test -p turn-gui --test performance -- --nocapture --test-threads=1

.PHONY: accessibility-acceptance
accessibility-acceptance: ## Reproduce zoom, motion, contrast, AccessKit and IME acceptance without opening a window
	$(CARGO) test -p turn-gui theme::tests --lib -- --test-threads=1
	$(CARGO) test -p turn-gui terminal::tests::a_compo --lib -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots accessibility_ -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots every_hierarchy_level_is_a_reachable_tree_item -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots maximum_zoom_keeps_the_minimum_window_navigable -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots reduced_motion_keeps_loading_static_and_allows_the_window_to_settle -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots the_command_palette_lists_commands_with_their_shortcuts -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots closing_a_modal_returns_accessibility_focus_to_the_selected_tree_row -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots the_custom_pane_editor_is_a_named_modal_dialog -- --test-threads=1
	$(CARGO) test -p turn-gui --test snapshots a_write_lease_conflict_offers_only_explicit_safe_alternatives -- --test-threads=1

.PHONY: release-acceptance
release-acceptance: ## Prove version matching, update safety and the final macOS bundle
	$(CARGO) test -p turn-proto request::tests -- --test-threads=1
	$(CARGO) test -p turn-proto response::tests -- --test-threads=1
	$(CARGO) test -p turn-gui update::tests --lib -- --test-threads=1
	$(CARGO) test -p turnd update_status --lib -- --test-threads=1
	bash -n scripts/package-macos-app.sh scripts/verify-macos-app.sh scripts/release-macos.sh scripts/install-macos-update.sh scripts/local-update-acceptance.sh
	TURN_INSTALLER_VERSION_SELF_TEST=1 ./scripts/install-macos-update.sh
	@if [ "$$(uname -s)" = Darwin ]; then \
		set -e; \
		tmp="$$(mktemp -d /tmp/turn-release-acceptance.XXXXXX)"; \
		trap 'rm -rf "$$tmp"' EXIT; \
		./scripts/package-macos-app.sh "$$tmp/Turn.app"; \
		./scripts/verify-macos-app.sh "$$tmp/Turn.app"; \
		./scripts/local-update-acceptance.sh "$$tmp/Turn.app" "$$tmp/update"; \
	else \
		echo "release-acceptance: macOS bundle/signature check runs in the macOS CI job"; \
	fi

.PHONY: privacy-acceptance
privacy-acceptance: ## Reproduce local-data inventory, export, retention and deletion without a window
	$(CARGO) test -p turn-store privacy::tests -- --test-threads=1
	$(CARGO) test -p turn-proto --lib contract:: -- --test-threads=1
	$(CARGO) test -p turnd privacy::tests -- --test-threads=1
	$(CARGO) test -p turnd core::requests::privacy::tests -- --test-threads=1
	$(CARGO) test -p turnd --test binary offline_installation_deletion_ -- --test-threads=1
	$(CARGO) test -p turn-gui --bin turn release_commands_are_windowless_and_update_status_parses_both_socket_spellings -- --test-threads=1

.PHONY: mvp-acceptance
mvp-acceptance: ## Run the complete functional v0.1.0 release gate, serially
	@test -s PRODUCT.md
	@test -s docs/MVP_ACCEPTANCE.md
	@test -s docs/REVIEWER_ACCEPTANCE.md
	@test -s docs/ACCESSIBILITY_ACCEPTANCE.md
	@$(MAKE) --no-print-directory verify TEST_THREADS=1
	@$(MAKE) --no-print-directory performance-acceptance
	@$(MAKE) --no-print-directory privacy-acceptance
	@$(MAKE) --no-print-directory release-acceptance
	@echo "mvp-acceptance: functional v0.1.0 gate passed"

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
MACOS_RELEASE_DIR ?= $(CURDIR)/dist

.PHONY: macos-app
macos-app: ## Build an ad-hoc signed local Turn.app for macOS acceptance
	./scripts/package-macos-app.sh "$(MACOS_APP)"

.PHONY: macos-release
macos-release: ## Build, Developer ID sign, notarize and publish channel metadata locally
	./scripts/release-macos.sh "$(MACOS_RELEASE_DIR)"

.PHONY: install-macos-update
install-macos-update: ## Install the stable macOS update without stopping a live daemon
	./scripts/install-macos-update.sh

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
