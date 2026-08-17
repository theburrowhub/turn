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

## Post-v0.1 successor

ADR-059 keeps every redaction, provenance, late-response and accessibility guarantee above, but moves Agent
and Process detail inside the selected `NodeView` on the single WorkSurface. The inspector is no longer a
second narrow destination for primary agent content. New acceptance must additionally cover requested versus
effective launch facts, stable instance/attempt history, context usage versus shared quota, context links,
handoff lineage and honest stale/unavailable observations. At least one authenticated live adapter must
provide effective model plus context usage, at least one real provider/account must provide quota, and at
least one provider transcript must support bounded pull/handoff; these may be different providers, while
negative fixtures prove unsupported states remain unavailable rather than fabricated. See
`docs/AGENT_NODE_VIEWS_AND_CONTEXT.md` §4, §6 and §11.

ADR-064 adds the following exact detail views and cross-product fixtures to that successor acceptance. Each
is primary content for the selected tree Node on the same WorkSurface; none introduces an inspector tree,
activity queue or hidden lifecycle authority:

| Selected target | Required facts and negative proof |
| --- | --- |
| AccountProfile | Provider/profile/ExecutionTarget identity; auth/retirement state; requested/effective model and flags; independently timestamped context and quota windows; and a bounded activity inbox with source, coverage and freshness. Unsupported, missing, partial, stale, rate-limited and failed observations render those exact states, never false zero usage/remaining or an authoritative empty inbox. Another profile's cache, conversation, job or activity never appears. |
| Conversation inventory result or adopted Agent | Exact ConversationKey, profile/target/namespace, provider title only when `title_read` is current, revision, ownership match, resumability and coverage. Search/matching is advisory and read-only. Adopt and resume are visibly separate; adopt shows a stopped Node and zero launch/input, while resume previews the new attempt's target/account/model/cwd/containment consequences. |
| Conversation title | Requested, provider-observed and effective title remain separate. `title_read` and `conversation_rename` have independent capability/freshness rows. A rename action is absent when only read is supported and never reports success without the exact expected-revision receipt. |
| WorkItem | Stable source/project/external key, field/state mapping, per-field authority, assignee mapping, source/item revisions, page/cache coverage, rate status, conflict/reconcile state and receipts. Close/reopen or source deletion is never inferred from a dismissed card, cache miss or partial page. Credential references remain opaque. |
| Native Job and iteration | Stable NativeJobKey, provider schedule/time zone/revision, requested/effective model and flags, native/normalised state, freshness, next/last run and each stable iteration/result/linked runtime. Flow recurrence is labelled as a different authority; local hide/delete and provider cancel/delete are different actions. |
| Web Resource | A bounded inert reference/snapshot and provenance. It exposes no interactive Browser storage, history, ambient credentials or automatic load. |
| Browser Node | Exact isolated partition, current origin/address, stable history entry, load/TLS/error state, reviewed localhost/local-HTML binding, permissions, popup/download disposition and storage-clear action. Page/script content remains untrusted and cannot become an inspector action or Attention resolution. Restore displays metadata without reloading. |
| Pending permission on a remote-capable surface | Exact provider options and interaction/attempt/authority revisions. A remote response control exists only under a still-valid single-use foreground-desktop-issued encrypted grant; raw remote terminal input is disabled for that known sensitive interaction. Credentials, grant changes, administration and host trust have no remote action. |

Native headless snapshot tests prove rendering and redaction only. Full remote-GUI behaviour requires an
authenticated revision/sync and input-lease integration test; headless status and companion projections
must be tested separately so their smaller allowlists cannot inherit full WorkSurface controls.
