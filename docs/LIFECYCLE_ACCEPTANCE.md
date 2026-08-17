# Command Palette and lifecycle acceptance

Run from the repository root without opening the desktop application:

```sh
make lifecycle-acceptance
```

The target exercises the same typed protocol, daemon persistence, hierarchy actions and native-window
policy used by Turn.

| Requirement | Reproducible evidence |
| --- | --- |
| Quick Preview and Open Node are real hierarchy actions | `palette_hierarchy_commands_reuse_the_typed_tree_actions` |
| Search and typed Attention/Running/Failed/Archived filters are palette commands | the `Command::ALL` and keymap completeness tests plus `palette_hierarchy_commands_reuse_the_typed_tree_actions` |
| Pane movement, process signals and Template application remain discoverable commands | the keymap completeness suite and existing pane, terminal and Template suites |
| Workspace/Session rename edits the current value | `session_lifecycle_commands_use_real_values_and_typed_operations`; the old generated `"(renamed)"` path does not exist |
| Duplicate, archive and restore are separate real operations | the request catalogue plus the Desk command tests |
| Favorite and pin persist independently | `favourite_and_pin_are_durable_independent_session_choices` |
| Session ordering uses the daemon-owned `move_tree_node` operation | the hierarchy movement suite; Move Session Up/Down routes through that same action |
| Closing the Turn window preserves processes by default | `close_turn_defaults_to_preserving_every_running_process` and `close_turn_defaults_to_closing_only_the_window` |
| Stop-all/per-Session exit waits for daemon answers | `TurnApp` keeps the native close cancelled until every typed `close_session` request settles |
| Closing a view, detaching, stopping a Process, ending a Session and archiving remain distinct | the lifecycle, terminal and lease suites selected by this target |
| Atajos are configurable and discoverable | `every_command_is_bound_exactly_once_and_appears_in_the_palette` and the shortcut-settings suite |

Pause/resume is intentionally not exposed as a command yet. Turn can own a PTY while the semantic Agent is
a descendant process, and a restored process may be observable without being signalable. An OS-level
`SIGSTOP`/`SIGCONT` button would therefore sometimes pause the wrong runtime while retaining a checkout
lease. The honest current operations are interrupt, stop, end Session, and explicit relaunch/resume where
the adapter proves it is resumable. There is no disabled or placeholder pause control.

Destructive actions still use their contextual confirmation. Duplicate, favorite, pin, filter, preview and
open actions are non-destructive and execute immediately. If a palette command lacks a valid typed target,
the status bar says exactly what selection or state is required.

## Accepted successor lifecycle matrix

ADR-064 extends the post-v0.1 lifecycle target. These rows are acceptance obligations, not claims that
`make lifecycle-acceptance` proves them today:

| Requirement | Required successor evidence |
| --- | --- |
| Foreground Session activation needs no second action when safe | One Session selection emits one idempotent `activate_session`, restores/attaches and may start one configured default Shell only from a current fully preflighted plan. Retry does not double-start. Stale authority, target/account/cwd/command drift, an ambiguous survivor, missing containment, a permission or unsafe input owner launches nothing and presents one exact recovery action. Selecting a child, resource, history result or ended Session never activates it. |
| External WorkItems retain two lifecycle authorities | The canonical WorkItem follows its closed local transition table, while create/edit/assign/transition/close/reopen each require the exact `WorkItemSource` capability, item/source revision and external receipt. Timeout-after-possible-write enters reconciliation. Archive, dismiss or local projection deletion never closes or deletes the source item. |
| Provider-native Jobs are not Flow recurrence | Fixtures preserve one stable Job identity and ordered iteration identities across provider/daemon disconnect. List/create/update/pause/resume/run-now/cancel/delete are independent capabilities and receipts. Hide/dismiss/local deletion has no provider effect; destructive provider deletion is a separately labelled foreground action. |
| Conversation inventory, adoption and resume remain separate | Search and bounded history pages launch nothing and cannot prove absence from partial coverage. Adopt creates one stopped Node and installation-wide exact ConversationKey ownership without provider input. Resume performs a fresh foreground target/account/model/cwd/containment preflight and creates at most one new RuntimeAttempt. Similarity never binds ownership. |
| Web preview and Browser have different continuity | Restoring an inert Web Resource loads no URL or script. Restoring a Browser Node restores inert metadata and partition identity but does not reload or navigate; reviewed navigation/history/popup/download/storage operations are explicit and receipt-backed. Destroy clears only that partition and never claims server-side deletion. |
| Title read and rename do not share authority | Provider title read is a read-only capability. Rename is a separately advertised, expected-revision/idempotent operation whose requested title becomes effective only after correlated provider evidence; ambiguous or rate-limited outcomes reconcile rather than invent success. |
| Remote permission response is narrow, not terminal input | A full remote GUI or companion may send allow/deny only for a known typed interaction under an exact single-use foreground-issued grant. While that interaction is sensitive, raw remote PTY input is rejected. Credential/admin/trust/grant/destructive operations stay desktop-foreground-only; an unclassifiable generic TUI receives no fabricated typed-permission guarantee. |
| Account activity is profile-scoped and tri-state | Context windows, quota windows and the bounded activity inbox keep exact provider/AccountProfile/target identity plus independent source, coverage and freshness. Missing, partial, stale, rate-limited and failed reads never become zero usage, zero remaining or authoritative empty history. |

A full remote GUI is a revisioned WorkSurface client subject to the same typed lifecycle operations and input
leases as a local GUI. A headless status client and a companion are bounded projections with closed action
allowlists; neither may inherit arbitrary lifecycle or PTY authority merely because the full GUI supports it.
