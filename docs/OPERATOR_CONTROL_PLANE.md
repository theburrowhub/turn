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
3. **Selection changes a view, not work.** Clicking a node neither starts nor stops a process, changes Layout,
   sends input, acknowledges Attention nor mutates a Flow.
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
    ├── GroupNode                (may contain one primary presentation of a member)
    ├── AgentNode ── AgentInstance ── RuntimeAttempt*
    │   ├── SubagentNode ── AgentInstance ── RuntimeAttempt*
    │   └── ProcessNode / LogNode
    ├── ToolNode ── RuntimeAttempt*
    ├── JobNode ── NativeJob ── NativeJobIteration*
    └── ResourceNode / WorkItemNode
```

A Workspace is a persistent project boundary. A Session is one operator-recognisable unit of work. Running a
Flow creates or reuses exactly one Session and records a `FlowRun`; it does not introduce a parallel
navigation root. Every visible child has a stable `NodeId`, one `NodeKind`, one owning Session and at most
one presentation Group.

`RuntimeAttempt*` means zero or more attempts over the lifetime of an agent/tool; an observed child may
exist before any launch/runtime evidence arrives.

The canonical kinds are:

- `Agent` and `Subagent` for semantic agent identities;
- `Shell`, `Command`, `Tui`, `Service`, `Process` and `Log` for terminal/runtime work;
- `Group`, `Team`, `Flow` and `Job` for explicit organisation, Turn orchestration and provider-native
  scheduled/background work without conflating those authorities;
- `WorkItem` for local or externally sourced work records; and
- `Note`, `File`, `Diff`, `Web`, `Browser` and `Media` for typed resources, with inert `Web` preview and
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
| `GroupMembership` | explicit single Group presentation | yes, as an operator override | no | no |
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
then its owning Session. Evidence strength and source priority choose a tier; if two different parents remain
equal at the strongest tier, placement is ambiguous and stays unassigned rather than using an arbitrary id to
invent parentage. Stable edge id orders only the displayed competing references. Team and Flow membership, non-winning ancestry and lineage render as
activatable references to that row, never aliases. Moving a Group changes only the primary display edge;
semantic child counts traverse SpawnEdges, process counts traverse ProcessEdges and neither changes. A
fixture combining spawn parent, process parent, Group, multiple Teams and Flow membership is the canonical
placement oracle.

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
| ConversationKey ↔ RuntimeAttempt | one canonical provider/profile/target/namespace conversation may span verified ordered attempts; one attempt names at most one ConversationKey; across the installation one ConversationKey has at most one current AgentInstance owner, while endpoint generation only fences transport |
| RuntimeAttempt ↔ durable handle | an attempt has zero-or-one current handle; one `(ExecutionTarget, backend, handle, generation)` binds one current attempt and reuse creates a separately evidenced generation |
| RuntimeAttempt ↔ root PTY | an attempt has zero-or-one root PTY; one PTY/backend generation belongs to exactly one attempt, though many Panes may view it |
| RuntimeAttempt → OS Process | zero-to-many; at most one declared root process and every surfaced process identity belongs to exactly one current attempt/owner or remains an explicit unresolved observation |
| Layout → Pane → runtime/input owner | a durable Pane belongs to exactly one Session Layout and binds zero-or-one content/runtime owner; an owner has zero-to-many viewers; a temporary Pane belongs to exactly one Surface instead of a Layout |
| ClientConnection → Surface | one connection generation owns one-or-more Surfaces; each live Surface belongs to exactly one connection generation and is never transferred implicitly |
| runtime/input owner → InputLease | exactly zero-or-one current input/resize lease holder; viewers without it are read-only |
| Session → FlowRun | zero-to-many immutable runs; a Flow Node projects exactly one run and a work Node records each producing step/run reference |
| WorkItemKey → WorkItem Node | one `(source_id, source_profile_id, project_namespace, external_item_id)` maps to at most one canonical Node; a Node has zero-or-one current external binding plus retained rebinding lineage |
| Job Node ↔ NativeJob | exactly one-to-one; one job has ordered stable iteration ids, and any runtime/agent spawned by an iteration remains a separate referenced Node |

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

Selecting a Session restores its exact Layout, zoom and focused Pane. Selecting a child replaces the centre
with its unique content while leaving that Layout untouched. Back/forward navigation and return-to-Session
use stable keys and revisions, not widget history.

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
| Group | member overview and aggregated Attention only; it owns no runtime or checkout |
| WorkItem | canonical local fields or source-of-truth fields/local overlay, sync revision/staleness/conflict, comments/assignees and linked work without runtime authority |
| Note/File/Diff | inert bounded content, canonical source and privacy/checkout facts |
| Web/Media | explicitly loaded inert isolated preview with origin/source and no ambient credentials |
| Browser | isolated interactive navigation, address/history, popup/download disposition, storage/permission state and reviewed local/loopback origin policy |
| Job | provider-native background/recurring/scheduled identity, schedule, iterations, survival, status and capability-gated controls |

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

The WorkItem transition table is exhaustive; every move carries the expected item/source revision and an
idempotent operation id:

| From | May transition to |
| --- | --- |
| `backlog` | `ready`, `cancelled` |
| `ready` | `backlog`, `active`, `blocked`, `cancelled` |
| `active` | `blocked`, `review`, `done`, `cancelled` |
| `blocked` | `ready`, `active`, `cancelled` |
| `review` | `active`, `blocked`, `done`, `cancelled` |
| `done` | `ready` only as an explicit reopen |
| `cancelled` | `backlog` only as an explicit reopen |

A `WorkItemSource` may bind canonical cards to an external issue/work system without becoming a second
tree or queue. `WorkItemKey = (source_id, source_profile_id, project_namespace, external_item_id)` is stable
and never derived from title, URL or list order. Its versioned adapter declares source/account/target identity,
field and state mappings, assignee identity mapping, authority per field (`source|turn|reviewed_merge`), supported
filters/sorts, page/cursor bounds, cache TTL, webhook/poll mechanism, request/rate budgets and credential
reference. Snapshots and deltas carry source epoch, cursor/watermark, item revision/ETag, freshness and
coverage; partial or rate-limited reads may add/stale cards but never prove deletion or `exact(0)`. Create,
edit, comment, assign, transition, close and reopen are separately advertised capabilities. A write uses
compare-and-swap against the exact source revision and produces an external receipt before the local
projection advances; timeout after a possible write enters `reconcile_required` and is never replayed.
Conflicting field revisions remain side-by-side in one exact conflict view until an authorised foreground
resolution or a declared deterministic per-field policy commits a new source revision. Source deletion,
permission loss, unknown assignee, mapping changes, cursor gaps, offline cache and rate limiting have
distinct states. Credentials stay in the target's keystore/broker, never in a card, export, log or client.
Closing or reopening a card follows the source mapping and the table above; dismissing its Attention entry
or deleting Turn's projection cannot close or delete the external item.

### 3.2 Window chrome and feedback

The application icon and Turn name lead the top bar. Creation/navigation icon buttons follow on the left.
Connection, active execution target, daemon state, effective app version and other global metadata are
right-aligned and each fact appears once. `+ Session` is scoped inside its Workspace row/menu, not outside the
tree.

Transient operational progress, warnings and errors use the bottom status bar with scope and recovery
action. They do not displace content at the top. Ending or deleting a Session removes it from the active tree
immediately; survivor cleanup is reported separately and cannot veto the operator's intent.

`StatusEvent` is not Attention. It carries severity, scope, operation id, text key/arguments, progress,
created/expiry time, optional recovery action and announcement policy. The bar shows the highest-severity,
most-recent event plus an overflow count; activation opens bounded history ordered by severity/time. Success
expires after five seconds, progress is replaced by its terminal event, warnings persist until superseded or
dismissed and errors persist until their operation is reconciled. A status becomes Attention only through a
separate typed actionable demand. Screen readers announce a progress operation at start and terminal state,
not on every update; concurrent lower-priority events remain reachable without notification spam.

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

One declarative `CreationCatalog` supplies the Workspace `+`, toolbar, command palette and contextual menus.
It contains capability-gated entries for Session, Flow, Agent, Shell, command/TUI, service, log, Group, Team,
WorkItem/source, provider-native Job, Note, File, Diff, inert Web preview, isolated Browser, Media, worktree
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
children as they start. Cancellation restores the invoking tree selection. Contextual surfaces may group and
order entries for available space, but action id, label, defaults, validation and capability result always
come from the same catalog revision.

First run and later diagnostics use one resumable setup checklist: discover supported CLIs and remote
targets, show exact detected versions/capabilities, guide installation or authentication without collecting
credentials, request notification/microphone permissions only when the related feature is invoked, and
explain degraded integration plus remediation. Setup can be skipped without disabling generic terminal use.
Every probe is bounded, read-only unless a consequence is explicitly shown, cancellable and recorded as a
redacted diagnostic receipt; remote trust and host identity require a separate explicit adoption.

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

Start policies are a closed set: `manual`, `with_run`, `after_success`, `after_result`, `all_of`, `any_of` and
`bounded_recurrence`. Dependency readiness is evidence, not permission: only the Flow policy that the
operator reviewed up front may consume it automatically.

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

Pause changes no active StepAttempt state. A dependency/current policy moves `blocked → ready`; the separately
idempotent start operation moves `ready → starting`. A retry never reopens a terminal attempt: it creates a
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
`update_resource`: the grant fixes permitted `Note|File|Diff|Web|Media` kinds, owning Session/Group, typed
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
TTL applies before submission and never rewinds a submitted message or its evidence. Per-destination FIFO
capacity and byte/count budgets are declared; overflow refuses visibly rather than dropping. Delivery
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
events remain non-blocking: overflow records a per-source gap and schedules an asynchronous bounded resync;
it never silently drops while preserving an exact count and never blocks the Agent, terminal or UI.

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

Claude Code, Codex, Gemini, OpenCode and future/custom agents implement the same capability vocabulary:

`launch`, `resume`, `branch`, `stop`, `structured_status`, `questions`, `permissions`, `subagents`,
`transcript`, `context_usage`, `provider_quota`, `model_switch`, `messaging`, `context_transfer`,
`shared_identity`, `durable_attach`, `delegated_control`, `native_jobs`, `conversation_inventory`,
`title_read` and `conversation_rename`.

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
never becomes their semantic identity. `ConversationKey = (provider_id, AccountProfileId,
ExecutionTargetId, provider_namespace, normalized_provider_conversation_id)` is canonical across the whole
installation. Each `RuntimeEndpointBinding` names exactly one ConversationKey, endpoint generation,
AgentInstance, RuntimeAttempt and proof. The store permits at most one `current` owner of a ConversationKey
across every endpoint and at most one current binding for an instance; a second claim is rejected before
input, transcript or context authority is issued. Siblings never share input, transcript cursors, context
grants, quota attribution or Attention subjects merely because they share the service.

`BindingState` is independently closed: `proposed → current|refused`, `current → stale|unbound|retired`,
`stale → current|unbound|retired`, and `unbound → proposed|retired`; `refused|retired` are terminal for that
binding id. Endpoint mismatch, generation discontinuity, ownership conflict or stale proof changes only
BindingState and connectivity. It never changes RuntimeAttempt Lifecycle; `Lost` still requires separate
bounded absence evidence from the RuntimeBackend/provider.

Warm attach enumerates the endpoint's own conversation inventory, proves the exact binding and launches
nothing. A service reconnect preserves bindings only when endpoint fingerprint, generation continuity and
conversation ownership all verify; otherwise every affected binding becomes `stale` independently. A
service crash/restart cannot merge siblings or silently cold-start them. Fallback to a dedicated runtime or
provider resume is an explicit per-instance operation that creates a new RuntimeAttempt and retains lineage.
Duplicate conversation claims, cross-account handles, late events from an old endpoint generation and one
sibling attempting another's operation are refused and surfaced as scoped diagnostics/Attention when
actionable. Endpoint backpressure and failure are isolated so one conversation cannot block unrelated
instances or the hierarchy.

A capability-gated `ConversationInventory` is separate from live RuntimeInventory. It queries one exact
provider/AccountProfile/ExecutionTarget/namespace and returns bounded pages of private current and historical
conversation descriptors: ConversationKey, provider title when `title_read` is supported, created/updated
time, native status, model/mode hints, ownership match, resumability, source revision, freshness and explicit
unknown fields. Search declares supported server/client predicates, normalisation, result and scan bounds;
cursor gaps, truncation, rate limiting and partial coverage can never prove absence. Results from different
profiles or targets are never coalesced, cached into another profile or exposed to a surface without its
read grant. Matching is exact-key first and otherwise advisory; title/text similarity never binds work.

`Adopt conversation` is a foreground, revision-fenced operation that creates one stopped canonical
Node/AgentInstance and proposed/current binding without launching or sending input. `Resume conversation`
is a separate foreground operation with account/target/model/cwd/containment preflight; it creates a new
RuntimeAttempt only after the provider proves resumability and current ownership. A historical or
unsupported result remains viewable metadata and cannot be fabricated as live. Conversation title read and
rename are separate capabilities: reading never implies write; rename uses an expected provider revision,
idempotent operation id and provider receipt, reports requested/effective title, and degrades independently
when unsupported, stale, rate-limited or ambiguous.

### 6.3 Provider-native jobs

A provider may expose scheduled, recurring or background work through `native_jobs`; this is never inferred
from terminal output and never conflated with Turn's `bounded_recurrence` Flow policy. `NativeJobKey =
(provider_id, AccountProfileId, ExecutionTargetId, provider_namespace, provider_job_id)` has one current
Job Node. The descriptor records provider schedule/revision/time zone, requested/effective model and flags,
native state, next/last run, creation/ownership provenance, capability facts and freshness. Each provider
execution is a stable `NativeJobIteration` child with ordinal/native id, scheduled/started/finished times,
result/error and optional exact AgentInstance/RuntimeAttempt link; one iteration is never the job identity.

The normalised native state is `scheduled|running|paused|completed|failed|cancelled|unknown`; adapters retain
the native value and declare every mapping. Provider evidence, not app presence, determines whether a job or
iteration survives Session end, daemon restart, host reboot or provider disconnect. List/create/update/
pause/resume/run-now/cancel/delete are independent capability-gated operations with expected job revision,
profile/target generation and durable receipts. An ambiguous mutation is reconciled by NativeJobKey before
any retry. Dismissing Attention, hiding a card or deleting Turn's projection never cancels/deletes the
provider job; destructive provider deletion is a distinct consequence-labelled foreground operation.
Questions, permissions, failures and unread results from an iteration enter the same Attention Queue with
that exact Job/iteration/attempt route. A job is imported/exported only as inert configuration text and must
be locally adopted; provider job ids, authority and schedule activation never cross the package boundary.

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

File editing is an explicit FileBackend operation, not terminal keystroke synthesis. Open returns canonical
root-relative path, host/generation, file identity, byte/encoding bounds, content hash and revision. Save is
an atomic compare-and-swap against all of them; external changes yield a three-way conflict view and zero
overwrite. Root/descriptor jails reject absolute escape, symlink/hardlink/mount swaps and check/use races,
including on remote targets. Autosave is opt-in per file and obeys the same revision fence; binary,
oversized, unsupported-encoding, permission and offline cases remain read-only or refused with exact reason.
Resource Node edits do not mutate a file unless the operator invokes this FileBackend save operation.

Lifecycle operations are explicit and idempotent:

- **Attach view** binds a surface to a live attempt and changes no work;
- **Resume** proves the same provider conversation and creates a new attempt under the same instance;
- **Restart** reuses the declared task/command as fresh work; a generic Tool keeps its Node with a new
  attempt, while an Agent gets a new Node/instance with lineage unless the adapter proves Resume semantics;
- **Switch model/mode** records requested/effective configuration; it preserves an instance only when the
  provider proves conversation continuity and otherwise performs an explicit Branch/new instance;
- **Branch** creates a new instance with lineage;
- **Interrupt** sends the runtime's non-terminal interrupt operation; **terminate** requests graceful process
  exit; **kill** uses the declared forceful backend action and terminalises the attempt with that evidence;
- **Recycle** replaces runtime infrastructure while preserving Node/instance/conversation only through a
  proven durable attach/resume; otherwise it is refused and offers an explicit fresh Restart;
- **Destroy** fences the semantic Node/instance, revokes input/grants/context, removes its active row and
  writes a durable tombstone; process, worktree, branch and artifact cleanup remain separate dispositions;
- **End Session** removes the active navigation record authoritatively and reports survivors separately.

At restore, Turn first reattaches verified durable handles. It never starts work merely because metadata was
restored. Foreground Session selection may execute only the exact preflighted activation plan in §3.3;
selecting a child/resource/history result never starts work. A Flow may continue only from persisted policy
and receipts that explicitly authorised that step. Ambiguous writes, input or message delivery become
`submitted_unconfirmed` and are not replayed.

The recovery survivor matrix is normative; every cell is an independent reducer output. “Unchanged” means
the exact id, generation and last evidence are preserved, not that liveness is inferred. The semantic/runtime
half is:

| Event | Node | AgentInstance | provider conversation | RuntimeAttempt | OS process/runtime | PTY |
| --- | --- | --- | --- | --- | --- | --- |
| UI view reload, same daemon/surface generation | unchanged | unchanged | unchanged | unchanged | unchanged | reattach view only; bytes/process unchanged |
| client disconnect or replacement connection | unchanged | unchanged | unchanged | unchanged | unchanged | runtime stays; old Surface detaches and a new view must attach explicitly |
| daemon restart | restore exact id | restore exact id | historical until revalidated | `Reconnected` only by proved handle, else `Orphaned` then bounded `Lost` | probe exact durable handle; never launch | reattach only by proved backend identity, else lost |
| owning shell exit/restart | retained | retained | historical | old attempt terminal; restart creates a new attempt | exact shell exits; descendants reduce independently | old PTY closed; restart allocates a new PTY |
| local host reboot | retained | retained | historical | local attempt `Orphaned` then `Lost`; remote attempt re-probed | local process absent; remote handle unknown until probe | local PTY lost; remote PTY detached until probe |
| remote disconnect | retained | retained | last binding stale | Lifecycle unchanged; independent connectivity becomes `Disconnected` and observability stale | unknown on pinned remote host; no local substitute | detached; never rebound by name alone |
| remote reconnect accepted | unchanged | unchanged | binding current only after endpoint proof | connectivity `Connected`; Lifecycle becomes `Reconnected` only if exact durable reattach resolves a prior orphan, otherwise retains its proved value | same remote identity verified | exact remote PTY reattached |
| remote reconnect refused/mismatched | unchanged | unchanged | remains stale/historical | connectivity remains `Disconnected|Unknown`; Lifecycle changes to `Lost` only after separate bounded absence proof | no process claim and no local fallback | remains detached/lost |
| topology/telemetry source loss | unchanged | unchanged | unchanged | unchanged; no lifecycle inference | unchanged; no exit inference | unchanged |
| End Session / Destroy Node | durable tombstone | retained historical and fenced | retained historical | stop disposition or explicit unreachable survivor | exact stop receipt or explicit survivor | close/detach receipt or explicit survivor |

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
| End Session / Destroy Node | retain intents/results and artifact references | queued/pre-write refused; possible write reconciled | queued/pre-write refused; possible write reconciled | close active interactions with exact disposition; retain audit tombstone | revoked immediately | revoked immediately | revoked immediately |

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
revocation fences the next read. File/Diff/Web/Media resources require a ContextPacket or a separately
specified future source capability; they cannot masquerade as a Note. The Note View lists current consumers,
remaining budgets and stale/revoked state without exposing their bearer.

`ContextPacket` is a one-shot portable handoff. It records source/target, lineage, selection, budget,
redaction/review state, content hash and delivery evidence without treating submission as receipt.

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

An `AccountProfile` is a non-secret identity scoped to provider plus ExecutionTarget and backed by an
isolated provider config/auth home or OS-keystore/agent reference. Foreground operations create, adopt,
launch the provider's external authentication flow, validate, rename, retire and delete a profile; Turn
never asks for or stores the credential itself. The CreationCatalog chooses an account with fixed precedence:
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
reference into `draft` without reading credential bytes; authenticate invokes the external provider flow;
validate records provider/account identity and capability evidence; rename changes only the display label by
compare-and-swap; retire removes launch/default eligibility but retains evidence; delete requires no active
attempt, current binding, default, grant or retained reference and never deletes provider-side data. Every
verb has its own operation id, expected profile/target generation and receipt. Only a current `active`
profile with proved isolation and required capability may become or remain a default; when it ceases to be
eligible, the default becomes explicitly unset and no other profile is silently selected.

Profiles have independent transcript/cache roots, endpoint bindings, quota observations and revocation
state. A profile with active attempts or retained audit references may be retired but not destructively
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

Only actionable evidence enters one global logical Attention Queue. Every entry carries one
`AttentionSubject` tagged union: `Exact { node_id, instance_id?, attempt_id?, generation?, demand_ref:
PendingInteraction(id, revision)|Result(id, revision)|Condition(kind, revision), verified_action_owner?,
view_target }`, `Provisional { authenticated_parent_or_external_scope,
evidence_revision }`, or `Unassigned { session_id, evidence_revision }`. Only `Exact` may contain an input or
action owner; when it does not, routing still opens that exact Node View with the action disabled and a
truthful owner-unavailable reason. Its type is one of permission, question, decision, failure, lost/disconnected, reviewable
result, resource pressure, quota policy or provisional evidence that requires confirmation. Normal
running/idle status, usage updates and informational telemetry remain in the tree/HUD `StatusProjection`;
they never enter `Next Attention` or change queue order.

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
capability grants allow. Sensitive permissions, credentials and authority changes always require the
foreground desktop surface.

`CompanionAction` is closed to `route_attention`, `mark_result_read`, `acknowledge`, `snooze`, `dismiss`,
`submit_free_text_response`, `submit_permission_response`, `interrupt` and `request_writer_lease`.
`submit_free_text_response` is valid only
for a verified non-sensitive question or decision schema; it can never answer a permission or credential
prompt. Each action carries expected queue/subject/
interaction and authority revisions, expires, is operation-idempotent and returns a receipt. Permission
approval is remote-capable only through `submit_permission_response` and a single-use
`RemotePermissionResponseGrant` issued on the foreground desktop. The grant fixes provider/profile,
Session/Node/instance/attempt/generation, exact PendingInteraction id/revision, the closed provider-offered
response ids and maximum scope/duration; the response carries all expected revisions, is end-to-end
encrypted to the daemon, expires, cannot widen an option and returns a provider-correlated receipt before
the interaction closes. Denial is a typed response too. Credential/password/secret entry, grant issuance or
expansion, daemon administration, remote-host trust/rotation, force kill/destroy, checkout integration and
publish/merge are always desktop-foreground-only. Offline companions may retain an
encrypted local draft, never a mutation; reconnect submits only after revision revalidation and otherwise
returns stale/refused. Writer-lease handoff is explicit and visible on both surfaces.

## 11. Generic terminal tools and resources

Turn remains a complete terminal even when no agent integration is present: real PTY semantics, resize,
alternate screen, IME, clipboard/path drop, search, safe links, keyboard navigation, bounded scrollback and
explicit signal/lifecycle controls. Shells, `k9s`, logs and custom TUIs are first-class nodes, not degraded
agents. Their output may reveal provisional prompts or failures, but Turn does not fabricate agent concepts.

Services and log streams may be created independently or discovered below another runtime. File, Diff, Note,
Web and Media content is inert, bounded and source-labelled. Restoring a resource does not load a URL, execute
content, open a file externally or delete the underlying data. The detailed Web/path isolation rules in
`docs/AGENT_NODE_VIEWS_AND_CONTEXT.md` remain normative.

`WebPreview` and `Browser` are different kinds and capabilities. A Web Resource stores only a bounded inert
snapshot/reference and loads nothing until an explicit preview; its isolated renderer has scripts, forms,
navigation, popups, downloads, ambient cookies/credentials, daemon sockets and local-file access disabled.
A Browser Node is an explicitly created interactive browsing context with a dedicated storage partition and
typed `navigate|back|forward|reload|stop|open_reviewed_popup|accept_reviewed_download|clear_storage`
operations. Its address, history entry ids, load/TLS/error state, permissions and popup/download dispositions
are daemon-visible facts, but page content, links and script messages are untrusted data and never become a
Turn control operation, Attention resolution or authority grant.

The Browser policy defaults to no ambient provider/Turn credentials, no daemon/control origin, blocked
popups, quarantined non-executable downloads and denied device/clipboard/filesystem permissions. A popup is
opened only as a new Browser Node after origin/consequence review; a download becomes an inert File Resource
only after size/type/hash/path policy and never auto-opens. Local HTML is copied by descriptor from one
reviewed confined root into a synthetic isolated origin; `file://`, symlink/hardlink/mount escapes and live
workspace access are refused. A loopback/localhost URL requires a foreground review binding exact scheme,
resolved IP set, port, target host/generation and expiry; navigation re-resolves and fails on DNS rebinding,
host/port change or remote-to-local fallback. Browser history/restoration never reloads a page automatically,
and destroying the Node clears its partition without claiming to delete server-side data.

Workspace and Session views provide local/remote file exploration and source-control/worktree operations as
typed views over the same execution target and checkout authority. Status, diff, stage, unstage, commit,
commit-and-push, fetch, pull, push, branch, history, conflict inspection/resolution and worktree cleanup state
their exact repository/host, base revision and consequences. Generated commit messages remain editable
drafts and never commit automatically. Destructive discard/cleanup and history-rewriting operations remain
explicit; a remote outage never redirects an operation to a local repository.

## 12. Local voice input

`docs/LOCAL_VOICE_INPUT.md` is normative. The common path is hold shortcut, speak, release, edit the inline
draft and use the normal send gesture. Capture and inference remain on the physical foreground device in a
crash-isolated worker; the model is an optional explicit verified download. There is no cloud, remote-host,
auto-send, approval or voice-command fallback.

The draft freezes the exact surface, Node, instance, attempt, pending interaction and input owner. Selection
changes cannot retarget it. Voice never creates, orders, acknowledges or resolves Attention, and automatic
focus defers while capture/review is active without hiding new work.

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

`docs/ACCESSIBILITY_ACCEPTANCE.md` covers every NodeKind WorkSurface plus CreationCatalog, Flow/Team edit and
run controls, integration diagnostics, status history/HUD, file/SCM/conflict views, remote writer handoff and
companion actions. It specifies names/roles/states, focus order/restoration, keyboard alternatives to drag,
live-region announcements and platform/screen-reader evidence rather than relying on one generic row.

## 14. Authority, privacy and failure semantics

All state-changing operations are typed, authenticated, generation-fenced, idempotent and scoped to a
Workspace/Session/FlowRun plus exact targets. Capabilities are least-privilege and short-lived when delegated.
Administrative control, context brokerage, remote runtime access and companion access use different tokens.

Portable Workspace/Flow content is not machine trust. A shareable definition may contain inert node shape,
roles, prompts, commands as unadopted text, dependencies and presentation, but it cannot carry credentials,
account bindings, local executable overrides, consent, capability grants, host identity or a decision to run.
Import shows those differences and creates no runtime until a local adoption receipt binds the definition to
known tools, paths, execution targets and policy.

An export uses package-local `PortableId`s only. Import always creates a fresh namespace and remints
Workspace, Session, Node, Team, FlowDefinition and relationship ids; a live or executable `FlowRun` is never
portable. An optional `PortableRunReport` is inert bounded history: redacted definition/step labels, declared
terminal summaries and artifact content hashes addressed only by package-local ids. Import renders it as a
read-only Resource; it cannot satisfy a dependency, prove completion, resume/retry work or supply runtime,
operation, revision or authority identity. Runtime attempts, provider conversations, NativeJob keys, PIDs,
PTYs, operation ids, receipts, revisions, grants, tombstones and machine/host ids never cross the boundary.
References resolve through the package map, unresolved references are inert errors and no local collision
can update or resurrect an existing object. The origin content hash is provenance, never identity or trust.

Multiple surfaces and clients consume authoritative daemon revisions. Reconnect starts from a bounded
snapshot plus journal position rather than a best-effort stream. Durable tombstones prevent a disconnected
client from resurrecting ended/deleted entities. Conflict policy covers nodes, every relationship family,
Flow definitions/runs and lifecycle operations; no whole-document last-write may silently erase an unrelated
edge. A runtime has one explicit input/resize writer lease at a time. Other viewers receive bounded catch-up
and live output, but their bytes cannot interleave with the writer; lease handoff is visible and generation-
fenced.

`StateStreamKey` is a closed tagged scope: `Installation(daemon_generation)` owns the one Attention Queue,
ExecutionTarget/profile catalogue and target-independent policy; `Workspace(daemon_generation,
WorkspaceId)` owns its Sessions, Nodes, Flows and relationships; `ExecutionTarget(daemon_generation,
ExecutionTargetId,target_generation)` owns target-wide RuntimeInventory, NativeJobs, endpoint connectivity
and target/profile observations. A snapshot closes at domain revision `R` with a journal watermark; events
begin at `R+1`, are strictly sequenced inside that domain and clients acknowledge each subscribed revision.
A gap, compacted cursor or generation change forces a fresh snapshot for that domain before mutation. There
is no invented total order across independent domains.

Every cross-domain reference carries the exact source-domain revision. A route validates both Attention
Queue revision and subject-domain revision; a Workspace default pointing to an AccountProfile validates the
Workspace and ExecutionTarget revisions. A mutation spanning domains supplies an expected revision vector
and transaction id; the daemon durably prepares every domain fence, commits one result/new-revision vector
or none, and appends the same barrier/transaction receipt to each affected stream. Clients do not expose a
partial cross-domain result: they apply it only after all named barrier revisions arrive or obtain a bounded
multi-domain snapshot at that vector. Recovery resolves the transaction receipt before any retry.

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
canonical hierarchy, WorkSurface/Node Views, status history, CreationCatalog, Flow/Team views, terminal
stream and Attention routes through the versioned daemon protocol; headless clients expose the same objects
as structured commands/events. Capability negotiation makes unsupported rendering or control explicit.
Remote foreground may request ordinary revision-fenced mutations and an input lease within its invitation
scope. A versioned server-side allowlist intersects protocol variant, invitation scope, target policy and
current evidence; every variant not explicitly present is denied even if a client can encode it. The only
remote permission path is the single-use typed response in §10. Credential/secret entry, daemon
administration, authority/grant issue, host trust/key rotation, destructive lifecycle and repository
publish/integration remain absent and fail server-side. While an adapter reports a current sensitive typed
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

1. provider-neutral identities, topology observations and capability contract;
2. one WorkSurface plus exact Attention routes over those identities;
3. truthful launch receipts, lifecycle attempts and durable/local/remote runtime seam;
4. one CreationCatalog, FlowRun and worktree-safe typed control;
5. context transfer, messages, Teams, dependency execution and verification flows;
6. quota/context/resource telemetry and companion projections;
7. resource Node Views and local voice input;
8. scale, live-provider, remote, packaged accessibility and failure-recovery proof.

A vertical may ship incrementally behind capability/status labels. It may not claim provider parity,
restoration, zero subagents, delivery, continuity or completion without the corresponding evidence.

## 16. Two independently falsifiable completion gates

### 16.1 Specification integration complete

The product-specification goal is complete only when, on the same commit:

1. the versioned manifest fixes every requirement id, acceptance id and hashes of its normative outcome and
   oracle; removal or semantic change names an accepted ADR in the manifest revision;
2. every inventory row maps one-to-one to a non-empty proof obligation and the mutation tests prove that
   paired deletion, requirement weakening and trivial-oracle substitution fail the gate;
3. the contract, Product, Architecture, Protocol, Roadmap, decisions and detailed specifications use one
   ontology and contain no unresolved contradiction or relevant capability gap;
4. at least two non-author adversarial audits of the final frozen diff return no P0/P1 finding; every P2 is
   either closed or named with a justified product boundary;
5. `make verify` and the specification gate are green and the exact commit is merged to `main`.

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
