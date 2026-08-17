# Attention policy acceptance

This checklist exercises the user-visible boundary. The automated equivalents run with
`make verify`; the named tests below make a failure easy to reproduce in isolation.

## Automated proof

```sh
cargo test -p turnd attention_policy_resolves_all_four_persistent_levels_and_sessions_can_differ
cargo test -p turnd queue_priority_is_reordered_and_persisted
cargo test -p turnd a_session_mute_is_restored_after_attention_runtime_restarts
cargo test -p turn-core a_configured_custom_action_emits_the_exact_command
cargo test -p turn-core simultaneous_demands_never_become_a_timed_focus_cascade
cargo test -p turn-core typing_defers_focus_rather_than_dropping_the_signal
cargo test -p turn-gui a_custom_action_reaches_the_command_runner_exactly_once
cargo test -p turn-gui selecting_a_tree_node_never_acknowledges_or_resolves_attention
```

## Manual sound, notification and custom action

1. Open Settings with a Session selected. In “Attention, sounds and notifications”, select
   the Session level.
2. Enable `sound`, `notify` and `custom` for “Question asked”. Choose the alert sound. Replace
   the custom command with `touch /tmp/turn-attention-custom-accepted`.
3. Make that Session's Agent ask a question. Confirm that the OS notification names the
   demand, the alert sounds once, and the file is created. Turn never displays the stored
   command again; replacing it is deliberately blind because it may contain credentials.
4. Delete the acceptance file when finished.

## Manual hierarchy, focus and persistence

1. Give two Sessions different “Question asked” actions at the Session level. Trigger both
   and confirm their effects differ. Reset one Session value and confirm the Template,
   Workspace or Global value shown underneath becomes effective again.
2. Enable the typing guard and a focus action. Type continuously in another Session while the
   demand arrives: it must queue without moving focus, then may move only after typing stops.
3. Raise one demand and lower another with the queue's priority controls. Snooze one, mute its
   Session and dismiss another. Restart Turn: priority, snooze and mute remain; the dismissed
   demand does not return.
4. Select a Process row that has an outstanding permission. Selection alone must not remove,
   acknowledge or reorder the demand. Its permission banner must show the exact command, cwd,
   risk, Agent and Process before “Go to this session” is used.

## Accepted post-v0.1 Agent Node route

This is the ADR-059 acceptance target, not evidence about the current v0.1 build. Implementation must add
automated daemon/GUI tests and native snapshots that prove:

1. `Next Attention`, an exact or aggregate row badge, an OS notification and a governor-approved automatic
   Focus each apply the same daemon-resolved route, open the exact semantic Node View and reveal its safe
   action in one interaction, including when another Workspace or Session is visible. A deferred Focus keeps
   the same `attention_id`/subject/surface generation until it runs; a denied or retired-surface Focus never
   navigates or transfers to another window.
2. A semantic child that shares an ancestor PTY remains the selected subject. Only a verified
   `interaction_owner_node_id` receives input; a provisional ancestor or sibling is never focused.
3. Selecting, rendering or scrolling never acknowledges or resolves Attention and selection alone never
   clears unread. Only `mark_node_result_read` after the exact completed-result revision is primary content
   on a foreground WorkSurface clears that node's matching unread revision.
   A separately queued `TurnComplete` review remains in the daemon's exact order until acknowledged,
   dismissed or superseded by a defined runtime event.
4. Questions render their text/options and no approval buttons. Allow/deny appears only for an exact typed
   pending approval id and records the operator's explicit choice as `delivery_pending`.
5. Writing or submitting a response does not clear the demand. Only adapter evidence for the same instance,
   runtime attempt and prompt id resolves it; unavailable evidence remains `submitted_unconfirmed`.
6. Several simultaneous children preserve independent queue entries, unread state and prompt identity across
   relaunch. Attempt-scoped stale demands disappear without erasing valid instance-scoped review work.
7. Context-window warnings, account quota percentages and profile activity summaries cannot focus, resolve
   or reorder the queue. Only a typed `ContextBlocked` or `QuotaExhausted` event enters normal
   policy/confidence resolution. Missing, partial, stale or failed observations remain explicitly unknown;
   they never render as zero usage, zero remaining quota or an authoritative empty inbox.
8. Selecting an Agent, child, resource, historical conversation or job result never starts/resumes it and
   never shows a generic “Start pane” gate. Warm attach to an already live runtime is automatic; cold
   resume/restart requires its semantic lifecycle action. The only selection-triggered start exception is
   ADR-064's foreground Session activation contract below, which is typed, preflighted and fail-closed.
9. An authenticated parent/external-worker or unassigned node-less demand opens its exact
   `ProvisionalAttentionView` in the owning Session. It never invents a Node/AgentInstance, borrows an input
   owner or invokes Session activation; later exact binding produces a new route revision.

The cross-layer data and failure contract is `docs/AGENT_NODE_VIEWS_AND_CONTEXT.md` §5 and §11.

## Accepted ADR-064 attention and activation extensions

This is a post-v0.1 target, not evidence about the current build. Its automated fixtures, protocol captures
and native snapshots must prove all of the following without adding a second navigator or replacing the
single WorkSurface:

1. Selecting a foreground Session with a current, proved-safe activation plan sends exactly one idempotent
   `activate_session` operation. In the same interaction Turn restores its Layout, attaches proved-live
   attempts and, only for an empty Session, may start exactly its configured default Shell. Repeated delivery
   cannot double-start it and no “Start pane” or second confirmation is shown. A stale Session revision,
   changed target/account/cwd/command/authority, ambiguous survivor, missing containment, permission need or
   unsafe input owner starts zero processes and exposes one precise recovery action in that Session View.
   Background restore, child/resource/history selection and merely viewing an ended Session still launch
   nothing.
2. An externally sourced WorkItem and a provider-native Job/iteration have exact Attention subjects and
   routes in the same tree. Dismiss, snooze, mark-read and Session deletion mutate neither the external item
   nor the provider job. A close/reopen, job pause/resume/cancel or permission/result response advances the
   projection only after a revision-fenced source/provider receipt; timeout-after-possible-write becomes
   `reconcile_required`, never an automatic replay.
3. Conversation inventory search and similarity matches create no Attention, ownership or runtime. Adoption
   creates one stopped canonical Node and sends no provider input; resume is a separate foreground preflight
   and creates at most one new attempt. Current ownership is checked installation-wide across exact provider,
   profile, execution target, namespace and conversation id before any input or context authority exists.
4. Inert Web preview and interactive Browser Nodes are distinct routes. Preview, page content, navigation,
   popup, download and script messages cannot fabricate a typed demand, resolve Attention or become a control
   operation. A Browser action requiring operator review routes to the exact Browser Node; restoring history
   never reloads a page automatically.
5. Provider title read and conversation rename are independently advertised. A read never exposes a rename
   action without its own capability; a revision-fenced rename remains pending/reconciling until a correlated
   provider receipt establishes the effective title. Usage, context and the bounded activity inbox remain
   scoped to one AccountProfile and execution target with independent source, coverage and freshness.
6. A full remote GUI or companion may answer a known typed permission only with a single-use, expiring,
   foreground-desktop-issued, end-to-end-encrypted grant bound to the exact provider option, profile,
   Session/Node/instance/attempt/generation and interaction/authority revisions. Allow and deny both wait for
   provider evidence. Replay, widening, stale/offline use, cross-profile use and raw PTY input at that known
   sensitive interaction are refused server-side. Credential entry, grant issuance/expansion, administration,
   host trust and destructive authority stay local. For an unclassifiable generic TUI Turn makes no claim that
   arbitrary input is a permission response and therefore never upgrades heuristics into this remote path.

## Accepted M15 local dictation boundary

This is an ADR-060 target, not evidence about v0.1. `make dictation-acceptance` and native snapshots must
prove:

1. Recording, local inference and inline draft review set `sensitive_operation` only on their exact surface.
   An automatic Focus route for that surface is deferred with the same attention id/subject/connection
   generation; another window is never selected as a substitute.
2. Queue ordering, badges, unread state, notification identity and demand resolution are unchanged while
   dictation runs. Finishing/cancelling/failing transcription creates no Attention entry and acknowledges or
   dismisses nothing.
3. Manual `Next Attention`, badge and notification routing still work. They cancel live microphone capture,
   preserve any completed memory-only draft under its original target and apply the exact demand route; the
   draft never follows selection.
4. Zero target bytes exist before explicit Insert/Send. Dictation into an exact free-text question remains
   `delivery_pending`/`submitted_unconfirmed` until evidence for that same instance/attempt/prompt proves
   resolution. A spoken word never resolves it by itself.
5. Permission, password/credential, provisional/unassigned, raw-TTY and unverified alternate-screen demands
   expose no dictation action. “Yes”, “allow” and similar transcript text can neither invoke an approval nor
   call `route_attention`, `activate_session` or a lifecycle operation.
6. Blur, Escape, selection/attempt/prompt/surface-generation change, device loss, timeout, worker crash and a
   late result all stop/cancel without retargeting or changing Attention. After the sensitive state clears,
   the governor either applies its original still-valid route or degrades it under the normal policy.

The complete local-input contract is `docs/LOCAL_VOICE_INPUT.md`.
