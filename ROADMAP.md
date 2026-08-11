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
`wgpu`). ADR-039 records the decision, its cost and its downsides. The window milestone was consequently
reopened as M7, rebuilt natively, and is now complete for the functional v0.1.0 baseline.

There is one Rust test runner and one lockfile. The reproducible source of truth is
`cargo test --workspace --all-targets -- --test-threads=1`; this roadmap no longer hard-codes a total that becomes false
whenever a regression test lands. PTY and loopback hook tests use real operating-system resources and are
run serially for the release audit.

The upgraded first vertical now runs through the production daemon, store, protocol projection and GUI
model: Workspace and fenced main Session, explicit Reviewer spawn, stable preview, Quick Preview,
temporary Pane close without process termination, and UI reconnect to the same live daemon. Separate
daemon-restart coverage restores durable state as `Orphaned`/`Lost` and proves no relaunch. Another integration test sends
the event through the real loopback Claude hook transport and production normaliser. The separate packaged
acceptance also passed against authenticated Claude Code 2.1.226, including the UI-only close/reopen boundary;
the exact observations and reproducible harness are in `docs/REVIEWER_ACCEPTANCE.md`.

The hardening pass also proves two authority boundaries that the happy-path vertical did not: only one
daemon may own a canonical data directory even when configured with another socket, and ambiguous or
out-of-order worker Attention remains scoped to its authenticated hook parent and external id across
restart instead of being attributed or resolved session-wide.

| Milestone | Delivers | Status |
| --- | --- | --- |
| M0 — Domain | `turn-core`: entities, two-axis state, event vocabulary, attention | **Done** |
| M1 — Terminal substrate | `turn-pty`: ptys, bounded buffers, supervision | **Done** |
| M2 — Agent integration | `turn-agents` + `turn-hook`: adapters, hook server, heuristics, registry | **Done** |
| M3 — Persistence | `turn-store`: SQLite, migrations, redaction, eight repositories | **Done** |
| M4 — Protocol | `turn-proto`: framing, requests, responses, pushes, view models | **Done** |
| M5 — The daemon | `turnd`: assembles everything, owns the ptys | **Done for v0.1.0**; deterministic and authenticated packaged verticals passed |
| M6 — Unified hierarchy foundation | ADR-040: checkout leases, Agent/View split, safe previews, protocol v4 | **Done for v0.1.0**; hierarchy, safety and lifecycle hardening landed |
| M7 — The window | Native Rust on the GPU: one hierarchy, user-chosen panes, inspector, effects | **Done for v0.1.0**; tree management, terminal UX and accessibility contract landed |
| M8 — First vertical | One Session and background Reviewer, end to end | **Done**; packaged Claude Code 2.1.226 run and UI reconnect passed |
| M9 — Hardening | Measurement, restore semantics, packaging and release gates | **Functional baseline done**; public distribution and broad platform sign-off continue post-MVP |

M6 blocks incompatible M7/M8 UI work. Its exit proof is the reproducible
`Workspace → main Session+lease → Claude fixture → Reviewer background node → normalised preview → Quick
Preview → temporary Pane → close without stop → reconnect the UI` sequence in
`docs/UNIFIED_HIERARCHY_UPGRADE.md`: the live-runtime part restarts only the UI; a separate daemon restart
proves metadata recovery and honest loss without claiming PTY reattachment.

---

## M0 — Domain layer · **Done**

**Delivers.** `turn-core`: typed ids; `Workspace`, `Session`, `ProcessNode`/`SessionTree`, `Layout`/`Pane`,
`Template`; the two-axis `Lifecycle` × `Turn` model with a derived `DisplayState`; the `TurnEvent`
vocabulary with the `Confidence` ladder; and the attention subsystem — policy, queue, focus governor,
manager. No I/O, no clock reads inside logic.

**Verified by.** Reproduce with `cargo test -p turn-core -- --test-threads=1`. The tests that pin the
product's guarantees rather than its mechanics include:

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

**Verified by.** Reproduce with `cargo test -p turn-pty -- --test-threads=1`; the suite uses real ptys and
real processes rather than mocks. Load-bearing cases include:

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

**Verified by.** Reproduce with `cargo test -p turn-agents -- --test-threads=1` and
`cargo test -p turn-hook -- --test-threads=1`. The contract tests are the load-bearing ones, because hook
payloads are a contract Turn does not own, and both fixtures are now recorded off the wire:

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
- `invariants::*` — product-rule tests: a heuristic cannot steal focus, no payload can claim a
  permission was allowed, no payload becomes something to run, hierarchy only ever comes from a tool's own
  report, and an agent cannot name its session after a command-line flag
- `server::tests` — including a real HTTP client over a real socket, an unknown token refused and counted,
  and the server continuing to answer agents after its receiver is dropped

---

## M3 — Persistence · **Done**

**Delivers.** `turn-store`: the `Store` facade (`open_default`, `open_in`, `open_at`, `open_in_memory`),
schema versioned in SQLite's own `user_version` with append-only migrations and a loud refusal of any
database from a newer build, WAL and enforced foreign keys, `codec` for tag-vs-JSON columns, `redact` for
secret hygiene, `location` for platform paths, and eight repositories (workspace, session, node, event,
attention, template, settings, hierarchy) using `ON CONFLICT DO UPDATE` rather than `REPLACE`.
Every free-text repository route now builds a redacted copy before SQL; structural ids/FKs remain stable
and credential-shaped Workspace/checkout paths are rejected rather than rewritten. Byte-level coverage
plants one token across Workspace, Session, Layout/Pane, Template, process/Agent, settings, Attention,
Preview and event fields and scans both database and WAL.

Local-data governance is now built as well: the schema has a closed redacted export catalogue, future files
are surfaced as unclassified rather than omitted, Settings controls event/preview/terminal/log retention,
and authenticated scope operations cover report, export, deletion and compaction. Installation-wide purge
is an offline lock-protected command which preserves checkout work. Turn has no telemetry endpoint. Reproduce
the privacy contract with `make privacy-acceptance`; see `docs/PRIVACY.md`.

The line that matters most: `SessionRepo::load_for_restore` **downgrades anything stored as running to
`Lifecycle::Orphaned`**, because a stored `Alive` only ever meant "alive when we last wrote".

**Verified by.** Reproduce with `cargo test -p turn-store -- --test-threads=1`. The integration tests are
named after the promises:

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

**Verified by.** Reproduce with `cargo test -p turn-proto -- --test-threads=1`. The catalogue-level and
conversation-level tests matter most:

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

## M5 — The daemon · **Built for the automated vertical**

`main.rs` parses options, initialises logging, resolves a `Config` and calls `turnd::start`; the library
declares `config`, `paths`, `instance`, `logging`, `options`, `error`, `server` and `core`, the last split
into `spawn`, `supervise`, `restore`, `events`, `requests`, `views`, `attention`, `clients`, `command` and
`output`. The release command reproduces the current count rather than freezing one here; integration tests
live in `tests/{desk,agents,surface,restart,attention,binary,cells}.rs`.

**Delivers.**

1. **The reducer.** One place takes a `TurnEvent` from the hook
   server, an `ExitInfo` from a `PtyProcess`, an `ObservedProcess` from the supervisor and an `Inference`
   from `OutputHeuristic`, and folds all four into one authoritative `SessionTree`. Hook external ids,
   observed pids and declared parent/child relationships are correlated here rather than in the GUI.
2. **Pty facts as events.** `ProcessStarted`, `ProcessExited`, `ProcessFailed` and
   `ProcessSpawnedChild` are emitted through the same reducer as adapter events.
3. **The unix socket server**, speaking `turn-proto` — accept, handshake, per-connection request loop,
   push fan-out, and the lagged-subscriber replay path.
4. **The session lifecycle** — create from a Template, materialise Panes into `PtyProcess`es via
   `AdapterRegistry::select` and `AgentAdapter::prepare`, register with the `HookServer`, tear down, and
   delete the scratch directory.
5. **Store integration** off the reactor. `rusqlite` is synchronous; every call goes through a blocking
   boundary or a dedicated writer, with the daemon owning ordering.
6. **Restore.** Load with `load_for_restore`, corroborate orphaned pids against the process table, compute
   `RestoreState`, and **offer** relaunches for Panes marked `Relaunch` without performing any. A UI restart
   over the still-running daemon instead reuses the live PTY and replay stream.
7. **Daemon lifecycle** — socket path, a canonical-data-directory process lock acquired before SQLite,
   independent socket ownership checks, log location, graceful shutdown, crash recovery, and a clear answer
   to "the daemon is not running".

**Verified by.** `tests/agents.rs::the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart`
crosses the real loopback hook server and production Claude normaliser; `tests/desk.rs` covers PTY input,
output and reconnect replay; `tests/restart.rs` proves a daemon restart relaunches nothing; the child-process,
exit/attention and socket lifecycle cases have dedicated integration tests. An authenticated external Claude
Code binary also passed from the packaged native window, followed by a UI close/reopen against the surviving
daemon and PTY. `docs/REVIEWER_ACCEPTANCE.md` keeps that evidence separate from the deterministic CI floor.

---

## M6 — Unified hierarchy foundation · **Done for v0.1.0**

This milestone makes ADR-040 true below the UI before M7 builds on it.

**Delivers.** Normalised Workspace/Session/Process ownership plus one revisioned `HierarchySnapshot`;
closed Session modes; canonical checkout identity with one store-wide fenced primary-writer claim; lossless
Agent naming and relationship confidence; background nodes independent of Pane bindings; bounded/redacted
Activity Preview; per-surface tree state; and protocol v3 with structured lease conflict/recovery.

Migration 003 is append-only and conservative: one primary checkout record per Workspace, legacy Session
assignments marked `read_only_enforced=false`, compatible legacy binding import and reconciliation flags.
It creates no lease, starts/kills/moves no process, changes no filesystem permission and never chooses the
“most recent” Session as writer. Daemon reconciliation or explicit user action is the first place authority
can be granted.

Later append-only corrections keep the same fail-closed rule: migration 005 erases historical raw hook
bodies, migration 006 re-resolves legacy checkout identity without adopting a writer, and migration 007
persists parent/external-id correlation for node-less Attention without inventing a node. Migration 008
records demand kind and whether postmortem Attention truthfully survives its runtime owner exiting.
Migration 009 performs the one-time, retryable physical purge that applies the current durable-redaction
boundary to legacy free text and rebuilds/truncates SQLite only after structural identities pass validation.

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
   retains its original timestamp after restart; a distinct recovered/stale UI marker remains open.
8. Quick Preview/temporary Pane close removes only a binding; the Agent and lease remain.

**Status detail.** Domain, append-only migrations, canonical checkout ownership, lease fencing/conflict
transactions, hierarchy projection, per-surface state, pane bindings, preview redaction/history and the
reproducible Reviewer vertical are implemented and tested. Legacy lease reconciliation is conservative:
ambiguous live owners enter `recovery_required` instead of receiving authority, and a daemon restart fences
every unreleased lease before loading Sessions. Durable Attention entries keep their identity, age, snooze,
acknowledgement, ordering and exact node-or-parent/external-id correlation scope, while Preview history is
returned newest-first. A canonical data-directory process lock also prevents a differently configured
daemon from reaching SQLite and fencing the live owner. The remaining work is the
explicit migration-reconciliation flow, advanced management API surface (rename/correct/filter/manual
order), performance measurement and the live CLI smoke test; “types exist” is still not an exit criterion.
Template-origin conflicts now retain the Template id and inputs through typed read-only/worktree requests;
the daemon, not `TemplateSummary`, re-instantiates the complete Layout/env/Attention/tmux/naming contract.
Read-only now launches shells, Agent panes, init commands and descendants under an inherited macOS Seatbelt
write guard; it keeps commands stopped whenever that technical guard is unavailable. Worktree maps primary
absolute cwd values repository-relatively.

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
semantic Quick Preview and explicit open offers remembered replace/right/below/temporary placement.
Temporary panes can be promoted without restarting their process; active panes can be moved, duplicated,
retyped, floated with persistent geometry and docked again. Selection, pane focus and Attention are
separate. The right inspector is contextual and collapsible. Typed checkout conflicts offer
focus/read-only/worktree/cancel, first run can create a Workspace, Quick New chooses the Coding Template,
and the Attention Queue is an explicit overlay with open/snooze/mute/priority/dismiss actions. Attention
policies are editable per field through Global → Workspace → Template → Session settings; OS sound,
notification and user-configured custom actions all cross a tested UI boundary. GPU snapshots render
the real widget tree and AccessKit tests require `Tree`/`TreeItem` semantics with no duplicate `ListItem`
navigator. Named modal Dialog/AlertDialog nodes constrain focus; separate live regions announce
connection, application state, selection, pane focus and attention. High contrast is measured, reduced
motion removes transitions/spinners/cursor blink, macOS preferences are inherited live when unset, and
the 300% zoom/native-minimum layout retains both hierarchy and terminal access.

**What is verified.** The cell model against a real `vt100` stream, including the cases that are easy to get
silently wrong — `a_parsed_screen_becomes_the_grid_the_client_paints`,
`a_wide_glyph_from_a_real_stream_is_not_painted_twice`,
`a_wide_cell_that_would_not_fit_is_refused_rather_than_written_as_half`,
`a_hidden_cursor_is_reported_as_absent_rather_than_as_a_position`,
`a_full_screen_program_reports_its_alternate_screen_and_its_input_modes`,
`the_indexed_palette_matches_the_xterm_cube_and_greys` — plus
`every_state_has_a_glyph_as_well_as_a_colour` and
`the_attention_colour_is_reserved_for_states_that_block_the_user`. The snapshot integration target has 105
tests, including 65 committed PNG baselines such as `a_busy_desk_with_a_pending_permission` and
`an_empty_window_says_so_rather_than_looking_broken`. The snapshots are a capability, not a formality: the first one caught two labels drawn on top of each
other, which no logic test could see.

**What is not.** Broad packaged VoiceOver, Orca and current input-method sign-off remains release hardening.
Snapshot baselines are native GPU output and still need reviewed Linux platform coverage. The functional
keyboard, AccessKit, zoom, contrast, motion and IME contract is automated, and the packaged authenticated
Claude session has passed.

**Exit criterion.** Met for the functional v0.1.0 native window; the broader platform matrix remains post-MVP.

---

## M8 — First vertical · **Done**

The deterministic vertical crosses the production daemon/store model and, separately, the real loopback
Claude hook server and production normaliser. The live-runtime proof reconnects a replacement UI to the
same daemon and asserts that Reviewer is a named child with no automatic Pane, its preview is
stable/redacted, a temporary Pane can close without stopping it and Layout stays unchanged. Daemon-restart
tests make the narrower claim that relationship/preview/process metadata survives as `Orphaned`/`Lost` and
nothing is relaunched.

The same scenario passed against authenticated Claude Code 2.1.226 from the packaged native window. It
recorded the real hook shapes and terminal modes, verified a uniquely named Reviewer with no invented PID or
Pane, closed a temporary Pane without changing Layout, closed only the GUI, and reopened against the same
daemon, socket, PTY, Session and write lease. `docs/REVIEWER_ACCEPTANCE.md` records the exact environment and
deviations from Claude Code 2.1.224.

**Acceptance target.** Seven scenarios define completion. Their deterministic core is automated and the
packaged external-Claude vertical has passed:

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

**Verified by.** The opt-in packaged live harness and its passing run record, plus deterministic Agent,
Attention, hierarchy, terminal, lease, restart and snapshot suites in the ordinary release gate.

---

## M9 — Hardening · **Functional v0.1.0 baseline done**

Authority fencing, atomic event checkpoints, restore diagnostics, durable redaction, measured performance,
local privacy controls, accessibility contracts, local quality gates and the macOS release/update machinery
have landed. `make mvp-acceptance` is the single serial release gate and `docs/MVP_ACCEPTANCE.md` maps every
global criterion to its evidence.

**Delivered for the functional baseline.**

- 30 Workspaces, 30 Sessions and 120 Processes are measured with enforced response, memory, queue, cadence
  and storage budgets; hardware and before/after profiles live in `docs/PERFORMANCE.md`.
- Restore remains honest: UI replacement reuses the live daemon and PTYs, daemon restart relaunches nothing,
  and no path fabricates `Reconnected` without ownership of the PTY master.
- macOS CI builds the final three-sibling bundle topology on every PR. Version/protocol skew is rejected,
  compatible UI updates preserve the daemon, and incompatible daemon replacement is deferred while PTYs live.
- SQLite and private files have complete redacted inventory/export, scoped deletion, bounded retention,
  lock-protected offline purge and explicit zero-telemetry reporting.
- macOS and Ubuntu enforce format, all-target Clippy, the applicable full test surface and release binaries;
  macOS additionally verifies the ad-hoc bundle and GPU snapshot baselines.

**Still post-MVP.** Publishing the first credentialed Developer ID/notarized tag, clean-machine channel
operation, a Linux archive/update channel and reviewed Linux GPU baselines remain distribution work. Broad
packaged VoiceOver/Orca/current-IME sign-off remains platform acceptance. Surviving daemon death with a live
PTY, tmux and the other scopes listed under Post-MVP are not silently claimed by v0.1.0.

**Exit criterion.** Met for the functional baseline: every required row in `PRODUCT.md` §7 is checked, and
every unchecked row is explicitly classified outside issue #21's MVP scope.

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
| 6 | ~~**Where does the daemon live, who starts it, and what is the single-instance rule?**~~ **Settled:** `turn` resolves one absolute data-dir/socket pair, reuses a reachable endpoint or starts a detached sibling `turnd`; a debug source build uses its fixed Cargo workspace. The daemon's canonical data-directory lock — not socket occupancy — is the store/PTY ownership authority, so concurrent windows and socket aliases cannot create two owners. See ADR-042. | Done. |
| 8 | **Per-`PaneKind` buffer bounds.** `with_capacity` takes both bounds; nothing uses them non-default yet. | M9 measurement. |
| 9 | **Does `TerminalBuffer::replay()` need scrollback, or is the visible screen enough?** Today a re-attached Pane starts with no history above the fold (ADR-023). | M8 scenario 5, with real users. |
| 10 | ~~**How is `turn-hook` located and versioned at runtime?**~~ **Settled:** it is a packaged sibling beside `turnd`, never an arbitrary `PATH` result; source bootstrap builds both together. Packaging executes `--build-info` for all three siblings and refuses any version/protocol skew before signing. | Done. |

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
adapters that never panic on malformed input (covered by the reproducible `turn-agents` suite).
*Still missing:* **a CI oracle for "parsed but fires nothing".** The only real oracle found is a JSON-RPC
`hooks/list` call over `codex app-server`, which is not automated — the in-repo stand-in asserts literal
spellings, which protects against our typos and not against upstream drift. Also missing: a re-record routine
on upgrade, and any coverage beyond one version on one machine. A contract test only fires when someone runs
it, so a release between CI runs still reaches users first.

### 1a. Checkout aliasing and stale lease authority — **hardened; recovery UX open**

The catastrophic failure is two Sessions believing they exclusively own the same Git index/files. Raw path
strings alias through symlinks and spelling; a daemon can crash between side effect and commit; a stale
client can release a newer claim; deleting/recreating a Workspace can reset locally scoped generation.

*Mitigated by:* canonical-path uniqueness across Workspaces and one monotonic fence per canonical path
inside the canonical data directory, plus a uid-scoped host lock keyed by checkout device/inode across
deliberately separate data directories,
`BEGIN IMMEDIATE` acquisition, ownership checks across Workspace/Session/checkout, fenced heartbeat/release,
blocking `recovery_required`, a canonical-data-directory process lock established before SQLite/restore, and
canonical Session/Pane cwd containment repeated at the final PTY boundary. Adversarial tests cover concurrent
aliases, delete/recreate generations, stale release, transaction rollback, same-data/different-socket daemons,
symlink data-dir aliases, cross-data-dir checkout aliases, daemon loss with a surviving writer, independent
Git worktrees, explicit release, absolute/`..`/symlink cwd escapes and worktree→primary launches.
Migration 003 grants no lease; migration 006 trusts no pre-existing one.

*Still missing:* the audited product flow that clears migration-006 reconciliation after the user proves the
old writer stopped. Main-checkout/worktree cwd containment is not an OS sandbox. Read-only processes now add
a macOS path-scoped Seatbelt guard for the checkout and external Git metadata, while Linux remains fail-closed
with process launch disabled; broader credential/network/service isolation remains separate. The
local-filesystem `flock` is an
advisory boundary between cooperating Turn daemons, not protection from the same user deliberately replacing
the lock inode.

### 1b. Activity Preview can become a durable exfiltration/lying channel — **hardened; recovered-state UX open**

A terminal line may contain secrets, prompt injection, bidi/invisible text or a transient spinner that looks
like stable progress. Persisting “last lines” also recreates the misleading restored-conversation problem
ADR-036 rejected.

*Mitigated by:* semantic-source priority, control/bidi/ANSI and carriage-return normalisation, stability/noise
filtering, known-secret redaction, 20-per-node/2,000-global retention, no raw PTY/hook source, newest-first
history and bounded snapshot updates rather than append-only byte events. Adversarial store/restart tests
prove hook bodies and seeded secrets do not survive SQLite/WAL migration; GUI tests and snapshots prove
provisional confidence is expressed in words as well as colour.
*Still missing:* a distinct recovered/stale marker for a persisted Preview whose original timestamp predates
the current daemon, plus manual assistive-technology acceptance of that wording.

### 1c. Protocol v3 projection drift — **hardened; documentation oracle open**

If hierarchy bootstrap, bounded pushes and per-surface state use different ownership/order rules, the GUI can
show the right terminal under the wrong Agent or apply one window's selection to another. Shared Rust types
do not prevent two derivations.

*Mitigated by:* one daemon-derived `HierarchySnapshot`, monotonic revision and full replacement, typed
`HierarchyKey`, structured lease conflict and server-provided relationship/preview confidence. Catalogue,
conversation, restart, delayed-snapshot and per-surface tests now cover request/response/push variants,
revision recovery and private selection state.
*Still missing:* a mechanical prose-to-code check for `docs/PROTOCOL.md`, plus long-running multi-window soak
coverage beyond the deterministic revision-gap cases.

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

### 2b. Accessibility and IME are now Turn's problem

The webview supplied both. A GPU-drawn window supplies neither: there is no DOM, so every accessible name
must be constructed deliberately, and text composition for CJK input, dead keys and the candidate window is
work rather than a platform service.

*Mitigated by:* `eframe` built with AccessKit and automated tests that require the unified navigator to
expose one `Tree`, reachable `TreeItem`s at every hierarchy level and no duplicate legacy `ListItem`
navigator. State and confidence have words/glyphs in addition to colour; live regions separate state,
selection, pane focus and attention; custom sheets expose modal Dialog/AlertDialog roles. A measured
high-contrast palette, desktop-aware reduced motion, 300% zoom acceptance and terminal commit/preedit tests
are reproduced by `make accessibility-acceptance`.
*Still missing:* recorded packaged VoiceOver/Orca and real input-method runs from
`docs/ACCESSIBILITY_ACCEPTANCE.md`. Structural AccessKit and deterministic IME coverage are necessary but
do not prove the current platform bridge and assistive-technology releases end to end. Both were free
before and are now Turn's maintenance responsibility.

### 3. Memory and throughput at the design point are measured and regression-gated

The harness models 30 Workspaces, 30 active/recent Sessions, 120 relevant Processes and a noisy 40×120
terminal. Per-Pane byte rings remain a hard 2 MiB; terminal/image storage has a deterministic 600 MiB ceiling,
GUI queues are bounded, and viewport-lazy row construction caps expensive work. The optimised reference run
measured a 32 MiB process peak RSS, 320 µs Session-switch p95 and 55 µs output-apply p95.

*Mitigated by:* every buffer bounded rather than unbounded; `TerminalBuffer::with_capacity` taking both
bounds so they can be tuned per `PaneKind`; `MAX_OUTPUT_CHUNK_BYTES` capping a frame; backpressure that
degrades to "you lagged, here is a replay" rather than to stutter; and `OutputEncoding` already negotiated in
the handshake so a binary channel is additive.
*Still missing:* long-running profiler and compositor evidence across GPU/driver combinations. The release
gate measures and fails the production model/protocol/UI application path, not photons or idle energy use.

ADR-039 changed the shape of this risk without shrinking it. Sending cells rather than bytes to the client
removes the base64 inflation from the pane path and removes the second parse entirely, which should help. It
adds a cost the webview did not have: a GPU frontend repaints, so the per-frame work scales with **painted
cells** rather than with bytes received. The deterministic harness now guards settle frames and viewport
work; platform compositor profiling remains a narrower post-MVP measurement.

### 4. The join is the riskiest code in the system — **implemented and adversarially covered**

One reducer must fold four independent signal sources — hook payloads, pty exits, supervisor observations,
heuristic inferences — into one authoritative `SessionTree`. A wrong correlation there does not crash; it
silently attributes a permission request to the wrong Agent, or marks the wrong node dead, and the user is
told something confidently false. Correlation keys are weak by nature: a tool's own `session_id`, an OS pid
that can be reused, a command line that can be ambiguous.

*Mitigated by:* concentrating the join in exactly one place in the daemon; the `Relation` ladder refusing to
downgrade a confirmed link; `SessionTree::relink` refusing cycles; nodes with no attributable parent staying
unassigned; and external-id/PID lookup remaining daemon-owned. Recorded-id routing, a real hook-transport
Reviewer vertical, id-less single-child correlation, ambiguous multi-child attribution, sibling Attention
preservation, out-of-order explicit worker ids, two independent parents in one Session, restart durability,
dead-process cleanup and recycled-PID store queries all have regression tests. An id-less worker event is
`inferred_high` only with one live candidate; otherwise it remains node-less/`unknown`. An unknown explicit
id never falls through to a different unique child; its queue identity is the authenticated parent plus
external id, and a resume cannot clear another parent's demand.
*Still missing:* broader adversarial sequences for a hook arriving after its node died, PID reuse across two
live Sessions and prolonged same-tool/same-directory workloads. These are hardening gaps, not permission to
invent a relationship today.

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
- **Codex contract drift remains an upstream risk, not a known local inversion.** ADR-037 and ADR-038 now
  record the live facts: the handler list key is `hooks`, event keys are PascalCase, `Stop` exists but is
  trust-gated, and `notify` carries the first-run turn boundary. Their cited contract tests resolve and pin
  the literal spellings. Re-capturing fixtures on a Codex upgrade is still manual, so a future upstream
  rename can make a Session quiet while the current local suite stays green.
- **Documentation-test citation drift has no automated guard.** The six citations that triggered this
  finding resolve again, but a CI job does not yet parse backticked test names and compare them with
  `cargo test -- --list`. A renamed test can therefore leave prose stale without breaking CI.
- **Store file permissions are closed on supported Unix platforms.** The data directory is forced to
  `0700`, SQLite and sidecars to `0600`, and the on-disk security suite checks the real files. Keep the test:
  a relaxed mode would expose commands, cwd, lease metadata and Activity Previews even though raw hooks are
  excluded.
- **The pty tests are load-sensitive, and more of them than was thought.**
  `turn_pty::process::a_process_sees_the_size_we_gave_it` failed once with `OpenPty(Os { code: -6 })` while
  other cargo builds saturated the machine, then passed on rerun — a third test beyond the two already known
  to flake this way. They spawn real ptys; CI parallelism will find this.
- **Automatic relaunch remains forbidden and is now test-proven.** Restart may offer recovery metadata but
  never launches by itself; only the explicit `RelaunchNode` request crosses the launch boundary. Keep the
  daemon restart and protocol conversation regressions because this is a safety invariant, not a UI choice.

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

4. **The Linux sibling package is not yet a distributed artefact.** The macOS contract is complete:
   the three binaries are version checked, sealed into a hardened `Turn.app`, notarized by the tag workflow,
   published for arm64/Intel and consumed through daemon-safe channel manifests. Linux still has builds and
   tests but no equivalent archive/update channel.
5. **`docs/PROTOCOL.md` is not checked against the code by CI.** `turn-proto`'s catalogue tests keep
   `Request::expected_result` honest, but nothing asserts the prose document still matches it.

### Unused declarations

6. **`turn-core` declares `time` and `tracing` and uses neither.** `now_ms()` uses `std::time`, and there
   is not a single `tracing` call in the crate.
7. **`turn-pty` declares `serde` and `serde_json` and uses neither.** `ScreenSnapshot` and `ExitInfo` are
   deliberately not serialisable, which is fine — the dependencies should go.
8. **The root workspace declares `regex` and no crate uses it.**

### Design debt

9. **Process-event emission is verified; two semantic events still lack an adapter source.**
   `ProcessStarted`, `ProcessExited`, `ProcessFailed` and `ProcessSpawnedChild` are constructed in `turnd`
   (`core/spawn.rs`, `core/events/exit.rs`, `core/supervise.rs`) and exercised by unit plus daemon integration
   suites. `AgentQuestionAsked` is still generated by **no adapter**; `AgentTaskCompleted` likewise, though
   `turnd` derives one from a user correction.
10. **`Lifecycle::Reconnected` is assigned by nothing.** `load_for_restore` produces `Orphaned`.
    `turnd/src/core/restore.rs` assigns `Lost`, and its own module documentation says `Reconnected` is
    deliberately never produced there — so the state exists in the model, is reachable in the protocol, and
    is set by no code at all. That is a real gap, not an oversight to be tidied: something must eventually
    claim a successful re-attach.
11. **`RestoreBehaviour` is read and tested.** `turnd/src/core/restore.rs` branches on `Skip` and `Relaunch`
    only to decide what may be offered; `a_restart_relaunches_nothing_even_for_a_pane_that_says_it_is_safe_to`
    proves restart performs no launch, and the protocol conversation proves only the user can accept an
    offered relaunch. The default remains `ReattachOnly`.
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
