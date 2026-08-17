# Operator control-plane acceptance

**Status:** normative proof plan for the accepted post-v0.1 target; it does not claim these scenarios pass.

This matrix makes product completion reproducible. Every frozen requirement has exactly one primary proof
obligation. A primary obligation may name several evidence classes because a deterministic fake proves
semantics while a live or packaged run proves that a real integration supplies the promised evidence.

Evidence classes:

- **A — automated:** deterministic unit, property, integration, protocol or native snapshot test in CI;
- **D — destructive/recovery:** crash, race, disconnect, stale-generation or resource-pressure harness;
- **L — live:** opt-in authenticated run against the exact external CLI/provider version recorded in the
  evidence manifest;
- **P — packaged/platform:** signed/ad-hoc packaged application on each claimed OS, including assistive or
  device behavior that a headless fake cannot prove;
- **M — measured:** declared workload/hardware, raw measurements and enforceable regression budgets.

An acceptance record stores commit, build/version, platform, adapter/provider versions, fixture or account
scope, timestamps, command, result and artifact hashes. Unsupported capabilities need degradation evidence,
not a skipped test. “The PR merged”, “CI is green”, a screenshot, a mocked provider or a count of tests is
never sufficient by itself.

## Outcome, hierarchy and views

| Acceptance | Requirement | Evidence | Passing oracle |
| --- | --- | --- | --- |
| `ACP-OUT-001` | `PRD-OUT-001` | A, M, P | At the design workload, an operator creates work, reaches every pending action through Attention and identifies every running/finished unit without opening panes to poll. |
| `ACP-OUT-002` | `PRD-OUT-002` | A, L | One mixed Session runs all supported dedicated adapters plus shell, TUI and log nodes; shared behavior is uniform and capability gaps are explicit. |
| `ACP-OUT-003` | `PRD-OUT-003` | A, D | Every interrupting surface consumes the same ordered queue/revision; no Layout, provider widget or telemetry path can move focus independently. |
| `ACP-OUT-004` | `PRD-OUT-004` | A, P | Recorded pointer/keyboard journeys meet §3.3 exactly: attach and Attention use at most one action, default create/Flow launch two, and unresolved authority is one consolidated review; no generic start or redundant confirmation appears. |
| `ACP-HIE-001` | `PRD-HIE-001` | A, P | Every durable unit appears under exactly one Workspace/Session root and all navigation/accessibility paths use that one tree. |
| `ACP-HIE-002` | `PRD-HIE-002` | A | A fixture gives the same nodes ancestry, group, dependency, context, lineage and message edges; each graph retains independent meaning and serialization. |
| `ACP-HIE-003` | `PRD-HIE-003` | A, D | Identity/cardinality property tests vary every id independently, enforce the §2.3 table, reject mismatched joins and migrate legacy aliases to distinct ids plus resolution-only tombstones. |
| `ACP-HIE-004` | `PRD-HIE-004` | A, D, L | Nested children receive stable identities/lifecycles, survive UI reconnect, reconcile after daemon/runtime recovery and never attach completion to a reused attempt. |
| `ACP-HIE-005` | `PRD-HIE-005` | A | Group add/move/delete and multi-Team membership alter only their named relationship; ancestry, context, checkout and execution authority remain unchanged and Team membership never moves a row. |
| `ACP-HIE-006` | `PRD-HIE-006` | A | Conflicting explicit/inferred/out-of-order edges keep the strongest evidence; unresolved children render unassigned with provenance. |
| `ACP-HIE-007` | `PRD-HIE-007` | A | A generated schema inventory matches domain/store/protocol/view/detailed-contract kind and relationship sets byte-for-byte; every kind round-trips and no vNext id aliases another type. |
| `ACP-HIE-008` | `PRD-HIE-008` | A, P | A node with spawn/process ancestry, one Group, multiple Teams and Flow membership renders once under fixed precedence; equal strongest evidence for two different parents remains ambiguous/unassigned, stable edge id orders only references, and Group moves leave semantic counts/Attention unchanged. |
| `ACP-VIE-001` | `PRD-VIE-001` | A, P | Selecting every hierarchy kind produces exactly one revisioned ViewTarget and one WorkSurface destination. |
| `ACP-VIE-002` | `PRD-VIE-002` | A | Selection changes no persisted Layout, process count, PTY input, Flow operation, unread flag or Attention state. |
| `ACP-VIE-003` | `PRD-VIE-003` | A, P | Snapshot/state-table tests cover content plus loading/empty/unsupported/disconnected/stopped/lost/stale for every NodeKind without generic fallback lies. |
| `ACP-VIE-004` | `PRD-VIE-004` | A, L, P | Dedicated Agent/subagent fixtures and live runs show every required region, exact parent/attempt and truthful missing fields. |
| `ACP-VIE-005` | `PRD-VIE-005` | A, P | Selecting live work attaches with zero action; all source and snapshot searches contain no generic start-pane affordance; stopped work names the exact consequence. |
| `ACP-VIE-006` | `PRD-VIE-006` | A, P | Inspector data exists only inside the Node View, previews remain bounded, and Session return restores exact layout/zoom/focus across navigation/reconnect. |
| `ACP-VIE-007` | `PRD-VIE-007` | A, P | Native snapshots at minimum/normal/maximum widths show icon/name/buttons left, metadata right once, and scoped operational status only at the bottom. |
| `ACP-VIE-008` | `PRD-VIE-008` | A, D, P | Session creation starts from its Workspace; end/delete removes the row before cleanup completes and a survivor warning cannot resurrect or veto it. |
| `ACP-VIE-009` | `PRD-VIE-009` | A, D, P | Search/group/board activation selects the same canonical Node/ViewTarget and co-attaches rather than duplicates work; an injected view panic leaves tree, transports and other views usable. |
| `ACP-VIE-010` | `PRD-VIE-010` | A, P | Concurrent success/progress/warning/error fixtures obey priority, replacement, expiry, overflow/history and recovery rules; live-region output announces start/terminal once and creates no Attention without a typed demand. |
| `ACP-VIE-011` | `PRD-VIE-011` | A, D, P | Table/board/search projections share Node ids/revisions; every legal/illegal state move, priority/due/tag/comment/assignee conflict and keyboard alternative is tested, while edits produce zero lifecycle/turn/dependency/start/Attention effect absent a separately configured demand. |

## Creation, flows and delegated work

| Acceptance | Requirement | Evidence | Passing oracle |
| --- | --- | --- | --- |
| `ACP-CRE-001` | `PRD-CRE-001` | A, P | All four creation surfaces are generated from one catalog revision and expose identical action ids, names, defaults, validation and unavailable reasons; contextual grouping/order may differ without changing meaning. |
| `ACP-CRE-002` | `PRD-CRE-002` | A, P | Default Session/Agent/Tool flows complete from Workspace + task/preset + optional prompt; only unresolved authority/collision choices interrupt. |
| `ACP-CRE-003` | `PRD-CRE-003` | A, D | Duplicate/raced create ids yield one visible run and one runtime per spec; every injected failure has complete rollback or a visible recoverable receipt. |
| `ACP-CRE-004` | `PRD-CRE-004` | A | Built-in and custom catalog entries traverse the same validation/adapter route; undeclared custom capabilities are rejected and rendered unsupported. |
| `ACP-CRE-005` | `PRD-CRE-005` | A | Template duplication contains no live/runtime/provider ids; Flow launch pins one immutable revision and later edits affect only later runs. |
| `ACP-CRE-006` | `PRD-CRE-006` | A, P | Single, multi, cancelled and cross-Workspace creates produce the specified selection/focus outcome; no child steals focus and no unsafe/missing owner receives input. |
| `ACP-CRE-007` | `PRD-CRE-007` | A, D, P | Clean/degraded/skipped setup discovers exact versions, guides auth/consent/trust at point of use, stores no credential canary and leaves generic terminal usable after every cancellation/failure. |
| `ACP-FLW-001` | `PRD-FLW-001` | A | Schema round-trip and invalid-case tests cover every declared node, role, command/prompt, dependency, context, execution, isolation, Attention and resource field. |
| `ACP-FLW-002` | `PRD-FLW-002` | A, D | A FlowRun survives reconnect/restart with immutable inputs, grants, operations, attempts, receipts/results and terminal state; definition edits do not alter it. |
| `ACP-FLW-003` | `PRD-FLW-003` | A, D | Each non-recurring closed start policy advances once on current typed evidence; unknown/idle/stale evidence and later definition edits do not. |
| `ACP-FLW-004` | `PRD-FLW-004` | A, D, L | An authorised conductor uses every allowed operation without another prompt; expired, over-limit, wrong-attempt, disallowed path/provider and self-expansion requests fail and route exactly once. |
| `ACP-FLW-005` | `PRD-FLW-005` | A, L | Independent reviewers fan out, the synthesiser cannot start early, every result remains inspectable and disagreement/missing evidence becomes exact Attention. |
| `ACP-FLW-006` | `PRD-FLW-006` | A, D, P | Flow writers receive unique worktrees/branches; primary `main` stays checked out and switchable in the operator checkout, and cleanup never removes data without explicit authority. |
| `ACP-FLW-007` | `PRD-FLW-007` | A, D | Fuzzed terminal/prose/escape output performs zero control operations; typed endpoint calls require capability and return one correlated durable receipt. |
| `ACP-FLW-008` | `PRD-FLW-008` | A, D, M, L | During large mixed-provider fan-out, create returns after receipts and a subsequent control request, terminal keystroke and Node route meet published budgets; Turn injects no join, while any provider-internal wait is labelled without stalling siblings/UI. |
| `ACP-FLW-009` | `PRD-FLW-009` | A, D | State-machine/property tests cover every legal/illegal transition and crash point; pause starts nothing, cancel revokes grants before dispositions, abort is fenced, retry creates one bounded attempt and all provisioning survivors reconcile. |
| `ACP-FLW-010` | `PRD-FLW-010` | A, D | Deterministic clocks exercise DST gaps/folds, backward/forward jumps, sleep/reboot, duplicate ticks, overlap and each missed-run policy; stable occurrence ids run at most once and bounds make backlog finite. |
| `ACP-FLW-011` | `PRD-FLW-011` | A, D | Every Flow/Grant/delegated-operation request and response round-trips the versioned schema; stale revisions/generations and duplicate ids fail or replay one receipt, and crash-at-effect-boundary resumes only from durable saga state. |
| `ACP-FLW-012` | `PRD-FLW-012` | A, D, L | A grant matrix creates/updates each allowed Resource kind and progress record once with exact provenance/receipts; wrong author/schema/kind/target/revision, byte/node/rate/expiry excess, delete/reparent/file mutation and content-as-control all fail before effect and deduplicate one expansion demand. |

## Agent topology and adapters

| Acceptance | Requirement | Evidence | Passing oracle |
| --- | --- | --- | --- |
| `ACP-TOP-001` | `PRD-TOP-001` | A, L | Equivalent native events from every dedicated adapter produce the same source/epoch/scope/snapshot-or-delta envelope, parent and child attempts, causal operation, canonical lifecycle/turn facts and provider-only provenance. |
| `ACP-TOP-002` | `PRD-TOP-002` | A, D | Permutations, duplicates, delayed completion, reused ids and generation changes converge to one correct graph without cross-attempt mutation. |
| `ACP-TOP-003` | `PRD-TOP-003` | A, D | Process-first then structured and structured-first then process fixtures converge to one node; weaker reparenting cannot dislodge stronger provenance. |
| `ACP-TOP-004` | `PRD-TOP-004` | A, L, P | The Cartesian product of semantic/live/completed × direct/descendants × current-attempt/instance-lifetime matches an independent event manifest for proved-live, spawning, orphaned, conflicted, completed-with-no-active-attempt, nested, disconnected and unsupported cases; no oracle calls the production aggregate query and unavailable evidence never displays zero. |
| `ACP-TOP-005` | `PRD-TOP-005` | A, L, P | A PTY-less child streams bounded activity, timer/tools/tokens/result, survives view unmount/reconnect and retains completed history without a fabricated terminal. |
| `ACP-TOP-006` | `PRD-TOP-006` | A, D | At least three agent levels plus process descendants remain searchable/navigable through reconnect; persisted evidence restores honest completed/lost states. |
| `ACP-TOP-007` | `PRD-TOP-007` | A, L, P | Agent direct/descendant and Session/Workspace aggregates match queries at one graph revision and display coverage/confidence; no projection can disagree silently. |
| `ACP-TOP-008` | `PRD-TOP-008` | A, D, L | Complete empty snapshots alone produce exact zero; best-effort starts unknown, 1,025-event overflow/drop/disconnect/sequence gap immediately degrades exact to lower-bound/unknown, and a later matching asynchronous snapshot restores exactness without blocking input. |
| `ACP-TOP-009` | `PRD-TOP-009` | A, D, L | A total table-driven reducer maps every declared provider-native state, rejects unknown native values and illegal Lifecycle/Turn transitions, tests source-local ordering and comparable conflicts, correlates TurnId/revision/pending-result evidence and keeps turn completion, process exit and live children independent. |
| `ACP-ADP-001` | `PRD-ADP-001` | A | A repository guard rejects provider-name branching outside adapter packages/fixtures/docs allowlist; all behavior dispatches through the registry contract. |
| `ACP-ADP-002` | `PRD-ADP-002` | A, L | Claude Code, Codex, Gemini and OpenCode pass the same state/topology/capability suite; differences appear only as capability/evidence values. |
| `ACP-ADP-003` | `PRD-ADP-003` | A | Unknown/custom/shell fixtures always select an adapter and expose only declared semantics with explicit unsupported/degraded reasons. |
| `ACP-ADP-004` | `PRD-ADP-004` | A, L | The capability manifest and test manifest are bijective; every live-dependent advertised cell links a passing dated versioned evidence record. |
| `ACP-ADP-005` | `PRD-ADP-005` | A, D | Hung/erroring usage, transcript and hook sources for one adapter leave other adapters, selection and terminal input within latency budgets. |
| `ACP-ADP-006` | `PRD-ADP-006` | A, L | Capability-selected launch/resume/branch/switch/stop succeeds for each claiming adapter; unsupported calls fail before side effects without daemon provider checks. |
| `ACP-ADP-007` | `PRD-ADP-007` | A, L, P | For each dedicated adapter, diagnostics show detected version, effective mechanism, fresh valid/rejected events and downgrade remediation; redacted export contains no canary secret. |
| `ACP-ADP-008` | `PRD-ADP-008` | A, D | A cross-product of adapter/version, account, host, endpoint and attempt/epoch proves stale or differently scoped facts cannot enable an operation; only current capability intersection plus authority succeeds. |
| `ACP-ADP-009` | `PRD-ADP-009` | A, D, L, P | Self-test preview/cancel/timeout/crash/live cases use disposable identity, enforce resource/quota disclosure, restore hooks, remove temporary resources and leave the inspected Session byte-for-byte unchanged with redacted receipts. |
| `ACP-ADP-010` | `PRD-ADP-010` | A, D, L, P | Exactly five independent instances/conversations in two AccountProfiles share one live endpoint while input/transcript/context/Attention remain isolated; duplicate conversation ownership, sibling operations, account/generation mismatch, endpoint crash/restart/backpressure and explicit per-instance fallback converge without merge, cross-talk or duplicate launch. |

## Lifecycle, runtime and resources

| Acceptance | Requirement | Evidence | Passing oracle |
| --- | --- | --- | --- |
| `ACP-LIF-001` | `PRD-LIF-001` | A, D, P | Two views attach/detach without changing process identity; one explicit input/resize owner exists, catch-up is bounded and competing bytes cannot interleave. |
| `ACP-LIF-002` | `PRD-LIF-002` | A, D, L | Table-driven tests prove §7 identity/conversation/process/data/reversibility for attach, resume, restart, switch, branch, interrupt, terminate, kill, recycle and destroy; unsupported continuity refuses instead of changing verb semantics. |
| `ACP-LIF-003` | `PRD-LIF-003` | A, D | Concurrent and post-timeout retries with one id create at most one runtime and return the same durable created/attached/recovered/refused/uncertain receipt. |
| `ACP-LIF-004` | `PRD-LIF-004` | A, D, P | A recovery matrix independently exercises UI reload, daemon/shell restart, host reboot, local/remote reconnect and event-source loss with declared survivors. |
| `ACP-LIF-005` | `PRD-LIF-005` | A, D | Metadata restore and selection produce zero launches; verified durable handles attach; only a persisted still-valid Flow step can start once. |
| `ACP-LIF-006` | `PRD-LIF-006` | A, D, P | End/delete under unkillable, offline and racing-client conditions removes navigation, persists a resurrection fence and reports every survivor/cleanup state. |
| `ACP-LIF-007` | `PRD-LIF-007` | A, D, P | Both complete §7 event×entity tables generate tests for same-generation UI reload, client replacement, daemon/shell restart, reboot, disconnect, accepted/refused remote reconnect, source loss and destroy; every Node/instance/attempt/conversation/runtime/PTY/receipt/packet/message/Attention/grant/link/lease cell varies independently and matches exactly. |
| `ACP-LIF-008` | `PRD-LIF-008` | A, D, L, P | Complete/partial/gapped target-wide snapshots reconcile known, unknown and survivor handles; exact adopt creates one typed owner/attempt, ignore is revision/expiry scoped and exact terminate cannot touch a sibling, other host or same-named local runtime. |
| `ACP-RUN-001` | `PRD-RUN-001` | A, L | Local plus one remote backend pass the same create/attach/resize/input/signal/observe/close contract and return location-bound handles. |
| `ACP-RUN-002` | `PRD-RUN-002` | A, D, P | App restart reattaches the same process; host reboot performs only declared cold reconstruction/resume and labels lost process continuity explicitly. |
| `ACP-RUN-003` | `PRD-RUN-003` | A, D, L | DNS/host/generation/path changes and outage refuse the operation with scoped stale state; no local process/path is opened as fallback. |
| `ACP-RUN-004` | `PRD-RUN-004` | A, P | Shell, alternate-screen TUI, service and log fixtures pass every applicable `TERMINAL_ACCEPTANCE.md` case—PTY, resize, IME, clipboard/path drop, safe links, scrollback, search, keyboard and lifecycle—and display no invented agent facts. |
| `ACP-RUN-005` | `PRD-RUN-005` | A, D, P | Restore/delete of each resource kind performs zero file/network/external-open/content execution; only a typed foreground action may load or mutate its Turn record. |
| `ACP-RUN-006` | `PRD-RUN-006` | A, D, L, P | Local/remote explorer and SCM fixtures cover status/diff/stage/unstage/commit(+push)/fetch/pull/push/branch/history/conflict/discard/worktree cleanup with editable generated messages, exact repository receipts and explicit destructive review. |
| `ACP-RUN-007` | `PRD-RUN-007` | A, D, P | A matrix covers direct/Template/Flow Session, Agent, Tool and Pane create plus activate/restore/resume/restart/recycle/switch/branch, attach/adopt and legacy migration; `chdir`, absolute paths, symlink/alias/mount escape and `git -C` writes fail continuously, uncontained adoption is unmanaged, guarded read-only is classified separately, and the final scan finds zero primary-main write leases/processes/registrations/locks while `main` remains switchable. |
| `ACP-RUN-008` | `PRD-RUN-008` | A, D, L | MITM, wrong/stale host key, replay, revoked credential, key-rotation race and log/export canaries fail before effects; one real remote path proves authenticated encryption and no raw credential persistence. |
| `ACP-RUN-009` | `PRD-RUN-009` | A, D, L | Runtime, file and repository contract suites reject cross-capability use; wrong host/generation/root/revision and remote outage produce one refusal/uncertain receipt and zero mutation of a same-named local path/repository. |
| `ACP-RUN-010` | `PRD-RUN-010` | A, D, L, P | Local/remote text opens and atomic saves cover external edit conflicts, retry/merge, encoding/size/permission/offline states plus symlink/hardlink/mount/TOCTOU/root escape; no failed save overwrites bytes, falls back locally or mutates via terminal/Resource-only edit. |

## Context, communication and telemetry

| Acceptance | Requirement | Evidence | Passing oracle |
| --- | --- | --- | --- |
| `ACP-CTX-001` | `PRD-CTX-001` | A, D | Destination/current-attempt broker reads only allowlisted sources/bounds; expiry, revoke, reconnect and stale generation fail closed and leave an audit. |
| `ACP-CTX-002` | `PRD-CTX-002` | A, D | Packet preparation/delivery preserves immutable provenance/body+framing hash/review+redaction state and distinguishes queued/submitted/received/uncertain/failed; crash fixtures prove one-off review loss and no hidden body replay. |
| `ACP-CTX-003` | `PRD-CTX-003` | A, L | Oversized native transcripts yield bounded older digest + exact recent tail + separately gated artifact; receipt states complete/budgeted/partial and omissions. |
| `ACP-CTX-004` | `PRD-CTX-004` | A, D, P | An authorised Flow creates declared context with no extra dialog; out-of-scope sources produce one consolidated exact review and cancellation provisions nothing. |
| `ACP-CTX-005` | `PRD-CTX-005` | A, L | Native branch/continuation preserves verified ids; unavailable support visibly becomes portable handoff with new identity and correct lineage, never silent resume. |
| `ACP-CTX-006` | `PRD-CTX-006` | A, D, L | FIFO/TTL/capacity/idle/input-owner/generation cases use a structured endpoint and yield correlated queued/submitted/received/read/acted/expired/refused/unconfirmed states with no duplicate delivery or PTY fallback. |
| `ACP-CTX-007` | `PRD-CTX-007` | A, D | Only a valid typed result/revision satisfies an acyclic edge; idle/exit/prose cannot; authorised downstream starts once and other nodes stay blocked. |
| `ACP-CTX-008` | `PRD-CTX-008` | A, D | A cross-product authority test proves every edge/message/context body grants zero undeclared context, execution, checkout, focus or approval rights. |
| `ACP-CTX-009` | `PRD-CTX-009` | A, D | Foreground root issue/update/renew/expand/revoke plus valid `submit_delegated_operation` exercises succeed and retain issuer/grant/generation; every agent call to a direct context/packet/message endpoint and every widening/wrong source/destination/expired/post-revoke exercise fails before disclosure and routes one exact demand. |
| `ACP-CTX-010` | `PRD-CTX-010` | A, D, L | Descriptor/root TOCTOU, symlink/hardlink/mount, remote MITM/replay and control-framing fixtures fail closed; canaries in paths/files/transcripts/env/diagnostics never enter unauthorised packet/store/log sinks. |
| `ACP-CTX-011` | `PRD-CTX-011` | A, D, L | Every declared/forbidden BodyAuthority×Transport×Evidence combination, transition, byte/count/TTL/input gate and crash boundary matches the closed machine; pre-write body loss terminates, all eight post-submit evidence combinations remain independent, late exact evidence may reconcile unconfirmed without writing, overflow refuses, PTY is never used and ambiguity cannot retry. |
| `ACP-CTX-012` | `PRD-CTX-012` | A, D, L, P | Pinned Note reads return only the exact revision; reviewed-live reads accept only allowed authors/schema/revisions within cumulative budgets, audit each returned revision and surface consumers, while edit races, budget reset, redirect, revoke/read race and non-Note source fail without leakage. |
| `ACP-OBS-001` | `PRD-OBS-001` | A, L, P | Launch/switch/resume views and receipts show requested/effective/current values, warnings and evidence for every claiming adapter with secrets absent. |
| `ACP-OBS-002` | `PRD-OBS-002` | A, L, P | Context and quota fixtures with conflicting percentages/windows render separately with correct labels, scopes, units and reset semantics. |
| `ACP-OBS-003` | `PRD-OBS-003` | A, L | Multiple accounts/providers/hosts update independent rows and scopes; shared allowances never appear as per-agent facts. |
| `ACP-OBS-004` | `PRD-OBS-004` | A, M, P | Runtime view and tree status expose the exact lifecycle/turn/task/child/age/resource/unread facts at the required revision and pressure state. |
| `ACP-OBS-005` | `PRD-OBS-005` | A, P | Unknown/unsupported/stale/rate-limited/fetch-failed fixtures have distinct accessible labels and cannot render a numeric zero/current value. |
| `ACP-OBS-006` | `PRD-OBS-006` | A, D, M | Bounded independent collectors cancel/timeout/cache correctly; hidden views stop expensive polling and selection/input latency stays within budget. |
| `ACP-OBS-007` | `PRD-OBS-007` | A, D | Canary secrets in argv/env/output/provider errors are absent from protocol, UI snapshots, store, logs, diagnostics and exported telemetry. |
| `ACP-OBS-008` | `PRD-OBS-008` | A, D, L, P | Two providers, hosts and accounts exercise create/adopt/external-auth/validate/default/retire/delete plus concurrent launches; precedence and LaunchReceipt freeze are exact, defaults affect only future work, active references block delete, sibling reads of auth/config roots are denied by the declared sandbox/broker, unsupported isolation refuses cross-profile concurrency, and credential/config/transcript/conversation/quota/fallback never crosses profiles. |

## Attention and voice

| Acceptance | Requirement | Evidence | Passing oracle |
| --- | --- | --- | --- |
| `ACP-ATT-001` | `PRD-ATT-001` | A, D, L | Each structured demand routes to exact semantic subject, current attempt, interaction/result and verified owner; stale/mismatched pairs are refused. |
| `ACP-ATT-002` | `PRD-ATT-002` | A | Ordering/property tests cover the closed actionable type set, safety class, age/policy ties and independent lifecycle/turn/unread/pressure/quota axes; 100 normally working agents produce zero queue entries. |
| `ACP-ATT-003` | `PRD-ATT-003` | A, P | Shortcut, badge, desktop notification, HUD and permitted automatic focus resolve the same route/revision and land in one interaction. |
| `ACP-ATT-004` | `PRD-ATT-004` | A, D, L | Navigation/read/ack/resolve mutate only their named axis; submission remains pending through disconnect until exact provider completion evidence. |
| `ACP-ATT-005` | `PRD-ATT-005` | A, D | Heuristic/fuzz evidence can only badge/propose; only focus-worthy structured evidence reaches the governor, including after replay/reconnect. |
| `ACP-ATT-006` | `PRD-ATT-006` | A, P | Every sensitive/user-active state defers and later applies/expires the original route; queue, badge and unread revision remain intact; manual navigation wins. |
| `ACP-ATT-007` | `PRD-ATT-007` | A, D, P | Parent turn transitions and new turns do not clear a live/completed child's node, status age or unread result; only exact result rendering marks it read. |
| `ACP-ATT-008` | `PRD-ATT-008` | A, D, P | Reconnect/cross-device clients converge on one queue revision; each companion action is capability-gated and sensitive actions require declared surfaces. |
| `ACP-ATT-009` | `PRD-ATT-009` | A, D | Dedup, ageing/no-starvation, snooze/new-revision wake, dismiss-without-resolve, mute-with-badge, cooldown/manual-route and every policy inheritance field match `ATTENTION_ACCEPTANCE.md`. |
| `ACP-ATT-010` | `PRD-ATT-010` | A, D, P | Node-less/owner-less evidence opens the exact ProvisionalAttentionView; no Node/input owner is created or borrowed, and later binding/stale revisions cannot redirect or submit the original route. |
| `ACP-VOI-001` | `PRD-VOI-001` | A, D, P | Network denial, remote Session and worker crash/hang/OOM prove PCM/inference stay local and Sessions/Attention/runtimes continue unaffected; the packaged worker has no outbound socket route. |
| `ACP-VOI-002` | `PRD-VOI-002` | A, D, P | Selection/attempt/prompt/window changes never retarget a capture; zero target bytes precede explicit reviewed Insert/Send and uncertain delivery never replays. |
| `ACP-VOI-003` | `PRD-VOI-003` | A, D, P | Bad signature/digest/size/redirect/symlink/race fail before parsing; verified model works offline and inventory/export/delete accounts for all artifacts. |
| `ACP-VOI-004` | `PRD-VOI-004` | A, D, P | Spoken control/approval words, permission focus and Attention changes cannot invoke control, choose approval, retarget draft or mutate queue state. |
| `ACP-VOI-005` | `PRD-VOI-005` | A, D, P | Sandbox-denial probes prove no daemon socket/repository/credential/arbitrary-filesystem/network access; CR/LF/control/bidi inputs sanitise, and PCM/hypothesis/draft canaries are absent from protocol/store/journal/log/diagnostic/crash/export sinks. |

## Authority, collaboration, scale and quality

| Acceptance | Requirement | Evidence | Passing oracle |
| --- | --- | --- | --- |
| `ACP-SAF-001` | `PRD-SAF-001` | A, D | Missing/wrong auth, scope, operation id, revision and generation fail before side effects; exact retries return one receipt. |
| `ACP-SAF-002` | `PRD-SAF-002` | A, D | Cross-use, theft after expiry, escalation and revocation cases cannot exchange administrative/control/context/remote/companion capabilities. |
| `ACP-SAF-003` | `PRD-SAF-003` | A, D | Importing a hostile shared workspace creates no process, consent, credential/account binding or authority until an explicit local adoption receipt. |
| `ACP-SAF-004` | `PRD-SAF-004` | A, D | Disconnected/reordered clients recover from authoritative snapshot+journal; edges/scalars/immutable definitions/append-only runs/lifecycle ids follow the §14 conflict rules and deletion fences prevent resurrection. |
| `ACP-SAF-005` | `PRD-SAF-005` | A, D, P | Two simultaneous viewers cannot interleave input; 15-second lease expiry/renewal and atomic visible transfer are generation-fenced, and each unsent draft remains only on its original client. |
| `ACP-SAF-006` | `PRD-SAF-006` | A, D | Crash at every external-effect boundary yields exactly one definite/uncertain/manual state and no automatic duplicate launch/write/message/cleanup. |
| `ACP-SAF-007` | `PRD-SAF-007` | A, D, L | Forced provider/model/flag/host/context/telemetry failures show exact effective behavior or refuse; forbidden local/fresh/fabricated fallback never occurs. |
| `ACP-SAF-008` | `PRD-SAF-008` | A, D, P | Closed-inventory guard rejects unknown durable data; export/delete/retention includes every declared class and remote tombstones reach proved/pending states. |
| `ACP-SAF-009` | `PRD-SAF-009` | A, D, P | Expired/replayed invitations and scope escalation fail; encrypted read-only presence reveals no credentials and cannot type/control until an explicit visible writer/control grant. |
| `ACP-SAF-010` | `PRD-SAF-010` | A, D | Snapshot R/event R+1, ack, gap, compaction, generation, offline draft and per-object conflict fixtures converge; cursor loss forces resnapshot, stale mutations refuse and compacted deletion ids never resurrect. |
| `ACP-SAF-011` | `PRD-SAF-011` | A, D | Imports with colliding/hostile ids remint every semantic object through one package map, omit all forbidden runtime/authority ids, preserve inert origin hash and cannot mutate or resurrect pre-existing state. |
| `ACP-SAF-012` | `PRD-SAF-012` | A, D, P | Every CompanionAction passes allowed-scope/revision/expiry/receipt and offline-stale cases; free text works only for verified non-sensitive questions/decisions, every permission/credential/authority/destructive/integration action is absent/refused remotely, and writer handoff is visible before bytes are accepted. |
| `ACP-SAF-013` | `PRD-SAF-013` | A, D, P | Boundary clocks/counts enforce each §14 retention value, referenced Flow revisions survive, compacted journals retain deletion fences, ephemeral presence/drafts are absent and export/delete accounts for every retained record. |
| `ACP-SCL-001` | `PRD-SCL-001` | A, M, P | The fixed minimum profile runs 50 Sessions, 100 live runtimes and 1,000 nodes with concurrent Attention for the declared duration while all correctness and latency budgets pass. |
| `ACP-SCL-002` | `PRD-SCL-002` | A, D, M | Viewport materialisation and large-payload subscriptions remain bounded; queue saturation recovers by revision and terminal input is never blocked. |
| `ACP-SCL-003` | `PRD-SCL-003` | A, D, M | Every §13 pressure source degrades/refuses and recovers as declared, preserves every runtime absent an external OS kill and records that Turn issued no undeclared signal; proposed intervention appears as exact Attention. |
| `ACP-SCL-004` | `PRD-SCL-004` | A, P | The detailed accessibility matrix reaches every named kind/surface/action at min/max zoom, reduced motion and active IME with names/roles/order/focus/live-region oracles and non-colour status. |
| `ACP-SCL-005` | `PRD-SCL-005` | A, M | Artifacts from the fixed profile report raw workload and p50/p95/p99 route/view/input plus numeric memory/disk/queue values; every published bound independently fails on regression. |
| `ACP-SCL-006` | `PRD-SCL-006` | A, D, L | The named duplicate/stale/outliving/offline/conflict/writer/reboot/remote matrix converges without false counts, resurrection, lost Attention or duplicate effects. |
| `ACP-SCL-007` | `PRD-SCL-007` | A, D, M, P | The 30-minute sustained+burst run on the minimum profile stays within every numeric budget while individually saturating GPU/memory/PTY/fd/process/disk/journal/hook/remote/collector boundaries and retaining exact recovery evidence. |
| `ACP-SCL-008` | `PRD-SCL-008` | A, P | Packaged platform/screen-reader runs execute every row added to `ACCESSIBILITY_ACCEPTANCE.md`, including non-drag alternatives, deterministic focus restoration and companion/remote-handoff semantics. |
| `ACP-SCL-009` | `PRD-SCL-009` | A, D, L, M, P | Desktop and authenticated remote/headless clients consume identical snapshots/events/routes and perform all negotiated ordinary operations under latency/backpressure bounds; reconnect/gap/writer handoff converges, while CSRF/replay/origin/storage attacks and every desktop-only mutation fail server-side with local revocation/audit proof. |

## Critical end-to-end scenarios

The row matrix is exhaustive; these journeys catch integrations that can satisfy isolated component tests
while still failing the product.

### E2E-01 — mixed-provider delegated team

From a clean Workspace, one Flow action creates a conductor plus Claude Code, Codex, Gemini, OpenCode and a
shell/log worker in separate writable worktrees. Children appear under the correct semantic parents even
when some have no PTY. Model/mode/account/context/quota fields show effective or honest unavailable values.
Independent results arm one synthesiser. Questions and the final review reach the exact views through
Attention. The primary `main` checkout remains available throughout.

### E2E-02 — nested children and truthful counts

Authenticated versioned runs ask Claude Code to create exactly three children and Codex to create exactly
five; equivalent runs cover Gemini and OpenCode whenever they advertise the capability. One child creates a
grandchild, one is discovered through process evidence before structured evidence, one finishes while its
parent remains busy and one outlives the parent. Direct/live/total counts and tree placement stay correct
across UI reconnect and daemon restart. A claiming adapter that fails to emit its observation degrades with
the exact mechanism/remedy; a partial adapter displays `N+ observed`; an unavailable capability displays
unsupported/unknown, never zero.

The same harness starts with a complete empty snapshot and proves `exact(0)`, then injects event 1,025,
drops a start delta, disconnects/restarts the source and verifies immediate `unknown`/lower-bound. A later
matching closed snapshot restores the independently expected exact count without pausing terminal input.

### E2E-03 — portable cross-provider handoff

A large source conversation is handed to a different provider. The operator or pre-authorised Flow reviews
one target-budgeted packet containing older digest, recent exact tail and a gated full artifact reference.
The target receives it once, recapitulates without automatic execution and shows new instance plus handoff
lineage. A disconnect after possible submission becomes uncertain and is not replayed.

### E2E-04 — one attention route under active input

While the operator types in a TUI and then records a voice draft, children across providers emit a permission,
question, failure and completed result. Automatic focus is deferred without loss; ordering remains stable.
Manual Next Attention cancels capture without retargeting its draft and lands on the exact action owner.
Navigation changes no demand. Submission resolves only after the corresponding adapter evidence.

### E2E-05 — durable local and remote recovery

The UI restarts, daemon restarts, local host reboots and a remote target disconnects at each external-effect
boundary. Warm reattach preserves the same runtime; cold reconstruction is labelled separately; uncertain
effects do not repeat; remote work never launches locally. Semantic nodes, lineage, receipts and Attention
remain honest even where the runtime is Lost.

### E2E-06 — authoritative ending and cleanup

Ending a Session with an unresponsive local process, unreachable remote process and active companion removes
the Session from the active tree immediately, fences all clients and records survivors/pending cleanup in
history/status. Neither reconnect nor stale mutation resurrects it. No external worktree, branch or file is
deleted without a separate authorised cleanup.

### E2E-07 — high-volume operator loop

At the full scale envelope, the operator creates a Flow, filters/searches the tree, visits Attention, switches
Node Views, types in a noisy terminal and reviews results. Background transcripts, usage collectors, logs and
previews remain bounded. p50/p95/p99 input, route and view-switch latency plus memory/disk/queue evidence are
recorded, and resource pressure never terminates work.

### E2E-08 — setup to isolated work without trapping `main`

On a clean machine the operator skips one provider, authenticates another outside Turn, adopts a pinned
remote target and launches direct, Template and Flow work. Cancelled/failed probes leave no credentials or
resources. Every lifecycle path uses isolated worktrees, the primary checkout can switch branches throughout
and a seeded live v4 MainCheckout must be resolved before the migration/release can report success.

### E2E-09 — multi-client, companion and portable import

Two desktop surfaces and an offline companion race edge edits, Attention actions, input lease handoff and
Session deletion across journal compaction. They converge under the declared conflict rules without byte
interleaving or resurrection; stale companion actions refuse. Importing a hostile package with colliding ids
remints inert objects, grants no authority and creates no process.

### E2E-10 — shared runtime, accounts and lost-work recovery

Two isolated account profiles launch five independent instances through one multiplexed provider endpoint.
Each keeps its conversation, transcript cursor, input, context, quota scope and Attention subject. The
endpoint restarts while an unrelated target-wide inventory discovers a known handle, an unmatched survivor
and a same-named handle on another host. Exact reconciliation restores valid bindings, refuses a duplicate
conversation claim, adopts only the selected survivor and terminates only the exact target+handle; changing
the default account affects one later launch and no active instance.

### E2E-11 — living brief, delegated artifacts and remote operation

A reviewed Flow lets a conductor update one bounded Note brief and publish typed progress/resources. Linked
agents pull only allowed Note revisions within cumulative budgets; a hostile update, redirect and content-as-
control attempt fail. Desktop and a full remote surface see the same tree, board metadata, Node Views,
status and Attention revisions. A revisioned remote file edit conflicts safely with an external change;
writer handoff converges, and permission/credential/grant/publish actions remain unavailable remotely.

## Completion report

The final implementation report must list, for the exact merged commit:

1. the frozen requirement count and zero unmatched requirement/acceptance ids;
2. status and evidence artifact for every `PRD-*` row;
3. provider capability matrix with fixture and live-evidence links;
4. results for every named end-to-end journey and the recovery/security/accessibility matrices;
5. independent adversarial audit findings and dispositions;
6. CI checks and `main` commit proof.

Any unmet row is reported as a gap. It is never converted into a completion claim by changing wording,
reducing the test surface or calling an accepted design “implemented”.
