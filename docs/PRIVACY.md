# Local data and privacy

Turn has no telemetry, analytics or product-usage reporting transport. It never sends terminal contents,
process output, environment values, credentials, local records or usage measurements **for telemetry**.
Every privacy report/export states `telemetry_enabled: false` and `telemetry_endpoints: 0`, so this is an
inspectable result rather than an assumption based on a missing settings screen.

That is not a claim that an agent supervisor is offline. User-launched provider CLIs can contact their
configured providers outside Turn. The implemented handoff explicitly submits reviewed content to another
local Agent. Accepted later features add operator-authorised ContextPacket/ContextLink delivery to a named
destination instance/provider/host, inert Web preview retrieval and explicit foreground Browser navigation
to a reviewed origin or local HTML file. A custom
action can also perform whatever transfer its user-authored command performs. These are functional,
purpose-bound transfers, not telemetry: before authorisation the UI names destination, data categories,
scope/budget and known downstream retention, and Turn records bounded metadata/audit where specified. It
never silently reuses that authority for analytics.

An immutable FlowRun may satisfy that authorisation once, up front, for its exact destinations, categories,
scope, budget and retention disclosure. A conductor cannot expand it; out-of-grant transfer returns to a
foreground review. This reduces repeated prompts without turning agent output into consent.

Accepted M15 local dictation keeps PCM and inference on the physical operator device. Its explicit model
download is still a functional network transfer: the UI names artifact origin, model id, size and licence,
and the origin can observe the requester's network address. No audio or transcript accompanies that request.

Here “operator-authorised” describes Turn's authenticated supported flow. Without a per-agent OS sandbox or
UI-held authority inaccessible to child processes, an unsandboxed malicious same-uid agent can steal the
administrative capability and impersonate a foreground UI; privacy and security acceptance must expose that
limit rather than claim hostile-process consent isolation.

## What Turn stores

The authoritative inventory has two closed catalogues:

- SQLite rows: Workspaces, Sessions, Layouts, Process/Agent nodes, Events, Attention, Activity Previews,
  Pane bindings, Settings layers, Templates, tree presentation state, checkout identities/leases/fences and
  workspace audit events. A schema migration adding a table without adding it to the inventory makes export
  fail rather than silently omit the table.
- Private files: SQLite and its WAL/SHM/journal sidecars, the owner-only daemon log, injected Agent scratch
  configuration, bounded terminal journals/checkpoints and runtime control files. Any future file under the
  Turn data directory is reported as `unclassified_local_file` until it receives a semantic category.

Daemon-managed Git worktree roots are listed with their aggregate size, but their payload is never exported
or deleted. They contain user work. Likewise, file payloads which may contain terminal output, log text or
injected configuration are represented by metadata only. Their `content` explains the omission.

Every exported datum contains `origin`, `data_type`, `timestamp_ms` (explicitly `null` when the backing
format has no timestamp), `bytes` and redacted `content`. Known credentials are scrubbed again at the export
boundary. Unknown or secret Settings values are replaced by `<redacted>`.

### Accepted control-plane data extension (not yet implemented)

ADR-059/061/062/063/064/065 and M11–M17 add categories only after they enter the same closed catalogue:

- semantic/resource nodes and private Note records;
- `AgentInstance`, `RuntimeAttempt`, safe `LaunchSpec`/`LaunchReceipt`/`RuntimeConfigurationReceipt` and
  capability fingerprint/generation/status history—never bearer bytes;
- `ContextLink`, authority generation, ContextBroker read-audit metadata and lineage edges;
- `ContextPacket` id/hash/manifest/status/evidence metadata, never a dedicated semantic body copy;
- `ContextScope`/`QuotaScope` plus bounded last-known samples and provenance;
- `AgentMessage` hash/delivery evidence (not body), DependencyEdge/result evidence, Team membership/roles/
  policy and safe RuntimeEndpoint continuity fingerprints;
- immutable FlowDefinition revisions, FlowRun inputs/state, DelegationGrant metadata, safe operation/effect
  receipts and recurrence observations—never bearer values or secret prompt/environment expansions;
- provider topology observations and integration diagnostics after raw native payload removal/redaction;
- multi-client revision/journal/tombstone records, input-lease history and scoped share invitation/session
  metadata—never invitation secrets, encrypted content keys or presence/typing history;
- remote-cleanup tombstones and authenticated purge evidence, never bearer/key bytes;
- File/Diff canonical references and private validated Web/Browser navigation identity, never the referenced
  file, branch, rendered document or site payload.

ADR-063 adds eight explicitly classified families. None may hide inside a generic JSON/blob column:

| Family | Durable Turn-owned data | Content deliberately not copied into the catalogue |
| --- | --- | --- |
| WorkItem/board | Node id/revision, closed state, priority, due date, bounded tags/assignees/comments and conflict/audit metadata | runtime state, dependency state and Attention authority; a board remains a projection |
| Delegated Resource/Progress | typed Resource body, author/schema/revision, bounded progress samples, grant/operation ids and receipts | referenced file/site payload, terminal bytes and any control instruction inferred from content |
| Shared RuntimeEndpoint | endpoint/binding ids, safe continuity fingerprint, instance/conversation/account/profile references, generation and recovery state | provider credentials, raw auth/config roots, transcript/input/context bodies and provider-native payloads |
| RuntimeInventory/ResourceInventory | target/host/generation, observation coverage, safe handle/process-start fingerprint, bounded host memory/swap/pressure and process RSS/attribution, reconciliation decision and proof | raw process environment, argv/command-line secrets, open-file contents and unmatched runtime transcript |
| FileBackend edit | canonical target/root/revision/encoding/size metadata, conflict/save receipt and redacted audit | file body, edit buffer, merge buffer, diff body and adopted external repository data |
| Note-backed live brief | private Note body and immutable body revisions required by an active/pinned disclosure, link revision policy and read audit | any additional provider-side copy produced after delivery |
| AccountProfile | safe provider/profile id, ownership kind, redacted root fingerprints, validation/default/retirement state and active-reference count | credentials, tokens, cookies, raw auth/config files, transcripts and provider conversation bodies |
| Remote/headless operator | client/session/scope/revision, authentication-method class, lease/route/audit metadata and revocation proof | bearer/session/key bytes, terminal/input/context bodies duplicated for audit, presence and typing history |

ADR-064 adds eight more independently classified families. They do not inherit a broader category merely
because their UI appears in the same tree or WorkSurface:

| Family | Durable Turn-owned data | Content deliberately not copied into the catalogue |
| --- | --- | --- |
| Foreground Session activation | selected Session/Node ids, preflight generation, reserved attempt, safe effective launch receipt and accepted/refused/uncertain outcome | terminal input/output, secret environment expansion, credentials and any second copy of provider configuration |
| External WorkItemSource | source id/version, credential-reference id, external item identity, field-authority map, filter hash, cursor/watermark, coverage/freshness, mapping version and bounded sync/conflict/write receipts | credential bytes, unselected remote fields, remote attachments, provider response bodies and source-wide issue history |
| Remote typed permission response | client/grant/interaction/attempt/revision ids, selected closed option, accepted/refused/uncertain receipt, anti-replay/audit metadata and redacted permission kind/risk | bearer/session/key bytes, credential/password answers, raw PTY bytes, unbounded prompt/tool payload and encrypted frame plaintext duplicated for audit |
| Provider-native job | profile/target/job identity, bounded schedule/time-zone/enabled state, safe iteration identity/state/timestamps, freshness and exact control receipts | job prompt/output/transcript, provider credentials and unrelated provider scheduler data |
| ConversationInventory | provider/profile/target namespace, stable conversation identity, bounded safe title observation, state/timestamps, page/coverage/freshness and match/adopt/resume receipts | message/transcript bodies, provider search response bodies, credentials and conversations outside the requested profile/target scope |
| Web preview / Browser | private reviewed origin or canonical local-file identity, content hash/revision where local, isolation policy generation, bounded navigation receipt and blocked-popup/redirect reason | rendered/page body, DOM, script/storage/cookie state, form/input values, downloads, ambient browser history and unreviewed sibling local files |
| Provider title read/rename | bounded untrusted provider title, source revision/freshness and requested/effective rename receipt, separately from the local display alias | conversation/transcript body and provider response payload beyond the bounded title |
| Companion profile inbox | AccountProfile/metric scope, bounded context/quota samples, units/windows/reset/freshness, safe activity identity/summary/read state and sync cursor | provider credentials, transcript/prompt bodies, raw provider event payloads and any sibling profile's data |

ADR-065 adds eight final classified families. A Group projection never changes ownership of CheckoutScope,
and a pushed notification never becomes a second copy of the underlying private body:

| Family | Durable Turn-owned data | Content deliberately not copied into the catalogue |
| --- | --- | --- |
| Recursive Group / CheckoutScope | Group parent/order/tree revision, separate scope/repository/worktree/target identity, creator provenance, lifecycle and bounded operation receipts | repository files/diffs, branch contents, credentials, runtime output and worktree payload |
| WorkspaceOnboarding | operation/Workspace/target/path/repository identity, phase, local consent and bounded partial/reconcile/publish receipt metadata | repository objects/files, SSH credential/passphrase, raw clone output and external hosting response body |
| Dedicated adapter / quota connector | roster/capability/version evidence ids, AccountProfile/target-scoped bounded samples, coverage/freshness/error and redacted live-evidence receipts | credentials, raw billing/provider responses, transcript/activity bodies and sibling-profile data |
| ModelEndpointProfile | safe route/origin/protocol/model metadata, target/profile/credential-reference kind+generation, health/freshness and redacted validation/launch receipts | API key/token/secret values, raw discovery response, request/response body and target environment value |
| ResourceInventory | latest bounded host capacity/pressure and process identity/RSS/owner/coverage snapshots plus exact intervention receipt | argv, environment, open files, transcript, command/output bodies and unrelated host process detail |
| DisplayNameFact / proposal | bounded sanitised label, source kind/confidence/revision, pin mode and proposal generator/hash/expiry metadata | raw terminal/transcript/task capture, secret/control-stripped source and provider response body |
| Notification delivery | endpoint/grant/delivery/live ids, scope/privacy/rate/expiry/generation, minimal encrypted-payload hash, state and redacted failure/revocation receipt | device bearer/private key, plaintext prompt/transcript/command/path/account body, provider payload and downstream device cache |
| Specification capability ledger | opaque source-snapshot/evidence digests, stable feature/disposition/rationale and PRD/ACP/ADR trace | source repository content, credentials, user records and runtime/provider payloads |

A WorkItemSource remains an external data owner. Import copies only mapped fields selected by the configured
filter into the explicitly private WorkItem category; paging and reconciliation do not grant Turn a right to
export unrelated source fields. The cache stores bounded mapped records and typed coverage, not raw API
responses. Its credential is a keychain/broker reference and never enters SQLite, filters, cursors or
diagnostics. Removing a source deletes Turn's cache/configuration only after imported items are explicitly
kept detached or deleted; it never closes or deletes external items. A Turn close/reopen is an external
functional mutation only when its separate revision-fenced receipt says so, and privacy deletion never
silently performs it.

Provider-native jobs and conversations are likewise provider-owned. Turn's dismiss/read-state is local
metadata and is not provider deletion; provider cancel/disable/delete/rename/resume are explicit functional
transfers whose target and consequence are named before dispatch. Deleting Turn metadata cannot erase a
provider transcript, conversation or job. `ConversationInventory` search text and raw result page are
memory-only; only the bounded mapped metadata rows above enter the cache, and transcript content still needs
the independent transcript/content authority. Title read and provider rename never change the classification
of titles as untrusted sensitive metadata.

Web preview and Browser do not share storage. Web is inert and may retain only the validated private URL plus
a bounded fetch receipt; Browser uses a per-node ephemeral isolated partition with no ambient browser
profile. DOM, script state, cookies, local/session storage, form values, response bodies, page cache,
downloads and navigation history are memory-only and destroyed on node close, scope loss or process exit.
A reviewed local HTML file remains user-owned: Turn records canonical descriptor identity and content hash,
does not copy the file or adjacent resources, and node deletion never touches it. A future opt-in persistent
browser profile would be a new catalogue category and cannot ship under this contract.

A remote/Companion permission response intentionally transmits one closed option over the authenticated
encrypted channel. Encryption is a transport property, not permission to persist plaintext. Server and
client retain only the bounded receipt/audit fields above; prompt detail and frame bodies are memory-only and
cleared on resolution, expiry, disconnect or revocation. Credential/password prompts, host trust and grant
administration remain local-only and have no remotely serialisable answer schema.

Companion usage, context and activity are always keyed by AccountProfile before storage or transmission.
An unavailable, expired or rate-limited sample is stored as that typed state, never numeric zero. Activity
summary is bounded/redacted before caching and does not import provider message bodies. Signing out or losing
scope clears that client's memory cache; server retention follows the exact per-profile controls below and
cannot be queried through a sibling profile.

An adopted AccountProfile auth/config root remains user/provider-owned external data: Turn may validate and
use it only through the declared broker/sandbox and never exports, compacts or deletes that root. A root that
Turn creates is installation-owned private configuration; deleting the profile removes it only after all
launches, endpoint bindings and issued capabilities are fenced and only if its canonical owner/mode/root
receipt still matches. Shared endpoint processes and provider stores keep their own auth/transcript data;
Turn owns only the bounded binding metadata above. Changing the default profile never changes this ownership
or migrates already frozen LaunchReceipts.

Remote trust configuration and client allow-lists are installation-owned owner-only Settings. A Turn-issued
private key or refresh secret lives only in the platform credential store (or a separately inventoried
owner-only secret file when no credential store exists); SQLite stores its non-secret id/fingerprint and
revocation state. External identity-provider accounts/keys remain provider-owned. Access/session bearer
values and replay challenge bytes are memory-only, expire at the protocol deadline and are never included in
report/export/log/audit. Client deletion revokes the fingerprint before metadata removal. Full remote and
headless operation intentionally transmits the selected terminal/view/context and operator input over the
authenticated encrypted connection; the connection screen names server, client, scope and downstream cache
policy. The reduced companion receives only its negotiated projection. All client body caches are bounded,
memory-only and cleared on scope loss, sign-out or process exit.

Safe account references, provider conversation/job/external-item ids, host, cwd/worktree and link endpoints
are sensitive metadata. A Web or Browser URL is private content, including its path: reports expose only a
sanitised origin and exports redact the complete URL unless the operator explicitly selects that content
category. A reviewed local-HTML path is redacted under the same rule. Report/export
labels origin and scope, applies policy-aware redaction and never exports provider credentials, raw
environment values or unbounded transcripts. Packet/message drafts are
memory-only, but delivery—and every unreviewed-per-read ContextLink response authorised by a grant—can make
bytes durable downstream in a provider transcript, terminal screen/scrollback or ADR-052 journal; privacy
output must not claim those recipient copies are revocable.
Because packet/message bytes are not durable, daemon loss before proven submission records the applicable
lost/review-required state; Turn never reconstructs or re-sends content from its retained hash/manifest.

A ModelEndpointProfile's origin, route/model catalogue and credential-reference metadata are sensitive. Raw
discovery pages are memory-only and bounded; only validated mapped model ids/labels and coverage/freshness may
be cached. A secret value is written directly to the target's keystore/agent/broker and has no readable Turn
field. Environment references retain only variable name, target and availability. Report/export redact private
origins and show credential-reference kind/generation, never value. Deleting a route removes Turn metadata and
Turn-created secret material after reference checks; it does not delete an externally owned environment,
broker account or provider model.

Notification pairing records a public endpoint identity and secret reference, not the device token/private
key. Plaintext payload exists only in bounded memory before endpoint encryption and contains no transcript,
prompt/answer body, command, path, account or raw provider payload. Outbox persistence, when required for
offline retry, retains only encrypted bytes plus minimal route hash/state and expires at the delivery boundary.
Revocation deletes queued encrypted bytes for that endpoint generation and retains a bounded non-secret audit
receipt. Device OS/cloud notification history is downstream external retention disclosed at pairing and cannot
be erased by Turn; Turn never claims that gateway acceptance proves device deletion or reading.

Private Note bodies, WorkItem comments and delegated Resource bodies are exceptions to the metadata-only
default because they are explicit Turn-owned content. Reports show only their item/byte counts and content
hashes. Export includes their bodies only when the authenticated foreground operator names respectively
`note_content`, `work_item_content` or `resource_content`; selecting one never selects another. Remote and
companion exports cannot add a content category that their negotiated policy omitted. File snapshots,
conflict buffers, RuntimeInventory payloads, progress replacement drafts and AccountProfile auth/config
bytes are memory-only and are absent even from a content-selected export.

AgentInstance/Session/Workspace deletion first fences launches and revokes ContextLinks, delegation/share
grants and issued broker capabilities, then removes scoped attempts, samples, read-audit, lineage,
packet/message/dependency/Team/FlowRun/runtime-endpoint/client-tombstone/resource metadata and Turn-owned Note
content. It removes journals owned by the deleted
subtree, but cannot selectively erase packet/message bytes from an ancestor Shell-owned journal; the result
reports that retained Turn-owned category and directs the operator to delete the Shell/Session or disable
history. Provider transcripts are external and reported as outside Turn's deletion authority. An offline
remote host may likewise retain an owner-only capability/socket/key artifact: delete completes logical
revocation locally but reports `remote_residual`/`pending_purge`, retains a non-secret cleanup tombstone and
performs an authenticated exact-host/generation purge when the host reconnects. Only purge proof changes
that result to physically removed. Removing File/Diff/Web/Browser nodes never removes referenced user data,
site data or provider data and performs no navigation; it clears only Turn's bounded metadata and ephemeral
Browser partition. Ending/archive revokes links permanently.

Live semantic records are not aged out: active Notes/Teams/dependencies, instances, current/bounded recent
attempt detail, lineage, scopes and endpoints remain until their explicit owner deletion, under the exact
size/count limits below. Older ended attempts fold into one constant-size aggregate receipt per AgentInstance,
not one digest per pruned attempt. Link authority ends at its required expiry/revoke, and live links are
refused at either endpoint's declared bound. Only expired metadata, audits, packet/message delivery records
and usage history use time retention. Cleanup tombstones remain until
verified purge; reaching their bound disables creation of new remote artifacts rather than dropping proof.
`privacy-compact` applies those rules but never deletes an active Note because it is old.
Any migration that adds one of these tables/files before report/export/delete/compact catalogue and tests
know it must fail acceptance.

A Note revision disclosed by a live reviewed-link or pinned by any retained ContextLink/FlowRun is live
content, not history. It is retained byte-for-byte until the last such reference is revoked/deleted and the
corresponding read audit reaches its own retention boundary. An edit that would exceed a Note count/byte
bound while all candidate revisions are still required is refused before changing the current revision; it
does not discard a pin, reset a link budget or silently sever a consumer. Once unreferenced, old revisions
are compacted by the exact count/time/byte rule below.

### Accepted local voice-input data extension (not yet implemented)

ADR-060/M15 adds only these durable categories:

- `local_speech_model`: installation-owned model file; report id/digest/size/origin/licence/engine metadata,
  export metadata only and never duplicate the weights;
- `local_speech_model_receipt`: signed catalogue version, verified digest/size, installed time and compatible
  worker/engine versions;
- `local_speech_model_partial`: owner-only bounded download temporary, removed on cancel/error and swept
  without following symlinks during startup/`privacy-compact`;
- `input.dictation.model`, `input.dictation.language`, `input.dictation.max_seconds` and
  `keyboard.bindings["input.dictate"]` Settings.

PCM, partial hypotheses, final inline drafts, waveforms, transcript hashes and microphone device ids have no
durable category because Turn forbids persisting them. Privacy report/export explicitly asserts zero such
records instead of omitting the question. The daemon protocol never receives them. After explicit Insert/
Send, the text is ordinary target input and may be retained in the provider transcript, terminal screen/
scrollback, shell history or ADR-052 journal; local audio does not imply local delivered text. Explicit
**Copy** places the draft in the OS clipboard, outside Turn's later deletion authority.

## Inspect and export without opening a window

These commands authenticate to the already-running daemon and never start a companion or GUI:

```sh
turn --privacy-report
turn --privacy-report session:sess_ab12
turn --privacy-export /absolute/path/turn-export.json --scope workspace:ws_ab12
turn --privacy-compact
```

Scopes are `installation`, `workspace:<id>`, `session:<id>` and
`agent:<session-id>:<node-id>`. Report output includes per-category and total item/byte counts plus the policy
currently in force. Export uses owner-only, create-new file semantics: it never follows an existing
destination symlink and never overwrites a file.

The incompatible M11–M17/ADR-064 privacy protocol adds exact `node:<session-id>:<node-id>`,
`agent-instance:<id>`, `team:<id>`, `flow-run:<id>`, `note:<id>`, `work-item:<id>`,
`work-item-source:<id>`, `resource:<id>`, `native-job:<id>`, `account-profile:<id>`,
`runtime-target:<target-id>:<generation>` and `remote-client:<id>` scopes. ConversationInventory, provider
title and Companion metric/activity records are children of their exact `account-profile` scope; Browser/Web
records are children of their canonical `node` scope. A child scope includes only records canonically owned
by that subject; references
owned elsewhere are emitted as redacted links and named as retained external scopes. `runtime-target` export
contains only Turn's bounded inventory metadata and reconciliation proof, never a fresh target query or raw
process payload. `account-profile` contains safe metadata and validation receipts, never the auth/config root.
The old `agent:` form maps only after validating the one-to-one Node/AgentInstance join; it never resolves
from a display name, provider id, conversation id or cwd.

## Delete

Selective deletion is authenticated through the live daemon:

```sh
turn --privacy-delete session:sess_ab12
turn --privacy-delete workspace:ws_ab12 --kill
turn --privacy-delete agent:sess_ab12:proc_ab12
# Accepted M11–M17/ADR-064 forms:
turn --privacy-delete note:note_ab12
turn --privacy-delete work-item:item_ab12
turn --privacy-delete work-item-source:source_ab12
turn --privacy-delete resource:res_ab12
turn --privacy-delete native-job:job_ab12
turn --privacy-delete flow-run:run_ab12
turn --privacy-delete account-profile:profile_ab12
turn --privacy-delete remote-client:client_ab12
```

The default disposition is a polite termination; `--kill` requests a hard stop. Keeping processes while
deleting their identity is refused. Session and Workspace deletion removes SQLite records, Settings owners,
scratch configuration, journals, checkpoints, previews and bindings. Agent deletion removes its subtree,
Attention/Event references, previews, bindings, scratch and history owned by that subtree. A parent Shell's
journal is a different owner and is retained; the response reports that retained category as well as records/
files and bytes removed, compaction, and any process identity that escaped Turn's control.

Accepted scoped deletion has the same revoke/fence-first rule. Note deletion revokes every link and removes
all Turn-owned body revisions after in-flight reads finish; it cannot erase already delivered downstream
copies. WorkItem deletion removes its metadata/comments but no Node, runtime, dependency or Attention.
Resource deletion removes only the Turn-owned semantic record/body, never a referenced file/site or control
effect. FlowRun deletion is refused while active, then removes its run-owned inputs, resources, progress and
receipts while retaining separately owned FlowDefinition revisions required elsewhere. AccountProfile
deletion is refused while a LaunchReceipt, live binding or issued capability refers to it; after fencing it
removes safe metadata and only a still-matching Turn-created private root, never an adopted root or provider
transcript. Remote-client deletion revokes its sessions/leases/capabilities and removes its audit at normal
retention; it never destroys the Workspace. Runtime-target metadata has no standalone delete operation:
target forget requires foreground confirmation, fences inventory generation and cannot terminate a runtime.

WorkItemSource deletion revokes its credential reference and sync generation, then removes filter/cursor/cache
and sync receipts; imported WorkItems require the explicit detach-or-delete choice and no provider item is
mutated. NativeJob privacy deletion removes only Turn's projection, iteration history and local dismiss state;
provider cancel/disable/delete is a different typed operation and is never implied. AccountProfile deletion
also removes its ConversationInventory/title/usage/context/activity cache after fencing live references, but
cannot delete provider conversations, titles, activity, quota history or jobs. Browser/Web node deletion
clears the ephemeral Browser partition and bounded receipts without touching a local HTML file or origin.

Installation deletion is offline because a process must not unlink its own open database:

```sh
turnd --delete-installation-data
# Add --data-dir and --socket when using non-default locations.
```

The command acquires the same canonical data-directory lock as normal daemon startup. It exits with code 3
without deleting anything if a daemon is live. Once exclusive, it removes SQLite and every sidecar, logs,
scratch, terminal history, tokens, stale sockets and unclassified future entries without following symlinks.
It deliberately retains the stable `.turnd.lock` inode and `worktrees/` user checkout roots.

M15 installation deletion also removes verified model files, receipts and partials without following
symlinks. Removing one model is an authenticated foreground operation; if it is active, Turn first cancels
the worker job and retires its descriptor, then removes only the catalogue-matched file/partials. Session,
Node and Agent deletion do not remove installation-owned models and have no PCM/draft record to find.

## Retention controls

The Settings hierarchy exposes these controls under **Records**:

| Key | Default | Scope |
| --- | ---: | --- |
| `records.event_retention_days` | 30 days | Global |
| `records.event_limit` | 50,000 | Global |
| `records.event_session_floor` | 50 | Global |
| `records.preview_retention_days` | 30 days | Global |
| `records.previews_per_agent` | 20 | Global |
| `records.preview_limit` | 2,000 | Global |
| `records.terminal_history` | on | Global, Workspace, Template, Session |
| `records.terminal_journal_mib` | 8 MiB per Pane | Global |
| `records.terminal_checkpoint_mib` | 4 MiB per Pane | Global |
| `records.daemon_log_mib` | 4 MiB | Global |

The accepted M11–M17/ADR-064 schemas must add the relevant exact controls below before their migrations ship; they
are not current v0.1 settings:

| Target key | Default | Scope / behavior |
| --- | ---: | --- |
| `records.runtime_attempts_per_agent` | 100 | Global; current plus at most 99 ended detail records; older attempts fold into one constant-size aggregate receipt, never one digest each |
| `records.session_activation_receipts_per_session` | 100 | Global; current plus newest safe preflight/outcome receipts; referenced uncertain/current attempt evidence is preserved |
| `records.session_activation_receipt_days` | 30 days | Global; unreferenced terminal activation receipts older than the boundary compact even below count |
| `records.lineage_edges_per_agent` | 256 | Global; refuses a new live edge at the bound and cascades on AgentInstance deletion |
| `records.context_scopes_per_agent` | 32 | Global; active scopes are owner-lifetime and refuse creation at the bound |
| `records.quota_scopes_per_account` | 32 | Global; bounded per safe provider/account owner and cascades when that owner is removed |
| `records.runtime_endpoints_per_agent` | 16 | Global; bounded configured continuity endpoints per AgentInstance |
| `records.remote_cleanup_tombstones` | 1,000 | Global; never pruned before authenticated purge proof; at capacity no new remote artifact is created |
| `records.active_context_links_per_agent` | 64 | Global; each live link counts at both endpoints and creation is refused at either bound |
| `records.expired_context_link_days` | 30 days | Global; active authority is governed by required expiry/revoke, not this history limit |
| `records.expired_context_link_limit` | 10,000 | Global |
| `records.context_read_audit_days` | 30 days | Global |
| `records.context_read_audit_limit` | 50,000 | Global |
| `records.context_packet_metadata_days` | 30 days | Global |
| `records.context_packet_metadata_limit` | 10,000 | Global |
| `records.agent_message_metadata_days` | 30 days | Global |
| `records.agent_message_metadata_limit` | 10,000 | Global |
| `records.usage_sample_days` | 30 days | Global |
| `records.usage_samples_per_scope` | 2,880 | Global; newest bounded observations per Context/Quota scope |
| `records.note_max_kib` | 256 KiB | Global or Workspace override; one active Note |
| `records.notes_per_workspace_mib` | 16 MiB | Global or Workspace override; active Notes require explicit delete when full |
| `records.note_revisions_per_note` | 50 | Global; maximum unreferenced historical revisions in addition to current/required revisions |
| `records.note_revision_days` | 180 days | Global; an unreferenced historical revision is removed when older than this bound even if fewer than 50 remain |
| `records.note_revision_mib_per_workspace` | 64 MiB | Global; includes current, pinned, live-disclosed and historical bodies; an edit is refused if required revisions leave no room |
| `records.coordination_edges_per_session` | 1,000 | Global; active Dependency/Team records require explicit delete when full |
| `records.dependency_result_summary_kib` | 4 KiB | Global; optional control-stripped/redacted text in the closed result schema |
| `records.flow_definitions_per_workspace` | 1,000 | Global; active logical definitions, with revision retention below |
| `records.flow_definition_revisions` | 50 | Global; maximum unreferenced historical revisions per definition in addition to current/referenced revisions |
| `records.flow_definition_revision_days` | 180 days | Global; an unreferenced revision is removed when older than this bound even if fewer than 50 remain |
| `records.flow_definition_revision_mib_per_workspace` | 64 MiB | Global; referenced revisions count; a new revision is refused rather than evicting required evidence |
| `records.flow_runs_per_session` | 1,000 | Global; active runs and current effect receipts are never time-pruned |
| `records.flow_operation_receipts_per_run` | 10,000 | Global; refuses further expansion at the bound and raises exact Attention |
| `records.topology_observations_per_attempt` | 10,000 | Global; folds old observations into bounded final child/coverage receipts without losing active identities |
| `records.work_items_per_workspace` | 10,000 | Global; current canonical WorkItems; creation is refused at the bound |
| `records.work_item_revisions_per_item` | 100 | Global; current plus at most 99 historical metadata revisions; oldest unreferenced revision compacts first |
| `records.work_item_revision_days` | 180 days | Global; an unreferenced historical metadata revision is removed when older |
| `records.work_item_tags_per_item` | 32 | Global; unique validated tags; the mutation is refused at the bound |
| `records.work_item_assignees_per_item` | 16 | Global; exact identity references; the mutation is refused at the bound |
| `records.work_item_comments_per_item` | 500 | Global; creation is refused at the bound; no comment is silently replaced |
| `records.work_item_comment_kib` | 16 KiB | Global; one UTF-8 comment after control stripping |
| `records.work_item_content_mib_per_workspace` | 64 MiB | Global; comments/tags/assignees/current and retained revisions; mutation is refused at the cap |
| `records.work_item_sources_per_workspace` | 64 | Global; source metadata/credential references only; adding another source is refused |
| `records.work_item_source_receipts` | 10,000 | Global; newest bounded sync/write/conflict receipts installation-wide |
| `records.work_item_source_receipt_days` | 30 days | Global; receipts older than the boundary compact after current cursor/conflict references are preserved |
| `records.delegated_resources_per_flow_run` | 1,000 | Global; live typed Resources owned by one FlowRun; further creation raises exact Attention |
| `records.delegated_resource_max_kib` | 256 KiB | Global; one Turn-owned Resource body after schema validation |
| `records.delegated_resource_mib_per_workspace` | 64 MiB | Global; mutation is refused rather than pruning an active Resource |
| `records.delegated_progress_per_operation` | 100 | Global; latest plus at most 99 replaced progress records; records older than 7 days compact first |
| `records.delegated_progress_days` | 7 days | Global; a replaced progress record is removed when older even if fewer than 100 remain |
| `records.delegated_progress_max_kib` | 4 KiB | Global; one progress record including safe provenance, never terminal or file content |
| `records.runtime_bindings_per_endpoint` | 64 | Global; independently owned active/recoverable bindings on one shared endpoint; creation is refused at the bound |
| `records.runtime_inventory_handles_per_target` | 10,000 | Global; known plus unmatched handles in one target generation; overflow marks the snapshot gapped |
| `records.runtime_inventory_snapshot_mib` | 16 MiB | Global; maximum redacted snapshot per target; overflow is a gap, never silent truncation/exactness |
| `records.runtime_inventory_observation_days` | 7 days | Global; only the latest complete/partial/gapped snapshot per live generation is owner-lifetime |
| `records.resource_inventory_processes_per_target` | 10,000 | Global; reuse-safe process rows in the same target snapshot; overflow marks coverage gapped and never drops into exact accounting |
| `records.runtime_reconciliation_receipts` | 10,000 | Global; newest safe adopt/ignore/terminate proofs installation-wide |
| `records.runtime_reconciliation_receipt_days` | 180 days | Global; a receipt is removed when older even if fewer than 10,000 remain |
| `records.native_jobs_per_profile` | 10,000 | Global; current provider-job projections; overflow marks inventory gapped rather than dropping a job silently |
| `records.native_job_iterations_per_job` | 100 | Global; current plus newest 99 safe metadata-only iteration records; active/uncertain iteration is never compacted |
| `records.native_job_iteration_days` | 180 days | Global; ended unreferenced iteration metadata older than the boundary compacts even below count |
| `records.conversation_inventory_entries_per_profile` | 10,000 | Global; metadata-only bounded cache; overflow marks coverage gapped and refuses authoritative search/zero |
| `records.conversation_inventory_cache_minutes` | 15 minutes | Global; expiry changes the cache to stale and never deletes provider data or fabricates an empty result |
| `records.provider_title_observations_per_conversation` | 10 | Global; bounded untrusted requested/effective title observations and rename receipts |
| `records.name_facts_per_node` | 10 | Global; newest bounded sanitised source facts/proposal metadata; manual pinned alias remains owner-lifetime |
| `records.name_proposal_days` | 7 days | Global; unapplied proposal metadata expires; raw captured source is never durable |
| `records.model_endpoint_profiles_per_target` | 32 | Global; safe route metadata only; creation is refused at the bound |
| `records.model_discovery_entries_per_profile` | 10,000 | Global; mapped metadata cache; overflow marks discovery partial, raw page remains memory-only |
| `records.model_discovery_cache_minutes` | 15 minutes | Global; expiry becomes stale and never proves absence or changes a running attempt |
| `records.model_endpoint_receipts_per_profile` | 100 | Global; newest redacted validation/launch/switch receipts; active/uncertain evidence is retained |
| `records.workspace_onboarding_receipts` | 10,000 | Global; newest phase/partial/reconcile/publish metadata, never repository/SSH/clone output bodies |
| `records.workspace_onboarding_receipt_days` | 180 days | Global; terminal unreferenced receipts compact at the boundary while uncertain recovery evidence remains |
| `records.web_browser_navigation_receipts_per_node` | 100 | Global; safe origin/file identity, policy generation and outcome only; DOM/history/form/cookie bodies remain memory-only |
| `records.web_browser_navigation_receipt_days` | 30 days | Global; unreferenced terminal receipts older than the boundary compact even below count |
| `records.open_file_snapshots_per_client` | 16 | Global; memory-only open/edit/merge buffers; the seventeenth open is refused |
| `records.file_snapshot_mib` | 8 MiB | Global; one memory-only decoded snapshot; larger files require an external tool |
| `records.file_save_audit_limit` | 50,000 | Global; newest redacted save/conflict receipts installation-wide |
| `records.file_save_audit_days` | 180 days | Global; receipts are compacted when older even if below the count bound |
| `records.account_profiles_per_provider_host` | 32 | Global; safe profile metadata; creation/adoption is refused at the bound |
| `records.account_validation_receipts_per_profile` | 100 | Global; newest safe receipts |
| `records.account_validation_receipt_days` | 30 days | Global; a validation receipt is removed when older even if fewer than 100 remain |
| `records.account_profile_private_root_mib` | 64 MiB | Global; one Turn-created owner-only auth/config root; writes are refused at the bound |
| `records.remote_operator_sessions` | 64 | Global; simultaneous authenticated full/headless sessions; new sessions are refused at the bound |
| `records.remote_operator_audit_limit` | 50,000 | Global; newest redacted remote scope/action/revocation records installation-wide |
| `records.remote_operator_audit_days` | 180 days | Global; records are compacted when older even if below the count bound |
| `records.remote_replay_nonces` | 10,000 | Global; hashed nonce/id metadata only, expires after 10 minutes; overflow refuses a new remote mutation |
| `records.remote_permission_receipts` | 10,000 | Global; newest redacted typed-option receipts, never prompt/frame/credential bodies |
| `records.remote_permission_receipt_days` | 180 days | Global; resolved receipts compact at the boundary while active/uncertain reconciliation evidence remains |
| `records.notification_endpoints` | 64 | Global; active/retired safe endpoint and grant metadata; new pairing refuses at the bound |
| `records.notification_encrypted_outbox_mib` | 16 MiB | Global; encrypted minimal payloads only; overflow expires lowest-priority eligible delivery without changing Attention and records a gap |
| `records.notification_delivery_hours` | 24 hours | Global; pending encrypted delivery expires; accepted/failed/revoked metadata compacts after seven days |
| `records.notification_delivery_audit_days` | 7 days | Global; non-secret state/endpoint-generation/hash metadata only |
| `records.companion_activity_items_per_profile` | 1,000 | Global; newest bounded safe activity metadata per AccountProfile; overflow requires a gap/resync marker |
| `records.companion_activity_days` | 30 days | Global; handled/unhandled provider activity metadata older than the boundary compacts without changing Attention |
| `records.sync_journal_days` | 30 days | Global; earlier cursors must resnapshot and cannot replay mutations |
| `records.sync_journal_mib_per_workspace` | 256 MiB | Global; compaction publishes a new minimum accepted revision before removing segments |
| `records.client_tombstone_days` | 30 days | Global; compacted deletion is still fenced by minimum revision, non-reused ids and update-never-upserts rules |
| `records.status_diagnostic_days` | 7 days | Global; bounded redacted operational/integration history |
| `records.status_diagnostic_per_workspace` | 1,000 | Global; progress replacement/coalescing occurs before insertion |
| `records.input_lease_history_days` | 7 days | Global; metadata only, never draft or input bytes |
| `records.share_invitation_days` | 30 days | Global; invitation secrets/keys and ephemeral presence are never stored |
| `records.share_audit_days` | 180 days | Global; redacted scope/action/receipt metadata only |
| `records.local_speech_models_mib` | 8,192 MiB | Global; M15 refuses install at the cap and never silently deletes the selected model |

Browser DOM/storage/history and ConversationInventory search text/raw pages intentionally have no retention
setting because their durable count is zero. A migration that persists them fails the closed-catalogue gate
rather than inheriting a permissive default.

Live Node/Agent/Team/FlowRun/dependency/lineage/scope/endpoint/source/current-job records remain owner-lifetime data. Compaction applies
the historical limits above, uses only one constant-size aggregate for pruned attempts and refuses new live
records or remote artifacts at a declared bound; it never silently ages out active semantics or cleanup proof.

For every paired count/time rule, compaction removes only unreferenced history and keeps exactly the newest
records that satisfy **both** limits: no more than the count and none older than the duration. Byte limits are
checked after that compaction. Current, active, referenced, pinned, purge-proof and still-auditable Note
revisions are never candidates; if those alone consume the byte cap, the proposed write fails atomically.
All sizes are measured over canonical stored bytes including row/blob framing but excluding SQLite page/WAL
overhead, which remains visible in the installation total.

Installed local speech models likewise live until explicit model or installation deletion, not a time-to-
live. Their total is bounded by `records.local_speech_models_mib`; each stable model id has at most one active
verified artifact plus one generation-fenced upgrade partial, and an engine/catalogue upgrade removes an
obsolete artifact only after no worker descriptor references it. Partials have the catalogue byte cap, are
removed immediately on cancel/failure and are swept on startup/compaction; an unclassified model-directory
entry makes inventory fail rather than being adopted.

Event/Preview retention and log bounds are enforced immediately after a related setting write and at least
once per minute while the daemon runs. Turning terminal history off stops journalling and removes existing
history without stopping the live Session; turning it back on applies to newly launched Panes. Journal and
checkpoint size changes apply when a Pane next starts. `privacy-compact` applies retention immediately,
truncates the WAL, vacuums SQLite and reports before/after storage.

## Reproducible acceptance

```sh
make privacy-acceptance
```

The target covers the closed SQLite catalogue, redacted export, credential-free log/export output,
create-new/symlink safety, scoped deletion, dynamic retention/history controls, authenticated protocol/CLI
shapes, offline physical purge, preservation of checkout work and refusal while a daemon owns the lock.
When M11–M17 ship, the same target must additionally prove new-category inventory, sensitive-metadata
redaction, revoke-before-delete races, bounded compaction and resource-reference deletion without touching
user files or loading Web content. Boundary-clock/count tests cover Flow revisions, sync journal/minimum
revision, status/diagnostics, input leases and share invitation/audit retention; a client older than 30 days
must resnapshot and can never resurrect a deleted id.
The ADR-063 fixture uses a different recognisable canary for a WorkItem comment, delegated Resource body,
progress provenance, each of five shared-endpoint binding conversations, unmatched RuntimeInventory handle,
file body/edit/conflict buffer, current/pinned/live Note revisions, both AccountProfile roots/credentials and
remote input/session secret. It proves category-selected Note/WorkItem/Resource export includes only its
chosen body; all other canaries are absent from report, default/content-selected export, logs, diagnostics,
projection snapshots, Attention, sync journals, crash artifacts and remote/companion caches. Metadata
canaries appear only redacted in their declared owner scope. Count/time/byte boundary tests exercise exactly
limit minus one, limit and limit plus one; required Note pins survive compaction, overflow refuses atomically,
and scoped delete reports every retained external/downstream owner without deleting a sibling, adopted root,
file, runtime, provider transcript or Workspace.
The ADR-064 fixture adds distinct canaries for a WorkItemSource credential/raw response/unselected field,
permission prompt/frame/credential answer, native-job output, conversation body/search page, Web/Browser DOM,
cookie/form value/local sibling file, provider title payload and both selected and sibling-profile Companion
events. It proves those bytes are absent from report, every export category, SQLite/WAL, sync journal,
diagnostics, Attention, crash artifacts and server/client caches after the declared memory lifetime. It also
proves mapped source fields, safe job/conversation/title metadata, numeric usage with unit/window/freshness and
typed permission receipts appear only in their exact source/profile/node/client scope; unavailable usage is
never serialised as zero. Source/profile/node privacy deletion leaves external items, jobs, conversations,
titles, local HTML and sites unchanged.
The ADR-065 fixture adds independent canaries for worktree/repository content, clone/SSH output, each of six
adapter transcripts and two quota-provider raw pages, model-gateway secret/discovery body, process argv/env,
automatic-name raw capture and notification token/plaintext. It proves only bounded sanitised roster,
CheckoutScope, onboarding, quota, route/model, resource-aggregate, name-fact and encrypted-delivery metadata
appear in their exact target/profile/node/endpoint scope. No canary appears in report/export, SQLite/WAL,
sync/status/Attention logs, diagnostics, process argv, PTY, crash artifacts or another profile/target/endpoint.
Boundary tests distinguish unmeasured resource data from measured zero, remove a notification generation's
queued ciphertext on revoke, preserve externally owned credentials/repositories/worktrees/device history, and
delete/compact every new Turn-owned category under the numerical controls above.
When M15 ships, it must additionally seed recognisable PCM/transcript markers and prove they are absent from
protocol captures, SQLite/WAL, filesystem, logs, events, Attention, journals, diagnostics, exports and crash
artifacts before explicit delivery. It covers model/receipt/partial inventory, metadata-only export, signed-
catalogue bounds, symlink-safe delete/compact and the disclosed downstream retention after Insert/Send.
