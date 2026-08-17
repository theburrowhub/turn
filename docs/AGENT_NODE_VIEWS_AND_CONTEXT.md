# Agent node views, durable instances, and context routing

**Status:** Accepted product and architecture target; not yet implemented unless a requirement is explicitly
marked as existing. This document defines the post-v0.1.0 direction and the acceptance boundary for its
implementation.

**Precedence:** This specification extends ADR-040, ADR-043 and the unified hierarchy. It supersedes the
click-to-zoom interaction in ADR-048. Where older documents say that tree selection changes nothing, read
that as “selection changes no durable Layout, process, terminal focus or Attention state”; selection now
chooses the content shown in the surface to the right of the tree.

## 1. Outcome

Turn keeps the product thesis that distinguishes it from a terminal multiplexer or an agent dashboard:
the scarce resource is the operator's attention. The application must identify the exact agent that needs
a decision, permission, answer or result review, rank that demand in one daemon-owned queue, and take the
operator to the actionable place with one interaction.

The same model must also make agents comprehensible when they do not need the operator. Every durable item
under a Session is a node in the existing left hierarchy. Selecting a node switches the single content
surface to that node's unique view. It does not create a Pane, zoom a Pane, move a process, change the saved
Layout or imply that the node owns a terminal.

The target hierarchy is:

```text
Workspace
└── Session (owns checkout/worktree, Layout and Attention policy)
    ├── optional explicit Group
    │   └── Shell, standalone Agent or resource
    ├── Shell (owns a local PTY and its Pane bindings)
    │   └── Agent
    │       ├── semantic subagent
    │       └── managed process or tool
    ├── standalone Agent (only when its adapter proves a non-shell runtime)
    └── shell or process without agent identity
```

Canvas coordinates, overlapping cards and graph edges are not part of Turn. Ownership stays legible as a
tree; context access and conversation lineage are separate typed relationships.

## 2. Non-negotiable invariants

1. **Attention remains central.** There is one global logical `AttentionQueue`, ordered by the daemon. Node
   views project it; they do not create a competing queue or a second attention state.
2. **One tree, one content surface.** The Workspace hierarchy is the only navigation home. Everything to
   its right is one `WorkSurface`, even if a view internally uses headers, tabs or a details drawer.
3. **One selected node, one unique view.** Selecting an Agent or child shows content for that semantic
   subject, not merely the nearest ancestor terminal.
4. **Agent/child selection is navigation, not execution.** It never launches, cold-resumes, stops,
   acknowledges, approves, writes to a PTY or mutates the saved Layout. Activating a Session is a separate
   typed intent—even when one click both selects and activates it—and retains ADR-049's safe connected-
   client auto-start contract.
5. **Views do not own work.** `AgentNode`, `AgentInstance`, `RuntimeAttempt`, `Pane` and provider conversation
   identity are different concepts with different lifetimes.
6. **Observed state wins over requested state.** Model, permissions, sandbox, account, flags, host and
   integration mode show requested and effective values separately. Unknown stays unknown.
7. **Context access is explicit authority.** Being a child, sharing a Pane, checkout or provider does not
   grant transcript access. A context link does not create parentage or execution authority.
8. **Delivery is not acceptance.** A successful write or provider call means submitted. Only provider
   evidence can mark received, read or acted observations, and none is inferred merely to fill another.
9. **No invisible downgrade.** Remote failure never launches locally, resume failure never becomes a fresh
   conversation, and unsupported launch flags never silently disappear. A visible effective launch receipt
   records every fallback.
10. **Secrets never become metadata.** Safe flag names and typed modes may be displayed; tokens, credential
    values and raw environment values may not.

## 3. Domain vocabulary

### 3.1 Node and relationship axes

`NodeId` remains the stable semantic identity rendered in the tree. Nodes use a closed, extensible kind:

- `Agent` — an operator-visible agent identity;
- `Subagent` — an agent reported by another agent, possibly without its own PTY;
- `Shell` or `Process` — a runtime/tool without an independent agent identity;
- `Note`, `File`, `Diff` or `Web` — user-created resources when their capability ships;
- `Group` — an explicit organisational boundary inside one Session, never an inferred process relationship.

Each kind declares a truthful `ContentCapability`: live terminal, structured activity/transcript, file,
diff, web, note, group overview or technical process detail. A client must not render a semantic subagent
as an empty terminal merely because an ancestor owns one.

ADR-044's local ownership remains: a Shell node owns the PTY and Pane bindings, while the Agent launched
inside it is a confirmed child. An AgentInstance's current attempt may carry a verified `RuntimeBinding` to
that Shell/runtime owner. Its AgentNodeView may project the bound live terminal while keeping the Agent as
the visible semantic subject; it never claims that the Agent owns the PTY. A provider-side thread with no
local shell uses a different adapter capability and does not fabricate one.

`Group`, `Note`, `File`, `Diff` and `Web` are accepted resource-node kinds but are sequenced after the first
Agent Node milestone. A Group is explicit presentation inside one Session and owns no checkout, lease,
Attention policy or runtime. Notes are Turn-owned private content; File and Diff nodes refer to canonical,
checkout-confined sources and never own user files; Web nodes hold a validated URL and perform no network
load merely because the tree restored. Removing any resource node forgets Turn's record, not the referenced
file, branch or site. Their creation, persistence, privacy and content-security acceptance is M14 scope.

The supported flow accepts resource creation/edit only from a foreground operator operation. A Group is a direct child of its
owning Session and Groups never nest. Any Agent, Shell/Process or resource in that Session may have at most
one explicit presentation membership in a Group; changing it does not rewrite the runtime/process parent,
which remains visible as a reference in the Node View. Resource payloads cannot reparent themselves. Deleting
a non-empty Group requires the explicit safe `reparent_children` disposition; it never cascades. Titles are
sanitised/bounded. Note text is bounded and stored exactly as private user content, while its projection is
escaped and inert. File/Diff references use the same descriptor-relative regular-file jail as repository
context and render unavailable rather than following a moved target outside the checkout. Markdown, SVG,
HTML and other active-looking File/Diff/Note content render inert with no script or remote loads.

The initial Web kind accepts only `https://host[:port]/path`: userinfo, query and fragment are all refused,
as are non-public IP ranges and `file:`, `data:`, `javascript:`, custom/IPC schemes. The complete stored URL,
including its path, is private content rather than safe display/log metadata; tree and audit projections show
only a sanitised origin. For every connection and redirect Turn resolves every A/AAAA answer, rejects the
whole answer set if any address is non-public, chooses an approved address and pins the socket to that exact
address while preserving TLS SNI and the HTTP `Host`; redirects repeat the process and cannot downgrade
scheme. A second resolver lookup cannot choose the connected address, and the initial implementation does
not use an ambient proxy. Web runs in an isolated origin with no inherited cookies, provider/daemon
credentials, filesystem/IPC access, downloads, popups or automatic external navigation. Content loads only
after explicit foreground navigation. Restore reconstructs private content and an unloaded view only; URL
changes and navigation never happen from page script without a new typed foreground operation.

Three edge families remain independent:

| Edge | Meaning | May affect tree placement | Grants context read | Transfers work/control |
| --- | --- | --- | --- | --- |
| `HierarchyEdge` | owns, spawned, contains or explicitly grouped | yes | no | no |
| `ContextLink` | one AgentInstance may pull bounded context from another | no | yes, within scope | no |
| `LineageEdge` | an instance continued, handed off or branched from another | no; shown as reference | no by itself | records an explicit operation |

Process-derived hierarchy edges retain their confidence. Context and lineage must never be inferred from
process ancestry, matching directories, shared accounts or similar titles.

### 3.2 Stable agent identity and runtime attempts

An `AgentInstance` is Turn's stable identity for one operator-recognisable agent. Its `AgentInstanceId`
survives warm view attachment and survives cold resume, model switch or runtime restart only when the
adapter verifies continuity of the same provider conversation. For the first migration a current agent
`NodeId` may also be its instance id, but the concepts remain distinct in APIs and storage.

Every agentic Node owns exactly one AgentInstance; non-agent nodes own none. An AgentInstance belongs to one
Node for its lifetime and owns zero or more RuntimeAttempts, with at most one `current_attempt_id`. Attention
and visible selection carry `node_id + agent_instance_id` when agentic and the daemon rejects a mismatched
pair; attempt-scoped interactions additionally carry the current `runtime_attempt_id` and generation.
ContextLinks and handoff destinations name AgentInstances and revalidate the same join. This one-to-one join
is daemon-derived and never reconstructed from provider ids, cwd or titles. A create, branch or new-target
handoff commits its Node + AgentInstance pair together in one store transaction; neither half can exist
alone. External launch remains the separate visible saga described below.

A `RuntimeAttempt` is one concrete runtime-configuration epoch. It starts with a launch/resume or with a
verified in-place model switch that changes the effective execution contract while preserving the same
provider conversation and process binding:

```text
AgentInstance
├── RuntimeAttempt 1 (ended)
├── RuntimeAttempt 2 (lost)
└── RuntimeAttempt 3 (current)
    ├── provider conversation/thread id
    ├── PTY/runtime binding
    └── descendant processes
```

The identifiers must not be substituted for one another:

| Identity | Lifetime and authority |
| --- | --- |
| `NodeId` | semantic tree subject and view target |
| `AgentInstanceId` | stable agent history across attempts |
| `RuntimeAttemptId` | one launch/resume/configuration epoch and its evidence |
| `ProviderConversationId` | vendor-owned conversation/thread; optional and capability-scoped |
| `RuntimeId` / PTY id | concrete process transport; never semantic identity |
| `PaneId` | a view binding only |

An adapter may share one provider runtime service across many nodes while preserving an isolated provider
thread per `AgentInstance`. That topology is an adapter fact and must not collapse the nodes. Conversely,
one node may have a dedicated CLI process. The UI presents the stable agent first and exposes the topology
as technical detail.

Launch/resume attempts own an immutable `LaunchSpec`/`LaunchReceipt`. An in-place switch never fabricates a
launch: it owns a `RuntimeConfigurationReceipt` with previous/requested/effective model, adapter proof of the
same conversation and process binding, observation time and transferred binding generation. Ending the old
epoch and installing the new current attempt is one fenced store transition; attempt-scoped capabilities
rotate even though the OS process continues. An unverified switch remains an explicitly uncertain runtime
observation and cannot claim continuity or rewrite either receipt.

Workspace, Session, provider, safe account scope, host and checkout/worktree binding are immutable instance
facts. A fresh provider/account/host/worktree or unverified conversation creates a new Node + AgentInstance
with an explicit lineage edge; it never rewrites the old identity. A model switch or cold resume stays on
the instance only when the adapter verifies continuity of the same provider conversation. Failure to prove
continuity is a refusal, followed by an optional explicit fresh-instance action—not a fallback.

### 3.3 Runtime specifications and receipts

Every launch/resume attempt has an immutable `LaunchSpec` and `LaunchReceipt`. A verified in-place model-
switch attempt has no launch record; it has the `RuntimeConfigurationReceipt` defined above.

`LaunchSpec` records requested provider/tool, model, account reference, permission and approval modes,
sandbox, safe flag names, cwd, checkout/worktree, host, resume intent, provider conversation, adapter and
source of each override. User arguments and Turn-injected arguments remain distinguishable.

`LaunchReceipt` records what actually happened: effective model/account/modes, adapter and CLI versions,
safe effective flag names, host/cwd/worktree, provider conversation id, runtime id, start time, integration
level, capabilities, unsupported options, fallbacks and evidence provenance. It never stores secrets or raw
environment values.

Requested, effective and current runtime values are separate. For example, a provider may accept a
requested permission mode, fall back at launch, and later change mode interactively. The node view must
show all three without rewriting history.

### 3.4 Lifecycle operations

- **Warm attach:** connect a view to the still-live attempt. It never launches a command.
- **Cold resume:** create a new attempt that resumes a verified provider conversation.
- **Fresh start:** create a new Node + AgentInstance and provider conversation. “Replace” may archive the
  old node and select the new one in the same presentation position, but never reuses its ids.
- **Restart runtime:** create a new attempt under the same AgentInstance only with verified conversation
  continuity. Previous attempts remain history; otherwise the operation refuses and offers Fresh start.
- **Switch model in place:** when the adapter proves the same conversation/process and effective new model,
  atomically end the old epoch, create a configuration-receipt attempt and rotate attempt capabilities. It
  neither launches nor resumes anything; insufficient proof leaves the observed model uncertain.
- **Branch:** create a new Node + AgentInstance with an explicit lineage edge and an independently resumable
  conversation when the provider supports it.
- **Handoff/continue with:** target an existing instance or provision a new one through an idempotent,
  fenced saga, attach one bounded context packet and record lineage. It does not create a Pane.

The unattended daemon never launches work merely because it restored metadata. A connected client may
start only a runtime whose persisted auto-start contract is explicit and whose checkout, account, host,
command and safety prerequisites still resolve. Anything ambiguous becomes an actionable Attention entry,
not a guessed launch. A remote target that is offline fails closed; Turn does not execute locally in the
same-looking cwd.

`activate_session` is the explicit ADR-049 intent for that connected-client path. A single Session-row
gesture may select then activate, but selecting an Agent/child, applying an Attention route or restoring a
tree never invokes it. An Attention route may cross into a stopped Session to show the exact demand and
recovery state; it does not start or resume anything.

Attempt creation is fenced by the AgentInstance generation and a caller operation id. Exactly one attempt
may become current; concurrent restart/resume requests either return the idempotent result or lose the
compare-and-swap and resynchronise. A failed replacement never kills a still-live current attempt. Once the
old attempt has ended, a failed new one remains visible in history with no current attempt and one truthful
recovery action.

## 4. The WorkSurface and node views

### 4.1 View target

Each client surface owns one `ViewTarget`, derived from its selected `HierarchyKey`:

| Selected row | WorkSurface mode |
| --- | --- |
| Workspace | `WorkspaceView`: project, checkouts, Sessions and aggregate attention |
| Session | `SessionView`: the saved Pane Layout |
| Group | `GroupNodeView`: an overview of its children and references |
| Agent or Subagent | `AgentNodeView`: unique activity/conversation plus runtime and context controls |
| Shell or Process | `ProcessNodeView`: its live terminal when owned, otherwise technical detail |
| Note/File/Diff/Web | the matching typed resource view |

Changing `ViewTarget` is surface-local navigation. It preserves the selected Session Layout exactly. A
second surface may look at a different node. Re-selecting the same node is idempotent. Selecting its Session
returns to the Session Layout at the exact previous Pane focus and zoom. Explicit Pane open, split and zoom
commands continue to exist; a tree click is not one of them.

The current narrow inspector becomes an optional details region inside the active view. It must not form a
second navigation column or duplicate identity, usage and Attention controls already present in the node
header.

### 4.2 AgentNodeView

The view has four stable regions:

1. **Attention strip.** The exact outstanding demand, confidence, age, queue position and primary action.
   Permission and question are distinct typed presentations; questions show answer choices, while only a
   verified approval request may show allow/deny. A provider-native response targets the exact pending id;
   other adapters use verified PTY input. In both cases the operator, never Turn, chooses the response.
2. **Identity and runtime header.** Agent name, provider/tool, requested/effective model, account label,
   host, checkout/worktree/branch, lifecycle/turn state, resume capability and integration confidence.
3. **Primary content.** The live terminal of a verified Shell `RuntimeBinding`; structured activity/
   transcript for a semantic or provider-side agent; or an honest stopped/offline/lost state with the
   safest available recovery action. The header always names the semantic subject and the distinct input
   owner.
4. **Context and runtime details.** Context window, provider quota, launch receipt, attempt history, context
   links and handoff lineage. This may collapse, but its values remain accessible without opening another
   global inspector.

There is no generic “Start pane” empty state. Selecting a live resource attaches its view automatically;
that opens no process. A stopped, lost or cold-restorable agent shows one specific Resume/Restart recovery
action, and only that explicit lifecycle intent may create an attempt. ADR-049 Session activation remains a
separate safe auto-start path. A Pane is an implementation detail, not a task the operator must create before
seeing an agent.

### 4.3 Tree rows and status grouping

The normal tree preserves hierarchy and stable sibling order. Each agent row may show:

- `NEEDS YOU · INPUT` for an actionable permission, question, credential or input demand;
- `NEEDS YOU · REVIEW` for an enqueued turn completion, failure or result decision;
- `UNREAD` as an independent marker for a completed result not yet shown;
- `RUNNING` for current work;
- `IDLE`, `SLEEPING`, `OFFLINE`, `LOST` or `UNKNOWN` as truthful lifecycle states;
- compact context consumption when measured, never a fabricated zero.

Workspace and Session rows aggregate separate counts for queued Attention, unread-result and running. A
temporary status filter may flatten references with actionable entries first in the daemon's exact queue
order, then unread-only → running → idle → unknown. It must point to the same nodes and preserve the tree as
the navigation authority. It does not duplicate nodes or own separate read/attention state.

An unread completion is not automatically an input blocker, but Turn deliberately retains turn completion
as an Attention trigger: policy decides whether it queues a result for review. Read state, turn state and
`AttentionEntry` state remain independent. Selection alone never clears unread. After the foreground
WorkSurface has actually presented the exact result revision as primary content, it sends
`mark_node_result_read`; the daemon clears only that revision. This does not acknowledge or resolve a queued
completion. It is an intentional difference from a status dashboard that treats `done` as informational
only.

## 5. Attention routing is the primary interaction

### 5.1 Exact subject and action owner

Every agentic `AttentionEntry` names a validated `node_id + agent_instance_id` pair when Turn has evidence.
It may also name a distinct `interaction_owner_node_id`, current `runtime_attempt_id`, generation and exact
pending interaction id when a semantic subagent must be answered through an ancestor's verified terminal.
The subject remains visible before, during and after the route; the UI must not pretend the ancestor is the
agent that asked.

The daemon resolves an `AttentionRoute`, not the client. It contains `surface_id`, surface-connection and
daemon generation, `attention_id`, Workspace/Session, a tagged exact/provisional/unassigned subject,
optional interaction owner/attempt/pending id where verified, and a bounded Node/demand-view bootstrap
revision. vNext `route_attention` (the successor to v4
`goto_attention`), an aggregate or exact badge, a notification deep link, and an automatic `Effect::Focus`
granted by the governor all apply that route as one visual operation:

1. select the exact subject in its normal hierarchy, or its owning Session for a provisional demand;
2. activate its unique Node View or exact `ProvisionalAttentionView`;
3. reveal the typed question, approval or result-review affordance;
4. focus the verified input/action control when the route is actionable and safe.

Opening a closed Workspace or switching Session is part of that route. The operator must not then hunt for
a Pane, open a preview or press a second “focus” button. Automatic OS-focus effects still pass through the
existing focus governor; once granted, the same exact NodeView route is applied rather than merely raising
the window or Session. A user invoking `Next Attention`, clicking a badge or opening a notification has
already consented to navigation. An aggregate badge asks the daemon for the first actionable entry in that
scope using the global queue order. Notifications carry an opaque deep link to their exact entry and daemon
generation; activation revalidates it instead of trusting stale title/body text.

Each client reports surface-scoped activity. The daemon binds automatic Focus to exactly the focused
connected surface, or while the application is backgrounded, the most recently focused surface that remains
connected. The route carries that surface's connection generation. If it disconnects or is replaced, the
effect is denied/degraded to queue/badge and is never transferred to another window. A route is navigation
only and never calls `activate_session`.

Some current integrations can authenticate only a parent plus external worker id, or no node at all. The
route subject is therefore tagged: an exact Node/AgentInstance when known, otherwise a
`ProvisionalAttentionView` keyed by the immutable attention id and authenticated parent/external scope, or
an unassigned Session demand. The latter views show the evidence and resolution limit without fabricating a
Node or borrowing input. If later evidence binds an exact node, the daemon issues a new route revision.

### 5.2 Resolution semantics

- Visiting, selecting, focusing or scrolling a node never resolves Attention.
- `acknowledge` means “the operator saw this demand”, not “the agent received an answer”.
- Writing an answer or approval moves the demand to `delivery_pending`; it stays visibly unresolved until
  a trusted adapter event confirms the prompt closed or the turn resumed.
- If confirmation is unavailable, the state remains `submitted_unconfirmed` and offers explicit correction;
  it must not silently disappear.
- A new prompt replaces only the exact superseded prompt. Question and permission ids may not be joined by
  title, timing or Session alone.
- Agent interruption initiated by the operator does not create a false “finished” result.
- `TurnComplete` creates unread-result state and is resolved through the configured Attention policy. A
  real integrated event always creates a new unread result revision and the default policy badges and
  enqueues it for review. Explicit `Action::Nothing` may configure no queue/effect for that trigger; mute
  suppresses interruption only and does not remove badge, queue or unread evidence. A queued review remains
  until explicit acknowledgement/
  dismissal or a reducer-defined superseding event; merely opening the result is not that event. When an
  adapter cannot observe turn completion, the header remains visibly `UNKNOWN`/limited-integration rather
  than treating missing evidence as silence.
- Relaunch clears only attempt-scoped demands invalidated by the ended attempt. Instance-scoped review or
  handoff state remains attached to the stable AgentInstance.

Node views use the same queue actions—snooze, dismiss, priority and mute—and never maintain local copies.
After a confirmed resolution the UI may make `Next Attention` the default action, but it may not jump while
the operator is typing, editing a context packet or using a modal.

### 5.3 Telemetry does not steal attention

Context consumption, token counts, cost and provider quota are informational. They may warn visually, but
raw thresholds cannot move focus or reorder the queue. Only a typed, attributable runtime event such as
`ContextBlocked` or `QuotaExhausted` may create an Attention demand, with the normal source confidence and
policy guards.

## 6. Runtime metadata contract

Context-window consumption and provider/account quota are different measurements and must never share one
percentage or label.

### 6.1 Conversation context

`ContextUsageSnapshot` belongs to one stable `ContextScopeId` for a provider conversation; Agent/attempt
views reference rather than duplicate it. It contains an explicit measurement kind (`used`, `remaining` or
provider-reported `percent`), amount, unit, optional known total/effective window, source, `observed_at`,
adapter-defined `expires_at` and freshness. Turn computes a percentage or complement only from an exact
amount and total with compatible units. When the provider reports only a percentage or no measurement, the
missing fields stay absent. A cached last-known value is visibly stale.

### 6.2 Provider quota

`QuotaSnapshot` belongs to one stable `QuotaScopeId` and declares its real scope: safe account reference,
provider, organisation, host and optional plan. It may contain several windows, each with explicit
used/remaining/percent semantics, amount, unit, optional total, reset time, hard/soft classification,
source, `observed_at`, `expires_at` and freshness. Turn never derives “remaining” from a provider's “used”
unless the same sample supplies an exact total. Account-level quota shared by ten nodes is stored/rendered
once and referenced from each node; it must never look like consumption caused by that node.

### 6.3 Required node header fields

The compact AgentNodeView header prioritises current Attention, effective model, conversation-context
consumption, shared quota scope/reset, current permission mode and host/worktree. Any requested/effective
mismatch is promoted into that header. Expandable Runtime detail exposes, when known:

- semantic identity and stable instance id;
- provider, tool, adapter/CLI version and integration level;
- requested, effective and current model;
- safe account/profile label and quota scope, never credentials;
- context used/remaining and provider quota windows as separate groups;
- requested/effective/current permission, approval and sandbox modes;
- safe requested/effective launch flags plus visible omissions/fallbacks;
- local/remote host, cwd, checkout/worktree, branch and dirty/ahead/behind evidence;
- current attempt, process/runtime binding, start/stop time and resume capability;
- source, confidence, observation time and stale/unknown state for every dynamic fact.

Missing worktrees and offline hosts remain represented as `missing`/`offline`; destructive or local fallback
actions are unavailable. A verified same-conversation model switch creates a new RuntimeAttempt and
preserves the AgentInstance. An account/provider/host/worktree change creates a new AgentInstance and
lineage. The header never rewrites a previous attempt's receipt.

## 7. Context between agents

Turn supports two complementary channels. Neither is implied by the tree.

### 7.1 Live ContextLink: scoped pull

A `ContextLink` is a durable, revocable grant from source AgentInstance to destination AgentInstance. Only
an explicit foreground operator action on the authenticated control channel can create, expand or renew it;
an agent, hook payload, transcript or repository file may propose but never authorise one. The default grant
is directional; a bidirectional relationship is two grants. Initial scope is one Workspace, including
separate Sessions/worktrees; cross-Workspace links are refused because they cross an operator's project
boundary. Every grant has a purpose, closed scopes, cumulative request/byte/token limits and a required
expiry; “until either Session ends” is the longest default, not an unbounded capability.
Foreground create/update/revoke carry an operation id and are idempotent across a lost response; lifecycle,
expiry and endpoint-delete revocations are internal. Each source and destination counts the live link against
`records.active_context_links_per_agent`; reaching the bound refuses creation rather than merging grants or
silently revoking an existing one.

“Only the foreground operator” is an invariant of Turn's authenticated, supported control flow, not a claim
of hostile same-uid process isolation. A compromised local agent running as the operator could steal the
daemon's administrative capability or impersonate a UI unless a per-agent OS sandbox or a UI-owned authority
that the agent cannot access is active. The UI, threat model and acceptance tests state that limitation; the
daemon still rejects agent-event, hook, transcript and repository-file attempts to exercise these operations.

Durable means the grant survives a UI or daemon restart, not that it outlives its authority. Each link has a
fenced generation and records the source/destination Workspace, Session, provider, safe account scope and
host authority epoch. A daemon generation change invalidates every bearer; the durable link issues a new one
only after both endpoints and authority epoch revalidate. Every new RuntimeAttempt likewise rotates adapter
material and revalidates the link. A model
switch may remain active after validation; a provider, account, host or Workspace change suspends it and
requires explicit reauthorisation. Ending/archiving either Session revokes the link permanently because
ADR-047 makes those the same lifecycle action; restoring never reactivates it. Deleting either instance
first commits revocation and invalidates adapter-issued capabilities, then removes the node. Expiry and
manual revoke forbid any later broker-response commit.

The destination pulls only what its grant permits:

- `summary` — bounded stable facts and recent decisions;
- `activity` — normalised recent activity, never raw terminal bytes;
- `transcript` — provider-normalised turns or explicit ranges when an adapter exposes them;
- `repository` — verified Git facts and files inside explicit canonical roots and allowlisted paths.

Continuous terminal/scrollback pull is not an initial scope: ADR-052's VT archive cannot be redacted
reliably. An operator may select and review a bounded rendered `terminal_excerpt` for one ContextPacket, but
that is a one-shot disclosure, not a live link. Repository grants canonicalise every path, reject symlink
escapes, deny secret/config patterns such as credential files by default, and cap files and bytes. Opening is
descriptor-relative beneath an already verified root (`openat2` resolve-beneath/no-symlink/no-cross-device
where available, or an equivalent component walk with no-follow descriptors), accepts regular files only,
enforces limits while reading and revalidates device/inode on the opened descriptor. Mount crossings and
multiply linked files are refused unless that exact root/inode was separately reviewed and allowlisted. The
same jail runs independently locally and in the remote helper; a provider-reported transcript or repository
path is evidence to validate, never an authority to open it.

Links never inject a full transcript at creation time. They publish a small capability description to the
destination adapter and let it request the minimum context on demand. Every response is normalised,
control-stripped, passed through best-effort known-secret/secret-shaped redaction, provenance-labelled and
framed as untrusted quoted data rather than an instruction. That detector cannot prove arbitrary transcript
or file content secret-free: the reviewed allowlist, narrow scope and bounds are the security boundary. A
live link is not reviewed per read, and its creation UI warns that returned content may persist in the
destination provider. Reads are metadata-audited; returned bodies are not copied into Turn's semantic event
log.

Broker responses are bounded and non-streaming. A read atomically reserves its maximum request/byte/token
budget against the link generation so parallel reads cannot overspend, then obtains and buffers the data.
Immediately before exposing bytes it revalidates every endpoint/epoch/scope/expiry, adjusts the reservation
to the actual size and commits the read hash/audit record. Revocation that commits first returns no body; a
read that commits first is already disclosed, keeps its budget charge even if the connection drops, and
cannot be recalled. Failed pre-commit reads release their reservation.

Agents never receive the daemon control-socket capability. A separate local-only `ContextBroker` data plane
issues a high-entropy, short-lived bearer capability only after the operator-created grant exists. It is bound to
link id/generation, destination AgentInstance, current RuntimeAttempt, allowed source/scopes, purpose,
limits and expiry. The broker derives the destination from that capability—no caller-supplied destination
id can broaden it—rate-limits it, rotates it for every attempt and invalidates it on attempt end, archive,
delete, expiry or revoke. It is passed by an inherited descriptor or an owner-only, no-symlink attempt file,
never argv, environment, log, terminal or transcript. This prevents accidental cross-wiring, stale/replayed
authority and access by other OS users; without per-agent OS sandboxing it does not isolate a capability
from a malicious same-uid process that steals another attempt's file or memory. The grant UI and acceptance
state that boundary instead of calling logical destination binding a process sandbox.

Remote reads are executed on the source host through a jailed adapter path. Failure to reach that host or
prove the source path never falls back to a same-named local file. The remote channel requires mutually
authenticated pinned host identity, confidentiality/integrity, per-request nonce/idempotency and replay
rejection. It contacts the online source authority for every read and caches neither grants nor bodies for
offline use. Remote capability/socket/key files are owner-only, no-symlink catalogued data and never enter
argv, environment, logs or transcripts. Helpers prefer memory or unlinked descriptors, but a disconnected
host may retain a catalogued file after logical revoke/delete. Turn keeps a bounded cleanup tombstone,
reports `remote_residual`/`pending_purge`, and retries an authenticated host-scoped purge on reconnect until
the host proves the exact generation gone; it never reports physical deletion while that proof is absent.
Revocation still forbids every response commit after its online authority linearisation point and therefore
does not depend on remote file cleanup, but it cannot recall context the destination/provider already
consumed or retained; the UI states both limits before grant creation.

### 7.2 ContextPacket: one-shot handoff

A handoff is a versioned, bounded `ContextPacket` plus a `LineageEdge`. It supports continue-with, review,
second-opinion, delegation and branch intents. The first implementation is same-Workspace, including
different Sessions/worktrees; cross-Workspace transfer requires a future explicit export/import boundary.
The source may target an existing compatible instance or describe a new AgentInstance without changing
Layout.

The canonical packet is assembled from typed sources:

- objective, current task, completed decisions and unresolved work;
- verified repository root, checkout/worktree, branch, HEAD, status and relevant diff/file references;
- commands and exit codes, tests, managed processes and subagents;
- stable Activity Preview and adapter-normalised conversation turns when permitted;
- prior context lineage and explicit operator instruction;
- a manifest of omitted, redacted, stale and truncated material with provenance.

Raw PTY bytes, typed credential fields, hidden previews, arbitrary environment values and unbounded provider
transcripts are excluded. Provider parsers normalise user/assistant/tool turns, strip control sequences and
apply best-effort known-secret/secret-shaped redaction before budgeting. No detector proves arbitrary source
text secret-free; exact source allowlists, bounds and operator review are the disclosure boundary.

The budget reserves most of the target's effective context window for future work. The packet uses a typed
digest for older material and complete recent turns up to the lower of the configured cap and one quarter
of that window. When the window is unknown, a conservative configured cap applies. Optional source ranges
remain pullable through an accompanying short-lived grant rather than embedding a second full transcript.
That grant, its exact scopes, expiry and downstream-retention warning are part of the reviewed packet
manifest; delivery never creates implicit context authority.

The vNext `prepare_context_packet` operation replaces—rather than aliases—the implemented v4
`prepare_context_handoff`; their schemas and guarantees differ. It remains review-before-send because this
is a cross-agent disclosure boundary. Preparation creates only an expiring draft and optional target launch
spec: no node, process or grant exists yet. The review is rendered directly in the Node View rather than a
modal maze and shows the exact canonical sanitised body, known-secret redaction result, trusted transport
template/version, grant manifest and retention warning. `deliver_context_packet` carries only an operation
id plus the opaque draft capability, so neither body nor envelope can be replaced in flight. The canonical
body hash is passed unchanged into the reviewed transport encoder. A native adapter that exposes decoding
can prove the decoded body matches; PTY fallback proves only the deterministic submitted envelope/bytes, not
what the downstream program decoded.

The draft/capability is memory-only and bound to its preparing surface connection and daemon generation.
Disconnect before `deliver_context_packet` is accepted discards it. Once accepted, the current daemon may
retain the body only in the in-flight saga's bounded memory; durable state stores its hash/manifest/phases,
never reconstructible bytes. A daemon-generation change therefore cannot resume delivery from a hash. It
reconciles already attempted external effects, then: preserved submission evidence stays submitted; an
in-progress/ambiguous write becomes `submitted_unconfirmed`; otherwise any prepared/installed grant is
revoked and the delivery becomes `draft_lost`/`review_required`. The operator must prepare and review a new
packet, with a new operation id, before the already provisioned compatible target can receive anything.

Delivery revalidates that the target has no pending question, permission or other interaction. Adapter-
native delivery is preferred; a terminal fallback is one bracketed paste into the verified idle PTY owned by
the target RuntimeBinding's Shell. For a new target, delivery is a durable idempotent saga keyed by one
operation id: atomically commit the Node/AgentInstance as `provisioning`, preassign its attempt identity and
launch nonce, launch and fence the attempt, prepare any reviewed grant, deliver once, then record evidence
independently. Process launch, bearer installation and context submission are three distinct external
effects, each preceded by a durable intent. Only a phase proven not to have attempted its next external effect
may resume automatically while the same daemon generation still owns the reviewed in-memory body. Across a
daemon restart, reconciliation may fence/probe/adopt effects but never progresses to another effect or a
context write. A crash after a launch attempt but before its receipt becomes
`launch_unconfirmed`; recovery may probe and adopt only the process/endpoint matching the preassigned
identity, and never respawns automatically. An ambiguous bearer installation revokes that link generation
and becomes `grant_install_unconfirmed`; it is not installed again automatically. An in-progress or possibly
partial context write becomes `submitted_unconfirmed` and is never retried. A definitely failed launch
leaves a visible recoverable target and no context write; logical cleanup never pretends an external process
was transactionally rolled back. Clients query the durable delivery record and may explicitly retry only a
launch proven not to have happened while the reviewed draft remains live, adopt/terminate an exact
unconfirmed process, or delete the target. `draft_lost` always requires a new prepare/review rather than a
retry of the old delivery.

An optional reviewed ContextLink is committed `pending_activation` only after the target attempt exists. Its
short-lived broker bearer is installed through the adapter's inherited descriptor/owner-only attempt file;
the trusted packet envelope carries only a non-secret link descriptor, never the bearer. If a PTY-only
adapter cannot install that channel, delivery with a grant is refused rather than pasting authority into the
terminal/transcript. The grant becomes usable immediately before the one write. Launch failure, definite
write failure or uncertain write revokes it; evidenced submission leaves it active only for the reviewed
scope/budget/expiry.

Intent controls the reviewed recipient instruction. `context_only`, `review` and `second_opinion` ask for a
compact recap and then a stop; Turn claims that behavior only when adapter evidence observes it.
`continue_with` is offered as **Send & continue** only when the reviewed packet includes a separate concrete
next instruction; after an evidenced receipt the target may begin that instruction without another operator
round trip. It still cannot approve, expand context authority or infer missing work. With no reviewed next
instruction the operation is context-only, whatever its original label.

Control progress and semantic evidence are separate:

```text
draft → reviewed → delivery_started → finished | failed | submitted_unconfirmed
                         phase = provisioning | launching | grant_pending | submitting | awaiting_evidence

evidence = { submitted?, received?, read?, acted? }
```

Each evidence field has its own observed timestamp/source or remains unknown; observing `acted` never
fabricates `read` or `received`. Durable metadata records operation/packet ids, source/destination, target
generation, phase, intent, content/encoder hashes, budget/redaction/truncation flags, timestamps and evidence
sources. Packet bodies and provider transcripts receive no dedicated semantic body record in Turn. Delivery
necessarily copies the body downstream: it may persist in a provider transcript and, for PTY fallback, the
program's screen, Shell-owned scrollback and ADR-052 journal. Revocation cannot recall those copies. The
review names this retention; “ephemeral” describes the pre-send draft and Turn's semantic store, not the
recipient.

### 7.3 Branch and handoff lineage

Branching preserves the source and creates a new Node + AgentInstance. Native provider branching is used
only when the adapter can verify the resulting conversation ids; otherwise Turn creates a normal handoff
and labels it as such. The UI shows `branched from`, `continued from` or `handed off from` references without
moving either node in the ownership tree.

### 7.4 Messages, dependencies and teams

Context access, handoff and direct coordination are not synonyms. A future `AgentMessage` is a short,
typed, destination-addressed instruction or status—not a hidden transcript transfer. It has a bounded body,
sender/destination instance, purpose, creation/expiry time, idempotency key and evidence-backed
state split across three independent axes:

```text
draft_state    = live | consumed | lost
review_state   = pending | reviewed | review_required
delivery_state = not_started | queued | submitted | received | submitted_unconfirmed | failed
```

Preparation starts at `live/pending/not_started`. The explicit deliver action records `reviewed`, consumes
the reviewed draft and enters `queued`; submission advances only that delivery axis. Per-destination
ordering is FIFO. Messages are
delivered only to a verified idle adapter endpoint and never into a pending permission or question. No
message body is treated as trusted executable control. The supported flow accepts prepare/review/deliver
only from a foreground operator; an agent or conductor can propose a message only as an Attention item. The draft is client-bound and
memory-only, durable state stores hash/metadata/evidence, uncertain delivery is never retried, and provider/
terminal downstream retention is disclosed exactly as for a ContextPacket.
Its delivery capability is bound to the current daemon generation. After daemon loss, proven submission
remains evidence, an in-progress write becomes `submitted_unconfirmed`, and a pre-write message becomes
`lost/review_required/not_started`; the old operation is terminal and its hash can never reconstruct or
replay the body. Retrying requires a newly prepared body, another visible review and a new operation id.

`DependencyEdge` is a fourth non-tree relationship: one node declares that another node's typed result is a
prerequisite. A dependency is satisfied by a durable closed-schema `DependencyResult`, not by observing that
a process became idle. It contains only a result state, producer node/instance/attempt and revision ids,
bounded operation/artifact ids or content hashes, verified canonical references, and timestamped provenance/
confidence. An optional human summary uses the declared durable-text byte limit plus control stripping and
best-effort known-secret redaction. Raw PTY/output, transcript turns, file/diff bodies, environment values and
arbitrary provider payloads are forbidden. The graph rejects cycles and projects `blocked`, `ready`, `failed`
or `cancelled` evidence into each Node View and the Attention policy. It never starts, advances, interrupts
or retries a dependent Agent. A user or agent may propose the next operation, but only a foreground operator
action may execute it. ADR-049 Session activation may still start its independently persisted safe runtime
contract; becoming dependency-ready is never an activation signal. Dependencies render as references and
badges; they do not reparent nodes, form a second canvas or turn Turn into a workflow scheduler.

A Team is an explicit Session-scoped coordination object with member AgentInstances, roles and an optional
conductor/synthesiser. Its members keep normal tree positions and independent Attention subjects. A
conductor may propose delegation, messages and dependency changes only within a user-authorised policy;
it cannot grant context access, approve permissions, take checkout write authority or invoke focus. It emits
typed evidence/proposals; only the AttentionManager and governor may emit Focus under operator policy. Final
reconciliation is a visible result/Attention item with repository evidence before integration.

M13 runtime continuity introduces a `RuntimeEndpoint` record for a configured provider-runtime service or
external multiplexer. It stores a non-secret endpoint/host fingerprint, conversation binding, capabilities
and observations—never a bearer, descriptor or raw transcript. Warm attach to a verified live endpoint
launches nothing; cold resume still creates a generation-fenced attempt and requires verified conversation
continuity. Daemon recovery cannot duplicate an attempt, and a missing/mismatched endpoint becomes lost.
This contract tests reconnect to an already configured adapter endpoint; it does not implement general
Remote/SSH Session creation, which remains later scope.

Direct messaging, observational dependencies and Teams are sequenced after live context links. Their data
types are reserved now so instance and lineage schemas do not need another identity migration later.

## 8. Protocol and persistence target

The protocol additions are version-gated and derive views from daemon-owned state:

- ids: `agent_instance_id`, `runtime_attempt_id`, `context_link_id`, `context_packet_id`,
  `context_scope_id`, `quota_scope_id`, `node_view_subscription_id`,
  `agent_message_id`, `dependency_edge_id`, `team_id` and `runtime_endpoint_id`;
- navigation requests: `get_node_view`, `subscribe_node_view`, `unsubscribe_node_view` and
  `route_attention`, plus `update_surface_activity` and separate `activate_session`;
- lifecycle requests: `create_agent_instance`, `restart_agent_instance`, `branch_agent_instance`,
  `delete_agent_instance`, `get_runtime_continuity` and `attach_runtime_attempt`;
- context/operator-authority requests: `create_context_link`, `update_context_link`,
  `revoke_context_link`, `prepare_context_packet`, `deliver_context_packet`,
  `get_context_packet_delivery` and `respond_to_agent_interaction`;
- M14 resource requests: `create_resource_node`, `update_resource_node`, `delete_resource_node` and
  `set_group_membership`;
- M13 coordination requests: `prepare_agent_message`, `deliver_agent_message`, `set_dependency_edge`,
  `remove_dependency_edge`, `create_team`, `update_team` and `delete_team`;
- responses: `node_view`, `node_view_subscription`, `attention_route`, `session_activation`,
  `agent_instance`, `runtime_continuity`, `context_link`, `context_packet`,
  `context_packet_delivery`, `agent_message`, `agent_message_delivery`, `dependency_edge` and `team`;
- pushes: subscription-scoped `node_view_changed`, `runtime_attempt_changed`, `context_usage_changed`,
  `quota_scope_changed`, `context_link_changed`, `context_packet_changed`, `agent_message_changed`,
  `dependency_edge_changed`, `team_changed` and `runtime_continuity_changed`;
- `AttentionView`: tagged exact Node/AgentInstance, authenticated provisional scope or unassigned Session;
- `AttentionRoute`: surface/connection/daemon generation, attention id, the same tagged subject, optional
  verified interaction owner and NodeView or provisional-demand bootstrap revision;
- `TreeSurfaceState`: selected hierarchy key; `ViewTarget` is derived and never broadcast between surfaces.

The current v4 `HierarchyKey` has `workspace`, `session` and `process` tags. The incompatible vNext protocol
replaces the last tag with a general `node` tag whose payload includes the closed Node kind, covering Agent,
Subagent, Shell/Process, Group, Note, File, Diff and Web without forging process identity. Migration converts
every old process key losslessly; a mixed-version peer is rejected at handshake rather than guessing.

`get_node_view` is keyed by both `surface_id` and `HierarchyKey`. Its response repeats the key and a
revision so a client can discard a late response after selection changes. Large content is subscribed only
for the visible node; hierarchy snapshots remain bounded summaries. `subscribe_node_view` returns a
surface-scoped subscription id, exact subject, content kind, initial revision and negotiated byte/item
bounds. Every push repeats the subscription id, subject and monotonic revision. Reselection, replacement
connection, disconnect or explicit unsubscribe cancels it; bounded backpressure produces a typed gap and
requires resubscribe. A client drops pushes from a retired subscription or non-current subject.

Linked-context reads are deliberately absent from the administrative UI protocol. They travel only through
the `ContextBroker` data plane using the current attempt's short-lived, destination-bound capability. This
separation avoids intentionally giving a cooperative adapter the daemon control token and prevents a caller-
supplied id from broadening the grant; it cannot stop a malicious same-uid process from stealing an
administrative capability elsewhere without OS isolation.

Persist semantic/resource nodes, AgentInstances, RuntimeAttempt/runtime-endpoint metadata, safe launch/
resume specs/receipts and configuration-transition receipts, links, lineage, context-read audit, handoff/message delivery metadata, context/quota scopes
and bounded samples, dependency edges/results, Team roles/policy and surface selection. Every new table/file
category enters ADR-057's closed privacy catalogue before its migration can open. Do not persist PTY
handles, bearer capabilities, secrets, raw provider credentials or a dedicated semantic copy of context
packet/message bodies. Delivered bytes can still exist in provider/terminal retention as §7.2 states.
Provider transcript access remains adapter-owned and capability-scoped.
Exact count/byte limits and delete cascades in `docs/PRIVACY.md` cover RuntimeAttempt detail, lineage,
ContextScope, QuotaScope, RuntimeEndpoint, remote-cleanup tombstones and dependency summaries. Pruned
attempts fold into one constant-size aggregate rather than an unbounded digest-per-attempt series; reaching
a live-record/tombstone bound refuses the new record or remote artifact instead of silently pruning active
semantics or cleanup proof.

## 9. Failure behavior

- A selected node deleted during load returns to the nearest surviving SessionView without changing work.
- A semantic subagent with no content adapter shows its verified Activity Preview and capability limits;
  it does not borrow an ancestor transcript.
- A stale context sample is labelled stale; a missing sample renders unavailable rather than `0%`.
- A shared runtime service failure marks each affected attempt honestly while keeping independent nodes and
  conversations.
- An unknown delivery result is retry-fenced. Turn never repeats an ambiguous launch, bearer installation or
  context submission; it may only probe/adopt the exact preassigned launch identity and revokes an ambiguous
  bearer generation.
- A context read/handoff and revoke are linearised at final response/write authorisation: revoke-first emits
  no body/write; delivery/read-first records disclosure and cannot be recalled. No stale cached body bypasses
  that decision.
- An Attention route with no safe input owner still opens the exact Node or provisional demand view and
  explains the missing input route. It never focuses a plausible sibling.
- Viewing a stopped, lost or archived node never creates “Start pane” friction or an implicit relaunch.

## 10. Delivery sequence

1. **View target foundation.** Introduce `WorkSurface`/`NodeView`, remove click-to-zoom, integrate the current
   inspector and keep explicit Pane commands.
2. **Attention-first route.** Make every queue entry, tree badge, notification and governor-approved
   automatic Focus land on the exact Node View and verified action owner in one interaction; separate unread
   results from demands.
3. **Stable instances.** Add AgentInstance/RuntimeAttempt identity, launch receipt, migration and history;
   stop replacing semantic identity on relaunch.
4. **Runtime observability.** Populate model/account/mode/flags/host/worktree plus context and shared quota
   snapshots with provenance and freshness.
5. **Context packet v2.** Reconcile the existing rich daemon packet with one normative schema, add adapter
   transcript input, budget manifests, delivery receipts and idempotent target provisioning.
6. **Live context links and branching.** Add scoped pull, revocation, provider adapters, remote jail and
   verified native branch support.
7. **Coordination.** Add typed messages, observational dependency results and user-authorised Teams without
   a scheduler, canvas, hidden context rights or a second Attention authority.
8. **Durable runtime continuity.** Add external multiplexing or provider-runtime services only after warm
   attach, cold resume, remote failure and updater behavior have adversarial acceptance coverage.
9. **Resource nodes.** Add Group, Note, File, Diff and Web creation/persistence only after their canonical
   ownership, private-data, content-security and no-load-on-restore contract passes M14 acceptance.

Each slice must ship end to end—domain, store migration, protocol, daemon, native UI and acceptance—rather
than leaving a field that always displays `unknown` while documentation claims support.

## 11. Acceptance contract

The implementation is not complete until deterministic automated/native snapshot tests prove the local
contract and reproducible, versioned credentialed acceptance records prove the live-provider observations:

- Two semantic children sharing one ancestor PTY open different Node Views; neither click changes Layout.
- Returning to the Session restores its exact prior Layout, zoom and focused Pane.
- `Next Attention`, a tree badge, an OS notification and a governor-approved automatic Focus each reach the
  exact semantic subject and safe action control with one route, including across Sessions; a deferred route
  retains the same `attention_id`, subject and surface generation, and a retired surface never transfers the
  jump to another window.
- Provisional parent/external-worker and unassigned demands open their exact demand view without inventing a
  Node or input owner; later binding produces a new route revision.
- Selection alone never clears unread. Only `mark_node_result_read` after the exact result revision is the
  primary content of a foreground WorkSurface clears that revision, and it never acknowledges Attention.
- A turn completion remains discoverable and ordered for review even after its unread marker is cleared.
- Questions never render approval buttons; only a verified pending approval id can render allow/deny.
- Submitting a response stays pending until adapter evidence confirms resolution.
- Simultaneous children keep independent queue subjects, prompts, unread state and runtime attempts.
- Warm attach launches nothing; selection never resumes or starts an Agent and no Node View contains a
  generic “Start pane” gate. A specific lifecycle action or ADR-049 Session activation is required.
- Relaunch/resume and a verified same-conversation in-place model switch keep AgentInstance/tree identity and
  history while creating a new RuntimeAttempt epoch; fresh start creates a new Node/AgentInstance, and
  concurrent transitions are generation-fenced and idempotent.
- UI close/reopen preserves a live attempt; daemon or machine restart never duplicates an instance,
  recovered scrollback never claims liveness, old-attempt capabilities fail, and failed resume never becomes
  a fresh conversation.
- Requested/effective model, account, permissions, sandbox and flags cannot silently diverge.
- Context usage and shared account quota have distinct labels, scopes, reset/freshness and unknown states.
- At least one authenticated live adapter supplies effective model and context usage, at least one real
  provider/account supplies a quota window, and at least one provider transcript supports bounded pull and
  handoff. These may be different providers; unsupported adapters have negative fixtures instead of fake
  values.
- Telemetry thresholds alone cannot focus, reorder or resolve Attention.
- A live ContextLink binds the logical destination/attempt and ignores caller-supplied destination ids;
  tests also state that this is not same-uid process isolation without an OS sandbox.
- The supported control flow accepts create/renew/expand only from a foreground operator surface and rejects
  agent-event attempts; tests and UI also expose that an unsandboxed malicious same-uid process may steal the
  administrative capability and impersonate that surface. Every broker read is destination/attempt-bound,
  descriptor-jailed, atomically budgeted and audited. Revoke/read races are linearised before body commit,
  and ending/archive or deletion revokes authority permanently.
- A handoff packet is bounded and reviewed; its canonical bytes/hash enter the reviewed encoder unchanged.
  Native decoding is verified where supported, while PTY fallback claims deterministic submission only.
- A destination with a pending interaction rejects handoff without a partial write.
- Packet preparation creates no target/process/grant. New-target delivery exposes provisioning/launch/grant-
  install/write states and preassigns launch identity. Crash tests after every external-effect boundary prove
  that uncertain launch is only probed/adopted, uncertain grant installation is revoked and uncertain write
  is not retried; no ambiguous effect is automatically repeated or described as rolled back.
- `context_only`, `review` and `second_opinion` encode a reviewed recap-and-stop instruction and never claim
  compliance without evidence; `continue_with` can start only the separately reviewed next instruction
  behind **Send & continue**, without approving anything or inventing missing work.
- Submitted, received, read and acted states are never inferred from one another.
- Creating a branch/handoff target does not create a Pane or mutate the source Layout.
- Message delivery is ordered, bounded and retry-fenced; it cannot answer an existing prompt or imply
  receipt without evidence.
- Agent/conductor-authored message, dependency or Team changes remain proposals/Attention; only a foreground
  operator operation commits them, and no durable message body or bearer enters protocol/storage.
- Dependency cycles are rejected, idle is not mistaken for a completed result, and the bounded closed result
  schema rejects raw output/transcript/diff/file/environment/provider payloads. No dependency automatically
  starts/advances/retries work, and a Team keeps exact per-agent Attention subjects.
- RuntimeEndpoint reconnect proves mutual endpoint/conversation/instance/generation identity, launches
  nothing on warm attach and fails closed on mismatch; it does not claim general Remote/SSH Session support.
- Offline hosts, missing worktrees and unsupported provider features remain visible and never fall back to a
  different host, checkout, model, account or fresh conversation.
- Group/Note/File/Diff/Web creation, restore and deletion preserve Session ownership, never load Web on
  restore, never delete referenced user data and pass descriptor-jail/TOCTOU/hardlink/mount, inert-content,
  query/userinfo/fragment refusal, all-answer DNS validation, approved-IP socket pinning on every connection/
  redirect, private URL-content handling, origin/credential isolation, Group-reparent, privacy and export
  coverage before M14 ships.
- Privacy report/export/delete/compact tests cover instances, attempts, safe launch/configuration facts, links and broker
  audits, lineage, usage scopes/samples, packet/message metadata, dependency/Team/runtime-continuity and
  resource records; deletion revokes capabilities before removing either endpoint and reports any retained
  provider or ancestor-Shell journal copy plus remote `pending_purge` artifact it cannot yet erase. Count/
  byte bounds cover live ContextLinks, lineage, scope, endpoint and result records as well as retained history.

## 12. Product boundaries

Turn remains a supervisor, not an autonomous workflow scheduler. User-directed create, delegate, branch,
resume and context operations are in scope; the supported authenticated flow never accepts agent events as
hidden context authority, permission approval or a way to move focus outside the Attention policy. This is
not same-uid hostile-process isolation: without an active OS sandbox or UI-owned authority inaccessible to
the agent, a compromised local process may steal the administrative capability and impersonate the operator.
