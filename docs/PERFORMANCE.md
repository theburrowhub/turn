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
expanded/historical nodes, nested child events across six dedicated adapters, simultaneous Attention,
independent usage collectors and one noisy terminal/log stream. It records p50/p95/p99 rather than a single
average. ADR-063 adds this exact non-optional sub-fixture:

- ten independently bound AgentInstances share one RuntimeEndpoint: five use deliberately account-absent
  `endpoint_unscoped` scopes and five use profiled scopes (three AccountProfile A, two AccountProfile B).
  Each has a unique conversation owner, all ten concurrently exchange only their authorised input,
  ContextPacket, transcript and Attention traffic, and the endpoint crashes/restarts once. Unscoped bindings
  create zero AccountProfile/quota/activity/inventory state;
- AccountAuthenticationIntent metadata independently fills10,000 small records or exactly8,192 maximum≤4-KiB records/32 MiB while Turn-created roots exercise the
  64-MiB item/2,048-MiB aggregate bounds; each N+1 prelaunch refusal and correlation-only crash recovery stays
  asynchronous and cannot delay launch on another profile;
- one target-wide RuntimeInventory contains 100 known live handles, 1,900 unmatched handles and a subsequent
  partial/gapped generation while reconcile/adopt/ignore/terminate previews are computed;
- 500 canonical WorkItems render as table, board and search projections while 50 conflict revisions arrive;
- 20 Notes each retain 10 immutable revisions, five are pinned and five are reviewed-live ContextLink briefs;
- 500 delegated Resources and 5,000 progress replacements are validated, projected and compacted without
  interpreting their content as control;
- 16 clients hold 1 MiB FileBackend snapshots while an external write forces one atomic-save conflict and
  merge/retry sequence; 256 active FileSaveIntents fill the 2,048-MiB temporary reservation envelope and the
  next count/byte/terminal-slot request refuses before byte one while crash reconciliation remains lookup-only;
  synthetic boundary fixtures fill 16 snapshots/client, 128/1,024 MiB globally and exercise close/loss/expiry
  release without reading on N+1;
  and
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
  Turn Flow recurrences across separate schedule/iteration/presence/projection/reconciliation axes, including
  correlated create/run-now recovery, provider restart, local activity-hide/forget/restore, exact iteration
  cancel, pause/resume and delete-provider-job receipts;
- four profile-scoped ConversationInventories expose 10,000 metadata-only rows through search/pagination;
  exact adopt/resume, duplicate ownership and stale/gapped results run concurrently with title-read and rename
  collectors that degrade independently, while independently10,000 small or exactly8,192 maximum≤4-KiB ConversationRenameIntents/32 MiB exercise owned/
  unowned correlation-only recovery and exact pre-effect N+1 refusal;
- eight inert WebPreview nodes and isolated Browser nodes exercise cached switching, reviewed public, localhost
  and local-HTML navigation, redirects, history and blocked popups without loading content into the hierarchy
  projection. WebPreview independently fills32 nonterminal intents,10,000 rich≤4-KiB receipts,100,000≤512-byte
  replay fences and the combined64-MiB metadata family (including the exact10,000-receipt+51,072-fence maximum-item
  fixture), plus32 live states,256-MiB bodies and eight renderers/256 MiB; it exercises10 redirects,8-MiB
  transfer, 16-MiB decode, 20:1 expansion, 30-second fetch and 15-minute release with zero reconciliation
  refetch. The run fills512 Browser creation intents, independently10,000 small or8,192 maximum records/32 MiB
  plus100,000 small or98,304 maximum creation replay fences/48 MiB;256 active navigation intents,
  100 receipts/Node, 10,000 receipts/128 MiB and 100,000 replay fences; eight≤256-MiB renderers within 1,024 MiB;
  100 history entries/Node, independently10,000 small or8,192 maximum entries/64 MiB with≤8-KiB entries and
  4-KiB/2,048-scalar URLs; and≤128-MiB
  partitions within 512 MiB. It crashes each intent
  boundary and proves correlation-only recovery/no redispatch. Thirty-two local snapshots fill 256 MiB and 32
  download quarantines fill their active/4-GiB-shared envelope; each exact N+1 refusal and quarantine-to-ticket
  zero-copy handoff runs under renderer and shared-memory limits;
  and
- each Companion profile receives context/quota samples and a 1,000-row bounded activity inbox; ten recognised
  remote permission prompts are answered through the typed encrypted path while raw PTY bytes are refused.

ADR-065 adds these independent non-optional sub-fixtures:

- a generated admission fixture reaches each 1,024-Workspace, 10,000-Session, 100,000-Node, 50,000-AgentInstance,
  10,000-live/100,000-detail RuntimeAttempt and 4,096-Pane family cap independently, then fills the mixed
  200,000-record/1,024-MiB semantic-core envelope with maximum 64-KiB records and 256-KiB Layouts. It measures
  deterministic pre-effect N+1 refusal, eligible Attempt-detail folding, external coverage gap and post-fence
  release without materialising every body in memory. At the 100,000-Node boundary the compact hierarchy index
  remains≤6 MiB, visible summaries use≤500-row/1-MiB automatic pages, exact reveal materialises one target within
  1 MiB and a 4,097-operation/1-MiB-overflow delta yields a scoped gap rather than a full replacement. A separate sparse-file/accounting fixture fills 256
  journaled Panes and the 2,048-MiB journal/1,024-MiB checkpoint physical pools, proving the 257th launch does
  nothing and replacement does not double-count. A quota-backed PhysicalDiskLedger run independently fills
  all ten 8/4/3/2/2/2/8/100/4/2-GiB classes and 135-GiB total, while small real files cover sparse/COW/copy/
  rename/unknown-root accounting without requiring that physical capacity on CI;
- 1,000 Nodes include a 128-level Group chain plus 250 nested siblings; subtree move/promote/delete, a
  concurrent cycle race and one corrupt persisted cycle remain bounded, while 50 Session-owned CheckoutScopes
  run create/adopt/missing/unbind/remove reconciliation without blocking or registering primary `main`. Four
  active Surfaces/connection, eight live+dormant/owner and all 64 installation records fill 16 MiB with maximum
  expanded/manual-order/filter fields; mint/resume/retire/30-day-expiry and each N+1 remain bounded;
- the CheckoutFenceRegistry reaches 100,000 live-or-uncompacted records/64 MiB and 100,000 Turn-owned lock
  inodes; one additional writer/worktree/lock request refuses pre-effect, and terminal compaction plus exact
  ownerless-lock sweep stays asynchronous without delaying primary-main checks or repository reads;
- 10,000 RepositoryMutationIntents fill the 256-MiB envelope across local and hosted verbs; N+1 refusal,
  exact-OID/ref reconciliation and every commit×push suboutcome remain asynchronous and never repeat Git or
  network effects;
- the same hierarchy cycles collapse/expand, filters, variable row heights, resize/zoom and 100 concurrent
  topology revisions while automatic layout produces no overlap, extra domain revision or focus jump;
- all six dedicated adapters execute their 23-cell capability matrices concurrently; Kimi and MiniMax quota-
  only connectors add independent profile samples while a slow/failing cell cannot delay another adapter;
- 32 ModelEndpointProfiles each expose 10,000 bounded mapped model rows, with endpoint revision churn, one
  oversized discovery, missing-secret refusal and launch/switch receipts under the shaped network;
- the existing 2,000-handle target inventory includes 10,000 reuse-safe process rows, host RAM/swap/pressure,
  current/closed/unmatched attribution and one failed remote collector without local fallback or false zero;
- 64 notification endpoints plus 32 grants each/2,048 total fill their bounds; independently10,000 small or exactly8,192 maximum≤4-KiB control records/32 MiB,
  a 10,000-item/16-MiB encrypted collapse-aware burst and 100,000 terminal receipts/256 MiB exercise each
  count/byte N+1, exactly eight attempts with≤15-minute backoff, presence release, offline retry, pairing crash/
  lookup-only reconcile, retire/revocation during batch and monotonic live start/update/end while
  NotificationHostMode exposes zero inbound listener; and
- 1,000 bounded name proposals plus 100 simultaneous new/open/clone/SSH WorkspaceOnboarding operations cover
  stale proposal, source redaction and cancellation/partial clone reconciliation. Separate publication fills
  256 nonterminal RepositoryPublishIntents, independently10,000 small or8,192 maximum records/64 MiB plus
  100,000 small or98,304 maximum replay fences/48 MiB, exercises every phase/crash/reconcile
  boundary without redispatch and keeps primary `main` unoccupied.

ADR-066 adds these independent non-optional sub-fixtures to that same 30-minute run:

- one confined Directory source exposes 2,000 entries while 32 watches/connection and all 2,048 global watches
  receive rename/delete/overflow events; each N+1 pre-subscription refusal and unwatch/gap/invalidation/
  disconnect release remains bounded. The eight ordinary subscription families fill64/connection,4,096
  global, 16-MiB metadata and 64-event/1-MiB per-subscription, 256-event/8-MiB per-connection, 4,096-event/64-
  MiB global queues plus shared RSS; duplicate/replacement/gap/release/reconnect stays bounded. One repository traverses 10,000 commit ids and changed-file pages, and terminal plus UTF-8
  document searches hit exactly 10,000 matches and the 1,000,000-cell/100,000-line/16-MiB scan boundaries;
- 32 concurrent MediaImports include one exact 256-MiB boundary item and a decoder-bomb refusal, while selected
  Media playback fills one/Surface, four/connection, 32 installation-wide and≤64-MiB/item/512-MiB family
  decoder state while a different Node keeps switching; temporary+blob ownership fills one Workspace 10,240-
  MiB physical pool without double count, import/playback count/family/shared-byte/10,000-receipt N+1 refuses
  pre-read/chunk/spawn/decode and decoder frames/caches never enter snapshots;
- 64 RepositoryHostProfiles,64 installation-wide WorkItemSources, independently1,000,000 small or983,040
  maximum≤512-byte/480-MiB WorkItemKeyRegistry entries and all10,000 source-operation slots, plus64 CommitProposalProviderProfiles with
  current plus 31 historical revisions, 1,000 live proposals and all 10,000 retained proposal Attempts,
  independently fill10,000 small or8,192 maximum RepositoryHostCredentialIntents/32 MiB,128 active grants/profile and100,000 terminal grants/
  256 MiB; authenticate/rotate correlation recovery and separate RepositoryBackend/WorkItemSource grants,
  cross-Workspace source/key/operation admission,
  tombstone folding, executable and broker profiles, descriptor/policy churn, sandbox canaries, timeout,
  process/RSS/output limits, compaction eligibility and stale-index refusal;
- all 32 active TransferTickets cover cross-target, client-stream and Browser-to-File-Resource endpoints with
  chunk/backpressure/cancel/reconcile races; one virtual streamed source reaches the 2-GiB boundary without a
  2-GiB resident buffer and temporary-byte accounting is sampled independently;
- all four ContentProjections/client and 64/128 MiB installation-wide switch plain/Markdown and atomically
  replace/clear/invalidate without eviction on N+1 while one 10,000-entry CommandCatalogue is paged, searched,
  shortcut-conflicted and invoked; renderer and catalogue queues saturate independently;
- all 16 PortableExports and 16 PortableImports reserve exact 64-MiB packages to fill their shared 2-GiB
  temporary cap while destination revision, cancellation, crash/reconcile and 10,000-receipt compaction race;
  N+1 reads, writes and remints nothing;
- 100 product announcements exercise signature/revocation/high-water and link review, while one current update
  intent, 100 terminal receipts and the exact 2-GiB combined temporary/downloaded/verified/staged allocation
  exercise query/evidence bijection, concurrent discovery refusal, capacity reservation, cancellation/crash,
  manifest/package substitution and compaction. Apply/rollback runs exactly 30 times on disposable
  installation roots rather than the general latency-population count;
- one WorkItem receives 10,000 ordered activity events and one Workspace fills all 200 PresentationHistory
  entries while concurrent source updates, compaction, undo/redo ownership and gap/resnapshot execute.

ADR-067 adds these independent non-optional phases to the same run:

- the current-daemon diagnostic ring fills2,048×4-KiB rows/8 MiB while4,096 source clear-high-water rows/1 MiB,
  a256-row/1-MiB page, the eighth live subscription family and independently10,000 small or8,192 maximum
  clear receipts/32 MiB exercise overflow-gap, filter/copy, source/all clear, delayed producer, reconnect and
  restart non-resurrection. Canary scanning and clear never raise terminal-input p99 above1 ms or Attention
  routing p99 above100 ms;
- the frozen23-section/2,048-definition settings registry drives sidebar/search/deep-link over all five
  Global/Workspace/Template/Session/Temporary sources. Persistent settings independently fill16,384×64-KiB
  records/1,024 MiB,64 simultaneous1-MiB reset previews/64 MiB, and100,000 small or98,304 maximum receipts/
  384 MiB. Count/item/family/stale-revision/apply/cancel/expiry N+1 changes no unrelated key and UI search/
  selection remains within the common WorkSurface latency budget;
- 64 one-MiB local report drafts fill64 MiB while independently10,000 small or8,192 maximum review receipts/
  32 MiB exercise edit/redact/discard/expiry/stale-source, clipboard-only, reviewed isolated-Browser open and
  create-new FileBackend export. Preparation allocates no network/file/Browser state and report-body canaries
  never enter URL, receipt, log, diagnostic or crash data; and
- 128 encrypted ephemeral collaborator messages fill128 KiB at512 body bytes/256 scalars under four sends/
  10 seconds and500-ms spacing. Replacement/retract/30-second expiry/disconnect races emit exact tombstones,
  survive no reconnect and create zero AgentMessage, ContextPacket, input, StatusEvent or Attention mutation;
- document phases independently fill64×64-KiB view states/4 MiB,64 source blobs/512 MiB, two256-MiB decoder
  high-waters/512 MiB,256 page tiles/512 MiB and64 text indexes/64 MiB under the shared-RSS cap. Page, zoom,
  rotation, search, source-change, decoder-bomb, close and cleanup remain responsive;32 print intents plus
  10,000 small or8,192 maximum receipts/32 MiB and two64-MiB spools/128 MiB exercise prepare/cancel/native-
  dispatch/reconcile without repeat printing or retained source/page bytes;
- 64 simultaneous64-KiB terminal clipboard gestures fill4 MiB while typed copy, paste, X-primary middle-click
  and128-path drop race focus, attachment, grid, InputLease, InputSafety, cancel and30-second expiry. Ordinary
  input remains p99<1 ms, OSC 52 read/write storms produce zero clipboard access/response and remote targets
  admit zero body;
- 128 two-KiB AttentionAudioCues fill256 KiB under16/client, eight/10-second and two-second/per-subject limits.
  One hundred concurrent agents emit duplicate/stale/current `done|needs_you` edges; supported cues begin≤300
  ms, muted/late/replayed cues emit zero audio and neither audio failure nor saturation changes Attention;
- 64 revision-pinned bulk previews fill16 MiB and each reaches256 candidates. Independently64 active intents,
  10,000 small or2,048 maximum overall records/128 MiB and100,000 small or65,536 maximum per-instance receipts/
  128 MiB drive sequential restart, cancellation and crash at every boundary with exact final accounting and
  no duplicate attempt. Eco independently fills64×128-KiB scheduler queues/8 MiB and64 nonterminal plus10,000
  small or8,192 maximum records/32 MiB while a deterministic clock proves≤2 exits/minute, stale eligibility
  exclusion, automatic wake and no input/Attention latency breach;
- agent-controlled Browser phases fill256 active grants/2 MiB,256 nonterminal actions,10,000 small or8,192
  maximum action records/64 MiB and100,000 small or98,304 maximum replay fences/48 MiB. Create/navigate/read/
  click/type, grant expiry, visible Stop, page-revision race and renderer crash stay inside the existing Browser
  worker/partition/shared-RSS caps; a noisy agent cannot starve human Browser or terminal work;
- Browser Memory Saver independently fills10,000 small states and8,192 maximum4-KiB states/32 MiB while a
  deterministic clock crosses the exact five-minute boundary. Every loading, audible, agent-controlled,
  popup, download, print, action and dirty form/POST exclusion is toggled at the final eligibility edge;
  discard releases renderer/partition/shared charges only after quiescence, and reselection produces one fresh
  BrowserNavigationIntent or one typed blocked reason. Hidden/visible churn, policy drift and crash never
  double-load, retain private page bytes or report fictitious memory savings;
- all2,048 subsets of the eleven closed optional-control ids resolve across Global/Workspace/Template/Session/
  Temporary precedence, minimum/normal/maximum viewport and300-percent zoom. Unknown, duplicate and every
  critical-control injection stay visible; hiding an allowed slot leaves palette/keyboard invocation within
  the common command and focus budgets and allocates no state outside the existing SettingsRecord;
- PTY capacity fills256 current observations/1 MiB and256 monitor states/1 MiB while the one-minute sampler,
  two-minute freshness edge, exact80-percent/required-headroom boundaries and five-minute reminder dedupe race
  target reconnect and spawn. Independently64 nonterminal remediations and10,000 small or8,192 maximum
  records/64 MiB plus100,000 small or98,304 maximum replay fences/48 MiB exercise review, privilege dispatch,
  kernel apply, persistent write, verify, rollback and every crash boundary. Sampling/pressure never starves
  terminal input or Attention, and intent/fence N+1 causes zero privileged effect;
- endpoint continuity fills64 bindings on one RuntimeEndpoint,128 five-second verification buffers at
  256 KiB/32 MiB,100,000 small or65,536 maximum 4-KiB receipts under256 MiB and then each independent N+1.
  Canonical 64-claim HMAC verification with one invalid claim commits63 current+one stale in one bounded
  transaction; invalid root leaves64 stale. Rotation, replay, crash and profile-rebind contention remain within
  the control/input/Attention budgets and never serialize one bad claim into sibling outage;
- terminal parking drives exactly twelve warm renderer parks/Surface plus a refused thirteenth, exact five-/ten-minute
  expiry and 256 local park records while output and Attention continue. Isolated phases fill128 zero-PTY
  shadow observers/target and256 global at128-KiB state/8-MiB control bounds, then64 single-target background-
  write channels at1 MiB. Painter↔shadow↔writer swaps, sequence gaps, capture/resync, five-second timeout,
  thirty-second linger, handle loss and automatic selection/Attention attach preserve the same Attempt, flush
  at most4,096 exact-lease bytes once and keep PTY-device count unchanged. All-protected/cache/process/shared-
  RSS N+1 yields without signalling work. A signal/kill/delete audit under memory, PTY, count and elapsed-time
  pressure records exactly zero automatic detached-session reaps; only a separately enabled eligible Eco
  fixture may exit a process;
- four exact profiled provider/target namespaces build encrypted private transcript indexes from10,000
  identity-pinned synthetic documents each. Isolated count and byte phases hit5-MiB source reads,200-KiB
  encrypted normalised segment tails/document plus postings,512-MiB/profile-target and1-GiB installation/
  account-root caps, eight refreshes,256 queued sources, two queries/Surface/32 global and20-hit/80-KiB pages.
  Historical-view phases fill one buffer/Surface,16 global and2-MiB/item/32-MiB family together with first-page
  outbox/chunk reservations; N+1 leaves the old Surface byte-identical and a committed CAS has its first page.
  Incremental change, unreadable/oversize/alias/device/
  remote-local-fallback input, parser revision, partial/gapped coverage, query cancellation, key revocation,
  generation swap and deletion never cross profiles, report false empty, retain a raw page or delay input/
  Attention; selected hits reveal only their canonical read-only ViewTarget; and
- dependency-gated Flow phases publish4,096 immutable results/run and independently100,000/256-MiB results
  installation-wide, then fill10,000 FlowOperationReceipts/run. Success, final failure, retry, same-transaction
  any-of tie, stale/duplicate/missing/deleted producer and reconnect prove one embedded step-readiness receipt
  can ready/start one preassigned StepAttempt while idle/done/text and N+1 start none;
- 64 active Companion launch grants/2 MiB and64 simultaneous launch intents fill10,000 small or8,192 maximum
  records/64 MiB. Shaped-network duplicate/disconnect/revoke and crash before checkout, launch and graph
  registration produce one canonical Node/Attempt or one honest reconcile state, never a hidden registry;
- 1,024 exact corrupt-store quarantines fill their2-GiB subcap inside the8-GiB operational-store class while64
  recovery intents and10,000 small or8,192 maximum receipts/32 MiB exercise rename/fsync/crash/race/full-disk,
  recover/start-fresh/export/discard. Capacity N+1 leaves the original untouched and read-only; no empty save
  or implicit quarantine deletion occurs; and
- the existing10,000 RepositoryMutationIntent/256-MiB family is refilled with every advanced typed verb:
  initialize, detached checkout, branch rename/delete, stash push/pop, merge, rebase, revert and force-with-
  lease. Plans reach1,000 commits/10,000 paths, each crash/conflict/partial/ref/reflog/index/worktree/remote-tip
  boundary reconciles without replay, and count/byte/primary-checkout/protected-branch N+1 has zero Git/provider
  effect while terminal input and Attention retain their budgets; and
- 64 authenticated clients concurrently create Sessions/Nodes and edit disjoint plus conflicting presentation
  fields in one Workspace. Exact self-echo dedup, ordered application, injected gap+automatic resnapshot,
  disconnect/reconnect and local dirty-draft preservation converge within the normal StateStream/remote budgets
  with zero duplicate runtime, watcher loop, full-store merge buffer or lost selection; unexpected filesystem
  replacement takes the bounded read-only quarantine path; and
- instrumented startup, runtime, update, collaboration, crash and shutdown emit exactly zero telemetry/event/
  install-count requests, identifiers, queued bytes or retained rows. The separately shaped signed-update
  request includes only its closed metadata schema and fails on any generic event field.

Every bullet is also run as an isolated saturation phase. During each phase, without exception, local terminal
input remains p99 below 1 ms and local Attention routing remains p99 below 100 ms; a passing aggregate cannot
hide a violation in one new utility path.

The release gate uses a fixed minimum reference profile: base Apple M1 (8-core), 8 GiB RAM, internal SSD,
supported macOS, 1,920×1,080 display, AC power, no swap at start and an optimised packaged build. The same
workload is recorded on each claimed Linux platform, but a faster developer or CI host cannot replace the
minimum-profile artifact. After a five-minute warm-up, the harness runs 30 minutes continuously and injects
a 10-second topology/Attention/output burst every minute. `Turn-owned` includes daemon, every GUI/client,
SpeechWorker, isolated Browser renderer, Media decoder, NotificationHost/delivery worker, remote transport,
transfer and updater helpers, commit-proposal sandbox helper, provider broker/collector and watchdog
spawned or retained by Turn. Managed user/agent processes, external provider-owned services/caches and
checkout data are reported separately and excluded from Turn's own RSS/disk budget; no Turn helper may be
misclassified into those exclusions.

The only general helper family is the closed nine-kind `AuxiliaryWorkerOwnerKey` in `docs/PROTOCOL.md`.
NotificationHost/NotificationDelivery/RemoteTransport/ContextBrokerRemoteRead/Transfer/Updater/
ProviderBroker/ProviderCollector/Watchdog have exact per-kind count caps1/32/128/128/32/1/32/32/64, share128
live-or-cleanup-pending slots, and reserve≤128 MiB each/≤1,024 MiB family-wide before any effect. The benchmark
fills each kind count, the cross-kind count and family bytes independently, then proves N+1 parks/refuses and
owner cycling cannot release a hung charge before quiescence.

The supervisor enforces two non-overlapping admission budgets:≤512 MiB for daemon/GUI/client core and≤1,024
MiB shared by every variable Turn allocation, including snapshots, projections, all queues/buffers, Browser
renderer/partition state, Media decoder state, SpeechWorker and every broker/proposal/update/transfer helper.
Each allocation reserves both its family cap and the shared variable pool before read, spawn, subscription,
decode, render or dispatch. A family maximum is therefore an upper bound, not a promise that all family maxima
coexist. The harness fills every family in isolation and then adversarially fills mixed families until exactly
the shared boundary; the next byte/process refuses or parks pre-effect, core stays responsive and sampled total
RSS remains≤1.5 GiB. Neither allocator overcommit nor post-hoc killing can satisfy this oracle.

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
| End/delete recovery reduction | a full 4,096-subject Workspace reduction including lifecycle/replacement launch races and terminal park/detach/wake/shadow/writer state schedules p95 below 100 ms/p99 below 200 ms CPU before asynchronous cleanup; reservation-preserving ProcessCleanupCharge transfer, buffer wipe and tombstone fencing cannot add an operator interaction, block row removal, delete a profile-owned index or breach terminal-input/Attention budgets |
| Remote Attention route | exact demand creation through full-remote/headless receipt p95 below 150 ms and p99 below 300 ms under the declared network profile |
| Remote input/event | authenticated input-to-PTY enqueue and revision-event application each p95 below 125 ms and p99 below 250 ms; wrong lease/revision/profile has zero enqueue |
| Remote WorkSurface switch | cached metadata view p95 below 175 ms and p99 below 350 ms; first bounded heavy subscription p95 below 400 ms and p99 below 800 ms |
| Headless mutation receipt | ordinary negotiated mutation through durable receipt p95 below 150 ms and p99 below 300 ms; desktop-only refusal uses the same ceiling |
| Remote reconnect | authenticated reconnect, gap detection and bounded resnapshot p95 below 2 s and p99 below 5 s; no mutation is replayed before convergence |
| Flow fan-out | create call returns after durable operation receipts, never after child completion; parent and later operations remain interactive |
| Shared endpoint | all ten mixed profiled/unscoped binding queues make progress; per-binding input/event p99 below 250 ms and endpoint restart convergence p99 below 5 s with zero cross-scope/conversation bytes or invented AccountProfile state |
| Target inventory | 2,000-handle complete/partial/gapped reconciliation p95 below 100 ms and p99 below 200 ms CPU, below 16 MiB serialized, without blocking input or claiming exactness after a gap |
| Resource inventory | 10,000 process rows plus host capacity/pressure aggregate p95 below 150 ms and p99 below 300 ms CPU, below the same 16 MiB target snapshot cap; failed/partial collection never renders zero or blocks input |
| Recursive Groups / CheckoutScope | tree projection and one subtree CAS p95 below 50 ms/p99 below 100 ms at 1,000 nodes; cycle/depth/corruption rejection p99 below 100 ms and checkout reconciliation stays asynchronous |
| Automatic hierarchy arrangement | visible-row reflow p95 below 16 ms/p99 below 32 ms at 1,000 nodes with viewport virtualization; every adjacent pair and exact prefix-sum spacer meets the bounded `TreeRowGap` equation, topology applies once, resize/zoom/filter emits no domain revision and no row overlaps or escapes logical/accessibility order |
| Surface lifecycle | daemon mint/resume/retire and 256-KiB state validation p95 below 25 ms/p99 below 50 ms; 64 records/16 MiB, owner/connection caps and dormant expiry never block another Surface's navigation or retain ephemeral child bytes |
| Dedicated roster / quota connectors | every six-adapter capability result and two quota-only samples apply p95 below 50 ms/p99 below 100 ms after receipt; one stalled cell consumes no other provider queue |
| Model endpoint | cached search/filter over 10,000 mapped models p95 below 75 ms/p99 below 150 ms; bounded remote discovery p95 below 400 ms/p99 below 800 ms; stale/cancelled pages cannot replace current route |
| Notification delivery | outbox projection/encryption enqueue p95 below 10 ms/p99 below 25 ms; a 10,000-item burst remains within 16 MiB, batches without blocking Attention/input and revocation fences queued dispatch within one scheduler turn |
| Local name proposal | sanitise/project/apply p95 below 25 ms/p99 below 50 ms after generator receipt; generation never blocks navigation/input and stale results are constant-time refused |
| Workspace onboarding / repository publication | each catalogue or publish-phase admission returns a durable start/refusal receipt p95 below 100 ms/p99 below 200 ms; clone/publish provider work is asynchronous, lookup-only reconciliation cannot stall another creation and no uncertain phase is redispatched |
| Board/Note/Resource | each projection/update p95 below 50 ms and p99 below 100 ms; progress compaction p99 below 200 ms; content bodies never enter the hierarchy snapshot |
| Context packet / AgentMessage | packet review/delivery admission at 1-MiB body+1-MiB review and message admission at 4 KiB apply p95 below 50 ms/p99 below 100 ms excluding source/transport I/O; 128 packet bodies/256 MiB and 10,000 message bodies/40-MiB body+64-MiB working sets remain hard inside shared RSS, source disconnect after acceptance loses neither body, and cleanup never blocks input or Attention |
| External WorkItemSource | apply and project one 500-item page p95 below 100 ms/p99 below 200 ms locally and below 300/600 ms over the shaped remote source; filter/cursor/rate-limit/conflict handling never blocks input or turns stale cache into an empty exact result |
| Native jobs | apply a 200-job/2,000-iteration snapshot or delta burst p95 below 100 ms/p99 below 200 ms; Flow recurrence remains independently responsive and no dismissed history is fetched/rendered eagerly |
| Conversation inventory | cached metadata search over 10,000 rows p95 below 75 ms/p99 below 150 ms; one bounded provider page including shaping p95 below 400 ms/p99 below 800 ms; cancellation/reselection prevents an old page or title from replacing the current view |
| Private transcript search | cached encrypted-index query over10,000 entries applies p95 below75 ms/p99 below150 ms and a20-hit/80-KiB page switches to its canonical read-only ViewTarget within the normal view budget; refresh parsing yields at least every25 ms CPU, runs no faster than five minutes, and source/index/query/key-revocation saturation or deletion never blocks input/Attention or reports false complete empty |
| Dependency-gated Flow | current-result derivation plus embedded readiness-receipt commit applies p95 below25 ms/p99 below50 ms for one edge and p95 below100 ms/p99 below200 ms for the4,096-result boundary; start dispatch is asynchronous, and replay/stale/deleted/idle/done/N+1 cases create zero extra StepAttempt |
| Browser/WebPreview | inert WebPreview or cached Browser WorkSurface switch meets the normal switch budget; intent admission/lookup applies p95 below 25 ms/p99 below 50 ms excluding network, network/renderer work is asynchronous and never blocks GUI/input, WebPreview's 32 states/256-MiB bodies/eight renderers/256 MiB and Browser's eight renderers/100 history entries per Node/10,000 global/512-MiB partition state remain hard, and one noisy page cannot delay terminal input or Attention beyond their budgets |
| Document view / print | cached page/zoom/fit/rotation applies p95 below16 ms/p99 below32 ms; open admission and page/search result application are p95 below50 ms/p99 below100 ms excluding separately reported FileBackend/decode I/O; print prepare/dispatch receipt applies p95 below100 ms/p99 below200 ms excluding native spool/driver time. View/blob/decoder/cache/index/spool count+byte N+1 refuses pre-read/pre-dispatch and cleanup cannot breach input/Attention budgets |
| Agent Browser control | grant/action admission or lookup applies p95 below25 ms/p99 below50 ms; a≤256-KiB accessibility read applies p95 below50 ms/p99 below100 ms after renderer response; network/page work is asynchronous. The256-grant/256-action/64-MiB-record/48-MiB-fence limits and existing Browser renderer/partition caps remain hard, Stop/revoke fences within one scheduler turn and one agent cannot delay a human Browser, input or Attention |
| Remote typed permission | exact encrypted response through durable accepted/refused/uncertain receipt p95 below 175 ms/p99 below 350 ms under shaping; stale/grantless/raw-PTY attempts meet the same refusal ceiling and enqueue zero bytes |
| Safe control visibility | resolving and projecting all optional-control slots p95 below 5 ms/p99 below 10 ms locally after a settings revision; palette/keyboard availability and critical controls remain immediate at maximum zoom |
| PTY capacity pressure | local current-reading classification and status/Attention commit p95 below 25 ms/p99 below 50 ms excluding the scheduled OS probe; a fresh critical preflight refuses before PTY open within the normal launch budget, while remote probe latency is reported separately and never blocks unrelated input |
| Title read/rename | a title observation or rename receipt applies p95 below 50 ms/p99 below 100 ms after adapter receipt; a slow title collector never delays launch, conversation inventory, tree selection or local alias editing |
| Companion profile inbox | one 1,000-item activity page plus usage/context cells applies p95 below 100 ms/p99 below 200 ms; profile switch p95 below 50 ms/p99 below 100 ms from cache; expired/unavailable cells never wait to fabricate a value |
| FileBackend conflict | 1 MiB local/remote snapshot open p95 below 100/400 ms and p99 below 200/800 ms; conflict detection/receipt p95 below 150 ms and p99 below 300 ms with zero overwritten bytes |
| Directory / commit graph | one 2,000-entry page/watch delta applies p95 below 75 ms/p99 below 150 ms; one 500-node commit page or 1,000 changed-file page applies p95 below 100 ms/p99 below 200 ms, and the 10,000-object cap reports a gap rather than blocking |
| Advanced repository operations | preflight/admission/refusal of a≤1,000-commit/10,000-path closed init/checkout/rename/delete/stash/merge/rebase/revert/force-with-lease plan applies p95 below100 ms/p99 below200 ms CPU; durable phase/reconcile receipt applies p95 below150 ms/p99 below300 ms after Git/provider I/O. Conflict/ambiguity remains asynchronous and no operation blocks input, claims primary `main`, retries a possible effect or bypasses the existing RepositoryMutationIntent bounds |
| Text search | each ≤25 ms CPU slice yields on time; cached next/previous is p95 below 10 ms/p99 below 25 ms, cancellation is observed p99 below 50 ms, and bounded coverage at 16 MiB or 1,000,000 cells/100,000 lines never reports false no-match |
| Terminal clipboard | local selection-to-copy dispatch applies p95 below25 ms/p99 below50 ms excluding OS clipboard latency; paste/path-drop uses the ordinary p99<1-ms input enqueue after gesture/lease/safety validation. A64-gesture/4-MiB saturation or OSC52 storm never waits, reaches a remote clipboard, emits reply bytes or retains a body after30 seconds |
| Attention audio | canonical edge-to-play start is≤300 ms when supported; enqueue applies p95 below10 ms/p99 below25 ms and never blocks the p99<100-ms Attention route. The128-cue/256-KiB,16/client,eight/10-second and per-subject cooldown bounds remain hard; mute, failure and late/replayed edges emit zero audio/state mutation |
| Bulk restart / Eco hibernation | derive a256-row revision-pinned preview or Eco eligibility queue p95 below100 ms/p99 below200 ms over the 10,000-agent inventory; each sequential restart/hibernate/wake admission receipt p95 below100 ms/p99 below200 ms excluding runtime I/O. Bulk/Eco count+byte saturation,≤2 Eco exits/minute, cancellation and reconciliation preserve terminal input/Attention budgets and never duplicate an attempt |
| Off-screen terminal parking | safe park decision/detach admission p95 below16 ms/p99 below32 ms; exact-handle shadow attach or automatic viewer attach/resync scheduling p95 below50 ms/p99 below100 ms excluding backend capture, with first repaint p95 below100 ms/p99 below200 ms after capture. Twelve-park/256-client, shadow/writer count+queue+RSS and4,096-byte wake bounds remain hard; gaps/saturation yield presentation only, zero-PTY scans remain exact, and no path emits an undeclared runtime signal |
| Automatic detached-session reaping | exactly0 timer/pressure/count/age-triggered runtime signals, session kills or deletions in every workload; any nonzero value fails rather than counting as successful reclamation |
| Companion agent launch | allowlisted Companion request to durable launch/refusal receipt p95 below150 ms/p99 below300 ms under network shaping; canonical checkout/runtime/graph completion is asynchronous and timed separately. Grant/intent/record saturation, revoke/disconnect and crash cannot create a second Node or occupy primary `main` |
| Corrupt-store recovery | failure classification and read-only recovery-status publication p95 below100 ms/p99 below200 ms after separately reported descriptor/hash/fsync I/O; recover/start-fresh/export/discard intent admission/lookup meets the same CPU budget. The1,024-item/2-GiB subcap and receipt N+1 preserve original bytes and cannot fall through to default save |
| Cross-client Workspace convergence | a typed mutation self-echo is deduplicated and a distinct ordered event applies p95 below50 ms/p99 below100 ms locally or within the remote-event budget; gap detection begins automatic resnapshot p95 below100 ms/p99 below200 ms before network transfer. A64-client create/edit burst produces one canonical Session/Node per receipt, no store-watcher I/O/merge buffer/runtime restart and no overwrite of a dirty local draft |
| Product telemetry | startup, steady-state, failure, update, collaboration and shutdown produce exactly0 product-analytics/install-count requests,0 analytics queue bytes and0 retained analytics rows; any nonzero value fails rather than being treated as a latency sample |
| Resident terminals | at most 128 live-or-retained PTY states; each reserves a 2-MiB raw ring plus 4-MiB current grid before spawn, parsed state remains≤8 MiB/item/512 MiB family with≤5,000 scrollback rows, daemon images≤16 payloads/16 MiB/item/512 MiB family and visible client caches≤12 payloads/12 MiB/item/256 MiB family; every byte charges shared RSS and pressure trims only old unpinned history/non-authoritative cache state |
| Terminal image pipeline | eight 8-MiB scan buffers, eight 8-MiB multipart assemblies and two complete 128-MiB decode high-waters are independent hard families inside shared RSS; 128 concurrent partial sequences and decoder bombs enter bounded discard/refusal without blocking text, input or Attention, and only≤4-MiB final RGBA transfers to the retained store |
| Terminal projection transport | attachments≤64/Surface,256/connection,4,096/32 MiB; baselines≤2 MiB/item/256 MiB; output queues≤512 chunks or8 MiB/PTY and4,096/256 MiB global; pump batches≤128×1 MiB; outboxes≤256 frames/8 MiB per connection and4,096/128 MiB global; automatic large responses use≤180-KiB raw/256-KiB encoded chunks with four streams/connection,16/120 MiB global and exact gap/digest cancellation |
| Local dictation | one microphone lease/target, two 10-MiB PCM buffers, one 32-KiB hypothesis, one≤512-MiB live-or-cleanup SpeechWorker and one existing≤32-KiB local draft plus≤4-KiB voice metadata remain hard under shared RSS; 300-second capture/inference and two-second shutdown boundaries do not block terminal input or Attention |
| Media import / playback | prepare/chunk/validation progress applies p95 below 50 ms/p99 below 100 ms; commit/reconcile receipt p95 below 150 ms/p99 below 300 ms after I/O, playback controls p95 below 16 ms/p99 below 32 ms, 32 states/512-MiB decoder family/shared-pool admission remains hard, and decoder pressure cannot breach input/Attention budgets |
| Repository host / commit proposal | cached profile/grant mutation applies p95 below 50 ms/p99 below 100 ms; proposal admission/terminal receipt p95 below 100 ms/p99 below 200 ms excluding the provider's separately reported generation latency, and limit/crash cleanup p99 below 500 ms after detection |
| Transfer | chunk admission/progress receipt p95 below 25 ms/p99 below 50 ms; pause/cancel/reconcile scheduling p95 below 100 ms/p99 below 200 ms excluding separately reported backend I/O, with at most 32 active tickets and no whole-file resident buffer |
| Content projection / catalogue | one ≤2-MiB projection renders p95 below 75 ms/p99 below 150 ms; a 200-entry request-only catalogue page/search over 10,000 entries applies p95 below 50 ms/p99 below 100 ms, while≤512 CatalogueScans retain≤32-KiB metadata each/16 MiB total and invocation dispatch p95 below 25 ms/p99 below 50 ms |
| Explorer/history/search | request-only directory/commit-graph/changed-file/search pages are≤4 MiB/1 MiB/2 MiB/200 KiB respectively and retain zero bytes after response; stateful DirectoryScan≤1,024×16 KiB/16 MiB, DirectoryWatch≤2,048×8 KiB/16 MiB and TextSearchSession≤512×16 KiB/8 MiB saturate independently with declared TTL/release and gap semantics |
| Announcement / update | one 100-item signed feed validates/projects p95 below 100 ms/p99 below 200 ms; singleton update admission, discover/verify/stage state and receipt folding p95 below 150 ms/p99 below 300 ms excluding separately reported download/disk I/O; exact 2-GiB combined allocation and N+1 never duplicate bytes; 30 disposable-root apply/rollback runs each preserve live-daemon/PTY invariants |
| WorkItem activity / presentation history | one 200-event page and one 200-entry history projection each apply p95 below 50 ms/p99 below 100 ms; a 10,000-event ingest/compaction burst converges p95 below 200 ms/p99 below 400 ms per 500-event batch without blocking current item mutation or input |
| WorkItem source query | four queries/connection and32 global reserve≤2 MiB each/64 MiB family before provider read; one request-only page is≤500 safe summaries,≤2 KiB/item and≤1 MiB logical with a≤512-byte authenticated cursor, p95 apply below50 ms/p99 below100 ms after provider response, and retains zero page bytes after transfer |
| Conversation/native inventory | conversation queries and native-job page reads each admit four/connection,32 global and≤2 MiB/item/64 MiB family; NativeJob additionally pins eight scans/connection,512×32-KiB/16 MiB. Request-only pages are≤500 safe2-KiB items/1 MiB, scan≤10,000,cursor≤512 bytes and apply p95<100 ms/p99<200 ms after provider response without false-complete or retained raw/page bytes |
| WorkItem activity page | request-only page≤200 events,≤8 KiB/event and≤1 MiB with≤512-byte authenticated cursor; 200-small-event count and128-maximum-event byte fixtures apply p95<50 ms/p99<100 ms and retain zero page bytes |
| Hierarchy projection | vNext compact index≤6 MiB across≤111,024 Workspace+Session+Node coordinates and complete bootstrap+wrapper≤7,680 KiB; each visible page≤500 rows/1 MiB and row≤2 KiB, reveal≤1 MiB, filter bitmap≤16 KiB raw/24 KiB NDJSON and delta≤4,096 ops/180 KiB serialized in one≤256-KiB frame; no terminal/transcript/log/note/media/inspector body enters index or page |
| Lazy tree | only viewport + fixed overscan + exact selection/restore/Attention reveal target materialise text/paint/accessibility rows; no Load-more action and a gap refreshes index+affected pages without blocking input |
| Heavy subscriptions | the seven ordinary families admit at most 64/connection and 4,096 global with the declared shared queue/byte/RSS limits; selected visible subject plus an explicit bounded preview set, and reselection/gap/disconnect releases the old generation before reconnect resnapshot |
| Turn-owned memory | combined RSS of every Turn-owned process above is at most 1.5 GiB under the enforced 512-MiB core+1,024-MiB shared-variable budgets; growth from minute 10 to minute 30 is at most 128 MiB; one SpeechWorker is at most 512 MiB, one active Browser renderer 256 MiB, each of at most two commit-proposal sandbox helpers is at most 512 MiB, and the closed AuxiliaryWorker family is≤128 live-or-cleanup workers,≤128 MiB/item and≤1,024 MiB total under its exact per-kind limits, all charging the shared pool and still counting toward the aggregate |
| Turn-owned operational store | SQLite/WAL, semantic metadata, receipts/fences, indexes, reconstructible cache and daemon logs remain≤8 GiB physical; StateStream and terminal journals are not hidden here and compaction stabilises eligible growth |
| Turn-owned physical disk ledger | reports logical-reserved, allocated-physical and reclaim-pending for operational/state-sync/terminal-history/FileSave-temp/portable-temp/account-root/speech-model/Media/Transfer-or-quarantine/update classes at exact 8/4/3/2/2/2/8/100/4/2-GiB caps and≤135 GiB total; Media also remains≤10 GiB/Workspace. Sparse/COW/compression/refcount never hides or double-counts extents, every class/total N+1 refuses pre-write and no Turn-owned byte is excluded |
| External disk | user-owned checkouts/repositories, provider-owned caches and explicit final external destinations are separately reported by owner/path class, never counted as Turn scratch and are the only exclusions from the 135-GiB total |
| Queues | GUI inbound/outbound/awaiting remain64/256/512 per connection and are each≤4,096 items/≤4 KiB each/16 MiB installation-wide, with inbound critical reservations16/client+1,024 global; native-dialog and Companion dispatch are one/owner,64 global and256/512 KiB; topology is1,024/source and4,096/≤4 KiB/16 MiB global; message is256 items and1 MiB per destination; each remote connection is256 frames or8 MiB and each frame is at most256 KiB; notification outbox is10,000 items or16 MiB; Directory/watch, search, decoder, proposal, transfer, projection/catalogue, announcement/update, activity and history boundaries each declare a named queue no larger than4,096 items or16 MiB with refusal/gap/resync rather than falling into an unmeasured generic channel; every other boundary has the same maximum |
| Network volume | after warm-up, Turn protocol egress averaged across the 30-minute run is at most 5 Mbit/s and p99 one-second egress at most 15 Mbit/s; payload bodies count, TLS framing does not |
| Resource pressure | may coalesce, slow preview or park reconstructible views; pressure alone never terminates/suspends live work. Only the independently opt-in exact-eligibility Eco policy may hibernate idle resumable work, at≤2/minute with durable evidence and automatic wake |
| Evidence | workload, hardware, commit, build profile and raw p50/p95/p99/memory/disk/queue results are retained |

The harness injects a slow provider collector, a 1,025-event topology overflow, shared-endpoint binding
backpressure, an inventory generation gap, Note edit/revoke races, delegated Resource/progress overflow,
board conflicts, FileBackend external-edit conflict, recursive Group/cycle races, resource-process scan,
model discovery, notification outbox/revocation, onboarding cancellation, directory/watch overflow, commit-
graph gaps, maximum-bound text search/cancel, media decoder/import pressure, repository-host grant churn,
proposal sandbox canaries and process/RSS/output limits, transfer chunk/temp saturation, projection/catalogue
overflow, signing/revocation/package-substitution races, activity compaction/history ownership, full semantic-
recovery reservation/migration saturation, view failures and
GPU/GUI memory, PTY, file-descriptor, process-limit, store/disk/journal, shaped-network and collector pressure
while the operator switches nodes and types on desktop/full-GUI, issues authorised structured reads/navigation
from headless and exercises Companion's closed mappings. Every latency population has at least 10,000 observations (reconnect has 100 and update apply/rollback has 30),
uses nearest-rank percentiles and reports misses/timeouts as infinite latency; warm-up samples are excluded.
It fails on false zero counts, stale-route application, dropped Attention, duplicate effects, cross-binding or
cross-profile bytes, duplicate auto-starts, background-selection launches, hidden WorkItemSource or
conversation gaps, Flow/native-job conflation, stale title overwrite, remote permission PTY fallback,
cross-profile Companion samples, false resource zero/double count, stale model/name result, notification
resolution/resurrection/public listener, Group cycle/hang, duplicate clone, unbounded Browser/history growth,
overwritten files or runtime termination
as well as on latency regression.

Passing performance never substitutes for semantic acceptance. The complete cross-gate obligations are
`ACP-FLW-008`, `ACP-ADP-005`, `ACP-VIE-011`, `ACP-FLW-012`, `ACP-ADP-010`, `ACP-LIF-008`,
`ACP-RUN-010`, `ACP-CTX-012`, `ACP-OBS-008`, `ACP-SCL-001` through `ACP-SCL-009`, `ACP-LIF-009`,
`ACP-VIE-012`, `ACP-ATT-011`, `ACP-ATT-012`, `ACP-ADP-011` through `ACP-ADP-013`, `ACP-CTX-013`,
`ACP-RUN-011`, `ACP-HIE-009`, `ACP-HIE-010`, `ACP-CRE-008`, `ACP-OBS-009` through `ACP-OBS-011`, `ACP-SAF-014`,
`ACP-SCL-010`, `ACP-RUN-006`, `ACP-RUN-012` through `ACP-RUN-016`, `ACP-VIE-013`, `ACP-VIE-014`,
`ACP-CRE-009`, `ACP-CRE-010` and `ACP-SAF-015`;
each named ACP must pass
its functional, durability, loss, migration and privacy dimensions independently before these measurements
can be release evidence.
