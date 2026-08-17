# Gemini CLI, OpenCode, and external-app acceptance

Run the complete reproducible check without opening Turn:

```sh
make adapter-acceptance
```

This verifies the dedicated adapter selection, safe launch degradation, hook or
plugin configuration, normalized lifecycle/permission/question/failure events,
confidence and source attribution, resume arguments, and graphical-process tree
placement.

## Supported contracts

- Gemini CLI `0.46.0`: `crates/turn-agents/tests/fixtures/gemini-cli-0.46.0.json`
  follows the hook reference bundled with that release. Turn subscribes to
  `SessionStart`, `BeforeAgent`, `AfterAgent`, `BeforeModel`, `BeforeTool` for
  `ask_user`, `Notification`, and `SessionEnd`.
- OpenCode `1.18.16`: `crates/turn-agents/tests/fixtures/opencode-1.18.16.json`
  follows the event bridge and schema sources at Git tag `v1.18.16`. The fixture
  includes the tool's own `info.version` field.

The version is part of each fixture name and adapter contract constant. Do not
edit an old fixture in place for a new tool release: add a newly versioned file,
run the contract tests against both, then remove the old one only when support is
intentionally dropped.

## Live smoke test

The contract suite is offline and deterministic. Before changing a supported
version, repeat this live smoke in a disposable Turn Session using the real CLI:

1. Start `gemini`, send one prompt, provoke one `ask_user` question and one tool
   permission, then exit. Confirm the Agent moves active → question/permission →
   completed, reports its session id and model, and can resume.
2. Start `opencode`, send one prompt, provoke a question and permission, create a
   child session, then exit. Confirm the parent/child hierarchy and the same state
   transitions, then resume with the recorded session id.
3. From either agent launch Godot or Blender. Confirm an `EXTERNAL APP` child is
   shown under the launching process, selection leaves desktop focus untouched,
   and its inspector says the interface lives outside Turn.
4. Repeat with Gemini hooks disabled and with `opencode --pure`. Both CLIs must
   still start; the inspector must report inferred integration.
5. Stop `turnd` during a callback. The CLI must continue without a warning or an
   extra interaction.

Capture callback JSON from the disposable hook endpoint, remove user content and
secrets without changing field names or types, save it under the exact observed
tool version, and rerun `make adapter-acceptance`.

## Accepted provider-parity target

The checks above prove the current Gemini/OpenCode adapter surface; they do not prove ADR-062's complete
provider-neutral topology. Before Turn advertises that target, Claude Code, Codex, Gemini and OpenCode run
the same capability-driven fixture/degradation suite, the generic fallback and user-declared custom adapter
run the honesty suite, and every advertised live-dependent cell carries a dated authenticated record.

### One complete capability vocabulary

The shared manifest has exactly one row for each canonical capability below. A provider may report
`supported`, `unsupported`, `degraded` or `unknown`; it may not omit a row or invent a provider-only synonym.

| Capability | Common contract exercised |
| --- | --- |
| `launch` | New instance/attempt, requested-versus-effective launch receipt and failure before partial registration. |
| `resume` | Exact provider conversation plus profile/target binding, fresh attempt lineage and stale-id refusal. |
| `branch` | Exact source conversation/revision, distinct destination identity and unsupported-with-zero-effect path. |
| `stop` | Idempotent exact-attempt stop, uncertain-effect receipt and no process-name fan-out. |
| `structured_status` | Total native map into independent Lifecycle and TurnState axes, with source/revision/freshness. |
| `questions` | Exact pending interaction, correlated answer/cancel and no free-text fallback for a typed permission. |
| `permissions` | Typed scope/options, exact attempt/turn correlation and durable resolution evidence. |
| `subagents` | Versioned snapshot/delta/gap topology, stable semantic identity and attempt-fenced parentage. |
| `transcript` | Per-conversation ordered cursor, bounded content, provenance and explicit unavailable/gap state. |
| `context_usage` | Scoped used/limit/unit/window facts with observation time, expiry and honest unavailable state. |
| `provider_quota` | Account/profile-scoped remaining/reset/unit facts, rate-limit/error degradation and no fabricated zero. |
| `model_switch` | Requested/effective model receipt, exact current attempt and unsupported-before-effect behavior. |
| `messaging` | Structured destination, delivery/evidence state and refusal rather than generic PTY injection. |
| `context_transfer` | Budgeted packet capability, exact source/destination attempt and reviewed delivery receipt. |
| `shared_identity` | Proved provider conversation identity without merging Turn AgentInstances. |
| `durable_attach` | Exact endpoint/runtime generation reattachment and stale/mismatched identity refusal. |
| `delegated_control` | Closed operation vocabulary, bounded grant and correlated accepted/refused/uncertain receipt. |

The fixture manifest is bijective with these rows and the production capability manifest. Shared fixtures,
provider fixtures, degraded/unsupported/unknown fixtures and live evidence all use the same identifiers.
Unknown native values, missing fields and illegal state transitions fail the adapter contract rather than
falling through to an inferred provider behavior.

### Generic and custom honesty suite

An unrecognised command always selects the generic terminal adapter. A user-declared custom adapter always
selects deterministically from its declaration. Each is run through all 17 vocabulary rows and must:

1. advertise only evidence it can prove, with an explicit state, mechanism, limits, reason and remediation;
2. leave Lifecycle and raw terminal/process observation available without fabricating TurnState, questions,
   permissions, subagents, quota, transcript, resume or structured identity;
3. reject an undeclared operation before launch/input/network/file effects and without falling through to a
   dedicated provider implementation; and
4. degrade one capability independently, preserving terminal input, selection and all unrelated capabilities.

Fixtures cover an unknown binary, a shell, a renamed wrapper, a valid minimal custom declaration, every
invalid/undeclared custom field, an unavailable executable and a declaration whose evidence mechanism fails
at runtime. UI and protocol snapshots distinguish unsupported, degraded, unknown and stale from exact zero.

### Topology and count matrix

The child-topology smoke asks Claude Code for exactly three children and Codex for exactly five, then repeats
for Gemini and OpenCode whenever they advertise `subagents`. It verifies parent/child ids, UI reconnect,
daemon recovery, duplicate/out-of-order/stop-without-start evidence and degraded mechanisms. Count assertions
cover the full Cartesian product below at one graph revision:

- metric: `semantic_children`, `live_children`, `completed_children`;
- scope: `direct`, `descendants`; and
- parent lifetime: `current_attempt`, `instance_lifetime`.

That is 12 independently expected cells for each fixture revision. Fixtures include a nested child, a
completed child with no active attempt, a completed-but-still-live child, spawning/orphaned/conflicted/lost
children and an old parent attempt. A separate valid empty authoritative snapshot proves `exact(0)` for every
applicable cell; an empty best-effort source remains unknown. Event 1,025 forces bounded overflow/gap and
immediately degrades exact coverage. A matching asynchronous snapshot restores it without blocking input.
Partial `N+ observed`, unknown, unsupported and stale are separate protocol/UI values. The expected event and
snapshot manifest is generated independently and never invokes the production aggregate query.

### Capability dispatch

`launch`, `resume`, `branch`, `model_switch` and `stop` dispatch only through the capability-selected adapter
method. The matrix exercises supported success, adapter-declared degraded behavior, unsupported/unknown,
timeout, cancellation, stale capability, wrong version/profile/host/endpoint/attempt/epoch and duplicate
operation id. Unsupported or mismatched calls fail before side effects. The daemon, store, protocol, UI and
Attention reducers contain no provider-name re-check after registry selection. Question/permission answers,
messaging, context transfer, durable attach and delegated control likewise select their typed method from the
current capability intersection plus authority; no operation silently falls back to PTY text.

### Shared live endpoint isolation

One authenticated live fixture binds exactly five simultaneous AgentInstances and five distinct conversation
ids to one `RuntimeEndpoint` generation: three instances use `AccountProfile A` and two use `AccountProfile B`.
The provider must advertise multiplexing across those two profiles; otherwise the capability is unsupported
and the test does not approximate it with multiple endpoints. The fixture interleaves unique canary prompts,
transcript pages, context/quota observations, questions and Attention demands for all five and proves:

- exactly one current owner per conversation and at most one current binding per instance;
- no input, transcript cursor, context grant, quota attribution, Attention subject or writer lease crosses a
  binding or either profile;
- a duplicate conversation claim, cross-profile handle, sibling operation and late old-generation event is
  refused with zero effect on the five current bindings;
- saturation/backpressure on instance 2 leaves instances 1, 3, 4 and 5 within their latency budget; and
- endpoint disconnect/crash/restart makes each binding stale independently, then exact reattach or explicit
  per-instance fallback creates correct lineage without merged identity, duplicate launch or cross-talk.

The run is not accepted with `N`, fewer than five bindings, one profile, a mocked service or one endpoint per
instance. Its transcript and Attention assertions use per-binding canaries that are removed from published
artifacts after their absence outside the intended binding has been proved.

### Authenticated live-evidence manifest

Every live-dependent advertised capability cell links one immutable record with all of these fields:

- evidence schema/id, capability id and claimed `supported|degraded` state plus mechanism and limits;
- adapter id/version, provider and exact CLI/service version, executable or artifact digest and Turn commit;
- pseudonymous AccountProfile id, ExecutionTarget/host id, RuntimeEndpoint id/generation, attempt/epoch and
  fixture id, omitting credentials and user content;
- authenticated test identity/environment class, invocation id, start/end time, observed-at, expires-at and
  freshness policy;
- expected oracle, actual typed result, per-step result, timeout/resource/quota consequences and final pass;
- raw-record digest/signature or trusted CI attestation, fixture/manifest digest, redaction version/result,
  cleanup receipt and links to bounded encrypted artifacts; and
- failure/downgrade reason, remediation and superseded-record id when the cell is no longer current.

CI rejects an advertised live-dependent cell whose record is absent, failed, expired, for another capability
scope/version/profile/host/endpoint, unauthenticated, digest-invalid, unredacted or missing cleanup proof. A
record proves only its exact cell; one provider/profile cannot approve another.

Every run also captures the integration diagnostic: detected CLI version, configured/effective mechanism,
last successful invocation, last valid/rejected event, achieved level, freshness, downgrade reason/remediation
and redacted export. The frozen obligations are `ACP-TOP-001` through `ACP-TOP-009` and `ACP-ADP-001`
through `ACP-ADP-010` in `docs/CONTROL_PLANE_ACCEPTANCE.md`.

Primary contract references:

- Gemini CLI: <https://geminicli.com/docs/hooks/reference/>
- OpenCode plugins: <https://opencode.ai/docs/plugins/>
- OpenCode configuration merging: <https://opencode.ai/docs/config/>
- OpenCode CLI session flag: <https://opencode.ai/docs/cli/>
