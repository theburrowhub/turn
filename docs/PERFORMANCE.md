# Performance envelope

Turn v0.1 treats this as its minimum production-shaped workload:

- 30 Workspaces and 30 active or recent Sessions;
- 120 relevant Processes (30 terminal owners and 90 semantic/background children);
- stable previews and simultaneous attention across the hierarchy;
- a 40x120 terminal receiving 1,024 consecutive screen updates.

Run the deterministic acceptance suite without opening a window:

```sh
make performance-acceptance
```

For reference-profile numbers from an optimised build:

```sh
cargo test -p turn-gui --test performance --release -- --nocapture --test-threads=1
```

The fixture is built from the real domain model, protocol projections, `TurnView`,
egui harness and `PaneFeed`. It does not substitute a flat list or a fake terminal.

## CI budgets

| Dimension | Budget / invariant |
| --- | --- |
| Workload shape | exactly 30 Workspaces, 30 Sessions and 120 Processes |
| Session switch | p95 below 50 ms in a debug CI build; settles in at most 6 frames |
| Terminal output | p95 below 5 ms for one 40x120 update; 1,024 updates below 3 s CPU/wall |
| Terminal input | bounded non-blocking enqueue p95 below 1 ms |
| Other terminal | one update below 20 ms after the noisy-terminal burst |
| Hierarchy projection | below 1.5 MiB serialised (reference: 190,544 bytes) |
| Lazy tree | at most 17 of 180 rows built for a 500 px viewport, plus an explicit reveal target |
| Preview cadence | watched panes at 500 ms; background panes at 1,500 ms; neither starves |
| GUI queues | 64 inbound messages, 256 outbound intents, 512 written requests awaiting replies; native-dialog/companion queues hold 1 |
| Terminal memory | 60 MiB raw-ring cap for 30 terminals; 492 MiB worst-case image stores/cache; combined below 600 MiB |
| Terminal disk | 360 MiB across 30 default journals plus checkpoints |
| GUI history | 5,000 compact rows per attached pane |

Wall-clock limits deliberately leave headroom for shared CI runners. Shape, queue,
retention, cadence and lazy-render limits are deterministic and fail independently
of runner speed.

## Reference profile

Recorded 2026-08-11 on a MacBook Pro `Mac16,5`, Apple M4 Max (16 cores),
64 GiB RAM, macOS 26.5.2, Rust 1.97.1, aarch64. The release harness reported:

- Session switch p95: 320 µs; 7 ms process CPU for 30 switches; one settle frame;
- output apply p95: 55 µs; 53 ms process CPU/wall for 1,024 updates;
- update of the quiet terminal after the burst: 9 µs;
- input enqueue p95: below the timer's 1 µs resolution;
- process peak RSS after constructing/running the fixture: 32 MiB;
- hierarchy projection: 190,544 bytes.

`getrusage(RUSAGE_SELF)` supplies process CPU and peak RSS on macOS/Linux. The
latency clock is `std::time::Instant`; the test emits every measurement with the
`turn-performance` prefix for collection in CI logs.

## Profiles before and after

The exact debug harness was run before and after each hot-path correction on the
reference machine:

| Bottleneck | Before | After | Change |
| --- | ---: | ---: | ---: |
| Hierarchy built and painted all 180 expanded rows | Session switch p95 12,273 µs | 6,121 µs with viewport + 120 px overscan | -50.1% |
| `PaneFeed::apply` cloned the complete grid twice per update | output apply p95 347 µs | 181 µs with one atomic recovery clone | -47.8% |
| GUI transport and one-shot worker channels were unbounded | no finite memory ceiling | 64 inbound / 256 outbound / 512 awaiting reply / 1 one-shot | finite by construction |

Lazy rendering still traverses the compact row keys for ordering, search and keyboard
navigation; only off-viewport text layout, painting, controls and accessibility nodes
are deferred. An off-screen search/keyboard reveal target is always materialised before
scrolling, so the optimisation does not remove functionality.

## Backpressure and retention audit

All production queues crossing a GUI thread boundary are bounded. If the inbound
transport queue fills, Turn drops that connection and reconnects for a fresh revisioned
projection instead of retaining stale screens indefinitely. If outbound intent capacity
is reached, the UI remains non-blocking and returns a retryable rate-limit failure for
user-visible actions. The daemon's socket writer, command queue, client frames, Agent
hook events and PTY broadcast were already bounded.

Only the selected Session's panes are attached and painted. PTY output is coalesced for
8 ms by the daemon; screen updates are row diffs unless a full grid is cheaper. Preview
sampling is rate-limited, preview history is capped by the store, terminal byte/history
rings rotate, and terminal journals checkpoint before their per-pane disk ceiling.

To repeat the channel audit:

```sh
rg -n 'unbounded_channel|mpsc::unbounded|Unbounded(Sender|Receiver)' crates/turn-gui/src
```

The expected production result contains no unbounded channel constructor.

## Accepted control-plane envelope

**Status:** post-v0.1 target; the v0.1 measurements above do not prove it.

M17 expands the production-shaped workload to 50 concurrent Sessions, 100 live runtimes, 1,000 total
expanded/historical nodes, nested child events across four dedicated adapters, simultaneous Attention,
independent usage collectors and one noisy terminal/log stream. It records p50/p95/p99 rather than a single
average. ADR-063 adds this exact non-optional sub-fixture:

- five independently bound AgentInstances share one RuntimeEndpoint: three freeze AccountProfile A and two
  freeze AccountProfile B, each has a unique conversation owner, all five concurrently exchange input,
  ContextPacket, transcript and Attention traffic, and the endpoint crashes/restarts once;
- one target-wide RuntimeInventory contains 100 known live handles, 1,900 unmatched handles and a subsequent
  partial/gapped generation while reconcile/adopt/ignore/terminate previews are computed;
- 500 canonical WorkItems render as table, board and search projections while 50 conflict revisions arrive;
- 20 Notes each retain 10 immutable revisions, five are pinned and five are reviewed-live ContextLink briefs;
- 500 delegated Resources and 5,000 progress replacements are validated, projected and compacted without
  interpreting their content as control;
- 16 clients hold 1 MiB FileBackend snapshots while an external write forces one atomic-save conflict and
  merge/retry sequence; and
- one local desktop, one full remote GUI, one headless client and one reduced companion remain connected at
  the same time, receive the same revision/Attention stream permitted by their scopes and exercise writer
  handoff. Desktop-only operations remain rejected on every remote surface and count as correctness, not
  successful throughput.

ADR-064 adds these independent non-optional sub-fixtures to the same run:

- 1,000 foreground Session reselections mix existing runtimes, stopped restorable children, empty Sessions
  that resolve a default Shell and preflight refusals; no case waits for a second operator action and no
  reconnect/preview/background selection launches a process;
- four external WorkItemSources deliver the 500 WorkItems through bounded pages, duplicated/out-of-order
  deltas, one expired cursor, one rate limit, close/reopen and 50 compare-and-swap conflicts while preserving
  a visibly stale cache;
- 200 provider-native jobs and 2,000 bounded historical/current iterations update independently from 100
  Turn Flow recurrences, including provider restart, dismiss, cancel, disable and delete receipts;
- four profile-scoped ConversationInventories expose 10,000 metadata-only rows through search/pagination;
  exact adopt/resume, duplicate ownership and stale/gapped results run concurrently with title-read and rename
  collectors that degrade independently;
- eight inert Web previews and eight isolated Browser nodes exercise cached switching, reviewed public,
  localhost and local-HTML navigation, redirects, history and blocked popups without loading content into the
  hierarchy projection; and
- each Companion profile receives context/quota samples and a 1,000-row bounded activity inbox; ten recognised
  remote permission prompts are answered through the typed encrypted path while raw PTY bytes are refused.

The release gate uses a fixed minimum reference profile: base Apple M1 (8-core), 8 GiB RAM, internal SSD,
supported macOS, 1,920×1,080 display, AC power, no swap at start and an optimised packaged build. The same
workload is recorded on each claimed Linux platform, but a faster developer or CI host cannot replace the
minimum-profile artifact. After a five-minute warm-up, the harness runs 30 minutes continuously and injects
a 10-second topology/Attention/output burst every minute. `Turn-owned` includes daemon, every GUI/client,
SpeechWorker, isolated Browser renderer, remote transport helper, provider broker/collector and watchdog
spawned or retained by Turn. Managed user/agent processes, external provider-owned services/caches and
checkout data are reported separately and excluded from Turn's own RSS/disk budget; no Turn helper may be
misclassified into those exclusions.

Remote measurements pass through a deterministic shaping proxy between client and daemon: 40 ms round-trip
latency, a repeating 0/5/10/5 ms packet-delay pattern (10 ms maximum jitter), every 100th packet dropped,
20 Mbit/s downstream, 5 Mbit/s upstream and a 32 KiB transport-write ceiling. The proxy seed is fixed in the
artifact. Reported remote p95/p99 includes shaping, TLS/authentication, serialization, daemon routing and
client application; local daemon timestamps alone cannot satisfy it. Full-remote, headless and companion
traffic each use a separate connection and queue. A test run with shaping disabled is diagnostic only.

WorkItemSource and provider collector fixtures use separate deterministic integration endpoints behind the
same network profile; their end-to-end timers include adapter transport, authentication, parsing and durable
receipt. This is a repeatable Turn overhead budget, not a claim that an arbitrary live provider meets it. Live
evidence records provider latency separately and cannot replace the deterministic minimum-profile run.

| Dimension | Target budget / invariant |
| --- | --- |
| Terminal input enqueue | p99 below 1 ms; never waits for child creation, topology, usage or transcript work |
| Attention route | daemon decision plus client application p95 below 50 ms and p99 below 100 ms on the minimum release profile |
| WorkSurface switch | p95 below 50 ms and p99 below 100 ms on the minimum release profile for cached/metadata views; heavy content arrives by subscription |
| Foreground Session activation | selection-to-cached-attach p95 below 50 ms/p99 below 100 ms; selection-to-durable start/refusal receipt p95 below 100 ms/p99 below 200 ms. Child readiness is asynchronous and timed separately; the UI never blocks and duplicate selection produces at most one attempt |
| Remote Attention route | exact demand creation through full-remote/headless receipt p95 below 150 ms and p99 below 300 ms under the declared network profile |
| Remote input/event | authenticated input-to-PTY enqueue and revision-event application each p95 below 125 ms and p99 below 250 ms; wrong lease/revision/profile has zero enqueue |
| Remote WorkSurface switch | cached metadata view p95 below 175 ms and p99 below 350 ms; first bounded heavy subscription p95 below 400 ms and p99 below 800 ms |
| Headless mutation receipt | ordinary negotiated mutation through durable receipt p95 below 150 ms and p99 below 300 ms; desktop-only refusal uses the same ceiling |
| Remote reconnect | authenticated reconnect, gap detection and bounded resnapshot p95 below 2 s and p99 below 5 s; no mutation is replayed before convergence |
| Flow fan-out | create call returns after durable operation receipts, never after child completion; parent and later operations remain interactive |
| Shared endpoint | all five binding queues make progress; per-binding input/event p99 below 250 ms and endpoint restart convergence p99 below 5 s with zero cross-profile/conversation bytes |
| Target inventory | 2,000-handle complete/partial/gapped reconciliation p95 below 100 ms and p99 below 200 ms CPU, below 16 MiB serialized, without blocking input or claiming exactness after a gap |
| Board/Note/Resource | each projection/update p95 below 50 ms and p99 below 100 ms; progress compaction p99 below 200 ms; content bodies never enter the hierarchy snapshot |
| External WorkItemSource | apply and project one 500-item page p95 below 100 ms/p99 below 200 ms locally and below 300/600 ms over the shaped remote source; filter/cursor/rate-limit/conflict handling never blocks input or turns stale cache into an empty exact result |
| Native jobs | apply a 200-job/2,000-iteration snapshot or delta burst p95 below 100 ms/p99 below 200 ms; Flow recurrence remains independently responsive and no dismissed history is fetched/rendered eagerly |
| Conversation inventory | cached metadata search over 10,000 rows p95 below 75 ms/p99 below 150 ms; one bounded provider page including shaping p95 below 400 ms/p99 below 800 ms; cancellation/reselection prevents an old page or title from replacing the current view |
| Browser/Web | inert Web or cached Browser WorkSurface switch meets the normal switch budget; navigation dispatch never blocks GUI/input, history is bounded, and one noisy page cannot delay terminal input or Attention beyond their budgets |
| Remote typed permission | exact encrypted response through durable accepted/refused/uncertain receipt p95 below 175 ms/p99 below 350 ms under shaping; stale/grantless/raw-PTY attempts meet the same refusal ceiling and enqueue zero bytes |
| Title read/rename | a title observation or rename receipt applies p95 below 50 ms/p99 below 100 ms after adapter receipt; a slow title collector never delays launch, conversation inventory, tree selection or local alias editing |
| Companion profile inbox | one 1,000-item activity page plus usage/context cells applies p95 below 100 ms/p99 below 200 ms; profile switch p95 below 50 ms/p99 below 100 ms from cache; expired/unavailable cells never wait to fabricate a value |
| FileBackend conflict | 1 MiB local/remote snapshot open p95 below 100/400 ms and p99 below 200/800 ms; conflict detection/receipt p95 below 150 ms and p99 below 300 ms with zero overwritten bytes |
| Hierarchy projection | below 8 MiB serialized at 1,000 nodes; no terminal/transcript/log/media body in the snapshot |
| Lazy tree | only viewport + fixed overscan + explicit reveal target materialise text/paint/accessibility rows |
| Heavy subscriptions | selected visible subject plus an explicit bounded preview set; reselection cancels old generation |
| Turn-owned memory | combined RSS of every Turn-owned process above is at most 1.5 GiB; growth from minute 10 to minute 30 is at most 128 MiB; one SpeechWorker is at most 512 MiB, one active Browser renderer 256 MiB and each remote/broker/helper 128 MiB, all still counting toward the aggregate |
| Turn-owned disk | at most 1 GiB growth during the run, excluding checkouts/provider caches; retention/compaction stabilises growth |
| Queues | GUI inbound/outbound/awaiting remain 64/256/512; topology is 1,024 per source; message is 256 items and 1 MiB per destination; each remote connection is 256 frames or 8 MiB and each frame is at most 256 KiB; every other boundary is at most 4,096 items or 16 MiB and declares refusal/gap/resync |
| Network volume | after warm-up, Turn protocol egress averaged across the 30-minute run is at most 5 Mbit/s and p99 one-second egress at most 15 Mbit/s; payload bodies count, TLS framing does not |
| Resource pressure | may coalesce, slow preview or park reconstructible views; never terminates/suspends live work |
| Evidence | workload, hardware, commit, build profile and raw p50/p95/p99/memory/disk/queue results are retained |

The harness injects a slow provider collector, a 1,025-event topology overflow, shared-endpoint binding
backpressure, an inventory generation gap, Note edit/revoke races, delegated Resource/progress overflow,
board conflicts, FileBackend external-edit conflict, view failures and GPU/GUI memory, PTY, file-descriptor,
process-limit, store/disk/journal, shaped-network and collector pressure while the operator switches nodes
and types on all four surfaces. Every latency population has at least 10,000 observations (reconnect has 100),
uses nearest-rank percentiles and reports misses/timeouts as infinite latency; warm-up samples are excluded.
It fails on false zero counts, stale-route application, dropped Attention, duplicate effects, cross-binding or
cross-profile bytes, duplicate auto-starts, background-selection launches, hidden WorkItemSource or
conversation gaps, Flow/native-job conflation, stale title overwrite, remote permission PTY fallback,
cross-profile Companion samples, unbounded Browser/history growth, overwritten files or runtime termination
as well as on latency regression.

Passing performance never substitutes for semantic acceptance. The complete cross-gate obligations are
`ACP-FLW-008`, `ACP-ADP-005`, `ACP-VIE-011`, `ACP-FLW-012`, `ACP-ADP-010`, `ACP-LIF-008`,
`ACP-RUN-010`, `ACP-CTX-012`, `ACP-OBS-008`, `ACP-SCL-001` through `ACP-SCL-009`, `ACP-LIF-009`,
`ACP-VIE-012`, `ACP-ATT-011`, `ACP-ADP-011`, `ACP-CTX-013`, `ACP-RUN-011`, `ACP-OBS-009` and `ACP-SCL-010`;
each named ACP must pass
its functional, durability, loss, migration and privacy dimensions independently before these measurements
can be release evidence.
