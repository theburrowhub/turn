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

## Accepted successor boundary

Templates may contribute defaults to the unified CommandCatalogue creation filter and a foreground Session activation plan,
but they remain inert configuration rather than external or provider authority. ADR-064/065/066 acceptance must
prove:

- applying or selecting a Session created from a Template needs no separate start interaction only when the
  daemon has frozen and preflighted the exact target, AccountProfile, cwd, isolated checkout, command,
  effective model/flags and authority generation. Exactly one idempotent activation may restore/attach and
  start the exact bounded eligible saved descriptor set—or, for an empty Session, its one configured default
  Shell. Any drift or unresolved consequence starts
  nothing and produces one consolidated recovery action;
- a Template can name a WorkItemSource definition only by a non-secret profile reference. It cannot contain
  source credentials, item revisions, assignee authority or an instruction that closes/reopens an external
  item. Source writes still require current capabilities, compare-and-swap and external receipts;
- a provider-native Job definition imported or captured by a Template remains inert. Creation/invocation/job/
  iteration ids, active provider schedules, receipts, deletion fences and provider mutation authority are never
  copied. Adoption creates a new locally reviewed definition and separately reserves its Node/CreationId;
  activation is a consequence-labelled provider operation distinct from Turn Flow recurrence;
- ConversationInventory results, provider conversation ids, current ownership and resume authority are not
  captured as launchable Template state. A Template may hold a non-authoritative search/default descriptor;
  adoption and resume remain separate revision-fenced foreground operations;
- a WebPreview Resource Template stores only an inert bounded reference/snapshot. A Browser Template creates a new
  empty isolated partition and cannot copy cookies, history, reviewed localhost/local-HTML bindings,
  downloads, ambient credentials or automatic navigation;
- provider title-read and conversation-rename capabilities are not inferred from a saved display label, and
  AccountProfile usage/context/activity values are observations, never Template values or zero defaults;
- neither a Template nor a Flow step may store, mint or widen a remote permission-response grant. An exact
  single-use grant is issued separately on the foreground desktop for one live typed interaction; raw remote
  PTY input remains blocked while that known sensitive interaction is pending.
- a Template may preserve a recursive Group shape and an inert CheckoutScope request/default, but never a
  live Group binding, repository/worktree identity, creator provenance, dirty-state claim or remove/merge/
  publish authority. Application validates the whole acyclic tree and provisions a fresh target-bound scope;
  partial failure has receipts and cannot occupy primary `main`;
- WorkspaceOnboarding inputs stored in a Template are non-secret suggestions only. Applying a Template cannot
  open/clone/adopt SSH or publish a repository implicitly; each uses its own preassigned foreground operation,
  current target/path/repository/auth reference and consequence review;
- a Template may name `ModelEndpointProfileId` and model request but cannot copy an endpoint credential,
  discovered catalogue, health claim or effective receipt. Launch preflight revalidates the current route,
  target/profile/credential generation and fails without fallback. Quota samples from Kimi/MiniMax or any
  provider are observations and cannot become Template defaults;
- generated DisplayName proposals, captured source bytes/confidence and a manual pinned alias are not copied as
  generation authority. A Template may contain an explicit declared static label or an inert name-policy id;
  stale output cannot rename the new Node/Group or invoke provider rename; and
- NotificationEndpoint, DeliveryGrant, outbox/live state and device secret references are never Template or
  FlowDefinition content. A run may declare which canonical Attention events are eligible under an already
  active separately issued endpoint grant, but cannot pair, widen, persist or resolve through it.

Full remote GUI clients may apply the same revision-fenced Template operation as the local WorkSurface only
when the exact operation is present in both the invitation capability set and current registry/role policy.
Headless status clients and companions may inspect a
bounded projection but cannot apply Templates or inherit activation authority.

## ADR-067 Companion allowlisted launch

`ACP-CRE-011` does not let a Template launch from a Companion by itself. Packaged and headless fixtures prove:

- a local foreground grant may reference one immutable Template revision only after resolving its dedicated
  adapter, AccountProfile, optional fixed model, exact Workspace/Session/target and confined safe-cwd root;
- the grant copies no command/env/flags/path/credential/checkout authority from the Template. Its checkout
  policy is closed to read-only or a fresh isolated worktree, and primary `main` is impossible;
- the Companion selects one allowlist entry and preassigned Node/Instance/Attempt/Checkout identities; changed
  Template/profile/target/catalogue revision, revoke or≤24-hour expiry refuses before launch;
- duplicate/disconnect/crash at checkout, process and graph-registration boundaries returns one canonical
  receipt and at most one ordinary hierarchy Node. The Template and grant remain separate records, and grant
  revocation never edits the Template or kills an already registered agent.
