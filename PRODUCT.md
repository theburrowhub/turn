# Turn — Product

**Run agents in parallel. Step in when it's your turn.**

Status of this document: it describes the accepted product and distinguishes automated evidence from
manual release acceptance. Every claim marked as met is backed by a named test in this workspace,
including daemon and native-GUI integration suites. Counts are deliberately not frozen in prose; reproduce
the current evidence rather than trusting an old survey:

```sh
cargo test --workspace --all-targets -- --test-threads=1
```

See `ROADMAP.md` for sequencing and `ARCHITECTURE.md` for how the pieces fit — its §0 holds the
authoritative per-crate status table.

---

## 1. Executive summary

Turn is a desktop terminal workspace for running, organising and supervising AI coding agents on
macOS and Linux.

The premise is narrow and specific. Coding agents are now good enough that one person can usefully
run five or ten of them at once, on different tasks, in different repositories. The bottleneck is no
longer the agents' capability; it is the human's ability to notice which one needs them, and when.
Every agent is a long-running interactive process that periodically stops and waits — for an answer,
for an approval, for a decision — and every second it spends blocked is a second of parallelism
thrown away. Meanwhile the same agent finishes a hundred things that need no human at all, and each
of those is a chance to interrupt someone who did not need interrupting.

Turn's job is to be correct about that distinction. It runs each agent in a real pty inside a
persistent Workspace, organises work into Sessions, projects Workspace, Session, Agent and tool identity
through one persistent hierarchy, and maintains one ordered Attention Queue whose top item is the thing
the user should look at next. It tells the user
when it is their turn, and otherwise stays out of the way.

Turn is not an agent, not a model client, and not a chat interface. It supervises the agent CLIs the
user already installs and pays for — Claude Code, Codex CLI, Gemini CLI, and any other interactive
terminal program — as they exist today, without asking their vendors for anything.

---

## 2. The problem

Someone running several coding agents in a terminal today has three unpleasant options, and in
practice uses all three at once.

**Tabs.** One agent per terminal tab or tmux window. This works up to about three. Past that, the
tab bar stops being a map and becomes a memory test: which tab was the refactor, which was the flaky
test, which one is waiting on you, which one died forty minutes ago. Nothing in the tab strip
distinguishes "thinking hard" from "blocked on a yes/no question", so the only way to find out is to
visit each one in turn. Visiting them all is the exact cost the parallelism was supposed to avoid.

**Notifications.** Wire up every agent to post an OS notification when it stops. This surfaces the
blocked ones, at the price of a notification stream that is mostly noise: an agent that finishes
twenty tool calls and asks nothing produces twenty notifications, none of which require action. The
predictable outcome is that notifications get muted, at which point the blocked agents are invisible
again — and worse than before, because now the user believes they have a system.

**Polling.** Keep glancing. This is what most people actually do, and it is where the time goes.

Underneath all three is a modelling failure that no amount of UI polish fixes: existing tools collapse
*the process is running* and *the agent owes me a reply* into a single notion of "busy" or "done". They
are different axes and they change independently. Claude Code can finish its turn while a `cargo test`
it launched keeps running for another two minutes. A shell can stay alive for a week without ever
owing anybody anything. An agent can crash while its last reported state was "waiting for you", and
then sit in a queue forever claiming to await a human who can no longer help it. Tools that collapse
the two axes report "done" while work continues, and "busy" while an agent sits blocked.

Turn's thesis is that if you model those two axes separately, rank the resulting demands honestly, and
put a hard governor in front of anything that moves the user's viewport, the parallel workflow stops
being exhausting.

---

## 3. Product principles

These are load-bearing. Each one shows up as a constraint in the code, and each one has tests that
exist specifically to stop a future change from quietly violating it.

### 3.1 Workspace is the navigation root; Session is the unit of work

Not the tab, not the window, not the pane. A Session is one task or one run: a name in the user's own
words, a working directory, a Layout of Panes, a process tree, an Attention policy and a history. It
belongs to a Workspace, which is the persistent project that outlives it.

This matters because tasks are what users think in, and because the interesting states are per-task,
not per-window. "The auth refactor needs me" is useful. "Pane 4 needs you" is not. It also gives
duplication a clear meaning: duplicating a Session copies the shape, the settings and the tags, and
deliberately copies none of the live processes (`model::session::tests::duplicating_a_session_keeps_
the_shape_and_drops_the_processes`).

Navigation begins one level above it. The accepted hierarchy is Workspace → Session → Agent/Tool →
Child, shown once in the left tree. A Workspace's primary checkout has at most one active exclusive writer;
concurrent work is technically read-only where viable or isolated in a worktree. AgentNodes live
independently from Panes, so a subagent can
run, preview, ask for attention and finish without changing the centre Layout. ADR-040 and
`docs/UNIFIED_HIERARCHY_UPGRADE.md` define the detailed domain, migration and interaction contract.

### 3.2 Agents form a hierarchy, and Turn never invents one

A main Agent spawns subagents. Agents spawn processes: test runners, dev servers, builds, editors,
sometimes a GUI application. Turn shows that tree, because "which of the eleven things under this
Session is the one that failed" is a question users actually have.

The hard rule is that the tree is only as confident as its evidence. Each parent edge records both what
the relationship means and how confidently Turn knows it. A link declared by the tool can be
`spawned_by/explicit`; a link derived from the OS process table is provisional. Anything else stays
unknown and renders under the Session rather than under a plausible-looking process parent. Relationship
confidence is not event confidence: Turn may know explicitly that a process-table scan happened while the
edge inferred from that scan remains provisional. A stronger edge is never overwritten by a weaker one.

### 3.3 The user's attention is a scarce resource, and Turn spends it deliberately

Every interruption has a price. Turn therefore treats "move the user's focus" as the most expensive
operation in the product and puts it behind a governor that no per-Session policy can opt out of:
never interrupt mid-keystroke, never move more than three times in ten seconds, never let a Session
the user was just moved away from immediately drag them back.

Two consequences that follow from taking this seriously:

- **A deferral is not a dropped signal.** When a focus change is held back because the user is
  typing, the demand stays in the queue and the badge stays on the Session; the jump lands once the
  user's hands stop. But it expires after sixty seconds, because being yanked somewhere on account of
  something that happened two minutes ago is worse than not being moved at all.
- **Silence is a decision, not an absence.** `Action::Nothing` is an explicit variant, so a Session
  configured to stay quiet is distinguishable from one that was never configured.

### 3.4 Confidence is part of the data, and a guess may never act like a fact

Turn mixes reliable signals (a hook payload, an exit status) with unreliable ones (pattern matching
on terminal output). Presenting them identically would mean lying to the user in the tool's own voice.
So every event carries a `Confidence`, the event's source caps how much confidence it is allowed to
claim, and provisional states render as provisional.

The sharpest form of this rule: **a heuristic can never move the user's focus.** It is enforced twice,
independently — once when the event is built (`EventSource::PtyHeuristic` caps at
`Confidence::InferredHigh`) and once when policy is resolved (any focus action from a
non-focus-worthy confidence degrades to a badge). See §6.10.

### 3.5 It must work with the tools as they exist today

Turn integrates with agent CLIs through mechanisms those tools already ship: Claude Code's hook
engine, Codex's hooks and `notify` callback, and — for everything else — the OS process table and the
terminal output itself. No vendor cooperation is required, no forks, no patched binaries, and
crucially **no modification of the user's own configuration**. Claude Code hooks are injected with
`--settings`, which adds a settings layer and leaves `~/.claude/settings.json` and
`.claude/settings.json` untouched (`claude::tests::preparing_writes_a_settings_file_and_passes_it_
without_touching_user_config`).

The corollary is that integration quality varies by tool, so Turn states it plainly rather than
pretending to a uniform experience: four Integration Levels, shown per Session, with a note explaining
what was set up and therefore why detection is or is not working.

### 3.6 Turn observes; the user decides

Turn is a supervisor, not an autopilot. It never approves a permission, never relaunches a process on
restore without being asked, and never executes a command it read out of agent output. Risk ratings on
a pending permission are a display and ordering aid, explicitly not an authorisation decision. When a
process cannot be recovered after a restart, Turn reports `Lifecycle::Lost` — an honest "we don't
know" — rather than a silent respawn.

### 3.7 Views do not own work

An AgentNode is runtime identity; a Pane is one view of it. An Agent may have zero, one or several panes,
and closing a pane never implies stopping the Agent. Discovery of a subagent updates the hierarchy in the
background and does not mutate Layout, selection, focus or Attention.

The tree may show a compact Activity Preview so background work is legible without rendering every
terminal. A preview is normalised, provenance-labelled, redacted and bounded status — never transcript,
scrollback or restored conversational memory. A recovered preview retains its original timestamp; the
distinct recovered/stale visual marker is still open and must not be claimed as delivered. Opening Quick
Preview changes no Layout or process state.

---

## 4. Personas and use cases

### Persona A — the parallel operator (primary)

A senior engineer who runs four to ten agents at once across two or three repositories: one doing a
refactor, one writing tests, one reviewing a PR, one investigating a production issue. They know
exactly what they want each agent to do; their problem is scheduling themselves across all of them.
They will happily configure per-Session policy if it buys them quiet.

What they need: an ordered Attention Queue with a keyboard shortcut that always lands on the right
next thing; badges that distinguish "blocked" from "finished a turn"; the ability to mute a Session
they want to check on manually; and confidence that a quiet Session is quiet because nothing needs
them, not because a signal was lost.

### Persona B — the long-run supervisor

Runs one or two agents on jobs measured in tens of minutes, alongside their own editing work in
another window. They are not watching; they are working. Being pulled out of their own flow by
anything less than a genuine blocker is a net loss.

What they need: notifications that fire on blockers only; `FocusIfBackground` semantics so Turn does
nothing while the window is frontmost; and Session state that is still true when they come back an
hour later — including "the agent finished, but the test run it started is still going".

### Persona C — the reviewer

Uses agents to inspect work rather than produce it: a review Agent beside `lazygit`, a Session per
PR. Sessions are short-lived and highly repetitive, so the setup cost per Session has to be near zero.

What they need: Templates (the shipped `PR Review` Template is exactly this shape), name patterns that
fill in the branch, and Session duplication that copies the shape without the processes.

### Use cases the design is aimed at

1. **Fan out one task across agents.** Two agents receive the same prompt in isolated worktrees, or one
   works while the other reviews read-only; compare the results. The product never starts two unarbitrated
   writers against the same primary checkout.
2. **Watch a long run without babysitting it.** Start an agent on a large refactor, keep working, get
   pulled in only when it is blocked — and not when it merely finished a turn.
3. **Handle a burst of simultaneous blockers.** Three agents block within a second of each other.
   Exactly one focus change happens; the other two are badged and queued; the shortcut walks all
   three in priority order.
4. **Understand a failure inside a big tree.** A Session with an Agent, two subagents, a dev server
   and a test runner reads as failed because one child failed, and the tree says which.
5. **Come back after closing the window.** A replacement UI reattaches to the same live daemon without
   disturbing its processes. After a daemon restart, durable state returns but runtimes are reported
   `Orphaned`/`Lost` and relaunch is only offered. Proven PTY reattachment across daemon death remains
   unimplemented; see §6 and §7.

---

## 5. MVP scope

The MVP is a desktop application that does the following, on macOS and Linux, with parity as a
requirement rather than an aspiration.

**Terminal and process substrate**
- Real ptys per PTY-backed runtime node, with zero-to-many Panes as views. Interactive programs,
  full-screen TUIs (`vim`, `lazygit`, `btop`), correct resize, working directory and environment.
- Bounded buffers per PTY-backed runtime: a raw byte ring for replay and a parsed vt100 screen for
  rendering and stable-preview extraction.
- Backpressure that degrades cleanly: a slow consumer is told it lost data and resynchronises from a
  replay rather than growing an unbounded queue.
- Search over the live screen and retained scrollback, safely validated hyperlinks, quoted path
  drops, IME/dead-key input, program-owned mouse/alternate-screen modes and live appearance
  preferences. Reproduce the whole interaction contract with `make terminal-acceptance`.
- Process supervision: discovery of what the processes Turn started went on to start, scanned on
  demand rather than polled.

**Domain and attention**
- Workspaces, Sessions, Panes, Layouts, Templates.
- Closed Session modes: `main_checkout`, `read_only`, `isolated_worktree`; daemon-owned arbitration that
  allows at most one writer on the primary checkout inside one canonical Turn data directory and returns
  structured alternatives on conflict. Separate data directories do not yet share a checkout lock.
- The two-axis state model (`Lifecycle` × `Turn`) with a derived `DisplayState` for the UI.
- The Attention Queue: deduplication, priority, ageing without starvation, snooze, acknowledge,
  dismiss, and a `next-attention` command that is deterministic rather than a lottery.
- Focus governance: typing guard, rate limit, ping-pong guard, per-Session cooldown, mute.
- Hierarchical Attention policy with quiet defaults, independently overridable at Global, Workspace,
  Template and Session levels.

**Agent integration**
- Claude Code at Integration Level `Structured`: turn boundaries, permission requests with a risk
  rating, confirmed subagent hierarchy, and `background_tasks` so "turn finished" is never confused
  with "work finished".
- Codex CLI at `Structured` where its hook trust model allows, degrading to its `notify` channel and
  reporting the lower level rather than failing to launch.
- A generic heuristic layer for every other interactive tool, capped at `InferredHigh` confidence and
  therefore incapable of moving focus.
- A local hook server bound to `127.0.0.1` only, with a per-node token.

**Application**
- A daemon (`turnd`) that owns the ptys, so the UI can restart without killing the work.
- A native desktop window, drawn on the GPU rather than in a webview (ADR-039): one unified Workspace tree
  containing Sessions, Agents, tools and relevant processes; user-chosen Panes showing real terminals; a
  logical Attention Queue reached through the tree/commands; an optional contextual inspector; and a
  permission banner that the user answers (ADR-040).
- Background AgentNodes with zero-to-many Pane bindings, bounded Activity Preview and explicit Quick
  Preview/temporary-pane actions.
- SQLite persistence of Workspaces, checkout assignments, Sessions, Layouts, Templates, leases, bindings,
  safe previews, per-surface tree state and event history.

**Explicit non-goals inside the MVP**
- Turn does not write a terminal emulator. The daemon embeds `vt100` and the window paints the cells that
  parser produces — one emulator, one parse, no second opinion (ADR-008, ADR-039).
- Turn does not run models, hold API keys, or proxy agent traffic.
- Turn does not modify the user's agent configuration files.

---

## 6. Deferred, post-MVP

Listed because each one is a thing the architecture must not foreclose, not because it is planned for
the first release.

| Deferred | Why it waits | What must not be foreclosed |
| --- | --- | --- |
| tmux-backed Sessions | The daemon already provides persistence across UI restarts, which was tmux's main draw. Persistence across daemon exit is a real gap tmux would close, but it is not worth the MVP's complexity budget. | `Workspace::tmux_enabled` and `Session::tmux` flags and the `TmuxSession`/`TmuxPane`/`TmuxTerminal` kinds already exist in the model. |
| Codex `app-server` (JSON-RPC over a unix socket) | Far richer than hooks — `turn/started`, `turn/completed`, `thread/status/changed`, `ProcessExited`, approval requests, token usage — but a second, differently-shaped integration path. | The `EventSource::SideChannel` variant and the adapter trait already accommodate it without a redesign. |
| Remote and SSH Sessions | The whole pty/supervision layer assumes local processes. | Nothing in the domain layer assumes locality; `ProcessNode` holds a `cwd` string, not a handle. |
| Session sharing, multi-user, web access | Contradicts the 127.0.0.1-only security posture; needs an auth model Turn does not have. | The daemon↔UI protocol is a real protocol, not in-process calls. |
| Cost and token dashboards | `Capabilities::usage_events` is false for the Claude Code adapter today because the hooks Turn subscribes to do not carry usage. | `AgentInfo` already has `tokens_used` and `cost_usd`. |
| Agent-authored workflows, task queues, cron-style scheduling | This is agent orchestration. Turn supervises; it does not drive. Crossing that line changes the product. | — |
| Windows support | The pty layer, signal handling and process supervision are unix-shaped, and `PtyProcess::terminate` uses `libc::kill` on unix with a `kill()` fallback elsewhere. | `portable-pty` and `sysinfo` are cross-platform; the `#[cfg(unix)]` seams are already narrow. |
| Plugin API for third-party adapters | The adapter trait is not yet stable enough to expose. | `AgentAdapter` is already the only tool-specific seam. |
| Heuristic correction UI ("this state is wrong") | Needs the heuristic layer to exist first. | `EventSource::UserCorrection` is already in the vocabulary at `Explicit` confidence. |

---

## 7. MVP acceptance criteria

Checked items are demonstrated by named unit, integration, protocol or native snapshot tests that **run and
pass** in this workspace. The automated Reviewer vertical crosses the production daemon, store, protocol,
GUI state and real loopback Claude hook transport; it does not claim an authenticated external CLI or a
signed application bundle was exercised. Reproduce the current suite with the command at the head of this
document. Unchecked items say what evidence or implementation is still missing, not when it will arrive.

### Terminal substrate

- [x] **An interactive program runs on a real pty and its output reaches the screen.**
  `turn-pty::process::tests::a_process_runs_and_its_output_reaches_the_screen`,
  `input_written_to_the_pty_reaches_the_process`.
- [x] **The process sees the geometry Turn gave it.** Verified against the tty itself via `stty size`:
  `process::tests::a_process_sees_the_size_we_gave_it`.
- [x] **Resize reaches both the kernel and Turn's own screen model.**
  `process::tests::resize_reaches_both_the_kernel_and_our_screen_model`. Both halves matter: without
  the ioctl the program draws at the old width; without the buffer update Turn's snapshots and
  heuristics read a screen the user is not looking at.
- [x] **Full-screen TUIs are handled and detected.** Alternate-screen entry and exit are tracked so
  output heuristics can stand down inside a TUI redraw:
  `process::tests::a_full_screen_application_is_recognised`,
  `buffer::tests::a_full_screen_app_is_detected_so_heuristics_can_stand_down`.
- [x] **ANSI escapes are interpreted, not displayed.**
  `buffer::tests::ansi_colour_and_cursor_movement_are_interpreted_not_shown`.
- [x] **Buffers are bounded and admit when they dropped data.**
  `buffer::tests::the_byte_ring_is_bounded_and_admits_when_it_dropped_data`,
  `a_single_write_larger_than_the_ring_keeps_its_tail`,
  `heavy_output_does_not_grow_memory_without_bound`.
- [x] **A slow consumer is told it fell behind rather than buffering forever.** Four thousand lines
  flooded past an idle subscriber produce a `Lagged` error carrying the skipped count, while the
  authoritative buffer stays correct:
  `process::tests::a_slow_subscriber_is_told_it_fell_behind_rather_than_buffering_forever`.
- [x] **One noisy process does not stall another.**
  `process::tests::heavy_output_does_not_block_a_second_process`.
- [x] **A re-attaching Pane can rebuild the screen exactly.**
  `process::tests::a_reattaching_pane_can_rebuild_the_screen_from_replay`,
  `buffer::tests::replay_reconstructs_the_screen_without_replaying_the_whole_ring`.
- [x] **A signal death is never reported as a clean exit.**
  `process::tests::killing_a_process_is_reported_as_a_signal_death`,
  `state::tests::signals_count_as_failure`.
- [x] **Interrupt reaches the foreground process group, not just the direct child.** Sent as the
  control character through the tty, which is what reaches the children an agent spawned:
  `process::tests::interrupting_reaches_the_foreground_process_group`.
- [x] **A missing binary fails cleanly instead of panicking.**
  `process::tests::spawning_a_command_that_does_not_exist_fails_cleanly`.
- [x] **A zero-sized terminal is clamped rather than crashing the parser.**
  `buffer::tests::a_zero_sized_terminal_is_clamped_instead_of_panicking`.

### State model

- [x] **Finishing a turn is not the same as exiting.**
  `state::tests::finishing_a_turn_is_not_the_same_as_exiting`, and at Session level
  `model::session::tests::a_session_whose_agent_finished_but_child_still_runs_reads_as_running`.
- [x] **A crashed Agent never keeps claiming it awaits the user.** A dead process outranks any stale
  turn state, so it leaves the queue: `state::tests::a_crashed_process_never_keeps_claiming_it_
  awaits_you`, `attention::manager::tests::a_dead_process_leaves_the_queue_behind`.
- [x] **A clean exit after a reported task completion reads as done, not merely stopped.**
  `state::tests::a_clean_exit_after_task_done_reads_as_done_not_stopped`.
- [x] **A plain process carries no turn axis.** A shell owes the user nothing:
  `state::tests::a_plain_process_with_no_agent_axis_is_just_running`,
  `model::node::tests::an_agent_gets_the_turn_axis_and_a_shell_does_not`.
- [x] **An unrecoverable process is reported as lost, not assumed dead or alive.**
  `state::tests::a_lost_process_is_reported_rather_than_assumed_dead_or_alive`.

### Attention

- [x] **Simultaneous demands produce one ordered next item.** Three Sessions block at once; the
  blocked permission goes first and the full order is deterministic:
  `attention::queue::tests::simultaneous_demands_produce_one_ordered_next`,
  `attention::manager::tests::three_simultaneous_demands_are_walked_one_at_a_time`.
- [x] **A chatty Agent is still one demand, and cannot reset its own age to jump the queue.**
  `queue::tests::repeated_demands_collapse_instead_of_piling_up`,
  `a_repeated_demand_cannot_reset_its_age_to_jump_the_queue`.
- [x] **Two subagents blocked at once are two demands, not one.**
  `queue::tests::two_subagents_in_one_session_are_two_demands`,
  `event::tests::two_subagents_waiting_at_once_do_not_collapse`.
- [x] **Ageing prevents starvation without reordering priority classes.** An hour-old idle prompt
  still loses to a fresh blocked permission, but beats an equally-ranked fresher demand:
  `queue::tests::aging_prevents_starvation_without_reordering_priority_classes`.
- [x] **Snooze, acknowledge and dismiss behave as specified,** including that asking again
  un-acknowledges: `queue::tests::snoozed_demands_disappear_until_their_deadline`,
  `acknowledged_demands_rank_below_pending_ones_but_stay_reachable`,
  `asking_again_un_acknowledges_a_demand`.
- [x] **The user is never interrupted mid-keystroke, and the signal is not lost.** Deferred, then
  delivered when their hands stop: `attention::focus::tests::typing_defers_focus_rather_than_
  dropping_the_signal`, `manager::tests::focus_waits_for_the_user_to_stop_typing_then_happens`.
- [x] **A stale deferred jump is dropped rather than fired late.**
  `manager::tests::a_stale_deferred_jump_is_dropped_rather_than_fired_late`.
- [x] **A per-Session policy cannot opt out of the typing guard.** The guard lives in the governor,
  not the policy: `focus::tests::a_policy_cannot_opt_out_of_the_typing_guard`.
- [x] **A burst of simultaneous completions moves the user exactly once.**
  `focus::tests::agents_finishing_at_the_same_instant_produce_one_focus_change`, and the sliding
  window holds under sustained load: `focus_changes_never_exceed_the_ceiling_within_any_single_window`.
- [x] **A Session cannot immediately reclaim focus it just lost.**
  `focus::tests::a_session_cannot_immediately_reclaim_focus_it_just_lost`.
- [x] **Manual navigation always works and is never rate limited.** Pressing the shortcut is consent:
  `manager::tests::walking_the_queue_manually_is_not_rate_limited`.
- [x] **A new subagent badges and never moves the user,** even at `Explicit` confidence:
  `policy::tests::a_new_subagent_never_moves_focus_even_when_explicit`,
  `manager::tests::a_new_subagent_badges_without_moving_the_user`.
- [x] **A completed turn badges but does not queue a blocking demand.**
  `manager::tests::a_completed_turn_badges_but_does_not_queue_a_blocking_demand`.
- [x] **A muted Session badges and does nothing else.**
  `manager::tests::a_muted_session_badges_and_nothing_more`.
- [x] **Answering the Agent clears the demand.**
  `manager::tests::answering_the_agent_clears_the_demand`.

### Confidence and integration

- [x] **A heuristic cannot promote itself to a fact.** The event source clamps the requested
  confidence: `event::tests::a_heuristic_cannot_promote_itself_to_explicit`.
- [x] **A heuristic can never move the user's focus,** enforced at both the policy layer and the
  manager: `policy::tests::a_guessed_permission_badges_instead_of_stealing_focus`,
  `manager::tests::a_guessed_permission_never_produces_a_focus_effect`.
- [x] **Provisional demands rank below confirmed ones of the same kind, and are upgraded in place
  when a hook confirms them.** `queue::tests::provisional_demands_rank_below_confirmed_ones_of_the_
  same_kind`, `upsert_upgrades_confidence_when_a_hook_confirms_a_guess`.
- [x] **Claude Code hooks are installed without touching the user's own configuration.** The settings
  file is written into Turn's per-Session scratch directory and passed with `--settings`; the user's
  own flags keep their position: `claude::tests::preparing_writes_a_settings_file_and_passes_it_
  without_touching_user_config`.
- [x] **Claude Code's payloads map to the event vocabulary,** including the three `Notification`
  types told apart rather than flattened, permission requests carrying a command and a risk rating,
  and confirmed subagent start/stop: `claude::tests::notification_types_are_told_apart`,
  `a_permission_request_carries_the_command_and_a_risk_rating`,
  `subagents_are_reported_explicitly_with_their_type`.
- [x] **`background_tasks` makes "turn finished while work continues" a reported fact, not an
  inference.** `tests/contract_claude.rs::a_stop_with_background_work_is_reported_as_such`.
- [x] **The adapter survives malformed, mistyped and truncated payloads without panicking.** An
  adapter that panics takes the daemon's event loop with it:
  `claude::tests::malformed_payloads_do_not_panic`,
  `tests/contract_claude.rs::no_recorded_or_corrupted_payload_can_panic_the_adapter`.
- [x] **An unrecognised hook event is dropped rather than guessed at.** New releases add events; they
  must not become noise: `claude::tests::an_unknown_hook_event_is_ignored_rather_than_guessed_at`.
- [x] **Real recorded payloads are pinned by a contract test** against
  `tests/fixtures/claude-code-2.1.221.json`, captured from a live run:
  `tests/contract_claude.rs::the_recorded_hook_payloads_still_carry_the_fields_the_adapter_reads`.
- [x] **A command that merely mentions an agent is not classified as one.** `echo "ask claude"` is not
  a coding agent: `turn-pty::supervisor::tests::a_command_that_only_mentions_an_agent_is_not_
  classified_as_one`.
- [x] **An unrecognised process is admitted as unknown rather than mislabelled,** including a GUI
  application an Agent launched: `supervisor::tests::an_unrecognised_process_is_admitted_as_unknown_
  rather_than_guessed`.
- [x] **Descendants are walked transitively from the real process table.**
  `supervisor::tests::a_child_we_spawn_is_found_as_a_descendant`.
- [x] **Risk ratings err upward and cannot be laundered by a reassuring tool name.** `Read` with
  `rm -rf` is High: `risk::tests::the_command_outweighs_a_reassuring_tool_name`,
  `an_unrecognised_tool_defaults_upward`.
- [x] **Codex is configured through inline TOML hooks, not a file path,** with PascalCase event keys, the
  handler list spelled `hooks` exactly as in Claude Code, a shell-quoted command because Codex runs it
  through a shell, and `notify` naming the program only:
  `tests/contract_codex.rs::hooks_are_configured_as_an_inline_toml_struct_and_never_as_a_path`,
  `the_handler_list_key_is_hooks_because_handlers_fires_nothing`,
  `subscribed_event_keys_are_pascal_case_and_from_the_known_set`,
  `notify_names_the_program_only_and_the_url_travels_in_the_environment`.
- [x] **A per-node token never travels in a process's argv.** `/proc/<pid>/cmdline` is world-readable, so a
  token passed as `--url` would let any agent Turn launched harvest every other Codex session's token with
  one `ps` and forge events for all of them. Both Codex mechanisms take the URL from `TURN_HOOK_URL`
  instead: `tests/contract_codex.rs::notify_names_the_program_only_and_the_url_travels_in_the_environment`. This
  reverses part of ADR-027's reasoning and landed during this survey.
- [x] **A full Codex launch configures both mechanisms, because neither is sufficient alone,** and
  degrades honestly when hook trust is unavailable:
  `tests/contract_codex.rs::a_first_launch_configures_both_mechanisms_but_claims_only_what_it_can_prove`,
  `without_hook_trust_the_adapter_reports_wrapper_and_says_what_is_missing`.
- [x] **A Codex tool-call payload is never turned into an approval or a command to run.**
  `tests/contract_codex.rs::a_tool_call_payload_is_never_turned_into_an_approval_or_a_command_to_run`,
  `a_permission_request_is_reported_for_the_user_and_never_answered`.
- [x] **The heuristic layer stands down inside a full-screen application,** because a `vim` buffer
  containing `(y/n)` is not a permission prompt, and it counts how often it did.
  `heuristic::tests` (19 tests), `OutputHeuristic::stood_down()`.
- [x] **A quiet terminal is not, on its own, treated as an Agent waiting for the user.** The single most
  common false positive available is ruled out by construction: the "awaiting input" rule requires a
  positive marker of an agent input affordance. `heuristic::tests`.
- [x] **Heuristic inference is debounced, so a spinner clearing between frames does not produce a stream
  of started/waiting/started events,** and it is testable without sleeping because the caller passes the
  time in. `heuristic::tests`, `HeuristicConfig { idle_after_ms: 2_000, debounce_ms: 750 }`.
- [x] **Inference is only pointed at programs that hold a conversation.** `HEURISTIC_EXECUTABLES` is a
  closed list; anything unlisted gets a plain terminal and no claims. `registry::tests`.
- [x] **Adapter selection always answers.** An unrecognised command runs as a plain terminal rather than
  being refused, and the reason is reported in plain language for the Session details panel — including
  the distinct case of "Turn knows this tool but it is not on your PATH". `registry::tests` (12 tests),
  `Selection::is_installed`.
- [x] **`RUST_LOG=debug claude` is still recognised as Claude Code,** and a shell one-liner is
  deliberately not unpicked. `registry::tests`, `registry::executable_of`.
- [x] **The hook server binds `127.0.0.1` only, on an ephemeral port, and refuses an unknown token.**
  A request without a valid per-node 256-bit token is rejected and counted in `HookStats::refused`, so
  another process on the machine cannot forge "your Agent is waiting for you". `server::tests`
  (13 tests), including a real HTTP client over a real socket.
- [x] **The hook server never answers with a decision.** Claude Code's protocol permits a response body
  that allows or denies a tool call; Turn always replies with an empty 200. `server::tests`.
- [x] **A hostile `Content-Length` costs nothing.** The 256 KiB body limit is applied by the server
  before the bytes are buffered. `server::tests`.
- [x] **A daemon that stops draining loses events rather than slowing every Agent on the machine.** The
  event channel is bounded at 1,024 and a full channel drops and counts; a dropped receiver does not stop
  the server answering agents. `server::tests`.
- [x] **A broken `turn-hook` helper can never break an agent session.** No URL, no daemon listening, an
  unreadable payload, a refused connection — the process exits 0 and prints nothing.
  `turn-hook` (15 tests).
- [x] **The helper accepts both payload conventions,** stdin as Claude Code's `command` hooks deliver it
  and argv as Codex's `notify` does, and takes its destination from `--url` or `TURN_HOOK_URL`.
  `turn-hook` tests.

### Hierarchy

- [x] **A tool-reported subagent link is confirmed; a process-table guess is inferred; anything else
  renders at the root.** `model::node::tests::subagents_hang_off_their_parent_with_a_confirmed_link`,
  `an_unattributable_process_stays_at_the_root_rather_than_being_guessed_under_a_parent`.
- [x] **A confirmed link is never overwritten by an inferred one, and an inferred link is upgraded
  when the tool confirms it.** `node::tests::a_confirmed_link_is_not_overwritten_by_an_inferred_one`,
  `an_inferred_link_is_upgraded_when_the_tool_confirms_it`.
- [x] **Removing a parent promotes its children instead of deleting them,** and cycles are refused:
  `node::tests::removing_a_parent_promotes_its_children_instead_of_deleting_them`,
  `relinking_refuses_to_build_a_cycle`.
- [x] **A Session's aggregate state is the most severe, not the most recent.** One failure among nine
  healthy processes reads as failed: `node::tests::the_aggregate_state_is_the_most_severe_not_the_
  most_recent`.
- [x] **One revisioned hierarchy snapshot contains Workspace → Session → Agent/Tool → Child in draw
  order.** The GUI bootstraps from it rather than joining independent Workspace, Session and process lists.
  `a_full_snapshot_round_trips_checkout_name_relationship_preview_and_binding_facts`,
  `a_delayed_full_hierarchy_snapshot_cannot_rewind_navigation`.
- [x] **A reported subagent appears with its true declared name and no Pane binding.** Discovery does not
  change Layout, selection, pane focus, OS focus or the Attention Queue.
  `reviewer_is_a_named_background_child_and_never_opens_a_pane`,
  `the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart`.
- [x] **Relationship kind and relationship confidence survive persistence and protocol separately from
  event confidence.** A process-table edge remains visibly provisional even when the observation event is
  explicit. `hierarchy_rows_project_preview_bindings_and_runtime_capability_without_coupling_lifetimes`,
  `an_inferred_permission_and_an_inferred_relationship_are_drawn_as_guesses`.
- [x] **Tree selection, active Session, focused Pane and pending Attention are independent.** Expansion and
  selection restore per stable UI surface and one window never adopts another window's selection.
  `tree_selection_is_private_to_a_surface`,
  `selection_expansion_and_focus_are_different_typed_actions`.
- [x] **Quick Preview and a temporary Pane are explicit view actions.** Closing either leaves the Agent
  alive; a node without a PTY opens Preview/Process Details rather than a fake terminal. Temporary
  bindings belong to one live UI surface and expire on replacement/disconnect/restart without changing
  the saved Layout or another surface.
  `quick_preview_is_semantic_and_does_not_replace_the_layout`,
  `a_temporary_reviewer_pane_is_visually_distinct_from_the_saved_layout`.
- [x] **Next Attention preserves the semantic Agent while routing input to its authentic runtime.** A
  background Reviewer remains selected and owns the demand; only an integrated/explicit ancestor runtime
  with an existing Pane may receive keyboard focus. No provisional edge is followed and no Pane opens as a
  side effect. `semantic_attention_selects_the_child_but_focuses_its_runtime_owner_pane` and the hook E2E.
- [x] **Every hierarchy level has one optional contextual inspector rather than another navigation tree.**
  Workspace exposes paths, repositories, checkouts, shared resources, write authority and configuration;
  Session exposes checkout/mode/Template, Attention, process counts and safe history; Agent and Process
  expose identity, work, readable parent navigation, relationship/origin confidence, runtime facts,
  metrics and handoff/event metadata where applicable. Values from inference stay visibly provisional and
  secrets are redacted at the daemon boundary. The panel collapses, becomes an overlay on narrow windows
  and has an explicit AccessKit context. Reproduce the contract with `make inspector-acceptance`.

### Checkout safety

- [x] **Creating a `main_checkout` Session stores its assignment and acquires the exclusive lease in one
  atomic transaction before any init command, process or Pane is materialised.** Failure rolls back the
  Session and performs no external side effect. `creating_a_main_session_and_lease_is_atomic_on_conflict`,
  `a_second_main_checkout_session_is_rejected_before_any_runtime_state_exists`.
- [x] **A conflicting writer returns structured data naming the owner and the allowed alternatives:** focus
  owner, read-only, isolated worktree or cancel. No client parses a human error string to construct them.
  `a_write_conflict_carries_owner_and_alternatives_as_typed_context`,
  `a_write_lease_conflict_offers_only_explicit_safe_alternatives`.
- [x] **A heartbeat timeout never steals a lease.** Restart reconciliation verifies the owner/processes or
  asks the user; closing the UI, archiving a live Session or `keep_processes` does not release ownership.
  `daemon_restart_fences_every_unreleased_lease_without_forging_a_heartbeat`,
  `a_recovery_lease_cannot_authorise_add_pane_or_relaunch`.
- [x] **Read-only truth is visible.** Turn distinguishes enforced read-only from unenforced legacy metadata;
  agent instructions alone never count as enforcement. `read_only_creation_uses_the_primary_without_claiming_its_lease`,
  `a_read_only_alternative_never_launches_without_a_technical_guard`.

### Layouts and Templates

- [x] **Splits, close, resize, swap, zoom and focus cycling behave correctly and keep the geometry
  invariant.** 16 tests in `model::layout::tests`, including that three same-direction splits produce
  one flat split of three equal siblings rather than a lopsided nest, that a Pane cannot be resized
  out of existence, that the last Pane cannot be closed, and that operations on an unknown Pane fail
  instead of corrupting the tree.
- [x] **Two Sessions instantiated from one Template share no Pane identity but have identical shape
  and commands.** `model::template::tests::two_sessions_from_one_template_share_no_pane_ids`.
- [x] **Saving a live Layout as a Template drops process bindings.**
  `template::tests::saving_a_live_layout_as_a_template_drops_process_bindings`.
- [x] **A hand-edited Layout with sizes that do not add up is normalised on load.**
  `layout::tests::a_hand_edited_layout_with_bad_sizes_is_normalised_on_load`.
- [x] **First run offers one portable built-in: Two Shells.** It has equal columns, starts only
  the Workspace shell and needs no optional third-party executable.
  `template::tests::the_built_in_set_is_present_and_valid`,
  `template::tests::the_first_run_preset_is_two_equal_portable_shells`.
- [x] **The complete Template lifecycle is available without editing JSON.** The visual UI creates,
  captures with an explicit name, edits, duplicates, deletes, selects Global/Workspace defaults and
  applies a Template to a stopped Session. It preserves geometry and Pane/startup configuration;
  missing tools are visible before launch, built-ins stay read-only, and deleting an in-use Template
  clears references without changing instantiated Sessions. Reproduce with `make template-acceptance`.
- [x] **Choosing a safe alternative after a Template lease conflict preserves the complete Template.**
  The client retains only Template id/name/cwd/branch/task intent; the daemon authoritatively reapplies
  Layout, commands, env, Attention, tmux and naming. An unenforced read-only alternative launches nothing;
  a worktree remaps absolute
  cwd values into the isolated checkout without modifying the primary tree.
  `template_lease_conflict_alternatives_keep_coding_authoritative_and_isolated`.

### Security

- [x] **A process cannot rewrite the user's clipboard by printing an escape sequence.** OSC 52 writes
  are refused and counted so the UI can say a process tried:
  `buffer::tests::a_clipboard_write_from_the_process_is_refused_but_recorded`. A clipboard *read* request
  is refused too, so a process cannot exfiltrate what the user last copied:
  `a_clipboard_read_request_from_the_process_is_refused_and_recorded`.
- [x] **A process cannot resize its own terminal by printing an escape sequence.** Geometry is Turn's to
  decide, not the program's: `buffer::tests::a_resize_request_from_the_process_is_refused`.
- [x] **A process-supplied title is stripped of whole escape sequences, not just control characters,
  and length-capped.** `ESC [ 2 J` must not leave a visible `[2J` in the sidebar, and a nested OSC
  must not leave its payload behind:
  `buffer::tests::a_malicious_title_is_stripped_of_control_characters`. The cap is applied when the title
  arrives rather than when it is read, so an enormous title is never retained:
  `an_enormous_title_is_capped_when_it_arrives_not_when_it_is_read`. Invalid UTF-8 is replaced rather than
  fatal: `a_title_of_invalid_utf8_is_replaced_rather_than_fatal`.
- [x] **A title cannot lie about itself with Unicode.** A bidirectional override must not let a title
  render reversed, and invisible tag characters must not be smuggled into a label:
  `buffer::tests::a_title_cannot_reverse_its_own_rendering_with_a_direction_override`,
  `a_title_cannot_smuggle_invisible_tag_characters_into_a_label`. The same rule holds for screen content
  the UI renders: `screen_rows_never_carry_invisible_or_direction_changing_characters`.
- [x] **OSC 0/2 titles are scoped to the PTY that emitted them and survive UI detach.** The daemon updates
  only that Pane's bound process projection, keeps declared/integration/user Agent names above the process
  title, and never changes Layout, focus or Attention. A title explicitly chosen by the user remains above
  the OSC title. Two real PTYs, detached observation, durable projection and hostile title input are covered
  by `core::titles::tests::real_ptys_keep_dynamic_titles_isolated_and_preserve_stronger_names`; Pane chrome
  priority is covered by `desk::tests::pane_headers_prefer_user_titles_then_their_own_bound_process_title`.
- [x] **Every hierarchy label is safe at the daemon boundary, even when an adapter or OS source is not.**
  Workspace, Session and Template names reject C0/C1/ANSI/bidi/zero-width input rather than silently
  rewriting identity. Discovered Agent and process metadata is sanitised and bounded before reducer, push,
  inspector and SQLite; argv is capped by argument count, per-argument length and aggregate length.
  `navigation_names_reject_adversarial_text_instead_of_rewriting_it`,
  `an_agent_declaration_is_safe_and_bounded_in_event_tree_and_inspector` and
  `enormous_hostile_supervisor_argv_is_only_projected_as_bounded_safe_text`.
- [x] **One enormous line cannot grow the screen model without bound.** It is bounded by the terminal
  geometry: `buffer::tests::one_enormous_line_is_bounded_by_the_terminal_geometry`.
- [x] **Activity Preview cannot become a transcript or secret side channel.** ANSI, control sequences,
  bidi controls and unstable prompt/spinner text are removed; known secrets are redacted before SQLite;
  raw PTY bytes and raw hook payloads never enter preview storage; retention is capped at 20 per node and
  2,000 globally. `redacts_credentials_before_the_preview_can_reach_disk_or_ui`,
  `an_unredacted_sensitive_preview_never_reaches_navigation` and the on-disk secret suite.
- [x] **Checkout identity resists path aliases.** Lease arbitration uses canonical filesystem identity,
  rejects cross-Workspace/checkpoint mismatches, and reports worktree resources that remain shared.
  `lease_ownership_rejects_cross_workspace_session_and_checkout_ids`,
  `a_second_workspace_alias_is_refused_before_it_can_mint_a_checkout`.

### Persistence

- [x] **A whole desk survives a restart, and reports what it cannot vouch for.** Workspaces, Sessions, the
  layout tree, process metadata, Templates, settings and the event log all come back.
  `restart_restores_the_desk::a_whole_desk_survives_a_restart_and_reports_what_it_cannot_vouch_for`,
  `settings_templates_and_preferences_come_back_too`,
  `a_second_session_from_the_same_template_is_stored_independently`.
- [x] **A stored `Alive` is never believed after a restart.** `SessionRepo::load_for_restore` downgrades
  anything stored as running to `Lifecycle::Orphaned`, because a stored `Alive` only ever meant "alive when
  we last wrote". `restart_restores_the_desk::a_partial_restore_is_recorded_so_the_ui_can_explain_itself`.
- [x] **A pending demand for the user outlives the daemon that recorded it when its runtime owner remains
  corroborated.** An Agent that blocked on a permission at 17:58 is still blocked at 18:02 when its restored
  node/parent remains `Orphaned`; a queue rebuilt from nothing would drop it silently. Interaction demands
  whose owner is lost are removed, while explicit postmortem failure/completion evidence may survive.
  `restart_restores_the_desk::a_pending_demand_for_the_user_outlives_the_daemon_that_recorded_it`.
- [x] **The event log still says which states were guesses,** weeks later — every row keeps its
  `Confidence` and its source. `restart_restores_the_desk::the_event_log_still_says_which_states_were_guesses`.
- [x] **Renaming a Session does not erase its history.** Writes are `INSERT ... ON CONFLICT DO UPDATE`, never
  `REPLACE`, which would delete the old row first and cascade away the Session's nodes, events and pending
  attention. `repo::session::tests`,
  `restart_restores_the_desk::closing_a_pane_in_a_stored_session_does_not_leave_the_process_row_behind`.
- [x] **A database written by a newer build is refused at open time,** rather than being written to and
  silently losing the fields that build depends on.
  `turn_store::tests::a_database_from_a_newer_build_is_refused_at_open_time`.
- [x] **A long-running install prunes its history without losing the recent past.**
  `restart_restores_the_desk::a_long_running_install_prunes_its_history_without_losing_the_recent_past`.
- [x] **No recognisable secret in current durable free text reaches SQLite or its WAL** — every repository
  redacts Workspace, Session, Layout/Pane, Template, process/Agent, Attention, Preview, settings and event
  fields before rows are built; filesystem fencing identities are rejected rather than rewritten. This is
  asserted by writing and scanning real files, not by unit-testing the redactor.
  `secrets_never_reach_the_disk::no_secret_value_is_present_anywhere_in_the_files_on_disk`,
  `a_secret_survives_nowhere_even_after_the_daemon_restarts_and_prunes`,
  `a_process_environment_is_not_persisted_wholesale_even_when_it_looks_innocent`.
- [x] **A restored Pane is never given a scrollback the Agent no longer remembers.** Only process metadata
  is persisted — pid, command, cwd, lifecycle, relation, exit code, external id — never the pty, the
  terminal grid or the parser state. `repo::node::tests` (12 tests).
- [x] **Migration 003 grants no write lease.** It creates primary checkout assignments, imports compatible
  legacy pane bindings and marks ambiguous Workspaces for reconciliation. It launches, kills and moves no
  process and never claims a legacy Session is technically read-only merely by changing metadata.
  `v3_orphaned_worktree_claims_become_honest_primary_readers`, migration and legacy-reconciliation suites.
- [x] **Durable restoration preserves hierarchy edges, names, previews and permanent bindings without
  opening a new Pane; a replacement UI also recovers its per-surface expansion against the live daemon.**
  A persisted preview keeps its original timestamp; a distinct recovered/stale visual marker is still an
  acceptance gate and is not claimed complete. A daemon restart restores metadata, not a live PTY.
  `the_reviewer_vertical_survives_a_ui_restart_without_changing_layout` and
  `the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart`.

### The daemon↔UI boundary

- [x] **A full working conversation completes over the real framing.** Handshake, correlated
  request/response, unsolicited pushes.
  `turn-proto::conversation::a_full_working_conversation_completes_over_the_real_framing`.
- [x] **A guessed state reaches the client as a guess, and never as a focus jump.** The confidence rule
  survives the serialisation boundary.
  `conversation::a_guessed_state_reaches_the_client_as_a_guess_and_never_as_a_focus_jump`,
  `the_governors_verdicts_stay_distinguishable_across_the_boundary`.
- [x] **A UI restart rebuilds its terminals without touching the processes.**
  `conversation::a_ui_restart_rebuilds_its_terminals_without_touching_the_processes`.
- [x] **A partial restore offers a relaunch that only the user can accept.**
  `conversation::a_partial_restore_offers_a_relaunch_that_only_the_user_can_accept`.
- [x] **A firehose of output reassembles in order and admits what was dropped.**
  `conversation::a_firehose_of_output_reassembles_in_order_and_admits_what_was_dropped`.
- [x] **A hostile stream of rubbish never costs more than the bad lines.** A malformed frame from a buggy
  client cannot take the connection down.
  `conversation::a_hostile_stream_of_rubbish_never_costs_more_than_the_bad_lines`,
  `contract::the_catalogue_reassembles_correctly_under_pathological_chunking`.
- [x] **A stale client is told which side is old, and the connection ends** rather than half working.
  `conversation::a_stale_client_is_told_which_side_is_old_and_the_connection_ends`.
- [x] **Every request names a response that exists, and every response is produced by some request.** The
  pairing is load-bearing rather than documentation that might be stale.
  `contract::every_request_names_a_response_variant_that_exists`,
  `every_response_variant_is_produced_by_at_least_one_request`.
- [x] **The protocol has no way to say "approve this permission", no way to say "run this command", and
  exactly one way to restart anything.** Enforced by omission, which is the strongest form available to a
  type definition. `request::tests`.
- [x] **A subagent appearing pushes a tree the client can draw without guessing.**
  `conversation::a_subagent_appearing_pushes_a_tree_the_client_can_draw_without_guessing`.
- [x] **Protocol v4 bootstraps navigation with one `HierarchySnapshot` and a monotonic revision.** A missed
  revision triggers full resync; the client never applies a hierarchy diff to stale state.
  `a_delayed_full_hierarchy_snapshot_cannot_rewind_navigation` and protocol conversation tests.
- [x] **Lease conflicts are typed.** The wire payload carries owner Workspace/Session/checkout and recovery
  choices independently from the human-readable message. `a_write_conflict_carries_owner_and_alternatives_as_typed_context`.
- [x] **Preview and pane-binding pushes are coalesced current state, not append-only `TurnEvent`s.** Tree
  expansion and selection are scoped requests/acks, not broadcast domain events.
  `tree_selection_is_private_to_a_surface` and hierarchy push/coalescing tests.

### Functional acceptance and post-MVP release work

The functional v0.1.0 gate is defined in `docs/MVP_ACCEPTANCE.md`. Completed rows below
have current reproducible evidence; unchecked rows are explicit post-MVP scope and do
not weaken or block that functional baseline.

- [x] **Authenticated Claude Code in the packaged native app.** Complete Workspace creation, leased main
  Session, PTY interaction, named Reviewer spawn, Quick Preview, temporary Pane, close-without-stop and UI
  restart passed against Claude Code 2.1.226 and a real authenticated account. The exact environment,
  observed hook/terminal behaviour and repeatable harness are recorded in `docs/REVIEWER_ACCEPTANCE.md`.
- [ ] **Successful live-process reattachment after daemon death.** UI restart over a still-running daemon is
  covered. A PTY master cannot survive its owning daemon today, and `Lifecycle::Reconnected` is not forged.
  Issue #21 explicitly places daemon-crash survival outside the MVP.
- [ ] **Manual desktop acceptance.** Verify sound and OS notification delivery, VoiceOver/Orca, terminal
  IME/dead keys, clipboard and alternate-screen TUIs on packaged macOS/Linux builds. The application-owned
  accessibility/IME contract and reproducible checklist are complete; broad packaged platform sign-off is
  release hardening.
- [x] **Measured performance envelope.** CPU/wall time, peak RSS, input/output latency, hierarchy size,
  lazy rendering and preview cadence are enforced with 30 Sessions and 120 relevant Processes. Reference
  hardware and before/after profiles live in `docs/PERFORMANCE.md`.
- [ ] **tmux-backed Sessions.** Flags and node kinds exist; no backend exists in the MVP. This is deliberate
  post-MVP persistence work, not a substitute for Turn's hierarchy.
- [x] **Clean format and lint gates.** Reproduce with `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets -- -D warnings`; the audit report records the observed run.
