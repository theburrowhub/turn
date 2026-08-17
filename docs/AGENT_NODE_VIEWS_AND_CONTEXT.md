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
    ├── Flow                         (projects one immutable FlowRun)
    ├── Team                         (member references; never duplicate rows)
    ├── WorkItem                     (canonical card; optional exact external binding)
    ├── Job ── NativeJobIteration*   (provider-native schedule/work; referenced runtimes stay separate)
    ├── optional Group*              (bounded recursive presentation; optional CheckoutScope projection)
    ├── Agent ── AgentInstance ── RuntimeAttempt*
    │   ├── Subagent ── AgentInstance ── RuntimeAttempt*
    │   └── Process / Log
    ├── Shell / Command / Tui / Service / Process / Log
    └── Note / File / Diff / WebPreview / Browser / Media
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
- `Shell`, `Command`, `Tui`, `Service`, `Process` or `Log` — runtime/tool work without an independent agent identity;
- `Note`, `File`, `Diff`, `WebPreview`, `Browser` or `Media` — typed resources when their capability ships;
- `WorkItem` or `Job` — canonical external-work projection or provider-native job/iteration identity;
- `Group`, `Team` or `Flow` — explicit organisation/run projections, never inferred process relationships.

Each kind declares a truthful `ContentCapability`: live terminal, structured activity/transcript, service,
log, file, diff, web/media, note, WorkItem/Job, group/team/Flow overview or technical process detail. A client must not render a semantic subagent
as an empty terminal merely because an ancestor owns one.

ADR-044's local ownership remains: a Shell node owns the PTY and Pane bindings, while the Agent launched
inside it is a confirmed child. An AgentInstance's current attempt may carry a verified `RuntimeBinding` to
that Shell/runtime owner. Its AgentNodeView may project the bound live terminal while keeping the Agent as
the visible semantic subject; it never claims that the Agent owns the PTY. A provider-side thread with no
local shell uses a different adapter capability and does not fabricate one.

`Group`, `Note`, `File`, `Diff`, `WebPreview`, `Browser` and `Media` are accepted resource-node kinds but are sequenced after the first
Agent Node milestone. A Group is explicit presentation inside one Session and owns no checkout, lease,
Attention policy or runtime. Notes are Turn-owned private content; File and Diff nodes refer to canonical,
checkout-confined sources and never own user files; WebPreview nodes hold a validated URL and perform no network
load merely because the tree restored. Removing any resource node forgets Turn's record, not the referenced
file, branch or site. Their creation, persistence, privacy and content-security acceptance is M14 scope.
Media stores a canonical bounded local/remote source reference and declared MIME evidence, never copied
payload by default; restore does not fetch, decode, autoplay, capture or emit network traffic. Explicit view
uses a crash-isolated decoder with the same root/host confinement and treats metadata as untrusted.

The supported flow accepts resource creation/edit only from a foreground operator operation or one exact
`submit_delegated_operation` derived from a reviewed Flow grant. The grant may pre-list an existing resource
or allow creation within its exact kind/owner/schema/author and cumulative node/byte/revision/rate/expiry
bounds; the daemon derives those fields and the compare-and-swap target rather than trusting agent payload
authority. An unauthenticated/out-of-grant agent event or payload can only propose it. A Group is a direct child of its
owning Session or one parent Group in the same Session. Groups form a bounded acyclic forest with maximum
depth 128. Every create/reparent/subtree move/removal compare-and-swaps the Session-scoped
`GroupTreeRevision`, revalidates same-Session ownership, uniqueness, depth and acyclicity after concurrent
changes, and either commits the whole subtree operation or none of it. Any Agent, Shell/Process or resource
in that Session may have at most one explicit presentation membership in a Group; changing it does not
rewrite the runtime/process parent, which remains visible as a reference in the Node View. Resource payloads
cannot reparent themselves. Removing a non-empty Group requires exactly
`refuse|promote_children|move_children_to_session`; it never cascades into runtime, context, Attention or
checkout deletion. Titles are
sanitised/bounded. Note text is bounded and stored exactly as private user content, while its projection is
escaped and inert. File/Diff references use the same descriptor-relative regular-file jail as repository
context and render unavailable rather than following a moved target outside the checkout. Markdown, SVG,
HTML and other active-looking File/Diff/Note content render inert with no script or remote loads.
Traversal uses a visited set and hard depth/node bounds. Existing duplicate/cycle/depth corruption returns
typed `group_tree_corrupt`, renders only the bounded proved prefix and permits no ordinary Group mutation;
repair is a separate exact foreground recovery operation, never an inferred reparent.

A Group may project one optional `CheckoutScopeBinding`, but its `CheckoutScopeBindingId`, the Session-owned
`CheckoutScopeId`, canonical repository/worktree identity and GroupId stay distinct.
`CheckoutScopeBindingState` is only
`bound → unbound`, terminal for that binding id; dropping the projection leaves an active CheckoutScope
unchanged, while `unbind_checkout_scope` is the separate operation that drives scope `unbinding → unbound`.
The Group gains no runtime or repository authority;
the binding supplies only default cwd/isolation for new descendants and for explicit `move_and_rehome`.
Merely moving a live Node is presentation-only. `move_and_rehome` preflights every affected stopped
descriptor, refuses live writers and never rewrites a running cwd. `CheckoutScopeState` is closed:
`provisioning → active|reconcile_required`, `active → missing|conflicted|unbinding|removing`,
`missing|conflicted → active|unbinding|reconcile_required`, `unbinding → unbound|reconcile_required`,
`removing → removed|reconcile_required`; reconciliation advances only to a freshly proved state and
`unbound|removed` are terminal for that scope id. Create/adopt/bind/unbind/remove/reconcile carry exact
Session, target/trust, repository, worktree, `creator=turn_created|adopted` and scope generations. Missing/foreign worktrees become
`missing|conflicted`, never a same-looking local fallback. Unbind or Group deletion preserves the worktree; removal is
a distinct foreground destructive operation requiring fresh dirty, unpublished, ownership and survivor
proof. Agent-per-branch Flows still allocate separate scopes; a Group only makes one visible.
A one-step catalogue create/adopt preassigns CheckoutScope, Session and optional Group/binding ids under one
composite operation id; its receipt records every worktree/Session/Group boundary, and any partial external
effect remains reconcile-required instead of duplicating or hiding a resource.

The initial WebPreview kind accepts only `https://host[:port]/path`: userinfo, query and fragment are all refused,
as are non-public IP ranges and `file:`, `data:`, `javascript:`, custom/IPC schemes. The complete stored URL,
including its path, is private content rather than safe display/log metadata; tree and audit projections show
only a sanitised origin. For every connection and redirect Turn resolves every A/AAAA answer, rejects the
whole answer set if any address is non-public, chooses an approved address and pins the socket to that exact
address while preserving TLS SNI and the HTTP `Host`; redirects repeat the process and cannot downgrade
scheme. A second resolver lookup cannot choose the connected address, and the initial implementation does
not use an ambient proxy. WebPreview runs in an isolated origin with no inherited cookies, provider/daemon
credentials, filesystem/IPC access, downloads, popups or automatic external navigation. Content loads only
for one explicit foreground inert preview and cannot navigate, submit forms or promote itself into Browser.
Restore reconstructs private content and an unloaded view only; URL changes never happen from page script.

Browser is a distinct explicitly created process-isolated Node with one dedicated cookie/storage partition
and typed `navigate|back|forward|reload|stop|open_reviewed_popup|accept_reviewed_download|clear_storage`
operations. Address/history/load/TLS/error/permission/popup/download state is revisioned, but page content,
links and script messages are hostile data and never control protocol, Attention or authority. Ambient Turn/
provider credentials, daemon origin, device/clipboard/filesystem access and popups/downloads are denied.
A popup becomes a separate Browser Node only after exact origin/consequence review and receives no opener
authority. A download is quarantined non-executable and becomes an inert File Resource only after exact size/
type/hash/confined-path review; it never auto-opens. Local HTML is descriptor-copied from one reviewed jailed
root into a synthetic origin; `file://`, live Workspace access and link/mount escapes fail. Loopback requires
a short-lived review binding scheme, all resolved IPs, port, target fingerprint/generation and expiry; every
navigation/redirect revalidates DNS and forbids rebinding, host/port change or remote-to-local fallback.
History/restore never reloads automatically, and destroy clears only the partition, not server data.

Relationship axes remain independent:

| Edge | Meaning | May affect tree placement | Grants context read | Transfers work/control |
| --- | --- | --- | --- | --- |
| `OwnershipEdge` | Workspace/Session owns a Node | root/fallback | no | no |
| `SpawnEdge` | one semantic runtime caused another | primary when verified below Group override | no | no |
| `ProcessEdge` | observed OS ancestry | primary only without Group/Spawn | no | no |
| `GroupMembership` | one explicit presentation Group, including Group-in-Group | primary operator override | no | no |
| `TeamMembership` / `FlowMembership` | role/run references | no; activatable reference only | no | only through Flow policy |
| `ContextLink` | one AgentInstance may pull bounded context from an AgentInstance or exact Note revision/policy | no | yes, within scope | no |
| `LineageEdge` | an instance continued, handed off or branched from another | no; shown as reference | no by itself | records an explicit operation |
| `DependencyEdge` / `MessageEdge` | typed result gate / delivery evidence | no | no implicit access | only a reviewed Flow policy may start work |

Every operational Node has one primary row using Group → strongest verified Spawn → strongest verified
Process → Session fallback; a Group uses its explicit parent Group or Session. Equal strongest competing
parents remain visibly unassigned instead of being tie-broken into invented ancestry. Team/Flow and
non-winning relationships are references, not aliases; semantic
counts use SpawnEdges rather than display placement. Process-derived edges retain confidence. Context and
lineage must never be inferred from process ancestry, matching directories, shared accounts or similar titles.

### 3.2 Stable agent identity and runtime attempts

An `AgentInstance` is Turn's stable identity for one operator-recognisable agent. Its `AgentInstanceId`
survives warm view attachment and survives cold resume, model switch or runtime restart only when the
adapter verifies continuity of the same provider conversation. Migration always mints a distinct
`AgentInstanceId`; an old Node-shaped identifier survives only as a resolution tombstone and is never emitted
as the new instance id.

Every agentic Node owns exactly one AgentInstance; non-agent nodes own none. An AgentInstance belongs to one
Node for its lifetime. `AttemptOwner` is a tagged union: an AgentInstance owns agent/subagent attempts, while
a runtime-capable non-agent Node (`Shell|Command|Tui|Service|Process|Log`) owns its own attempts directly;
exactly one variant is present. Every owner has ordered zero-or-more attempts, an optional
`active_attempt_id` naming at most one non-terminal attempt and an optional `latest_attempt_id` that may be
terminal. Attention
and visible selection carry `node_id + agent_instance_id` when agentic and the daemon rejects a mismatched
pair; attempt-scoped interactions additionally carry the current `runtime_attempt_id` and generation.
ContextLink destinations and handoff destinations name AgentInstances and revalidate the same join. This one-to-one join
is daemon-derived and never reconstructed from provider ids, cwd or titles. A create, branch or new-target
handoff commits its Node + AgentInstance pair together in one store transaction; neither half can exist
alone. External launch remains the separate visible saga described below.

A `RuntimeAttempt` is one concrete runtime-configuration epoch. For agent owners it starts with a launch/resume or with a
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

For a runtime-capable non-agent Node it starts with create/adopt/restart/recycle and never fabricates an
AgentInstance or provider conversation. One `(ExecutionTarget, backend, handle, generation)`, PTY generation
or surfaced OS process identity belongs to at most one current AttemptOwner; conflicts remain unresolved
rather than being attached twice. A durable Pane belongs to one Session Layout and binds at most one runtime
owner; a temporary Pane belongs to one Surface. One client connection generation owns each live Surface,
and neither a Surface nor an input lease transfers implicitly.

The identifiers must not be substituted for one another:

| Identity | Lifetime and authority |
| --- | --- |
| `NodeId` | semantic tree subject and view target |
| `AgentInstanceId` | stable agent history across attempts |
| `RuntimeAttemptId` | one launch/resume/configuration epoch and its evidence |
| `ConversationKey` | canonical provider/profile/target/namespace plus normalised vendor conversation id; optional and capability-scoped |
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
- **Adopt runtime:** bind a proved already-running external runtime as one new attempt; later view attachments
  reuse it and never create another attempt.
- **Cold resume:** create a new attempt that resumes a verified provider conversation.
- **Fresh start:** create a new Node + AgentInstance and provider conversation. “Replace” may archive the
  old node and select the new one in the same presentation position, but never reuses its ids.
- **Restart runtime:** a generic Tool/Process creates a fresh attempt under its existing Node; an Agent keeps
  its instance only with verified conversation continuity and otherwise creates a fresh Node/instance with
  lineage. Previous attempts remain history.
- **Switch model in place:** when the adapter proves the same conversation/process and effective new model,
  atomically end the old epoch, create a configuration-receipt attempt and rotate attempt capabilities. It
  neither launches nor resumes anything; insufficient proof leaves the observed model uncertain.
- **Branch:** create a new Node + AgentInstance with an explicit lineage edge and an independently resumable
  conversation when the provider supports it.
- **Handoff/continue with:** target an existing instance or provision a new one through an idempotent,
  fenced saga, attach one bounded context packet and record lineage. It does not create a Pane.
- **Interrupt/terminate/kill:** respectively send a non-terminal interrupt, request graceful exit and apply
  the declared forceful backend action; receipts preserve the distinct outcome.
- **Recycle:** replace runtime infrastructure while preserving Node/instance/conversation only when durable
  attach/resume proves continuity; otherwise refuse and offer Fresh start.
- **Destroy:** fence/remove the semantic Node and revoke its capabilities with a durable tombstone; cleanup
  of surviving processes, worktrees, branches or artifacts is a separate disposition.

The unattended daemon never launches work merely because it restored metadata. A connected client may
start only a runtime whose persisted auto-start contract is explicit and whose checkout, account, host,
command and safety prerequisites still resolve. Anything ambiguous becomes an actionable Attention entry,
not a guessed launch. A remote target that is offline fails closed; Turn does not execute locally in the
same-looking cwd.

`activate_session` is the explicit ADR-049 intent for that connected-client path. A single Session-row
gesture may select then activate, but selecting an Agent/child, applying an Attention route or restoring a
tree never invokes it. An Attention route may cross into a stopped Session to show the exact demand and
recovery state; it does not start or resume anything. Activation restores Layout/attaches live attempts and
materialises the exact bounded eligible descriptor set in one preflighted plan—or exactly one configured
default Shell when that set is empty. The plan fixes Session/policy revisions and descriptor target,
profile, cwd, isolation, command and authority generations; any changed/ambiguous/unsafe element rejects the
whole materialisation before spawn with one consolidated recovery result. It cannot choose an undeclared
descriptor, resume an unverified conversation or require a follow-up “Start pane”.

`WorkspaceOnboarding` is the single resumable catalogue path for
`create_directory|open_directory|clone_repository|adopt_ssh_target`. It freezes operation id, intended
Workspace, ExecutionTarget/generation, canonical path, repository/remote identity and authentication
reference before effect. Partial directory/fetch/checkout/trust/cleanup outcomes remain exact receipts;
resume reconciles by operation and remote/repository identity instead of cloning twice. SSH identity and path
stay pinned, with no same-name local fallback. Publication is never onboarding: `publish_repository` is a
separate local-foreground consequence review with destination, visibility, branch/upstream and credential
reference. Every writer uses an isolated checkout, leaving the operator's primary `main` checkout free.
Each `WorkspaceOnboardingId` carries closed `WorkspaceOnboardingState=prepared|running(phase)|cancel_requested(last_proved_phase)|
reconcile_required(last_proved_phase,possible_effect)|completed|cancelled|failed(reason,residuals)` and phase
`preflight|path_probe|directory|target_adoption|remote_fetch|checkout|workspace_commit|cleanup`. Each intent
freezes one finite ordered phase plan. A possible effect always reconciles by exact receipt/identity before
advancing and is never replayed; cancellation fences new effects and classifies every cleanup/residual.
`completed|cancelled|failed` are terminal for that id.

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
| WorkItem | `WorkItemNodeView`: canonical fields, exact external binding/sync/conflict evidence and related runtime references |
| Job | `JobNodeView`: canonical NativeJob identity, schedule/state, ordered iterations and exact runtime references |
| Agent or Subagent | `AgentNodeView`: unique activity/conversation plus runtime and context controls |
| Shell or Process | `ProcessNodeView`: its live terminal when owned, otherwise technical detail |
| Note/File/Diff/WebPreview/Browser | the matching typed resource or isolated browser view |

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
   a negotiated legacy local adapter may use verified PTY input, while remote permission response has only
   the exact single-use typed grant path. In every case the operator, never Turn, chooses the response.
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

### 5.4 Background and headless delivery

Background delivery projects the canonical queue through `NotificationEndpointId`; it never creates a
second Attention authority. A foreground-paired `DeliveryGrantId` binds one endpoint public key/token
reference, device/profile, allowed Workspaces/ExecutionTargets, event classes, privacy detail, rate/batch
bounds, generation and expiry. `DeliveryGrantState` is closed: `proposed → active|invalid|revoked`,
`active → expired|invalid|revoked`; terminal states never reactivate. Secret material stays in the keystore/
agent and never enters reads, exports, logs or diagnostics.

Each `NotificationDeliveryId` follows this closed `NotificationDeliveryState` machine:

```text
eligible -> held_present | queued | superseded | expired
held_present -> queued | superseded | expired
queued -> submitted | superseded | expired
submitted -> accepted | failed_retryable | failed_terminal | superseded | expired
failed_retryable -> queued | failed_terminal | superseded | expired
accepted | failed_terminal | superseded | expired -> terminal
```

The retry edge retains the same delivery id, increments a bounded attempt counter and applies bounded jitter;
exhaustion becomes `failed_terminal`. Gateway acceptance proves neither device delivery, read nor Attention
resolution. `CollapseFamilyKey=(NotificationEndpointId,complete AttentionSubject identity,demand_kind)` is
stable across revisions, while `CollapseKey=(CollapseFamilyKey,subject_revision)` identifies one delivery;
only a newer current revision in the same family may supersede an older one. Outbox insertion and flush both
revalidate grant, authoritative queue revision, resolution and presence. Payloads are encrypted and minimal;
failure changes no Attention, unread or runtime state. A deep link carries only opaque identity and always
resynchronises/revalidates the exact route before display or action.

Live status uses `LiveStreamKey=(NotificationEndpointId,AttentionSubject identity,attempt_generation)` and
monotonic event revision. Start/update/end are collapse-aware; end/tombstone fences every late tick. Presence
may hold a notification but never pauses the authoritative stream, and release sends only still-current work.
`NotificationHostMode=owner_local|loopback_observer` accepts only authenticated owner-local or loopback
observation input and makes outbound HTTPS delivery. It ignores public bind host/port and exposes zero public
inbound listeners. This host is distinct from `RemoteOperatorSurface`; a headless client consumes the same
revisioned hierarchy/Attention objects and gains no notification, input or control authority from delivery.

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

### 6.4 Target resource inventory

`ResourceInventoryObservation` extends the target RuntimeInventory snapshot family rather than creating a
second owner graph. `ResourceScopeKey=(ExecutionTargetId,target_generation)` and
`RuntimeResourceRowKey=(ExecutionTargetId,target_generation,backend_handle,handle_generation)` are canonical.
The host row carries physical memory total/available/used, swap total/free, measured pressure, accounting
method, observed time and exact `complete|partial|gapped|unavailable|unsupported|stale` coverage. It also
distinguishes `measured_nonempty|measured_empty|unmeasured`; absent or failed facts never become zero.

Each process row uses reuse-safe `(target_boot_id,pid,process_start_time)`, bounded parent edges, own RSS and
deduplicated descendant RSS. Proved attribution names exact RuntimeAttempt, Node and Session with ownership
`owned_current|owned_closed_session|unmatched_survivor|ambiguous`. A live process retained by a closed Session
keeps that attribution. Cycles, inaccessible processes, shared endpoints and overlapping trees become
partial/shared buckets rather than double-counting or guessed splits. Every Node/Session/target aggregate
names numerator, denominator, coverage and revision and remains target-bound.

Observation never acts. `terminate_resource_owner` is a local-foreground consequence-labelled operation that
re-probes target/trust generation, backend handle generation, process start identity and expected observation
before using the exact RuntimeInventory termination path. PID/name-only or host-wide kills and remote-to-
local fallback are invalid; stale remote responses affect no sibling.

### 6.5 Display names and proposals

`DisplayNameFact` is local metadata for one Node/Group and source revision. Its source is exactly
`declared|structured_task|provider_observed|generated|operator_alias|fallback`, with confidence, observed time
and bounded sanitised label. `NameMode=follow_source|pinned`; operator edit or `apply_name_proposal` pins until
explicit unpin. Reconnect, provider title and generated output cannot overwrite a pin. Local rename sends no
provider command or terminal input, and `conversation_rename` stays an independent capability/operation.
Resolution precedence is exact: pinned `operator_alias` first; in `follow_source`, newest current fact at
`declared > structured_task > provider_observed > generated > fallback`, with source revision then observed
time resolving facts inside one tier. Unpin excludes the retained alias fact until it is explicitly pinned
again; equal unresolved facts cannot use Node id as a semantic tie-break.

`NameProposalId` binds bounded captured source bytes/hash, target scope and Node/Group revision, generator
identity/model, redaction policy and expiry. Generation is on-demand unless a reviewed local policy bounds it;
remote output cannot reach an undeclared generator. Controls, bidi/invisible injection, paths, secrets and
multiline labels are invalid. A stale proposal changes nothing, and Group proposals use bounded member
summaries rather than concatenated transcripts.
Captured proposal bytes remain memory-only; durable state stores only NameProposalId, bounded metadata,
content hash, expiry and accepted/refused receipt.

## 7. Context between agents

Turn supports two complementary channels. Neither is implied by the tree.

### 7.1 Live ContextLink: scoped pull

A `ContextLink` is a durable, revocable grant from a tagged source—an AgentInstance or an exact Note Resource
Node—to a destination AgentInstance. A Note source defaults to a pinned content revision; an explicit
`follow_reviewed_revisions` mode fixes the permitted author/grant set, schema and cumulative revision/byte/
token budget, and audits the exact revision returned on every pull. It cannot follow another resource id,
reset budget on edit or expose File/Diff/WebPreview/Browser/Media content implicitly. Only
an explicit foreground operator action on the authenticated control channel can issue, expand or renew root
context authority. That action may itself create the link or issue an ADR-061 `DelegationGrant` whose
immutable Flow revision authorises an exact current agent attempt to exercise bounded link/packet operations.
The agent never authorises or widens the scope; a hook payload, transcript or repository file may only
propose. The default grant
is directional; a bidirectional relationship is two grants. Initial scope is one Workspace, including
separate Sessions/worktrees; cross-Workspace links are refused because they cross an operator's project
boundary. Every grant has a purpose, closed scopes, cumulative request/byte/token limits and a required
expiry; “until either Session ends” is the longest default, not an unbounded capability.
Foreground issue/update/revoke and delegated exercise carry an operation id, issuer/grant provenance and are
idempotent across a lost response; lifecycle, expiry and endpoint-delete revocations are internal. Each
source and destination counts the live link against
`records.active_context_links_per_agent`; reaching the bound refuses creation rather than merging grants or
silently revoking an existing one.

“Only the foreground operator issues root authority” is an invariant of Turn's authenticated, supported
control flow; exercising its exact delegated capability is not new authorisation. This is not a claim
of hostile same-uid process isolation. A compromised local agent running as the operator could steal the
daemon's administrative capability or impersonate a UI unless a per-agent OS sandbox or a UI-owned authority
that the agent cannot access is active. The UI, threat model and acceptance tests state that limitation; the
daemon still rejects agent-event, hook, transcript and repository-file attempts without the separate exact
delegated-control capability.

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
second-opinion, delegation and branch intents. Live ContextLinks are same-Workspace. A packet may target a
different Session/worktree directly; crossing a Workspace uses the explicit portable export/import boundary
in the master contract: package-local identities are reminted, runtime/authority ids are stripped, imported
content stays inert, and a fresh destination review/adoption is required before delivery. No link or grant
crosses with it. A FlowRun never crosses either: its attempts/grants/receipts are constitutive evidence. An
optional imported `PortableRunReport` is only a bounded redacted summary/content-hash artifact with untrusted
provenance; it cannot decode as a FlowRun, satisfy a dependency, emit Attention or resume/retry/reconcile.
Re-execution adopts a reminted FlowDefinition and creates a fresh preflight/FlowRun. The source may target an existing compatible instance or describe a new AgentInstance
without changing Layout.

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
is a cross-agent disclosure boundary, unless the immutable FlowRun already contains an operator-reviewed
source/destination/transform/redaction/budget policy and a current DelegationGrant exercise stays exactly
inside it. Preparation creates only an expiring draft and optional target launch
spec: no node, process or grant exists yet. The review is rendered directly in the Node View rather than a
modal maze and shows the exact canonical sanitised body, known-secret redaction result, trusted transport
template/version, grant manifest and retention warning. `deliver_context_packet` carries only an operation
id plus the opaque draft capability, so neither body nor envelope can be replaced in flight. The canonical
body hash is passed unchanged into the reviewed transport encoder. A native adapter that exposes decoding
can prove the decoded body matches; PTY fallback proves only the deterministic submitted envelope/bytes, not
what the downstream program decoded.
For a preauthorised Flow, `PrepareContextPacket` and `DeliverContextPacket` are distinct closed delegated
variants. Delivery supplies only prepared-packet id, preparation receipt and content hash; the daemon proves
the same FlowRun/grant/agent attempt created it, retains the sealed single-use capability server-side and
charges its cumulative disclosure/delivery budget exactly once. The agent never receives a reusable bearer
and cannot replace the body, destination or policy revision between the two operations.

Control progress is one closed machine, separate from body authority and semantic evidence:

```text
PacketAuthority = AdHocDraft(body=live|consumed|lost,
                             review=pending|reviewed|review_required)
                | FlowRecipe(policy_revision, recipe_hash,
                             body=reassemblable|live|consumed|lost,
                             review=preauthorised|review_required)
DeliveryState = draft | reviewed |
                delivery_started(phase) |
                launch_unconfirmed | grant_install_unconfirmed |
                submitted_unconfirmed | finished |
                failed(reason) | draft_lost
phase = provisioning | launching | grant_pending | submitting | awaiting_evidence
reason = expired | refused | target_incompatible | launch_failed | grant_failed |
         write_definitely_failed | policy_invalid | operator_cancelled
evidence = { submitted?: EvidenceFact, received?: EvidenceFact,
             read?: EvidenceFact, acted?: EvidenceFact }
```

| From | May transition to |
| --- | --- |
| `draft` | `reviewed`, `draft_lost`, `failed(expired\|refused\|policy_invalid\|operator_cancelled)` |
| `reviewed` | `delivery_started(provisioning\|grant_pending\|submitting)`, `draft_lost`, `failed(expired\|refused\|target_incompatible\|policy_invalid\|operator_cancelled)` |
| `delivery_started(provisioning)` | `delivery_started(launching\|grant_pending\|submitting)`, `failed(target_incompatible\|launch_failed\|operator_cancelled)` |
| `delivery_started(launching)` | `delivery_started(grant_pending\|submitting)`, `launch_unconfirmed`, `failed(launch_failed\|operator_cancelled)` |
| `delivery_started(grant_pending)` | `delivery_started(submitting)`, `grant_install_unconfirmed`, `failed(grant_failed\|operator_cancelled)` |
| `delivery_started(submitting)` | `delivery_started(awaiting_evidence)`, `submitted_unconfirmed`, `failed(write_definitely_failed)` |
| `delivery_started(awaiting_evidence)` | `finished`, `submitted_unconfirmed` |
| `launch_unconfirmed` | `delivery_started(launching\|grant_pending\|submitting)` after explicit exact reconciliation with the live body, or `draft_lost\|failed(launch_failed\|operator_cancelled)` |
| `submitted_unconfirmed` | `finished` only from independently correlated submission/receipt evidence |
| terminal `grant_install_unconfirmed\|finished\|failed\|draft_lost` | none for the same operation id |

Cancellation after an external-effect intent can select `operator_cancelled` only after that effect is
proved not to have started; otherwise the corresponding unconfirmed state is mandatory. Body-authority
transitions are closed too. Ad-hoc preparation is `live/pending + draft`; review makes it
`live/reviewed`. Flow preparation is `reassemblable/preauthorised + draft|reviewed`; accepting delivery
materialises `live/preauthorised` bytes. Every pre-write phase requires a live reviewed/preauthorised body.
Committing the write intent atomically changes `live → consumed`; only consumed may reach awaiting-evidence,
submitted-unconfirmed or finished. A terminal pre-write refusal/expiry/cancellation/draft loss or uncertain
revoked grant discards bytes as `lost/review_required`. `launch_unconfirmed` retains live bytes only in the
same daemon generation; after loss it may only reconcile the process and then become draft-lost/failed.
`failed(write_definitely_failed)` and submitted states retain consumed; `failed(policy_invalid)` requires Flow
`lost/review_required`. Evidence is empty through `delivery_started(submitting)` except for effect intents,
and may accrue only in
`awaiting_evidence|submitted_unconfirmed|finished`. `finished` requires proved submission but does not imply
receipt/read/action. Every evidence fact has its own source, revision and timestamp; no fact implies another.
Decoder and store migration reject any state/phase/reason/evidence combination or transition not listed.

An ad-hoc draft/capability is memory-only and bound to its preparing surface connection and daemon generation.
Disconnect before `deliver_context_packet` is accepted discards it. Once accepted, the current daemon may
retain the body only in the in-flight saga's bounded memory; durable state stores its hash/manifest/phases,
never reconstructible bytes. A daemon-generation change therefore cannot resume delivery from a hash. It
reconciles already attempted external effects, then: proved submission becomes `finished`; an
in-progress/ambiguous write becomes `submitted_unconfirmed`; otherwise any prepared/installed grant is
revoked and the delivery becomes `draft_lost` with ad-hoc `lost/review_required`. The operator must prepare and review a new
ad-hoc packet, with a new operation id, before the already provisioned compatible target can receive
anything. A definitely-not-started Flow packet may instead be reassembled from its immutable still-current
policy and exercised under a new operation id; possible submission always remains fenced and cannot replay.

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

Durable metadata records operation/packet ids, source/destination, target
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

Context access, handoff and direct coordination are not synonyms. The accepted `AgentMessage` is a short,
typed, destination-addressed instruction or status—not a hidden transcript transfer. It has a bounded body,
sender/destination instance, purpose, creation/expiry time, idempotency key and evidence-backed state. Its
state is this closed product:

```text
BodyAuthority = AdHoc(body=live|consumed|lost,
                      review=pending|reviewed|review_required)
              | FlowRecipe(policy_revision,
                           body=reassemblable|consumed|lost,
                           review=preauthorised|review_required)
Transport     = prepared | queued | submitted | submitted_unconfirmed | refused | failed | expired
Evidence      = { received?: EvidenceFact, read?: EvidenceFact, acted?: EvidenceFact }
```

| BodyAuthority | Legal transport/evidence |
| --- | --- |
| ad-hoc `live/pending` | `prepared`, empty evidence |
| ad-hoc `live/reviewed` | `prepared\|refused\|failed\|expired`, empty evidence |
| ad-hoc `consumed/reviewed` | `queued\|submitted\|submitted_unconfirmed\|refused\|failed(queue_body_lost)\|expired`; evidence empty unless submitted/unconfirmed |
| ad-hoc `lost/review_required` | only `failed(body_lost)`, empty evidence |
| Flow `reassemblable/preauthorised` | `prepared\|refused\|failed\|expired`, empty evidence |
| Flow `consumed/preauthorised` | `queued\|submitted\|submitted_unconfirmed\|refused\|failed\|expired`; evidence empty unless submitted/unconfirmed |
| Flow `lost/review_required` | only `failed(policy_invalid\|policy_reassembly_required)`, empty evidence |

Transport transitions are only `prepared → queued|refused|failed|expired`, `queued →
submitted|submitted_unconfirmed|refused|failed|expired` and independently proved
`submitted_unconfirmed → submitted`; all other transport terminals are immutable. Evidence is permitted only
after submitted/unconfirmed and its three optional facts are independently monotonic. Decoder and migration
reject every unlisted combination or transition.

Ad-hoc preparation starts `live/pending/prepared`; reviewed Flow content starts
`reassemblable/preauthorised/prepared`. Queue acceptance consumes the body. Per-destination ordering is FIFO
with explicit count/byte capacity and TTL; overflow refuses visibly. Messages are delivered only to a
verified idle structured adapter endpoint and never into a pending permission, question or human draft.
Generic PTY injection is not an AgentMessage transport. No
message body is treated as trusted executable control. A foreground operator may prepare/review/deliver
directly. A conductor may do so without another prompt only through the exact destination, body/purpose
bounds and expiry of a current `DelegationGrant` in an immutable `FlowRun`; otherwise it can only propose a
message as an Attention item. Delegated preparation and delivery are separate closed variants. Delivery
contains only prepared-message id, preparation receipt and content hash; the daemon verifies that the same
FlowRun/grant/agent attempt created it, retains the sealed one-use capability server-side and charges its
budget once. The agent never receives a reusable bearer or replaces body/destination in the delivery call.
The draft is client-bound and memory-only outside an already reviewed Flow;
durable state stores hash/metadata/evidence, uncertain delivery is never retried, and provider/
downstream retention is disclosed exactly as for a ContextPacket.
Its delivery capability is bound to the current daemon generation. After daemon loss, proven submission
remains evidence, an in-progress write becomes `submitted_unconfirmed`, and a pre-write ad-hoc message
becomes `lost/review_required/failed(body_lost)`. An accepted queued ad-hoc body whose bytes die before any
possible write preserves `consumed/reviewed` and becomes `failed(queue_body_lost)`. A queued Flow operation
whose assembled bytes die becomes `lost/review_required/failed(policy_reassembly_required)`; an invalidated
policy becomes `failed(policy_invalid)`. The old operation is terminal and
its hash can never reconstruct or replay the body. Independently correlated late evidence may refine
`submitted_unconfirmed → submitted` without another write. Retrying requires a newly prepared body, another
visible review or a deterministic still-valid Flow reassembly, always as a new message and operation id. A
destination disconnect before possible write leaves the exact state `queued` with separate
`dispatch_ineligible(disconnected)` only while body/authority stay current; generation mismatch reaches the
declared refused/failed result. `suspended` is not an AgentMessage state.

`DependencyEdge` is a fourth non-tree relationship: one node declares that another node's typed result is a
prerequisite. A dependency is satisfied by a durable closed-schema `DependencyResult`, not by observing that
a process became idle. It contains only a result state, producer node/instance/attempt and revision ids,
bounded operation/artifact ids or content hashes, verified canonical references, and timestamped provenance/
confidence. An optional human summary uses the declared durable-text byte limit plus control stripping and
best-effort known-secret redaction. Raw PTY/output, transcript turns, file/diff bodies, environment values and
arbitrary provider payloads are forbidden. The graph rejects cycles and projects `blocked`, `ready`, `failed`
or `cancelled` evidence into each Node View and the Attention policy. Outside a `FlowRun`, it never starts,
advances, interrupts or retries a dependent Agent. Inside a FlowRun, only the immutable start policy and
resource/authority bounds reviewed before launch may consume a matching current result automatically; an
idle process, guessed completion or later agent proposal never does. ADR-049 Session activation may still
start its independently persisted safe runtime contract. Dependencies render as references and badges; they
do not reparent nodes or form a second canvas.

A Team is an explicit Session-scoped coordination object with member AgentInstances, roles and an optional
conductor/synthesiser. An instance may belong to multiple Teams. Members keep their one primary tree row and
independent Attention subjects; the Team View contains activatable references and never duplicate rows. A
conductor may execute delegation, messages and dependency changes only within a current typed
`DelegationGrant`; outside it those are proposals. It cannot expand the grant, approve permissions, occupy
the primary checkout, take undeclared context/write authority or invoke focus. It emits typed operations and
evidence; only the AttentionManager and governor may emit Focus under operator policy. Final reconciliation
is a visible result/Attention item with repository evidence before integration.

A required Flow step failure with `fail_run` enters `failing` and first fences starts and grants. It cannot
commit terminal failed while another step/effect is active or unclassified. Every active runtime follows its immutable
`leave_running|interrupt_then_terminate|terminate` failure disposition; every not-started step receives an
exact skip/cancel receipt. `leave_running` creates a detached-survivor receipt and removes that attempt from
scheduling without hiding or terminating it. Only after every step/effect is terminal, skipped/cancelled or
detached with a definite receipt may the FlowRun become failed. Uncertainty remains reconcile-required, and
specifically `reconcile_required(last_proved=failing,desired_terminal=failed)`; later survivor evidence cannot
mutate the immutable failed result.

M16 runtime continuity introduces a `RuntimeEndpoint` record for a configured provider-runtime service or
external multiplexer. It stores a non-secret endpoint/host fingerprint, capabilities and observations—never
a bearer, descriptor or raw transcript. A separate `RuntimeEndpointBinding` joins one endpoint generation,
provider/account/host scope, AgentInstance/RuntimeAttempt and verified conversation. Semantic ownership uses
`ConversationKey=(provider_id, AccountProfileId, ExecutionTargetId, provider_namespace, normalized_provider_conversation_id)`
across every endpoint record/generation; endpoint generation
fences transport and is not part of that semantic uniqueness key. Across all endpoints one ConversationKey
has at most one current AgentInstance owner, and one instance has at most one current binding.

Binding state is closed as `proposed|current|refused|stale|unbound|retired`. Legal transitions are
`proposed → current|refused`, `current → stale|unbound|retired`, `stale → current|unbound|retired`, `unbound →
proposed|retired`, and none from refused/retired for the same binding id. A duplicate claim is rejected before
authority and cannot create a second current owner. Missing/mismatched endpoint evidence changes BindingState/connectivity only; `Lifecycle::Lost` needs a
separate bounded RuntimeBackend absence proof. Transcript cursors, input, context grants, Attention and
identity stay isolated per binding and every operation names binding id/generation plus ConversationKey hash.
Warm attach enumerates and verifies the exact binding and launches nothing; cold resume still creates a
generation-fenced attempt and requires verified conversation continuity. Daemon or endpoint recovery cannot
duplicate/merge attempts. A service failure cannot block unrelated instances; fallback is an explicit per-
instance new attempt rather than an automatic shared restart.
This contract tests reconnect to an already configured adapter endpoint; general Remote/SSH Session creation
and its Runtime/File/Repository backend security are the wider M16 scope.

External work identity is exactly
`WorkItemKey=(source_id, source_profile_id, project_namespace, external_item_id)`. One key has at most one
canonical WorkItem Node and one Node has zero-or-one current binding plus immutable rebinding lineage; title,
URL, search result and page position are never identity. `WorkItemSource` declares profile-isolated field/
state/assignee mappings, authority, predicates/sorts, coverage/cursor/cache/rate bounds and supported write
operations. Partial, gapped or rate-limited observations never prove absence, and an ambiguous external
mutation reconciles by WorkItemKey plus source revision instead of replaying. WorkItem state, metadata and
external sync remain separate from runtime, Flow, dependency and Attention authority.

The adapter capability vocabulary is exactly 22 independently evidenced facts:
`launch|resume|branch|stop|structured_status|questions|permissions|subagents|transcript|context_usage|
provider_quota|model_switch|messaging|context_transfer|shared_identity|durable_attach|delegated_control|
native_jobs|conversation_inventory|title_read|conversation_rename|model_gateway`. Generic `rename` is not a
capability. Each fact reports `supported|unsupported|degraded|unknown`, mechanism, limits, freshness and
expiry under exact adapter/CLI/provider/profile/target/endpoint/attempt scope. Claude Code, Codex, Gemini,
OpenCode, GitHub Copilot and Grok each have a dedicated adapter and
must run the complete `supported|unsupported|degraded|unknown` status matrix plus stale/version-bound
freshness fixtures; executable-name inference or
the generic terminal adapter cannot stand in for one while dedicated support is claimed. Kimi and MiniMax
are first-class AccountProfile-scoped quota/activity connectors only unless their own launch adapter is
separately advertised; that connector grants no launch, transcript, conversation or control authority.
vNext negotiation exposes only the adapter capability schema version and canonical registry hash. The frozen
`docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv` source-capability ledger remains build/release authority and has no
read/import/mutation wire shape.
`ConversationInventory` queries one exact provider/AccountProfile/ExecutionTarget/provider namespace and
returns bounded private pages keyed by ConversationKey with timestamps, native status, model/mode hints,
ownership/resumability evidence, source revision, coverage/freshness and optional title—never ambient
transcript bodies. It declares predicates, normalisation, provider-side versus complete-cache search and
cursor/page/scan/cache/rate bounds. Partial/gapped/rate-limited pages cannot prove absence. Exact-key proof
may bind; title/text similarity is advisory only. Adopt creates one stopped Node/AgentInstance and binding
without launch. Resume is a separate preflighted operation that creates an attempt only after resumability
and global ownership revalidation. Title read never implies rename; `conversation_rename` requires exact
provider revision and a receipt with requested/effective title, and uncertain/unsupported mutation may create
only an explicit local alias.

Provider-native work uses
`NativeJobKey=(provider_id, AccountProfileId, ExecutionTargetId, provider_namespace, provider_job_id)` and one
current Job Node per key. Ordered iterations carry stable ordinal/native id, scheduled/started/finished time,
result/error and optional exact AgentInstance/RuntimeAttempt link. The total normalised state is
`scheduled|running|paused|completed|failed|cancelled|unknown`. List/create/update/pause/resume/run-now/cancel-
iteration/delete are independently advertised; mutations carry job revision and profile/target generation,
and an ambiguous effect reconciles by NativeJobKey before retry. Dismissing Attention, hiding/deleting the
projection or ending a Session never cancels provider work. Export/import carries only inert configuration,
never provider job identity, activation or authority.

`ModelEndpointProfileId` names one non-secret, revisioned `ModelEndpointProfile` scoped to one ExecutionTarget. It records a
canonical HTTPS origin, TLS/pin policy, supported wire protocols, bounded untrusted model catalogue,
provider/AccountProfile eligibility, health/freshness and only
`CredentialReferenceKind=environment|os_keystore|target_host_agent|external_broker`. Raw keys are write-only
at the secret broker and never appear in protocol reads, argv, durable
environment, logs, diagnostics or exports. `ModelEndpointProfileState` is closed as
`draft → validating|retired|deleted`, `validating → active|invalid|retired`,
`active → validating|degraded|retired`, `degraded → validating|active|retired`,
`invalid → validating|retired|deleted`, `retired → validating|deleted`; deleted is terminal with a tombstone.
Create/update/validate/set-default/retire/delete are separate revision-fenced foreground operations.
Validation bounds redirects, time, bytes and models and rejects non-HTTPS/userinfo, rebinding and private/
loopback/metadata destinations unless target policy explicitly adopted the exact origin.

Launch preflight intersects the adapter's `model_gateway` fact, endpoint protocol, AccountProfile, target,
requested model and live credential reference. LaunchSpec freezes requested profile/model and LaunchReceipt
records effective endpoint revision, wire route/model and redacted credential-reference kind. Default/profile
changes affect only future attempts; partial model discovery never proves absence, and untrusted model ids
cannot inject flags or environment. TLS, health, endpoint or catalogue failure never silently falls back to a
different provider, model, account, endpoint or local/remote route.
`switch_agent_model` requires exact current attempt/profile generations plus `model_switch` and
`model_gateway` facts as applicable. Same-conversation proof atomically closes the old configuration epoch and
creates one new RuntimeAttempt receipt. A refused, failed or uncertain switch leaves the prior attempt and
input authority intact; inability to prove continuity offers explicit Branch/new instance, never silent
restart or fallback.

Direct messaging, Flow-aware dependencies and Teams are sequenced after live context links. Their data
types are reserved now so instance and lineage schemas do not need another identity migration later.

## 8. Protocol and persistence target

The protocol additions are version-gated and derive views from daemon-owned state:

- ids: `agent_instance_id`, `runtime_attempt_id`, `context_link_id`, `context_packet_id`,
  `context_scope_id`, `quota_scope_id`, `node_view_subscription_id`,
  `agent_message_id`, `dependency_edge_id`, `team_id`, `flow_definition_id`, `flow_run_id`,
  `delegation_grant_id`, `runtime_endpoint_id`, `runtime_endpoint_binding_id`,
  `execution_target_id`, `account_profile_id`, `model_endpoint_profile_id`, `work_item_id`, `progress_id`,
  `checkout_scope_id`, `checkout_scope_binding_id`, `workspace_onboarding_id`,
  `resource_inventory_subscription_id`, `name_proposal_id`, `notification_endpoint_id`, `delivery_grant_id`,
  `notification_delivery_id`, `live_notification_subscription_id`, `remote_permission_response_grant_id`,
  `input_lease_id` and package-only `portable_run_report_id`;
- navigation requests: `get_node_view`, `subscribe_node_view`, `unsubscribe_node_view` and
  `route_attention`, plus `update_surface_activity` and separate `activate_session`;
- lifecycle requests: `create_agent_instance`, `restart_agent_instance`, `branch_agent_instance`,
  `switch_agent_model`, `delete_agent_instance`, ExecutionTarget create/adopt/trust/bind/retire/delete,
  `get_runtime_continuity` and `attach_runtime_attempt`;
- onboarding requests: begin/resume/get/cancel/reconcile `WorkspaceOnboarding` with a closed intent, plus the
  separate local-foreground `publish_repository`;
- context/operator-authority requests: `create_context_link`, `update_context_link`,
  `revoke_context_link`, `prepare_context_packet`, `deliver_context_packet`,
  `get_context_packet_delivery` and `respond_to_agent_interaction`;
- M14 resource requests: `create_resource_node`, `update_resource_node`, `delete_resource_node` and
  `set_group_membership`, `move_group_subtree`, local `repair_group_tree`, CheckoutScope create/adopt/bind-projection/unbind-projection/
  unbind/remove/reconcile and `move_and_rehome`, plus closed-CAS `update_work_item_metadata` and delegated
  `PublishProgress`;
- M13 coordination requests: `prepare_agent_message`, `deliver_agent_message`, `set_dependency_edge`,
  `remove_dependency_edge`, `create_team`, `update_team` and `delete_team`;
- Flow/authority requests: create/get/version/archive FlowDefinition, preflight/start/get/pause/resume/
  cancel/abort/retry/reconcile FlowRun, issue/get/revoke DelegationGrant and
  `submit_delegated_operation`; its closed packet/message variants include both Prepare and Deliver;
- continuity/input requests: target-global runtime inventory/Recovery, input lease acquire/renew/handoff/
  release and attempt/binding/lease-fenced `write_runtime_input`/`resize_runtime_input`, plus bounded target
  ResourceInventory get/subscribe/unsubscribe and exact `terminate_resource_owner`;
- profile requests: create/adopt/list/get/authenticate/validate/rename/default/retire/delete
  `AccountProfile` under its closed lifecycle, and list/get/create/update/validate/discover/default/retire/
  delete `ModelEndpointProfile` under its independent closed lifecycle;
- source/provider requests: query and separately create/edit/comment/assign/transition/close/reopen by exact
  WorkItemKey; query/adopt/resume/title-read/conversation-rename by exact ConversationKey; and independently
  list/create/update/pause/resume/run-now/cancel-iteration/delete by exact NativeJobKey;
- naming requests: get DisplayNameFacts, set/unpin a local alias and generate/apply an exact NameProposal;
- background-delivery requests: local list/get/pair NotificationEndpoint, revoke DeliveryGrant, inspect/flush
  outbox and subscribe/unsubscribe live notification status; NotificationHost is not RemoteOperatorSurface;
- remote permission requests: local-only issue/revoke `RemotePermissionResponseGrant` and the sole remote/
  Companion `submit_permission_response` operation, distinct from legacy `approve_permission`;
- responses: `node_view`, `node_view_subscription`, `attention_route`, `session_activation`,
  `agent_instance`, `runtime_continuity`, `context_link`, `context_packet`,
  `context_packet_delivery`, `agent_message`, `agent_message_delivery`, `dependency_edge`, `team`,
  `workspace_onboarding`, `checkout_scope_provisioning_receipt`, `checkout_scope`, `group_tree`,
  `resource_inventory`, `display_name_facts`, `name_proposal`, `model_endpoint_profile`, `model_catalogue`,
  `notification_endpoint`, `delivery_grant`, `notification_outbox` and their typed receipts;
- pushes: subscription-scoped `node_view_changed`, `runtime_attempt_changed`, `context_usage_changed`,
  `quota_scope_changed`, `context_link_changed`, `context_packet_changed`, `agent_message_changed`,
  `dependency_edge_changed`, `team_changed`, `runtime_continuity_changed`, `resource_inventory_changed`,
  `display_name_facts_changed`, `model_endpoint_profile_changed`, `notification_outbox_changed` and
  `live_notification_status_changed`;
- `AttentionView`: tagged exact Node/AgentInstance, authenticated provisional scope or unassigned Session;
- `AttentionRoute`: surface/connection/daemon generation, attention id, the same tagged subject, optional
  verified interaction owner and NodeView or provisional-demand bootstrap revision;
- `TreeSurfaceState`: selected hierarchy key; `ViewTarget` is derived and never broadcast between surfaces.

The current v4 `HierarchyKey` has `workspace`, `session` and `process` tags. The incompatible vNext protocol
replaces the last tag with a general `node` tag whose payload includes the closed Node kind, covering Agent,
Subagent, Shell, Command, Tui, Service, Process, Log, Group, Team, Flow, WorkItem, Job, Note, File, Diff,
WebPreview, Browser and Media
without forging process identity. Migration converts
every old process key losslessly; a mixed-version peer is rejected at handshake rather than guessing.

Every vNext mutation inherits the protocol's `MutationEnvelope`: operation/principal/capability, daemon and
authority generations, every touched StateStreamKey revision, exact object revisions and foreground surface/
connection when required. Operation-specific payloads cannot omit or weaken those fences. Input/resize uses
AttemptOwner, RuntimeAttempt, optional RuntimeEndpointBinding, lease and client/surface generations plus a
per-lease sequence; negotiated vNext/remote/multi-client peers cannot call v4 `write_pty`/`resize_pty`.

State ownership uses exactly `Installation(daemon_generation)|Workspace(daemon_generation,WorkspaceId)|
ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)`. Installation owns global Attention,
notification endpoints/grants/outbox and target/profile catalogues; Workspace owns semantic work, GroupTree/
CheckoutScope projections, display-name facts and defaults; ExecutionTarget owns Runtime/Resource inventory,
Recovery, NativeJobs, Runtime/Model endpoints and target/profile observations. A cross-stream transaction carries an exact
expected/new revision vector plus one barrier receipt; independent streams have no invented total order and a
gap blocks affected mutation until resnapshot. In particular, unmatched target handles remain in one
installation-visible target Recovery View when no Workspace exists or after its former Workspace is deleted;
a Workspace receives only explicitly bound references.

`ExecutionTarget` is an installation-owned stable id with closed state transitions `proposed →
probing|retired|deleted`, `probing → trust_pending|connected|disconnected|mismatch|retired`, `trust_pending →
connected|mismatch|retired`, `connected → disconnected|mismatch|retired`, `disconnected →
probing|connected|mismatch|retired`, `mismatch → trust_pending|retired`, `retired → probing|deleted`, and none
from deleted. Trust pins an independently proved fingerprint/generation; connectivity never grants or rotates
trust. Revisioned Workspace bindings expose only a closed backend scope, while
target inventory/Recovery survives every binding and Workspace. AccountProfile uses the closed lifecycle
`draft|authenticating|validating|active|auth_failed|expired|revoked|retired|deleted`; only active profiles
on the exact trusted target generation with proved isolation can become a default or launch, and deleted is
a permanent tombstone. `WorkItemState` and `ProgressUpdate` use the exact closed schemas and transition tables
in `docs/PROTOCOL.md`; neither can project itself into Flow/attempt/turn/dependency/Attention authority.
Desktop/remote/Companion consume the same bounded per-provider/Profile/Target `AccountActivityProjection`:
context, every quota window/reset and exact conversation/job-iteration/Attention inbox references retain
source, coverage, confidence, freshness and explicit unknown/unsupported/stale/rate-limited/fetch-failed.
Partial/absent data is never zero; profile caches never merge, and read/dismiss changes no provider work.

Remote agent input is also closed rather than inferred from “ordinary” operations. The handshake carries the
hash of a versioned exact per-operation allowlist and every absent/new operation defaults denied. For an
agent attempt, raw bytes are accepted only while verified `InputSafetyState=ordinary`; non-authorising
questions/decisions use the exact typed interaction route. A recognised permission uses only
`submit_permission_response` under a single-use local-foreground-issued grant bound to client, provider/
profile, Session/Node/instance/attempt/generation, interaction revision, provider-offered options, scope and
expiry. Credential, host-trust, authority-grant, destructive-confirmation and unknown states reject both a
remote response and bytes. A PTY-only adapter
that cannot prove classification advertises no remote input. Generic Shell/TUI raw input is disabled by
default and requires a separately consequence-labelled raw-terminal-execution invitation because Turn cannot
claim that arbitrary typed commands are non-destructive; typed control-plane refusal does not sandbox those
commands.

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
resume specs/receipts and configuration-transition receipts, ConversationKey bindings/inventory cache,
NativeJob/iteration metadata, WorkItemSource keys/conflicts, AccountProfile/activity observations, Browser
partition/history metadata without page bodies, GroupTree revisions, CheckoutScopes/projection bindings,
WorkspaceOnboarding phase receipts, ResourceInventory bounded observations, ModelEndpointProfiles and non-
secret credential-reference metadata, DisplayNameFacts and proposal hashes/receipts, NotificationEndpoints/
DeliveryGrants/outbox/live tombstones, permission-response grants/receipts, links, lineage, context-read audit,
handoff/message delivery metadata, context/quota scopes and bounded samples, dependency edges/results, Team
roles/policy and surface selection. Proposal source bytes, notification plaintext/token/private key and raw
model credentials are memory-only or keystore-owned and never durable protocol state. Every new table/file
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
7. **Flows and coordination.** Add typed messages, dependency results, Teams and ADR-061 FlowRuns/
   DelegationGrants without a canvas, hidden context rights or a second Attention authority.
8. **Durable runtime continuity.** Add external multiplexing or provider-runtime services only after warm
   attach, cold resume, remote failure and updater behavior have adversarial acceptance coverage.
9. **Resource nodes.** Add Group, Note, File, Diff, WebPreview, Browser and Media creation/persistence only after their canonical
   ownership, private-data, content-security and no-load-on-restore contract passes M14 acceptance.
10. **Group/checkouts and onboarding.** Ship recursive GroupTree CAS, CheckoutScope projection/lifecycle and
    resumable WorkspaceOnboarding with exact partial-effect recovery and primary-main isolation.
11. **Complete adapters and routing.** Ship the six dedicated adapter matrices, quota-only connectors and
    target-bound ModelEndpointProfile gateway path without hidden provider/model/account fallback.
12. **Resource observability and naming.** Add target ResourceInventory attribution/intervention and local
    DisplayNameFact/NameProposal precedence without provider or terminal authority.
13. **Background Attention delivery.** Add paired notification grants, bounded encrypted outbox/live status
    and zero-public-listener host mode as projections of the canonical queue.

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
  generic “Start pane” gate. Foreground Session activation restores/attaches and materialises only its exact
  bounded eligible descriptor set in one preflighted plan—or exactly one configured default Shell when that
  set is empty—in the same interaction; child/history/background selection never invokes it.
- Relaunch/resume and a verified same-conversation in-place model switch keep AgentInstance/tree identity and
  history while creating a new RuntimeAttempt epoch; fresh start creates a new Node/AgentInstance, and
  concurrent transitions are generation-fenced and idempotent.
- UI close/reopen preserves a live attempt; daemon or machine restart never duplicates an instance,
  recovered scrollback never claims liveness, old-attempt capabilities fail, and failed resume never becomes
  a fresh conversation.
- Requested/effective model, account, permissions, sandbox and flags cannot silently diverge.
- Context usage and shared account quota have distinct labels, scopes, reset/freshness and unknown states.
- AccountProfile fixtures exercise every declared lifecycle edge and reject every other one; only active,
  isolated exact-target profiles become defaults, ineligibility unsets rather than falls back, and delete
  reports each live binding/attempt/default/grant/audit blocker.
- At least one authenticated live adapter supplies effective model and context usage, at least one real
  provider/account supplies a quota window, and at least one provider transcript supports bounded pull and
  handoff. These may be different providers; unsupported adapters have negative fixtures instead of fake
  values.
- Claude Code, Codex, Gemini, OpenCode, GitHub Copilot and Grok each pass all 22 capability fixtures with
  exact `supported|unsupported|degraded|unknown` status and stale/version-bound evidence; Kimi/MiniMax quota
  connectors fail every launch/transcript/conversation/control attempt.
- ModelEndpointProfile tests exercise every closed lifecycle edge, all credential-reference kinds, network/
  discovery bounds and launch/switch freeze. A failed or uncertain switch leaves the prior attempt current;
  no endpoint/provider/model/account/local fallback occurs.
- Telemetry thresholds alone cannot focus, reorder or resolve Attention.
- A live ContextLink binds the logical destination/attempt and ignores caller-supplied destination ids;
  tests also state that this is not same-uid process isolation without an OS sandbox.
- Root create/update/renew/expand/revoke is accepted only from a foreground operator surface. An exact
  current Agent attempt may exercise one immutable pre-authorised operation only through
  `submit_delegated_operation`; source, destination, transformation, limits and generation are derived from
  the unexpired ADR-061 grant, and every direct administrative endpoint call by an agent is refused. All
  other agent-event attempts become proposals. Tests and UI also expose that an unsandboxed malicious same-uid process may steal the
  administrative capability and impersonate a surface. Every broker read is destination/attempt-bound,
  descriptor-jailed, atomically budgeted and audited. Revoke/read races are linearised before body commit,
  and ending/archive or deletion revokes authority permanently.
- A handoff packet is bounded and reviewed; its canonical bytes/hash enter the reviewed encoder unchanged.
  Native decoding is verified where supported, while PTY fallback claims deterministic submission only.
- A destination with a pending interaction rejects handoff without a partial write.
- Packet preparation creates no target/process/grant. New-target delivery exposes provisioning/launch/grant-
  install/write states and preassigns launch identity. Crash tests after every external-effect boundary prove
  that uncertain launch is only probed/adopted, uncertain grant installation is revoked and uncertain write
  is not retried; no ambiguous effect is automatically repeated or described as rolled back.
- Table-driven packet tests accept every declared PacketAuthority/DeliveryState/evidence transition and
  reject every other state, phase, reason and cross-product. Client disconnect, daemon restart and Flow
  recipe recovery land only in one of `draft_lost`, `launch_unconfirmed`, `grant_install_unconfirmed`,
  `submitted_unconfirmed` or `finished` as the proved boundary requires; a lost body/hash never advances an old op.
- `context_only`, `review` and `second_opinion` encode a reviewed recap-and-stop instruction and never claim
  compliance without evidence; `continue_with` can start only the separately reviewed next instruction
  behind **Send & continue**, without approving anything or inventing missing work.
- Submitted, received, read and acted states are never inferred from one another.
- Creating a branch/handoff target does not create a Pane or mutate the source Layout.
- Message delivery is ordered, bounded and retry-fenced; it cannot answer an existing prompt or imply
  receipt without evidence.
- Table-driven message tests reject undeclared body/transport/reason/evidence combinations. A definitely
  pre-write queued Flow message whose bytes die becomes terminal `failed(policy_reassembly_required)` and only a
  new op may reassemble it; a destination disconnect remains queued or becomes submitted-unconfirmed/
  refused from exact write/generation evidence and never invents `suspended`.
- Agent/conductor-authored message, dependency or Team changes execute only within an exact current
  DelegationGrant; otherwise they remain proposals/Attention. No durable message body or bearer enters
  protocol/storage.
- A delegated packet/message fixture completes Prepare then Deliver using only ids/receipt/hash, returns one
  durable receipt, consumes budget once and proves the agent never receives/reuses the sealed bearer; every
  cross-grant/attempt/generation/body/destination substitution fails before disclosure or write.
- WorkItemSource fixtures prove one WorkItemKey cannot bind two Nodes, partial/gapped/rate-limited pages never
  prove zero/deletion, every external write is source-revision CAS, and an ambiguous receipt reconciles rather
  than replays. Every illegal WorkItem transition fails without runtime/dependency/Attention effect.
- Progress fixtures exercise every queued/running/blocked/terminal edge, revision/sequence/percent-reset rule
  and prove even a terminal update cannot terminalise its StepAttempt or FlowRun without typed result evidence.
- Dependency cycles are rejected, idle is not mistaken for a completed result, and the bounded closed result
  schema rejects raw output/transcript/diff/file/environment/provider payloads. Outside a FlowRun no
  dependency starts work; inside it only an immutable reviewed start policy advances once from the exact
  current result. A Team keeps exact per-agent Attention subjects.
- A fail-run fixture leaves two sibling attempts live at the first required failure and proves the run enters
  `failing`; it reaches failed only after every sibling is terminal, skipped/cancelled or has an exact
  detached-survivor/disposition receipt. Any uncertain effect instead stays
  `reconcile_required(last_proved=failing,desired_terminal=failed)`; later survivor output cannot rewrite the result.
- RuntimeEndpoint reconnect proves mutual endpoint/conversation/instance/generation identity, launches
  nothing on warm attach and fails closed on mismatch; general Remote/SSH Session support is M16.
- Two endpoint records and two generations claiming the same ConversationKey can never expose two current
  owners. Binding proposed/refused/stale/unbound changes no Lifecycle, and only an independent bounded absence proof produces
  Lost.
- Conversation inventory tests isolate provider/profile/target/namespace caches, refuse title-only adoption,
  and prove `title_read` never enables `conversation_rename`. NativeJob tests preserve one key and ordered
  iterations across daemon absence and prove projection/Attention dismissal never invokes provider delete.
- Target inventory with zero Workspace bindings and after Workspace deletion remains reachable in the local
  target-global Recovery View. Remote/companion enumeration fails; adopting one exact item creates only the
  requested destination Node and cannot mutate another target/stream.
- ResourceInventory fixtures distinguish measured empty, unmeasured and every coverage state; reuse-safe
  process roots avoid PID aliasing, closed-Session survivors retain ownership, aggregates do not double-count,
  and `terminate_resource_owner` shares the exact RuntimeInventory receipt and affects no sibling.
- Display-name fixtures enforce pinned-alias and follow-source precedence, reject stale/redaction/control
  proposals, persist no captured source bytes and prove local rename never emits conversation rename or input.
- Remote-registry tests enumerate every operation and fail any missing/default-allowed classification.
  Sensitive/unknown prompt states enqueue zero remote bytes; a recognised permission accepts only the exact
  single-use `submit_permission_response`; a PTY-only unclassifiable agent gets no remote lease, and a generic
  raw Shell grant is visibly/auditably labelled as terminal execution authority.
- vNext/remote/multi-client negotiation rejects legacy `write_pty|resize_pty`; stale attempt/binding/lease/
  surface/sequence input accepts zero bytes, while an identical duplicate returns the original receipt.
- Offline hosts, missing worktrees and unsupported provider features remain visible and never fall back to a
  different host, checkout, model, account or fresh conversation.
- A 128-deep same-Session Group forest accepts only atomic acyclic GroupTreeRevision moves; concurrent cycle,
  depth and delete races fail without partial subtree change. Projection unbind preserves CheckoutScope,
  scope unbind/removal follows its own machine, live writers block rehome/removal and the composite catalogue
  receipt never creates duplicate/ownerless worktrees.
- Every WorkspaceOnboarding intent is crash/cancel tested at each frozen phase. Reconciliation never repeats a
  possible clone/checkout/trust effect, SSH never falls back local, publication is separate and no writer
  occupies the operator's primary `main` checkout.
- Notification fixtures exercise every grant/delivery transition, bounded same-id retry, revision-aware
  collapse, revocation during batching, encrypted minimal payloads, background/killed clients, late live
  ticks and same-titled children. Failure/acceptance resolves no Attention, deep links resynchronise, and
  NotificationHost exposes zero public listeners or RemoteOperator authority.
- Group/Note/File/Diff/WebPreview/Browser creation, restore and deletion preserve Session ownership, never load WebPreview/Browser on
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

Turn is an operator control plane, not an unbounded autonomous scheduler. Operator-authored Flows, bounded
recurrence and typed delegated creation/dependency advancement are in scope under ADR-061 and
`docs/OPERATOR_CONTROL_PLANE.md`. The supported authenticated flow never accepts agent events as hidden
context authority, permission approval, grant expansion or a way to move focus outside Attention policy.
This is not same-uid hostile-process isolation: without an active OS sandbox or UI-owned authority
inaccessible to the agent, a compromised local process may steal the administrative capability and
impersonate the operator.
