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

**The upgraded first vertical is implemented and covered end to end by deterministic tests.** The native
window consumes the production daemon protocol and presents one Workspace → Session → Agent/Tool →
Child hierarchy, user-chosen panes, background subagents, activity previews, temporary panes, a contextual
inspector and the Attention Queue. The Reviewer scenario crosses the real loopback Claude hook transport,
the production daemon/store boundary and a UI reconnect to that same live daemon without opening a Pane or
changing the saved Layout. Separate daemon-restart tests restore durable metadata as `Orphaned`/`Lost` and
prove that nothing is relaunched; they do not claim PTY reattachment.
Primary-checkout conflicts are typed: a Template-origin request can focus the owner or create read-only /
worktree variants without flattening the Template in the client. The daemon preserves its complete Layout,
environment, Attention policy, tmux and naming intent; worktree cwd values are remapped into the isolated
checkout. A read-only Session launches only behind a technical guard; when Turn cannot apply one, launch
remains blocked and model instructions alone never count as enforcement.

That is not the same claim as a released product. Nobody has yet accepted the complete scenario with an
authenticated, currently installed Claude Code binary inside the packaged native app. Packaging,
performance measurement, manual VoiceOver/Orca acceptance, terminal IME sign-off and broader integration
coverage remain open — see `ROADMAP.md` §M8 and §M9.

| Crate | What it is | Status |
| --- | --- | --- |
| `turn-core` | Domain, two-axis state model, event vocabulary, attention subsystem | Built; reproduce its suite |
| `turn-proto` | The daemon↔client protocol: envelope, framing, requests, pushes, cells, view models | Built; protocol v3 |
| `turn-store` | SQLite persistence, migrations, secret redaction and hierarchy repositories | Built; append-only migrations |
| `turn-pty` | Ptys, bounded terminal buffers, process supervision | Built |
| `turn-hook` | Zero-dependency helper for tools that shell out instead of POSTing | Built |
| `turn-agents` | Adapter layer (Claude Code, Codex, heuristics), registry, loopback hook server | Built |
| `turnd` | The daemon that owns PTYs, Sessions, hierarchy, leases and Attention | Built for the automated vertical |
| `turn-gui` | Native GPU window with the unified hierarchy and terminal panes | Built for the automated vertical |

The release audit runs the whole workspace serially, then format, Clippy and the native snapshot suite.
Counts are deliberately not frozen here because this repository changes quickly: reproduce the evidence
with the commands below. There is one test runner; the frontend is Rust, so there is no `pnpm`, no `vitest`
and no second lockfile. The tests are real: `turn-pty` spawns
actual processes on actual ptys and asks the tty itself via `stty size`; `turn-agents` asserts against hook
payloads recorded from a live Claude Code run; `turn-store` writes real SQLite files and searches them for
secrets; `turn-gui`'s snapshot tests render the real widget tree through `wgpu` with no display attached and
diff it against committed PNGs. The snapshot integration target contains 29 tests, 15 of which maintain
committed PNG baselines;
the dense fixture contains 30 Sessions, not a measured 30-Agent performance result. See `ROADMAP.md` for
what each milestone delivered and how it was verified.

**The frontend was replaced.** A Tauri shell around a TypeScript/`xterm.js` frontend was built, rejected by
the product owner on sight, and deleted — `ui/` and `crates/turn-ui` are gone. The window is now native Rust
drawn on the GPU. ADR-039 in `DECISIONS.md` records the decision, why the swap cost ~13k lines of TypeScript
rather than the product, and what it costs from here.

What is still missing, and worth knowing before you build:

- **No authenticated live-CLI acceptance.** The production reducer, protocol, hook transport, persistence
  and UI are joined by tests, but the packaged app has not completed the scenario against a user's live
  Claude account. Treat “Built” as automated evidence, not release acceptance.
- **Accessibility and input need human sign-off.** The hierarchy and terminal panes expose AccessKit
  semantics and the input path preserves composed text, but VoiceOver/Orca and terminal IME behaviour still
  require manual acceptance on supported platforms.
- **Advanced product hardening remains.** Complete context menus, permanent Pane placement choices,
  performance budgets at 30 Sessions, packaging and recovery UX are M9 work.
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
cargo test --workspace --all-targets -- --test-threads=4

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
cargo run -p turn-gui                   # the native window, 1440x900; starts turnd if needed

# Its visual tests. They render the real widget tree through wgpu with no display
# attached and diff against the PNGs in crates/turn-gui/tests/snapshots/.
cargo test -p turn-gui
UPDATE_SNAPSHOTS=1 cargo test -p turn-gui   # re-record after an intended change
```

### First run: create real work

1. In an empty window, click **+ Workspace** in the left hierarchy. **⌘N** is the keyboard route to the
   same first-run flow when no Workspace exists.
2. Enter a name and an existing absolute project directory, then choose **Create and continue**. Turn
   persists the Workspace and opens the Session form; this is not a launcher-only placeholder.
3. Choose the Workspace and Template, name the task, optionally add a note, and choose **Create session**.
   The new Session is selected in the hierarchy and owns the main-checkout write lease.

After first run, **+ Workspace** always creates another Workspace. Select a Workspace and use **+ Session**
or **⌘N** to open the full Session form. **⌘⇧N** is **Quick New**: it creates `Session N` immediately in
the visibly selected Workspace using that Workspace's default Template, then `Coding`, then the first
available Template. It never silently chooses a different Workspace, and a second create is refused while
the first is still awaiting its daemon response.

`turn` reuses a daemon already listening on its socket. When none is listening it starts `turnd` as a
detached companion, so closing the window does not stop the daemon, its PTYs or its agents. Packaged builds
place `turn`, `turnd` and `turn-hook` beside one another; `turnd` resolves the helper beside itself rather
than searching `PATH`. A debug source build falls back to the exact Cargo workspace it was compiled from
and builds both companions before starting the daemon.
`TURN_TURND_BIN` can name an explicit companion binary for an unusual development layout. Companion output
is appended to `turnd.log` under `TURN_DATA_DIR` (or Turn's platform data directory), and a launch failure is
shown in the window.

Starting the daemon yourself remains useful for debugging, but is not required:

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
