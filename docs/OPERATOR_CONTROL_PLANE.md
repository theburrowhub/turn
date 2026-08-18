# Turn operator control plane

**Status:** accepted post-v0.1 product contract; unless another document records named evidence, the
capabilities in this document are not implemented by the v0.1 build.

**Two different finish lines:** integrating this specification means freezing a semantically complete,
internally consistent requirement/proof inventory and merging it to `main`. It does **not** mean the product
has implemented the inventory. Product realisation is a later, stricter gate defined in §16.

This document defines Turn as the operator control plane for many concurrent agentic and terminal
processes. It joins the existing hierarchy, terminal, Attention, Agent Node, context and local-voice
contracts into one product. `docs/PRODUCT_REQUIREMENTS.md` is the frozen requirement inventory,
`docs/CONTROL_PLANE_ACCEPTANCE.md` is the proof matrix, and `docs/CONTROL_PLANE_GAP_AUDIT.md` records the
evidence-backed implementation gaps at the audited baseline. Neither a merged change nor a working happy
path is enough to claim this target: every inventory row must have its required evidence.

## 1. Product outcome

Turn lets one operator create, direct and understand a large body of concurrent work without polling a wall
of terminals. The work may be performed by Claude Code, Codex, Gemini, OpenCode, another agent CLI, a shell,
a TUI such as `k9s`, a long-running service, a log stream or an ordinary command. Agent-specific richness is
capability-gated, but all of them live in one coherent operational model.

The product succeeds when the operator can answer four questions immediately:

1. What work exists, and how is it related?
2. Which exact thing needs me now, and what decision or input does it need?
3. What is every process actually doing, with which model, account, flags, resources and remaining limits?
4. How can I create, continue, split, hand off or stop that work with the fewest safe interactions?

The left hierarchy is the canonical map. The right side is one selection-driven `WorkSurface`. Attention is
the interruption authority. A reusable Flow and a bounded delegation grant provide orchestration without
letting terminal output become executable control.

### 1.1 Non-negotiable product invariants

1. **Attention is the centre of the product.** Every question, permission, failure, completed turn and
   reviewable result routes to the exact semantic subject and safe action owner. Navigation alone never
   acknowledges or resolves it.
2. **The tree is truth, not decoration.** Agent delegation, provider-side children, local subprocesses,
   presentation grouping, dependencies, lineage and context are separate typed relationships. Turn never
   invents a parent merely to make the tree look plausible.
3. **Selection changes a view, not work.** Agent/child selection never starts/stops a process, changes Layout,
   sends input, acknowledges Attention or mutates a Flow. The sole composite exception is a foreground click
   on a Session row, which may select it and issue the separately typed `activate_session` intent once.
4. **Views do not own runtimes.** Semantic Agent identity, runtime attempt, provider conversation, operating-
   system process, PTY and Pane bindings have different identities and lifecycles.
5. **Provider dialects stop at adapters.** Core, store, protocol projections, Attention and UI consume one
   provider-neutral contract. Unsupported and unknown are visible values; neither is displayed as zero or
   silently emulated.
6. **The common path is low interaction.** Safe view attachment, already-authorised Flow steps and work
   creation happen automatically. Consequential, destructive or authority-expanding actions remain explicit
   and say exactly what they will do; there is no generic `Start pane` gate.
7. **Persistence is honest.** Turn distinguishes durable semantic work from an attachable runtime. A lost
   process is `Lost`, not a fresh conversation pretending to be restored.
8. **No invisible downgrade.** Remote failure never starts locally, resume failure never starts fresh,
   unsupported flags never disappear, and stale telemetry never becomes current truth.
9. **The primary checkout is reserved without exception.** No process launched, resumed, restarted,
   branched, restored, activated or otherwise managed by Turn may acquire a write lease on, make the branch
   unavailable in, or write to the operator's primary `main` checkout. This covers foreground and background
   work, single and parallel creation, Templates, Flows, Agents, Tools and generic terminals. Read-only
   inspection requires an enforced guard; every write-capable path uses a dedicated worktree or another
   explicit isolation backend.
10. **No text is executable authority.** Agent or terminal output may propose operations and raise Attention;
    only typed protocol operations covered by current capabilities and authority grants may act.

## 2. Canonical domain model

### 2.1 Ownership hierarchy

```text
Workspace
└── Session
    ├── FlowNode ── FlowRun
    ├── TeamNode                 (members are references, not duplicate rows)
    ├── GroupNode                (may contain Groups and one primary presentation of a member)
    ├── AgentNode ── AgentInstance ── RuntimeAttempt*
    │   ├── SubagentNode ── AgentInstance ── RuntimeAttempt*
    │   └── ProcessNode / LogNode
    ├── ToolNode ── RuntimeAttempt*
    ├── JobNode                    (projects one NativeJob; iteration records stay in JobNodeView)
    └── ResourceNode / WorkItemNode
```

A Workspace is a persistent project boundary. A Session is one operator-recognisable unit of work. Running a
Flow creates or reuses exactly one Session and records a `FlowRun`; it does not introduce a parallel
navigation root. Every visible child has a stable `NodeId`, one `NodeKind`, one owning Session and at most
one presentation Group. A Group may itself have at most one parent Group in the same Session; the resulting
presentation graph is a bounded acyclic forest rather than a one-level list.

`RuntimeAttempt*` means zero or more attempts over the lifetime of an agent/tool; an observed child may
exist before any launch/runtime evidence arrives.

The canonical kinds are:

- `Agent` and `Subagent` for semantic agent identities;
- `Shell`, `Command`, `Tui`, `Service`, `Process` and `Log` for terminal/runtime work;
- `Group`, `Team`, `Flow` and `Job` for explicit organisation, Turn orchestration and provider-native
  scheduled/background work without conflating those authorities;
- `WorkItem` for local or externally sourced work records; and
- `Note`, `File`, `Diff`, `WebPreview`, `Browser` and `Media` for typed resources, with inert `WebPreview` content and
  interactive `Browser` intentionally distinct.

Kinds declare their `ContentCapability`; clients do not guess a view from a title or executable name. The
kind set is closed per protocol version and extensible through version negotiation.

### 2.2 Independent relationship axes

The same nodes may participate in several graphs. They must never be collapsed into one `parent_id`:

| Relationship | Meaning | Affects canonical tree placement | Grants context | Starts work |
| --- | --- | --- | --- | --- |
| `OwnershipEdge` | Workspace/Session owns a durable node | supplies the root/fallback | no | no |
| `SpawnEdge` | one runtime or agent created another | yes when verified | no | no |
| `ProcessEdge` | observed OS parent/child relationship | only when no stronger display edge exists | no | no |
| `GroupMembership` | explicit single Group presentation, including Group-in-Group | yes, as an operator override | no | no |
| `TeamMembership` | role membership in zero or more Teams | no; Team View holds references | no | no |
| `FlowMembership` | node/step belongs to one or more recorded runs | no; Flow View holds references | no | only through run policy |
| `DependencyEdge` | typed result gates a downstream step | no | no | only through an authorised Flow policy |
| `ContextLink` | destination may pull bounded source context | no | yes | no |
| `LineageEdge` | branch, resume, continuation or handoff provenance | no | no | no |
| `MessageEdge` | bounded instruction/status delivery | no | no implicit transcript access | no by itself |

Every inferred relationship carries source, confidence, observed time and revision. Strong evidence is never
replaced by weaker evidence. An unassigned child remains visibly unassigned under its Session rather than
being attached to a plausible agent.

One operational Node has exactly one primary row. Its display parent is chosen deterministically in this
order: its one explicit Group, its strongest verified semantic SpawnEdge, its strongest verified ProcessEdge,
then its owning Session. A Group's display parent is its explicit parent Group or its owning Session. Every
GroupMembership mutation is one compare-and-swap transaction over a Session-scoped `GroupTreeRevision`; it
revalidates same-Session ownership, a maximum depth of 128, uniqueness and acyclicity after concurrent moves.
Evidence strength and source priority choose an ancestry tier; if two different parents remain equal at the
strongest tier, placement is ambiguous and stays unassigned rather than using an arbitrary id to invent
parentage. Stable edge id orders only the displayed competing references. Team and Flow membership,
non-winning ancestry and lineage render as activatable references to that row, never aliases. Moving a Group
changes only the primary display edge; semantic child counts traverse SpawnEdges, process counts traverse
ProcessEdges and neither changes. Removing a non-empty Group requires one closed disposition:
`refuse|promote_children|move_children_to_session`; it never cascades into runtime, context, Attention or
checkout deletion. A fixture combining nested Groups, spawn parent, process parent, multiple Teams and Flow
membership is the canonical placement oracle, including concurrent reparent, cycle, depth and delete races.

The canonical tree continuously derives `ProjectedRows`, the deterministic logical/accessibility preorder
after expansion and filtering. `MaterializedRows` is only its viewport subset; virtualization never changes
identity or order. Layout packs ProjectedRows compactly without overlap from bounded row metrics and needs no
tidy/arrange action, persists no row coordinates and never changes parentage, identity, selection, runtime,
input ownership or Attention. Restore, resize, zoom, variable row height and concurrent topology preserve
that order; an extreme or failed row is bounded rather than allowed to overlap, hide or reorder another row.
`TreeRowGap` is one versioned design token in `0..=8` logical pixels at 100% zoom and scales only with UI
zoom. For adjacent ProjectedRows, the latter's top equals the former's bottom plus exactly that gap;
indentation changes only the horizontal axis. A virtualized spacer equals the prefix sum of the omitted
ProjectedRows' bounded heights plus their gaps, so virtualization cannot manufacture blank space or change
order. A selected/focused row that remains projected is pinned materialized and keeps its scroll anchor.

Scale does not turn the canonical tree into a 100,000-row full payload. One `HierarchyIndexSnapshot` at a
pinned daemon/hierarchy revision contains only compact non-reused key, parent index, closed kind, sibling-order,
visibility/Attention flags and closed `RowMetricClass` for every admitted Workspace+Session+Node coordinate
(≤111,024 total), with complete/gapped coverage and≤6 MiB encoded. It contains no label, preview, terminal,
transcript, log, Note, media or inspector body. The metric class lets every client compute exact omitted-height
prefix sums at its zoom without loading row text.

Visible row summaries are fetched automatically in stable scope pages of≤500 rows/1 MiB. A
`get_hierarchy_page.begin` with no caller id names the exact Surface/index/filter revision and scope; the daemon
mints `HierarchyScanId`, and continuation pins that revision, page sequence and predecessor digest. Scope is exactly
`installation_roots|workspace(WorkspaceId)|session(SessionId)|subtree(NodeId)`; a page is
`complete|partial(next_cursor)|gapped(minimum_revision)`. Sixteen scans/connection and 1,024 installation-wide
exist for 60 seconds idle, with≤16 MiB scan metadata; finish, explicit close, revision gap, scope loss,
disconnect or expiry releases. N+1 returns a typed refusal without evicting a visible scan.

There is no Load-more interaction. The client prefetches viewport+fixed overscan. Selection, keyboard search,
restore and Attention routing call `reveal_hierarchy_key`, which returns the exact≤128-Group ancestor chain plus
Workspace/Session and enough≤1-MiB row pages to materialise the target at one revision; stale/gapped reveal
resnapshots automatically and never substitutes a same-labelled row. Closed filters are daemon-evaluated into
a≤16-KiB raw revisioned match bitmap (closed packed-bit codec≤24 KiB NDJSON wire) over the compact index, so filtered nodes are neither hidden by missing pages
nor fetched eagerly.

After bootstrap, `hierarchy_changed` carries only an ordered `HierarchyDelta` of≤4,096 compact topology/flag/
metric operations and≤180 KiB serialized in one complete≤256-KiB encoded frame, or
`gap(affected_scope,minimum_revision)`. Count and bytes are independent limits; overflow becomes the scoped gap,
never a fragmented unsolicited push or full detailed replacement. A gap automatically refreshes the compact
index and affected visible pages; unrelated current pages remain
usable only when their pinned revision is still named by the replacement. This preserves total navigation,
exact selection and Attention reveal while bounding wire, client memory and repaint work.

Expansion, filtering and topology have a separate deterministic visibility reducer, not a layout side
effect. Collapsing a focused descendant retains its selection identity but focuses the collapsed projected
ancestor; filtering it out retains identity and focuses the filter control, restoring row focus if it becomes
projected again; deleting it selects/focuses the nearest following projected sibling, then previous sibling,
then owning Session. Only that user/topology operation may apply the fallback—ordinary layout reflow may not.

A Group may project an optional Session-owned `CheckoutScopeBinding` to one proved local or remote repository
worktree. `CheckoutScopeId` and repository/worktree identity remain distinct from GroupId; the binding grants
no runtime or repository authority and does not make Group an execution owner. It supplies the default
cwd/isolation input for newly created descendants and for an explicit `move_and_rehome` operation;
merely moving an existing Node changes presentation only. `move_and_rehome` separately preflights and records
each stopped descriptor it changes, refuses live writers, and never silently rewrites a running process cwd.
Create/adopt/bind/unbind/remove/reconcile use stable repository, worktree and target generations. Missing or
foreign worktrees make the CheckoutScope `missing|conflicted` and any current binding `stale`; neither permits
local-path fallback. Deleting a Group or unbinding keeps the
worktree; removal of an app-created worktree/branch is a separate foreground destructive operation with dirty,
unpublished, ownership and survivor proof. Agent-per-branch Flows remain the preferred automation: each
writable member gets its dedicated worktree and an optional Group projection only makes that separately owned
isolation visible in the tree.

### 2.3 Stable identities

An `AgentInstance` is the operator-recognisable agent across verified continuation. A `RuntimeAttempt` is one
concrete launch, adoption of an already-running external runtime, resume, restart or effective-configuration
epoch. Attaching a view to an existing attempt never creates an attempt. A provider conversation/thread id,
operating-system pid, durable-runtime handle, PTY, input owner and Pane are bindings, not substitute ids.

An agent Node owns exactly one AgentInstance. The instance owns ordered RuntimeAttempts and at most one
current attempt. A verified resume or in-place model switch may preserve the instance and add a new attempt;
a branch, handoff, fresh start or unverified continuity creates a new Node/instance with lineage. Every
operation carries expected instance, attempt and generation so a late result cannot mutate a replacement.

The target schema never aliases these ids, including during migration. Cardinalities are:

| Relation | Cardinality and rule |
| --- | --- |
| Workspace → Session → Node | one-to-many; every Node has exactly one owning Session |
| agent-kind Node ↔ AgentInstance | exactly one-to-one; non-agent kinds have none |
| attempt owner | exactly one tagged owner: `AgentInstance` for `Agent`/`Subagent`, otherwise its runtime-capable Node (`Shell`, `Command`, `Tui`, `Service`, `Process` or `Log`) |
| attempt owner → RuntimeAttempt | zero-to-many ordered attempts; at most one current non-terminal attempt per owner |
| ConversationKey ↔ RuntimeAttempt | one canonical provider/account-scope/target/namespace conversation may span verified ordered attempts; one attempt names at most one ConversationKey; across the installation one ConversationKey has at most one current AgentInstance owner, while endpoint generation only fences transport |
| RuntimeAttempt ↔ durable handle | an attempt has zero-or-one current handle; one `(ExecutionTarget, backend, handle, generation)` binds one current attempt and reuse creates a separately evidenced generation |
| RuntimeAttempt ↔ root PTY | an attempt has zero-or-one root PTY; one PTY/backend generation belongs to exactly one attempt, though many Panes may view it |
| RuntimeAttempt → OS Process | zero-to-many; at most one declared root process and every surfaced process identity belongs to exactly one current attempt/owner or remains an explicit unresolved observation |
| Layout → Pane → runtime/input owner | a durable Pane belongs to exactly one Session Layout and binds zero-or-one content/runtime owner; an owner has zero-to-many viewers; a temporary Pane belongs to exactly one Surface instead of a Layout |
| ClientConnection → Surface | one connection generation owns one-or-more Surfaces; each live Surface belongs to exactly one connection generation and is never transferred implicitly |
| runtime/input owner → InputLease | exactly zero-or-one current input/resize lease holder; viewers without it are read-only |
| Session → FlowRun | zero-to-many immutable runs; a Flow Node projects exactly one run and a work Node records each producing step/run reference |
| Session → Group forest | zero-to-many Groups; every Group and presented non-Group Node has zero-or-one current GroupMembership, all inside the same Session; depth is at most 128 and one GroupTreeRevision fences the forest |
| Session → CheckoutScope → Group projection | a Session owns zero-to-many scopes; one scope has zero-or-one current CheckoutScopeBinding to one Group, while the Group never becomes scope/repository identity or authority |
| WorkItemId/WorkItemKey → WorkItem Node | one WorkItemId owns exactly one canonical Node in one Session; an installation-wide `(source_id, source_profile_id, project_namespace, external_item_id)` has at most one current binding to that pair. Other Workspaces may hold authorised activatable references, never duplicate Nodes; a preassigned pair may own one external CreateIntent before the key exists |
| Job Node ↔ NativeJob create/job | a preassigned Job Node owns exactly one current NativeJobCreateIntent until it atomically binds zero-or-one NativeJobKey; once bound it projects exactly one job with ordered stable iteration keys, and any spawned runtime/agent remains a separate referenced Node |
| ExecutionTarget → ModelEndpointProfile | zero-to-many non-secret route profiles; one launch/switch receipt freezes exactly one profile revision/model/credential generation or explicitly none, never an implicit fallback |
| WorkspaceOnboardingId → Workspace | one operation preassigns zero-or-one intended Workspace identity and yields at most one completed Workspace; retries reuse the operation and never create a second target/repository clone |
| NotificationEndpoint → DeliveryGrant → Delivery | one endpoint has zero-or-many grant generations with at most one active equivalent scope; one NotificationDeliveryId carries one exact Attention revision and retry history, while a collapse family may supersede older revisions only |

Legacy rows that reused a `NodeId` as an instance id are migrated atomically to freshly minted ids with an
alias tombstone used only to resolve old references; new protocol responses never expose the alias as the
new identity.

`LaunchSpec` records requested provider, executable, model, account, modes, safe flag names, cwd, host,
worktree, environment profile and capabilities. `LaunchReceipt` records the effective values, adapter,
runtime binding, provider conversation, warnings and provenance. Secret values and raw environment content
are excluded. Requested, effective and currently observed facts are separate fields.

## 3. One hierarchy and one WorkSurface

The hierarchy remains visible and is the only canonical navigator. Selecting its Workspace, Session or Node
sets one `ViewTarget` and changes one `WorkSurface`:

```text
Hierarchy selection ──► daemon-resolved ViewTarget ──► WorkSurface
                                                   ├── Workspace overview
                                                   ├── Session Layout / FlowRun overview
                                                   └── typed Node View
```

`ViewTarget` is a closed daemon-resolved algebra, never a label, path or client-selected free-form key:

```text
ViewTarget = workspace(WorkspaceId, WorkspaceRevision)
           | session(WorkspaceId, SessionId, SessionRevision, LayoutRevision)
           | node(WorkspaceId, SessionId, NodeId, NodeRevision, closed ContentKind)
           | historical_conversation(
               provider, AccountProfileId, AccountProfileRevision,
               ExecutionTargetId, TargetGeneration, provider_namespace,
               ConversationKey, PrivateTranscriptSearchIndexGeneration,
               TranscriptSourceRevision)
```

The first three variants are derived bijectively from one current `HierarchyKey`; the historical-conversation
variant is derived only from an authenticated current private-search hit. It is Surface-scoped read-only
presentation: it is not a Node, has no tree row, owner, runtime, input route or process, and cannot be adopted,
resumed, branched, launched, messaged or used as context merely by being viewed. Selecting it replaces the same
WorkSurface while retaining the prior hierarchy selection as navigation origin; Back or the next hierarchy
selection replaces it rather than opening another window or tree. Profile/grant/target/index/source revision
loss invalidates it immediately and yields a precise bottom-status refusal. A late response for a previous
Surface or ViewTarget generation is discarded and cannot change selection. Invalidation automatically returns
the same WorkSurface to the current retained hierarchy-origin target, or the owning Workspace overview when that
origin disappeared; it never leaves a blank panel, opens another view or asks the operator to start anything.

Selecting a Session restores its exact Layout, zoom and focused Pane. Selecting a child replaces the centre
with its unique content while leaving that Layout untouched. Back/forward navigation and return-to-Session
use stable keys and revisions, not widget history.

`SurfaceId` is installation-minted, monotonic/non-reused and never caller-selected. `open_surface` either asks
the daemon for a new id under the authenticated `SurfaceOwner=local(LocalOperatorIdentityId)|
remote(RemoteClientId)` or resumes an exact existing owner/id/revision; it atomically binds one current
connection generation and invalidates the predecessor before returning state. A changed owner, retired/expired
id or forged resume never creates an alias. `retire_surface` is an idempotent owner+revision-fenced presentation-
only mutation that releases the record, its PresentationHistory and every quiescent ephemeral child; any
still-live/uncertain Turn worker first transfers its existing reservation to `ProcessCleanupCharge`. The owner's compact
monotonic high-water rejects reuse without one tombstone per retired Surface. Installation owns
`SurfaceRegistry`, owner high-water and a bounded `SurfaceHistoryIndex` of≤256 Workspaces with nonempty history
for that Surface; Workspace remains the owner of each indexed PresentationHistory partition.

One connection may own four live Surfaces, one SurfaceOwner may retain eight live-or-dormant Surfaces and the
installation 64. `TreeSurfaceState` contains at most one selected key, closed
`expansion_default=collapsed|expanded` plus 2,000 exception keys, 2,000 manual-order keys, 32 closed filters
each≤256 encoded bytes, one bounded visibility mode and one scroll anchor; keys are
deduplicated, authorised and stale keys prune on hydration. One encoded Surface record is≤256 KiB and the
aggregate≤16 MiB. Count/bytes and owner high-water reserve before mint/resume; each N+1 returns a typed refusal
without changing connection, selection, history or temporary Panes.

A `TemporaryPane` is a Surface-owned ephemeral view, never a durable Pane/Layout and never a process, PTY or
renderer launch. One Surface holds≤8, one connection≤32 and the installation≤512; each record is≤4 KiB and
the aggregate≤2 MiB. Exact view activity refreshes a 30-minute idle deadline. Close, successful promotion,
source invalidation, disconnect, Surface retirement, daemon restart or expiry releases. Promotion reserves
durable Pane/Layout/core capacity before one atomic owner swap; failure preserves the temporary view. Count/
byte N+1 mints nothing and does not alter another view.

Each active Surface additionally owns one≤4-KiB `SurfaceConnectionBinding`,≤4/connection and≤64/256 KiB
installation-wide. Open/resume reserves it before atomically replacing a validated predecessor; disconnect/
replacement revokes and releases the binding before dormancy, reconnect starts a fresh generation, and a
fifth/65th/oversize attempt changes no Surface or owner.

Disconnect makes the Surface dormant after revoking temporary Pane/input/subscription/playback/edit/projection
authority and releasing quiescent children. A still-live/uncertain worker transfers its exact correlation,
family slot and family/shared-RSS bytes atomically to Installation-owned `ProcessCleanupCharge`; it cannot veto
dormancy and a reconnect cannot inherit it. Its bounded navigation record remains for 30 days. Exact resume refreshes only that Surface's
deadline. Explicit retirement, owner deletion or dormant expiry deletes presentation state and history after
the nonreuse high-water commits. Reconnect may resume an authorised id or request a new bounded one, but never
implicitly clones a Surface. `get_hierarchy` is then a pure read over one already-open Surface and cannot claim,
mint or transfer it.

`set_tree_expanded_all(value)` sets the default and clears exceptions in one constant-size mutation; it never
enumerates 100,000 keys. `set_tree_expanded(key,value)` removes that key when value equals the default and
otherwise stores it as one exception. The 2,001st exception refuses atomically, while expand-all remains
representable at any admitted hierarchy size.

Retire/owner-delete/dormant-expiry is a daemon-derived cross-stream invalidation: it atomically advances the
Installation SurfaceRegistry/high-water and one barrier over the current revisions of every indexed Workspace
history partition, making all entries unreachable before the Surface record is released. The client never
supplies or can omit that vector. Physical history compaction may follow, still under each Workspace's 200-entry
cap. A 257th nonempty Workspace partition records `history_not_recorded(capacity)` for that presentation action
after bounded eligible compaction; the navigation mutation still applies and cannot grow the index or block
Surface retirement.

### 3.1 Node View catalogue

| Kind | Primary content |
| --- | --- |
| Agent | identity/task, lifecycle and turn, current attempt, verified input, activity/transcript, children, context, quota, launch truth and actions |
| Subagent | role/task, parent, elapsed time, lifecycle/turn, tools/tokens when known, live bounded transcript or structured activity, result and lineage |
| Shell/Command/TUI | owned live terminal, command and cwd, process tree, exit/restart history and safe lifecycle actions |
| Service | health, endpoint metadata, resource use, logs and restart policy without implying agent turns |
| Process | technical identity, ancestry evidence, resource state, output/log handles and owner references |
| Log | bounded streaming output, source, filter/search and retention state |
| Flow/Team | members, roles, dependencies, grants, progress, messages, results and blocked steps |
| Group | nested member overview, optional checkout-scope binding and aggregated Attention; it owns no runtime, repository or checkout authority |
| WorkItem | canonical local fields or source-observed fields plus a retained local proposal, sync revision/staleness/conflict, comments/assignees and linked work without runtime authority |
| Note/File/Diff | inert bounded content, canonical source and privacy/checkout facts |
| WebPreview/Media | explicitly loaded inert isolated preview with origin/source and no ambient credentials |
| Browser | isolated interactive navigation, address/history, popup/download disposition, storage/permission state and reviewed local/loopback origin policy |
| Job | provider-native create/job identity, private definition or unavailable reason, independent schedule/iteration/presence/projection/mutation state, survival and nine exact capability-gated operations |

Every Node View defines loading, empty, unsupported, disconnected, stopped, lost and stale states. If an
already-live binding can be attached safely, Turn does so without asking. A stopped or lost node exposes a
precise action such as `Resume conversation`, `Restart command` or `Create fresh attempt`; it never shows a
generic `Start pane` action or makes view availability depend on a Pane.

The existing inspector becomes the details region of the selected view. Compact previews in the tree are
bounded status, never a second transcript or an implicit content view.

Search results, status groupings and an optional board are derived query views over the same stable Node ids
and WorkSurface routes. Board columns are saved queries over closed `WorkItemState` values
`backlog|ready|active|blocked|review|done|cancelled`; a card may project priority, due instant, tags, bounded
comments and an assignee AgentInstance/Team role. Compare-and-swap metadata edits update the canonical Node
record and append author/revision provenance. They never alter Lifecycle/TurnState, satisfy a dependency,
start work or resolve an interaction. A separately configured policy may translate a due/blocked/review
revision into one normal deduplicated Attention demand; the board owns no queue. A card is never a second
runtime, second terminal client by accident or a competing navigation authority. Activating it selects the
canonical tree Node/ViewTarget. Drag has keyboard/menu equivalents, and cross-column moves validate the
closed transition table before committing. A failed Node View is isolated behind a recoverable per-view
boundary so one bad renderer cannot take down the tree, terminal transport or other work.

The WorkItem transition table is exhaustive for Turn-initiated transitions of a Turn-authority state; every
move carries the expected item revision and an idempotent operation id. Local `create_work_item` atomically
mints one WorkItemId+NodeId under an exact Session/optional Group at `backlog`; no other initial local state is
accepted:

| From | May transition to |
| --- | --- |
| `backlog` | `ready`, `cancelled` |
| `ready` | `backlog`, `active`, `blocked`, `cancelled` |
| `active` | `blocked`, `review`, `done`, `cancelled` |
| `blocked` | `ready`, `active`, `cancelled` |
| `review` | `active`, `blocked`, `done`, `cancelled` |
| `done` | `ready` only as an explicit reopen |
| `cancelled` | `backlog` only as an explicit reopen |

A `WorkItemSource` binds canonical cards to an external repository-host issue/work system without becoming a
second tree or queue. In this closed contract every external WorkItemSource is repository-host-backed and
stores exact `RepositoryHostProfileId`,
`RepositoryHostCapabilityGrantId(kind=work_item_source)` and their revisions; every read/sync/mutation
revalidates that active grant's host,
target/trust, project scope, credential generation and expiry. `source_profile_id` in
`WorkItemKey=(source_id,source_profile_id,project_namespace,external_item_id)` is that immutable
RepositoryHostProfileId. A local-only WorkItem has no WorkItemSource rather than an invented host profile.
The key is installation-wide,
stable and never derived from title, URL or list order. `WorkItemBindingId` has immutable lineage and closed
state `proposed → current|refused`, `current → stale|detached|tombstoned`, `stale → current|detached|
tombstoned`, `detached → proposed|tombstoned`, with refused/tombstoned terminal. Import atomically installs a
preassigned WorkItemId+NodeId under an exact Session/optional Group and binds one fresh observed key; two
imports, including across Workspaces, CAS to one owner and the loser receives an authorised reference or
refusal. Rebind requires a detached binding and retains lineage. Detach, archive/forget/restore projection,
source deletion and local WorkItem deletion are explicit local operations and emit zero provider mutation;
key and Node tombstones fence every late sync resurrection.

External create is a visible saga before provider identity exists. `create_external_work_item` first commits
one WorkItemId, NodeId, `WorkItemCreationId` and `WorkItemCreateIntent` under its destination before dispatch.
Its closed state is `prepared → dispatching|cancelled`, `dispatching → bound|refused|reconcile_required`,
`reconcile_required → bound|not_created|reconcile_required`; bound/refused/cancelled/not_created are terminal.
Only `cancel_external_work_item_creation` may CAS prepared→cancelled and has zero source effect. Create is
advertised only with `create_correlation=idempotency_key_lookup|provider_receipt_lookup`; both modes provide a
side-effect-free exact outcome query and a write-only idempotency key is insufficient. The correlated
receipt binds one WorkItemKey to that same Node exactly once. `reconcile_external_work_item_mutation` looks up
only the original creation/mutation correlation and never redispatches. Title/body/time similarity is never
identity.

The source itself has closed `WorkItemSourceState=draft|validating|active|degraded|revoked|deleted`:
draft→validating/revoked/deleted, validating→active/degraded/revoked, active→validating/degraded/revoked,
degraded→validating/active/revoked, revoked→validating/deleted, deleted terminal. Each mapping/filter/
credential change mints a new source generation. One `WorkItemSyncRunId` follows
`prepared → fetching|cancelled`, `fetching → applying|partial|gapped|failed|cancelled`,
`applying → complete|partial|gapped|failed|cancelled`; terminals never resume. Cancellation while applying
fences the next page, lets the current atomic page commit-or-not, records its last applied cursor plus honest
partial/gapped coverage and then reaches cancelled; it never rolls back applied data or claims unseen items.
WorkItemSources are installation-owned. At most 64 non-deleted source records exist across all Workspaces;
the sixty-fifth create refuses before credential, provider or configuration effect. Delete frees that live
slot only after revoking its credential reference and generation and preserving every still-current binding,
mutation, conflict, identity and resurrection fence in its independently bounded receipt/tombstone owner;
nonterminal sync or mutation evidence cannot be compacted merely to admit another source.
Source ids are installation-monotonic and never reused. An installation-owned `WorkItemKeyRegistry` admits at
most1,000,000 current/binding/tombstone entries and480 MiB, each≤512 bytes. Independent fixtures reach the
count with small entries or bytes with983,040 maximum entries. Import reserves the exact key;
external create reserves an unbound slot before provider effect and binds its correlated returned key once.
N+1 count/byte admission refuses before provider request or canonical Node creation. A per-key tombstone
persists while its source can emit accepted generations; after terminal source deletion and all operations
settle, it may fold into the monotonic source-id/generation fence, which rejects every late old-source event
without retaining the key or permitting id reuse.

At most 10,000 WorkItemSource operation slots exist installation-wide across nonterminal sync/create/mutation
intents and uncompacted terminal receipts. Every provider request reserves its terminal slot before dispatch;
N+1 refuses before provider effect. Terminal rich metadata compacts after 30 days only after operation replay,
key/binding, source-generation and active-conflict fences are durable. Nonterminal, possible-effect,
reconcile-required and active-conflict evidence never ages out; same-operation retry cannot evade the bound.
Coverage (`complete|partial|gapped|unavailable`), freshness (`fresh|stale|expired`) and
backoff (`ready|rate_limited(retry_at)|offline|auth_required`) are independent axes. Pages/webhooks carry run,
source/mapping/filter/credential generation, cursor/watermark and item revision; an older generation or
post-detach/tombstone event has zero effect. `WorkItemPresenceState=observed|stale|missing|source_deleted|
unknown`; only fresh complete exact-scope absence can produce missing, while source_deleted needs an exact
provider tombstone/event. Filter exclusion, permission loss, partial/gapped/offline/rate-limit state never
proves deletion or exact zero.

Interactive `query_work_items` is independently bounded from a background SyncRun. Four queries/connection
and32 installation-wide may hold one≤2-MiB `WorkItemSourceQueryBuffer` each/64 MiB family-wide, also charged
to shared RSS before provider read. The request-only result is≤500 safe summaries,≤2 KiB/item and≤1 MiB
logical, whichever comes first; it excludes body/comments/credentials/raw provider data. Its authenticated
≤512-byte cursor binds source/project, every source/mapping/filter/credential generation, sort/filter, ordinal
and predecessor digest, and coverage is `complete|partial(next_cursor)|gapped(minimum_revision)`. The 501st or
next byte continues rather than truncating; stale cursor or oversize provider page gaps. Completion, failure,
cancellation,30-second request deadline or disconnect releases raw bytes, while response-stream/outbox bytes
are charged separately and the WorkItemPage retains nothing after atomic transfer.

The field schema is closed to title, body, link, state, priority, due, tags, comments and assignee. The source
declares exhaustive native mapping and authority `external|turn|reviewed_merge` per field. Local
`update_work_item_metadata` accepts only Turn-authority fields and the local transition table above. Incoming
fresh external observations retain native value, mapped value, source/mapping revision and provenance; an
external-authority state may legitimately jump to any mapped WorkItemState without pretending a Turn command
edge occurred. Reviewed-merge retains both revisions. Source edit/comment/assign/transition/close/reopen are
non-overlapping variants; comments/assignees have stable provider subidentities so a sync echo deduplicates.

Each outbound mutation owns `WorkItemMutationIntentId` and closed state `prepared → dispatching|cancelled`,
`dispatching → submitted|refused|reconcile_required`, `submitted → resolved|reconcile_required`, and
`reconcile_required → resolved|not_applied|reconcile_required`; terminals never retry. Non-create writes CAS
the exact binding, source, mapping and item revisions. `WorkItemConflictId` owns immutable per-field local/
external revisions and state `active → resolved|superseded|abandoned`; a newer source revision supersedes the
old conflict, and two resolvers have one CAS winner. Active mutation/conflict evidence cannot compact.
Credentials remain broker-only. No local hide/delete/dismiss, source config deletion or sync state closes,
reopens or deletes a provider item.

### 3.2 Window chrome and feedback

The application icon and Turn name lead the top bar. Creation/navigation icon buttons follow on the left.
Connection, active execution target, daemon state, effective app version and other global metadata are
right-aligned and each fact appears once. `+ Session` is scoped inside its Workspace row/menu, not outside the
tree.

Control-visibility customisation is presentation-only and fail-visible. The exact eleven optional ids are the
closed `HideableControlId` union in `docs/PROTOCOL.md`; everything else is unhideable. In particular Attention/
Next Attention, blocked and recovery routes, Delete/End, Restart, Search, Close and every destructive
consequence action remain visible at every supported viewport and zoom even if a stored settings value is
hostile or from a newer build. Hiding an allowed menu/header slot does not remove its canonical command:
palette and keyboard routes retain the same entry id, availability, typed effect and receipt. Unknown or
duplicate ids are ignored as visible and surfaced as invalid settings. The generated settings, chrome/menu,
CommandCatalogue and accessibility inventories share one bijection, so renderer code cannot invent a hidden
safety control or a pointer-only action.

`SemanticRecoveryInventoryKey` is exactly `workspace(WorkspaceId)|installation`. A Session/Agent End writes
semantic survivors to its owning Workspace inventory; deleting that Workspace atomically migrates its existing
entries and new container survivors to the Installation inventory with former ownership provenance. OS/runtime
handles instead remain in the distinct ExecutionTarget-owned `TargetRuntimeRecoveryInventory`. Neither
inventory invents a Session or grants provider authority.

Every eligible semantic effect that could survive End is covered by exactly one paired
`WorkspaceSemanticReservation` and `InstallationMigrationReservation` before it can become eligible or
produce its declared effect. An allocating subject owns that pair; an inherited child is recorded inside the
named parent's declared family bundle and owns no second SubjectKey, reservation or slot. The
closed 26-kind `SemanticRecoverySubject` registry, exact keys, family bundles, eligibility, allocate/inherit/
one-to-one transfer and release proofs are authority-hashed in
`SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv`; the independent family classification accounts for every durable
Workspace family exactly once and rejects future omissions. Inert Nodes, deterministic relationship history,
Attention, revoked grants/links/leases, presentation state and OS handles are explicitly excluded rather than
hidden in a catch-all.

The paired reservation shares one non-reused ReservationId/fingerprint, binds subject and Workspace, reserves
one≤16-KiB metadata/evidence record in the 4,096-subject/64-MiB Workspace budget and simultaneously one
migration slot in the 32,768-subject/512-MiB installation budget. Active subject storage remains under its own
category; the recovery record references it and never copies its body. Inherited child effects allocate zero
and remain covered by the parent's exact key, fences and revision vector;
create→projection, launch→live Node and quarantine→ticket transfers retain the same reservation id and retire
the source behind replay. Admission N+1 refuses before effect, while End only consumes an existing slot and
cannot refuse. Workspace deletion consumes its pre-reserved InstallationMigrationReservation and activates
the corresponding InstallationSemanticRecoveryEntry with the same id/byte charge and no new capacity
allocation. Late evidence attaches to the exact
tombstoned subject; an unmatched runtime handle uses `TargetRuntimeRecoveryInventory`, never an unreserved
semantic row. Resolved terminal metadata compacts after180 days only after nonreuse/replay proof and quiescence;
live, possible-effect, reconcile-required or cleanup-required evidence never ages out.

SQLite schema11→12 backfills only eligible legacy live/orphaned/reconnected/spawning process Nodes and stable
external-conversation Agent/Subagent identities under an exclusive data-directory lock and one transaction.
The DDL, closed census, validation, paired rows, registry-digest marker and user_version commit together; any
unknown/corrupt/dangling/duplicate/oversize/N+1/I/O/crash rolls back to an exactly reopenable schema11 with no
process, filesystem or network effect. A schema12 writer/restore remains blocked until pair bijection, digest
and census marker validate, and End/delete at full capacity performs zero reservation INSERT.

Transient operational progress, warnings and errors use the bottom status bar with scope and recovery
action. They do not displace content at the top. End/delete is a total daemon reducer, not a user-authored
survivor form: in one serial transaction it snapshots every semantic subject, discards uncommitted
Session-owned draft/body state, revokes Session authority, applies any still-valid predeclared rehome and
otherwise tombstones/retains independently live or uncertain semantic evidence in the exact
Workspace/Installation SemanticRecoveryInventory and pre-reserved capacity above. Concurrent evidence is reduced against the ending tombstone and cannot make the action
return “fix this first”. The Session leaves the active tree immediately; separate process/worktree/artifact
cleanup reports survivors but can neither veto nor resurrect the committed removal. Provider/user-data
destruction remains a different explicit operation and may refuse safely without blocking End.

Direct Archive remains the reversible hide-only verb and stops no process. It suspends ContextLinks and
revokes their bearers rather than permanently deleting them; restore requires full endpoint/authority
revalidation and performs no read by itself. Existing and newly arriving Attention stays canonical and routes
in one action to the exact archived subject's temporary WorkSurface without silently unarchiving it, starting
anything or changing link state. End/delete is the distinct permanent authority-revocation path above.

`StatusEvent` is not Attention. It carries severity, scope, operation id, text key/arguments, progress,
created/expiry time, optional recovery action and announcement policy. The bar shows the highest-severity,
most-recent event plus an overflow count; activation opens bounded history ordered by severity/time. Success
expires after five seconds, progress is replaced by its terminal event, warnings persist until superseded or
dismissed and errors persist until their operation is reconciled. A status becomes Attention only through a
separate typed actionable demand. Screen readers announce a progress operation at start and terminal state,
not on every update; concurrent lower-priority events remain reachable without notification spam.

`StatusEventOwner=Installation|Workspace(WorkspaceId)|ExecutionTarget(ExecutionTargetId,target_generation)`
selects its StateStreamKey; no event is ownerless. One encoded event is≤4 KiB. Each owner retains at most
1,000 current/uncompacted events, while all owners together retain at most 100,000 events/256 MiB. An operation
or external producer that may create a persistent warning/error reserves one event plus terminal-history
capacity before it can produce an effect; each already admitted producer also owns one overflow/gap slot.
Progress for the same `(owner,operation_id)` replaces in place. Active warning/error/recovery state never
compacts. Eligible success/reconciled/dismissed history compacts after seven days behind an owner minimum
revision and gap marker. After eligible compaction, count/byte N+1 refuses a new effect-producing operation or
producer before effect; externally arriving excess uses its reserved gap event and preserves the source
receipt/recovery evidence rather than silently deleting an active error. Status is a bounded projection of
that receipt, not a second effect authority.

### 3.3 Common-path interaction budgets

One *interaction* is a deliberate click/tap, activation keystroke or submitted gesture; typing task text does
not count per character. Pointer and keyboard routes have the same authority budget:

| Journey | Maximum after its starting context is visible |
| --- | --- |
| select and attach a live Node | one selection, zero confirmations |
| reselect a safe foreground Session | one selection, zero secondary start/restore action |
| route `Next Attention` to its exact action | one command, zero acknowledgement side effects |
| quick-create the default Session/Agent/Tool | open `+` and submit: two interactions, zero modal confirmations |
| launch a valid saved Flow | choose and launch: two interactions, zero per-child prompts |
| resume/restart from a stopped Node View | one consequence-labelled action; one consolidated review only if authority changed |

When a consequential choice is genuinely unresolved, Turn presents all such choices in one review and one
commit action rather than a confirmation chain. Creation from a different Workspace changes only the
explicit target shown in that review; it never silently reuses the currently selected Session.

Foreground Session activation is a typed idempotent operation, not merely selection. Selecting a Session
whose current revision already has a proved safe activation plan automatically restores its Layout and
attaches live attempts, materialises every eligible saved stopped runtime descriptor, and when no runtime
descriptor exists may create and start exactly its configured default Shell in the same interaction. The
plan fixes the bounded descriptor set plus each target, account, cwd, isolation, command and authority
generation and must have passed preflight without unresolved choice or new consequence. A stopped default
or saved descriptor may start only through a still-current, previously reviewed activation policy. Any stale revision,
ambiguous survivor, changed command/target/account, missing containment, permission need or unsafe input
owner fails closed in the WorkSurface with one precise consolidated recovery action. Merely restoring
persisted metadata, selecting a child, visiting history or viewing an ended Session never starts work.

## 4. Fast creation through one catalogue

One declarative, revisioned `CommandCatalogue` supplies every action surface. Its `category=creation`
projection is the `CreationCatalog`; that name denotes a filter over the same entries/revision, never a
second source. Each surface applies an explicit contextual placement filter: entries exposed in two surfaces
keep identical id/label/default/schema/capability semantics, but surfaces need not expose identical sets.
Session creation appears only inside its owning Workspace row/menu, never in the global toolbar. The catalogue
supplies Workspace `+`, toolbar, command palette and contextual menus with capability-
gated entries for Session, Flow, Agent, Shell, command/TUI, service, log, Group, Team,
WorkItem/source, provider-native Job, Note, File, Diff, inert WebPreview, isolated Browser, Media, worktree
and execution target. Labels, defaults, validation and availability
come from that one source so surfaces cannot drift.

Creating common work requires choosing the Workspace, a task/preset and optional prompt. Turn derives safe
defaults for cwd, isolation, adapter and Attention policy, presents only unresolved choices, preflights the
whole operation and performs it idempotently. A failed multi-node creation either rolls back unobserved
resources or retains a clearly failed `FlowRun` with recovery receipts; it never leaves invisible processes.
Provider, account, model, permission/sandbox mode and inherited values remain inspectable before launch
without forcing the operator through fields whose safe effective defaults are already known. Multi-create
progress and failures are per item, so one slow or failed member neither freezes creation nor hides successful
members.

After a successful single-item create, Turn selects the new canonical Node and places keyboard focus in its
verified prompt/composer or terminal input when it is ready; if no safe input owner exists, focus stays in the
tree and the Node View explains why. A multi-create selects the Flow/operation summary and never jumps among
children as they start. Cancellation restores the invoking tree selection. Contextual surfaces may filter,
group and order entries for scope/available space, but any exposed action's id, label, defaults, validation
and capability result always come from the same catalogue revision.

First run and later diagnostics use one resumable setup checklist: discover supported CLIs and remote
targets, show exact detected versions/capabilities, guide installation or authentication without collecting
credentials, request notification/microphone permissions only when the related feature is invoked, and
explain degraded integration plus remediation. Setup can be skipped without disabling generic terminal use.
Every probe is bounded, read-only unless a consequence is explicitly shown, cancellable and recorded as a
redacted diagnostic receipt; remote trust and host identity require a separate explicit adoption.

`WorkspaceOnboarding` is the single resumable catalogue path for `create_directory`, `open_directory`,
`clone_repository` and `adopt_ssh_target`. It freezes an operation id, intended Workspace identity,
ExecutionTarget, canonical path, repository/remote identity, authentication reference and current target
generation before any effect. `WorkspaceOnboardingId` is allocated before work and every command carries it,
the idempotency operation id and expected onboarding/target revision. `WorkspaceOnboardingState` is closed:
`prepared|running(phase)|cancel_requested(last_proved_phase)|reconcile_required(last_proved_phase,
possible_effect)|completed|cancelled|failed(reason,residuals)`. `OnboardingPhase` is the closed current step
`preflight|path_probe|directory|target_adoption|remote_fetch|checkout|workspace_commit|cleanup`; each intent
freezes one finite ordered phase plan before `prepared → running(preflight)`. `prepared` may also move to
`cancelled`; `running(phase)` may advance only to the plan's next phase, `completed`, `failed`,
`cancel_requested` or `reconcile_required`. `cancel_requested` fences new effects and moves only to
`running(cleanup)|cancelled|reconcile_required`. Reconciliation may select a proved next phase, cleanup or a
terminal result only from the exact phase receipt and observed target/repository identity. `completed|
cancelled|failed` are terminal. Every phase appends a bounded started/definite/uncertain/no-effect receipt.
`cancel_workspace_onboarding` prevents new phases and cancels only the currently declared cancellable effect;
`reconcile_workspace_onboarding` probes the exact preassigned directory/repository/remote identity and never
repeats an ambiguous effect. Open/adopt is inert
until local capability consent has been decided; clone is
cancel-safe and reports each created directory, fetched object, checkout and uncertain cleanup state rather
than hiding a partial repository. A retry reconciles by operation id and remote/repository identity instead of
cloning twice. SSH host identity and path are pinned and a failed remote operation never falls back to a
same-named local path. `publish_repository` is a distinct foreground operation with destination, visibility,
branch/upstream, credential-reference and consequence review; onboarding and successful local creation never
publish automatically. Every writer uses an isolated checkout and no onboarding path occupies the operator's
primary `main` checkout.

Publication preassigns one ExecutionTarget-owned `RepositoryPublishIntentId`. Before any host, ref or local
configuration effect, it freezes the canonical request fingerprint, exact hosted RepositoryAuthority,
RepositoryId/CheckoutScope/non-primary classification, isolated-worktree generation and lease, provider host/
account/destination/visibility/credential generation, source branch/tree/commit and expected remote ref,
upstream/config identities plus provider correlation for create-new repository and ref lookup. Providers that
cannot prove create and ref outcomes by exact correlation advertise publication unsupported.

Every state/receipt carries monotonic `highest_applied_phase=none|remote_created|remote_published|published`.
The closed saga is `prepared→creating_remote|cancelled|refused`,
`creating_remote→remote_created|no_effect|reconcile_required(creating_remote,none)`,
`remote_created→pushing|partial(remote_created,reason)`,
`pushing→remote_published|partial(remote_created,proved_push_no_effect)|
reconcile_required(pushing,remote_created)`,
`remote_published→configuring_upstream|partial(remote_published,reason)`,
`configuring_upstream→published|partial(remote_published,proved_config_no_effect)|
reconcile_required(configuring_upstream,remote_published)`. Reconcile-required moves only to the same phase's
proved successor, the matching phase-aware partial/no-effect result, or itself; it never lowers the highest
applied phase. Published, no_effect, partial, cancelled and refused are terminal. `no_effect` is legal only with
highest=none and proof that remote creation, ref push and config write did not happen; after remote creation every
non-success terminal is partial and preserves that external effect. Cancellation is legal only from prepared
with no-effect proof. Each effect phase commits its
dispatch marker and exact sealed postcondition before its one dispatch. `reconcile_repository_publish` queries
only provider correlation, exact remote old/new object ids or local config descriptor/value; it never creates a
repository, pushes a ref, writes config, rotates credentials or changes a lease. Same-id replay returns or
advances only from a proved predecessor and never re-dispatches an uncertain phase.

At most256 publication intents are nonterminal and10,000 nonterminal-plus-uncompacted records exist
installation-wide; one record is≤8 KiB and the aggregate≤64 MiB, reached by8,192 maximum records independently
of the count boundary. Exactly one nonterminal intent may own a
RepositoryId or canonical host/account/destination key. Active, terminal, journal, provider-correlation and
semantic-recovery capacity plus one of100,000 minimal replay fences≤512 bytes/48 MiB aggregate (98,304 maximum
fences at the byte boundary) reserves before
the first effect; every count/byte/fence N+1 refuses without host, Git, config,
filesystem, lease or primary-checkout effect. Terminal richness compacts after 180 days only after operation,
destination, object/ref/config, CheckoutScope/lease and correlation replay fences persist. Nonterminal, partial-
success and possible-effect evidence never ages out. Minimal fences survive for the installation lifetime;
full capacity refuses new publication instead of forgetting an operation and risking replay.

WorkspaceOnboarding is Installation-stream owned because its durable intent exists before any Workspace can.
The installation admits at most 100 nonterminal onboardings and reserves a bounded terminal receipt before
the first filesystem, target or network effect; the 101st begin refuses before creating a path, target,
repository request or Workspace. Completed Workspace creation is one revision-vector-fenced cross-stream
commit to the preassigned identity. Terminal phase evidence folds into at most 10,000 receipts after 180 days;
nonterminal, possible-effect, residual-cleanup and reconcile-required evidence never ages out.

Templates remain reusable Layout/configuration. Flows are reusable execution graphs. Duplicating a Session
copies its shape and selected Flow inputs but never claims to copy a live provider conversation.

## 5. Flows and bounded delegated control

### 5.1 FlowDefinition and FlowRun

A `FlowDefinition` is a versioned, parameterised blueprint containing:

- node specifications: kind, role, provider/tool, prompt/command, cwd and requested launch values;
- typed dependencies and start policies;
- context sources, packet budgets and allowed automatic links;
- Team/conductor/reviewer roles and result/synthesis expectations;
- execution target and worktree/isolation strategy;
- Attention policy, resource budgets, concurrency and timeout bounds;
- recurrence/missed-run/overlap policy when a bounded recurring Flow is enabled;
- the exact `DelegationGrant` offered to any agent that may expand the run.

A `FlowRun` is the durable execution of one immutable Flow revision inside one Session. It records inputs,
preflight, node/attempt operations, dependencies, grants, receipts, results and terminal state. Replaying the
same operation id cannot create duplicates. Editing a definition never mutates an existing run.

Run initiation and step readiness are distinct closed unions. `FlowRunTrigger` is
`manual{accepted_preflight_revision}` or
`bounded_recurrence{definition_revision, occurrence_id, scheduled_instant, schedule_policy_revision}`. Manual
requires one foreground `start_flow_run`; recurrence consumes one durable unique occurrence receipt and
creates at most one new FlowRun for that occurrence—never an arbitrary StepAttempt. `StepStartPolicy` is
`manual`, `with_run`, `after_success(edge)`, `after_result(edge,predicate)`,
`all_of(edges,predicates)` or `any_of(edges,predicates)`. Manual requires `start_flow_step`; `with_run`
becomes ready once when its owning FlowRun enters `running`; only the four dependency variants consume current
typed `DependencyResult`s. `any_of` records the first committed matching result, with stable edge id breaking
a same-transaction tie. Every readiness receipt is keyed by run, step, immutable policy revision and trigger
identity, so replay cannot advance twice. Dependency readiness is evidence, not permission: only the
operator-reviewed immutable policy may consume it automatically.

Each edge result is an immutable `DependencyResultKey` naming run, edge, producer step/StepAttempt ordinal+
generation and result revision. Its two axes are closed and separate: `outcome` is exactly
`succeeded|failed|cancelled|aborted`, while `origin` is exactly `step_terminal|verified_external`. Only the
producer's atomic terminal receipt—or the immutable policy's internal event derived from its exact canonical
external Turn receipt—may publish it; `verified` is never a fifth outcome or implicit success. Output text,
hooks, idle/done state, deletion and absence cannot publish. Lowest-ordinal published success wins;
without success only the final terminal attempt after retry disposition becomes current. The readiness record
is the `step_readiness` variant of durable `FlowOperationReceipt`, containing the canonical ordered result-key
set/digest and preassigned target StepAttempt; ready and blocked→ready commit together before the separately
idempotent start consumes it once. Results/receipts are bounded and remain while run/replay/recovery refers.

The verified-external path is an internal daemon reducer derived only from a canonical terminal Turn receipt
kind fixed by the immutable policy; there is no client, adapter or remote publication operation. For each
dependency policy, ready also reserves one RuntimeLaunchIntent/recovery slot and emits one daemon-only
`ReadyStepDispatchEvent` keyed to the exact readiness digest and preassigned StepAttempt. Crash before launch
consumes that reservation once; after possible effect reconciliation only looks up the exact launch intent.
An impossible required dependency atomically records Blocked plus one deduplicated actionable Attention demand,
or enters the immutable fail-run reducer. Navigation cannot resolve it, and neither deletion nor prose can
manufacture success.

This is also the sole adaptation of a dependency-gated “launch after these agents finish” workflow
(`CAP-109`). A creation shortcut may collect upstream Nodes and prepare the immutable FlowDefinition/FlowRun,
context links and dashed dependency projection in one foreground interaction, but it cannot persist an armed
shell command on an ordinary Node. Unknown upstream state is not success; a deleted/missing required producer
is an impossible dependency with explicit blocked/failure evidence, not silently satisfied; and a generic
`done` badge is insufficient unless it commits the exact typed DependencyResult required by the reviewed
StepStartPolicy. Only the readiness receipt may start the preassigned StepAttempt once. Reload/reconnect
reconstructs that receipt/state without redispatch, and removing a visual edge never changes an immutable run.

`FlowRunState` is closed: `preflighting`, `provisioning`, `running`, `paused(resume_state)`, `failing`,
`cancelling`, `reconcile_required(last_proved, desired_terminal?)`, `completed`, `failed`, `cancelled` or
`aborted`. `resume_state` is exactly `provisioning|running`; reconciliation retains the last proved phase
and an optional monotonic desired terminal result. Its legal transition table is:

| From | May transition to |
| --- | --- |
| `preflighting` | `provisioning`, `failed`, `cancelled`, `aborted` |
| `provisioning` | `running`, `paused(provisioning)`, `failing`, `cancelling`, `reconcile_required`, `aborted` |
| `running` | `paused(running)`, `failing`, `cancelling`, `completed`, `reconcile_required`, `aborted` |
| `paused(provisioning)` | `provisioning`, `failing`, `cancelling`, `reconcile_required`, `aborted` |
| `paused(running)` | `running`, `failing`, `cancelling`, `reconcile_required`, `aborted` |
| `failing` | `failed`, `cancelling`, `reconcile_required`, `aborted` |
| `cancelling` | `cancelled`, `reconcile_required`, `aborted` |
| `reconcile_required` | itself while evidence is appended; with no desired terminal, its exact `last_proved` or a proved terminal; with desired `failed`, only `failing|failed`, `cancelling` after explicit cancel, or strengthened `aborted`; with desired `cancelled`, only `cancelling|cancelled` or strengthened `aborted`; with desired `aborted`, only `aborted`. A desired terminal is never cleared or weakened |
| terminal (`completed`, `failed`, `cancelled`, `aborted`) | none |

`pause` prevents new step starts but does not suspend existing
runtimes; `resume` re-evaluates only still-current evidence. `cancel` atomically prevents new starts and
revokes grants, then applies each active step's predeclared `leave_running|interrupt_then_terminate|terminate`
disposition. `abort` is an explicit emergency action that fences all operations and applies preauthorised
force-kill dispositions; it reaches `aborted` only when every effect is classified, otherwise persists an
`aborted` desired terminal state in `reconcile_required`. It never deletes worktrees or artifacts. Cancelling during provisioning records
every created, rolled-back, surviving or uncertain resource. A manual retry creates a new bounded
`StepAttempt` under the same run, preserves earlier evidence and cannot exceed the definition's attempt
limit. Terminal runs never resume; retrying the whole run creates a new `FlowRunId` with lineage.

`StepAttemptState` is independently closed:

| From | May transition to |
| --- | --- |
| `blocked` | `ready`, `cancelled` |
| `ready` | `starting`, `cancelled` |
| `starting` | `running`, `failed`, `cancelled`, `reconcile_required` |
| `running` | `succeeded`, `failed`, `cancelled`, `aborted`, `reconcile_required` |
| `reconcile_required(last_proved, desired_disposition?)` | itself; with no desired disposition, its exact last proved state or a proved terminal; with a desired cancel/abort disposition, only that terminal (cancel may strengthen to abort) after receipt reconciliation |
| terminal (`succeeded`, `failed`, `cancelled`, `aborted`) | none |

Pause changes no active StepAttempt state. One exact `StepStartPolicy` readiness receipt moves
`blocked → ready`; the separately idempotent start operation moves `ready → starting`. A retry never reopens a terminal attempt: it creates a
new bounded StepAttempt linked to the old one and reevaluates its start policy.

Run aggregation is deterministic. A definition marks every step required or optional and fixes
`on_failure: fail_run|continue|skip_dependants` plus an active-attempt failure disposition
`leave_running|interrupt_then_terminate|terminate`. `completed` requires every required step's final attempt to
be `succeeded`, every other step to be terminal or have a durable policy-derived skip receipt, and no
unreconciled effect. A required `failed` attempt or permanently impossible required dependency enters
`failing` when the fixed policy says `fail_run`. Entering `failing` atomically prevents new starts, revokes
grants, marks every not-started dependant with an exact skip/failure receipt and applies the predeclared
failure disposition to every active attempt. It reaches `failed` only after every step and external effect
is terminal, deliberately detached by `leave_running` with an immutable survivor receipt, or otherwise
classified; any uncertain effect instead enters `reconcile_required(last_proved=failing,
desired_terminal=failed)`. `continue`/`skip_dependants` records the exact skipped
optional/downstream steps and cannot waive a required success unless the definition did so up front.
`cancelled` and `aborted` arise only from their explicit desired-terminal operations after all dispositions
are classified. Terminal derivation revokes remaining grants before committing the run result.

`bounded_recurrence` requires an IANA time zone, local schedule, explicit daylight-saving gap/fold policy,
`skip|latest_once|bounded_all` missed-run policy, `skip|queue_one` overlap policy, maximum catch-up count and
either an end instant or maximum occurrence count. Each occurrence id is derived from definition revision
and scheduled ordinal, so wall-clock rollback, sleep, reboot and duplicate scheduler delivery cannot run it
twice. Forward clock jumps and downtime follow the missed-run policy; no rule creates an unbounded backlog.

The versioned protocol exposes create/read/version/archive Flow definitions; preflight/start/get/pause/
resume/cancel/abort/retry/reconcile Flow runs; issue/get/revoke grants; and submit a typed delegated
operation. Each request carries operation id, expected definition/run revision and authority generation and
returns a durable receipt. A partial saga is recovered from its last receipt, never by replaying an unknown
external effect.

### 5.2 DelegationGrant

An operator may authorise a conductor or other agent to perform a bounded set of typed control operations
without repeated confirmations. The grant names the issuing surface/operation, Workspace/Session/FlowRun,
agent instance and attempt, authority generation, expiry, maximum nodes and concurrency, provider/tool
allowlist, cwd roots, worktree policy, context scopes, permitted dependency/message operations and resource
limits.

Within that grant the agent may create declared agents/tools, organise Teams, add permitted dependencies,
open approved ContextLinks, send bounded messages, publish bounded Resource Nodes/progress and request a
review/synthesis pattern. Delegated resource authority is a closed set of `create_resource` and
`update_resource`: the grant fixes permitted `Note|File|Diff|WebPreview|Media` kinds, owning Session/Group, typed
payload schema, author identities, cumulative node/byte/revision count, update rate and expiry. Update is
compare-and-swap and may target only a resource created by this grant or an exact pre-listed resource. Every
revision stores grant/agent/attempt provenance and a durable receipt. It may not delete or reparent a
resource, mutate an underlying user file merely by changing a Resource Node, turn content into executable
control, approve permissions, expand its own grant, reuse the primary checkout, access arbitrary context,
delete user data, merge/publish, execute text scraped from output or act after its attempt/generation
changes. A `ProgressUpdate` is a bounded closed status/result record attached to the producing step and
projected through its existing Node/Flow View; it never creates another Attention queue. Exceeding any grant
bound creates one exact Attention demand describing the requested expansion.

`ProgressUpdate` has `progress_id`, producer AgentInstance/attempt/generation, FlowRun/step/StepAttempt,
grant and operation ids, monotonic `sequence`, expected previous revision, `phase`, optional integer
`percent` in `0..100`, bounded message key/arguments, bounded artifact/result references and observed time.
Its closed phase transitions are `queued → running|blocked|failed|cancelled`, `running → blocked|succeeded|
failed|cancelled`, `blocked → running|failed|cancelled`; terminal `succeeded|failed|cancelled` never reopen.
Within one uninterrupted `running` phase percent cannot decrease; after `blocked → running` the next update
must name an explicit reset reason or retain the previous floor. Publishing the same operation id is
idempotent; a higher sequence with the exact expected revision atomically replaces the projection while
retaining bounded audit history, and a duplicate sequence with different content is refused. Progress text,
percent, artifact content and terminal phase are untrusted evidence: none can start a dependency, resolve an
interaction, grant authority or terminalise a Flow without the independently typed result/receipt required
by its state machine.

Agent control uses a local typed endpoint or CLI whose arguments map directly to protocol operations. It
returns structured receipts. Raw terminal escape sequences, prose, markdown links and provider output are
never interpreted as control messages.

### 5.3 Teams, messages and verification flows

A Team is Session-scoped; an AgentInstance may belong to zero or more Teams, and Team membership never
changes the member's primary tree row. It records members, roles and optional conductor/synthesiser. A verification Flow
can fan out independent reviewers, arm the synthesiser only after the declared results arrive, preserve each
review as evidence and route disagreements or incomplete proof to Attention. The UI renders this in the
Team/Flow Node View as activatable member references; it does not duplicate member rows or introduce a
canvas as a second source of topology.

`AgentMessage` is a bounded, typed instruction or status with sender, destination, purpose, expiry,
idempotency key and delivery evidence. Its state is one closed product of three explicitly independent axes:

Its canonical UTF-8 body is≤4 KiB. FIFO capacity is 256 items/1 MiB per destination, 16 unaccepted ad-hoc
drafts per source connection, 10,000 prepared-or-queued live bodies and32 MiB of body bytes installation-wide,
with a 64-MiB family working-set cap including queue/encoder overhead. All bytes also charge
`runtime.turn_variable_rss_mib`; pre-submission TTL is≤600 seconds and admission reserves one of 100,000
body-free terminal metadata/replay slots retained 30 days. Item/destination/global/family/shared/terminal N+1
refuses before endpoint effect.

```text
BodyAuthority = AdHoc(body=live|consumed|lost, review=pending|reviewed|review_required)
              | FlowRecipe(policy_revision, body=reassemblable|consumed|lost,
                           review=preauthorised|review_required)
Transport     = prepared | queued | submitted | submitted_unconfirmed | refused | failed | expired
Evidence      = { received?: EvidenceFact, read?: EvidenceFact, acted?: EvidenceFact }
```

Only these transport transitions are legal:

| From | May transition to | Required fact |
| --- | --- | --- |
| `prepared` | `queued`, `refused`, `failed`, `expired` | current destination/generation, capacity, TTL and authority revalidation before queueing; otherwise a definite pre-write result |
| `queued` | `submitted`, `submitted_unconfirmed`, `refused`, `failed`, `expired` | structured endpoint write receipt, or a precise definite/ambiguous write boundary |
| `submitted_unconfirmed` | `submitted` only | independently correlated late evidence proves the original write; this is reconciliation, never another write |
| `submitted` | none | accepted transport write is immutable; later semantic evidence changes only `Evidence` |
| terminal (`refused`, `failed`, `expired`) | none | the operation id can never be retried |

An ad-hoc body can queue only as `live/reviewed`; queue acceptance atomically changes it to
`consumed/reviewed`. A Flow body can queue only as `reassemblable/preauthorised` under the exact still-current
policy and becomes `consumed/preauthorised`. A client or daemon loss before any possible write changes an
ad-hoc draft to `lost/review_required/failed(reason=body_lost)`; loss of an accepted queued ad-hoc body before
any possible write preserves `consumed/reviewed` and terminates as `failed(reason=queue_body_lost)`. A queued
Flow operation whose ephemeral assembled body is lost before any possible write becomes
`lost/review_required/failed(reason=policy_reassembly_required)`. Its immutable recipe may be reassembled
only after policy, destination and grant revalidation as a new message and operation id. An invalid recipe
instead becomes `lost/review_required/failed(reason=policy_invalid)`. In every failure the old operation is
terminal. Loss after a write may have started yields only `submitted_unconfirmed`, never a retry or an
inferred failure.

The valid cross-axis combinations are closed; anything else is rejected on decode/store migration:

| BodyAuthority | Transport | Evidence |
| --- | --- | --- |
| ad-hoc `live/pending` | `prepared` | empty |
| ad-hoc `live/reviewed` | `prepared|refused|failed|expired` | empty |
| ad-hoc `consumed/reviewed` | `queued|submitted|submitted_unconfirmed|refused|failed(reason=queue_body_lost)|expired` | empty unless transport is submitted or unconfirmed |
| ad-hoc `lost/review_required` | `failed(reason=body_lost)` | empty |
| Flow `reassemblable/preauthorised` | `prepared|refused|failed|expired` | empty |
| Flow `consumed/preauthorised` | `queued|submitted|submitted_unconfirmed|refused|failed|expired` | empty unless transport is submitted or unconfirmed |
| Flow `lost/review_required` | `failed(reason=policy_invalid|policy_reassembly_required)` | empty |

`received`, `read` and `acted` are monotonic optional facts with independent source/revision/timestamps.
Observing one never fabricates either of the others, so all eight evidence combinations are representable.
TTL applies before submission and never rewinds a submitted message or its evidence. Queue acceptance moves
the body+existing reservation from connection+Surface ownership to `(daemon generation, Workspace,
AgentMessageDeliveryId,destination instance/current-attempt generation)`; source disconnect then cannot release
it. Definite pre-write terminal/expiry releases only after queue/encoder/transport buffers are quiescent;
possible write remains charged until the same proof, and daemon death proves memory reclamation before the
durable recovery reducer runs. Per-destination FIFO capacity and byte/count budgets are exact; overflow refuses visibly rather than dropping. Delivery
requires a verified compatible structured endpoint, current generation and no pending question/permission
or human input draft. Generic PTY injection is not a message transport.

Writable parallel members receive dedicated worktrees. The primary checkout and its `main` branch remain
free for the operator. Worktree creation, branch naming, cleanup eligibility and merge readiness are durable
receipts; deleting a Turn node never deletes a worktree or branch without a separately authorised operation.

Turn-managed fan-out is asynchronous: its create/control call returns after durable receipts and Turn never
injects a synchronous child join. The daemon/UI loops, terminal input, hierarchy navigation and later
control operations remain within their budgets. A provider CLI may independently choose to wait for its own
child; Turn reports that as provider state and does not claim to control it. A child's failure, cancellation or
disconnect neither terminates nor changes its parent to awaiting-operator unless independent evidence says
the parent itself needs the operator. Dependencies are evaluated from the event queue rather than by a
synchronous wait in the UI or parent runtime.

## 6. Provider-neutral agent topology

Every dedicated adapter translates native provider evidence into the canonical two-dimensional state rather
than inventing a third state machine. `Lifecycle` is `Spawning|Alive|Reconnected|Orphaned|Lost|Exited|
Signaled|Stopped`; `TurnState` is `Idle|Active|AwaitingUser|Done|TaskDone|Failed|Unknown`. The axes are reduced
independently: provider “waiting” cannot establish process liveness, provider “completed” cannot establish
process exit, and an OS exit cannot establish a successful turn result. Native labels remain in provenance.

Every adapter ships a versioned, total `NativeStateMap`: each native value maps to one canonical value, an
explicit no-change, or `Unknown`; undeclared values are rejected as a capability/version mismatch rather
than guessed. These evidence classes are the cross-provider canonical mapping:

| Evidence class | Lifecycle reduction | TurnState reduction |
| --- | --- | --- |
| launch intent committed, no contrary effect receipt | `Spawning` | no change |
| verified live process/runtime heartbeat | `Alive` | no change |
| proved durable PTY/runtime reattachment after orphaning | `Reconnected` | no change |
| daemon ownership lost while the process may still live | `Orphaned` | no change |
| bounded reconciliation proves the process/runtime absent | `Lost` | no change |
| exit/signal/operator-stop receipt | `Exited` / `Signaled` / `Stopped` | no change |
| provider ready with no active turn | no change | `Idle` |
| provider turn/tool activity | no change | `Active` |
| typed question/decision/permission awaiting the human | no change | `AwaitingUser` |
| turn result, task result or failure for an exact turn revision | no change | `Done` / `TaskDone` / `Failed` |
| unsupported, stale, contradictory or unparseable evidence | no change | `Unknown` only for the affected turn claim |

Legal Lifecycle transitions are closed:

| From | May transition to |
| --- | --- |
| `Spawning` | `Alive`, `Orphaned`, `Lost`, `Exited`, `Signaled`, `Stopped` |
| `Alive` | `Orphaned`, `Lost`, `Exited`, `Signaled`, `Stopped` |
| `Reconnected` | `Alive`, `Orphaned`, `Lost`, `Exited`, `Signaled`, `Stopped` |
| `Orphaned` | `Reconnected`, `Lost`, `Exited`, `Signaled`, `Stopped` |
| terminal (`Lost`, `Exited`, `Signaled`, `Stopped`) | none for the same RuntimeAttempt |

A resume/restart after a terminal Lifecycle creates a new RuntimeAttempt. Legal TurnState transitions are:

| From | May transition to |
| --- | --- |
| `Unknown` | any state proved by current evidence |
| `Idle` | `Active`, `AwaitingUser`, `Done`, `TaskDone`, `Failed`, `Unknown` |
| `Active` | `Idle`, `AwaitingUser`, `Done`, `TaskDone`, `Failed`, `Unknown` |
| `AwaitingUser` | `Idle`, `Active`, `Done`, `TaskDone`, `Failed`, `Unknown` after correlated resolution/cancellation/evidence |
| `Done` | `TaskDone` as a proved refinement of the same turn; `Idle|Active|AwaitingUser` only for a new `TurnId`; otherwise `Unknown` on evidence loss |
| `TaskDone`, `Failed` | `Unknown` on evidence loss, or `Idle|Active|AwaitingUser` only for a new `TurnId` |

The reducer first fences by exact instance/attempt/generation and native observation epoch, then compares
monotonic native revision/sequence only within the same source. It does not compare unrelated native counters
or apply one global source rank. Instead, each transition names its admissible evidence class: launch intents
establish only `Spawning`; later authenticated RuntimeBackend/process facts may causally advance it to a live
or terminal state; durable stop/kill receipts and backend exit facts arbitrate the terminal result. Turn facts
rank authenticated structured provider endpoint, trusted versioned hook, bounded transcript parser, then
PTY heuristic, but only for the same turn revision and claim. A lower-ranked source may fill an unknown claim
and may report a causally later revision, but cannot contradict a stronger fact for the same revision.
Conflicting comparable Turn facts at the strongest admissible class reduce TurnState to `Unknown`, record
both sources and schedule bounded reconciliation. Because Lifecycle has no `Unknown` value, a comparable
Lifecycle conflict retains the last accepted value, marks `lifecycle_evidence=conflict` and forbids any
destructive inference until reconciliation. A stale source revision, old epoch or illegal transition is
retained only as rejected provenance. Parent exit never terminalises a still-live child.

`TurnId` is a daemon-canonical opaque epoch for one provider turn, paired with monotonic `turn_revision`.
Adapters correlate native run/message/tool identifiers into that id and may not compare their native
counters with another source. `AwaitingUser` carries one exact pending interaction id; only a correlated
resolution, cancellation or attempt-terminal fact may leave it, to `Active|Idle|Done|TaskDone|Failed` for
the same TurnId. New work after `Done|TaskDone|Failed` requires a new TurnId even when the provider reuses a
conversation id.

`AgentTopologyObservation` is a versioned envelope with:

- `source_id`, adapter/provider version and a new `observation_epoch` after every registration/reconnect;
- parent AgentInstance/RuntimeAttempt/generation plus the covered provider/host/account scope;
- `snapshot_begin|snapshot_item|snapshot_end|delta|heartbeat|gap` kind, monotonic source sequence and the
  snapshot revision/watermark that an end closes;
- advertised coverage domain and metric set, plus `complete_snapshot|sequenced_complete|best_effort|
  gap_detected|unavailable` coverage;
- stable provider child id, child AgentInstance/RuntimeAttempt/generation when known, provider conversation
  id and causal invocation/operation id;
- child role/task/model/mode, canonical lifecycle/turn deltas and bounded activity/result facts;
- canonical `TurnId`, monotonic `turn_revision` and exact pending-interaction/result correlation whenever a
  TurnState fact is present;
- source, confidence, native revision, observation time and explicit unknown/unsupported fields.

A `snapshot_end` is valid only when it matches the same source, epoch, parent attempt/generation and begin,
declares its final sequence/watermark and item count, and no overflow/gap occurred. Each discovery source
declares whether its domain is authoritative or additive. A metric has complete coverage only when an
authoritative source closes the whole domain or every advertised additive domain that can contribute is
closed at the same reconciliation revision. Best-effort sources can add positive nodes but cannot prove
absence.

Ingestion is idempotent and tolerates duplicate, late and out-of-order events. A child completion cannot be
applied to another attempt that reused a provider id. Structured evidence reconciles a process-discovered
child without producing a duplicate. Reparenting requires stronger evidence and retains provenance. A
sequence gap, hook-channel drop, disconnected receiver, adapter restart, epoch/generation change, stale
heartbeat or invalid snapshot end immediately invalidates exact coverage for the affected scope. Topology
events remain non-blocking. The exact source+epoch queue is≤1,024 events and all queues share≤4,096 events/
≤4 KiB each/16 MiB. Reservation precedes delivery; any per-source/global/item/byte excess records one durable
per-source gap, invalidates exact coverage and schedules an asynchronous bounded resync. Drain, gap retirement
or owner/epoch loss releases only after apply quiescence. It never silently drops while preserving an exact
count and never blocks the Agent, terminal or UI.

`ObservedCountView` carries `metric: semantic_children|live_children|completed_children`, `scope:
direct|descendants`, `parent_scope: current_attempt|instance_lifetime`, `value:
exact(n)|lower_bound(n)|unknown|unsupported`, coverage, source-set/epochs,
snapshot watermark, graph revision, observed time, freshness, reason and remediation. `exact(0)` requires a
valid closed coverage set for that exact parent scope and metric, zero matching graph nodes and
no gap since its watermark. A best-effort or gapped source yields `lower_bound(n)` when positive nodes are
known and `unknown` otherwise. Workspace, Session and Agent aggregates are computed once from the same
revisioned graph; UI rows and tests do not recompute them. Their oracle is an independent expected event
fixture/snapshot manifest, never the query implementation under test.

The count predicates are normative. The semantic graph includes non-tombstoned `Agent|Subagent` Nodes joined
to exactly one AgentInstance and reached only through verified `SpawnEdge`s; ProcessEdges, Groups, Teams and
Flow references never add a semantic child. `current_attempt` accepts at each traversed parent only SpawnEdges
whose source attempt/generation is its active attempt; `instance_lifetime` accepts verified SpawnEdges from
any of that parent's retained attempts. `direct` is one accepted edge; `descendants` is the duplicate-free
transitive closure applying the same parent-scope rule at every level. `semantic_children` includes historical
non-deleted matches. `live_children` is the subset whose active (at most one non-terminal) attempt has proved
Lifecycle `Alive|Reconnected`; `TurnState` does not affect it. Any matching `Orphaned` or Lifecycle-conflict
child prevents an exact live count: known proved-live children yield a lower bound and none yield `unknown`
until reconciliation. `Spawning` is not yet proved live. `completed_children` is the subset
with an exact latest typed result or `Done|TaskDone|Failed` evidence for its latest turn revision, whether or
not it still has an active attempt, and may overlap live children. `Lost`, `Exited`,
`Signaled` or `Stopped` without result remains semantic but is neither live nor completed; a process exit alone
never manufactures completion. Active UI labels “direct”, “live” and “total descendants” map respectively to
`semantic_children/direct/instance_lifetime`, `live_children/descendants/current_attempt` and
`semantic_children/descendants/instance_lifetime`; retained
tombstones/history require a separate explicit history query.

### 6.1 Adapter capability contract

Claude Code, Codex, Gemini, OpenCode, GitHub Copilot, Grok and future/custom agents implement the same
capability vocabulary:

`launch`, `resume`, `branch`, `stop`, `structured_status`, `questions`, `permissions`, `subagents`,
`transcript`, `context_usage`, `provider_quota`, `model_switch`, `mode_switch`, `messaging`, `context_transfer`,
`shared_identity`, `durable_attach`, `delegated_control`, `native_jobs`, `conversation_inventory`,
`title_read`, `conversation_rename` and `model_gateway`.

Capabilities are evidence, not marketing labels or one global provider bit. Each fact is keyed by adapter
and CLI version, provider, account, ExecutionTarget/host, endpoint, AgentInstance/attempt/generation and
observation epoch where applicable; it names `supported|unsupported|degraded|unknown`, mechanism, limits,
freshness and expiry. Provider evidence capabilities, RuntimeBackend capabilities, broker/endpoint
capabilities and delegated authority remain separate records. An operation is enabled only by the
intersection of the relevant current facts plus an authority grant; no record widens another's scope. The
daemon invokes adapter methods through the registry and never hardcodes one provider after capability
selection. Core and UI contain no provider-name branches. A generic terminal adapter always exists, but it
advertises only what it can prove.

Every advertised capability has shared contract fixtures, provider-specific fixtures and degradation tests.
Capabilities dependent on credentials or a live service additionally require a recorded live smoke test
before the product labels them available. One provider's timeout or usage error does not block another.
The six named dedicated adapters run the same complete capability matrix against supported, unsupported,
degraded, stale and version-bound fixtures; none may be replaced by executable-name inference or the generic
terminal adapter while the product claims dedicated support. Kimi and MiniMax are permanently scoped as
first-class quota/activity connectors under the same AccountProfile contract, not launch adapters. Their
connector surface grants no launch, transcript, conversation or control capability.

The `permissions` fact additionally carries its own revision and the closed
`response_transport=typed(schema_id,schema_version,transport_generation)|verified_local_pty(encoder_id,
encoder_version,transport_generation)|none`. Only a fresh `supported+typed` fact enables typed permission
operations. Only a fresh `supported+verified_local_pty` fact enables the deterministic local-desktop PTY
fallback. `unsupported`, `degraded`, `unknown`, stale/expired or `none` enables neither semantic response
path; unrelated ordinary terminal use remains available under its own input-safety state. A generic/opaque
TUI cannot advertise either transport from prompt heuristics.

Each Agent View exposes an integration diagnostic: detected provider/CLI version, requested and achieved
integration level, installed event mechanisms, last valid/rejected observation, last successful invocation,
downgrade reason, freshness, a capability-specific self-test and redacted export. A label such as `inferred`
without the resulting observability limits and remediation is insufficient. A self-test is an explicit
foreground operation that previews network/quota/process consequences, uses a disposable identity/runtime,
never mutates the inspected Session or its hooks, has hard time/resource limits and produces a redacted
cleanup receipt. Read-only probes may run automatically only when their declared boundary is technically
enforced.

### 6.2 Shared provider runtimes

A `RuntimeEndpoint` may multiplex many AgentInstances and conversations over one provider service, but it
never becomes their semantic identity. `ProviderAccountScope` is the closed union
`profiled(AccountProfileId,revision)|endpoint_unscoped(UnscopedRuntimeScopeId)`. The daemon mints the opaque
unscoped id into one durable RuntimeEndpoint record and preserves it across app, multiplexer and endpoint
restart only when signed endpoint-continuity proof verifies. One endpoint admits≤64 non-retired bindings and
the next refuses before identity or authority mutation. Observing a candidate generation first stales the
exact old bindings; failed root/MAC/key/endpoint proof leaves them all stale and never mints a replacement
scope. After one canonical complete 1..64-claim batch authenticates at the root, each claim is validated
independently: a valid claim becomes current while an unavailable, replayed, stale or ownership-conflicted
claim remains stale with a closed reason and cannot block a valid sibling. The ordered result vector and all
root/per-binding replay high-waters commit with one inventory revision. A different endpoint
root/scope requires a separate foreground create operation. It prevents thread-id aliasing only inside that
exact provider/target/endpoint continuity root.
It creates no AccountProfile, credential, quota, activity, inventory or cross-endpoint authority and cannot be
silently converted to `profiled`.

`ConversationKey = (provider_id, ProviderAccountScope, ExecutionTargetId, provider_namespace,
normalized_provider_conversation_id)` is canonical across the whole installation. Each
`RuntimeEndpointBinding` names exactly one ConversationKey, endpoint generation,
AgentInstance, RuntimeAttempt and proof. The store permits at most one `current` owner of a ConversationKey
across every endpoint and at most one current binding for an instance; a second claim is rejected before
input, transcript or context authority is issued. Siblings never share input, transcript cursors, context
grants, quota attribution or Attention subjects merely because they share the service.

`BindingState` is independently closed: `proposed → current|refused`, `current → stale|unbound|retired`,
`stale → current|unbound|retired`, and `unbound → proposed|retired`; `refused|retired` are terminal for that
binding id. Endpoint mismatch, generation discontinuity, ownership conflict or stale proof changes only
BindingState and connectivity. It never changes RuntimeAttempt Lifecycle; `Lost` still requires separate
bounded absence evidence from the RuntimeBackend/provider.

Domain `attach_runtime_attempt` enumerates the endpoint's own conversation inventory, proves the exact binding for one pre-existing attempt and launches
nothing. A service reconnect preserves bindings only when endpoint fingerprint, generation continuity and
conversation ownership all verify. Canonical encoding, expiry/skew, non-exportable key epoch and strictly
increasing proof sequences are mandatory; duplicate/unknown fields, missing/extra claims, old epochs and root
replay fail before authority. Key rotation stales affected bindings until a new proof; key/broker loss is
explicit continuity-unavailable evidence and never a reminted scope. A
service crash/restart cannot merge siblings or silently cold-start them. Fallback to a dedicated runtime or
provider resume is an explicit per-instance operation that creates a new RuntimeAttempt and retains lineage.
Duplicate conversation claims, cross-scope handles, late events from an old endpoint generation and one
sibling attempting another's operation are refused and surfaced as scoped diagnostics/Attention when
actionable. Endpoint backpressure and failure are isolated so one conversation cannot block unrelated
instances or the hierarchy.

A capability-gated `ConversationInventory` is separate from live RuntimeInventory and requires
`ProviderAccountScope=profiled` plus its AccountProfile read grant. It queries one exact
provider/AccountProfile/ExecutionTarget/namespace and returns bounded pages of private current and historical
conversation descriptors: ConversationKey, provider title when `title_read` is supported, created/updated
time, native status, model/mode hints, ownership match, resumability, source revision, freshness and explicit
unknown fields. Search declares supported server/client predicates, normalisation, result and scan bounds;
cursor gaps, truncation, rate limiting and partial coverage can never prove absence. Results from different
profiles or targets are never coalesced, cached into another profile or exposed to a surface without its
read grant. Matching is exact-key first and otherwise advisory; title/text similarity never binds work.

Adopt is one local, capacity-reserved ownership transaction with caller-visible preassigned Node,
AgentInstance, endpoint-binding and receipt identities. It freezes the complete descriptor/source revision,
destination Session/tree, profile/grant/target/adapter/endpoint and global ownership-registry revisions, creates
one stopped Agent identity and emits zero provider request, launch or input. Incomplete coverage, stale evidence,
N+1 and concurrent ownership leave no partial Node or binding; duplicate operation bytes return the same
terminal receipt and reconciliation is lookup-only. The UI label “Resume conversation” is only a route to the
canonical `resume_agent_instance` operation for an already owned stopped Agent; an unowned row offers Adopt,
and adoption never auto-resumes.

An `endpoint_unscoped` binding is visible only through its exact RuntimeContinuityView and supports only
capability-gated operations on that already-bound thread; it cannot enumerate/adopt arbitrary conversations,
read quota/activity, resolve a credential or search outside the binding. Associating it with a profile is a
separate foreground rebind with endpoint/thread/profile proof, a new profiled ConversationKey and retained
lineage; default-account change and endpoint restart never perform that transition.

One connection runs≤4 inventory queries and the installation≤32, with one≤2-MiB raw-provider/sanitisation
buffer each/64 MiB family plus shared RSS reserved before read. A request-only page is≤500 safe descriptors,
≤2 KiB/item and≤1 MiB logical; a query scans≤10,000. Its authenticated≤512-byte cursor binds every provider/
Profile/Target/namespace generation, predicates/normalisation, source revision, page ordinal and predecessor
digest. Row501/next byte continues; oversize raw page, candidate10,001, stale cursor, rate limit or incomplete
cache is partial/gapped/unavailable, never exact zero. Completion/failure/cancel/30-second deadline/disconnect
releases raw bytes after I/O quiescence and the response page survives no request.

Private transcript **body** search (`PRD-OBS-012`) is a separate, explicitly enabled local-desktop service,
not a richer `ConversationInventory`. It builds one encrypted index for one exact profiled provider/target/
namespace from only adapter-declared, identity-pinned transcript sources. The same encrypted generation stores
postings and a final≤200-KiB normalised user/assistant segment tail per indexed document inside the existing
≤512-MiB profile/target and≤1-GiB installation bounds; it follows the same redaction, key-revocation and delete
cascade and is never a second cache. Results contain the exact
ConversationKey, source/index revision, bounded title/project label, timestamp and query-centred snippet, with
complete/partial/gapped/unavailable/disabled coverage. They never expose another profile or target, treat a
failed scan as empty, infer ownership/resumability from text, or become a ContextPacket. Disable/profile-
retire revokes the per-index encryption key before unlink and never changes provider transcripts.

Search is a navigation aid: choosing a hit reveals one canonical read-only historical-conversation ViewTarget
inside the same WorkSurface. It does not adopt, resume, start, bind, send input, create or resolve Attention,
and it never introduces a second tree or result window. Index refresh is no faster than five minutes, scans
≤10,000 documents, reads≤5 MiB and indexes≤200 KiB of one transcript tail; a page is≤20 hits/≤80 KiB. Exact
source/index generations and local Surface authority fence every query, and stale or partial coverage remains
visible beside the results. The daemon seals each result row to the query/page digest; selection revalidates
that seal, the exact ConversationKey and every profile/target/index/source generation, and reserves the
historical-view buffer, first≤64-KiB page and outbox/chunk capacity before atomically changing only the Surface
ViewTarget. Capacity N+1 leaves the prior Surface/query intact; after a successful CAS the reserved first page
returns in the same response, so no blank view or extra start action exists. A copied, forged, stale or
cross-profile row produces zero navigation or authority. Historical pages come only from that encrypted
segment tail and never reopen provider files. A historical or unsupported result remains viewable metadata
and cannot be fabricated as live. Conversation title read and
rename are separate capabilities: reading never implies write; rename uses an expected provider revision,
idempotent operation id and provider receipt, reports requested/effective title, and degrades independently
when unsupported, stale, rate-limited or ambiguous.

### 6.3 Provider-native jobs

A provider may expose scheduled, recurring or background work through `native_jobs`; this is never inferred
from terminal output and never conflated with Turn's `bounded_recurrence` Flow policy. `NativeJobKey =
(provider_id, AccountProfileId, ExecutionTargetId, provider_namespace, provider_job_id,
provider_job_incarnation)` has one installation-wide owner Job Node. Incarnation is a provider-stable
non-reused generation/tombstone identity; an adapter that cannot prove non-reuse must derive a stable
incarnation from authoritative provider evidence or report native-job identity unsupported.

`list_native_jobs.begin` has no caller scan id and mints one exact scan. Eight scans/connection and512 global
retain≤32-KiB generation/watermark/cursor metadata each/16 MiB for60 seconds idle. A page read separately
reserves≤4 provider buffers/connection and≤32 global,≤2 MiB/item/64 MiB family plus shared RSS for≤30 seconds
before read. Each request-only page is≤500 safe jobs,≤2 KiB/item and≤1 MiB logical; a scan observes≤10,000 and
uses a≤512-byte authenticated ordinal/predecessor cursor. The501st/next byte continues; oversize/raw/stale/
generation/rate failures gap rather than prove absence. Complete final page, gap, generation loss, disconnect
or TTL releases scan metadata; page completion/failure releases raw/result bytes and no live scan is evicted.

`adopt_native_job` projects a discovered exact-key observation into one destination Session/optional Group
without provider effect. It requires a complete current inventory/get revision and atomically CASes the global
key registry from unowned to one minted NodeId plus destination relationship. Same-operation replay returns
that owner; a competing Workspace receives the already-owned reference only when authorised, otherwise a
non-disclosing conflict. It refuses while any create intent is nonterminal in the same profile/namespace, so a
late create correlation cannot race an adoption into a second Node. Stale generation, changed payload or
cross-Workspace duplication emits zero provider request and zero second Node.

`create_native_job` first commits an installation-minted Job `NodeId`, `NativeJobCreationId` and
`NativeJobCreateIntent` under one exact destination Session and optional current Group before any provider
dispatch; its idempotent operation receipt always returns those reserved identities. The intent's closed
state is `prepared → dispatching|cancelled`, `dispatching →
bound|refused|reconcile_required`, and `reconcile_required → bound|not_created|reconcile_required`, with
`bound|refused|cancelled|not_created` terminal for that creation id. The correlated receipt atomically binds
the NativeJobKey to the same Node, so an uncertain create always has a visible WorkSurface/Attention route.
Only `cancel_native_job_creation` may perform `prepared → cancelled`; it is revision-fenced, refuses once
dispatch begins and has zero provider effect. The creation id is separate from NativeJobKey and is never
guessed from label, schedule or definition. An adapter may advertise `create` only when it declares
and proves `create_correlation=idempotency_key_lookup|provider_receipt_lookup`, where both modes expose a
side-effect-free query with exact applied/not-applied evidence; a write-only idempotency key is insufficient.
The resulting receipt maps that id exactly once to a NativeJobKey. `run_now` likewise requires a Turn-minted
`NativeJobInvocationId` and the same lookup-capable correlation that maps once to a NativeJobIterationKey; schedule or
concurrent-trigger timing is never identity. Timeout enters `reconcile_required(tagged_creation_or_invocation_id,
operation_id,possible_effect=create_job|create_iteration)` and queries only that correlation. No exact
lookup correlation means the affected mutation is unsupported, not a best-effort duplicate risk.

`NativeJobDefinitionSpec` is closed to `provider_template_ref(template_id,revision)` or
`reviewed_instruction(content_type,byte_count<=65536,sha256,private_bytes)`. Create/update freeze a
`NativeJobConfigurationIntent` containing the exact requested definition, schedule/time zone, model and safe
flags plus pre-operation effective revision. Each requested field has independent
`pending|accepted(provider_receipt,effective_hash?)|refused(reason)|uncertain(correlation)` state. The
JobNodeView separately exposes `NativeJobEffectiveConfigurationObservation`: last provider-proved definition,
schedule, model and flags with their own native values, revision and freshness, or `unavailable(reason,
last_hash?)`. A request for B never replaces proved A merely because it was sent; timeout, coercion and refusal
show A, requested B and its state independently until correlated provider evidence proves an effective value.
After restart, an accepted request may remain available as Turn's bounded requested record while provider
effective definition is honestly unavailable; it is never labelled provider-proved.

Turn may retain at most 64 KiB of the exact requested private definition under the Job/creation scope so the
View can explain the request. A provider-observed effective body uses the same 64 KiB cap; oversized/opaque
content records only unavailable reason plus safe byte-count/hash evidence. Bodies never enter logs,
diagnostics, terminal/context, broad exports or control decoding, and opaque provider payloads never become
Turn commands or context authority.

Schedule and execution are independent axes. `NativeJobScheduleState` is closed to
`scheduled|paused|completed|failed|cancelled|unknown`. `NativeJobIterationKey=(NativeJobKey,
normalized_provider_iteration_id)` is stable and revisioned; an adapter that cannot prove a stable iteration
id cannot advertise iteration control. `NativeJobIterationState` is closed to
`queued|running|succeeded|failed|cancelled|unknown`. Each iteration retains its native value and exhaustive
mapping, scheduled/started/finished times, bounded result/error metadata and optional exact
AgentInstance/RuntimeAttempt reference. Thus a paused schedule can truthfully retain one running iteration,
and `cancel_iteration` always fences one exact queued/running iteration key+revision.

The schedule reducer permits scheduled→paused/completed/failed/cancelled/unknown, paused→scheduled/completed/
failed/cancelled/unknown and unknown→any state; completed/failed/cancelled are terminal for one job incarnation.
The iteration reducer permits queued→running/succeeded/failed/cancelled/unknown, running→succeeded/failed/
cancelled/unknown and unknown→any state; succeeded/failed/cancelled are terminal for one iteration key. An
older observation never regresses a terminal. Safe result/error metadata is closed to status code, summary≤4
KiB, error code/message≤4 KiB, original byte count/hash and at most 16 inert references≤512 bytes each, for a
32 KiB total record; provider output/transcript bodies remain in separately authorised Resources/transcripts.
Oversize records set `truncated=true` with count/hash when proved and never spill raw bytes.

One NativeJob materialises at most 1,100 iteration keys: 1,000 `queued|running|unknown` active/control-capable
rows plus 100 newest unreferenced terminal rows. Every active row retains the exact key/revision needed by
`cancel_iteration`; terminal compaction cannot consume an active slot or make an admitted iteration
unaddressable. A Turn-initiated 1,001st active iteration refuses before provider effect. Unpausable external
iteration 1,001 marks coverage gapped and disables exact absence/control until a bounded rescan fits; it does
not overwrite an active key. Terminal row 101 compacts the oldest eligible unreferenced terminal row or emits
the same honest gap if none is eligible.

`NativeJobPage` carries NativeJobScanId, daemon scan ordinal, profile/target/adapter generations, fixed provider
snapshot watermark when supported, page sequence, predecessor-cursor digest, next cursor, terminal flag,
`complete|partial|gapped` coverage and freshness. Pages apply idempotently only inside one uninterrupted chain;
only the greatest started scan that reaches a complete terminal page may atomically replace inventory or prove
absence. A later-started scan fences every older page. Without a provider snapshot/watermark, a scan is never
complete for absence/deletion. Stable provider event id+revision may advance one exact key; an unversioned
webhook only triggers get/list. Ordering is `(target_generation,profile_revision,adapter_generation,
provider_revision_or_scan_ordinal,event_id)`, and stale pages/events cannot undo a terminal state/tombstone.

`NativeJobPresenceState=observed|stale|missing|provider_deleted` is independent from schedule/iteration and
from local `NativeJobProjectionState=visible|activity_hidden|forgotten`. Its closed projection reducer is
`visible → activity_hidden|forgotten`, `activity_hidden → visible|forgotten` and `forgotten → visible`.
`activity_hidden` suppresses only the optional local activity/unread card: the canonical Job row and
JobNodeView remain visible. `missing` requires complete fresh exact-key absence without a
Turn delete receipt or exact provider deletion tombstone. `provider_deleted` is terminal for one incarnation
and requires either a correlated Turn delete intent plus proved exact-key absence, or an authenticated stable
provider deletion event/tombstone naming that exact key/incarnation and revision. Partial/filtered/gapped
absence is only stale; generic complete absence without deletion evidence is missing. The presence reducer is
observed→stale/missing/provider_deleted, stale→observed/missing/provider_deleted and missing→observed/
provider_deleted; provider_deleted is terminal and reuse must mint another incarnation.

Every update, pause, resume, run-now, iteration-cancel and provider-delete owns a durable
`NativeJobMutationIntentId`. Its immutable record fixes operation id/fingerprint, exact tagged job/invocation/
iteration subject, requested configuration where applicable, expected revisions/generations,
`possible_effect=update_job|pause_job|resume_job|create_iteration|cancel_iteration|delete_job` and one proved
`correlation=idempotency_key_lookup|provider_receipt_lookup`. The closed reducer is
`prepared→dispatching|cancelled`, `dispatching→submitted|refused|reconcile_required`,
`submitted→resolved|reconcile_required`, and `reconcile_required→resolved|not_applied|reconcile_required`;
cancelled/refused/resolved/not_applied are terminal. The intent is persisted before dispatching; recovery from
dispatching is lookup-only and never repeats the provider call. `cancel_native_job_mutation` affects only
prepared. Conflicting definition/schedule/delete intents serialize by exact job revision, iteration cancels
serialize per iteration and multiple run-now intents remain independent through distinct InvocationIds.
Delete conflicts with every nonterminal intent. The Job derives a bounded sorted active/reconcile set rather
than collapsing several intents into one mutation flag.

Projection operations use `NativeJobProjectionSubject=job(NativeJobKey)|creation(NativeJobCreationId)` plus
NodeId, so refused/cancelled/not-created creates remain truthful local Views without inventing a key.
`forgotten` removes the row/View but retains NodeId plus the tagged key/creation visibility fence so sync or a
late receipt cannot create another Node. `delete_native_job_local_data` is a separate zero-provider-effect
privacy operation over that tagged subject and terminal evidence. It retains an installation-lifetime minimal
operation fingerprint/result fence and sets `privacy_suppressed`; background list/get may refresh only key,
presence and deletion evidence and cannot recache erased definition/iteration bodies until explicit Restore.

Forget atomically moves every already-live Attention for the subject to
the general `ProvisionalAttentionView` before removing the row, and its transaction serialises with new demand
creation. Thus Attention before/during/after forget preserves one immutable AttentionId and route. A later actionable provider observation routes to
the general `ProvisionalAttentionView`, keyed by immutable AttentionId plus owning Session, profile/target,
the tagged key/creation subject and observation revision; it may include NativeJobIterationKey or an input-owner reference only
when that exact relationship is proved. An explicit Restore projection action returns the same NodeId to
`visible`; it never auto-recreates a row, loses the demand
or treats dismiss as provider mutation.

Provider evidence, not app presence, determines whether a job or iteration survives Session end, daemon
restart, host reboot or provider disconnect. The closed normalized operation keys are `list`, `get`, `create`,
`update`, `pause`, `resume`, `run_now`, `cancel_iteration` and `delete_job`. `list` returns a bounded coverage/
freshness/cursor-bearing page and `get` the exact revisioned observation. Every mutation is independently
capability-gated with operation id, exact creation/job/iteration subject, definition/schedule fields where
applicable, expected revisions, profile/target generation, reserved intent capacity and lookup-capable
correlation; inability to reserve the durable intent/replay fence or to reconcile by lookup refuses before
provider effect. Durable receipts never conflate read results.
`reconcile_native_job_mutation` carries the original operation id and tagged creation/invocation/job/iteration
subject, reads only the advertised correlation mechanism and never redispatches. Separately,
`adopt_native_job`, `cancel_native_job_creation`, `cancel_native_job_mutation`,
`hide_native_job_activity`, `forget_native_job_projection`, `restore_native_job_projection` and
`delete_native_job_local_data` are local operations, fence their exact tagged subjects/revisions and have zero
provider effect.
Questions, permissions, failures and unread results route through the same Attention Queue to the Job Node's
exact iteration record and referenced attempt, or the exact provisional view above—not to another tree row. Import/export carries only inert
configuration without provider ids, active schedule or authority and requires local adoption.

Session/Workspace End or deletion never drops a Job, creation/mutation intent or Job Attention and never asks
the operator to solve a survivor plan. The daemon atomically applies one total
`NativeJobContainerDisposition=rehome(destination Session,optional Group)|delete_terminal_local_data|
recovery_inventory`: it rehomes each valid surviving/nonterminal subject, deletes only terminal local data
whose privacy preconditions already hold, and otherwise records the exact subject/last proof/desired cleanup
in the owning `WorkspaceSemanticRecoveryInventory`. Rehome preserves NodeId, key/creation ownership, receipts and reroutes
existing Attention in the same transaction; every disposition emits zero provider request. A late receipt/page/
demand resolves through the retained destination after the container row has gone, never an orphan or duplicate.
The same container preflight enumerates every nonterminal or uncertain MediaImport and its reserved Node/blob
identities. Each exact import revision is cancelled only when pre-effect cancellation is proved, rehomed with
unchanged reservation, or moved to the owning `WorkspaceSemanticRecoveryInventory` in reconcile-required state; no cleanup
failure can veto End/delete, strand temporary bytes, erase ambiguity or publish them implicitly.

That preflight also enumerates every container-referencing `RuntimeLifecycleIntent`/receipt and reserved
replacement `RuntimeLaunchIntent`, CommitProposal and linked installation-owned CommitProposalAttempt,
RepositoryPublishIntent, WebPreviewLoadIntent, BrowserNodeCreationIntent, BrowserNavigationIntent,
BrowserDownloadQuarantine, TransferTicket, PortableExport and Installation-owned PortableImport destination.
`ContainerSagaDisposition` is total per exact revision: `cancel_pre_effect|terminalise_no_effect|
rehome(exact compatible destination)|workspace_recovery|installation_recovery`. Pre-effect cancellation is
legal only with proved no effect. A generating Proposal and its Attempt keep their immutable link until the
Attempt terminalises, but lose editor-apply authority; Workspace deletion moves the Proposal evidence to
Installation recovery while the Attempt remains Installation-owned. A prepared publication cancels only with
no-effect proof; remote-created/remote-published/configuring/reconcile-required phase evidence retains exact
destination/object/ref/config/correlation identity in recovery, revokes the ended checkout lease and never
deletes provider data or resumes a later phase implicitly. A prepared WebPreview load cancels only with proved
zero request; fetching/reconcile-required/fetch-unconfirmed source, URL hash, policy and HTTP correlation
rehomes or enters semantic recovery. End immediately revokes all presentation authority; a proved-quiescent
allocation releases, while any possibly live socket/worker/buffer and its exact charge transfer atomically to
recovery-owned cleanup until quiescence instead of blocking row removal or escaping quota. Late evidence may
terminalise only that retained intent and can neither render into a deleted Session nor fetch again. A prepared Browser creation/navigation
intent cancels only with proved no effect; dispatching/reconcile-required or dispatched-unconfirmed identity,
partition, renderer token and possible-load evidence rehomes or enters semantic recovery before graph removal.
Late recovery may close the retained receipt but can never publish a Node into a deleted Session or redispatch a
load/history/stop effect. A receiving/sealed/transferring-
ownership/reconcile-required Browser quarantine keeps response identity, sealed descriptor/hash and shared
temporary-byte reservation until discard, ticket handoff, rehome or reconciliation. A transferring/reconcile-
required ticket keeps its temp-byte reservation, endpoints, chunk ledger and possible-publication proof until
rehome or reconciliation. A committing/reconcile-required export keeps exact output identity, and a committing import
keeps destination/remint evidence, in the applicable recovery inventory; neither rewrites/reimports. Before
commit, a deleted destination makes export/import stale or cancelled with zero effect. Each saga pre-reserves
its semantic recovery slot before effect, so full capacity cannot make End/delete refuse. No cancel, provider,
transfer, file cleanup or receipt delay can veto row removal; every late result resolves only through the
retained saga/tombstone identity.

Lifecycle and terminal-presentation state use the same total reducer. A prepared lifecycle intent cancels only
with proof that no signal, stop or launch escaped. Definite old-stop evidence fences any replacement that has
not taken effect; dispatching, possible-effect or reconcile-required stop/replacement evidence transfers the
pre-reserved identity and `ProcessCleanupCharge` to lookup-only recovery. A late receipt may refine only that
record; the ending tombstone prevents replacement publication, Node/Session resurrection and renewed input
authority. `TerminalWarmViewPark` expires, `TerminalWakeInputBuffer` is cancelled and wiped,
`TerminalOffscreenClientDetach` is fenced and Session input authority is revoked. A target-owned shadow
observer/background writer transfers unchanged with an exact live rehome/recovery survivor, or retires when
its runtime ends; neither path transfers wake bytes or the old input lease. Uncertain quiescence keeps its
original charge in recovery and cannot veto End/delete. The profile/target-owned encrypted
private transcript index survives unchanged; only Session/Surface query buffers, cursors and the historical
view are invalidated. This distinction prevents container deletion from silently becoming provider-data or
profile-index deletion.
Protocol maps `list` to `list_native_jobs`, `get` to `get_native_job`, `create` to `create_native_job`,
`update` to `update_native_job`, `pause` to `pause_native_job`, `resume` to `resume_native_job`, `run_now` to
`run_native_job_now`, `cancel_iteration` only to `cancel_native_job_iteration` and `delete_job` only to
`delete_native_job`; the local cancel/reconcile/projection operations are not adapter aliases. UI labels distinguish “List provider jobs”, “Get provider job”, “Create provider job”,
“Update provider job”, “Pause provider job”, “Resume provider job”, “Run provider job now”, “Cancel current
iteration” and “Delete provider job”.

### 6.4 Model endpoint routing

`ModelEndpointProfile` is a non-secret, revisioned routing object for a provider-compatible gateway or direct
endpoint. Its stable id is scoped to one ExecutionTarget and records display label, canonical HTTPS origin,
TLS/pin policy, supported wire protocols, bounded discovered model catalogue, provider/account eligibility,
health/freshness and only a
`CredentialReferenceKind=environment|os_keystore|target_host_agent|external_broker`. Raw API keys are write-only
at the secret broker boundary and never enter protocol reads, argv, durable environment values, logs,
diagnostics, exports or shared configuration. An environment reference exposes its variable name and
availability, never its value.

`ModelEndpointProfileState` is closed: `draft → validating|retired|deleted`, `validating → active|invalid|
retired`, `active → validating|degraded|retired`, `degraded → validating|active|retired`, `invalid →
validating|retired|deleted`, `retired → validating|deleted`; `deleted` is terminal with an id tombstone.
Create/update/validate/set-default/retire/delete are separate operation-idempotent, revision-fenced foreground
operations. Validation bounds redirects, response size/time/model count and rejects non-HTTPS endpoints,
userinfo, DNS rebinding, loopback/private/metadata destinations unless a target policy explicitly adopted the
exact origin. TLS or endpoint identity changes fail closed; health failure never silently routes to another
endpoint or native provider.

Launch preflight intersects adapter `model_gateway`, endpoint protocol, selected AccountProfile, target,
requested model and current credential reference. `LaunchSpec` freezes the requested endpoint/model while
`LaunchReceipt` records effective endpoint revision, wire route/model and redacted credential-reference kind.
Changing a profile/default affects only future attempts. Custom adapters inherit gateway mapping only through
declared base-adapter capability data, never a provider-name UI branch. Model discovery is untrusted data:
ids/labels are bounded and sanitised, cannot inject flags or environment, and an unavailable/partial catalogue
never proves that a model is absent.

## 7. Runtime lifecycle and continuity

Turn supports local and remote `ExecutionTarget`s through a `RuntimeBackend`. The first durable backend may
wrap an existing terminal multiplexer or another attachable session mechanism; the domain depends only on
typed create/attach/resize/input/signal/observe/close operations and a stable runtime handle. File and source-
control views use separate capability-gated `FileBackend` and `RepositoryBackend` contracts with typed
list/read/write/watch and status/diff/stage/unstage/commit/history/fetch/pull/push/branch/discard/worktree
operations. Every request carries target host, generation, confined root/repository identity, operation id
and expected revision; external effects return their own receipts. Runtime, file and repository capabilities
never substitute for one another.

`ExecutionTargetId` is a stable semantic id, never a hostname. Its descriptor contains kind
`local|ssh|custom`, display label, authenticated endpoint fingerprint, trust generation, path namespace,
backend/capability revisions, connectivity/freshness and non-secret credential reference.
`ExecutionTargetState` is closed: `proposed → probing|retired|deleted`, `probing → trust_pending|connected|
disconnected|mismatch|retired`, `trust_pending → connected|mismatch|retired`, `connected → disconnected|
mismatch|retired`, `disconnected → probing|connected|mismatch|retired`, `mismatch → trust_pending|retired`,
and `retired → probing|deleted`; `deleted` is terminal with an id tombstone. Create records inert endpoint
text; adopt/probe is bounded and non-mutating; foreground trust pins the proved fingerprint; rotation is a
separate consequence-labelled operation; retire prevents new launches while retaining survivors/evidence;
delete requires no active runtime, profile, job, Workspace default or audit reference. List/get are bounded
reads. Every mutator carries operation id, expected target/trust revision and returns a receipt; reconnect or
same-named host discovery never changes identity or trust implicitly.

ExecutionTargets are Installation-stream catalogue records with monotonic ids that are never reused. At most
256 non-deleted targets exist; create/adopt preassigns the id and reserves one of 10,000 nonterminal-or-
uncompacted operation/receipt slots before descriptor insertion, probe connection or trust/binding change.
N+1 target or operation admission refuses before local/external effect. Each mutator is a single durable
intent→local transaction/receipt state; same-operation lost-response replay returns that result, and a crash
before commit has no target/trust/binding effect. Probe connectivity is observational and never grants trust.
Terminal rich receipts compact after 180 days only after target-reference, trust-generation, operation replay
and descriptor-non-substitution fences are durable. Deleted target identity folds into the installation
monotonic high-water plus terminal descriptor fingerprint/trust fence; nonterminal/possible-effect evidence
never ages out and deletion cannot free a slot until every declared survivor/reference disposition completes.

Every Turn-managed repository/worktree writer is fenced by an Installation-owned `CheckoutFenceRegistry`.
Its key is the canonical repository identity plus canonical checkout/worktree identity and its value freezes
`CheckoutScopeId`, writer kind, operation id, target/trust generation, isolated-worktree generation, lease
generation and the exact primary/non-primary proof. Fence ids and lease generations come from one durable
monotonic high-water and are never reused. At most 100,000 live-or-uncompacted fence records consuming at
most 64 MiB and 100,000 Turn-owned checkout lock inodes may exist installation-wide. The daemon reserves a
record, bytes and lock-inode slot and persists `reserved` before creating a worktree, registering a writer or
acquiring its filesystem lock; N+1 refuses before any repository, worktree, process or lock effect.

The closed fence reducer is `reserved → held|released_no_effect|reconcile_required`, `held → releasing →
released|reconcile_required`, and `reconcile_required → held|released_no_effect|released|reconcile_required`.
Every uncertain value retains original operation, possible-effect class, canonical identities and expected
lock owner. Lookup-only reconciliation proves the exact worktree/process/lock identity and never creates,
deletes, kills or reacquires anything. A released rich record may compact after 180 days only after operation
replay, CheckoutScope/reference and generation-non-reuse fences are durable; reserved, held, releasing and
possible-effect records never age out. A lock inode is swept only after the registry has durably reached a
released terminal, an exclusive nonblocking acquisition proves that exact inode has no owner and fresh
inventory proves no registered writer; filename, pid or elapsed time alone can never authorise removal.

`CheckoutScope` owns worktree lifecycle independently of its optional Group projection. It is keyed by stable
`CheckoutScopeId`, Session, ExecutionTarget/trust generation, canonical repository identity, worktree identity,
branch/ref and creator provenance `turn_created|adopted`. Its closed state is:

```text
provisioning -> active | reconcile_required(origin=provisioning, last_proved=none,
                                             desired_terminal=none,
                                             possible_effect=created_or_adopted)
active -> missing | conflicted | unbinding | removing
missing | conflicted -> active | unbinding
unbinding -> unbound | reconcile_required(origin=unbinding,
                                           last_proved=active|missing|conflicted,
                                           desired_terminal=unbound,
                                           possible_effect=ownership_release)
removing -> removed | reconcile_required(origin=removing, last_proved=active,
                                         desired_terminal=removed,
                                         possible_effect=worktree_delete)
reconcile_required(origin=provisioning) -> active | unbound | reconcile_required
reconcile_required(origin=unbinding) -> unbound | reconcile_required
reconcile_required(origin=removing) -> removed | reconcile_required
unbound | removed -> terminal for that CheckoutScopeId
```

Every `reconcile_required` value also retains the original operation id and exact external-effect receipt.
Fresh complete identity-bound inventory may prove one listed transition only: provisioning absence with no
survivor reaches terminal `unbound`; a terminal unbind/remove desire never silently returns to the
last-proved operational state or changes from `removed` to `unbound`. A separate `CheckoutScopeBindingId`
relates at most one Group projection to a scope; its closed state is `proposed → current|refused|unbound`, `current →
stale|unbound`, `stale → current|unbound`, with `refused|unbound` terminal.
`unbind_group_checkout_scope` removes only that presentation/default relationship and never changes the
CheckoutScope state or worktree. The distinct `unbind_checkout_scope` releases Turn's scope ownership and
reaches terminal `unbound` while preserving the worktree; `remove_checkout_scope` is the only path that may
delete the proved worktree and reaches terminal `removed`. Only fresh `active → missing|conflicted` inventory
makes a current binding `stale`; scope unbind/remove compare-and-swaps the scope, GroupTree and binding revisions and terminalises any
`proposed|current|stale` binding as `unbound` in the same transaction, so no Group can retain a default to a released
or removed scope. One catalogue action may create/adopt the worktree, create its Session and
optional Group projection, then select it; a partial effect persists exact reconciliation receipts.

Deleting a Group that owns a projection is atomically the same binding-only release as
`unbind_group_checkout_scope`: the request fences GroupTree, Group, scope and binding revisions, moves any
`proposed|current|stale` binding to `unbound`, preserves the CheckoutScope/worktree and only then removes or
promotes the Group according to its declared disposition.

Repository inventory distinguishes a complete empty/list result from read failure, partial/gapped data and a
stale target generation. Only complete current evidence may declare a CheckoutScope missing—and atomically
make its current Group binding stale—or declare an unregistered
worktree orphan. Adopted scopes default to unbind, never disk/branch deletion. Remove requires fresh dirty,
unpublished, path-owner, repository and live-writer proof and rejects repository/home/filesystem-root or ancestor
targets. Merge and publish are separate foreground operations. Unbind releases dead cwd defaults but does not
stop or relabel a live runtime; moving a runtime to another CheckoutScope is an explicit migrate/relaunch. Every
local/remote action stays target-bound and main remains checked out and switchable in the operator checkout.

Each RuntimeBackend also exposes bounded, capability-declared inventory snapshots for its entire authenticated
ExecutionTarget, not only handles already linked to the current Workspace. `RuntimeInventoryObservation`
names target/fingerprint/generation, adapter/backend, snapshot epoch/sequence/watermark, stable handle,
process/session metadata safe to reveal, ownership match and freshness. A complete snapshot reconciles known
attempts and lists unmatched live handles in an installation-level Recovery View owned by the stable
ExecutionTarget and keyed by exact target+handle; a Workspace may show a filtered projection but is never
the candidate's owner. The route survives Workspace deletion, discloses a candidate only to a surface with
that target's inventory grant and does not invent a Session, Node, AgentInstance or parent. Partial/gapped
inventory can add candidates but
cannot prove absence. The operator may adopt one candidate into an explicit Session, ignore that exact
revision for a bounded time, or terminate that exact handle after a consequence preview. Adoption creates
the proper Node/owner/RuntimeAttempt with a receipt; termination revalidates target, generation and handle
and never broad-kills a host or same-named runtime. Runtimes surviving end/delete, daemon loss or a failed
attach remain visible here until reconciled.

The same target snapshot family supplies a `ResourceInventoryObservation`; it is an extension of
RuntimeInventory, not a second owner graph. `ResourceScopeKey = (ExecutionTargetId, target_generation)` and
each `RuntimeResourceRowKey = (ExecutionTargetId, target_generation, backend_handle, handle_generation)`.
The host observation carries physical memory total/available/used, swap total/free, measured pressure signals,
accounting method, observed time and `complete|partial|gapped|unavailable|unsupported|stale` coverage. Optional
facts stay absent when unmeasured; absence, collector error and a failed remote read never become zero. A
result distinguishes `measured_nonempty|measured_empty|unmeasured` explicitly.

PTY capacity is a distinct target-scoped ResourceInventory fact, not inferred from Turn's Session count. One
current `PtyCapacityObservation` names target/trust generation, devices used, ceiling, backend safe headroom,
measurement source/time, coverage and freshness. The target monitor samples at most once per minute. Exact
80-percent usage enters `elevated`; `used≥ceiling-required_headroom` enters `critical`; any missing, partial,
unsupported or older-than-two-minute fact is `unknown`, never healthy or zero. Level changes publish one
visible target status and elevated/critical publish one deduplicated resource-pressure Attention demand before
the first capacity-refused launch receipt; a held level reminds no more than once per five minutes. The spawn
preflight revalidates the target and fresh observation, refuses critical before opening a PTY and never kills,
detaches or reaps live/watched/unowned/tmux work as a pressure shortcut.

Automatic ceiling remediation is absent unless that RuntimeBackend exposes a closed privileged provider with
durable correlation, exact before/after reread, confined persistent configuration and rollback proof. One
foreground consequence review names target, measured ceiling/use, proposed ceiling, persistence across reboot,
fixed provider identity and rollback. The durable intent commits before elevation and accepts no caller shell,
argv, path, service label or config bytes; the OS privilege broker owns the secret prompt. Kernel change,
persistent write, verify and rollback are separately receipted phases. Cancellation is pre-dispatch only;
crash/lost reply performs lookup and reread, never repeats privilege. Partial or rollback-failed state remains
uncertain and Attention-routed. An unsupported target shows bounded target-specific manual guidance and no
misleading `Fix automatically` control.

Each process row uses a reuse-safe root identity `(target boot id, pid, process start time)`, bounded parent
edges, own RSS and deduplicated descendant RSS. It attributes to an exact RuntimeAttempt, Node and Session when
proved; ownership is `owned_current|owned_closed_session|unmatched_survivor|ambiguous`. A live process retained
by an ended/archived Session remains attributed to that closed owner instead of becoming a fabricated orphan.
Cycles, inaccessible processes, shared RuntimeEndpoints and overlapping trees are surfaced as partial/shared
buckets; they are never double-counted or split by guess. Session/Node/target aggregates name numerator,
denominator, coverage and revision, and remain locally/remote target-bound.

Observation never terminates work. `terminate_resource_owner` is a foreground consequence-labelled operation
that re-probes and revalidates target/trust generation, exact backend handle generation, process start identity
and expected resource observation before delegating to the existing exact RuntimeInventory termination path.
PID/name-only kills, broad host kills and remote-to-local fallback are forbidden. A late remote response is
discarded after target-generation change, and failure affects no sibling runtime.

File editing is an explicit FileBackend operation, not terminal keystroke synthesis. Open returns canonical
root-relative path, host/generation, file identity, byte/encoding bounds, content hash and revision. Save is
an atomic compare-and-swap against all of them; external changes yield a three-way conflict view and zero
overwrite. Root/descriptor jails reject absolute escape, symlink/hardlink/mount swaps and check/use races,
including on remote targets. Autosave is opt-in per file and obeys the same revision fence; binary,
oversized, unsupported-encoding, permission and offline cases remain read-only or refused with exact reason.
Resource Node edits do not mutate a file unless the operator invokes this FileBackend save operation.

Open mints a daemon-owned `FileEditSnapshotId` scoped to the authenticated connection and Surface; clients
cannot choose/reuse it. Before descriptor read, the daemon reserves one of 16 snapshots for that connection,
one of 128 installation-wide and the declared bytes against a 1,024-MiB memory-only aggregate; each snapshot
is≤8 MiB. N+1 count/bytes refuses without reading file content. State is `open→stale|closed`, `stale→closed`.
Exact editor/save activity refreshes a 60-minute idle deadline. Explicit `close_file_edit`, source/root/target
identity invalidation, Surface/connection loss or idle expiry closes and releases bytes; a reconnect cannot
inherit the id. An ordinary external content revision leaves the pinned base open so save can return its
three-way conflict, not false invalidation. Successful save atomically advances that same snapshot's base
identity/hash/revision; conflict leaves it unchanged. Snapshot bytes are memory-only and never journalled,
exported or restored.

Each save is one ExecutionTarget-owned `FileSaveIntentId` preassigned by the client with its operation id.
Before writing a byte, the daemon freezes the exact open-snapshot authority and before hash, intended bytes
and after hash, owner-only sibling temporary identity and replace policy, and reserves one active-intent slot,
the declared temporary bytes and one terminal receipt. Its closed reducer is:

```text
prepared -> writing_temp | refused | cancelled
writing_temp -> temp_sealed | failed_no_replace | reconcile_required(possible_temp)
temp_sealed -> replacing | cancelled | failed_no_replace
replacing -> applied | conflict_no_replace | failed_no_replace |
             reconcile_required(possible_replace)
reconcile_required -> applied | conflict_no_replace | failed_no_replace | reconcile_required
```

`temp_sealed` includes the exact descriptor identity, length and hash. Replace reopens and revalidates the
destination descriptor/root/mount plus before hash/revision, fsyncs the sealed temporary and performs one
atomic replace; it never truncates the destination in place. Lookup-only reconcile reads the exact destination
and temporary identities: intended after hash plus proved replacement identity reaches `applied`; unchanged
before identity with proved absent/unpublished temp reaches `failed_no_replace`; a different destination is
`conflict_no_replace`; incomplete evidence remains `reconcile_required`. It never writes, renames, deletes or
retries replacement. Same-operation replay returns this intent/receipt.

At most 256 nonterminal save intents and 2,048 MiB of combined save temporaries may exist installation-wide;
one file remains at most 8 MiB. The existing 50,000 terminal-receipt/180-day bounds are hard. Active/temp/
receipt capacity is reserved atomically before byte one, so N+1 refuses with no file or temporary effect.
Terminal richness compacts only after operation replay, file-generation and after-hash evidence is durable;
nonterminal/possible-replace evidence never ages out. Owner-local cleanup may unlink a temporary only after a
terminal no-replace/applied record and exact descriptor/hash/open-handle proof; age or filename is insufficient.

Lifecycle operations are explicit and idempotent:

- **Attach existing attempt** (`attach_runtime_attempt`) domain-binds one exact proved live/orphaned attempt,
  may promote only its proved binding/reconnected state and returns a durable attachment receipt; it never
  launches, stops or creates an attempt;
- **Attach view** (`attach_pane` plus automatic resync) binds a surface to that live attempt and changes no
  domain state or work; `detach_runtime_view` is its presentation-only inverse;
- **Resume** starts from an exact terminal/stopped attempt or the committed adoption receipt's explicit
  `no_prior_attempt`, proves the same provider conversation and creates the next or first attempt under the
  same instance without stopping live work;
- **Restart** applies to an exact live attempt: it stops that attempt once and creates one replacement. A
  generic Tool keeps its Node; an Agent keeps its Node/instance/conversation only with exact continuity proof,
  otherwise Restart refuses and offers separate Fresh Start. It never converts to Resume or Fresh Start;
- **Switch model/mode** records requested/effective configuration; it preserves an instance only when the
  provider proves conversation continuity and otherwise refuses while offering a separately explicit Branch/
  new-instance action; the switch operation itself never branches silently;
- **Branch** creates a new instance with lineage;
- **Interrupt** sends the runtime's non-terminal interrupt operation; **terminate** requests graceful process
  exit and reports still-running at timeout without escalation; **kill** is a separate reviewed forceful backend
  action and terminalises the attempt only with that evidence; subscriber/view detach is presentation-only;
- **Recycle** replaces runtime infrastructure while preserving Node/instance/conversation only through a
  proven durable attach/resume; otherwise it is refused and offers an explicit Fresh Start;
- **Destroy** fences the semantic Node/instance, revokes input/grants/context, removes its active row and
  writes a durable tombstone; process, worktree, branch and artifact cleanup remain separate dispositions;
- **End Session** first commits every required semantic survivor rehome/tombstone disposition, then removes
  the active navigation record authoritatively; later process/worktree/artifact survivors are reported
  separately and cannot restore it.

At restore, Turn first reattaches verified durable handles. It never starts work merely because metadata was
restored. Foreground Session selection may execute only the exact preflighted activation plan in §3.3;
selecting a child/resource/history result never starts work. A Flow may continue only from persisted policy
and receipts that explicitly authorised that step. Ambiguous writes, input or message delivery become
`submitted_unconfirmed` and are not replayed.

The recovery survivor matrix is normative; every cell is an independent reducer output. “Unchanged” means
the exact id, generation and last evidence are preserved, not that liveness is inferred. The semantic/runtime
half is:

A survivor reduction also closes every Session-scoped relationship explicitly. Cross-Session rehome atomically
terminalises current TeamMembership, FlowRun scheduling membership and DependencyEdge authority into
historical receipts, preserves their result/provenance evidence and revokes related grants before the Node
moves. It never retargets those immutable relations. Same-Session reparent may retain a relation only when
every immutable Session/FlowRun/Team/endpoint field still matches. A missing or stale relationship
disposition falls back to a terminal historical receipt in the applicable Workspace/Installation
SemanticRecoveryInventory; it never blocks End/delete.

| Event | Node | AgentInstance | provider conversation | RuntimeAttempt | OS process/runtime | PTY |
| --- | --- | --- | --- | --- | --- | --- |
| UI view reload, same daemon/surface generation | unchanged | unchanged | unchanged | unchanged | unchanged | reattach view only; bytes/process unchanged |
| client disconnect or replacement connection | unchanged | unchanged | unchanged | unchanged | unchanged | runtime stays; old Surface detaches, and selecting/approaching it makes the client automatically issue the explicit typed attach operation with no second user interaction |
| daemon restart | restore exact id | restore exact id | historical until revalidated | `Reconnected` only by proved handle, else `Orphaned` then bounded `Lost` | probe exact durable handle; never launch | reattach only by proved backend identity, else lost |
| owning shell exit/restart | retained | retained | historical | old attempt terminal; restart creates a new attempt | exact shell exits; descendants reduce independently | old PTY closed; restart allocates a new PTY |
| local host reboot | retained | retained | historical | local attempt `Orphaned` then `Lost`; remote attempt re-probed | local process absent; remote handle unknown until probe | local PTY lost; remote PTY detached until probe |
| remote disconnect | retained | retained | last binding stale | Lifecycle unchanged; independent connectivity becomes `Disconnected` and observability stale | unknown on pinned remote host; no local substitute | detached; never rebound by name alone |
| remote reconnect accepted | unchanged | unchanged | binding current only after endpoint proof | connectivity `Connected`; Lifecycle becomes `Reconnected` only if exact durable reattach resolves a prior orphan, otherwise retains its proved value | same remote identity verified | exact remote PTY reattached |
| remote reconnect refused/mismatched | unchanged | unchanged | remains stale/historical | connectivity remains `Disconnected|Unknown`; Lifecycle changes to `Lost` only after separate bounded absence proof | no process claim and no local fallback | remains detached/lost |
| topology/telemetry source loss | unchanged | unchanged | unchanged | unchanged; no lifecycle inference | unchanged; no exit inference | unchanged |
| End Session, after total survivor reduction | each destroyed subject gets a durable tombstone; each rehomed survivor keeps the exact NodeId, otherwise it remains reachable through `WorkspaceSemanticRecoveryInventory` after Session-scoped relations become historical receipts | retained historical/fenced, rehomed or recovery-inventoried with the Node | retained historical | exact stop, rehome or unreachable-survivor disposition | exact stop receipt or separately inventoried survivor | exact `ContainerSagaDisposition` terminal-close result, an existing detach replay fence, or separately inventoried survivor |
| Destroy Node, after total child/survivor reduction | target gets a durable tombstone; surviving children are independently rehomed or recovery-inventoried before parent removal and each relationship follows its exact same-Session-retain or terminal-history receipt | target retained historical/fenced; surviving children unchanged | retained historical | target stop disposition; child attempts reduce/rehome independently | exact stop receipt or separately inventoried survivor | exact `ContainerSagaDisposition` terminal-close result, an existing detach replay fence, or separately inventoried survivor |

The effect/authority half is:

| Event | durable receipts | ContextPacket delivery | AgentMessage | Attention | DelegationGrant | ContextLink | InputLease |
| --- | --- | --- | --- | --- | --- | --- | --- |
| UI view reload, same daemon/surface generation | unchanged | unchanged; never replay | unchanged; never replay | unchanged | unchanged | unchanged | unchanged; a replacement connection is instead a disconnect and expires it |
| client disconnect or replacement connection | unchanged | unsubmitted client-bound draft lost; accepted delivery unchanged | unsubmitted client-bound draft `failed(body_lost)`; accepted delivery unchanged | unchanged; deferred route to old generation invalid | durable grant unchanged; client-held exercise capability expires | durable link unchanged; client has no broker bearer | expired immediately |
| daemon restart | restore committed intents/results | proved submission stays; possible write `submitted_unconfirmed`; definitely pre-write ad-hoc draft `draft_lost`; a durable Flow recipe requires a new operation after reassembly | proved submission stays; possible write `submitted_unconfirmed`; prepared ad-hoc draft `failed(body_lost)`; queued ad-hoc `failed(queue_body_lost)`; queued Flow operation `failed(policy_reassembly_required)` and only a new operation may reassemble | restore exact unresolved entries/tombstones | durable grant record is retained but dispatch-ineligible; old bearer/generation revoked, and only explicit current-policy reissue can reactivate | retained but dispatch-ineligible; broker capability rotated after endpoint revalidation | expired |
| owning shell exit/restart | retain; queued effect to old attempt refused | target-old-attempt delivery fails or preserves already-submitted evidence | target-old-attempt queue refuses; submitted evidence remains | exact old-attempt interactions close with disposition | old-attempt grant revoked | old-attempt destination suspended | expired |
| local host reboot | retain; pending local effect classified uncertain/manual | local possible write fenced; no replay | local possible write fenced; no replay | restore and add exact recovery demand when actionable | local capability revoked | suspended pending explicit resume/new attempt | expired |
| remote disconnect | retain; unacknowledged remote effect scoped uncertain | definitely-not-started durable phase remains in its declared state with `dispatch_ineligible(disconnected)`; possible write is `submitted_unconfirmed`; no local fallback | `queued` remains `queued` with separate `dispatch_ineligible(disconnected)` only while its durable body and authority remain current; possible write is `submitted_unconfirmed`; otherwise it reaches its declared terminal failure | one deduplicated host-scoped recovery demand | retained but dispatch-ineligible, then revoked on generation change/expiry | retained but dispatch-ineligible until same host/generation revalidation | expired |
| remote reconnect accepted | append exact reattach/reconciliation receipt | continue only a proved not-started durable phase; never replay a possible write | a `queued` item may continue only if body/authority remained current and no write started; possible write stays fenced | same demand resolves only after exact recovery evidence | a retained grant may receive a new bearer only after policy/attempt revalidation | resume with a rotated bearer and same bounded generation lineage | a new lease must be acquired |
| remote reconnect refused/mismatched | append refusal/mismatch receipt | definitely-not-started phase reaches its declared failure; possible write remains fenced; no fallback or replay | queued item reaches `refused|failed` according to the exact mismatch; possible write remains `submitted_unconfirmed`; no fallback or replay | host-scoped demand remains at a newer revision | remains dispatch-ineligible or revoked | remains dispatch-ineligible or revoked | absent |
| topology/telemetry source loss | unrelated receipts unchanged; topology gap appended | unchanged | unchanged | only actionable observability failure creates one deduplicated demand | unchanged | unchanged | unchanged |
| End Session, after total survivor reduction | retain intents/results and artifact references | every queued/pre-write Flow delivery is invalidated and needs a new operation; possible writes reconcile without resend; only an explicitly reviewed ad-hoc delivery may survive when its exact instance, attempt and authority bindings remain unchanged | every queued/pre-write Flow message is invalidated and needs a new operation; possible writes reconcile without resend; only an explicitly reviewed ad-hoc message may survive when its exact endpoint and authority bindings remain unchanged | rehome unresolved entries/routes atomically or route them to the exact tombstone/Recovery view; close only destroyed-subject interactions | revoke every immutable Session-scoped grant; destination use requires a newly issued grant | revoke every Session-scoped link; destination use requires a newly reviewed link | expired and reacquired explicitly after any rehome |
| Destroy Node, after total child/survivor reduction | retain target and child intents/results/artifact references | target queued/pre-write refused and possible write reconciled; a rehomed child's delivery survives only when every immutable source, destination, instance, attempt, scope and authority field still matches | same split and exact-field test | close target interactions; atomically rehome or recovery-route each surviving child's exact unresolved entry | target grant revoked; a child grant is retained only for same-Session reparenting when every immutable field still matches, otherwise newly issued | target link revoked; a child link is retained only for same-Session reparenting when every immutable field still matches, otherwise newly reviewed | target expired; child lease independently reduced |

These two tables are the test generator input. No implementation may collapse columns, copy a result from a
neighbouring entity or interpret a blank cell; adding an event or entity requires a new fully populated row or
column in the same revision.

Remote targets pin host identity, authenticated generation, path namespace, capability set and connection
state. Transport provides authenticated encryption and replay protection: SSH uses pinned host keys plus
explicit rotation, or another backend supplies mutually authenticated equivalent guarantees. Credentials are
OS-keystore/agent references rather than portable secrets, never enter logs/receipts, and revocation closes
new operations immediately. MITM, stale-key, replay and rotation races fail before effects. A remote outage
preserves semantic state and raises scoped Attention; it cannot fall back to local execution or resolve a
same-named local file. Context, telemetry, quota and cleanup observations retain the host/account scope that
produced them.

No target operation creates a `MainCheckout` worker. Create, Template, Flow, add-node/pane, activate,
restore, resume, restart, recycle, model switch and branch all choose enforced read-only inspection or a
fresh isolated worktree before launch. The v4 `main_checkout` mode and write lease are migration-only input:
new requests are refused; stopped legacy Sessions convert to read-only or a worktree; a live legacy worker is
quarantined as `migration_required`, can receive no new input/effects, and must be explicitly stopped or
recreated in isolation before the migration can complete. A release cannot claim compliance while such a
survivor exists. Every managed local/remote descendant also runs inside a fail-closed filesystem-write policy
that denies the canonical primary working tree, its aliases/mounts and primary index/lock paths; write access
is allowlisted to its own worktree and declared non-primary scratch/backend roots. Cwd validation alone is
insufficient. Attach/adopt is refused as managed writable work unless that containment can be proved; an
uncontained external process may remain an explicitly unmanaged observation but receives no Turn input,
control or compliance claim. The primary path and `main` branch are never registered by a secondary worktree.
The post-migration scan permits only technically enforced read-only inspection and requires zero write leases,
unguarded/write-capable primary-path processes and secondary registrations/locks of `main`.

## 8. Context, handoff and coordination

The complete authority and retention contract remains in `docs/AGENT_NODE_VIEWS_AND_CONTEXT.md`; this section
adds the end-to-end operational requirements.

`ContextLink` grants one destination instance bounded pull access to named sources with scope, expiry,
revocation and read audit. A source is a tagged `AgentInstanceSource` or `ResourceSource` naming an exact
`Note` Resource Node. Resource links default to one pinned Note revision. A reviewed
`follow_reviewed_revisions` policy may instead expose later revisions only from its exact author/grant set,
schema, cumulative revision/byte/token budgets and expiry; every pull records the revision actually disclosed.
Changing the Note never expands link scope, resets budget or silently authorises another resource, and
revocation fences the next read. File/Diff/WebPreview/Media resources require a ContextPacket or a separately
specified future source capability; they cannot masquerade as a Note. The Note View lists current consumers,
remaining budgets and stale/revoked state without exposing their bearer.

ContextLink and broker admission is closed:≤64 live links at either endpoint and≤10,000 active links
installation-wide. One current destination attempt receives one memory-only≤4-KiB bearer, with≤10,000/
32 MiB aggregate. One non-streaming read is≤1 MiB and≤30 seconds; at most four buffers belong to one
destination attempt,16 to one Link and256/256 MiB installation-wide. Every bearer/buffer charges shared
variable RSS before source open/helper dispatch. Pre-commit revoke/failure/expiry/attempt end releases after
I/O quiescence; a committed disclosure keeps its cumulative budget charge even if disconnected, but its body
releases after the sole destination write quiesces. Every N+1 has zero source/helper/budget effect.

`ContextPacket` is a one-shot portable handoff. It records source/target, lineage, selection, budget,
redaction/review state, content hash and delivery evidence without treating submission as receipt.

One canonical UTF-8 body is≤1 MiB and one inert rendered review≤1 MiB, reserving≤2 MiB/packet. One source
connection holds at most 16 unaccepted ad-hoc drafts; the installation holds 128 live draft-or-accepted bodies
and 256 MiB of packet working sets, also charged to `runtime.turn_variable_rss_mib`. Pre-submission TTL is
≤600 seconds and the 10,000 body-free metadata/replay slots reserve before preparation. Acceptance atomically
moves body+count/family/shared reservation from connection+Surface to `(daemon generation, owning Workspace,
ContextPacketDeliveryId,target generation)`, so source disconnect cannot release an accepted saga. Definite
pre-write terminal/expiry releases only after encoder/write-buffer quiescence; possible write stays charged
until quiescence, while daemon death proves memory reclamation and only then runs evidence-only recovery.
Item/count/family/shared/metadata N+1 refuses before source read/assembly/provision/launch/grant/write.

Portable handoff assembly is target-aware. It prefers structured provider exports, then verified terminal
or artifact evidence. When content exceeds the target budget it includes a digest of older material, a
recent verbatim tail and bounded references to a complete local artifact that the destination may read only
when separately authorised. It never floods a destination into immediate compaction or silently truncates
the newest decisions.

Only a foreground operator can issue, expand or renew root context authority. That act may issue a
`DelegationGrant` whose immutable Flow revision pre-authorises exact source kinds/ids, destination roles,
transforms, redaction policy, cumulative request/byte/token budgets and expiry. An agent exercising that
typed capability may create only the declared ContextLinks/Packets; the agent does not authorise them and
cannot widen scope. Every exercise records the originating operator operation, grant/generation and receipt.
Outside that authority, Turn shows one consolidated review rather than a series of prompts. A one-off draft
lost before submission requires a new review; a Flow may deterministically reassemble a definitely-not-started
packet from its still-valid policy, while an ambiguous submission remains fenced and is never replayed.
Native provider branching is used when it can prove lineage; otherwise the UI says portable handoff and does
not pretend conversation continuity.

Live ContextLinks never cross a Workspace. A ContextPacket may cross only through §14 portable export/import:
the package remints semantic ids, carries no runtime/bearer/grant/machine authority and remains inert after
import. Delivery then requires a fresh exact destination, budget, retention disclosure and operator review or
a new locally issued Flow grant; imported approval cannot authorise it.

Context acquisition and delivery are hostile-input boundaries. Local and remote reads use descriptor/root
jails with no symlink, hardlink, mount or check/use escape; remote channels authenticate the pinned target,
encrypt content and reject replay. Canonical framing marks handoff text/data as untrusted and non-executable.
Control/newline/invisible characters are normalised or rejected as declared, known-secret canaries are
redacted before review, and the final immutable body hash covers framing, selection and omissions. Tests put
canaries in paths, files, transcripts, environment, diagnostics and remote responses and prove absence from
unauthorised packets, stores and logs.

Context links, packets, messages, dependencies and process ancestry never imply one another. A dependency is
satisfied only by a typed durable result, not by idle status. Context or message content cannot approve a
permission or become a control operation.

## 9. Runtime truth and telemetry

Agent and Tool views distinguish five classes of observation:

1. **launch truth:** requested and effective provider, executable, model, account, modes, safe flags, cwd,
   host, worktree and adapter capabilities;
2. **conversation context:** consumed/remaining tokens or percentage for one conversation/attempt;
3. **provider quota:** one or more account/provider/host time windows with reset times;
4. **runtime resources:** pid/runtime handle, CPU, memory, child count, output rate and pressure state;
5. **work progress:** lifecycle, turn, current task/tool, elapsed/status age and unread result revision.

Every value carries scope, source, confidence, observed time, freshness and error state. Context utilisation
is never labelled quota. A shared provider allowance is never attributed to one Agent. Unknown,
unsupported, stale, rate-limited and fetch-failed are visibly different. Safe flag names and typed modes may
be shown; credential values, raw environment values and unredacted command secrets may not.

Usage collection is independent per provider/account/remote host, bounded and cache-aware. Slow or failed
providers do not delay the selected Node View or hide fresh values from others. Expensive network/subprocess
collection occurs on demand and at a bounded cadence while relevant views or policies subscribe.

Display naming is local metadata, never identity or provider authority. `DisplayNameFact` records Node/Group
and source revision, source `declared|structured_task|provider_observed|generated|operator_alias|fallback`,
confidence, observed time and a bounded sanitised label. `NameMode` is `follow_source|pinned`; an operator edit
or explicit `apply_name_proposal` pins the local alias until the operator unpins it, so reconnect, provider
title change and later generated output cannot overwrite it. Provider `conversation_rename` remains a separate
operation and local rename sends no provider command or terminal bytes.

Provider rename uses an ExecutionTarget-owned `ConversationRenameIntentId`, not a best-effort title write.
The requested title is a sanitised single line of at most 512 UTF-8 bytes and 200 Unicode scalars. The intent
freezes operation/fingerprint, ConversationKey, profile/target/adapter/capability generations, expected
provider-title revision, requested title/hash, provider correlation and a tagged subject proof:
`owned(AgentInstanceId,current optional RuntimeAttemptId,binding+ownership generations)` or
`unowned(global ownership-registry revision,exact ConversationInventory observation)`. If a current owner
exists, unowned is refused; an owned conversation need not have a live attempt.

Its reducer is `prepared→dispatching|cancelled`, `dispatching→submitted|refused|reconcile_required`,
`submitted→resolved|reconcile_required`, and
`reconcile_required→resolved|not_applied|reconcile_required`; terminals never reactivate. Cancel is prepared-
only. `resolved` requires a correlated provider receipt containing effective title and new provider revision;
same-title observation, temporal proximity or inventory refresh never proves correlation. Lookup-only
reconcile never renames again. Uncertainty retains the last-proved provider title and changes neither local/
pinned alias nor effective provider display. The adapter advertises `conversation_rename=supported` only when
it has exact idempotency plus receipt lookup; otherwise the operator may choose the separate local-alias action.

At most one rename is nonterminal per ConversationKey and 10,000 nonterminal-or-uncompacted intents, each≤4
KiB/64 MiB aggregate, exist installation-wide. Intent/terminal/recovery capacity reserves before provider
dispatch; N+1 has zero provider/alias effect. Terminal richness compacts after 180 days only after minimal
operation/key/correlation/result replay fences persist; nonterminal/possible-effect evidence never ages out.

`NameProposalId` binds the captured bounded source bytes/hash, target scope, Node/Group revision, generator
identity/model, redaction policy and expiry. Proposals are on-demand unless a reviewed local policy enables
bounded generation, use target-aware source acquisition, never send raw remote output to an undeclared local
or network generator, and cannot carry controls, bidi/invisible injection, paths, secrets or multiline text.
Applying a stale proposal fails without changing the current label. Group proposals use bounded member
summaries rather than concatenated transcripts; same-cwd or same-title nodes remain independently keyed.

An `AccountProfile` is a non-secret identity scoped to provider plus ExecutionTarget and backed by an
isolated provider config/auth home or OS-keystore/agent reference. Foreground operations create, adopt,
launch the provider's external authentication flow, validate, rename, retire and delete a profile; Turn
never asks for or stores the credential itself. A creation-category catalogue entry chooses an account with fixed precedence:
explicit launch input, immutable Flow/Template input, Workspace default, then target/provider default. The
preflight names the choice and a LaunchReceipt freezes the effective profile id. Changing a default affects
only future launches and never migrates, relabels or shares an active instance.

`AccountProfileState` is closed and revisioned:

| From | May transition to |
| --- | --- |
| `draft` | `authenticating`, `validating`, `retired`, `deleted` |
| `authenticating` | `validating`, `auth_failed`, `retired` |
| `validating` | `active`, `auth_failed`, `expired`, `revoked`, `retired` |
| `active` | `validating`, `expired`, `revoked`, `retired` |
| `auth_failed` | `authenticating`, `validating`, `retired`, `deleted` |
| `expired` | `authenticating`, `validating`, `revoked`, `retired` |
| `revoked` | `authenticating`, `retired` |
| `retired` | `authenticating`, `deleted` |
| `deleted` | none; a permanent id tombstone remains |

Create allocates an empty isolated credential reference; adopt binds an exact existing provider config
reference into `draft` without reading credential bytes. Authentication is an Installation-owned
`AccountAuthenticationIntent` durable before any broker, helper or Browser launch. It freezes operation/
fingerprint, Profile/provider/target/trust/profile/config-reference generations, broker/policy revision, exact
provider correlation, origin state, expiry and `possible_effect=credential_reference_change`. At most one is
nonterminal per profile. Its reducer is:

```text
prepared -> dispatching | cancelled | expired
dispatching -> awaiting_provider | refused | reconcile_required
awaiting_provider -> authenticated | auth_failed | reconcile_required
reconcile_required -> authenticated | not_applied | auth_failed | reconcile_required
```

Terminals never reactivate. Timeout/crash after dispatch is reconcile-required, and recovery queries exact
provider/broker correlation without relaunching a flow or rewriting a credential reference. Correlated
`authenticated` atomically records the effective credential generation and moves the profile to `validating`;
only a separate validation may reach active. Ambiguity remains non-launch-eligible. Retire/revoke fences every
callback; late success cannot reactivate, and delete refuses a nonterminal/possible-effect intent. Cancel is
prepared-only. Same operation/fingerprint returns the same intent; changed fingerprint conflicts.

Before launch, a Turn-created private root reserves its remaining bytes under a broker-enforced 64-MiB per-root
and 2,048-MiB installation envelope. If the target/provider flow cannot confine writes and enforce that quota,
external authentication is unsupported rather than launched. Authentication admits 10,000 nonterminal-or-
uncompacted≤4-KiB intents/32 MiB installation-wide, with exactly8,192 maximum records at the independent
byte boundary while the count boundary uses smaller records, and terminal richness for 180 days; intent, terminal
receipt, root bytes and recovery capacity reserve atomically prelaunch. N+1 has zero broker/Browser/provider/
root effect. Nonterminal/possible-effect evidence never ages out; folding retains operation, profile/target/
credential generation and provider correlation high-water.

Validate records provider/account identity and capability evidence under its own operation id and exact
effective credential generation; rename changes only the display label by
compare-and-swap; retire removes launch/default eligibility but retains evidence; delete requires no active
attempt, current binding, default, grant, authentication intent or retained reference and never deletes provider-side data. Every
verb has its own operation id, expected profile/target generation and receipt. Only a current `active`
profile with proved isolation and required capability may become or remain a default; when it ceases to be
eligible, the default becomes explicitly unset and no other profile is silently selected.

Profiled bindings have independent transcript/cache roots, quota observations and revocation state. An
`endpoint_unscoped` binding has none of those AccountProfile-owned categories and cannot satisfy their
authority checks. A profile with active attempts or retained audit references may be retired but not destructively
deleted; deletion reports every blocker and never deletes provider-side data implicitly. Cross-profile
conversation attach, transcript/context read, quota attribution or credential/config-home reuse is refused.
For every Turn-managed child, multi-profile support requires a sandbox or provider broker that exposes only
its selected profile's auth/config material and denies sibling roots; when the target cannot enforce that
boundary, the adapter reports profile isolation unsupported and Turn refuses a concurrent cross-profile
launch rather than claiming confidentiality. This supported-flow guarantee does not prevent an unrelated
malicious same-uid process outside the sandbox from reading user-owned provider files. Default changes and
concurrent launches are revision-fenced, and a missing/expired profile fails that launch without falling back
to another account.

Desktop and companion usage surfaces consume the same bounded `AccountActivityProjection` keyed by exact
provider, AccountProfile and ExecutionTarget. It contains independently timestamped context-window
observations, each provider quota window/reset, and a cursor-bounded activity inbox of exact conversation,
job-iteration and Attention references. Every value preserves source, coverage, confidence, freshness and
`unknown|unsupported|stale|rate_limited|fetch_failed`; absence or a partial page never renders as zero usage,
zero remaining quota or an empty authoritative inbox. Caches and cursors never cross profiles. Companion
filter/read/dismiss operations mutate only their named projection or canonical Attention revision; they do
not acknowledge provider work, resolve a prompt or retire a job.

## 10. Attention owns operator interruption

Only actionable evidence enters one global logical Attention Queue. It admits200,000 active entries/768 MiB;
90,000 entries/351.5625 MiB are dedicated to the eight normal plus one observability-gap reservation for every one
of10,000 admitted live RuntimeAttempts, and110,000 count slots/416.4375 MiB remain for all other declared producers
and admission races. Reservation and later materialisation are one charge. Every entry carries one
`AttentionSubject` tagged union: `Exact { node_id, instance_id?, attempt_id?, generation?, demand_ref:
PendingInteraction(id, revision)|Result(id, revision)|Condition(kind, revision), verified_action_owner?,
view_target }`, `Provisional { authenticated_parent_or_external_scope,
evidence_revision }`, or `Unassigned { session_id, evidence_revision }`. Only `Exact` may contain an input or
action owner; when it does not, routing still opens that exact Node View with the action disabled and a
truthful owner-unavailable reason. Its type is one of permission, question, decision, failure, lost/disconnected, reviewable
result, resource pressure, quota policy or provisional evidence that requires confirmation. Normal
running/idle status, usage updates and informational telemetry remain in the tree/HUD `StatusProjection`;
they never enter `Next Attention` or change queue order.

The Installation-owned queue admits at most200,000 unresolved/snoozed/dismissible entries or768 MiB, each
entry≤4 KiB, plus200,000 terminal mutation/route receipt slots/768 MiB retained for180 days. Count and byte
caps are independently reachable—200,000 smaller entries or196,608 maximum entries—not simultaneously
misreported as the same boundary. An operation or producer that
can emit a new actionable demand reserves an entry and terminal receipt before it becomes effect-capable;
each current RuntimeAttempt reserves eight distinct PendingInteraction/demand slots plus one dedicated
observability-gap slot before spawn. Other asynchronous producers declare an equivalent finite reservation.
N+1 producer admission refuses before spawn/subscription/provider effect. Exact dedup may replace only the
same complete tagged subject+revision. Existing unresolved entries never compact or lose route identity;
terminal richness compacts only after subject resolution/tombstone, read/dismiss semantics and replay fences
are durable. A producer exceeding its declared simultaneous cardinality is backpressured; if its provider
cannot pause, the dedicated slot creates one exact actionable `producer_observability_gap`, preserves the
underlying terminal/provider evidence and disables typed response rather than dropping or inventing a prompt.

`PendingInteraction` is Workspace-owned,≤8 KiB safe metadata and closed to at most eight nonterminal records
per RuntimeAttempt,100,000 nonterminal records/768 MiB installation-wide. A dedicated80,000-slot/625-MiB
partition reserves all eight records for every one of 10,000 admitted live RuntimeAttempts; the remaining
20,000 count slots/143 MiB are independent headroom for other declared producers and admission races (18,304
additional records if every one is the full8 KiB). RuntimeAttempt admission
reserves those eight records and their Attention slots before spawn; later materialisation consumes each token
without a second charge or capacity check. A provider may replace only an identical
interaction revision; a distinct ninth uses the reserved gap path above. At most100,000 terminal interaction
receipts each≤4 KiB/384 MiB remain for180 days; count and bytes saturate independently, with exactly98,304
maximum receipts at the byte boundary and smaller receipts at the count boundary. Terminal compaction retains non-reused id, attempt/input-route, selected-option,
claim and replay fences. Nonterminal/claimed/submitted/possible-effect interactions never age out. Provider
completion, attempt end or explicit typed cancellation terminalises the exact record; a newer prompt never
silently overwrites an older live one.

The default ordering is safety-critical permissions/failures, explicit questions/decisions, completed unread
results, then actionable stalled/disconnected/pressure/quota conditions, with age and operator policy as
secondary inputs. Deduplication keys the complete tagged subject, its interaction/result/evidence revision
and demand kind. Ageing
guarantees eventual service inside a safety class without allowing low-severity items to outrank a new
critical permission. Policy resolves field-by-field `Global → Workspace → Template → Session`.
Snooze hides one entry until deadline or a materially newer revision; dismiss closes only that entry and does
not resolve the underlying interaction; mute suppresses automatic focus/notifications but not queue/badge;
focus cooldown never blocks manual routing. Unread is independent of lifecycle: a finished result can be
unread, and viewing it marks only that exact revision read.

`Next Attention`, badges, desktop/companion notifications and governor-approved automatic focus all call the
same daemon route. The route lands on the exact Node View and action. Navigating, rendering, marking read,
acknowledging and resolving are separate operations. A submitted response remains pending until adapter
evidence closes the interaction.

Structured explicit evidence may request focus subject to the global focus governor. Heuristics can create a
badge/provisional demand but never move focus or claim a typed question/permission without confirmation.
Typing, dragging, terminal alternate-screen input, modal work and voice capture/review defer automatic focus
without dropping or retargeting the entry. Manual navigation always wins.

When evidence has no safe Node or input owner, routing opens a `ProvisionalAttentionView` keyed by the demand
and evidence revision. It never invents a Node, borrows a sibling's input owner or guesses a destination.
Later binding creates a new route revision; stale views cannot submit against it.
It preserves the same `AttentionId`, atomically replaces the tagged subject/deduplication key and retires the
old route revision, so binding cannot duplicate the queue entry.

Compact system status, an optional HUD and authenticated remote/mobile companions are projections of the same
queue and revisions, never independent queues. A companion may submit only closed-schema actions its current
capability grants allow. Permission-response grants are immutable: widening requires revoking the old id and
issuing a new one. Issue/revoke and every credential or authority mutation require
`LocalDesktopForegroundAuthority`, the authenticated visible native desktop client that is still foreground
at the daemon's serial validation point. Voice capture/worker, hooks, NotificationHost, HUD/headless,
Companion, RemoteOperatorSurface, Browser and WebPreview never have that role. Only an exact typed permission
response may arrive remotely under the single-use grant defined below.

Background delivery is a projection of that queue through `NotificationEndpointId`, never another Attention
authority. `NotificationEndpointState` is closed: `reserved→active|retired|deleted`, `active→retired`,
`retired→deleted`; deleted is terminal. Endpoint ids come from one durable installation high-water, are never
reused and re-pairing always mints a new id. Retire prevents new delivery, atomically revokes every proposed/
active grant generation, expires its outbox, emits live tombstones and deletes only the local secret reference;
it changes no Attention or downstream device history. Delete is allowed only from retired after every pairing/
grant/delivery intent is terminal and retains id/generation/replay fences.

Pairing is one Installation-owned `NotificationPairingIntent` with operation/payload fingerprint, preassigned
EndpointId and initial DeliveryGrantId, endpoint/grant generations, exact endpoint-catalogue revision, peer
correlation, scope/classes/privacy/rate/batch policy and issue/expiry times. Its reducer is
`prepared→dispatching|cancelled|expired`, `dispatching→awaiting_peer|paired|refused|reconcile_required`,
`awaiting_peer→paired|refused|reconcile_required`, and
`reconcile_required→paired|not_paired|reconcile_required`; terminals never reactivate. Timeout/crash after
dispatch is always reconcile-required, never safe expiry. Prepared pairing expires exactly 600 seconds after
created_at; an awaiting-peer deadline at 600 seconds moves to reconcile-required. `paired` atomically activates the endpoint and
initial grant; lookup-only reconcile queries exact peer correlation and never pairs again. Cancel is legal only
from prepared. Cancelled/expired pre-dispatch proves no peer effect and atomically makes the reserved endpoint
a deleted tombstone plus initial grant expired; refused/not_paired similarly deletes the reserved endpoint and
makes the grant invalid. These terminal couplings release live endpoint/grant capacity without another user
action while retaining non-reused ids/correlation/replay evidence. At most one nonterminal pairing exists per preassigned endpoint.

A foreground-issued immutable `DeliveryGrant` binds one endpoint public key/token reference, device/profile,
allowed Workspaces/ExecutionTargets, event classes, privacy detail, rate/batch bounds, generation and expiry.
Its state is `proposed→active|invalid|revoked|expired`, `active→expired|invalid|revoked`; terminal states cannot
be reactivated. Each issue/regrant mints a globally non-reused GrantId and monotonic endpoint generation; at
most one equivalent scope fingerprint is active. Widening, rekey or policy change revokes the old id and issues
a new one. Tokens and private keys remain in the keystore/agent and are absent from UI reads, store exports,
logs and diagnostic payloads. Revocation or 401/403 invalidates only the exact grant generation.

At most 64 reserved/active/retired endpoints, 32 proposed-or-active grants per endpoint and 2,048 installation-
wide are admitted. Pair/issue/revoke/retire/delete share 10,000 nonterminal-or-uncompacted control records,
each≤4 KiB and≤64 MiB aggregate, with terminal richness retained 180 days. Before peer/gateway effect the
daemon reserves endpoint/grant/control/terminal-delivery/outbox bytes and semantic-recovery capacity; every
count/byte N+1 refuses pre-effect. Terminal compaction retains endpoint/grant high-water, scope fingerprint,
operation replay and peer correlation; nonterminal/possible-effect evidence never ages out.

`NotificationDeliveryState` is closed for one stable `NotificationDeliveryId`: `eligible → held_present|queued|superseded|
expired`, `held_present → queued|superseded|expired`, `queued → submitted|superseded|expired`, `submitted →
accepted|failed_retryable|failed_terminal|superseded|expired`, and `failed_retryable → queued|failed_terminal|
superseded|expired`. One delivery permits exactly eight total gateway submissions including the first; each
retry retains NotificationDeliveryId, increments the attempt counter and fixes jittered next-eligible time
with backoff capped at 15 minutes. A ninth submission is unrepresentable and exhaustion becomes failed_terminal. Gateway acceptance never means device delivery, reading or demand
resolution. `CollapseFamilyKey` includes endpoint, stable tagged Attention subject identity and demand kind;
`CollapseKey` adds the exact subject revision. A newer current revision supersedes older family
members, while different children or kinds never collapse by title. Outbox insertion and flush both revalidate
grant, current queue revision, resolution and presence. Batching, bounded retry/jitter, per-endpoint rate limit
and encrypted minimal payloads prevent transcript/path/command/secret disclosure. Delivery failure changes no Attention,
unread or runtime state. A deep link always resynchronises the authoritative queue and revalidates the exact
route before showing or acting; an offline/stale notification cannot submit.

The encrypted outbox admits at most 10,000 live deliveries and 16 MiB, each ciphertext≤16 KiB and≤24 hours.
Terminal delivery audit admits 100,000 records, each≤4 KiB and≤256 MiB aggregate, for seven days. Outbox and
terminal slots reserve together before eligibility; overflow records one bounded gap without changing
Attention and cannot silently evict a still-current higher-priority delivery. Terminal folding preserves
delivery id, endpoint/grant generation, collapse/retry and replay fences.

An optional live status stream uses `LiveStreamKey = (endpoint, AttentionSubject identity, attempt generation)`
and monotonic event revision. Start/update/end are collapse-aware, and an end or tombstone fences every late
tick so a resolved/deleted/ended subject cannot resurrect. Presence may hold an alert but never pauses the
authoritative stream; leaving presence releases only still-current queued demands.

`NotificationHostMode` accepts authenticated local owner-only or loopback observation input and makes outbound
HTTPS delivery only. It creates no public HTTP/WebSocket/UI listener, ignores configured public bind host/port
and exposes zero inbound network ports. The ordinary remote GUI remains a separate deployment. Packet/listener
tests, endpoint revocation during a batch, background/killed clients, duplicate replay, two same-titled
subagents, offline acceptance and late live ticks are normative failure oracles.

`CompanionAction` is closed to ten variants with one canonical mapping only:
`route_attention→route_attention`, `mark_result_read→mark_node_result_read`,
`acknowledge→acknowledge_attention`, `snooze→snooze_attention`, `dismiss→dismiss_attention`,
`submit_free_text_response→respond_to_agent_interaction`,
`submit_permission_response→submit_remote_permission_response`, `interrupt→interrupt_runtime_owner` and
`request_writer_lease→request_input_lease_handoff`, plus the deliberately narrow
`launch_allowlisted_agent→launch_companion_agent`. Unknown variants and aliases are denied.
Every action first carries one common `CompanionActionEnvelope`: stable action/operation id, issue and hard
expiry times, exact RemoteClientId/revision, RemoteSessionId/revision, surface/connection generation,
Workspace/Session scope and negotiated registry hash. It then carries every exact field/revision required by
the mapped canonical operation; expiry is at most 30 seconds. Before any canonical effect, the daemon reserves
nonterminal and terminal-receipt capacity and persists immutable
`CompanionActionIntent(action_id,operation_id,mapping,canonical_request_hash,principal/scope/revisions,expiry)`.
Its reducer is `prepared→dispatching|refused|expired`, `dispatching→submitted|reconcile_required`,
`submitted→resolved|reconcile_required`, and `reconcile_required→resolved|not_applied|reconcile_required`;
recovery looks up the canonical operation receipt and never redispatches. A `CompanionActionReceipt` projects
that intent and canonical outcome; identical replay returns it and a changed payload under that id is refused. Expired,
offline or old-session envelopes never queue for later replay.
Free text is only for a verified non-authorising question/decision. Interrupt names one exact AttemptOwner/
attempt/binding generation and never means stop-all. Writer-lease request creates a visible expiring handoff
proposal; it neither acquires a lease nor accepts bytes.

Companion launch does not accept a launch spec. A local foreground operator creates one≤24-hour grant over an
exact Workspace/Session/target/trust revision and≤32 immutable template+dedicated-adapter+AccountProfile+
optional-model+safe-cwd-root+read-only-or-new-isolated-worktree entries. The Companion chooses one entry only.
Command, env, flags, path, parent, account, model override, target and primary-checkout values are absent from
the action schema. Before checkout/process/graph effect, the daemon preassigns NodeId, AgentInstanceId,
RuntimeAttemptId and CheckoutScopeId and persists one intent. Canonical checkout/create/register receipts make
duplicate, disconnect, register/launch ordering and crash lookup-only; at most one ordinary hierarchy Node and
runtime appears. Revoke/expiry fences queued launches but does not kill an already registered agent. There is
no phone-owned project registry or hidden mirror.

Remote invitation, client and session authority is closed. `RemoteInvitationState=prepared|active|consumed|
expired|revoked|invalidated`, with prepared→active/revoked/expired/invalidated, active→consumed/revoked/expired/
invalidated and terminal states never reactivating. `RemoteClientState=connecting|active|disconnected|revoked|
expired` has only connecting→active/disconnected/revoked/expired, active→disconnected/revoked/expired and
disconnected→connecting/revoked/expired; revoked and expired are terminal. `RemoteSessionState=negotiating|
active|disconnected|revoked|expired` has only negotiating→active/disconnected/revoked/expired and active→
disconnected/revoked/expired; every destination other than active is terminal and reconnect mints a new
RemoteSessionId and connection generation. Revoking/expiring a client terminates every child session and
invalidates its grants, leases and subscriptions. Any child session transition from negotiating or active to
disconnected/revoked/expired—unless caused by a terminal-client cascade—invalidates only that session's
descendants and atomically moves an otherwise connecting/active reusable client to disconnected; it does not
revoke or expire the client, so a fresh device-authenticated open remains possible.
Only the local desktop may create/revoke invitations and list/get/revoke client/session records; redemption
and open below are the only authenticated remote paths that create/advance preassigned identities.

`create_remote_invitation` first stores prepared with one-time-secret verifier, public invitation metadata and
hard deadline no later than 600 seconds; after keystore-backed secret generation succeeds it atomically activates the record and only
then returns the secret once. Crash/restart while prepared invalidates it; no timer or client can activate it.

Invitation creation preassigns RemoteClientId and RemoteSessionId. Redemption carries a stable
`RemoteRedemptionId` and atomically consumes the invitation, creates/advances those exact records and persists
one `RemoteRedemptionReceipt` before returning any credential. It binds authenticated origin, device key,
closed role, Workspace/Session scope and negotiated protocol manifest. Concurrent redemption has one winner;
replay with the same id recovers the same redacted receipt and session, while a different id or payload is
refused. A lost response never consumes a second invitation or mints another client/session.

Exactly one negotiating/active RemoteSession may belong to a RemoteClient. After a disconnected session,
`open_remote_session` carries stable RemoteSessionOpenId, current client revision, authenticated origin/device
proof, same-or-narrower role/scope and current manifest hash. It atomically reserves one new RemoteSessionId,
moves a disconnected client to connecting and persists `RemoteSessionOpenReceipt`; handshake completion moves
client/session to active together. Same-id replay recovers that reservation, while concurrent opens, an active
session or a widened role/scope refuse. The predecessor remains terminal and all its capabilities remain invalid.

Redemption and open begin a 60-second device-key challenge. Verified challenge plus negotiated manifest moves
client connecting/session negotiating→active together. Failure, timeout, daemon-generation change or transport
loss moves both to disconnected and terminalises the session; disconnect of an active session atomically moves
an otherwise nonterminal client active→disconnected. The server persists only device public key and current
credential-verifier hash/generation, never bearer material. Same redemption/open-id replay signed by that device
may rotate the verifier and return one replacement ephemeral credential for the same reserved identity; it
invalidates any prior generation and cannot create another client/session. Exactly two rate-limited,
fingerprinted bootstrap frames exist before an active registry session: `redeem_remote_invitation` and
`open_remote_session`; all other names are denied.
An active session expires no later than 86,400 seconds; expiry or explicit session revoke atomically moves an
otherwise active client to disconnected, after which a fresh device-key-authenticated open is required. Client
revoke/expiry remains terminal and cascades instead.

`RemotePresence` is an ephemeral revisioned tuple keyed by exact RemoteClientId, RemoteSessionId, surface and
Workspace. It contains `present|idle|typing`, an optional authorised selected ViewTarget, update revision and
expiry no later than 30 seconds. `update_remote_presence` revalidates the active session, scope and exact target;
disconnect/expiry emits a tombstone, and state snapshots plus `remote_presence_changed` project it to authorised
peers. It is never journaled beyond the live replay window, replayed offline or interpreted as navigation,
read/acknowledgement, input, lease, Attention or control authority.

Presence admits≤128 records,≤4 KiB each/512 KiB family plus shared RSS. Remote anti-replay independently
admits≤10,000 nonce hashes,≤256 bytes each/2 MiB; count saturation uses small records and byte saturation uses
8,192 maximum records. Both reserve before acceptance/effect. Presence releases on disconnect/30-second expiry;
nonce releases on receipt/session revocation/connection loss or exact10-minute expiry. Restart inherits none,
and every count/item/byte N+1 produces zero remote mutation.

`PresenceChatMessage` is the separate optional human-to-human cursor overlay for authenticated encrypted
`full_gui` peers. One current message belongs to an exact client/session/Workspace/Surface/connection plus
authorised ViewTarget/revision,
contains one sanitised single paragraph≤512 UTF-8 bytes/256 scalars inside a≤1-KiB item, and expires within
30 seconds. There are≤128 messages/≤128 KiB installation-wide; each client may send four/rolling10 seconds
with≥500 ms between accepts. Replacement reserves before swap; retract/replace/expiry/disconnect/revoke/scope/
Surface/connection/ViewTarget-revision loss emits a live tombstone and reconnect inherits nothing. It is absent from durable store,
journal, diagnostics, export, notification and crash data. It is never agent input/context, command, navigation,
Attention, acknowledgement, resolution or authority; hiding/muting the overlay changes no canonical state.

The role matrix is exact. `full_gui` may use the registry's non-denied requests only when each is also present
in its invitation capability set; `headless_status` is limited to pure reads, bounded subscriptions,
`ack_state_revision` and surface-local navigation and has no domain mutation or input; `companion` is limited
to the nine mappings above plus `open_surface`, `retire_surface`, `get_state_snapshot`, state subscribe/
unsubscribe, `ack_state_revision` and
scoped account-activity get/subscribe/unsubscribe, its own permission-response receipt, presence updates and
delivery acknowledgement for a grant addressed to that exact client/session. A Companion cannot submit the
right-hand wire operation directly: the daemon performs the mapping from its envelope. Role, invitation and
object scope are an intersection, never a union or fallback.

All `subscribe_state_stream`, `subscribe_node_view`, `subscribe_resource_inventory`,
`subscribe_target_recovery_view`, `subscribe_account_activity`, `subscribe_live_notification_status` and
`subscribe_work_item_activity` records share one memory-only `LiveSubscriptionRegistry`; DirectoryWatch keeps
its separate source-specific count contract. The daemon mints each LiveSubscriptionId and owns it by exact
authenticated connection generation plus `state_stream(key)|node_view(Surface,key,content_kind)|
resource_inventory(ResourceScopeKey)|target_recovery(local admin Surface,ExecutionTarget)|
account_activity(profile,source_generation)|live_notification(endpoint,scope)|
work_item_activity(item,source_generation)`. An identical request
for the same canonical key returns the existing id. A changed revision/bound atomically reserves and replaces
that key only after success; failure leaves the old subscription current.

One connection owns at most 64 such subscriptions and the installation at most 4,096. Each metadata record is
≤4 KiB within a 16-MiB aggregate. One subscription queues at most 64 events or 1 MiB; all subscriptions on one
connection share 256 events/8 MiB and the installation shares 4,096 events/64 MiB. Queue bytes also charge
`runtime.turn_variable_rss_mib`. Admission reserves connection/global count, metadata bytes, per-subscription/
connection/global queue bytes and one terminal gap marker before registering any producer; every N+1 refuses
with zero producer subscription and leaves existing ids untouched. DirectoryWatch events use their separate
watch counts but charge the same connection/global queue and shared-memory pools.

Every vNext unsolicited subscription event is≤180 KiB serialized in one complete≤256-KiB frame, independently
of those queue limits. An oversized resource/recovery/account/notification/WorkItem/Node-view delta consumes
the pre-reserved gap and stops only that subscription; the client runtime automatically issues its exact
snapshot/read, streams a large logical response if needed and applies it only after full digest/generation
verification. Pushes are never fragmented, no partial view renders and no operator reload action appears.

Overflow/coalescing loss emits the pre-reserved terminal `gap(resnapshot_required)`, stops delivery and releases
the record; coalescing may remove only intermediate progress for the same object lineage and never terminal or
gap evidence. Explicit unsubscribe, authorisation/scope/source/owner invalidation, owning Surface loss where
applicable, RemoteSession/client/connection loss or process exit releases count and bytes. A reconnect inherits
no id and must snapshot before a fresh subscription. Unsubscribe/release is idempotent and a stale id cannot
name a later record.

Remote dispatch exposes runtime/resource reads and resource/live-notification subscription only to exact
invitation scopes in `full_gui|headless_status`, alongside the already scoped state/Node/account/WorkItem
families. Target-recovery read/subscription and its push remain local-administrative-surface-only and
absent from the remote registry. The matching push names are explicit; absence can never be substituted by a
generic state event.

`RemotePermissionResponseGrant` is immutable and binds GrantId/revision, remote client role/id and client
revision, RemoteSessionId/revision/expiry, surface and connection generation, provider/profile,
Workspace/Session, Node/AgentInstance, exact AttemptOwner/
RuntimeAttempt/InputRoute/binding generations, PendingInteraction id/revision, permission-fact revision,
typed transport generation, provider-offered response ids, bounded consequence metadata, issue time and an
expiry no later than the hard privacy limit. Its closed state is `active → consumed|revoked|expired|
invalidated`; all four destinations are terminal. Delivery has independent `pending_ack|acknowledged|failed`:
issue returns a local receipt and sends only that exact client's end-to-end encrypted capability plus minimal
redacted permission kind/consequence/options. Remote acknowledgement binds the same connection generation;
failure, disconnect, generation replacement, interaction/attempt/binding change or capability downgrade
invalidates it. Remote clients cannot list/get grants; the desktop may inspect/revoke the exact active record.
There is no offline permission draft or replay. A grant may target only `full_gui|companion`; headless_status
cannot receive or acknowledge it. Issue atomically requires no existing PermissionResponseClaim and loses to
a concurrent claim. It also CASes a unique
`RemotePermissionGrantIssueKey=(PendingInteractionId,interaction_revision,RemoteClientId,client_revision,
RemoteSessionId,session_revision)` so two concurrent issues to one grantee cannot create sibling active grants;
the same operation replays and a different one conflicts. It is permitted only while the current
`InputSafetyState=sensitive_interaction(class=permission)` proves the same interaction and fresh typed
transport; a generic sensitive class cannot obtain a permission grant.

Grant delivery has the closed reducer `pending_ack→acknowledged|failed`, with acknowledged/failed terminal.
The only valid grant/delivery pairs are `(active,pending_ack|acknowledged)`, `(consumed,acknowledged)` and
`(revoked|expired|invalidated,acknowledged|failed)`. Ack and a terminal grant race atomically; failed delivery
invalidates the grant, consumption requires acknowledged delivery and no terminal grant retains pending_ack.

`submit_local_permission_response` requires LocalDesktopForegroundAuthority and no grant.
`submit_remote_permission_response` requires an active acknowledged grant and matching authenticated client/
session/surface/connection/nonce. Both name one provider-offered option and repeat the exact owner, route,
binding, interaction, InputSafety, permission-fact and typed-transport revisions.

Every PendingInteractionId is installation-minted and never reused. All semantic permission paths—local typed,
remote typed and verified local PTY fallback—contend on one durable unique
`PermissionResponseClaimKey=(AttemptOwner,RuntimeAttemptId,attempt_generation,InputRouteId,route_generation,
PendingInteractionId,interaction_revision)`. The immutable
`PermissionResponseClaim` fixes ClaimId, operation id, path, chosen provider option, every safety/route/binding/
transport generation and optional exact GrantId. In one transaction before the first possible provider effect,
the daemon reserves one nonterminal and one terminal-receipt slot, CASes absence→claimed, persists the prepared
response receipt and, for remote, CASes the grant
active→consumed; it also invalidates every sibling grant for that interaction. A different operation, client,
path or option loses even if it raced from another local window, remote client or PTY encoder. Same-id replay
returns the first receipt. Once claimed, rejection, uncertainty, disconnect or crash never releases the key or
permits redispatch; only later correlated evidence changes the receipt. Claim/tombstone compaction is forbidden
until the interaction and receipt are terminal and the anti-replay retention boundary has elapsed.
Capacity exhaustion refuses before claim/effect and raises one bounded system Attention; active/uncertain
records never exceed the hard bound.

The receipt has independent closed axes:
`PermissionDispatchState=prepared|effect_armed|definite_no_effect|submitted|possible_effect` and
`PermissionEvidenceState=not_started|pending|not_applied|resolved|cancelled|attempt_ended|reconcile_required`. Prepared
moves through one durable pre-effect boundary. The only valid pairs/transitions are
`(prepared,not_started)→(effect_armed,not_started)`;
`(effect_armed,not_started)→(definite_no_effect,not_applied)|(submitted,pending)|
(possible_effect,reconcile_required)`; `(submitted,pending)→(submitted,resolved|cancelled|attempt_ended)`;
and `(possible_effect,reconcile_required)→(possible_effect,not_applied|resolved|cancelled|attempt_ended)`.
Provider-proved rejection may also move submitted/pending to submitted/not_applied. No other
cross-product is representable, and only correlated provider evidence may leave pending/reconcile-required.
No provider call or PTY enqueue may begin before effect_armed is committed. Recovery of effect_armed across a
daemon-generation change always writes possible_effect/reconcile_required and performs lookup/journal-sequence
reconciliation without dispatch; prepared remains safe to resume only through identical explicit replay.
`get_permission_response_receipt` recovers it; `reconcile_permission_response` performs correlation lookup only
and never sends the option again. Attention closes only from the later evidence state, not transport acceptance.
Definite-no-effect/not-applied retains the claim tombstone; retry requires a provider-observed fresh
PendingInteraction revision and can never reuse the old claim.

Each attempt exposes revisioned `InputSafetyState=ordinary|non_authorising_interaction|sensitive_interaction|
unknown`; every variant binds exact InputRoute, AttemptOwner, RuntimeAttempt/binding generations, state revision,
permission-fact revision and response-transport generation, and interaction variants add id/revision.
Reconnect, stream gap, binding/attempt change, stale/degraded/unknown capability or unclassified semantic state
becomes `unknown` until fresh evidence. `write_runtime_input` revalidates that complete state in the same serial
action as byte enqueue. Ordinary input accepts only its tagged lease-fenced bytes. A recognised permission under
fresh supported typed transport blocks raw bytes on every surface and uses the two typed operations above.
Fresh supported verified-local-PTY transport permits only a `verified_local_permission_fallback` request from
LocalDesktopForegroundAuthority: it carries the exact offered option and revisions, acquires the same durable
PermissionResponseClaim, and the daemon's versioned encoder deterministically derives/compares the bytes.
A typed↔PTY or local↔two-remotes race accepts one matching option through one claim and zero bytes/effects from
every loser. Degraded,
unknown, stale, unsupported or none permits no semantic permission response. Remote/Companion and local voice/
hook/background surfaces have no PTY fallback, grant widening or generic approval alias; opaque generic TUIs
retain honestly labelled ordinary terminal capability but receive no semantic permission guarantee.

## 11. Generic terminal tools and resources

Turn remains a complete terminal even when no agent integration is present: real PTY semantics, resize,
alternate screen, IME, clipboard/path drop, search, safe links, keyboard navigation, bounded scrollback and
explicit signal/lifecycle controls. Shells, `k9s`, logs and custom TUIs are first-class nodes, not degraded
agents. Their output may reveal provisional prompts or failures, but Turn does not fabricate agent concepts.

Services and log streams may be created independently or discovered below another runtime. File, Diff, Note,
WebPreview and Media content is inert, bounded and source-labelled. Restoring a resource does not load a URL, execute
content, open a file externally or delete the underlying data. The detailed WebPreview/path isolation rules in
`docs/AGENT_NODE_VIEWS_AND_CONTEXT.md` remain normative.

`WebPreview` and `Browser` are different kinds and capabilities. A WebPreview Resource stores only a bounded inert
snapshot/reference and loads nothing until an explicit preview; its isolated renderer has scripts, forms,
navigation, popups, downloads, ambient cookies/credentials, daemon sockets and local-file access disabled.
A Browser Node is an explicitly created interactive browsing context with a dedicated storage partition and
typed `navigate|back|forward|reload|stop|open_reviewed_popup|accept_reviewed_download|clear_storage`
operations. Its address, history entry ids, load/TLS/error state, permissions and popup/download dispositions
are daemon-visible facts, but page content, links and script messages are untrusted data and never become a
Turn control operation, Attention resolution or authority grant.

`load_web_preview` is the sole load path. It requires the exact foreground connection+Surface, WebPreview Node/
source revision, private canonical HTTPS URL≤4 KiB/2,048 scalars with no userinfo/query/fragment, policy/DNS
generation and a daemon-minted Workspace-owned `WebPreviewLoadIntentId`. The intent freezes operation/fingerprint,
source URL hash, up to 10 redirect identities, destination Surface and HTTP correlation before request. Its
reducer is `prepared→fetching|cancelled|refused`, `fetching→loaded|failed|reconcile_required`, and
`reconcile_required→loaded|failed|fetch_unconfirmed|reconcile_required`; terminals never reactivate. Fetching
commits before network. Reconcile inspects only the exact renderer/HTTP correlation and never fetches again; if
possible request evidence survives but body/result proof is irrecoverably lost, fetch_unconfirmed is terminal.
Same-operation replay returns the receipt and performs zero network.

The response admits only the closed declared MIME set `text/plain;charset=utf-8|text/markdown;charset=utf-8|
text/html;charset=utf-8`,≤8 MiB transferred compressed bytes,≤16 MiB decoded bytes, decompression ratio≤20:1
and≤30-second fetch; MIME sniff mismatch, other charset, oversize, unsupported/changed MIME, redirect overflow,
address-policy change or decoder failure closes without rendering partial content. One ephemeral
WebPreviewLoadState belongs to a connection+Surface+Node/source revision, with one/Surface, four/connection and 32
installation-wide. Its exact `WebPreviewFetchCorrelation` is≤8 KiB/≤256 KiB across all32 states and contains
only intent/URL-hash/policy/DNS/socket/renderer generations, never URL path/header/body. Bodies are≤16 MiB/item/256 MiB aggregate; at most eight inert renderer contexts exist,
each≤64 MiB/256 MiB aggregate. Every allocation also charges the shared variable-RSS pool. Count/body/renderer/
shared bytes reserve before request; N+1 performs no DNS/network/read/render. `close_web_preview`, source change,
navigation to another ViewTarget, Node/Surface/connection/renderer loss or 15-minute idle expiry first fences the
HTTP/renderer correlation and requests cancellation. A fetching intent reaches `failed(closed_no_result)` only
after the daemon proves the socket/worker stopped and no result or owned buffer can arrive; otherwise it enters
`reconcile_required` and lookup-only recovery. State/body/renderer/shared charges release only after that same
quiescence proof (or process death that proves OS reclamation), never merely because the owner disconnected.
The correlation slot/bytes follow that same proof and otherwise transfer without duplication to
ProcessCleanupCharge.
Outstanding close/loss therefore continues to occupy its caps and cannot be used to create orphan fetches by
cycling Surfaces. Reconnect inherits no view authority; it may only recover the original intent, and restore/
background selection never calls load.

The renderer receives only the daemon-decoded and inert-sanitised top-level document over a sealed one-way
channel and owns no socket or fetch API. External/internal CSS URLs, images, fonts, media, iframe/object/embed,
refresh, preload and every other subresource reference are removed or shown as inert labelled links; rendering
therefore cannot make a second network, local-file or daemon request.

At most32 WebPreviewLoadIntents are nonterminal;10,000 rich terminal receipts≤4 KiB each,100,000 minimal
operation/Node/source/URL/policy/correlation/disposition replay fences≤512 bytes each, and a combined64-MiB
active+receipt+fence family are installation-wide hard bounds. Counts and family bytes saturate independently;
10,000 maximum receipts plus51,072 maximum fences reach64 MiB exactly. Active, receipt, replay, journal and semantic-recovery capacity reserves before request. Rich receipts
compact after 30 days only behind a minimal fence; nonterminal/possible-request evidence never ages out.

The Browser policy defaults to no ambient provider/Turn credentials, no daemon/control origin, blocked
popups, quarantined non-executable downloads and denied device/clipboard/filesystem permissions. Every
`create_browser_node`, reviewed popup, ContentProjection link and announcement link uses one Workspace-owned
`BrowserNodeCreationIntent`. It freezes operation id and fingerprint, source kind/id/revision/hash when present,
policy generation, preassigned Browser Node/partition ids and exact destination graph revisions. Its reducer is
`prepared→created|dispatching|cancelled|refused`, `dispatching→created|not_created|reconcile_required`, and
`reconcile_required→created|not_created|reconcile_required`; terminals never reactivate. An inert no-load create
atomically moves prepared→created with graph publication and zero renderer/network effect. A load-capable path
commits dispatching before renderer/network dispatch and withholds graph publication until the exact preassigned
Node/partition binding is durably proved. Crash, timeout or lost reply after dispatch can only inspect that
binding and renderer correlation: it never reloads. Exact proof publishes that one Node as created, exact
pre-dispatch absence may become not_created, and uncertainty remains reconcile_required in semantic recovery.
Cancellation is legal only from prepared with no-effect proof. Same-operation replay returns the same intent.

Agent-driven browsing is a narrower principal over this same Browser kind, not a hidden browser or general
tool bridge. A local foreground operator adopts an expiring Workspace grant with≤64 public-HTTPS origin rules;
cloned/shared flags never activate it. One exact AgentInstance/RuntimeAttempt may create logged-out isolated
Browser Nodes carrying immutable owner evidence, then use only typed `navigate|read|click|type` operations at
the current navigation/accessibility revision. Read returns≤256 inert accessibility rows/256 KiB; click names a
stable element and type supplies≤4 KiB to a non-secret field. Raw selectors/scripts/requests, password/payment,
upload/path, localhost/private/file/daemon, credentials/clipboard/devices and a human/other-agent Node are
unrepresentable. Popups/downloads remain blocked or await a separate human review. Every controlled Node shows
an out-of-page agent badge and one `Stop`; revoke/expiry/attempt loss fences queued work. Each action has a
durable pre-effect intent and lookup-only receipt, so crash recovery never recreates/navigates/clicks/types a
second time or applies to a newer page.

The installation admits512 nonterminal BrowserNodeCreationIntents and10,000 nonterminal-plus-uncompacted
records, each≤4 KiB and≤32 MiB aggregate (8,192 maximum records at the independent byte boundary), with one
nonterminal intent per preassigned Node. Admission atomically reserves active, terminal receipt, one of100,000
minimal replay fences≤512 bytes/48 MiB aggregate (98,304 maximum fences at the independent byte boundary), journal and semantic-recovery capacity before any renderer,
network or graph effect. N+1 count/byte/receipt/recovery refusal creates no Node, partition or request. Terminal
richness compacts after 180 days only after the operation/fingerprint/Node/tombstone/correlation replay fence is
durable; nonterminal or possible-effect evidence never ages out. Minimal fences survive for the installation
lifetime; saturation refuses new creation rather than permitting operation/Node replay.

Every `navigate|back|forward|reload|stop` uses a Browser-Node-owned `BrowserNavigationIntent` and serialises one
nonterminal intent per exact Node/partition/navigation generation. Every direct, popup, announcement,
ContentProjection or redirect URL uses one canonical private BrowserUrl≤4 KiB/2,048 Unicode scalars; oversize
input refuses before dispatch. The intent freezes operation/fingerprint, reviewed URL or exact history-entry
target, policy/address/localhost grant or local-snapshot identity and a daemon-minted
renderer correlation. The reducer is `prepared→dispatching|cancelled|refused`,
`dispatching→applied|no_effect|reconcile_required`, and
`reconcile_required→applied|no_effect|dispatched_unconfirmed|reconcile_required`; terminals never reactivate.
Dispatching is durable before any load/history/stop signal. Reconcile queries only the exact renderer journal,
partition and navigation token; it never navigates, reloads, traverses history or sends stop. If loss destroys
the only proof after possible dispatch, `dispatched_unconfirmed` is terminal evidence and the same operation can
never dispatch again. A new operator action uses a new id and the current observed generation.

At most 256 BrowserNavigationIntents are nonterminal installation-wide; 100 rich terminal receipts per Browser
Node and 10,000 installation-wide are retained, each record≤8 KiB and the registry≤128 MiB. Up to 100,000 minimal
replay fences survive rich-receipt compaction; saturation refuses pre-effect. Active, terminal, replay, journal
and semantic-recovery capacity reserve before dispatch. Rich terminal receipts compact after 30 days only when
their minimal operation/Node-generation/fingerprint/disposition/correlation fence is durable; nonterminal and
reconcile-required evidence never ages out.

Browser runtime state is also bounded independently of receipts. One Node holds at most100 memory-only history
entries; the installation holds at most10,000 and≤64 MiB encoded, reached by8,192 maximum entries independently
of the count boundary. Each entry is≤8 KiB, contains one
BrowserUrl≤4 KiB/2,048 scalars, origin/TLS/load identity and either a control-stripped title≤1 KiB/256 scalars
or `title_omitted(oversize)`—never a misleading prefix. At most eight renderer contexts are live, each≤256 MiB and
≤1,024 MiB aggregate. Ephemeral partition/site-data/cache state is≤128 MiB per Node and≤512 MiB aggregate.
Each renderer owns at most current+pending `BrowserPage` safe metadata:≤16 records installation-wide,≤32 KiB
each/512 KiB aggregate. DOM/script/storage/body remain inside renderer/partition caps. A third generation
dispatches nothing; success atomically replaces current after old references quiesce, while stop/failure/Node/
partition/renderer/daemon loss discards pending. A hung renderer transfers the identical page slot/bytes to
ProcessCleanupCharge.
Creation/navigation reserves the worst-case 8-KiB history entry plus renderer and partition bytes before
launch/load; each count/byte N+1 leaves the Node
in inert unloaded state and performs no request. History insertion at the bound atomically drops only the oldest
non-current entry after proving no intent targets it; otherwise navigation refuses pre-effect. Explicit storage
clear, Node close or scope loss fences the renderer generation and requests termination. Quiescent exit
destroys partition data and releases; a non-reusable/hung renderer transfers its renderer/partition/shared
reservations to `ProcessCleanupCharge` until quiescence/OS reclamation. Navigation away releases page
cache only after its buffers quiesce, and parked reusable renderers retain their original slot+charge with only
inert durable Node metadata. Restore/reconnect inherits no renderer,
page, history or partition bytes and never loads automatically.

Browser Memory Saver is a separate opt-in `lifecycle_behavior` policy and defaults off. Its daemon-owned
`BrowserMemorySaverState` may discard only the exact current Browser revision after five continuous hidden
minutes, with a final eligibility check excluding loading, audible, agent-controlled, popup, download,
print, action and unsubmitted form/POST work. Discard is not a cosmetic parked label: renderer, page and
partition charges remain owned until quiescence or OS-reclamation proof, and the Node exposes `discarding`,
`cleanup_pending`, `discarded(history lost)` or a precise blocked state instead of claiming memory was freed.
The discarded projection retains only the canonical current public-HTTPS URL and its reviewed
origin/policy/address revisions; DOM, form/POST, content, cookie, storage, credential and history bodies are
destroyed.

Reselecting that Node is the only automatic rehydration trigger. In the same daemon generation it preassigns
one fresh idempotent `BrowserNavigationIntent` without another operator click only while the current policy
still permits that exact public origin/address. It never inherits ambient credentials and never auto-opens
localhost, a private address or local-file content. A changed policy/origin/address produces one exact bottom-
status reason, never a generic start-pane action. A possible-effect or crashed rehydration is recovered by
lookup only and never dispatched twice. This narrow same-daemon `discarded→selected` edge does not weaken the
rule that restore, reconnect, daemon restart and passive background selection perform zero automatic load.
Every redirect is revalidated before follow. A Location/final URL that exceeds BrowserUrl bounds stops before
that follow or history commit and records only `bounded_redirect(url_oversize)` plus a hash; it never expands
the intent/receipt/history reservation or stores the raw oversize value. One navigation follows at most ten
redirects; an eleventh stops before follow/history commit as `bounded_redirect(redirect_count)` and retains only
ten bounded redirect identity hashes.
All Browser renderer/partition/local-snapshot/history bytes also charge the installation's 1,024-MiB shared variable-
RSS pool before effect; their family maxima cannot be added together or borrowed from the separate 512-MiB
daemon/GUI/client-core reservation.

Local HTML uses a descriptor-verified memory-only `BrowserLocalSnapshot`, never live workspace access. One
foreground navigation freezes canonical confined root/regular descriptor, file identity/hash, Browser and
policy generations, then atomically reserves the declared bytes before reading. Its closed ephemeral reducer is
`reserved→reading|discarded`, `reading→sealed|discarded`, `sealed→loaded|discarded` and
`loaded→discarded`; discarded is terminal. Cancel/stop before read, descriptor/open failure, read/hash/descriptor
drift, navigation replacement, Node/scope/owner loss or renderer/daemon process loss follows the legal edge from
its current state to discarded and atomically releases family plus shared-variable memory charge; no state is
restored after loss. Each snapshot is at most 8 MiB, at most 32 exist and their aggregate is at
most 256 MiB. N+1 count/bytes refuses before reading. The sealed bytes are served once through a synthetic
isolated origin with no `file:` privilege or sibling resolution and are destroyed on navigation away, Node
close, scope loss or process exit. Durable receipts retain descriptor/hash/outcome only, never bytes.
`file://`, symlink/hardlink/mount escapes and adjacent-resource loads are refused. A loopback/localhost URL requires a foreground review binding exact scheme,
resolved IP set, port, target host/generation and expiry; navigation re-resolves and fails on DNS rebinding,
host/port change or remote-to-local fallback. Browser history/restoration never reloads a page automatically,
and destroying the Node clears its partition without claiming to delete server-side data.

A download response body never enters renderer memory or ambient browser storage. Before accepting body byte
one, the Workspace stream persists a `BrowserDownloadQuarantineId` with exact Browser/partition/navigation/
response identity, safe declared type/length, a maximum reserved size≤2 GiB, owner-only non-executable
temporary descriptor and pre-reserved active/terminal/recovery capacity. It reserves bytes from the same
installation-wide 4,096-MiB transfer-temporary pool used by TransferTickets. Missing length reserves the
review-policy maximum; invalid/oversized length or unavailable count/bytes refuses the body pre-effect. At
most 32 quarantines are nonterminal and 10,000 terminal receipts are retained for 30 days.

The quarantine reducer is `reserved→receiving|refused|cancelled|expired`, `receiving→sealed|failed|cancelled|expired`,
`sealed→transferring_ownership|discarded|expired`, `transferring_ownership→ticketed|reconcile_required`, and
`reconcile_required→ticketed|discarded|reconcile_required`; terminals never reactivate. `sealed` freezes exact
descriptor, received size, detected type and SHA-256. Sealed review is one action: it preassigns the
TransferTicket and File Node/blob, reserves the ticket slot, then atomically transfers temporary ownership and
the existing byte charge into the ticket without copying or redownloading. Ticket-cap N+1 leaves the sealed
quarantine unchanged. Lookup-only reconcile may prove the exact quarantine-or-ticket descriptor ownership and
reserved identities; it never reads the network, copies bytes or publishes a Node. Reserved/receiving/sealed
state expires 30 minutes after `created_at`; cancellation, expiry, Node close or scope loss discards only after exact no-ticket/
no-open-handle proof, otherwise retains recovery evidence. No quarantine auto-opens or becomes visible content.

Workspace and Session views provide local/remote file exploration and source-control/worktree operations as
typed views over the same execution target and checkout authority. Status, diff, stage, unstage, commit,
commit-and-push, fetch, pull, push, branch, history, conflict inspection/resolution and worktree cleanup state
their exact repository/host, base revision and consequences. Generated commit messages remain editable
drafts and never commit automatically. Destructive discard/cleanup and history-rewriting operations remain
explicit; a remote outage never redirects an operation to a local repository.

The RepositoryBackend wire set is closed to reads `get_repository_status|get_repository_diff|
list_repository_branches|get_repository_conflicts|get_commit_graph|get_commit_changed_files` and mutations
`stage_repository_paths|unstage_repository_paths|commit_repository|commit_and_push_repository|
fetch_repository|pull_repository|push_repository|create_repository_branch|switch_repository_branch|
initialize_repository|checkout_repository_commit|rename_repository_branch|delete_repository_branch|
stash_repository_changes|pop_repository_stash|merge_repository_branch|rebase_repository_branch|
revert_repository_commit|force_push_repository|resolve_repository_conflict|discard_repository_changes|
cleanup_repository_worktree`. Every request except initialize carries
one closed `RepositoryAuthority`:
`filesystem(RepositoryBackend handle,target/trust,RepositoryId,CheckoutScopeId/revision)` or
`hosted(the same filesystem authority plus RepositoryHostProfileId/revision and active
RepositoryHostCapabilityGrantId(kind=repository_backend)/revision/scope/expiry)`. A credential-free local or
remote-target repository with no configured hosting account is fully representable by `filesystem`; status,
diff, staging, local commit, branch/conflict and worktree operations require no host profile. Fetch, pull,
push, commit-and-push and every provider-hosted effect require `hosted`; a filesystem authority cannot cause
network access. Both variants also bind canonical checkout identity,
closed `primary|non_primary` classification and the applicable HEAD/index/worktree/remote observation
revisions. Reads may inspect an enforced read-only primary checkout. Every mutation first proves
`non_primary` after canonical path, descriptor, symlink, mount, alias and resolved Git-index/lock identity;
any primary-tree, primary-index, primary-lock or primary-branch target refuses before provider/file effect.
A Turn-managed writer additionally proves the exact owned isolated-worktree generation and active lease.
Mutation receipts distinguish definite-no-effect, applied and reconcile-required; commit-and-push
has separately correlated commit and push outcomes. Destructive discard/conflict/worktree cleanup requires
local foreground consequence review and exact descriptor/hash/Turn-owned worktree survivor disposition.
Unknown aliases and generic shell-command payloads are unrepresentable.

The advanced verbs do not inherit Git CLI breadth. Initialize accepts only an exact empty confined directory
inside a newly reserved non-primary CheckoutScope and preassigns RepositoryId. Detached checkout, branch
rename/delete and stash push/pop freeze exact HEAD/ref/index/worktree/stash-object revisions and survivor
dispositions. Merge, rebase and revert require a finite≤1,000-commit/10,000-path preflight with a closed strategy
and preserve `applied|conflicted|aborted|reconcile_required` rather than flattening partial state. Pop never
drops its stash ref until worktree+index application is proved. Force push is desktop-foreground
`force_with_lease` only: hosted authority, active grant, exact observed remote object, protected-branch policy
and consequence review are mandatory; raw force/wildcards/unknown tip cannot be encoded. All are forbidden in
the primary checkout, reserve the same RepositoryMutationIntent before effect and reconcile exact objects/refs/
index/worktree/provider correlation without rerunning Git.

Every RepositoryBackend mutation uses a preassigned, ExecutionTarget-owned
`RepositoryMutationIntentId`. Before the first index, worktree, ref or network effect, one transaction freezes
the operation id, exact RepositoryAuthority and CheckoutFence generation, canonical descriptors, reviewed
verb/arguments, all expected pre-state object/ref/index/worktree/remote identities and the closed expected
postcondition fingerprint; it also reserves its intent/receipt/journal/recovery capacity. The reducer is
`prepared → dispatching|refused|cancelled`, `dispatching → applied|no_effect|reconcile_required`, and
`reconcile_required → applied|no_effect|reconcile_required`; terminals never reactivate. A definite adapter
error may become `no_effect` only with effect-specific proof, not from timeout, disconnect or process exit.

Commit seals the exact tree, parent vector, bounded message/author policy and deterministically expected commit
object id before moving its branch ref. Network effects additionally freeze remote identity, old/new ref object
ids and a provider correlation token when supported. Commit-and-push records a product state for
`local_commit_outcome × remote_push_outcome`, so proved local commit plus ambiguous push is never flattened or
recommitted. Pull and all multi-ref/worktree verbs seal their finite before/after plan; if an implementation
cannot predict or correlate a unique postcondition, it must refuse that verb before effect rather than claim
reconciliation support.

`reconcile_repository_mutation` is lookup-only against exact object ids, refs, index/worktree fingerprints,
remote observation generation and provider correlation. It never invokes Git mutation, network retry,
credential rotation or cleanup. Proved full postcondition reaches `applied`; proved unchanged complete
precondition reaches `no_effect`; external divergence or incomplete/gapped evidence remains
`reconcile_required` with its suboutcome vector. Same-operation replay returns the same intent. At most 10,000
nonterminal-or-uncompacted mutation intents consuming 256 MiB exist installation-wide; each intent is at most
32 KiB and terminal richness compacts after 180 days only after operation replay, object/ref non-substitution,
CheckoutScope/lease and remote-correlation fences persist. N+1 count/bytes/receipt/journal/recovery admission
refuses before local or provider effect; nonterminal and possible-effect evidence never ages out.

Authorised full-GUI remote surfaces may request only registry-listed ordinary stage/unstage/commit/fetch/
create-branch/switch-branch operations under that exact grant. Commit-and-push, pull, push, conflict
resolution, discard and worktree cleanup require LocalDesktopForegroundAuthority and are absent from the
remote registry; catalogue indirection cannot reclassify them. Headless clients remain read-only and
Companion has no repository operation.

### 11.1 Bounded exploration, history and text search

`list_directory` has a closed paging union. `begin` carries target/trust/root and reuse-safe directory identity
plus expected observed revision but no client-chosen scan id; the daemon mints DirectoryScanId and pins the
actual directory revision in page 0. `continue` carries that id/revision, next page sequence, prior cursor
digest and opaque next cursor. Reuse against another directory/generation/revision or skipped/replayed changed
cursor gaps the scan. Each request-only `DirectoryPage` binds those fields and `complete|partial|gapped`
coverage, with≤2,000 entries,≤2 KiB/entry and≤4 MiB logical including envelope. The stateful DirectoryScan is
only≤16-KiB pinned-revision/cursor metadata,≤16/connection and≤1,024/16 MiB installation-wide; page bytes are
owned by the generic response stream/outbox and survive no request. Entries expose bounded
name/kind/size/time/identity only; listing/watching never follows symlink,
hardlink or mount aliases. `DirectoryWatchId` starts from one complete revision, emits monotonic create/remove/
rename/metadata events and a gap on overflow, target change or cursor loss; each watch owns≤8-KiB metadata
inside a≤2,048/16-MiB family and resnapshot is mandatory before mutation. `CommitGraphPage` is a request-only
≤500-node/≤2-KiB-node/≤1-MiB logical response with parent ids from a traversal capped at 10,000;
`CommitChangedFilesPage` is a request-only≤1,000-row/≤2-KiB-row/≤2-MiB response for one commit. Their
≤512-byte authenticated cursors encode exact repository revision and offset without a retained server object.
Missing parents, cycles, replacement, oversize row, stale revisions or overflow are explicit gaps, never an
invented graph or effect.

`TextSearchSession` pins one closed source coordinate kind. Terminal search uses
`TerminalTextRevision=(AttemptOwner,buffer_generation,first_seq,last_seq,cell_grid_revision)` and matches
`(logical_line_id,start_cell,end_cell)` over the exact retained decoded cell grid. Note/editor search uses
`TextDocumentRevision+content_hash` and UTF-8 byte ranges that land on scalar boundaries. Query is≤4 KiB and
the scan observes≤10,000 matches. A TextSearchSession retains only≤16-KiB query/source/cursor/count metadata;
one request-only page returns≤200 matches,≤1 KiB each/≤200 KiB logical, and no 10,000-item result set is kept.
Across all Surfaces there are≤512 sessions/8 MiB. One document search scans at most 16 MiB of UTF-8 source; one terminal search stops before
either 1,000,000 decoded cells or 100,000 logical lines. Work is cancellation-aware and yields after each
≤25 ms CPU slice. Coverage is exactly `complete|bounded(limit,cursor)`; a bounded scan never reports global
`no_match` and continuation pins the same source revision plus cursor. Next/previous/wrap/no-match changes only that surface's typed cursor; scrollback eviction,
reflow/grid change or document revision invalidates highlights/results before movement. Search emits zero
terminal bytes, lifecycle, selection, read/Attention or source mutation.

Terminal residency is an independent closed resource family rather than an implication of the 10,000-live-
Attempt semantic cap. The installation admits at most 128 live-or-retained `TerminalRuntimeState`s. Before PTY
spawn it reserves the state plus a 2-MiB `TerminalByteRing` and 4-MiB current-grid allowance; raw-ring aggregate
is≤256 MiB. `TerminalScreen` is≤8 MiB/state and≤512 MiB aggregate with≤5,000 scrollback rows, trimming only
oldest unpinned scrollback under pressure and preserving current cells plus an honest truncated/gap boundary.
`TerminalImageStore` is≤16 payloads/16 MiB per terminal and≤512 MiB aggregate. Image admission evicts only
unplaced LRU payloads and otherwise shows the bounded refusal while text continues. A visible client-side cache
is one/Surface+Pane,≤12 payloads/12 MiB, four/connection,64 installation-wide and≤256 MiB aggregate; it may
evict to a placeholder/refetch because it is never authoritative. All bytes also charge the 1,024-MiB shared
pool. A stopped unpinned state with a complete durable checkpoint may release before a new launch; live states,
current cells and search/view pins never do. The 129th live state refuses pre-spawn when no such release is
possible, and teardown releases only on exact owner/checkpoint/pin proof.

Transient image work is separately admitted before allocation: eight≤8-MiB scan buffers/64 MiB, eight≤8-MiB
multipart assemblies/64 MiB and two≤128-MiB complete decode high-waters/256 MiB. The decode item includes live
input, inflate, allocator, raster, resize scratch and final RGBA; implementation allocator limits are remaining-
budget limits, not extra memory. No slot/byte enters bounded discard-to-terminator mode, coalesces one visible
refusal and keeps text/input/Attention moving. Success transfers only≤4-MiB final RGBA into the retained store;
all other bytes release on success/abort/generation loss/quiescence.

Attachment and transport state are closed too. PaneAttachment is≤8 KiB and≤64/Surface,256/connection,4,096/
32 MiB global; a cells attachment owns one≤2-MiB projection baseline under256 MiB aggregate. Authoritative
TerminalOutputQueue is≤512 shared chunks or8 MiB/PTY and4,096 chunks/256 MiB global; buffer-first overflow
records an exact gap, retires only the old attachment generation and makes the client runtime perform an
automatic streamed, atomic fresh-generation resync with no operator action. At most128 pump batches hold≤16 frames/1 MiB each/
128 MiB. Connection outboxes hold≤256 frames/8 MiB each and4,096/128 MiB globally, every frame≤256 KiB.
Logical responses above192 KiB are automatic contiguous≤180-KiB raw streams, four/connection,16 global,
≤7,680 KiB/item/120 MiB total, with a≤128-byte RequestId and digest; no partial applies. Image fetch admits
eight/Surface,32/connection,128 global and≤4 MiB/item/128 MiB, transfers verified bytes into a reserved client
cache without duplicate shared charge and cancels on detach/reselection/gap/disconnect. One automatic same-view
refetch is allowed; repeat failure remains a labelled placeholder with no operator action or loop.

These pinned views have a closed live lifecycle. One authenticated connection may own at most 16
DirectoryScans (1,024/16 MiB installation-wide) and eight CatalogueScans (512/16 MiB installation-wide); one Surface may
own at most eight TextSearchSessions (512/8 MiB installation-wide). One authenticated connection may also own 32
DirectoryWatches, with 2,048/16 MiB installation-wide. A watch is admitted only from one complete current scan
revision and reserves its≤8-KiB item/count/event-gap capacity before subscribing; each per-connection/global N+1 refuses
before backend subscription. Its first overflow, cursor loss, source/target invalidation or generation change
emits one terminal `gap(resnapshot_required)` and releases the subscription; explicit unwatch or owning
connection loss also releases it. A reconnect cannot inherit an old WatchId and must rescan completely.
Directory idle TTL is 60 seconds, catalogue
idle TTL 30 seconds and text-search idle TTL 15 minutes. Exact continuation/movement refreshes only its own
TTL. A complete final page, explicit close, source invalidation, owning connection/Surface loss or TTL expiry
releases the pin; later use returns `gapped(expired|closed|disconnected)` for a page scan or `stale` for search,
never false complete/no-match. Admission first expires eligible records, then refuses per-owner or global N+1
without evicting another live scan; a reconnect begins a new id and cannot resume an old connection's pin.

### 11.2 Inert media import and playback

`MediaImportId` freezes a reviewed drop descriptor or pasted byte source, destination Session/Group, a
preassigned NodeId, reserved blob identity/capacity, regular-file identity when applicable, declared/sniffed
MIME, size≤256 MiB, SHA-256, owner-only temporary identity and operation fingerprint. Its reducer is
`prepared→reading|cancelled`, `reading→validated|cancelled|refused|failed`,
`validated→committing|cancelled`, `committing→committed|failed|reconcile_required`, and
`reconcile_required→committed|failed|cancelled|reconcile_required`; refused/failed/committed/cancelled are
terminal. Read/validate is descriptor-based and rejects symlink/hardlink/mount/TOCTOU, MIME disagreement,
polyglot/decoder bomb and size/hash change. The validated state seals the temp descriptor/hash. Before any
blob/Node publication, commit durably records its exact Node/blob/destination reservation and moves to
`committing`. Crash or cancellation after that point performs lookup-only reconciliation against the sealed
blob and Node binding: proved publication becomes committed, proved absence may cancel/fail and uncertainty
stays reconcile-required. It never rereads a changed source, repeats a copy or exposes a partial Node.

Preparation atomically reserves one of 32 installation-wide nonterminal MediaImport slots, one of 10,000
terminal receipts, semantic-recovery capacity and the declared bytes against the owning Workspace's single
10,240-MiB Media physical-byte pool before descriptor read or first pasted chunk. The owner-only temporary
uses that reservation; commit transfers descriptor/blob ownership and charge without copying or double-
counting. A duplicate content hash adjusts only after exact refcount/blob proof in the same transaction.
Count, item≤256 MiB, Workspace bytes, receipt or recovery N+1 refuses pre-read/pre-chunk. Nonterminal and
reconcile-required evidence never ages out; terminal richness may compact after 30 days only after Node/blob/
operation/refcount replay fences persist, and cleanup cannot free the byte charge on ambiguous publication.

A dropped local/remote file is read only through its pinned backend descriptor. A pasted/remote-client byte
source instead reserves one authenticated `MediaImportStreamId`; `put_media_import_chunk` carries exact
import/stream revisions, monotonically indexed offset, bytes≤4 MiB, chunk SHA-256 and declared final total/
hash. Duplicate identical chunks are idempotent, while changed replay, overlap, gap or overflow fails. Bounded
backpressure advances no revision until capacity exists. JSON-line base64 remains below the 8-MiB wire cap;
no ambient path or unbound upload body is accepted.

Playback uses a sandboxed decoder with no network, filesystem outside the immutable blob, script, clipboard,
daemon/control socket or terminal input. `MediaPlaybackStateId` is daemon-minted, memory-only and owned by one
authenticated connection+Surface+MediaNode/blob+playback generation. It contains
state=`stopped|loading|ready|playing|paused|ended|error`, codec/
container identifiers each≤64 ASCII bytes or one closed error code
`unsupported_codec|unsupported_container|corrupt_media|decode_failed|resource_limit|source_unavailable|
sandbox_failure|internal`, `elapsed_ms`, known-or-unknown `duration_ms`, `muted`,
`volume_millipercent=0..1000`, at most 64 caption tracks with stable id≤64 bytes/32 scalars, normalised BCP-47
language≤35 ASCII bytes, kind=`subtitles|captions|descriptions`, inert label≤128 bytes/64 scalars and optional
selected track. Play/pause, seek to bounded absolute milliseconds, mute, exact volume and select/disable
caption track carry expected playback revision/generation; stale/out-of-range/unknown track refuses. Restore/
selection never loads/decodes/autoplays. Unsupported codec/error remains inspectable and has zero fallback to
an external app. The complete encoded state is≤32 KiB; admission/track replacement that cannot fit refuses
without changing the current state.

Exactly one current playback may belong to a Surface, at most four to one authenticated connection and 32
installation-wide. One decoder working-set reservation is≤64 MiB, all playback decoder/frame/cache state is
≤512 MiB aggregate and every byte also charges `runtime.turn_variable_rss_mib`. First explicit play and
automatic same-Surface source replacement reserve state count, family bytes and shared bytes before decoder
spawn/read; replacement commits atomically only after admission, so failure preserves the previous state and
N+1 has zero read/decode/autoplay. Stop, ended/error, source/blob invalidation, selection of a different
ViewTarget, Node close, Surface loss or owning connection loss first fence the exact decoder generation and
request termination; state/count/family/shared charges remain in `cleanup_pending` until descriptor/process/
thread/shared-buffer quiescence or OS-reclamation proof. A decoder exit with that proof releases; a hung or
uncertain decoder retains its slot+bytes in recovery-owned cleanup and cannot be bypassed by cycling Surfaces
or connections. End/delete remains total using that existing recovery reservation. Reconnect inherits no
control or view authority. Pressure may pause/park only after an exact state transition and never kills or
changes the underlying Media Node/blob.

### 11.3 Repository host identity and proposals

`RepositoryHostProfileId` names a non-secret installation record bound to canonical HTTPS/SSH host identity,
ExecutionTarget/trust generation, provider account id, declared scopes and external CredentialReference. State
is closed: `draft→authenticating|revoked|deleted`, `authenticating→validating|degraded|revoked`,
`validating→active|degraded|revoked`, `active→validating|degraded|revoked`,
`degraded→authenticating|validating|active|revoked`, `revoked→authenticating|deleted`; deleted is terminal.
For rotation, `active→degraded(reason=rotation_pending)` is the only pre-effect transition; it revokes current
grants before dispatch and no implicit path returns active. Create/adopt/authenticate/validate/rotate/revoke/delete are distinct foreground intents. These profiles are
optional for filesystem-only RepositoryBackend work and mandatory only for hosted/network effects or a
host-backed WorkItemSource.

Authenticate and rotate each use one Installation-owned `RepositoryHostCredentialIntent(kind)` durable before
broker/provider effect. It freezes operation/fingerprint, profile/target/trust/canonical host/account/scopes,
profile and old effective credential generations, pre-reserved next generation, broker-policy revision, exact
provider correlation and expiry. The reducer is `prepared→dispatching|cancelled|expired`,
`dispatching→awaiting_provider|refused|reconcile_required`,
`awaiting_provider→credential_received|auth_failed|reconcile_required`, and
`reconcile_required→credential_received|not_applied|auth_failed|reconcile_required`; terminals never
reactivate. Post-dispatch timeout/crash remains reconcile-required. Reconcile queries correlation only and
never repeats auth/rotation or secret writes. Providers without exact lookup/idempotency correlation advertise
these operations unsupported.

Authenticate couples the permitted profile→authenticating transition before dispatch. Rotate atomically
persists the intent and next generation, moves active→degraded(rotation_pending) and revokes every active host
grant before dispatch. Correlated credential receipt makes the next generation effective and moves only to
validating; explicit validation alone reaches active. Refused/not-applied rotation remains degraded until the
old generation is explicitly revalidated. No grant auto-reactivates or regrants. Profile revoke remains locally
authoritative despite uncertain provider cleanup, fences late callbacks and delete refuses any nonterminal/
possible-effect credential intent. One intent may be nonterminal per profile.

The installation admits 10,000 nonterminal-or-uncompacted credential intents, each≤4 KiB and≤32 MiB aggregate,
with exactly8,192 maximum records at the independent byte boundary while the count boundary uses smaller
records, and terminal richness for 180 days. Count/bytes/terminal/recovery/next-generation capacity reserves before
broker/provider effect; N+1 has no credential/grant/profile/provider effect. Folding retains operation,
profile/host/account/credential-generation/grant-revocation/correlation fences; possible-effect evidence never
ages out.
`RepositoryHostCapabilityGrantId` identifies one immutable grant bound to the exact profile/revision,
target/trust/host/account/credential generation, capability kind=`repository_backend|work_item_source`,
repository-or-project scope and expiry. Its closed state is `active→revoked|expired`; terminal ids never
reactivate and regrant mints a new id. At most 128 grants per profile are active; terminal grant
richness folds into a non-reused id/generation/scope high-water under the separate receipt bounds. Grant and revoke are separate local-foreground operations; profile
revocation/deletion atomically revokes every active grant. A RepositoryBackend handle carries its exact
`RepositoryHostProfileId+RepositoryHostCapabilityGrantId(kind=repository_backend)+revisions`; this is the
`hosted` RepositoryAuthority variant. A filesystem-only handle carries no invented profile/grant. A host-backed
WorkItemSource carries its exact
`RepositoryHostProfileId+RepositoryHostCapabilityGrantId(kind=work_item_source)+revisions`. Every backend/source operation revalidates that
binding and cannot substitute the other kind. Failure/revocation of one leaves the other unchanged. Reads
reveal safe host/account/scope/state only; secrets remain write-only at the broker and deletion waits for
credential intents, then never removes
provider data or an external credential.

`CommitProposalProviderProfileId` names an installation-owned profile with revision and state
`draft→validated|retired|deleted`, `validated→retired`, `retired→validated|deleted`, deleted terminal. It
freezes exactly one provider:
`sandboxed_executable(canonical descriptor identity,SHA-256)` or
`model_gateway(ModelEndpointProfileId+revision,model/route generation)`, plus sandbox-policy revision and
numeric limits `wall≤30s,cpu≤10s,processes≤4,RSS≤512MiB,stdout≤8KiB,stderr≤8KiB`. Local foreground
create/adopt/update/validate/retire/delete uses operation ids; an executable/hash or broker-route change mints
a new profile revision and can never alter an in-flight attempt.

The installation admits at most 64 CommitProposalProviderProfiles and current plus 31 historical revisions
per profile. Referenced revisions do not compact; an update at the bound refuses before changing the current
profile. Delete refuses while a nonterminal/uncertain Attempt references the profile, then removes provider
configuration while retaining the attempt's independently bounded descriptor/policy hashes and replay proof.

Each `CommitProposalAttemptId` and exact profile/repository/snapshot/policy revision is durable before
dispatch. `CommitProposalAttemptState` is closed:
`prepared→dispatching|failed(cancelled_or_expired_pre_dispatch)`,
`dispatching→succeeded|failed(timeout|crash|signal|limit|ambiguous_broker|expired_during_dispatch|invalid_output)`;
succeeded/failed are terminal and there is deliberately no reconcile/redispatch state. The transaction that
moves Proposal `prepared→generating` also moves its Attempt `prepared→dispatching` before external effect.
Attempt success and Proposal `generating→ready`, or Attempt failure and Proposal `generating→failed|expired`,
commit atomically. Expiry/cancel racing dispatch kills any helper tree, fences the broker result and cannot
leave a nonterminal Attempt behind or reuse its operation id.

A sandboxed executable starts in a newly created empty non-repository cwd, with an allowlisted
environment that contains no HOME/workspace/provider/credential values, only stdin/stdout/stderr open, no
inherited sockets/PTYs/descriptors and only the sealed canonical redacted staged snapshot on stdin. The
enforced sandbox may read its exact executable and required system libraries plus its empty temp cwd, but
denies repository/workspace/home/arbitrary filesystem, daemon/control socket, keychain, clipboard, devices
and all network. At most two executable helpers run installation-wide; each reserves≤512 MiB RSS and the
1,024-MiB family/shared-variable pool before spawn. A third admitted Attempt waits without a process and
cannot hold a repository descriptor. Cancel/expiry fences the helper; its slot/bytes release only after tree/
descriptor/buffer quiescence or OS reclamation, otherwise ProcessCleanupCharge inherits the same reservation.
A model-gateway profile spawns no helper; the daemon sends the same sealed snapshot only
through the exact pre-existing pinned ModelEndpointProfile broker. If the platform cannot enforce the chosen
boundary, generation is unsupported. Timeout, crash, signal, child/process/RSS/output limit or ambiguous
broker result terminalises that attempt and never respawns or redispatches under the same operation id; a new
operator generation request creates a new attempt.

At most 10,000 non-compacted CommitProposalAttempts exist installation-wide. Generation reserves both its
nonterminal and terminal receipt slot before dispatch; N+1 refuses before helper spawn or broker request.
Terminal Attempts compact after 30 days only when their CommitProposal is terminal and the minimal operation/
profile/repository/snapshot/result replay fence is durable. Nonterminal Attempts and terminal Attempts whose
CommitProposal is nonterminal never age out, and retry under the same operation id cannot evade either bound.

`CommitProposalId` freezes filesystem-or-hosted RepositoryAuthority, RepositoryId/revision, staged-index revision/hash, a redacted text diff≤128 KiB,
omission manifest and exact CommitProposalProviderProfile/Attempt revisions. State is
`prepared→generating|refused|expired`, `generating→ready|failed|expired`, `ready→applied_to_editor|discarded|
expired`; terminals do not resume. Output is sanitised UTF-8≤8 KiB. Generation can read no unstaged file,
credential or network except the exact broker profile above. Apply CASes one commit-message
editor draft revision only; every proposal phase emits zero stage/commit/push/branch/file effect.

### 11.4 Reviewed transfer tickets

`TransferTicketId` is owned by one exact Workspace stream, independently of endpoint location, and freezes
`upload|download`, source and destination as two independent `TransferEndpoint`s. The caller must be authorised
for that Workspace and for each endpoint separately; a cross-target transfer never borrows authority or stream
ownership from either side.
Each endpoint is `backend_descriptor(ExecutionTargetId,target/trust/root/descriptor generations,identity)`,
`authenticated_client_stream(RemoteClientId,RemoteSessionId,surface/connection generation)`, source-only
`browser_download(BrowserNodeId,partition/navigation revision,download id,response identity,size/type/hash)`
or destination-only `turn_file_resource(preassigned NodeId/blob id,Session/optional Group graph revisions)`;
no singular
target/root field may stand for both. The ticket also freezes size≤2 GiB, SHA-256, chunk size≤4 MiB, the sole
destination policy `create_new`, expiry≤30 minutes and operation
fingerprint. State is `prepared→transferring|cancelled|expired`, `transferring→paused|completed|failed|
reconcile_required|cancelled|expired`, `paused→transferring|cancelled|expired`, and
`reconcile_required→completed|failed|paused|cancelled|expired|reconcile_required`; completed/failed/cancelled/
expired are terminal. Chunks are indexed/hash-checked and idempotent; owner-only temporary bytes become a non-
executable file through create-new atomic rename only after full size/hash verification. A cancel/expiry racing
possible publication records the requested disposition but reaches cancelled/expired only after lookup proves
that no destination was published; a proved output reaches completed and uncertainty stays reconcile-required.
Crash/disconnect reconciles by ticket/chunk ledger, never restarts into a same-named local/remote path or
silently overwrites.

The 4,096-MiB transfer-temporary allocation is one shared physical-byte budget across active TransferTickets
and BrowserDownloadQuarantines. Quarantine-to-ticket handoff changes owner/accounting identity atomically and
does not reserve, copy or count the same descriptor twice. Direct ticket preparation reserves its bytes before
source I/O; a quarantine reserves before response body byte one. Count, byte, terminal-receipt and semantic-
recovery N+1 always refuse before their respective effect.

When an endpoint is an authenticated client stream, `put_transfer_chunk|get_transfer_chunk` binds exact
TicketId/revision, client/session/surface generation, endpoint role, monotonically indexed offset, bytes≤4
MiB and chunk hash. Get returns only the reviewed source chunk; put admits only reserved capacity. Duplicate
same-hash calls are idempotent, while changed replay, overlap, gap, expiry, revoke, wrong direction or stale
generation returns a receipt and zero bytes/effect. Other endpoint pairs stream only through their bound
FileBackend transports, never through a same-name fallback.

### 11.5 Inert content projection

`ContentProjection` is surface-local and binds exact source kind/id/revision/hash, mode=`plain|markdown`,
sanitizer version and source size≤2 MiB. Markdown accepts no raw HTML, script/event handler, remote/local image,
network fetch, form, control sequence or unsafe scheme; reviewed links remain inert until their separate open
action. Unsupported/binary/oversize/decode failure is explicit. Switching mode never changes source bytes,
terminal input sequence, file revision, selection ownership or Attention, and a later source revision cannot
silently replace the pinned projection.

Exactly one current ContentProjection may belong to one Surface; one authenticated connection may hold four,
with 64 and 128 MiB installation-wide and each source≤2 MiB. `set_content_projection` reserves the replacement
count/bytes and validates/sanitises the pinned source before atomically replacing that Surface's old projection;
failure preserves the old one. `clear_content_projection`, source invalidation, Surface loss or owning
connection loss releases bytes immediately. Reconnect never inherits a projection. Each N+1 refuses without
evicting or changing another Surface, and projection bodies/results remain memory-only.

`open_reviewed_content_projection_link` binds ContentProjectionId/revision, pinned source id/revision/hash,
LinkId plus normalised URL/text hash, isolated Browser policy generation, preassigned Browser NodeId and exact
destination Workspace/Session/optional Group graph revisions. Only a foreground HTTPS consequence review may
commit that new Browser Node; stale projection/source/link/destination produces zero navigation or Node. It
never reuses current selection as an implicit destination.

### 11.6 Catalogue, announcements, updates and activity

#### Canonical signed-artifact boundary

`SignedArtifactDomain` is exactly `command_extension|product_announcement|update_manifest|update_package|
voice_model_manifest`.
Each domain owns a distinct installation `SigningTrustStore(domain,revision)`; a verifier is forbidden from
looking up a key in another store. `SignedEnvelopeV1` is a closed schema with exactly:

- `domain`, `schema_version=1`, bounded `payload_type` and `payload_sha256`;
- `signer_key_id`, monotonic `signer_key_epoch`, `issued_at_ms`, `expires_at_ms`;
- exact audience `{channel,platform,architecture_or_none,cohort_or_all}` and monotonic `sequence`;
- `parent_manifest_sha256`, required only for `update_package` and forbidden otherwise;
- `algorithm=ed25519` and the signature bytes.

The signature preimage is byte-for-byte
`UTF8("TURN-SIGNED-V1\0") || u32be(domain_byte_length) || UTF8(domain) ||
u64be(canonical_envelope_byte_length) || canonical_envelope_bytes`, where `canonical_envelope_bytes` is the
RFC-8785 canonical JSON representation of every envelope field except `signature`. Signed structured payloads
are schema-validated with duplicate and unknown keys rejected, canonicalised by the same RFC-8785 rules and
hashed as exact canonical bytes. An update package hashes its exact streamed bytes. The package envelope's
`parent_manifest_sha256` is the SHA-256 of the complete canonical signed-manifest envelope (including its
signature), and its payload hash/size/platform/architecture/version must also equal that manifest. Code
signing/notarisation is an additional platform check, never a substitute.

Each trust store contains domain, revision, active/revoked key ids and epochs, rotation provenance and a
monotonic high-water per exact audience. It retains at most 256 active/retired/revoked key-epoch records and
4,096 exact-audience high-water records; capacity exhaustion refuses a new key/audience before effect while
existing verification remains available and no revocation fence is dropped. Key ids are meaningful only inside their domain. Rotation must be to
a higher epoch and be authorised by the current domain root/threshold policy; revocation is terminal, and an
app-bundled root replacement is a separately signed installation event. A request may carry only
`expected_trust_store_revision`; it can never choose a root/key. Same audience+sequence+payload hash is an
idempotent replay; same sequence with another hash, any lower sequence, expired/not-yet-valid envelope,
revoked/old key, wrong audience/domain/root or payload mutation is rejected before catalogue/feed/download/
stage effect. High-water and revocation fences survive compaction and rotation.

Signed command-extension payloads contain only stable catalogue entries referencing operations, schemas and
capabilities already registered in this build. They contain no executable bytes/path, plugin, shell command,
new operation or new capability; revocation disables every entry atomically before invocation. Announcement
links and update manifest/package are revalidated against the current store and high-water immediately before
open, stage and apply. A valid object from one domain can never validate or advance another domain's store.

The canonical revisioned `CommandCatalogue` contains stable `CommandEntryId`, category=`creation|general`,
provenance=`built_in|signed_extension|local_operator`, label≤512 bytes/128 scalars, at most 32 keywords≤64
bytes each, typed parameter schema≤16 KiB, availability reason key+arguments≤2 KiB,
capability predicate, availability reason, consequence class and exact typed operation. A local-operator entry
is admitted only through foreground schema validation against an already registered typed operation and
declared capability set; repository content, terminal/agent output, imported packages and labels can never
register an entry. Toolbar, palette, context menu, shortcuts and the `CreationCatalog` filter project the same
entry ids/revision. Normalised Unicode
prefix/fuzzy search scans at most 10,000 entries, query≤256 bytes and returns at most 200 with deterministic
score/id order. Invocation revalidates current catalogue/capability/object revisions; labels, terminal/agent
output and arbitrary command strings are never executable catalogue payloads.

Catalogue get/search binds the requesting surface/connection and one closed evaluation scope:
`installation_zero_state(installation revision, optional ExecutionTarget/trust)` or
`workspace(Workspace id/revision, optional selected ViewTarget, ExecutionTarget/trust)`, plus its exact state
watermark. Zero-state exposes New/Open/Clone/SSH adoption and other installation actions before any Workspace
exists; Session creation is unavailable there and remains only in an exact Workspace row/menu. The daemon—not the client—evaluates every capability
predicate against those current facts and returns each entry's available/disabled reason plus the evaluation
watermark; a different surface/selection/target must query or revalidate rather than reuse availability.
`get` uses daemon-minted CatalogueScanId, page≤200 and response≤1 MiB: begin has no scan id and pins catalogue+
evaluation watermark, while continuation carries scan id, next page sequence, predecessor cursor digest and
opaque cursor. Context/revision change gaps the scan. Search scans≤10,000 and returns≤200/1 MiB under the same
pinned context. CatalogueScanId uses the common count/TTL/close/disconnect lifecycle in §11.1.

Local entry CRUD is closed to `register_local_command_catalogue_entry|update_local_command_catalogue_entry|
revoke_local_command_catalogue_entry`; each is LocalDesktopForegroundAuthority, CASes catalogue revision and
can reference only an already registered operation/schema/capability. `ShortcutBindingId` binds platform,
scope=`global|workspace`, normalised chord, exact entry/revision and provenance with state
`active|disabled_conflict|revoked`. One `ShortcutSlot=(platform,scope,chord)` has at most one active binding.
If a second viable binding arrives without an explicit local resolution, the slot becomes
`disabled_conflict`, all contenders are sorted by `(provenance,stable entry id,entry revision)` for display
only and pressing the chord executes nothing. An explicit local foreground replace names the chosen and every
displaced binding/revision and alone restores one active winner. Built-in/signed updates never displace an
active local resolution, arrival order never chooses a winner, and revoke never auto-activates a shadow.
Shortcut lookup invokes the same exact current entry/policy as every other surface.

`AnnouncementId` has its accepted `SignedEnvelopeV1(product_announcement)` identity/trust-store revision,
signed channel/platform audience, revision, issued/expiry time, sanitised inert text≤16
KiB and at most three reviewed HTTPS links. State is `active→dismissed|expired|superseded`; terminals never
reactivate and a higher signed revision alone may supersede. `AnnouncementOperatorIdentity=local(
LocalOperatorIdentityId)|remote(RemoteClientId)` is derived from the authenticated connection, never supplied
by the caller; Companion/headless cannot dismiss. Dismissal is keyed by
`(AnnouncementOperatorIdentity,AnnouncementId,revision)`. Installation retains a
`AnnouncementHighWater=(channel,audience,key_epoch,highest_revision)` plus terminal-id fence, so compaction,
offline replay or an older still-valid signature cannot reactivate an accepted/dismissed/superseded revision.
Invalid signature/audience/size/content is absent.
Display/dismiss/link review creates no operational StatusEvent, Attention, focus, command, setup consent or
update authority.

`open_reviewed_announcement_link` likewise binds AnnouncementId/revision, LinkId/URL hash, signed audience,
isolated Browser policy generation, preassigned Browser NodeId and exact destination Workspace/Session/
optional Group graph revisions. It is a domain creation mutation, never surface navigation; stale signature,
expiry, dismissal, link or destination yields zero Node/network action.

`UpdateIntentId` is Installation-stream owned and is minted before discovery. Its immutable `UpdateQuery`
freezes operation id, channel/platform/architecture/current version, expected update-manifest and
update-package trust-store revisions and the current anti-rollback high-water. Evidence is a closed tagged
union rather than pretending every discovery already has a package:

- `QueryOnly` is valid only for `idle`, `no_update` or `failed(phase=discovery)` and contains no accepted
  manifest/package identity;
- `ManifestAccepted` is mandatory for `available|downloading|downloaded` and freezes the accepted
  `SignedEnvelopeV1(update_manifest)`, its trust-store revision and the manifest-declared expected package
  envelope/size≤2 GiB/digest/minimum compatibility/anti-rollback fields; and
- `ReleaseAccepted` is mandatory for `verified` and every later release-bearing state and additionally freezes
  the separately accepted `SignedEnvelopeV1(update_package)`, its current trust-store revision and exact
  parent-manifest hash. A later `failed|discarded` record retains the highest evidence variant and phase it
  reached. Any state/evidence combination outside this union is invalid.

State is
`idle→no_update|available|failed`, `available→downloading|discarded`, `downloading→downloaded|available|failed|
discarded`, `downloaded→verified|failed|discarded`, `verified→staged|discarded`,
`staged→applying|discarded`, `applying→applied|rollback_required|apply_reconcile_required`,
`apply_reconcile_required→applied|rollback_required|apply_reconcile_required`,
`rollback_required→rolling_back`, `rolling_back→rolled_back|failed|rollback_reconcile_required`, and
`rollback_reconcile_required→rolled_back|failed|rollback_reconcile_required`; terminals do not resume.
The installation retains exactly one current UpdateIntent. Exact same-query/same-trust discovery is an
idempotent lookup. A different discovery while the current intent is nonterminal or still owns package bytes
refuses before network; after a terminal intent has no package bytes, replacement atomically folds its safe
evidence into one of at most 100 rich receipts plus the independent signing/anti-rollback/replay fences.
Receipt capacity is reserved before discovery and a record that cannot compact safely blocks a new intent
before network rather than evicting evidence.

Before the first package byte, download reserves the declared size against one installation-wide logical
package allocation of at most 2 GiB. Download temporary, downloaded, verified and staged bytes are successive
states of that same owner-only allocation—rename/transition cannot double-count another 2 GiB. Insufficient
capacity or a second intent refuses before network/write. Cancel/discard releases bytes only after proved
absence; crash or uncertain cleanup retains the one allocation and exact chunk ledger. Download resume is
chunk/digest fenced; verify precedes stage. Before apply the daemon derives, never accepts
from the client, exactly one `LiveUpdatePlan=daemon_absent_install|compatible_daemon_preserve|
incompatible_daemon_with_live_ptys_refuse|incompatible_idle_daemon_refuse` from current daemon/protocol/PTY
inventory revisions, matching `docs/RELEASE.md`; only the first two may apply. Apply/rollback require
LocalDesktopForegroundAuthority and durable pre-effect intent. Crash/timeout uses lookup-only installation/
backup evidence under the matching reconcile state and never repeats replace or rollback. Discovery/download/
failure never blocks terminal use or installs. Rollback/anti-rollback and restart evidence are durable;
package bytes/credentials never enter logs or export.

`WorkItemActivityEventId` identifies one immutable event with WorkItemId, closed
`WorkItemActivityKind=created|imported|state_changed|metadata_changed|comment_added|assignee_changed|
sync_observed|conflict_detected|conflict_resolved|projection_changed|source_deleted`, actor/provenance,
operation/source receipt, pre/post item revisions, observed timestamp+clock source/freshness, optional provider
effective timestamp and optional stable external event id. `WorkItemActivityDelta` is a matching tagged union:
created/imported refs; exact from/to state; bounded changed-field tags plus redacted safe values; CommentId/
external-comment ref only (never body); prior/new assignee refs; source/sync revision+coverage; ConflictId+
field tags/resolution choices; projection from/to; or source tombstone revision. Encoded delta is≤8 KiB and
unknown kind/delta combinations are rejected. Per item order is `(post_revision,event_sequence,event_id)`;
timestamps are display facts, never ordering. Local commit appends atomically, source echo deduplicates by
stable external id/receipt and never creates a second event. Pages are request-only and contain at most200
events,≤8 KiB/event and≤1 MiB logical with an authenticated≤512-byte WorkItem/revision/checkpoint/order cursor
and complete/partial/gapped coverage. Count and byte limits are independent (200 small events versus128
maximum events); response completion/failure retains zero page bytes. Compaction emits a checkpoint/gap without changing current WorkItem. Activity is permission-
scoped evidence, never another mutation log, runtime or Attention authority.

### 11.7 Reversible presentation history

`ReversiblePresentationOperation` is closed to the exact wire requests `set_tree_expanded`,
`set_tree_expanded_all`, `set_tree_presentation`, `select_tree_node`, `set_surface_view_mode`, `set_inspector_width`,
`set_board_presentation` and `set_terminal_appearance`; only those requests create history. It cannot
contain hierarchy/domain data, runtime/input, provider/source/SCM, context, Attention, grant/credential,
lifecycle or destructive effects. The `set_tree_expanded_all` inverse stores the prior
`expansion_default` and complete bounded exception set, never a hierarchy-row enumeration.
`LocalOperatorIdentityId` is a non-PII installation-minted stable id created
only by authenticated local-control bootstrap, never accepted from a request and retained until installation-
data deletion; each local connection binds it. `PresentationHistoryOwner=(LocalOperatorIdentityId,surface_id)|
(RemoteClientId,RemoteSessionId,surface_id)` is daemon-derived from that authenticated connection/current
surface/session, never caller-selected, and partitions every Workspace history. Every record stores operation/
owner, exact surface/object and history generation, pre/post revisions, canonical inverse and receipt. Each
Workspace retains at most 200 entries total across owner-partitioned undo/redo stacks. Undo/redo requires the
same live owner/surface/session and CASes current object+history generations, so one client or surface cannot
undo another. A new edit after undo clears only that owner's redo, concurrent mismatch marks the affected
entry invalidated, deletion makes it unavailable and compaction retains a checkpoint. Restart preserves valid
history without transferring ownership. Neither undo nor redo may synthesize or replay an excluded effect.

## 12. Local voice input

`docs/LOCAL_VOICE_INPUT.md` is normative. The common path is hold shortcut, speak, release, edit the inline
draft and use the normal send gesture. Capture and inference remain on the physical foreground device in a
crash-isolated worker; the model is an optional explicit verified download. There is no cloud, remote-host,
auto-send, approval or voice-command fallback.

The draft freezes the exact surface, Node, instance, attempt, pending interaction and input owner. Selection
changes cannot retarget it. Voice never creates, orders, acknowledges or resolves Attention, and automatic
focus defers while capture/review is active without hiding new work.

M15 admits one device-scoped MicrophoneLease and one frozen DictationTarget installation-wide. PCM is mono
signed-16-bit little-endian at16 kHz:≤300 seconds/9,600,000 bytes reserve one 10-MiB buffer, with exactly two/
20 MiB for active capture plus pending inference. One hypothesis is≤32 KiB. One live-or-cleanup-pending
SpeechWorker reserves≤512 MiB and the same family/shared amount before spawn; inference ends at300 seconds,
graceful shutdown gets two seconds and a hang transfers—not duplicates—the slot/bytes to ProcessCleanupCharge.
The completed transcript uses the existing one≤32-KiB LocalInputDraft/Surface (eight/client,64/2 MiB global)
plus≤4 KiB voice metadata (64/256 KiB global), so it is not copied into a second body. Existing nonempty draft,
lease/target/buffer/worker/item/family/shared N+1 opens no microphone, spawns nothing and preserves current text.
Same-window daemon reconnect retains the local draft but every Insert/Send requires an explicit fresh target;
Surface/window/client-process exit drops it.

The worker sandbox has no daemon/control socket, repository/workspace, provider credentials, arbitrary
filesystem or outbound network access; it receives only the verified model descriptor and a bounded audio
stream. Recognition strips or normalises NUL, ESC/C1, unsafe bidi/invisible controls and CR/LF before the
editable draft. PCM, hypotheses and unsent drafts remain memory-only and are absent from protocol, store,
journal, logs, diagnostics, crash reports and exports. Canary tests cover every one of those sinks and prove
worker compromise cannot mutate a Session or read workspace data.

## 13. Scale, resource pressure and accessibility

The post-v0.1 control-plane envelope is at least 50 concurrent Sessions, 100 live runtimes, 1,000 expanded
or historical nodes and simultaneous Attention changes without terminal input starvation. The tree is
virtualised; hierarchy summaries are bounded; only visible Node Views subscribe to large transcript,
terminal, log or media payloads; background status streams use coalescing and backpressure.

The durable semantic core is itself admission-bounded, including empty containers. Installation admits at
most 1,024 non-deleted Workspaces, 10,000 Sessions (1,024/Workspace), 100,000 base Nodes
(10,000/Session,50,000/Workspace), 50,000 AgentInstances, 10,000 live RuntimeAttempts, 100,000 current+ended
RuntimeAttempt detail records, and 4,096 durable Panes (64/Session). Group and WorkItem shells are charged
Nodes, not free parallel records. Every Workspace, Session, base Node, AgentInstance, retained RuntimeAttempt
detail, Pane, Layout, TeamMembership, DependencyEdge and Flow/relationship core record also charges one shared
200,000-record/1,024-MiB encoded envelope; ordinary metadata is≤64 KiB/item and Layout is≤256 KiB for≤64 Pane
descriptors. Separately bounded Note/content/body/revision categories still charge their own pools and cannot be
smuggled into the core record.

Count plus worst-case encoded bytes reserve atomically before identity publication, filesystem/worktree,
external launch/provider/network/file effect, PTY/renderer allocation or graph mutation. Eligible ended attempt
detail may first fold into its constant-size aggregate; no live/current/referenced semantic record is evicted.
If any family or shared reservation fails, N+1 returns the exact saturated key and performs zero effect. An
unpausable external discovery beyond capacity records a bounded coverage gap and disables authoritative
absence/control; it never grows silently or invents empty state. End/delete remains total because every
possible survivor already owns its recovery reservation: removing/tombstoning core rows releases their count
and bytes only after the durable nonreuse/reference fence commits.

At most 256 terminal Panes retain history. Each history-enabled Pane reserves its complete 8-MiB journal and
4-MiB checkpoint allowances before PTY/process launch, under exact 2,048-MiB and 1,024-MiB installation pools;
the 257th refuses rather than silently disabling history. Rotation/checkpoint replacement stays inside the
reservation and transfers charge without a double allocation. Nonterminal/resource Panes still obey the
4,096-Pane and semantic-core bounds.

One Installation-owned `PhysicalDiskLedger` classifies every Turn-owned allocation exactly once as operational
store≤8 GiB, StateStream journals≤4 GiB, terminal history≤3 GiB, FileSave temporary≤2 GiB, portable temporary
≤2 GiB, account private roots≤2 GiB, local speech models≤8 GiB, Media pools≤100 GiB installation-wide and
≤10 GiB/Workspace, Transfer/Browser-quarantine temporary≤4 GiB or update package≤2 GiB. These exact class caps
sum to one hard 135-GiB Turn-owned total; family maxima are not silently additive beyond it.

The ledger charges `max(outstanding worst-case reservation, filesystem allocated bytes including sidecars and
declared allocation overhead)`, reports logical-reserved/physical/reclaim-pending per class and total, and
charges a refcounted extent to one current owner. Sparse/COW/compression cannot hide it. Admission reserves
family+total before create/extend; copy reserves both sides while rename/seal/ownership transfer moves one
charge atomically. Unknown bytes below an owner-only Turn root become charged to the
`operational_store(unclassified_quarantine)` substate, consume both the 8-GiB operational cap and total, raise
system Attention and block every new write until classified or removed; this is not an eleventh class. Only separately reported user-owned checkouts/repositories, provider-owned caches and explicit final
external destinations are excluded; no scratch may use them. Boot reconciliation completes before the next
write, and cleanup frees charge only after absence proof.

Turn-worker cleanup uses the same no-early-release rule in memory. `ProcessCleanupCharge` admits at most 4,096
≤4-KiB body-free records/16 MiB, one pre-reserved before each worker spawn. When a connection, Surface or Node
owner disappears before descriptor/process/thread/socket/shared-buffer quiescence, one atomic transfer revokes
authority and moves—not duplicates—the worker correlation, family slot and family/shared-RSS reservation to
that Installation record. Surface dormancy/retirement and End/delete still complete. Only quiescence or
OS-reclamation proof releases inherited capacity; a hung worker stays charged and owner cycling cannot evade a
renderer/decoder/fetch/helper family cap.

There is no generic unbounded “helper”. `AuxiliaryWorkerOwnerKey` is the closed
NotificationHost/NotificationDelivery/RemoteTransport/ContextBrokerRemoteRead/Transfer/Updater/
ProviderBroker/ProviderCollector/Watchdog union in `docs/PROTOCOL.md`, with per-kind counts
1/32/128/128/32/1/32/32/64, a cross-kind128 live-or-cleanup-pending cap,≤128 MiB per worker and≤1,024 MiB
family-wide. The complete kind+family+shared-memory+cleanup reservation precedes process/task/socket/source/
network effect. End, cancellation, deadline or owner/generation loss revokes I/O, waits no more than two
seconds before terminating the owned tree, and releases only on quiescence/OS proof; until then the same
reservation is held by `ProcessCleanupCharge`. Speech, Browser, Media and commit-proposal workers cannot evade
their stricter caps by changing kind.

Renderer pressure may detach or park invisible views, lower preview cadence and evict reconstructible caches.
Turn itself may not terminate, suspend or lose an Agent/subagent/runtime merely to reduce resource use; it
cannot promise to prevent an operating-system OOM killer. Runtime resource policies are explicit Flow/
Session settings, and pressure-based intervention raises Attention with the exact affected nodes and proposed
action. The pressure matrix independently saturates renderer/GPU, memory, PTY allocation, file descriptors,
process limits, store/disk/journal, hook/provider queues, remote backpressure and telemetry collectors. Each
case has preflight/refusal, bounded degradation, exact status/Attention, resync/recovery and proof that Turn
issued no undeclared runtime signal.

Selection and Attention routing remain keyboard-accessible and screen-reader-labelled at every hierarchy
level. Status never depends on colour alone. Reduced motion, zoom, IME composition and focus restoration
apply equally to Node, Flow, Team and companion views. Performance acceptance records p50/p95/p99 input,
route and view-switch latency plus bounded memory/disk/queue behaviour on the fixed minimum profile and
30-minute sustained/burst workload in `docs/PERFORMANCE.md`.

`docs/ACCESSIBILITY_ACCEPTANCE.md` covers every NodeKind WorkSurface plus the CommandCatalogue creation filter, Flow/Team edit and
run controls, integration diagnostics, status history/HUD, file/SCM/conflict views, remote writer handoff and
companion actions. It specifies names/roles/states, focus order/restoration, keyboard alternatives to drag,
live-region announcements and platform/screen-reader evidence rather than relying on one generic row.

Settings are one hierarchical operator surface, not disconnected dialogs. Its denominator is the exact
23-section union and allowed-scope matrix in `docs/PROTOCOL.md`: `agents`, `accounts`, `custom_agents`,
`model_endpoints`, `usage`, `commit_proposals`, `terminal`, `shell`, `runtime_backends`, `work_items`,
`lifecycle_behavior`, `appearance`, `attention_hud`, `notifications`, `voice`, `operator_presence`,
`companion`, `remote_access`, `collaboration_access`, `ssh_targets`, `updates`, `privacy`, `diagnostics`.
The registry is generated against that independently frozen denominator, never from its own current rows.
Grouped sidebar, search, keyboard route and exact deep link select the same canonical editor with deterministic
focus; an unsupported platform row remains reachable and labelled. Every value shows the complete resolver
source `default|Global|Workspace|Template|Session|SurfaceTemporary`; there is no Node settings scope.

A per-section reset computes one≤1-MiB/60-second preview from exact registry+owner+resolved revisions with only
that section's≤256 setting keys. Management actions, unknown unclassified keys, credentials, profiles, trust,
runtimes, Attention and deletion controls cannot enter its patch. The inline Apply is the sole consequence
review, returns one idempotent owner-scoped receipt and preserves every other key/scope/section. Concurrent
edits conflict with current truth; cancel/expiry applies nothing. Search, deep links and reset cannot introduce
another settings authority or a commercial feature gate.

Image and PDF documents are distinct non-editing Views over one exact local or remote FileBackend descriptor,
revision and hash, never an editor/Browser/Media fallback. The closed PNG/JPEG/WebP/PDF path admits≤256 MiB,
≤10,000 PDF pages and bounded geometry into one view/Surface, four/connection and64 installation-wide. Source
blob, two isolated decoder high-waters, four visible tiles/view and extracted-text index have independent
count/byte reservations under shared RSS; scripts, forms, embedded files, SVG/animation, network, clipboard and
filesystem siblings are unavailable. Revision/target/view loss revokes the object URL and decoder before
cleanup. Restore and selection never open or decode automatically. Page, fit, zoom, rotate and bounded search
are pure view operations. Print is a separate desktop-foreground reviewed intent with exact page/layout/printer
revision,≤64-MiB isolated spool, durable pre-dispatch state and lookup-only `printed|not_printed|
submitted_unconfirmed|reconcile_required`; it never retries ambiguity.

Terminal clipboard access is likewise explicit local gesture state, not a terminal capability. Copy uses the
exact visible selection; paste≤64 KiB and≤128-path drop bind the current Surface, attachment/grid, InputLease,
InputSafety and user-gesture generation, then use the one canonical RuntimeInputReceipt. X primary-selection
middle-click is the same path, not a bypass setting. Bodies are memory-only for≤30 seconds and never sync,
persist, enter context or reach a remote target/client. The VT parser consumes terminal-originated OSC 52 read
and write—including fragmented/encoded/oversize forms—before OS access and returns zero bytes.

Optional sound cues distinguish `done` from `needs-you` under visible mute, volume and per-subject cooldown,
start without blocking terminal input and never substitute for visual, screen-reader or structured Attention.
They derive only from fresh canonical result or actionable-demand edges, carry no task text, start≤300 ms when
supported, expire rather than play late and survive no restart. They cannot create, route, acknowledge or
resolve Attention. A reviewed bulk-idle restart similarly remains a separate≤256-candidate operation: its
60-second revision-pinned preview excludes working, waiting/blocked, background/recurrent, child-bearing,
remote, missing-session, primary-checkout and unresumable agents. It dispatches one canonical restart at a time;
per-instance receipts reconcile without duplicate stop/start, cancellation stops before the next candidate and
the final summary accounts once for every included/excluded row.

Eco hibernation is opt-in lifecycle automation, not a pressure escape hatch. Only an exact continuously Idle,
off-screen, local, resumable agent with no prompt, Attention, input lease, child, recurring Flow or background
task may enter the≤256-candidate queue, and no more than two hibernate in a rolling minute. The typed adapter
operation preserves the same Session/multiplexer/scrollback evidence. Returning selection, Attention route or
eligible Flow demand wakes and reattaches automatically—there is no `Start pane` action. Unknown/ambiguous
exit or wake is never replayed; wake failure becomes actionable without deleting continuity or Attention.

Off-screen terminal parking (`PRD-RUN-022`) is presentation lifecycle, not Eco and not runtime lifecycle. A
view switch immediately parks at most one renderer generation in a hard≤12 LRU for five minutes; expiry or
eviction drops only reconstructible renderer/cache state and leaves the attachment and runtime unchanged. A
separate ten-continuous-minute off-screen generation may then retire only the exact client attachment/projection
when the daemon proves the runtime, durable session, output-gap and Attention observation survive independently.
Live plain-shell clients whose view owns the PTY, working/waiting/blocked/unknown work, selected/focused or
Attention-routed Views, active input/resize/IME/dictation/draft state and uncertain detach outcomes are
ineligible. A zero-watcher sweep only reconciles that second path and releases a stale client PTY, never the
durable session. Renderer pressure and detach eligibility cannot widen one another or signal work.

Where the backend proves a stable tmux-equivalent handle, one fixed zero-PTY control client may keep canonical
output observation while the painter is absent. It is generation-fenced to that target/backend/socket/session/
attempt, feeds the same TerminalByteRing with sequence/gap/backpressure and owns no second scrollback or input
lease. A separate shared background-write control channel is lazy, binds only one exact session at a time and
accepts only bytes already admitted by the canonical InputLease/InputSafety/RuntimeInputReceipt path. Painter,
per-session observer and shared writer are mutually exclusive Turn clients; viewer approach retires the control
client before repaint. Timeout, malformed output, lost reply, crash or target/handle drift reports a gap or
possible effect and never kills, respawns or blindly resends. Count, queue, process-RSS and cleanup charges are
bounded, and an FD oracle proves the control path consumes zero PTY devices.

Selection, keyboard reveal or an accepted Attention route automatically attaches and repaints the same
ViewTarget/attempt through the ordinary generation-fenced resync path—there is no `Start pane` action. At most
4,096 UTF-8 input bytes typed during that≤10-second attach window are held against the exact Attempt and
InputLease; confirmed attach flushes once in order, while failure/generation or selection change expires them
with a bottom-status receipt instead of retargeting or splicing a resume command. Restore/reconnect does not
start a missing runtime. Turn intentionally has no detached-session reaper (`PRD-SAF-021`): age, count, memory
pressure, invisibility or missing attachment can only park reconstructible presentation, raise Attention or
offer an explicit exact-owner lifecycle action. Only independently opted-in exact-eligibility Eco may
hibernate; every End/terminate/kill/delete remains typed and operator-authorised.

Diagnostics are a separate current-daemon memory ring, not StatusEvent, terminal history or raw logging. It
holds≤2,048 structured redacted rows/≤8 MiB with exact source, sequence, coverage, freshness and gap. Filter
and copy derive from the same pinned≤256-row/1-MiB page. A local-foreground all/source clear advances the
durable body-free source high-water and invalidates stale pages/subscriptions, but cannot address StatusEvent,
Attention, audit, security, operation or recovery evidence. Overflow/restart is visibly gapped and logging
pressure can never block input, runtime or Attention.

Preparing a bug report is another capability: one≤1-MiB/30-minute local Surface draft freezes an exact
diagnostic selection, inclusion manifest, omission/redaction report and safe system-version allowlist. Prepare,
edit and discard perform no clipboard, file, Browser, network, issue or provider effect. A separate desktop-
foreground review may copy the exact digest, create a new file or open a fixed body-free HTTPS support route in
the isolated Browser; Turn never auto-uploads report bytes or places them in a URL. Credentials, environment
values, raw terminal/transcript/file/provider bodies and unauthorised paths never enter either surface.
Commercial licence activation, subscription upgrade, paid-seat enforcement and entitlement gating are outside
the product and absent from settings, protocol, authority, telemetry and feature availability.
Product telemetry is independently rejected rather than hidden under that exclusion: Turn has no analytics,
install-count, stable installation/client identifier, event queue, endpoint or opt-in/always-on reporting
setting. Startup, runtime, update, collaboration, failure and shutdown emit zero product analytics. Signed
update discovery remains a fixed-purpose identifier-free operation whose schema/storage/receipt cannot accept
generic events.

An unreadable Installation/Workspace store never becomes an empty successful store. Turn confines and hashes
the exact regular descriptor, atomically renames+fsyncs its≤64-MiB original into a non-reused owner-only
quarantine before any default save, then exposes one scoped recovery status. Race, alias, disk/permission/fsync
uncertainty, oversize or the1,024-item/2-GiB operational-store subcap leaves the original untouched and the
owner read-only. Recover validated content, explicitly start fresh with an omission review, create-new export
and destructive discard are separate idempotent intents. Quarantine has no time expiry; neither dismissal nor
successful export implies deletion, and ambiguous replace/export/delete is lookup-only.

Changes from another device/client converge through the same authenticated daemon StateStream, not by watching
or merging the internal store file. A remote create/register mutation adds its canonical Session/Node and
receipt immediately to every subscribed tree; the origin deduplicates only its exact operation echo. Other
events apply in sequence or trigger automatic gap+resnapshot. A local selection remains on the same ViewTarget,
live runtimes are neither replaced nor relaunched, domain conflicts use exact CAS, and locally dirty drafts/
previews are marked stale rather than overwritten. Unexpected internal-store filesystem replacement goes
read-only/quarantine; user files enter only through reviewed PortableImport. Thus cross-device immediacy does
not create a second JSON/watcher authority.

Peer chat is a short-lived co-presence projection, not an agent message or context handoff. Each text update is
bounded, redacted, rate-limited and fenced to the exact Workspace/ViewTarget plus sender/session revision; it
expires or retracts on close, departure, view change or disconnect. It is never durable and grants no terminal
input, command, permission, Attention, notification, focus, provider or control authority.

## 14. Authority, privacy and failure semantics

All state-changing operations are typed, authenticated, generation-fenced, idempotent and scoped to exactly
one declared `StateStreamKey` owner plus every referenced object, authority and target generation. A
cross-stream mutation supplies and atomically fences its complete revision vector; Installation-owned work
never invents a Workspace/Session/FlowRun scope. Capabilities are least-privilege and short-lived when delegated.
Administrative control, context brokerage, remote runtime access and companion access use different tokens.

Portable Workspace/Flow content is not machine trust. A shareable definition may contain inert node shape,
roles, prompts, commands as unadopted text, dependencies and presentation, but it cannot carry credentials,
account bindings, local executable overrides, consent, capability grants, host identity or a decision to run.
Import shows those differences and creates no runtime until a local adoption receipt binds the definition to
known tools, paths, execution targets and policy.

An export uses package-local `PortableId`s only. Import always creates a fresh package-map namespace. Its
closed destination is `new_workspace(preassigned WorkspaceId,Installation revision)` or
`existing_container(WorkspaceId,optional SessionId,optional GroupId,exact graph revisions)`. The first remints
the package Workspace and every child id; the second maps only the package root to the explicitly selected
existing container and remints every imported Session, Node, Team, FlowDefinition and relationship id. A live
or executable `FlowRun` is never portable. An optional `PortableRunReport` is inert bounded history: redacted definition/step labels, declared
terminal summaries and artifact content hashes addressed only by package-local ids. Import renders it as a
read-only Resource; it cannot satisfy a dependency, prove completion, resume/retry work or supply runtime,
operation, revision or authority identity. Runtime attempts, provider conversations, NativeJob keys, PIDs,
PTYs, operation ids, receipts, revisions, grants, tombstones and machine/host ids never cross the boundary.

`PortableContextPacketArtifact` is the only ContextPacket representation allowed in that package. It contains
a package-local artifact id and source labels, schema/sanitizer versions, bounded redacted older digest,
bounded exact recent tail, optional inert artifact bytes/refs, selection/budget/omission/redaction manifests,
UTF-8/body/framing hashes and untrusted provenance timestamps. It contains no local ContextPacketId,
destination, runtime/conversation/operation/revision/grant/credential/host identity or executable command.
Package import remints an inert `ImportedContextArtifactId`; it is read-only and cannot deliver, satisfy a
dependency or seed a launch. A separate fresh `prepare_context_packet` against the destination's current
Workspace/Session/instance/attempt and budget must review/select from that artifact before normal delivery.

Portable export/import are typed idempotent sagas. `PortableExportId` follows
`prepared→assembling|cancelled`, `assembling→review_required|failed`,
`review_required→committing|cancelled|stale`, `committing→written|reconcile_required`, and
`reconcile_required→written|not_written|reconcile_required`. `PortableImportId` follows
`prepared→validating|cancelled`, `validating→review_required|refused|failed`,
`review_required→committing|cancelled|stale`, `committing→committed|reconcile_required`, and
`reconcile_required→committed|not_imported|reconcile_required`; terminals never resume. Both pin a regular-
file descriptor identity, package hash/schema/size≤64 MiB and reviewed manifest. Durable intent precedes
atomic create-new write or remint transaction; crash recovery checks the exact output/import receipt and never
rewrites/reimports. Commit-import requires a fresh local-foreground destination review and exact destination
Workspace/Session/Group revisions; hostile ids/unresolved refs become inert errors, never caller-selected ids.
References resolve through the package map, unresolved references are inert errors and no local collision
can update or resurrect an existing object. The origin content hash is provenance, never identity or trust.

PortableExport is owned by its one exact source Workspace stream; PortableImport is Installation-stream owned
because validation precedes destination choice. Commit-import uses one exact Installation+destination
revision vector and never transfers ownership of the saga. At most 16 nonterminal exports and 16 nonterminal
imports exist installation-wide. Preparation reserves one active slot, one eventual terminal receipt and the
full declared package allowance before assembly/validation; N+1 or aggregate temporary capacity failure
refuses before reading, writing or reminting. Export/import assembly and validation temporaries share one
owner-only 2-GiB installation cap, each package remains≤64 MiB, and final external output is never counted as
a Turn temporary. Cancel/terminal cleanup releases bytes only after proved absence; committing or
reconcile-required evidence and bytes never age out. At most 10,000 rich terminal receipts remain for 30 days;
compaction first preserves minimal operation/package/path/destination/result replay and collision fences.

Multiple surfaces and clients consume authoritative daemon revisions. Reconnect starts from a bounded
snapshot plus journal position rather than a best-effort stream. Durable tombstones prevent a disconnected
client from resurrecting ended/deleted entities. Conflict policy covers nodes, every relationship family,
Flow definitions/runs and lifecycle operations; no whole-document last-write may silently erase an unrelated
edge. A runtime has one explicit input/resize writer lease at a time. Other viewers receive bounded catch-up
and live output, but their bytes cannot interleave with the writer; lease handoff is visible and generation-
fenced.

`StateStreamKey` is the closed tagged scope
`Installation(daemon_generation)|Workspace(daemon_generation,WorkspaceId)|
ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)`. The sole normative closed assignment
of every durable and ephemeral record family to those streams is the ownership inventory under
“Accepted multi-client, companion and remote-backend target” in `docs/PROTOCOL.md`; this document incorporates
that inventory by reference and intentionally maintains no second object list. In particular, adding a record
family without adding it to that one inventory, or assigning it differently anywhere else, fails acceptance.
Installation-global catalogues/registries, Workspace semantic graph/history/intent state, ExecutionTarget
backend/effect state and connection/Surface-scoped ephemeral state follow those tagged categories; a
cross-stream create/remint is one revision-vector transaction and never transfers ownership implicitly.

A snapshot closes at domain revision `R` with a journal watermark; events
begin at `R+1`, are strictly sequenced inside that domain and clients acknowledge each subscribed revision.
A gap, compacted cursor or generation change forces a fresh snapshot for that domain before mutation. There
is no invented total order across independent domains.

Durable state journals are byte-bounded as well as time-bounded: Installation≤512 MiB, each Workspace≤256
MiB, each ExecutionTarget≤128 MiB and all streams combined≤4,096 MiB. Before a local mutation effect, the
daemon reserves its event plus every cross-stream barrier fragment. It first snapshots/compacts eligible
segments, publishes the new per-stream minimum accepted revision and then either admits the whole vector or
refuses before effect. External observation overflow commits one current-state/gap marker inside the
producer's reserved slot and requires resnapshot. Compaction may remove incremental history after 30 days or
the byte boundary, but never current objects, nonterminal/uncertain intents, operation replay, deletion/nonreuse
fences or unresolved multi-stream barriers. A client below any minimum revision cannot delay compaction or
mutate; it receives `state_gap` and a bounded vector snapshot.

Every cross-domain reference carries the exact source-domain revision. A route validates both Attention
Queue revision and subject-domain revision; a Workspace default pointing to an AccountProfile validates the
Workspace and ExecutionTarget revisions. A mutation spanning domains supplies an expected revision vector
and transaction id; the daemon durably prepares every domain fence, commits one result/new-revision vector
or none, and appends the same barrier/transaction receipt to each affected stream. Clients do not expose a
partial cross-domain result: they apply it only after all named barrier revisions arrive or obtain a bounded
multi-domain snapshot at that vector. Recovery resolves the transaction receipt before any retry.

A permission issue/claim/dispatch supplies one exact `PermissionAuthorityVector=(Installation revision,
Workspace revision,ExecutionTarget revision)`: remote grant/session lives in Installation, interaction/claim/
receipt in Workspace, and permission fact/typed transport in ExecutionTarget. The daemon locks and revalidates
all three, commits the shared transaction barrier or none, and never treats one fresh component as authority
for stale peers. RemotePresence is memory-only connection state outside every durable stream/journal.

Within a domain, independent edge add/remove operations commute by edge id; scalar edits use compare-and-
swap; immutable FlowDefinition revisions and append-only FlowRun receipts never merge; a deletion tombstone
wins over every older update; lifecycle effects converge by operation id. Offline clients retain drafts only
and may not replay queued mutations after revalidation failure. IDs are never reused, and a compacted
deletion fence rejects stale resurrection permanently.

An input lease has subject, client/surface, generation, acquired/renewed time and 15-second expiry, renewed at
most every five seconds. Disconnect stops renewal; no client can steal a live lease. Explicit handoff commits
the new generation atomically and both clients see it before new bytes are accepted. A draft stays only on
the client that owns it; Turn never promises to transfer or restore another client's unsent draft.

Optional operator-to-operator sharing is a separate authenticated capability, never implied by remote
execution. Invitations are short-lived and bind an end-to-end encrypted channel to one Workspace/Session and
declared read/input/control scopes. Presence and typing indicators are ephemeral; durable mutations still use
the authoritative journal. A remote viewer cannot obtain daemon administration, provider credentials,
checkout writes or the runtime input lease without an explicit grant and visible handoff.

A full `RemoteOperatorSurface` is distinct from the reduced companion action set. It may render the same
canonical hierarchy, WorkSurface/Node Views, status history, CommandCatalogue creation filter, Flow/Team views, terminal
stream and Attention routes through the versioned daemon protocol; headless clients expose the same objects
as structured commands/events. Capability negotiation makes unsupported rendering or control explicit.
Remote foreground may request ordinary revision-fenced mutations and an input lease within its invitation
scope. A versioned server-side allowlist intersects protocol variant, invitation scope, target policy and
current evidence; every variant not explicitly present is denied even if a client can encode it. The only
remote permission path is the single-use typed response in §10. Credential/secret entry, daemon
administration, authority/grant issue, host trust/key rotation, destructive lifecycle, repository
credential/profile/grant administration, push/pull/commit-and-push, merge/conflict resolution,
discard/cleanup and `publish_repository|get_repository_publish|reconcile_repository_publish` remain absent and fail server-side. Scoped stage/unstage/commit/fetch/
branch operations explicitly present in the registry remain available to an authorised full GUI. While an adapter reports a current sensitive typed
interaction, raw PTY/input bytes from every remote surface are blocked for that attempt; only the matching
revision-fenced typed response can pass. For a generic external TUI whose prompts cannot be classified, Turn
makes no sensitive-input prevention claim and remote raw input is disabled by default, enabled only by a
separate consequence-labelled invitation capability. These guarantees cover Turn-managed typed operations,
not commands a separately authorised shell user could type. Loss,
reconnect, snapshot/gap recovery, focus routing, unread state and writer handoff have the same semantics as
the desktop rather than a second remote state model. Deployment requires authenticated encrypted origin,
CSRF/replay protection, no ambient provider credentials in browser storage, bounded subscriptions and a
visible local revocation/audit surface.

Turn does not persist raw microphone audio, voice drafts, provider credentials, complete environment values
or unbounded transcripts. Context, terminal history, resource content, usage/account metadata, launch receipts
and remote tombstones follow the closed inventory, export, retention and deletion contract. Provider or
terminal downstream retention is disclosed before handoff/message delivery.

Control-plane retention is finite: compaction keeps at most the newest 50 unreferenced FlowDefinition
revisions per definition, deletes every unreferenced revision older than 180 days even when fewer than 50
remain, and refuses a new unreferenced revision once the Workspace's 64 MiB revision-body cap cannot be met;
revisions referenced by retained runs remain and count against the cap, so saturation fails closed rather
than deleting evidence. Sync journals compact after every active client has
acknowledged or 30 days, with permanent id deletion fences; redacted status/diagnostic history keeps 1,000
entries per Workspace for seven days; input-lease metadata keeps seven days and no draft bytes; expired share
invitation/audit metadata keeps 30/180 days respectively and ephemeral presence is never stored. Explicit
pin/export and legal-policy overrides are visible, scoped exceptions. `docs/PRIVACY.md` owns the complete
category table and deletion proof.

Failures preserve what is known and expose recovery:

- an uncertain launch is probed only by its preassigned identity and never duplicated blindly;
- an uncertain input, context packet or message is not retried automatically;
- stale revisions cannot act on a replacement attempt;
- disconnection leaves semantic work visible and marks runtime truth stale/lost;
- deleting/ending a Turn record does not claim that unreachable external processes or artifacts were erased;
- every partial Flow operation has a durable receipt and a deterministic retry/compensate/manual state.

## 15. Delivery order

This contract is delivered as coherent verticals, not isolated widgets:

1. provider-neutral identities, six-adapter topology/capability evidence, quota-only connectors and model-route
   profiles;
2. one recursive canonical hierarchy, separate CheckoutScopes and one WorkSurface plus exact Attention routes;
3. truthful launch receipts, lifecycle attempts and durable/local/remote runtime plus ResourceInventory seam;
4. one CommandCatalogue creation-filter/WorkspaceOnboarding path, FlowRun and worktree-safe typed control;
5. context transfer, messages, Teams, dependency execution and verification flows;
6. quota/context/resource/name telemetry and companion projections;
7. resource Node Views, background Attention delivery and local voice input;
8. frozen capability-ledger, scale, live-provider, remote, packaged accessibility and failure-recovery proof.

A vertical may ship incrementally behind capability/status labels. It may not claim provider parity,
restoration, zero subagents, delivery, continuity or completion without the corresponding evidence.

## 16. Two independently falsifiable completion gates

### 16.1 Specification integration complete

The product-specification goal is complete only when, on the same commit:

1. the versioned manifest fixes every requirement id, acceptance id and hashes of its normative outcome and
   oracle; removal or semantic change names an accepted ADR in the manifest revision;
2. the neutral capability-coverage ledger fixes the audited source snapshot and gives every discovered
   feature a stable id, evidence digest, `adopted|adapted|rejected|irrelevant` disposition, rationale and linked
   requirement/acceptance/ADR; unknown disposition, silent deletion, link drift or semantic weakening fails;
3. every inventory row maps one-to-one to a non-empty proof obligation and the mutation tests prove that
   paired deletion, requirement weakening and trivial-oracle substitution fail the gate;
4. the contract, Product, Architecture, Protocol, Roadmap, decisions and detailed specifications use one
   ontology and contain no unresolved contradiction or relevant capability gap;
5. at least two non-author adversarial audits of the final frozen diff return no P0/P1 finding; every P2 is
   either closed or named with a justified product boundary;
6. `make verify` and the specification gate are green and the exact commit is merged to `main`.

This gate may pass while implementation statuses remain `baseline`, `partial`, `target` or `conflict`. It is
a claim that the accepted destination and its tests are complete, not that the destination exists yet.

### 16.2 Product realisation complete

The product is complete only when every row is `implemented`, has a matching immutable implementation-
evidence record whose implementation commit exists in the current history, and its requirement-derived ACP
entrypoint plus all named end-to-end, live-provider, recovery, remote-security, privacy, scale and packaged
accessibility suites execute on the clean completion commit and produce hash-matching current-run artifacts.
Advertised capability manifests must match current provider/live evidence; CI must be green; an independent
final audit must find no open P0/P1/P2; and the completion commit must be merged to `main`.

`make product-spec-acceptance` proves §16.1 structure and frozen content.
`make product-completion-acceptance` is deliberately red until §16.2 is true; it permits `implemented` as the
only final state and rejects any remaining baseline/partial/target/conflict. The Roadmap continues to name
which verticals are accepted, in progress or implemented so neither green command can be misrepresented.
