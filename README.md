# Turn

**Run agents in parallel. Step in when it's your turn.**

Turn is a desktop terminal workspace for running, organising and supervising AI coding agents on macOS
and Linux. Agents work in parallel; Turn tells you which one needs you.

It runs each agent in a real pty inside a persistent **Workspace**, organises work into **Sessions**,
tracks the agent hierarchy, and maintains one ordered **Attention Queue** whose top item is the thing you
should look at next. It integrates with the agent CLIs you already use — Claude Code, Codex CLI, and
anything else interactive — through mechanisms those tools already ship, without modifying your
configuration.

Turn is not an agent, not a model client and not a chat interface. It supervises; you decide.

- `PRODUCT.md` — the problem, the principles, MVP scope, and acceptance criteria with test evidence.
- `ARCHITECTURE.md` — module map, integration levels, the confidence ladder, security model, performance
  budget.
- `DECISIONS.md` — the ADR log, including the empirical findings that changed the design.
- `ROADMAP.md` — milestones, risks, open decisions, technical debt.
- `CONTRIBUTING.md` — house style and the rules a change must not break.

---

## Current state, honestly

**The daemon and the library crates exist and their tests pass. The window is being rebuilt from scratch,
and Turn has still not been shown to run end to end against a real agent.** That is the honest summary: the
first frontend was finished and rejected (ADR-039), the second one currently draws chrome and nothing else,
and the moment where one real Claude Code session drives one real pane in a running window has not been
reached — see `ROADMAP.md` §M6 and §M7.

| Crate | What it is | Status |
| --- | --- | --- |
| `turn-core` | Domain, two-axis state model, event vocabulary, attention subsystem | Built, 120 tests |
| `turn-proto` | The daemon↔client protocol: envelope, framing, requests, pushes, cells, view models | Built, 172 tests |
| `turn-store` | SQLite persistence, migrations, secret redaction, seven repositories | Built, 140 tests |
| `turn-pty` | Ptys, bounded terminal buffers, process supervision | Built, 47 tests |
| `turn-hook` | Zero-dependency helper for tools that shell out instead of POSTing | Built, 21 tests |
| `turn-agents` | Adapter layer (Claude Code, Codex, heuristics), registry, loopback hook server | Built, 169 tests |
| `turnd` | The daemon that assembles all of the above | Built, 88 tests |
| `turn-gui` | The window: native Rust drawn on the GPU, no webview | In progress, 80 tests |

**`cargo test --workspace` ran and passed 730 tests in one green run on 2026-08-04**, immediately after the
webview frontend was retired. One command and one test runner: the frontend is Rust, so there is no `pnpm`,
no `vitest` and no second lockfile. Several crates are modified daily and two of them are being extended as
this is written, so the number is higher than 730 by now — reproduce it with the commands below rather than
trusting it. The tests are real: `turn-pty` spawns
actual processes on actual ptys and asks the tty itself via `stty size`; `turn-agents` asserts against hook
payloads recorded from a live Claude Code run; `turn-store` writes real SQLite files and searches them for
secrets; `turn-gui`'s snapshot tests render the real widget tree through `wgpu` with no display attached and
diff it against committed PNGs. See `ROADMAP.md` for what each milestone delivered and how it was verified.

**The frontend was replaced.** A Tauri shell around a TypeScript/`xterm.js` frontend was built, rejected by
the product owner on sight, and deleted — `ui/` and `crates/turn-ui` are gone. The window is now native Rust
drawn on the GPU. ADR-039 in `DECISIONS.md` records the decision, why the swap cost ~13k lines of TypeScript
rather than the product, and what it costs from here.

What is still missing, and worth knowing before you build:

- **No end-to-end run.** Nothing below has been observed working together with a real agent, so treat
  "Built" as "compiles, and its own contract is tested", not as "works".
- **`turn-gui` is a spike, not a window.** It settles the stack — cells, theme, chrome, snapshot testing —
  and has no daemon connection, keymap, palette, tree panel or effect channels yet. M6 is reopened in
  `ROADMAP.md`, and M7 is blocked behind it again.
- **This window cannot be used with a screen reader yet, and has no IME work.** Both were free with a
  webview and neither is now. `every_session_row_is_reachable_by_its_accessible_name` is committed failing
  and `#[ignore]`d rather than deleted, because it is a real gap. `ROADMAP.md` §Risks 2b.
- **The snapshot baselines are macOS-only.** They were recorded through Metal, and `egui_kittest` allows no
  differing pixels by default, so CI runs the comparison on macOS and says why in the workflow instead of
  skipping it silently. Linux still compiles the snapshot target every push.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` are both clean.

---

## Building and testing

### Prerequisites

- **Rust** stable. `rust-toolchain.toml` pins the channel and requests `rustfmt` and `clippy`; the
  workspace MSRV is 1.85.
- **A C toolchain.** `rusqlite` is used with the `bundled` feature, so SQLite is compiled from source and
  there is no system library to install.
- **Nothing else.** There is no node, no pnpm and no system webkit any more: the window is Rust. On Linux it
  needs no packages to build or link either — X11, Wayland, xkbcommon and Vulkan are all reached by `dlopen`
  rather than linked — though *running* it of course needs a display and a working Vulkan driver.
- **To run `turn-gui`'s snapshot tests on a headless Linux box** you need a software Vulkan device:

  ```sh
  sudo apt-get install -y mesa-vulkan-drivers libvulkan1
  ```

  The committed baselines were recorded on macOS/Metal, so a Linux run needs its own baseline first — see
  `.github/workflows/ci.yml`, which spells this out where CI can be seen to be honest about it.

### Commands

```sh
# Everything. This is what CI runs.
cargo test --workspace -- --test-threads=4

# One crate
cargo test -p turn-core
cargo test -p turn-pty -- --test-threads=4

# --test-threads=4 matters: the turn-pty tests open real ptys, and a runner that
# exhausts the pty table fails with a confusing openpty error rather than a test
# failure.

# The checks CI runs, in CI's order
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Build the binaries
cargo build --release --bin turnd
cargo build --release --bin turn        # the window
cargo build --release --bin turn-hook
```

The window:

```sh
cargo run -p turn-gui                   # the native window, 1440x900

# Its visual tests. They render the real widget tree through wgpu with no display
# attached and diff against the PNGs in crates/turn-gui/tests/snapshots/.
cargo test -p turn-gui
UPDATE_SNAPSHOTS=1 cargo test -p turn-gui   # re-record after an intended change
```

The window does **not** start `turnd` — the daemon's lifetime is deliberately longer than the window's, so
start it first:

```sh
cargo run --bin turnd &
cargo run -p turn-gui
```

Test names are full sentences describing the guaranteed behaviour, so `cargo test -- --list` is a readable
specification:

```sh
cargo test --workspace -- --list | grep ': test'
```

### Environment

- `TURN_DATA_DIR` overrides where `turn-store` puts its database. Resolution is a pure function of an
  explicit override and this variable, so tests never mutate process-global state that other tests read.
- `TURN_HOOK_URL` is how `turn-hook` learns where to POST when the agent's configuration cannot carry an
  argument.
- `TURN_NODE_ID` is set on every process Turn spawns, so the supervisor can attribute strays.

---

## Repository layout

```
Cargo.toml                     workspace root; all shared dependency versions live here
rust-toolchain.toml            pinned channel + rustfmt/clippy
.github/workflows/ci.yml       macOS and Linux matrix from the first milestone; one cargo job, no node

crates/
  turn-core/                   domain layer — no I/O, no clock reads inside logic
    src/ids.rs                 prefixed typed-id newtypes
    src/state.rs               Lifecycle × Turn, and the derived DisplayState
    src/event.rs               TurnEvent, EventKind, Confidence, EventSource
    src/model/                 Workspace, Session, ProcessNode/SessionTree, Layout/Pane, Template
    src/attention/             policy, queue, focus governor, manager (emits Effects)

  turn-pty/                    ptys and terminals
    src/process.rs             PtyProcess: spawn, write, resize, subscribe, replay, exit reporting
    src/buffer.rs              bounded byte ring + bounded vt100 screen; OSC 52 refusal; title sanitising
    src/supervisor.rs          on-demand process-table scans; conservative classification

  turn-agents/                 the only tool-specific code in the workspace
    src/adapter.rs             the AgentAdapter trait, IntegrationLevel, Capabilities, HookEndpoint
    src/claude.rs              Claude Code — Structured, HTTP hooks via --settings
    src/codex.rs               Codex CLI — inline TOML hooks + notify, degrading to Wrapper
    src/heuristic.rs           output inference, capped at InferredHigh, stands down in a TUI
    src/registry.rs            adapter selection that always answers and always explains
    src/server.rs              the loopback hook receiver: 127.0.0.1, per-node tokens
    src/risk.rs                permission risk rating — display and ordering only
    tests/fixtures/            payloads captured from a live Claude Code 2.1.221 run

  turn-hook/                   zero-dependency helper; exits 0 whatever happens
  turn-store/                  SQLite: migrations, codec, redact, location, repo/*
  turn-proto/                  envelope, request, response, events, framing, bytes, view/*
  turnd/                       the daemon — built, never yet run against a real agent
    src/core/                  the single owner of state: spawn, supervise, restore, events, requests
    src/server/                the unix-socket accept loop and per-connection tasks

  turn-gui/                    the window: native Rust drawn on the GPU (eframe/egui over wgpu)
    src/cells.rs               a pane's screen as cells; converted from the daemon's parsed screen
    src/theme.rs               the palette, and state_marker(): a colour and a glyph, never one alone
    src/view.rs                status bar, permission banner, sidebar, terminal pane, attention queue
    tests/snapshots.rs         the widget tree rendered through wgpu with no display, diffed against PNGs
    tests/snapshots/           the committed baselines — recorded on macOS/Metal

docs/PROTOCOL.md               the wire protocol, kept honest by turn-proto's catalogue tests
```

---

## The one rule to know before reading the code

**A heuristic can never move the user's focus.** Terminal-output inference is capped at
`Confidence::InferredHigh` by its `EventSource`, and any focus action arising from a confidence below
`Integrated` degrades to a badge. It is enforced twice, independently, and tested at each point. A missed
notification costs a glance; a false focus change costs the thought you were holding, and teaches you not
to trust the tool.

Everything else follows from that asymmetry. See `CONTRIBUTING.md`.

## Licence

MIT.
