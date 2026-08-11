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
