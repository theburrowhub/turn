# Turn — Decision log

One entry per decision that would be expensive to reverse. Each records the context, the alternatives
that were genuinely considered, the decision, its consequences — including the downside, stated plainly
— and its status.

Status values:

- **Accepted, implemented** — decided, compiled, and covered by tests that run and pass.
- **Accepted, implementation in progress** — decided; code exists and is being written, and none of its
  behaviour is verified here.
- **Accepted, not yet implemented** — decided; the module is a placeholder or absent.
- **Superseded by ADR-nnn** — no longer in force.

| # | Decision | Status |
| --- | --- | --- |
| [001](#adr-001) | Rust daemon plus a Tauri/`xterm.js` UI | Daemon half accepted, implemented; **UI half superseded by ADR-039** |
| [002](#adr-002) | A daemon from day one, not later | Accepted, implemented |
| [003](#adr-003) | Hooks are the primary detection mechanism; heuristics are a fallback | Accepted, implemented |
| [004](#adr-004) | Two-axis state model with a derived `DisplayState` | Accepted, implemented |
| [005](#adr-005) | `Confidence` is first-class, and a heuristic can never move focus | Accepted, implemented |
| [006](#adr-006) | SQLite for persistence | Accepted, implemented |
| [007](#adr-007) | tmux is out of the MVP | Accepted |
| [008](#adr-008) | Do not write a terminal emulator | Accepted, implemented |
| [009](#adr-009) | Parse vt100 in the daemon, not only in the UI | Accepted, implemented |
| [010](#adr-010) | Record signal deaths by platform name, not by number | Accepted, implemented |
| [011](#adr-011) | Claude Code hooks use `type: "http"`; no helper process by default | Accepted, implemented |
| [012](#adr-012) | The `UserPromptSubmit` field is `prompt`, not `user_prompt` | Accepted, implemented |
| [013](#adr-013) | Codex hooks are an inline TOML struct, not a file path | Accepted, implemented |
| [014](#adr-014) | `background_tasks` makes "turn done, work continuing" a reported fact | Accepted, implemented |
| [015](#adr-015) | Time is always a parameter, never read inside logic | Accepted, implemented |
| [016](#adr-016) | The attention manager emits `Effect`s; it never performs them | Accepted, implemented |
| [017](#adr-017) | Flat process tree with parent pointers and an evidence ladder | Accepted, implemented and extended by ADR-040 |
| [018](#adr-018) | Focus guards live in the governor, not in the policy | Accepted, implemented |
| [019](#adr-019) | Scan the process table on demand, never on a timer | Accepted, implemented |
| [020](#adr-020) | Prefixed typed-id newtypes | Accepted, implemented and extended by ADR-040 |
| [021](#adr-021) | Inject agent configuration into a Turn-owned scratch directory | Accepted, implemented |
| [022](#adr-022) | A deferred focus request carries its originating policy | Accepted, implemented |
| [023](#adr-023) | Replay from the parser's formatted contents, not the raw ring | Accepted, implemented |
| [024](#adr-024) | Risk assessment is display and ordering only; it never authorises | Accepted, implemented |
| [025](#adr-025) | The hook server never answers with a decision, and drops rather than stalls | Accepted, implemented |
| [026](#adr-026) | `turn-hook` has no dependencies and exits 0 unconditionally | Accepted, implemented |
| [027](#adr-027) | The Codex callback URL travels in an environment variable | Accepted, implemented |
| [028](#adr-028) | Heuristics run only against a closed list of conversational CLIs | Accepted, implemented |
| [029](#adr-029) | Adapter selection always answers | Accepted, implemented |
| [030](#adr-030) | Newline-delimited JSON over a unix socket, with base64 for bytes | Accepted, implemented |
| [031](#adr-031) | One flat `Request` enum, and product rules enforced by protocol shape | Accepted, implemented |
| [032](#adr-032) | View models derive; they never duplicate a rule | Accepted, implemented |
| [033](#adr-033) | Schema version in SQLite's `user_version`, append-only migrations, no downgrades | Accepted, implemented |
| [034](#adr-034) | `ON CONFLICT DO UPDATE`, never `INSERT OR REPLACE` | Accepted, implemented |
| [035](#adr-035) | Redact durable secrets, preserve structural identity | Accepted, implemented |
| [036](#adr-036) | Persist node metadata, never terminal history | Superseded by ADR-044 |
| [037](#adr-037) | Codex does not validate keys inside the hooks struct; a contract test is the only guard | Accepted, implemented |
| [038](#adr-038) | Codex's turn boundary comes from `notify`, not its `Stop` hook, because `notify` is not gated on trust | Accepted, implemented |
| [039](#adr-039) | The frontend is native Rust drawn on the GPU, not a webview | Accepted, implemented for the first vertical; supersedes the UI half of ADR-001 |
| [040](#adr-040) | One hierarchy projection, one main-checkout writer, background subagents | Implemented v0.1 history; Layout-only centre superseded by ADR-059 and primary writer target superseded by ADR-065 |
| [041](#adr-041) | Runtime events checkpoint Session, event log and Attention in one transaction | Accepted, implemented |
| [042](#adr-042) | The desktop bootstraps a detached sibling daemon and serialises creation until operations have IDs | Accepted, implemented |
| [043](#adr-043) | Agent context handoffs are reviewed, bounded daemon capabilities | Accepted, implemented |
| [044](#adr-044) | A terminal pane hosts the user's shell, and an agent runs inside it | Accepted, implemented |
| [045](#adr-045) | Turn never writes its own words into a program's screen | Accepted, implemented |
| [046](#adr-046) | Archive, close and delete are three verbs, and delete forgets only Turn's record | Implemented v0.1 history; target survivor semantics superseded by ADR-047/050/057/066 |
| [047](#adr-047) | Ending a Session takes its row out of the tree; a Workspace is a project and stays | Implemented v0.1 history; navigation rule retained, survivor/primary-writer clauses extended by ADR-066/065 |
| [048](#adr-048) | The tree points at one worker: click a managed node to show its pane, its owner to restore | Superseded by ADR-059; retained as the v0.1 implementation record |
| [049](#adr-049) | Coming back to a Session starts it; the daemon still starts nothing on its own | Accepted, implemented; overturns the restore half of the never-relaunch rule |
| [050](#adr-050) | Ending is authoritative: a process Turn cannot stop is reported, never a veto | Implemented process-cleanup rule; semantic preflight and recovery inventory extended by ADR-066 |
| [051](#adr-051) | Settings resolve in the daemon through one explicit hierarchy | Accepted, implemented |
| [052](#adr-052) | Terminal history is a private bounded journal, never proof of liveness | Accepted, implemented; persistence retained and relaunch replacement narrowly superseded by ADR-059 |
| [053](#adr-053) | The control socket admits only the owner with a per-generation capability and bounded load | Accepted, implemented |
| [054](#adr-054) | Read-only Sessions use an inherited macOS checkout write guard and fail closed elsewhere | Implemented macOS-first; promotion to writable primary is superseded by ADR-065 |
| [055](#adr-055) | Checkout write authority is a host-global inode lock inherited by every writer | Implemented legacy exclusion; steady-state primary writers are superseded by ADR-065 |
| [056](#adr-056) | A compatible app update replaces files, never the live daemon | Accepted, implemented for macOS |
| [057](#adr-057) | Local-data control is complete, inspectable and daemon-authoritative | Implemented base; unconditional cascade semantics extended by ADR-063/064/066 |
| [058](#adr-058) | Functional v0.1.0 has one evidence-backed release gate | Accepted, implemented |
| [059](#adr-059) | Agent nodes select one WorkSurface view and keep instance, context and attention identity separate | Accepted, not yet implemented |
| [060](#adr-060) | Local dictation is a reviewed exact-target input path, never an attention or approval authority | Accepted, not yet implemented |
| [061](#adr-061) | Turn is an operator control plane with reusable Flows and bounded delegated control | Accepted, not yet implemented |
| [062](#adr-062) | All agent providers normalize into one durable topology and capability contract | Accepted, not yet implemented |
| [063](#adr-063) | Operational scale features remain first-class control-plane objects | Accepted, not yet implemented |
| [064](#adr-064) | Close the remaining operator-loop capabilities as typed first-class contracts | Accepted, not yet implemented |
| [065](#adr-065) | Make source-capability coverage explicit and close the final operational gaps | Accepted, not yet implemented |
| [066](#adr-066) | Bound the remaining operator utilities without creating parallel authority | Accepted, not yet implemented |

---

<a id="adr-001"></a>
## ADR-001 — Rust daemon plus a Tauri/`xterm.js` UI

**Status:** Split. The **daemon half is accepted and implemented** — a Rust daemon owning ptys, the process
table and all state (ADR-002), which is the half of this decision that survived and the half everything
else rests on. The **UI half is superseded by ADR-039**: the Tauri shell and the TypeScript/`xterm.js`
frontend were built, rejected by the product owner on sight, and deleted. `crates/turn-ui` and `ui/` no
longer exist, and no webview, `xterm.js` instance or Tauri bundle exists anywhere in the repository.

The record below is kept unedited from here down, because it is the more useful artefact for having been
wrong. It reasons carefully about alternatives, names the WKWebView/WebKitGTK divergence as the single
biggest platform risk in the stack, and never asks whether a webview is acceptable for this product at all.
That last question was the one that mattered, and it was never put to the product owner — see ADR-039's
context. The divergence risk was retired by deleting the webview rather than by ever being measured: no run
on Linux/WebKitGTK happened, so every claim ADR-001 makes about the webview's behaviour remains untested
and now always will.

### Context

Turn must be a real desktop application on **macOS and Linux with parity**, not a Mac app with a Linux
port bolted on later. It has to embed many interactive terminals, hold long-lived pty handles, walk the
OS process table, and run a local HTTP server — while feeling like a native application rather than a
web page.

### Alternatives considered

**Swift + AppKit.** Best-in-class on macOS: native performance, real system integration, and a mature
terminal ecosystem to learn from. Rejected because it forfeits Linux entirely. Linux is where a
meaningful share of the audience runs agents, and "we'll port it" is not a plan — it is a rewrite of
the entire view layer plus a second, differently-shaped process and pty layer.

**Electron + Node.** Fastest path to a UI, and `xterm.js` is a first-class citizen. Rejected for the
backend, not the frontend: the pty and supervision layer would be Node, which means per-pty native
addons, GC pauses in the middle of a 30-pane output stream, and a runtime shipped per app. Turn's hot
path is bytes from a pty to a renderer with bounded memory and predictable latency; that is precisely
where Rust is worth its cost.

**Rust + a native GUI toolkit** (`egui`, GTK, `iced`). Rejected because none of them has a terminal
widget in the same league as `xterm.js`, and writing one is ADR-008.

**Rust daemon + Tauri shell + web frontend + `xterm.js`.** Chosen. The daemon is Rust, which suits
ptys, process tables and bounded buffers. The renderer is the best terminal widget available. Tauri
uses the system webview, so no runtime is shipped.

### Consequences

- One Rust codebase for everything that touches the OS, and `turn-core` is testable without a window.
- `xterm.js` gives correct terminal rendering — a genuinely hard problem — for free.
- **Downside:** Tauri uses the *system* webview, which means WKWebView on macOS and WebKitGTK on
  Linux. Those diverge, and rendering or input bugs will appear on one platform and not the other. This
  is the single biggest platform risk in the stack. The mitigation is already in place: CI builds and
  tests on `macos-latest` **and** `ubuntu-latest` from the first milestone, with the Linux webkit system
  dependencies installed, so the Linux build can never quietly go stale.
- **Downside:** two languages, two toolchains, two test runners, and a serialisation boundary that
  must be kept in step (mitigated by ADR-006's sibling decision to define the protocol in Rust and
  generate/derive the TypeScript view of it, rather than hand-maintaining a copy).
- **Downside:** a webview renderer will never match a native GPU terminal on raw throughput. Turn's
  budget (§8 of `ARCHITECTURE.md`) is set with that in mind, and backpressure is designed so that
  exceeding it degrades to "you lagged, here is a replay" rather than to stutter.

> **Retrospect (ADR-039).** Two of these consequences read differently now. The last downside was the
> correct instinct and the wrong conclusion: the answer to "a webview will never match a native GPU
> terminal" turned out to be a native GPU terminal, not a budget that accommodates the webview. And the
> "two languages, two toolchains, two test runners" downside was priced as an ongoing tax; it was also the
> thing that made the frontend cheap to delete, because the boundary it required was a versioned protocol
> rather than a shared object graph. The tax and the insurance were the same line item.

---

<a id="adr-002"></a>
## ADR-002 — A daemon from day one, not later

**Status:** Accepted, implemented. `crates/turnd` owns PTYs, process supervision, restore, events, requests,
views, attention and client projections behind the Unix-socket protocol. The deterministic Reviewer
vertical and daemon unit/integration suites exercise that boundary; packaging/lifetime policy remains M9.

### Context

Agents run for tens of minutes. A UI that owns the ptys kills the work every time it is closed,
crashed, reloaded during development, or updated. The alternative — introducing a daemon after the fact
— means retrofitting a process boundary through code written on the assumption of shared memory, which
in practice means rewriting the state layer.

### Alternatives considered

**UI owns everything, add a daemon in v2.** Rejected. The process boundary changes the shape of the
state layer: what is a request, what is a stream, what can be lost, who owns the authoritative tree.
Discovering that later is the expensive order.

**Daemon as an optional mode.** Rejected as two code paths for the core lifecycle, which is exactly
the code that must not have two paths.

### Decision

`turnd` owns the ptys, the Session registry, the attention manager, the hook server and all store
writes, from the first milestone. The UI is a client over `turn-proto`.

### Consequences

- The UI can be restarted, crashed or hot-reloaded during development without killing an Agent
  mid-refactor.
- Sessions can run with **no UI attached at all**, which is what makes daemon-side vt100 parsing
  (ADR-009) useful and what makes `let _ = output_tx.send(data)` correct rather than sloppy.
- All UI state is derived from a single authoritative source, so two windows cannot disagree.
- **Downside — and it must be said clearly:** this buys survival of *the UI* exiting, **not** survival
  of *the daemon* exiting. The pty master lives in the daemon's file table, and `PtyProcess::drop`
  deliberately terminates the process it owns, because closing a Session must not leave strays holding
  ptys — a finite kernel resource. Kill the daemon and the Agents die with it. Current restore uses
  `Lifecycle::Orphaned`/`Lost`; `Reconnected` is reserved for a future persistent backend rather than
  forged. Closing the gap properly is tmux's job (ADR-007), deferred.
- **Downside:** a daemon needs a lifecycle of its own — start, stop, upgrade, version skew against the
  UI, log location, and a story for "the daemon is not running". None of that is free.
- **Downside:** debugging is harder. A bug is now potentially in the daemon, in the protocol, or in the
  UI.

---

<a id="adr-003"></a>
## ADR-003 — Hooks are the primary detection mechanism; heuristics are a fallback

**Status:** Accepted, implemented. All four levels exist and are tested: `claude` and `codex`
(`Structured`), `codex` degraded to `notify` (`Wrapper`), `heuristic` (`Heuristic`) and
`registry::GenericTerminalAdapter`. What is missing is a daemon to drive them.

### Context

Turn's whole value rests on knowing when an Agent needs the user. There are three ways to find out:
ask the tool (hooks and side channels), watch the terminal (pattern matching), or watch the process
(the OS process table). They differ enormously in reliability.

### Alternatives considered

**Heuristics first, hooks as an optimisation.** Superficially attractive because it works for every
tool uniformly with no per-tool code, and it was the obvious approach before the spike. Rejected on
evidence: pattern matching on agent output is guaranteed to break. Output formats change between
releases, a TUI's redraw looks like a prompt, spinners and progress bars produce output that looks like
activity while the Agent is idle, and an Agent quoting documentation can print a permission dialog
verbatim. A detection layer that is wrong occasionally, at unpredictable times, is worse than one that
is honest about not knowing.

**Wrap each agent CLI in a shim** that parses its stdout/stderr. Rejected: it breaks interactivity
(these are TUIs on a pty, not pipelines) and it is a heuristic wearing a costume.

**Hooks only, refuse to support anything else.** Rejected: it would exclude every tool without a hook
engine, including tools users legitimately want in a Pane, and it would make Turn useless as a plain
terminal.

### Decision

Four Integration Levels, best-effort per tool, always stated in the UI:

| Level | Mechanism | Confidence ceiling |
| --- | --- | --- |
| `Structured` | The tool's own hook engine or JSON-RPC | `Explicit` |
| `Wrapper` | A side channel Turn controls (Codex `notify`) | `Explicit` |
| `Heuristic` | Terminal output patterns and the process table | `InferredHigh` |
| `GenericTerminal` | Nothing; process facts only | `Explicit` about the *process* |

Every signal, whatever its origin, is normalised into a `TurnEvent` before anything downstream sees it,
so consumers never learn which tool produced an event — only how much to trust it.

### Consequences

- Claude Code gets exact turn boundaries, permission requests before they block, and **confirmed**
  subagent hierarchy from `SubagentStart`/`SubagentStop`. No inference at all.
- Adding a tool is one `AgentAdapter` implementation, and only that.
- Every tool works at some level, so Turn is never useless.
- **Downside:** hook payloads are an evolving contract Turn does not own. A rename breaks detection
  silently. Mitigated by pinning real recorded payloads in
  `crates/turn-agents/tests/fixtures/claude-code-2.1.221.json` and asserting the fields the adapter
  reads (`tests/contract_claude.rs`). That test failing after an upgrade is the system working — but it
  only fires when someone runs it, so an upgrade between CI runs can still break a user.
- **Downside:** unrecognised hook events are dropped rather than guessed at, so a genuinely useful new
  event does nothing until an adapter is updated.
- **Downside:** per-tool code means uneven quality, and users will notice that Codex behaves
  differently from Claude Code. Turn's answer is to say so in the UI rather than hide it.

---

<a id="adr-004"></a>
## ADR-004 — Two-axis state model with a derived `DisplayState`

**Status:** Accepted, implemented. `crates/turn-core/src/state.rs`; reproduce the current crate suite.

### Context

"Is this thing busy?" has two answers that change independently: whether the OS process is running,
and whether the Agent owes the user a reply. Claude Code finishes a turn while a `cargo test` it
launched runs for another two minutes. A shell stays alive for a week owing nobody anything. An Agent
crashes while its last reported state was "waiting for you".

### Alternatives considered

**One flat status enum** (`running`, `waiting`, `done`, `failed`). Rejected: it forces a lie at exactly
the moments that matter. Whichever value is chosen for "turn finished but a child is still running" is
wrong for half the UI.

**A struct of independent booleans.** Rejected: it permits nonsense combinations and pushes the
interpretation into every call site.

**Two axes plus a *stored* flat state** kept in sync. Rejected: two sources of truth drift, and the
drift shows up as a sidebar claiming `completed_turn` for a process that crashed.

### Decision

`Lifecycle` (process) and `Option<Turn>` (conversational turn, `None` for non-agents) are the stored
truth. `DisplayState` is **derived** by a pure function and never assigned:

```rust
DisplayState::derive(&lifecycle, turn.as_ref())
```

Order inside `derive` is itself a decision: `lifecycle.is_failure()` is checked first, so a dead
process outranks any stale turn state and a crashed Agent leaves the queue instead of claiming to
await a human forever.

### Consequences

- The headline product bug — "done" while work continues — is structurally impossible.
- Non-agents get no turn axis, so a shell can never be `WaitingForUser`.
- `DisplayState` carries its own presentation logic (`label()`, `demands_user()`, `severity()`), which
  the sidebar, the queue and the aggregate all reuse. One definition of "which of these is worse".
- **Downside:** `derive` is a precedence table, and precedence tables are where subtle bugs live.
  `CompletedTurn` (severity 35) outranking `Running` (20) means a Session whose Agent finished while a
  child runs displays the turn state; that is intentional, and it took a test with a comment to make it
  legible (`model::session::tests::a_session_whose_agent_finished_but_child_still_runs_reads_as_running`).
- **Downside:** two axes are more to explain, in the UI and to contributors.

---

<a id="adr-005"></a>
## ADR-005 — `Confidence` is first-class, and a heuristic can never move focus

**Status:** Accepted, implemented. Enforced at two independent points, with tests at each.

### Context

Turn mixes reliable signals (hook payloads, exit statuses) with unreliable ones (output pattern
matching). Rendering them identically means repeating a guess to the user in the product's own
authoritative voice.

### Alternatives considered

**Only ingest reliable signals.** Rejected as ADR-003 explains: it abandons every tool without a hook
engine.

**Ingest everything and treat it uniformly.** Rejected. The failure mode is the worst one available:
Turn confidently yanks the user out of their editor because a spinner looked like a prompt. Users do
not forgive that twice.

**A per-adapter "trusted" boolean.** Rejected as too coarse. There is a real difference between "a
weak guess from output shape" and "a strong pattern we have fixtures for", and the queue should rank
them differently even though neither may move the user.

### Decision

A five-rung ladder on every event — `Unknown` < `InferredLow` < `InferredHigh` < `Integrated` <
`Explicit` — with two predicates: `is_provisional()` (render as a guess) and `may_steal_focus()`
(`Integrated` and `Explicit` only).

Enforced twice, independently:

1. **At construction.** `TurnEvent::new` computes `confidence.min(source.max_confidence())`.
   `EventSource::PtyHeuristic` caps at `InferredHigh`, so an adapter that asks for `Explicit` gets
   `InferredHigh` and there is no way around it short of lying about the source.
2. **At policy resolution.** `AttentionPolicy::resolve` degrades any focus action to `Badge` when
   `!confidence.may_steal_focus()`, whatever the Session's policy asked for.

The reasoning is asymmetric cost. A missed notification costs a delay the user recovers from by
glancing at the sidebar. A false focus change costs them the thought they were holding, and teaches
them to distrust the product. So heuristics may badge, highlight, notify and enqueue — every channel
the user consults on their own schedule — and are structurally barred from the one channel that
consults them on Turn's schedule.

### Consequences

- The heuristic layer becomes safe to ship. It can be wrong without being harmful.
- Provisional demands rank below confirmed ones of the same kind, and `upsert` upgrades them in place
  when a hook later confirms the guess — so a heuristic that turns out to be right costs nothing.
- `EventSource::UserCorrection` sits at `Explicit`, which gives the "this state is wrong" flow a home
  without a special case.
- **Downside:** a heuristic that is genuinely certain still cannot move the user. Accepted
  deliberately; the ceiling is the guarantee.
- **Downside:** two enforcement points mean two things to keep in step. They are tested separately
  (`event::tests::a_heuristic_cannot_promote_itself_to_explicit`,
  `policy::tests::a_guessed_permission_badges_instead_of_stealing_focus`,
  `manager::tests::a_guessed_permission_never_produces_a_focus_effect`) precisely because belt and
  braces is the point.

---

<a id="adr-006"></a>
## ADR-006 — SQLite for persistence

**Status:** Accepted, implemented. `crates/turn-store`, behind a `Store` facade with WAL and
enforced foreign keys. See also ADR-033, ADR-034, ADR-035, ADR-036.

### Context

Turn needs to persist Workspaces, Sessions, Layouts, Templates, process nodes, event history and
attention entries. The access patterns are: read everything at start-up, write small updates
constantly, and append events at whatever rate agents produce them. Event history wants range queries
by time and by Session.

### Alternatives considered

**JSON files on disk.** Simple and diffable, and adequate for configuration. Rejected for event
history: no range queries, and rewriting a whole file per event is untenable. A hybrid — JSON for
config, something else for events — means two persistence layers and two consistency stories.

**An embedded key-value store** (`sled`, `redb`). Rejected: event history wants ordered queries and
filters, which means hand-rolling indexes over a KV store — reimplementing the part of SQLite that is
hardest to get right.

**Postgres or any server database.** Rejected outright for a local desktop app.

**SQLite via `rusqlite` with `bundled`.** Chosen. Range queries, transactions, a schema, and no
runtime dependency on a system library because the amalgamation is compiled in.

### Consequences

- One file per installation, trivially backed up, inspectable with any `sqlite3` client, which is
  worth a great deal when debugging a user's state.
- Transactions give a real answer to "the daemon was killed mid-write".
- `bundled` means no "install libsqlite3" step and no version skew across macOS and Linux.
- Millisecond `i64` timestamps throughout the domain (already the case) make ordering trivial in SQL.
- **Downside:** schema migrations become a permanent obligation from the first release. There is no
  such thing as a schema-free change once users have data.
- **Downside:** `bundled` compiles C. Build times go up and the C toolchain becomes a hard
  requirement in CI (already installed on both runners).
- **Downside:** `rusqlite` is synchronous, so every store call from the async daemon must be moved off
  the reactor — a `spawn_blocking` boundary or a dedicated writer thread. Forgetting this stalls the
  event loop, and it will not be obvious in testing.

---

<a id="adr-007"></a>
## ADR-007 — tmux is out of the MVP

**Status:** Accepted. Flags and node kinds exist in the model; no code reads them.

### Context

tmux is the established answer to "my terminal work should outlive my terminal". Routing Sessions
through it would make work survive not just the UI but the daemon and a reboot of the app. Users who
already live in tmux would expect it.

### Alternatives considered

**tmux as the substrate for every Session.** Rejected. It inverts the architecture: Turn would drive
tmux rather than own ptys, which costs direct control of resize, exit status, hierarchy and — most
importantly — the byte stream. Every feature would be mediated through `tmux` command output, and the
Confidence ladder would sag, because "what tmux says about a pane" is a weaker signal than "the pty we
hold".

**tmux as an opt-in mode alongside owned ptys.** Rejected *for the MVP*, not forever. It means two
substrate implementations behind every operation — spawn, write, resize, kill, snapshot, re-attach —
and a matrix of restore behaviours to test. That is a large amount of the complexity budget for a
benefit the daemon already partially delivers.

### Decision

Own the ptys directly (ADR-002). Ship no tmux integration in the MVP, and do not foreclose it: the
`Workspace::tmux_enabled` and `Session::tmux` flags and the `TmuxSession`, `TmuxPane` and
`TmuxTerminal` variants are already in the model so an existing Session's shape does not have to
change to gain it later.

### Consequences

- One substrate. Resize, exit codes, replay and hierarchy all work against a pty Turn holds.
- **Downside, and it is a real one:** work does not survive the daemon exiting. A crash, an upgrade or
  an accidental `pkill` takes every Agent with it. `Lifecycle::Lost` is how Turn tells the truth about
  it, which is honest but not a substitute. Users who genuinely need reboot-survivability will notice
  the gap, and "the daemon already provides persistence" is only true against UI restarts.
- **Downside:** tmux users get no path to bring their existing sessions in.

---

<a id="adr-008"></a>
## ADR-008 — Do not write a terminal emulator

**Status:** Accepted, implemented, and still in force — with one substitution. `vt100` for parsing and
`portable-pty` for the pty layer are unchanged. Rendering is no longer `xterm.js`: since ADR-039 the client
paints the daemon's already-parsed cells itself. That does not make Turn the author of a terminal emulator,
which is what this ADR is about; the grammar, the modes, the widths and the compatibility quirks are still
`vt100`'s problem. What Turn now owns is a painter, and the distinction is the whole point of ADR-039's
"cells, not bytes" section.

### Context

Turn embeds many terminals. A terminal emulator is a deceptively enormous artefact: the full CSI/OSC/DCS
grammar, character widths, combining marks, bidirectional text, mouse protocols, bracketed paste,
alternate screen, scroll regions, sixel, and decades of accumulated compatibility quirks that programs
genuinely depend on.

### Alternatives considered

**Write one, for control and performance.** Rejected. It is a multi-year project on its own, it is not
where Turn's value is, and getting it 95% right is worse than useless — the missing 5% is the TUI the
user needs.

**Use one emulator for everything.** Not actually available: the rendering-side emulator (`xterm.js`,
in a webview) and the daemon-side model (`vt100`, in Rust, with no UI attached) have different jobs.
See ADR-009.

### Decision

`portable-pty` for pty creation and process spawning. `vt100` for the daemon-side screen model.
`xterm.js` for rendering. Turn writes the parts that are Turn's: the ownership model, the buffering and
backpressure, the state machine, the attention logic.

> **Retrospect (ADR-039).** "Use one emulator for everything" was rejected here as *not actually
> available*, and it is now what Turn does: a native client can read `vt100`'s parsed screen directly, so
> there is exactly one emulator and it lives in the daemon. The premise that made the alternative
> unavailable was the webview, not anything about terminals.

### Consequences

- Correct rendering of real programs — `vim`, `lazygit`, `btop` — for free.
- The wire between them is bytes, which is the only interface guaranteed to be lossless.
- **Downside:** Turn inherits three dependencies' bugs and release cadences, on the hot path. A `vt100`
  bug is a wrong thumbnail; an `xterm.js` bug is a visibly wrong terminal. Since ADR-039 there is no
  `xterm.js`: a `vt100` bug is now *both*, which concentrates the risk in one dependency and makes its
  correctness matter more than it did.
- **Downside:** anything the libraries do not support, Turn does not support. Sixel graphics are the
  obvious example.
- **Downside:** `portable-pty`'s API shapes Turn's own — most visibly in ADR-010.

---

<a id="adr-009"></a>
## ADR-009 — Parse vt100 in the daemon, not only in the UI

**Status:** Accepted, implemented, and load-bearing in a way this ADR did not anticipate — see the
retrospect under "Alternatives" below. `TerminalBuffer` keeps a byte ring **and** a `vt100::Parser`.

### Context

The renderer (`xterm.js`) already parses escape sequences, so a second parser in the daemon looks
redundant. But `xterm.js` lives in the UI, and by ADR-002 the UI is frequently absent.

### Alternatives considered

**Raw bytes only in the daemon; let the UI parse.** Rejected because it makes three things impossible
without an attached UI: thumbnails of Sessions nobody has open, output heuristics (the `Heuristic`
integration level would have nowhere to run), and knowing whether a Pane is in the alternate screen —
which is exactly the signal that tells heuristics to stand down inside a TUI redraw.

**Parse only in the daemon and ship a rendered grid to the UI.** Rejected: it means writing a renderer
(ADR-008) and it throws away the fidelity `xterm.js` provides.

> **Retrospect (ADR-039).** This rejected alternative is now the decision. Once the client is Rust, "ship a
> rendered grid" costs a painter rather than a renderer, and there is no `xterm.js` fidelity left to throw
> away. The parser this ADR added for thumbnails and heuristics turned out to be the thing that made a
> native client cheap, which is why the second parser it was defending against no longer exists at all: one
> parse, in the daemon, painted by the client. The byte ring stays — replay to a new client is still bytes,
> and ADR-023 still governs where a replay is taken from.

### Decision

Both, with distinct roles. The **byte ring** is for exact replay to a renderer, because bytes carry the
colours, cursor position and alternate-screen state that text cannot. The **parsed screen** is for
questions the daemon must answer alone: `snapshot()`, `in_alternate_screen()`, `tail(n)`, and the
sanitised process-set title.

### Consequences

- Thumbnails and heuristics work with zero UI attached, which is what makes the daemon genuinely useful
  rather than merely a process babysitter.
- `replay()` can come from the parser rather than the ring, which is strictly better (ADR-023).
- **Downside:** every byte is processed twice, once into the ring and once through the parser. It is on
  the hot path.
- **Downside:** the parsed screen is a second bounded resource per Pane (5,000 scrollback rows), and
  its resident cost is currently **unmeasured**. Thirty Panes at full scrollback is 400,000 cells each.
  Tracked as a risk in `ROADMAP.md`; the lever already exists in
  `TerminalBuffer::with_capacity`, which takes both bounds so they can be tuned per `PaneKind`.
- **Downside:** two representations can disagree if a code path updates one and not the other. This is
  why `PtyProcess::resize` deliberately does both halves, and why the reader thread writes the buffer
  *before* broadcasting.

---

<a id="adr-010"></a>
## ADR-010 — Record signal deaths by platform name, not by number

**Status:** Accepted, implemented. `Lifecycle::Signaled { signal: String }`,
`ExitInfo { code: i32, signal: Option<String> }`.

### Context

Turn must distinguish "exited with status 1" from "was killed". This distinction drives
`Lifecycle::is_failure()`, which drives `DisplayState::Failed`, which drives the sidebar and the
attention queue.

### Alternatives considered

**`Signaled { signal: i32 }`.** The conventional shape, and what a `WIFSIGNALED`/`WTERMSIG` pair would
give. Rejected because `portable-pty` does not give it: `ExitStatus::signal()` returns the platform's
own **name** for the signal ("Killed", "Terminated"), and the accompanying exit code is a meaningless
1. Converting a name back into a number would require a hand-maintained per-platform table, and any
name not in that table would degrade to `Some(0)` or `None` — losing information Turn already has, in
order to look more conventional.

**`Exited { code: i32 }` only, treating a kill as exit code 1.** Rejected: it makes a kill
indistinguishable from a legitimate failure, and the code `1` is fabricated.

### Decision

Store the platform's own string. The presence of a signal — not the exit code — is what distinguishes
a kill:

```rust
pub fn signalled(&self) -> bool { self.signal.is_some() }
```

### Consequences

- No information is invented and none is lost. The UI can show exactly what the OS said.
- `killing_a_process_is_reported_as_a_signal_death` asserts the real behaviour of a real kill on a real
  pty, not a mocked exit status.
- **Downside:** signal names are not portable, so any logic that branches on a *specific* signal
  (distinguishing SIGINT from SIGKILL, say, to decide whether a death was the user's own Ctrl-C) needs
  per-platform string matching. Nothing needs that today; if it ever does, the field is a `String` and
  the matching will be explicit and visible rather than hidden in a conversion table.
- **Downside:** `Lifecycle` is not `Copy` and its `Signaled` variant allocates. Immaterial at this
  frequency.

---

<a id="adr-011"></a>
## ADR-011 — Claude Code hooks use `type: "http"`; no helper process by default

**Status:** Accepted, implemented. Established empirically by a live spike against Claude Code
2.1.221, not from documentation.

### Context

Turn needs Claude Code to report events. The obvious mechanism is a `command`-type hook that shells out
to a small helper which POSTs to Turn. That means spawning a process per hook event, and a busy Agent
fires dozens of tool-adjacent hooks per turn.

### What the spike established

- `claude --settings <file-or-json>` injects an **additional** settings layer without touching the
  user's own `~/.claude/settings.json` or `.claude/settings.json`. This is the mechanism Turn uses to
  install hooks. Verified.
- Hooks of `"type": "http"` with a `"url"` **do fire** and POST the payload as a JSON body. Verified
  live against a local receiver. **No helper process is needed.**
- Available hook events include `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
  `PermissionDenied`, `PostToolUse`, `Notification` (with `notification_type` of `permission_prompt`,
  `idle_prompt`, `auth_success` or `agent_needs_input`), `SubagentStart`, `SubagentStop`, `Stop`,
  `StopFailure`, `SessionEnd`.
- `SubagentStart`/`SubagentStop` give **confirmed** subagent hierarchy. No inference is required for
  the case Turn cares most about.

### Decision

`HookTransport::Http` is the default. `HookTransport::Helper` remains as a fallback for builds whose
hook engine lacks HTTP handlers, shelling out to a `turn-hook` binary. Subscribe to ten events, not all
of them: each subscription costs the Agent a callback, and Turn only wants events that change a state it
renders. Timeout is 3 seconds — short, so that if the daemon is gone the Agent carries on rather than
stalling on every event.

### Consequences

- Zero process spawns per event. Latency is a local POST, well under a millisecond.
- The user's configuration is provably untouched, which is a trust property worth having.
- Subagent hierarchy is `Relation::Confirmed`, so the most valuable part of the tree needs no guessing.
- **Downside:** the hook server must be running and reachable, and must respond immediately and
  unconditionally 2xx. A non-2xx or a timeout must never block an Agent.
- **Downside:** an HTTP endpoint on localhost is an attack surface for any local process. Mitigated by
  binding `127.0.0.1` only and requiring a per-node token in the path (ADR-025).
- **Downside:** `HookTransport::Helper` and the whole Codex path depend on the `turn-hook` binary, which
  is now a real crate (ADR-026) but is a second thing to install, locate and keep in version step with
  the daemon.

---

<a id="adr-012"></a>
## ADR-012 — The `UserPromptSubmit` field is `prompt`, not `user_prompt`

**Status:** Accepted, implemented.

### Context

The published documentation names the field `user_prompt`. The live payload, captured from Claude Code
2.1.221 and committed at `crates/turn-agents/tests/fixtures/claude-code-2.1.221.json`, names it
`prompt`. The full observed `UserPromptSubmit` payload is `cwd`, `hook_event_name`, `permission_mode`,
`prompt`, `prompt_id`, `session_id`, `transcript_path`.

An adapter written from the documentation would silently produce `prompt_excerpt: None` — no error, no
warning, just a missing excerpt in every notification.

### Decision

Read `prompt` first and fall back to `user_prompt`. Trust the captured payload over the documentation,
and record the discrepancy where a future reader will find it. Pin it with a contract test that asserts
the recorded payload still has `prompt`.

### Consequences

- Works with the shipped binary today, and would keep working if a future release adopted the
  documented name.
- The general lesson is now policy: **the fixture is the contract, not the documentation.** Every field
  the adapter reads is asserted against a recorded payload.
- **Downside:** a fallback chain is a small amount of permanent cruft carried for a discrepancy that
  may be fixed upstream tomorrow.
- **Downside:** the fixture was recorded from one version on one machine. A field that varies by
  platform or configuration would not be caught.

---

<a id="adr-013"></a>
## ADR-013 — Codex hooks are an inline TOML struct, not a file path

**Status:** Accepted, implemented. Schema established empirically against codex-cli 0.146.0 using
`--strict-config`; `crates/turn-agents/src/codex.rs` implements it with unit and captured-contract tests.

### Context

The Claude Code adapter's shape — write a configuration file, pass its path — was the obvious template
for Codex. It does not work.

### What the spike established

- Passing a path fails outright: `hooks="/path/to/file.json"` produces
  `invalid type: string, expected struct HooksToml`.
- This exact inline form is accepted **and fires**, confirmed by a handler that recorded its own
  invocation:

  ```
  codex -c 'hooks={SessionStart=[{matcher="*",hooks=[{type="command",command="'\''/abs/turn-hook'\''"}]}]}'
  ```

- Event keys are **PascalCase**: `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`,
  `PostCompact`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`,
  `Stop`.
- The per-matcher handler list key is **`hooks`** — the same word Claude Code uses, not `handlers`.
- Payloads arrive on **stdin** as a single JSON object, keyed by `hook_event_name` exactly as Claude
  Code's are. A handler `args` array parses and is then silently ignored: argv reaches the handler
  empty, which is why Turn passes the callback URL in `TURN_HOOK_URL` instead.
- `command` is executed **through a shell**: `$HOME` and globs expand, and an unquoted path containing
  a space fails outright. Turn POSIX-quotes the path before embedding it.
- Of the three handler types, only `command` runs; `prompt` and `agent` warn *"not supported yet"*, as
  does `async`.
- A separate `notify` mechanism exists: `-c notify='["/path/prog"]'`. Codex appends one JSON argument
  whose keys are hyphenated: `type` (e.g. `"agent-turn-complete"`), `thread-id`, `turn-id`, `cwd`,
  `input-messages`, `last-assistant-message`, `client`.
- Within one session, the hooks' `session_id` and notify's `thread-id` are byte-identical, so Turn has
  one external identifier whichever mechanism reports it.
- `--dangerously-bypass-hook-trust` exists because Codex has a **persisted hook-trust model**, and a
  freshly configured hook does not run until the user grants trust. See ADR-038.

> **Correction, same day.** An earlier revision of this section stated the opposite for two of these
> facts: that the handler list key was `handlers` and that event keys were snake_case. Both were read out
> of the binary's strings rather than observed, and both were wrong — the snake_case strings belong to the
> hook-*trust* state keys, not to the config. They were corrected only after a handler was made to record
> its own invocation. The lesson is recorded as ADR-037: with this particular tool, reading strings out of
> a binary is not evidence, because it accepts the wrong spelling in silence.
- Also present, and out of MVP scope: `codex app-server --listen unix://PATH` (JSON-RPC with rich
  notifications including `turn/started`, `turn/completed`, `thread/status/changed`, `ProcessExited`,
  approval requests and token usage) with `codex --remote unix://PATH` to attach a TUI to it.

### Decision

The Codex adapter will emit inline `-c hooks={...}` TOML, not a file path. Because injected hooks may
require interactive trust, the adapter **must degrade gracefully to `notify`** and report the lower
`IntegrationLevel` rather than failing to launch. Design nothing that forecloses `app-server`.

### Consequences

- `AgentAdapter::prepare` returning a `LaunchPlan` with `args`, `env`, an achieved `level` and a
  human-readable `note` already accommodates both paths without a redesign; the `note` is where the
  user learns which one they got.
- The trust model is the real risk: if injected hooks need interactive approval, the first launch of
  every Codex Session could prompt. Degrading to `notify` is the mitigation, and `notify` gives turn
  completion — the single most valuable signal — even if it gives nothing else.
- **Downside:** constructing TOML inside a shell-safe command-line argument is fiddly and easy to get
  wrong. Note that `--strict-config` makes a *type* mistake a hard failure at launch, but **not** a key
  mistake: keys inside the hooks struct are not validated at all, which is the sharper trap and has its own
  entry (ADR-037).
- **Downside:** two integration paths for one tool means two things to test and two states to explain — and
  a full launch needs both of them at once, for a structural reason recorded in ADR-038.
- **Downside:** `notify` fires per completed turn with no permission or subagent events, so a
  Codex Session degraded to `Wrapper` loses the permission queue entirely — the feature users would
  most miss.

---

<a id="adr-014"></a>
## ADR-014 — `background_tasks` makes "turn done, work continuing" a reported fact

**Status:** Accepted, implemented.

### Context

The product's central modelling case — an Agent finishes its turn while work it started keeps running —
was expected to require inference: correlate the Agent's `Stop` against the process table, work out
which children are still alive, and decide whether to call the Session finished. That inference is
exactly the kind of guess ADR-005 caps at `InferredHigh`, which would have meant the case could never
drive a confident notification.

### What the spike established

The `Stop` payload contains `background_tasks` — **an array**. The full observed payload is
`background_tasks`, `cwd`, `effort`, `hook_event_name`, `last_assistant_message`, `permission_mode`,
`prompt_id`, `session_crons`, `session_id`, `stop_hook_active`, `transcript_path`.

Claude Code is telling Turn, explicitly, what it left running.

### Decision

`EventKind::AgentTurnCompleted` carries `background_tasks: usize`, populated from
`background_tasks.len()` at `Confidence::Explicit`. No inference. The notification text is written
against it, because "turn complete" reads as *finished*:

```rust
if *background_tasks > 0 {
    format!("Turn complete · {background_tasks} still running")
} else {
    "Turn complete".to_string()
}
```

### Consequences

- The hardest case in the product became the easiest, and at the highest confidence rung.
- The correlation-with-the-process-table machinery that would otherwise have been needed does not have
  to be written, tested or trusted.
- **Downside:** it only works for Claude Code. Codex `notify` has no equivalent field, so a Codex
  Session falls back to the process table — which means the case is `InferredHigh` there, and cannot
  drive focus. The asymmetry is visible to users.
- **Downside:** the count is a number, not a description. Turn can say "2 still running" but not what
  they are, unless it correlates with the process table anyway.
- **Downside:** the field is undocumented and could disappear. Asserted by
  `tests/contract_claude.rs::the_recorded_hook_payloads_still_carry_the_fields_the_adapter_reads`, with a
  comment saying exactly why it matters.

---

<a id="adr-015"></a>
## ADR-015 — Time is always a parameter, never read inside logic

**Status:** Accepted, implemented throughout `turn-core`.

### Context

The attention logic is entirely about time: typing grace periods, cooldowns, sliding rate-limit
windows, snooze deadlines, age bonuses, deferral TTLs. Logic that reads `SystemTime::now()` internally
can only be tested by sleeping, which makes the suite slow, flaky and unable to express "and an hour
later".

### Decision

Every function that needs the time takes `now_ms: i64`. `turn_core::now_ms()` exists for the edges —
the daemon and the UI — and is called nowhere inside the domain.

### Consequences

- The whole attention model is deterministic. `aging_prevents_starvation_without_reordering_priority_
  classes` constructs an hour-old demand as an integer, and the suite runs in milliseconds.
- Clock skew is testable and handled: `idle_for_ms` clamps at zero, and the governor uses
  `saturating_sub` throughout.
- **Downside:** `now_ms` threads through many signatures, and `AttentionManager::apply_focus_decision`
  already trips clippy's 7-argument limit partly because of it (see `ROADMAP.md` §Technical debt).
- **Downside:** a caller that passes a stale or wrong timestamp gets silently wrong behaviour rather
  than an error. Nothing validates monotonicity.

---

<a id="adr-016"></a>
## ADR-016 — The attention manager emits `Effect`s; it never performs them

**Status:** Accepted, implemented.

### Context

Deciding to interrupt someone and actually interrupting them are different concerns. The decision is
intricate and must be exhaustively tested; the performance is platform-specific — an OS notification, a
sound, a window focus change, a badge.

### Alternatives considered

**Callbacks or a trait the UI implements.** Rejected: testing then requires a mock implementation per
test, and the assertion becomes "the mock was called" rather than "the right thing was decided".

**Direct calls into a platform notification layer.** Rejected: it puts `turn-core` on the wrong side of
the I/O boundary and makes the decision logic untestable without a window.

### Decision

`ingest(event, policy, ctx, now_ms) -> Vec<Effect>`, where `Effect` is plain serialisable data:
`Badge`, `Highlight`, `PlaySound`, `Notify`, `Enqueued`, `Focus`, `FocusDeferred`, `FocusDenied`,
`RunCustom`, `Cleared`. The UI performs them and reports back.

### Consequences

- Every attention test is a pure function call with a literal timestamp and an asserted output vector.
  That is why there are 15 manager tests and 14 governor tests covering the interesting interactions.
- `Effect` crosses `turn-proto` unchanged, so the daemon and UI cannot disagree about what was decided.
- `FocusDeferred` and `FocusDenied` are effects too, which means "we decided not to move you, for this
  reason" is observable — in tests, in the event log, and for debugging a user's complaint that Turn
  did or did not jump.
- **Downside:** an effect can be produced for a UI that is not connected. The daemon must drop or
  coalesce those rather than queue them indefinitely. `turnd::core::clients` is where that belongs and it
  exists, but no such policy has been verified here — assume the gap is open until it is.
- **Downside:** the round trip matters. `FocusGovernor::record_grant` must be called by whoever applies
  a grant, or the guards have nothing to work with. `Effect::Focus` being emitted is not proof the user
  moved.

---

<a id="adr-017"></a>
## ADR-017 — Flat process tree with parent pointers and an evidence ladder

**Status:** Accepted; flat ownership implemented, relationship representation extended by ADR-040 and in
progress.

### Context

Nodes arrive out of order and with varying evidence. A subagent's hook can land before the parent's
spawn notification. A dev server appears only in the process table, with a `ppid` that may or may not
mean what it looks like.

### Alternatives considered

**Nested structs** (`children: Vec<ProcessNode>`). Rejected: re-parenting requires removing from one
subtree and inserting into another while holding a mutable borrow of both, and out-of-order arrival
makes re-parenting routine rather than exceptional.

**Trust `ppid` and build the tree from it.** Rejected. It is a guess dressed as a fact: shells
re-parent, wrappers intervene, pids are reused, and a `ppid` match can be pure coincidence. It also
directly violates the product rule against inventing relationships.

### Decision

`SessionTree` is `HashMap<NodeId, ProcessNode>` plus `order: Vec<NodeId>` for stable rendering. Every
node carries `parent: Option<NodeId>`. The legacy v2 representation stored a `Relation`:

- `Confirmed` — the tool reported it.
- `Inferred` — derived from the process table or the pty hierarchy. Rendered as a guess.
- `Unknown` — no parent established. Renders at the Session root.

ADR-040 retains the parent pointer and generalises edge evidence to `Relationship { kind, confidence }`.
Meaning (`spawned_by`, `owns_process`, related, unknown) is no longer conflated with certainty. Legacy
`Confirmed` tool edges migrate to `spawned_by/explicit`, `Inferred` to
`spawned_by/inferred_high`, and `Unknown` remains unknown. Event confidence that an observation occurred is
a separate axis.

`relink` enforces the evidence ladder: an explicit edge is never downgraded by an inferred one. It refuses cycles,
with a 1,000-hop bound in case the store hands it a corrupt tree. `remove` promotes children to roots
with an unknown runtime edge rather than deleting them or re-attaching them elsewhere.

### Consequences

- Out-of-order arrival is a non-event: insert, then relink when better evidence appears.
- Turn cannot silently invent a hierarchy, and the UI can distinguish a fact from a guess.
- A process nobody can attribute is still visible, at the root. An honest unattached node beats a
  confident wrong edge.
- **Downside:** rendering requires walking the map to find children (`children()`, `descendants()`),
  which is O(n) per level. Fine for tens of nodes; it would need an index at thousands.
- **Downside:** the tree can be a forest, and every consumer must handle multiple roots.
- **Downside:** relationship kind plus five confidence levels are more expressive and more work to render
  accessibly; the daemon-derived view supplies the spoken/provisional labels so the client does not invent
  them.

---

<a id="adr-018"></a>
## ADR-018 — Focus guards live in the governor, not in the policy

**Status:** Accepted, implemented.

### Context

Per-Session Attention policy is user-configurable, and moving the user's viewport is the most
disruptive thing Turn does. If the guards against interrupting a keystroke, thrashing focus, or
ping-ponging between two chatty Sessions are expressed *as policy*, then a user — or a bad default, or
a Template — can switch them off, and the product's core promise becomes optional.

### Alternatives considered

**Guards as policy fields with safe defaults.** Rejected: defaults are not guarantees. One
misconfigured Template and Turn becomes the thing it was built to replace.

**Guards enforced in the UI.** Rejected: the UI is a performer of effects (ADR-016), and enforcement
there would be untestable without a window and duplicated per platform.

### Decision

`FocusGovernor` sits between every focus request and any focus change, regardless of which policy asked.
It owns the rate limit (3 per 10 s), the minimum interval (2 s), the ping-pong guard (5 s) and the
sensitive-operation block. Policy may express *preference* — `do_not_interrupt_while_typing`,
`focus_only_if_idle`, `cooldown_seconds` — but cannot bypass the governor's own guards.

The `Grant` / `Defer` / `Deny` distinction is the product, not an implementation detail: a deferral
keeps the badge and lands the jump later; a denial never moves the user.

### Consequences

- `a_policy_cannot_opt_out_of_the_typing_guard` is a test that asserts a guarantee, not a behaviour.
- A burst of simultaneous completions moves the user exactly once, with the rest badged and queued.
- `AlreadyFocused` is a `Deny` rather than a silent success, so a no-op does not burn a slot in the
  rate limiter.
- Non-focus actions passed to `evaluate` are denied rather than silently granted, so a programming
  error fails closed.
- **Downside:** a user who genuinely wants aggressive focus cannot have it. `do_not_interrupt_while_
  typing: false` is honoured, but the rate limit and ping-pong guard are not negotiable. That is
  intentional and will occasionally be the wrong call for someone.
- **Downside:** the governor holds mutable state (`recent_grants`, `last_yielded`, `last_grant_ms`) and
  depends on `record_grant` being called by whoever applies a grant. Miss that call and the guards
  quietly stop working.

---

<a id="adr-019"></a>
## ADR-019 — Scan the process table on demand, never on a timer

**Status:** Accepted, implemented, and enforced by absence: `ProcessSupervisor` has no timer.

### Context

Discovering what the processes Turn started went on to start requires reading the OS process table.
The obvious implementation is a background poll every second.

### Alternatives considered

**Poll every second.** Rejected. Across ten Sessions with thirty processes it is continuous work for
information that is usually unchanged, it is a measurable idle CPU cost on a laptop, and it is exactly
the aggressive polling the product rules out. It would also fight the < 1% idle CPU budget.

**Platform-specific process event APIs** (kqueue `EVFILT_PROC` on macOS, `fanotify`/netlink on Linux).
Rejected for the MVP: two implementations, elevated permissions in some configurations, and it optimises
a path that is already cheap when triggered correctly.

### Decision

`refresh()` is called by the caller, when there is reason to believe something changed — a Pane is
opened, a Session is expanded, an Agent turn completed with `background_tasks > 0`, or the user asked.
`refresh` requests only `cmd`, `cwd` and `exe`; a full `sysinfo` refresh also collects per-process
memory, disk and CPU statistics, which is a great deal of work to throw away.

### Consequences

- An idle Turn does no process work at all.
- Scan cost is paid where it produces value, and the trigger points are explicit and reviewable.
- `descendants` indexes children by parent once rather than re-scanning per level, and bounds itself at
  depth 32 with a `seen` set so pid reuse cannot spin forever.
- **Downside:** the tree can be stale. A dev server that started and died between scans is never seen —
  which is acceptable for the inferred layer, and would not be for the confirmed one (hooks cover that).
- **Downside:** "when there is reason to believe something changed" is a judgement call spread across
  call sites in the daemon. Get it wrong and the tree looks broken to the user, with no error anywhere.

---

<a id="adr-020"></a>
## ADR-020 — Prefixed typed-id newtypes

**Status:** Accepted, implemented and extended by ADR-040. `ids.rs`, one macro, nine types.

### Context

Entity kinds are identified by opaque strings flowing through events, a protocol, SQLite and the native
UI. Passing a `PaneId` where a `SessionId` is expected is a class of bug that compiles fine and
fails at runtime, possibly much later.

### Alternatives considered

**Raw `String` or `Uuid` everywhere.** Rejected: no type safety, and a bare UUID in a log line tells
you nothing about what it identifies.

**Integer primary keys from the database.** Rejected: ids must exist before anything is persisted (a
Pane exists the moment it is split), and integers are unreadable in logs and event payloads.

### Decision

One `typed_id!` macro generating a `#[serde(transparent)]` newtype over `String`, with a 12-hex-character
UUID suffix and a per-type prefix: `ws_`, `sess_`, `proc_`, `pane_`, `tpl_`, `evt_`, `attn_`,
`checkout_`, `lease_`.
`from_stored` rebuilds from persistence without validation, because the store is trusted.

### Consequences

- Mixing id types is a compile error.
- `#[serde(transparent)]` means the wire format is a plain string, so JSON and SQLite stay readable and
  the prefix makes a log line self-describing.
- **Downside:** 12 hex characters from a v4 UUID is 48 bits. Collisions are not a practical concern at
  this scale, but it is not a UUID any more and should not be treated as globally unique.
- **Downside:** `from_stored` validates nothing, so a corrupt row produces a plausible-looking id
  rather than an error.
- **Downside:** `Default for` an id mints a fresh random value, which is convenient and a trap — a
  `..Default::default()` on a struct with an id field silently creates a new identity.

---

<a id="adr-021"></a>
## ADR-021 — Inject agent configuration into a Turn-owned scratch directory

**Status:** Accepted, implemented for Claude Code.

### Context

Turn must install hooks into agent CLIs. The naive approach is to edit the user's own configuration —
`~/.claude/settings.json` or the project's `.claude/settings.json`.

### Alternatives considered

**Edit the user's configuration, restore on exit.** Rejected outright. It is destructive, it races with
the user's own editor and with any other tool doing the same, and a crash leaves their configuration
modified. There is no acceptable version of "we edited your dotfiles and hopefully put them back".

**Environment variables only.** Rejected: not expressive enough for a hook table, and not what either
tool's configuration surface accepts.

### Decision

`LaunchContext::scratch_dir` is a directory Turn owns and deletes with the Session. Adapters write
throwaway configuration there and pass it by flag: `--settings <path>` for Claude Code, inline `-c` TOML
for Codex (ADR-013). The user's files are never read for modification and never written. `--settings`
is appended **after** the user's own args so their flags keep precedence.

### Consequences

- A trust property that can be stated flatly to a user and is asserted in a test: the emitted path is
  checked to be inside the scratch directory
  (`preparing_writes_a_settings_file_and_passes_it_without_touching_user_config`).
- A crash leaves nothing to clean up in the user's home directory.
- Per-Session scratch means per-Session configuration, which is what makes per-node hook tokens
  possible.
- **Downside:** Turn depends on each tool offering a configuration-injection flag. A tool that only
  reads a fixed path cannot reach `Structured`, and there is no workaround Turn is willing to take.
- **Downside:** scratch directories must actually be cleaned up. A leak means litter in the user's data
  directory. The cleanup path lives in the daemon: `turnd::paths` defines a `scratch` directory under the
  data dir, but no cleanup has been verified here, so treat the leak as unmitigated for now.
- **Downside:** `--settings` layering semantics are Claude Code's, not Turn's. If a future release
  changes precedence, an injected hook table could shadow something the user configured.

---

<a id="adr-022"></a>
## ADR-022 — A deferred focus request carries its originating policy

**Status:** Accepted, implemented. Recorded because it was a real bug found during implementation, and
the failure mode is not obvious from reading the code.

### Context

When the governor defers a focus change, the request is parked in `AttentionManager::deferred` and
re-evaluated on `tick`. The first implementation re-evaluated with `AttentionPolicy::default()`, since
`tick` has no event and therefore no obvious policy to hand.

Two things went wrong. A Session with a non-default policy had its own guard settings ignored on
re-evaluation. Worse, `tick` passed the session cooldown into `evaluate`, so the very effect that
caused the deferral had already started the cooldown — the re-evaluation deferred again, and again, and
the jump never landed. A permission that legitimately earned focus would be silently downgraded to a
badge forever.

### Decision

Two changes, both necessary:

1. `DeferredFocus` stores a clone of the policy in force when the request was made, and `tick`
   re-evaluates with it.
2. `tick` passes `None` for `session_last_effect_ms`, because a deferred jump is the tail of one
   already-approved effect, not a new one. The governor's own guards — typing, rate limit, ping-pong —
   still apply.
3. `DeferredFocus` carries the exact node or unresolved parent/external-id subject. Re-evaluation requires
   that same subject to remain actionable; another demand in the Session is not authority to focus it.
   Snooze, dismiss, mute and lifecycle resolution cancel their matching deferred requests.
4. The UI reports a bounded latest-keystroke heartbeat during a long burst. The daemon derives typing from
   time, so a transition-only client would otherwise let the grace expire while the user still types.

A separate cooldown rule fell out of the same investigation: only a **perceptible** effect starts the Session
cooldown (`is_perceptible`). Bookkeeping effects — enqueued, deferred, denied, cleared — are invisible
and must not silence a Session.

### Consequences

- `focus_waits_for_the_user_to_stop_typing_then_happens` asserts the jump actually lands, which is the
  behaviour the bug removed.
- The distinction between a *new* interruption and the *tail* of an approved one is now explicit rather
  than emergent.
- **Downside:** each deferred request clones an `AttentionPolicy` (seven `Vec`s). Cheap at these
  volumes, but it is a clone in a hot-ish path.
- **Downside:** three interacting time-based rules — cooldown, deferral TTL, rate limit — are the
  subtlest code in the crate. It is covered by tests, but it is not code to modify casually.

---

<a id="adr-023"></a>
## ADR-023 — Replay from the parser's formatted contents, not the raw ring

**Status:** Accepted, implemented.

### Context

When a Pane re-attaches, the UI needs bytes that reconstruct the terminal exactly as it is now. The
obvious source is the raw byte ring — replay everything that arrived.

### Alternatives considered

**Replay the whole raw ring.** Rejected for two reasons. It is large (up to 2 MiB per Pane, sent on
every re-attach), and — decisively — a bounded ring that has dropped data **starts mid-escape-sequence**.
Feeding that to a renderer corrupts it: a half-consumed CSI leaves the terminal in an undefined state,
which is far worse than a missing scrollback.

**Send a structured grid instead of bytes.** Rejected: it means defining a wire format for cells,
attributes and cursor state — reinventing what escape sequences already encode losslessly.

### Decision

`TerminalBuffer::replay()` returns `parser.screen().contents_formatted()` — the parser's own
reconstruction of the current screen, which is always self-consistent and far smaller. `raw()` remains
available for callers that want exactly what arrived, and `is_truncated()` reports when the ring has
dropped data.

### Consequences

- Re-attaching is correct even after a flood that overran the ring:
  `a_reattaching_pane_can_rebuild_the_screen_from_replay` and
  `replay_reconstructs_the_screen_without_replaying_the_whole_ring` both assert the rebuilt screen
  matches.
- The same mechanism is the recovery path for a lagged subscriber (`ARCHITECTURE.md` §8.2), which is why
  a dropped broadcast chunk is late data rather than lost data.
- **Downside:** replay gives the **visible screen**, not the scrollback. A re-attaching Pane starts with
  no history above the fold. That is a visible product limitation and the honest trade for correctness.
- **Downside:** it depends on `vt100`'s `contents_formatted` being faithful. A bug there is a wrong
  screen on every re-attach, and Turn has no independent check.

---

<a id="adr-024"></a>
## ADR-024 — Risk assessment is display and ordering only; it never authorises

**Status:** Accepted, implemented. `crates/turn-agents/src/risk.rs`; reproduce the current crate suite.

### Context

When an Agent asks to run a command, Turn knows the command, the tool name and the working directory
before the user does. It is tempting to use that: auto-approve reads, auto-deny anything matching
`rm -rf`, hold a "safe" allowlist.

### Alternatives considered

**Auto-approve low-risk tools.** Rejected. It is the single most dangerous feature Turn could ship. A
classifier that is right 99% of the time approves something catastrophic on the hundredth, and it does
so with the user's full credentials in their real repository. There is no version of this that is worth
the failure mode, and Turn's whole premise is that the human is the one who decides.

**Auto-deny high-risk patterns.** Rejected as the same mistake in the other direction, plus a new one:
it teaches users that Turn silently interferes, which makes every unexplained agent failure Turn's
fault.

**No risk assessment at all.** Rejected: the user still has to decide, and "you have three permission
requests" without any indication of which one deletes a directory is a worse queue than one that
orders by blast radius.

### Decision

`risk::assess(tool_name, command) -> Risk` produces `Low` / `Medium` / `High` for exactly two purposes:
colouring the approval banner and ordering the Attention Queue. It authorises nothing. Turn never
approves, never denies, and never executes a command it inferred from Agent prose.

Two calibration choices: it **errs upward** — an unrecognised tool is `Medium`, not `Low`, because
under-warning costs a bad surprise while over-warning costs a glance — and the `HIGH_RISK` list is
deliberately short and specific, because a long list of vague patterns marks everything high risk and a
warning that always fires is a warning nobody reads. The command outweighs a reassuring tool name:
`Read` with `rm -rf /important` is `High`.

### Consequences

- The blast radius of a wrong rating is a mis-coloured banner and a mis-ordered queue. Nothing executes
  and nothing is blocked.
- The permission banner can show the command, the risk explanation and the `cwd` **verbatim**, so the
  user can see they are about to approve something in the wrong repository.
- **Downside:** substring matching on a command line is crude. `rm -rf` inside a quoted string in an
  unrelated command rates `High`, and a genuinely destructive command Turn has never seen rates
  `Medium`. Accepted, because the rating is advisory.
- **Downside:** users may read the rating as a guarantee — "Turn said `Low`, so it is safe". The
  explanation strings are written to describe what the command *does* ("Reads only", "Runs a shell
  command in this directory") rather than to reassure.

---

<a id="adr-025"></a>
## ADR-025 — The hook server never answers with a decision, and drops rather than stalls

**Status:** Accepted, implemented. `crates/turn-agents/src/server.rs`; reproduce the current crate suite.

### Context

Turn's hook receiver sits directly in the path of the user's Agent. Claude Code's hook protocol allows a
response body that **allows or denies** the tool call that fired the hook, and it waits for the response.
Two temptations follow: answer with a decision, and apply backpressure when Turn is busy.

### Alternatives considered

**Answer with allow/deny.** Rejected. This is the same decision as ADR-024, arriving through a different
door: it would make Turn an authorisation layer, with the user's credentials, based on a substring match.
Refusing it in the *protocol handler* rather than only in policy means there is no configuration flag that
could turn it on.

**Await the downstream consumer before responding.** Rejected. A hook that hangs stalls the Agent that
fired it. Turn's whole premise is that Agents keep working; being the reason an Agent stops is the worst
available failure.

**Unbounded event buffer.** Rejected: it converts a slow daemon into unbounded memory growth.

### Decision

The handler does a hash lookup, a parse, a `try_send`, and returns an empty 200. Nothing awaits anything
downstream. The event channel is bounded at 1,024 and a full channel **drops the event** and increments
`HookStats::dropped`. If the receiver is dropped entirely — no daemon draining at all — the server keeps
answering agents and discards what it normalises.

### Consequences

- An Agent's turn is never slowed by Turn's own state, and never fails because the daemon is busy or gone.
- Losing events is visible rather than silent: `HookStats { accepted, refused, unparsable, dropped,
  emitted }` is surfaced to the UI.
- The 256 KiB body limit is applied by `DefaultBodyLimit` **before** the bytes are buffered, so a hostile
  `Content-Length` costs nothing.
- **Downside:** dropped events mean lost state. A burst of 1,024+ events while the daemon is blocked
  leaves the tree wrong until the next authoritative event arrives, with only a counter to say so. That
  counter is the mitigation and it is a weak one.
- **Downside:** an unknown token is refused with no distinguishing response, which is correct for security
  and unhelpful for debugging a misconfigured launch. `refused` being non-zero is the only signal.

---

<a id="adr-026"></a>
## ADR-026 — `turn-hook` has no dependencies and exits 0 unconditionally

**Status:** Accepted, implemented. `crates/turn-hook`, with an empty `[dependencies]` section.

### Context

Codex's hook handlers and its `notify` mechanism are both **command-based**: they run a program. So Turn
needs a helper binary, and that binary runs inside the user's agent process tree — potentially on every
tool call.

### Alternatives considered

**`reqwest` or `ureq` for the HTTP request.** Rejected. `reqwest` pulls an async runtime and a TLS stack
to POST a few hundred bytes to loopback; process start-up time would be dominated by initialising things
the helper will never use. The target is always `http://127.0.0.1:<port>`, so there is nothing for TLS to
protect and no certificate story to get wrong.

**A long-lived helper daemon per session.** Rejected: a second process lifecycle to manage, for a task
that is one connection and one write.

**Report failures on stderr and with a non-zero exit.** Rejected, and this is the important one.
Unsolicited stderr from a hook lands in the middle of the user's agent output, and an Agent that treats a
non-zero hook exit as a problem would then have a problem **caused by Turn's daemon not running**.

### Decision

Zero dependencies. The HTTP request is built by hand over `TcpStream`. Two payload conventions — stdin
(Claude Code `command` hooks) and argv (Codex `notify`, which appends the JSON as a final argument). The
destination comes from `--url` or `TURN_HOOK_URL`. `http://` only. 2-second socket timeout, 256 KiB
payload cap.

And the rule that overrides everything else: **`std::process::exit(0)`, explicitly and unconditionally**,
in `main`, whatever happened. Nothing is printed unless `--debug` was passed.

### Consequences

- A missing daemon, a refused connection, an unreadable payload or a malformed URL are all invisible to the
  Agent. Turn cannot break a session it is only observing.
- Start-up is a process spawn and a `connect`, which is what makes per-tool-call invocation tolerable.
- The helper carries no credentials: the only secret it handles is the per-node token, which arrives in a
  URL from Turn's own configuration and is useless off this machine.
- **Downside:** silent failure is genuinely hard to debug. A user whose Codex Session shows no state has
  no signal at all until they run the helper by hand with `--debug`.
- **Downside:** a hand-written HTTP client is a hand-written HTTP client. It handles exactly the one
  request shape it needs and nothing else, which is correct here and would be reckless anywhere else.
- **Downside:** it is a second artefact to build, ship, locate and keep in version step with the daemon.

---

<a id="adr-027"></a>
## ADR-027 — The Codex callback URL travels in an environment variable

**Status:** Accepted, implemented for hooks — and **being extended to `notify` as this is written, which
inverts one of the trade-offs recorded below.** The entry stands as the record of what was decided and why;
read the note at the end before trusting its argv-versus-environment reasoning.

### Context

The Codex hook schema was verified with `--strict-config` (ADR-013), but one detail was **not** verified
live: whether a hook *handler* entry accepts an `args` array. The key was read out of the binary's
strings. `--strict-config` means a wrong guess is a hard failure at launch, not a warning — the Session
simply would not start.

### Alternatives considered

**Pass `--url` as a handler argument, like the `notify` case.** Rejected on the evidence available: for
`notify` the array form *is* confirmed, so `--url` is passed explicitly there. For handlers it is not, and
guessing wrong breaks every Codex launch.

**Bake the URL into a generated wrapper script per Session.** Rejected: it turns one artefact into two,
adds a filesystem dependency to the hot path, and re-introduces a shell-quoting problem.

**Verify it live first.** The right answer, and it will be done. In the meantime the adapter should not
depend on an unverified assumption.

### Decision

The callback URL reaches the helper in the **`TURN_HOOK_URL`** environment variable, which is a surface
Codex cannot mis-parse. `turn-hook` reads `--url` first and falls back to the environment, so both paths
work. `execution_mode` is left unset on purpose: guessing at the semantics of `await` risks configuring
Codex to wait on Turn, and the helper is fast enough that the default costs nothing either way.

### Consequences

- The Codex launch depends only on shapes that were exercised against the real binary.
- The unverified assumption is documented in the module's own doc comment, next to the verified ones, so
  the next person can see exactly which claims to re-test.
- **Downside:** the URL — and therefore the per-node token — is in the process environment, visible to
  anything that can read `/proc/<pid>/environ` or run `ps eww` as the same user. That is the same trust
  boundary the token already assumes (a local process running as the user), but it is a wider surface than
  an argv entry, which is itself visible in `ps`. Neither is a secret from the user's own processes.
- **Downside:** two ways to supply the URL is a small amount of permanent surface carried for an
  uncertainty that one live test would remove.

### Correction in progress — the argv comparison above was the wrong way round

The reasoning that an environment variable is "a wider surface than an argv entry" does not survive contact
with the threat Turn actually has. Both are readable by a process running as the user, but they are not
equivalent in reach: on Linux `/proc/<pid>/environ` is readable **only by the owning process's own
credentials in practice**, whereas argv via `/proc/<pid>/cmdline` is **world-readable**. So a token in argv
is visible to every process on the machine, not merely to the user's own — and since Turn's whole purpose is
to run agents that generate their own commands, that means one agent could enumerate every other Codex
session's per-node token with a single `ps`, and forge events for all of them.

`notify` was the one path still passing `--url <url>` explicitly, on the grounds that the array form was the
confirmed one. It is being reduced to the program alone, so both Codex paths take the URL from
`TURN_HOOK_URL`. That removes the "two ways to supply the URL" downside as a side effect.

Once the change lands this ADR should be superseded rather than edited, and the successor should state the
rule positively: **a per-node token never appears in any process's argv.**

---

<a id="adr-028"></a>
## ADR-028 — Heuristics run only against a closed list of conversational CLIs

**Status:** Accepted, implemented. `HEURISTIC_EXECUTABLES`; reproduce the current crate suite.

### Context

The heuristic layer infers state from terminal output. The obvious scope is "anything Turn has no adapter
for", which would mean pointing pattern matching at `make`, `vim`, `psql` and every shell.

### Alternatives considered

**Run inference on every unrecognised program.** Rejected. Inference is only worth its false positives for
programs that actually hold a conversation. A build log contains the string "do you want to" as often as
anything else does, and a `vim` buffer can contain literally any text. The result would be confident
nonsense on programs that have no conversational state to report.

**Detect conversationality dynamically** — look for prompt-shaped output and enable inference if found.
Rejected as a heuristic guarding a heuristic, with the outer one unfalsifiable.

### Decision

Three constraints, each preventing a failure worse than the detection it forgoes:

1. **A closed list** — `gemini`, `aider`, `cursor-agent`, `opencode`, `crush`, `goose`, `amp`, `qwen`,
   `copilot`. Anything unlisted gets `GenericTerminal` and no claims.
2. **Stand down completely in the alternate screen.** A TUI repainting itself produces text matching
   anything you care to look for. `OutputHeuristic::stood_down()` counts how often this fired.
3. **A quiet terminal is not, on its own, an Agent waiting for you.** The "awaiting input" rule requires a
   positive marker of an agent's input affordance, because treating silence at a prompt as a demand turns
   every idle shell in the Workspace into a notification. That is the single most common false positive
   available, so it is ruled out by construction rather than tuned away.

Plus the anti-flicker guard: `debounce_ms` 750, `idle_after_ms` 2,000, change detected via `bytes_seen`
rather than diffing text, and only the last 12 lines matched so a resolved permission box does not stay
"alive" in the scrollback.

### Consequences

- The failure mode of the weakest tier is "says nothing", not "says something wrong".
- `Inference` carries the **name of the rule that fired** (`Working { rule }`, `AwaitingPermission { rule }`,
  `AwaitingInput { rule }`), and `classify` is separate from `observe`, so the UI can answer "why do you
  think that" and the rules can be tested against captured screens without sleeping.
- **Downside:** a new agent CLI gets nothing until someone adds it to the list. There is no discovery.
- **Downside:** the marker lists are English and taken from the shapes these CLIs render today. A localised
  or restyled CLI silently stops being detected, and nothing fails — the Session just goes quiet.
- **Downside:** an agent that spends its whole turn inside a full-screen UI is invisible to inference by
  rule 2. Correct, and a real gap.

---

<a id="adr-029"></a>
## ADR-029 — Adapter selection always answers

**Status:** Accepted, implemented. `registry`; reproduce the current crate suite.

### Context

Given a command line the user typed, Turn must decide which adapter runs it. The decision can fail in
several ways: no adapter matches, the adapter matches but the binary is not installed, or the command is a
shell one-liner wrapping something else.

### Alternatives considered

**Refuse unrecognised commands.** Rejected outright: "Turn does not recognise this command" must never mean
"Turn will not run this command". Turn is a terminal workspace first.

**Have the fallback adapter claim everything** (`handles` returns `true`). Rejected: it would shadow the real
adapters depending on iteration order. Selection reaches the fallback explicitly instead, and
`GenericTerminalAdapter::handles` returns `false` for everything.

**Unpick shell one-liners** to find the interesting program inside `sh -c '…'`. Rejected: guessing which
program in a pipeline matters produces confident mistakes, and the generic terminal is the right answer for
a shell invocation. `executable_of` does skip leading `VAR=value` assignments, because `RUST_LOG=debug
claude` is unambiguously still Claude Code.

### Decision

`AdapterRegistry::select` walks adapters strongest-first and falls back to `GenericTerminalAdapter`. It never
fails. The result is **reported, not just used**: `Selection` carries `level`, `capabilities`,
`executable: Option<PathBuf>` and a plain-language `note` for the Session details panel.

### Consequences

- Every command runs, at some level, and the user is told which.
- `Selection::is_installed()` isolates the genuinely confusing case: `Structured` level with no executable
  means the user typed `claude`, Turn knows how to integrate with it, and it is not on `PATH`. That is a
  failure with nothing to do with Turn, and it is reported as such rather than as "unrecognised".
- **Downside:** the `note` strings are prose in Rust source. They are not localisable and they will drift
  from what the UI wants to say.
- **Downside:** selection happens on the command line as typed, so a user who runs `claude` through a
  wrapper script of their own gets the generic terminal with no explanation of why.

---

<a id="adr-030"></a>
## ADR-030 — Newline-delimited JSON over a unix socket, with base64 for bytes

**Status:** Accepted, implemented. `turn-proto::framing`, `turn-proto::bytes`; reproduce the current crate suite.

### Context

The daemon↔UI boundary carries three very different kinds of traffic: small request/response pairs,
ordered state deltas, and high-volume raw terminal output. Terminal output is **bytes**, not text —
escape sequences carry the colours and cursor state, and a pty may emit any byte including invalid UTF-8.

### Alternatives considered

**Length-prefixed binary framing** (MessagePack, CBOR, protobuf, or a bespoke frame). More efficient, and
it carries bytes natively. Rejected for the MVP on debuggability: the most important boundary in the system
would become opaque, requiring tooling to inspect, and a second frontend would need a codec library.

**gRPC or a WebSocket.** Rejected: both add a stack for a boundary between two processes on the same machine,
and the unix socket already gives filesystem-permission access control.

**Two channels — JSON for control, a raw byte socket for output.** The right long-term answer, and
deliberately deferred rather than rejected.

### Decision

One JSON value per line, `\n`-terminated, UTF-8, over a unix socket. `socat - UNIX-CONNECT:...turnd.sock`
is a working client; a bug report can include the exact bytes. `MAX_LINE_BYTES` 8 MiB,
`MAX_OUTPUT_CHUNK_BYTES` 256 KiB. Terminal payloads are `TerminalBytes`, serialised as standard base64,
with the encoder and a deliberately strict decoder written in-crate so `turn-proto` has no third-party
surface beyond serde.

Two decoder guarantees, because a terminal multiplexer that drops its control connection on bad input is
worse than useless: **partial reads are normal** (`LineDecoder` buffers and never assumes chunk boundaries
mean anything), and **a bad line costs one line** — invalid JSON, an unknown shape or an over-long line
yields an error for that line and the decoder carries on with the next.

### Consequences

- The boundary is inspectable with `nc` and testable without tooling, which matters most in exactly the
  phase the project is in.
- A malformed frame from a buggy client cannot take the connection down.
- **Downside, quantified rather than hand-waved:** base64 inflates every payload by 33% and costs a pass
  over the data in each direction. Irrelevant for keystrokes and a redraw; not irrelevant for a
  `cargo build` firehose, where 10 MB of output becomes 13.3 MB on the wire plus encode and decode work.
- **The escape hatch is already in the handshake.** `OutputEncoding` is negotiated in `Welcome`, so a
  length-prefixed binary side channel can be added later **without a protocol break**. That is what makes
  accepting the cost now defensible rather than merely convenient.
- **Downside:** JSON numbers and 64-bit integers are a known trap for a JavaScript client. Timestamps are
  `i64` milliseconds, which stays inside `Number.MAX_SAFE_INTEGER` for the next 200,000 years, so this is
  survivable — but any future `u64` on the wire is a bug waiting to happen.

---

<a id="adr-031"></a>
## ADR-031 — One flat `Request` enum, and product rules enforced by protocol shape

**Status:** Accepted, implemented for the flat-enum/envelope rule. The v4 omission-based permission and
single-relaunch clauses below are historical and amended by ADR-049/064: target protocol has typed
operator-response and Session-activation operations while still forbidding inferred answers/commands.

### Context

The daemon's surface is large: Workspaces, Sessions, Panes, ptys, Templates, the Attention Queue, settings.
The conventional structure is a module per subsystem with its own request and error types.

### Alternatives considered

**A request enum per subsystem.** Rejected: no single place shows the complete surface of the daemon, and
nothing can mechanically check that every request has a named response.

**Per-request error types.** Rejected: the UI's error handling is generic — show the message, log the code —
and a client written in another language should not have to model forty failure types to be correct.

### Decision

One flat `Request` enum. Longer to read, but it makes two things true that matter more: the complete daemon
surface is visible in one place, and `Request::expected_result` can name the response for **every** operation
— checked by a test against the response catalogue, which is what keeps `docs/PROTOCOL.md` honest rather than
stale.

Failures never arrive as a `Response`. They arrive as `ServerMessage::Error` carrying a `ProtoError` with a
machine-readable `ErrorCode` and a `message` that humans read and code never parses.

Pushes are **addressed but not correlated**: they carry no request id, because no request caused them. A
client processes them in arrival order and treats each as the current truth about what it names.

And three product rules are enforced by the *shape* of the protocol rather than by the daemon remembering to
check them:

- **Historical protocol-v4 shape:** there was no semantic permission-response request; the human used
  `Request::WritePty`. Target vNext instead has exact revision-fenced local or grant-bound remote typed
  response requests, but Turn still never chooses an option or infers approval from output.
- **There is no request that runs a command Turn inferred from output.** A process starts from a Template, a
  Pane definition, or an explicit relaunch — all of which the user chose.
- **Historical protocol-v4 shape:** `Request::RelaunchNode` was the only restart path. Target vNext adds
  `activate_session` and distinct create/resume/restart/recycle/Flow operations, each explicit or covered by
  persisted reviewed policy; metadata, child selection and inferred output still start nothing.

### Consequences

- The three rules become structural. A future contributor cannot accidentally add auto-approval without
  adding a request variant, which is a visible, reviewable act.
- A client can treat the request→response pairing as load-bearing rather than as documentation.
- **Downside:** one flat enum grows monotonically and every client must handle an unknown variant gracefully.
- **Downside:** `WritePty` is a very wide capability — it can type anything into any Agent. The protocol's
  refusal to model approval is a good rule, but `WritePty` is the hole it leaves, and the daemon cannot tell
  an approval keystroke from any other.

---

<a id="adr-032"></a>
## ADR-032 — View models derive; they never duplicate a rule

**Status:** Accepted, implemented. `turn-proto::view`; reproduce the current crate suite.

### Context

The UI has to render a sidebar ordered by what needs the user, a state label per Session, a tree with some
edges marked as guesses, and an Attention Queue in priority order. Every one of those is the output of a rule
that lives in `turn-core`.

### Alternatives considered

**Send raw domain objects and let the UI compute.** Rejected. The UI would need `DisplayState::derive`,
`sidebar_rank`, `AttentionEntry::score` and the `Relation` ladder — which means those rules would exist twice,
and the second copy would be written by someone reading a screenshot. They would drift, and the drift would
be invisible until a user reported a Session showing the wrong state. (When this was written the second copy
would have been TypeScript. ADR-039 replaced that client with a Rust one, which makes the trap *more*
tempting rather than less — the rules are now importable, so a client could call `derive` itself instead of
rendering what arrived. It must not: a client that computes is a client that can disagree with the daemon,
whatever language it is in.)

**Send only rendered strings.** Rejected: the UI legitimately needs structure to lay out, sort locally and
animate.

### Decision

Purpose-built view models — `SessionSummary`, `SessionDetails`, `AgentSummary`, `TreeNodeView`,
`AttentionView`, `WorkspaceSummary`, `TemplateSummary` — under two rules:

- **Derive, never duplicate.** Anything already modelled in `turn-core` is embedded as that type
  (`Lifecycle`, `Turn`, `Layout`, `AttentionEntry`). The extra fields are strictly *derived* values the UI
  would otherwise need a copy of the rules to compute.
- **Provisional stays visible.** A guessed parent link and an inferred state both carry their uncertainty
  into the view model, so the UI can render a guess as a guess instead of promoting it to a fact.

### Consequences

- Every product rule has exactly one implementation, in Rust, tested without a window.
- The UI stays a renderer, which is what makes ADR-016 (effects as data) coherent end to end.
- **Downside:** more types, and a mapping layer to maintain. A field added to the domain often needs a field
  added to a view model and to the UI.
- **Downside:** derived values are snapshots. `AttentionEntry::score` depends on `now_ms`, so a view model
  sent at 17:58 is subtly stale at 18:02 unless the daemon re-pushes. Ordering is the daemon's answer, but
  a UI that sorts locally by a stale score will disagree with it.

---

<a id="adr-033"></a>
## ADR-033 — Schema version in SQLite's `user_version`, append-only migrations, no downgrades

**Status:** Accepted, implemented. `turn-store::migrations`; reproduce the current migration suite rather
than freezing a test count here.

### Context

Turn will ship many versions against a database on a user's disk. Migrations must be atomic, ordered and
survivable, and the interesting failure is a user running an older build against a newer database — after a
rollback, or with two builds installed.

### Alternatives considered

**A `schema_version` table.** Rejected for a bootstrap problem: the table itself needs creating before it can
record anything, which leaves a window where the tables are new and the recorded version is old.

**Mutable migrations** — fix a broken migration in place. Rejected: every machine that already ran the old
version would have a schema no later migration accounts for.

**Permit downgrades and hope.** Rejected as the worst option. Writing to a newer schema either fails on
unknown columns or — far worse — succeeds and drops the fields the newer build depends on, silently
destroying data the user will only notice later.

### Decision

The version lives in SQLite's own `user_version` header field, written **inside the same transaction as the
DDL**, so a migration either lands completely or not at all. Migrations are append-only: once a version has
shipped, its statements are frozen. A database from a newer build is **refused loudly**, and the daemon must
stop cold rather than write to it.

### Consequences

- No bootstrap window, and no half-migrated database.
- A rollback is survivable in the only way that matters: Turn declines to run rather than corrupting data.
- **Downside:** append-only means a mistake in a shipped migration is permanent; it can only be corrected by
  a *further* migration, which is more code than a fix would have been.
- **Downside:** "refuse loudly" is a hard stop for a user who has just downgraded and now has no working Turn
  and no obvious remedy. The error message is doing a lot of work, and there is no export path.
- **Downside:** `user_version` is a single `i32` with no room for a feature-flag or branch scheme.

---

<a id="adr-034"></a>
## ADR-034 — `ON CONFLICT DO UPDATE`, never `INSERT OR REPLACE`

**Status:** Accepted, implemented. `turn-store::repo`, eight repositories.

### Context

Every repository needs an upsert: save a Session whether or not it exists. `INSERT OR REPLACE INTO` is the
idiomatic SQLite spelling and the shortest.

### The problem with the idiomatic spelling

`REPLACE` **deletes the old row first**, then inserts. For a row that other tables reference with
`ON DELETE CASCADE`, that delete fires the cascade. Renaming a Session would take its nodes, its events and
its pending attention with it — and the insert that follows would leave a Session that looks fine and has no
history. Silent, total data loss on the most ordinary operation in the product.

### Decision

Every write is `INSERT ... ON CONFLICT DO UPDATE`, which updates in place and fires no cascade. Repositories
borrow the connection rather than owning it, so a caller can hold several at once and every write lands in
the same database with the same pragmas. None of them start threads or take locks; the daemon calls them from
a blocking context and controls ordering itself.

A Session save is **one transaction over three tables** — the session row, its layout document and its nodes
— because a Session whose layout survived but whose nodes did not is not a Session anybody can restore.

### Consequences

- Foreign keys and `ON DELETE CASCADE` can be used for what they are for (deleting a Session really should
  take its nodes) without the ordinary update path triggering them.
- Ordering and transaction scope are the daemon's, explicitly, rather than emergent from whichever repository
  happened to be called first.
- **Downside:** `ON CONFLICT DO UPDATE` statements are verbose — every column listed twice — and adding a
  column means editing two lists in one statement, which is easy to half-do.
- **Downside:** the rule has to be known to be followed. It is recorded in `repo/mod.rs`'s own doc comment
  precisely because the next contributor will otherwise reach for `REPLACE`.

---

<a id="adr-035"></a>
## ADR-035 — Redact durable secrets, preserve structural identity

**Status:** Accepted, implemented. `turn-store::redact` plus integration tests that write real SQLite files
and search them for secrets.

### Context

Turn launches processes with the user's environment, which on a developer machine reliably contains
`GITHUB_TOKEN`, `ANTHROPIC_API_KEY`, session cookies and cloud credentials. A store that survives restarts
also survives being copied into a bug report, synced to a backup, or read by anything else running as the
user. None of that may be written down.

But the *fact* that a variable was set is genuinely useful: "GITHUB_TOKEN was set" is exactly what explains
why an Agent could not authenticate after a restore.

### Alternatives considered

**Store no environment at all.** Rejected: it discards the diagnostic value entirely, and `ProcessNode`
already models `env_highlights` precisely because selected entries are worth having.

**An allowlist of safe variables.** Rejected as too brittle in the wrong direction: a variable not on the
list is invisible, and the list would need updating for every tool a user runs. The failure mode is
"diagnostics silently missing", which is hard to notice.

**Entropy-based detection** — redact values that look random. Rejected: an API key and a git commit hash look
identical, and a short token looks like a word.

### Decision

**Every repository constructs its row from a safe durable projection.** The value of any key that looks
like a credential is replaced and the key is kept. Every other free-text field is scanned for issuer-shaped
credentials, JWTs and private-key blocks, including Workspace/Session/Layout/Template metadata,
process/Agent fields, settings, Attention, Activity Preview and typed event/provenance JSON. Matching keys
is deliberately greedy — substring, case-insensitive — because redacting a variable called `MONKEY_MODE`
costs little, while missing one called `deploy_key` costs a repository.

Redaction never changes typed ids or foreign keys. Filesystem roots and canonical checkout paths are
authority-bearing structural identities: if scanning would change one, the repository rejects the write
instead of silently fencing a different path. Raw hook bodies remain excluded entirely under ADR-040;
pattern redaction is not permission to persist arbitrary source/prompt content.

### Consequences

- The asymmetry is the design: over-redaction is free, under-redaction is a leaked credential.
- Redaction happens **before the row is built**, not as a filter on the way out, so there is no code path
  that writes the value and hopes nothing reads it.
- A redacted command, cwd or external conversation id remains useful as an explanation but cannot safely
  relaunch, resume or correlate. The user/tool must provide a fresh operational value.
- Byte-level tests plant the same recognisable token in every durable free-text field and scan SQLite plus
  WAL after direct writes, the atomic runtime checkpoint, restart and pruning.
- Migration 009 covers historical stores rather than only current writes. Its open-time maintenance
  transaction classifies every schema `TEXT` column, redacts free text, refuses to mutate identities, then
  uses `secure_delete`, checked WAL truncation and `VACUUM` before clearing its retry marker. A busy WAL,
  structural credential or deduplication collision fails closed for explicit reconciliation.
- **Downside:** the first open of a populated pre-v9 store performs an O(durable text) scan and a database
  rebuild. It can therefore be noticeably slower and needs free disk space proportional to SQLite's
  `VACUUM` requirements.
- **Downside:** greedy substring matching will redact things users wanted to see, with no way to opt out per
  variable. `MONKEY_MODE=on` showing as redacted is confusing.
- **Downside:** shape matching deliberately avoids entropy guesses. A credential with no recognised prefix
  under an innocent key can still evade it; minimisation and raw-hook exclusion remain necessary.
- **Resolved by ADR-040:** key/shape redaction cannot make arbitrary callback free text safe. Claude's
  adapter therefore emits only typed facts and provenance, never the callback body in `TurnEvent::raw`;
  `EventRepo` additionally refuses raw data from every `EventSource::Hook`, and migration 005 removes hook
  bodies written by older builds. Non-hook diagnostic notes keep the redacted persistence described here.

---

<a id="adr-036"></a>
## ADR-036 — Persist node metadata, never terminal history

**Status:** Superseded by ADR-044. The lifecycle distinction established here remains in force;
the prohibition on durable terminal history does not.

### Context

A Session that survives a restart raises an obvious question: how much of the terminal comes back? The
temptation is to persist the scrollback so a reopened Pane looks as it did.

### Alternatives considered

**Persist the byte ring or the terminal grid per Pane.** Rejected on two grounds. Mechanically, it is 2 MiB
per Pane of write traffic for state that changes constantly. Conceptually — and this is the real reason — a
**restored scrollback is a screenshot of a conversation the Agent no longer remembers.** The user would see
their previous exchange, type a follow-up, and get a reply from an Agent with no memory of any of it. That is
worse than an empty Pane, because it looks like it works.

**Persist a conversation summary or the last N terminal lines as restored history.** Rejected: the same
confusion in a smaller box. ADR-040 permits a different artefact — one short, provenance-labelled Activity
Preview for navigation — precisely because it is never presented as scrollback, transcript or conversational
memory.

### Decision

Store only the metadata a restart needs in order to be honest: pid, command, cwd, `Lifecycle`, `Relation`,
exit code, and the tool's own `external_id`. That is enough to corroborate a stored PID for conservative
diagnostics and say "this was running and we can no longer reach/find it"; it is not enough to reattach a PTY.

Not stored: the pty, the scrollback, the terminal grid, the output channel. A pty master cannot outlive the
process that holds it, and the rest is ephemeral by nature.

ADR-040's exception is deliberately narrow. A preview contains normalised semantic status or one stable,
sanitised line; it is redacted before persistence, bounded to 20 snapshots per node and 2,000 globally,
and retains its original timestamp after restore. A distinct recovered/stale marker remains planned and
must not be claimed by clients that only show the timestamp. Raw PTY bytes, the grid, prompts, spinners and
unredacted hook payloads remain outside this exception.

### Consequences

- Persistence stays small and cheap: a Session save is a few rows, not megabytes.
- `external_id` is what makes resuming meaningful — the right answer to "bring my Agent back" is to resume
  the Agent's own conversation via its `--resume`-equivalent, not to redraw a picture of the old one.
- The honest states have somewhere to come from: the current daemon restore emits `Lifecycle::Orphaned`
  when the stored runtime may remain but its PTY is unreachable, and `Lifecycle::Lost` when it cannot be
  found. `Reconnected` remains reserved until a backend can prove reattachment; it is not emitted today.
- **Downside:** a re-attached Pane starts with no history above the fold, compounding the same limitation
  replay already has (ADR-023). Users who expected tmux-like fidelity will notice.
- **Downside:** re-attaching to a live process from metadata is not implemented, and matching a
  stored pid against the process table is inherently racy — pids are reused. The stored command line is the
  only corroboration, and it is weak.

---

<a id="adr-037"></a>
## ADR-037 — Codex does not validate keys inside the hooks struct; a contract test is the only guard

**Status:** Accepted, implemented. Established empirically against codex-cli 0.146.0 with
`--strict-config`. Guarded by `crates/turn-agents/tests/contract_codex.rs`.

### Context

ADR-013 established that `-c hooks={…}` must be an inline TOML struct. That left an obvious question: what
happens if we get a spelling inside it wrong?

The reassuring assumption — the one worth writing down because it is false — was that `--strict-config`
would catch it. Codex rejects a configuration it cannot parse, and it rejected `hooks="/path/file.json"`
outright with *invalid type: string, expected struct HooksToml*. It would be natural to conclude that the
whole `hooks` value is validated.

### What the spike established

Codex validates the **type** of `hooks` and not the **keys inside it**. All of these are accepted without
complaint, and then never fire:

- `handlers=[…]` where it wants `hooks=[…]` for the per-matcher handler list.
- `session_start=[…]` or `sessionStart=[…]` where it wants `SessionStart=[…]`.

Neither is an error. Neither is a warning. They are accepted, `hooks/list` reports zero hooks, and the
agent runs as though nothing had been configured.

This project walked straight into the trap it was documenting. Two of those wrong spellings were written
into this very ADR as though they were the correct ones, derived from strings in the Codex binary rather
than from a hook that had actually run. The tests asserted them, the tests passed, and the integration
would have received nothing. The error survived until a handler was configured to record its own
invocation — which is the only form of evidence that settles this.

This is the worst possible failure shape for Turn. A launch configured with a typo *looks* configured: the
Session starts, the agent runs, and Turn reports nothing about it. The user experiences a Turn that silently
does not work, and the natural conclusion is that Codex's hooks are broken rather than that Turn misspelled
a key.

### Alternatives considered

**Rely on `--strict-config`.** Rejected on the evidence above: it does not cover this, and believing it does
is precisely the trap.

**Validate against a schema read out of the Codex binary.** Rejected. The key names were read out of the
binary's strings once; treating that as a schema would encode a guess as a check, and it would need
re-deriving on every Codex release.

**Detect it at runtime — warn if no hook callback arrives within N seconds of a Codex launch.** Attractive,
and not sufficient. A quiet agent and a misconfigured one look identical for an unbounded period: a user who
launches Codex and then goes to lunch has no callbacks either. It also cannot distinguish a typo from
ungranted hook trust (ADR-013), which has the same symptom and a completely different remedy. Worth adding
later as a diagnostic, never as the guard.

**Build the TOML with a serialiser against typed structs.** Rejected for now: it moves the spelling into a
`#[serde(rename)]` attribute rather than eliminating it, so the same typo is still possible one layer down —
and it costs a dependency and a set of types for a fixed, tiny string.

### Decision

Pin the exact spelling in a contract test, and say in the test file why it exists.
`tests/contract_codex.rs` asserts the verified spellings against payloads captured from a live run:
`the_handler_list_key_is_hooks_because_handlers_fires_nothing`,
`subscribed_event_keys_are_pascal_case_and_from_the_known_set`,
`hooks_are_configured_as_an_inline_toml_struct_and_never_as_a_path`,
`the_handler_command_is_shell_quoted_because_codex_runs_it_through_a_shell` and
`notify_names_the_program_only_and_the_url_travels_in_the_environment`. The module documentation of
`crates/turn-agents/src/codex.rs` records the finding next to the verified facts, so the next person to
edit the adapter reads it before touching a key name.

The general rule this encodes: **when a tool accepts a wrong configuration silently, our own tests are the
only feedback loop, so they must assert the literal wire spelling rather than the behaviour.** Asserting
behaviour is impossible here — there is no behaviour to observe.

### Consequences

- The single most likely mistake anyone editing these two adapters can make — reaching for a plausible
  key name instead of the observed one — fails a test instead of shipping a silently mute integration.
- The assertions are deliberately literal, matching substrings of the generated TOML. That is ugly and it is
  the point: a test that checked "the adapter produces a valid configuration" would pass on the typo.
- The tests are now anchored to `tests/fixtures/codex-cli-0.146.0.json`, captured from real runs of both
  `codex exec` and the interactive TUI under a pty, with `CODEX_HOME` pointed at a throwaway directory so
  the user's own configuration and trust state were never touched.
- **Downside:** a literal assertion still only defends the spellings someone thought to pin, and it defends
  them against *our* drift rather than upstream's. If Codex renames `hooks` in a future release, these
  tests keep passing while the integration goes quiet. Re-capturing the fixture on each Codex upgrade is
  the only real coverage, and nothing automates that yet.
- **Downside:** a genuinely misconfigured Codex session remains indistinguishable at runtime from a quiet
  one, and from one whose hooks were never trusted. This ADR reduces the chance of causing that; it does
  not diagnose it.
- **Downside:** the guard is only as good as the reader. It protects the spellings we thought to pin; a new
  key added later with no assertion has no protection at all.
- **Downside:** a genuinely misconfigured Codex Session is still indistinguishable, at runtime, from a quiet
  one. This ADR reduces the chance of causing that; it does nothing to diagnose it.

---

<a id="adr-038"></a>
## ADR-038 — Codex's turn boundary comes from `notify`, not from its `Stop` hook, because `notify` is not gated on trust

**Status:** Accepted, implemented. `CodexTransport::HooksAndNotify` configures both mechanisms;
`tests/contract_codex.rs::a_first_launch_configures_both_mechanisms_but_claims_only_what_it_can_prove` and
`::the_turn_boundary_comes_from_notify_and_stop_is_never_subscribed_to`.

### Context

Turn's central question is "is it my turn". For Claude Code the answer arrives as the `Stop` hook, and the
whole adapter is one mechanism: install hooks, receive events.

Codex has a `Stop` hook event too — captured live, carrying `last_assistant_message` and `turn_id`. So the
symmetrical design is available. The reason Turn does not use it is not the event list; it is the trust
model.

A freshly configured Codex hook is **untrusted and does not run**. Under `codex exec` it runs nothing at
all, with no warning, no error and a normal exit: the session looks entirely healthy and zero callbacks
arrive. In the interactive TUI it instead blocks at startup on a modal — *"Hooks need review / 7 hooks are
new or changed"* — offering to review, trust, or continue without trusting. Granting trust writes a
`sha256:` hash per handler into `$CODEX_HOME/config.toml`, and **changing the handler command invalidates
it**, so Turn's own helper path is part of what the user trusted.

`notify` has no trust gate. With hooks left untrusted in a fresh `CODEX_HOME`, `notify` still delivered
`agent-turn-complete`.

That asymmetry decides the design. If the turn boundary came from `Stop`, then a user who has not yet
granted hook trust would get a Turn that shows their Codex session running forever and never says "your
turn" — the one thing the product exists to say — while appearing to work. Putting the boundary on `notify`
means the headline behaviour works on first launch, and granting trust later *adds* permissions and
subagents rather than switching the basics on.

> **Correction.** An earlier revision of this ADR asserted that Codex has no turn-completion hook event at
> all. That was wrong: it has `Stop`. The conclusion — configure `notify` as well — survives, but the reason
> is the trust gate, not a missing event.

### Alternatives considered

**Subscribe to Codex's `Stop` hook and use it as the boundary.** The symmetrical design, and rejected on the
trust gate above: it silently does nothing until the user has approved Turn's hooks, and it breaks again
whenever the helper path changes and invalidates that approval. A turn boundary is the wrong thing to make
conditional on a setup step the user may never take.

**Hooks only, and infer turn completion.** Rejected. The available inference is "no tool-use hook has fired
for a while", which is a timeout dressed as a signal: it is wrong during a long single tool call and wrong
again whenever the agent thinks without calling anything. Worse, it is a heuristic, so ADR-005 caps it at
`InferredHigh` and it may never move focus — meaning the one state users most want to be taken to would be
the one state Turn is structurally forbidden from taking them to.

**`notify` only.** Simpler, one mechanism, and it does deliver turn completion. Rejected as the default
because `notify` carries nothing else: no permission requests, no subagents. A Codex Session on `notify`
alone has no permission queue, which is the feature users would miss most. It survives as
`CodexTransport::NotifyOnly`, the honest degradation when hook trust is unavailable (ADR-013).

**Wait for `codex app-server`.** The JSON-RPC interface has `turn/started` and `turn/completed` and is
strictly better than either mechanism. Rejected for the MVP: it is a second, differently-shaped integration
path, and deferring it does not foreclose it (`EventSource::SideChannel` already accommodates it).

### Decision

A full Codex launch configures **both** mechanisms, because each covers what the other cannot guarantee:
`-c hooks={…}` for permissions, subagents and session detail, and `-c notify=[…]` for the turn boundary,
which must work whether or not hook trust has been granted. `Stop` is deliberately not subscribed to, so the
boundary has exactly one source and cannot be reported twice.

The two arrive by different routes and must stay distinguishable in the event log, so a hook payload is
recorded as `EventSource::Hook` and a `notify` payload as
`EventSource::SideChannel { channel: "notify" }`. Both are `Confidence::Explicit` — `notify` is a side
channel Turn configured deliberately, not a guess — and the contract test asserts the source, not merely the
event kind.

Because a `notify` payload proves nothing about whether hooks were trusted, the adapter claims
`IntegrationLevel::Structured` only once a *hook* payload has actually arrived, and reports `Wrapper` until
then, naming what detection is missing. This is asserted by
`::structured_is_earned_by_a_hook_payload_and_never_by_a_notify_payload`.

Turn never passes `--dangerously-bypass-hook-trust`. Granting hook trust is the user's security decision,
and the flag would take it on their behalf — asserted by
`::turn_never_bypasses_codex_hook_trust_on_the_users_behalf`.

### Consequences

- Codex reaches `Structured` on the strength of two mechanisms rather than one, and the reason is recorded
  where the next reader will look before trying to simplify it. Removing `notify` as redundant is a natural
  mistake and would silently remove turn detection entirely.
- Turn completion for Codex is `Explicit`, so it may legitimately move focus — the outcome the
  hooks-plus-inference alternative would have forfeited.
- **Downside:** two mechanisms for one tool means two configuration surfaces, two payload shapes, two sets
  of key spellings, and two ways for a launch to be half-configured. `notify` uses hyphenated keys
  (`last-assistant-message`, `thread-id`) where hooks use snake_case, so the adapter tolerates both and the
  normalisation is fiddlier than Claude Code's.
- **Downside:** the `notify` payload has no `background_tasks` equivalent, so the case ADR-014 made a
  reported fact for Claude Code is unavailable here. `background_tasks` is hard-coded to 0 for Codex rather
  than inferred, which is honest and also means the asymmetry is visible to users.
- **Downside:** `notify` is a command invocation, so this path depends on the packaged `turn-hook` sibling
  being present beside `turnd` (ADR-026, ADR-042). A missing helper degrades explicitly rather than falling
  back to an arbitrary binary on `PATH`; cross-binary version validation remains packaging work. Codex has
  no HTTP handler type, so unlike Claude Code there is no helper-free route.

---

<a id="adr-039"></a>
## ADR-039 — The frontend is native Rust drawn on the GPU, not a webview

**Status:** Accepted, implemented for the upgraded first vertical. Supersedes the UI half of ADR-001. The webview frontend
is deleted: `ui/` (51 TypeScript files, 13,317 lines, of which 18 test files and 3,821 lines were tests,
plus 1,390 lines of CSS) and `crates/turn-ui` (the Tauri shell, 2,230 lines) are gone, and `turn-ui` is out
of the workspace members. What exists in their place is `crates/turn-gui`: an `eframe`/`egui` window over
`wgpu`, covered by unit tests plus snapshot tests that render the real widget tree through `wgpu` with no
display attached. The deterministic Reviewer vertical now crosses the daemon/client model; the manual
authenticated external-CLI smoke test remains, as recorded in `ROADMAP.md` §M8.

### Context

ADR-001 chose a Tauri shell around a TypeScript frontend with `xterm.js`. That decision was recorded with
its alternatives and its downsides, and it was wrong on the one axis the log did not cover: **it was never
confirmed with the product owner.** The reasoning was technical and internal — best terminal widget, no
runtime shipped, one Rust codebase for everything touching the OS — and the question "is a webview
acceptable for this product at all?" was treated as settled by the engineering argument. It was not. When
the frontend was shown, it was rejected on sight, before any discussion of its behaviour. Turn is a
desktop tool for people who live in terminals, and a window that is a web page is the wrong thing for that
audience regardless of how well the web page performs.

Two things are worth writing down about that, because they are the reusable part:

- The rejection was not about quality. The frontend's own tests passed. It was about the category of the
  artefact, which is a product decision and not an engineering one, and the log should have carried an
  explicit confirmation rather than an unchallenged assumption. An ADR that lists alternatives is not the
  same as an ADR that was agreed.
- The cost of being wrong was bounded, and that was not luck. See below.

### The swap cost ~13k lines of TypeScript instead of the product

ADR-002 put a daemon in from day one and ADR-030/031/032 made the daemon↔client boundary a versioned
protocol of view models that arrive already derived. Both were argued at the time partly as insurance
against exactly this: a client is replaceable if it holds no product state.

That insurance paid, and it is measurable. Everything deleted here is rendering. Nothing deleted decided
anything a user would notice:

- No state was recomputed in the client. `state_label`, `severity`, `score`, `provisional` and
  `relation_is_provisional` arrive derived (ADR-032), so no product rule went out with the frontend.
- No pty, no process table, no attention queue, no persistence was in the client. The daemon owns them,
  which is why deleting the window did not stop or lose anything.
- The protocol did not change to accommodate a new client. The Rust window speaks the same requests and
  reads the same pushes.

The daemon, the six library crates and `turnd` are untouched by this ADR. They hold the overwhelming
majority of the workspace's tests — every one of them outside `turn-gui` — and not one needed editing to
change frontends. That ratio, a whole frontend replaced and the rest of the product unaltered, is the
concrete payoff of a boundary drawn early for a reason, and it is the argument to remember the next time an
early decision looks like premature structure.

### Alternatives considered

**Keep the webview and iterate on it.** Cheapest by a wide margin, and every test already passed. Rejected
because the objection was to the medium, not to the implementation. No amount of CSS makes a webview stop
being one, and the WKWebView/WebKitGTK divergence ADR-001 named as the biggest platform risk in the stack
would have stayed unexamined and unpaid for.

**Native per platform: AppKit on macOS, GTK on Linux.** The most native possible result. Rejected for the
same reason ADR-001 rejected Swift + AppKit — it forfeits parity by construction, and here it would mean
two view layers for one product rather than one, which is worse than the two-language problem it replaces.

**A Rust GUI toolkit that retains a real terminal widget.** There isn't one. That was true when ADR-001 was
written and it is still true, and it is why the honest form of this decision is "we paint the terminal
ourselves" rather than "we found a widget".

**`egui`/`eframe` over `wgpu`, with the terminal painted from cells.** Chosen. One codebase for both
platforms, drawn by the GPU through the same code path on Metal and Vulkan, so parity becomes a property of
the build rather than of discipline. `egui_kittest` renders the widget tree headlessly and compares against
committed PNGs, which makes the visual layer reviewable in CI — something the webview frontend never had.

### Cells, not bytes — and the second VT emulator is gone

The window does not receive an escape stream. The daemon already keeps an authoritative `vt100`-parsed
screen per pane (ADR-009) — it must, because on-demand previews and output heuristics work with no client
attached — so the client consumes that directly and paints a grid of cells with their colours and
attributes. `crates/turn-gui/src/cells.rs` holds `Grid`/`Cell`/`CellAttrs`/`Rgb` and the conversion from a
parsed screen, asserted by `a_parsed_screen_becomes_the_grid_the_client_paints`,
`a_full_screen_program_reports_its_alternate_screen_and_its_input_modes`,
`a_hidden_cursor_is_reported_as_absent_rather_than_as_a_position` and
`a_wide_glyph_from_a_real_stream_is_not_painted_twice`.

The consequence is the one worth having: **there is no longer a second terminal emulator.** The webview
frontend had two independent parsers of the same bytes — `vt100` in the daemon for heuristics and its
then-current thumbnail view, `xterm.js` in the window for the user — and every disagreement was a bug whose
symptom was "the sidebar says one thing and the terminal shows another". That class of bug is now
unrepresentable, because there is one parse and the client paints its result.

This does not reopen ADR-008. Turn still does not write a terminal emulator: parsing escape sequences,
tracking modes, scroll regions and character sets remains `vt100`'s job in the daemon. What the client
gained is a *painter* — draw these cells with these colours at this cell metric — which is a far smaller
and far more testable job than an emulator, and one a snapshot test can actually check.

### Consequences

- One toolchain. `cargo test --workspace`, `cargo clippy --workspace --all-targets` and
  `cargo fmt --all -- --check` are the whole gate; there is no second test runner, no lockfile in another
  language, no `pnpm build` step whose output the Rust build embeds. CI lost its `ui` job and its node/pnpm
  setup, and the Linux job lost five system packages: the window needs none, because X11, the keymap,
  Wayland and Vulkan are all reached by `dlopen` rather than linked.
- The visual layer became reviewable. `crates/turn-gui/tests/snapshots.rs` renders through `wgpu` with no
  display and diffs against committed PNGs; `UPDATE_SNAPSHOTS=1` re-records so an intentional change
  arrives as a reviewable image. This already earned itself: the first snapshot caught two labels drawn on
  top of each other, which no logic test could see.
- State is still never invented in the client, and the rule that colour never carries meaning alone is now
  structural rather than a convention: `theme::state_marker` returns a colour **and** a glyph together, so
  a caller cannot obtain one without the other. `every_state_has_a_glyph_as_well_as_a_colour` and
  `the_attention_colour_is_reserved_for_states_that_block_the_user` enforce it.
- **Downside: every widget is hand-built.** There is no scrollbar, no text input, no list virtualisation, no
  modal, no focus ring that anyone else wrote. `egui` supplies primitives and layout, not Turn's chrome. The
  sidebar, the permission banner, the attention queue and the command palette are each a function that
  allocates a rect and paints into it, and each of them is code that has to be maintained and tested. The
  webview gave all of this away for free; that was a real advantage and it has been given up.
- **Downside: there is no CSS.** A visual change is a code change, recompiled — not a stylesheet reload.
  There is no inspector to hover an element in, no computed-style panel, and no way to try three paddings in
  ten seconds. The 1,390 lines of CSS that were deleted did not vanish; the work they represented moved into
  Rust, where it is more verbose and slower to iterate on. Snapshot tests are the compensation, and they are
  a good one, but they are not the same as a live inspector.
- **Downside: accessibility is now work rather than something the platform provides.** A webview hands over
  the accessibility tree: an element with a role and a label is exposed to VoiceOver and Orca without
  anyone asking. A GPU-drawn window has no DOM, so every accessible name has to be constructed and attached
  deliberately. `egui` now exposes the unified navigator through AccessKit `Tree`/`TreeItem` roles; tests
  reach every hierarchy level by accessible name and reject duplicate legacy `ListItem` navigation.
  VoiceOver/Orca acceptance with real assistive technology remains work, so passing the structural test is
  necessary but not claimed sufficient.
- **Downside: IME is work too.** Composing Japanese, Chinese or Korean text, dead keys, and the candidate
  window are things WKWebView and WebKitGTK do. `egui` has IME support and `winit` delivers the events, but
  wiring them into a terminal that also has to forward raw bytes to a pty is unwritten and untested here,
  and it is the kind of thing that is invisible until a user who needs it cannot type.
- **Downside: the snapshot baseline is macOS-only for now.** The committed PNGs were recorded through Metal,
  and `egui_kittest`'s defaults allow no differing pixels at all, so they cannot be trusted against
  lavapipe on a Linux runner without a measured tolerance or a second baseline recorded there. CI states
  this in the workflow rather than skipping the tests quietly, and the Linux job still compiles the
  snapshot target on every push so it cannot rot. Tracked in `ROADMAP.md` §Technical debt.
- **Downside: `xterm.js`'s correctness was free and now is not.** Selection, copy on a wrapped line,
  reflow on resize, URL detection, bracketed paste, wide and combining glyphs at a cell boundary — all of
  it was someone else's solved problem. Cells arriving pre-parsed removes the hardest half (the emulator),
  but the interaction half is Turn's now, and the parts of it that are unwritten are unwritten.

### What was carried over from the deleted frontend

Recorded here because the code is gone and the reasoning in it was expensive to acquire. The full inventory
is in the retirement report; the load-bearing items are:

- **The terminal-sacred key set.** `ui/src/keys/keymap.ts` bound `mod` as Command on macOS and Control
  elsewhere, and carried five deliberate exceptions where `mod+key` on Linux *is* an ASCII control
  character a running process needs: `mod+K` (kill-line), `mod+N` (next-history), `mod+[` (**which is
  ESC**), `mod+]` (GS) and `mod+/` (undo). Its test asserted against all 26 `Ctrl`+letter chords plus
  `[ ] \ / -` and Space, and it exists because a nine-code version of the same test passed while the
  default map was quietly taking ESC away from every Linux user. Any Rust keymap must reproduce that
  exhaustive assertion, not a sample of it.
- **The ordering mirrors, and that they are mirrors.** Sidebar order was pinned → needs_user → severity →
  last_activity → name, and queue order was score descending with ties to the older demand. Both were
  written as copies of the daemon's own comparators, with the note that a client which starts deciding what
  is urgent gives the product two answers to its central question.
- **The two provisional-ness rules.** A guessed parent link is drawn as a guess and spoken as one; a
  provisional demand is rendered "possibly …" in the accessible name. Both are rendering decisions taken
  from daemon-supplied flags, never inferred client-side.
- **The permission banner's shape.** Prominent, never modal, carrying the attention id it is displaying
  (a session can hold two demands at once, so a "Later" button that re-searched by session would snooze the
  wrong one), and offering exactly one action: go to the pane. Nothing in it approves anything.
- **`Effect` handling.** Only `focus` may move the user; `focus_deferred` and `focus_denied` are verdicts to
  report, not instructions. `run_custom` is recorded and never executed. Every perceptible channel is
  delivered rather than computed and dropped.
- **The reconnect supervisor's two non-obvious rules**, from `crates/turn-ui/src/daemon/`: a malformed frame
  costs one line and never the connection (tearing down a socket over one bad frame takes thirty running
  agents off screen), and a handshake is not health — a connection must last 10s before the backoff resets,
  or a crash-looping daemon becomes a hot reconnect loop. Plus the reason `welcome` carries
  `daemon_pid` + `daemon_started_ms`: the pair distinguishes "my socket blipped and every pty is where I
  left it" from "a different daemon is running", which need different recoveries.

---

<a id="adr-040"></a>
## ADR-040 — One unified hierarchy, one main-checkout writer, background subagents

**Status:** Implemented v0.1 history with two target clauses superseded. The canonical hierarchy remains.
ADR-059 supersedes the Layout-only centre with one selected WorkSurface, and ADR-065 supersedes every
primary-checkout writer with operator-only primary `main` plus dedicated worktrees. The decision otherwise
extends ADR-017's flat parent-pointer model, ADR-032's daemon-derived views, ADR-033's append-only migration
discipline and ADR-039's native egui/wgpu client, and narrowly amends ADR-036 as described there.

### Context

Before this decision, the backend modelled `Workspace → Session → ProcessNode` while the native client
projected a flat Session list plus a separate process view, and Session creation did not arbitrate checkout
ownership. The implementation now projects one hierarchy and enforces a fenced canonical writer; this
context remains the reason the migration cannot preserve the former visual model.

A subagent is work, not a layout instruction. Automatically opening one spends screen space and focus on
an event the parent produced, violating Turn's rule that the user decides what to view.

### Decision

The left sidebar is the single persistent **navigation projection**: Workspace → Session → Agent/Tool →
child. It is derived from normalised ownership — `Session.workspace_id`, `ProcessNode.session_id` and
`ProcessNode.parent` — rather than stored as polymorphic tree foreign keys. Workspace containment and a
runtime parent relationship are different facts and remain different fields.

Sessions are not duplicated in top tabs, bottom strips or a permanent overview, and Agents are not
duplicated in a second persistent tree. **Historical v0.1 clause, superseded by ADR-059:** the right side was
an optional contextual inspector and the centre only the user/template-chosen Layout. The target instead
uses the selected Node's one WorkSurface. Tree selection, active Session, focused Pane and pending Attention are
independent. Tree expansion and selection persist per stable UI `surface_id`; they are not `TurnEvent`s and
are never broadcast as another window's selection.

**Historical v0.1 writer clause, superseded by ADR-065:** `SessionMode` was a closed enum with wire values
`main_checkout`, `read_only` and `isolated_worktree`.
Inside one canonical Turn data directory, every primary checkout has at most one unreleased blocking
`exclusive_write` claim, owned and arbitrated by the daemon.
Its semantic record is `workspace_id`, `session_id`, `checkout_id`, `mode`, `state`, `acquired_at` and
`heartbeat_at`; implementation identifiers, fencing generations and release timestamps may support that
contract but do not change it. The fencing generation is monotonic per canonical checkout path within that
store even if a
Workspace or lease row is deleted and recreated; every non-released state remains blocking. A heartbeat
timeout alone never transfers ownership. A second Session must
focus the owner, use technically enforced read-only mode where available, create an isolated worktree, or
cancel. When the failed request came from a Template, the safe alternative carries only the original
Template id and interpolation inputs: the daemon reloads the authoritative definition and preserves its
Layout, commands, environment, Attention policy, tmux flag and naming. The client never flattens a
`TemplateSummary` into guessed panes. Worktrees declare the shared resources they do not isolate.

The checkout lease is not the daemon-instance boundary. Before SQLite, migrations or restore can run,
`turnd` acquires a non-blocking operating-system lock on the canonical data directory. Socket ownership is
checked separately, so configuring another socket cannot create a second cooperating owner of the same
store and fence live leases while the first daemon still owns PTYs. The stable lock file is retained and the
kernel releases the lock on process death; unsupported locking fails closed.
Two deliberately separate data directories are independent authority domains today; a host-global claim
would require a checkout-scoped OS lock and remains hardening work.

The lease does not authorise an arbitrary launch directory. Session creation validates the Session cwd and
all configured Pane cwds against the assigned canonical checkout before acquiring the lease or running init
commands. Pane creation validates before mutating the Layout, and every Pane, relaunch and init command
repeats canonical containment at the final PTY boundary. This is only an initial-cwd invariant: it is not an
OS sandbox and does not stop same-user code from opening other paths or changing directory after launch.

An AgentNode exists independently of any Pane and may have zero or many pane bindings. A subagent reported
by its parent is inserted under that parent, preserving a declared name only when the source actually
declared one. Tool roles such as `Explore` or `default` are not silently promoted to names. Parent edges
carry relationship kind and confidence separately from the confidence that an event occurred. The subagent
starts in the background, never opens a Pane, changes Layout, steals focus or resolves attention. `Space`
opens a cheap Quick Preview; `Cmd+Enter` explicitly creates a temporary pane. Closing a pane never stops
the agent. A node without its own PTY opens Preview or Process Details, never a fabricated terminal.
A temporary binding belongs to one live `surface_id`; another surface never sees it, and replacement
connection, final disconnect or daemon restart expires it without changing the saved Layout or Agent.

Typed input is not trusted display input. User-authored Workspace/Session/Template names containing
control, C1, ANSI, bidi or invisible formatting are rejected so identity is never silently rewritten.
Agent/process declarations from adapters or the OS are normalised and bounded before reducer, push,
inspector and persistence; process argv has count, per-argument and aggregate caps. The raw supervisor
snapshot remains available only for PID traversal/classification and never becomes navigation text.

A worker callback that cannot yet be bound to a node retains its authenticated hook parent and optional
tool-owned external id as a correlation scope. That parent is not presented as the subject. An explicit
unknown id never falls through to the one different child currently visible, and an id-less resume only
resolves the provisional flow under the same parent. Migration 007 persists this scope so restart does not
broaden it to the Session.

The same subject boundary governs lifecycle and focus. A runtime exit closes only its exact runtime-bound
permission/question interaction and unresolved discovery scopes owned solely by that runtime; it preserves
TurnComplete/unread-result, failure and review demands marked `survives_owner_exit`, and never clears exact
children that may still be alive. A declared child stop also retires its earlier parent/external-id scope.
Every such mutation reaches SQLite and the queue projection.
Deferred focus is valid only while that exact subject remains actionable, and mute/snooze/dismiss cancel the
matching jump. Permission UI may show command, cwd and risk only after an exact node join; it never substitutes
the primary Agent for a modern unresolved or stale worker identity. A node-less but Session-scoped demand
raises that Session and its Workspace aggregate/queue without falsifying the parent Agent's primary state.

The full normative model, migration, events, API and wireframe are in
`docs/UNIFIED_HIERARCHY_UPGRADE.md`.

Activity Preview is not restored scrollback or a conversation summary: it is short, provenance-labelled
navigation status. The preview store persists no raw PTY bytes or grid, normalises and redacts before write, keeps at
most 20 snapshots per node and 2,000 globally. A recovered preview retains its original timestamp; a
separate recovered/stale visual marker remains planned and clients must not reinterpret it as fresh.
High-frequency preview updates are snapshot state and coalesced pushes, not append-only domain events.
ADR-044 later supersedes the terminal/scrollback prohibition while retaining this preview boundary:
raw history is a separate private terminal archive, never Activity Preview or semantic context.

Hook callbacks are hostile ingress, not event-log content. The Claude adapter reduces a callback directly
to typed `EventKind`, `EventSource`, confidence and node/session identity; it does not attach the callback
body to `TurnEvent::raw`. `EventRepo` enforces the boundary again for every hook source, preserving only the
typed fact and its non-sensitive tool/event-name provenance. Migration 005 nulls historical hook `raw`
columns so an upgrade does not leave the previous exposure behind. Adapter fixture files remain test source
artefacts in the repository, not runtime product data.

### Alternatives considered

**Keep separate Session and Agent navigators.** Rejected: identity, state and attention then have two
persistent homes, and selection has ambiguous meaning.

**Allow concurrent writers and show a warning.** Rejected: a warning does not prevent Git/index and file
races. Isolation must be a daemon-owned invariant, not agent guidance.

**Open every discovered child as a Pane.** Rejected: discovery is not user intent and a busy parent can
destroy a carefully chosen Layout.

**Make Pane the owner and reconstruct agents from open views.** Rejected: background work, restoration and
attention all need an identity that survives with no view.

### Consequences

- Navigation reflects the domain directly at 3 or 30 Sessions; previews make background work legible
  without rendering every terminal.
- Session creation becomes a conflict-capable operation and clients must present explicit alternatives.
- A Template conflict retry is a separate typed operation so safe mode selection cannot silently collapse a
  Coding Layout into the blank/shell fallback.
- `ProcessNode.pane_id` migrates to a binding table; legacy single bindings remain readable through the
  migration only.
- UI selection, pane focus and attention are separate persisted concepts and require explicit tests.
- A read-only Session needs OS/process enforcement where viable; model instructions alone are not a
  security boundary.
- Protocol v3 exposes one revisioned `HierarchySnapshot`; a client that misses a revision requests a full
  replacement rather than applying a guessed diff. Lease conflicts carry structured owner and recovery
  choices; clients never parse a human message to decide what to offer.
- Migration 003 creates checkout assignments, bindings, preview storage and reconciliation state but
  **grants no lease automatically**. Legacy Sessions start conservatively as unenforced read-only metadata
  until the daemon proves a sole viable writer or the user reconciles them. No process is launched, killed,
  moved or retroactively made safe by DDL.
- Migration 007 adds optional parent/external-id columns to durable Attention. Existing rows retain null
  scope, while new ambiguous worker demands deduplicate and resolve within their authenticated boundary.
- Migration 008 records whether a demand truthfully survives owner exit and its demand kind; legacy entries
  remain non-surviving instead of receiving guessed postmortem authority.

---

<a id="adr-041"></a>
## ADR-041 — Runtime events checkpoint Session, event log and Attention in one transaction

**Status:** Accepted, implemented for `Core::ingest`, with rollback and real-restart tests.

### Context

One normalised runtime event can change three durable projections at once: the Session tree/state (including
a newly declared node, tombstone or preview), the append-only event log, and the Attention Queue. Writing
those through three repository transactions allowed restart states that had never existed in memory: a
permission event without its Agent, `YOUR TURN` without the event that caused it, or a stopped node whose
old permission resurrected from the queue.

Publishing UI pushes between those writes made the split externally visible as well as crash-visible. A
client could be told an event was accepted even though the final queue write failed.

### Alternatives considered

**Keep independent repository writes and repair on restore.** Rejected. Restore cannot prove which of three
partial records was intended, and heuristics must not invent authority after a crash.

**Make the event log the only source of truth and replay every event.** Rejected for the MVP. It would turn
all current Session/layout migrations into event migrations, replay high-volume history at startup and make
per-surface UI state an awkward exception. The existing state-plus-audit model remains appropriate.

**Checkpoint the complete affected projections in one SQLite transaction.** Chosen.

### Decision

After the reducer and Attention Manager accept a runtime event in memory, `turnd` calls one store boundary:

```text
save Session (row → layout/bindings → nodes/previews)
→ append idempotent TurnEvent
→ replace durable Attention Queue
→ COMMIT
→ publish event/tree/node/session/effects/queue to clients
```

The ordering is contractual: a Stop-before-Start event may create a node tombstone, so the referenced node
must exist before the event foreign key is written; Attention is the projection produced by that same event
and comes last. Any failure rolls back all three.

No client push or focus/sound/notification effect is emitted before commit. A failed checkpoint creates a
FIFO barrier: the unapplied later events wait behind it, the failed event is retried before periodic semantic
work, and standalone Session/Attention flushes that could leak its in-memory state are refused until the
barrier clears. Protocol requests retry the oldest checkpoint before dispatch and return `unavailable` while
it remains blocked: reads cannot observe the uncommitted projection, and mutations cannot hitch a rejected
change onto a later retry. Effects published after a successful checkpoint do not write the queue twice.

### Consequences

- Restart sees a possible complete state, never a cross-product of independent writes.
- A referenced child declaration, its event and its demand can be proved with one rollback injection test.
- Permission, process failure, postmortem Attention and Stop-before-Start tombstones are exercised through
  a real daemon restart, not only repository mocks.
- **Downside:** every semantic runtime event currently rewrites the small durable Attention Queue. This is
  simple and correct at MVP scale; measured write amplification may justify a transactional delta later,
  but not separate commits.
- **Downside:** while SQLite remains unavailable the in-memory reducer is ahead of durable state and later
  runtime events wait in memory. The hook ingress remains bounded and may drop rather than stall Agents;
  explicit queue bounds/coalescing for a prolonged disk outage remain hardening work.

---

<a id="adr-042"></a>
## ADR-042 — The desktop bootstraps a detached sibling daemon and serialises creation until operations have IDs

**Status:** Accepted, implemented. Extends ADR-002's daemon boundary, ADR-030's socket transport and
ADR-040's Workspace/Session creation rules.

### Context

The native window was a real protocol client but still assumed that somebody had started `turnd` by hand.
On a clean machine that made the product's first instruction fail before the user could create a Workspace:
the form could render, but no authority existed to persist it or acquire its write lease. Requiring a shell
command before the first desktop action contradicted both the zero-state experience and the reason to ship a
desktop entry point.

Startup also has two distinct notions of ownership. A socket says where clients should connect; it does not
prove who may migrate or write the database. Conversely, the canonical data-directory lock establishes one
store/PTY owner even when two clients race through different socket aliases. Collapsing those concepts would
make stale endpoints or two simultaneously opened windows dangerous.

Finally, protocol request envelopes correlate a response to a transport request, but Workspace and Session
creation do not yet carry a durable operation id into the UI's product state. Allowing two creations to
overlap would let a late lease conflict or generic Session response consume the newer form's intent.

### Alternatives considered

**Require the user to install and start a service.** Rejected for the first-run product path. A service may
be a future distribution option, but creating the first Workspace cannot depend on terminal setup.

**Run the daemon in the window process.** Rejected. Closing or crashing the window would close every PTY,
and the protocol/process boundary that made the UI replaceable would become theatre.

**Spawn an ordinary child and kill it with the window.** Rejected for the same lifetime reason. The daemon
must outlive the UI that happened to start it.

**Search `PATH` for `turnd` and `turn-hook`.** Rejected. It can pair the window, daemon and hook helper from
different installations, and it makes the effective executable depend on mutable shell configuration a GUI
may not even inherit.

**Permit parallel create requests and infer which draft a response belongs to.** Rejected until the
protocol carries explicit operation ids. Workspace identity is insufficient when two Session attempts can
target the same Workspace.

### Decision

The `turn` desktop binary resolves one absolute data-directory/socket pair once at startup. It probes the
endpoint first. A reachable listener is left untouched and the normal protocol handshake decides whether it
is a compatible Turn daemon. If the endpoint is absent or stale, the window starts `turnd` as a **detached
companion** with the exact `--socket` and `--data-dir` values. No shell is involved; stdin is null, output is
appended to an owner-only, symlink-refusing `turnd.log`, and on Unix the companion receives its own process
session. Dropping the monitor or closing the window never terminates it.

Executable resolution is closed and ordered:

1. a non-blank `TURN_TURND_BIN` override for controlled development/package layouts;
2. `turnd` beside the running `turn` executable — the release package contract;
3. in debug builds only, `cargo run` against the fixed workspace manifest compiled into this source build.

The source fallback builds both `turnd` and `turn-hook` first. Release builds fail visibly if the packaged
sibling is absent; they do not search `PATH`. The release/CI build produces the sibling set `turn`, `turnd`
and `turn-hook`, and `turnd` locates `turn-hook` beside its own executable. A missing helper is non-fatal but
forces the affected adapter to report its degraded integration level.

Socket probing is only a bootstrap hint. `turnd` remains authoritative: before SQLite, migrations or
restore it must acquire the non-blocking lock on the canonical data directory. Two windows may race to
launch; one daemon wins that lock and the other's exit code 3 is treated as provisional contention until a
real handshake succeeds. The GUI never removes a non-socket filesystem entry, never adopts an unverified
listener, and never treats a different socket as permission for a second store owner.

Synchronous launch errors and later companion exits are shown in the window and logged. A successful
handshake clears only the companion diagnostic. The daemon surviving the UI is guaranteed; surviving a
daemon exit is not, because its process owns the PTY masters (ADR-002).

Until create operations gain explicit ids, the desktop permits exactly **one Workspace or Session creation
in flight**. New, Quick New and form submission all share that gate. The pending Template/workspace/name/task
intent remains attached to its own request; unrelated Session responses cannot clear it, and a connection
generation change returns the form to a visible failed/retry state rather than replaying the mutation to a
different daemon.

### Consequences

- A clean desktop launch can create a Workspace and Session without a separate daemon command.
- Quitting the window does not stop Agents. Reopening it reconnects through the same protocol and store.
- Endpoint discovery cannot weaken the data-directory singleton boundary; startup races fail safely.
- The helper install location is no longer open-ended: package all three sibling binaries from the same
  build. Signed bundles/archives and an explicit cross-binary version check remain M9 work.
- Companion failure is actionable in the UI and has a stable local log, rather than looking like an inert
  button.
- **Downside:** the first debug launch may block while Cargo builds both companions.
- **Downside:** there is no automatic idle-daemon shutdown policy; persistence is preferred to guessing
  that a background Agent is disposable.
- **Downside:** serial creation prevents legitimate parallel setup. Add operation ids to the protocol and
  drafts before relaxing this gate; timing-based correlation is not an acceptable substitute.

---

<a id="adr-043"></a>
## ADR-043 — Agent context handoffs are reviewed, bounded daemon capabilities

**Status:** Accepted, implemented for same-Session, controllable Agents.

### Context

Agents working in parallel need a deliberate way to share useful findings. Copying raw terminal history is
noisy, can contain secrets and control sequences, and cannot prove which text the user reviewed. Letting the
GUI write an arbitrary second-step payload would make the review cosmetic. Treating an Agent's pending
permission prompt as a convenient input boundary could also turn a handoff into an accidental approval.

### Alternatives considered

**Copy the visible terminal transcript.** Rejected. A rendered screen is transient, may omit scrollback and
contains untrusted output that was never designed as a compact prompt.

**Let the UI build and send a prompt directly with `write_pty`.** Rejected. The authority boundary would be
duplicated across clients, the displayed and delivered payloads could diverge, and retries could submit twice.

**Persist full handoff bodies for replay and audit.** Rejected. It creates a new durable secrets store and a
daemon restart cannot know whether a PTY accepted a write before failure.

**Prepare a daemon-owned capability, review its exact body, then deliver it once.** Chosen.

### Decision

Handoffs are limited to two distinct agentic nodes in the same non-archived Session. The source may be a
historical Agent, but contributes only bounded, stable, visible Activity Preview facts. Raw PTY bytes,
scrollback and inferred current-task prose are excluded. Labels, facts and the optional user instruction are
sanitised and passed through known-secret/secret-shaped redaction before the exact payload crosses the
protocol. No pattern detector proves arbitrary free text secret-free, so exact review remains the final
disclosure boundary.

Preparation creates an in-memory `HandoffId` capability bound to the client, Session, source and destination;
it performs no PTY write. Delivery names only that capability. The daemon revalidates all endpoints and
requires the destination to own a live PTY, be at an idle/done turn boundary and have no pending permission,
question or other interaction. The explicit Send action submits one bracketed paste plus Enter. The same
connection may retry a confirmed success without another write. Any uncertain write consumes and fences the
capability; Turn tells the user to inspect the Agent and never replays automatically.

The handoff subsystem creates no dedicated durable semantic body record: drafts expire after ten minutes,
disappear on client disconnect and never enter `Debug` output or the Event row. The replay fence retains only
ids/outcome metadata for one hour and is bounded. Once delivered, however, the bytes necessarily enter the
destination program and may persist in its provider transcript, visible terminal/scrollback and ADR-052
journal. Review must state that downstream boundary; disabling Session terminal history before launch is the
available Turn-side control for especially sensitive PTY delivery. Pane state, tree selection, focus and
layout are not part of the operation.

### Consequences

- The user sees exactly what will be submitted and nothing is sent during Review.
- Secrets and terminal control sequences do not gain a dedicated handoff store; downstream provider and
  terminal retention remains visible and outside that claim.
- Handoffs work for background Agents without opening panes or destabilising a Session layout.
- A handoff cannot masquerade as an answer to an existing permission or question prompt.
- **Downside:** only stable Preview facts are transferred, so early or poorly integrated Agents may provide
  little automatic context; the user can add an instruction explicitly.
- **Downside:** the destination must support bracketed paste and be at a safe input boundary.
- **Downside:** delivery proves a PTY submission, not semantic receipt. End-to-end acknowledgement would need
  an adapter-level protocol rather than a terminal heuristic.

---

<a id="adr-044"></a>
## ADR-044 — A terminal pane hosts the user's shell, and an agent runs inside it

**Status:** Accepted, implemented. Narrows ADR-017's evidence ladder to say that Turn's own launch is
evidence, and extends ADR-019's on-demand process scan with a single-pid liveness check.

### Context

A pane's process *was* the agent. Two reports followed from that one fact, and neither was a bug in the
code that could be fixed where it was observed:

* Leaving Claude Code with `/exit` left the pane flickering and dead. The process the pane was a view of
  had ended, so there was nothing left to draw and nothing left to type into. In every terminal the user
  already owns, quitting an agent returns them to a prompt.
* `+ Pane Agent` did nothing. With no `default_agent` configured, the pane's command resolved to `None`,
  the pane opened empty, and nothing said why.

### Alternatives considered

**Relaunch the agent when it exits.** Rejected outright. Turn never relaunches on its own — not on
restore, not after a crash — and the pane's emptiness is not a reason to make an exception.

**Keep the agent as the pane's process and re-draw the dead pane as a recovery offer.** This is what the
restore path does for a process that did not survive a daemon restart, and it is right *there*: the daemon
was not running, so it cannot know what the user wants. It is wrong here. The user quit the agent
deliberately, in a terminal, and expects the terminal.

**Put the agent in the shell's `argv`: `shell -i -c '<agent>; exec <shell> -i'`.** Tried, measured on real
pseudo-terminals, and rejected. It is the tidier shape — the command cannot race the shell's start-up — but
**zsh exits with 130 when a command given to it with `-c` is interrupted**, interactive or not, with `exec`
waiting after the semicolon or not. Ctrl-C in the pane would then take the pane down along with the agent:
the same report arriving by a different route. bash and `sh` survive it; zsh is the default shell on macOS.

**Host every pane's command in a shell.** Rejected. A pane that names a job — `npm run dev`, a test run —
exists to show that job and its ending. Wrapping it in a shell that outlives it would hide the exit code
that is the whole point of the pane.

### Decision

A pane whose command the adapter registry recognises as an agent runs `shell -i`, and the agent's command
line is **written to the pty**, exactly as if the user had typed it. The pane's process is the user's shell;
the agent is a command running in it; and when the agent ends the shell is still there with its prompt, its
pid and its scrollback. Which panes this applies to is decided by the adapter, not by the pane's label
(ADR-029): `claude` typed into a plain terminal pane is an agent, and `npm run dev` in an agent pane is not.

Writing the command in does not race the shell's start-up, and that was measured rather than assumed: the
bytes wait in the terminal's input queue until the shell begins reading, so they survive an rc file being
sourced and a line editor being set up — verified with a zero-millisecond delay against a shell with a slow
rc, on zsh, bash and `sh`, for both ctrl-C and ctrl-D. It also puts the command in the user's shell history,
which is what makes "the user can start it again by typing" an up-arrow rather than a paragraph of
documentation.

The agent's *environment* is never typed. One of its values is a URL carrying this node's hook token, and an
input line reaches the pane's screen, its scrollback and the user's history file. A launch into a shell Turn
is about to start puts it in that shell's environment, where it is invisible and inherited. A launch into a
shell that is already running — which is what `RelaunchNode` does — writes it to an owner-only file in the
node's scratch directory and tells the shell to source it.

One launch produces **two nodes**. A `Shell` node owns the pty and the pane. An agent node owns everything
that makes an agent an agent — its `AgentInfo`, its adapter, the integration level the launch achieved, its
hook token and its turn axis — and hangs off the shell at `Relation::Confirmed`. Confirmed is the whole
point: Turn wrote that command line itself, so the edge is knowledge, not an inference from the process
table. ADR-017's ladder is unchanged; this is a new source at its top rather than a new rung.

The agent's *pid* is a separate question from its *identity*. The shell forked it, so Turn never held it:
the pid is learned by matching one direct child of the shell against the executable Turn asked for, and
until that happens the node carries no pid rather than borrowing the shell's. Its death is noticed by
asking the kernel about that one pid on each tick — a single `kill(pid, 0)`, not the process-table refresh
ADR-019 keeps on demand — and recorded as `Lifecycle::Lost`, because Turn did not see it exit and will not
invent an exit code.

Everything Turn types now passes through a shell, so every word is quoted with `shell-words`: notably the
`--settings` path the adapter generated, which Turn produced from a data directory the user chose and which
may contain a space, a `$` or a backtick. It has to arrive at the agent unchanged, and it must not be able to
run anything.

`+ Pane Agent` always resolves to something: the workspace's configured agent, else the first agent CLI on
the user's `PATH`, else the shell with a printed sentence saying why it is only a shell. A configured agent
that is missing is named rather than silently replaced by a different one.

### Consequences

- Quitting an agent leaves a working shell in the pane. Ctrl-C leaves one too. The pane is never dead, and
  the user starts the agent again by typing — or with `RelaunchNode`, which types it for them into the shell
  that never died.
- Closing an agent pane still refuses to stop the agent, as it always did; the check now also asks whether
  the pane's shell is *hosting* one.
- `Stop Agent` signals the agent's own pid, which is the difference between stopping the agent and
  interrupting whatever is in the foreground. `Interrupt` still goes through the tty, and for a hosted
  agent that is not a detail but the only correct route.
- Attention routing walks from a hosted agent to its shell, because the shell's tty is where an answer is
  typed. A node with a pid but no terminal of its own is not an input boundary.
- **Downside:** an agent pane now starts two processes and shows two rows. The tree is one deeper, and a
  session's `running_count` counts the shell.
- **Downside:** the pane's shell sources the user's rc, which is what makes the agent's `PATH` theirs — and
  also means an rc that prints, or that `cd`s, does so in the pane. Launch-root containment still resolves
  and proves the starting directory; it does not follow a process that moves.
- **Downside:** the command Turn typed is in the user's shell history, alongside the commands they typed
  themselves. That is the same thing typing it would have done, and it is what makes an up-arrow work.
- **Downside:** Turn does not hold the agent's process, so `SIGTERM` from `TerminateNode` is refused in the
  moment between the launch and the sweep that identifies the pid.
- **Downside:** `Process::hosted` is in-memory only. After a daemon restart the surviving parent edge is
  still durable and still confirmed, but the recovery offer is the pane's — the shell's — as it is for
  every other pane.

---

<a id="adr-051"></a>
## ADR-051 — Settings resolve in the daemon through one explicit hierarchy

**Status:** Accepted, implemented. `turn-core::settings`, the settings requests in `turn-proto`, and the
native settings window.

### Context

Turn has defaults that apply globally, to a Workspace, to a Template, to one Session and temporarily to one
window. Letting every client merge those layers independently would create multiple sources of truth, make a
reset indistinguishable from writing a copied default, and leave headless clients unable to explain why a
value is active.

### Decision

The daemon is the only settings resolver. Precedence is Global, Workspace, Template, Session, Temporary;
later layers override earlier ones. A resolved entry carries its value, origin, shadowed layers and the
levels where it may be written. Owner ids are explicit for Workspace, Template and Session operations.

Writes and resets answer with the complete resolved settings document, not an acknowledgement. The GUI
replaces its copy with that answer and fetches again after any write, so it never patches or reimplements the
precedence table. Unknown values already stored by another Turn version remain visible and resettable, while
new writes still require a key and value shape known to this daemon. Secret values are redacted before they
cross the protocol boundary.

Keyboard bindings are resolved through the same hierarchy, but applied in the window because the daemon does
not receive physical key events. Conflict detection compares physical modifier equivalence on the current
platform, including Command/Control aliases, rather than comparing display strings.

### Consequences

- Every client observes one resolved value and can explain where it came from.
- Removing an override reveals the next layer without the client guessing what changed.
- Settings from a newer build can be removed safely by an older build without pretending it understands them.
- **Downside:** a write returns more data than the changed key, and every settings surface must treat that
  response as authoritative.

---

<a id="adr-052"></a>
## ADR-052 — Terminal history is a private bounded journal, never proof of liveness

**Status:** Accepted, implemented. Supersedes ADR-036's persistence prohibition while preserving its
lifecycle honesty. `turn-pty::journal`, `turnd::core::restore`, `turn-proto::cells::Scrollback`.

### Context

Reopening the UI against a live daemon already restores a Pane, but restarting the daemon discarded the
terminal even though Session and process metadata survived. That loses the visible result of long-running
work, colours, cursor, modes and scrollback precisely when recovery matters most. The old answer in ADR-036
avoided a misleading transcript by deleting it; the product now requires retaining the display while making
the independent process-lifecycle truth unmistakable.

Terminal output is also hostile and sensitive. It may contain credentials, source, prompts, OSC sequences
and arbitrary binary fragments. Redaction cannot safely transform a VT byte stream while preserving its
meaning, and an unbounded transcript would turn a chatty build into a disk-exhaustion primitive.

### Decision

Each persistent Pane process has a private directory at
`<data-dir>/terminal-history/<session-id>/<node-id>`. `turn-pty` writes the authoritative PTY read before
broadcasting it to clients. The directory hierarchy is `0700`, files are `0600`, and creation/recovery
refuses symlinked components or files.

The durable representation is an atomic checkpoint plus an append-only binary journal. Output and resize
records carry a monotonic sequence, length and CRC32. Recovery applies only complete, ordered, valid records
and truncates a partial or corrupt tail to the last valid boundary. A checkpoint rename may race a crash
before journal reset; sequence numbers make the old prefix idempotent.

The journal is capped at 8 MiB and the checkpoint payload at 4 MiB per Pane. Rotation checkpoints the
visible grid, cursor, alternate-screen flag and input modes, resets the journal and marks older scrollback as
truncated. Before rotation, replay reconstructs the parser's bounded 5,000-row scrollback exactly; after
rotation it reconstructs the retained terminal state and says that earlier rows were discarded. The steady
state is therefore bounded and never silently presents a partial record as complete.

Cell attachments carry the newest validated scrollback as the same compact styled cell runs used by the
screen. Transport is capped at 5,000 rows and 3 MiB inside the protocol's 8 MiB frame; the UI seeds its
Transcript from those rows and continues extending it from live updates.

Recovered terminal state is display-only. The process remains `Orphaned` or `Lost`; history never produces
`Alive`, `Reconnected` or a writable PTY. The implemented v0.1 `RelaunchNode` creates a new node and deletes
the retired node's archive. ADR-059 narrowly supersedes that replacement target: restart/resume will create
a fenced RuntimeAttempt under the same Node/AgentInstance, while a fresh unverified conversation creates a
new Node/AgentInstance. The attempt-aware journal migration must retain old display history without ever
claiming it is the current writable runtime. Unknown session/node directories are pruned on restore.
Archiving or closing without deletion retains the archive with the Session.

Raw terminal history is enabled for persistent Sessions by default because it is the feature being promised.
A sensitive Workspace/Session can set `TURN_TERMINAL_HISTORY=disabled` (also `0`, `false`, `off` or `no`)
before launch; then no archive is created and any old Session archive is removed at restore. `--no-persist`
also disables it. There is no content redaction: opt-out, owner-only permissions and hard byte limits are the
security boundary.

### Consequences

- Closing and reopening the UI or restarting `turnd` can reconstruct the terminal's visible state and
  retained scrollback without claiming that the old process survived.
- A torn final write, checkpoint/reset crash window and journal rotation have reproducible tests.
- Disk use is calculably bounded per Pane, and terminal files are not mixed into SQLite/WAL or semantic logs.
- **Downside:** terminal archives intentionally contain unredacted output and can contain secrets. Users must
  opt sensitive Sessions out before launch; retrospective redaction is not meaningful for VT bytes.
- **Downside:** rotation compacts to the current terminal state and discards older scrollback. The UI marks
  that boundary rather than offering infinite retention.
- **Downside:** writes add local disk I/O on the PTY reader thread. The bounded format favours simple crash
  recovery over maximum throughput; batching/sync policy should be changed only with durability benchmarks.

---

<a id="adr-045"></a>
## ADR-045 — Turn never writes its own words into a program's screen

**Status:** Accepted, implemented for inline-image refusals, which is the only place Turn was doing it.

### Context

A pane refused to draw a picture — a Kitty transmission naming a file on disk, which Turn will not open —
and explained itself by **feeding the sentence to the terminal parser**, at the cursor, with a newline
after it. The intent was right and is unchanged: a picture that silently did not appear is a bug report
nobody can write.

The surface was wrong, and it showed up as soon as a real agent ran in a real pane. Codex printed a line
about an interrupted MCP server; the sentence landed in the middle of it; the line wrapped; the row below
shifted by one. Turn had corrupted output that had nothing to do with pictures.

This is not a bug in the notice's wording or its length. A program's screen is the program's: it computed
a layout, it addresses its cursor absolutely, and it repaints. Anything Turn inserts is therefore
permanent — the program will never overwrite it, because the program does not know it is there — and every
row after it is off by one. The same reasoning already forbids inventing an exit code (ADR-010). This is
the display equivalent: Turn does not put words in a program's mouth.

### Alternatives considered

**Make the notice shorter, or draw it without a newline.** Rejected. Both reduce how much of the screen is
corrupted without changing that it is corrupted. A one-character marker in the wrong cell still moves the
program's text.

**Say nothing and count it.** Rejected for the reason the notice existed: the user watched something not
appear, and a counter they never look at does not tell them why.

**Reserve the cells the picture would have occupied and leave them blank.** Considered seriously, because
it is the least surprising thing for a program's layout. Rejected as the *only* answer: it is silent, and
for a refused file transmission Turn does not know the intended box in the first place.

**Keep the sentence, move it out of the grid.** Chosen.

### Decision

Refusals are recorded against the pane rather than drawn into it, as a bounded table of sentences with a
count each, and they travel to the client *beside* the grid — a field of `Grid`, checked on arrival like
every other field, never cells. The client shows them in a strip across the bottom of the pane, in the
same register as the find bar, which is Turn's furniture and Turn's to write in.

The strip can be dismissed, because while it is up it covers a row of the program's output and nothing
Turn has to say about a pane outranks the pane. Dismissal is recorded against *how much* the pane had
refused, not as a boolean, so putting away the first explanation does not silence the second. Clicking it
is not also a click in the pane, so dismissing a sentence does not move the program's cursor.

Identical refusals are counted rather than repeated, and the table is capped at eight distinct sentences,
so a program in a loop cannot grow it.

### Consequences

- A refused picture leaves the program's screen byte for byte what the program wrote, which is asserted
  directly: the same output with and without the refused sequence produces the same text and the same
  cursor.
- The explanation can no longer scroll away, since it is not on the screen that scrolls. The test that
  used to write five pictures hoping the notice would still be visible now just reads it.
- **Downside:** the strip covers the pane's bottom row while it is up. That is a row of the program's
  output hidden, which is recoverable, rather than a row overwritten, which is not.
- **Downside:** a ninth distinct kind of refusal does not reopen a dismissed strip, because the pane
  tracks eight and the ninth changes no count.
- **Downside:** Turn still shows nothing where the picture would have been, so a program whose layout
  assumed a picture occupies rows is laid out for a screen Turn did not draw. Honouring the box would need
  the size the sequence asked for, which a refused file transmission does not carry.

---

<a id="adr-053"></a>
## ADR-053 — The control socket admits only the owner with a per-generation capability and bounded load

**Status:** Accepted, implemented. `turnd::server::security`, protocol v4 and the native GUI transport.

### Context

The daemon control socket can read terminal history, write to every live PTY and terminate processes. File
mode `0600` and bounded frames were useful but incomplete: the accept loop did not verify kernel peer
credentials, distinguish Turn clients from any other local connector, cap open connections or limit request
frequency. A process could therefore exhaust descriptors or hold tasks, and an accidentally compatible
client could gain full terminal authority.

The hook HTTP listener is a different boundary. It accepts untrusted agent events on loopback under per-node
tokens and can never issue terminal commands. Sharing its listener, tokens or admission policy with the
control socket would turn a narrow event capability into daemon authority.

### Alternatives considered

**Rely on socket mode and UID alone.** Rejected. Kernel credentials are still checked because modes can be
misconfigured, but UID identifies an account, not an authorised client or daemon generation.

**Use one static token in the data directory.** Rejected. A copied token would never expire and could be
replayed after restart. It would also become another durable credential to migrate and back up.

**Put authentication on the hook HTTP server and route control through it.** Rejected. HTTP parsing and
agent-visible per-node tokens belong to a deliberately write-only semantic event surface, not to PTY control.

**Use an ephemeral capability plus kernel credentials and bounded per-client admission.** Chosen.

### Decision

Every accepted Unix stream is checked with `peer_cred`; only the daemon effective UID proceeds. The socket
remains `0600`. After binding, each daemon generation atomically publishes a fresh 244-bit capability as the
owner-only regular file `<socket>.token`. Creation is symlink-refusing, `0600`, synced and atomically renamed.
Shutdown revokes it before persistence and removes only a file whose contents still belong to that generation.

Protocol v4 adds `auth_token` to the opening `hello`. Missing, stale or invalid capabilities receive the same
fatal `unauthorized` rejection before the client is registered or any request reaches Core. Comparison is
constant-work over the expected secret, token-bearing types redact `Debug`, and rejection logs contain only
client labels/counters. The GUI re-reads the token for every connection attempt, so daemon restart rotates the
credential without application restart. The unauthenticated instance probe may identify a `rejected` peer as
Turn but cannot read or mutate state.

Admission is hard-bounded at 32 concurrent control connections with a five-second pre-authentication timeout.
Each authenticated connection has a token bucket of 256 frames and 128 frames/second; over-budget frames get
`rate_limited`, and 16 consecutive violations close the stream. Per-client output and Core queues remain
bounded. A writer that cannot finish after its reader closes is aborted after one second, so a non-reading
peer cannot retain its descriptor and permit indefinitely. Aggregate counters expose UID, capacity, auth,
timeout and rate-limit rejection counts without payloads or secrets.

The hook server remains a separate `127.0.0.1` HTTP listener with independent per-node tokens, limits and
statistics. No IPC capability is placed in a hook URL or agent configuration.

### Consequences

- Wrong-account peers fail at the kernel-credential boundary; missing or replayed generation tokens fail the
  protocol handshake before gaining PTY authority.
- Two valid windows can connect concurrently, while connection and request storms have calculable descriptor,
  task and queue bounds.
- Restart is explicit revocation: an old capability cannot authenticate to the replacement daemon.
- Authentication changes existing handshake authority, so the supported protocol window is v4 only.
- **Downside:** this is not a sandbox against malicious code running as the same user. Such code can read the
  owner-only token just as it can read the user's credentials; process isolation needs a separate OS sandbox.
- **Downside:** a same-user process can still deny service by repeatedly occupying bounded slots. Integrity and
  resource bounds are preserved, but availability against the account owner is not promised.

---

<a id="adr-046"></a>
## ADR-046 — Archive, close and delete are three verbs, and delete forgets only Turn's record

**Status:** Implemented v0.1 history. The three distinct user intents remain, but ADR-047/050/057/066
supersede the generic table and unconditional erasure below: target archive/detach/End/Stop-all/delete have
separate rows, and End/delete uses a total daemon-derived semantic-survivor reduction before authoritative cleanup.

### Context

A Session could be archived, which hides it and stops nothing, or closed, which stops its
processes and leaves it in the tree as `Paused`. Neither gets rid of it. The record stayed, the
row came back the moment archived rows were shown, and there was no answer to "I am done with
this, take it away" — reported, exactly, as *"no hay manera"*.

The reason it had been left out is worth naming, because it is a real fear rather than an
oversight: a Session names a checkout, a branch and sometimes a worktree, and a "delete" that
was vague about which of those it meant would be a destructive action on somebody's work.

### Alternatives considered

**Hide archived rows harder.** Rejected. It answers "off my screen" and not "gone"; the record
still accumulates, and a user who archived something to be rid of it has been told a small lie.

**Delete the checkout too, or offer it as a checkbox.** Rejected outright. Turn does not own
that directory — the user chose it, other tools use it, and it may hold work no one else has a
copy of. A terminal multiplexer that can delete a repository is not a tool anybody should leave
running.

**One verb with flags** (`close(delete: true)`). Rejected. The two ask different questions and
the answer to one is not the answer to the other. A flag on a dialog is also the shape most
likely to be clicked through.

### Decision

A third verb, distinct from both, and its promise is stated in one line: **delete forgets
Turn's record and nothing else.**

| | Stops processes | Leaves the tree | Record kept | Reversible |
| --- | --- | --- | --- | --- |
| Archive | no | yes | yes | yes |
| Close | yes | no | yes | the work is not |
| Delete | yes | yes | **no** | **no** |

**Historical v0.1 deletion list, superseded for the target:** what was deleted was the row, the layout, the process tree, the event log, the attention entries,
the activity previews, the pane bindings, the write lease, the scratch directory, and the
per-window tree state. A Workspace takes its Sessions with it, deleted one at a time rather
than by database cascade, because each has processes to stop and clients to detach and neither
is the schema's job.

What is not deleted: anything on the user's disk. The dialog therefore **names the checkout
path verbatim** — "/Users/x/personal-workspace/turn stays exactly as it is" — because a promise
about "your files" is not checkable by the person reading it and a path is.

`KeepProcesses` is refused. Forgetting a Session while its processes run would leave them alive
with nothing left that names them: not in the tree, not in the store, not in a pane. That is a
leak the user cannot see, let alone fix.

**Target vNext supersession:** replaying the same close/delete operation returns the identical durable
`Closed`; a new operation against the exact tombstone returns `Closed(already_closed)` with its original
disposition reference. Generic `Ack` is only the historical v0.1 answer and is forbidden for these four target
operations because it would lose the typed survivor result.

It is offered on the row's context menu and in the command palette, not as a fourth button on
the row. A row that carries four controls, one of them irreversible and one click from the
name, is worse than a row of three and a menu — and the palette is the surface where nothing is
hidden.

### Consequences

- An archived Session can be deleted, and that is deliberate: a filed-away row is the likeliest
  thing somebody clearing out their tree wants gone, and refusing there would leave it with no
  way out at all.
- `AttentionManager` gained `forget_session`, distinct from `engage_session`: engaging means a
  demand was *met* and keeps the session's runtime, while forgetting means the subject is gone
  and drops the mute deadline with it. Without it, `Next attention` could move focus to a
  Session that is not in the tree.
- The store's deletes now also clear `tree_ui_state`, which has no foreign key to cascade
  through. A test walks the *schema* — every table with a `session_id` column — rather than a
  list written by hand, so a table added later without a cascade fails the day it is added.
- **Downside:** there is no undo and no trash. A deleted Session's history is gone, and the
  only protection is the dialog. That is the cost of the promise being simple enough to state.
- **Downside:** deleting a Workspace with many Sessions is a long operation that stops each
  one's processes in turn, and the window has one acknowledgement to show for all of it.

---

<a id="adr-054"></a>
## ADR-054 — Read-only Sessions use an inherited macOS checkout write guard and fail closed elsewhere

**Status:** Accepted, implemented macOS-first for guarded read-only inspection. Explicit promotion to a
writable `main_checkout` is decode-only history superseded by ADR-065.

### Context

The safe alternative to a busy primary checkout was honest but inert: Turn persisted a read-only Session
with `read_only_enforced=false` and refused to launch its shell or Agents. Telling a model not to edit files
cannot turn that metadata into enforcement. A usable reviewer needs Git reads, searches and ordinary terminal
execution while the existing writer keeps the sole checkout lease, and the same boundary must reach child
processes, alternate working directories and pathname aliases.

### Alternatives considered

**Rely on prompts, read-only tool lists or command classification.** Rejected. A shell, Agent or child can
open files directly, and command spelling is not authority.

**Change checkout permissions while a read-only Session runs.** Rejected. Permissions are shared with the
legitimate writer and changing them is itself a race-prone global side effect.

**Copy or create a worktree for every reviewer.** Rejected as the meaning of read-only mode. It gives a
different filesystem snapshot and Git index, while `isolated_worktree` already exists for that trade-off.

**Wrap every process in a parameterised macOS Seatbelt policy and fail closed without it.** Chosen.

### Decision

`ReadOnlySandbox::for_checkout` first requires the fixed system `sandbox-exec`, then canonicalises the
checkout and resolves its Git directory and common directory. Git metadata outside the checkout is protected
too. The Seatbelt source allows normal execution and denies `file-write*` for a literal and subpath matcher
for every protected root. Canonical paths are passed as `-D` parameters rather than interpolated into policy
source.

The sandbox wraps the original command inside `ProcessSpec`; it therefore covers shells, recognised or
generic Agents, init commands, relaunches, splits and every descendant they create. Guarded processes receive
`TURN_READ_ONLY=1`, the canonical root and `GIT_OPTIONAL_LOCKS=0`. Cwd containment is still checked
independently. Every launch reconstructs the guard instead of trusting persisted `read_only_enforced`.

Creation persists `read_only_enforced=true` and materialises the Layout only when that construction succeeds.
On another platform or with a missing launcher, the Session remains visible and launches nothing; unsafe or
unresolvable protected metadata rejects creation rather than persisting an ambiguous boundary. It never takes
the primary write lease. The hierarchy, Session header, status bar and accessibility label state whether the
guard is enforced or unavailable, and Seatbelt denials remain visible in the terminal.

**Historical v0.1 clause, superseded by ADR-065:** write escalation reused the durable lease arbiter and
could promote a stopped reviewer to `main_checkout`. The target never promotes to primary; write escalation
provisions a dedicated worktree and changes authority only after that exact worktree is proved and fenced.

### Consequences

- A reviewer shell or Agent can run against the current checkout while another Session owns its write lease.
- Reproducible macOS tests block create, modify, delete and rename attempts from an alternate cwd, a symlink
  alias and a child process, while preserving Git reads and writes outside the protected roots.
- External gitfile/worktree metadata is covered instead of assuming `.git` lives below the checkout.
- Unsupported platforms keep the previous fail-closed behaviour and expose it instead of pretending that
  read-only metadata confines a process.
- **Downside:** Seatbelt is macOS-specific; Linux needs a separately audited inherited boundary before it may
  set enforcement true.
- **Downside:** this is path-scoped write protection, not full process isolation. Credentials, network,
  services and unprotected filesystem paths remain accessible, and the UI/docs must say so.

---

<a id="adr-047"></a>
## ADR-047 — Ending a Session takes its row out of the tree; a Workspace is a project and stays

**Status:** Implemented v0.1 navigation history. The rule that End removes the Session row while Stop-all
keeps the Workspace is retained. ADR-066 extends it with a total precommit semantic rehome/tombstone/recovery reduction and
recovery inventory; ADR-065 makes every primary-writer/free-for-next-writer clause decode-only history.

### Context

"End session" stopped every process and set the Session to `Paused`, leaving its row in the tree.
Reported as the Sessions "sitting there laughing at me every time I end them", and that is a fair
description: the row stayed, in the same place, with the same shape, next to rows that were live.
The only signal was a count going to zero. So the verb looked like it had done nothing, and a
tree in daily use filled with rows its owner had already finished with.

ADR-046 added `delete` and answered a different question — how to get rid of something for
good — while leaving the visible button doing the thing that was reported. The user asked twice
for Sessions to leave the interface, and got a third verb in a context menu.

### Alternatives considered

**Leave it and rely on Delete.** Rejected. Delete is for "forget this ever existed"; ending a
task is ordinary and happens many times a day. Making the ordinary act reach for the irreversible
one is not a design, it is a workaround.

**Keep the row and make it look ended** — greyed out, or in a collapsed "Ended" group. Rejected
for now: it is more interface, and the request was for fewer rows rather than better-labelled
ones. It stays available if the archived list turns out to be a poor home.

**Delete on end.** Rejected. A Session that has just ended is exactly the one most likely to be
wanted back — the branch is still there, the task may not be finished.

### Decision

**Historical v0.1 mechanics:** Ending a Session archived it, stopped processes and released its write lease;
that old archived row could be restored. **Target contract:** ADR-066 retains only the navigation outcome.
End serially derives and commits every semantic-survivor disposition without another user action, permanently
revokes Session authority and removes the active Session row behind a minimal tombstone/recovery record.
Process and legacy lease cleanup occur afterwards and cannot erase survivor evidence or veto removal. End is
not Direct Archive and does not expose that Session row for restoration; Direct Archive remains the distinct
reversible hide-only verb, while starting again from retained artifacts creates an explicitly new Session.

Releasing the lease is part of it. A Session whose processes have stopped is not writing to the
checkout, and leaving the lease held meant the user had to find "Release write lease" in a menu
before they could start work in their own Workspace again — a second, obscure step for something
already finished. The release is quiet: a failure is logged, not returned, because the Session has
already ended and refusing the whole operation would leave the user with neither outcome.

**A Workspace does not follow.** Stopping every Session in a Workspace takes every *Session* row
out of the tree and leaves the Workspace's own row where it is. A Session is a task and finishing
it means it is over; a Workspace is a *project* — a directory the user comes back to — and filing
the project away because its last task stopped would mean restoring it before starting the next
one. The control is therefore renamed to what it does: **"Stop all sessions in X"**, not "Close
workspace". Getting the project itself out of the tree is `ArchiveWorkspace`, or
`DeleteWorkspace`, both on the same row.

Every lifecycle command's title now says two things, and a test enforces both: what happens to
the work, and what happens to the row. The absence of the second is what made this defect
invisible until it was used.

### Consequences

- Ending a Session does what the word says. The tree holds what is being worked on.
- **Historical v0.1 consequence:** an Ended Session could be brought back from the archived list. The target
  reserves that reversible behavior for explicit Direct Archive; End itself is terminal for the Session row.
- **Historical v0.1 consequence, superseded by ADR-065:** the write lease followed work in the primary
  checkout. Target Turn writers never acquire primary `main`; ending only cleans legacy/quarantined state.
- **Historical v0.1 downside:** an Ended Session was in the archived list. The target replaces that hybrid
  with two honest verbs: reversible Archive and authoritative End.
- `close_session(terminate|kill)` has one user-visible job—End—and commits it in one action.
- **Downside:** the asymmetry between a Session and a Workspace has to be learnt. It is carried by
  the control's name rather than by documentation, which is the only place it can be learnt from.

---

<a id="adr-048"></a>
## ADR-048 — The tree points at one worker: click a managed node to show its pane, its owner to restore

**Status:** Superseded by ADR-059. This remains the historical v0.1 implementation record; explicit Pane
zoom still exists, but a tree click is no longer the accepted post-v0.1 gesture for it.

### Context

An agent managing work has children in the tree: the subagents it reported through its hooks, and
the processes it started that ADR-019's scan found. Listing them is what makes it possible to see
what an agent is *doing*. What the tree could not do was **show** you one. A subagent has no pane:
it runs inside its parent's, four of them share one screen, and finding the one that mattered meant
reading output from all four.

The request was precise: an entry per subagent or process the agent is managing, clicking one
maximises it, clicking the parent or the Session restores the layout, and Turn should notice when
one has gone quiet and offer a quick way to stop it.

### Alternatives considered

**Give each subagent its own pane.** Rejected. A subagent is not a process with a terminal — it is
a conversation inside the agent's. There is no pty to attach, and inventing a pane for one would
mean inventing its output.

**Open a temporary pane per subagent** (`OpenNodeAsTemporaryPane` already exists). Rejected as the
default: it is the right answer for *inspecting* a node's details, and the wrong one for "show me
this worker" — it adds a surface rather than picking one of the ones already there.

**A separate maximise control on each row.** Rejected. The row already means "this worker", and a
control beside it that means "this worker, but bigger" is a second way to say the same thing. The
click is the gesture.

### Decision

Clicking a node an agent is **managing** maximises the pane it runs inside; clicking the thing that
**owns** it — its agent, its shell, its Session, its Workspace — puts the layout back.

*Managed* is decided by ancestry, not by kind: a hook-reported subagent is agentic and a `gh` the
agent started is not, and both are things the agent is managing. The pane is the nearest ancestor's,
walking up until one is bound to a pane, because the binding may be on the agent or on the shell and
assuming which would be wrong for one of them.

The whole decision is a value in `spotlight`, testable without a window, because the part worth
getting right is *which* pane.

`zoom_pane` toggles, which is right for the keyboard chord and wrong here: clicking two subagents
that share a pane would maximise and then un-maximise it. So the window sends the request only when
the toggle would land on the state it wants, compared against the layout **the daemon last
reported** rather than against an idea the window keeps for itself.

**Silence is reported, not acted on.** A managed node that has produced nothing for two minutes says
so on its own row, in place of the activity preview it would otherwise repeat — a preview two
minutes old is not news. It is a word on a row: nothing is stopped, nothing steals focus, and being
wrong about it costs a line of text. A *shell* is never called idle; it is waiting for the person at
the keyboard. Neither is a node that has ended: it is finished, not idle.

**One control on the row: stop.** Managed nodes carry it and nothing else does, decided by kind
rather than by state — a control that appeared only when Turn thought something was wrong would make
its absence a claim Turn cannot support. Stopping a shell or the agent itself stays in the context
menu, because that is a decision about the pane.

### Consequences

- The tree becomes the way to point at one worker among many, which is the job it was closest to.
- A maximised pane always has a way back: every ancestor of the row that maximised it restores.
- **Downside:** clicking a managed row now changes the layout, so a user selecting rows to read
  their state will see panes maximise. Clicking the owner is one click back, and the alternative —
  a modifier or a second control — would hide the feature behind something to learn.
- **Downside:** the idle threshold is a constant. Two minutes is defensible and not derived from
  anything about the agent; a worker that thinks for three minutes will be called quiet once.
- **Downside:** none of this reaches a Session whose agent Turn did not launch. Without hooks there
  are no reported subagents, only the OS children the scan finds, so a hand-started `claude` shows
  its processes and not its workers. That is the launch path's problem, not this one's.

---

<a id="adr-049"></a>
## ADR-049 — Coming back to a Session starts it; the daemon still starts nothing on its own

**Status:** Accepted, implemented; its blanket “daemon never sends itself a start request” clause is
superseded by ADR-061 for a persisted, still-valid reviewed Flow policy, and its v0.1 client fan-out is
refined by ADR-064 into one foreground, revision-fenced Session activation operation. Overturns the restore half of
"Turn never relaunches", which was a product rule of this project from its brief onwards.

### Context

Turn restored a Session's layout and started nothing. Every pane came back empty with a button in
the middle of it, and the user's Session — four panes — was four buttons. It was reported three
times: as "one click per pane, unusable", then again after the collective "Start all N panes" was
added, and finally as *"I don't want this, it has to start on its own"*.

The rule it came from is a good rule, and I defended it three times: **Turn never runs the user's
commands for them.** A restore that relaunched could re-run a deploy, a migration, a script with a
side effect, and the user would find out afterwards.

What the rule got wrong was treating every pane the same. A Session assembled from a template is
made of shells, agent panes and file browsers — things whose whole content is "run this again" —
and the layout model has said so since it was written: `RestoreBehaviour::Relaunch` means *running
this again is harmless*, and every built-in template sets it. The rule was being applied as though
that value did not exist.

### Alternatives considered

**Fewer clicks.** Tried, shipped, and rejected by the owner within the hour: the collective offer
made four clicks into one, and one was still one too many. The complaint was never about the count.

**Start everything with `can_relaunch`.** Rejected. That flag means "this *could* be started" — it
is true for a pane naming `npm run deploy` — and using it as permission is the failure mode the
original rule existed to prevent.

**Start in the daemon, at restore.** Rejected, and this is the substantive half of the decision.
The daemon restores when it starts, which includes after a crash and at boot with no window
attached. Relaunching thirty panes with nobody watching is how a person discovers that Turn ran
something.

### Decision

The daemon marks each pane `auto_start` when its `RestoreBehaviour` is `Relaunch` and it could be
started at all. It does not act on it: `RelaunchNode` remains the only request that starts
anything, and the daemon does not send itself requests. The executable form of that is unchanged
and still passing.

ADR-061 adds one narrow post-v0.1 exception: the daemon scheduler may issue the next typed step operation
when an immutable restored FlowRun policy, current result, budgets, authority generation and prior receipts
all prove it was authorised up front. It does not generalise Session activation or metadata restore, and an
ambiguous effect is never replayed.

ADR-064 preserves the human-foreground boundary and the per-pane `Relaunch` decision while replacing the
window's v0.1 loop of `RelaunchNode` requests. One explicit Session selection submits one idempotent activation
plan for the exact current bounded eligible descriptor set—or one commandless default Shell when empty.
The daemon preflights every descriptor and authority fence before the first effect; a stale, unsafe or
ambiguous member launches nothing and produces one exact recovery action. Background restore, child selection
and reconnect still submit no activation.

The **window** acts on it, when it receives the restore report — which is the moment a person has
opened Turn and asked for that Session. It starts every `auto_start` pane, once, without asking,
and holds back exactly two cases: a pane that would use the checkout while a write confirmation is
pending, and any pane at all while a process from the previous daemon is still alive and out of
reach, because a replacement running beside it would do the work twice.

`ReattachOnly` now means what it says for a pane naming a command: **do not run this one by
itself.** A commandless terminal is always restored as the user's configured shell, including
legacy layouts that predate `Relaunch`; an empty panel is not a useful safety property. No pane
shows an inline start button. A consequential stopped command remains available from the Process
row's contextual `Start again` action without turning every recovered layout into a form.

### Consequences

- Coming back to a Session is coming back to the Session. Nothing to click.
- The safety the original rule protected is now expressed where it can be reasoned about — per
  pane, in a value the template author or the user sets — rather than as a blanket refusal.
- The two moments are separated: a daemon restoring alone starts nothing; a window that a person
  is looking at starts what is safe.
- **Downside:** a command-bearing pane left on the default `ReattachOnly` does not start
  automatically, so a hand-built job behaves differently from a template's. The default is the
  cautious one; commandless terminal panes are exempt because their fallback is only a shell.
- **Historical v0.1 downside, superseded by ADR-064:** two windows attached to the same Session could both act
  on the report; the second request was refused and produced noise. The vNext operation id plus Session/
  surface revision makes duplicate activation return the same receipt.
- **Downside:** this is a product rule reversed under pressure from its owner, on the third
  report. The rule was defensible and the way it was applied was not, and the record should say
  that the owner was right and I was slow.

<a id="adr-050"></a>
## ADR-050 — Ending is authoritative: a process Turn cannot stop is reported, never a veto

**Status:** Implemented for post-commit process cleanup. ADR-066 requires a total daemon-derived semantic-
survivor reduction in that commit and preserves unmatched processes in target-wide recovery inventory; only later
process/worktree/artifact cleanup is authoritative best-effort.

### Context

Choosing "End session" on a Session restored after a daemon restart produced a red banner —
*"Turn cannot safely stop processes that survived the previous daemon"* — and the Session could not
be got rid of at all. Retrying did not help; the instruction was to go and stop the process outside
Turn first, and only then would Turn allow the user to close their own task.

The reasoning behind the refusal was sound as far as it went, and is worth keeping on the record
because the code was right about the danger and wrong about the remedy. A process restored from a
previous daemon has a PID-shaped observation and no owned handle. Signalling it blindly is unsafe
because PIDs are reused, and fabricating its exit would release the checkout write lease while the
real process may still be writing to the very files the Session was working on. Both are true. The
conclusion drawn from them — *therefore the Session may not end* — does not follow.

It does not follow because the refusal changes nothing about the danger. The survivor keeps running
whether Turn refuses or not. Nothing about holding the Session open stops it, warns about it, or
reaches it. The only thing the refusal preserved was the row in the tree, and the row is what the
user was trying to be rid of. So the safety was notional and the cost was total: the destructive
verb, which exists precisely for "I am done with this", stopped working exactly when the daemon had
had a bad day.

The same guard also blocked `close_workspace` and `delete_workspace`, both of which pre-validated
every Session before touching the first one. One unreachable process in the fourth Session refused
the whole Workspace.

### Alternatives considered

**Kill by pid anyway.** Rejected, and this is the part of the original reasoning that survives
intact. PID reuse means the signal may land on an unrelated process, and Turn would have chosen to
do that on the user's behalf.

**Fabricate the exit and release the lease.** Rejected for the reason the original comment gives: a
Session whose lease is released while a real process still writes to the checkout is the
lost-work scenario the lease exists to prevent. What *is* released is the lease of a Session whose
processes Turn did stop, which is the ordinary case.

**A second, scarier confirmation — "force end".** Rejected. Two destructive doors for one act, and
the user has to learn which one their situation is. Ending already asks once; the question can carry
one more sentence.

### Decision

Ending is authoritative. **The point of no return is one serial daemon transaction, not a second form.** It
derives a closed disposition for every current semantic subject, discards uncommitted Session-owned drafts,
revokes Session authority, uses valid predeclared rehomes and otherwise tombstones/retains independently live
or uncertain evidence in the exact semantic recovery inventory. Missing/stale client input cannot refuse End. Past it, `close_session` treats
everything as best-effort: a process it cannot signal, a lease row that will not update, a write
that will not land. Each is logged; none of them abandons the act. The Session is terminally tombstoned, its row
leaves the tree, and the two errors that remain are asking about a Session that does not exist and
`KeepProcesses`, which is not destructive and can afford to be strict.

What replaces the veto is an answer. `Response::Closed { escaped }` is a new result shape carrying
the processes Turn could not stop, each with its title and last-known pid, and it is what
`CloseSession`, `DeleteSession`, `CloseWorkspace` and `DeleteWorkspace` now return. The window says
it twice: `SessionSummary::orphaned_count` lets the confirmation dialog say what ending *will not*
achieve before the click, and the answer's `escaped` list names the survivors with their pids
afterwards, because the user's next step is a process list in another terminal.

Nothing claims the orphan died. Its `Lifecycle::Orphaned` is untouched, which is the half of the
original reasoning that was never in question.

### Consequences

- The destructive verb works when the daemon has had a bad day, which is when it is needed most.
- A user who ends such a Session is told, in the dialog and again afterwards, that a process may
  still be running and how to find it.
- **Historical downside, superseded by ADR-063/066:** the v0.1 tree had no place for the survivor. Target
  RuntimeInventory retains it as an unmatched/recovery subject with its exact last evidence.
- **Historical downside, superseded by ADR-065:** v0.1 could release a primary lease while a writer
  survived. Target has zero Turn-owned primary writers; non-primary survivors retain recovery/lock evidence.
- **Downside:** `Response::Ack` no longer answers the close and delete requests, so any client
  matching on it for those four sees an unhandled shape rather than a compile error.

---

<a id="adr-055"></a>
## ADR-055 — Checkout write authority is a host-global inode lock inherited by every writer

**Status:** Accepted, implemented on Unix as legacy exclusion and still normative for non-primary checkout
ownership. ADR-065 supersedes steady-state primary writers: primary lock/lease rows are decode,
quarantine and cleanup evidence only; every target writer owns a dedicated worktree.

### Context

ADR-040's canonical-path fence was global only inside one SQLite database. Two otherwise correct Turn
daemons configured with different data directories could each create a lease for the same checkout. A lock
held only by the daemon would close one split-brain path but reopen it after a daemon crash even when a shell
or descendant survived and could still write the checkout. Git worktrees also share a common Git directory,
so repository identity is too broad: different worktree directories must remain independent write domains.

### Alternatives considered

**Put every installation in one registry database.** Rejected. It introduces another durable authority and
migration lifecycle merely to join stores, and still needs a crash-safe kernel boundary.

**Key a lock by canonical path text or Git common directory.** Rejected. Path aliases and renames can split
text identity, while a common Git directory falsely merges distinct worktrees.

**Hold an advisory lock only in the daemon.** Rejected. A surviving descendant would lose its exclusion at
exactly the moment Turn lost the PTY handle and had least evidence that writing had stopped.

**Use a uid-scoped filesystem-identity lock and inherit its descriptor into writers.** Chosen.

### Decision

Turn canonicalises the checkout directory and identifies it by Unix device/inode. Under the real,
uid-owned 0700 `checkout-locks` directory below Turn's stable platform data directory (independent of
`TURN_DATA_DIR`) it opens a retained 0600 lock inode with
`O_NOFOLLOW`, verifies type, owner and named/open inode identity, takes non-blocking exclusive `flock`, then
re-resolves the checkout to reject replacement during acquisition. Lock files are never unlinked. Owner
metadata is an atomically replaced sidecar containing daemon/data-dir identity, the lease and its typed owner.

The host lock is acquired before SQLite. The daemon preallocates one `LeaseId`, publishes it in the host
owner record and supplies it to the atomic Session/lease transaction. A local SQLite owner still returns the
existing focus/read-only/worktree/cancel choices. An owner from another daemon returns the same typed conflict
without `focus_owner`, because the current socket cannot focus it. Heartbeat, init, Pane launch, split and
relaunch require both the active SQLite generation and the matching host lock.

**Historical v0.1 primary-writer mechanism, unreachable for target launches:** for each main-checkout spawn the daemon duplicated only that lock descriptor with `FD_CLOEXEC` still set.
portable-pty normally closes every descriptor above stderr in `pre_exec`; Turn carries a small vendored
extension that preserves an explicit descriptor allowlist, clears `FD_CLOEXEC` only in the already-forked
child, and retains cleanup for all others. This avoids a cross-thread inheritance window in the daemon. The
launched process and its descendants therefore share the same locked open-file description.
Neither the lock type nor Drop calls `LOCK_UN`: explicit release first demotes SQLite and then closes the
daemon copy, while daemon loss leaves a surviving writer's copy authoritative until the kernel closes the
last descriptor.

On restart, SQLite remains `recovery_required`. Reclaim first reconciles the old process evidence and must
then reacquire the host lock with the existing lease id; time alone never permits takeover. Symlink aliases
collide, whereas distinct checkout/worktree directory inodes may write concurrently.

### Consequences

- Cooperating daemons for the same uid cannot both own one checkout even with separate data directories.
- Contention returns the remote Session/lease owner and locally actionable alternatives without partial
  Session, init-command or process side effects.
- A daemon crash cannot release authority while a descendant that inherited it is still alive; the final
  process exit makes recovery possible without a stale-file deletion protocol.
- Stable lock inodes and atomic owner sidecars avoid unlink races and partial heartbeat metadata.
- The stable data location is not subject to normal `/tmp` or runtime-directory cleanup; deliberately
  removing owner-owned lock files remains outside the cooperative same-user boundary.
- **Downside:** portable-pty is vendored for a narrow descriptor-preservation API until upstream provides it.
- **Downside:** `flock` is an advisory boundary between cooperating same-user processes. Code running as the
  account owner can close its inherited descriptor or tamper with owner-owned lock state; this is not a
  sandbox against malicious same-user code.
- **Downside:** unsupported platforms fail closed for main-checkout authority rather than silently falling
  back to SQLite-only exclusion.

---

<a id="adr-056"></a>
## ADR-056 — A compatible app update replaces files, never the live daemon

**Status:** Accepted and implemented for macOS packaging, release channels and update preflight.

### Context

The product is deliberately split: `turnd` owns PTYs and the window does not. A conventional app updater
that terminates every executable shipped in `Turn.app` would erase that advantage at release time. Merely
copying a new bundle is not enough either: an already-running daemon continues executing its old mapped
image, so the new UI must know whether it can speak to it. Saved Session state cannot answer whether a PTY
handle is still alive, and process-table guesses are not authority.

### Alternatives considered

**Stop `turnd` whenever `Turn.app` is replaced.** Rejected. It kills the PTYs the daemon exists to preserve,
including work the user may not be watching.

**Replace the bundle and hope protocol compatibility holds.** Rejected. A partially compatible UI can draw
a plausible terminal while silently misunderstanding hierarchy or control state.

**Infer liveness from saved lifecycle labels or `ps`.** Rejected. Stored `Alive` is downgraded on restart,
and an observed pid does not prove ownership of a PTY handle.

**Make the daemon self-update.** Rejected for this milestone. Replacing the process which owns PTY masters
requires descriptor transfer or a persistent multiplexer; exec/restart alone cannot preserve them.

### Decision

Every packaged component exposes machine-readable `--build-info`. Packaging requires one semantic version
for `turn`, `turnd` and `turn-hook`, and the UI/daemon protocol windows must be identical before anything is
signed. `Turn.app` carries a sealed `release.plist`; signed companion hashes prevent a mixed helper set.
Developer ID releases use hardened runtime, notarization, stapling and Gatekeeper verification on both
native macOS architectures.

The authenticated protocol adds read-only `get_update_status`. Its `active_ptys` is counted from live
`PtyProcess` handles inside the daemon. The updater intersects the new release's protocol window with the
daemon's:

- overlap: atomically replace the bundle, leave `turnd` and every PTY alone, and let the user reopen only
  the UI when convenient;
- no overlap plus active PTYs: defer and replace nothing;
- no overlap plus no active PTYs: require an explicit daemon stop, then retry.

The installer itself has no stop or kill path. It authenticates the daemon query, verifies the channel
checksum, notarized bundle, bundle id and signing team, rejects downgrades, stages beside the destination and
rolls back a failed rename. The new on-disk daemon is used only after the old daemon has ended normally.

### Consequences

- UI updates preserve live work whenever the protocol contract says they can.
- An incompatible binary is refused with the existing directional recovery message instead of half working.
- Release tags produce architecture-specific stable manifests and archives through `gh release create`.
- **Downside:** an incompatible update may wait indefinitely for long-lived PTYs; Turn chooses work
  preservation over urgency.
- **Downside:** an idle incompatible daemon still needs a separate explicit stop. This is one extra action,
  retained so an installer never learns to terminate runtime state silently.
- **Downside:** preserving PTYs across a genuinely incompatible daemon update remains impossible without a
  future descriptor-transfer or external multiplexer design.

---

<a id="adr-057"></a>
## ADR-057 — Local-data control is complete, inspectable and daemon-authoritative

**Status:** Accepted and implemented for the base inventory. ADR-063/064/066/067 extend its closed scopes and
supersede unconditional cascade. Turn-container Session/Workspace End/delete is total: independently live or
uncertain semantic records are rehomed, tombstoned or moved to bounded recovery and can never veto active-row
removal. Refusal remains valid only for a separately scoped provider/user-data or privacy deletion whose own
live-reference/consequence contract requires it. Exact links/grants/Attention and in-flight effects are fenced
before either kind of removal.

### Context

SQLite was bounded and redacted, terminal journals were private, and scratch configuration was removed on
normal lifecycle paths, but those separate properties did not let a user answer “what does Turn have?”,
export it for review, delete one ownership boundary, or prove a complete installation purge. A best-effort
list would become false as soon as a migration or new journal format landed. Deleting an open database from
the daemon would also race its own SQLite connection and any concurrent process.

### Decision

The domain defines one typed `PrivacyScope` and report/export/deletion vocabulary. The store maintains a
closed catalogue of every SQLite table and fails export when the schema contains an undeclared table. The
daemon adds filesystem metadata; payloads likely to contain terminal output, logs or injected configuration
are never copied into exports. Unknown future data-directory entries are explicitly inventoried. Every
export row carries origin, type, nullable timestamp, size and redacted content.

Authenticated daemon requests implement report, create-new owner-only export, Workspace/Session/Agent
deletion and compaction. The Settings hierarchy owns bounded Event, Preview, terminal-history and diagnostic
log policy; writes enforce it immediately and a periodic pass prevents drift. There is no telemetry
transport, and reports say so explicitly.

ADR-059 extends this closed-catalogue obligation before its schema migration may open: AgentInstances,
RuntimeAttempts, safe launch/resume and configuration-transition receipts, ContextLinks and broker-read audit, lineage, ContextPacket
metadata, ContextScope/QuotaScope samples, AgentMessage hash/delivery metadata, DependencyEdge/results,
Team roles/policy, safe RuntimeEndpoint fingerprints and resource-node records must all be classified. Account
references, provider conversation ids, host/cwd/worktree, link/message endpoints and WebPreview URLs are sensitive
metadata with policy-aware redacted export. Agent/Session/Workspace deletion first revokes live link/broker
capabilities, then applies a closed per-subject rehome/tombstone/retain-or-refuse plan. Only records proved
exclusively owned and terminal may cascade; independently live jobs, intents, deliveries, dependency/Team/
Attention evidence and runtime survivors retain exact receipts and routes. Their exact live-versus-history
limits are configurable and the privacy report/export/delete/
compact tests must fail if any new table or file category is omitted.

Installation purge is a separate `turnd --delete-installation-data` mode. It acquires the canonical
data-directory lock before touching files, follows no symlink, removes known and unclassified Turn-owned
entries, and retains only the stable lock inode and `worktrees/` user work.

### Consequences

- A migration or new filesystem root cannot be silently absent from a reviewable inventory.
- Selective operations remain serialized through the daemon that owns SQLite and PTYs.
- Full deletion deterministically refuses a live daemon and has a reproducible binary-level test.
- Terminal/log payloads are not included in export; review shows their existence, timestamp and size instead.
- Turning history back on affects newly launched Panes because a process spawned without a journal has no
  background journal writer to activate safely.

---

<a id="adr-058"></a>
## ADR-058 — Functional v0.1.0 has one evidence-backed release gate

**Status:** Accepted and implemented.

### Context

Issues #1–#20 delivered the consolidated MVP backlog, but the repository still described completed live
Claude and performance acceptance as pending in some places. Conversely, treating a functional MVP as a
broadly signed-off public release would overclaim Developer ID publication, Linux distribution, packaged
assistive-technology coverage and PTY survival after daemon death. A tracking issue is not evidence by
itself, and a pile of focused targets is easy to run selectively.

### Decision

`make mvp-acceptance` is the serial functional-v0.1.0 gate. It runs the complete format/lint/test suite and
adds the production-shaped performance, privacy and package/update proofs not fully reproduced by the
ordinary workspace tests. `docs/MVP_ACCEPTANCE.md` maps every global completion criterion to automated or
recorded manual evidence, including the authenticated packaged Claude Code run.

The gate is deliberately named **functional**, not public-release or platform-complete. Daemon-crash PTY
survival, the first credentialed notarized tag, Linux archive/update distribution and reviewed GPU baselines,
broad packaged VoiceOver/Orca/current-IME sign-off, tmux and the other declared post-MVP platforms remain
visible exclusions. An unchecked post-MVP row cannot be used to imply that the implemented MVP is partial,
and a checked functional row cannot be used to imply that those distribution claims passed.

### Consequences

- One command reproduces the code-owned v0.1.0 release decision without opening a desktop window.
- The live external-service proof remains a dated, versioned run record plus an opt-in repeatable harness;
  deterministic fixtures are never relabelled as authenticated evidence.
- `PRODUCT.md`, `ROADMAP.md` and `ARCHITECTURE.md` must distinguish completed functional scope from public
  distribution and broad platform sign-off.
- Future release criteria belong in the gate or its evidence map; closing another tracker without either is
  not sufficient proof.

---

<a id="adr-059"></a>
## ADR-059 — Agent nodes select one WorkSurface view and keep instance, context and attention identity separate

**Status:** Accepted, not yet implemented. Supersedes ADR-048's click-to-zoom interaction; extends ADR-040's
hierarchy/view split and ADR-043's context boundary; preserves ADR-044's Shell ownership; narrows ADR-049
auto-start to Session activation; and supersedes only ADR-052's relaunch-replaces-node target.

**Requirement scope:** `PRD-HIE-001..008`, `PRD-VIE-001..010`, `PRD-LIF-001..007`,
`PRD-RUN-001..009`, `PRD-CTX-001..011`, `PRD-OBS-001..007`, `PRD-ATT-001..010` and
`PRD-SAF-001..013`. The authority file expands and verifies every id individually.

### Context

The v0.1 hierarchy correctly made background AgentNodes independent from Panes, but its two inspection
paths do not satisfy the next use case. Quick Preview is intentionally a bounded status, while clicking a
managed child maximises the nearest ancestor Pane. Several semantic children can therefore show the same
terminal even though the operator selected different work. Relaunch also replaces `NodeId`, and the current
inspector cannot distinguish requested launch configuration, effective runtime facts, conversation context
and shared provider quota.

The product now needs persistent agent identity, provider-conversation continuity, unique semantic child
views, bounded pull context, one-shot work handoff, per-conversation context meters, account-scoped usage and
explicit attention state. Those capabilities must extend Turn's accepted hierarchy and authority model as a
coherent native domain, not as a second navigation metaphor or a loose set of UI widgets.

Most importantly, adding richer views must not dilute Turn's thesis. The job is still to route the operator
to the exact agent that needs a decision, permission, answer or result review. A dashboard that makes
agents observable but leaves the operator hunting for the actionable terminal would be a regression.

### Alternatives considered

**Adopt a canvas and treat each terminal/agent as a card.** Rejected. It duplicates the hierarchy with
spatial state, weakens keyboard scanning and makes the operator maintain a layout to discover attention.

**Keep the saved Session Layout visible and use the narrow inspector for all semantic content.** Rejected.
A subagent transcript, context packet, approval or stopped-agent history is primary content, not metadata
that can be compressed beside an unrelated terminal grid.

**Open or maximise a Pane for every tree selection.** Rejected. Semantic agents may have no PTY; shared
ancestor PTYs are not unique content; and selection must not rewrite a carefully chosen Layout.

**Treat a process, provider thread, Pane and Agent as one id.** Rejected. Their lifetimes diverge on warm
attach, cold resume, restart, branch, deletion and multi-view attachment. Reusing one id would either lose
history or falsely resurrect work.

**Copy entire transcripts automatically between linked agents.** Rejected. It spends context before the
target asks, creates an implicit disclosure boundary and makes revocation meaningless. It also turns
transient provider data into Turn-owned retention without necessity.

**Keep only the existing one-shot handoff.** Rejected. A reviewed snapshot is right for transferring work,
but wasteful when a collaborating agent needs one current fact and wrong for an ongoing, revocable read
relationship.

### Decision

Turn retains one Workspace → Session → optional explicit Group → Agent/Tool → Child tree. A Session remains
the owner of checkout/worktree, Layout and Attention policy; a Group is presentation inside it. The region to its
right is one `WorkSurface`. Selection chooses exactly one `WorkspaceView`, `SessionView` or typed `NodeView`;
it changes no saved Layout, process, terminal focus or Attention state. Selecting a Session restores its
unchanged Layout. The inspector becomes optional detail inside the active view. Explicit Pane/zoom commands
remain available and are no longer bound to a tree click.

Every agentic Node owns exactly one stable `AgentInstance`; non-agent nodes own none, and an instance never
moves to another Node. Each launch/resume or verified in-place model/configuration switch creates a
`RuntimeAttempt` epoch; warm view attachment selects the existing attempt. At most one attempt is current
under a generation fence; provider conversation/thread,
runtime/PTY and Pane ids remain
separate. Fresh/unverified conversations create a new Node/AgentInstance plus lineage. Immutable launch
specification and receipt records requested versus effective provider, model, account, permission/approval,
sandbox, safe flags, host, cwd and worktree facts with capability, source and fallback evidence. Unknown or
stale observations remain labelled as such. Conversation context consumption is separate from provider/
account quota, whose shared scope and reset window must be explicit.
Launch/resume epochs own launch receipts. A verified in-process switch owns a distinct configuration receipt
and atomically transfers the same conversation/process binding; it never fabricates another launch.

ADR-044 remains the local runtime ownership rule: Shell owns its PTY and Pane bindings, and Agent is its
confirmed child. A NodeView may project that terminal only through a verified RuntimeBinding while naming
the semantic Agent and the distinct input owner. Selecting an Agent is navigation and never launches or
resumes it. Warm visual attach is automatic because it creates no process; cold resume/restart is a typed
lifecycle operation, while ADR-049 auto-start remains limited to explicit connected-client Session
activation under a fully resolved persisted contract. No generic “Start pane” step gates agent content.

The daemon-owned global Attention Queue remains the only attention authority. An agentic entry carries a
validated Node/AgentInstance pair plus optional current attempt/generation/pending id and, when different, a
verified runtime interaction owner. vNext `route_attention` (successor to v4 `goto_attention`), badges,
notifications and governor-approved automatic Focus all apply one daemon-resolved, generation-checked
route. Exact subjects open their NodeView/action; authenticated parent/external or unassigned subjects open
an evidence view without inventing a Node/input owner. Navigation never acknowledges a demand. A result
becomes read only after its exact revision is primary content on a foreground surface. Submission stays
pending until evidence confirms resolution. Telemetry alone cannot move focus or reorder the queue.

Context uses separate typed edges:

- a directional, revocable `ContextLink` grants bounded pull access within one Workspace through a narrow,
  short-lived destination/current-attempt capability on a separate broker data plane—not the daemon control
  token;
- a reviewed `ContextPacket` transfers a bounded, sanitised snapshot with best-effort known-secret redaction
  and records handoff/branch lineage;
- neither edge reparents nodes, grants checkout writes, approves prompts or transfers execution control.

“Foreground operator only” is a supported authenticated-flow invariant, not isolation from a compromised
same-uid local process: without an active per-agent sandbox or UI-owned authority inaccessible to agents,
such a process may steal the administrative capability and impersonate the UI. The broker bearer still
provides narrower logical destination/attempt binding, replay resistance and separation from other OS users.

Provider-normalised transcript turns may contribute to a packet only under adapter capability and budget.
Older context is digested, recent complete turns are prioritised, most target context remains free for new
work, and optional detail stays behind a short-lived scoped pull reference shown in review. Preparation
creates only an expiring draft; new-target delivery is a fenced idempotent saga, not an external-process
transaction. Launch, broker-bearer installation and context write are separately journalled external effects:
an uncertain launch may only probe/adopt its preassigned identity, an uncertain install revokes that grant
generation and an uncertain write is never retried. Turn creates no dedicated durable semantic body
record, but delivered bytes may remain in provider transcripts, terminal screen/scrollback and ADR-052's
journal; review names that downstream retention and revocation cannot recall it. Durable records contain
provenance, hash, redaction/truncation facts and evidence-backed delivery state. `submitted`, `received`,
`read` and `acted` are never inferred from one another.

Short direct messages, dependency results and Teams are accepted, sequenced after context, and build on the
same stable instance ids. They are non-tree coordination edges, have no independent focus/attention authority
and cannot create hidden context access. A dependency result uses a bounded closed schema of state,
producer/revision ids, hashes/verified references, provenance and at most stripped/redacted summary; raw
runtime, transcript, diff/file, environment and provider payloads are invalid. ADR-061 supersedes this ADR's
original passive-only limit: outside a FlowRun it only projects ready/blocked/failure evidence, while an
immutable operator-reviewed Flow policy may advance it within an exact DelegationGrant. Permission approval,
unbounded scheduling and checkout-authority expansion remain out of scope. Session-owned Group, Note, File,
Diff and WebPreview resource nodes are accepted as M14 with explicit privacy/content-security acceptance; they own
no runtime or checkout authority and deletion never removes referenced user data. Initial WebPreview URLs forbid
userinfo/query/fragment, remain private content and connect only by pinning an approved public address after
validating every DNS answer for each request/redirect.

Every new durable category is subject to ADR-057 before migration: closed inventory, redacted/policy-aware
export, explicit count/byte bounds, revoke-before-delete cascade and report/export/delete/compact coverage.
An offline remote artifact remains `pending_purge` behind a bounded tombstone until authenticated exact-host/
generation purge proof; logical authority revocation never waits for that physical cleanup. Packet
bodies remain absent from Turn's semantic store, without making a false promise about recipient retention.

The complete domain, protocol target, failure behavior, delivery sequence, acceptance matrix and product
boundaries are normative in `docs/AGENT_NODE_VIEWS_AND_CONTEXT.md`.

### Consequences

- Every semantic agent, including a PTY-less subagent, can have unique actionable content without growing
  the saved Layout.
- One attention action crosses Workspace/Session boundaries, opens the exact subject and reaches its safe
  response control without erasing queue evidence.
- Restart, resume, model switch, branch and handoff preserve semantic history while exposing each concrete
  attempt and fallback honestly.
- Agents can share just-in-time context or transfer a bounded snapshot without confusing access, ownership
  and lineage.
- Runtime headers can answer which model/account/mode/host/worktree actually ran and whether usage data is
  current, unavailable or shared.
- **Downside:** every core layer needs new ids, migrations, view models and tests; this is not a GUI-only
  refactor and cannot be released honestly as a half-populated inspector.
- **Downside:** adapter capabilities will differ. A semantic transcript, receipt or quota may be unavailable
  for one provider, and the UI must carry that asymmetry rather than smoothing it into false parity.
- **Downside:** review-before-send remains one deliberate interaction at the context disclosure boundary.
  It is integrated into the NodeView, but minimising clicks does not justify invisible cross-agent data flow.
- **Downside:** the implemented ADR-048 click behavior must be removed while preserving explicit zoom and
  exact Layout restoration, which requires new native snapshots and keyboard/accessibility acceptance.

---

<a id="adr-060"></a>
## ADR-060 — Local dictation is a reviewed exact-target input path, never an attention or approval authority

**Status:** Accepted, not yet implemented. Depends on ADR-059's WorkSurface and stable instance/input identity;
preserves ADR-016/018's single Attention authority, ADR-045's program-owned screen boundary, ADR-051's
Settings hierarchy and ADR-057's closed local-data catalogue.

**Requirement scope:** `PRD-VOI-001..005`; expanded individually in the authority file.

### Context

Spoken prompt entry can remove a large amount of repetitive typing while an operator supervises several
Agents. It also combines unusually sharp risks: microphone capture, large third-party model files, native
inference code, stale Agent targets and text that may become executable as soon as it reaches a terminal.
Treating speech as “just another key” would bypass both Turn's exact identity model and the user's review
boundary. Treating it as an agent event or command source would undermine the product entirely.

The lowest-friction safe flow is still short: hold to speak, release into a focused inline draft, review and
press the normal send key. The extra boundary is not a modal or a mouse requirement; it is the point at which
the operator sees which Agent and real input owner will receive the text.

### Alternatives considered

**Insert into the PTY immediately after transcription, without Enter.** Rejected. A raw-mode program receives
those bytes even without a newline, so “not submitted” is not the same as “not disclosed”. It also makes a
stale-target race irreversible before the operator sees recognition errors.

**Run inference inside `turn-gui` or `turnd`.** Rejected. Native model parsers can hang, abort, exhaust memory
or crash on corrupt inputs. Dictation must not take down the window, daemon, Attention Queue or live PTYs.

**Use an operating-system/cloud speech service or silently fall back when local inference fails.** Rejected.
“Local” must name data placement, not merely the API invoked. PCM leaving the physical operator device is a
different product authority and needs a future explicit design.

**Interpret voice commands such as “approve”, “next agent” or “start”.** Rejected. Speech recognition is
probabilistic input. It cannot become a permission, lifecycle, focus or Attention authority.

### Decision

M15 adds operator-device-only dictation through one packaged local Whisper-compatible engine. It is disabled
with `input.dictation.model=none` by default and
downloads nothing until the operator explicitly selects an allowlisted model. The native client owns OS
microphone consent, bounded PCM and a memory-only inline draft. A sandboxed disposable `SpeechWorker`
receives only one verified read-only model descriptor and one bounded audio channel; it has no network,
daemon token, checkout, credentials or arbitrary filesystem access. Crash, hang, OOM and shutdown affect
only that worker and never replay partial work.

Starting capture snapshots an immutable exact `DictationTarget`: surface/connection/daemon generation,
Workspace/Session/Node, optional AgentInstance, RuntimeAttempt/generation, verified input owner and optional
pending free-text interaction revision. Selection, blur, Escape, device loss or surface close stops the mic
and never retargets. The transcript is control-stripped, newline-normalised, bounded and displayed in the
WorkSurface; zero bytes reach a PTY, adapter or provider before explicit **Insert** or **Send**. Permission,
credential/password, provisional/unassigned, raw-TTY and unverified alternate-screen targets are ineligible.

The protocol cannot start the microphone or request transcription. It exposes closed model install/list/
cancel/remove operations and generic `commit_operator_text` with exact target, expected input revision,
operation id and `insert|submit`. Every identity is revalidated immediately before the one fenced write;
possibly partial delivery is `submitted_unconfirmed` and is never retried automatically. Dictation origin
adds no confidence or capability. Questions stay pending until adapter evidence confirms the exact prompt
ended, and speech can never answer a permission.

Recording, transcription and draft review mark only their surface as `sensitive_operation`. Automatic Focus
defers with its original exact route; queue, badges, unread and notifications remain intact. Manual Attention
navigation wins and cancels live capture without redirecting a completed draft. Dictation itself never
creates, ranks, acknowledges, resolves or routes Attention and never activates a Session or runtime.

The daemon owns a signed, versioned closed model catalogue and model store. Every entry fixes HTTPS origin,
size, digest, engine compatibility, provenance and licence. Downloads are explicit, streaming-size-bounded,
owner-only, no-symlink, generation-fenced and atomically adopted only after full digest verification. Models,
receipts, partials and settings enter ADR-057; PCM, partial hypotheses and drafts never enter protocol,
SQLite/WAL, files, logs, journals, events, analytics, diagnostics or Turn crash reports. After explicit
delivery, normal terminal/provider retention applies and is shown before enablement.

The normative settings, protocol fields, failure behavior and adversarial/package acceptance matrix are in
`docs/LOCAL_VOICE_INPUT.md`.

### Consequences

- Dictation reduces typing while keeping the existing send action and exact target visible.
- Attention remains the single operator-routing authority even while voice input is active.
- A native speech-engine failure cannot take down supervision or live work.
- “Local audio” is testable: no PCM leaves the operator device or enters the daemon protocol.
- **Downside:** model downloads are large installation-owned data and require supply-chain, storage, licence,
  packaged-helper and cross-platform acceptance work.
- **Downside:** Turn deliberately keeps one review/send action and refuses dictation on inputs it cannot
  authenticate, so it will expose fewer voice affordances than a general OS dictation feature.

---

<a id="adr-061"></a>
## ADR-061 — Turn is an operator control plane with reusable Flows and bounded delegated control

**Status:** Accepted, not yet implemented. Extends ADR-040/059 and narrowly supersedes ADR-049's blanket
daemon-self-start prohibition plus their statements that dependencies can never advance work and
agent/conductor changes must always remain foreground proposals. It preserves the single Attention authority,
explicit permission decisions and checkout safety.

**Requirement scope:** `PRD-OUT-001..004`, `PRD-CRE-001..007`, `PRD-FLW-001..011` and
`PRD-SCL-001..008`; expanded individually in the authority file.

### Context

Supervising isolated terminals is not enough for the intended scale. The operator must be able to create a
repeatable multi-agent/tool run, fan independent work out, transfer selected context, sequence typed results
and gather a synthesis without approving the same safe operation one node at a time. A passive-only product
forces the operator to become the scheduler and defeats the reduction in interaction Turn is meant to buy.

The opposite extreme is also wrong. Treating agent prose as commands, allowing a conductor to mint its own
authority or advancing on guessed idle state would turn observation into an unsafe autopilot. The boundary
must be an explicit product object that is reviewable before work starts and enforceable afterwards.

### Alternatives considered

**Keep Turn purely observational.** Rejected. It cannot deliver fast reusable flows or manage a useful volume
of coordinated work; it preserves the exact manual polling/dispatch burden the product exists to remove.

**Let an agent control the daemon through terminal output.** Rejected. Output is untrusted content, has no
stable operation identity and is vulnerable to prompt injection, replay and stale-attempt races.

**Confirm every child, link and dependency separately.** Rejected. It is safe but needlessly interactive.
One up-front scoped grant provides the same authority boundary with far less operator interruption.

**Adopt a spatial canvas as the orchestration model.** Rejected. The persistent hierarchy and WorkSurface
remain Turn's canonical navigation. A Flow/Team view may project a graph or board, but geometry never owns
runtime identity or attention.

### Decision

Turn adds a versioned `FlowDefinition` and immutable `FlowRun` inside a normal Session. A definition declares
node specs, roles, prompts/commands, typed dependencies/start policies, context sources/budgets, execution
targets, worktree isolation, Attention policy, resource/concurrency bounds, optional bounded recurrence and
the exact `DelegationGrant`. One reviewed launch preflights and provisions the graph idempotently; every
external effect has a durable receipt and partial failures remain visible.

Closed `FlowRunTrigger`s are foreground `manual` and one-id-per-occurrence `bounded_recurrence`; the latter
creates a new run from an exact durable occurrence receipt and never directly advances a step. Closed
`StepStartPolicy` variants are `manual`, `with_run`, `after_success`, `after_result`, `all_of` and `any_of`.
Only the four dependency variants of the immutable policy may consume matching current typed
`DependencyResult`s; manual uses a foreground operation and with-run consumes the owning run's transition to
running. Process idleness, output text and agent proposals are not results. Outside a FlowRun a dependency
remains observational.

A `DelegationGrant` binds issuer, FlowRun, exact AgentInstance/RuntimeAttempt/generation, expiry, maximum
nodes/concurrency, provider/tool allowlist, cwd roots, worktree strategy, context scopes, message/dependency
verbs and resource budgets. Within it a conductor may create declared nodes, organise Teams, open declared
context, deliver bounded messages and request fan-out/synthesis without another prompt. It cannot approve a
permission, expand itself, use the primary checkout, delete user data, merge/publish, act after generation
change or turn text into control. An out-of-grant request becomes one exact Attention item.

Creation surfaces use the `category=creation` filter of the one declarative `CommandCatalogue`. Turn-managed fan-out and dependency operations are
asynchronous and return after durable receipts; they inject no synchronous child join, and daemon/UI, input,
navigation and later control remain responsive. A provider may independently wait for its own child, which
Turn reports as provider state rather than claiming to control it. Every writer, whether single/parallel or
foreground/background, receives a dedicated worktree; no Turn path uses the operator's primary `main`
checkout.

The complete normative contract is `docs/OPERATOR_CONTROL_PLANE.md`; frozen requirements and proof
obligations are in `docs/PRODUCT_REQUIREMENTS.md` and `docs/CONTROL_PLANE_ACCEPTANCE.md`.

### Consequences

- Turn can create repeatable mixed-agent/tool workflows with one safe interaction and keep the operator on
  exceptions, decisions and results.
- Teams, messages, dependencies, context and worktrees become typed control-plane objects rather than UI
  conventions.
- Attention remains the only focus/interruption authority, and permissions remain human decisions.
- **Downside:** recovery, idempotency, grant revocation and partial provisioning make Flows a cross-layer
  domain feature, not a command-palette shortcut.
- **Downside:** bounded automation must be tested adversarially; a missing limit or stale generation is an
  authority bug, not merely an orchestration bug.

---

<a id="adr-062"></a>
## ADR-062 — All agent providers normalize into one durable topology and capability contract

**Status:** Accepted, not yet implemented. Extends ADR-003/029/040/059. Existing per-provider adapters and
v0.1 hierarchy are a partial foundation; current false-zero counting and provider-specific resume dispatch
contradict this target.

**Requirement scope:** `PRD-TOP-001..009` and `PRD-ADP-001..009`; expanded individually in the authority
file.

### Context

An operator cannot manage many agents if child agents exist in the real runtime but disappear from Turn, or
if absence of integration is rendered as zero. Provider CLIs expose different hook, notification, transcript,
usage and control dialects. Copying one provider's ephemeral child cards or branching in the UI for every
tool would produce inconsistent lifecycles, counts and attention routes—the exact failure the unified tree is
supposed to eliminate.

### Alternatives considered

**Use OS child processes as the universal hierarchy.** Rejected. Provider-side children may have no process,
one process may host several conversations and executable-name inference cannot prove semantic delegation.

**Model only the richest provider and degrade every other provider to terminal.** Rejected. It hides useful
evidence and makes provider choice change Turn's core behavior instead of only its capability coverage.

**Let each provider own custom UI/state.** Rejected. Counts, Attention, restore and WorkSurface behavior would
diverge and provider state could bypass global safety rules.

### Decision

Every dedicated adapter translates native evidence into the same versioned `AgentTopologyObservation`:
source/observation epoch and covered domain, snapshot/delta/gap kind plus sequence/watermark, parent
instance/attempt/generation, stable native child/tool and child attempt identity, causal invocation, child
task/role, canonical lifecycle and turn transitions, transcript/activity/result handles, optional usage/model
data, native revision, confidence, observation time and explicit capability/unknown states. Provider labels
reduce through the existing two-dimensional Lifecycle/TurnState machine; turn completion is not process exit.
Ingestion is idempotent, order-independent and attempt-fenced. Structured and process evidence reconcile
into one node; only stronger evidence may reparent it.

The open capability vocabulary includes launch/resume/branch/stop, structured status, questions,
permissions, subagents, transcripts, context usage, provider quota, model switching, messaging, context
transfer, shared identity, durable attach and delegated control. The registry is the sole provider-specific
seam. Core, store, protocol projections, Attention and UI may not branch on provider names. Generic/custom
adapters always answer but advertise only proved behavior.

ADR-064 extends that same vocabulary, rather than adding provider branches, with `native_jobs`,
`conversation_inventory`, `title_read` and `conversation_rename`; the complete closed versioned list is in
`docs/ADAPTER_ACCEPTANCE.md`.

Subagents are durable semantic nodes with their own identity, lifecycle and result, not renderer-local cards.
Aggregates declare direct versus descendant scope, graph revision and observation coverage. Exact `0` needs
a matching complete authoritative snapshot or gap-free sequenced coverage for the exact scope/attempt/
generation. Drop, overflow, gap, disconnect/restart or stale heartbeat invalidates exactness until
asynchronous resync; best-effort evidence can only add a lower bound. Partial coverage renders `N+ observed`;
unavailable, unsupported and stale remain distinct. Each Agent View exposes integration version/mechanism/
freshness/downgrade diagnostics and an explicit disposable, bounded, redacted self-test record.

Every advertised capability maps bijectively to shared contract fixtures, provider fixtures and degradation
tests. Capabilities dependent on real provider behavior also require versioned authenticated live evidence.
One provider's timeout cannot block another provider, terminal input or the selected view.

### Consequences

- Claude Code, Codex, Gemini, OpenCode and future/custom agents can differ in evidence without differing in
  core identity, tree, Attention or lifecycle semantics.
- False-zero subagent summaries become impossible by type and acceptance contract.
- Provider-specific richer features remain possible behind explicit capabilities.
- **Downside:** some adapters will truthfully show unsupported/partial for a long time; parity cannot be
  achieved by presentation alone.
- **Downside:** live contract evidence must be maintained across CLI releases in addition to offline fixtures.

---

<a id="adr-063"></a>
## ADR-063 — Operational scale features remain first-class control-plane objects

**Status:** Accepted, not yet implemented. Extends ADR-059/061/062 and closes the operational capabilities
that would otherwise leave a multi-agent control plane unable to discover, isolate or manage all of the work
it claims to supervise.

**Requirement scope:** `PRD-VIE-011`, `PRD-FLW-012`, `PRD-ADP-010`, `PRD-LIF-008`, `PRD-RUN-010`,
`PRD-CTX-012`, `PRD-OBS-008`, `PRD-SCL-009`. The machine-checked one-to-one origin map lives in
`docs/PRODUCT_SPEC_V1.authority`; moving any requirement to another decision changes the frozen authority
root.

### Context

Independent agent identities are not enough when a provider multiplexes many conversations through one
service, a host retains runtimes no Workspace currently knows, teams coordinate through a changing brief,
or several provider accounts must stay isolated. At scale, agents also need a safe way to publish bounded
artifacts/progress, operators need revisioned file and work-item views, and a remote/headless client must see
the same truth rather than a second reduced model.

Treating these as UI conveniences would recreate the failures Turn exists to prevent: invisible surviving
work, cross-account or cross-conversation leakage, unreviewed writes, duplicate board/runtime identity and
remote state that disagrees with the desktop.

### Alternatives considered

**Leave runtime inventory and multiplexing to provider UIs.** Rejected. Turn could display zero while live
work survives, and a shared service could cross-wire instance identity without a daemon-owned binding.

**Represent live briefs, progress and board cards as untyped text.** Rejected. Text is hostile content, has
no revision or authority boundary and cannot safely drive work or Attention.

**Use one ambient provider account/config directory.** Rejected. Parallel sessions could leak credentials,
transcripts, conversations and quota attribution across accounts.

**Make remote operation a separate product/state store.** Rejected. It would split navigation, Attention,
input ownership and recovery truth. A remote surface must consume the same protocol and server-side policy.

### Decision

Turn adds the following provider-neutral control-plane objects and operations:

- one-to-many `RuntimeEndpointBinding`s with installation-wide current ConversationKey ownership while
  endpoint generations fence transport, plus isolated input/transcript/context/Attention. The key's closed
  account scope is either an independently authorised AccountProfile or an endpoint-bound opaque unscoped id
  for sources that deliberately expose no account field; the latter grants no profile/quota/credential power;
- authenticated target-wide `RuntimeInventoryObservation`s plus exact adopt/ignore/terminate reconciliation;
- Note-backed ContextLinks with pinned or explicitly reviewed live-revision policies;
- isolated non-secret `AccountProfile`s with external authentication, launch/default precedence and a
  retire/delete lifecycle;
- bounded delegated Resource/progress operations with typed schemas, compare-and-swap, limits and receipts;
- canonical work-item metadata projected as board/list/search without another topology or Attention queue;
- revisioned atomic FileBackend editing with conflict and confinement semantics; and
- a capability-negotiated remote/headless operator surface over the same hierarchy, WorkSurface, streams,
  Attention routes and mutation journal, while the server enforces its closed remote allowlist.

All of these preserve the same invariant: an observation or piece of content is not authority. Every
external effect names an exact target, identity, generation, revision and operation id, and every unavailable
capability is shown honestly rather than silently approximated.

### Consequences

- Provider services can be shared efficiently without collapsing AgentInstance identity.
- Processes that outlive or escape Workspace bookkeeping stay discoverable and individually reconcilable.
- Teams can share reviewed living briefs and publish progress without granting arbitrary filesystem/control
  access.
- Account, file, work-item and remote workflows become testable domain behavior instead of renderer state.
- **Downside:** host inventory, remote clients and account isolation enlarge the security/acceptance matrix.
- **Downside:** live-note and delegated-resource policies need strict cumulative bounds and revision audits.

---

<a id="adr-064"></a>
## ADR-064 — Close the remaining operator-loop capabilities as typed first-class contracts

**Status:** Accepted, not yet implemented. Extends ADR-059/061/062/063 after an independent final-reference
coverage audit rejected the previous specification candidate.

**Requirement scope:** `PRD-VIE-012`, `PRD-ADP-011`, `PRD-LIF-009`, `PRD-RUN-011`, `PRD-CTX-013`,
`PRD-OBS-009`, `PRD-ATT-011`, `PRD-SCL-010`. The machine-checked one-to-one origin map lives in
`docs/PRODUCT_SPEC_V1.authority`; moving any requirement to another decision changes the frozen authority
root.

### Context

The earlier candidate covered the hierarchy, WorkSurface, Attention, Flows, topology, context handoff and
telemetry, but it still forced avoidable operator actions and left real work outside the model. In
particular it did not fully specify external work-item sources, provider conversation history, provider-native
jobs, an interactive Browser, independent title operations, profile-scoped companion activity or a narrowly
delegated remote permission response. It also said both that selection never starts work and that returning
to a Session should be immediately useful.

Those are not optional visual extras. An operator supervising many processes needs the default foreground
Session to become usable in one action, must see provider work that exists outside current runtime handles,
and must distinguish a local projection from an externally authoritative object. Remote assistance is
incomplete if every routine typed permission requires returning to the desktop, but unsafe if raw terminal
input or a broad invitation can answer it.

### Alternatives considered

**Leave these as implementation details beneath existing broad rows.** Rejected. A broad row could pass
without testing pagination, ambiguous source writes, global conversation ownership, job survival, browser
origin isolation or permission-grant fencing; the missing behavior would remain invisible to the gate.

**Treat provider-native jobs as recurring Flows and history as RuntimeInventory.** Rejected. Provider jobs
have provider-owned schedules and iterations that may survive Turn, while historical conversations need no
live runtime. Combining them would invent identity, lifecycle and cancellation authority.

**Use one generic WebPreview view and raw remote PTY input.** Rejected. Inert reference content and interactive
browsing have different network/storage/origin consequences, and raw input can bypass a typed sensitive
interaction contract.

**Keep every permission desktop-only.** Rejected. It creates unnecessary operator polling and movement even
when the desktop can pre-authorise one exact closed response. Credentials, host trust, administration and
authority changes still cannot be delegated this way.

### Decision

Turn adds eight separately testable capabilities:

- a WorkItemSource adapter with stable external identity, per-field authority, bounded sync/cache coverage,
  revision-fenced writes and explicit conflict/reconciliation;
- provider-native Job/Iteration identity and controls, explicitly distinct from Flow recurrence and from
  dismissing Turn projections;
- foreground Session activation that restores/attaches and may materialise only its bounded fully preflighted
  eligible saved descriptors—or one safe default Shell when empty—in the same selection, otherwise failing
  closed with one precise recovery action;
- separate inert WebPreview and isolated interactive Browser semantics, including reviewed local HTML and
  loopback origins, navigation/history, popup/download and storage policy;
- a private bounded ConversationInventory with search/pagination, global ConversationKey ownership and
  separate adopt/resume operations;
- independent `title_read` and `conversation_rename` capabilities with revisioned receipts and degradation;
- one exact typed permission-response operation with closed authorization variants: direct authenticated
  local-foreground submission, or a single-use foreground-issued end-to-end encrypted remote grant. Raw PTY
  input is blocked on every surface when typed response exists; only verified local foreground retains a PTY
  fallback when it does not, and all secret/admin/trust/grant authority remains local; and
- AccountProfile-scoped context, quota and activity projections shared by desktop/companion, preserving
  freshness/coverage and never converting missing or partial evidence into zero.

Every object remains in the canonical tree or a projection of it, every selection routes to the one
WorkSurface, and every actionable event enters the same Attention Queue. No new canvas, runtime identity or
independent notification queue is introduced.

### Consequences

- External and provider-retained work can be discovered and controlled without identity or authority lies.
- Common safe foreground activation and narrowly delegated permissions reduce operator interactions while
  preserving explicit failure and review boundaries.
- WebPreview/browser, Flow/job, runtime/history and read/rename pairs can degrade independently and be tested
  independently.
- **Downside:** adapters now need more granular capabilities, live evidence and conflict fixtures.
- **Downside:** browser origins, external-source reconciliation and remote response grants substantially
  enlarge security, privacy, performance and packaged accessibility acceptance.

---

<a id="adr-065"></a>
## ADR-065 — Make source-capability coverage explicit and close the final operational gaps

**Status:** Accepted, not yet implemented. Extends ADR-049/059/061/062/063/064 after successive blind audits
rejected prior candidates and corrected both over-broad and missing claims.

**Requirement scope:** `PRD-HIE-009`, `PRD-HIE-010`, `PRD-CRE-008`, `PRD-ADP-012`, `PRD-ADP-013`, `PRD-OBS-010`,
`PRD-OBS-011`, `PRD-ATT-012`, `PRD-SAF-014`. The machine-checked one-to-one origin map lives in
`docs/PRODUCT_SPEC_V1.authority`; moving any requirement to another decision changes the frozen authority
root.

### Context

The first independent audit found capabilities that a requirements-only comparison could miss: recursive
organisation, branch-isolated group workflows, notification delivery while the UI is absent, target memory
pressure, two additional dedicated agent adapters, quota-only providers, configurable inference endpoints,
complete Workspace onboarding and safe generated names. A later audit also corrected an imprecise claim:
target-wide runtime/survivor inventory already existed, but it lacked host capacity and process-tree resource
accounting. A final exact-SHA blind audit then caught a visual-management capability omitted from both the
ledger and requirements: one-action active-work-surface top-level non-overlapping arrangement. Turn adapts that behavior to its canonical
tree instead of importing a spatial authority or requiring an operator cleanup action. Corrections must
narrow the gap rather than inflate the amount of purported missing work.

The deeper failure was methodological. A selective gap narrative has no frozen denominator, so an omitted
feature is indistinguishable from a deliberately rejected one. Turn therefore needs an independently auditable
coverage ledger as well as PRD and ACP rows. The ledger may cite an opaque frozen source snapshot and evidence
digests, but product/code/docs must remain source-neutral and must not inherit another product's names,
architecture or canvas authority.

### Alternatives considered

**Declare broad hierarchy, telemetry and adapter rows sufficient.** Rejected. They do not force cycle-safe
Group mutation, complete-versus-failed memory accounting, a six-adapter live matrix, quota-only isolation or
BYO model-route secret handling.

**Give Group runtime/repository authority because it presents a branch.** Rejected. Group remains a
presentation relationship. A Session-owned CheckoutScope owns repository/worktree identity and lifecycle;
the Group only projects it and supplies reviewed defaults. This preserves one hierarchy without creating a
second execution owner.

**Treat notification gateway acceptance as delivery or Attention resolution.** Rejected. Push is an encrypted,
revocable, collapse-aware projection. It can be accepted, fail or expire without saying anything about device
delivery, reading or the underlying interaction.

**Reuse AccountProfile or RuntimeEndpoint as a model gateway.** Rejected. Account identity, a multiplexed
provider service and a configurable inference route have different identities, secrets, network trust and
launch-freeze semantics.

**Copy every remote mutation exposed by the audited source.** Rejected as a deliberate safety adaptation.
The full remote surface may create ordinary work, type under a visible lease and perform revision-fenced
non-destructive mutations, including the explicitly registry-allowed scoped repository stage/unstage/commit/
fetch/branch operations. Credential entry, grant/authority issue, host trust, destructive process lifecycle,
repository push/pull/commit-and-push/merge/conflict-resolution/discard/cleanup and publish stay desktop-
foreground-only under `PRD-SAF-012` and `PRD-SCL-009`. This is
recorded as an adapted capability, not an omission.

### Decision

Turn adds nine independently testable obligations:

- same-Session recursive Groups with one parent, atomic CAS subtree operations, cycle/depth/corruption bounds
  and closed non-cascading removal dispositions;
- automatic compact non-overlapping layout of the canonical tree in stable logical/accessibility order, with
  no persisted coordinates, tidy command or mutation of hierarchy, selection, runtime, input or Attention;
- Session-owned CheckoutScope identity/lifecycle, optionally projected by a Group, with one-step agent-per-
  branch creation/adoption, truthful inventory/reconciliation and no implicit cwd/runtime/worktree deletion;
- one resumable WorkspaceOnboarding path for new/open/clone/SSH adoption, with crash-safe partial receipts and
  repository publication as a separate foreground operation;
- six dedicated agent adapters—Claude Code, Codex, Gemini, OpenCode, GitHub Copilot and Grok—under the same
  capability/live-evidence matrix, plus isolated Kimi and MiniMax quota/activity connectors;
- a distinct ModelEndpointProfile for target-bound BYO endpoint/model routing, bounded discovery, secret
  references, network trust and launch/switch receipts with no silent fallback;
- ResourceInventory fields over the existing target RuntimeInventory for host RAM/swap/pressure and
  reuse-safe process-tree accounting across current owners, closed owners and unmatched survivors;
- local DisplayNameFacts and bounded generated proposals whose manual pinning, redaction and revision rules
  never imply identity, provider rename or terminal input; and
- revocable device delivery grants, minimal encrypted outbox, subject/revision collapse, monotonic live status
  and a notification-only host that opens no public listener.

`docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv` freezes the audited denominator. Every stable feature id has one
`adopted|adapted|rejected|irrelevant` disposition, evidence digest, rationale and PRD/ACP/ADR trace. Unknown,
duplicate, missing, malformed, broken-link or weakened entries fail the specification gate and its mutation
suite. The file is authority-hashed with every other normative source.

### Consequences

- Nested agent-per-branch workflows remain visible in the canonical tree without making presentation equal
  ownership or blocking the primary `main` checkout.
- Dense or changing hierarchies remain readable without an operator arranging them or creating a second
  canvas/layout authority.
- Operator Attention can arrive when the full UI is backgrounded or absent without inventing a second queue
  or falsely acknowledging work.
- Capacity, adapter, quota, route and naming claims become scoped evidence rather than labels or guesses.
- A future audit can prove what was considered and why, instead of trusting a prose assertion of completeness.
- **Downside:** the protocol/store/security/privacy/performance/accessibility matrices gain new identities,
  lifecycle machines and destructive-boundary cases.
- **Downside:** any source-snapshot refresh now requires an explicit ledger review, ADR and external authority
  pin rotation rather than a quiet documentation edit.

---

<a id="adr-066"></a>
## ADR-066 — Bound the remaining operator utilities without creating parallel authority

**Status:** Accepted, not yet implemented. Extends ADR-056/059/061/063/064/065 after exact capability,
cross-document, security and performance audits showed that the previous candidate could pass while common
operator workflows remained untyped or trusted by assertion.

**Requirement scope:** `PRD-VIE-013`, `PRD-VIE-014`, `PRD-CRE-009`, `PRD-CRE-010`, `PRD-RUN-012`,
`PRD-RUN-013`, `PRD-RUN-014`, `PRD-RUN-015`, `PRD-RUN-016`, `PRD-SAF-015`. The machine-checked origin map
assigns each id only to this decision; every source-ledger row mapped to one of these requirements uses the
same origin.

### Context

The operator control plane already had one hierarchy, one WorkSurface, one Attention Queue and typed
runtime/context authority. It still lacked closed contracts for routine work around those pillars: command
discovery, bounded media, safe content rendering, repository-host identity, generated commit drafts, file
transfer, announcements, app updates, work-item history and narrowly reversible presentation changes.

Leaving those as renderer helpers would create exactly the hidden side channels this architecture rejects.
A toolbar callback could execute something the catalogue could not name; an update or announcement could be
called “signed” without canonical bytes or independent trust roots; a proposal helper could read the
repository it was meant only to summarise; and generic undo could replay an external or destructive effect.
Likewise, unqualified end/delete rules could erase Attention or Session-scoped authority while claiming the
operator had safely finished work.

### Alternatives considered

**Treat the utilities as implementation details of existing broad requirements.** Rejected. Broad rows could
pass without paging, replay fencing, sandbox enforcement, manifest/package binding, exact transfer recovery
or the closed undo allowlist.

**Reuse one trust root and one generic signed JSON envelope.** Rejected. Cross-domain key acceptance and
ambiguous serialization allow a valid object from one channel to authorise another. Command extensions,
announcements, update manifests, update packages and voice-model manifests need separate stores, domains and
replay watermarks.

**Let a local proposal command run with the Session environment.** Rejected. “Local” does not make a process
least-privilege; inherited cwd, descriptors, environment, credentials or sockets would expose far more than
the reviewed staged snapshot.

**Make all mutations undoable.** Rejected. Runtime input, provider/source/repository effects, Attention,
credentials, grants and destructive lifecycle cannot be reversed by restoring presentation state.

### Decision

Turn adds ten independently testable obligations without adding another navigation, runtime or authority
model:

- one contextual revisioned `CommandCatalogue` supplies toolbar, palette, context menus and shortcuts with
  stable ids, bounded search, typed schemas and identical current authority checks;
- signed product announcements remain inert and separate from operational StatusEvent and Attention;
- app update discovery/download/verify/stage/apply/rollback is an explicit crash-reconcilable lifecycle that
  preserves live terminal work and never silently installs;
- WorkItem activity is an ordered, paged, provenance-bearing evidence stream over the canonical WorkItem;
- Media is a distinct inert Node capability with descriptor/stream import, sealed hash publication,
  lookup-only recovery and sandboxed non-autoplay playback;
- repository-host profiles and independently scoped RepositoryBackend/WorkItemSource grants separate safe
  identity from external credentials and bind every request;
- a commit-proposal provider receives only one exact bounded redacted staged snapshot inside a fail-closed
  sandbox and can only return an editable draft;
- transfer tickets bind two separately fenced endpoints, immutable size/hash/chunk/expiry policy and
  create-new atomic publication with no same-name fallback;
- plain/Markdown is an inert exact-revision content projection; a link opens only through a separate reviewed
  Browser-Node operation; and
- undo/redo represents exactly eight presentation requests, including constant-size expand-all, partitioned by operator/surface/Session and
  structurally unable to contain runtime, input, provider, source, SCM, Attention, context, grant, credential
  or destructive effects.

All five signed artifact domains use a versioned canonical signing envelope, installation-owned independent
trust stores, monotonic key epochs/sequences, revocation and replay/high-water fences. Update packages are
separately signed and bind the exact parent-manifest hash. The commit-proposal profile freezes an executable
descriptor/hash or exact broker route, sandbox policy and numeric resource limits before spawn. New paths
have explicit performance workloads and may never consume the terminal-input or Attention latency budget.

This decision also closes cross-cutting survivor semantics found during the same audit. End/delete serially
derives every semantic, relationship and Attention disposition without blocking on user input. Cross-Session rehome terminates or
historicises Session-scoped Team/Flow/dependency relationships and revokes their authority; immutable grants
and ContextLinks are never retargeted. Once that semantic commit succeeds, unreachable process/worktree/
artifact cleanup is reported and cannot resurrect navigation. The primary `main` checkout remains
operator-only for both runtime and repository mutation paths.

### Consequences

- Common operator actions remain discoverable and low-interaction while using the same typed authority as
  their direct forms.
- Signatures, helper isolation, transfer recovery, updates and undo have mutation-resistant acceptance
  surfaces instead of security adjectives.
- Media, announcements, activity and rendered content enrich the WorkSurface without adding a canvas or
  another Attention queue.
- Lifecycle cleanup preserves the evidence the operator still needs and cannot silently retarget authority.
- **Downside:** five trust stores, helper sandboxing and crash-reconcilable staged artifacts enlarge platform,
  privacy, release and performance test matrices.
- **Downside:** presentation undo is intentionally narrow; an operator must use the domain's explicit repair
  or compensating operation for every external effect.

---

<a id="adr-067"></a>
## ADR-067 — Close the audited operator-surface and lifecycle denominator

**Status:** Accepted, not yet implemented. Extends ADR-059/061/064/065/066 after a file-and-registry source
census, adversarial trace audit and cross-document state audit proved that ledger presence alone could still
hide unowned state, an undisposed source behavior or an acceptance row that passed on a shallow UI.

**Requirement scope:** `PRD-CRE-011`, `PRD-ADP-014`, `PRD-LIF-010`, `PRD-LIF-011`,
`PRD-VIE-015`, `PRD-RUN-017`, `PRD-RUN-018`, `PRD-RUN-019`, `PRD-RUN-020`, `PRD-RUN-021`, `PRD-RUN-022`,
`PRD-OBS-012`, `PRD-ATT-013`,
`PRD-SAF-016`, `PRD-SAF-017`, `PRD-SAF-018`, `PRD-SAF-019`, `PRD-SAF-020`, `PRD-SCL-011`, `PRD-SCL-012`,
`PRD-SAF-021`, `PRD-SCL-013`. `PRD-CRE-007` remains owned by ADR-061 and `PRD-ADP-010` remains owned by ADR-063 even though
the refreshed source census maps new capability origins to those existing obligations. Only the new or
explicit-disposition rows whose requirements are listed above map to this decision. The census, its one-to-many
mapping table, capability ledger, PRD, ACP and state-
family manifest are one authority-hashed proof set; changing any denominator or origin changes that root.

### Context

The prior candidate described Turn's central purpose correctly but had not proved an exhaustive source
disposition. A second audit found common operator utilities and later lifecycle/safety behaviors that were
present in the audited source yet absent, over-generalised or attached to old decisions: diagnostics and bug
report preparation, hierarchical settings, short-lived collaborator chat, document viewing and terminal
clipboard gestures, Attention sound cues, bounded bulk recovery, conservative idle hibernation, agent-owned
isolated browser work, mobile companion launch, corrupt-store preservation, commercial entitlement surfaces
and anonymous telemetry. An acceptance row for each name was still insufficient: without exact state owners,
bounds, reducers, recovery and negative authority, a renderer mock could pass while the product remained
unsafe or incomplete.

The audit also exposed a methodological trap. A single source file can support several capabilities and one
capability can require several files; a one-file/one-row heuristic or generic catch-all mapping can report
100 percent while hiding semantics. Source coverage therefore needs a normalized candidate census plus a
one-to-many mapping whose basis is independently checked, not a count copied from the ledger.

### Alternatives considered

**Fold every new row into ADR-059 because it concerns the control plane.** Rejected. That makes origin
verification tautological and fails to record the new security, data-lifetime and source-denominator decision.

**Adopt source behavior literally.** Rejected. Turn has no commercial entitlement gate or product telemetry;
terminal-originated clipboard control is denied; agent browsing and Companion launch gain narrow reviewed
grants; idle hibernation is opt-in and evidence-conservative; document content is inert and printing is a
separate effect.

**Treat diagnostics, settings, sounds, clipboard and corrupt-file handling as renderer details.** Rejected.
Each can retain sensitive bytes, create an external effect, hide a gap, alter lifecycle or destroy the only
copy of operator data. They require typed ownership and falsifiable limits.

### Decision

Turn adds or explicitly disposes twenty-two obligations:

- one generated 23-section settings hierarchy with an independently frozen scope matrix, exact resolver
  provenance, canonical search/deep links and revision-pinned section reset;
- a redacted bounded current-daemon diagnostic ring and a separate local bug-report draft/review path whose
  clear/export/open effects cannot erase or leak operational evidence;
- short-lived collaborator chat fenced to one exact shared ViewTarget with no durable, input, context,
  Attention or control authority;
- exact-revision image/PDF views with isolated bounded decode/search and a distinct reviewed, reconcilable
  print intent;
- gesture-bound terminal copy/paste/path drop with memory-only bodies, existing input receipts and unconditional
  denial of terminal-controlled clipboard read/write;
- a closed advanced repository-operation set for init, detached checkout, branch rename/delete, stash push/pop,
  merge, rebase, revert and force-push, each revision-planned, non-primary, consequence-reviewed where
  destructive and reconciled by typed intent rather than an arbitrary Git command;
- optional rate-limited `done|needs_you` local sounds derived only from canonical state edges and supplementary
  to accessible visual/structured Attention;
- a reviewed≤256-candidate sequential bulk idle restart with per-instance canonical receipts and complete
  final accounting;
- opt-in Eco hibernation that admits only exact idle, off-screen, local, resumable work, preserves Session and
  scrollback, wakes automatically and never hides Attention;
- a locally adopted Workspace grant for one agent to create and operate only its own logged-out isolated
  Browser Nodes through bounded typed actions and a permanently visible Stop control;
- opt-in Browser Memory Saver that discards only an exact safely idle hidden page after five minutes, reports
  resource truth, retains no page/form/credential state and automatically rehydrates on same-daemon selection
  through one new idempotent reviewed navigation intent or a precise bottom-status refusal;
- a closed fail-visible set of optional menu/header controls whose setting cannot hide Attention, recovery,
  destructive, Delete/End, Restart, Search or Close routes and cannot remove palette/keyboard access;
- target-scoped PTY used/ceiling/headroom observations with exact freshness, early and critical Attention and
  a separately reviewed, durably reconciled privileged remediation only where before/after/rollback proof is
  available;
- off-screen viewer/xterm/PTY-client parking that proves durable work survives, caps safe parks by LRU, keeps
  zero-PTY tmux observation/input fenced and reattaches automatically without another user action;
- explicit rejection of age/count/memory/PTY-pressure killing of detached durable Sessions; those signals may
  raise Attention or trim proved reconstructible clients, never end user work;
- opt-in AccountProfile-scoped private cross-conversation body search with bounded redacted index/query, honest
  freshness/coverage/deletion and canonical view routing without implicit resume or input;
- a separate narrow expiring Companion launch grant over immutable safe templates, preassigned canonical ids
  and lookup-only recovery, with no arbitrary command/flags/cwd/account/target authority;
- byte-preserving corrupt-store quarantine before any default save, with no time-based evidence deletion and
  explicit recover/start-fresh/export/discard dispositions;
- daemon-authoritative additive convergence for changes from another authenticated client, with operation echo
  deduplication, revision conflicts and fieldwise presentation merge; internal store files are never a second
  watched mutation authority and external packages remain reviewed PortableImports;
- an explicit `irrelevant` disposition for commercial licence, subscription, seat and entitlement behavior;
- an explicit `rejected` disposition for product telemetry, analytics and install-count behavior, while
  purpose-bound signed update discovery remains identifier-free; and
- a normalized exhaustive source census/mapping gate plus closed machine-readable state-family ownership
  manifest, both mutation-tested against deletion, duplicate, stale digest, false mapping and undeclared state.

All positive paths reuse the canonical hierarchy, WorkSurface, Attention Queue, typed mutation envelope,
runtime identities and existing isolated Browser/terminal authority. None introduces a canvas, hidden mobile
registry, second settings resolver, second clipboard/input path or generic plugin authority. Every body,
intent, receipt, worker and queue has an exact owner, independent count/byte boundary and cleanup edge in
`docs/PROTOCOL.md`, `docs/PRIVACY.md` and `docs/PERFORMANCE.md`.

### Consequences

- A complete specification claim now depends on a reproducible denominator rather than confidence or document
  volume; an unmapped source candidate or undeclared state family is a hard NO-GO.
- Operator utilities remain low-interaction without allowing page content, remote clients, terminal escape
  sequences, stale lifecycle evidence or settings search to acquire authority.
- Data-loss and privacy-negative behaviors are dispositions with executable oracles, not undocumented absence.
- **Downside:** document decoding/printing, Browser automation, bulk/Eco lifecycle and corrupt-store recovery
  add platform-specific failure and resource-pressure matrices.
- **Downside:** any source refresh requires semantic remapping and independent audit; line-count parity or a
  generic capability fallback is intentionally unable to satisfy the gate.
