<div align="center">

# Turn

### Run agents in parallel. Step in when it's your turn.

Turn is a desktop terminal workspace for running, organising, and supervising AI coding agents.
It keeps agents, shells, TUIs, and background work organised—and brings you back only when a task
actually needs attention.

[![CI](https://github.com/TheBurrowHub/turn/actions/workflows/ci.yml/badge.svg)](https://github.com/TheBurrowHub/turn/actions/workflows/ci.yml)
[![Rust 1.85+](https://img.shields.io/badge/Rust-1.85%2B-b7410e?logo=rust)](rust-toolchain.toml)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-6e7681)](#platform-support)
[![License: MIT](https://img.shields.io/badge/license-MIT-7aa2f7)](#license)

</div>

![Turn supervising an agent, subagents, a shell, and a TUI](crates/turn-gui/tests/snapshots/busy_desk.png)

## Why Turn

Traditional terminals organise windows and tabs. Turn organises work.

A **Workspace** represents a persistent project. Each **Session** represents one task inside it. Agents,
subagents, shells, test runners, servers, and TUIs live in a single hierarchy, while the centre of the
window contains only the panes you chose to open.

When several agents work in parallel, Turn distinguishes activity from intervention. Finishing a tool
call is not the same as asking a question; completing an agent turn is not the same as exiting a process.
Actionable demands enter one ordered **Attention Queue**, so multiple agents never fight over focus.

## What makes it different

| Capability | Turn's approach |
| --- | --- |
| Unified navigation | One `Workspace → Session → Agent/Tool → Child` tree; no duplicate persistent navigators |
| Background subagents | Discovered under their parent without opening panes, changing layout, or stealing focus |
| Agent handoffs | Review a bounded, redacted context packet and explicitly pass it to another Agent in the same Session |
| Attention management | Per-session policies, badges, notifications, and one ordered Next Attention action |
| Real terminal workloads | PTYs with ANSI colour, alternate screen, mouse input, resize, bounded durable scrollback, shells, and TUIs |
| Stable layouts | Nested splits, reusable presets, drag-to-reorder, resize, balance, zoom, and per-session persistence |
| Checkout safety | One write lease for the main checkout; extra sessions are read-only or isolated in worktrees |
| Honest recovery | Restore layout and metadata without silently rerunning saved commands or destructive work |
| Integration without forks | Structured hooks where available, wrappers and heuristics where useful, generic terminal otherwise |

## Agent and tool support

- **Claude Code** — structured hooks plus terminal/process observation.
- **OpenAI Codex CLI** — structured hooks and notifications, with graceful fallback.
- **Other agent CLIs** — run through the generic terminal adapter even without a dedicated integration.
- **Shells and TUIs** — ordinary interactive processes remain first-class: shells, test runners, servers,
  logs, file explorers, `lazygit`, `btop`, and similar tools.

An absent integration never prevents a command from running. It only changes how confidently Turn can
describe its semantic state.

## Current status

Turn is an early-stage product with a working vertical, not a finished distribution.

Implemented today:

- Native GPU desktop window written in Rust.
- Workspace and Session creation with an optional task name.
- Portable first-run preset: two shell columns, with no dependency on optional tools.
- Visual layout editor and reusable layout presets.
- Persistent PTYs, terminal panes, process hierarchy, subagent previews, and temporary panes.
- Review-before-send context handoffs between controllable Agents in one Session.
- Claude Code and Codex adapter infrastructure.
- Attention policies, permission context, queue ordering, and typing-aware focus protection.
- SQLite persistence, write leases, safe restart recovery, and explicit process relaunch.
- macOS-enforced read-only Sessions: shells, Agents and child processes can inspect a checkout while
  Seatbelt blocks writes to it and its external Git metadata; unsupported platforms keep processes stopped.
- Automated macOS and Linux builds plus native UI snapshot coverage.

Still before a release-quality build:

- Signed installers, packaging, automatic updates, and a supported upgrade channel.
- Broad authenticated acceptance against current agent CLI releases.
- Measured performance acceptance at the full 30-session target.
- Manual VoiceOver, Orca, and IME sign-off.

The detailed delivery state and remaining risks live in [ROADMAP.md](ROADMAP.md).

## Quick start

### Prerequisites

- Rust 1.85 or newer; the repository pins the toolchain in [rust-toolchain.toml](rust-toolchain.toml).
- A C toolchain for bundled SQLite.
- macOS for the current priority experience, or Linux with a display and Vulkan driver at runtime.

### Build and run

```sh
git clone git@github.com:TheBurrowHub/turn.git
cd turn
make run
```

`make run` builds the release binaries, stops the previous development daemon, and opens the freshly
built app. Use `make run-reuse` when you deliberately want to reconnect without restarting the daemon,
or `make help` to see the complete development command set.

You can also run the native window directly during development:

```sh
cargo run -p turn-gui
```

## First session

1. Select **+ Workspace**, choose **Browse…**, and pick an existing project directory. Turn derives the
   Workspace name from the folder; you can edit it before creating the Workspace.
2. Select **+ Session** or press **⌘N**. A Session name is optional.
3. Choose the built-in **Two Shells** preset or create a layout from the same screen.
4. Use **+ Pane** to add a shell or agent, and **Layout** to redistribute or save the current arrangement.
5. Use **Next Attention** to jump to the next agent that actually needs you.

To pass useful context without copying a terminal transcript, right-click an Agent in the hierarchy and
choose **Pass context to Agent…**, or search for it in the Command Palette. Select an idle destination Agent in the same Session, optionally add an
instruction, review the exact redacted payload, then send it. Preparing the handoff writes nothing; sending
submits it once to the destination Agent without opening a pane or changing the layout.

To finish work, choose **Session → End session…** or press **⌘⇧K**. This stops Turn-owned processes while
keeping the Session's layout and history. **Detach all views · keep running** only closes the views.

## Restart and recovery

Turn never silently reruns saved commands after a daemon restart.

For a main-checkout Session, choose **Confirm write access**, then **Start pane** or **Start all**. If a
process survived but the new daemon cannot control it, stop that process outside Turn and choose
**Check & confirm access**. The daemon revalidates ownership at that moment.

Archiving Sessions or Workspaces never deletes the project directory.

## Architecture

Turn is one Rust workspace with a daemon-owned runtime and a thin native client.

| Crate | Responsibility |
| --- | --- |
| `turn-gui` | Native `eframe`/`egui` desktop interface rendered through `wgpu` |
| `turnd` | Authoritative owner of PTYs, Sessions, hierarchy, write leases, and Attention |
| `turn-pty` | PTY processes, private bounded journals/checkpoints, replay, resize, signals, and supervision |
| `turn-agents` | Claude Code, Codex, heuristic, and generic terminal adapters |
| `turn-store` | SQLite persistence, migrations, hierarchy records, and secret redaction |
| `turn-proto` | Versioned daemon/client protocol, requests, events, terminal cells, and view models |
| `turn-core` | Domain model, process/turn state machines, layouts, events, and Attention policy |
| `turn-hook` | Small helper for agent integrations that report by spawning a command |

The daemon owns runtime truth. The GUI renders revisioned projections and never invents process state,
relationships, permissions, or write authority.

## Safety principles

- A heuristic can badge a Session, but it can never move focus.
- Agent output is never interpreted as a command for Turn to execute.
- Closing a pane never terminates the process behind it.
- A main checkout has at most one active writing Session.
- Permission prompts show the exact Session, process, command, and working directory available to Turn.
- Restore never relaunches a process until the user explicitly asks.

See [SECURITY.md](docs/SECURITY.md) for the complete threat model.

## Development

```sh
# Format, lint, and test in the same order as CI
make verify

# Run the complete test suite
make test

# Update and inspect native UI snapshots after an intentional visual change
make snapshots
```

The PTY and agent integration suites use real operating-system resources. Test concurrency is intentionally
bounded by the Makefile so local and CI runs do not exhaust PTYs or file descriptors.

## Documentation

- [PRODUCT.md](PRODUCT.md) — product requirements, principles, use cases, and acceptance criteria.
- [ARCHITECTURE.md](ARCHITECTURE.md) — module boundaries, integration levels, security, and performance.
- [DECISIONS.md](DECISIONS.md) — architectural decision records and their trade-offs.
- [ROADMAP.md](ROADMAP.md) — milestones, open risks, technical debt, and release work.
- [Unified hierarchy upgrade](docs/UNIFIED_HIERARCHY_UPGRADE.md) — tree, Agent/Pane separation,
  write leases, previews, and persistence contracts.
- [Protocol](docs/PROTOCOL.md) — versioned daemon/client wire contract.
- [Contributing](CONTRIBUTING.md) — project conventions and invariant-preserving workflow.

## Platform support

| Platform | Status |
| --- | --- |
| macOS | Primary development and native snapshot platform |
| Linux | Built and tested in CI; runtime needs a display and Vulkan driver |
| Windows | Not currently supported |

## License

MIT.
