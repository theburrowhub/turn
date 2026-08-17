# Current control-plane gap audit

**Audited commit:** `e386f835a3eeced8a46aa60511a58ec8ddfa29b6`  
**Method:** independent read-only reviews of current domain, daemon, adapters, protocol, store, GUI and
product documents, followed by cross-checking against the frozen product inventory  
**Conclusion:** the v0.1 foundation is real; the complete operator-control-plane product is not implemented

This report exists to prevent specification work from being mistaken for product completion. At the audited
commit the inventory classifies 4 requirements as baseline, 43 partial, 72 accepted target and 17 in direct
conflict with current behavior. `make product-spec-acceptance` reproduces the current count, verifies the
versioned semantic-hash manifest and proves every row has one proof obligation; it does not turn the latter
132 rows into implemented features.

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
| Runtime continuity is limited to the daemon lifetime | The daemon owns PTYs; machine/daemon restart restores metadata as lost/orphaned and no RuntimeBackend/ExecutionTarget abstraction exists. | Separate warm attach and cold reconstruction guarantees across local/durable/remote backends with fail-closed location. |
| Local voice is specification only | `docs/LOCAL_VOICE_INPUT.md` explicitly states not implemented; no SpeechWorker/DictationTarget/model operations exist. | Packaged on-device worker, verified optional models, exact memory-only review target and zero Attention/control authority. |
| Multi-client/remote collaboration is not authoritative | The v0.1 socket supports clients but has no full snapshot+journal conflict model, durable tombstones, runtime input lease or scoped encrypted operator sharing. | Revisioned reconnect and conflict rules, one visible writer, capability-separated companions/sharing. |
| Primary `main` is an allowed v4 worker target | `CreateSession` defaults to `main_checkout`, current protocol exposes acquisition and legacy sessions may retain the write lease. | Every creation/lifecycle writer path resolves a dedicated worktree; legacy mode is migration-only and release proof finds zero primary-path workers/locks. |

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
- Every frozen requirement maps one-to-one to an acceptance oracle, including every named cross-feature journey.
- CI supplies a protected authority pin and runs traceability plus adversarial mutations. Paired deletion,
  fully co-edited deletion/weakening, origin swaps, requirement/oracle weakening, malformed cells and dirty,
  untracked or symlinked authority all fail for their exact expected reason. Separate SHA-1/SHA-256
  transition fixtures require all-`implemented` inventory, commit-bound production/oracle sources and fresh
  exact artifacts from an isolated checkout; no-op, stale, path-traversal, wrong-hash, extra-file/FIFO and
  caller-input attacks are rejected.

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
git grep -nE 'PRD-[A-Z]+-[0-9]{3}' -- docs/PRODUCT_REQUIREMENTS.md
git grep -nE 'ACP-[A-Z]+-[0-9]{3}' -- docs/CONTROL_PLANE_ACCEPTANCE.md
git status --short
```

The first command must report 136 requirements, 136 proof obligations, the exact manifest revision and the
honest current-state distribution above. The last command is included so an audit records the exact tree
whose claims it evaluated.
