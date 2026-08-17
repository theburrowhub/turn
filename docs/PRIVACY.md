# Local data and privacy

Turn has no telemetry, analytics or product-usage reporting transport. It never sends terminal contents,
process output, environment values, credentials, local records or usage measurements **for telemetry**.
Every privacy report/export states `telemetry_enabled: false` and `telemetry_endpoints: 0`, so this is an
inspectable result rather than an assumption based on a missing settings screen.

That is not a claim that an agent supervisor is offline. User-launched provider CLIs can contact their
configured providers outside Turn. The implemented handoff explicitly submits reviewed content to another
local Agent. Accepted later features add operator-authorised ContextPacket/ContextLink delivery to a named
destination instance/provider/host and explicit foreground Web navigation to a validated origin. A custom
action can also perform whatever transfer its user-authored command performs. These are functional,
purpose-bound transfers, not telemetry: before authorisation the UI names destination, data categories,
scope/budget and known downstream retention, and Turn records bounded metadata/audit where specified. It
never silently reuses that authority for analytics.

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

### Accepted Agent Node data extension (not yet implemented)

ADR-059/M11–M14 add categories only after they enter the same closed catalogue:

- semantic/resource nodes and private Note records;
- `AgentInstance`, `RuntimeAttempt`, safe `LaunchSpec`/`LaunchReceipt`/`RuntimeConfigurationReceipt` and
  capability fingerprint/generation/status history—never bearer bytes;
- `ContextLink`, authority generation, ContextBroker read-audit metadata and lineage edges;
- `ContextPacket` id/hash/manifest/status/evidence metadata, never a dedicated semantic body copy;
- `ContextScope`/`QuotaScope` plus bounded last-known samples and provenance;
- `AgentMessage` hash/delivery evidence (not body), DependencyEdge/result evidence, Team membership/roles/
  policy and safe RuntimeEndpoint continuity fingerprints;
- remote-cleanup tombstones and authenticated purge evidence, never bearer/key bytes;
- File/Diff canonical references and private validated Web URL content, never the referenced file, branch or
  site payload.

Safe account references, provider conversation ids, host, cwd/worktree and link endpoints are sensitive
metadata. A Web URL is private content, including its path: reports expose only a sanitised origin and
exports redact the complete URL unless the operator explicitly selects that content category. Report/export
labels origin and scope, applies policy-aware redaction and never exports provider credentials, raw
environment values or unbounded transcripts. Packet/message drafts are
memory-only, but delivery—and every unreviewed-per-read ContextLink response authorised by a grant—can make
bytes durable downstream in a provider transcript, terminal screen/scrollback or ADR-052 journal; privacy
output must not claim those recipient copies are revocable.
Because packet/message bytes are not durable, daemon loss before proven submission records the applicable
lost/review-required state; Turn never reconstructs or re-sends content from its retained hash/manifest.

AgentInstance/Session/Workspace deletion first fences launches and revokes ContextLinks and issued broker
capabilities, then removes scoped attempts, samples, read-audit, lineage, packet/message/dependency/Team/
runtime-endpoint/resource metadata and Turn-owned Note content. It removes journals owned by the deleted
subtree, but cannot selectively erase packet/message bytes from an ancestor Shell-owned journal; the result
reports that retained Turn-owned category and directs the operator to delete the Shell/Session or disable
history. Provider transcripts are external and reported as outside Turn's deletion authority. An offline
remote host may likewise retain an owner-only capability/socket/key artifact: delete completes logical
revocation locally but reports `remote_residual`/`pending_purge`, retains a non-secret cleanup tombstone and
performs an authenticated exact-host/generation purge when the host reconnects. Only purge proof changes
that result to physically removed. Removing File/Diff/Web nodes never removes referenced user data or
performs a Web request. Ending/archive revokes links permanently.

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

The incompatible M11–M14 privacy protocol adds exact `node:<session-id>:<node-id>`,
`agent-instance:<id>` and `team:<id>` scopes. The old `agent:` form maps only after validating the one-to-one
Node/AgentInstance join; it never resolves from a display name, provider id or cwd.

## Delete

Selective deletion is authenticated through the live daemon:

```sh
turn --privacy-delete session:sess_ab12
turn --privacy-delete workspace:ws_ab12 --kill
turn --privacy-delete agent:sess_ab12:proc_ab12
```

The default disposition is a polite termination; `--kill` requests a hard stop. Keeping processes while
deleting their identity is refused. Session and Workspace deletion removes SQLite records, Settings owners,
scratch configuration, journals, checkpoints, previews and bindings. Agent deletion removes its subtree,
Attention/Event references, previews, bindings, scratch and history owned by that subtree. A parent Shell's
journal is a different owner and is retained; the response reports that retained category as well as records/
files and bytes removed, compaction, and any process identity that escaped Turn's control.

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

The accepted M11–M15 schemas must add the relevant exact controls below before their migrations ship; they
are not current v0.1 settings:

| Target key | Default | Scope / behavior |
| --- | ---: | --- |
| `records.runtime_attempts_per_agent` | 100 | Global; current plus at most 99 ended detail records; older attempts fold into one constant-size aggregate receipt, never one digest each |
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
| `records.coordination_edges_per_session` | 1,000 | Global; active Dependency/Team records require explicit delete when full |
| `records.dependency_result_summary_kib` | 4 KiB | Global; optional control-stripped/redacted text in the closed result schema |
| `records.local_speech_models_mib` | 8,192 MiB | Global; M15 refuses install at the cap and never silently deletes the selected model |

Live Node/Agent/Team/dependency/lineage/scope/endpoint records remain owner-lifetime data. Compaction applies
the historical limits above, uses only one constant-size aggregate for pruned attempts and refuses new live
records or remote artifacts at a declared bound; it never silently ages out active semantics or cleanup proof.

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
When M11–M14 ship, the same target must additionally prove new-category inventory, sensitive-metadata
redaction, revoke-before-delete races, bounded compaction and resource-reference deletion without touching
user files or loading Web content.
When M15 ships, it must additionally seed recognisable PCM/transcript markers and prove they are absent from
protocol captures, SQLite/WAL, filesystem, logs, events, Attention, journals, diagnostics, exports and crash
artifacts before explicit delivery. It covers model/receipt/partial inventory, metadata-only export, signed-
catalogue bounds, symlink-safe delete/compact and the disclosed downstream retention after Insert/Send.
