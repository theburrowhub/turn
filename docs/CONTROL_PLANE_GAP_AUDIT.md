# Current control-plane gap audit

**Audited commit:** `e386f835a3eeced8a46aa60511a58ec8ddfa29b6`  
**Method:** independent read-only reviews of current domain, daemon, adapters, protocol, store, GUI and
product documents; three blind source-capability audits; then cross-checking against the frozen 112-row neutral
capability ledger and product inventory
**Conclusion:** the v0.1 foundation is real; the complete operator-control-plane product is not implemented

This report exists to prevent specification work from being mistaken for product completion. At the audited
commit the integrated target inventory classifies 4 requirements as baseline, 43 partial, 121 target and 17 in direct
conflict with current behavior. `make product-spec-acceptance` reproduces the current count, verifies the
versioned semantic-hash manifest and proves every row has one proof obligation. `baseline` records a current
foundation only; it is not the formal `implemented` status. Because no row yet has commit-bound implementation
evidence, `scripts/verify-product-completion.sh` deliberately rejects all 185 rows. Of those, 181 are additionally
partial, target-only or in direct conflict with current behaviour.

## Critical observed gaps

| Gap | Current evidence | Required outcome |
| --- | --- | --- |
| False-zero subagent summary | `SessionTree::subagent_count` counts only `NodeKind::Subagent` rows (`crates/turn-core/src/model/node.rs:432`), while process discovery may classify agent children as `Agent`; `SessionSummary` exposes a bare `usize` (`crates/turn-proto/src/view/session.rs:102`). | One covered semantic graph; exact zero only with positive fresh evidence, partial `N+ observed`, and distinct unknown/unsupported/stale. |
| Provider-specific resume in the daemon | The adapter trait owns `resume_args` (`crates/turn-agents/src/adapter.rs:182`) and Claude, Codex, Gemini and OpenCode implement it, but `resume_arguments` accepts only `"claude-code"` (`crates/turnd/src/core/requests/nodes.rs:655`). | Capability-selected adapter dispatch with common fixtures and live evidence; no daemon provider-name branch. |
| Relaunch replaces semantic identity | Current code says the old node leaves the tree and materialises a replacement (`crates/turnd/src/core/requests/nodes.rs:194`, `:258`). Only Workspace/Session/Node/Pane-era ids exist (`crates/turn-core/src/ids.rs:55`). | Stable AgentInstance with ordered RuntimeAttempts and verified provider-conversation continuity. |
| Node view is not the navigation surface | The accepted WorkSurface/AgentNodeView is documented but the current GUI still selects or opens Pane-oriented content; no WorkSurface/ViewTarget domain types exist. | One unique typed Node View per semantic child; selection mutates no Layout/process/input/Attention. |
| Attention navigation acknowledges implicitly | `goto_attention` is documented as “jumps ... and marks it acknowledged” (`crates/turnd/src/core/requests/attention.rs:35`). | Separate route, render, read, acknowledge, answer submission and evidence-backed resolution. |
| Runtime truth is not durable | Current `AgentInfo` has only a small optional model/token/cost surface; store schema ends at migration 11 (`crates/turn-store/src/migrations.rs:29`). | LaunchSpec/Receipt, instance/attempt, scoped context/quota/resource observations and freshness/provenance. |
| Flows/delegated control do not exist | Creation is Pane/Session driven and there are no FlowDefinition, FlowRun or DelegationGrant types or tables. Earlier product text also rejected all workflow scheduling. | One catalog, immutable runs, typed start policies, bounded conductor grants and asynchronous worktree-safe fan-out under ADR-061. |
| Provider topology is uneven | Claude and OpenCode expose some child evidence; Gemini declares no subagent events; heuristics cannot prove semantic parents. Current fixtures do not prove identical visible results live across all dedicated adapters. | ADR-062 normalized topology/capabilities, honest degradation, integration diagnostics and versioned live parity evidence. |
| Context transfer is only the v0.1 handoff | The current reviewed PTY handoff has no durable ContextLink, ContextPacket v2, lineage, target-aware budget or provider receipt types. | Scoped pull links plus bounded portable packets, branch/handoff lineage, receipts and Flow pre-authorisation. |
| Runtime continuity is limited to the daemon lifetime | The daemon owns PTYs; machine/daemon restart restores metadata as lost/orphaned and no RuntimeBackend/ExecutionTarget abstraction exists. | Separate domain `attach_runtime_attempt`, presentation `attach_pane`/resync and cold reconstruction guarantees across local/durable/remote backends with fail-closed location. |
| Local voice is specification only | `docs/LOCAL_VOICE_INPUT.md` explicitly states not implemented; no SpeechWorker/DictationTarget/model operations exist. | Packaged on-device worker, verified optional models, exact memory-only review target and zero Attention/control authority. |
| Multi-client/remote collaboration is not authoritative | The v0.1 socket supports clients but has no full snapshot+journal conflict model, durable tombstones, runtime input lease or scoped encrypted operator sharing. | Revisioned reconnect and conflict rules, one visible writer, capability-separated companions/sharing. |
| Primary `main` is an allowed v4 worker target | `CreateSession` defaults to `main_checkout`, current protocol exposes acquisition and legacy sessions may retain the write lease. | Every creation/lifecycle writer path resolves a dedicated worktree; legacy mode is migration-only and release proof finds zero primary-path workers/locks. |
| Foreground activation still needs a second action | Restored/stopped content can surface a start affordance and selection is not backed by a safe revisioned activation plan. | One Session selection restores/attaches and may start its exact bounded eligible saved descriptors or one preflighted safe default Shell; every changed consequence fails closed. |
| External issue/work sources are not modeled | Board metadata is local; no source identity, paging, cache coverage, revisioned writes or conflict/reconciliation adapter exists. | Canonical WorkItems with source/field authority, bounded sync, CAS writes, close/reopen and credential-safe conflict handling. |
| Provider-retained conversations and jobs are invisible | Runtime discovery covers live handles only and Turn has no provider-native job or historical conversation identity. | Private bounded ConversationInventory plus stable provider Job/Iteration objects, independent adopt/resume/control and honest survival/coverage. |
| Web content has no interactive isolated counterpart | Resource WebPreview semantics are inert and there is no Browser object with origin, storage, popup/download or reviewed local/loopback policy. | Separate inert WebPreview and isolated Browser Node whose untrusted content has no control authority. |
| Remote permission handling is all-or-nothing | Companion/full remote cannot answer typed permissions safely; raw PTY input could bypass a superficial deny list. | One exact foreground-issued E2EE response grant, revision fencing and raw-input block at known sensitive interactions; secrets/admin/trust remain local. |
| Conversation titles and profile activity are underspecified | Read/rename are not distinct capabilities and companion usage/history can lack profile/coverage/freshness truth. | Separate read/rename receipts and per-profile context/quota/activity projections that never invent zero. |
| Group presentation is one-level and has no complete branch-isolation lifecycle | Existing Group contracts prohibit nesting, while worktree-safe fan-out lacks recursive tree operations, creator provenance, missing-versus-failed inventory and explicit unbind/remove behavior. | Same-Session bounded acyclic Group forest plus separately owned CheckoutScope state projected into the tree; atomic subtree and exact target-bound worktree lifecycle while primary `main` stays free. |
| Attention has no durable background delivery contract | OS/companion projection language does not define endpoint/grant/outbox/retry/collapse/live-end state or a notification-only host with zero public listener. | Revocable scoped device grants, minimal encrypted subject/revision delivery, monotonic live fences and outbound-only headless mode; delivery never resolves Attention. |
| RuntimeInventory lacks capacity/accounting truth | Target-wide handles/survivors exist in the accepted target, but host used/total/swap/pressure, reuse-safe process trees, closed-owner attribution and measured-empty versus failure are absent. | ResourceInventory fields on the same target snapshot, explicit coverage/result state, deduplicated current/closed/unmatched attribution and exact re-probed intervention. |
| Dedicated/provider support denominator is incomplete | The previous target named four adapters plus generic/custom and did not distinguish quota-only providers or configurable model routes from AccountProfile/RuntimeEndpoint. | Six dedicated adapters under all 23 common cells, Kimi/MiniMax quota-only connectors and separate target-bound ModelEndpointProfiles with secret refs, discovery and no fallback. |
| Workspace onboarding and safe names are implicit ergonomics | Setup mentions integration diagnostics but not closed new/open/clone/SSH partial-effect recovery; local generated terminal/Group names lack source/revision/redaction/pinning. | One resumable WorkspaceOnboarding state machine with publish separated, plus bounded DisplayNameFact/NameProposal that cannot alter identity/provider title/input. |
| Active-work-surface visual cleanup was omitted from the first frozen denominator | The reference snapshot exposes one-action top-level non-overlapping arrangement for the active work surface, but two prior blind passes failed to give it a disposition or oracle. | Adapt it to automatic compact non-overlapping canonical-tree layout in stable logical/accessibility order, with no cleanup action, coordinates or domain effects. |
| Selective gap prose has no frozen denominator | A capability omitted from both requirement and oracle was previously indistinguishable from a deliberate product boundary. | A 112-row neutral snapshot/evidence ledger plus a reproducible selector-v1 source census, with adopted/adapted/rejected/irrelevant disposition and PRD/ACP/ADR trace; deletion, unknown, weakening, omitted candidate or broken evidence fails the gate. |

## What the integrated specification now closes

- One product outcome covers agents and ordinary terminal tools, fast creation, Flows, hierarchy, unique
  views, lifecycle, context transfer, telemetry, exact Attention, voice, remote/companion operation and scale.
- ADR-061 replaces the old passive-only orchestration boundary with explicit bounded automation while
  preserving human permissions, typed authority and the primary checkout.
- ADR-062 prevents the richest provider from becoming the implicit core model and prohibits false-zero
  topology by contract.
- The current v4 count limitation is named in `docs/PROTOCOL.md`; vNext fixes metric/scope/source epoch,
  snapshot/delta/gap/watermark, coverage invalidation and asynchronous resync, including a real exact-zero oracle.
- Product, architecture, roadmap, protocol, security, privacy and detailed Agent contracts use the same Flow/
  grant/dependency boundary.
- Flow lifecycle/recurrence/wire operations, deterministic tree placement, context authority, message state,
  remote security/backends, Attention policy, companion actions, sync/conflict/import, setup, status,
  accessibility, retention and fixed-profile scale all have explicit falsifiable contracts.
- ADR-064 adds separately falsifiable foreground Session activation, external WorkItemSource, provider-native
  jobs, ConversationInventory, WebPreview/Browser, title read/rename, delegated typed remote permission and
  profile-scoped companion activity contracts rather than hiding them inside broad feature rows.
- ADR-065 adds recursive Groups/CheckoutScopes, closed Workspace onboarding, six-adapter and quota-connector
  parity, ModelEndpointProfile, ResourceInventory capacity/accounting, safe name proposals and background
  Attention delivery plus automatic pure-projection hierarchy arrangement; each has an independent requirement and oracle.
- ADR-066 adds bounded Media/search/SCM-host/proposal/transfer/projection utilities, one general CommandCatalogue,
  domain-separated announcements/updates, ordered WorkItem activity and presentation-only undo/redo; canonical
  signing, sandbox and performance oracles prevent those helpers from becoming hidden authority.
- `PRODUCT_CAPABILITY_COVERAGE_V1.tsv` freezes 112 source-capability dispositions—51 adopted, 57 adapted,
  2 rejected and 2 irrelevant—with per-locator evidence digests and an opaque source/tree digest. The paired
  `PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv` freezes the selector-v1 production-module and closed-registry candidate set. Their
  schema/count/sequence/links/evidence are machine-checked, so a
  future audit has a visible denominator rather than a selective narrative.
- Every frozen requirement maps one-to-one to an acceptance oracle, including every named cross-feature journey.
- CI requires an out-of-band authority pin and runs traceability plus adversarial mutations. Paired deletion,
  fully co-edited deletion/weakening, origin swaps, requirement/oracle weakening, malformed cells and dirty,
  untracked or symlinked authority all fail for their exact expected reason. Separate SHA-1/SHA-256
  transition fixtures require all-`implemented` inventory, commit-bound production/oracle sources and fresh
  exact artifacts from an isolated checkout; no-op, stale, path-traversal, wrong-hash, extra-file/FIFO and
  caller-input attacks are rejected.
- The protected pin cannot authenticate a workflow that a malicious PR is itself permitted to replace. The
  merge policy must require an externally identified workflow/ruleset or a trusted workflow/authority diff;
  the checked-in gate is deliberately not described as a self-protecting trust anchor.

## What remains unproved

Everything marked partial, target or conflict in `docs/PRODUCT_REQUIREMENTS.md` remains open implementation
work. In particular, this specification change does not fix the currently visible subagent count, remove the
resume hardcode, add WorkSurface/AgentInstance/Flow types, provide runtime continuity, fetch quotas or ship
voice. Those claims can move only when their matching `ACP-*` oracle passes on the implementation commit.

The research phase used source inspection and existing recorded live evidence. It did not re-run every
external provider, remote host, microphone, packaged platform or the new scale envelope because the product
features needed by those tests do not exist yet. The proof plan therefore requires those live/destructive/
packaged measurements rather than treating their design as evidence.

## Reproduction

```sh
make product-spec-acceptance
make product-capability-source-acceptance CAPABILITY_SOURCE_REPOSITORY=/path/to/audited-source-repository
./scripts/verify-product-completion.sh # expected: E_NOT_IMPLEMENTED for all 185 rows
git grep -nE 'PRD-[A-Z]+-[0-9]{3}' -- docs/PRODUCT_REQUIREMENTS.md
git grep -nE 'ACP-[A-Z]+-[0-9]{3}' -- docs/CONTROL_PLANE_ACCEPTANCE.md
git status --short
```

The first command must report 185 requirements, 185 proof obligations, the exact manifest revision and the
honest current-state distribution above. Given a Git clone containing the frozen opaque snapshot, the second
command recomputes its complete tree digest, all 112 evidence-reference digests (covering 89 unique locator/digest pairs) and the selector-v1 census of 1,386 selected production modules/closed registry surfaces plus 2,188 normalized mappings; it neither fetches nor trusts the
caller's current branch. The third command must fail with `E_NOT_IMPLEMENTED: 185`; a green result would mean
the implementation-evidence goal had changed and requires a separate audit. The last command is included so an audit records the exact tree whose claims it
evaluated.
