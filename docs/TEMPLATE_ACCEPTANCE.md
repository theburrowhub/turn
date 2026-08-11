# Template lifecycle acceptance

This is the reproducible, headless acceptance artifact for Turn's complete Template lifecycle. Run from
the repository root:

```sh
make template-acceptance
```

The target does not open a window or require an installed agent CLI. It exercises the same pure UI
drafts, typed protocol requests, daemon handlers and SQLite repositories used by the desktop build.

| Requirement | Reproducible evidence |
| --- | --- |
| First run offers one safe built-in, Two Shells | `the_built_in_set_is_present_and_valid`, `the_first_run_preset_is_two_equal_portable_shells`, `the_layout_editor_starts_as_two_portable_shell_columns` |
| Create and edit visually without JSON | `the_template_editor_round_trips_every_startup_field_without_json`, `every_template_ui_action_maps_to_an_explicit_daemon_operation` |
| Capture a Session under an explicit editable name | `every_template_ui_action_maps_to_an_explicit_daemon_operation`, `saving_a_live_layout_as_a_template_drops_process_bindings` |
| Duplicate and protect built-ins | `the_safe_template_builtin_is_read_only_but_can_be_duplicated` |
| Select Global and Workspace defaults with the narrower choice winning | `a_workspace_template_default_overrides_the_global_template_default`, `template_lifecycle_preserves_configuration_and_never_leaves_dangling_sessions` |
| Preserve rows, columns, proportions and commands per cell | `template_lifecycle_preserves_configuration_and_never_leaves_dangling_sessions`, `two_sessions_from_one_template_share_no_pane_ids` |
| Preserve cwd, Template/Pane environment, restore, init, Attention, naming and tmux configuration | `the_template_editor_round_trips_every_startup_field_without_json`, `template_lifecycle_preserves_configuration_and_never_leaves_dangling_sessions`, `read_only_template_resolution_keeps_daemon_owned_configuration` |
| Resolve relative paths against the Workspace and refuse escape before launch | `workspace_a_cannot_create_a_main_or_template_session_rooted_in_workspace_b`, `worktree_template_mapping_preserves_relative_cwds_and_remaps_absolute_ones` |
| Warn about a missing executable before launch | `template_lifecycle_preserves_configuration_and_never_leaves_dangling_sessions` checks the daemon-owned `missing_commands` projection; the New Session and Settings pickers render it |
| Apply from UI/Command Palette without an extra start interaction | `every_template_ui_action_maps_to_an_explicit_daemon_operation`; the daemon materialises the Session immediately |
| Never replace a running layout implicitly | `template_lifecycle_preserves_configuration_and_never_leaves_dangling_sessions` proves the second application is refused while a process runs |
| Delete safely while Sessions use the Template | `template_lifecycle_preserves_configuration_and_never_leaves_dangling_sessions` proves defaults/references are cleared while the stored Session layout survives |

Templates deliberately cannot approve permissions. Agent permission requests remain explicit runtime
decisions regardless of the Template that created the Pane.
