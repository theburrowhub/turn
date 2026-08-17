# Gemini CLI, OpenCode, and external-app acceptance

Run the complete reproducible check without opening Turn:

```sh
make adapter-acceptance
```

This verifies the dedicated adapter selection, safe launch degradation, hook or
plugin configuration, normalized lifecycle/permission/question/failure events,
confidence and source attribution, resume arguments, and graphical-process tree
placement.

## Supported contracts

- Gemini CLI `0.46.0`: `crates/turn-agents/tests/fixtures/gemini-cli-0.46.0.json`
  follows the hook reference bundled with that release. Turn subscribes to
  `SessionStart`, `BeforeAgent`, `AfterAgent`, `BeforeModel`, `BeforeTool` for
  `ask_user`, `Notification`, and `SessionEnd`.
- OpenCode `1.18.16`: `crates/turn-agents/tests/fixtures/opencode-1.18.16.json`
  follows the event bridge and schema sources at Git tag `v1.18.16`. The fixture
  includes the tool's own `info.version` field.

The version is part of each fixture name and adapter contract constant. Do not
edit an old fixture in place for a new tool release: add a newly versioned file,
run the contract tests against both, then remove the old one only when support is
intentionally dropped.

## Live smoke test

The contract suite is offline and deterministic. Before changing a supported
version, repeat this live smoke in a disposable Turn Session using the real CLI:

1. Start `gemini`, send one prompt, provoke one `ask_user` question and one tool
   permission, then exit. Confirm the Agent moves active → question/permission →
   completed, reports its session id and model, and can resume.
2. Start `opencode`, send one prompt, provoke a question and permission, create a
   child session, then exit. Confirm the parent/child hierarchy and the same state
   transitions, then resume with the recorded session id.
3. From either agent launch Godot or Blender. Confirm an `EXTERNAL APP` child is
   shown under the launching process, selection leaves desktop focus untouched,
   and its inspector says the interface lives outside Turn.
4. Repeat with Gemini hooks disabled and with `opencode --pure`. Both CLIs must
   still start; the inspector must report inferred integration.
5. Stop `turnd` during a callback. The CLI must continue without a warning or an
   extra interaction.

Capture callback JSON from the disposable hook endpoint, remove user content and
secrets without changing field names or types, save it under the exact observed
tool version, and rerun `make adapter-acceptance`.

## Accepted provider-parity target

The checks above prove the current Gemini/OpenCode adapter surface; they do not prove ADR-062's complete
provider-neutral topology. Before Turn advertises that target, Claude Code, Codex, Gemini and OpenCode run
the same capability-driven fixture/degradation suite, the generic fallback and user-declared custom adapter
run the honesty suite, and every advertised live-dependent cell carries a dated authenticated record.

### One complete capability vocabulary

The shared manifest has exactly one row for each canonical capability below. A provider may report
`supported`, `unsupported`, `degraded` or `unknown`; it may not omit a row or invent a provider-only synonym.

| Capability | Common contract exercised |
| --- | --- |
| `launch` | New instance/attempt, requested-versus-effective launch receipt and failure before partial registration. |
| `resume` | Exact provider conversation plus profile/target binding, fresh attempt lineage and stale-id refusal. |
| `branch` | Exact source conversation/revision, distinct destination identity and unsupported-with-zero-effect path. |
| `stop` | Idempotent exact-attempt stop, uncertain-effect receipt and no process-name fan-out. |
| `structured_status` | Total native map into independent Lifecycle and TurnState axes, with source/revision/freshness. |
| `questions` | Exact pending interaction, correlated answer/cancel and no free-text fallback for a typed permission. |
| `permissions` | Typed scope/options, exact attempt/turn correlation and durable resolution evidence. |
| `subagents` | Versioned snapshot/delta/gap topology, stable semantic identity and attempt-fenced parentage. |
| `transcript` | Per-conversation ordered cursor, bounded content, provenance and explicit unavailable/gap state. |
| `context_usage` | Scoped used/limit/unit/window facts with observation time, expiry and honest unavailable state. |
| `provider_quota` | Account/profile-scoped remaining/reset/unit facts, rate-limit/error degradation and no fabricated zero. |
| `model_switch` | Requested/effective model receipt, exact current attempt and unsupported-before-effect behavior. |
| `messaging` | Structured destination, delivery/evidence state and refusal rather than generic PTY injection. |
| `context_transfer` | Budgeted packet capability, exact source/destination attempt and reviewed delivery receipt. |
| `shared_identity` | Proved provider conversation identity without merging Turn AgentInstances. |
| `durable_attach` | Exact endpoint/runtime generation reattachment and stale/mismatched identity refusal. |
| `delegated_control` | Closed operation vocabulary, bounded grant and correlated accepted/refused/uncertain receipt. |
| `native_jobs` | Provider-owned scheduled, recurring or background job plus iteration identities, lifecycle, schedule and exact control receipts; never inferred from a Turn Flow. |
| `conversation_inventory` | Profile/target-scoped history enumeration, bounded pagination/search, freshness, exact identity matching and adopt/resume eligibility. |
| `title_read` | Bounded provider-title observation with source revision/freshness and an explicit unavailable state, independent of rename support. |
| `conversation_rename` | Revision-fenced provider rename with requested/effective receipt and zero-effect refusal, independent of title-read support or Turn's local display alias. |

The fixture manifest is bijective with these rows and the production capability manifest. Shared fixtures,
provider fixtures, degraded/unsupported/unknown fixtures and live evidence all use the same identifiers.
Unknown native values, missing fields and illegal state transitions fail the adapter contract rather than
falling through to an inferred provider behavior.

Capability state is scoped to `(adapter version, AccountProfile, ExecutionTarget, RuntimeEndpoint/mechanism)`
and carries observation time, expiry, reason and remediation. It is never a provider-global boolean. Expired
authentication, profile isolation loss, target outage, mechanism failure or stale evidence degrades only the
affected cell to `degraded|unsupported|unknown`; it cannot borrow a sibling profile/target's proof or silently
fall back to another credential, endpoint, generic PTY or local implementation. Restart begins live-dependent
cells as unknown until current evidence arrives, while independently proved capabilities remain usable.

### Generic and custom honesty suite

An unrecognised command always selects the generic terminal adapter. A user-declared custom adapter always
selects deterministically from its declaration. Each is run through all 21 vocabulary rows and must:

1. advertise only evidence it can prove, with an explicit state, mechanism, limits, reason and remediation;
2. leave Lifecycle and raw terminal/process observation available without fabricating TurnState, questions,
   permissions, subagents, quota, transcript, resume, native jobs, conversation inventory, provider titles,
   provider rename or structured identity;
3. reject an undeclared operation before launch/input/network/file effects and without falling through to a
   dedicated provider implementation; and
4. degrade one capability independently, preserving terminal input, selection and all unrelated capabilities.

Fixtures cover an unknown binary, a shell, a renamed wrapper, a valid minimal custom declaration, every
invalid/undeclared custom field, an unavailable executable and a declaration whose evidence mechanism fails
at runtime. UI and protocol snapshots distinguish unsupported, degraded, unknown and stale from exact zero.

### Topology and count matrix

The child-topology smoke asks Claude Code for exactly three children and Codex for exactly five, then repeats
for Gemini and OpenCode whenever they advertise `subagents`. It verifies parent/child ids, UI reconnect,
daemon recovery, duplicate/out-of-order/stop-without-start evidence and degraded mechanisms. Count assertions
cover the full Cartesian product below at one graph revision:

- metric: `semantic_children`, `live_children`, `completed_children`;
- scope: `direct`, `descendants`; and
- parent lifetime: `current_attempt`, `instance_lifetime`.

That is 12 independently expected cells for each fixture revision. Fixtures include a nested child, a
completed child with no active attempt, a completed-but-still-live child, spawning/orphaned/conflicted/lost
children and an old parent attempt. A separate valid empty authoritative snapshot proves `exact(0)` for every
applicable cell; an empty best-effort source remains unknown. Event 1,025 forces bounded overflow/gap and
immediately degrades exact coverage. A matching asynchronous snapshot restores it without blocking input.
Partial `N+ observed`, unknown, unsupported and stale are separate protocol/UI values. The expected event and
snapshot manifest is generated independently and never invokes the production aggregate query.

### Capability dispatch

`launch`, `resume`, `branch`, `model_switch` and `stop` dispatch only through the capability-selected adapter
method. The matrix exercises supported success, adapter-declared degraded behavior, unsupported/unknown,
timeout, cancellation, stale capability, wrong version/profile/host/endpoint/attempt/epoch and duplicate
operation id. Unsupported or mismatched calls fail before side effects. The daemon, store, protocol, UI and
Attention reducers contain no provider-name re-check after registry selection. Question/permission answers,
messaging, context transfer, durable attach, delegated control, native-job control, conversation adoption/
resume and rename likewise select their typed method from the current capability intersection plus authority;
no operation silently falls back to PTY text. `title_read` and `conversation_rename` are dispatched and degraded
independently: a locally edited display alias never counts as a provider rename, and a provider rename receipt
never fabricates the ability to read the effective provider title.

### Foreground Session activation

`ACP-LIF-009` removes the generic secondary “start pane” interaction without making selection a broad launch
capability. Exactly one foreground selection of a canonical Session row resolves its bounded eligible saved-
runtime descriptor set, or a declared default Shell when the Session has none, and automatically performs
the preflighted attach/start plan. Tree expansion, hover/preview, search results, references, restore, reconnect,
Attention routing and background sync are not foreground selections and perform zero launch/attach effects.

Before any process effect, one revision-fenced preflight resolves and freezes the Session, bounded
descriptor/default set, WorkSurface, every ExecutionTarget, cwd/worktree containment, write lease and
AccountProfile where applicable,
adapter/version/capability generation, effective argv/environment policy and current operator authority. A
missing or stale fact refuses before spawn and leaves selection usable; the exact reason and recovery appear
in the WorkSurface and bottom status history, not behind a generic start button. The preflight operation id
and reserved attempt identities make rapid reselection/reconnect idempotent. An ambiguous spawn is reconciled
against its identity and is never repeated automatically. Existing pending permission, credential,
destructive or host-trust work is surfaced through Attention and cannot be bypassed by autoactivation.

The acceptance fixture selects Sessions containing an already-running child, several eligible saved/stopped
descriptors, an empty default-Shell plan, a missing executable, a stale profile/capability, a checkout
conflict and an uncertain launch. Every eligible
case needs one selection and zero follow-up actions; every refusal creates zero process effects, never changes
the requested target, and never offers a provider-generic PTY fallback.

### External WorkItemSource contract

`ACP-VIE-012` is a separate integration-adapter contract; it does not extend an agent's authority. A
`WorkItemSource` manifest declares source/version, credential-reference kind, supported filters, cursor and
page limits, rate-limit semantics, writable fields and an exhaustive native-to-`WorkItemState` mapping.
Each imported item is keyed by `(source_id, source_profile_id, project_namespace, external_item_id)`, never
title, URL or ordinal. Per-field
authority is explicit (`source|turn|reviewed_merge`), and unknown native state, assignee or field is
preserved as unmapped/degraded rather than coerced into a known value.

The source suite exercises initial and incremental sync, saved filter changes, every page boundary, duplicate
and out-of-order webhook/poll observations, a missing page, cursor expiry, cache restart and deletion from the
query result. Every projection carries source revision/watermark, coverage (`complete|partial|gapped`),
`observed_at`, expiry and stale reason. A filtered-out or temporarily absent item is never silently deleted.
Writes use the source's exact compare-and-swap token; conflict retains both the local proposal and latest
source value, raises one reviewable conflict and performs no last-writer-wins retry. Close and reopen are
different mapped mutations with separate receipts. Unsupported reopen is an explicit zero-effect refusal.

Rate limiting publishes retry time and keeps the last cache visibly stale; it never clears the board or
reports an empty authoritative result. Credentials remain a broker/keychain reference scoped to the source
and ExecutionTarget, never appear in a manifest, cursor, cache, log or diagnostic. External assignees map by
stable source identity to an exact local identity or remain visibly unmapped; an assignee never grants runtime,
Flow, Attention or repository authority. Source create/edit/close/reopen and sync all remain inert with respect
to Lifecycle, TurnState and the canonical runtime hierarchy.

### Native jobs and conversation continuity

`ACP-ADP-011` treats a provider-native scheduled, recurring or background job as a provider object with a
stable `NativeJob` identity and separately stable `NativeJobIteration` identities. Its schedule/time zone,
enabled state, next/last observation, survival across provider/Turn restarts, iteration lifecycle and
freshness come only from the `native_jobs` adapter method. Turn Flow recurrence remains a different authority;
neither object is inferred from or silently converted into the other. Dismiss acknowledges/hides a Turn
projection only, cancel affects the exact current iteration when supported, disable changes the exact
provider schedule, and delete removes the provider job only through distinct revision-fenced methods and
receipts. Timeout or ambiguous effect becomes reconcile-required and is never retried by name or schedule.

`ACP-CTX-013` requires `conversation_inventory` to enumerate active and historical conversations inside the
exact provider, AccountProfile, ExecutionTarget and provider namespace. Pages are bounded and cursor-stable;
search declares whether it is provider-side or over a complete, fresh local cache. Each result carries the
global conversation key, safe timestamps/state, source revision, coverage/freshness and match evidence.
Missing pages, rate limits and unsupported search are visible and can never produce an authoritative zero.
Titles are optional `title_read` observations, not identity.

Adopt binds one proved live conversation/runtime without spawning; resume creates a new fenced attempt from
one exact resumable conversation. Both revalidate inventory generation, profile/target, global conversation
ownership and current capability immediately before effect. Ambiguous matches, duplicate ownership, stale
rows and display-title-only matches are refused. Inventory access does not imply transcript access, and
search results never carry body content unless the independent bounded `transcript` capability authorises it.

### Remote permission and Companion observation

`ACP-ATT-011` permits a remote or Companion client to resolve a provider permission only when its negotiated,
versioned default-deny operation allowlist contains `submit_permission_response` and an explicit response grant binds
client, Workspace/Session, AgentInstance, RuntimeAttempt, interaction id, allowed options and expiry. Only an
authenticated local foreground operator may issue, expand or revoke that grant. The
encrypted authenticated request contains the exact typed option, operation id, expected interaction revision,
binding/connection generation and anti-replay nonce. The daemon revalidates all of them plus the adapter's
current `permissions` capability immediately before dispatch and returns a durable accepted/refused/uncertain
receipt. Credentials, free-form secret entry, host trust, grant administration and destructive administration
remain local-only and cannot be added by a client-advertised capability.

While Turn has a typed sensitive interaction pending, remote/Companion raw PTY input to that binding is
blocked; only its exact typed answer method can resolve it. No permission operation falls back to terminal
bytes. For an opaque generic TUI Turn cannot prove that arbitrary terminal text is or is not an approval: the
surface states that limitation, never labels raw input as a permission decision, and may disable remote raw
input by policy. Tests therefore claim prevention only for Turn-recognised typed interactions.

`ACP-SCL-010` gives the Companion a per-AccountProfile usage/context/activity inbox, not installation-wide
facts. Every usage cell names profile, provider scope, unit/window, used/remaining/limit when supplied,
`observed_at`, expiry and source; an unavailable, stale or rate-limited collector never becomes numeric zero.
Activity rows keep provider event identity, bounded safe summary, time, freshness and read/handled state.
They become Attention only through the normal typed reducer, never because the inbox is unread. AccountProfile
isolation, paging, reconnect and cache tests prove that no sibling profile's samples, conversation identity or
activity enters the selected profile, and degradation of any one collector leaves the other capabilities
honest and usable.

### Shared live endpoint isolation

One authenticated live fixture binds exactly five simultaneous AgentInstances and five distinct conversation
ids to one `RuntimeEndpoint` generation: three instances use `AccountProfile A` and two use `AccountProfile B`.
The provider must advertise multiplexing across those two profiles; otherwise the capability is unsupported
and the test does not approximate it with multiple endpoints. The fixture interleaves unique canary prompts,
transcript pages, context/quota observations, questions and Attention demands for all five and proves:

- exactly one current owner per conversation and at most one current binding per instance;
- no input, transcript cursor, context grant, quota attribution, Attention subject or writer lease crosses a
  binding or either profile;
- a duplicate conversation claim, cross-profile handle, sibling operation and late old-generation event is
  refused with zero effect on the five current bindings;
- saturation/backpressure on instance 2 leaves instances 1, 3, 4 and 5 within their latency budget; and
- endpoint disconnect/crash/restart makes each binding stale independently, then exact reattach or explicit
  per-instance fallback creates correct lineage without merged identity, duplicate launch or cross-talk.

The run is not accepted with `N`, fewer than five bindings, one profile, a mocked service or one endpoint per
instance. Its transcript and Attention assertions use per-binding canaries that are removed from published
artifacts after their absence outside the intended binding has been proved.

### Authenticated live-evidence manifest

Every live-dependent advertised capability cell links one immutable record with all of these fields:

- evidence schema/id, capability id and claimed `supported|degraded` state plus mechanism and limits;
- adapter id/version, provider and exact CLI/service version, executable or artifact digest and Turn commit;
- pseudonymous AccountProfile id, ExecutionTarget/host id, RuntimeEndpoint id/generation, attempt/epoch and
  fixture id, omitting credentials and user content;
- authenticated test identity/environment class, invocation id, start/end time, observed-at, expires-at and
  freshness policy;
- expected oracle, actual typed result, per-step result, timeout/resource/quota consequences and final pass;
- raw-record digest/signature or trusted CI attestation, fixture/manifest digest, redaction version/result,
  cleanup receipt and links to bounded encrypted artifacts; and
- failure/downgrade reason, remediation and superseded-record id when the cell is no longer current.

CI rejects an advertised live-dependent cell whose record is absent, failed, expired, for another capability
scope/version/profile/host/endpoint, unauthenticated, digest-invalid, unredacted or missing cleanup proof. A
record proves only its exact cell; one provider/profile cannot approve another.

Every run also captures the integration diagnostic: detected CLI version, configured/effective mechanism,
last successful invocation, last valid/rejected event, achieved level, freshness, downgrade reason/remediation
and redacted export. The frozen obligations are `ACP-TOP-001` through `ACP-TOP-009` and `ACP-ADP-001`
through `ACP-ADP-011`, plus `ACP-LIF-009`, `ACP-VIE-012`, `ACP-ATT-011`, `ACP-CTX-013`,
`ACP-RUN-011`, `ACP-OBS-009` and `ACP-SCL-010` in `docs/CONTROL_PLANE_ACCEPTANCE.md`.

Primary contract references:

- Gemini CLI: <https://geminicli.com/docs/hooks/reference/>
- OpenCode plugins: <https://opencode.ai/docs/plugins/>
- OpenCode configuration merging: <https://opencode.ai/docs/config/>
- OpenCode CLI session flag: <https://opencode.ai/docs/cli/>
