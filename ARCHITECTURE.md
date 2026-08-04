# Turn — Architecture

This document describes the system as it is built and as it is intended, and it distinguishes the two
everywhere. Rationale for the choices below lives in `DECISIONS.md`.

## 0. Status vocabulary and current state

The workspace is under active construction, so every module below carries one of three statuses:

- **Built** — declared in its crate's `lib.rs`, compiled, and covered by tests that run and pass.
- **In progress** — substantial code exists and is being written right now. Nothing about its behaviour is
  verified here.
- **Not started** — placeholder or empty directory.

Each crate was surveyed on its own, with `cargo test -p <crate>`. **Several crates were being modified
during the survey**, so the figures below are observations taken at different moments rather than one
simultaneous total — which is the reason the instruction is to reproduce them, not to trust them.

| Crate | Status | Tests | Notes |
| --- | --- | --- | --- |
| `turn-core` | Built | 120 | Domain, two-axis state, attention. |
| `turn-proto` | Built | 172 | Envelope, framing, requests, responses, pushes, bytes, cells, view models. |
| `turn-store` | Built | 140 | SQLite, migrations, redaction, seven repositories. |
| `turn-pty` | Built | 47 | Ptys, buffers, supervision. |
| `turn-hook` | Built | 21 | The `turn-hook` helper binary and its library. |
| `turn-agents` | Built | 169 | Adapter trait, Claude Code, Codex, heuristics, registry, hook server, risk. |
| `turnd` | Built | 88 | `config`, `paths`, `instance`, `server`, `core/*`; `main.rs` is a real entry point. |
| `turn-gui` | In progress | 80 | The native window: cells, theme, view, keymap, panes, transport, plus `wgpu` snapshots. |

There is one command and one test runner: the frontend is Rust now (ADR-039), so there is no `pnpm`, no
`vitest` and no second lockfile.

```sh
cargo test --workspace -- --test-threads=4
```

**These figures are a snapshot and this is the one section where that caveat is load-bearing.** The whole
workspace was green together at **730** in one run immediately after the webview frontend was retired
(2026-08-04). `turn-proto` and `turn-gui` have both been extended since — the protocol gained a cell
representation and the window gained a keymap, a pane arranger and a daemon transport — and `turnd` is being
changed to match, so the total is higher than 730 and moving. Reproduce it; do not quote it.

What is **not** done, so the statuses above are not read as "finished": nothing has been run end to end
against real agents in a real window — see `ROADMAP.md` §M7 — and `turn-gui` is a spike that settles the
stack rather than a finished window. One snapshot test,
`every_session_row_is_reachable_by_its_accessible_name`, is committed `#[ignore]`d because it does not pass:
the session rows are painted rather than composed from widgets, so `kittest` cannot find them in the
accessibility tree. It is the count's one ignored test and it is a real gap, not a placeholder.

`cargo test --workspace -- --test-threads=4` is what CI runs and is the right command once the daemon and the
adapter layer settle; while `turnd` and `turn-agents` are mid-write it will also build those, so a failure
there says nothing about the five settled crates.

**Nothing has yet been shown to run end to end.** `turnd` is the caller that assembles the pieces and it is
being written now: its modules and 71 test functions exist, but no claim in this document about its
behaviour has been verified. Everything below marked "intended" for the daemon should be read as the
design it is being built to, not as observed behaviour.

---

## 1. Shape of the system

Turn is one Rust cargo workspace: six library crates, the daemon, and the window. One process owns the work
(`turnd`, the daemon); another renders it (`turn-gui`). They talk over a protocol rather than sharing memory,
because the whole point of the daemon is that the UI can go away.

```
                     ┌──────────────────────────────────────────────────┐
                     │  turn-gui  (eframe/egui on wgpu — native, no     │
                     │  webview)                      IN PROGRESS       │
                     │  sidebar · panes painted cell by cell ·          │
                     │  attention queue · permission banner             │
                     └───────────────────┬──────────────────────────────┘
                                         │ turn-proto  BUILT
                                         │ newline-delimited JSON over a unix socket
                                         │ ── Request  → Response
                                         │ ── ServerMessage pushes (state, cells/output, effects)
                     ┌───────────────────┴──────────────────────────────┐
                     │  turnd — the daemon           IN PROGRESS        │
                     │                                                  │
                     │  owns: pty handles · session registry ·          │
                     │  one AttentionManager · the hook server ·        │
                     │  supervisor scan timing · all store writes ·     │
                     │  the ONLY place a pty fact and an agent fact     │
                     │  are joined                                      │
                     └──┬───────────┬──────────────┬──────────────┬─────┘
                        │           │              │              │
        ┌───────────────▼──┐ ┌──────▼───────────┐ ┌▼───────────┐ ┌▼──────────────────┐
        │ turn-pty  BUILT  │ │ turn-agents      │ │ turn-core  │ │ turn-store        │
        │                  │ │ BUILT            │ │ BUILT      │ │ BUILT             │
        │ PtyProcess       │ │                  │ │            │ │                   │
        │ TerminalBuffer   │ │ AgentAdapter     │ │ ids        │ │ Store facade      │
        │ ProcessSupervisor│ │ claude · codex   │ │ event      │ │ migrations        │
        │                  │ │ heuristic        │ │ state      │ │ redact · codec    │
        │                  │ │ registry         │ │ model      │ │ repo/{workspace,  │
        │                  │ │ server (127.0.0.1)│ │ attention  │ │  session, node,   │
        │                  │ │ risk             │ │            │ │  event, attention,│
        └────────┬─────────┘ └────┬─────────────┘ └─────▲──────┘ │  template,        │
                 │                │                     │        │  settings}        │
      ┌──────────▼───────────┐    │                     │        └───────────────────┘
      │ real ptys            │    │                     │
      │ agent CLIs · shells  │    │                     │
      │ TUIs · test runners  │    │                     │
      └──────────────────────┘    │                     │
                                  │                     │
   ┌───────────────┐   POST   ┌───▼──────────────┐      │
   │ turn-hook     │─────────▶│ hook server      │      │
   │ BUILT         │  http    │ 127.0.0.1 only   │      │
   │ (for tools    │          │ per-node token   │      │
   │  that shell   │          │ answers instantly│      │
   │  out: Codex)  │          └───┬──────────────┘      │
   └───────────────┘              │                     │
                          ┌───────▼─────────────────────┴──────┐
                          │  TurnEvent — the one vocabulary     │
                          │  every signal funnels into,         │
                          │  carrying a Confidence it cannot    │
                          │  lie about                          │
                          └─────────────────────────────────────┘
```

The dependency graph is strictly acyclic, and `turn-core` sits at the bottom with no I/O at all:

```
turn-core                                      ← turn-pty
turn-core, turn-pty                            ← turn-agents
turn-core                                      ← turn-proto
turn-core                                      ← turn-store
(nothing at all)                               ← turn-hook
turn-core, turn-proto, turn-store,
turn-pty, turn-agents                          ← turnd
turn-core, turn-proto                          ← turn-gui
```

`turn-gui` sits at the same level as `turnd` and depends on neither it nor anything below `turn-proto`. It
takes `turn-core` for the shared vocabulary — `DisplayState`, `Risk` — and `turn-proto` for the wire types,
and nothing else: no store, no pty, no agents. That is the boundary that made replacing the frontend cheap
(ADR-039), and it is worth keeping narrow for the same reason.

`turn-agents` depends on `turn-pty` only for `ScreenSnapshot`, which the heuristic layer classifies.
Joining "this pty exited" to "this Agent's hook said its turn ended" is the daemon's job, and keeping
that seam in one place is deliberate: it is the only spot where a wrong join can silently corrupt state.

`turn-hook` has **no dependencies at all**, on purpose. It runs inside the user's agent process tree,
possibly on every tool call, so it must start in microseconds and cannot afford an async runtime or a
TLS stack to POST a few hundred bytes to loopback. Its HTTP client is hand-written over `TcpStream`.

---

## 2. turn-core — domain, state, attention

**Status: Built. 116 tests.** No I/O, no pty, no database, no UI. This is why the rules that matter can
be tested exhaustively without spawning a process, and why every function that needs the time takes
`now_ms: i64` as a parameter instead of reading the clock.

### 2.1 Responsibility

Own the vocabulary and the decisions: what entities exist, what states they can be in, what events can
happen, and — given an event, a policy and what the user is currently doing — what should happen to the
user's screen.

### 2.2 Public surface

| Module | Types | Purpose |
| --- | --- | --- |
| `ids` | `WorkspaceId` `SessionId` `NodeId` `PaneId` `TemplateId` `EventId` `AttentionId` | Prefixed newtype strings (`sess_ab12cd34ef56`). A `PaneId` cannot be passed where a `SessionId` is expected, and the prefix keeps them readable in logs and SQLite. |
| `state` | `Lifecycle` `Turn` `AwaitingReason` `DisplayState` | The two-axis model. §2.3. |
| `event` | `TurnEvent` `EventKind` `Confidence` `EventSource` `Severity` `Risk` `AgentRef` | The single event vocabulary. §2.4. |
| `model` | `Workspace` `Session` `SessionStatus` `RestoreState` `ProcessNode` `NodeKind` `AgentInfo` `PendingPermission` `Relation` `SessionTree` `Layout` `LayoutNode` `Split` `Child` `Pane` `PaneKind` `RestoreBehaviour` `Direction` `Template` | Entities. §2.5. |
| `attention` | `AttentionPolicy` `Trigger` `Action` `Sound` `AttentionQueue` `AttentionEntry` `EntryState` `FocusGovernor` `UserContext` `FocusDecision` `FocusDenial` `DeferReason` `AttentionManager` `Effect` | Attention coordination. §2.6. |
| crate root | `now_ms()` | The one clock read, for the edges. |

### 2.3 The two-axis state model

`Lifecycle` tracks the OS process. `Turn` tracks the conversational turn and exists only for agents
(`ProcessNode::turn` is `Option<Turn>`; a shell has `None`). They change independently.

```
Lifecycle: Spawning → Alive → { Exited{code} | Signaled{signal} }
                    ↘ Orphaned      (still running after a UI restart, handle lost)
                    ↘ Reconnected   (running and re-attached)
                    ↘ Lost          (was running, cannot be found — an honest "we don't know")

Turn:      Idle → Active → { AwaitingUser{reason} | Done | TaskDone | Failed{reason} }
                          ↘ Unknown  (no adapter could tell us; never guessed at)

AwaitingReason: Permission(90) | Credentials(85) | Question(70) | Input(50)
                                        ^ base priority in the Attention Queue
```

`DisplayState` is the flat vocabulary the UI renders — `Starting`, `Running`, `WaitingForUser`,
`NeedsPermission`, `AskingQuestion`, `CompletedTurn`, `CompletedTask`, `Failed`, `Stopped`, `Idle`,
`Unknown`. It is **derived, never stored and never assigned**: `DisplayState::derive(&lifecycle,
turn.as_ref())` is a pure function of the two axes. That is what keeps them from drifting apart, and
it is why a crashed Agent cannot keep rendering as `NeedsPermission`: the derivation checks
`lifecycle.is_failure()` first, so a dead process outranks any stale turn state.

`DisplayState` also carries the presentation logic the UI needs and the queue reuses: `label()`
(`"YOUR TURN"`, `"PERMISSION"`, `"turn done"`), `demands_user()`, and `severity()` for ranking.

Note the deliberate asymmetry between `is_terminal()` and `is_failure()`: `Lost` is terminal but not a
failure, because failing to re-attach is not the same as the work having gone wrong.

### 2.4 The event vocabulary

Every signal — a Claude Code hook, a Codex `notify` callback, a pty heuristic, the process supervisor,
a user correction — is normalised into a `TurnEvent` before anything downstream sees it. Consumers
never learn which tool produced an event, only how much to trust it.

**Produces:** nothing; this is a data module.
**Consumed by:** `AttentionManager::ingest`, the daemon's state reducer, `turn-store`'s `EventRepo`,
`turn-proto`'s push messages.

`EventKind` variants, with their wire names (serde-tagged, so the JSON name is part of the contract):

| Wire name | Carries | Notes |
| --- | --- | --- |
| `process.started` | `pid`, `command` | |
| `process.exited` | `code` | Resolves attention for the node. |
| `process.failed` | `code?`, `signal?` | `Severity::Error`. |
| `process.spawned_child` | `child`, `pid`, `command`, `confirmed_parent` | The boolean is the whole point: false when the link came from the process table. |
| `agent.started` | `tool`, `model?`, `external_id?` | `external_id` is the tool's own session/thread id, needed to resume it. |
| `agent.turn_started` | `prompt_excerpt?` | Also the signal that clears a pending demand — the user answered. |
| `agent.turn_completed` | `last_message?`, `background_tasks: usize` | `background_tasks` is reported by Claude Code, not inferred. |
| `agent.waiting_for_user` | `reason`, `summary?` | |
| `agent.question_asked` | `question` | |
| `agent.permission_required` | `summary`, `command?`, `tool_name?`, `risk` | |
| `agent.permission_resolved` | `allowed` | Closes the demand. |
| `agent.task_completed` | `summary?` | |
| `agent.failed` | `reason` | |
| `agent.idle` | — | `Severity::Debug`. Explicitly *not* an attention moment. |
| `agent.subagent_started` | `agent_type?`, `agent_id?` | Confirmed hierarchy. |
| `agent.subagent_stopped` | `agent_id?` | |
| `session.needs_attention` | `reason` | For adapters with a coarser signal. |
| `session.attention_resolved` | — | |

Three properties of `TurnEvent` do real work:

- **`TurnEvent::new` clamps confidence** to `source.max_confidence()`. An adapter asks for a
  confidence; the source decides what it is allowed to have. Enforcement, not convention.
- **`dedup_key`** defaults to `session|kind_slug` and is extended with the node id by `with_node`, so
  two subagents blocking simultaneously are two demands while one chatty Agent repeating itself is one.
- **`attention_reason()`** is the single place where "an event happened" becomes "the user is needed".
  The attention manager never pattern-matches event kinds itself. `agent.turn_completed` deliberately
  returns `None`: finishing a turn is not, by itself, a demand.

`raw: Option<String>` keeps the untouched payload for debugging bad adapters. It is never rendered
as-is, and `turn-store`'s `redact` module is what stands between it and the database file (§7.5).

### 2.5 Entities

**`Workspace`** — the persistent project. Root path, git remote, environment applied to every process
started here, default shell and Agent, init commands, default Template, a baseline `AttentionPolicy`
Sessions may override, and a `tmux_enabled` flag that nothing reads yet.

**`Session`** — the unit of work. Name in the user's words, cwd, env, `Layout`, `SessionTree`,
`AttentionPolicy`, `SessionStatus` (`Active`/`Paused`/`Archived`), `RestoreState`, tags, git branch,
linked PR reference, pin/favourite, `parent_session` for duplicates. `display_state()` returns `Idle`
for an empty tree — a Session whose processes have not started is not a mystery — and otherwise the
tree's aggregate. `sidebar_rank()` returns `(pinned, demands_user, severity, last_activity_ms)` as a
tuple rather than an `Ord` impl, because ordering is a presentation concern that may differ per view.

**`ProcessNode` / `SessionTree`** — the process hierarchy. Stored **flat with parent pointers**, not as
nested structs: processes arrive out of order (a child's hook can land before the parent's spawn
notification), and re-parenting a flat map is trivial where re-parenting a tree is not.
`order: Vec<NodeId>` preserves insertion order so the tree renders stably instead of in hash order.

`SessionTree::relink` enforces the `Relation` ladder — a `Confirmed` link is never downgraded by an
`Inferred` one — and refuses to create a cycle, with a 1,000-hop defensive bound in case the store ever
hands it a corrupt tree. `remove` promotes children to roots with `Relation::Unknown` rather than
deleting them or silently re-attaching them elsewhere. `aggregate_state()` is the **most severe**
state, not the most recent.

**`Layout` / `Pane`** — the pane arrangement, a tree of `Split`s whose `children: Vec<Child>` hold a
fractional `size`. Splits hold a list rather than exactly two children so three side-by-side Panes are
one split with three children instead of a lopsided nest; that is what makes resize behave the way a
user expects. `split` joins an existing same-direction split as a sibling and shrinks everyone
proportionally. `resize` borrows from the next sibling with a 5% floor so a Pane cannot be resized out
of existence. `close` refuses on the last Pane and collapses a split left with one child. `zoomed`
never mutates the tree, so un-zooming restores exact previous geometry. `sizes_are_normalised()` is
the structural invariant; `normalise()` repairs a hand-edited Layout on load.

**`Template`** — a reusable Session shape. `from_layout` strips `node_id` bindings (a Template must not
remember which process it was cloned from); `instantiate` reassigns every `PaneId` so two Sessions from
one Template share no identity. Four built-ins: `Blank`, `Coding`, `PR Review`, `Pair of Agents`.

### 2.6 Attention

Four files, one job each.

**`policy`** — what a Session *wants*. Seven `Trigger`s (`TurnComplete`, `Question`,
`PermissionRequired`, `TaskComplete`, `Failure`, `WaitingForUser`, `SubagentAppeared`) each map to a
list of `Action`s (`Nothing`, `Badge`, `Highlight`, `Sound`, `Notify`, `Enqueue`, `Focus`,
`FocusIfIdle`, `FocusIfBackground`, `Custom`). Defaults are quiet: badge and enqueue on turn
completion; `FocusIfIdle` only for a blocked permission, which is the one case where the Agent is
burning wall-clock waiting on the human; badge only for a new subagent.

`AttentionPolicy::resolve(trigger, confidence)` is the first of the two heuristic guards: any focus
action fired by a confidence that fails `may_steal_focus()` degrades to `Badge`, and the result is
deduplicated so `[Badge, Focus, FocusIfIdle]` collapses to `[Badge]`.

**`queue`** — ordering. `AttentionEntry` is keyed for dedup on `session|node|reason`. `score(now_ms)`
is `base_priority + state_penalty + confidence_penalty + age_bonus + priority_boost`, where the age
bonus is capped at 15 minutes — enough to prevent starvation, deliberately far below one priority class
so it can never let an idle prompt outrank a blocked permission. `upsert` refreshes an existing entry,
upgrades its confidence, un-acknowledges it (the Agent asked again) and **keeps the original
`created_ms`** so a chatty Agent cannot reset its own age to jump the queue.

**`focus`** — the guards no policy may bypass, because they live in the governor rather than the policy:

| Constant | Value | Guard |
| --- | --- | --- |
| `TYPING_GRACE_MS` | 1,500 | The user still counts as typing this long after a keystroke. |
| `MIN_FOCUS_INTERVAL_MS` | 2,000 | Minimum gap between any two focus changes. |
| `MAX_FOCUS_CHANGES_PER_WINDOW` / `FOCUS_WINDOW_MS` | 3 / 10,000 | Sliding-window ceiling. |
| `PING_PONG_GUARD_MS` | 5,000 | A Session just moved away from cannot pull the user back. |

`evaluate` returns `Grant`, `Defer { until_ms, reason }` or `Deny { reason }`. The distinction is the
product: a **deferral** keeps the badge and lands the jump later, a **denial** never moves the user.
`AlreadyFocused` is a denial rather than a grant specifically so a no-op does not burn a slot in the
rate limiter. Non-focus actions passed here are denied rather than silently granted.

**`manager`** — the seam. `ingest(event, policy, ctx, now_ms) -> Vec<Effect>` is the only place that
decides the user gets interrupted, and it emits `Effect`s the UI performs rather than touching a screen
itself, which is what makes all of this testable without a window.

`Effect`: `Badge` · `Highlight` · `PlaySound` · `Notify` · `Enqueued` · `Focus` · `FocusDeferred` ·
`FocusDenied` · `RunCustom` · `Cleared`.

Details that are easy to get wrong and are pinned by tests:

- Only a **perceptible** effect (badge, highlight, sound, notify, focus, custom) starts the Session
  cooldown. Counting a deferral would let one postponed jump silence its own Session for the whole
  cooldown, and the jump would never land.
- A deferred focus request carries **the policy in force when it was made**. Re-evaluating with
  defaults on `tick` would ignore the Session's own guard settings — this was a real bug, found and
  fixed during implementation (ADR-022).
- `tick` passes `None` for the session cooldown, because a deferred jump is the tail of one
  already-approved effect, not a new one. The governor's own guards still apply.
- `DEFERRED_FOCUS_TTL_MS` = 60,000. Past that the pending jump is dropped; the badge already told the
  story.
- `goto_next` / `goto_after` bypass the governor entirely — pressing the shortcut is consent — but
  reset the rate limiter so automatic focus does not immediately fight manual navigation.
- A muted Session yields exactly one `Badge` and nothing else, so the sidebar still shows something
  happened.

### 2.7 Failure modes

- **A wrong `Turn` from a bad adapter** produces a wrong `DisplayState` and a wrong queue entry.
  Bounded by confidence clamping, and by `derive` letting process reality override turn state.
- **Clock skew** cannot produce negative durations: `idle_for_ms`, `score` and the governor all
  saturate or clamp (`model::session::tests::idle_time_never_goes_negative_on_a_clock_skew`).
- **A corrupt tree from the store** cannot hang `relink` (hop bound) or `descendants` (depth bound in
  `turn-pty`).
- **Float sizes in `Layout`** are the one place invariants can drift; `sizes_are_normalised()` exists
  to catch it in tests and `normalise()` to repair it on load. `AgentInfo` is deliberately not `Eq`
  because `cost_usd` is a float.
- **Locks:** `turn-core` holds none.

---

## 3. turn-pty — ptys, buffers, supervision

**Status: Built. 46 tests (buffer 20, process 18, supervisor 8).** Depends on `turn-core` (ids, state,
`NodeKind`), `portable-pty`, `vt100`, `sysinfo`, `tokio` (sync only), and `libc` on unix.

### 3.1 `PtyProcess` — one process on one pty

**Public surface:** `spawn(node_id, ProcessSpec, now_ms)` · `write` · `resize` · `subscribe` ·
`replay` · `snapshot` · `buffer` · `exit_info` · `exit_watcher` · `lifecycle` · `is_running` ·
`output_finished` · `interrupt` · `terminate` · `kill` · `bytes_written`.

Reading a pty blocks, so each process gets two dedicated OS threads:

- a **reader** thread that pumps 64 KiB chunks into the `TerminalBuffer` **first** (the buffer is
  authoritative and must never miss data) and then into a bounded
  `broadcast::Sender<Arc<Vec<u8>>>`. Chunks are `Arc`-shared rather than cloned per subscriber: with
  thirty terminals and several subscribers each, copying every chunk is exactly the waste that shows up
  as UI stutter.
- a **waiter** thread that polls `try_wait` every 100 ms and publishes `ExitInfo` once through a
  `watch` channel.

Three subtleties that are load-bearing:

- **`drop(pair.slave)` immediately after spawn.** Without it the pty never reports EOF.
- **`ExitInfo.signal: Option<String>`, not a number.** `portable-pty` reports a signal death by *name*
  ("Killed", "Terminated") and gives a meaningless exit code of 1, so the name — not the code — is how
  Turn tells a kill from a clean exit. Inventing a numeric mapping would only lose information
  (ADR-010).
- **`interrupt()` writes `0x03` to the pty** rather than signalling the pid, so the tty delivers SIGINT
  to the whole foreground process group. That reaches the children an Agent spawned; `kill(pid)` would
  not.

Environment: the process inherits the user's environment by default (`clean_env` is off), because
agents need the ambient `PATH` and credential helpers to work at all. `TERM=xterm-256color` is set
unless the caller overrides it, and `TURN_NODE_ID` marks the process as ours so the supervisor can
attribute strays.

**`impl Drop for PtyProcess` ends the process it owns** — `terminate()`, then `kill()`. This is the
ownership model, and §5.2 explains exactly what it does and does not buy.

**Failure modes:** `PtyError::OpenPty` when the pty table is exhausted; `PtyError::Spawn` for a missing
binary (never a panic); `PtyError::Unavailable` on a poisoned writer/child lock; `PtyError::NoPid` if
the child has no pid. The two threads use `.expect()` at spawn time only — failing to create a thread
is unrecoverable. `output_finished()` can be true before `exit_info()` is `Some`; consumers must
tolerate that ordering.

### 3.2 `TerminalBuffer` — two representations, both bounded

Turn keeps a raw **byte ring** (default 2 MiB) for exact replay and a **parsed vt100 screen** (default
5,000 scrollback rows) so the daemon can answer "what does this look like right now" with no UI
attached. Both earn their keep: bytes carry the colours, cursor position and alternate-screen state
that text cannot; the parsed screen is what makes thumbnails of unopened Sessions and output heuristics
possible.

`replay()` returns the **parser's** `contents_formatted()`, not the raw ring. It is far smaller and
always self-consistent, where a truncated ring can start mid-escape-sequence and corrupt the receiving
terminal (ADR-023). `raw()` exposes the ring for callers that want exactly what arrived.
`is_truncated()` admits when a replay would be partial.

`TerminalCallbacks` implements `vt100::Callbacks` and is where the terminal's security decisions live:
`set_window_title` sanitises and caps the title **on arrival** rather than on the way out of `snapshot()`,
because the incoming OSC payload is unbounded and retaining it would be a memory cost the process controls;
and `copy_to_clipboard`, `paste_from_clipboard` and `resize` are each **counted and dropped**. See §7.

**Failure modes:** a zero dimension is clamped to 1 in `ScreenSize::new`, because zero makes the parser
panic and makes no sense to the kernel either. A write larger than the whole ring keeps its tail.
Resize is a buffer-only operation; the caller is responsible for telling the kernel, and
`PtyProcess::resize` does both halves.

### 3.3 `ProcessSupervisor` — the fallback for hierarchy

**Public surface:** `refresh` · `descendants(pid)` · `children(pid)` · `observe(pid)` · `is_alive` ·
`process_count`, plus the free function `classify(command_line) -> NodeKind`.

This is the layer that notices what the processes Turn started went on to start: a dev server, a test
runner, a GUI application. Links it produces are `Relation::Inferred` and labelled as guesses.

Two decisions worth stating:

- **Scanning is on demand, not on a timer** (ADR-019). Polling the whole process table every second
  across thirty Sessions is precisely the aggressive polling the product rules out.
- **`refresh` asks only for `cmd`, `cwd` and `exe`.** A full `sysinfo` refresh also collects
  per-process memory, disk and CPU statistics, which is a great deal of work to throw away.

`descendants` indexes children by parent once rather than re-scanning per level, and bounds itself at
depth 32 with a `seen` set so pid reuse or a corrupt table cannot spin forever.

`classify` matches on the **executable** — last path segment of the first token — not the whole line,
so `echo "ask claude about it"` is not a coding agent. Argument-level patterns (`" test"`, `" build"`,
`" serve"`, `" dev"`, `" watch"`) come after. Anything unrecognised is `NodeKind::Unknown`, which still
appears in the tree: an honest "unknown process" beats a confident mislabelling.

**Failure modes:** a dead or never-existing pid returns `None`/`false`/empty rather than fabricating a
record. On Linux, `/proc` reads can fail for processes owned by another user; those simply do not
appear.

### 3.4 What turn-pty does *not* do

It produces no `TurnEvent`s. It reports `Lifecycle`, `ExitInfo` and `ObservedProcess`; converting those
into `process.started` / `process.exited` / `process.failed` / `process.spawned_child` and attaching them
to a `SessionTree` is the daemon's job, and it is deliberately not this crate's.

That conversion is now written in `turnd` — `core/spawn.rs` and `core/events/exit.rs` construct the
process events, `core/supervise.rs` the `process.spawned_child` ones — but it is being written as this is
read and none of it is verified here. Until it is, treat the join as the riskiest unproven code in the
system rather than as a solved problem (`ROADMAP.md` §Risks 4).

---

## 4. turn-agents — the adapter layer and the hook server

**Status: Being refactored. 92 unit tests plus 18 contract tests (6 Claude Code, 12 Codex) when last
green; the crate did not compile at the final check of this survey, mid-extraction of a `text` module and
a signature change to `notify_config`. The behaviour described below is what the crate did when last
verified — check it before relying on an API shape.** Modules: `adapter`,
`claude`, `codex`, `heuristic`, `registry`, `risk`, `server`.

### 4.1 The adapter contract

An `AgentAdapter` has exactly two jobs, and everything tool-specific lives behind them:

```rust
fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError>;
fn normalise(&self, payload: &serde_json::Value, ctx: &EventContext) -> Vec<TurnEvent>;
```

Plus identity and self-description: `id`, `provider`, `executables`, `best_level`, `capabilities`,
`handles(command)`, `detect()`.

`normalise` returns a `Vec` because one callback can mean two things at once. `Capabilities`
(`turn_events`, `permission_events`, `subagent_events`, `resumable`, `usage_events`,
`external_session_id`) exists so the UI never offers an action that will silently do nothing.

`LaunchPlan` reports the `IntegrationLevel` **actually achieved**, which may be lower than the
adapter's best if something was unavailable, plus a human-readable `note` surfaced in the Session
details so the user knows why detection is or is not working.

`LaunchContext::scratch_dir` is a directory Turn owns and deletes with the Session. Adapters write
throwaway configuration there and never touch the user's own files (ADR-021).

`which()` is hand-rolled — a dozen lines, respecting `PATH` and requiring the executable bit on unix —
rather than pulling in a crate, and it rejects a directory that shares a binary's name.

### 4.2 The four Integration Levels

`IntegrationLevel` is `Ord`, worst to best.

| Level | How state is known | `EventSource` | Max confidence | Can move focus | Implemented by |
| --- | --- | --- | --- | --- | --- |
| `GenericTerminal` | Not at all. Turn shows a terminal and knows only what the OS says about the process. | `Supervisor` | `Explicit` about the *process*, never about a turn | Yes, for process facts | `registry::GenericTerminalAdapter` |
| `Heuristic` | Inferred from terminal output. | `PtyHeuristic` | `InferredHigh` | **No** | `heuristic::HeuristicAdapter` |
| `Wrapper` | Launched through something Turn controls, which reports lifecycle. | `SideChannel` | `Explicit` | Yes | `codex` degraded to `notify` only |
| `Structured` | The tool reports events itself over a contract it owns. | `Hook` | `Explicit` | Yes | `claude`, `codex` with hooks |

### 4.3 The Confidence ladder, and why a heuristic can never move focus

```
Unknown  <  InferredLow  <  InferredHigh  ‖  Integrated  <  Explicit
                                          ‖
                       is_provisional() ──┘└── may_steal_focus()
```

The line between `InferredHigh` and `Integrated` is the only one that changes behaviour, and it is
enforced at **two independent points**:

1. **Construction.** `TurnEvent::new` computes `confidence.min(source.max_confidence())`.
   `EventSource::PtyHeuristic` caps at `InferredHigh`, so an adapter that asks for `Explicit` gets
   `InferredHigh` and there is no way around it short of lying about the source
   (`event::tests::a_heuristic_cannot_promote_itself_to_explicit`).
2. **Policy resolution.** `AttentionPolicy::resolve` turns any focus action into `Badge` when
   `!confidence.may_steal_focus()`, regardless of what the Session's policy asked for
   (`policy::tests::a_guessed_permission_badges_instead_of_stealing_focus`).

The reasoning is asymmetric cost. A missed notification costs the user a delay they recover from by
glancing at the sidebar; a false positive that yanks them out of their editor mid-thought costs them
the thing they were holding in their head, and — worse — teaches them not to trust the product. Pattern
matching on terminal output is *guaranteed* to produce false positives eventually: agents change their
output format between releases, a TUI redraw looks like a prompt, and an Agent echoing documentation
can print a permission dialog verbatim. So heuristics are allowed to badge, highlight, notify and
enqueue — every channel the user consults on their own schedule — and are structurally barred from the
one channel that consults them on Turn's schedule.

A third safeguard exists for a different reason: `Trigger::SubagentAppeared` maps to `[Badge]` in the
default policy, so a new subagent never moves the user **even at `Explicit` confidence**. That is a
product decision about relevance, not a confidence decision.

### 4.4 Claude Code adapter — `Structured`

Hooks are injected with `--settings <path>`, which adds a settings layer and leaves
`~/.claude/settings.json` and `.claude/settings.json` read normally and unmodified. The file is written
into `LaunchContext::scratch_dir`, and `--settings` is **appended** after the user's own args so their
flags keep precedence wherever Claude Code's parser gives later flags the final say.

Subscribed events, deliberately not all of them — each subscription costs the Agent a callback, and Turn
only wants the ones that change a state it renders: `SessionStart`, `UserPromptSubmit`,
`PermissionRequest`, `PermissionDenied`, `Notification`, `SubagentStart`, `SubagentStop`, `Stop`,
`StopFailure`, `SessionEnd`.

Transport is `HookTransport::Http` by default — `{"type":"http","url":...,"timeout":3}` — verified live
to fire and POST the payload as a JSON body. No helper process per event, which matters when a busy
Agent fires dozens of hooks. `HookTransport::Helper` shells out to `turn-hook` instead, for builds whose
hook engine lacks HTTP handlers.

| Hook event | `EventKind` | Note |
| --- | --- | --- |
| `SessionStart` | `agent.started` | Records `session_id` as `external_id` for resuming. |
| `UserPromptSubmit` | `agent.turn_started` | Reads `prompt` (the real field name), falling back to `user_prompt` (ADR-012). |
| `PermissionRequest` | `agent.permission_required` | `tool_input.command` → `risk::assess`. |
| `PermissionDenied` | `agent.permission_resolved{allowed:false}` | |
| `Notification` | depends on `notification_type` | `permission_prompt` → permission; `idle_prompt`/`agent_needs_input` → waiting; `auth_success` → **nothing** (progress, not a demand). |
| `SubagentStart` / `SubagentStop` | `agent.subagent_started` / `_stopped` | Confirmed hierarchy, no inference. |
| `Stop` | `agent.turn_completed` | `background_tasks.len()` — the turn is over, and these are still going (ADR-014). |
| `StopFailure` | `agent.failed` | |
| `SessionEnd` | `agent.idle` | |
| anything else | dropped | New releases add events; they must not become noise. |

`HOOK_TIMEOUT_SECONDS = 3`, short on purpose: if the daemon is gone the Agent must carry on rather than
stall on every event.

**Failure modes:** `prepare` returns `AdapterError::Config`/`Serialise` if the scratch directory is
unwritable, and the daemon must degrade rather than refuse to launch. `normalise` never panics on
malformed input — asserted against mangled real payloads, because an adapter that panics takes the
daemon's event loop with it. The standing risk is that hook payloads are an evolving contract Turn does
not own; `tests/fixtures/claude-code-2.1.221.json` was recorded from a live run and
`tests/contract_claude.rs` fails loudly when a field Turn depends on disappears. That test failing
after an upgrade is the system working.

### 4.5 Codex adapter — `Structured`, degrading to `Wrapper`

Codex needs **both** of its mechanisms, because neither is sufficient alone:

- **Hooks** (`-c hooks={...}`, inline TOML — never a path, ADR-013) cover session start and end, prompt
  submission, permission requests and subagents. Subscribed, in Codex's own PascalCase: `SessionStart`,
  `UserPromptSubmit`, `PermissionRequest`, `SubagentStart`, `SubagentStop`, `SessionEnd`. `PreToolUse` and
  `PostToolUse` are deliberately absent: they fire on every tool call and Turn maps them to nothing.
  `Stop` exists and is deliberately not subscribed to either — see below.
- **`notify`** (`-c notify=[...]`) delivers the turn boundary, as `type = "agent-turn-complete"`. Codex
  does have a `Stop` hook, so this is not a missing-event problem; it is a trust problem. A freshly
  configured hook is untrusted and does not run — under `codex exec`, silently and with a normal exit —
  whereas `notify` has no trust gate. Putting the boundary on `notify` is what makes "your turn" work on
  first launch instead of only after the user has approved Turn's hooks (ADR-038).

One property of the hooks configuration deserves naming here because it changes how the adapter must be
maintained: **Codex validates the type of `hooks` but not the keys inside it.** Writing `handlers=[…]`
where it wants `hooks=[…]`, or `session_start` for `SessionStart`, is accepted without complaint and then
simply never fires — a launch that looks configured, reports nothing, and appears to be Codex's fault.
This project made exactly that mistake and shipped passing tests asserting the wrong spelling; it was
caught only by making a handler record its own invocation. No external check can catch it, so the contract
tests pinned to captured payloads are the only protection (ADR-037).

`CodexTransport::HooksAndNotify` configures both and reaches `Structured`.
`CodexTransport::NotifyOnly` reports `Wrapper` with a note saying so, for when hook trust has not been
granted — Codex has a persisted hook-trust model, which is why `--dangerously-bypass-hook-trust`
exists. **The adapter reports the level it achieved rather than pretending to one it did not**
(`codex::tests::without_hooks_the_launch_degrades_to_notify_and_says_so`, and at contract level
`tests/contract_codex.rs::without_hook_trust_the_adapter_reports_wrapper_and_says_what_is_missing`).

Both mechanisms are command-based, so Codex reaches Turn only through the `turn-hook` helper (§4.8).
One detail is a deliberate hedge against an unverified assumption: whether a hook *handler* entry
accepts an `args` array was read out of the binary's strings but not exercised live, so the callback URL
travels to the helper in the **`TURN_HOOK_URL` environment variable** — a surface Codex cannot mis-parse.
`execution_mode` is left unset on purpose: guessing at the semantics of `await` risks configuring Codex
to wait on Turn.

**This is changing as of writing, and ADR-027 records the old shape.** `notify` originally passed
`--url <url>` explicitly, since the array form is confirmed. That put the per-node token in Codex's own
argv, which on Linux is readable by every process running as the user — so any agent Turn launched could
harvest every other Codex session's token with one `ps`. `notify_config` is being reduced to the program
alone, with the helper reading `TURN_HOOK_URL` from the environment it inherits, matching the hooks path.
Treat "the URL reaches the helper through the environment on both paths" as the intended shape and check
`crates/turn-agents/src/codex.rs` for where it actually landed.

### 4.6 Heuristic adapter — `Heuristic`

Inference from terminal output, for tools with no contract to honour. It is written to know it is the
weakest tier, and three rules constrain it because the failure each prevents is worse than the
detection it gives up:

1. **Every event uses `EventSource::PtyHeuristic`**, capping confidence at `InferredHigh`. A guess can
   badge a Session; it can never move focus. There is a test for exactly that.
2. **It stands down completely in the alternate screen.** A TUI repainting itself produces text that
   matches anything you care to look for — a `vim` buffer containing `(y/n)` is not a permission prompt.
   `OutputHeuristic::stood_down()` counts how often this fired.
3. **A quiet terminal is not, on its own, an Agent waiting for you.** The "awaiting input" rule
   requires a positive marker of an agent's input affordance, because treating silence at a prompt as a
   demand turns every idle shell in the Workspace into a notification. That is the single most common
   false positive available, so it is ruled out by construction rather than tuned away.

`HEURISTIC_EXECUTABLES` is a **closed list** — `gemini`, `aider`, `cursor-agent`, `opencode`, `crush`,
`goose`, `amp`, `qwen`, `copilot` — because inference is only worth its false positives for programs
that actually hold a conversation. Pointing it at `make` or `vim` would produce confident nonsense, so
anything unlisted gets `GenericTerminal` and no claims.

`OutputHeuristic` is per-Pane state, not a background task: the caller decides when to observe and
passes the time in, so the debounce is testable without sleeping. `classify(snapshot, now_ms)` is
separate from `observe(...)` so the rules can be tested against captured screens directly **and so the
UI can explain a badge**. `Inference` is the whole vocabulary: `Working{rule}`,
`AwaitingPermission{rule}`, `AwaitingInput{rule}`, `Undecided` — each carrying the name of the rule
that fired, which is what makes "why do you think that" answerable.

`HeuristicConfig` has two knobs: `idle_after_ms` (2,000) is how long output must be unchanged before a
settled prompt counts as waiting, and `debounce_ms` (750) is the anti-flicker guard so a spinner that
briefly clears between frames does not produce a stream of started/waiting/started events. Change
detection uses `bytes_seen` rather than diffing screen text. Only the last 12 lines are matched, so a
resolved permission box does not stay "alive" forever in the scrollback.

### 4.7 Registry — selection that always answers

One question: given a command line the user typed, which adapter runs it? `AdapterRegistry::select`
walks the registered adapters strongest-first and falls back to `GenericTerminalAdapter`, because
"Turn does not recognise this command" must never mean "Turn will not run this command".

`executable_of` skips leading `VAR=value` assignments (`RUST_LOG=debug claude` is still Claude Code) and
reduces a path to its file name. Anything cleverer — unpicking a shell one-liner or a pipeline — is
deliberately not attempted: guessing which program inside `sh -c '…'` matters would produce confident
mistakes, and the generic terminal is the right answer for a shell invocation.

The selection is **reported, not just used**. `Selection` carries `level`, `capabilities`,
`executable: Option<PathBuf>` and a plain-language `note`, so the Session details panel can tell the
user whether "waiting for you" will be a fact or a guess. `is_installed()` isolates the interesting
case: `Structured` level with no executable means the user typed `claude`, Turn knows how to integrate
with it, and it is not on `PATH` — a failure that has nothing to do with Turn, reported as such rather
than as "unrecognised".

`GenericTerminalAdapter::handles` returns `false` for everything. Claiming everything would shadow the
real adapters depending on iteration order; selection reaches the fallback explicitly instead.

### 4.8 Hook server — `server::HookServer`

The loopback endpoint structured adapters point their tools at. Four constraints, all about not being
in the way of the user's Agent:

- **It answers immediately.** The handler does a hash lookup, a parse, a `try_send` and returns.
  Nothing awaits anything downstream.
- **It never answers with a decision.** Claude Code's hook protocol allows a response body that allows
  or denies a tool call. Turn always replies with an empty 200: approving on the user's behalf is
  exactly what this product promises not to do
  (`server::tests` and `contract_claude` both assert it).
- **It is only reachable by processes holding a token.** It binds `127.0.0.1` (never `0.0.0.0`) on an
  ephemeral port; every registered node gets its own 256-bit token in the path
  (`POST /hook/{token}`). An unknown token is refused and **counted**.
- **It cannot be made to allocate.** `DefaultBodyLimit::max(MAX_BODY_BYTES)` — 256 KiB — is enforced by
  the server before the bytes are buffered, so a hostile `Content-Length` costs nothing.

`start_with_helper(Option<PathBuf>)` returns the server plus an `mpsc::Receiver<TurnEvent>`, the
daemon's end of the event stream. `register(session_id, node_id, adapter) -> HookEndpoint` mints a
token; `unregister` revokes it and any further post with it is refused like any forgery. Dropping the
server shuts the listener down, which is what makes it safe to create one per test without leaking
ports.

Two backpressure decisions: the event channel is bounded at `EVENT_CHANNEL_CAPACITY` = 1,024 and a full
channel **drops the event** rather than applying backpressure to Claude Code — if the daemon stops
draining, the correct behaviour is to lose events and say so, not to slow every Agent on the machine to
the speed of the slowest consumer. And if the receiver is dropped entirely, the server **keeps
answering agents** and discards what it normalises; agents must not start failing because the daemon
went away.

`HookStats { accepted, refused, unparsable, dropped, emitted }` is surfaced to the UI. `refused` is the
interesting one: a non-zero value means something on this machine posted to Turn without a valid token.

### 4.9 Risk assessment

`risk::assess(tool_name, command) -> Risk` rates a pending permission for **display and queue ordering
only**. It authorises nothing (ADR-024). It errs upward — an unrecognised tool is `Medium`, not `Low` —
because under-warning costs a bad surprise while over-warning costs a glance. The `HIGH_RISK` list is
deliberately short and specific; a long list of vague patterns marks everything high risk, and a warning
that always fires is a warning nobody reads. The command outweighs a reassuring tool name: `Read` with
`rm -rf /important` is `High`.

---

## 5. turn-hook and turnd

### 5.1 `turn-hook` — the helper that must never break a session

**Status: Built. 15 tests. Zero dependencies.**

Agents that cannot POST over HTTP themselves run a command instead. Codex's hook handlers and its
`notify` mechanism are both command-based, so this is the only way Turn hears from Codex at all.

One requirement overrides every other: **a broken helper must never break the user's agent session.**
Whatever happens — no URL, no daemon listening, an unreadable payload, a refused connection — the
process exits 0 and prints nothing. `std::process::exit(0)` is explicit and unconditional in `main`.
Diagnostics go to stderr only when `--debug` is passed, because unsolicited stderr from a hook lands in
the middle of the user's agent output.

Two payload conventions, because the two tools differ: **stdin**, as Claude Code's `command` hooks
deliver it, and **argv**, as Codex's `notify` does (the program is invoked with the event JSON appended
as a final argument). The destination comes from `--url` or from `TURN_HOOK_URL`. Only `http://` is
supported: the target is always a loopback port on this machine, so there is nothing for TLS to protect
and no certificate story to get wrong. `DEFAULT_TIMEOUT_MS` is 2,000 and `MAX_PAYLOAD_BYTES` is 256 KiB.

Zero dependencies is the point, not an accident: this binary may run on every tool call, so it must
start in microseconds and cannot afford an async runtime. The HTTP request is built by hand over
`TcpStream`.

### 5.2 `turnd` — the daemon

**Status: In progress, and being written as this is read.** `main.rs` is a real entry point that parses
options, initialises logging, resolves a `Config` and calls `turnd::start`. The library declares `config`,
`paths`, `instance`, `logging`, `options`, `error`, `server` and `core`, with `core` split into `spawn`,
`supervise`, `restore`, `events`, `requests`, `views`, `attention`, `clients`, `command` and `output`.
There are 71 test functions across the crate, including integration tests (`desk.rs`, `agents.rs`,
`surface.rs`, `restart.rs`, `attention.rs`, `binary.rs`).

**None of that is verified in this document.** The counts above are of code and test functions that exist,
not of tests observed to pass. Everything in this section describes the daemon's intended responsibility
and the design it is being built to; read it as specification, and check the crate before relying on any
of it.

#### Intended responsibility

The daemon is the only process that holds a pty handle. It owns:

- the Session registry and the authoritative `SessionTree` per Session;
- one `PtyProcess` per terminal Pane;
- one `AttentionManager` for the whole application — the queue is global by design, being the ordered
  list of everything wanting the user across every Workspace;
- the `HookServer` and its token table;
- supervisor scans, triggered on demand;
- all writes to `turn-store`, from a blocking context, controlling ordering itself;
- the `turn-proto` unix-socket endpoint the UI connects to.

It is also the **only** place where a pty fact and an Agent fact are joined: mapping a hook payload's
`session_id` to a `NodeId` via `SessionTree::find_by_external_id`, or a supervisor `ObservedProcess` to
an existing node via `find_by_pid`. Concentrating that join is deliberate; it is where a wrong
correlation would silently corrupt state.

#### Why the daemon exists from day one, and exactly what it buys

**It buys:** the UI can crash, be quit, be hot-reloaded during development, or be updated, and the
Agents keep running. Reconnecting re-attaches — the UI takes `PtyProcess::replay()` per Pane to rebuild
the screen exactly, then subscribes to the live stream.

**It does not buy:** survival of the daemon exiting. The pty master lives in the daemon's file table,
and `PtyProcess::drop` deliberately terminates the process it owns — `terminate()` then `kill()` —
because closing a Session must not leave strays holding ptys, which are a finite kernel resource. When
the daemon goes, the ptys close and the children get SIGHUP.

This is stated plainly rather than papered over, because the honest boundary is what `Lifecycle` is
shaped around. `Orphaned` (still running, handle lost), `Reconnected` (running and re-attached) and
`Lost` (was running, cannot be found) exist precisely so Turn can report the truth after a restart
instead of inventing an exit code. Making work survive the *daemon* is a different problem with a known
solution — tmux, which the model already has flags for — and it is deliberately out of the MVP
(ADR-007).

**Turn never relaunches a process automatically on restore.** It offers; the user decides.
`RestoreBehaviour::ReattachOnly` is the default, and `Relaunch` is opt-in per Pane for things safe to
re-run unprompted, like a shell. `turn-proto` enforces this structurally: `Request::RelaunchNode` is
the only request that starts anything, and it is user-initiated.

#### Intended event flow

```
hook POST ──▶ HookServer ──▶ adapter.normalise ──▶ TurnEvent (confidence clamped)
                                                       │
turn-hook ──▶ (same route) ────────────────────────────┤
pty exit  ──▶ ExitInfo ──▶ (daemon) ───────────────────┤
supervisor scan ──▶ ObservedProcess ───────────────────┤
OutputHeuristic::observe(snapshot) ────────────────────┤
                                                       ▼
                                           reduce into SessionTree
                                           (Lifecycle, Turn, Relation)
                                                       │
                                    ┌──────────────────┼──────────────────┐
                                    ▼                  ▼                  ▼
                          AttentionManager      turn-store          turn-proto
                             ::ingest        (append event, redact)  (push view models)
                                    │
                                 Vec<Effect> ──▶ turn-proto ──▶ UI performs them
                                                            (and acknowledges Focus, so
                                                             UserContext stays true)
```

`UserContext` — last keystroke, whether the window is frontmost, the active Session, whether a sensitive
operation is in flight — flows the other way, from UI to daemon, and must be kept fresh or the typing
guard degrades to nothing.

#### Failure modes to design for

- **The UI is gone when an `Effect::Notify` is produced.** Effects addressed to a disconnected UI must
  be dropped or coalesced, not queued indefinitely.
- **A hook arrives for an unknown `session_id`.** Drop it and log; never create a Session from a hook.
  The hook server already refuses an unknown *token*, which is the stronger check.
- **The store is unwritable** (disk full, permissions). The daemon must keep running with persistence
  degraded and say so, rather than exiting and taking the Agents with it.
- **A schema written by a newer build.** `turn-store` refuses a downgrade loudly, and the daemon must
  stop cold rather than write to it.
- **`rusqlite` is synchronous.** Every store call must be off the reactor or the event loop stalls, and
  it will not be obvious in testing.

---

## 6. turn-proto and turn-store

Both are **built**.

### 6.1 turn-proto — the daemon↔UI protocol

**Status: Built.** Modules: `envelope`, `framing`, `request`, `response`, `events`, `cells`, `screen`,
`bytes`, `error`, `geometry`, `view/{session,tree,attention,workspace}`. It is types, framing and the one
reading of a parsed screen as cells — no I/O, no tokio, no socket, so the contract can be tested without
either process existing.

**The connection.** A versioned envelope (`ClientFrame` / `ServerFrame`) carries four things: a
`hello`/`welcome` handshake resolved by `negotiate()`, id-correlated `request`/`response` pairs, and
unsolicited `event` pushes at any time.

```text
UI                                             turnd
 │  {"v":2,"type":"hello",…}                      │
 │ ─────────────────────────────────────────────► │
 │                    {"v":2,"type":"welcome",…}  │   negotiate()
 │ ◄───────────────────────────────────────────── │
 │  {"v":2,"type":"request","id":"r-1",…}         │
 │ ─────────────────────────────────────────────► │
 │                   {"v":2,"type":"response",…}  │   correlated by id
 │ ◄───────────────────────────────────────────── │
 │                      {"v":2,"type":"event",…}  │   unsolicited, any time
 │ ◄───────────────────────────────────────────── │
```

**Four guarantees enforced by omission** — the strongest form available to a type definition, and the
reason to look at what the protocol *lacks*:

1. **A heuristic cannot move the user.** Focus is never something a client is told to do directly; it
   arrives as an `Effect` the attention manager already cleared through the focus governor, and
   `Confidence` travels with every event so a guess stays a guess.
2. **Turn never approves a permission.** No request says so. Answering an Agent is `Request::WritePty` —
   the human typing.
3. **Turn never runs a command it inferred.** Processes start from a Template, a Pane definition, or
   `Request::RelaunchNode`. There is no "run this" verb.
4. **Turn never relaunches on its own.** A restore *reports* what it found and marks what could be started
   again; the client turns that into an offer (`PaneRestoreOutcome`).

**Compatibility.** Nothing uses `deny_unknown_fields`, deliberately: a newer daemon may add a field and an
older client must ignore it rather than fail. A change that would make an older client *misread* a message
bumps `PROTOCOL_VERSION`, and the handshake refuses the connection instead of letting it half work
(`conversation::a_stale_client_is_told_which_side_is_old_and_the_connection_ends`).

**Framing: newline-delimited JSON over a unix socket.** One JSON value per line, `\n`-terminated,
UTF-8. Chosen over length-prefixed binary for one reason that matters more than efficiency at this
stage: the most important boundary in the system stays readable.
`socat - UNIX-CONNECT:...turnd.sock` is a working client, a bug report can include the exact bytes, and
a second frontend can be written in any language without a codec library.

Two decoder guarantees, because a terminal multiplexer that drops its control connection on bad input is
worse than useless: **partial reads are normal** (`LineDecoder` buffers and never assumes chunk
boundaries mean anything), and **a bad line costs one line** — invalid JSON, an unknown shape or an
over-long line yields an error for that line and the decoder carries on. `MAX_LINE_BYTES` is 8 MiB and
`MAX_OUTPUT_CHUNK_BYTES` is 256 KiB.

**A terminal is cells, not bytes (protocol 2).** The daemon already keeps an authoritative `vt100` screen
per pane — it must, because thumbnails and the output heuristics work with no client attached — so a pane's
screen crosses the boundary already parsed: `cells::Grid`, with palette indices resolved to concrete `Rgb`
and reversed video applied. One VT emulator in the system, and with it the whole class of "the two screens
disagree" bug is gone. `screen::ScreenUpdate` carries the **rows that changed** with a per-attachment `seq`;
a client that misses one asks `resync_pane`, and the daemon independently makes the next update a whole
screen. `attach_pane`'s `stream` field defaults to `cells`; `bytes` stays available for anything that needs
the escape stream itself. Sizes, the diff rule and the resync rule are documented with measured numbers in
`docs/PROTOCOL.md` §2 and §8.

**Binary payloads: `TerminalBytes`, base64.** JSON has no byte type, and pty output is bytes — escape
sequences carry colours and cursor state, and a pty may emit invalid UTF-8. The cost is stated plainly
in the module's own docs: 33% inflation plus a pass over the data each way, irrelevant for keystrokes
and a redraw, not irrelevant for a `cargo build` firehose where 10 MB becomes 13.3 MB. Accepted for the
MVP because one human-readable frame format makes the boundary debuggable with `nc`. **The escape hatch
is already in the handshake:** `OutputEncoding` is negotiated in `Welcome`, so a length-prefixed binary
side channel can be added later without a protocol break. Base64 is implemented in-crate rather than
pulled in, keeping `turn-proto`'s third-party surface to serde alone.

**Requests: one flat enum.** Longer to read, but it makes the complete daemon surface visible in one
place and lets `Request::expected_result` name the response for every operation — checked by a test
against the response catalogue. Three product rules are enforced by the *shape* of the protocol rather
than by the daemon remembering to check them:

- **There is no request that approves an agent's permission.** Answering a permission prompt is typing
  into the agent's terminal, which is `Request::WritePty` — an explicit act by the human. Turn cannot
  approve on the user's behalf because the protocol gives it no way to say so.
- **There is no request that runs a command Turn inferred from output.** A process starts from a
  Template, a Pane definition or an explicit relaunch, all of which the user chose.
- **`Request::RelaunchNode` exists and nothing else restarts anything.** Restore offers; the user
  decides.

**Responses and errors.** Every success is a `Response` variant tagged with `result`. Failures never
arrive as a `Response` — they arrive as `ServerMessage::Error` carrying a `ProtoError` with a
machine-readable `ErrorCode` and a `message` that is for humans and never parsed. One error shape rather
than a per-request enum, because the UI's error handling is generic and a client in another language
should not have to model forty failure types to be correct.

**Pushes (`events`).** Everything the daemon says without being asked, which is the interesting half:
the whole point of the product is that thirty processes are getting on with things while the user looks
at one. Pushes are addressed but **not correlated** — no request id, because no request caused them. A
client processes them in arrival order and treats each as the current truth about what it names.

**View models (`view/*`): derive, never duplicate.** `SessionSummary`, `SessionDetails`,
`AgentSummary`, `TreeNodeView`, `AttentionView`, `WorkspaceSummary`, `TemplateSummary`. The daemon owns
every product rule. If the UI had to call `DisplayState::derive` itself, or decide whether a parent link
is a guess, or work out which of thirty Sessions is shouting loudest, those rules would exist twice —
and the second copy would be written by someone reading a screenshot. Now that the client is also Rust the
temptation is *larger*, not smaller: `turn-core` is importable, so a client could call `derive` itself.
It must not. A client that computes is a client that can disagree with the daemon. Anything already
modelled in `turn-core` is embedded as that type; the extra fields are strictly derived values.
**Provisional stays visible:** a guessed parent link and an inferred state both carry their uncertainty
into the view model, so the UI can render a guess as a guess.

**Two catalogue-level tests hold the contract together**, and they are the reason
`Request::expected_result` exists at all: `contract::every_request_names_a_response_variant_that_exists`
and `contract::every_response_variant_is_produced_by_at_least_one_request`. Between them a client can
treat the request→response pairing as load-bearing rather than as documentation that might be stale, and
`docs/PROTOCOL.md` cannot drift silently.

### 6.2 turn-store — SQLite persistence

**Status: Built. 119 tests** (106 unit, 9 in `tests/restart_restores_the_desk.rs`, 3 in
`tests/secrets_never_reach_the_disk.rs`, 1 doctest). Modules: `migrations`, `codec`, `redact`, `location`,
`error`, `repo/{workspace, session, node, event, attention, template, settings}`, behind a `Store` facade
(`open_default`, `open_in`, `open_at`, `open_in_memory`, plus `schema_version`, `journal_mode`,
`foreign_keys_enforced` and `compact`). WAL and enforced foreign keys are set at open.

Everything is synchronous: the daemon calls it from a blocking context and owns the ordering. There is no
runtime, no background thread and no lock in the crate.

**The persistence boundary is the most important thing about it**, because getting it wrong produces a
convincing lie. Persisted: Workspaces, Sessions, the layout tree, Templates, attention policies, the
Attention Queue, the event log, and process *metadata*. Never persisted: the pty master, the terminal grid
and its scrollback, the output broadcast channel, the vt100 parser state, live subscriptions.

And the line that makes restore honest: **`SessionRepo::load_for_restore` downgrades anything stored as
running to `Lifecycle::Orphaned`**, because a stored `Alive` only ever meant "alive when we last wrote".
Turn never relaunches anything on restore — it offers, and the user decides.

**Migrations.** The schema version lives in SQLite's own `user_version` header field rather than a table
Turn has to bootstrap: it is written inside the same transaction as the DDL, so a migration either lands
completely or not at all — there is no window where the tables are new and the recorded version is old.
Migrations are **append-only**; once a version has shipped its statements are frozen, because changing
them would leave every machine that already ran it with a schema no later migration accounts for.
**Downgrades are refused, loudly:** opening a newer database and writing to it would either fail on
unknown columns or, worse, succeed and drop the fields the newer build depends on.

**Codec.** Two column shapes, chosen per column. `tag` for payload-free enums, which land as a bare
`awaiting_user` rather than a quoted JSON scalar — those columns are filtered and grouped in SQL, and a
bare word keeps queries and `sqlite3` dumps readable. `json` for anything structured (a
`Lifecycle::Exited { code }`, a layout tree, a policy), because Turn always reads those whole, so
decomposing them into columns would buy nothing and cost a migration every time the domain grows a
field.

**Repositories.** Each borrows the connection rather than owning it, so a caller can hold several at
once and still have every write land in the same database with the same pragmas. None start threads or
take locks; the daemon calls them from a blocking context and controls ordering itself. Writes are
`INSERT ... ON CONFLICT DO UPDATE`, **never** `INSERT OR REPLACE`: `REPLACE` deletes the old row first,
which for a referenced row fires `ON DELETE CASCADE` and would take a Session's nodes, events and
pending attention with it — renaming a Session must not erase its history.

Per-repository notes worth knowing:

- **`session`** — a save is one transaction over three tables (session row, layout document, nodes),
  because a Session whose layout survived but whose nodes did not is not a Session anybody can restore.
- **`node`** — stores *only* metadata: pid, command, cwd, lifecycle, relation, exit code, external id.
  Enough to look a process up in the process table and try to re-attach, and enough to say "this was
  running and we can no longer find it" when that fails. **Not** stored: the pty, the scrollback, the
  terminal grid, the output channel — a pty master cannot outlive the process, and a restored scrollback
  would be a screenshot of a conversation the Agent no longer remembers.
- **`event`** — append-only. Nothing rewrites an event; a wrong state is corrected by a *new* event with
  `EventSource::UserCorrection`, which is what makes the log a usable account of what Turn believed and
  when. Every row keeps its `Confidence` and source, so weeks later Turn can still say "this read as
  waiting for you because a pty rule matched, not because the tool said so". `Retention` and
  `PruneOutcome` keep the log from eating the disk.
- **`attention`** — the queue is persisted, because it is the one piece of live state that must not
  evaporate on a restart: an Agent that blocked on a permission at 17:58 is still blocked at 18:02, and
  a queue rebuilt from nothing would quietly drop it until the Agent happened to say so again.
- **`settings`** — key/value with JSON values, not a column per preference, because a wide table needs a
  migration per preference and a migration is a thing that can fail on a user's machine. A value this
  build cannot parse is reported as a decode error naming the key, never silently defaulted.

**Errors are typed, not `anyhow` blobs**, because the daemon must react differently to each: a schema
from a newer build must stop it cold, a decode failure on one row must not take the whole Workspace list
down, and a missing data directory is something the user can fix.

**Location** resolution is a pure function of an explicit override and `TURN_DATA_DIR`, so the rules are
testable without a test mutating process-global state every other test in the binary also reads.

### 6.3 The UI

**Status: In progress.** `crates/turn-gui` is a **native window drawn on the GPU** — `eframe`/`egui` over
`wgpu`, one binary named `turn`, no webview, no HTML and no TypeScript anywhere in the repository. The
previous Tauri shell and TypeScript frontend were built, rejected by the product owner, and deleted; ADR-039
records why, what it cost and what it costs from here.

What exists today is the spike that settles the stack, not a finished window:

- **`src/cells.rs`** — a pane's screen as `Grid`/`Cell`/`CellAttrs`/`Rgb`, and the conversion from the
  daemon's `vt100`-parsed screen. The client paints cells; it does not parse an escape stream, so there is
  no second VT emulator and no way for two screens to disagree (ADR-009, ADR-039).
- **`src/theme.rs`** — the palette, and `state_marker()`, which returns a colour **and** a glyph together so
  that no caller can signal a state by colour alone. `every_state_has_a_glyph_as_well_as_a_colour` and
  `the_attention_colour_is_reserved_for_states_that_block_the_user` make that structural rather than a
  convention.
- **`src/view.rs`** — the status bar, the non-modal permission banner, the session sidebar with its indented
  hierarchy, a terminal pane painted cell by cell, and the attention queue.
- **`tests/snapshots.rs`** — `egui_kittest` renders the real widget tree through `wgpu` **with no display
  attached** and diffs against committed PNGs; `UPDATE_SNAPSHOTS=1 cargo test -p turn-gui` re-records. This
  is what makes a GPU-drawn frontend reviewable at all, and it has already earned itself: the first snapshot
  caught two labels drawn on top of each other, which the logic tests could not see.

The window is a **performer of effects and a reporter of context**, unchanged by the stack swap. It does not
decide when to interrupt — that is `AttentionManager`'s job, in the daemon, tested without a window — and it
never derives a state, a rank or a score; `state_label`, `severity`, `score`, `provisional` and
`relation_is_provisional` arrive computed (ADR-032). Only `Effect::focus` may move the user;
`focus_deferred` and `focus_denied` are verdicts to report.

Not built yet, stated plainly rather than implied by the section above: the live daemon connection, the
keymap, the command palette, the agent tree panel, the event log, the session overview, pane splitting and
dividers, the four perceptible effect channels, and the restore offers. The equivalents of all of these
existed in the deleted frontend and their designs are recorded — ADR-039 §"What was carried over" — but none
of that is code in this repository today. There is also no accessibility coverage yet
(`every_session_row_is_reachable_by_its_accessible_name` is committed failing and `#[ignore]`d) and no IME
work at all, which are the two things the webview used to provide for free.

---

## 7. Security model

Turn runs untrusted-ish programs (agents that generate their own commands) on the user's machine with
the user's credentials, and renders their output. The threat model is not a remote attacker; it is a
capable local program doing something surprising, either through a bug or through prompt injection in
content it read.

### 7.1 The hook server is local-only with per-node tokens — **implemented**

`HookServer` binds `("127.0.0.1", 0)` explicitly, never `0.0.0.0`: nothing off this machine has any
business reporting agent state. Every registered node gets its own 256-bit token, embedded in the path
(`POST /hook/{token}`), and an unknown token is refused and counted in `HookStats::refused`. The token
is what stops another process on the same machine from forging events for a Session — claiming an Agent
is blocked, or clearing a real permission demand. `unregister` revokes a token, after which any post
with it is treated as forgery.

The body limit (256 KiB) is applied by the server **before** the bytes are buffered, so a hostile
`Content-Length` costs nothing.

### 7.2 Turn never answers a hook with a decision — **implemented**

Claude Code's hook protocol allows a response body that allows or denies a tool call. Turn always
replies with an empty 200. Approving on the user's behalf is exactly what this product promises not to
do, and the protocol layer reinforces it: `turn-proto` has no request that approves a permission at all
(§6.1).

### 7.3 The process cannot drive the terminal's environment — **implemented**

Three `vt100::Callbacks` requests are refused and counted, and the counts are surfaced rather than hidden
because "a process tried this" is useful signal:

| Callback | Refused because | Counter |
| --- | --- | --- |
| `copy_to_clipboard` (OSC 52 write) | An Agent printing an escape sequence must not silently replace what the user is about to paste. | `blocked_clipboard_writes` |
| `paste_from_clipboard` (OSC 52 read) | **The exfiltration direction.** Answering it would write whatever the user last copied — often a password — straight into the process's stdin. | `blocked_clipboard_reads` |
| `resize` | Geometry belongs to the user's window, not the program in it. The pty size only ever changes because the UI said so. | `blocked_resizes` |

The read direction is the one worth naming separately: refusing clipboard *writes* protects the user's next
paste, but refusing clipboard *reads* is what stops a process turning the terminal into a data-exfiltration
channel for a secret the user never sent it.

### 7.4 Process-supplied titles and rendered rows are sanitised — **implemented**

A title arrives from an untrusted source and lands in Turn's sidebar. `sanitise_label` consumes whole
escape sequences — CSI to its final byte in `@..~`, OSC and other string sequences to BEL or `ESC \` —
rather than merely dropping control characters, because dropping only controls would leave a visible
`[2J` behind. It also strips characters that let text lie about itself: **bidirectional overrides**, which
would let a title render reversed, and **invisible Unicode tag characters**, which would let one smuggle a
hidden payload into a label. The result is trimmed and capped at 200 characters (`MAX_TITLE_CHARS`).

Two details matter more than they look:

- **The cap is applied on arrival, in `set_window_title`, not in `snapshot()`.** What arrives is an OSC
  payload the parser buffers without a bound, so an Agent can emit a megabyte-long title; retaining it per
  Pane would be a memory cost the *process* controls rather than Turn.
- **The same sanitisation applies to screen rows** (`sanitise_row`), not only titles, because anything the
  UI renders as text can carry the same trick.

Invalid UTF-8 in a title is replaced rather than fatal, and a single enormous line is bounded by the
terminal geometry.

### 7.5 Secrets are redacted before persistence — **implemented**

Turn launches processes with the user's environment, which on a developer machine reliably contains
`GITHUB_TOKEN`, `ANTHROPIC_API_KEY`, session cookies and cloud credentials. A store that survives
restarts also survives being copied into a bug report, synced to a backup, or read by anything else
running as the user — so none of that may be written down.

The rule is narrow and mechanical: **the value of any key that looks like a credential is replaced
before the row is built.** The key itself is kept, because "GITHUB_TOKEN was set" is exactly what Turn
needs in order to explain why an Agent could not authenticate after a restore, while its value is only a
liability. Matching is deliberately greedy — substring, case-insensitive — because redacting a variable
called `MONKEY_MODE` costs the user nothing while missing one called `deploy_key` costs them a
repository.

`ProcessNode::env_highlights` is the domain-side half of the same decision: selected entries, never the
whole environment.

Three integration tests assert the property rather than the mechanism, by writing real SQLite files and
searching them: `secrets_never_reach_the_disk::no_secret_value_is_present_anywhere_in_the_files_on_disk`,
`a_secret_survives_nowhere_even_after_the_daemon_restarts_and_prunes`, and
`a_process_environment_is_not_persisted_wholesale_even_when_it_looks_innocent`.

**Still open:** `TurnEvent::raw` holds the untouched hook payload, which can carry a `transcript_path`, a
`cwd` and a prompt excerpt, and key-based redaction does not touch free text. Whether `raw` is persisted at
all, or kept only in memory for debugging, is an open decision (`ROADMAP.md` §Open decisions).

### 7.6 No auto-approval, no auto-relaunch, no inferred execution — **implemented at every layer**

Three hard rules, each enforced structurally rather than by convention:

- Turn **never approves or denies a permission**. `risk::assess` colours a banner and orders a queue; it
  authorises nothing. The hook server always replies with an empty 200. The protocol has no
  approve request.
- Turn **never relaunches a process automatically on restore**. `RestoreBehaviour::ReattachOnly` is the
  default; `Request::RelaunchNode` is the only thing that restarts anything, and a user issues it.
- Turn **never executes a command it inferred from Agent output**. A command extracted from prose is
  display material only. This is why the heuristic layer produces events, never actions, and why
  `contract_codex` asserts that a tool-call payload is never turned into an approval or a command to run.

### 7.7 Scratch configuration is Turn's, not the user's — **implemented**

Adapters write hook configuration into a per-Session scratch directory Turn owns and deletes with the
Session. The user's own agent configuration files are never read for modification and never written.
Asserted, not assumed:
`claude::tests::preparing_writes_a_settings_file_and_passes_it_without_touching_user_config` checks that
the emitted path is inside the scratch directory.

### 7.8 The helper cannot be a weapon or a liability — **implemented**

`turn-hook` has no dependencies, only speaks `http://` to loopback, caps its payload at 256 KiB, times
out in 2 seconds, exits 0 unconditionally and prints nothing unless asked. It carries no credentials:
the only secret it handles is the per-node token, which arrives in a URL from Turn's own configuration
and is useless off this machine.

### 7.9 The window no longer has a sandbox, and that is a change for the worse — **stated, not mitigated**

Worth recording rather than quietly dropping. The deleted Tauri shell enforced a capability list: the
frontend had no filesystem access, no shell, no network plugin, and could reach only three commands plus the
notification plugin. That was a genuine second line of defence — if a rendering bug or a dependency had let
agent output influence the frontend, the frontend still could not have read a file or run a command.

`turn-gui` has no such boundary. It is a native process with the user's full privileges, and any code in it
can do anything the user can. Nothing replaces the capability list, and pretending the sandbox was
unimportant would be dishonest.

What still holds, and is what the security model actually rests on:

- **Agent output is data, never markup and never code.** There is no HTML, no `innerHTML` and no evaluator
  in the window at all — output arrives as `Cell`s with colours and attributes and is painted as glyphs, so
  the injection surface the webview had is gone rather than merely guarded. The escape-sequence classes that
  *are* dangerous are refused in the daemon before a client ever sees them (§7.2–§7.4: OSC 52, resize,
  title sanitising).
- **The window still authorises nothing.** §7.6's three rules are enforced in the daemon and in the protocol
  shape, not in the client, so a compromised or buggy client cannot approve a permission, relaunch a process
  or run an inferred command — there is no request that would let it.

So the net position is: one injection vector removed, one containment layer removed. The first is worth more
than the second in this specific product, because the containment only ever mattered if the injection
succeeded — but it is a trade, not a free win.

---

## 8. Performance budget and backpressure

### 8.1 Budget

Targets are for the design point: **30 concurrent panes across 10 sessions, one of them producing
build-volume output.** Values marked *enforced* are constants in the code today; values marked *target*
are not yet measured, because there is no running application.

| Property | Value | Status |
| --- | --- | --- |
| Retained raw bytes per Pane | 2 MiB (`DEFAULT_BYTE_CAPACITY`) | enforced |
| vt100 scrollback per Pane | 5,000 rows (`DEFAULT_SCROLLBACK_ROWS`) | enforced |
| Output channel depth per Pane | 512 chunks (`OUTPUT_CHANNEL_CAPACITY`) | enforced |
| Read granularity | 64 KiB (`READ_CHUNK`) | enforced |
| Exit detection latency | ≤ 100 ms (`WAIT_POLL_MS`) | enforced |
| Hook body limit | 256 KiB (`MAX_BODY_BYTES`), applied before buffering | enforced |
| Hook event buffer | 1,024 events (`EVENT_CHANNEL_CAPACITY`), then drop | enforced |
| Hook callback timeout | 3 s configured in the agent; `turn-hook` socket timeout 2 s | enforced |
| Protocol line limit | 8 MiB (`MAX_LINE_BYTES`); output chunk 256 KiB | enforced |
| Heuristic debounce / idle threshold | 750 ms / 2,000 ms | enforced |
| Process-table scans | on demand only, never on a timer | enforced by absence |
| Supervisor walk depth | 32 (`MAX_DEPTH`) | enforced |
| Focus changes | ≤ 3 per 10 s, ≥ 2 s apart | enforced |
| Keystroke to pty write | < 5 ms | target |
| Output to glass | < 50 ms at the 95th percentile | target |
| Idle daemon CPU | < 1% with 30 live panes and no output | target |
| Resident memory, 30 panes | to be measured — see below | **unmeasured** |

The memory figure is deliberately left open rather than guessed. Per-Pane byte rings are a hard 2 MiB
each, so 30 Panes is ~60 MiB of ring. The vt100 grid grows toward its 5,000-row cap as output arrives
and its per-cell cost has not been measured in this workspace; at 80 columns that is 400,000 cells per
Pane at full scrollback. If the measured figure is uncomfortable, the levers already exist:
`TerminalBuffer::with_capacity` takes both bounds, so they can be tuned per `PaneKind` — a build log
does not need the same scrollback as an Agent conversation. Tracked as a risk in `ROADMAP.md`, not as a
solved problem.

Base64 on the protocol is the other known cost, quantified in `turn-proto`'s own docs: 33% inflation
plus a pass each way, with `OutputEncoding` already negotiated in the handshake as the escape hatch.

Two of these targets were set against a webview and now are not (ADR-039). "Output to glass" no longer
includes a JavaScript event loop, a DOM write or a canvas composite — a native client paints cells straight
into a GPU frame — and ADR-001's assumption that the renderer bounds throughput no longer applies. That
should make the target easier, not harder. It is still a **target**: nothing here has been measured, and a
GPU frontend has its own failure mode the webview did not, namely a per-frame cost that scales with painted
cells rather than with bytes received. Thirty panes of dense colour at 60 fps is the measurement to take, and
it has not been taken.

### 8.2 Backpressure

The rule: **a slow consumer degrades, it never grows a queue and it never stalls a producer.** Five
mechanisms implement it, in three different currencies.

**Terminal output (bytes):**

1. **Bounded broadcast channel.** Each `PtyProcess` publishes into a `tokio::sync::broadcast` channel of
   512 chunks. A subscriber that falls behind receives `RecvError::Lagged(n)` telling it exactly how
   many messages it missed. Deliberately modest: falling behind should be detected and repaired quickly,
   not buffered indefinitely.
2. **Resynchronise from a replay.** The recovery path is prescribed, not left to the caller: on
   `Lagged`, discard what you have, take `PtyProcess::replay()` to rebuild the current screen exactly,
   and continue from the live stream. This works because the *buffer*, not the channel, is
   authoritative — the reader thread writes the buffer before it broadcasts, so a dropped chunk is never
   lost data, only late data.
3. **Bounded byte ring and bounded parser scrollback.** 2 MiB and 5,000 rows per Pane, with
   `is_truncated()` so a partial replay is known to be partial rather than silently wrong. A single
   write larger than the ring keeps its tail. An unbounded scrollback is a memory leak with a nice name.

**Agent events:**

4. **The hook server drops rather than stalls.** A full 1,024-event channel discards the event and
   increments `HookStats::dropped`. If the daemon stops draining, the correct behaviour is to lose
   events and say so, not to slow every Agent on the machine to the speed of the slowest consumer. A
   dropped receiver does not stop the server answering; agents must not start failing because the daemon
   went away.

**Attention:**

5. **The attention path has its own backpressure, in its own currency.** Dedup keys collapse repeated
   demands, the focus rate limiter caps interruptions per window, per-Session cooldowns and mutes cap
   them per Session, and the heuristic debounce collapses flicker before an event is even built. A burst
   of a hundred events cannot become a hundred interruptions.

Consequences worth being explicit about:

- **A send failure is not an error.** `let _ = output_tx.send(data)` — no subscribers right now simply
  means nobody is listening; the buffer still holds the bytes. This is what lets the daemon run Sessions
  with no UI attached at all.
- **One noisy process cannot stall another.** Each pty has its own reader thread and its own channel;
  there is no shared queue to head-of-line block. Asserted by
  `process::tests::heavy_output_does_not_block_a_second_process`.
- **The protocol carries the same contract to the UI.** `MAX_OUTPUT_CHUNK_BYTES` caps a frame, a bad
  line costs one line rather than the connection, and a UI that cannot keep up must be told it lagged
  and re-request a replay — exactly as an in-process subscriber does.
