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
| `turn-core` | Built | reproduce | Domain, state, attention and ADR-040 hierarchy/lease/preview values. |
| `turn-proto` | Built | reproduce | Framing, terminal cells, protocol-v3 hierarchy and protocol-v4 authenticated handshakes. |
| `turn-store` | Built | reproduce | Append-only migrations, store-wide fences, hierarchy and secure hook cleanup. |
| `turn-pty` | Built | reproduce | Ptys, buffers, supervision. |
| `turn-hook` | Built | reproduce | The `turn-hook` helper binary and its library. |
| `turn-agents` | Built | reproduce | Adapter trait, Claude Code, Codex, heuristics, registry, hook server, risk. |
| `turnd` | Built | reproduce | Atomic Session/lease lifecycle, PTYs, hierarchy projection and restore vertical. |
| `turn-gui` | Built for first vertical | reproduce | Unified native tree, panes, previews, inspector, attention and GPU snapshots. |

There is one command and one test runner: the frontend is Rust now (ADR-039), so there is no `pnpm`, no
`vitest` and no second lockfile.

```sh
cargo test --workspace --all-targets -- --test-threads=1
```

Counts are intentionally not pinned here: every regression proof changes them. The release audit runs the
workspace serially because PTY and loopback-hook tests allocate real operating-system resources.

What is **not** done, so “built” is not read as “shippable”: the deterministic Reviewer vertical crosses
daemon/store/protocol/GUI state and the real loopback Claude hook transport, but an authenticated installed
Claude Code binary has not completed the manual native-window smoke test. Advanced tree management,
performance measurement, packaging, Linux visual sign-off and IME remain; see `ROADMAP.md`.

---

## 1. Shape of the system

Turn is one Rust cargo workspace: six library crates, the daemon, and the window. One process owns the work
(`turnd`, the daemon); another renders it (`turn-gui`). They talk over a protocol rather than sharing memory,
because the whole point of the daemon is that the UI can go away.

The desktop executable is also the zero-state bootstrapper, but not the daemon owner. It resolves one
absolute data-directory/socket pair, reuses a reachable endpoint or starts a detached sibling `turnd`, then
connects through the same protocol either way. The companion has an independent process lifetime: closing
the window drops only its monitor, never the daemon or its PTYs. ADR-042 records the launch and packaging
contract.

```
                     ┌──────────────────────────────────────────────────┐
                     │  turn-gui  (eframe/egui on wgpu — native, no     │
                     │  webview)              BUILT: FIRST VERTICAL    │
                     │  unified hierarchy · user-chosen panes ·         │
                     │  contextual inspector · permission banner ·      │
                     │  reuse/start detached sibling daemon             │
                     └───────────────────┬──────────────────────────────┘
                                         │ turn-proto  v3 BUILT
                                         │ newline-delimited JSON over a unix socket
                                         │ ── Request  → Response
                                         │ ── ServerMessage pushes (state, cells/output, effects)
                     ┌───────────────────┴──────────────────────────────┐
                     │  turnd — the daemon     BUILT: FIRST VERTICAL    │
                     │                                                  │
                     │  owns: pty handles · session/checkout registry · │
                     │  canonical data-dir process lock ·               │
                     │  one AttentionManager · the hook server ·        │
                     │  write leases · hierarchy projection ·           │
                     │  supervisor timing · all store writes ·          │
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
                 │                │                     │        │  settings,        │
                 │                │                     │        │  hierarchy}       │
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

**Status: built, including ADR-040 hierarchy/lease/preview values.** No I/O, no pty, no database, no UI. This
is why the rules that matter can
be tested exhaustively without spawning a process, and why every function that needs the time takes
`now_ms: i64` as a parameter instead of reading the clock.

### 2.1 Responsibility

Own the vocabulary and the decisions: what entities exist, what states they can be in, what events can
happen, and — given an event, a policy and what the user is currently doing — what should happen to the
user's screen.

### 2.2 Public surface

| Module | Types | Purpose |
| --- | --- | --- |
| `ids` | `WorkspaceId` `SessionId` `NodeId` `PaneId` `TemplateId` `EventId` `AttentionId` `CheckoutId` `LeaseId` | Prefixed newtype strings (`sess_ab12cd34ef56`). A `PaneId` cannot be passed where a `SessionId` is expected, and the prefix keeps them readable in logs and SQLite. |
| `state` | `Lifecycle` `Turn` `AwaitingReason` `DisplayState` | The two-axis model. §2.3. |
| `event` | `TurnEvent` `EventKind` `Confidence` `EventSource` `Severity` `Risk` `AgentRef` | The single event vocabulary. §2.4. |
| `model` | Existing Workspace/Session/process/Layout/Template types plus `SessionMode`, `WorkspaceCheckout`, `WorkspaceWriteLease`, `Relationship`, `AgentName`, `ActivityPreview`, `PaneNodeBinding`, `TreeUiState` | Normalised entities and ADR-040 hierarchy support values. §2.5. |
| `attention` | `AttentionPolicy` `Trigger` `Action` `Sound` `AttentionQueue` `AttentionEntry` `EntryState` `FocusGovernor` `UserContext` `FocusDecision` `FocusDenial` `DeferReason` `AttentionManager` `Effect` | Attention coordination. §2.6. |
| crate root | `now_ms()` | The one clock read, for the edges. |

### 2.3 The two-axis state model

`Lifecycle` tracks the OS process. `Turn` tracks the conversational turn and exists only for agents
(`ProcessNode::turn` is `Option<Turn>`; a shell has `None`). They change independently.

```
Lifecycle: Spawning → Alive → { Exited{code} | Signaled{signal} }
                    ↘ Orphaned      (stored PID may live after daemon restart; PTY handle lost)
                    ↘ Reconnected   (reserved for a backend that proves reattachment; not emitted today)
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
(`"WAITING"`, `"PERMISSION"`, `"turn done"`), `demands_user()`, and `severity()` for ranking. `YOUR TURN`
belongs to the Session/Attention projection: an exact Agent remains `WAITING` or `PERMISSION`, and a scoped
unresolved child demand may badge the Session while its parent remains `RUNNING`.

Note the deliberate asymmetry between `is_terminal()` and `is_failure()`: `Lost` is terminal but not a
failure, because failing to re-attach is not the same as the work having gone wrong.

### 2.4 The event vocabulary

Every **session-scoped runtime fact** — a Claude Code hook, a Codex `notify` callback, a pty heuristic,
the process supervisor, a user correction — is normalised into a `TurnEvent` before downstream state and
attention consume it. Consumers retain its source and confidence without depending on a vendor payload.

ADR-040 makes the boundary explicit: not every wire push or persisted record is a `TurnEvent`. A lease
denial may happen before a Session exists; tree selection belongs to one UI surface; preview updates are
high-frequency current state. Those use typed lease/audit records, per-surface UI state and coalesced
snapshot pushes respectively. They are not forged into the session event log.

**Produces:** nothing; this is a data module.
**Consumed by:** `AttentionManager::ingest`, the daemon's state reducer, `turn-store`'s `EventRepo`,
`turn-proto`'s push messages.

`EventKind` variants, with their wire names (serde-tagged, so the JSON name is part of the contract):

| Wire name | Carries | Notes |
| --- | --- | --- |
| `process.started` | `pid`, `command` | |
| `process.exited` | `code` | Resolves attention for the node. |
| `process.failed` | `code?`, `signal?` | `Severity::Error`. |
| `process.spawned_child` | `child`, `pid`, `command`, relationship evidence | Runtime observation. The edge stores relationship kind and confidence separately from event confidence. |
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
| `agent.subagent_started` | `agent_type?`, `agent_id?`, declared name when actually supplied | Confirmed discovery; a role/type is not silently promoted to a name and no Pane is opened. |
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

`raw: Option<String>` may keep an untouched payload in memory for debugging bad adapters. It is never
rendered as-is and is not persisted by default; key-based environment redaction is insufficient for free
text such as prompts and transcript paths (§7.5 and `docs/SECURITY.md`).

### 2.5 Entities

**`Workspace` / `WorkspaceCheckout`** — the persistent project and the filesystem roots known beneath it.
The Workspace owns root path, git remote, launch environment, defaults and baseline policy. A checkout owns
canonical path identity, branch, whether it is primary, and an explicit list of ports, containers, databases,
caches or other resources a worktree does not isolate. Path spelling is never the exclusivity key.

**`Session`** — the unit of work. Name in the user's words, cwd, env, `Layout`, `SessionTree`,
`AttentionPolicy`, `SessionStatus` (`Active`/`Paused`/`Archived`), `RestoreState`, tags, git branch,
linked PR reference, pin/favourite, `parent_session` for duplicates, checkout assignment and the closed
`SessionMode` (`main_checkout`, `read_only`, `isolated_worktree`). `display_state()` returns `Idle`
for an empty tree — a Session whose processes have not started is not a mystery — and otherwise the
tree's aggregate. `sidebar_rank()` returns `(pinned, demands_user, severity, last_activity_ms)` as a
tuple rather than an `Ord` impl, because ordering is a presentation concern that may differ per view.

**`WorkspaceWriteLease`** — daemon-owned exclusivity for the primary checkout, not for a window or focus.
The semantic record carries Workspace, Session and checkout identity, `ExclusiveWrite`, state, acquisition
time and heartbeat. At most one non-released lease exists for a canonical primary checkout. Acquisition is
atomic and precedes Session insertion, init commands and process/Pane materialisation. Heartbeat expiry is
evidence for reconciliation, never authority to steal. Closing a UI, archiving while processes live or
`KeepProcesses` does not release it.

**`ProcessNode` / `SessionTree`** — the process hierarchy. Stored **flat with parent pointers**, not as
nested structs: processes arrive out of order (a child's hook can land before the parent's spawn
notification), and re-parenting a flat map is trivial where re-parenting a tree is not.
`order: Vec<NodeId>` preserves insertion order so the tree renders stably instead of in hash order.

Normalised ownership remains `Session.workspace_id`, `ProcessNode.session_id` and `ProcessNode.parent`.
ADR-040 does not introduce polymorphic Workspace/Session/Node parent foreign keys. A relationship edge
stores meaning (`spawned_by`, `owns_process`, related or unknown) and confidence independently. An agentic
node also carries lossless naming: declared name when the source supplied one, user-facing display name,
source, confidence and whether the user renamed it.

Navigation text is a separate trust boundary from typed event decoding. User-authored Workspace, Session
and Template names with C0/C1/ANSI/bidi/invisible formatting are rejected. Agent and OS process metadata is
sanitised and bounded before it reaches reducer, inspector, protocol or SQLite; discovered argv is capped
by count, per-argument characters and aggregate characters. Raw supervisor values remain transient inputs
to PID traversal/classification, not an alternate durable label source.

`SessionTree::relink` enforces the `Relation` ladder — a `Confirmed` link is never downgraded by an
`Inferred` one — and refuses to create a cycle, with a 1,000-hop defensive bound in case the store ever
hands it a corrupt tree. `remove` promotes children to roots with `Relation::Unknown` rather than
deleting them or silently re-attaching them elsewhere. `aggregate_state()` is the **most severe**
state, not the most recent.

**`Layout` / `Pane` / `PaneNodeBinding`** — the pane arrangement and its views onto runtime nodes. Layout is
a tree of `Split`s whose `children: Vec<Child>` hold a
fractional `size`. Splits hold a list rather than exactly two children so three side-by-side Panes are
one split with three children instead of a lopsided nest; that is what makes resize behave the way a
user expects. `split` joins an existing same-direction split as a sibling and shrinks everyone
proportionally. `resize` borrows from the next sibling with a 5% floor so a Pane cannot be resized out
of existence. `close` refuses on the last Pane and collapses a split left with one child. `zoomed`
never mutates the tree, so un-zooming restores exact previous geometry. `sizes_are_normalised()` is
the structural invariant; `normalise()` repairs a hand-edited Layout on load.

A binding belongs to a Session and connects one Pane to one node; a node may have zero, one or many
bindings. The binding table is authoritative after migration. Process identity never points back to one
privileged Pane, and closing a binding never stops the node. A node with semantic events but no independent
PTY can be shown as Preview or Process Details, not as a fabricated terminal.
A temporary binding additionally belongs to one live `surface_id`; it is hidden from other surfaces and
expires when that surface connection is replaced, its last client disconnects, or the daemon restarts.
Permanent Layout bindings and the Agent remain unchanged.

**`ActivityPreview` / `TreeUiState`** — bounded navigation state. Preview is sanitised, redacted,
provenance-labelled text derived from semantic status or one stable screen line; it is not terminal history.
Tree expansion and selection are stored per stable `surface_id`. Neither the preview projection nor UI state
changes process ownership, Layout, focus or Attention.

**`Template`** — a reusable Session shape. `from_layout` strips `node_id` bindings (a Template must not
remember which process it was cloned from); `instantiate` reassigns every `PaneId` so two Sessions from
one Template share no identity. Four built-ins: `Blank`, `Coding`, `PR Review`, `Pair of Agents`.
If main-checkout creation conflicts, typed read-only/worktree Template requests preserve the id and naming
inputs; only the daemon instantiates the authoritative Layout/env/Attention/tmux/init-command definition.

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

**`queue`** — ordering. `AttentionEntry` is keyed for dedup on Session, reason and an exact subject: a
known `node`, or an unresolved authenticated `parent + external id?` scope. A completely unanchored legacy
entry stays explicitly unassigned. `score(now_ms)` is
`base_priority + state_penalty + confidence_penalty + age_bonus + priority_boost`, where the age
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
- Response resolution and lifecycle cleanup are different operations. A response closes only its exact
  node or parent/external-id flow. A terminal runtime additionally owns node-less flows anchored on itself;
  its exit removes those and their deferred focus without erasing exact live children. `ProcessFailed`
  performs that cleanup and still proceeds through the failure policy to create the new failure demand.
- A deferred focus retains the same exact node or parent/external-id subject as its queue entry. `tick`
  requires that subject—not merely some demand in the same Session—to remain actionable. Snooze, dismiss,
  mute and terminal lifecycle cancel the matching deferred jump. Muting suppresses interruption while
  preserving the policy's durable queue evidence.

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

**Status: Built.** Depends on `turn-core` (ids, state,
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
that text cannot; the parsed screen makes attached cell rendering, on-demand previews and output
heuristics possible without a hidden Session-overview consumer.

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

That conversion lives in `turnd`: `core/spawn.rs` and `core/events/exit.rs` construct the process events,
and `core/supervise.rs` constructs `process.spawned_child`. Unit tests cover exit/supervisor joins and the
daemon integration suites cover spawn, child adoption, Attention cleanup and process termination. The join
remains the riskiest reducer in the system, so the out-of-order and ambiguous-agent regressions stay
load-bearing (`ROADMAP.md` §Risks 4); it is no longer unverified code.

---

## 4. turn-agents — the adapter layer and the hook server

**Status: built and green in the serial workspace audit; reproduce with `cargo test -p turn-agents`.**
Modules: `adapter`,
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

**Status: Built. Zero dependencies.**

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

**Status: built for the automated first vertical.** `main.rs` is a real entry point that parses
options, initialises logging, resolves a `Config` and calls `turnd::start`. The library declares `config`,
`paths`, `instance`, `logging`, `options`, `error`, `server` and `core`, with `core` split into `spawn`,
`supervise`, `restore`, `events`, `requests`, `views`, `attention`, `clients`, `command` and `output`.
Unit and integration coverage lives beside those modules and in `tests/{desk,agents,surface,restart,
attention,binary,cells}.rs`; counts are intentionally reproduced rather than frozen here.

The release audit runs `cargo test -p turnd -- --test-threads=1` and the full workspace with all targets
serially. The exact
Reviewer tests cross both the production reducer and the real loopback Claude hook transport. An
authenticated external Claude Code session in the packaged native window remains a separate acceptance
gate; deterministic coverage is not evidence that credentials, signing or the installed release work.

#### Desktop bootstrap and process lifetime

`turn-gui::transport::socket::startup_paths` resolves the data directory and control socket once and anchors
relative overrides before any development fallback changes working directory. `turn-gui::companion` then
probes that exact endpoint. It never replaces a reachable listener or a non-socket filesystem entry. An
absent/refused socket causes a detached launch with explicit `--socket` and `--data-dir`; the protocol
handshake, not the probe, establishes that the listener is a compatible daemon.

The launch source order is `TURN_TURND_BIN`, a `turnd` sibling beside `turn`, then — for debug builds only —
`cargo run` against the fixed source-workspace manifest. The debug path builds both `turnd` and `turn-hook`
before launch. Packaged and CI builds produce `turn`, `turnd` and `turn-hook` as siblings; the daemon locates
the helper beside its own executable and never searches `PATH`. This prevents pairing components from two
installations. Missing `turn-hook` degrades only adapters that require command hooks and is reported in the
launch plan.

The companion receives null stdin, append-only owner-only log files and, on Unix, its own process session.
The GUI retains a child handle only to reap and surface exit status; dropping it is not termination.
Synchronous launch errors and later exits are visible in the window and `turnd.log`. Exit code 3 is treated
as provisional startup contention because a second window may have lost a safe race; a successful handshake
with the winning daemon clears that diagnostic.

Endpoint occupancy is not authority over durable state. Before opening SQLite or restoring anything,
`turnd` acquires the canonical data-directory process lock described below. That lock makes two simultaneous
launches and two socket aliases safe: at most one process owns the store and PTYs. UI lifetime independence
does not change the daemon-restart limitation — if `turnd` exits, its PTY masters do too.

#### Responsibility

The daemon is the only process that holds a pty handle. It owns:

- the Session registry and the authoritative `SessionTree` per Session;
- an exclusive process lock on the canonical data directory, acquired before SQLite, migrations or
  restore, independently of the chosen control socket;
- checkout identity and the write-lease arbiter for every Workspace;
- the revisioned `HierarchySnapshot` projection and reconciliation of legacy checkout ownership;
- one `PtyProcess` per PTY-backed runtime node; zero-to-many Panes may view that node;
- zero-to-many Pane bindings per runtime node and bounded/coalesced Activity Preview state;
- one `AttentionManager` for the whole application — the queue is global by design, being the ordered
  list of everything wanting the user across every Workspace;
- the `HookServer` and its token table;
- supervisor scans, triggered on demand;
- all writes to `turn-store`, from a blocking context, controlling ordering itself;
- the `turn-proto` unix-socket endpoint the UI connects to.

It is also the **only** place where a pty fact and an Agent fact are joined: mapping a hook payload's
`session_id` to a `NodeId` via `SessionTree::find_by_external_id`, or a supervisor `ObservedProcess` to
an existing node via `find_by_pid`. Concentrating that join is deliberate; it is where a wrong
correlation would silently corrupt state. When Claude delivers a worker callback through the parent's hook
endpoint without `agent_id`, the reducer binds it to a child only when exactly one live candidate exists,
and downgrades that attribution to `inferred_high`. With zero or several candidates the event stays
node-less/`unknown`; it never borrows the parent or an arbitrary sibling. An explicit but not-yet-declared
worker id is stronger evidence than the current tree shape and also remains unresolved instead of falling
through to a different unique child. Node-less Attention is durably scoped by authenticated hook parent and
optional external worker id, so a prompt submission resolves only that exact provisional flow and cannot
clear a sibling or another parent in the same Session.

Creating a `main_checkout` Session is one daemon transaction boundary: canonicalise and validate the
checkout plus the effective Session/Pane working directories, persist the Session/assignment and acquire its
exclusive lease atomically, then run init commands and materialise processes/Panes. Working-directory
validation is repeated immediately before every PTY launch because symlinks and stored Layouts can change
after creation. The canonical cwd must be contained by the Session's registered primary or worktree checkout;
this constrains where a process starts, not which files same-user code can later access. A conflict rolls the
store transaction back and returns the current owner and allowed alternatives before any external side
effect. Duplicating a Session never copies an active lease. Release is fenced and occurs only after no
runtime node owned by the Session remains running; that same atomic operation demotes the Session to
`ReadOnly`. UI close, archive and `KeepProcesses` are not release signals.

#### Why the daemon exists from day one, and exactly what it buys

**It buys:** the UI can crash, be quit, be hot-reloaded during development, or be updated, and the
Agents keep running. Reconnecting re-attaches — for each visible Pane, the UI attaches to its bound live
runtime node, receives the daemon-owned screen/replay and subscribes to the live stream. The Pane is a view;
the runtime node, not that view, owns the PTY buffer.

**It does not buy:** survival of the daemon exiting. The pty master lives in the daemon's file table,
and `PtyProcess::drop` deliberately terminates the process it owns — `terminate()` then `kill()` —
because closing a Session must not leave strays holding ptys, which are a finite kernel resource. When
the daemon goes, the ptys close and the children get SIGHUP.

This is stated plainly rather than papered over, because the honest boundary is what `Lifecycle` is
shaped around. `Orphaned` (stored runtime may still exist, handle lost) and `Lost` (the recorded runtime
cannot be found) report the current restart truth instead of inventing an exit code. `Reconnected` is
reserved for a future backend that can prove PTY reattachment; the current daemon does not emit it on
restore. Making work survive the *daemon* is a different problem with a known
solution — tmux, which the model already has flags for — and it is deliberately out of the MVP
(ADR-007).

**Turn never relaunches a process automatically on restore.** It offers; the user decides.
`RestoreBehaviour::ReattachOnly` is the default, and `Relaunch` marks Panes whose command may be offered
again, such as a shell; it is not authority to run them unprompted. `turn-proto` enforces this structurally: `Request::RelaunchNode` is
the only request that starts anything, and it is user-initiated.

Before opening SQLite, running migrations or restoring state, `turnd` must own the non-blocking OS lock on
the canonical data directory. A different socket path is not a second ownership domain. Only after that
process boundary is established does a new daemon atomically change every unreleased checkout lease to
`recovery_required` while preserving its identity, generation and last heartbeat. Restore never adopts the
former daemon's authority, and heartbeat/launch paths require an `active` lease. The durable Attention
queue is loaded without replay: id, age, confidence, priority, snooze and acknowledgement survive exactly;
its exact node or unresolved parent/external-id correlation scope survives as well. Interaction demands
whose exact node/parent no longer runs are removed; postmortem evidence explicitly marked
`survives_owner_exit` (for example failure/completion facts) remains.

#### Event flow

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
                                               ▼
                                  one SQLite checkpoint
                             Session → Event → Attention → COMMIT
                                               │
                                    ┌──────────┴───────────────┐
                                    ▼                          ▼
                              turn-proto                  Vec<Effect>
                           (push view models) ───────────▶ UI performs them
                                                            (and acknowledges Focus, so
                                                             UserContext stays true)
```

`UserContext` — last keystroke, whether the window is frontmost, the active Session, whether a sensitive
operation is in flight — flows the other way, from UI to daemon. State transitions are sent immediately;
while a typing burst continues, the client coalesces a bounded heartbeat and schedules a repaint before
the daemon's 1,500 ms grace can expire. Sending only the first key makes a long burst look idle; sending
every key turns protection state into socket pressure. Neither is acceptable.

#### Failure modes to design for

- **The UI is gone when an `Effect::Notify` is produced.** Effects addressed to a disconnected UI must
  be dropped or coalesced, not queued indefinitely.
- **A hook arrives for an unknown `session_id`.** Drop it and log; never create a Session from a hook.
  The hook server already refuses an unknown *token*, which is the stronger check.
- **The store is unwritable** (disk full, permissions). The daemon keeps PTYs running, suppresses the
  uncommitted event's projections/effects and places later runtime events behind a FIFO retry barrier.
  Session/Attention standalone writes cannot leak the partial projection. Every protocol request first
  retries the oldest checkpoint and returns `unavailable` if it is still blocked, so neither a read nor a
  rejected mutation can cross the barrier. A prolonged outage still needs a surfaced degraded-state
  indicator and bounded/coalesced deferred-event hardening.
- **A schema written by a newer build.** `turn-store` refuses a downgrade loudly, and the daemon must
  stop cold rather than write to it.
- **`rusqlite` is synchronous.** Every store call must be off the reactor or the event loop stalls, and
  it will not be obvious in testing.

---

## 6. turn-proto and turn-store

Both base layers and their ADR-040 protocol-v3/migration extensions are built and exercised through the
daemon vertical. Planned operations are labelled as such in `docs/PROTOCOL.md`; types alone do not count.

### 6.1 turn-proto — the daemon↔UI protocol

**Status: protocol v4 authenticated hierarchy vertical built.** Framing, terminal cell transport,
request correlation and the safety omissions remain unchanged. Version 3 replaces independent navigation
bootstrap with one revisioned hierarchy projection and adds structured checkout conflict, Preview, binding
and per-surface tree-state operations. Version 4 makes the opening `hello` carry the daemon generation's
ephemeral capability. The protocol crate is still types only — no I/O, tokio or socket — so the contract can
be tested without either process existing.

**The connection.** A versioned envelope (`ClientFrame` / `ServerFrame`) carries four things: a
`hello`/`welcome` handshake resolved by `negotiate()`, id-correlated `request`/`response` pairs, and
unsolicited `event` pushes at any time.

```text
UI                                             turnd
 │  {"v":4,"type":"hello","auth_token":…}       │
 │ ─────────────────────────────────────────────► │
 │                    {"v":4,"type":"welcome",…}  │   authenticate + negotiate()
 │ ◄───────────────────────────────────────────── │
 │  {"v":4,"type":"request","id":"r-1",…}         │
 │ ─────────────────────────────────────────────► │
 │                   {"v":4,"type":"response",…}  │   correlated by id
 │ ◄───────────────────────────────────────────── │
 │                      {"v":4,"type":"event",…}  │   unsolicited, any time
 │ ◄───────────────────────────────────────────── │
```

**Four guarantees enforced by omission** — the strongest form available to a type definition, and the
reason to look at what the protocol *lacks*:

1. **A heuristic cannot move the user.** Focus is never something a client is told to do directly; it
   arrives as an `Effect` the attention manager already cleared through the focus governor, and
   `Confidence` travels with every event so a guess stays a guess.
2. **Turn never approves a permission.** No request says so. Pending questions and permissions are answered
   only by `Request::WritePty` — the human typing. A reviewed context handoff is refused at a pending prompt
   and may target only an idle or done Agent.
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
per PTY-backed runtime node — it must, because on-demand previews and output heuristics work with no client
attached — so a bound Pane's screen crosses the boundary already parsed: `cells::Grid`, with palette indices resolved to concrete `Rgb`
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
  into the agent's terminal, which is `Request::WritePty` — an explicit act by the human. The separate
  reviewed context-handoff capability is refused while a question, permission or interaction is pending,
  so it cannot become an approval side channel.
- **There is no request that runs a command Turn inferred from output.** A process starts from a
  Template, a Pane definition or an explicit relaunch, all of which the user chose.
- **`Request::RelaunchNode` exists and nothing else restarts anything.** Restore offers; the user
  decides.

**Responses and errors.** Every success is a `Response` variant tagged with `result`. Failures never
arrive as a `Response` — they arrive as `ServerMessage::Error` carrying a `ProtoError` with a
machine-readable `ErrorCode`, a human-only `message` and optional typed `context`. Generic failures need no
payload. A `conflict` may carry `context.kind = workspace_write_lease_conflict` with owner
Workspace/Session/checkout, lease identity/generation and the closed alternatives `focus_owner`,
`create_read_only`, `create_isolated_worktree`, `cancel`. `stale_lease_generation` identifies a fenced
heartbeat/release attempt. No client parses `message` or `detail` to decide recovery.

**Pushes (`events`).** Everything the daemon says without being asked, which is the interesting half:
the whole point of the product is that thirty processes are getting on with things while the user looks
at one. Pushes are addressed but **not correlated** — no request id, because no request caused them.
Terminal attachments keep their per-attachment sequence. Hierarchy pushes carry a monotonic projection
revision; a client that observes a gap discards the stale hierarchy and requests a full replacement.
`ActivityPreviewChanged`, `PaneBindingsChanged` and `WorkspaceWriteLeaseChanged` carry current bounded
state and do not enter the append-only `TurnEvent` log.

**View models (`view/*`): derive, never duplicate.** `HierarchySnapshot` is the navigation model:
`revision`, `TreeSurfaceState { surface_id, … }` and ordered `WorkspaceTreeView` roots containing
checkout/lease state and `SessionTreeView` children. Runtime node rows retain parent/depth and draw order,
derived state/badges, relationship meaning/confidence, zero-to-many `pane_bindings` and safe Activity
Preview.
`SessionSummary`, `SessionDetails`, `AttentionView` and administrative Workspace/Template projections remain
useful, but the client does not join them to invent a second tree. The daemon owns every product rule. A
client never calls `DisplayState::derive`, guesses an edge, computes urgency or synthesises a declared name.
Provisional state remains explicit on the wire.

**Protocol v3 navigation operations.** Bootstrap is `GetHierarchy`; `ListWorkspaces`, `ListSessions` and
`GetSession` remain administrative/detail endpoints, not a navigation recipe. `SetTreeExpanded` and
`SelectTreeNode` are scoped by stable `surface_id` and acknowledged without broadcasting another surface's
selection. Implemented node actions include `GetPreviewHistory`, `SetPreviewVisibility`,
`OpenNodeAsTemporaryPane` and `FocusPaneForNode`; rename and audited relationship correction remain planned.
Lease release carries expected fencing generation; `CreateSession` implicitly requests the main checkout,
while dedicated read-only/worktree operations express the safe alternatives. No creation path returns a
partial Session on conflict.
`CreateReadOnlySessionFromTemplate` and `CreateWorktreeSessionFromTemplate` keep a failed Template request
authoritative instead of asking the GUI to reconstruct panes from `TemplateSummary`; absolute primary cwd
values are mapped repository-relatively for the isolated checkout.

**Two catalogue-level tests hold the contract together**, and they are the reason
`Request::expected_result` exists at all: `contract::every_request_names_a_response_variant_that_exists`
and `contract::every_response_variant_is_produced_by_at_least_one_request`. Between them a client can
treat the request→response pairing as load-bearing rather than as documentation that might be stale. These
tests do not validate prose or example field lists; the release audit checks those against the Rust types.

### 6.2 turn-store — SQLite persistence

**Status: built, including the hierarchy repository and append-only migrations through 009.** Modules: `migrations`,
`codec`, `redact`, `maintenance`, `location`, `error`,
`repo/{workspace, session, node, event, attention, template, settings, hierarchy}`, behind a `Store` facade
(`open_default`, `open_in`, `open_at`, `open_in_memory`, plus `schema_version`, `journal_mode`,
`foreign_keys_enforced` and `compact`). WAL and enforced foreign keys are set at open.

Everything is synchronous: the daemon calls it from a blocking context and owns the ordering. There is no
runtime, no background thread and no lock in the crate.

**The persistence boundary is the most important thing about it**, because getting it wrong produces a
convincing lie. Persisted: Workspaces/checkouts, Session assignment and mode, Layouts, Templates, attention
policies/queue, event log, process metadata/relationships/names, Pane bindings, bounded redacted previews,
leases and per-surface tree state. Never persisted: the pty master, raw terminal bytes, grid or scrollback,
the output broadcast channel, parser state, live subscriptions or unredacted preview source.
Every repository builds those rows from a redacted durable copy. Known credential shapes are scanned in
all free-text fields and sensitive-key values are replaced; ids/FKs stay stable. Authority-bearing
Workspace/checkout paths are rejected rather than rewritten when scanning would alter them.

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

Migration 003 is conservative and side-effect free. It creates one deterministic primary-checkout record
per Workspace, assigns legacy Sessions as `read_only` with enforcement false, imports non-conflicting legacy
Pane bindings and marks the Workspace for lease reconciliation. It creates **no active write lease** and
does not launch, kill, move, chmod or relaunch a process. DDL cannot prove that a recent Session is the sole
writer; daemon reconciliation or an explicit user decision must do that later. Conflicting legacy binding
sources are surfaced for reconciliation rather than silently choosing two authorities.

Migration 005 removes historical raw hook callback bodies from both SQLite and WAL. Migration 006
re-resolves legacy checkout identity, preserves fence monotonicity inside the store and marks every ambiguous legacy
claim for explicit reconciliation rather than granting authority. Migration 007 adds the authenticated
parent and optional external-worker correlation scope to node-less Attention entries; old rows remain valid
and acquire null scope rather than being broadened by a guessed parent. Migration 008 distinguishes
interaction demands from postmortem turn-complete/task-complete/failure evidence and records whether each
demand truthfully survives its runtime owner exiting; legacy rows remain non-surviving interactions.
Migration 009 marks every pre-v9 store for a retryable credential purge. Open then classifies every current
`TEXT` column, redacts legacy free text transactionally, refuses to rewrite structural identities, rebuilds
the database with `secure_delete`/`VACUUM`, verifies WAL truncation and clears the marker only at the end.
A busy WAL, SQLite failure, structural credential or correlation-key collision leaves the marker intact and
fails the open rather than claiming a partial cleanup succeeded.

The schema additions are Session mode/checkout/enforcement fields and
`workspace_checkouts`, `checkout_write_fences`, `workspace_write_leases`, `activity_previews`,
`pane_node_bindings` and `tree_ui_state`. `checkout_write_fences` is keyed by canonical path and survives
Workspace/lease recreation, so generation is monotonic for that filesystem identity inside one canonical
data directory. Separate data directories do not yet share a checkout-scoped OS lock.
Partial uniqueness treats every non-released lease state — including stale/recovery-required — as blocking.

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
- **`node`** — stores runtime metadata: pid, command, cwd, lifecycle, relationship, naming, exit code and
  external id.
  Enough to corroborate a stored PID for conservative diagnostics and say "this was running and we can no
  longer reach/find it". It cannot reattach the PTY. **Not** stored: the pty, the scrollback, the
  terminal grid, the output channel — a pty master cannot outlive the process, and a restored scrollback
  would be a screenshot of a conversation the Agent no longer remembers.
- **`hierarchy`** — checkout assignments; acquire/heartbeat/fenced release for main-checkout leases;
  zero-to-many Pane bindings; bounded, sanitised Activity Preview; and selection/expansion scoped by stable
  `surface_id`. Lease ownership is validated across Workspace, Session and checkout, and canonical checkout
  identity is protected by a unique active-owner constraint.
- **`event`** — append-only. Nothing rewrites an event; a wrong state is corrected by a *new* event with
  `EventSource::UserCorrection`, which is what makes the log a usable account of what Turn believed and
  when. Every row keeps its `Confidence` and source, so weeks later Turn can still say "this read as
  waiting for you because a pty rule matched, not because the tool said so". `Retention` and
  `PruneOutcome` keep the log from eating the disk.
- **`attention`** — the queue is persisted, because it is the one piece of live state that must not
  evaporate on a restart: an Agent that blocked on a permission at 17:58 is still blocked at 18:02, and
  a queue rebuilt from nothing would quietly drop it until the Agent happened to say so again. A known
  Agent is keyed by node; an out-of-order or id-less worker demand is keyed by authenticated parent plus
  optional external worker id, preserving ambiguity without turning it into session-wide authority.
- **cross-repository runtime checkpoint** — an accepted `TurnEvent` persists its complete Session
  projection, the idempotent event row and the resulting Attention Queue inside one transaction, in that
  order. Client pushes and perceptible effects happen only after commit. A rollback therefore cannot leave
  a permission without its Agent, a tombstone without its Stop event or a stale demand beside a stopped
  owner. While its FIFO retry barrier is blocked, protocol reads and writes fail `unavailable` before their
  handlers instead of observing or adopting the uncommitted projection (ADR-041).
- **`settings`** — key/value with JSON values, not a column per preference, because a wide table needs a
  migration per preference and a migration is a thing that can fail on a user's machine. A value this
  build cannot parse is reported as a decode error naming the key, never silently defaulted.

**Errors are typed, not `anyhow` blobs**, because the daemon must react differently to each: a schema
from a newer build must stop it cold, a decode failure on one row must not take the whole Workspace list
down, and a missing data directory is something the user can fix.

**Location** resolution is a pure function of an explicit override and `TURN_DATA_DIR`, so the rules are
testable without a test mutating process-global state every other test in the binary also reads.

### 6.3 The UI

**Status: built for the upgraded first vertical.** `crates/turn-gui` is a native window drawn on the GPU — `eframe`/`egui` over
`wgpu`, one binary named `turn`, no webview, HTML or TypeScript. ADR-039 records the stack decision;
ADR-040 defines the accepted information architecture.

The persistent left surface is one Workspace hierarchy: Workspace → Session → Agent/Tool → child. It is
the only navigation home for those identities. There is no parallel Session tab strip, permanent overview,
permanent Attention Queue navigator or optional second Agent tree. Collapse and stable row ordering make
the same projection work at 3, 10 and 30 Sessions; search, state filters and virtualisation remain planned.
The centre contains only the
user/template-selected Layout. The right side is an optional contextual inspector, and Quick Preview is a
non-layout overlay.

The client holds four independent references: selected `HierarchyKey`, active Session, focused Pane and
pending Attention. `Enter` activates or focuses an existing Pane, `Space` opens Quick Preview and
`Cmd+Enter` explicitly opens a temporary Pane; merely moving tree selection performs none of those actions.
Expansion and selection persist through daemon-owned `TreeSurfaceState` keyed by stable `surface_id`, but
one surface's selection is never broadcast to another. Rows expose native accessibility Tree/TreeItem roles,
names that include state/confidence in words, and complete keyboard equivalents.

Zero state is a real creation flow, not a log message. **+ Workspace** is always present; with no Workspace,
`Cmd+N` opens the Workspace form and **Create and continue** chains into the Session form. Once a Workspace
exists, **+ Session** and `Cmd+N` open the explicit Workspace/Template/task form, while `Cmd+Shift+N` creates
from the visibly selected Workspace and its preferred Template. Creation requests fail immediately while
offline and are never replayed across a connection generation.

The protocol does not yet carry durable operation ids for those creates, so the client serialises the
creation lifecycle: only one Workspace or Session creation may be awaiting a response. Its pending
Workspace/Template/name/task intent cannot be replaced by a second command or cleared by an unrelated
Session response. A reconnect returns the preserved form to an explicit failed/retry state. This is a
correctness fence, not the intended final concurrency model; ADR-042 requires operation ids before it may be
relaxed.

What exists today:

- **`src/cells.rs`** — a pane's screen as `Grid`/`Cell`/`CellAttrs`/`Rgb`, and the conversion from the
  daemon's `vt100`-parsed screen. The client paints cells; it does not parse an escape stream, so there is
  no second VT emulator and no way for two screens to disagree (ADR-009, ADR-039).
- **`src/theme.rs`** — the palette, and `state_marker()`, which returns a colour **and** a glyph together so
  that no caller can signal a state by colour alone. `every_state_has_a_glyph_as_well_as_a_colour` and
  `the_attention_colour_is_reserved_for_states_that_block_the_user` make that structural rather than a
  convention.
- **`src/view.rs`** — the single Workspace hierarchy, terminal painter, non-modal permission banner,
  contextual inspector, Quick Preview, temporary Pane and explicit Attention Queue overlay. The queue is
  not a persistent navigation surface.
- **`tests/snapshots.rs`** — `egui_kittest` renders the real widget tree through `wgpu` **with no display
  attached** and diffs against committed PNGs; `UPDATE_SNAPSHOTS=1 cargo test -p turn-gui` re-records. This
  is what makes a GPU-drawn frontend reviewable at all, and it has already earned itself: the first snapshot
  caught two labels drawn on top of each other, which the logic tests could not see.

The window is a **performer of effects and a reporter of context**. It does not
decide when to interrupt — that is `AttentionManager`'s job, in the daemon, tested without a window — and it
never derives a state, hierarchy edge, rank, score or preview confidence; those arrive in protocol view
models (ADR-032). Only `Effect::focus` may move the user;
`focus_deferred` and `focus_denied` are verdicts to report.

Not built yet, stated plainly: tree search/filter/manual order, daemon-authoritative rename and relationship
correction, permanent Pane placement choices, complete context menus, IME sign-off and packaging. AccessKit
tests now require `Tree`/`TreeItem` roles for every hierarchy level and explicitly reject duplicate legacy
`ListItem` navigation; screen-reader acceptance on real macOS/Linux assistive technology remains.

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

The daemon projects a changed OSC 0/2 title onto the exact PTY-backed `ProcessNode`, persists it, and
pushes the refreshed tree/hierarchy. It observes titles even when no window is attached, so reopening the
UI starts from the title the process last set. The node title is deliberately a low-priority
`NameSource::ProcessTitle`: declared, integration, structured-task and user-renamed Agent names remain
authoritative. Pane chrome uses the bound node's title unless the Pane carries `title_is_user_set`; no
title update mutates Layout, focus or Attention. The real-PTY acceptance path is
`core::titles::tests::real_ptys_keep_dynamic_titles_isolated_and_preserve_stronger_names`.

Invalid UTF-8 in a title is replaced rather than fatal, and a single enormous line is bounded by the
terminal geometry.

### 7.5 Secrets are redacted before persistence — **implemented**

Turn launches processes with the user's environment, which on a developer machine reliably contains
`GITHUB_TOKEN`, `ANTHROPIC_API_KEY`, session cookies and cloud credentials. A store that survives
restarts also survives being copied into a bug report, synced to a backup, or read by anything else
running as the user — so none of that may be written down.

The rule is mechanical at the durable boundary: **the value of any key that looks like a credential is
replaced before the row is built, and every other free-text field is scanned for recognised credential,
JWT and private-key shapes.** The key itself is kept, because "GITHUB_TOKEN was set" explains why an Agent
could not authenticate after restore, while its value is only a liability. Matching keys is deliberately
greedy — substring, case-insensitive — because redacting `MONKEY_MODE` costs little while missing
`deploy_key` costs a repository. Workspace/Session/Layout/Template, process/Agent, settings, Attention,
Preview and event/provenance writes all pass through this projection. Typed ids/FKs are untouched;
authority-bearing filesystem paths fail rather than being silently rewritten.

`ProcessNode::env_highlights` is the domain-side half of the same decision: selected entries, never the
whole environment.

Byte-level integration tests assert the property rather than the mechanism by planting one token in every
durable free-text route and scanning real SQLite/WAL files after direct writes, the atomic runtime
checkpoint, restart and pruning. A redacted command/cwd/external id is no longer an operational resume
credential; relaunch/resume/correlation needs a fresh value.

ADR-040 closes the persistence side of the former `TurnEvent::raw` question: a Claude callback exists only
as transient adapter ingress and is reduced to typed facts without being attached to its `TurnEvent`.
`EventRepo` drops `raw` for every hook source, migration 005 deletes historical callback bodies, and a raw
callback is never a preview source. Free text cannot be made safe by environment-key matching alone.

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

### 7.10 Checkouts, leases and previews — **implemented for the first vertical**

Write exclusivity is enforced on canonical checkout identity, not a user-supplied path string. Session,
Workspace and checkout ownership must agree before acquisition. Symlink/path aliases cannot create a second
lease, and worktree creation must stay below an approved parent and report shared resources it does not
isolate. Read-only mode exposes whether the guard is enforced; an agent prompt saying “do not write” is not
a security boundary.

Lease acquisition happens before any init command or process spawn. Heartbeats and fencing generations
prevent a stale daemon/client from releasing a newer owner's lease, but time alone never proves the old
writer is dead. Separately, the daemon acquires an advisory exclusive lock on the canonical data directory
before SQLite, migrations or restore; socket aliases therefore cannot create two cooperating store owners.
The lock file is never removed, uses a stable inode and is released by the kernel on process death. Unsupported
or unsafe filesystems fail closed. Migration 003 grants no lease and changes no filesystem permissions.

Activity Preview and agent names cross an untrusted-text boundary before entering navigation chrome. They
use the same control/bidi sanitisation as titles, known-secret redaction, strict character/history limits
and provenance. Raw PTY bytes, prompts, spinners and raw hook bodies are not stored as previews. Restored
previews retain their original `updated_ms`; the current client does not yet add a separate recovered/stale
marker, so it must not present their age as fresh activity.

---

## 8. Performance budget and backpressure

### 8.1 Budget

Targets are for the design point: **30 concurrent panes across 10 sessions, one of them producing
build-volume output.** Values marked *enforced* are constants in the code today; values marked *target*
are not yet measured in a release-build profiling run.

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
