# Contextual inspector acceptance

This is the reproducible acceptance artifact for the optional Workspace, Session, Agent and Process
inspectors. Run from the repository root:

```sh
make inspector-acceptance
```

The target opens no desktop window. Native snapshot tests render through the headless GPU harness and the
remaining checks exercise the same typed protocol, daemon, redaction boundary and client routing used by
the application.

| Requirement | Reproducible evidence |
| --- | --- |
| Workspace paths, repository, checkouts, shared resources, lease and configuration | `workspace_contextual_inspector_is_optional_accessible_and_not_a_second_tree` and `workspace_contextual_inspector.png` |
| Session mode, checkout, branch, Template, Attention, processes and history | `session_contextual_inspector_exposes_context_attention_and_safe_history` and `session_contextual_inspector.png` |
| Agent identity, provider/model, work, permissions, context, metrics, parent and handoffs | `every_inspector_kind_is_complete_redacted_and_honest` |
| Process PID, PPID, process group, argv, cwd, exit, origin and logs/history | `every_inspector_kind_is_complete_redacted_and_honest` |
| Inferred relationships and origins never become certain | `every_inspector_kind_is_complete_redacted_and_honest` |
| Secrets do not cross the daemon inspection boundary | `every_inspector_kind_is_complete_redacted_and_honest` plus the `turn-store` redaction suite |
| One read-only typed request; mismatched or late answers cannot impersonate the selection | `an_inspector_request_for_a_hierarchy_row_is_one_typed_read_only_request`, `an_inspector_answer_must_match_the_row_that_was_requested`, `a_late_inspector_answer_is_never_presented_as_the_current_selection` |
| Optional, collapsible and one accessible context rather than a duplicate tree | `workspace_contextual_inspector_is_optional_accessible_and_not_a_second_tree` |
| Responsive overlay at narrow widths | `a_narrow_contextual_inspector_becomes_an_accessible_overlay` and `session_contextual_inspector_narrow.png` |

Inspector event rows contain bounded typed summaries and provenance, never raw hook payloads or terminal
transcripts. Workspace and Session environment variables are projected by name only. Contextual actions use
the same existing typed operations as the hierarchy; opening or closing the inspector changes no process,
Pane, layout, lease or Attention state.
