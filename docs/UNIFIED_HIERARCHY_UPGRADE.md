# Turn — Unified hierarchy upgrade

**Status:** normative, accepted, implemented and audited for the first vertical.
**Precedence:** this document supersedes earlier navigation, simultaneous-session and automatic-subagent-pane decisions wherever they conflict. See ADR-040.

## Product invariant

Turn has one persistent navigation surface and one hierarchy projection:

```text
Workspace
└── Session
    ├── Agent
    │   ├── Subagent
    │   └── Process
    ├── Shell
    ├── TUI
    └── Background Process
```

The left tree is the source of truth for navigation, state and supervision. Workspace, Session, Agent and Process are not duplicated as persistent tabs, thumbnail strips or a second tree. The optional right inspector shows details for the selected node; it never repeats navigation. The centre contains only panes chosen by the user, a template, restoration, or an explicit automation.

Normalised ownership remains in the existing Workspace/Session/process records; the tree does not replace
those foreign keys. The UI keeps three independent values: `selected_tree_node`, `focused_pane` and pending
`AttentionEntry` values. Selecting an item does not focus a pane or resolve attention. Opening or closing a
pane does not start or stop its AgentNode.

## Domain model

```text
Workspace 1 ── * Checkout
    │             └── Primary or Worktree; declares shared resources
    ├── 0..1 unreleased blocking WorkspaceWriteLease claim for the primary checkout
    └── * Session
          ├── mode: MainCheckout | ReadOnly | IsolatedWorktree
          ├── checkout_id / cwd / branch / layout
          ├── * ProcessNode
          │      ├── agent metadata when agentic
          │      ├── parent + Relationship{kind, confidence}
          │      ├── AgentName{declared, display, source, confidence, user_renamed}
          │      └── ActivityPreview
          └── * Pane binding * ── 1 ProcessNode
```

Normalised ownership stays explicit: `Session.workspace_id`, `ProcessNode.session_id`, and `ProcessNode.parent` remain foreign keys/pointers. The unified tree is a projection of these records, not a polymorphic table that hides ownership.

### Session modes and checkout safety

- `main_checkout`: read/write access to the workspace primary checkout and ownership of its exclusive write lease.
- `read_only`: review/research against the primary checkout. Turn must use a technical guard when viable and must require explicit escalation before a write-capable relaunch.
- `isolated_worktree`: independent path and branch. It may write concurrently, but Turn warns about declared shared ports, containers, databases, caches, credentials and services.

An active second main-checkout session is a conflict, never a silent success. Creation must return the
current owner and the alternatives: focus it, create read-only, create an isolated worktree, or cancel. If
the failed creation came from a Template, the safe retry retains only its Template id and interpolation
inputs; the daemon re-instantiates authoritative Layout, commands, environment, Attention, tmux, name and
cwd rules. Read-only launches no process without enforcement, while worktree cwd values are mapped to the
equivalent repository-relative directory in the isolated checkout.

```text
WorkspaceWriteLease
  id, workspace_id, session_id, checkout_id
  mode = exclusive_write
  state = active | recovery_required | stale | released
  acquired_ms, heartbeat_ms, released_ms?, generation
```

Within one canonical Turn data directory/store, SQLite and the daemon transaction enforce one canonical
writer namespace per checkout. A uid-scoped host lock keyed by checkout device/inode joins that authority
across deliberately separate data directories. Every non-released
lease blocks acquisition; the fencing generation remains monotonic even if a Workspace is deleted and
recreated. The daemon owns acquisition, heartbeat and release. A new daemon first changes every unreleased
lease to `recovery_required`; loading the former Session never adopts it. A stale or recovery lease is never
stolen solely because a timer elapsed: release and reacquisition remain explicit and fenced.

Checkout leases assume one store owner. Before opening SQLite, applying migrations or restoring Sessions,
the daemon acquires an exclusive process lock on the canonical data directory. Socket paths are only control
endpoints and do not define store ownership: a second daemon using another socket or a symlink alias is
refused before it can fence live leases. Process death releases the kernel lock; the stable lock file itself
is never removed.

The checkout lock is acquired before SQLite and its descriptor is inherited by every main-checkout process.
If the daemon dies, a surviving writer keeps the lock; reconciliation can reacquire it only after the final
writer exits. Symlink aliases collide, while distinct Git worktree directories keep independent authority.

### AgentNode independent of Pane

`ProcessNode` remains the persisted runtime identity; an agentic node gains the complete AgentNode fields.
It exists without a Pane and may have zero, one or several Pane bindings. Events and daemon runtime state
are correlated by its `NodeId`; only a PTY-backed node has a daemon-owned `PtyProcess` and terminal buffer,
while a semantic subagent normally has neither. Panes are views. `ClosePane(KeepProcesses)` removes only a
binding; terminating an Agent remains a separate explicit request.

Agent naming is lossless:

```text
declared_name: name supplied by parent/integration
display_name: user-visible name
name_source: explicit_parent_event | integration | structured_task |
             process_title | inferred | fallback
name_confidence: explicit | integrated | inferred_high | inferred_low | unknown
user_renamed: bool
```

Priority is explicit parent name, hook/integration name, structured task, process title, high-confidence inference, then `Subagent N`. A user rename changes only `display_name` and `user_renamed`.

Typed input is not trusted display input. Workspace, Session and Template names containing C0/C1, ANSI,
bidi or invisible formatting are rejected so identity is never repaired silently. Discovered Agent/process
names, task, command, cwd, title and structured argv are sanitised and bounded before state, protocol,
inspector and persistence. Raw OS metadata remains transient for PID traversal/classification only.

Parent edges carry both meaning and confidence. `spawned_by` is not synonymous with certainty; inferred edges remain visibly provisional and can be corrected.

When a worker callback arrives through an authenticated parent endpoint without a resolvable node, Turn
persists the Attention subject as `(session_id, parent_node_id, external_worker_id?)`. The parent is a
correlation boundary, not a claim that the parent itself asked. An explicit unknown worker id never falls
through to a different unique child, and an id-less response under one parent cannot clear an unresolved
demand under another parent in the same Session. A later declaration may bind the same external id; a
subsequent callback for that id then correlates to the exact node and resolves the provisional scope.
Declaration alone does not silently acknowledge a pending demand. Terminal lifecycle uses a separate ownership rule: the exact child
and its matching pre-declaration scope are retired together, while a dead parent clears unresolved scopes
anchored on that runtime without erasing exact children that may still be alive. Every live cleanup is
persisted and projected before restart can resurrect stale Attention.

Permission detail follows identity, never Session proximity. A client may show command, cwd and risk only
for the exact `node_id` (including an exact primary-Agent summary received before the tree). A scoped
node-less demand or stale exact id remains provisional in the queue and never borrows the primary Agent's
pending permission.

Attention navigation also separates semantic subject from input ownership. `Next Attention` selects the
exact AgentNode. If a semantic subagent shares an ancestor's PTY, the daemon may focus that existing runtime
Pane only across an integrated/explicit `spawned_by` or `owns_process` edge, returning both node identities.
It never relabels the parent, crosses a distinct child runtime/provisional edge or creates a Pane. With no
safe existing input Pane, selection remains on the subject and opening stays explicit.

## Activity preview

Raw terminal state and activity preview are different products. A preview is a stable, redacted, compact
fact, not restored conversation history:

```text
ActivityPreview
  node_id, raw_source_sequence, normalized_text
  source_type, confidence, stable
  contains_sensitive_data, redacted, updated_ms
```

Source priority is semantic event, adapter state, detected relevant action, stable rendered line, process fallback. Normalisation strips ANSI/control sequences, resolves carriage-return rewrites, rejects spinners/prompts/repeated noise, preserves Unicode, applies known-secret redaction and caps the result at a safe character boundary.

Updates are coalesced rather than byte-driven. The current daemon samples watched PTY-backed nodes at a
500 ms interval and unwatched ones at 1.5 s, suppresses archived Sessions, and emits semantic changes
immediately; byte traffic never maps one-for-one to UI updates. Expansion-aware suppression for collapsed
nodes and the wider semantic source ladder remain planned. The persisted last preview lets the tree return
immediately after a UI restart. Preview history is bounded to 20 entries per node and 2,000 globally,
pruned on write; this semantic preview store contains no raw PTY bytes, terminal grid or scrollback.
ADR-044's separate private bounded terminal archive does not feed hierarchy previews. The product requires global,
per-Session and per-Agent visibility controls; protocol v3 currently enforces the per-node Agent control,
while the broader scopes remain planned rather than client-local guesses.

Quick Preview is an overlay driven by the selected node. `Space` opens it without changing layout, session, pane focus, process state or OS focus. It contains stable preview history and can promote the node to an explicit pane action. `Esc` closes it.

A temporary Pane is scoped to one live UI `surface_id`. Another surface never sees its binding. A
replacement connection, the last disconnect and a daemon restart remove that temporary binding before a
new hierarchy snapshot is built; expansion/selection, permanent Layout bindings and the Agent survive. This
is intentionally different from restoring a user-saved Pane: an ephemeral view without its owning UI would
be an unfocusable phantom.

## State machines

Agent runtime state remains the existing two-dimensional model:

```text
Lifecycle: Spawning → Alive
                 ├──────→ Orphaned → Lost
                 └──────→ Exited(code) | Signaled(signal) | Stopped(signal)
Turn:      Idle → Active ↔ AwaitingUser(reason)
                  ├────→ Done → Active
                  ├────→ TaskDone
                  └────→ Failed(reason)
            any state may degrade to Unknown when no adapter can vouch for it
```

`Reconnected` remains a reserved lifecycle value for a backend that can prove PTY reattachment; the current
daemon-restart path deliberately emits only `Orphaned` or `Lost`. Reconnecting a UI to the same live daemon
reattaches a view and does not change the runtime lifecycle.

Node state is a daemon-derived projection whose actual labels include `starting`, `RUNNING`, `WAITING`,
`PERMISSION`, `QUESTION`, `turn done`, `DONE`, `FAILED`, `STOPPED`, `IDLE` and `UNKNOWN`. `YOUR TURN` is the
Session/Attention label, not a replacement for an Agent's exact
`WAITING` or `PERMISSION` state. A Session may therefore say `YOUR TURN` while its exact child still says
`WAITING`, and a scoped unresolved demand may badge the Session while the parent Agent remains `RUNNING`.
Completing a turn does not imply process exit. Process exit clears pending permissions/questions, the exact
node's Attention and unresolved child scopes owned by that runtime; it does not clear exact live siblings or
children.

Session aggregate state is derived from every node plus pending attention. Session lifecycle/mode is independent:

```text
Active ↔ Paused → Archived
  │
  └── restore: Live | Reattached | PartiallyRestored | LayoutOnly
```

## Events

The accepted event model reserves these stable names:

- `workspace.write_lease_requested`, `.acquired`, `.denied`, `.released`
- `session.read_only_created`, `session.worktree_created`
- `agent.declared`, `.renamed`, `.spawned`, `.relationship_discovered`, `.relationship_corrected`
- `agent.preview_updated`, `.preview_redacted`, `.pane_opened`, `.pane_closed`
- Client audit records `tree.node_expanded`, `.node_collapsed`, `.node_selected`

This list is not a claim that every name is an `EventKind` in the current build. Runtime `agent.spawned` is
implemented as a `TurnEvent`; previews, Pane bindings, leases and per-surface tree state currently use their
dedicated typed records/pushes. The `workspace_audit_events` schema reserves pre-Session audit storage, but
its complete repository/emission path and the remaining rename/correction audit names are planned. A client
must not fabricate them locally.

`agent.spawned` creates or updates an AgentNode under its reported parent, preserves a name only when the
parent/integration actually declared one, starts preview tracking and pushes a tree change. Claude/Codex
`agent_type` values such as `Explore` or `default` are roles, not automatically declared names. It does
**not** create a Pane, mutate Layout, select the node or emit a focus effect.

Runtime `TurnEvent` keeps its required Session identity. Lease request/denial before Session creation and
tree expansion/selection are persisted in dedicated audit/UI-state records rather than forged into a
session-scoped agent event. Lease acquisition/release may emit a `TurnEvent` only once an owning Session
exists; the canonical lease record remains the lease table.

Every durable free-text field is scanned before SQL, including typed event/provenance JSON, Session tree,
Layout/Template, Attention and preview state. Typed ids/FKs are immutable. Workspace/checkout filesystem
identities fail instead of being rewritten when scanning would alter them; a redacted operational value is
not eligible for silent relaunch, resume or correlation.

An accepted runtime event crosses one durable boundary before any client sees it:

```text
Session tree/layout/preview → event log → Attention Queue → COMMIT → UI pushes/effects
```

A failure rolls the complete checkpoint back and places later runtime events behind a FIFO retry barrier.
This prevents restart from combining a permission with a missing Agent, a tombstone with no Stop event or a
resolved runtime with stale Attention.

## Internal APIs

The list below is the accepted product boundary, not a claim that every operation is already exposed by
protocol v3. The current vertical implements hierarchy list/subscription via revisioned snapshots,
expand/collapse/select, preview history/visibility, temporary Pane open/focus/close, and lease
acquire/release plus read-only/worktree creation. Rename, relationship correction, visibility modes,
explicit redaction, permanent `OpenNodeAsPane`, `ListPanesForNode` and direct preview subscriptions remain
planned and must not be simulated only in the GUI.

```text
Tree:    ListWorkspaceTree, SubscribeTreeChanges, ExpandNode, CollapseNode,
         SelectNode, CorrectRelationship, RenameNode, SetVisibilityMode
Preview: SubscribeActivityPreviews, GetActivityPreview, GetPreviewHistory,
         SetPreviewVisibility, RedactPreview
Pane:    OpenNodeAsPane, OpenNodeAsTemporaryPane, FocusPaneForNode,
         FocusPaneForAttention, ClosePane, ListPanesForNode
Lease:   AcquireWorkspaceWriteLease, ReleaseWorkspaceWriteLease,
         GetWorkspaceWriteLease, CreateReadOnlySession, CreateWorktreeSession,
         CreateReadOnlySessionFromTemplate, CreateWorktreeSessionFromTemplate
```

Implemented members are exposed as request/response pairs and revisioned full-snapshot pushes. A client
that misses a revision asks for resync rather than applying a partial guess. Tree UI state is keyed by a
stable `surface_id`; selection/expansion from two windows is never broadcast between them. Mutations remain
daemon-authoritative; the GUI never fabricates a relationship, state, lease or preview confidence.

## SQLite migration policy

Migration 003 is append-only and transactional. It adds:

- session `mode`, `checkout_id`, `worktree_path`, and `read_only_enforced` columns;
- process-node declared/display naming, relationship kind/confidence and preview visibility fields;
- `workspace_checkouts`, `workspace_write_leases`, `activity_previews`, `pane_node_bindings`, and `tree_ui_state` tables;
- indexes for active leases, preview history, pane bindings and workspace tree loading.

`pane_node_bindings` is authoritative after migration. During backfill, a non-null `Pane.node_id` in the
saved Layout wins over the legacy reverse `ProcessNode.pane_id`; a legacy reverse pointer is imported only
when no Layout pane contradicts it, and disagreements produce a reconciliation audit record.

Existing Sessions migrate conservatively. Migration 003 creates no lease and chooses no writer. Migration
005 removes historical raw hook callback bodies. Migration 006 re-resolves filesystem identity and marks
every pre-existing Workspace and every non-released legacy claim as requiring explicit reconciliation;
neither database open nor daemon launch may clear that gate or acquire authority as a side effect. Until
the audited reconciliation action is implemented, those upgraded Workspaces remain deliberately
fail-closed rather than being assigned to the “most recent” Session. Migration 007 adds optional
`parent_node_id` and `subject_external_id` columns to durable Attention entries so restart preserves an
unresolved callback's narrow scope; legacy rows receive nulls and no inferred owner. Migration 008 adds
`survives_owner_exit` and `demand_kind`, keeping legacy rows as non-surviving interactions while allowing
turn/task completion and failure evidence to remain actionable after the runtime exits. Migration 009 marks
legacy durable text for a retryable physical credential purge; structural ids and fencing paths are validated
and never rewritten, while SQLite/WAL are rebuilt before the marker is cleared. No worktree is invented. Existing
`pane_id` values become rows in `pane_node_bindings`. Existing `Relation::Confirmed` maps to
`spawned_by/explicit`; `Inferred` maps to `spawned_by/inferred_high`; `Unknown` remains unknown. Existing
agent titles become display names, with no fabricated declared name. No process is launched, killed,
moved, or retroactively made read-only by the migration.

## Updated wireframe

```text
┌──────────────────────┬──────────────────────────────────────┬───────────────┐
│ TURN                 │ Fix climbing bugs                    │ Inspector  ×  │
│ WORKSPACES           │ space-troopers / fix/climbing  MAIN  │ Agent         │
│ ▾ space-troopers     │ ◆ YOUR TURN                          │ Reviewer      │
│   ▾ Fix climbing     ├──────────────────────────────────────┤ parent Claude │
│     ◆ Claude WAITING │ ┌────────────────┬─────────────────┐ │ state RUNNING │
│       “Commit?”      │ │ Claude Code    │ Shell           │ │ relation      │
│     ▾ Reviewer  ●    │ │                │                 │ │ preview       │
│       “Reviewing…”   │ ├────────────────┴─────────────────┤ │ actions       │
│     ▾ Tests     ●    │ │ Fang                             │ │               │
│       “12/18”        │ │                                  │ │               │
│       ○ Jest         │ └──────────────────────────────────┘ │               │
│       ○ Typecheck    │                                      │               │
│     ○ Shell          │                                      │               │
│     ○ Fang           │                                      │               │
│ ▸ turn               │                                      │               │
│ ▸ personal-infra     │                                      │               │
├──────────────────────┴──────────────────────────────────────┴───────────────┤
│ ● turnd · Next attention: Reviewer · Focus: Claude · Layout: Coding         │
└──────────────────────────────────────────────────────────────────────────────┘
```

Keyboard while the tree owns focus: Up/Down visible node, Right expand/first child, Left collapse/parent, Enter activate/focus existing pane, Space Quick Preview, Cmd+Enter temporary pane, Esc close preview/return to tree. Expand/collapse and selection persist.

## Historical incompatible implementation identified

This is the audit that drove the migration. The current first vertical has removed or replaced each item;
the list remains so a future refactor does not reintroduce them:

1. `turn-gui::view` renders only a flat Session sidebar; Workspaces and process nodes are absent.
2. The Attention Queue is a second persistent right navigation panel, while the right side must become an optional contextual inspector. Queue ordering remains a logical service and `Next Attention` command, with non-navigation UI on demand.
3. `ToggleSessionOverview` and its hidden thumbnail feed duplicate Sessions outside the tree and consume
   background rendering work. The command, shortcut, feed, repaint cadence and module are removed, not hidden.
4. `ToggleAgentTree` assumes an optional second tree. The unified tree is always the navigation source.
5. `ProcessNode.pane_id: Option<PaneId>` cannot express zero-to-many views and couples node lifetime to one view.
6. Session creation and duplication do not arbitrate checkout ownership.
7. `Relation::{Confirmed,Inferred,Unknown}` lacks the five-level confidence vocabulary and relation kind.
8. Subagent hook handling stores only `agent_type`; it must preserve the parent-declared name separately.
9. No stable/redacted preview pipeline or persisted tree expansion/selection exists.
10. Protocol clients bootstrap Workspaces, Sessions and per-Session trees separately; a unified workspace-tree projection and change push are required.

## Implementation and migration sequence

Steps 1–8 and the deterministic half of step 9 are implemented. The authenticated live Claude Code smoke
test in step 9 remains pending.

1. Land this normative document and ADR-040; update product/architecture/protocol claims.
2. Add domain types, event variants and deterministic migration 003 with upgrade tests.
3. Add store repositories and a daemon lease arbiter. Refuse conflicting main-checkout creation with a structured conflict containing the owner.
4. Add naming/relationship/preview ingestion. Preserve background AgentNodes independently of panes.
5. Add unified hierarchy protocol/view and full-replacement pushes; keep old list endpoints only as non-navigation compatibility until protocol v3 is adopted.
6. Replace the GUI sidebar/queue/overview/second-tree assumptions with the unified tree, contextual inspector and keyboard model.
7. Add Quick Preview and a surface-scoped temporary pane binding. Closing it removes the binding only.
8. Reconnect a replacement UI to the live daemon with tree UI state, relations, previews, permanent
   bindings and live processes intact, without opening a new Pane; expire temporary bindings before a new
   surface bootstrap. On daemon restart, restore the same durable metadata but report runtimes as
   `Orphaned`/`Lost`; do not claim PTY reattachment.
9. Demonstrate the vertical with a deterministic fixture adapter, then run a manual Claude Code smoke test when the CLI and credentials are available.

## Reproducible vertical acceptance

An automated integration test must execute this sequence without a paid external service:

```text
create workspace
→ create main session and acquire lease
→ launch deterministic primary-agent fixture
→ ingest explicit agent.spawned(Reviewer)
→ assert Reviewer is a child with declared name and no pane binding
→ ingest noisy ANSI/CR preview bytes and assert one stable redacted preview
→ request Quick Preview and temporary Preview/Details pane explicitly
→ close temporary pane and assert Reviewer remains alive
→ restart the UI client while the same daemon remains alive
→ assert tree edge, name, preview, lease and live runtime remain
→ assert no new pane was opened during restore
→ separately restart the daemon and assert durable metadata remains, runtimes are Orphaned/Lost,
  and nothing is relaunched automatically
```

The deterministic Reviewer fixture has a semantic stream but no independent PTY, matching Claude Code's
current subagent hook contract. Its temporary pane therefore renders Preview/Details; a terminal pane is
only offered when an integration supplies a real attachable stream.

Twenty native GUI tests verify tree keyboard semantics and selection/focus/attention independence; twelve
of them maintain committed PNG baselines, including an ordinary desk and a dense 30-Session tree. This is
not a measured 30-Agent performance claim.

Reproduce the vertical and the native UI evidence with:

```sh
cargo test -p turnd --test agents \
  the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart \
  -- --exact --test-threads=1 --nocapture
cargo test -p turnd --test agents \
  an_idless_worker_permission_round_trips_through_hooks_to_the_reviewer \
  -- --exact --test-threads=1 --nocapture
cargo test -p turnd \
  two_hook_parents_keep_out_of_order_and_idless_attention_in_their_own_scopes \
  -- --test-threads=1 --nocapture
cargo test -p turnd \
  a_data_directory_rejects_another_socket_and_recovers_after_sigkill \
  -- --test-threads=1 --nocapture
cargo test -p turnd --lib \
  core::requests::hierarchy::tests::the_reviewer_vertical_survives_a_ui_restart_without_changing_layout \
  -- --exact --test-threads=1
cargo test -p turn-gui --test snapshots -- --test-threads=1
```

The loopback-hook test exercises the real transport and production normaliser, but deliberately supplies
the supported explicit `agent_name: Reviewer` and task fields. Claude Code 2.1.221's recorded payload in
this repository supplied an external worker id and role (`Explore`) but not that parent-declared display
name. Until a live installed version emits an explicit name, Turn must display the role/fallback honestly
and enrich the same node later if a declaration arrives; it must never invent `Reviewer` from `Explore`.
