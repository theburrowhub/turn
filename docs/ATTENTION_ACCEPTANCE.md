# Attention policy acceptance

This checklist exercises the user-visible boundary. The automated equivalents run with
`make verify`; the named tests below make a failure easy to reproduce in isolation.

## Automated proof

```sh
cargo test -p turnd attention_policy_resolves_all_four_persistent_levels_and_sessions_can_differ
cargo test -p turnd queue_priority_is_reordered_and_persisted
cargo test -p turnd a_session_mute_is_restored_after_attention_runtime_restarts
cargo test -p turn-core simultaneous_demands_never_become_a_timed_focus_cascade
cargo test -p turn-core typing_defers_focus_rather_than_dropping_the_signal
cargo test -p turn-gui selecting_a_tree_node_never_acknowledges_or_resolves_attention
```

## Manual sound and notification

1. Open Settings with a Session selected. In “Attention, sounds and notifications”, select
   the Session level.
2. Enable `sound` and `notify` for “Question asked”. Choose the alert sound.
3. Make that Session's Agent ask a question. Confirm that the OS notification names the
   demand and the alert sounds once. Neither effect may execute a shell command, submit input,
   resolve the demand or change its queue order.

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
4. Questions render their exact schema/options and use only `respond_to_agent_interaction`; they expose no
   permission control. A recognised permission renders every provider-offered option and uses one attempt/
   route-scoped PermissionResponseClaim through local typed, grant-bound remote typed or verified-local-PTY.
5. Submission does not clear the demand. A question has its own response receipt pending provider evidence;
   a permission exposes the distinct prepared/effect-armed/submitted/possible-effect dispatch axis and
   pending/not-applied/resolved/cancelled/attempt-ended/reconcile-required evidence axis. Only correlated
   evidence for the exact instance/attempt/interaction resolves either; uncertainty remains visible.
6. Several simultaneous children preserve independent queue entries, unread state and prompt identity across
   relaunch. Attempt-scoped stale demands disappear without erasing valid instance-scoped review work.
7. Context-window warnings, account quota percentages and profile activity summaries cannot focus, resolve
   or reorder the queue. Only a typed `ContextBlocked` or `QuotaExhausted` event enters normal
   policy/confidence resolution. Missing, partial, stale or failed observations remain explicitly unknown;
   they never render as zero usage, zero remaining quota or an authoritative empty inbox.
8. Selecting an Agent, child, resource, historical conversation or job result never starts/resumes it and
   never shows a generic “Start pane” gate. Presentation `attach_pane` plus resync to an already live runtime is automatic; cold
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
   attempts and starts the exact bounded eligible saved-runtime descriptor set—or, only when empty, exactly
   its configured default Shell. Repeated delivery cannot double-start any attempt and no “Start pane” or
   second confirmation is shown. A stale Session revision,
   changed target/account/cwd/command/authority, ambiguous survivor, missing containment, permission need or
   unsafe input owner starts zero processes and exposes one precise recovery action in that Session View.
   Background restore, child/resource/history selection and merely viewing an ended Session still launch
   nothing.
2. An externally sourced WorkItem and a provider-native Job/iteration have exact Attention subjects and
   routes in the same tree. A forgotten Job uses the general provisional route fenced by immutable
   AttentionId, Session, profile/target and observation revision; iteration/input-owner references require
   proof. Dismiss, snooze, mark-read, local activity-hide/forget and Session deletion mutate neither external
   object. All nine native-job adapter operations are distinct: list, get, create, update, pause, resume,
   run-now, cancel-iteration and delete-job. Each mutation advances the projection only after its revision-
   fenced receipt; external WorkItem close/reopen remain separately typed WorkItem mutation schemas and object
   family reached through a source. Possible-write
   timeout becomes exact-subject `reconcile_required` and lookup never replays the mutation.
3. Conversation inventory search and similarity matches create no Attention, ownership or runtime. Adoption
   creates one stopped canonical Node and sends no provider input; resume is a separate foreground preflight
   and creates at most one new attempt. Current ownership is checked installation-wide across exact provider,
   profile, execution target, namespace and conversation id before any input or context authority exists.
4. Inert WebPreview and interactive Browser Nodes are distinct routes. Preview, page content, navigation,
   popup, download and script messages cannot fabricate a typed demand, resolve Attention or become a control
   operation. A Browser action requiring operator review routes to the exact Browser Node; restoring history
   never reloads a page automatically.
5. Provider title read and conversation rename are independently advertised. A read never exposes a rename
   action without its own capability; a bounded pre-effect intent with tagged owner/proved-unowned subject and
   lookup-capable correlation remains pending/reconciling until a correlated provider receipt establishes the
   effective title/new revision. Same-title observation never resolves it and lookup never redispatches. Usage, context and the bounded activity inbox remain
   scoped to one AccountProfile and execution target with independent source, coverage and freshness.
6. LocalDesktopForegroundAuthority may use exact `submit_local_permission_response`; a full remote GUI or
   Companion may use only `submit_remote_permission_response` after exact E2EE grant delivery/ack. Grant
   consumption races revoke/expiry/reconnect/interaction/attempt/binding/capability change with one CAS winner,
   and dispatch/evidence receipt recovery never resends. Binary and non-binary options wait for provider
   evidence. Typed transport blocks raw bytes everywhere; only fresh supported verified-local-PTY admits the
   daemon-encoded desktop fallback. Offline drafts/replay, widening, stale/cross-profile use and every remote/
   voice/hook/background fallback fail. Credential, grant administration, host trust and destructive authority
   stay local; an opaque TUI gets no fabricated semantic guarantee.

## Accepted ADR-065 background delivery and live-status projection

This post-v0.1 target proves `ACP-ATT-012`. It projects the canonical queue; it does not add another queue,
resolution state or authority:

1. Pairing persists a preassigned non-reused endpoint/initial-grant pair, operation/fingerprint, peer
   correlation, catalogue/generations and all count/byte/terminal/recovery reservations before dispatch. Every
   prepared/dispatching/awaiting/reconcile crash and late peer reply returns that exact record without a second
   pair. Prepared expiry at 600 seconds tombstones its endpoint/grant reservation; an awaiting deadline instead
   reconciles. Endpoint reserved/active/retired/deleted and immutable proposed/active/expired/invalid/revoked grant
   states follow their closed machines; retire revokes all generations/outbox/live state and late replies cannot
   reactivate. Secret canaries are absent from protocol reads, UI/store/export/logs; 401/403 invalidates only
   the exact generation. Regrant/rekey/widen always revokes then mints a new globally non-reused grant id.
2. Outbox insert and flush independently revalidate grant, queue/subject revision, resolution and presence.
   `CollapseKey` includes endpoint, full tagged subject identity/revision and demand kind: replay of one demand
   emits at most one eligible alert, while two same-titled subagents or two demand kinds remain two. Batching,
   jitter and exactly eight total submissions with≤15-minute backoff remain within declared endpoint and
   global count/byte bounds; the ninth submission is structurally impossible.
3. Payloads are end-to-end encrypted and contain only the minimum route/display class authorised by the
   privacy scope—never transcript, prompt/answer body, command, path, account, secret or raw provider payload.
   Gateway `accepted` is not `delivered`, `read`, `acknowledged` or `resolved`; retryable/terminal failure and
   offline expiry mutate none of those canonical states.
4. Presence may hold the alert portion only. When presence leaves, the host releases only a still-current
   demand after a fresh revision check; a resolved, superseded, deleted or expired item is discarded. A
   notification deep link resynchronises snapshot/events and routes only after exact subject revalidation, so
   an offline stale action cannot submit.
5. Live status uses one key per endpoint+subject+attempt generation and monotonic revisions. Start/update/end
   converge under duplicate/out-of-order delivery. Resolve, result-read where applicable, deletion and attempt
   end emit a terminal update/tombstone; a delayed tick cannot resurrect it.
6. `NotificationHostMode` accepts only authenticated owner-local/loopback observations and performs outbound
   HTTPS delivery. A packet/listener oracle enumerates every interface/port and proves that it never constructs
   or binds public HTTP, WebSocket or renderer services, even when public bind settings exist. Ordinary remote
   GUI mode is tested separately and cannot be enabled implicitly.
7. Packaged tests cover desktop background, client killed, headless service restart, network outage, replay,
   revocation during a queued batch, expiry, dead token, two concurrent children, presence races, resolved
   deep links and late live ticks. No path changes queue order, marks read, acknowledges, responds to or
   resolves the underlying Attention demand.

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
4. Zero target bytes exist before explicit Insert/Send. Dictation into an exact free-text question uses the
   normal `respond_to_agent_interaction` response receipt: a definitely queued/submitted response and an
   ambiguous `submitted_unconfirmed` write remain distinct until evidence for that same instance/attempt/
   prompt proves resolution. Permission dispatch/evidence states are not available to dictation. A spoken
   word never resolves anything by itself.
5. Permission, password/credential, provisional/unassigned, raw-TTY and unverified alternate-screen demands
   expose no dictation action. “Yes”, “allow” and similar transcript text can neither invoke an approval nor
   call `route_attention`, `activate_session` or a lifecycle operation.
6. Blur, Escape, selection/attempt/prompt/surface-generation change, device loss, timeout, worker crash and a
   late result all stop/cancel without retargeting or changing Attention. After the sensitive state clears,
   the governor either applies its original still-valid route or degrades it under the normal policy.

The complete local-input contract is `docs/LOCAL_VOICE_INPUT.md`.

## Accepted ADR-067 Attention sound-cue boundary

`ACP-ATT-013` is an independent oracle, not evidence from the visual queue tests:

1. A deterministic canonical edge fixture emits `done` only for a fresh working→idle/completed result revision
   with no actionable demand, and `needs_you` only for a fresh current PendingInteraction/Attention demand.
   Running ticks, ordinary output, provider text, duplicate/replayed/stale revisions and chat/status events emit
   no cue.
2. Supported playback begins within300 ms, lasts≤2 seconds and distinguishes the two signed built-in assets.
   Visible enable/mute,0..1000 volume,≥2-second per-subject cooldown,≤8 cues/client/10 seconds,16/client and
   128/256-KiB installation limits are exact; late or saturated events drop rather than evict/replay.
3. Every fixture proves the queue/route/focus/unread/result and PendingInteraction revisions are byte-for-byte
   unchanged by enqueue, playback, mute, failure and completion. A cue cannot create, route, acknowledge,
   snooze, dismiss or resolve Attention.
4. Restart/reconnect restores no cue and never autoplays. Missing/failed audio remains labelled while visual,
   screen-reader and structured Attention evidence stays complete and within its normal latency budget.
