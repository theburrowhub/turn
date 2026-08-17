# Product requirement inventory

**Baseline audited:** `e386f835a3eeced8a46aa60511a58ec8ddfa29b6`  
**Inventory status:** frozen accepted target  
**Implementation status:** mixed; this file is not a claim that the target is built

This is the completeness ledger for Turn's operator-control-plane goal. Its exact ids plus hashes of every
normative outcome and acceptance oracle are frozen in `PRODUCT_REQUIREMENTS_V1.manifest`. A requirement may
be changed or removed only by an explicit accepted ADR that increments the manifest revision and updates this
inventory, its normative contract and its acceptance row in the same commit. Quietly dropping or weakening a
row to make a milestone look complete is forbidden and is covered by mutation tests.

`PRODUCT_SPEC_V1.authority` also hashes every normative document and the three gate scripts. Its repository
digest file permits convenient local verification; the checked-in CI job additionally requires the protected
repository variable `PRODUCT_SPEC_V1_AUTHORITY_SHA256`. While that unchanged job runs, co-editing requirements,
manifest, authority, local pin and verifier still fails with `E_AUTHORITY_CI_PIN`. Changing the frozen product
version therefore requires an explicit out-of-band pin rotation whose audit trail is separate from the
proposing commit.

This is not a claim that same-repository CI can authenticate its own workflow against a malicious committer:
a PR that is allowed to delete or replace the required job can bypass any check stored in that PR. Repository
policy must require a workflow/ruleset whose identity the change cannot redefine, or a trusted operator must
compare the workflow and authority root before merge. The machine gate prevents accidental or concealed
semantic drift within the executed workflow; the external merge-policy boundary prevents removal of the gate.

Status vocabulary:

- **baseline** — the audited v0.1 implementation has direct named evidence for the stated scope;
- **partial** — useful implementation exists, but the complete requirement does not;
- **target** — accepted and specified, but no implementation evidence exists;
- **conflict** — current behavior or another accepted statement contradicts the requirement.
- **implemented** — the complete target has a matching immutable implementation-evidence record and its ACP
  oracle passes on the same commit.

Each row maps to one proof obligation in `docs/CONTROL_PLANE_ACCEPTANCE.md`. The detailed design is
normative where linked; an unqualified `§` points to `docs/OPERATOR_CONTROL_PLANE.md`. The status column is
an honest snapshot, not the specification exit condition. Product-realisation completion requires every row
to become `implemented`; the specification gate deliberately permits honest gaps.

## Product outcome

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-OUT-001` | One operator can create, supervise and direct many concurrent units of agentic and terminal work without polling panes. | `OPERATOR_CONTROL_PLANE.md` §1 | partial | `ACP-OUT-001` |
| `PRD-OUT-002` | Claude Code, Codex, Gemini, OpenCode, future/custom agents, shells, TUIs, services and logs share one operational model while exposing only proved capabilities. | §1, §6, §11 | partial | `ACP-OUT-002` |
| `PRD-OUT-003` | The ordered Attention Queue, not terminal layout or provider UI, is the sole authority for routing operator interruption. | §1.1, §10 | partial | `ACP-OUT-003` |
| `PRD-OUT-004` | The common path meets fixed interaction budgets: one action for attach/Attention, two for default creation/Flow launch, zero redundant confirmations, and one consolidated authority review when required. | §1.1, §3.3, §4–5 | conflict | `ACP-OUT-004` |

## Hierarchy and identity

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-HIE-001` | Workspace → Session → semantic children is the one canonical navigable hierarchy. | §2.1; `UNIFIED_HIERARCHY_UPGRADE.md` | baseline | `ACP-HIE-001` |
| `PRD-HIE-002` | Ownership, spawn/process ancestry, presentation, dependency, context, lineage and message relationships remain typed, independent and provenance-labelled. | §2.2 | partial | `ACP-HIE-002` |
| `PRD-HIE-003` | Node, AgentInstance, RuntimeAttempt, provider conversation, runtime handle, OS process, PTY, input owner, Pane, FlowRun and client/surface have distinct ids and declared cardinalities. | §2.3; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §3 | target | `ACP-HIE-003` |
| `PRD-HIE-004` | Nested agents, provider-side children and subprocesses remain recoverable nodes with their own lifecycle, attempt, causal spawn and completion identity. | §2, §6 | conflict | `ACP-HIE-004` |
| `PRD-HIE-005` | Groups and Teams organise the existing nodes without rewriting runtime ancestry, granting context or becoming another topology authority. | §2.1–2.2, §5.3 | target | `ACP-HIE-005` |
| `PRD-HIE-006` | Uncertain parentage stays unassigned/provisional; stronger evidence is never overwritten by weaker evidence. | §2.2; `UNIFIED_HIERARCHY_UPGRADE.md` §2 | baseline | `ACP-HIE-006` |
| `PRD-HIE-007` | One closed NodeKind and relationship ontology is identical across domain, store, protocol, views and detailed contracts; migrations never expose aliased identities. | §2; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §3 | conflict | `ACP-HIE-007` |
| `PRD-HIE-008` | Every operational Node has exactly one deterministic primary tree row; Group override, spawn, process and Session fallback precedence is fixed while Team/Flow/non-winning relations are references. | §2.2 | target | `ACP-HIE-008` |

## Navigation, views and window behavior

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-VIE-001` | One hierarchy selection resolves one Workspace, Session or Node `ViewTarget` in one right-hand `WorkSurface`. | §3; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §4 | target | `ACP-VIE-001` |
| `PRD-VIE-002` | Selecting a child/resource changes only the view; it never changes Layout, launches work, sends input or acknowledges Attention, while foreground Session activation is governed only by `PRD-LIF-009`. | §3–3.3 | partial | `ACP-VIE-002` |
| `PRD-VIE-003` | Every NodeKind has a truthful unique content view with explicit loading, empty, unsupported, disconnected, stopped, lost and stale states. | §3.1 | target | `ACP-VIE-003` |
| `PRD-VIE-004` | Agent/subagent views expose semantic identity, task, parent, attempt, turn/lifecycle, transcript/activity, result, children, context, runtime truth and safe actions. | §3.1, §6, §9 | target | `ACP-VIE-004` |
| `PRD-VIE-005` | A live binding attaches automatically; stopped/lost work offers a precise lifecycle action and never a generic `Start pane` gate. | §1.1, §3.1 | conflict | `ACP-VIE-005` |
| `PRD-VIE-006` | The inspector is the selected view's detail region, previews are bounded status, and returning to Session restores the exact saved Layout/focus. | §3–3.1 | partial | `ACP-VIE-006` |
| `PRD-VIE-007` | Top-bar buttons sit with icon/name on the left, global metadata is right-aligned and unique, and operational messages use the bottom status bar. | §3.2 | partial | `ACP-VIE-007` |
| `PRD-VIE-008` | `+ Session` lives in Workspace scope; end/delete removes the active tree row immediately and reports surviving cleanup separately. | §3.2, §7 | partial | `ACP-VIE-008` |
| `PRD-VIE-009` | Search, status groupings and optional board/card views are projections of canonical Node ids/routes, never duplicate runtimes or navigation; one failed Node View is crash-isolated. | §3.1 | target | `ACP-VIE-009` |
| `PRD-VIE-010` | Concurrent operational messages use a bounded, prioritised bottom StatusEvent history with expiry/recovery and non-spamming accessible announcements; status becomes Attention only through a separate demand. | §3.2 | target | `ACP-VIE-010` |
| `PRD-VIE-011` | Board/work-item views have closed state, priority, due, tag, comment and assignee semantics over canonical Node revisions; they never become a second runtime, topology or Attention authority. | §3.1 | target | `ACP-VIE-011` |
| `PRD-VIE-012` | External WorkItemSource adapters preserve source identity/authority, mapping, pagination/cache/coverage, CAS writes, conflict/reconcile, close/reopen, assignees, rate limits and credential isolation in canonical cards. | §3.1; `PROTOCOL.md` | target | `ACP-VIE-012` |

## Creation and reusable flows

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-CRE-001` | One declarative CreationCatalog supplies Workspace `+`, toolbar, palette and context menus with identical labels, defaults and capability gates. | §4 | target | `ACP-CRE-001` |
| `PRD-CRE-002` | Common Session/Agent/Tool creation needs only Workspace, task/preset and optional prompt; Turn derives safe defaults and asks only unresolved consequential choices. | §4 | partial | `ACP-CRE-002` |
| `PRD-CRE-003` | Multi-node creation is preflighted, operation-id idempotent and leaves either visible receipts/recovery or no unobserved resources. | §4, §5.1 | target | `ACP-CRE-003` |
| `PRD-CRE-004` | Built-in and user-declared agents/tools use the same catalog and adapter contract; a custom entry cannot acquire undeclared capabilities. | §4, §6.1 | partial | `ACP-CRE-004` |
| `PRD-CRE-005` | Templates copy reusable layout/configuration; Flows create versioned execution runs; neither claims to copy live provider identity. | §4, §5.1 | partial | `ACP-CRE-005` |
| `PRD-CRE-006` | Successful single creation selects its canonical Node and safe input, multi-create selects its summary, cancellation restores prior selection and contextual surfaces may group but not redefine catalogue actions. | §3.3–4 | target | `ACP-CRE-006` |
| `PRD-CRE-007` | Resumable first-run/setup discovers versions and capabilities, explains degraded integration and guides provider authentication, notification/microphone consent and remote trust without collecting credentials or blocking generic terminal use. | §4, §6.1 | target | `ACP-CRE-007` |

### Reusable flows and delegation

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-FLW-001` | A versioned parameterised FlowDefinition declares nodes, roles, providers/tools, prompts/commands, dependencies, context, execution/isolation, Attention and resource policy. | §5.1 | conflict | `ACP-FLW-001` |
| `PRD-FLW-002` | One immutable FlowRun inside one Session is projected by a Session-owned Flow node and records inputs, operations, attempts, grants, receipts, results and terminal state independently of later definition edits. | §5.1 | target | `ACP-FLW-002` |
| `PRD-FLW-003` | Closed start policies support manual, with-run, typed dependency and bounded recurrence triggers; only up-front authorised policies may advance automatically. | §5.1 | conflict | `ACP-FLW-003` |
| `PRD-FLW-004` | A DelegationGrant lets an exact current agent perform bounded typed creation/organisation/context/message operations without repeated prompts and cannot self-expand. | §5.2 | conflict | `ACP-FLW-004` |
| `PRD-FLW-005` | Teams and verification flows support independent fan-out, declared roles, typed results and a gated synthesiser/judge while preserving every result as evidence. | §5.3 | target | `ACP-FLW-005` |
| `PRD-FLW-006` | Every Flow writer uses a dedicated worktree/isolation backend and never occupies or locks the operator's primary `main` checkout. | §1.1, §5.3 | partial | `ACP-FLW-006` |
| `PRD-FLW-007` | Agent-directed control is a typed capability endpoint with structured receipts; terminal/prose output can only propose work or raise Attention. | §5.2 | target | `ACP-FLW-007` |
| `PRD-FLW-008` | Turn-managed fan-out and dependency calls return after durable receipts and inject no synchronous join; daemon/UI, terminal, navigation and later control remain responsive while provider-internal waits are reported truthfully. | §5.3 | target | `ACP-FLW-008` |
| `PRD-FLW-009` | FlowRun has a closed state machine and idempotent pause/resume/cancel/abort/step-retry/reconcile semantics, including provisioning rollback, immediate grant revocation and explicit active-runtime dispositions. | §5.1 | target | `ACP-FLW-009` |
| `PRD-FLW-010` | Bounded recurrence fixes timezone, DST fold/gap, clock rollback, sleep/reboot catch-up, overlap, maximum backlog and finite end/occurrence behavior with stable occurrence ids. | §5.1 | target | `ACP-FLW-010` |
| `PRD-FLW-011` | Versioned wire operations cover Flow definitions/runs, DelegationGrant issue/revoke, delegated typed operations and partial-saga recovery with expected revisions/generations and durable receipts. | §5.1–5.2; `PROTOCOL.md` | target | `ACP-FLW-011` |
| `PRD-FLW-012` | A bounded grant may create/update exact typed Resource Nodes and publish progress with provenance, size/rate/revision limits and receipts, but cannot delete/reparent, mutate underlying files or turn content into control. | §5.2; `PROTOCOL.md` | target | `ACP-FLW-012` |

## Agent topology and adapter parity

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-TOP-001` | Every dedicated adapter emits the same versioned topology observation envelope with parent/attempt, child identity, causal invocation, lifecycle/turn, revision and evidence. | §6 | partial | `ACP-TOP-001` |
| `PRD-TOP-002` | Ingestion is idempotent, generation-fenced and safe under duplicates, late/out-of-order events, reused provider ids and parent termination. | §6 | partial | `ACP-TOP-002` |
| `PRD-TOP-003` | Structured and OS/process discoveries reconcile into one node; reparenting needs stronger evidence and retains provenance. | §6; `UNIFIED_HIERARCHY_UPGRADE.md` §3–4 | partial | `ACP-TOP-003` |
| `PRD-TOP-004` | Direct, live and total subagent counts derive from the normalised graph; confirmed `0`, partial `N+ observed`, `unknown`, `unsupported` and `stale` are never conflated. | §6 | conflict | `ACP-TOP-004` |
| `PRD-TOP-005` | A child may have no PTY yet still expose bounded live activity/transcript, timing, tools/tokens and a typed result without becoming an ephemeral UI card. | §3.1, §6 | conflict | `ACP-TOP-005` |
| `PRD-TOP-006` | Arbitrary-depth agent and process descendants remain navigable and recoverable after UI reconnect and, where evidence persists, daemon/runtime recovery. | §2, §6–7 | partial | `ACP-TOP-006` |
| `PRD-TOP-007` | Workspace, Session and Agent aggregates name direct-versus-descendant scope, coverage/confidence and revision and all derive from one graph. | §6 | target | `ACP-TOP-007` |
| `PRD-TOP-008` | Topology streams identify source/epoch/scope and snapshot/delta/gap sequencing; exact zero requires a closed authoritative coverage set and any overflow, gap, restart or staleness invalidates it until asynchronous resync. | §6; `PROTOCOL.md` | conflict | `ACP-TOP-008` |
| `PRD-TOP-009` | Provider-native state reduces through one specified transition/tie-break table into Lifecycle and TurnState; turn completion never implies process exit and parent exit never erases a live child. | §6; `UNIFIED_HIERARCHY_UPGRADE.md` §5 | partial | `ACP-TOP-009` |

### Adapter parity

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-ADP-001` | The adapter registry is the sole provider-specific seam; core, store, protocol projections and UI contain no provider-name behavior branches. | §1.1, §6.1 | conflict | `ACP-ADP-001` |
| `PRD-ADP-002` | Claude Code, Codex, Gemini and OpenCode implement the same capability vocabulary and common state/topology semantics. | §6.1 | partial | `ACP-ADP-002` |
| `PRD-ADP-003` | The generic fallback and user-declared custom adapters advertise honest supported/unsupported/unknown/degraded capabilities and never fabricate agent semantics. | §6.1, §11 | partial | `ACP-ADP-003` |
| `PRD-ADP-004` | Every advertised capability has shared fixtures, provider fixtures, degradation cases and live evidence when credentials/service behavior affect the claim. | §6.1 | partial | `ACP-ADP-004` |
| `PRD-ADP-005` | One provider's slow/failing usage, hook, transcript or control endpoint does not block another provider or the selected view. | §6.1, §9 | target | `ACP-ADP-005` |
| `PRD-ADP-006` | Launch/resume/branch/model/stop dispatch through capability-selected adapter methods; daemon code does not re-check a provider name. | §6.1–7 | conflict | `ACP-ADP-006` |
| `PRD-ADP-007` | Each Agent exposes provider/CLI version, achieved integration, event mechanisms, last valid/rejected observation, downgrade reason, freshness, self-test and redacted diagnostics. | §6.1 | target | `ACP-ADP-007` |
| `PRD-ADP-008` | Capability facts are independently scoped by mechanism, adapter/version, provider/account, target/host, endpoint and attempt/epoch; operations require their current intersection plus authority. | §6.1 | target | `ACP-ADP-008` |
| `PRD-ADP-009` | Integration self-tests are explicit, consequence-previewed, disposable, bounded and redacted, clean up fully and cannot mutate the inspected Session, hooks or quota except as disclosed. | §4, §6.1 | target | `ACP-ADP-009` |
| `PRD-ADP-010` | A shared provider RuntimeEndpoint can multiplex independently bound instances/conversations with unique ownership, isolated input/context/transcript/Attention and per-binding recovery/fallback. | §6.2; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §7.4 | target | `ACP-ADP-010` |
| `PRD-ADP-011` | Provider-native scheduled, recurring and background jobs have stable job/iteration identity, schedule and lifecycle evidence, survive according to provider truth, route Attention normally and expose independent capability-gated controls; Flow recurrence and dismiss/delete remain distinct. | §2.1, §3.1, §6.3 | target | `ACP-ADP-011` |

## Runtime lifecycle and execution targets

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-LIF-001` | Attach/detach/view close never means start/terminate; multiple viewers share one attempt through explicit input/resize ownership and bounded catch-up. | §1.1, §7 | partial | `ACP-LIF-001` |
| `PRD-LIF-002` | Resume, restart, model/mode switch, branch, interrupt, terminate, kill, recycle and destroy have distinct, idempotent identity/continuity semantics. | §7; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §3.4 | conflict | `ACP-LIF-002` |
| `PRD-LIF-003` | Create/attach/resume has a durable idempotency key and receipt reporting `created`, `attached`, `recovered`, `refused` or uncertain outcome. | §2.3, §7 | target | `ACP-LIF-003` |
| `PRD-LIF-004` | Recovery separately specifies UI reload, daemon restart, shell restart, host reboot, disconnect, remote outage and observation-source loss. | §7, §14 | partial | `ACP-LIF-004` |
| `PRD-LIF-005` | Background restore reattaches proved handles but launches nothing from metadata, child selection or history; only a persisted authorised Flow policy or the separately preflighted foreground Session activation may advance work. | §3.3, §7 | partial | `ACP-LIF-005` |
| `PRD-LIF-006` | End/delete is operator-authoritative, immediately updates navigation, fences resurrection and reports rather than hides unreachable survivors. | §3.2, §7, §14 | partial | `ACP-LIF-006` |
| `PRD-LIF-007` | A normative survivor matrix declares Node/instance/attempt/conversation/runtime/PTY/receipt/Attention/grant/message/input-lease outcomes for UI/daemon/shell restart, reboot, disconnect and observation loss. | §7 | target | `ACP-LIF-007` |
| `PRD-LIF-008` | Target-wide runtime inventory reconciles known attempts and exposes unmatched/orphaned live handles without invented Nodes, with exact adopt/ignore/terminate actions scoped to target+handle+generation. | §7; `PROTOCOL.md` | target | `ACP-LIF-008` |
| `PRD-LIF-009` | Foreground Session selection restores/attaches and, only under a current fully preflighted activation plan, materialises its bounded eligible saved runtimes or one safe default Shell in the same interaction; every unresolved or changed consequence fails closed with one precise recovery action. | §3.3, §7 | target | `ACP-LIF-009` |

### Execution targets and resources

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-RUN-001` | RuntimeBackend supplies typed create/attach/resize/input/signal/observe/close over local and remote ExecutionTargets. | §7 | target | `ACP-RUN-001` |
| `PRD-RUN-002` | Durable runtime provides warm process continuity; cold reconstruction/provider resume is labelled as a different guarantee. | §7 | target | `ACP-RUN-002` |
| `PRD-RUN-003` | Remote host identity, generation, namespace and capabilities are pinned; outage never falls back to local execution or local same-name paths. | §7–8 | target | `ACP-RUN-003` |
| `PRD-RUN-004` | Shells, TUIs, commands, services and logs retain full terminal/runtime behavior without being treated as degraded agents. | §1, §11 | baseline | `ACP-RUN-004` |
| `PRD-RUN-005` | Resource nodes restore inertly and cannot load, execute, navigate or delete referenced user data as a side effect. | §3.1, §11; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §3.1 | target | `ACP-RUN-005` |
| `PRD-RUN-006` | Workspace/Session views provide target-bound file exploration and source-control/worktree operations with exact repository/host and explicit destructive consequences. | §11 | target | `ACP-RUN-006` |
| `PRD-RUN-007` | Every Turn-managed write-capable create, add, activate, restore, resume, restart, recycle, switch and branch path uses isolation; v4 MainCheckout is migration-only and release proof requires zero primary-main leases/processes/locks. | §1.1, §7 | conflict | `ACP-RUN-007` |
| `PRD-RUN-008` | Remote runtime/file/repository transport has pinned or mutual authentication, confidentiality, integrity, replay protection, explicit key rotation/revocation and secret-safe credential storage/diagnostics. | §7, §14; `SECURITY.md` | target | `ACP-RUN-008` |
| `PRD-RUN-009` | RuntimeBackend, FileBackend and RepositoryBackend are distinct capability seams; every remote file/SCM request binds host/generation/root/revision and outage causes zero same-name local effects. | §7, §11 | target | `ACP-RUN-009` |
| `PRD-RUN-010` | File editing opens a bounded revisioned snapshot and saves atomically through FileBackend with conflict recovery, root/descriptor confinement, remote identity and no implicit terminal/resource mutation. | §7; `PROTOCOL.md` | target | `ACP-RUN-010` |
| `PRD-RUN-011` | Inert WebPreview and isolated interactive Browser are distinct capabilities; Browser navigation/history, reviewed popup/download, partitioned storage and reviewed localhost/local-HTML paths cannot acquire ambient credentials, file access or control authority. | §3.1, §11; `SECURITY.md` | target | `ACP-RUN-011` |

## Context, communication and runtime truth

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-CTX-001` | A ContextLink is an explicit scoped, expiring, revocable, audited pull grant bound to destination instance/current attempt. | §8; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §7.1 | target | `ACP-CTX-001` |
| `PRD-CTX-002` | A ContextPacket is an immutable one-shot snapshot with source/target, lineage, selection, budget, review/redaction and delivery evidence. | §8; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §7.2 | target | `ACP-CTX-002` |
| `PRD-CTX-003` | Portable handoff is target-budgeted: older digest, recent exact tail and separately authorised bounded full artifact, with completeness/omissions in the receipt. | §8 | target | `ACP-CTX-003` |
| `PRD-CTX-004` | Reviewed Flow authority may create declared links/packets automatically; work outside it receives one consolidated exact review and never a prompt cascade. | §5.1–5.2, §8 | conflict | `ACP-CTX-004` |
| `PRD-CTX-005` | Branch, native continuation and portable handoff use separate lineage labels and never claim provider continuity without proof. | §2.2–2.3, §8 | target | `ACP-CTX-005` |
| `PRD-CTX-006` | AgentMessage has bounded content, sender/target/purpose/TTL/idempotency, finite per-target queue, idle/input gates and independent delivery receipts. | §5.3, §8 | target | `ACP-CTX-006` |
| `PRD-CTX-007` | DependencyResult is closed-schema durable evidence; idle/exit alone does not satisfy it, and only authorised Flow policy may start downstream work. | §2.2, §5.1, §8 | target | `ACP-CTX-007` |
| `PRD-CTX-008` | Context, messages, dependencies, ancestry and permissions grant none of one another's authority; no delivered text becomes control. | §2.2, §5.2–5.3, §8 | partial | `ACP-CTX-008` |
| `PRD-CTX-009` | Only a foreground operator issues root context authority; an agent may exercise but never expand exact pre-authorised Flow/DelegationGrant scopes, and every use retains issuer/grant/generation provenance. | §5.2, §8; `SECURITY.md` | conflict | `ACP-CTX-009` |
| `PRD-CTX-010` | Local/remote context acquisition and delivery enforce descriptor/root jails, TOCTOU/symlink/hardlink/mount defenses, authenticated encrypted anti-replay transport, non-executable framing and canary-proved redaction. | §8; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §7 | target | `ACP-CTX-010` |
| `PRD-CTX-011` | AgentMessage has one closed transport state machine, structured endpoint only, exact queue/byte/TTL/input gates and no retry after ambiguous submission; read and acted remain separate. | §5.3 | target | `ACP-CTX-011` |
| `PRD-CTX-012` | ContextLink may use an exact Note Resource as a pinned or explicitly reviewed live brief with author/schema/revision/budget bounds; edits never widen/reset authority and every disclosed revision is audited. | §8; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §7.1 | target | `ACP-CTX-012` |
| `PRD-CTX-013` | A private bounded ConversationInventory searches and pages exact provider/profile/target history, reports coverage/freshness, enforces installation-wide current ownership and keeps adopt, resume and similarity matching distinct without launching implicitly. | §6.2, §7; `PROTOCOL.md` | target | `ACP-CTX-013` |

### Runtime truth and telemetry

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-OBS-001` | Every attempt exposes requested, effective and current model/account/modes/safe flags/cwd/host/worktree plus adapter capability receipt. | §2.3, §9 | target | `ACP-OBS-001` |
| `PRD-OBS-002` | Conversation context usage and provider quota are distinct scoped observations with their own units and reset/window semantics. | §9; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §6 | target | `ACP-OBS-002` |
| `PRD-OBS-003` | Multiple accounts/providers/remote hosts are represented independently; a shared quota is never attributed to one agent. | §9 | target | `ACP-OBS-003` |
| `PRD-OBS-004` | Runtime/process resources and work progress expose lifecycle, turn, current task/tool, child counts, elapsed/status age, CPU/memory/output pressure and unread revision. | §9 | partial | `ACP-OBS-004` |
| `PRD-OBS-005` | Every observation has scope, source, confidence, revision, observed time and freshness; unknown, unsupported, stale, rate-limited and failed differ visually. | §9 | partial | `ACP-OBS-005` |
| `PRD-OBS-006` | Collection is independent, bounded, cache-aware and demand-driven so unavailable telemetry cannot stall input or node selection. | §9 | target | `ACP-OBS-006` |
| `PRD-OBS-007` | Telemetry may show safe names/typed modes but never credential values, raw environment content or secrets parsed from commands/output. | §9, §14 | partial | `ACP-OBS-007` |
| `PRD-OBS-008` | Account profiles have isolated external auth/config roots and foreground create/adopt/auth/validate/default/retire/delete lifecycle; launch freezes the resolved account and default changes never migrate or cross-contaminate active instances. | §9; `PROTOCOL.md` | target | `ACP-OBS-008` |
| `PRD-OBS-009` | Provider conversation title read and rename are separate capability facts and operations; rename is revision-fenced and receipt-backed, and either function degrades independently without invented success. | §6.1–6.2, §9 | target | `ACP-OBS-009` |

## Attention and operator input

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-ATT-001` | Every AttentionEntry carries exact Node/instance/attempt/generation, interaction or result revision, action/input owner and ViewTarget when evidence permits. | §10; `AGENT_NODE_VIEWS_AND_CONTEXT.md` §5 | target | `ACP-ATT-001` |
| `PRD-ATT-002` | Permission, question/decision, failure/lost, unread result, stalled, pressure, quota-policy and provisional evidence remain typed and ordered by one policy. | §10 | partial | `ACP-ATT-002` |
| `PRD-ATT-003` | `Next Attention`, badges, notifications and permitted automatic focus resolve through the same daemon route in one interaction. | §10 | partial | `ACP-ATT-003` |
| `PRD-ATT-004` | Navigate, render, mark-read, acknowledge and resolve are separate; answer submission stays pending until provider evidence closes the exact interaction. | §10 | conflict | `ACP-ATT-004` |
| `PRD-ATT-005` | Explicit structured evidence may request governed focus; heuristics may only badge/propose and never claim permission/question certainty. | §10; `PRODUCT.md` §3.4 | baseline | `ACP-ATT-005` |
| `PRD-ATT-006` | Typing, dragging, alternate-screen input, modal work and voice capture/review defer automatic focus without dropping, resolving or retargeting the demand. | §10, §12 | partial | `ACP-ATT-006` |
| `PRD-ATT-007` | Unread/result revision, lifecycle, turn and status age are independent; an active or completed child cannot disappear because its parent turn changed. | §6, §9–10 | partial | `ACP-ATT-007` |
| `PRD-ATT-008` | Desktop, compact HUD and authenticated remote/mobile companions project the same queue/revisions and have explicit action-capability limits. | §10 | target | `ACP-ATT-008` |
| `PRD-ATT-009` | Attention admits actionable evidence only and specifies deduplication, safety-class ageing/no-starvation, snooze, dismiss, mute, cooldown and field-level Global→Workspace→Template→Session policy. | §10; `ATTENTION_ACCEPTANCE.md` | partial | `ACP-ATT-009` |
| `PRD-ATT-010` | Node-less or owner-less evidence routes to an exact revisioned ProvisionalAttentionView without inventing a Node, borrowing input ownership or allowing stale submission. | §10; `ATTENTION_ACCEPTANCE.md` | partial | `ACP-ATT-010` |
| `PRD-ATT-011` | A remote/full/companion surface may answer only an exact typed permission through a single-use foreground-issued E2EE revision-fenced grant; secrets, administration, trust and authority remain local, and raw remote input cannot bypass a known sensitive interaction. | §10, §14; `SECURITY.md` | target | `ACP-ATT-011` |

## Local voice

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-VOI-001` | Audio and inference stay on the foreground physical device in a crash-isolated worker; there is no cloud or Session-host fallback. | §12; `LOCAL_VOICE_INPUT.md` §1, §4 | target | `ACP-VOI-001` |
| `PRD-VOI-002` | Capture freezes exact surface/Node/instance/attempt/interaction/input owner; a memory-only editable draft reaches the target only after explicit Insert/Send. | §12; `LOCAL_VOICE_INPUT.md` §2, §7 | target | `ACP-VOI-002` |
| `PRD-VOI-003` | Model installation is optional and explicit, manifest/digest/size/licence verified, offline after install and separately inventoried/deletable. | §12; `LOCAL_VOICE_INPUT.md` §5, §8 | target | `ACP-VOI-003` |
| `PRD-VOI-004` | Voice never acts as control, approval or Attention authority and cannot retarget input after selection/generation changes. | §10, §12; `LOCAL_VOICE_INPUT.md` §3 | target | `ACP-VOI-004` |
| `PRD-VOI-005` | The voice worker has no control socket, workspace, credentials, arbitrary filesystem or network; unsafe controls/newlines are sanitised and canaries prove PCM/hypotheses/drafts reach no persistent or diagnostic sink. | §12; `LOCAL_VOICE_INPUT.md` §9 | target | `ACP-VOI-005` |

## Authority, collaboration, scale and quality

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-SAF-001` | Every mutation is typed, authenticated, idempotent, scope-checked and generation-fenced with a durable operation receipt. | §14 | partial | `ACP-SAF-001` |
| `PRD-SAF-002` | Administrative, delegated-control, context-broker, remote-runtime and companion capabilities are separate, least-privilege and revocable. | §5.2, §14 | target | `ACP-SAF-002` |
| `PRD-SAF-003` | Shared/portable workspace content cannot convey machine identity, credentials, consent, executable configuration or authority without local adoption. | §14 | target | `ACP-SAF-003` |
| `PRD-SAF-004` | Multi-client state has authoritative revisions, reconnect snapshot/replay, durable tombstones and conflict rules for nodes, edges, Flows and lifecycle. | §14 | target | `ACP-SAF-004` |
| `PRD-SAF-005` | Co-attached viewers have explicit exclusive writer/lease handoff; concurrent input cannot interleave silently. | §7, §14 | target | `ACP-SAF-005` |
| `PRD-SAF-006` | Uncertain launch/input/context/message/remote cleanup is never retried blindly and retains a deterministic reconcile/manual state. | §14 | partial | `ACP-SAF-006` |
| `PRD-SAF-007` | No resume, model, flag, host, provider, context or telemetry failure silently downgrades to a different behavior or fabricated fact. | §1.1, §14 | partial | `ACP-SAF-007` |
| `PRD-SAF-008` | Closed privacy inventory, retention, export and deletion cover runtime metadata, terminal history, context, resource content, usage, models and remote tombstones. | §14; `PRIVACY.md` | partial | `ACP-SAF-008` |
| `PRD-SAF-009` | Optional operator sharing is separately granted, end-to-end encrypted and scope-bound; ephemeral presence never grants input, control, credentials or checkout authority. | §14 | target | `ACP-SAF-009` |
| `PRD-SAF-010` | Multi-client sync fixes generation/revision/watermark/ack/gap/compaction semantics, per-object conflict rules and permanent deletion fences; offline drafts never replay as mutations without revalidation. | §14; `PROTOCOL.md` | target | `ACP-SAF-010` |
| `PRD-SAF-011` | Portable imports use package-local ids, remint every local semantic id, omit runtime/authority/machine ids and cannot collide with, update or resurrect existing objects. | §14 | target | `ACP-SAF-011` |
| `PRD-SAF-012` | Companion actions are a closed revision-fenced set with expiry/receipts/offline refusal; only an exact separately granted typed permission response may be remote, while secrets, authority, administration, destructive lifecycle, host trust and repository integration stay desktop-foreground-only. | §10, §14; `PROTOCOL.md` | target | `ACP-SAF-012` |
| `PRD-SAF-013` | Retention is numerically bounded for Flow revisions, sync journals, status/diagnostics, input leases and share metadata; compaction preserves stale-client resurrection fences and deletion/export proof. | §14; `PRIVACY.md` | target | `ACP-SAF-013` |

### Scale and quality

| Requirement | Normative outcome | Contract | Current | Acceptance |
| --- | --- | --- | --- | --- |
| `PRD-SCL-001` | The control plane handles at least 50 concurrent Sessions, 100 live runtimes, 1,000 nodes and simultaneous Attention changes on declared hardware. | §13 | partial | `ACP-SCL-001` |
| `PRD-SCL-002` | Tree virtualisation, bounded projections, visible-view-only heavy subscriptions, coalescing and bounded queues protect terminal input from background work. | §13; `PERFORMANCE.md` | partial | `ACP-SCL-002` |
| `PRD-SCL-003` | UI/resource pressure may park views or caches but never silently terminate/suspend agent, subagent or tool runtimes. | §13 | target | `ACP-SCL-003` |
| `PRD-SCL-004` | Keyboard, screen reader, non-colour status, reduced motion, zoom, IME and focus restoration cover every hierarchy and WorkSurface kind. | §13; `ACCESSIBILITY_ACCEPTANCE.md` | partial | `ACP-SCL-004` |
| `PRD-SCL-005` | Performance evidence reports workload, hardware and p50/p95/p99 view-route/input latency plus bounded memory, disk, queue and recovery behavior. | §13; `PERFORMANCE.md` | partial | `ACP-SCL-005` |
| `PRD-SCL-006` | Adversarial recovery covers duplicate create, stale hook, child outliving parent, offline completion, client conflict, writer handoff, reboot and remote outage. | §6–7, §14 | partial | `ACP-SCL-006` |
| `PRD-SCL-007` | Scale evidence uses a fixed minimum host/build profile, 30-minute sustained plus burst workload and numeric memory/disk/queue limits; pressure covers GPU, memory, PTY, descriptors, processes, disk/journal, hooks, remote and collectors. | §13; `PERFORMANCE.md` | target | `ACP-SCL-007` |
| `PRD-SCL-008` | Detailed accessibility acceptance covers every NodeKind WorkSurface, catalog, Flow/Team controls, diagnostics, status/HUD, file/SCM/conflict views, remote writer handoff and companions with keyboard/screen-reader/focus oracles. | §13; `ACCESSIBILITY_ACCEPTANCE.md` | target | `ACP-SCL-008` |
| `PRD-SCL-009` | A full authenticated remote/headless operator surface can use the canonical hierarchy, views, flows, terminal streams and Attention protocol with identical revision/recovery semantics while server-side desktop-only authority remains unavailable. | §14; `PROTOCOL.md` | target | `ACP-SCL-009` |
| `PRD-SCL-010` | Desktop and companion project usage, context and bounded activity inbox per exact AccountProfile/target with independent freshness/coverage and never turn absence, partial pages or errors into false zero. | §9–10; `PERFORMANCE.md`, `PRIVACY.md` | target | `ACP-SCL-010` |

## Completeness rule

`make product-spec-acceptance` verifies the exact manifest ids and semantic hashes, the one-to-one `PRD-*` ↔
`ACP-*` mapping and mutation resistance. This is the specification gate only. `make
product-completion-acceptance` separately requires every status to be `implemented`, a one-to-one immutable
implementation-evidence record, a reachable implementation commit, execution of the requirement-derived
oracle target and matching hashes for its current-run artifacts. Honest gaps must never be renamed to make the
specification gate green.
