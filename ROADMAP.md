# Turn — Roadmap

Milestones, what each delivers, and how it is verified. Then the living lists: post-MVP, open decisions,
risks and technical debt.

The current state is stated honestly. `ARCHITECTURE.md` §0 holds the authoritative per-crate status table;
this document says what happens next and how we will know it worked.

---

## Where the project actually is

**The frontend was replaced.** The Tauri shell and the TypeScript/`xterm.js` window were built, rejected by
the product owner on sight, and deleted: `ui/` and `crates/turn-ui` no longer exist and `turn-ui` is out of
the workspace. The window is now native Rust drawn on the GPU (`crates/turn-gui`, `eframe`/`egui` over
`wgpu`). ADR-039 records the decision, its cost and its downsides. The window milestone is therefore
**reopened** as M7 below, and the tests it used to count are gone rather than renamed.

There is one Rust test runner and one lockfile. The reproducible source of truth is
`cargo test --workspace -- --test-threads=1`; this roadmap no longer hard-codes a total that becomes false
whenever a regression test lands. PTY and loopback hook tests use real operating-system resources and are
run serially for the release audit.

The upgraded first vertical now runs through the production daemon, store, protocol projection and GUI
model: Workspace and fenced main Session, explicit Reviewer spawn, stable preview, Quick Preview,
temporary Pane close without process termination, and UI restart/restore. A separate integration test sends
the event through the real loopback Claude hook transport and production normaliser. This is reproducible
without a paid external service. A manual smoke test against an authenticated, currently installed Claude
Code CLI is still pending and must not be conflated with the deterministic proof; current Claude hook
payloads may provide a role/external id without a parent-declared display name.

| Milestone | Delivers | Status |
| --- | --- | --- |
| M0 — Domain | `turn-core`: entities, two-axis state, event vocabulary, attention | **Done** |
| M1 — Terminal substrate | `turn-pty`: ptys, bounded buffers, supervision | **Done** |
| M2 — Agent integration | `turn-agents` + `turn-hook`: adapters, hook server, heuristics, registry | **Done** |
| M3 — Persistence | `turn-store`: SQLite, migrations, redaction, seven repositories | **Done** |
| M4 — Protocol | `turn-proto`: framing, requests, responses, pushes, view models | **Done** |
| M5 — The daemon | `turnd`: assembles everything, owns the ptys | **Code complete**, exit criterion unverified |
| M6 — Unified hierarchy foundation | ADR-040: checkout leases, Agent/View split, safe previews, protocol v3 | **Implemented for the first vertical**; release audit/hardening remains |
| M7 — The window | Native Rust on the GPU: one hierarchy, user-chosen panes, inspector, effects | **Implemented for the first vertical**; advanced tree management remains |
| M8 — First vertical | One Session and background Reviewer, end to end | **Automated vertical complete**; authenticated live-CLI smoke test pending |
| M9 — Hardening | Measurement, restore semantics, Linux parity, packaging | **Not started**; two CI boxes already green |

M6 blocks incompatible M7/M8 UI work. Its exit proof is the reproducible
`Workspace → main Session+lease → Claude fixture → Reviewer background node → normalised preview → Quick
Preview → temporary Pane → close without stop → restart and reattach` sequence in
`docs/UNIFIED_HIERARCHY_UPGRADE.md`.

---

## M0 — Domain layer · **Done**

**Delivers.** `turn-core`: typed ids; `Workspace`, `Session`, `ProcessNode`/`SessionTree`, `Layout`/`Pane`,
`Template`; the two-axis `Lifecycle` × `Turn` model with a derived `DisplayState`; the `TurnEvent`
vocabulary with the `Confidence` ladder; and the attention subsystem — policy, queue, focus governor,
manager. No I/O, no clock reads inside logic.

**Verified by.** 116 tests. The ones that pin the product's guarantees rather than its mechanics:

- `state::tests::finishing_a_turn_is_not_the_same_as_exiting`
- `state::tests::a_crashed_process_never_keeps_claiming_it_awaits_you`
- `event::tests::a_heuristic_cannot_promote_itself_to_explicit`
- `attention::queue::tests::simultaneous_demands_produce_one_ordered_next`
- `attention::queue::tests::aging_prevents_starvation_without_reordering_priority_classes`
- `attention::focus::tests::a_policy_cannot_opt_out_of_the_typing_guard`
- `attention::focus::tests::agents_finishing_at_the_same_instant_produce_one_focus_change`
- `attention::manager::tests::a_guessed_permission_never_produces_a_focus_effect`
- `model::node::tests::a_confirmed_link_is_not_overwritten_by_an_inferred_one`

---

## M1 — Terminal substrate · **Done**

**Delivers.** `turn-pty`: `PtyProcess` (spawn, write, resize, subscribe, replay, interrupt, terminate,
kill, exit reporting), `TerminalBuffer` (bounded byte ring plus bounded vt100 screen, OSC 52 refusal, title
sanitisation), `ProcessSupervisor` (on-demand process-table scans, transitive descendants, conservative
classification).

**Verified by.** 46 tests against real ptys and real processes, not mocks (38 at the start of this survey;
eight terminal-hardening tests were added during it, covering clipboard reads, self-resize requests and
Unicode tricks in titles and rows):

- `process::tests::a_process_sees_the_size_we_gave_it` — asks the tty itself via `stty size`
- `process::tests::resize_reaches_both_the_kernel_and_our_screen_model`
- `process::tests::a_full_screen_application_is_recognised`
- `process::tests::a_slow_subscriber_is_told_it_fell_behind_rather_than_buffering_forever`
- `process::tests::heavy_output_does_not_block_a_second_process`
- `process::tests::a_reattaching_pane_can_rebuild_the_screen_from_replay`
- `process::tests::killing_a_process_is_reported_as_a_signal_death`
- `process::tests::interrupting_reaches_the_foreground_process_group`
- `buffer::tests::a_clipboard_write_from_the_process_is_refused_but_recorded`
- `buffer::tests::a_malicious_title_is_stripped_of_control_characters`
- `supervisor::tests::a_command_that_only_mentions_an_agent_is_not_classified_as_one`

---

## M2 — Agent integration · **Done**

**Delivers.** `turn-agents`: the `AgentAdapter` trait and the four Integration Levels; the Claude Code
adapter (`Structured`, HTTP hooks via `--settings`); the Codex adapter (`Structured` via inline TOML hooks
plus `notify`, degrading to `Wrapper`); the heuristic layer (`InferredHigh`, closed executable list, stands
down in the alternate screen); the registry (selection that always answers and always explains); the
loopback hook server (127.0.0.1, per-node 256-bit tokens, immediate empty-200 responses, bounded event
channel). Plus `turn-hook`: a zero-dependency helper that cannot break an agent session.

**Verified by.** 161 tests in `turn-agents` (118 unit, 15 Claude Code contract, 19 Codex contract, 9
invariants) and 19 in `turn-hook`, all green on 2026-08-04. The contract tests are the load-bearing ones,
because hook payloads are a contract Turn does not own, and both fixtures are now recorded off the wire:

- `contract_claude::the_recorded_hook_payloads_still_carry_the_fields_the_adapter_reads` — against
  `tests/fixtures/claude-code-2.1.221.json`, recorded from live runs against Claude Code 2.1.221
- `contract_claude::the_recorded_stop_failure_reports_the_real_error_code` — the field is `error`, not the
  documented `message`
- `contract_claude::no_recorded_or_corrupted_payload_can_panic_the_adapter`
- `contract_codex::hooks_are_configured_as_an_inline_toml_struct_and_never_as_a_path`
- `contract_codex::the_handler_list_key_is_hooks_because_handlers_fires_nothing`
- `contract_codex::subscribed_event_keys_are_pascal_case_and_from_the_known_set`
- `contract_codex::without_hook_trust_the_adapter_reports_wrapper_and_says_what_is_missing`
- `contract_codex::a_tool_call_payload_is_never_turned_into_an_approval_or_a_command_to_run`
- `invariants::*` — nine tests, one per product rule: a heuristic cannot steal focus, no payload can claim a
  permission was allowed, no payload becomes something to run, hierarchy only ever comes from a tool's own
  report, and an agent cannot name its session after a command-line flag
- `server::tests` — including a real HTTP client over a real socket, an unknown token refused and counted,
  and the server continuing to answer agents after its receiver is dropped

---

## M3 — Persistence · **Done**

**Delivers.** `turn-store`: the `Store` facade (`open_default`, `open_in`, `open_at`, `open_in_memory`),
schema versioned in SQLite's own `user_version` with append-only migrations and a loud refusal of any
database from a newer build, WAL and enforced foreign keys, `codec` for tag-vs-JSON columns, `redact` for
secret hygiene, `location` for platform paths, and seven repositories (workspace, session, node, event,
attention, template, settings) using `ON CONFLICT DO UPDATE` rather than `REPLACE`.

The line that matters most: `SessionRepo::load_for_restore` **downgrades anything stored as running to
`Lifecycle::Orphaned`**, because a stored `Alive` only ever meant "alive when we last wrote".

**Verified by.** 119 tests, one of them a doctest. The integration tests are named after the promises:

- `restart_restores_the_desk::a_whole_desk_survives_a_restart_and_reports_what_it_cannot_vouch_for`
- `restart_restores_the_desk::a_pending_demand_for_the_user_outlives_the_daemon_that_recorded_it`
- `restart_restores_the_desk::a_partial_restore_is_recorded_so_the_ui_can_explain_itself`
- `restart_restores_the_desk::the_event_log_still_says_which_states_were_guesses`
- `restart_restores_the_desk::closing_a_pane_in_a_stored_session_does_not_leave_the_process_row_behind`
- `restart_restores_the_desk::a_long_running_install_prunes_its_history_without_losing_the_recent_past`
- `secrets_never_reach_the_disk::no_secret_value_is_present_anywhere_in_the_files_on_disk`
- `secrets_never_reach_the_disk::a_secret_survives_nowhere_even_after_the_daemon_restarts_and_prunes`
- `secrets_never_reach_the_disk::a_process_environment_is_not_persisted_wholesale_even_when_it_looks_innocent`
- `tests::a_database_from_a_newer_build_is_refused_at_open_time`

---

## M4 — Protocol · **Done**

**Delivers.** `turn-proto`: the versioned `ClientFrame`/`ServerFrame` envelope with a `hello`/`welcome`
handshake and `negotiate()`; one flat `Request` enum whose `expected_result` names a response for every
operation; `Response` and a single `ProtoError` shape with a machine-readable `ErrorCode`; `ServerEvent`
pushes; `TerminalBytes` (base64, with the cost documented and `OutputEncoding` negotiated as the escape
hatch); newline-delimited JSON framing robust to partial reads and bad lines; and the `view` projections
that keep product rules out of the client.

Four guarantees enforced **by omission** — the strongest form available to a type definition: no request
approves a permission, none runs an inferred command, none relaunches on its own, and focus arrives only as
an `Effect` the governor already cleared. Nothing uses `deny_unknown_fields`, so an older client ignores a
newer daemon's added fields; a change that would make an older client *misread* a message bumps
`PROTOCOL_VERSION` and the handshake refuses the connection rather than letting it half work.

**Verified by.** 127 tests. The catalogue-level and conversation-level ones matter most:

- `contract::every_request_names_a_response_variant_that_exists`
- `contract::every_response_variant_is_produced_by_at_least_one_request`
- `contract::the_catalogue_reassembles_correctly_under_pathological_chunking`
- `conversation::a_full_working_conversation_completes_over_the_real_framing`
- `conversation::a_guessed_state_reaches_the_client_as_a_guess_and_never_as_a_focus_jump`
- `conversation::a_ui_restart_rebuilds_its_terminals_without_touching_the_processes`
- `conversation::a_partial_restore_offers_a_relaunch_that_only_the_user_can_accept`
- `conversation::a_firehose_of_output_reassembles_in_order_and_admits_what_was_dropped`
- `conversation::a_hostile_stream_of_rubbish_never_costs_more_than_the_bad_lines`
- `conversation::a_stale_client_is_told_which_side_is_old_and_the_connection_ends`
- `conversation::a_subagent_appearing_pushes_a_tree_the_client_can_draw_without_guessing`

---

## M5 — The daemon · **Code complete, exit criterion unverified**

`main.rs` parses options, initialises logging, resolves a `Config` and calls `turnd::start`; the library
declares `config`, `paths`, `instance`, `logging`, `options`, `error`, `server` and `core`, the last split
into `spawn`, `supervise`, `restore`, `events`, `requests`, `views`, `attention`, `clients`, `command` and
`output`. 85 tests pass, including the integration tests in
`tests/{desk,agents,surface,restart,attention,binary}.rs`.

**The exit criterion below is still unmet:** no scripted client has created a Session whose hook POST changed
what the client was told, and nothing here has been driven by a real agent. The deliverables are therefore
still listed as deliverables — passing tests are not the same as the milestone being met.

**Delivers.**

1. **The reducer** — the piece nothing else can supply. One place that takes a `TurnEvent` from the hook
   server, an `ExitInfo` from a `PtyProcess`, an `ObservedProcess` from the supervisor and an `Inference`
   from `OutputHeuristic`, and folds all four into one authoritative `SessionTree`. This includes the
   correlation Turn does not have yet: hook `session_id` → `NodeId` via `find_by_external_id`, observed pid
   → node via `find_by_pid`, and the `Relation` ladder applied on every link.
2. **Pty facts as events.** `EventKind::ProcessStarted`, `ProcessExited`, `ProcessFailed` and
   `ProcessSpawnedChild` are defined and have never been emitted. This is where they come from.
3. **The unix socket server**, speaking `turn-proto` — accept, handshake, per-connection request loop,
   push fan-out, and the lagged-subscriber replay path.
4. **The session lifecycle** — create from a Template, materialise Panes into `PtyProcess`es via
   `AdapterRegistry::select` and `AgentAdapter::prepare`, register with the `HookServer`, tear down, and
   delete the scratch directory.
5. **Store integration** off the reactor. `rusqlite` is synchronous; every call goes through a blocking
   boundary or a dedicated writer, with the daemon owning ordering.
6. **Restore.** Load with `load_for_restore`, try to re-attach each `Orphaned` node against the process
   table, set `Reconnected` or `Lost`, compute the Session's `RestoreState`, and **offer** relaunches for
   Panes marked `Relaunch` without performing any.
7. **Daemon lifecycle** — socket path, single-instance guard, log location, graceful shutdown, and a clear
   answer to "the daemon is not running".

**Verified by** (to be written):

- A test that spawns a real `claude` (or a scripted stand-in) through the real adapter, receives a real
  hook POST, and asserts the resulting `SessionTree` state — the first test that exercises the join.
- A test that a supervisor-inferred child and a hook-confirmed subagent land in the same tree with the
  right `Relation` on each, and that the confirmed one is not downgraded on a later scan.
- A test that a pty exit clears that node's attention demands.
- A protocol-level test over a real socket: connect, handshake, create a Session, write to a pty, receive
  output pushes, disconnect, reconnect, and get a replay that matches.
- A restore test: write a Session with a live process, kill the daemon, restart, and assert `Reconnected`
  for what is still there and `Lost` for what is not — with **nothing relaunched**.
- A test that the daemon survives an unwritable store: it keeps serving and reports degraded persistence.

**Exit criterion.** `turnd` starts, a scripted client over `socat` can create a Session that runs `claude`,
and a hook POST from that Agent changes what the client is told.

---

## M6 — Unified hierarchy foundation · **Implemented for the first vertical**

This milestone makes ADR-040 true below the UI before M7 builds on it.

**Delivers.** Normalised Workspace/Session/Process ownership plus one revisioned `HierarchySnapshot`;
closed Session modes; canonical checkout identity with one global fenced primary-writer claim; lossless
Agent naming and relationship confidence; background nodes independent of Pane bindings; bounded/redacted
Activity Preview; per-surface tree state; and protocol v3 with structured lease conflict/recovery.

Migration 003 is append-only and conservative: one primary checkout record per Workspace, legacy Session
assignments marked `read_only_enforced=false`, compatible legacy binding import and reconciliation flags.
It creates no lease, starts/kills/moves no process, changes no filesystem permission and never chooses the
“most recent” Session as writer. Daemon reconciliation or explicit user action is the first place authority
can be granted.

**Verified by.** All of these must be automated before the milestone closes:

1. Two concurrent acquisitions, symlink/path aliases and Workspace delete/recreate yield one canonical
   owner and a monotonically fenced generation.
2. A stale heartbeat/release cannot mutate a newer lease; `recovery_required` remains blocking.
3. Conflicting Session creation rolls back its rows and runs no init command/process/Pane side effect.
4. v2→v3 migration over multiple possibly-live Sessions creates zero leases and asks for reconciliation.
5. `HierarchySnapshot` restores Workspace → Session → Agent/Tool → Child, rejects stale revisions and keeps
   selection scoped to one stable `surface_id`.
6. A fixture-declared `Reviewer` appears under its parent with no Pane; a role such as `Explore` is not
   fabricated as its declared name.
7. ANSI/carriage-return/noisy output produces one bounded redacted preview, stores no raw source/secret and
   is stale after restart until fresh activity.
8. Quick Preview/temporary Pane close removes only a binding; the Agent and lease remain.

**Status detail.** Domain, append-only migrations, canonical checkout ownership, lease fencing/conflict
transactions, hierarchy projection, per-surface state, pane bindings, preview redaction/history and the
reproducible Reviewer vertical are implemented and tested. Legacy lease reconciliation is conservative:
ambiguous live owners enter `recovery_required` instead of receiving authority. The remaining work is
advanced management API surface (rename/correct/filter/manual order), performance measurement and the live
CLI smoke test; “types exist” is still not an exit criterion.

---

## M7 — The window · **Implemented for the first vertical**

The first window implementation shipped as a Tauri shell plus a TypeScript/`xterm.js` frontend, reached code-complete with its
own suites green, and was **rejected by the product owner on sight**. It has been deleted — `ui/`, 51
TypeScript files and 13,317 lines, and `crates/turn-ui`, 2,230 lines — and with it the tests that used to be
counted here. ADR-039 records why, what it cost and what it costs from here. The milestone is reopened rather
than marked done with an asterisk, because a deleted window is not a delivered one.

**Delivers.** The `turn-proto` v3 client (framing, handshake, request correlation, hierarchy revision/resync,
push handling and terminal lag recovery); one accessible Workspace → Session → Agent/Tool → Child tree; the
Layout renderer with split, close, resize, zoom and focus; contextual inspector and Quick Preview; logical
Attention Queue through badges/commands and `goto_next`, not a second permanent navigator; and the
permission banner showing command, `cwd` **verbatim** and risk explanation.

Also the other half of attention: performing `Effect`s (badge, highlight, sound, OS notification, focus) and
**reporting `UserContext` back** — last keystroke, foreground state, active Session, sensitive operation.
Without that report the typing guard is inert, so it is not optional polish.

**What exists now.** `crates/turn-gui` renders one accessible
Workspace → Session → Agent/Tool → Child tree; there are no persistent Session tabs, thumbnail strip or
second Agent tree. The centre is the saved user Layout, subagents stay in the background, Space opens a
semantic Quick Preview and Cmd+Enter/double-click can open a temporary Pane. Selection, pane focus and
Attention are separate. The right inspector is contextual and collapsible. Typed checkout conflicts offer
focus/read-only/worktree/cancel, first run can create a Workspace, Quick New chooses the Coding Template,
and the Attention Queue is an explicit overlay with open/snooze/mute/dismiss actions. GPU snapshots render
the real widget tree and AccessKit tests require `Tree`/`TreeItem` semantics with no duplicate `ListItem`
navigator.

**What is verified.** The cell model against a real `vt100` stream, including the cases that are easy to get
silently wrong — `a_parsed_screen_becomes_the_grid_the_client_paints`,
`a_wide_glyph_from_a_real_stream_is_not_painted_twice`,
`a_wide_cell_that_would_not_fit_is_refused_rather_than_written_as_half`,
`a_hidden_cursor_is_reported_as_absent_rather_than_as_a_position`,
`a_full_screen_program_reports_its_alternate_screen_and_its_input_modes`,
`the_indexed_palette_matches_the_xterm_cube_and_greys` — plus
`every_state_has_a_glyph_as_well_as_a_colour` and
`the_attention_colour_is_reserved_for_states_that_block_the_user`, and the two snapshots
`a_busy_desk_with_a_pending_permission` and `an_empty_window_says_so_rather_than_looking_broken`. The
snapshots are the new capability, not a formality: the first one caught two labels drawn on top of each
other, which no logic test could see.

**What is not.** Tree search/filter modes, manual ordering, user rename and audited relationship correction,
full context menus, permanent open-placement choices and IME sign-off remain. Snapshot baselines are native
GPU output and still need platform CI coverage. No manual authenticated Claude session has been accepted.

**Exit criterion.** The automated first-vertical form is met. Human live-CLI acceptance remains open.

---

## M8 — First vertical · **Automated vertical complete; live smoke pending**

The deterministic vertical is implemented twice at the boundary that matters: a daemon/store restart test
and an integration test through the real loopback Claude hook server and production normaliser. Both assert
that Reviewer is a named child with no automatic Pane, its preview is stable/redacted, a temporary Pane can
close without stopping it, the Layout stays unchanged, and the relationship/preview/process metadata
survive UI restart.

**The true remaining gap.** Nobody has yet completed the same scenario by launching an authenticated
external Claude Code binary from the packaged native window. That smoke test may expose installed-version,
credentials, signing, notification or Metal/Vulkan behaviour the deterministic proof cannot. It also must
record exactly which current hook fields Claude supplies; the test fixture's explicit `Reviewer` name/task
is a supported payload shape, not evidence that every installed Claude release emits those fields.

To be clear about the other direction too: this is not the *only* thing between the code and a working
product. M9 holds the unmeasured performance budget, the unfinished restore semantics, Linux sign-off and
packaging, and none of that is optional for something a person installs. M8 is the milestone that turns
"built" into "seen to work once".

**Delivers.** Seven scenarios, working, on both platforms:

1. **One Agent.** New Workspace, new Session from the `Coding` Template, `claude` runs, output renders,
   input works, state is correct throughout — `Running` → `NeedsPermission` → `Running` → `CompletedTurn`.
2. **Three at once.** Three Sessions block within a second. Exactly one focus change. Two badges. The
   shortcut walks all three in priority order, and the queue drains.
3. **Turn done, work continuing.** An Agent finishes a turn with `background_tasks > 0`. The notification
   says "N still running". The Session does not read as finished.
4. **The hierarchy.** One Workspace snapshot contains the main Session, Agent, declared background Reviewer
   and inferred dev server. Neither child opens a Pane. Relationship uncertainty is visible and a failure
   still makes the Session read as failed.
5. **A UI restart.** Close the window with three Agents mid-task. Reopen. All three re-attach, screens are
   rebuilt from replay, and no process was touched.
6. **A tool with no integration.** Run `gemini` (heuristic) and `make` (generic terminal) in Panes.
   `gemini` badges on a guessed state and **never** moves focus; `make` makes no claims. The Session details
   panel explains both.
7. **Checkout conflict.** A second main writer is refused before external side effects; the chooser focuses
   the owner or creates read-only/worktree. Restart and a stale heartbeat do not steal the first lease.

**Verified by.** A scripted end-to-end harness where possible, and a written manual checklist for the rest,
run on macOS and Linux before the milestone closes.

---

## M9 — Hardening · **Not started, and partly overtaken**

Nothing in this milestone has been done as a milestone, but two of its boxes have been ticked in passing:
`cargo clippy --workspace --all-targets` and `cargo fmt --all -- --check` are both clean as of 2026-08-04
(§Technical debt). Everything that needs measuring is still unmeasured, and Linux is still unrun.

**Delivers.**

- **Measurement, replacing the unmeasured budget.** Resident memory at 30 live Panes; keystroke-to-pty
  latency; output-to-glass at the 95th percentile; idle daemon CPU; and the base64 protocol overhead under a
  build firehose. Then tune `TerminalBuffer::with_capacity` per `PaneKind` if the numbers say so — a build
  log does not need an Agent conversation's scrollback.
- **Restore semantics finished.** Pid-reuse defence when re-attaching (corroborate with the stored command
  line), `RestoreBehaviour` honoured, and the relaunch offer flow.
- **Linux parity signed off** — one GPU stack checked on both backends deliberately rather than hoped for:
  the window opened and used under Vulkan, and the snapshot baselines recorded on Linux so CI compares them
  there too instead of only on macOS/Metal.
- **A clean CI run**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the full
  test suite and the `ui` job, all green on both runners. See §Technical debt: `fmt`, clippy and both test
  suites now pass locally on macOS; what is left is the `ui` job's Node version, which needs raising to 22,
  and the fact that no runner has yet reported any of it. It should all be fixed long before M9 rather than
  saved up for it.
- **Packaging**: a signed macOS bundle, a Linux artefact, `turn-hook` installed and locatable, and a
  first-run experience that works with no configuration.
- **Contract-drift monitoring**: re-record the Claude Code fixture against the current release, add a Codex
  fixture recorded live rather than shaped from documentation, and verify the two Codex assumptions still
  outstanding (§Open decisions).

**Exit criterion.** Every box in `PRODUCT.md` §7 is checked, or explicitly waived with a reason.

---

## Post-MVP

In rough order of how much they are wanted, not of how hard they are. `PRODUCT.md` §6 records why each waits
and what must not be foreclosed.

1. **tmux-backed Sessions** — closes the one honest gap in persistence: work does not survive the daemon
   exiting. Flags and node kinds already exist in the model.
2. **Codex `app-server`** (`--listen unix://PATH`, JSON-RPC) — `turn/started`, `turn/completed`,
   `thread/status/changed`, `ProcessExited`, approval requests and token usage. Richer than hooks, and
   `EventSource::SideChannel` already accommodates it.
3. **Binary output channel** — `OutputEncoding` is already negotiated in the handshake, so a length-prefixed
   side channel is additive rather than a protocol break.
4. **Cost and token dashboards** — `AgentInfo` already has `tokens_used` and `cost_usd`;
   `Capabilities::usage_events` is false today because the hooks Turn subscribes to do not carry usage.
5. **Heuristic correction UI** — "this state is wrong". `EventSource::UserCorrection` already exists at
   `Explicit` confidence, and `Inference` already carries the rule name so the UI can say why it guessed.
6. **Agent resume after a process ends** — `AgentInfo::external_id` and `resumable` are stored precisely for
   this, and it is the right answer to "bring my Agent back" rather than restoring a scrollback.
7. **Remote and SSH Sessions.**
8. **More adapters** — Gemini CLI at better than `Heuristic`, Aider, and whatever ships next.
9. **Plugin API for third-party adapters** — once `AgentAdapter` has stopped moving.
10. **Windows support.**
11. **Session sharing / multi-user** — contradicts the 127.0.0.1-only posture; needs an auth model Turn does
    not have.

---

## Open decisions

Live questions. Each names what would settle it.

ADR-040 closed two former questions: raw hook callbacks are ingress-only (Claude never attaches them to a
`TurnEvent`, `EventRepo` rejects them for every hook source, and migration 005 removes historical rows), and
Attention is one global logical queue reached through hierarchy badges/commands rather than a permanent
global or per-Workspace navigator.

| # | Question | Settled by |
| --- | --- | --- |
| 2 | ~~Does a Codex hook *handler* entry accept an `args` array?~~ **Settled live (0.146.0): it parses and is silently ignored — argv reaches the handler empty.** Payloads arrive on stdin. Routing the URL through `TURN_HOOK_URL` was the right call for a second reason: argv is world-readable. | Done. |
| 3 | ~~Does installing hooks into Codex require interactive trust on first launch?~~ **Settled live: yes.** See Risk 5 — `codex exec` silently runs nothing, the TUI blocks on a review dialog, `notify` is ungated. `HooksAndNotify` therefore cannot claim `Structured` by default; what remains is a product decision, not a question about Codex. | Done; the two product decisions moved to Risk 5. |
| 4 | **What are the `execution_mode` semantics** (`blocking` / `await`)? Left unset deliberately, because guessing risks configuring Codex to wait on Turn. | Reading Codex's source or a controlled experiment. |
| 5 | **How does the daemon defend against pid reuse when re-attaching?** The stored command line is the only corroboration and it is weak. | M9 restore hardening, tested adversarially. |
| 6 | **Where does the daemon's socket live, and what is the single-instance rule?** `turnd` now answers both in code, unverified here: `paths::resolve_socket_path` takes flag, then `TURN_SOCKET`, then `<runtime-or-data-dir>/turnd.sock`, refusing a path over 100 bytes because `sun_path` is 104 on macOS; `instance` probes for an answering daemon via the real handshake rather than trusting the socket file's existence. The window's side was settled and then deleted with it (ADR-039): the retired shell never started `turnd` — the daemon's lifetime is deliberately longer than the window's — and showed `reconnecting` with the socket path while it retried, with a backoff a connection had to survive ten seconds to spend. Those two rules are the ones to reproduce in `turn-gui`, and until they are, nothing on this side is settled. Whether *something else* should start the daemon on first run is still open. | M9 packaging for the remaining half. |
| 8 | **Per-`PaneKind` buffer bounds.** `with_capacity` takes both bounds; nothing uses them non-default yet. | M9 measurement. |
| 9 | **Does `TerminalBuffer::replay()` need scrollback, or is the visible screen enough?** Today a re-attached Pane starts with no history above the fold (ADR-023). | M8 scenario 5, with real users. |
| 10 | **How is `turn-hook` located at runtime,** and what happens on version skew with the daemon? | M9 packaging. |

---

## Risks

The biggest technical risks, with what is already done about each and what is still missing. Risks 1 and 5
were rewritten after live spikes against Claude Code 2.1.221 and codex-cli 0.146.0 on 2026-08-04; what they
now say is observed, not projected.

### 1. Hook payloads are a contract Turn does not own — **demonstrated, not hypothetical**

Claude Code and Codex will rename fields, drop events and change payload shapes between releases. Detection
breaks **silently** — no error, just a Session that stops reporting. Two live spikes found **six** such
breaks already shipped in this repo, none of which any test or build could see:

| Adapter | Wrong | Right | Symptom |
| --- | --- | --- | --- |
| Claude | `SessionStart` over HTTP | HTTP handlers are filtered out of `SessionStart` before dispatch | no session was ever resumable; `external_session_id = true` was a lie |
| Claude | `StopFailure.message` | `StopFailure.error` | every API failure rendered as one generic string |
| Claude | `Notification` catch-all | 10+ real types incl. `worker_permission_prompt`, `agent_completed` | a *permission* degraded to "waiting"; *completions* raised a demand |
| Codex | `session_start=` | `SessionStart=` | parses, fires nothing |
| Codex | `handlers=[…]` | `hooks=[…]` | parses, fires nothing |
| Codex | `to_ascii_lowercase()` fold | `SessionStart` → `sessionstart` matched no arm | a delivered payload was dropped |

*Mitigated by:* both fixtures now recorded off the wire and verified as such
(`claude-code-2.1.221.json`, `codex-cli-0.146.0.json` — session ids, transcript paths and UUIDv7 timestamps
all cross-check against artefacts still on the recording machine); contract tests labelled CAPTURED vs
DERIVED per assertion so a documented guess can never be mistaken for an observation; unknown events dropped;
adapters that never panic on malformed input (161 tests in `turn-agents`).
*Still missing:* **a CI oracle for "parsed but fires nothing".** The only real oracle found is a JSON-RPC
`hooks/list` call over `codex app-server`, which is not automated — the in-repo stand-in asserts literal
spellings, which protects against our typos and not against upstream drift. Also missing: a re-record routine
on upgrade, and any coverage beyond one version on one machine. A contract test only fires when someone runs
it, so a release between CI runs still reaches users first.

### 1a. Checkout aliasing and stale lease authority — **M6 blocker**

The catastrophic failure is two Sessions believing they exclusively own the same Git index/files. Raw path
strings alias through symlinks and spelling; a daemon can crash between side effect and commit; a stale
client can release a newer claim; deleting/recreating a Workspace can reset locally scoped generation.

*Mitigated by:* canonical-path uniqueness across Workspaces, one global monotonic fence per canonical path,
`BEGIN IMMEDIATE` acquisition, ownership checks across Workspace/Session/checkout, fenced heartbeat/release
and blocking `recovery_required` state. Migration 003 grants no lease.
*Still missing:* the concurrent/path-alias/recreate tests, daemon transaction proof before init/spawn, and a
manual recovery flow that never interprets timeout as death. M6 cannot close on schema types alone.

### 1b. Activity Preview can become a durable exfiltration/lying channel — **M6 blocker**

A terminal line may contain secrets, prompt injection, bidi/invisible text or a transient spinner that looks
like stable progress. Persisting “last lines” also recreates the misleading restored-conversation problem
ADR-036 rejected.

*Mitigated by:* semantic-source priority, control/bidi/ANSI and carriage-return normalisation, stability/noise
filtering, known-secret redaction, 20-per-node/2,000-global retention, no raw PTY/hook source and a stale-on-
restore label. High-frequency changes are snapshot state, not append-only events.
*Still missing:* adversarial SQLite/restart tests seeded with secrets and noisy TUI output, plus UI proof that
stale/provisional provenance is spoken as well as coloured.

### 1c. Protocol v3 projection drift — **M6/M7 blocker**

If hierarchy bootstrap, bounded pushes and per-surface state use different ownership/order rules, the GUI can
show the right terminal under the wrong Agent or apply one window's selection to another. Shared Rust types
do not prevent two derivations.

*Mitigated by:* one daemon-derived `HierarchySnapshot`, monotonic revision and full replacement, typed
`HierarchyKey`, structured lease conflict and server-provided relationship/preview confidence.
*Still missing:* catalogue/conversation tests for revision gaps, daemon restart, surface isolation and every
request→response/push variant; `docs/PROTOCOL.md` still has no mechanical prose-to-code check.

### 2. ~~Webview divergence between macOS and Linux~~ — **struck: the webview is gone**

This was the risk ADR-001 named as the single biggest in the stack: WKWebView on macOS against WebKitGTK on
Linux, diverging in rendering, input handling, IME behaviour and performance, with `xterm.js` inside both.

It is retired, and it is worth being precise about how, because "retired" usually means "measured". This one
was never measured. It was **deleted along with the webview** (ADR-039). Nobody ever ran the frontend on
WebKitGTK, so the divergence was never observed, quantified or ruled out — the artefact that carried the risk
no longer exists. That is a legitimate way to close a risk and a poor way to learn anything, and the reason
it is struck rather than ticked.

Two of its children did not go away and are now risks 2a and 2b.

### 2a. One GPU stack, two backends: Metal and Vulkan

`turn-gui` draws through `wgpu`, which is Metal on macOS and Vulkan on Linux. That is one renderer and one
codebase instead of two engines, which is a large reduction — but it is not zero. Text rasterisation,
blending and colour handling can differ between backends, drivers vary far more on Linux than on macOS, and a
software rasteriser (lavapipe) is a third behaviour again.

*Mitigated by:* one codebase, so a fix is a fix on both; `egui`'s own font rasterisation and tessellation
happening on the CPU, so glyph shaping is identical and only the composite differs; and CI compiling
`turn-gui` including its snapshot target on `ubuntu-latest` every push, with no system packages needed at all
(X11, Wayland, xkbcommon and Vulkan are all reached by `dlopen`).
*Still missing:* the snapshot baselines exist only for macOS/Metal, so the comparison runs on macOS only and
the workflow says so in place of skipping quietly. Nobody has opened the window on Linux. Recording a Linux
baseline under lavapipe, and measuring how far it sits from the Metal one, is the first real evidence this
risk will have — and it needs a Linux machine, which is also what M9's Linux sign-off needs.

### 2b. Accessibility and IME are now Turn's problem, and are unbuilt

The webview supplied both. A GPU-drawn window supplies neither: there is no DOM, so every accessible name
must be constructed deliberately, and text composition for CJK input, dead keys and the candidate window is
work rather than a platform service.

*Mitigated by:* `eframe` built with `accesskit`, so there is a real accessibility tree to populate rather than
nothing at all; session rows already calling `widget_info` with their state in words; and a test that names
the gap instead of hiding it.
*Still missing:* that test — `every_session_row_is_reachable_by_its_accessible_name` — is committed
**failing** and `#[ignore]`d, because the rows are painted with the raw painter rather than composed from
widgets, so `kittest`'s queries cannot see them. Until it passes, this window cannot be used with a screen
reader. IME has no code and no test at all. Both were free before and neither is now; this is the clearest
price paid for ADR-039.

### 3. Memory and throughput at the design point are unmeasured

The budget names 30 Panes across 10 Sessions with one producing build-volume output. Per-Pane byte rings are
a hard 2 MiB, so 30 Panes is ~60 MiB of ring — fine. The vt100 grid grows toward its 5,000-row cap and its
per-cell cost has never been measured here; at 80 columns that is 400,000 cells per Pane at full scrollback.
On top of that, base64 on the protocol inflates output by 33% plus a pass each way, which is irrelevant for
keystrokes and not irrelevant for a `cargo build` firehose.

*Mitigated by:* every buffer bounded rather than unbounded; `TerminalBuffer::with_capacity` taking both
bounds so they can be tuned per `PaneKind`; `MAX_OUTPUT_CHUNK_BYTES` capping a frame; backpressure that
degrades to "you lagged, here is a replay" rather than to stutter; and `OutputEncoding` already negotiated in
the handshake so a binary channel is additive.
*Still missing:* an actual measurement. It is an M9 deliverable and the numbers could force a redesign of the
output path, which is why it should not wait until M9 if a cheap benchmark can be built earlier.

ADR-039 changed the shape of this risk without shrinking it. Sending cells rather than bytes to the client
removes the base64 inflation from the pane path and removes the second parse entirely, which should help. It
adds a cost the webview did not have: a GPU frontend repaints, so the per-frame work scales with **painted
cells** rather than with bytes received, and thirty panes of dense colour cost something at 60 fps even when
nothing changed. Nothing here is measured either, and "we replaced a measured-nothing with a different
measured-nothing" is the accurate summary.

### 4. The join is the riskiest code in the system and it is unproven

One reducer must fold four independent signal sources — hook payloads, pty exits, supervisor observations,
heuristic inferences — into one authoritative `SessionTree`. A wrong correlation there does not crash; it
silently attributes a permission request to the wrong Agent, or marks the wrong node dead, and the user is
told something confidently false. Correlation keys are weak by nature: a tool's own `session_id`, an OS pid
that can be reused, a command line that can be ambiguous.

*Mitigated by:* concentrating the join in exactly one place in the daemon (a decision, not an accident); the
`Relation` ladder refusing to downgrade a confirmed link; `SessionTree::relink` refusing cycles; nodes with
no attributable parent rendering at the root rather than under a guess; and `find_by_external_id` /
`find_by_pid` already existing as the only two lookup paths.
*Still missing:* confidence in the reducer. It is now written — `turnd/src/core/events/mod.rs`, with
`events/tree.rs` and `events/exit.rs` beside it — and none of it is verified here, so this risk has moved
from "unwritten" to "unproven", which is a smaller step than it sounds. What is definitely absent is the
adversarial coverage that would retire it: out-of-order arrival, a hook for a node that died, a reused pid,
and two Sessions running the same tool in the same directory.

### 5. Codex's hook trust model — **confirmed; now a product decision, not an unknown**

Measured live. A newly configured hook is `untrusted`, and the two front ends behave completely differently:

- **`codex exec` silently runs nothing.** No warning, no error, normal exit, zero callbacks — indistinguishable
  from a broken Turn.
- **The interactive TUI blocks at startup** on *"Hooks need review — N hooks are new or changed / Hooks can run
  outside the sandbox after you trust them"* until the user picks review, trust-all, or continue-untrusted.
- **`notify` has no trust gate** — verified delivering `agent-turn-complete` with hooks left untrusted in a
  fresh `CODEX_HOME`.
- Granting writes a `trusted_hash` per handler into `$CODEX_HOME/config.toml`, keyed on the handler's command.
  **Changing the command flips it back to `modified` and re-prompts**, so `turn-hook`'s path and contents are
  part of what the user trusted.

*Mitigated by:* a first launch reporting `Wrapper` with a note naming the pending decision rather than
claiming `Structured` on faith; `ConfirmedHooksAndNotify` reporting `Structured` only once
`CodexAdapter::hooks_confirmed_live` has seen a hook payload (a `notify` payload proves nothing);
`--dangerously-bypass-hook-trust` never passed, asserted for all three transports.
*Needs a product-owner decision (two of them):*
1. **What Turn does on a first Codex launch.** The honest options are: say nothing and run degraded (a
   permission queue that silently does not exist); or tell the user, before or at launch, that granting hook
   trust in the Codex TUI is required for permissions and subagents. Turn cannot detect the difference itself.
2. **Whether re-prompting on every Turn update is acceptable.** Any change to `turn-hook`'s path invalidates
   the trust hash, so shipping a new Turn build re-triggers Codex's review dialog for every user.

*Still missing (engineering):* nothing about the trust model. The degraded path remains materially worse —
`notify` gives no permission and no subagent events.

### 6. A rejected permission in Claude Code emits nothing at all

Verified live: a human rejecting an interactive `Write` produced `PermissionRequest` and then **silence** — no
`PermissionDenied`, and no `Stop`, because rejection aborts the query and Stop hooks do not run on abort.
`PermissionDenied` fires only when an auto-mode classifier refuses (gated on
`decisionReason.classifier === "auto-mode"` in the shipped code). So Turn's pending-permission state currently
clears only on the next `UserPromptSubmit`: a user who rejects and then walks away leaves a `PERMISSION` badge
and a queue entry that are both false, for as long as they like.

*Mitigated by:* nothing yet. The state is honest about its source; it is simply stale.
*Needs a product-owner decision.* Every available remedy costs something the product has ruled out elsewhere:
a timeout invents a state transition no tool reported; a pty heuristic could see the dialog close but ADR-005
caps it at `InferredHigh`, which by design cannot clear a demand raised at `Explicit`; asking the user to
dismiss it manually puts the work back on them. Choose one, or accept the staleness and say so in the UI.

### 7. Claude Code's `SessionStart` needs a process spawn, so resume rests on one narrow path

`SessionStart` is the only source of the external session id, and Claude Code 2.1.221 refuses to deliver it
over HTTP (it is filtered before dispatch and logged only to `--debug-file`). Turn now registers that one
event as a `command` handler even on the HTTP transport — one `turn-hook` spawn per session start, everything
else still on HTTP. If a future release filters command handlers too, or renames the event, `--resume` stops
working with no other signal to fall back on. No hook payload from Claude Code carries `model` at all, so
`AgentRef.model` is permanently `None` for that tool.

*Mitigated by:* `EVENTS_WITHOUT_HTTP_DELIVERY` isolating the exception to one named event, and a contract test
that fails if a release starts sending `model`.
*Still missing:* a second route to the session id. Reading it out of `transcript_path` is possible and has not
been decided on.

### Also watching

- **`rusqlite` is synchronous.** Every store call from the async daemon must be off the reactor. Forgetting
  it stalls the event loop and will not be obvious in testing.
- **Silent failure in `turn-hook`.** Exiting 0 unconditionally is correct (ADR-026) and makes a
  misconfigured Codex Session indistinguishable from a working quiet one. `--debug` is the only signal.
- **The greedy redaction rule** will hide variables users wanted to see, with no per-variable opt-out.
- **Append-only migrations** mean a mistake in a shipped migration is permanent and can only be corrected by
  another migration.
- **`Request::WritePty` is a very wide capability.** The protocol's refusal to model approval is a good rule;
  `WritePty` is the hole it leaves, and the daemon cannot tell an approval keystroke from any other.
- **`DECISIONS.md` ADR-037 and ADR-038 assert the disproven Codex facts, inverted.** ADR-037's "what the spike
  established" says Codex wants `handlers=` and snake_case event keys and that the contract test asserts the
  config does *not* contain `,hooks=[` — the exact opposite of what the live spike found and of what the
  passing test asserts. ADR-038's title claims Codex has no turn-completion hook event; it has `Stop`, which
  fired live before `notify` with the same turn id. An engineer following either ADR would reintroduce the
  dead-integration bug. Same disproven claims in `ARCHITECTURE.md` §4.5 (:584, :589) and `PRODUCT.md` 442-456.
  These are documents, so they should be **superseded rather than edited**.
- **Six test-name citations across the docs no longer resolve** (checked mechanically against every `fn` in
  the workspace): `the_handler_list_key_is_hooks_because_handlers_fires_nothing`,
  `subscribed_event_keys_are_pascal_case_and_from_the_known_set`,
  `without_hook_trust_the_adapter_reports_wrapper_and_says_what_is_missing`,
  `notify_names_the_program_only_and_the_url_travels_in_the_environment`,
  `a_first_launch_configures_both_mechanisms_but_claims_only_what_it_can_prove`,
  `the_recorded_hook_payloads_still_carry_the_fields_the_adapter_reads`. Three of them assert the *inverted* fact,
  which is worse than a dangling link. A CI check that resolves every backticked test name in the docs would
  make this class of drift impossible; there is none.
- **`turn-store`'s database file is `0644` in a `0755` directory.** `location.rs:57` calls `create_dir_all`
  and nothing calls `set_permissions` anywhere in the crate. The store keeps every command line, cwd and
  structured event excerpt, lease and Activity Preview by design. ADR-040 removes untouched hook payloads,
  but the remaining metadata is still private and file mode remains an exposure.
- **The pty tests are load-sensitive, and more of them than was thought.**
  `turn_pty::process::a_process_sees_the_size_we_gave_it` failed once with `OpenPty(Os { code: -6 })` while
  other cargo builds saturated the machine, then passed on rerun — a third test beyond the two already known
  to flake this way. They spawn real ptys; CI parallelism will find this.
- **"Turn never relaunches" is documented and structurally true, but not test-proven.** No suite that can be
  run today asserts it: `invariants.rs` proves no *adapter* can be made to produce a relaunch, and the daemon's
  own guarantee rests on `RelaunchNode` being the only path, which lives in unverified `turnd` code.

---

## Technical debt

Concrete, verified items. Each is real today.

### Blocking a green CI

1. **`cargo clippy --workspace --all-targets -- -D warnings` passes.** Kept here rather than deleted because
   this list previously named four findings — three in `turn-core`, one in `turn-pty` — and all four have
   been fixed. Re-measured 2026-08-04 across the whole workspace: clean. One finding appeared in `turn-gui`
   while the frontend was being retired (`needless_borrows_for_generic_args` in `view.rs`) and was fixed in
   passing; a new crate is where this drifts first.
2. **`cargo fmt --all -- --check` passes.** Kept here for the same reason as clippy: this list previously
   named 30 unformatted files and 101 diff hunks, and it was the first step CI would have gone red on.
   Re-measured 2026-08-04 across the whole workspace: no diff. Keeping it that way needs the habit, not
   another fix.
3. **The snapshot tests only run on macOS.** Not a CI misconfiguration — a missing baseline, written out in
   `.github/workflows/ci.yml` rather than skipped quietly. The PNGs in `crates/turn-gui/tests/snapshots/`
   were recorded through Metal, and `egui_kittest` allows a per-pixel threshold of 0.6 with **zero** differing
   pixels by default, so they cannot be trusted against lavapipe without a measured tolerance or a second
   baseline. The Linux job still compiles the snapshot target every push, so it cannot rot, and it runs
   `turn-gui`'s logic tests. Lifting it: `sudo apt-get install -y mesa-vulkan-drivers libvulkan1` on the
   runner, `UPDATE_SNAPSHOTS=1 cargo test -p turn-gui` on a Linux machine, review the images, commit them,
   then drop the two `if:` guards. Needs a Linux machine, like §Risks 2a and M9's Linux sign-off.

### Missing artefacts

4. **No `turn-hook` install story.** It is built as a workspace binary; nothing locates it at runtime, and
   there is no version-skew check against the daemon.
5. **`docs/PROTOCOL.md` is not checked against the code by CI.** `turn-proto`'s catalogue tests keep
   `Request::expected_result` honest, but nothing asserts the prose document still matches it.

### Unused declarations

6. **`turn-core` declares `time` and `tracing` and uses neither.** `now_ms()` uses `std::time`, and there
   is not a single `tracing` call in the crate.
7. **`turn-pty` declares `serde` and `serde_json` and uses neither.** `ScreenSnapshot` and `ExitInfo` are
   deliberately not serialisable, which is fine — the dependencies should go.
8. **The root workspace declares `regex` and no crate uses it.**

### Design debt

9. **Process events are emitted only by unverified daemon code.** `EventKind::ProcessStarted`,
   `ProcessExited`, `ProcessFailed` and `ProcessSpawnedChild` are defined, serialised and tested in
   isolation in `turn-core`. They are now constructed in `turnd` (`core/spawn.rs`,
   `core/events/exit.rs`, `core/supervise.rs`), which has not been verified here — so the debt is no longer
   "nothing emits them" but "nothing has been shown to emit them correctly". `AgentQuestionAsked` is still
   generated by **no adapter**; `AgentTaskCompleted` likewise, though `turnd` derives one from a user
   correction.
10. **`Lifecycle::Reconnected` is assigned by nothing.** `load_for_restore` produces `Orphaned`.
    `turnd/src/core/restore.rs` assigns `Lost`, and its own module documentation says `Reconnected` is
    deliberately never produced there — so the state exists in the model, is reachable in the protocol, and
    is set by no code at all. That is a real gap, not an oversight to be tidied: something must eventually
    claim a successful re-attach.
11. **`RestoreBehaviour` is now read, in unverified code.** `turnd/src/core/restore.rs` branches on
    `Skip` and `Relaunch` when deciding what may be offered. The default is still `ReattachOnly`, and no
    test observed here exercises any branch.
12. **`AttentionPolicy` is cloned per deferred focus request** (seven `Vec`s). Cheap at these volumes, and
    it is a clone in a hot-ish path, kept deliberately (ADR-022).
13. **Typed ids are 12 hex characters — 48 bits, not a UUID.** Fine at this scale; they should not be
    treated as globally unique. Relatedly, `Default` on an id **mints a fresh identity**, so a stray
    `..Default::default()` silently creates a new one.
14. **Resolved: raw hook callbacks never cross the durable boundary.** Claude emits only typed facts;
    `EventRepo` discards raw data from every hook source; migration 005 clears older rows; and
    `turn-store/tests/secrets_never_reach_the_disk.rs` scans SQLite plus its WAL for free text deliberately
    chosen not to match any credential redactor. Non-hook diagnostic notes remain redacted and persistent.
15. **The heuristic marker lists are English** and taken from the shapes today's CLIs render. A localised or
    restyled CLI stops being detected with nothing failing — the Session just goes quiet.
16. **`Session::duplicate` names the copy `"{name} (copy)"`** in English, in the domain layer, and the UI
    that owns presentation now exists — so this is a real duplication of responsibility rather than a
    prospective one.
