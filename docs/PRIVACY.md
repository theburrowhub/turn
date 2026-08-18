# Local data and privacy

Turn has no telemetry, analytics or product-usage reporting transport. It never sends terminal contents,
process output, environment values, credentials, local records or usage measurements **for telemetry**.
Every privacy report/export states `telemetry_enabled: false` and `telemetry_endpoints: 0`, so this is an
inspectable result rather than an assumption based on a missing settings screen.
This includes anonymous install counts, crash/product analytics, stable or rotating installation identifiers,
always-on counters and a telemetry consent toggle: none has a schema row, endpoint, request, worker, queue or
retained record. `DO_NOT_TRACK` cannot be the only way to obtain zero telemetry because the path does not exist.
Purpose-bound update discovery carries only the signed-update channel/platform/version fields declared in
`PROTOCOL.md`, never an installation/client identifier, and a test fails if it can be repurposed as an event
collector.

That is not a claim that an agent supervisor is offline. User-launched provider CLIs can contact their
configured providers outside Turn. The implemented handoff explicitly submits reviewed content to another
local Agent. Accepted later features add operator-authorised ContextPacket/ContextLink delivery to a named
destination instance/provider/host, inert WebPreview retrieval and explicit foreground Browser navigation
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
  Turn data directory is reported inside `operational_store(unclassified_quarantine)` until it receives its
  semantic category; that is a charged substate, not another disk class.

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
- File/Diff canonical references and private validated WebPreview/Browser navigation identity, never the referenced
  file, branch, rendered document or site payload.

ADR-063 adds eight explicitly classified families. None may hide inside a generic JSON/blob column:

| Family | Durable Turn-owned data | Content deliberately not copied into the catalogue |
| --- | --- | --- |
| WorkItem/board | Node id/revision, closed state, priority, due date, bounded tags/assignees/comments and conflict/audit metadata | runtime state, dependency state and Attention authority; a board remains a projection |
| Delegated Resource/Progress | typed Resource body, author/schema/revision, bounded progress samples, grant/operation ids and receipts | referenced file/site payload, terminal bytes and any control instruction inferred from content |
| Shared RuntimeEndpoint | endpoint/binding ids, safe continuity fingerprint, instance/conversation, closed profiled-or-endpoint-unscoped account-scope reference, generation and recovery state | provider credentials, raw auth/config roots, transcript/input/context bodies, provider-native payloads and any AccountProfile/quota/activity inference from an unscoped binding |
| RuntimeInventory/ResourceInventory | target/host/generation, observation coverage, safe handle/process-start fingerprint, bounded host memory/swap/pressure and process RSS/attribution, reconciliation decision and proof | raw process environment, argv/command-line secrets, open-file contents and unmatched runtime transcript |
| FileBackend edit | canonical target/root/revision/encoding/size metadata, conflict/save receipt and redacted audit | file body, edit buffer, merge buffer, diff body and adopted external repository data |
| Note-backed live brief | private Note body and immutable body revisions required by an active/pinned disclosure, link revision policy and read audit | any additional provider-side copy produced after delivery |
| AccountProfile | safe provider/profile id, ownership kind, redacted root fingerprints, validation/default/retirement state and active-reference count | credentials, tokens, cookies, raw auth/config files, transcripts and provider conversation bodies |
| Remote/headless operator | invitation/client/session/redemption ids, scope/revisions/expiry, authentication-method class, role/manifest, lease/route/action-receipt/audit metadata and revocation proof | invitation secret, bearer/session/private-key bytes, terminal/input/context bodies duplicated for audit, presence and typing history |

ADR-064 adds eight more independently classified families. They do not inherit a broader category merely
because their UI appears in the same tree or WorkSurface:

| Family | Durable Turn-owned data | Content deliberately not copied into the catalogue |
| --- | --- | --- |
| Foreground Session activation | selected Session/Node ids, preflight generation, reserved attempt, safe effective launch receipt and accepted/refused/uncertain outcome | terminal input/output, secret environment expansion, credentials and any second copy of provider configuration |
| External WorkItemSource | source id/version, credential-reference id, external item identity, field-authority map, filter hash, cursor/watermark, coverage/freshness, mapping version and bounded sync/conflict/write receipts | credential bytes, unselected remote fields, remote attachments, provider response bodies and source-wide issue history |
| Typed permission response | local/remote/verified-PTY operation kind, ClaimId and unique interaction fence, exact route/owner/binding/client/session/grant/interaction/capability/transport revisions, selected closed option, sibling-grant invalidation, grant terminal state, dispatch/evidence receipt axes, anti-replay/audit metadata and redacted permission kind/risk | bearer/session/key bytes, credential/password answers, raw PTY or encoded fallback bytes, unbounded prompt/tool payload, offline permission drafts and encrypted frame plaintext duplicated for audit |
| Provider-native job | profile/target/incarnation-aware NativeJobKey or pre-key CreationId, scan/coverage, create/mutation/invocation correlation and minimal replay fences, requested versus effective configuration, exact Job-scoped private definition up to 64 KiB, schedule/presence/projection reducers, iteration key/state/timestamps and safe result/error metadata up to 32 KiB, receipts/tombstones/privacy-suppressed flag | definition/result bytes beyond bounds, job output/transcript, provider credentials, unrelated scheduler data and opaque provider payloads |
| ConversationInventory | provider/profile/target namespace, stable conversation identity, bounded safe title observation, state/timestamps, page/coverage/freshness and match/adopt/resume receipts | message/transcript bodies, provider search response bodies, credentials and conversations outside the requested profile/target scope |
| WebPreview / Browser | private validated URL/origin or canonical local-file identity, content hash/revision where local, isolation policy generation, bounded load/navigation receipt, blocked-popup/redirect reason, bounded quarantine state/size/type/hash metadata and opt-in Memory Saver state containing only exact policy/address revisions plus `history_lost` | response/rendered/page body, headers, DOM, script/storage/cookie state, form/input/POST values, quarantine/download bytes, ambient browser history and unreviewed sibling local files |
| Provider title read/rename | bounded untrusted provider title, source revision/freshness and requested/effective rename receipt, separately from the local display alias | conversation/transcript body and provider response payload beyond the bounded title |
| Companion profile inbox | AccountProfile/metric scope, bounded context/quota samples, units/windows/reset/freshness, safe activity identity/summary/read state and sync cursor | provider credentials, transcript/prompt bodies, raw provider event payloads and any sibling profile's data |

ADR-066 adds eight independently classified utility and presentation families. Their records are not a licence
to persist source bodies, credentials, signing secrets or sandbox inputs:

| Family | Durable Turn-owned data | Content deliberately not copied into the catalogue |
| --- | --- | --- |
| Media import | MediaImportId/state, source descriptor identity/hash/MIME/size receipt, destination Node/blob reference, owner-only imported blob and bounded terminal codec/error evidence | per-surface playback state, elapsed/duration/volume/caption selection and decoder frames/caches are memory-only; source sibling files, filesystem path beyond confined display policy, network/script/control data are excluded |
| Repository host profile | safe profile/host/target/account/scope/state, external credential-reference id and independent backend/source grant metadata | tokens, cookies, SSH private keys, credential values, provider issue/repository bodies |
| Commit proposal | repository/staged-index hash, omission/redaction manifest, CommitProposalProviderProfile/Attempt ids and revisions, executable-or-broker descriptor hash, sandbox-policy revision, numeric limit set, bounded generated draft and terminal sandbox/result receipt | environment values, inherited descriptors, unstaged files, secret matches, binary/raw diff beyond the sealed memory-only 128 KiB generation input, provider prompt/response body |
| Transfer ticket | direction, confined descriptors, target/root generations, size/hash/chunk bitmap, expiry/state/receipt and owner-only temporary bytes while active | unrelated directory contents, remote credentials, file body after completed handoff or terminal cleanup |
| Content projection/search/explorer | no durable projection, search cursor/range, Directory/Commit page/watch or rendered-cache payload; only bounded safe operation/gap audit when required | all projection/search/page/watch state, rendered DOM, terminal input, Note/file body duplicated beyond its owning record and directory contents outside requested page/root |
| Catalogue/announcement/update | catalogue entries/bindings; exact domain/schema/payload hash/signer id+epoch/issued/expiry/audience/sequence/optional parent-manifest hash/signature identity, accepted trust-store revision, revocation/high-water receipt, per-operator dismissal, safe inert text/link ids and updater state/receipt | private signing keys, credentials, arbitrary command/output text, unsigned bodies, staged package bytes in records/logs/exports and web content behind announcement links |
| WorkItem activity | WorkItemActivityEvent identity/order, closed kind+typed redacted delta, observed/effective time provenance, safe actor provenance and operation/source receipt | comment-body duplication, credential, unbounded/source payload, runtime transcript or duplicate provider event body |
| Presentation history | non-PII installation-minted LocalOperatorIdentityId or remote client/session owner, surface, whitelisted operation, Workspace history/object generations, pre/post/inverse and terminal receipt | human identity/profile data, domain/provider/runtime/input/Attention/credential payloads, source content and excluded operation bodies |
| Surface navigation state | daemon-minted non-reused SurfaceId, non-PII local/remote owner, state/connection revision, bounded selected/expanded/manual-order/filter/visibility/anchor fields and dormant deadline | transient search query, content body, temporary Pane, input, subscription, playback, editor/projection bytes and another owner's state |

The exhaustive source audit adds the following independently classified families. Their transient bodies do
not inherit the retention of a nearby Browser, terminal or diagnostic surface:

| Family | Durable Turn-owned data | Content deliberately not retained |
| --- | --- | --- |
| Document view/print | exact source descriptor revision/hash and body-free reviewed print intent/receipt/correlation | source blob, decoded tiles, text index, object URL and print spool are memory-only and released at quiescence; no embedded file/form/script/link body |
| Terminal clipboard gesture | paste/drop `RuntimeInputReceipt` source kind+digest and body-free outcome only | copied/pasted text, path manifest and OS clipboard contents; OSC 52 reads/writes are swallowed before access |
| Attention audio cue | no durable cue; settings retain only enabled/mute/volume/cooldown | task/result/prompt text, audio capture, generated audio and playback history |
| Bulk restart and Eco lifecycle | exact instance/attempt/policy/eligibility revisions, operation ids and bounded per-instance outcomes; Eco opt-in policy | terminal bodies, provider transcript, command text and any new copy of scrollback/session data |
| Off-screen terminal parking | no durable Park or wake-input body; only ordinary attachment/gap/status receipts survive | renderer/xterm cache, held input≤4,096 bytes and park/wake timers are local memory-only and never create a transcript, scrollback or runtime copy |
| Runtime domain attachment | Workspace-owned RuntimeAttachmentReceipt with operation/fingerprint, exact owner/attempt/binding/target/handle generations, endpoint-correlation digest and closed attached/recovered/refused/uncertain result | PTY/output/input/transcript, provider payload, raw process command/environment, endpoint proof body and any second runtime copy |
| Runtime view replay | Installation-owned RuntimeViewReplayFence with operation/fingerprint, Surface owner/sequence, PaneAttachment/attempt/PTY/buffer generations and attach/resync/detach outcome | terminal cells/bytes, baseline/image/cache, input, scrollback and any lifecycle/provider authority |
| Container close/delete result | Installation-owned ContainerCloseReceipt with exact container/tombstone identity, operation/fingerprint, daemon-derived revision-vector/disposition-root digests and fixed semantic/runtime/cleanup survivor counts+roots; each existing recovery row stores the receipt/serial ordinal/leaf needed to verify its typed root | per-survivor lists, semantic subject bodies, terminal output, provider/user data and survivor details already owned/paged by their recovery inventories |
| Automatic detached-session reaping | none; the capability is rejected and has no timer, setting, queue, intent or receipt | no process/session/runtime is killed or deleted from age, count, memory pressure, invisibility or attachment state |
| Agent-controlled Browser | reviewed Workspace grant, exact agent/attempt/Browser ownership, typed action hashes and bounded receipts | page/DOM/accessibility response, form value, typed body, cookie/storage, credential and screenshot |
| Companion agent launch | reviewed allowlist grant, preassigned Node/instance/attempt/checkout ids and canonical launch/registration receipts | arbitrary command/env/flags/path, credential bytes, mobile draft or hidden mirror copy |
| Corrupt store recovery | owner-only exact quarantined original bytes, identity/hash/failure metadata and reviewed recovery receipt until explicit disposition | no diagnostic/telemetry upload or parsed field extraction beyond the safe omission/recovery report |
| Cross-client Workspace convergence | canonical typed StateStream mutations/receipts already owned by Installation/Workspace; exact per-surface presentation state and conflict metadata | no watched/internal-store copy, external JSON merge buffer, duplicate runtime/session record, another client's draft or self-echo history |
| Safe control visibility | only the≤11 closed optional-control ids in the existing resolved settings record | no click history, hidden-action body, Attention/recovery/destructive-control state or separate UI registry |
| PTY capacity and remediation | one target-scoped current used/ceiling/headroom/coverage/freshness observation plus exact privileged-provider before/after/config/rollback intent and receipt metadata | process/terminal contents, administrator credential, shell/argv, arbitrary config bytes, unrelated device entries and another target's capacity |
| Private transcript body search | opt-in exact profile/target/namespace index generation, encrypted postings plus bounded title/cwd/source locator/snippet metadata and final≤200-KiB normalised user/assistant segment tail per indexed document, coverage/freshness and body-free policy/rebuild/delete receipts | encryption key, raw query/provider page, transcript bytes beyond that bounded encrypted index, another profile/target, credentials and provider transcript mutation/deletion |
| Anonymous product telemetry | none | every install-count, event, usage, device/client identifier and analytics payload; the family is rejected rather than silently omitted |

ADR-065 adds nine obligations but only eight final classified families: automatic tree arrangement persists
no coordinate, geometry or cleanup history and introduces no data family. A Group projection never changes
ownership of CheckoutScope, and a pushed notification never becomes a second copy of the underlying private body:

| Family | Durable Turn-owned data | Content deliberately not copied into the catalogue |
| --- | --- | --- |
| Recursive Group / CheckoutScope | Group parent/order/tree revision, separate scope/repository/worktree/target identity, creator provenance, lifecycle and bounded operation receipts | repository files/diffs, branch contents, credentials, runtime output and worktree payload |
| WorkspaceOnboarding / RepositoryPublishIntent | onboarding operation/Workspace/target/path/repository identity and phase; separately, publish operation/destination/visibility/object/ref/config/correlation identity and bounded partial/reconcile receipt metadata | repository objects/files, SSH credential/passphrase, raw clone/push output, credential value and external hosting response body |
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
metadata and is not provider deletion; provider job pause/resume/run-now/cancel-current-iteration/delete-job
and conversation rename/resume are explicit functional transfers whose target and consequence are named
before dispatch. Deleting Turn metadata cannot erase a
provider transcript, conversation or job. `ConversationInventory` predicate/search text and raw metadata
result page are memory-only; only the bounded mapped metadata rows above enter that cache. The separately
enabled `PrivateTranscriptSearchIndex` may durably retain only its encrypted exact-scope body-derived index:
postings, bounded metadata/snippet and the final≤200-KiB normalised segment tail/document needed for the
read-only historical view. All components share the same≤512-MiB profile/target and≤1-GiB installation caps,
key, redaction and revoke-before-unlink lifecycle; no tail is a second cache. Its query/view buffer and page
remain request-only and its
existence grants no transcript mutation, provider deletion, context or runtime authority. Title read and provider rename never change the classification
of titles as untrusted sensitive metadata. A rename intent stores only bounded requested/effective title,
hashes, exact subject/generations and provider correlation—not transcript/response bodies. Provider uncertainty
never rewrites a local/pinned alias, and deleting Turn rename metadata never renames or deletes the provider
conversation. Scoped profile/conversation deletion refuses nonterminal/possible-effect rename evidence.

WebPreview and Browser do not share storage. WebPreview is inert and may retain only the validated private URL plus
a bounded fetch receipt; Browser uses a per-node ephemeral isolated partition with no ambient browser
profile. DOM, script state, cookies, local/session storage, form values, ordinary response bodies, page cache
and navigation history are memory-only and destroyed on node close, scope loss or process exit. Download
bodies bypass the renderer into the explicitly bounded owner-only non-executable BrowserDownloadQuarantine;
metadata is durable for recovery, bytes occupy the shared transfer-temporary pool and are deleted at terminal
cleanup or atomically transferred to a reviewed TransferTicket without a second copy. A reviewed local HTML
file remains user-owned: Turn reads one descriptor-verified memory-only snapshot≤8 MiB, stores no persistent
copy or adjacent resource and destroys the bytes on navigation/close/loss; node deletion never touches the
source file. A future opt-in persistent
browser profile would be a new catalogue category and cannot ship under this contract.

A remote/Companion permission response intentionally transmits one closed option over the authenticated
encrypted channel. Encryption is a transport property, not permission to persist plaintext. Grant delivery
contains only capability plus bounded redacted kind/consequence/option metadata for the exact grantee; prompt
and frame bodies are memory-only. No permission draft persists offline. Disconnect, client/session/profile/
Session/Node/attempt deletion, interaction replacement, capability downgrade, expiry or revocation invalidates
the grant and clears client plaintext; terminal anti-replay/receipt metadata follows the limits below.
Credential/password prompts, host trust and grant administration remain local-only.

Companion usage, context and activity are always keyed by AccountProfile before storage or transmission.
An unavailable, expired or rate-limited sample is stored as that typed state, never numeric zero. Activity
summary is bounded/redacted before caching and does not import provider message bodies. Signing out or losing
scope clears that client's memory cache; server retention follows the exact per-profile controls below and
cannot be queried through a sibling profile.

An adopted AccountProfile auth/config root remains user/provider-owned external data: Turn may validate and
use it only through the declared broker/sandbox and never exports, compacts or deletes that root. A root that
Turn creates is installation-owned private configuration; deleting the profile removes it only after all
launches, authentication intents, endpoint bindings and issued capabilities are terminal/fenced and only if its canonical owner/mode/root
receipt still matches. Shared endpoint processes and provider stores keep their own auth/transcript data;
Turn owns only the bounded binding metadata above. Changing the default profile never changes this ownership
or migrates already frozen LaunchReceipts.

Before any external auth helper/broker/Browser launch, a Turn-created root reserves its remaining per-root and
installation byte quota plus intent/terminal/recovery slots. Auth flows that cannot confine writes and enforce
that quota are unsupported. Authentication receipts contain correlation and generations but no callback/
credential bytes. Retire/revoke fences late callbacks; selective profile deletion refuses nonterminal or
possible-effect auth and never deletes a provider account or adopted root.

Remote trust configuration and client allow-lists are installation-owned owner-only Settings. A Turn-issued
private key or refresh secret lives only in the platform credential store (or a separately inventoried
owner-only secret file when no credential store exists); SQLite stores its non-secret id/fingerprint and
revocation state. External identity-provider accounts/keys remain provider-owned. Access/session bearer
values and replay challenge bytes are memory-only, expire at the protocol deadline and are never included in
report/export/log/audit. Client deletion revokes the fingerprint before metadata removal. Full remote GUI
operation intentionally transmits the selected terminal/view/context and operator input over the authenticated
encrypted connection. Headless status transmits only its authorised reads, subscriptions, cursors and
surface-local navigation; it has no input or domain-mutation channel. The connection screen names server,
client, scope and downstream cache policy. The reduced companion receives only its negotiated projection. All
client body caches are bounded, memory-only and cleared on scope loss, sign-out or process exit.

Safe account references, provider conversation/job/external-item ids, host, cwd/worktree and link endpoints
are sensitive metadata. A WebPreview or Browser URL is private content, including its path: reports expose only a
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
Endpoint retire revokes every local grant generation, outbox ciphertext and secret reference while retaining
bounded non-secret correlation/high-water evidence; delete is refused until pairing/grant/delivery uncertainty
is terminal. Neither operation contacts or deletes a provider/device account. A selective endpoint export or
delete names one exact non-reused endpoint generation and never exposes or mutates a sibling endpoint/scope.

Private Note bodies, WorkItem comments and delegated Resource bodies are exceptions to the metadata-only
default because they are explicit Turn-owned content. Reports show only their item/byte counts and content
hashes. Export includes their bodies only when the authenticated foreground operator names respectively
`note_content`, `work_item_content` or `resource_content`; selecting one never selects another. Remote and
companion exports cannot add a content category that their negotiated policy omitted. File snapshots,
conflict buffers, RuntimeInventory payloads, progress replacement drafts and AccountProfile auth/config
bytes are memory-only and are absent even from a content-selected export.

AgentInstance/Session/Workspace deletion first fences launches and revokes ContextLinks, delegation/share
grants and issued broker capabilities. It then removes only destroyed-subject attempts, samples, read-audit,
lineage, packet/message/dependency/Team/FlowRun/runtime-endpoint/client-tombstone/resource metadata and Turn-
owned Note content; a live/uncertain semantic child, NativeJob, MediaImport or Attention subject is atomically
rehomed when a valid declared destination exists and otherwise retained with its identity/evidence/route in
the owning WorkspaceSemanticRecoveryInventory; deleting that Workspace atomically migrates those entries and
new semantic survivors to InstallationSemanticRecoveryInventory. Missing rehome input never blocks End or Turn-container deletion. It removes journals owned by the deleted
subtree, but cannot selectively erase packet/message bytes from an ancestor Shell-owned journal; the result
reports that retained Turn-owned category and directs the operator to delete the Shell/Session or disable
history. Provider transcripts are external and reported as outside Turn's deletion authority. An offline
remote host may likewise retain an owner-only capability/socket/key artifact: delete completes logical
revocation locally but reports `remote_residual`/`pending_purge`, retains a non-secret cleanup tombstone and
performs an authenticated exact-host/generation purge when the host reconnects. Only purge proof changes
that result to physically removed. Removing File/Diff/WebPreview/Browser nodes never removes referenced user data,
site data or provider data and performs no navigation; it clears only Turn's bounded metadata and ephemeral
Browser partition. End/delete revokes links permanently. Direct hide-only Archive retains only the bounded
suspended link record, revokes its bearer and permits no disclosure until explicit restore revalidation.

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
- `local_speech_model_receipt`: exact `voice_model_manifest` envelope identity (payload hash, signer id+epoch,
  audience, sequence, accepted trust-store revision and revocation/high-water receipt), verified artifact
  digest/size, installed time and compatible worker/engine versions;
- `SigningTrustStore(voice_model_manifest)`: safe public-root/key ids+epochs, revision, revocations and
  per-audience high-water only; private signing keys are never Turn data and never appear in report/export;
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

The incompatible ADR-063/064 privacy protocol adds exact `node:<session-id>:<node-id>`,
`agent-instance:<id>`, `team:<id>`, `flow-run:<id>`, `note:<id>`, `work-item:<id>`,
`work-item-source:<id>`, `resource:<id>`, `native-job:<session-id>:<job-node-id>`, `account-profile:<id>`,
`runtime-target:<target-id>:<generation>` and `remote-client:<id>` scopes. ConversationInventory, provider
title and Companion metric/activity records are children of their exact `account-profile` scope; Browser/WebPreview
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
# Accepted ADR-063/064 forms:
turn --privacy-delete note:note_ab12
turn --privacy-delete work-item:item_ab12
turn --privacy-delete work-item-source:source_ab12
turn --privacy-delete resource:res_ab12
turn --privacy-delete native-job:sess_ab12:node_job_ab12
turn --privacy-delete flow-run:run_ab12
turn --privacy-delete account-profile:profile_ab12
turn --privacy-delete remote-client:client_ab12
```

The default process disposition is polite termination; `--kill` requests a hard stop. Identity/evidence for a
live semantic survivor is never silently erased and never becomes a user prerequisite: the daemon derives a
total rehome/tombstone/Workspace-or-Installation SemanticRecoveryInventory disposition in the serial commit,
while an OS process that survives a committed stop remains in the distinct ExecutionTarget-owned
TargetRuntimeRecoveryInventory with a durable survivor receipt. Session and Workspace deletion atomically applies those semantic
dispositions and removes their active navigation/owned records, Settings owners, scratch configuration,
journals, checkpoints, previews and bindings; later process cleanup cannot veto or resurrect the container.
Agent deletion tombstones the target and removes only terminal target-owned data; independently live children
and their Attention/Event/history are rehomed under their exact identities before parent removal. A parent
Shell's journal is a different owner and is retained; the response reports every retained/rehomed category,
records/files and bytes removed, compaction, and any process identity that escaped Turn's control.

Accepted scoped deletion has the same revoke/fence-first rule. Note deletion revokes every link and removes
all Turn-owned body revisions after in-flight reads finish; it cannot erase already delivered downstream
copies. WorkItem deletion removes its active canonical Node row and projection metadata/comments, retains only
the minimum WorkItemId/NodeId/WorkItemKey tombstone fence, and mutates no runtime or dependency. It never
creates, resolves or drops Attention: any exact existing Attention route or other independently owned live
reference is atomically rerouted to the tombstone/provisional view before deletion rather than left dangling.
Resource deletion removes only the Turn-owned semantic record/body, never a referenced file/site or control
effect. FlowRun deletion is refused while active, then removes its run-owned inputs, resources, progress and
receipts while retaining separately owned FlowDefinition revisions required elsewhere. AccountProfile
deletion is refused while a LaunchReceipt, live binding or issued capability refers to it; after fencing it
removes safe metadata and only a still-matching Turn-created private root, never an adopted root or provider
transcript. Remote-client deletion revokes its sessions/leases/capabilities and removes its audit at normal
retention; it never destroys the Workspace. Runtime-target metadata has no standalone delete operation:
target forget requires foreground confirmation, fences inventory generation and cannot terminate a runtime.

WorkItemSource deletion revokes its credential reference and source/sync generations, then requires exact
`detach_bindings|delete_local_items`; active create/mutation/conflict reconciliation blocks destructive
evidence removal. It removes filter/cursor/cache only after late-event fences persist, and never mutates a
provider item. Reversible `forget_native_job_projection` changes only visibility and retains definition, history,
correlation and receipts so Restore can recover the same truthful View. `delete_native_job_local_data` is the
separate destructive local privacy operation and is refused while CreateIntent, any MutationIntent, permission/
Attention or replay evidence is nonterminal/reconcile-required. Once terminal, it removes private definition,
iteration/result history and local activity while retaining only NodeId plus tagged NativeJobKey-or-CreationId
visibility/deletion fence, every provider tombstone, and minimal installation-lifetime operation id+canonical
fingerprint+subject+terminal-result replay fences. Thus a failed create with no NativeJobKey remains fenced and
identical/changed replay after privacy deletion or normal compaction returns the compacted result/conflict with
zero provider request. `privacy_suppressed` prevents automatic list/get from recaching erased definition or
history; only explicit Restore may admit fresh provider observations. Neither operation emits a provider
request; pause/resume/run-now/cancel-current-iteration/delete-job remains separately typed.
AccountProfile deletion
also fences private transcript-search readers, revokes that profile/index key, schedules confined encrypted-index
unlink, and removes its ConversationInventory/title/usage/context/activity cache after fencing live references, but
cannot delete provider conversations, titles, activity, quota history or jobs. Browser/WebPreview node deletion
clears the ephemeral Browser partition and bounded receipts without touching a local HTML file or origin.
Media deletion decrements only Turn's blob reference and erases owner-only bytes at the last reference; an
active import/playback is cancelled first and no original dropped file is removed. RepositoryHostProfile
deletion first fences terminal every authenticate/rotate intent and late callback, revokes both independent
grant kinds, removes safe metadata and a Turn-owned credential reference only,
never the external secret/provider account/repository/issue. Transfer terminal cleanup removes temp bytes;
completed destination files require their normal File deletion authority. Commit proposals, projections,
search, announcement dismissal, update discard and presentation-history compaction remove only their declared
local records and cannot mutate source, repository, provider, terminal, Attention or installed version.

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

Every Turn-owned file belongs to exactly one Installation `PhysicalDiskLedger` class: `operational_store`,
`state_sync_journal`, `terminal_history`, `file_save_temporary`, `portable_temporary`,
`account_private_root`, `speech_model`, `media_pool`, `transfer_or_quarantine` or `update_package`. The latter
nine are never hidden in the operational figure. The ledger reports, per class and total, reserved logical
bytes, allocated physical bytes and reclaim-pending bytes. Its charged value is the greater of outstanding
worst-case reservation and filesystem-allocated bytes including sidecars/metadata; a refcounted physical
extent is charged exactly once to its current owner. Copy, COW/reflink, compression and sparse files do not
make a byte disappear from the report.

Private transcript-search index files are charged to `account_private_root`, including encryption framing,
temporary generation and reclaim-pending extents. Their≤1-GiB installation subcap is inside that class's
existing2-GiB cap and the135-GiB total. Generation swap reserves old+new physical allocation until the old
descriptor is quiescent and its key revoked; compression, sparse postings or key deletion do not release the
physical charge until block absence is proved.

Before creating or extending any file, the sole writer reserves both the exact family pool and 135-GiB total,
including the storage backend's declared allocation-unit/metadata overhead. If that overhead cannot be bounded
or current free/quota space is smaller, the operation refuses or pauses before the next chunk. Rename, seal,
refcount and ownership transfer move one charge atomically; a real copy reserves source+destination, and cleanup
releases only after descriptor/block absence is proved. Effectful operations pre-reserve their terminal and
cleanup metadata, so a full payload class cannot veto End/delete. A boot scan reconciles ledger rows against
every owner-only Turn root before writes; unknown entries enter the
`operational_store(unclassified_quarantine)` substate, consume both the 8-GiB operational cap and the total,
raise system Attention and permit no new write until classified or removed. This substate is not an eleventh
class.

User-owned checkouts/repositories, provider-owned caches and final explicitly selected external export/save
destinations are outside Turn-owned roots and outside the 135-GiB total, but are reported in a separate
`external_user_or_provider` line with owner/path class and never used as scratch. No other exclusion exists.

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

The accepted target schemas introduced by ADR-059 through ADR-067 must add the relevant exact controls below
before their migrations ship; they are not current v0.1 settings:

| Target key | Default | Scope / behavior |
| --- | ---: | --- |
| `runtime.turn_core_rss_mib` | 512 MiB | Global hard reservation for daemon, GUI/client core and supervisor; it cannot be borrowed by payload/worker admission and may only be lowered by configuration |
| `runtime.turn_variable_rss_mib` | 1,024 MiB | Global hard shared pool across every Turn-owned ephemeral payload, queue, renderer, decoder, SpeechWorker and helper working set; every family reservation charges both its own bound and this pool before effect, and it may only be lowered |
| `records.auxiliary_workers_installation` | 128 | Global hard live-or-cleanup-pending count across the closed nine-kind AuxiliaryWorker union; count saturation uses small workers independently of bytes |
| `records.auxiliary_worker_notification_hosts` | 1 | Global hard NotificationHost kind count |
| `records.auxiliary_worker_notification_deliveries` | 32 | Global hard concurrent NotificationDelivery kind count |
| `records.auxiliary_worker_remote_transports` | 128 | Global hard RemoteTransport kind count, still subject to the cross-kind128 cap |
| `records.auxiliary_worker_context_reads` | 128 | Global hard dispatched ContextBrokerRemoteRead kind count; admitted read buffers above this park without source/helper effect |
| `records.auxiliary_worker_transfers` | 32 | Global hard Transfer kind count |
| `records.auxiliary_worker_updaters` | 1 | Global hard Updater kind count |
| `records.auxiliary_worker_provider_brokers` | 32 | Global hard ProviderBroker kind count |
| `records.auxiliary_worker_provider_collectors` | 32 | Global hard ProviderCollector kind count |
| `records.auxiliary_worker_watchdogs` | 64 | Global hard Watchdog kind count |
| `records.auxiliary_worker_item_mib` | 128 MiB | Global hard complete effective RSS/owned working-set reservation for one AuxiliaryWorker, excluding separately bounded payload bytes that are charged once to their typed family |
| `records.auxiliary_worker_mib` | 1,024 MiB | Global hard live-or-cleanup-pending family aggregate, independently reachable with eight maximum workers and also charged to shared variable RSS |
| `records.auxiliary_worker_shutdown_seconds` | 2 seconds | Global maximum grace after authority revocation before owned process/task/socket tree termination; charge persists to quiescence or OS proof |
| `records.process_cleanup_charges` | 4,096 | Global hard body-free Installation records; one≤4-KiB slot reserves before worker spawn and owner-loss transfers existing family/shared reservations rather than releasing or duplicating them |
| `records.process_cleanup_charge_mib` | 16 MiB | Global hard cleanup-metadata aggregate; quiescence or OS-reclamation proof alone releases the inherited worker slot/bytes, while Surface/Node/Session deletion still completes |
| `records.operational_store_mib` | 8,192 MiB | Global hard physical pool for SQLite/WAL, current semantic metadata, receipts/fences, indexes, bounded reconstructible cache and daemon logs; none of the nine payload/journal classes below may hide here |
| `records.media_blob_mib_installation` | 102,400 MiB | Global hard physical aggregate across every Workspace MediaImport temporary+MediaBlob pool; the 10,240-MiB per-Workspace limit also applies |
| `records.turn_owned_physical_disk_mib` | 138,240 MiB | Global hard 135-GiB sum across operational store 8 GiB, sync journals 4 GiB, terminal history 3 GiB, FileSave temp 2 GiB, portable temp 2 GiB, account roots 2 GiB, speech models 8 GiB, Media 100 GiB, transfer/quarantine 4 GiB and update packages 2 GiB; no Turn-owned byte is excluded |
| `records.workspaces_installation` | 1,024 | Global hard non-deleted Workspace count; create/onboarding reserves before path, repository, target or filesystem effect, and the SurfaceHistoryIndex 257th-Workspace oracle remains reachable |
| `records.sessions_per_workspace` | 1,024 | Global hard non-deleted Session count per Workspace |
| `records.sessions_installation` | 10,000 | Global hard non-deleted Session aggregate; duplicate/create/restore reservation precedes Layout, checkout, process or provider effect |
| `records.workspace_write_leases_installation` | 1,024 | Global hard active or ended-owner recovery lease count, at most one per Workspace; an End rehomes or retags the existing row and never allocates another |
| `records.workspace_write_lease_item_kib` | 4 KiB | Workspace/checkout, tagged current-or-ended Session owner, generations, body-free process-start/host-lock evidence and state only |
| `records.workspace_write_lease_mib` | 4 MiB | Global hard exact1,024×4-KiB aggregate; release waits for exact quiescence/reclamation proof but End never waits |
| `records.checkout_lease_minimal_fences` | 100,000 | Installation-lifetime checkout identity/generation/high-water fences; first writer admission pre-reserves one before authority, so Workspace delete only transfers it and cannot refuse |
| `records.checkout_lease_minimal_fence_item_bytes` | 512 bytes | Body-free checkout identity, last lease/owner generation and nonreuse/release result; grants no writer/process authority |
| `records.checkout_lease_minimal_fence_mib` | 50 MiB | Global hard aggregate independently reachable from the count bound; N+1 refuses a future writer acquisition before effect, never Session/Workspace End or delete |
| `records.container_close_rich_receipts` | 16,384 | Global hard current-reservation+rich-terminal pool. Each Session reserves one close-or-delete slot; each Workspace reserves one delete slot and one nonempty close-epoch slot, so the joint 10,000-Session+1,024-Workspace maximum consumes 12,048 before terminal-history headroom |
| `records.container_close_receipt_item_kib` | 16 KiB | Global hard reference-only receipt; subject details remain in paged semantic/runtime recovery inventories |
| `records.container_close_receipt_mib` | 256 MiB | Global hard exact16,384×16-KiB aggregate. Retained-history pressure may refuse only new Workspace/Session admission before effect, never close/delete of an admitted container |
| `records.container_close_receipt_days` | 180 days | Rich terminal receipt boundary; fold requires the minimal exact operation/container/tombstone/result fence |
| `records.container_close_survivor_memberships` | 1,000,000 | Installation-owned immutable receipt→typed-survivor membership count; next-close and Workspace-delete memberships reserve before subject/runtime/helper admission or rehome, so close only consumes and never refuses |
| `records.container_close_survivor_membership_item_bytes` | 512 bytes | Receipt/serial point, typed stable survivor key, inventory/revision locator, ordinal, leaf digest and body-free ViewTarget only |
| `records.container_close_survivor_membership_mib` | 480 MiB | Independent byte boundary admits exactly983,040 maximum memberships; N+1 refuses new work/rehome before effect and rehome during close falls back to pre-reserved recovery |
| `records.container_close_survivor_membership_days` | 180 days | A membership folds only with its rich close receipt; old roots are immutable and a survivor crossing later closes receives distinct memberships |
| `records.container_close_minimal_fences` | 1,000,000 | Independent installation-lifetime count bound; the byte-boundary fixture instead admits exactly983,040 maximum fences. Original close/delete fences are pre-reserved with container admission; N+1 may refuse only a new operation whose tombstone/already-empty outcome is already true |
| `records.container_close_minimal_fence_item_bytes` | 512 bytes | Global hard operation/fingerprint/container/tombstone/result fence; it contains no subject body |
| `records.container_close_minimal_fence_mib` | 480 MiB | Global hard exact983,040×512-byte aggregate, tested independently from the count bound; redundant-operation saturation returns `replay_capacity_refused_already_closed` with zero mutation and no required interaction |
| `records.recovery_inventory_queries_per_connection` | 4 | Local-desktop foreground only; every request binds one exact semantic inventory revision or immutable ContainerCloseReceiptId+three-root vector and closed filter |
| `records.recovery_inventory_queries_installation` | 32 | Shared global hard request-only count across semantic and consolidated close-survivor reads; reconnect inherits none and N+1 reads no subject body |
| `records.recovery_inventory_query_item_mib` | 1 MiB | One existing ChunkedResponseStream/outbox reservation, including page envelope and redacted rows |
| `records.recovery_inventory_query_mib` | 32 MiB | Shared global hard aggregate for the 32 maximum request-only response buffers; no durable cache or second owner |
| `records.recovery_inventory_page_rows` | 500 | Global hard logical page count; receipt-filtered rows verify receipt id, serial ordinal, leaf and typed root |
| `records.recovery_inventory_page_item_kib` | 2 KiB | Redacted typed identity/state/ViewTarget only; no subject body, output, transcript, credential, provider payload or command |
| `records.recovery_inventory_page_mib` | 1 MiB | Global hard logical page size independently enforced with the row bound |
| `records.recovery_inventory_cursor_bytes` | 512 bytes | Authenticated cursor binds inventory/receipt revision, closed filter, typed root, ordinal and predecessor digest |
| `records.recovery_inventory_query_deadline_seconds` | 30 seconds | Timeout, disconnect, atomic transfer or gap wipes and releases the request-only buffer |
| `records.nodes_per_session` | 10,000 | Global hard base Node count including Agent, Group, Tool and Resource kinds; a Group is not a second uncharged record |
| `records.nodes_per_workspace` | 50,000 | Global hard base Node aggregate per Workspace |
| `records.nodes_installation` | 100,000 | Global hard base Node aggregate; creation/adoption reserves before graph, process, provider, file or network effect |
| `records.agent_instances_installation` | 50,000 | Global hard AgentInstance count, each attached to one charged Agent/Subagent Node and covered by the shared semantic bytes |
| `records.live_runtime_attempts_installation` | 10,000 | Global hard nonterminal RuntimeAttempt count; spawn reserves this plus Node/AgentInstance/PendingInteraction/recovery capacity before launch |
| `records.runtime_attempt_detail_records` | 100,000 | Global hard current+ended detail count across AgentInstances; eligible ended detail folds into the per-Agent constant-size aggregate before N+1, otherwise a new spawn refuses pre-effect |
| `records.runtime_attachment_receipts` | 100,000 | Global hard current+rich-terminal Workspace-owned domain-attachment receipt count; capacity reserves before backend attach/CAS |
| `records.runtime_attachment_receipt_item_kib` | 8 KiB | Global hard body-free owner/attempt/binding/target/handle/correlation/result record |
| `records.runtime_attachment_receipt_mib` | 512 MiB | Global hard family bytes, independently admitting exactly65,536 maximum receipts; count and bytes never saturate together |
| `records.runtime_attachment_receipt_days` | 180 days | Rich terminal boundary; uncertain/current correlation never ages out, and terminal richness folds only behind an exact minimal replay fence |
| `records.runtime_attachment_minimal_fences` | 1,000,000 | Independent installation-lifetime operation/fingerprint/owner/attempt/result count bound; saturation refuses domain attach before backend effect |
| `records.runtime_attachment_minimal_fence_mib` | 480 MiB | Independent byte bound, exactly983,040 maximum≤512-byte fences; count and bytes use separate fixtures |
| `records.runtime_effect_rich_records` | 100,000 | Shared global hard intent+receipt-bundle count across launch, lifecycle, configuration and interrupt; the four kinds cannot each spend this limit |
| `records.runtime_effect_rich_item_kib` | 8 KiB | Global hard complete operation/fingerprint/kind/subject/generations/correlation/result metadata bundle; no terminal, transcript, credential or environment body |
| `records.runtime_effect_rich_mib` | 512 MiB | Shared global hard aggregate; the independent maximum-item fixture admits exactly65,536 bundles |
| `records.runtime_effect_rich_days` | 180 days | Terminal rich retention; nonterminal, possible-effect, reconcile-required and cleanup-pending evidence never ages out |
| `records.runtime_effect_minimal_fences` | 1,000,000 | Shared independent installation-lifetime operation/fingerprint/kind/subject/result count; reservation precedes CAS/signal/spawn/provider effect |
| `records.runtime_effect_minimal_fence_item_bytes` | 512 bytes | Global hard body-free replay record; existing-Node Resume/Restart/Recycle launch remains independently replayable while inheriting, never duplicating, that Node's semantic recovery slot |
| `records.runtime_effect_minimal_fence_mib` | 480 MiB | Shared global hard exact983,040×512-byte aggregate; count and bytes use independent fixtures and N+1 refuses pre-effect |
| `records.runtime_lifecycle_nonterminal_per_attempt_owner` | 1 | Exact AttemptOwner hard concurrency bound; competing distinct operation ids serialise and one refuses before signal/stop/launch |
| `records.runtime_lifecycle_nonterminal_installation` | 10,000 | Global hard lifecycle-intent concurrency bound charged inside the shared runtime-effect pool |
| `records.runtime_configuration_nonterminal_per_instance` | 1 | Exact AgentInstance hard concurrency bound; competing switches cannot each publish a configuration epoch |
| `records.runtime_configuration_nonterminal_installation` | 10,000 | Global hard configuration-receipt concurrency bound charged inside the shared runtime-effect pool |
| `records.live_terminal_states_installation` | 128 | Global hard live-or-retained PTY TerminalRuntimeState count; the 129th launch drops only eligible stopped/unpinned/checkpointed state or refuses before process effect |
| `records.terminal_raw_ring_item_mib` | 2 MiB | Global hard complete per-terminal raw ring reservation, including its explicit truncated boundary |
| `records.terminal_raw_ring_mib` | 256 MiB | Global hard aggregate, exactly reachable by 128×2-MiB rings and charged to `runtime.turn_variable_rss_mib` before PTY spawn |
| `records.terminal_current_grid_item_mib` | 4 MiB | Global hard per-terminal current-grid reservation made before PTY spawn/resize; current cells are not evicted under output pressure |
| `records.terminal_screen_item_mib` | 8 MiB | Global hard parsed current-grid+scrollback working set per terminal; oldest unpinned scrollback trims before this boundary |
| `records.terminal_screen_mib` | 512 MiB | Global hard family aggregate including every 4-MiB current-grid reservation and additionally charged to shared variable RSS |
| `records.terminal_scrollback_rows` | 5,000 | Global hard per-terminal decoded scrollback count; trimming sets an exact incomplete-before boundary |
| `records.terminal_image_store_payloads` | 16 | Global hard retained daemon payloads per terminal, with only unplaced LRU eviction |
| `records.terminal_image_store_item_mib` | 16 MiB | Global hard daemon image-store bytes per terminal; no hidden encoded/decode buffer survives an input sequence |
| `records.terminal_image_store_mib` | 512 MiB | Global hard installation aggregate within shared variable RSS; failed admission shows a bounded refusal and preserves PTY text |
| `records.terminal_image_scan_buffers` | 8 | Global hard concurrent encoded image sequences; excess enters bounded discard-to-terminator state and emits one refusal |
| `records.terminal_image_scan_item_mib` | 8 MiB | Global hard encoded iTerm/Kitty/Sixel sequence buffer |
| `records.terminal_image_scan_mib` | 64 MiB | Global hard exact 8×8-MiB aggregate within shared variable RSS |
| `records.terminal_image_chunk_assemblies` | 8 | Global hard concurrent multipart Kitty assemblies across PTYs |
| `records.terminal_image_chunk_item_mib` | 8 MiB | Global hard assembled encoded body before inflate/decode |
| `records.terminal_image_chunk_mib` | 64 MiB | Global hard exact 8×8-MiB aggregate within shared variable RSS |
| `records.terminal_image_decode_working_sets` | 2 | Global hard concurrent native header/inflate/raster/downsample jobs; a third sequence is visibly refused before decoder allocation |
| `records.terminal_image_decode_item_mib` | 128 MiB | Global hard complete encoded+expanded+decoder+raster/downsample working-set reservation |
| `records.terminal_image_decode_mib` | 256 MiB | Global hard exact two-job aggregate within shared variable RSS; success transfers only final RGBA bytes to TerminalImageStore |
| `records.terminal_image_client_caches_per_surface` | 1 | Global hard cache for the one visible terminal projection on a Surface; it owns no terminal truth |
| `records.terminal_image_client_caches_per_connection` | 4 | Global hard live caches, exactly reachable through four Surfaces |
| `records.terminal_image_client_caches_installation` | 64 | Global hard live cache count, exactly reachable through 64 Surfaces |
| `records.terminal_image_client_cache_item_mib` | 12 MiB | Global hard≤12-payload client texture/cache bound; eviction displays placeholder/refetch |
| `records.terminal_image_client_cache_mib` | 256 MiB | Global hard aggregate within shared variable RSS; N+1 opens no extra cache and never loses authoritative screen state |
| `records.pane_attachments_per_surface` | 64 | Global hard exact vNext Pane attachment count for one Surface |
| `records.pane_attachments_per_connection` | 256 | Global hard count, exactly four Surfaces×64 attachments |
| `records.pane_attachments_installation` | 4,096 | Global hard live aggregate; attach N+1 creates no stream/subscriber/baseline |
| `records.pane_attachment_item_kib` | 8 KiB | Global hard stream/owner/generation/sequence/gap metadata item |
| `records.pane_attachment_mib` | 32 MiB | Global hard aggregate exactly 4,096×8 KiB |
| `records.runtime_view_replay_fences` | 1,000,000 | Independent installation-lifetime exact attach/resync/detach operation-fingerprint count; attach/resync reserves its own fence plus the attachment's future detach fence before replacement |
| `records.runtime_view_replay_fence_item_bytes` | 512 bytes | Global hard Surface owner/sequence, attachment/attempt/PTY/buffer generations and typed outcome; no terminal body/lifecycle receipt |
| `records.runtime_view_replay_fence_mib` | 480 MiB | Independent exact983,040-maximum-fence byte boundary; saturation refuses a new attachment/resync before mutation, while an admitted detach consumes its non-borrowable reservation |
| `records.terminal_screen_baseline_item_mib` | 2 MiB | Global hard per cells-attachment diff/resync baseline |
| `records.terminal_screen_baseline_mib` | 256 MiB | Global hard family aggregate within shared variable RSS; count remains independently reachable with small grids |
| `records.terminal_output_queue_chunks_per_terminal` | 512 | Global hard shared-Arc broadcast chunks for one PTY, additionally bounded to8 MiB |
| `records.terminal_output_queue_mib_per_terminal` | 8 MiB | Global hard per-terminal queued raw output; overflow gaps lagging subscribers after authoritative buffer update |
| `records.terminal_output_queue_chunks_installation` | 4,096 | Global hard all-terminal queued-chunk count |
| `records.terminal_output_queue_mib` | 256 MiB | Global hard aggregate within shared variable RSS; every raw chunk is≤64 KiB and referenced rather than copied per subscriber |
| `records.terminal_pump_batches` | 128 | Global hard active projection/writer batches; absence parks/gaps only the attachment |
| `records.terminal_pump_batch_frames` | 16 | Global hard frames in one batch, additionally bounded to1 MiB |
| `records.terminal_pump_batch_item_mib` | 1 MiB | Global hard complete writer-batch allocation |
| `records.terminal_pump_batch_mib` | 128 MiB | Global hard exact 128×1-MiB aggregate within shared variable RSS |
| `records.protocol_outbox_frames_per_connection` | 256 | Global hard authenticated-connection pending frames, additionally bounded to8 MiB |
| `records.protocol_outbox_mib_per_connection` | 8 MiB | Global hard connection outbox bytes; a slow peer gaps/resyncs or disconnects without stalling PTY/Attention |
| `records.protocol_outbox_frames_installation` | 4,096 | Global hard pending-frame aggregate across authenticated connections |
| `records.protocol_outbox_mib` | 128 MiB | Global hard aggregate within shared variable RSS; each encoded frame is≤256 KiB |
| `records.protocol_outbox_critical_frames_per_connection` | 32 | Non-borrowable subset reserved for input receipts, Attention, lifecycle/control and gaps |
| `records.protocol_outbox_critical_mib_per_connection` | 1 MiB | Non-borrowable critical byte subset; terminal/content traffic is limited to the remaining224 frames/7 MiB |
| `records.protocol_outbox_critical_frames_installation` | 512 | Non-borrowable global critical subset; terminal/content traffic is limited to3,584 frames |
| `records.protocol_outbox_critical_mib` | 16 MiB | Non-borrowable global critical subset inside the128-MiB outbox family |
| `records.client_inbound_messages_per_connection` | 64 | Global hard local-client connection-generation queue; 16 slots are non-borrowable for lifecycle, Attention, input receipts and scoped gaps |
| `records.client_inbound_messages_installation` | 4,096 | Global hard aggregate; 1,024 slots are the corresponding non-borrowable critical partition |
| `records.client_inbound_item_kib` | 4 KiB | Global hard decoded safe message item; larger presentation data uses a bounded response stream or scoped gap |
| `records.client_inbound_mib` | 16 MiB | Global hard exact4,096×4-KiB family aggregate inside shared variable RSS |
| `records.client_outbound_intents_per_connection` | 256 | Global hard not-yet-written local-client intents; N+1 is typed backpressure and emits zero socket byte |
| `records.client_outbound_intents_installation` | 4,096 | Global hard aggregate across current connection generations |
| `records.client_outbound_intent_item_kib` | 4 KiB | Global hard exact request metadata/body fragment; bulk bodies use their named typed family |
| `records.client_outbound_intent_mib` | 16 MiB | Global hard exact4,096×4-KiB family aggregate inside shared variable RSS |
| `records.client_awaiting_requests_per_connection` | 512 | Global hard written-request registry; no current entry is evicted to admit another |
| `records.client_awaiting_requests_installation` | 4,096 | Global hard aggregate; N+1 is not written |
| `records.client_awaiting_request_item_kib` | 4 KiB | Global hard request id/operation id/expected result/reconciliation metadata, never a response body |
| `records.client_awaiting_request_mib` | 16 MiB | Global hard exact4,096×4-KiB family aggregate inside shared variable RSS |
| `records.native_dialogs_per_window` | 1 | Global hard local-window descriptor; replacement requires the same dialog id and expected revision |
| `records.native_dialogs_installation` | 64 | Global hard current descriptor count |
| `records.native_dialog_item_kib` | 4 KiB | Global hard safe type/subject/revision descriptor; provider/file body is excluded |
| `records.native_dialog_kib` | 256 KiB | Global hard exact64×4-KiB aggregate; window/process loss revokes and releases |
| `records.companion_action_dispatch_per_session` | 1 | Global hard one current descriptor per exact RemoteSession generation |
| `records.companion_action_dispatch_installation` | 64 | Global hard aggregate; N+1 performs zero remote effect |
| `records.companion_action_dispatch_item_kib` | 8 KiB | Global hard action/subject/revision/grant metadata, never context/prompt/credential body |
| `records.companion_action_dispatch_kib` | 512 KiB | Global hard exact64×8-KiB aggregate; session/connection loss revokes and releases |
| `records.topology_queue_events_per_source` | 1,024 | Global hard exact SourceId+ObservationEpoch queue; overflow gaps only that source and never blocks the producer |
| `records.topology_queue_events_installation` | 4,096 | Global hard aggregate across discovery sources |
| `records.topology_queue_event_item_kib` | 4 KiB | Global hard one structured safe topology envelope; transcript/output/provider body is absent |
| `records.topology_queue_mib` | 16 MiB | Global hard exact4,096×4-KiB aggregate inside shared variable RSS; count/item/bytes reserve before delivery |
| `records.chunked_response_streams_per_connection` | 4 | Global hard automatic large logical responses for one connection |
| `records.chunked_response_streams_installation` | 16 | Global hard aggregate; N+1 returns typed backpressure before source/body read |
| `records.chunked_response_item_kib` | 7,680 KiB | Global hard logical result/reassembly buffer; each raw chunk≤180 KiB and encoded frame≤256 KiB |
| `records.chunked_response_mib` | 120 MiB | Global hard family aggregate exactly reachable by16×7,680-KiB results within shared variable RSS |
| `records.protocol_request_id_bytes` | 128 ASCII bytes | Global hard nonempty `[A-Za-z0-9._:-]+`; N+1 rejects before registry/outbox/stream allocation |
| `records.terminal_image_fetches_per_surface` | 8 | Global hard visible image fetches, matching the maximum wanted ids |
| `records.terminal_image_fetches_per_connection` | 32 | Global hard four-Surface aggregate |
| `records.terminal_image_fetches_installation` | 128 | Global hard semantic fetch count; N+1 leaves a labelled placeholder |
| `records.terminal_image_fetch_item_mib` | 4 MiB | Global hard logical RGBA body, verified by ImageId/digest before cache transfer |
| `records.terminal_image_fetch_mib` | 128 MiB | Global hard family aggregate; stream/fetch names one allocation and shared RSS charge, never two copies |
| `records.panes_per_session` | 64 | Global hard Pane count including terminal and resource Panes; temporary Panes additionally remain Surface-owned ephemeral state |
| `records.panes_installation` | 4,096 | Global hard durable Pane aggregate; Pane/Layout capacity reserves before attach, PTY or renderer creation |
| `records.temporary_panes_per_surface` | 8 | Global hard Surface-owned ephemeral views; they never count as durable Layout rows or start a process/PTY/renderer |
| `records.temporary_panes_per_client` | 32 | Global hard live count per connection, exactly reachable through four Surfaces×eight views |
| `records.temporary_panes_installation` | 512 | Global hard live aggregate, exactly reachable through 64 Surfaces×eight views; N+1 mints nothing |
| `records.temporary_pane_item_kib` | 4 KiB | Global hard safe identity/revision/view metadata item; no content body |
| `records.temporary_pane_mib` | 2 MiB | Global hard encoded aggregate; count+bytes reserve before mint |
| `records.temporary_pane_idle_minutes` | 30 minutes | Global exact idle deadline; close/promotion/source invalidation/Surface/connection/daemon loss releases |
| `records.temporary_settings_keys_per_surface` | 256 | Global hard declared-key count in one local-client/Surface record; unknown keys refuse |
| `records.temporary_settings_value_kib` | 4 KiB | Global hard one validated encoded value; secret/undeclared values remain forbidden |
| `records.temporary_settings_record_kib` | 64 KiB | Global hard complete record including key/revision/provenance framing |
| `records.temporary_settings_records_per_client` | 8 | Global hard one per Surface for one LocalClientInstanceId |
| `records.temporary_settings_records_installation` | 64 | Global hard local-window aggregate, exactly one per possible Surface |
| `records.temporary_settings_mib` | 4 MiB | Global hard aggregate, exactly 64×64 KiB; same-window reconnect preserves and window/process/Surface exit drops |
| `records.persistent_settings_keys_per_owner` | 256 | Global hard override count in one exact Global/Workspace/Template/Session record; unknown retained keys count and remain individually resettable |
| `records.persistent_settings_value_kib` | 4 KiB | Global hard one validated encoded value or opaque retained newer-version value; secrets still use references/redaction, never literal secret bytes |
| `records.persistent_settings_record_kib` | 64 KiB | Global hard complete owner record including revision/provenance framing |
| `records.persistent_settings_records` | 16,384 | Global hard lazy nonempty owner-record count; N+1 refuses only the setting write, not owner creation/use |
| `records.persistent_settings_mib` | 1,024 MiB | Global hard exact16,384×64-KiB encoded aggregate inside the operational-store cap |
| `records.settings_registry_sections` | 23 | Global compile-time closed SettingsSectionId union+scope matrix; commercial entitlement/licence has no row |
| `records.settings_registry_definitions` | 2,048 | Global compile-time stable definitions, each≤2 KiB and exactly one section |
| `records.settings_reset_previews_per_surface` | 1 | Global hard memory-only current preview; replacement reserves before atomic swap |
| `records.settings_reset_previews_per_client` | 8 | Global hard one per Surface for one LocalClientInstanceId |
| `records.settings_reset_previews_installation` | 64 | Global hard aggregate count |
| `records.settings_reset_preview_item_mib` | 1 MiB | Global hard exact registry/owner/revision/digest plus≤256 safe rows≤2 KiB each and framing |
| `records.settings_reset_preview_mib` | 64 MiB | Global hard exact64×1-MiB aggregate inside shared variable RSS |
| `records.settings_reset_preview_seconds` | 60 seconds | Apply/cancel/expiry/stale revision/Surface/window/client or daemon loss releases; reconnect inherits no mutation authority |
| `records.settings_mutation_receipts` | 100,000 | Global hard body-free persistent set/reset/section-reset receipt count, each≤4 KiB; terminal capacity reserves before mutation |
| `records.settings_mutation_receipt_mib` | 384 MiB | Global hard encoded aggregate reached by98,304 maximum receipts; count and bytes saturate independently |
| `records.settings_mutation_receipt_days` | 180 days | Rich receipt compacts only after operation/owner/registry/record/replay fences remain |
| `records.local_input_drafts_per_surface` | 1 | Global hard visible composer draft; a new target marks it stale and never silently replaces it |
| `records.local_input_drafts_per_client` | 8 | Global hard one per Surface for one LocalClientInstanceId |
| `records.local_input_drafts_installation` | 64 | Global hard memory-only aggregate count |
| `records.local_input_draft_item_kib` | 32 KiB | Global hard editable UTF-8 body including typed/voice-origin text after sanitisation |
| `records.local_input_draft_mib` | 2 MiB | Global hard aggregate, exactly 64×32 KiB; no duplicate VoiceTranscript body |
| `records.ime_compositions_per_surface` | 1 | Global hard active local composition; zero target bytes until commit |
| `records.ime_compositions_per_client` | 8 | Global hard one per Surface for one LocalClientInstanceId |
| `records.ime_compositions_installation` | 64 | Global hard memory-only aggregate count |
| `records.ime_composition_item_kib` | 32 KiB | Global hard composition text/target metadata item |
| `records.ime_composition_mib` | 2 MiB | Global hard aggregate, exactly 64×32 KiB; focus/target/Surface/window loss cancels |
| `records.semantic_core_records` | 200,000 | Global hard shared count across every durable current semantic metadata record not assigned a stricter body pool: Workspace, Session, base Node (including Group and WorkItem shell), AgentInstance, every retained RuntimeAttempt detail, Pane, Layout, TeamMembership, DependencyEdge and Flow/relationship core; all family limits also charge this envelope |
| `records.semantic_core_item_kib` | 64 KiB | Global hard encoded metadata item bound, excluding separately bounded Note/content/body/journal categories; Layout alone may use the exact 256-KiB limit below |
| `records.layout_item_kib` | 256 KiB | Global hard encoded Layout bound for at most 64 Pane descriptors and focus/zoom/binding metadata, never terminal/content bodies |
| `records.semantic_core_mib` | 1,024 MiB | Global hard encoded aggregate across the semantic core; family maxima are not additive and every create/adopt/launch reserves count+bytes atomically |
| `records.hierarchy_index_mib` | 6 MiB | Global hard per vNext compact index over≤111,024 Workspace+Session+Node coordinates; key/parent/kind/order/flags/RowMetricClass only, with no label/body |
| `records.hierarchy_bootstrap_wire_kib` | 7,680 KiB | Global hard complete logical vNext get_hierarchy result including index, first≤1-MiB page, tree/filter/framing; carried automatically in≤180-KiB raw/≤256-KiB encoded stream frames and applied atomically |
| `records.hierarchy_page_rows` | 500 | Global hard row summaries per page, each≤2 KiB and response≤1 MiB |
| `records.hierarchy_scans_per_client` | 16 | Global hard live scans per authenticated connection; 1,024 installation-wide and N+1 does not evict a visible scan |
| `records.hierarchy_scan_mib` | 16 MiB | Global hard memory-only scan metadata aggregate; row/page response bytes additionally charge the shared variable-RSS pool while queued |
| `records.hierarchy_scan_idle_seconds` | 60 seconds | Complete final page, explicit close, revision gap, scope loss, disconnect or exact idle expiry releases; reconnect inherits none |
| `records.hierarchy_filter_bitmap_kib` | 16 KiB raw / 24 KiB NDJSON | Global hard one revisioned packed-bit match bitmap over the compact index; closed codec/framing expansion fits the wire bound and contains no query text or label |
| `records.hierarchy_reveal_depth` | 128 Group levels + Workspace/Session | Global hard exact ancestor chain; response plus materialising pages≤1 MiB and label similarity is never fallback |
| `records.hierarchy_delta_operations` | 4,096 | Global hard ordered compact operations/push; count saturation uses small operations independently of bytes |
| `records.hierarchy_delta_raw_kib` | 180 KiB | Global hard serialized vNext delta payload in one complete≤256-KiB encoded frame; the first excess operation emits a scoped gap and automatic refresh, never a fragmented push |
| `records.journaled_terminal_panes` | 256 | Global hard Panes with retained terminal history; N+1 with history enabled refuses before PTY/process launch rather than silently disabling history |
| `records.terminal_journal_mib_installation` | 2,048 MiB | Global hard physical aggregate, exactly 256×the 8-MiB per-Pane ceiling; allocation reserves before launch and rotation never exceeds it |
| `records.terminal_checkpoint_mib_installation` | 1,024 MiB | Global hard physical aggregate, exactly 256×the 4-MiB per-Pane ceiling; checkpoint replacement transfers charge without double count |
| `records.runtime_attempts_per_agent` | 100 | Global; current plus at most 99 ended detail records; older attempts fold into one constant-size aggregate receipt, never one digest each |
| `records.session_activation_receipts_per_session` | 100 | Global; current plus newest safe preflight/outcome receipts; referenced uncertain/current attempt evidence is preserved |
| `records.session_activation_receipt_days` | 30 days | Global; unreferenced terminal activation receipts older than the boundary compact even below count |
| `records.lineage_edges_per_agent` | 256 | Global; refuses a new live edge at the bound and cascades on AgentInstance deletion |
| `records.context_scopes_per_agent` | 32 | Global; active scopes are owner-lifetime and refuse creation at the bound |
| `records.quota_scopes_per_account` | 32 | Global; bounded per safe provider/account owner and cascades when that owner is removed |
| `records.runtime_endpoints_per_agent` | 16 | Global; bounded configured continuity endpoints per AgentInstance |
| `records.remote_cleanup_tombstones` | 1,000 | Global; never pruned before authenticated purge proof; at capacity no new remote artifact is created |
| `records.active_context_links_per_agent` | 64 | Global; each live link counts at both endpoints and creation is refused at either bound |
| `records.active_context_links_installation` | 10,000 | Global hard active-link aggregate; N+1 creates no bearer, source access or delegated authority |
| `records.context_broker_bearers` | 10,000 | Global hard memory-only one-current-bearer-per-Link destination attempt bound; rotation replaces only after reservation |
| `records.context_broker_bearer_item_kib` | 4 KiB | Global hard capability/owner/scope/expiry metadata item; bearer bytes never persist or enter logs/argv/environment |
| `records.context_broker_bearer_mib` | 32 MiB | Global hard reachable aggregate within shared variable RSS; count may be reached with smaller encoded bearers |
| `records.context_broker_reads_per_attempt` | 4 | Global hard in-flight non-streaming read buffers for one destination attempt |
| `records.context_broker_reads_per_link` | 16 | Global hard in-flight read buffers for one ContextLink across allowed parallel attempts |
| `records.context_broker_reads_installation` | 256 | Global hard in-flight aggregate; N+1 opens no source and dispatches no remote helper |
| `records.context_broker_read_item_mib` | 1 MiB | Global hard returned-body buffer per read, excluding bounded metadata |
| `records.context_broker_read_mib` | 256 MiB | Global hard aggregate exactly reachable by 256 full buffers and additionally charged to shared variable RSS |
| `records.context_broker_read_seconds` | 30 seconds | Global hard wall deadline; timeout fences result and releases only after descriptor/network/write quiescence |
| `records.expired_context_link_days` | 30 days | Global; active authority is governed by required expiry/revoke, not this history limit |
| `records.expired_context_link_limit` | 10,000 | Global |
| `records.context_read_audit_days` | 30 days | Global |
| `records.context_read_audit_limit` | 50,000 | Global |
| `records.context_packet_metadata_days` | 30 days | Global |
| `records.context_packet_metadata_limit` | 10,000 | Global body-free metadata/replay slots; one reserves before preparation and remains through terminal compaction |
| `records.context_packet_drafts_per_client` | 16 | Global hard unaccepted ad-hoc drafts per source connection; disconnect discards them |
| `records.context_packet_live_bodies` | 128 | Global hard draft-or-accepted body count; acceptance transfers ownership to daemon+Workspace+delivery+target generation rather than releasing on source disconnect |
| `records.context_packet_body_mib` | 1 MiB | Global hard canonical UTF-8 body item; requested context budgets above it refuse before source read |
| `records.context_packet_review_mib` | 1 MiB | Global hard inert rendered review item; body+review working set is≤2 MiB |
| `records.context_packet_working_mib` | 256 MiB | Global hard body+review+encoder aggregate inside `runtime.turn_variable_rss_mib`; release requires buffer quiescence/OS reclamation |
| `records.context_packet_ttl_seconds` | 600 seconds | Global hard pre-submission draft/accepted-body deadline; no caller or Flow can widen it |
| `records.portable_package_mib` | 64 MiB | Global hard per reviewed export/import package; larger packages refuse before file write or remint |
| `records.portable_context_artifact_mib` | 16 MiB | Global hard per inert package member including digest/tail/artifact bytes and manifests; no live ids or authority fields are retained |
| `records.active_portable_exports` | 16 | Global hard nonterminal bound although each saga is source-Workspace owned; N+1 prepare refuses before reading or assembling |
| `records.active_portable_imports` | 16 | Global hard Installation-stream nonterminal bound; N+1 prepare refuses before reading, validating or reminting |
| `records.portable_temporary_mib` | 2,048 MiB | Global hard owner-only aggregate across export assembly and import validation temporaries; prepare reserves the full declared≤64-MiB allowance and capacity failure has zero file/remint effect |
| `records.portable_terminal_receipts` | 10,000 | Global terminal safe operation/package/path/destination/result metadata; capacity is reserved before assembly/validation and no package body is retained |
| `records.portable_terminal_receipt_days` | 30 days | Rich terminal metadata compacts only after minimal operation/package/path/destination/result replay and collision fences are durable; committing/reconcile evidence never ages out |
| `records.agent_message_metadata_days` | 30 days | Global |
| `records.agent_message_metadata_limit` | 100,000 | Global body-free terminal metadata/replay slots; one reserves before queue admission |
| `records.agent_message_drafts_per_client` | 16 | Global hard unaccepted ad-hoc drafts per source connection; disconnect discards them |
| `records.agent_message_body_kib` | 4 KiB | Global hard canonical UTF-8 body item |
| `records.agent_messages_per_destination` | 256 | Global hard prepared-or-queued FIFO count for one exact destination generation |
| `records.agent_message_mib_per_destination` | 1 MiB | Global hard body bytes for one exact destination generation |
| `records.agent_message_live_bodies` | 10,000 | Global hard prepared-or-queued body count; accepted queue ownership is daemon+Workspace+delivery+destination generation |
| `records.agent_message_body_mib` | 32 MiB | Global hard body-byte aggregate reached by8,192 maximum bodies; count and bytes saturate independently inside the family/shared pool |
| `records.agent_message_working_mib` | 64 MiB | Global hard body+queue+encoder aggregate inside `runtime.turn_variable_rss_mib`; release requires buffer quiescence/OS reclamation |
| `records.agent_message_ttl_seconds` | 600 seconds | Global hard pre-submission deadline; submitted evidence never rewinds |
| `records.usage_sample_days` | 30 days | Global |
| `records.usage_samples_per_scope` | 2,880 | Global; newest bounded observations per Context/Quota scope |
| `records.note_max_kib` | 256 KiB | Global or Workspace override; one active Note |
| `records.notes_per_workspace_mib` | 16 MiB | Global or Workspace override; active Notes require explicit delete when full |
| `records.note_revisions_per_note` | 50 | Global; maximum unreferenced historical revisions in addition to current/required revisions |
| `records.note_revision_days` | 180 days | Global; an unreferenced historical revision is removed when older than this bound even if fewer than 50 remain |
| `records.note_revision_mib_per_workspace` | 64 MiB | Global; includes current, pinned, live-disclosed and historical bodies; an edit is refused if required revisions leave no room |
| `records.coordination_edges_per_session` | 1,000 | Global; active Dependency/Team records require explicit delete when full |
| `records.dependency_result_summary_kib` | 4 KiB | Global; optional control-stripped/redacted text in the closed result schema |
| `records.dependency_results_per_flow_run` | 4,096 | Global hard immutable edge+producer-StepAttempt result records; capacity reserves before producer terminal commit and N+1 starts no dependant |
| `records.dependency_result_item_kib` | 8 KiB | Global hard closed state/provenance/hash/reference/summary record, with summary itself≤4 KiB and no raw output/transcript/file/diff/environment |
| `records.dependency_result_mib_per_flow_run` | 32 MiB | Global hard exact4,096×8-KiB aggregate; referenced results remain with the run |
| `records.dependency_results_installation` | 100,000 | Global hard immutable-result count across runs |
| `records.dependency_result_mib_installation` | 256 MiB | Global hard encoded aggregate; count and bytes saturate independently |
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
| `records.work_item_sources` | 64 | Global hard installation-wide non-deleted source bound; N+1 create refuses before credential/provider/configuration effect, and delete frees a slot only after current binding/mutation/conflict and resurrection fences move to bounded owners |
| `records.work_item_key_registry_entries` | 1,000,000 | Global hard Installation-owned current/binding/tombstone slots, each≤512 bytes; import/external-create reserves before Node/provider effect and N+1 refuses |
| `records.work_item_key_registry_mib` | 480 MiB | Global hard encoded registry bound reached by983,040 maximum entries; count and bytes saturate independently, and exact key tombstone folds only after terminal source deletion into the monotonic source-id/generation fence |
| `records.work_item_source_operations` | 10,000 | Global hard nonterminal plus uncompacted terminal sync/create/mutation slots; terminal capacity is reserved before every provider request and N+1 refuses before effect |
| `records.work_item_query_buffers_per_client` | 4 | Global hard concurrent interactive source queries per authenticated connection |
| `records.work_item_query_buffers_installation` | 32 | Global hard aggregate; N+1 opens no provider page and allocates no response |
| `records.work_item_query_buffer_item_mib` | 2 MiB | Global hard raw-provider/sanitisation working set for one query, never retained or exported |
| `records.work_item_query_buffer_mib` | 64 MiB | Global hard exact32×2-MiB aggregate, also charged to shared variable RSS before provider read |
| `records.work_item_query_deadline_seconds` | 30 seconds | Completion/failure/cancel/gap/deadline/disconnect releases raw bytes after I/O quiescence |
| `records.work_item_page_rows` | 500 | Global request-only safe summaries; the501st continues and never truncates to complete |
| `records.work_item_page_item_kib` | 2 KiB | Global hard safe list projection excluding body/comments/credentials/raw provider fields |
| `records.work_item_page_mib` | 1 MiB | Global hard logical response; count and bytes are independently saturable and body transfer uses the generic stream/outbox |
| `records.work_item_cursor_bytes` | 512 bytes | Global hard authenticated cursor binding source/project/generations/filter/sort/ordinal/predecessor; oversize or stale gaps |
| `records.work_item_source_receipt_days` | 30 days | Global; terminal richness compacts only after operation replay, key/binding, source-generation and conflict fences survive; nonterminal/possible-effect/reconcile/active-conflict evidence never ages out |
| `records.delegated_resources_per_flow_run` | 1,000 | Global; live typed Resources owned by one FlowRun; further creation raises exact Attention |
| `records.delegated_resource_max_kib` | 256 KiB | Global; one Turn-owned Resource body after schema validation |
| `records.delegated_resource_mib_per_workspace` | 64 MiB | Global; mutation is refused rather than pruning an active Resource |
| `records.delegated_progress_per_operation` | 100 | Global; latest plus at most 99 replaced progress records; records older than 7 days compact first |
| `records.delegated_progress_days` | 7 days | Global; a replaced progress record is removed when older even if fewer than 100 remain |
| `records.delegated_progress_max_kib` | 4 KiB | Global; one progress record including safe provenance, never terminal or file content |
| `records.runtime_bindings_per_endpoint` | 64 | Global; independently owned active/recoverable bindings on one shared endpoint, each carrying either exact profiled scope or that endpoint's opaque unscoped scope; creation is refused at the bound |
| `records.runtime_endpoint_continuity_verification_buffers` | 128 | Global hard request-only authenticated proof buffers, one per endpoint; broker disconnect or five-second deadline releases only after verification quiesces |
| `records.runtime_endpoint_continuity_verification_buffer_item_kib` | 256 KiB | Global hard complete canonical 1..64-claim proof/MAC working set; never durable or exported |
| `records.runtime_endpoint_continuity_verification_buffer_mib` | 32 MiB | Global hard exact128×256-KiB aggregate |
| `records.runtime_endpoint_continuity_receipts` | 100,000 | Global hard rich terminal result vectors; capacity reserves before candidate observation or endpoint/binding CAS |
| `records.runtime_endpoint_continuity_receipt_item_kib` | 4 KiB | Global hard body-free root/per-binding result record |
| `records.runtime_endpoint_continuity_receipt_mib` | 256 MiB | Global hard rich aggregate, independently admitting exactly65,536 maximum records |
| `records.runtime_endpoint_continuity_receipt_days` | 180 days | Rich terminal retention; revalidating evidence never ages out and compaction requires the exact minimal fence |
| `records.runtime_endpoint_continuity_minimal_fences` | 1,000,000 | Independent installation-lifetime exact operation/fingerprint/endpoint/proof/result count; a scalar sequence high-water is insufficient |
| `records.runtime_endpoint_continuity_minimal_fence_item_bytes` | 512 bytes | Global hard body-free changed-request replay fence |
| `records.runtime_endpoint_continuity_minimal_fence_mib` | 480 MiB | Global hard exact983,040×512-byte aggregate, independently tested from count |
| `records.conversation_profile_rebind_buffers` | 128 | Global hard request-only local-foreground buffers, one per old binding; authority/Surface/connection loss releases after quiescence |
| `records.conversation_profile_rebind_buffer_item_kib` | 64 KiB | Global hard canonical old/new ownership-CAS working set |
| `records.conversation_profile_rebind_buffer_mib` | 8 MiB | Global hard exact128×64-KiB aggregate |
| `records.conversation_profile_rebind_receipts` | 100,000 | Global hard Workspace-owned rich terminal committed/refused receipts; reservation precedes CAS |
| `records.conversation_profile_rebind_receipt_item_kib` | 4 KiB | Global hard body-free old/new key, binding, registry and result record |
| `records.conversation_profile_rebind_receipt_mib` | 256 MiB | Global hard rich aggregate, independently admitting exactly65,536 maximum records |
| `records.conversation_profile_rebind_receipt_days` | 180 days | Rich terminal retention behind the already-reserved exact minimal fence |
| `records.conversation_profile_rebind_minimal_fences` | 1,000,000 | Independent installation-lifetime exact operation/fingerprint/old-new-key/result count; N+1 refuses before CAS |
| `records.conversation_profile_rebind_minimal_fence_item_bytes` | 512 bytes | Global hard body-free replay and changed-request conflict record |
| `records.conversation_profile_rebind_minimal_fence_mib` | 480 MiB | Global hard exact983,040×512-byte aggregate, tested independently from count |
| `records.workspace_semantic_recovery_reservations` | 4,096 | Global hard per Workspace across current recoverable semantic subjects and inventoried survivors; N+1 subject admission refuses before effect, while End consumes its existing reservation |
| `records.workspace_semantic_recovery_mib` | 64 MiB | Global hard per Workspace; each reservation preallocates one ≤16-KiB metadata/evidence record and never copies the subject body |
| `records.installation_semantic_recovery_reservations` | 32,768 | Global hard installation-wide migration slots reserved concurrently with Workspace admission; Workspace delete moves an existing slot and never allocates/refuses |
| `records.installation_semantic_recovery_mib` | 512 MiB | Global hard installation inventory/reservation budget; active subject bodies remain charged to their own category |
| `records.semantic_recovery_terminal_days` | 180 days | Global; resolved terminal metadata compacts only after identity/replay fences are durable and releases its reservation; live, uncertain and cleanup-required evidence never ages out |
| `records.runtime_inventory_handles_per_target` | 10,000 | Global; known plus unmatched handles in one target generation; overflow marks the snapshot gapped |
| `records.runtime_inventory_snapshot_mib` | 16 MiB | Global; maximum redacted snapshot per target; overflow is a gap, never silent truncation/exactness |
| `records.runtime_inventory_observation_days` | 7 days | Global; only the latest complete/partial/gapped snapshot per live generation is owner-lifetime |
| `records.resource_inventory_processes_per_target` | 10,000 | Global; reuse-safe process rows in the same target snapshot; overflow marks coverage gapped and never drops into exact accounting |
| `records.runtime_reconciliation_receipts` | 10,000 | Global; newest safe adopt/ignore/terminate proofs installation-wide |
| `records.runtime_reconciliation_receipt_days` | 180 days | Global; a receipt is removed when older even if fewer than 10,000 remain |
| `records.native_jobs_per_profile` | 10,000 | Global; current provider-job projections; overflow marks inventory gapped rather than dropping a job silently |
| `records.native_job_scans_per_client` | 8 | Global hard daemon-minted pinned scans per authenticated connection; 512 installation-wide |
| `records.native_job_scan_item_kib` | 32 KiB | Global hard profile/target/adapter generation, snapshot watermark and chained-cursor metadata |
| `records.native_job_scan_mib` | 16 MiB | Global hard exact512×32-KiB scan metadata aggregate |
| `records.native_job_scan_idle_seconds` | 60 seconds | Complete final page/gap/generation loss/disconnect/TTL releases; reconnect inherits none |
| `records.native_job_page_buffers_per_client` | 4 | Global hard concurrent raw-provider page reads per authenticated connection; 32 installation-wide |
| `records.native_job_page_buffer_item_mib` | 2 MiB | Global hard raw-provider/sanitisation working set before page projection |
| `records.native_job_page_buffer_mib` | 64 MiB | Global hard exact32×2-MiB aggregate, also charged to shared variable RSS |
| `records.native_job_page_deadline_seconds` | 30 seconds | Completion/failure/cancel/gap/deadline/disconnect releases after I/O quiescence |
| `records.native_job_page_rows` | 500 | Global request-only safe job summaries; one item≤2 KiB/page≤1 MiB, scan≤10,000 and cursor≤512 bytes |
| `records.native_job_definition_kib_per_job` | 64 KiB | Global; exact private Job-scoped definition only, never logs/diagnostics/broad export or executable control |
| `records.native_job_result_metadata_kib` | 32 KiB | Global hard bound per iteration; summary/error sublimits and inert-reference counts apply before persistence, with only count/hash truncation evidence beyond it |
| `records.native_job_iterations_per_job` | 1,100 | Global hard materialised iteration-key aggregate per Job: up to 1,000 active plus 100 newest terminal safe metadata records; no active key is evicted or made uncontrollable to retain a terminal row |
| `records.native_job_active_iterations_per_job` | 1,000 | Global hard materialisation bound for queued/running/unknown iterations; Turn-initiated run-now refuses before provider effect at the bound, while excess external inventory marks coverage gapped and disables control rather than growing or fabricating absence |
| `records.native_job_terminal_iterations_per_job` | 100 | Global hard newest unreferenced terminal metadata rows inside the 1,100 aggregate; eligible terminal compaction never removes an active key |
| `records.native_job_iteration_days` | 180 days | Global; ended unreferenced iteration metadata older than the boundary compacts even below count |
| `records.native_job_nonterminal_intents_per_profile` | 10,000 | Global hard admission bound across create/mutation intents; the next Turn mutation refuses before provider effect and raises system Attention until reconciliation frees capacity |
| `records.native_job_terminal_receipts_per_profile` | 10,000 | Global; newest terminal safe metadata receipts after immutable replay-fence extraction; external overflow marks inventory gapped |
| `records.native_job_mutation_receipt_days` | 180 days | Global; terminal rich receipts compact after the boundary, while minimal operation replay and deletion/visibility identity fences survive until installation deletion |
| `records.native_job_replay_fences` | 1,000,000 | Installation hard bound of minimal non-secret operation id/fingerprint/subject/result records; when full and uncompacted, new provider-affecting native-job operations refuse before intent/effect |
| `records.conversation_inventory_entries_per_profile` | 10,000 | Global; metadata-only bounded cache; overflow marks coverage gapped and refuses authoritative search/zero |
| `records.conversation_inventory_cache_minutes` | 15 minutes | Global; expiry changes the cache to stale and never deletes provider data or fabricates an empty result |
| `records.conversation_inventory_queries_per_client` | 4 | Global hard concurrent queries per authenticated connection; 32 installation-wide |
| `records.conversation_inventory_query_item_mib` | 2 MiB | Global hard raw-provider/cache/sanitisation working set, never retained/exported |
| `records.conversation_inventory_query_mib` | 64 MiB | Global hard exact32×2-MiB aggregate, also charged to shared variable RSS |
| `records.conversation_inventory_query_deadline_seconds` | 30 seconds | Completion/failure/cancel/deadline/disconnect releases after I/O quiescence |
| `records.conversation_inventory_page_rows` | 500 | Global request-only safe descriptors; one item≤2 KiB/page≤1 MiB, scan≤10,000 and authenticated cursor≤512 bytes |
| `records.conversation_adoption_receipts` | 100,000 | Global hard Workspace-owned rich terminal receipts; count and aggregate-byte boundaries are independent and admission reserves before the ownership CAS |
| `records.conversation_adoption_receipt_item_kib` | 4 KiB | Global hard receipt metadata bound; no transcript, terminal bytes, credential or context body |
| `records.conversation_adoption_receipt_mib` | 256 MiB | Global hard rich aggregate; the maximum-item byte fixture admits exactly65,536 receipts while the count fixture uses smaller records |
| `records.conversation_adoption_receipt_days` | 180 days | Rich terminal metadata retention; compaction requires its already-reserved permanent exact result fence |
| `records.conversation_adoption_minimal_fences` | 1,000,000 | Independent installation-lifetime count bound; N+1 refuses adoption before CAS and cannot be evaded by rich compaction |
| `records.conversation_adoption_minimal_fence_item_bytes` | 512 bytes | Global hard operation/fingerprint/ConversationKey/preassigned identities/result fence, including committed `no_prior_attempt` |
| `records.conversation_adoption_minimal_fence_mib` | 480 MiB | Global hard exact983,040×512-byte aggregate, independently tested from the count bound |
| `records.private_transcript_search_enabled` | explicit local-desktop opt-in | Exact profiled provider/target/namespace only; `endpoint_unscoped` is unsupported and no caller path/root/parser is accepted |
| `records.private_transcript_search_documents_per_index` | 10,000 | Global hard exact ConversationKey/source-revision documents; overflow marks coverage partial before another source body read |
| `records.private_transcript_search_source_read_mib` | 5 MiB | Global hard sequential tail read per regular identity-pinned source; only the final200 KiB of normalised text may enter the encrypted index |
| `records.private_transcript_search_index_mib_per_profile_target` | 512 MiB | Global hard encrypted postings/title/cwd/source-locator/snippet/normalised-segment-tail aggregate for one profile/target/namespace index |
| `records.private_transcript_search_index_mib_installation` | 1,024 MiB | Global hard physical/logical reservation inside—not in addition to—the existing2-GiB `account_private_root` class |
| `records.private_transcript_search_refresh_minutes` | 5 minutes minimum | One refresh/profile-target,≤8 installation-wide; source/parser/policy generation change creates a new fenced index generation |
| `records.private_transcript_search_refresh_sources` | 256 | Global hard queued exact source identities/profile-target,≤2 MiB each and≤64 MiB family before source open |
| `records.private_transcript_search_queries_per_surface` | 2 | Global hard local-desktop request buffers;≤32 installation-wide and reconnect inherits none |
| `records.private_transcript_search_query_item_mib` | 2 MiB | Global hard query/ranking/snippet working set,≤64 MiB family and≤30-second deadline |
| `records.private_transcript_search_page` | 20 hits / 80 KiB | Global request-only page; each hit≤4 KiB, snippet≤160 scalars, query2..256 scalars, scan≤10,000 entries and cursor≤512 bytes |
| `records.private_transcript_search_nonterminal_operations_per_scope` | 1 | Exact profile/target/namespace hard bound across enable/rebuild/disable/delete; competing operation ids admit at most one before source/key effect |
| `records.private_transcript_search_nonterminal_operations_installation` | 8 | Global hard operation concurrency; N+1 reads no transcript and changes no key/index/policy |
| `records.private_transcript_search_rich_operation_receipts` | 10,000 | Global hard current+terminal body-free operation records; nonterminal, possible-key-effect and delete-uncertain rows never age out |
| `records.private_transcript_search_operation_receipt_item_kib` | 8 KiB | Complete operation/fingerprint/scope/generations/descriptor-worker-correlation/result metadata only |
| `records.private_transcript_search_operation_receipt_mib` | 64 MiB | Global hard aggregate independently admitting exactly8,192 maximum receipts |
| `records.private_transcript_search_operation_receipt_days` | 180 days | Terminal rich boundary; compaction requires the permanent minimal replay/key-generation/result fence |
| `records.private_transcript_search_minimal_fences` | 1,000,000 | Installation-lifetime hard operation/fingerprint/scope/key-generation/result count; reservation precedes source read or key effect |
| `records.private_transcript_search_minimal_fence_item_bytes` | 512 bytes | Body-free changed-request/replay/key-effect fence |
| `records.private_transcript_search_minimal_fence_mib` | 480 MiB | Global hard byte boundary admitting exactly983,040 maximum fences independently of count |
| `records.historical_conversation_view_buffers_per_surface` | 1 | Exact local Surface/connection/ViewTarget generation hard bound; selection atomically replaces only after new capacity |
| `records.historical_conversation_view_buffers_installation` | 16 | Global hard request-only view/page buffer count; reconnect inherits none |
| `records.historical_conversation_view_buffer_item_mib` | 2 MiB | One bounded encrypted-tail decode/page/outbox working set; no provider read or durable body copy |
| `records.historical_conversation_view_buffer_mib` | 32 MiB | Global hard exact16×2-MiB aggregate |
| `records.historical_conversation_view_deadline_seconds` | 30 seconds | Deadline, Surface/ViewTarget change, disconnect, transfer or gap wipes and releases the buffer |
| `records.private_transcript_search_retention` | while enabled and exact profile/target consent remains | Disable/delete/retire revokes the per-index key before unlink; uncertain physical cleanup retains body-free evidence and provider transcripts are untouched |
| `records.provider_title_observations_per_conversation` | 10 | Global; bounded untrusted requested/effective title observations and rename receipts |
| `records.conversation_rename_intents` | 10,000 | Global hard ExecutionTarget-owned nonterminal+uncompacted terminal bound, one nonterminal per ConversationKey and each≤4 KiB; reserve before provider dispatch |
| `records.conversation_rename_mib` | 32 MiB | Global hard safe intent/receipt aggregate reached by8,192 maximum records; count and bytes saturate independently, title/response beyond declared bounds is absent and N+1 refuses pre-effect |
| `records.conversation_rename_days` | 180 days | Global; terminal richness folds after operation/key/correlation/result replay fences persist; nonterminal/possible-effect evidence never ages out |
| `records.name_facts_per_node` | 10 | Global; newest bounded sanitised source facts/proposal metadata; manual pinned alias remains owner-lifetime |
| `records.name_proposal_days` | 7 days | Global; unapplied proposal metadata expires; raw captured source is never durable |
| `records.model_endpoint_profiles_per_target` | 32 | Global; safe route metadata only; creation is refused at the bound |
| `records.execution_targets` | 256 | Global hard Installation-owned non-deleted catalogue bound; create/adopt preassigns a monotonic non-reused id and N+1 refuses before descriptor/probe/trust effect |
| `records.execution_target_operations` | 10,000 | Global hard nonterminal plus uncompacted terminal create/adopt/probe/trust/bind/retire/delete slots; terminal receipt capacity is reserved before effect |
| `records.execution_target_operation_days` | 180 days | Terminal richness compacts only after target-reference/trust/descriptor/operation replay fences survive; nonterminal/possible-effect evidence never ages out |
| `records.model_discovery_entries_per_profile` | 10,000 | Global; mapped metadata cache; overflow marks discovery partial, raw page remains memory-only |
| `records.model_discovery_cache_minutes` | 15 minutes | Global; expiry becomes stale and never proves absence or changes a running attempt |
| `records.model_endpoint_receipts_per_profile` | 100 | Global; newest redacted validation/launch/switch receipts; active/uncertain evidence is retained |
| `records.active_workspace_onboardings` | 100 | Global hard Installation-stream nonterminal bound; the 101st begin refuses before path, target, repository, network or Workspace effect |
| `records.workspace_onboarding_receipts` | 10,000 | Global; bounded phase/partial/reconcile/publish metadata, never repository/SSH/clone output bodies; terminal capacity is reserved before the first effect |
| `records.workspace_onboarding_receipt_days` | 180 days | Global; terminal unreferenced receipts compact at the boundary while uncertain recovery evidence remains |
| `records.active_repository_publish_intents` | 256 | Global hard ExecutionTarget-owned nonterminal bound, with one per RepositoryId or canonical host/account/destination key; pre-effect reservation includes terminal/journal/correlation/recovery |
| `records.repository_publish_intents` | 10,000 | Global hard nonterminal+uncompacted-terminal count; each safe record≤8 KiB and N+1 performs no host/Git/config/lease effect |
| `records.repository_publish_mib` | 64 MiB | Global hard safe intent/receipt aggregate reached by8,192 maximum records; count and bytes saturate independently, and repository body/diff/credential/provider response body are absent |
| `records.repository_publish_replay_fences` | 100,000 | Installation-lifetime hard minimal operation/destination/object/ref/config/scope/lease/correlation fence count; full capacity refuses pre-effect |
| `records.repository_publish_replay_fence_mib` | 48 MiB | Global hard fence aggregate reached by98,304 maximum fences, each≤512 bytes; count and bytes saturate independently and terminal rich compaction never borrows or deletes this allowance |
| `records.repository_publish_days` | 180 days | Global; terminal richness folds after operation/destination/object/ref/config/scope/lease/correlation fences persist; nonterminal/partial/possible-effect evidence never ages out |
| `records.active_web_preview_load_intents` | 32 | Global hard Workspace-owned nonterminal bound; active/receipt/replay/journal/recovery and ephemeral capacity reserve before DNS or request |
| `records.web_preview_load_receipts` | 10,000 | Global hard rich terminal count, each safe record≤4 KiB and containing only URL hash, redirect identities, policy/correlation and outcome—not URL path, headers or body |
| `records.web_preview_load_replay_fences` | 100,000 | Global hard minimal operation/Node/source/URL-hash/policy/correlation/disposition fences, each≤512 bytes; saturation refuses before DNS/network |
| `records.web_preview_load_mib` | 64 MiB | Global hard active+rich-receipt+minimal-fence aggregate; count and family bytes saturate independently—for example10,000 maximum rich receipts plus51,072 maximum fences reach64 MiB exactly—and every durable record is metadata only |
| `records.web_preview_load_receipt_days` | 30 days | Global; rich terminal receipts compact only behind a minimal replay fence; nonterminal/possible-request evidence never ages out |
| `records.web_preview_states_per_surface` | 1 | Global hard current memory-only state; replacement reserves before atomic swap and failure preserves the old preview |
| `records.web_preview_states_per_client` | 4 | Global hard live count per authenticated connection, reachable through the four-Surface connection cap; N+1 performs no DNS/request/read/render |
| `records.web_preview_states_installation` | 32 | Global hard live aggregate; close/source/view/Node/Surface/connection/renderer invalidation or expiry fences and stops the exact HTTP/renderer correlation, but releases only after proved socket/worker/buffer quiescence; reconnect inherits no view authority |
| `records.web_preview_fetch_correlation_item_kib` | 8 KiB | Global hard exact intent/URL-hash/policy/DNS/socket/renderer generation metadata for one live-or-cleanup fetch; no URL path/header/body |
| `records.web_preview_fetch_correlation_kib` | 256 KiB | Global hard exact32×8-KiB aggregate; releases only with socket/worker/buffer quiescence and otherwise transfers its existing charge to cleanup |
| `records.web_preview_body_item_mib` | 16 MiB | Global hard decoded memory-only item after≤8-MiB transferred,≤20:1 and safe-MIME validation |
| `records.web_preview_body_mib` | 256 MiB | Global hard body aggregate within `runtime.turn_variable_rss_mib`; bytes never persist |
| `records.web_preview_renderers` | 8 | Global hard live inert renderer count; admission reserves before launch and N+1 renders nothing |
| `records.web_preview_renderer_item_mib` | 64 MiB | Global hard per-renderer memory bound |
| `records.web_preview_renderer_mib` | 256 MiB | Global hard renderer aggregate within `runtime.turn_variable_rss_mib`; exit releases all owned preview states |
| `records.web_preview_idle_minutes` | 15 minutes | Global exact idle expiry; close/source/ViewTarget/Node/Surface/connection/renderer loss requests earlier stop but retains every charge until proved quiescent or process death proves OS reclamation |
| `records.active_browser_node_creation_intents` | 512 | Global hard Workspace-owned nonterminal bound; active/terminal/replay/journal/recovery capacity reserves before graph/renderer/network effect |
| `records.browser_node_creation_intents` | 10,000 | Global hard nonterminal+uncompacted-terminal bound; each safe record≤4 KiB and one nonterminal intent owns one preassigned Node |
| `records.browser_node_creation_mib` | 32 MiB | Global hard safe intent/receipt aggregate reached by8,192 maximum records; count and bytes saturate independently and source bodies/DOM/page bytes are excluded |
| `records.browser_node_creation_replay_fences` | 100,000 | Installation-lifetime hard minimal operation/fingerprint/Node/tombstone/correlation fence count; saturation refuses pre-effect |
| `records.browser_node_creation_replay_fence_mib` | 48 MiB | Global hard fence aggregate reached by98,304 maximum fences, each≤512 bytes; count and bytes saturate independently and never contain source/body/URL content |
| `records.browser_node_creation_days` | 180 days | Global; terminal richness compacts after replay/Node/tombstone/correlation fences persist; nonterminal/possible-effect evidence never ages out |
| `records.active_browser_navigation_intents` | 256 | Global hard, with one nonterminal intent per exact Browser Node/partition generation; N+1 has no renderer/network/history/stop effect |
| `records.web_browser_navigation_receipts_per_node` | 100 | Global hard rich-terminal bound per Node; safe origin/file/history identity, policy generation and outcome only |
| `records.web_browser_navigation_receipts` | 10,000 | Global hard rich terminal count; active/terminal/journal/recovery capacity reserves before dispatch |
| `records.web_browser_navigation_replay_fences` | 100,000 | Global hard minimal operation/Node-generation/fingerprint/disposition/correlation fences; saturation refuses pre-effect |
| `records.web_browser_navigation_mib` | 128 MiB | Global hard active+rich-receipt+minimal-fence aggregate; every rich/intent record≤8 KiB, every minimal fence is bounded inside the same pool and DOM/form/cookie bodies are absent |
| `records.web_browser_navigation_receipt_days` | 30 days | Global; rich terminal receipts compact only behind a minimal replay fence; nonterminal/reconcile evidence never ages out |
| `records.web_browser_history_entries_per_node` | 100 | Global hard memory-only count; bounded eviction never removes current or intent-targeted entries |
| `records.web_browser_history_entries` | 10,000 | Global hard memory-only installation aggregate; N+1 refuses before load when safe eviction is impossible |
| `records.web_browser_url_kib` | 4 KiB / 2,048 scalars | Global hard canonical private URL bound for direct, popup, link and redirect targets; oversize input refuses before dispatch and oversize redirect stops before follow/history commit |
| `records.web_browser_title_kib` | 1 KiB / 256 scalars | Global hard control-stripped history title; oversize is omitted with a typed reason, never misleadingly truncated |
| `records.web_browser_history_entry_kib` | 8 KiB | Global hard encoded memory-only entry including URL, title-or-omission, origin/TLS/load identity |
| `records.web_browser_history_mib` | 64 MiB | Global hard memory-only aggregate reached by8,192 maximum entries; count and bytes saturate independently, also charge shared RSS and reserve before load |
| `records.web_browser_redirects` | 10 | Global hard per navigation; an eleventh or oversize Location stops before follow/history commit and only bounded identity hashes enter the receipt |
| `records.active_browser_renderers` | 8 | Global hard live renderer count; admission reserves before launch and parked Nodes remain inert/unloaded |
| `records.browser_renderer_item_mib` | 256 MiB | Global hard memory bound for one renderer context |
| `records.browser_renderer_mib` | 1,024 MiB | Global hard live-or-cleanup-pending aggregate across renderer contexts; N+1 launches/loads nothing, owner loss transfers the original charge to ProcessCleanupCharge until quiescence/OS reclamation and this family also charges the shared ephemeral-memory pool |
| `records.browser_partition_item_mib` | 128 MiB | Global hard memory-only per-Node site-data/page-cache bound; no ambient profile or persistent fallback |
| `records.browser_partition_mib` | 512 MiB | Global hard memory-only aggregate; clear/Node close/scope/renderer-process loss destroys bytes, reconnect inherits none and this family also charges the shared ephemeral-memory pool |
| `records.browser_pages_installation` | 16 | Global hard current+pending BrowserPage metadata across eight active renderers; a third generation per renderer dispatches nothing |
| `records.browser_page_item_kib` | 32 KiB | Global hard safe navigation/origin/TLS/load/permission/correlation metadata; DOM/script/storage/body bytes remain inside BrowserPartition/renderer caps |
| `records.browser_page_kib` | 512 KiB | Global hard exact16×32-KiB metadata aggregate, also charged to shared variable RSS |
| `records.browser_memory_saver_states` | 10,000 | Global hard memory-only count with at most one exact policy-generation state per Browser Node; N+1 leaves the renderer running |
| `records.browser_memory_saver_state_kib` | 4 KiB | Global hard safe owner/policy/address/state/history-loss metadata; no page, DOM, form, POST, cookie, credential, storage or history body |
| `records.browser_memory_saver_state_mib` | 32 MiB | Global hard family aggregate reached by8,192 maximum-size states independently of count and also charged to shared variable RSS |
| `records.browser_memory_saver_hidden_minutes` | 5 minutes | Exact minimum continuous hidden interval; any visible or excluded state resets eligibility and restart/reconnect never resumes the timer or loads |
| `records.hidden_optional_control_ids` | 11 | Global hard closed ids inside one existing≤256-byte appearance setting value; unknown/duplicate ids are ignored fail-visible and no action history is retained |
| `records.browser_local_snapshots` | 32 | Global hard memory-only live bound; exact count and bytes reserve before descriptor read and N+1 reads nothing |
| `records.browser_local_snapshot_mib` | 256 MiB | Global hard family aggregate within `runtime.turn_variable_rss_mib`; every discard edge atomically releases both charges and bytes never survive navigation/stop/Node/scope/owner/process loss or crash |
| `records.browser_local_snapshot_item_mib` | 8 MiB | Global hard per-snapshot bound; oversize refuses before descriptor read |
| `records.active_browser_download_quarantines` | 32 | Global hard Workspace-owned nonterminal bound; active/terminal/recovery and shared-byte capacity reserve before response body byte one |
| `records.browser_download_quarantine_item_mib` | 2,048 MiB | Global hard per-quarantine bound; the sole aggregate authority is `records.transfer_temp_mib`, and handoff transfers its charge atomically without copy/redownload |
| `records.browser_download_quarantine_minutes` | 30 minutes | Global; reserved/receiving/sealed state expires from created_at and deletes only after exact no-ticket/no-open-handle proof; uncertainty retains its charge/evidence |
| `records.browser_download_quarantine_receipts` | 10,000 | Global redacted terminal state/response/hash/ownership evidence; payload/path bytes excluded and terminal capacity reserves before body byte one |
| `records.browser_download_quarantine_receipt_days` | 30 days | Global; terminal richness compacts only after descriptor ownership, ticket and operation-replay fences remain; nonterminal/uncertain evidence never ages out |
| `records.open_file_snapshots_per_client` | 16 | Global hard live per authenticated connection; the seventeenth reserves/reads nothing |
| `records.open_file_snapshots_installation` | 128 | Global hard live aggregate; N+1 refuses before descriptor read without evicting another editor |
| `records.file_snapshot_mib` | 8 MiB | Global hard per memory-only decoded snapshot; larger files require an external tool |
| `records.file_snapshot_total_mib` | 1,024 MiB | Global hard family aggregate within `runtime.turn_variable_rss_mib`; count+family+shared bytes reserve atomically before descriptor read |
| `records.file_snapshot_idle_minutes` | 60 minutes | Global; exact activity refreshes, while close/source-root-target invalidation/Surface or connection loss/expiry releases and reconnect cannot inherit |
| `records.active_file_save_intents` | 256 | Global hard nonterminal bound; intent, temporary-byte and terminal-receipt capacity reserve atomically before byte one and N+1 has no file/temp effect |
| `records.file_save_temp_mib` | 2,048 MiB | Global hard combined owner-only sibling temporary bound; each save≤8 MiB and a temporary remains charged until exact terminal cleanup proof |
| `records.file_save_audit_limit` | 50,000 | Global; newest redacted save/conflict receipts installation-wide |
| `records.file_save_audit_days` | 180 days | Global; terminal richness compacts only after replay/file-generation/after-hash fences persist; nonterminal/possible-replace evidence never ages out |
| `records.live_subscriptions_per_client` | 64 | Global hard memory-only count shared by all seven state-stream, Node-view, resource-inventory, local target-recovery, account-activity, live-notification and WorkItem-activity families; exact duplicate returns its current id |
| `records.live_subscriptions_installation` | 4,096 | Global hard aggregate; changed-key replacement reserves before atomic swap and N+1 registers no producer |
| `records.live_subscription_item_kib` | 4 KiB | Global hard safe metadata per subscription; source bodies and queued payloads are separate |
| `records.live_subscription_mib` | 16 MiB | Global hard metadata aggregate; count+bytes reserve before producer registration |
| `records.live_subscription_queue_events` | 64 | Global hard per-subscription pending events, within≤1 MiB; one terminal gap slot is pre-reserved |
| `records.live_subscription_event_raw_kib` | 180 KiB | Global hard serialized payload for every vNext unsolicited common/Node-view event, with complete encoded frame≤256 KiB; excess consumes the pre-reserved gap and triggers automatic snapshot/read |
| `records.live_subscription_queue_events_per_client` | 256 | Global hard shared pending-event count per authenticated connection, within≤8 MiB |
| `records.live_subscription_queue_events_installation` | 4,096 | Global hard shared pending-event count installation-wide, within≤64 MiB |
| `records.live_subscription_queue_mib` | 64 MiB | Global hard installation aggregate, with per-subscription≤1 MiB and per-connection≤8 MiB; also charges `runtime.turn_variable_rss_mib` |
| `records.directory_entries_per_page` | 2,000 | Global; larger directories require cursor paging and never silently truncate to complete |
| `records.directory_entry_item_kib` | 2 KiB | Global hard safe name/kind/size/time/identity projection; aliases are not followed |
| `records.directory_page_mib` | 4 MiB | Global hard request-only logical page including envelope; body bytes use ChunkedResponseStream/outbox and retain no separate family state |
| `records.directory_scans_per_client` | 16 | Global hard live per authenticated connection; 1,024 installation-wide, with N+1 refusal rather than eviction of a live pin |
| `records.directory_scan_item_kib` | 16 KiB | Global hard pinned target/revision/cursor/sequence metadata, excluding request-only page bytes |
| `records.directory_scan_mib` | 16 MiB | Global hard exact 1,024×16-KiB metadata aggregate; count and bytes reserve before backend listing |
| `records.directory_scan_idle_seconds` | 60 seconds | Final page/close/invalidation/disconnect/TTL releases the revision pin; later continuation is explicitly gapped |
| `records.directory_watches_per_client` | 32 | Global hard live per authenticated connection; N+1 refuses before backend subscription and event bytes additionally charge the shared live-subscription queue/shared-memory pools |
| `records.directory_watches_installation` | 2,048 | Global hard live aggregate; unwatch, terminal gap/invalidation or connection loss releases count+queue charge, and reconnect cannot inherit |
| `records.directory_watch_item_kib` | 8 KiB | Global hard owner/revision/cursor/gap metadata excluding events, which charge the shared subscription queues |
| `records.directory_watch_mib` | 16 MiB | Global hard exact 2,048×8-KiB metadata aggregate |
| `records.commit_graph_nodes_per_page` | 500 | Global request-only; one node≤2 KiB, page≤1 MiB, traversal scans≤10,000 object ids and reports a gap beyond it |
| `records.commit_changed_files_per_page` | 1,000 | Global request-only; one row≤2 KiB, page≤2 MiB and exact≤512-byte authenticated cursor paging retains no server object |
| `records.text_search_matches` | 10,000 | Global observed-match limit per surface/source revision; no result set is retained and one request-only page is≤200 matches/200 KiB |
| `records.text_search_sessions_per_surface` | 8 | Global hard live per Surface; 512 installation-wide, with N+1 refusal and no eviction of another live search |
| `records.text_search_session_item_kib` | 16 KiB | Global hard query/source/cursor/count metadata including query≤4 KiB, excluding request-only result pages |
| `records.text_search_session_mib` | 8 MiB | Global hard exact 512×16-KiB metadata aggregate |
| `records.text_search_match_item_kib` | 1 KiB | Global hard request-only match identity/range/context projection |
| `records.text_search_idle_minutes` | 15 minutes | Close/source invalidation/Surface loss/TTL releases results and later movement is stale, never false no-match |
| `records.media_import_mib` | 256 MiB | Global hard per item/source snapshot; oversize refuses before copy/decode |
| `records.active_media_imports` | 32 | Global hard installation-wide nonterminal bound; active+terminal+recovery and declared physical bytes reserve before descriptor read or first chunk |
| `records.media_blob_mib_per_workspace` | 10,240 MiB | Global or Workspace override; one physical-byte pool shared by active import temporaries and committed refcounted blobs, with zero-copy ownership transfer/no double count |
| `records.media_import_receipts` | 10,000 | Global terminal safe descriptor/hash/state receipts; active imports are separately capped at 32 installation-wide |
| `records.media_import_receipt_days` | 30 days | Global; terminal richness compacts only after Node/blob/operation/refcount replay fences persist; nonterminal/reconcile-required evidence never ages out |
| `records.media_playback_per_surface` | 1 | Global hard current memory-only state; same-Surface source replacement reserves before atomic swap and failure preserves the old state |
| `records.media_playback_per_client` | 4 | Global hard live count per authenticated connection, reachable through the four-Surface connection cap; N+1 reads/spawns/decodes nothing |
| `records.media_playback_installation` | 32 | Global hard live-or-cleanup-pending aggregate; logical stop/source/selection/Node/Surface/connection invalidation fences and requests termination but releases only after decoder descriptor/process/thread/shared-buffer quiescence or OS-reclamation proof; reconnect inherits no authority |
| `records.media_playback_state_kib` | 32 KiB | Global hard encoded state item; count+state bytes reserve before begin/replacement and oversize preserves the current state |
| `records.media_playback_codec_bytes` | 64 ASCII bytes | Global hard each codec/container identifier; arbitrary decoder/provider text is never retained |
| `records.media_playback_caption_tracks` | 64 | Global hard per state; each id≤64 bytes/32 scalars, normalised BCP-47 language≤35 ASCII bytes, kind=`subtitles|captions|descriptions` and inert label≤128 bytes/64 scalars |
| `records.media_playback_decoder_item_mib` | 64 MiB | Global hard per-playback decoder/frame/cache working set; ended/error fences and requests termination, then releases only after the same decoder quiescence/OS-reclamation proof as the installation slot |
| `records.media_playback_decoder_mib` | 512 MiB | Global hard family aggregate within `runtime.turn_variable_rss_mib`; both pools reserve before decoder spawn/read |
| `records.repository_host_profiles_per_target` | 64 | Global hard bound; new create/adopt refuses before authentication effect |
| `records.repository_host_active_grants_per_profile` | 128 | Global hard active bound; N+1 grant refuses before authority is issued and active grants never compact |
| `records.repository_host_terminal_grants` | 100,000 | Global hard redacted terminal grant count; active-to-terminal reserves capacity and ids never reactivate |
| `records.repository_host_terminal_grant_mib` | 256 MiB | Global hard terminal grant aggregate, each safe receipt≤4 KiB and never secret values |
| `records.repository_host_terminal_grant_days` | 180 days | Global; rich terminal grants fold only after non-reused id/generation/scope/profile replay high-water remains |
| `records.repository_host_receipts_per_profile` | 100 | Global hard newest safe profile validation/control presentation receipts, never secret values |
| `records.repository_host_receipt_days` | 180 days | Global; rich receipts compact after the boundary only when the independent credential-intent/grant replay fences remain |
| `records.repository_host_credential_intents` | 10,000 | Global hard nonterminal+uncompacted terminal authenticate/rotate bound, one nonterminal per profile and each≤4 KiB; reserve pre-effect |
| `records.repository_host_credential_mib` | 32 MiB | Global hard safe intent/receipt aggregate reached by8,192 maximum records; count and bytes saturate independently and N+1 refuses before broker/provider/profile/grant effect |
| `records.repository_host_credential_days` | 180 days | Global; terminal richness folds after operation/profile/host/account/credential/grant/correlation fences remain; possible-effect evidence never ages out |
| `records.checkout_fences` | 100,000 | Global hard Installation bound across reserved, live, uncertain and uncompacted terminal writer/worktree fences; a slot is reserved before any worktree, writer or lock effect |
| `records.checkout_fence_mib` | 64 MiB | Global hard encoded registry bound; N+1 byte refuses before effect and nonterminal/possible-effect evidence never compacts |
| `records.checkout_lock_inodes` | 100,000 | Global hard Turn-owned checkout-lock bound; removal requires a durable released fence plus exact ownerless exclusive-lock and fresh no-writer proof |
| `records.checkout_fence_days` | 180 days | Global; only rich released metadata compacts after replay/reference/non-reuse fences remain; fence ids and lease generations never reuse |
| `records.repository_mutation_intents` | 10,000 | Global hard ExecutionTarget-owned nonterminal+uncompacted-terminal count; pre-effect reservation includes receipt/journal/recovery and N+1 performs no Git/provider effect |
| `records.repository_mutation_mib` | 256 MiB | Global hard encoded intent bound, each≤32 KiB; nonterminal/possible-effect evidence never compacts |
| `records.repository_mutation_days` | 180 days | Global; terminal richness folds only after replay, object/ref non-substitution, CheckoutScope/lease and remote-correlation fences remain |
| `records.commit_proposal_provider_profiles` | 64 | Global hard installation bound; N+1 create/adopt refuses before provider validation or current-state change |
| `records.commit_proposal_provider_revisions` | 32 | Global current plus 31 historical revisions per profile; referenced revisions do not compact and an update at the bound refuses |
| `records.commit_proposal_attempts` | 10,000 | Global hard installation-wide nonterminal+terminal bound; a terminal slot is reserved before helper/broker dispatch and N+1 generation refuses before effect |
| `records.commit_proposal_sandbox_helpers` | 2 | Global hard concurrent executable-helper bound; a third Attempt remains admitted/queued but spawns nothing until a slot is reserved |
| `records.commit_proposal_sandbox_helper_item_mib` | 512 MiB | Global hard RSS reservation per helper, also charged to `runtime.turn_variable_rss_mib` before spawn |
| `records.commit_proposal_sandbox_helper_mib` | 1,024 MiB | Global hard live-or-cleanup-pending helper aggregate; hung termination transfers the same charge to ProcessCleanupCharge until OS reclamation |
| `records.commit_proposal_attempt_days` | 30 days | Global; terminal Attempt metadata compacts only after its Proposal is terminal and minimal replay proof is durable; nonterminal Attempts and terminal Attempts whose Proposal is nonterminal never age out |
| `records.commit_proposals_per_workspace` | 1,000 | Global hard terminal+ready bound; generation input diff≤128 KiB is memory-only and draft≤8 KiB |
| `records.commit_proposal_days` | 7 days | Global; unpinned terminal/ready proposal metadata/draft expires without repository effect |
| `records.active_transfer_tickets` | 32 | Global hard admission bound; N+1 refuses before allocating temp bytes or network transfer |
| `records.transfer_temp_mib` | 4,096 MiB | Global hard owner-only physical-byte bound shared by active tickets and Browser quarantines; preparation/body admission reserves capacity and ownership handoff never double-counts |
| `records.transfer_receipts` | 10,000 | Global terminal ticket/chunk-hash/outcome receipts; file bodies excluded |
| `records.transfer_receipt_days` | 30 days | Global; terminal receipts compact after destination/replay evidence is safe |
| `records.content_projections_per_surface` | 1 | Global hard live bound; successful set atomically replaces that Surface's old projection and failure preserves it |
| `records.content_projections_per_client` | 4 | Global hard live memory-only bound per authenticated connection, reachable through the four-Surface connection cap; N+1 refuses without eviction |
| `records.content_projections_installation` | 64 | Global hard live aggregate, reachable through the 64-Surface installation cap; clear/source invalidation/Surface or connection loss releases and reconnect cannot inherit |
| `records.content_projection_item_mib` | 2 MiB | Global hard source/body item bound; oversize is explicit before replacement |
| `records.content_projection_mib` | 128 MiB | Global hard family aggregate within `runtime.turn_variable_rss_mib`, exactly reachable by 64×2-MiB projections; replacement reserves both pools before swap and projection bytes never persist |
| `records.surfaces_per_connection` | 4 | Global hard active count; opening a fifth mints nothing and transfers no ownership |
| `records.surface_connection_binding_item_kib` | 4 KiB | Global hard authenticated connection-generation/Surface/owner/revision authority metadata, no view body |
| `records.surface_connection_binding_kib` | 256 KiB | Global hard exact64×4-KiB active-binding aggregate; disconnect atomically revokes/releases it before dormancy |
| `records.surfaces_per_owner` | 8 | Global hard live+dormant count for one daemon-derived local or remote owner |
| `records.surfaces_installation` | 64 | Global hard live+dormant aggregate; count+bytes+nonreuse high-water reserve before mint/resume |
| `records.surface_state_item_kib` | 256 KiB | Global hard encoded record bound including selected/expanded/manual-order/filters/visibility/anchor/history references |
| `records.surface_state_mib` | 16 MiB | Global hard encoded aggregate; N+1 changes no connection, selection, history or temporary Pane |
| `records.surface_expanded_keys` | 2,000 exceptions | Global hard per Surface over closed collapsed/expanded default; expand-all flips default+clears exceptions without enumerating keys, while exceptions are unique, authorised and stale keys prune on hydration |
| `records.surface_manual_order_keys` | 2,000 | Global hard per Surface; duplicate/unknown/cross-scope keys refuse before state change |
| `records.surface_filters` | 32 | Global hard per Surface, each closed filter≤256 encoded bytes and no transient search query persists |
| `records.surface_history_workspaces` | 256 | Global hard nonempty Workspace PresentationHistory partitions indexed per Surface; N+1 applies navigation but returns history_not_recorded after bounded eligible compaction |
| `records.surface_dormant_days` | 30 days | Global; exact resume refreshes only its own deadline, while retire/owner deletion/expiry releases state+history after monotonic nonreuse high-water persists |
| `records.command_catalogue_entries` | 10,000 | Global built-in, signed-extension or foreground-validated local-operator stable entries; page/search returns at most 200/1 MiB and repository/import/process output cannot add one |
| `records.command_catalogue_scans_per_client` | 8 | Global hard live per authenticated connection; 512 installation-wide, N+1 refuses without dropping a current evaluation pin |
| `records.command_catalogue_scan_item_kib` | 32 KiB | Global hard pinned evaluation-scope/watermark/catalogue-revision/cursor metadata; request-only page/search bodies are excluded |
| `records.command_catalogue_scan_mib` | 16 MiB | Global hard exact 512×32-KiB metadata aggregate; count+bytes reserve before evaluation |
| `records.command_catalogue_scan_idle_seconds` | 30 seconds | Final page/close/context invalidation/disconnect/TTL releases the pin; later continuation reports a gap |
| `records.command_shortcut_bindings` | 20,000 | Global hard active/conflict/revoked binding bound; N+1 local admission refuses and no conflict is silently activated |
| `records.signing_key_epochs_per_domain` | 256 | Global hard per each of five domain-disjoint stores; active/retired/revoked public metadata and terminal revocation fences never compact or reactivate, and N+1 rotation refuses before admission |
| `records.signing_audience_high_water_per_domain` | 4,096 | Global hard per store; each exact audience retains monotonic sequence+payload hash across compaction/rotation and a new audience at N+1 refuses before admission |
| `records.active_announcements` | 100 | Global signed safe metadata/text≤16 KiB each; N+1 admission refuses unless atomically superseding the same id/revision lineage, never inventing priority or operational Attention |
| `records.announcement_terminal_receipts` | 10,000 | Global per-derived-operator dismissal/expired/superseded receipts; capacity is reserved before dismissal and N+1 refuses that local mutation without changing announcement state |
| `records.announcement_terminal_receipt_days` | 365 days | Terminal receipts older than the boundary fold only into the exact channel/audience/key-epoch high-water before removal |
| `records.announcement_high_water` | 512 | Installation-lifetime hard one-per-channel/audience accepted epoch/revision fences; trusted epoch advance replaces its predecessor atomically, and a new scope at N+1 is refused before feed admission |
| `records.current_update_intents` | 1 | Global hard Installation-stream bound including discovery; same-query retry is lookup-only and a different query refuses before network until the current terminal intent owns no package bytes and is atomically replaced |
| `records.update_package_storage_mib` | 2,048 MiB | Global hard combined bound for the one current download-temporary/downloaded/verified/staged allocation; declared size is reserved before the first byte and state transition/rename cannot duplicate the allowance |
| `records.staged_update_packages` | 1 | Global; this is the staged state of the same current ≤2-GiB allocation, not a second allocation; another stage/intent refuses before write or explicitly discards with proved byte absence |
| `records.update_receipts` | 100 | Global safe manifest/state/apply/rollback evidence; capacity is reserved before discovery, older terminal richness folds only after independent signing/anti-rollback/minimal operation replay fences are durable, and package bytes/credentials are excluded |
| `records.work_item_activity_events_per_item` | 10,000 | Global; safe delta≤8 KiB, with stable checkpoint/gap before terminal compaction |
| `records.work_item_activity_mib_per_workspace` | 64 MiB | Global or Workspace override; a Turn mutation reserves event capacity before effect, external overflow marks activity coverage gapped |
| `records.work_item_activity_page_rows` | 200 | Global request-only page count; count saturation uses small events independently of bytes |
| `records.work_item_activity_page_mib` | 1 MiB | Global hard logical page; at≤8 KiB/event exactly128 maximum events reach bytes before the200-row cap |
| `records.work_item_activity_cursor_bytes` | 512 bytes | Global hard authenticated WorkItem/revision/checkpoint/order cursor; stale/oversize yields a gap |
| `records.presentation_history_per_workspace` | 200 | Global hard total across owner/surface/session-partitioned undo+redo stacks; compaction checkpoints presentation only and never transfers an entry to another owner |
| `records.account_profiles_per_provider_host` | 32 | Global; safe profile metadata; creation/adoption is refused at the bound |
| `records.account_validation_receipts_per_profile` | 100 | Global; newest safe receipts |
| `records.account_validation_receipt_days` | 30 days | Global; a validation receipt is removed when older even if fewer than 100 remain |
| `records.account_profile_private_root_mib` | 64 MiB | Global hard per Turn-created owner-only auth/config root; broker writes reserve remaining bytes before launch and oversize refuses |
| `records.account_profile_private_roots_mib` | 2,048 MiB | Global hard aggregate across Turn-created auth/config roots; N+1 bytes refuse before broker/Browser/provider/root effect |
| `records.account_authentication_intents` | 10,000 | Global hard nonterminal+uncompacted terminal bound, one nonterminal per profile and each≤4 KiB; reserve before external auth launch |
| `records.account_authentication_mib` | 32 MiB | Global hard safe intent/receipt aggregate reached by8,192 maximum records; count and bytes saturate independently, callback/credential bytes are excluded and N+1 refuses prelaunch |
| `records.account_authentication_days` | 180 days | Global; terminal richness folds only after operation/profile/target/credential/correlation fences remain; possible-effect evidence never ages out |
| `records.remote_active_invitations` | 128 | Global; unconsumed prepared/active invitations across all roles; creation refuses at the bound and terminal metadata moves to the audit bound |
| `records.remote_invitation_ttl_seconds` | 600 seconds | Hard maximum from activation; shorter operator expiry wins, and prepared records invalidate on restart |
| `records.remote_clients` | 128 | Global hard nonterminal-client bound across full/headless/Companion roles; new pairing refuses before secret creation and terminal metadata moves to the audit bound |
| `records.remote_operator_sessions` | 64 | Global; simultaneous authenticated full/headless/Companion sessions; new or reconnect sessions are refused at the bound |
| `records.remote_sessions_per_client` | 1 | Global; at most one negotiating/active session for one RemoteClientId; terminal reconnect predecessors do not count but retain audit evidence |
| `records.remote_handshake_ttl_seconds` | 60 seconds | Hard maximum for redemption/open device-key challenge and manifest negotiation; timeout terminalises the session and disconnects the client |
| `records.remote_session_ttl_seconds` | 86,400 seconds | Hard maximum active session lifetime before a fresh device-key-authenticated open; a shorter invitation/client expiry wins |
| `records.remote_operator_audit_limit` | 50,000 | Global; newest redacted remote scope/action/revocation records installation-wide |
| `records.remote_operator_audit_days` | 180 days | Global; records are compacted when older even if below the count bound |
| `records.remote_redemption_receipts` | 10,000 | Global; newest redacted redemption/session-open ids, identity reservations and outcomes; no invitation secret or session material |
| `records.remote_redemption_receipt_days` | 180 days | Global; terminal receipts compact at the boundary after invitation/client/session audit dependencies are terminal |
| `records.companion_action_ttl_seconds` | 30 seconds | Hard maximum from issue to pre-effect validation; shorter session/grant expiry wins and expired actions never dispatch |
| `records.companion_nonterminal_action_intents` | 10,000 | Global hard admission bound; N+1 refuses before canonical dispatch and raises one bounded system Attention until reconciliation frees capacity |
| `records.companion_action_receipts` | 50,000 | Global terminal bound; reserve capacity before dispatch, newest redacted action-to-canonical-receipt mappings only, no free text/prompt/terminal/permission body |
| `records.companion_action_receipt_days` | 180 days | Global; terminal mappings compact at the boundary after linked canonical receipt retention permits it |
| `records.remote_presence_entries` | 128 | Global live bound; newest revision per active client/session/surface only, 30-second hard expiry, memory-only and never written to store/journal/export/diagnostics |
| `records.remote_presence_item_kib` | 4 KiB | Global hard safe owner/scope/selected-key/state/revision metadata; no body, token or authority |
| `records.remote_presence_kib` | 512 KiB | Global hard exact128×4-KiB aggregate, also charged to shared variable RSS before accepting an update |
| `records.presence_chat_messages_per_owner` | 1 | Global hard current message per exact active full-GUI client/session/Workspace/Surface/connection+authorised ViewTarget/revision; replacement reserves before swap |
| `records.presence_chat_messages` | 128 | Global hard live memory-only aggregate; N+1 projects nothing and cannot block canonical Presence/Attention |
| `records.presence_chat_body_bytes` | 512 bytes / 256 scalars | Global hard sanitised single-paragraph UTF-8 body inside the complete item |
| `records.presence_chat_item_kib` | 1 KiB | Global hard owner/revision/expiry/body item; no token, authority, context or command fields |
| `records.presence_chat_kib` | 128 KiB | Global hard exact128×1-KiB aggregate inside shared variable RSS |
| `records.presence_chat_ttl_seconds` | 30 seconds | Replacement/retract/disconnect/revoke/scope/Surface/connection/ViewTarget-revision loss or TTL emits a live tombstone then releases; no persistence/offline replay |
| `records.presence_chat_rate_per_10_seconds` | 4 | Global hard per client with≥500 ms between accepted sends; excess has zero peer projection |
| `records.remote_replay_nonces` | 10,000 | Global; hashed nonce/id metadata only, expires after 10 minutes; count saturation uses small records independently of bytes and overflow refuses a new remote mutation |
| `records.remote_replay_nonce_item_bytes` | 256 bytes | Global hard hash/session/generation/expiry metadata item; no request body/token |
| `records.remote_replay_nonce_mib` | 2 MiB | Global hard byte aggregate reached by8,192 maximum records; count and bytes are independently saturated and both reserve before remote mutation |
| `records.remote_permission_grant_ttl_seconds` | 120 seconds | Hard maximum from issue; shorter interaction/client/session expiry wins |
| `records.remote_permission_active_grants_per_client` | 32 | Global; at most one active grant per exact interaction/client generation and 256 installation-wide; overflow refuses issue |
| `records.remote_permission_terminal_grants` | 10,000 | Global; newest redacted consumed/revoked/expired/invalidated metadata; no capability or prompt body |
| `records.remote_permission_terminal_grant_days` | 30 days | Global; compact only after linked response/reconciliation evidence is terminal and the anti-replay window ended |
| `records.permission_response_nonterminal_claims` | 10,000 | Global hard admission bound across local typed, remote typed and verified-local-PTY paths; N+1 refuses before claim/effect and raises bounded system Attention |
| `records.permission_response_terminal_receipts` | 10,000 | Global hard terminal receipt/claim bound; terminal capacity is reserved before effect and contains only redacted dispatch/evidence/claim metadata, never prompt/frame/encoded-byte/credential bodies |
| `records.permission_response_receipt_days` | 180 days | Global; terminal receipt/claim metadata compacts only after the linked interaction is terminal and anti-replay boundary elapsed; if no terminal slot can be reserved, a new response refuses before claim/effect |
| `records.pending_interactions_per_attempt` | 8 | Global hard Workspace-owned nonterminal bound per current RuntimeAttempt; all eight plus one Attention observability-gap slot are reserved before spawn and a ninth cannot overwrite a live prompt |
| `records.pending_interactions` | 100,000 / 768 MiB | Global hard installation-wide nonterminal count/byte bounds, each safe metadata record≤8 KiB;80,000 slots/625 MiB are dedicated to the eight-token reservation for10,000 live RuntimeAttempts and20,000 count slots/143 MiB are headroom (18,304 full-size records); reservation becomes the materialised record without double-charge, and count/bytes saturate independently |
| `records.pending_interaction_terminal_receipts` | 100,000 | Global hard redacted terminal interaction/attempt/input-route/option/claim metadata count, each≤4 KiB; prompt and credential bodies are excluded |
| `records.pending_interaction_terminal_receipt_mib` | 384 MiB | Global hard encoded terminal-receipt aggregate reached by98,304 maximum records; count and bytes saturate independently and terminal capacity reserves with interaction admission |
| `records.pending_interaction_terminal_days` | 180 days | Rich terminal metadata compacts only after non-reused id, route/option/claim and replay fences remain; nonterminal/claimed/submitted/possible-effect records never age out |
| `records.attention_active_entries` | 200,000 | Global hard Installation Queue bound for unresolved/snoozed/dismissible≤4-KiB entries; 90,000 are dedicated to eight normal plus one gap route for each of 10,000 live RuntimeAttempts and 110,000 remain for other declared producers/admission races; each producer pre-reserves and active entries never compact |
| `records.attention_active_mib` | 768 MiB | Global hard encoded active-entry bound split351.5625 MiB attempt-dedicated/416.4375 MiB other-producer headroom; count and bytes saturate independently (196,608 maximum-size entries fill bytes), reservation becomes the materialised entry without double-charge, exact dedup alone replaces in place and an unpausable producer overrun uses its separately reserved actionable gap |
| `records.attention_terminal_receipts` | 200,000 | Global redacted route/mutation terminal receipt slots; one is reserved with every active-entry admission, so the 200,000th active entry is reachable and the next refuses before effect |
| `records.attention_terminal_receipt_mib` | 768 MiB | Global hard encoded terminal-receipt reservation+materialised bound at≤4 KiB each; count and bytes saturate independently and materialisation consumes the reserved slot/bytes without double-charge |
| `records.attention_terminal_receipt_days` | 180 days | Terminal richness compacts only after subject resolution/tombstone, read/dismiss semantics and replay fences are durable |
| `records.notification_endpoints` | 64 | Global hard reserved/active/retired endpoint bound; preassigned ids are monotonic/non-reused and N+1 pairing refuses before peer effect |
| `records.notification_grants_per_endpoint` | 32 | Global hard proposed+active bound per endpoint; one equivalent scope active and N+1 issue refuses before authority/peer effect |
| `records.notification_grants` | 2,048 | Global hard installation proposed+active bound; terminal ids fold only after endpoint generation/scope/high-water fences persist |
| `records.notification_control_records` | 10,000 | Global hard pair/issue/revoke/retire/delete nonterminal+uncompacted-terminal bound, each≤4 KiB; reserve before peer/gateway effect |
| `records.notification_control_mib` | 32 MiB | Global hard encoded control bound reached by8,192 maximum records; count and bytes saturate independently, N+1 refuses pre-effect and nonterminal/possible-effect evidence never ages out |
| `records.notification_control_days` | 180 days | Global; terminal richness folds after operation/correlation/id/generation/scope replay fences remain |
| `records.notification_pairing_ttl_seconds` | 600 seconds | Global hard prepared expiry and awaiting-peer deadline; after dispatch the deadline enters reconcile-required, while proved no-effect terminal coupling releases live endpoint/grant capacity |
| `records.notification_outbox_deliveries` | 10,000 | Global hard live encrypted-delivery count; terminal capacity reserves with eligibility and overflow emits a gap without Attention mutation |
| `records.notification_ciphertext_kib` | 16 KiB | Global hard per-delivery encrypted payload bound; plaintext bodies are never persisted |
| `records.notification_encrypted_outbox_mib` | 16 MiB | Global hard encrypted minimal payload aggregate; N+1 refuses eligibility rather than silently dropping current high-priority work |
| `records.notification_delivery_hours` | 24 hours | Global hard pending encrypted-delivery lifetime |
| `records.notification_delivery_attempts` | 8 | Global hard total gateway submissions per NotificationDeliveryId including the first; retry backoff is capped at 15 minutes and a ninth is unrepresentable |
| `records.notification_delivery_audit` | 100,000 | Global hard terminal receipt count, each≤4 KiB and non-secret state/endpoint/grant/collapse/retry/hash metadata only |
| `records.notification_delivery_audit_mib` | 256 MiB | Global hard terminal audit aggregate; receipt capacity reserves before delivery eligibility |
| `records.notification_delivery_audit_days` | 7 days | Global; folding retains delivery/endpoint/grant/collapse/retry/replay fences |
| `records.companion_activity_items_per_profile` | 1,000 | Global; newest bounded safe activity metadata per AccountProfile; overflow requires a gap/resync marker |
| `records.companion_activity_days` | 30 days | Global; handled/unhandled provider activity metadata older than the boundary compacts without changing Attention |
| `records.sync_journal_days` | 30 days | Global; earlier cursors must resnapshot and cannot replay mutations |
| `records.sync_journal_mib_per_workspace` | 256 MiB | Global; compaction publishes a new minimum accepted revision before removing segments |
| `records.sync_journal_mib_installation` | 512 MiB | Global hard Installation-stream cap; mutation/barrier bytes reserve before effect and external overflow emits current-state+gap |
| `records.sync_journal_mib_per_target` | 128 MiB | Global hard cap per ExecutionTarget stream; a slow cursor resnapshots and cannot pin segments or turn observation loss into empty state |
| `records.sync_journal_mib_total` | 4,096 MiB | Global hard aggregate across every StateStreamKey; compaction/refusal is atomic for a cross-stream barrier and preserves current/nonterminal/replay/deletion/barrier evidence |
| `records.client_tombstone_days` | 30 days | Global; compacted deletion is still fenced by minimum revision, non-reused ids and update-never-upserts rules |
| `records.status_event_days` | 7 days | Global; eligible success/reconciled/dismissed rich history compacts behind the exact owner minimum revision, while active warning/error/recovery state never ages out |
| `records.status_events_per_owner` | 1,000 | Global hard per exact Installation/Workspace/ExecutionTarget owner; effect-capable producers reserve a normal plus gap slot and progress replacement/coalescing occurs before insertion |
| `records.status_events_installation` | 100,000 | Global hard all-owner count bound; after eligible compaction N+1 producer admission refuses before effect rather than dropping a persistent error |
| `records.status_event_mib` | 256 MiB | Global hard all-owner encoded bound, each event≤4 KiB; external excess uses the producer's reserved gap and preserves the source receipt/recovery evidence |
| `records.diagnostic_sources` | 4,096 | Global hard exact source high-water rows; source registration reserves before log admission and overflow never blocks runtime/input/Attention |
| `records.diagnostic_source_item_bytes` | 256 bytes | Global hard source identity/clear-sequence/non-replay metadata, never a diagnostic body |
| `records.diagnostic_source_mib` | 1 MiB | Global hard exact4,096×256-byte durable clear-high-water aggregate |
| `records.diagnostic_log_entries` | 2,048 | Global hard current-daemon memory-only structured redacted ring count; overflow advances earliest sequence and coverage becomes gapped |
| `records.diagnostic_log_item_kib` | 4 KiB | Global hard sequence/time/severity/source/text-key/safe-argument/correlation metadata after pre-admission redaction |
| `records.diagnostic_log_mib` | 8 MiB | Global hard exact2,048×4-KiB memory-only ring aggregate inside shared variable RSS |
| `records.diagnostic_log_page_rows` | 256 | Global request-only page bound, each row≤4 KiB/page≤1 MiB and authenticated cursor≤512 bytes |
| `records.diagnostic_log_clear_receipts` | 10,000 | Global hard body-free clear operation/scope/revision/count receipts, each≤4 KiB |
| `records.diagnostic_log_clear_receipt_mib` | 32 MiB | Global hard encoded aggregate reached by8,192 maximum receipts; count and bytes saturate independently |
| `records.diagnostic_log_clear_receipt_days` | 30 days | Rich receipts compact only after the durable source/all clear high-water and operation replay fence remain |
| `records.bug_report_drafts_per_surface` | 1 | Global hard local editable memory-only draft, distinct from LocalInputDraft/Note/diagnostic ring |
| `records.bug_report_drafts_per_client` | 8 | Global hard one per Surface for one LocalClientInstanceId |
| `records.bug_report_drafts_installation` | 64 | Global hard current+stale visible draft count |
| `records.bug_report_draft_item_mib` | 1 MiB | Global hard title/body/inclusion/redaction/system-version manifest and framing; credentials/raw bodies/unauthorised paths are absent |
| `records.bug_report_draft_mib` | 64 MiB | Global hard exact64×1-MiB client-memory aggregate inside shared variable RSS |
| `records.bug_report_draft_minutes` | 30 minutes | Consume/discard/expiry/Surface/window/client loss releases; daemon/log change marks stale and disables reviewed effect without deleting local edits |
| `records.bug_report_review_receipts` | 10,000 | Global hard body-free draft digest/destination/action/subreceipt count, each≤4 KiB |
| `records.bug_report_review_receipt_mib` | 32 MiB | Global hard encoded aggregate reached by8,192 maximum receipts; count and bytes saturate independently |
| `records.bug_report_review_receipt_days` | 30 days | Rich metadata compacts only after exact Browser/File/clipboard operation replay fences remain; no report body is retained |
| `records.document_views_per_surface` | 1 | Global hard memory-only exact-source view; replacement reserves before swap and restore never opens |
| `records.document_views_per_connection` | 4 | Global hard current+cleanup-pending views for one authenticated connection |
| `records.document_views_installation` | 64 | Global hard count; N+1 reads/decodes zero source bytes |
| `records.document_view_state_kib` | 64 KiB | Global hard safe source/view/page/zoom/error metadata; no source or decoded body |
| `records.document_view_state_mib` | 4 MiB | Global hard exact64×64-KiB aggregate |
| `records.document_blob_item_mib` | 256 MiB | Global hard memory-only exact descriptor/revision bytes |
| `records.document_blob_mib` | 512 MiB | Global hard aggregate, also charged to shared variable RSS |
| `records.document_decoders` | 2 | Global hard live-or-cleanup-pending isolated workers |
| `records.document_decoder_item_mib` | 256 MiB | Global hard full decode working-set reservation |
| `records.document_decoder_mib` | 512 MiB | Global hard aggregate, retained through uncertain cleanup |
| `records.document_page_tiles_per_view` | 4 | Global hard decoded visible-cache entries, each≤32 MiB and≤8 million pixels |
| `records.document_page_tiles_installation` | 256 | Global hard count under the independent512-MiB aggregate |
| `records.document_page_cache_mib` | 512 MiB | Global hard memory-only aggregate |
| `records.document_text_index_item_mib` | 16 MiB | Global hard extracted plain-text index per exact source revision |
| `records.document_text_index_mib` | 64 MiB | Global hard memory-only aggregate |
| `records.document_print_intents_nonterminal` | 32 | Global hard and one per view; capacity reserves before spool/native effect |
| `records.document_print_receipts` | 10,000 | Global hard body-free operation/source/layout/printer-correlation records, each≤4 KiB |
| `records.document_print_receipt_mib` | 32 MiB | Global hard aggregate reached by8,192 maximum records |
| `records.document_print_receipt_days` | 30 days | Rich metadata compacts behind permanent operation/result replay proof; ambiguity never auto-retries |
| `records.document_print_spools` | 2 | Global hard live-or-cleanup-pending isolated spools |
| `records.document_print_spool_item_mib` | 64 MiB | Global hard reviewed selected-page subset only |
| `records.document_print_spool_mib` | 128 MiB | Global hard memory-only aggregate, released only after native job/descriptor quiescence |
| `records.terminal_clipboard_gestures_per_surface` | 1 | Global hard local-client memory-only gesture |
| `records.terminal_clipboard_gestures_per_client` | 8 | Global hard across one LocalClientInstanceId |
| `records.terminal_clipboard_gestures_installation` | 64 | Global hard current gesture count |
| `records.terminal_clipboard_gesture_item_kib` | 64 KiB | Global hard copied/pasted text or total path manifest; body is wiped on every terminal edge |
| `records.terminal_clipboard_gesture_mib` | 4 MiB | Global hard exact64×64-KiB memory-only aggregate |
| `records.terminal_clipboard_gesture_seconds` | 30 seconds | Global hard expiry; no reconnect/offline inheritance |
| `records.attention_audio_cues_per_client` | 16 | Global hard queued+playing local derived cues |
| `records.attention_audio_cues_installation` | 128 | Global hard count; stale/duplicate/late admission drops rather than evicts current work |
| `records.attention_audio_cue_item_kib` | 2 KiB | Global hard kind/subject/revision/deadline metadata with no task text |
| `records.attention_audio_cue_kib` | 256 KiB | Global hard exact128×2-KiB memory-only aggregate |
| `records.attention_audio_cue_seconds` | 2 seconds | Global hard source-edge/playback deadline; restart never restores or autoplays |
| `records.bulk_restart_candidates` | 256 | Global hard rows in one revision-pinned preview/intent |
| `records.bulk_restart_previews_per_surface` | 1 | Global hard memory-only60-second preview |
| `records.bulk_restart_previews_installation` | 64 | Global hard current preview count |
| `records.bulk_restart_preview_item_kib` | 256 KiB | Global hard ordered safe candidate/outcome-reason metadata |
| `records.bulk_restart_preview_mib` | 16 MiB | Global hard exact64×256-KiB memory-only aggregate |
| `records.bulk_restart_intents_nonterminal` | 64 | Global hard with one per Workspace and one candidate dispatched at a time |
| `records.bulk_restart_records` | 10,000 | Global hard overall intents/summaries, each≤64 KiB |
| `records.bulk_restart_record_mib` | 128 MiB | Global hard aggregate reached by2,048 maximum records |
| `records.bulk_restart_instance_receipts` | 100,000 | Global hard body-free exact candidate+canonical restart outcomes, each≤2 KiB |
| `records.bulk_restart_instance_receipt_mib` | 128 MiB | Global hard aggregate reached by65,536 maximum records |
| `records.bulk_restart_receipt_days` | 180 days | Rich terminal metadata compacts behind overall/per-candidate replay fences; possible effects never age out |
| `records.eco_scheduler_candidates` | 256 | Global hard exact eligible rows per Workspace scheduler generation |
| `records.eco_scheduler_queues` | 64 | Global hard one≤128-KiB queue per Workspace |
| `records.eco_scheduler_mib` | 8 MiB | Global hard exact64×128-KiB memory-only aggregate |
| `records.eco_attempts_per_minute` | 2 | Global hard rolling dispatch rate; resource pressure cannot bypass it |
| `records.eco_intents_nonterminal` | 64 | Global hard and one per AgentInstance |
| `records.eco_records` | 10,000 | Global hard intent/receipt count, each≤4 KiB |
| `records.eco_record_mib` | 32 MiB | Global hard aggregate reached by8,192 maximum records |
| `records.eco_receipt_days` | 30 days | Rich eligibility/session/outcome proof compacts behind non-replay identity; uncertainty never retries |
| `records.terminal_warm_parks_per_surface` | 12 | Global hard renderer-cache bound; a thirteenth Park is not admitted until one quiescent renderer is evicted, while protected/nonquiescent work remains charged to its ordinary attachment family |
| `records.terminal_view_parks_per_client` | 256 | Global hard local presentation records, each≤256 KiB while warm and≤4 KiB detached/blocked |
| `records.terminal_view_park_mib_per_client` | 32 MiB | Global hard local warm-renderer metadata/cache charge; eligible eviction only, never PTY/runtime state |
| `records.terminal_view_park_minutes` | 5 minutes warm / 10 minutes off-screen | Global defaults; off-screen detach requires exact durable-work survival and protected states defer indefinitely |
| `records.terminal_wake_input_bytes` | 4,096 bytes | Global hard whole-frame UTF-8 buffer per exact Pane/Attempt/InputLease,≤64/client and≤256 installation-wide/≤1 MiB |
| `records.terminal_wake_input_seconds` | 10 seconds | Exact attach/lease success flushes once; every failure/generation/selection change expires visibly and reconnect inherits none |
| `records.terminal_shadow_observers_per_target` | 128 | Global hard fixed zero-PTY control clients,≤256 installation-wide; one Turn painter/shadow/writer per durable session |
| `records.terminal_shadow_state_kib` | 128 KiB | Global hard exact handle/generation/grid/watermark/gap state;≤32 MiB installation-wide |
| `records.terminal_shadow_input_mib` | 8 MiB | Global hard≤512 control/output chunks per observer and≤256 MiB family; overflow gaps/resyncs the view, never pauses or kills the session |
| `records.terminal_shadow_process_mib` | 8 MiB | Global hard measured RSS per fixed control client and≤512 MiB family; cleanup uncertainty retains charge |
| `records.terminal_background_write_channels_per_target` | 1 | Global hard lazy fixed control client,≤64 installation-wide; binds one exact durable handle at a time |
| `records.terminal_background_write_queue_mib` | 1 MiB | Global hard≤64 literal-byte commands/channel/≤64 MiB family; one command≤64 KiB and lost reply is never resent |
| `records.terminal_background_write_linger_seconds` | 30 seconds | Global hard idle lifetime with five-second command deadline; painter/shadow attach retires it first |
| `records.automatic_detached_session_reaper` | 0 | Rejected capability: no durable or ephemeral timer/queue/receipt and no process/session body is retained or deleted by pressure |
| `records.agent_browser_grants_active` | 256 | Global hard active reviewed Workspace grants |
| `records.agent_browser_grant_item_kib` | 8 KiB | Global hard owner/policy/expiry plus≤64 public-HTTPS origin rules |
| `records.agent_browser_grant_mib` | 2 MiB | Global hard active-grant aggregate |
| `records.agent_browser_actions_nonterminal` | 256 | Global hard and one per exact agent-owned Browser Node |
| `records.agent_browser_action_records` | 10,000 | Global hard typed body-free action receipts, each≤8 KiB |
| `records.agent_browser_action_mib` | 64 MiB | Global hard aggregate reached by8,192 maximum records |
| `records.agent_browser_action_fences` | 100,000 / 48 MiB | Global hard≤512-byte installation-lifetime operation/action replay proof;98,304 maximum records at byte boundary |
| `records.agent_browser_action_days` | 30 days | Rich metadata compacts behind replay fence; page/accessibility/type bodies are never durable |
| `records.pty_capacity_observations` | 256 | Global hard one current durable observation per exact ExecutionTarget generation; replacement never retains an unbounded sample history |
| `records.pty_capacity_observation_item_kib` | 4 KiB | Global hard used/ceiling/headroom/source/time/coverage/freshness metadata with no process or terminal body |
| `records.pty_capacity_observation_mib` | 1 MiB | Global hard exact256×4-KiB aggregate |
| `records.pty_capacity_monitors` | 256 | Global hard one daemon-generation sampler state per current ExecutionTarget; it reuses the existing ResourceInventory collector and starts no generic helper |
| `records.pty_capacity_monitor_mib` | 1 MiB | Global hard exact256×4-KiB current reading/level/reminder metadata; daemon/target loss releases it |
| `records.pty_remediation_nonterminal` | 64 | Global hard and one privileged remediation intent per exact ExecutionTarget; N+1 has zero platform mutation |
| `records.pty_remediation_records` | 10,000 | Global hard nonterminal+rich-terminal target/provider/before/after/config/rollback records, each≤8 KiB |
| `records.pty_remediation_mib` | 64 MiB | Global hard aggregate reached by8,192 maximum records independently of count |
| `records.pty_remediation_replay_fences` | 100,000 / 48 MiB | Installation-lifetime hard≤512-byte operation/provider/before/after/rollback proofs;98,304 maximum at byte boundary |
| `records.pty_remediation_days` | 180 days | Terminal richness compacts behind replay proof; nonterminal, reconcile-required, partial and rollback-failed evidence never ages out |
| `records.companion_launch_grants_active` | 64 | Global hard locally reviewed grants, each≤32 immutable entries and≤24-hour expiry |
| `records.companion_launch_grant_mib` | 2 MiB | Global hard safe template/adapter/profile/target/cwd-root/checkout-policy metadata |
| `records.companion_launch_intents_nonterminal` | 64 | Global hard and one per active grant |
| `records.companion_launch_records` | 10,000 | Global hard preassigned-id/graph/checkout/runtime records, each≤8 KiB |
| `records.companion_launch_record_mib` | 64 MiB | Global hard aggregate reached by8,192 maximum records |
| `records.corrupt_store_quarantines` | 1,024 | Global hard and one current unresolved quarantine per Installation/Workspace owner; N+1 leaves original untouched/read-only |
| `records.corrupt_store_quarantine_item_mib` | 64 MiB | Global hard exact original regular-file bytes; larger input remains untouched/read-only |
| `records.corrupt_store_quarantine_mib` | 2,048 MiB | Global hard owner-only physical aggregate inside, not in addition to, the 8-GiB operational-store class |
| `records.corrupt_store_quarantine_days` | explicit disposition only | No time-based deletion; recover/start-fresh/export does not imply discard |
| `records.corrupt_store_recovery_intents_nonterminal` | 64 | Global hard and one per exact StoreOwnerKey |
| `records.corrupt_store_recovery_receipts` | 10,000 | Global hard body-free descriptor/hash/disposition/subreceipt records, each≤4 KiB |
| `records.corrupt_store_recovery_receipt_mib` | 32 MiB | Global hard aggregate reached by8,192 maximum records |
| `records.input_lease_history_days` | 7 days | Global; metadata only, never draft or input bytes |
| `records.share_invitation_days` | 30 days | Global; invitation secrets/keys and ephemeral presence are never stored |
| `records.share_audit_days` | 180 days | Global; redacted scope/action/receipt metadata only |
| `records.local_speech_models_mib` | 8,192 MiB | Global; M15 refuses install at the cap and never silently deletes the selected model |
| `records.microphone_leases_installation` | 1 | Global hard physical-device lease in M15; another Turn window requires explicit handoff before microphone open |
| `records.active_dictation_targets_installation` | 1 | Global hard frozen capture target; admission precedes microphone open and never follows selection |
| `records.dictation_target_item_kib` | 8 KiB | Global hard exact InputTarget/generation/revision metadata; no transcript or PCM |
| `records.voice_pcm_buffers_installation` | 2 | Global hard one active-capture plus one pending-inference buffer; a third capture allocates/opens nothing |
| `records.voice_pcm_buffer_item_mib` | 10 MiB | Global hard reserved mono signed-16-bit little-endian 16-kHz buffer for≤300 seconds/9,600,000 bytes |
| `records.voice_pcm_buffer_mib` | 20 MiB | Global hard exact two-buffer aggregate within shared variable RSS |
| `records.voice_hypotheses_installation` | 1 | Global hard current worker/capture hypothesis count |
| `records.voice_hypothesis_item_kib` | 32 KiB | Global hard sanitised memory-only hypothesis; it is never a draft or protocol body |
| `records.voice_transcript_drafts_per_surface` | 1 | Global hard provenance/review metadata over the existing LocalInputDraft body |
| `records.voice_transcript_drafts_per_client` | 8 | Global hard one per Surface for one LocalClientInstanceId |
| `records.voice_transcript_drafts_installation` | 64 | Global hard memory-only metadata count |
| `records.voice_transcript_draft_metadata_kib` | 4 KiB | Global hard origin/model/target/stale/truncation metadata, excluding the shared LocalInputDraft body |
| `records.voice_transcript_draft_metadata_kib_installation` | 256 KiB | Global hard exact 64×4-KiB metadata aggregate |
| `records.speech_workers_installation` | 1 | Global hard live-or-cleanup-pending sandbox worker count; N+1 spawns nothing |
| `records.speech_worker_item_mib` | 512 MiB | Global hard process RSS reservation, charged to shared variable RSS before spawn |
| `records.speech_worker_mib` | 512 MiB | Global hard live-or-cleanup-pending family aggregate; ProcessCleanupCharge inherits the same slot/bytes on a hang |
| `records.speech_inference_seconds` | 300 seconds | Global hard post-capture wall deadline |
| `records.speech_worker_shutdown_seconds` | 2 seconds | Global hard graceful-stop interval before forced tree termination and cleanup accounting |

Browser DOM/storage/history, ConversationInventory predicate/search text and both inventory/private-search raw
query pages intentionally have no retention setting because their durable count is zero. The separately
opted-in encrypted `PrivateTranscriptSearchIndex` has only the exact index/key-revocation policy above inside
`account_private_root`; it is not a raw page or provider transcript copy. Any other migration that persists
these bodies fails the closed-catalogue gate rather than inheriting a permissive default.

Live Node/Agent/Team/FlowRun/dependency/lineage/scope/endpoint/source/current-job and semantic-recovery records remain owner-lifetime data. Compaction applies
the historical limits above, uses only one constant-size aggregate for pruned attempts and refuses new live
records or remote artifacts at a declared bound before effect; it never silently ages out active semantics or
cleanup proof. Because semantic subjects pre-reserve both Workspace and installation recovery capacity, this
admission refusal can never occur on End/delete itself.

Session End/delete wipes Session/Surface-owned drafts, wake bytes, private-search query/view buffers and page
cursors, but it does not own the profile/target `PrivateTranscriptSearchIndex` and therefore cannot revoke its
key or unlink it. Lifecycle/replacement effects with uncertain cleanup retain only their exact bounded recovery
metadata and ProcessCleanupCharge after the Session row disappears; late evidence cannot restore the container.
Deleting provider transcripts, profile data or the encrypted index remains a separate scoped operation with
its own consent and refusal semantics.

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
user files or loading WebPreview content. Boundary-clock/count tests cover Flow revisions, sync journal/minimum
revision, status/diagnostics, input leases and share invitation/audit retention; a client older than 30 days
must resnapshot and can never resurrect a deleted id.
The ADR-063 fixture uses a different recognisable canary for a WorkItem comment, delegated Resource body,
progress provenance, five profiled plus five deliberately account-absent endpoint-unscoped binding
conversations, unmatched RuntimeInventory handle,
file body/edit/conflict buffer, current/pinned/live Note revisions, both AccountProfile roots/credentials and
remote input/session secret. It proves category-selected Note/WorkItem/Resource export includes only its
chosen body; all other canaries are absent from report, default/content-selected export, logs, diagnostics,
projection snapshots, Attention, sync journals, crash artifacts and remote/companion caches. Metadata
canaries appear only redacted in their declared owner scope. Unscoped fixtures create no AccountProfile,
credential/quota/activity row or cross-endpoint cache key. Count/time/byte boundary tests exercise exactly
limit minus one, limit and limit plus one; required Note pins survive compaction, overflow refuses atomically,
and scoped delete reports every retained external/downstream owner without deleting a sibling, adopted root,
file, runtime, provider transcript or Workspace.
The ADR-064 fixture adds distinct canaries for a WorkItemSource credential/raw response/unselected field,
permission prompt/frame/credential answer, native-job output, conversation body/search page, WebPreview/Browser DOM,
cookie/form value/local sibling file, provider title payload and both selected and sibling-profile Companion
events. It proves those bytes are absent from report, every export category, SQLite/WAL, sync journal,
diagnostics, Attention, crash artifacts and server/client caches after the declared memory lifetime. It also
proves mapped source fields, safe job/conversation/title metadata, numeric usage with unit/window/freshness and
typed permission receipts appear only in their exact source/profile/node/client scope; unavailable usage is
never serialised as zero. Source/profile/node privacy deletion leaves external items, jobs, conversations,
titles, local HTML and sites unchanged.
The ADR-065 fixture adds independent canaries for worktree/repository content, clone/SSH output, each of the
six adapters' transcript/event routes where supported plus explicit unsupported/degraded negative routes,
two quota-provider raw pages, model-gateway secret/discovery body, process argv/env,
automatic-name raw capture and notification token/plaintext. It proves only bounded sanitised roster,
CheckoutScope, onboarding, quota, route/model, resource-aggregate, name-fact and encrypted-delivery metadata
appear in their exact target/profile/node/endpoint scope. No canary appears in report/export, SQLite/WAL,
sync/status/Attention logs, diagnostics, process argv, PTY, crash artifacts or another profile/target/endpoint.
Boundary tests distinguish unmeasured resource data from measured zero, remove a notification generation's
queued ciphertext on revoke, preserve externally owned credentials/repositories/worktrees/device history, and
delete/compact every new Turn-owned category under the numerical controls above.
The ADR-066 fixture seeds distinct recognisable canaries into an imported source sibling and decoder
frame/cache; the sealed proposal input, omission-manifest input and provider response; upload/download bodies;
an announcement body, update manifest and package; a WorkItem comment; source/search/page/render payloads; and
every pre/post/inverse payload class excluded from presentation history. It proves that an accepted media blob,
active transfer temporary and the single combined update temporary/downloaded/verified/staged allocation exist
only in their declared owner-only stores while live;
the WorkItem comment remains only in its owning WorkItem; and every other excluded byte is absent from
SQLite/WAL, records, receipts, history, reports/exports, logs, diagnostics, Attention, sync journals, remote
snapshots/caches, process argv/environment and crash artifacts. Separate domain signatures expose only bounded
envelope metadata and never private signing keys or a second package/body copy. Exact cancel, completion,
expiry, owner End/delete and count/time/byte compaction fixtures remove each Turn-owned blob, partial and cache
from its declared owner without touching the original source, repository, remote endpoint or external WorkItem.
Recovery fixtures admit exactly 4,096 subjects in one Workspace and 32,768 installation-wide using metadata-only
reservations, refuse each N+1 subject before effect, then End a full Workspace and delete it using/moving only
the pre-existing slots. Late evidence consumes no new slot, unmatched runtime evidence enters only the target
inventory, and the 180-day terminal boundary releases resolved reservations while preserving non-reuse fences;
no capacity edge may refuse End or discard live/uncertain evidence.
When M15 ships, it must additionally seed recognisable PCM/transcript markers and prove they are absent from
protocol captures, SQLite/WAL, filesystem, logs, events, Attention, journals, diagnostics, exports and crash
artifacts before explicit delivery. It covers model/receipt/partial inventory, metadata-only export, signed-
catalogue bounds, symlink-safe delete/compact and the disclosed downstream retention after Insert/Send.
