# Turn — Unified hierarchy upgrade

**Status:** normative, accepted, implementation target for the first vertical.  
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
    ├── 0..1 active WorkspaceWriteLease for the primary checkout
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

An active second main-checkout session is a conflict, never a silent success. Creation must return the current owner and the alternatives: focus it, create read-only, create an isolated worktree, or cancel.

```text
WorkspaceWriteLease
  id, workspace_id, session_id, checkout_id
  mode = exclusive_write
  state = active | released | stale
  acquired_ms, heartbeat_ms, released_ms?
```

SQLite enforces at most one active `exclusive_write` lease per workspace/checkout with a partial unique index. The daemon owns acquisition, heartbeat and release. A stale lease is never stolen solely because a timer elapsed: Turn first proves that its owning session cannot still be live, or asks the user.

### AgentNode independent of Pane

`ProcessNode` remains the persisted runtime identity; an agentic node gains the complete AgentNode fields. It exists without a pane, owns its event stream and retained terminal buffer, and may have zero, one or several pane bindings. Panes are views. `ClosePane(KeepProcesses)` removes only a binding; terminating an agent remains a separate explicit request.

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

Parent edges carry both meaning and confidence. `spawned_by` is not synonymous with certainty; inferred edges remain visibly provisional and can be corrected.

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

Updates are coalesced rather than byte-driven: visible active nodes at most every 250–500 ms, visible background nodes every 1–2 s, collapsed nodes only on semantic changes, archived nodes never. The persisted last preview lets the tree return immediately after a UI restart. Preview history is bounded to 20 entries per node and 2,000 globally, pruned on write; no raw PTY bytes, terminal grid or scrollback are stored. Visibility may be disabled globally, per Session or per Agent.

Quick Preview is an overlay driven by the selected node. `Space` opens it without changing layout, session, pane focus, process state or OS focus. It contains stable preview history and can promote the node to an explicit pane action. `Esc` closes it.

## State machines

Agent runtime state remains the existing two-dimensional model:

```text
Lifecycle: Spawning → Alive ↔ Reconnected
                 ├──────→ Orphaned → Reconnected | Lost
                 └──────→ Exited(code) | Signaled(signal) | Stopped(signal)
Turn:      Idle → Active ↔ AwaitingUser(reason)
                  ├────→ Done → Active
                  ├────→ TaskDone
                  └────→ Failed(reason)
            any state may degrade to Unknown when no adapter can vouch for it
```

The visible state is a daemon-derived projection with this precedence: `FAILED`, `PERMISSION`, `YOUR TURN`, `WAITING`, `RUNNING`, `DONE`, `STOPPED`, `IDLE`, `UNKNOWN`. Completing a turn does not imply process exit. Process exit clears pending permissions/questions and resolves attention for that node.

Session aggregate state is derived from every node plus pending attention. Session lifecycle/mode is independent:

```text
Active ↔ Paused → Archived
  │
  └── restore: Live | Reattached | PartiallyRestored | LayoutOnly
```

## Events

All events retain timestamp, workspace/session/node/parent identity, source, confidence, severity, payload, deduplication key and optional redacted raw source. Add these stable names:

- `workspace.write_lease_requested`, `.acquired`, `.denied`, `.released`
- `session.read_only_created`, `session.worktree_created`
- `agent.declared`, `.renamed`, `.spawned`, `.relationship_discovered`, `.relationship_corrected`
- `agent.preview_updated`, `.preview_redacted`, `.pane_opened`, `.pane_closed`
- Client audit records `tree.node_expanded`, `.node_collapsed`, `.node_selected`

`agent.spawned` creates or updates an AgentNode under its reported parent, preserves a name only when the
parent/integration actually declared one, starts preview tracking and pushes a tree change. Claude/Codex
`agent_type` values such as `Explore` or `default` are roles, not automatically declared names. It does
**not** create a Pane, mutate Layout, select the node or emit a focus effect.

Runtime `TurnEvent` keeps its required Session identity. Lease request/denial before Session creation and
tree expansion/selection are persisted in dedicated audit/UI-state records rather than forged into a
session-scoped agent event. Lease acquisition/release may emit a `TurnEvent` only once an owning Session
exists; the canonical lease record remains the lease table.

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
         ClosePane, ListPanesForNode
Lease:   AcquireWorkspaceWriteLease, ReleaseWorkspaceWriteLease,
         GetWorkspaceWriteLease, CreateReadOnlySession, CreateWorktreeSession
```

Implemented members are exposed as request/response pairs and revisioned full-snapshot pushes. A client
that misses a revision asks for resync rather than applying a partial guess. Tree UI state is keyed by a
stable `surface_id`; selection/expansion from two windows is never broadcast between them. Mutations remain
daemon-authoritative; the GUI never fabricates a relationship, state, lease or preview confidence.

## SQLite v3 migration

Migration 003 is append-only and transactional. It adds:

- session `mode`, `checkout_id`, `worktree_path`, and `read_only_enforced` columns;
- process-node declared/display naming, relationship kind/confidence and preview visibility fields;
- `workspace_checkouts`, `workspace_write_leases`, `activity_previews`, `pane_node_bindings`, and `tree_ui_state` tables;
- indexes for active leases, preview history, pane bindings and workspace tree loading.

`pane_node_bindings` is authoritative after migration. During backfill, a non-null `Pane.node_id` in the
saved Layout wins over the legacy reverse `ProcessNode.pane_id`; a legacy reverse pointer is imported only
when no Layout pane contradicts it, and disagreements produce a reconciliation audit record.

Existing Sessions migrate conservatively. A Workspace with no live legacy process may assign its most
recent active Session `main_checkout` and acquire a lease during daemon reconciliation. If two or more
legacy Sessions may still be writing, none is silently declared safe: they retain `read_only_enforced =
false`, the Workspace enters `needs_lease_reconciliation`, and the UI asks the user to focus/stop/isolate
before granting a lease. Other inactive Sessions become read-only; no worktree is invented. Existing
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
3. `ToggleSessionOverview` and the thumbnail grid duplicate Sessions outside the tree. They are removed from persistent navigation.
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
7. Add Quick Preview and temporary pane binding. Closing it removes the binding only.
8. Restore tree UI state, relations, preview, bindings and live processes without opening a new pane.
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
→ restart Store/daemon/client model
→ assert tree edge, name, preview, lease and process metadata remain
→ assert no new pane was opened during restore
```

The deterministic Reviewer fixture has a semantic stream but no independent PTY, matching Claude Code's
current subagent hook contract. Its temporary pane therefore renders Preview/Details; a terminal pane is
only offered when an integration supplies a real attachable stream.

Separate GUI tests verify tree keyboard semantics, selection/focus/attention independence and snapshots at ordinary and dense (30-agent) sizes.

Reproduce the vertical and the native UI evidence with:

```sh
cargo test -p turnd --test agents \
  the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart \
  -- --exact --test-threads=1 --nocapture
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
