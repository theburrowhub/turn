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
