# The Turn daemon protocol

Normative contract for the implemented `turn-proto` version **4**. Versions 2 and 3 are historical context
for the retained terminal-cell and hierarchy transports, not supported modes. Operations explicitly labelled planned
below are product commitments, not wire operations in this build.

This is the contract between `turnd` — which owns every pty, all state and the
attention manager — and a UI client, which renders and forwards keystrokes.

Fields shown without an ellipsis are normative. Examples containing `…` deliberately omit unrelated view
fields. The exact wire source of truth is `crates/turn-proto/src/`; catalogue and conversation tests guard
operation names, request-to-response pairing and protocol version. Revision semantics and the prose contract
also have integration/audit coverage, but no test can prove that prose is current merely by existing.

---

## 1. Transport and framing

**Newline-delimited JSON over a unix socket.** One JSON object per line,
`\n`-terminated, UTF-8. The socket path is `turnd`'s to choose and publish; this
document calls it `$SOCKET`.

NDJSON was chosen over a length-prefixed binary framing deliberately. The most
important boundary in the system stays readable: `socat - UNIX-CONNECT:$SOCKET`
is a working client, a bug report can contain the exact bytes, and a second
frontend can be written in any language with no codec library. The price is paid
in §2.

### Limits

| Limit | Value | Meaning |
| --- | --- | --- |
| `max_line_bytes` | 8 MiB | Longest line either side accepts. Announced in `welcome`. |
| `max_output_chunk_bytes` | 256 KiB | Legacy v4 raw bytes per `pane_output` NDJSON message before splitting; base64 makes that message larger than256 KiB. VNext `terminal_output` raw payload is≤64 KiB and its complete frame≤256 KiB. |
| `max_stream_chunk_bytes` | 180 KiB | Raw bytes in one automatic chunked-response frame; base64 plus the complete envelope remains≤256 KiB. |
| `max_request_id_bytes` | 128 ASCII bytes | Nonempty `[A-Za-z0-9._:-]+`; rejected before request registration or response allocation. |
| `max_screen_cells` | 65,536 | Largest `rows * cols` `attach_pane` accepts, and the most cells any grid may describe. |
| `max_image_pixels` | 1,048,576 | Most pixels one inline image may carry — 4 MiB of RGBA in one bounded logical chunked response. |
| `max_placed_images` | 8 | Most inline images one screen may place at a time. |

In v4 the largest legitimate message is a pane screen of a few kilobytes (§2.2). In vNext no application
outbox frame exceeds 256 KiB. A logical response above 192 KiB is automatically framed as
`response_stream_begin(request_id,stream_generation,content_kind,total_bytes,digest)`, ordered
`response_stream_chunk(seq,offset,raw_bytes≤180 KiB)` and `response_stream_end(total_chunks,digest)`; this is
transport, not a user interaction or a second request. No logical result exceeds≤7,680 KiB: the hierarchy
bootstrap reaches that ceiling (index≤6 MiB, first page≤1 MiB, tree/filter/framing remainder), pane-image and
DirectoryPage are≤4 MiB, CommitChangedFilesPage≤2 MiB, and every smaller page/reveal obeys its own declared
limit. Unsolicited vNext pushes are never response-streamed; their serialized payload is≤180 KiB or they emit
a typed gap. The receiver accepts only contiguous offsets under the declared total
and matching digest. Gap, duplicate-different chunk, reselection, request cancellation or connection loss
drops the whole partial and requires the owning snapshot/refetch operation to restart from current authority;
no partial result applies. The line limit remains defence in depth against a peer that writes without newline.
Every stream envelope repeats only the≤128-byte RequestId, numeric generation/sequence/offset/total, a closed
≤32-byte ASCII ContentKind and a 64-byte lowercase SHA-256 digest; it has no caller-controlled metadata map,
path, title or error body. These maxima prove that 180-KiB raw base64 plus JSON/envelope stays below256 KiB.

`max_screen_cells` is 256x256, far past any real terminal — a 6K display at a tiny font
is about 20,000 cells. It is a limit a client can *hit*, so it is announced rather than
assumed: `attach_pane` above it answers `invalid_argument` rather than silently drawing
something else, and a grid above it is refused on the way in, before anything is
allocated for it.

### Decoder guarantees

Implementations must match `turn_proto::LineDecoder`, which guarantees two things.
A multiplexer that drops its control connection on bad input would take thirty
running agents down with it.

1. **Partial reads are normal.** A frame may arrive in forty pieces, or forty
   frames in one piece. Chunk boundaries carry no meaning.
2. **A bad line costs one line.** Invalid JSON, an unknown message shape, or a
   line over the limit produces an error for *that line* and the stream continues.
   The receiver replies with an `error` frame; the connection stays up.

Specifics:

- A line over the limit is reported once, and the rest of it is discarded as it
  arrives rather than buffered. The next line parses normally.
- Blank lines and `\r\n` terminators are tolerated and skipped.
- A frame never contains a raw newline: `serde_json` escapes them inside strings,
  so a pasted multi-line command is still one line.

### Ordering

- Responses are correlated by `id`, **not** by arrival order. Requests may be
  pipelined; a client need not wait for one response before sending the next,
  which matters for `write_pty` at typing speed.
- `pane_screen` and `pane_output` pushes for one pane arrive in `seq` order. A gap
  means an update was lost: for cells the recovery is `resync_pane`, for bytes it is
  admitted by `pane_output_gap` and recovered by re-attaching (§8.2).
- No runtime-event push or Attention effect is emitted until the daemon has committed
  the event's Session → event-log → Attention checkpoint. A failed checkpoint is
  retried behind a FIFO barrier. Before dispatching any request the daemon retries the
  oldest checkpoint; while it remains blocked the response is `unavailable`. Reads
  therefore cannot observe its partial projections and rejected writes cannot be
  persisted accidentally by a later retry.
- Nothing else is ordered relative to anything else.

---

## 2. What a terminal looks like on the wire

A pane can be carried two ways, and the client chooses at `attach_pane` with
`stream`:

| `stream` | Payload | For |
| --- | --- | --- |
| `cells` (**default**) | The parsed screen: `grid` on attach, `pane_screen` updates after | Rendering. Anything that draws a terminal. |
| `bytes` | The escape stream: `replay` on attach, `pane_output` frames after | Anything that needs the stream itself — capturing a log, a client with its own VT emulator. |

**Cells are the default because the daemon has already parsed the screen.** It has to:
on-demand previews and the output heuristics work with no client attached, so a `vt100` screen
exists for each PTY-backed runtime node whether or not anybody is looking. Sending that screen through a
bound Pane means there is
exactly one terminal emulator in the system, which removes the whole class of bug where
the daemon's idea of a pane and the client's disagree — and it means a client does not
need an emulator at all.

The byte path is not deprecated. Escape sequences are the only lossless record of what a
program emitted, and a log capture or a future web frontend built on `xterm.js` wants
precisely that.

### 2.1 Bytes: base64, and what it costs

Terminal traffic is bytes, not text: a pty may emit any byte at all, including invalid
UTF-8. JSON has no byte type, so byte fields (`data`, `replay`) are **standard base64
with padding**, no line breaks.

**The cost, stated plainly.** Base64 inflates payloads by 33% and costs a pass over the
data in each direction. For interactive use this is irrelevant. For a `cargo build`
firehose it is not: 10 MB of output becomes 13.3 MB on the wire plus encode and decode
work. This is accepted in exchange for one human-readable frame format. The escape hatch
is already in the handshake: `output_encoding` is negotiated in `welcome`, so a
length-prefixed binary side channel can be added by agreement rather than by a protocol
break. A pane nobody attached as `bytes` is never base64-encoded at all.

Decoding is **strict**. Whitespace, missing padding, the URL-safe alphabet and
non-canonical trailing bits are all rejected rather than repaired — a protocol that
quietly fixes its own input is one whose two implementations will eventually
disagree.

### 2.2 Cells: the grid, run-length encoded

A 40x120 screen is 4,800 cells and it travels on every update, so the encoding matters.
One JSON object per cell would be about 30 bytes each. Measured on a realistic screen —
a build log, half the rows carrying text — that is **202,930 bytes**.

So a row is a list of **runs**: consecutive cells sharing a colour and an attribute set
become one object carrying the run's text and how many cells it covers.

```jsonc
{"rows":2,"cols":4,"cursor":[1,0],
 "runs":[[{"t":"ok","n":2,"f":[200,40,40],"a":1},{"n":2}],[{"n":4}]]}
```

| Field | Meaning |
| --- | --- |
| `t` | The run's text. Absent when the run is blank. |
| `n` | Cells the run covers. |
| `f`, `b` | Foreground and background as `[r,g,b]`, **already resolved**. Absent means "the theme's own", which is what lets a themed background work. |
| `a` | Attribute bits: `1` bold, `2` italic, `4` underline, `8` inverse, `16` dim, `32` wide, `64` wide-trailer. Absent when none are set. |

Three run shapes, and decoding tells them apart without ambiguity:

- `t` absent — `n` blank cells in the run's style. A blank 120-column row is `{"n":120}`.
- `t` holding exactly `n` characters — one character per cell.
- `n` of 1 with `t` longer than one character — one cell holding a grapheme cluster: an
  emoji with a modifier, a combining accent.

Anything else is **rejected**, as is a row that is not exactly `cols` wide, a grid whose
`runs` count is not `rows`, and a grid over `max_screen_cells`. The cell cap is checked
before anything is allocated, so a thirty-byte line cannot ask the receiver for
gigabytes.

Grid-level fields are spelled out (`rows`, `cols`, `cursor`, `alternate_screen`,
`modes`, `scrollback_offset`, `scrollback_len`) because there is one of each and a frame
in a bug report should be readable; run fields are single letters because there are
thousands of them per screen.

**Measured sizes**, for the same 40x120 pane:

| Message | Bytes |
| --- | --- |
| A blank screen | 526 |
| A screen with 20 rows of build output | 2,156 |
| The same screen at one object per cell | 202,930 |
| A keystroke echo (one row, cursor moved) | 133 |
| One new line of output on a busy screen | 186 |
| A scroll — every row differs, so the whole grid is sent | 2,259 |

**Honest note on the cost.** Runs are a large win on ordinary screens and a poor one on
pathological ones: a screen where no two adjacent cells share a style degrades to one
object per cell, and `max_screen_cells` is what bounds that case at roughly 2 MB. Colour
travels as three decimal numbers rather than a packed integer or a hex string, which
costs a few bytes per style change and keeps the frame readable. The daemon also pays for
one `String` per non-blank cell when it builds a grid; that is the price of a cell that
can hold a grapheme cluster, and it is paid only for panes somebody is actually watching.

### 2.3 Inline images

A pane may hold pictures. Three protocols put them there — iTerm2's `OSC 1337 File=`,
Sixel, and the Kitty graphics protocol — and the daemon decodes all three, because it has to:
the number of *cells* a picture occupies depends on its pixel dimensions, and the cells are
what the terminal parser scrolls, clears and overwrites.

**Placement travels in the cells.** Every cell a picture covers carries an *image marker*
as its text — a character in private-use plane 16 encoding which of the screen's
`max_placed_images` slots the picture is in, and which tile of it that cell shows — and the
`image` attribute bit (`0x80`). A grid also carries a small `images` table mapping slots to
payload ids:

```jsonc
"images":[{"slot":0,"id":6023794128384115081,"rows":8,"cols":30,
           "width":240,"height":160,"preserve_aspect":true}]
```

`rows`/`cols` are the **cell box** the daemon gave the picture; `width`/`height` are the
payload's own pixel size. Both are needed, and the split is deliberate: only the client knows
how many pixels a cell is, so only the client can fit the picture inside the box without
distorting it. `preserve_aspect` is absent when true, which is the usual case.

Doing it this way is what makes a picture behave like text without a second implementation of
a terminal. Scrolling moves it because rows move. `clear` drops it because cells are cleared.
A program printing over the middle of one punches a hole in it and the surviving tiles still
say which part of the picture they are. A client re-attaching gets the pictures still on
screen because they are *in* the screen the daemon has been keeping.

**Pixels do not travel in the grid.** A grid crosses the socket many times a second and a
megabyte of picture must not, so `pane_image` fetches a payload once per id and a client
caches it (§5). Ids are derived from the contents. On a `rows` update the `images` field is
absent when the table has not changed — which is every update for a picture that is merely
scrolling — and `[]` when the screen has lost its last picture. An empty list is not the same
as silence.

**A receiver must treat all of it as hostile.** Every bound is enforced on the way in, before
anything is indexed or allocated: `max_placed_images` on the table's length, one placement per
slot, the cell box against the marker alphabet, and `max_image_pixels` on the declared pixel
size. A grid that breaks any of them is refused whole, exactly like a row whose runs do not
account for its cells.

**What the client renders for a cell it cannot resolve.** A marker whose slot the table does
not fill, or a payload not yet fetched, is a framed placeholder rather than a blank. A picture
that silently did not appear is indistinguishable from a defect.

**Anything Turn refuses to show is said in the pane.** A payload over the limit, a
decompression bomb, a format Turn does not read, iTerm2 without `inline=1` (which is a request
to write a file to the user's disk), or Kitty's `t=f`/`t=t`/`t=s` (which are requests to read
one) all produce a line of ordinary text in the pane reading
`[turn: image not shown — …]`. Turn also never writes the Kitty protocol's
acknowledgement back to the pty: nothing types into a pane except a human.

### 2.4 What is *not* sanitised

Screen cells are the terminal's own contents, passed through as the program wrote them.
That is the opposite of the rule for **labels** — a pane title or Activity Preview line is
stripped of control, bidirectional and invisible characters, because those end up in
Turn's chrome where text could lie about itself. Inside a pane the client paints cell by
cell, so no cell can reorder its neighbours, and filtering would mean a terminal that
does not show what the program printed.

---

## 3. Envelope

Every frame, in both directions, carries `v` — the protocol version it is written
against — alongside a `type` discriminator.

```jsonc
{"v": 4, "type": "request", "id": "r-1", "request": {"op": "..."}}
```

Negotiating once and trusting the connection afterwards would be enough. The
version rides on every frame anyway because it costs a handful of bytes and buys
two things: a frame captured out of a log is self-describing, and a peer that
reconnects with different code — which is what happens all day while developing a
UI against a running daemon — is caught rather than tolerated.

| Direction | `type` | Payload |
| --- | --- | --- |
| UI → daemon | `hello` | Opening frame. Anything before it is refused with `handshake_required`. |
| UI → daemon | `request` | `id` + `request` |
| daemon → UI | `welcome` | Handshake accepted |
| daemon → UI | `rejected` | Handshake refused; nothing else follows |
| daemon → UI | `response` | `id` + `response` |
| daemon → UI | `error` | Optional `id` + `error`. No `id` for a failure belonging to no request. |
| daemon → UI | `event` | An unsolicited push |

### What changed in version 2

`attach_pane` gained `stream`, and its default is `cells` (§2). That is a change of
*meaning* for an existing request rather than an addition: a version 1 client omits the
field and expects `pane_output` bytes, and would attach successfully and then be sent
`pane_screen` frames it has no code for. So the version moved, and
`min_protocol_version` moved with it — a daemon cannot serve both defaults from one
attachment without guessing what a silent client meant.

Also new, and additive: `resync_pane`, the `screen` result, the `pane_screen` push, the
`screen` field on `attached`, and `max_screen_cells` in `limits`.

### What changed in version 3

Version 3 is the ADR-040 hierarchy and checkout-safety boundary. It is incompatible because an older client
would bootstrap independent Workspace, Session and per-Session process lists, silently omit Session mode and
lease conflict choices, assume one Pane per node, and never resynchronise the unified navigation projection.

Version 3 adds:

- `get_hierarchy` and `HierarchySnapshot { revision, tree_state, workspaces }` as the only navigation
  bootstrap; the stable `surface_id` lives inside `tree_state`;
- per-surface tree expansion, selection, filters, visibility, viewport and manual ordering, which are
  acknowledged but not broadcast;
- closed `SessionMode` values `main_checkout`, `read_only`, `isolated_worktree` and checkout/lease operations;
- typed `workspace_write_lease_conflict` and `stale_lease_generation` error context;
- relationship kind plus confidence, lossless Agent naming, safe Activity Preview and zero-to-many Pane
  bindings;
- revisioned `hierarchy_changed` full replacements and bounded preview/binding/lease pushes in implemented v3/v4.

The incompatible vNext target replaces that unpaged full hierarchy payload with the bounded compact-index,
row-page, reveal and delta/gap contract below; a v4 peer cannot guess or negotiate those shapes.

Additive since v3, so the version does not move: `pane_image` and the `pane_image` result
(§2.3), the `images` table on a grid and the `image` attribute bit on a cell, and
`max_image_pixels` and `max_placed_images` in `limits`. An older client ignores the table, and
because an image cell's *text* is a private-use marker with the `image` bit set it draws as
nothing recognisable rather than as somebody else's character — which is the reason the marker
alphabet is in plane 16 rather than in the private-use area real fonts use.

The cell protocol introduced in v2 and hierarchy introduced in v3 are retained unchanged.

### What changed in version 4

Version 4 makes the opening handshake an authority boundary. `hello` gains an `auth_token` read from the
owner-only `<socket>.token` file. A v3 client would omit it and could no longer control its panes, so this is
not an additive rollout. Missing, invalid and stale-generation values are refused with `unauthorized` before
client registration. `rate_limited` is also added for per-client admission control.

This pre-release codebase serves v4 only:
`MIN_PROTOCOL_VERSION == PROTOCOL_VERSION == 4`. Legacy list/detail operations may remain as administrative
endpoints, not as a dual-v2 compatibility mode.

### Compatibility rules

- No type uses `deny_unknown_fields`. An older client **must** ignore fields it
  does not know; a newer client must tolerate a daemon that omits them.
- `null`-valued optionals are omitted from the wire wherever the field is
  meaningfully absent. A client must treat "absent" and "null" identically.
- Adding a request, a response variant, a push or an optional field does **not**
  bump the version. Removing or renaming a field, changing its meaning, or removing
  a variant does.

---

## 4. Handshake

The client sends `hello`. The daemon calls `negotiate(v)` and replies `welcome` or
`rejected`.

```jsonc
// UI → daemon
{"v":4,"type":"hello","client":"turn-gui","client_version":"0.1.0",
 "auth_token":"<64 lowercase hexadecimal characters from $SOCKET.token>"}
```

`accepts_encoding` may be included (`["base64"]`); an empty or absent list means
base64.

```jsonc
// daemon → UI
{"v":4,"type":"welcome","protocol_version":4,"min_protocol_version":4,
 "agreed_version":4,"daemon_version":"0.1.0","daemon_pid":51234,
 "daemon_started_ms":1700000000000,
 "limits":{"max_line_bytes":8388608,"max_output_chunk_bytes":262144,
           "max_screen_cells":65536,"max_image_pixels":1048576,
           "max_placed_images":8},
 "output_encoding":"base64"}
```

The client re-reads `$SOCKET.token` before every connection attempt; a daemon restart rotates the capability.
The value is never logged or formatted through `Debug`. `daemon_pid` and `daemon_started_ms` are how a reconnecting UI tells "my socket
hiccupped" from "the daemon restarted and nothing survived" — which decides whether
it must re-attach every pane.

Inside the supported window the **client's** version is used, not the daemon's
newest. That is what makes a rollout window mean anything.

### Mismatch

Rejection is the point. A stale UI that half works is worse than one that refuses
to start: an unrecognised variant deserialises as an error the UI reports as
"unknown", a renamed field arrives as `None`, and the user sees a terminal that
looks fine above a sidebar that quietly lies about what is running.

```jsonc
// daemon → UI — a daemon that has moved on to protocol 4..=5, client speaks 3
{"v":3,"type":"rejected","error":{
  "code":"unsupported_version",
  "message":"This Turn app is too old for the daemon it is talking to (app speaks protocol 3, daemon needs 4 or newer). Quit Turn and start it again to pick up the matching app",
  "detail":"client=3 supported=4..=5"}}
```

The other direction names the daemon instead: *"The running Turn daemon is older
than this app … Stop the daemon so Turn can start a current one."* Both carry
`code: "unsupported_version"`, which is `is_fatal_to_connection` and **not**
`is_retryable`.

A daemon must be able to read `v` out of a frame it cannot otherwise parse, so a
version problem is reported as one rather than as `malformed_message`.
`turn_proto::peek_version(line)` reads the version off the raw line and nothing
else; `turn_proto::version_refusal(line)` turns it straight into the `rejected` or
`error` payload above, and returns nothing when the frame's version is one this
build speaks — a frame that is merely nonsense stays `malformed_message`. Take the
line from `LineDecoder::next_line`, because a `FrameError` deliberately keeps only
a short excerpt of it.

---

## 5. Errors

One shape for every failure. Code is what software branches on; `message` is
shown to the user verbatim and is never parsed. `detail` is for logs.

```jsonc
{"v":4,"type":"error","id":"r-9","error":{
  "code":"not_found","message":"No such session","detail":"sess_gone"}}

{"v":4,"type":"error","error":{
  "code":"malformed_message","message":"A message could not be understood"}}
```

| Code | Meaning | Retryable | Fatal to connection |
| --- | --- | --- | --- |
| `unsupported_version` | Version windows do not overlap | no | **yes** |
| `unauthorized` | Missing, invalid or stale daemon capability | no | **yes** |
| `handshake_required` | A request arrived before `hello` | no | **yes** |
| `already_handshaked` | A second `hello` on one connection | no | no |
| `malformed_message` | Not valid JSON, or not a message this protocol defines | no | no |
| `line_too_long` | Over the frame limit; the line was discarded | no | no |
| `rate_limited` | This client exceeded its frame budget | **yes** | no |
| `not_found` | The id does not exist | no | no |
| `invalid_argument` | Well-formed, but the arguments make no sense | no | no |
| `conflict` | Contradicts current state (closing the last pane) | no | no |
| `pane_not_attached` | Output requested for a pane never attached | no | no |
| `process_not_running` | Nothing to write to, resize, interrupt or kill | no | no |
| `refused` | Turn will not do this on principle — see §9 | no | no |
| `unavailable` | The store, the pty layer or an agent binary is not there | **yes** | no |
| `internal` | A daemon bug | **yes** | no |

`message` and legacy `detail` are for people/logs. A client branches only on `code` and optional tagged
`context`. Checkout conflicts are therefore self-contained:

```jsonc
{"v":4,"type":"error","id":"r-lease","error":{
  "code":"conflict","message":"The primary checkout already has a writer",
  "context":{"kind":"workspace_write_lease_conflict",
    "workspace_id":"ws_9f2a1c","checkout_id":"checkout_a4c9",
    "requesting_session_id":"sess_requester",
    "lease":{"id":"lease_72ce","workspace_id":"ws_9f2a1c",
      "session_id":"sess_owner","checkout_id":"checkout_a4c9",
      "mode":"exclusive_write","state":"active","acquired_ms":1700000000000,
      "heartbeat_ms":1700000004000,"released_ms":null,"generation":4},
    "owner":{"session_id":"sess_owner","session_name":"Fix climbing bugs",
      "mode":"main_checkout","cwd":"/repo","branch":"fix/climbing",
      "last_activity_ms":1700000004000},
    "alternatives":["focus_owner","create_read_only","create_isolated_worktree","cancel"]}}}
```

`stale_lease_generation` carries `lease_id`, `expected_generation` and `actual_generation`; it means a stale
actor attempted a heartbeat or release and must resynchronise. Neither context authorises stealing a lease.

---

## 6. Requests — UI → daemon

Operations are tagged `op`. Every request carries a client-supplied `id`; the
daemon echoes it untouched. Ids are client-supplied so the UI can key its pending
map on something it already has, without a round trip to learn the key.

`session_id`, `workspace_id`, `pane_id`, `node_id`, `template_id`, `handoff_id`,
`attention_id`, `checkout_id` and `lease_id` are the prefixed string ids from `turn_core::ids`.
The v4 `HierarchyKey` is tagged `workspace`, `session` or `process`; a raw string is never accepted where
the kind matters. ADR-059's incompatible vNext key is instead tagged `workspace`, `session` or general
`node`, with a closed node kind so Group/Note/File/Diff/WebPreview/Browser never impersonate a process.
Migration maps every v4 process key losslessly; protocol negotiation rejects a mixed-version guess.

The vNext `ViewTarget` is also a closed tagged value:

```text
ViewTarget = workspace(WorkspaceId, WorkspaceRevision)
           | session(WorkspaceId, SessionId, SessionRevision, LayoutRevision)
           | node(WorkspaceId, SessionId, NodeId, NodeRevision, ContentKind)
           | historical_conversation(
               provider, AccountProfileId, AccountProfileRevision,
               ExecutionTargetId, TargetGeneration, provider_namespace,
               ConversationKey, PrivateTranscriptSearchIndexGeneration,
               TranscriptSourceRevision)
```

`workspace|session|node` are daemon-derived from one exact current `HierarchyKey`; clients cannot supply their
revisions or content kind. `historical_conversation` is daemon-derived only by
`select_private_transcript_search_hit` from a sealed current result and is read-only Surface presentation. It
does not mint a Node/HierarchyKey, ownership, ConversationBinding, RuntimeAttempt, InputRoute, grant or context
authority. `Surface.active_view_target` and its monotonic revision are nested fields of the existing durable
`Surface` family; `TreeSurfaceState.selected` remains the hierarchy navigation origin when this non-tree
variant is active. Profile/grant/target/index/source loss atomically replaces it with the daemon-derived
ViewTarget for that still-current hierarchy origin, or the owning Workspace overview when the origin no longer
exists, and emits one bottom-status reason; it can never leave an invalid/blank target or a `Start pane`. Every node-view
read/subscription and late response repeats exact Surface+ViewTarget revisions and is discarded after either
changes.

Tree coordinates are deliberately absent from the protocol. Every client derives `ProjectedRows` as the same
logical/accessibility preorder from hierarchy, sibling order and expansion/filter state; `MaterializedRows`
is only its viewport subset. Restore, resize, zoom or a topology delta cannot request or persist a tidy
operation, and layout recomputation changes no daemon revision, selection, runtime, input owner or Attention
state. The versioned client design contract fixes `TreeRowGap` to `0..=8` logical pixels at 100% zoom:
adjacent ProjectedRows use exactly one gap and a virtualized spacer is the exact prefix sum of omitted bounded
projected heights and gaps. Neither coordinates nor the derived spacer enter daemon state.

Expansion/filter/topology visibility is a separate revisioned reducer: collapse retains hidden selection
identity and focuses the collapsed projected ancestor; filter retains identity and focuses the filter control;
deletion selects/focuses following projected sibling, then previous, then owning Session. A selected/focused
row that remains projected is pinned materialized. The reducer event, never layout reflow, owns that fallback.

### Unified hierarchy

| `op` | Fields | Answers with |
| --- | --- | --- |
| `open_surface` / `retire_surface` | authenticated connection generation, operation id, closed `new(expected Installation SurfaceRegistry revision)|resume(daemon-minted SurfaceId,owner,state revision)` / exact owning connection+Surface+state revision; mint/resume reserves count+encoded bytes before binding and retire makes the daemon derive the complete≤256-Workspace SurfaceHistoryIndex/revision vector, atomically commits registry/nonreuse high-water+all history invalidations, then releases | `surface_state`+ownership receipt / cross-stream retired receipt |
| `get_hierarchy` | v4: `surface_id,include_archived?`; vNext begin: exact owning Surface/revision, include-archived and closed filter revision, with no caller scan id | v4 full `HierarchySnapshot`; vNext≤6-MiB `HierarchyIndexSnapshot` over≤111,024 compact coordinates plus daemon-minted first `HierarchyScanId`/≤500-row/1-MiB page, with complete encoded response+wrapper≤7,680 KiB |
| `get_hierarchy_page` / `close_hierarchy_scan` | closed `begin(owning Surface,index/filter revision,scope=installation_roots|workspace|session|subtree,max_rows≤500,max_bytes≤1 MiB)` with no caller id, or `continue(daemon-minted HierarchyScanId,pinned daemon/hierarchy/filter/scope revision,next page sequence,predecessor digest,opaque cursor)` / exact owning connection+scan+revision | begin mints scan id; both return≤500 summaries/1 MiB with `complete|partial(next)|gapped(minimum_revision)` / `ack` |
| `reveal_hierarchy_key` | owning Surface, exact HierarchyKey and expected hierarchy/filter revision; daemon derives scope/path | exact≤128-Group ancestor chain plus Workspace/Session and enough≤1-MiB pages to materialise the target, or typed stale/gap; never label matching |
| `get_inspector` | `key: HierarchyKey` | `inspector` |
| `set_tree_expanded` | `surface_id`, connection generation, operation id, Workspace/history generation, `key: HierarchyKey`, object revision, `expanded` | `tree_state` + presentation-history receipt |
| `set_tree_expanded_all` | `surface_id`, connection generation, operation id, Workspace/history generation, expected state revision, `expanded`; sets `expansion_default` and clears exceptions without enumerating rows | `tree_state` + presentation-history receipt |
| `select_tree_node` | `surface_id`, connection generation, operation id, Workspace/history generation, `selected: HierarchyKey?`, prior selection revision | `tree_state` + presentation-history receipt |
| `set_tree_presentation` | `surface_id`, connection generation, operation id, Workspace/history generation, `filters: [TreeFilter]`, `visibility_mode`, `scroll_anchor?`, object revision | `tree_state` + presentation-history receipt |
| `set_surface_view_mode` / `set_inspector_width` | `surface_id`, connection generation, operation id, Workspace/history/object generations and one closed view mode / bounded logical width | surface state + presentation-history receipt |
| `set_board_presentation` / `set_terminal_appearance` | `surface_id`, connection generation, operation id, Workspace/history/object generations and closed board grouping/sort/density / closed terminal theme/font-size/line-spacing fields | surface state + presentation-history receipt |
| `move_tree_node` | `surface_id`, `key`, `before?` | `tree_state` |
| `rename_node` | `session_id`, `node_id`, `name` | `node` |
| `correct_relationship` | `session_id`, `node_id`, `parent_node_id?`, `relationship_kind` | `node` |
| `get_preview_history` | `session_id`, `node_id`, `limit?` (clamped to 20) | `preview_history` |
| `set_preview_visibility` | `session_id`, `node_id`, `visibility` | `ack` |
| `open_node_as_temporary_pane` | exact owning connection+`surface_id`, `session_id`, `node_id`, expected source revision and temporary-Pane count/metadata reservation | daemon-minted `node_pane` with 30-minute idle deadline; no process/PTY/renderer launch |
| `open_node_as_pane` | `surface_id`, `session_id`, `node_id`, `target_pane_id`, `placement` | `layout` |
| `promote_temporary_pane` | `surface_id`, `session_id`, `pane_id`, `target_pane_id`, `placement` | `layout` |
| `focus_pane_for_node` | `surface_id`, `session_id`, `node_id` | `pane_focus` |
| `focus_pane_for_attention` | `surface_id`, `session_id`, `subject_node_id` | `pane_focus` |

`get_hierarchy` is navigation bootstrap. In vNext its compact index contains only key/parent/kind/order/flags/
RowMetricClass and complete/gapped coverage—no label or body. Row summaries are≤2 KiB each. A scan is
memory-only, with 16/connection, 1,024 installation-wide,≤16-MiB metadata and 60-second idle TTL; completion,
close, gap, scope loss, disconnect or expiry releases it, and N+1 never evicts another visible scan. Filter
matches are one revisioned≤16-KiB raw bitmap encoded by the closed packed-bit codec into≤24 KiB on NDJSON.
`list_workspaces`, `list_sessions`, `get_session` and
`get_process_tree` remain useful to administration, search and details, but composing them into a second
navigation tree is a client bug. The hierarchy revision is monotonic for the daemon lifetime; after a revision
gap or daemon identity change, v4 requests its legacy full snapshot while vNext requests a fresh compact index
and only affected visible pages. Viewport/overscan prefetch and reveal are automatic and expose no Load-more
interaction.

`get_inspector` is an on-demand read for the selected hierarchy row. Its response identity must equal the
requested `HierarchyKey`; a client discards a late answer after selection changes. Inspector history is
bounded and redacted, environment values are never projected, and an inferred parent or origin retains its
confidence instead of becoming a fact merely because it appears in a detail panel. Inspector data is not
part of `HierarchySnapshot`, so opening one row does not make every hierarchy refresh carry logs and
configuration.

Presentation writes are per stable daemon-minted `surface_id`. They are not `TurnEvent`s, do not change active Session or
Pane focus, and do not produce a broadcast. `move_tree_node` changes only the stable order of siblings; it
cannot reparent a node or move selection.

`surface_id` is immutable and daemon-minted for one bounded Surface record. `open_surface` alone mints or
resumes it for the exact authenticated SurfaceOwner and atomically transfers connection-generation ownership;
`get_hierarchy` is a pure read and cannot claim it. Connection replacement revokes every ephemeral child's
view/input authority and releases each quiescent child; a still-live/uncertain Turn worker atomically transfers
its existing slot/family/shared-byte reservations to `ProcessCleanupCharge` before the Surface owner can
disappear. Bounded tree expansion/selection remains dormant for≤30 days. Explicit retire/owner deletion/expiry removes
that presentation state and history after monotonic nonreuse high-water persists. Temporary bindings are also
removed when their last client disconnects and when the daemon restarts; they are ephemeral view state, not
restorable process state.

`TemporaryPane` is not a durable Pane/Layout row and never starts a process, PTY or renderer. One Surface owns
at most eight, one connection 32, the installation 512; each record is≤4 KiB inside a 2-MiB aggregate and has
a 30-minute idle deadline refreshed only by exact view activity. Open reserves every count+byte before mint;
close, promotion, source invalidation, Surface retire, connection loss, daemon restart or exact idle expiry
releases it. Promotion first reserves the durable Pane/Layout/core capacity and then atomically swaps ownership;
failure leaves the temporary view unchanged. Each N+1 mints nothing and changes no existing view.

`rename_node` and `correct_relationship` are daemon-authoritative Agent mutations. They reject non-Agent
targets; relationship correction additionally refuses cycles, cross-Session parents and invalid root edge
kinds. Both preserve integration provenance and append a durable audited `TurnEvent`; corrected edges carry
explicit confidence. Clients update from the returned projection and never pretend a local edit succeeded.

`set_preview_visibility: hide` is enforced at the daemon projection and history boundaries: hierarchy
snapshots omit the current activity preview and `get_preview_history` returns no entries. It does not erase
the Process stream or stop the Agent; restoring `inherit` or `show` exposes only stable, redacted preview facts.

`preview_history.entries` is ordered **newest first**. The optional limit is applied to that newest end, so
with six stored facts and `limit: 4` the response contains `[6, 5, 4, 3]`; entry zero is the item Quick Preview
highlights as current.

`focus_pane_for_attention` keeps navigation identity and input ownership separate. The selected semantic
subject remains `subject_node_id`; focusing it does not itself resolve or acknowledge Attention. If that semantic Agent has no attachable runtime, the daemon
may return the nearest Pane-owning ancestor only across an `owns_process` or `spawned_by` relationship at
`integrated` or `explicit` confidence. It never crosses a distinct child runtime, follows a provisional
relationship or creates a Pane. `PaneFocusView.node_id` names the node represented by the actual Pane and
`attention_subject_node_id` names the unchanged semantic subject. A PreviewDetails-only temporary Pane is
not an input channel; a client may close that ephemeral view through presentation-only detach before focusing
the resolved runtime Pane.

### Accepted Agent Node protocol target (not in v4)

ADR-059 changes the presentation contract, not the v4 wire by documentation alone. Until the Rust enums,
store migrations, catalogue tests and GUI ship together, the operations below are reserved target names and
a v4 peer must not send or claim them:

Every state-changing vNext request carries the same closed `MutationEnvelope`; the table below lists only
operation-specific fields:

```text
MutationEnvelope = {
  operation_id, authenticated_principal, capability_id,
  daemon_generation, authority_generation,
  state_streams[]: { state_stream_key, expected_stream_revision },
  expected_object_revisions[],
  foreground?: { surface_id, surface_connection_generation }
}
```

Every object and edge read from one stream contributes its exact id/revision to
`expected_object_revisions`; an operation touching more than one state stream carries every affected stream
key/revision in canonical order. The daemon validates the entire envelope before the first durable or
external effect. Reusing an operation id with the same canonical request returns the original durable
receipt; reusing it with different bytes is `operation_id_conflict`. Missing/stale authority, domain,
object, surface or connection generation has zero effect. Read-only requests omit this envelope unless they
advance a cursor; such an acknowledgement is a mutation. No operation-specific row may weaken these rules.
The sole closed fencing variant is `daemon_current_container_snapshot` for total
`close_session|delete_session|close_workspace|delete_workspace`: the request still pins foreground authority
and exact container identity/generation/disposition, then the daemon acquires that container's serial writer
and derives the complete current stream/object revision vector inside the transaction. Since the operation is
defined over every current descendant, concurrent graph or Attention change is included rather than treated
as stale and cannot force a second user action; the exact container generation still prevents retargeting. A
newly registered mutation is unreachable until the protocol registry declares its streams, client- or
daemon-derived revision recipe, authority class, idempotency fingerprint and remote policy.

`OperationRegistry.vNext` is the global dispatcher registry, not a remote allowlist. It covers every
authenticated request path—native desktop, endpoint broker, reviewed verifier, committed internal policy and
RemoteOperatorSurface—and assigns each exact name one direction, effect class, authority class, daemon-derived
StateStream set/fence recipe, canonical idempotency fingerprint and dispatch policy. Registry membership makes
an operation decodable; it grants no authority by itself. An operation is reachable only when at least one
declared dispatch path can satisfy its authority predicate. Conversely, a name absent from the registry is
unrepresentable even if prose or a client happens to spell it.

`docs/OPERATION_REGISTRY_CAP105_112_VNEXT.tsv` is the authority-hashed machine-readable closure for every
request introduced or normatively changed by CAP-105 through CAP-112. It is a required slice of the eventual
complete generated registry, not permission to omit earlier operations from that complete registry. The TSV
and operation tables are bijective for that slice, and `scripts/verify-operation-registry.sh` also compares
each remotely allowed row with the separately declared RemoteOperatorSurface non-denied projection. A
LocalDesktop-only row must have `remote_policy=deny`, `remote_role_predicate=none` and a native-local dispatch
path; removing it from the remote projection therefore cannot make it unreachable, and adding it to that
projection cannot escalate it.

CAP-107 adds only automatic presentation/attachment reducers over
`TerminalWarmViewPark|TerminalOffscreenClientDetach|TerminalWakeInputBuffer` and existing exact attach/resync
machinery; it deliberately adds no caller-visible `park`, `wake` or `reap` operation. CAP-108 rejects automatic
work reaping, so it deliberately registers no operation or internal termination dispatch. These two no-wire
closures are gate markers: inventing either operation fails the registry audit rather than acquiring a default
policy.

| Planned `op` | Principal fields | Planned answer |
| --- | --- | --- |
| `get_state_snapshot` | exact authorised StateStreamKey and known revision? | `state_snapshot` |
| `subscribe_state_stream` / `unsubscribe_state_stream` | exact authorised StateStreamKey, known revision and event/byte bounds / subscription id | `state_stream_subscription` / `ack` |
| `ack_state_revision` | operation id, exact subscription+StateStreamKey and monotonic applied revision | `ack` |
| `get_node_view` | `surface_id`, `key: HierarchyKey`, `known_revision?`, exact closed content kind and declared logical byte/item limits≤7,680 KiB | logical `node_view`, automatically chunked when>192 KiB and atomically applied |
| `subscribe_node_view` | `surface_id`, exact key/revision, content kind, byte/item bounds | `node_view_subscription` |
| `unsubscribe_node_view` | `surface_id`, subscription id | `ack` |
| `route_attention` | `surface_id`, `surface_connection_generation`, `attention_id?`, `scope?`, daemon generation | `attention_route` |
| `activate_session` | foreground surface/connection, operation id, Session id/revision, activation generation, exact preflighted policy revision and ordered bounded descriptor→preassigned Node/AttemptOwner/AttemptId reservation map, or one preassigned default Shell Node/Attempt when that set is empty | `session_activation` with per-reservation created/attached/refused/reconcile state |
| `update_surface_activity` | `surface_id`, connection generation, focused/foreground/typing/session/sensitive state | `effects` |
| `create_agent_instance` | foreground surface, operation id, exact Workspace/Session/optional parent/tree/target/adapter/account/checkout/capability-catalogue/launch-policy revisions and closed launch spec; daemon preassigns and reserves NodeId+AgentInstanceId+RuntimeAttemptId+RuntimeLaunchIntentId plus receipt/replay/recovery before effect | one Agent Node/Instance/Attempt plus durable `RuntimeLaunchReceipt(created|refused|uncertain)`; zero-effect refusal preserves no partial identity and uncertain reconciliation never relaunches |
| `grant_companion_agent_launch` / `revoke_companion_agent_launch` | LocalDesktopForegroundAuthority, operation id, exact Workspace/Session/target/trust/policy revisions and≤32 immutable allowlisted entries `(TemplateRevision,adapter,AccountProfileRevision,model?,safe-cwd-root,CheckoutPolicy)` with expiry≤24 hours / exact CompanionAgentLaunchGrantId+revision | active grant+receipt / revoked grant+receipt |
| `launch_companion_agent` / `get_companion_agent_launch` / `reconcile_companion_agent_launch` | authenticated CompanionActionEnvelope, operation id, exact active grant+one allowlist entry, current Workspace/Session/target/trust/template/account/adapter/catalogue revisions and preassigned NodeId+AgentInstanceId+RuntimeAttemptId+CheckoutScopeId; no caller command, env, flags, model override, path or parent / intent id or original operation id / new operation id, exact intent+grant+reserved identities+runtime/graph/checkout correlations and original receipt; lookup-only | `companion_agent_launch_intent`+canonical instance/graph/checkout receipt |
| `resume_agent_instance` | foreground surface, operation id, exact Agent Node/AgentInstance and either its latest terminal attempt or `no_prior_attempt` bound to the exact committed ConversationAdoptionReceipt; ConversationKey/current ownership plus `proposed|current` binding, target/adapter/capability and launch-policy revisions, accepted continuity/resume preflight, preassigned RuntimeAttemptId+RuntimeLaunchIntentId+RuntimeLaunchReceiptId and lookup correlation; reserves receipt/replay/recovery before effect and has no fresh-work fallback | same AgentInstance plus its next (or first) RuntimeAttempt/launch receipt and atomic proposed→current binding promotion only after continuity proof, or zero-effect typed refusal |
| `restart_runtime_owner` | LocalDesktopForegroundAuthority or one-use `BulkRestartDispatchAuthority(BulkIdleRestartIntentId,intent_revision,candidate_ordinal,candidate_digest,preassigned_restart_operation_id)`, operation id, preassigned RuntimeLifecycleIntentId, exact tagged AttemptOwner/current live attempt+binding/target/backend/handle/process-start and all generations, accepted relaunch/isolation preflight, preassigned replacement RuntimeAttemptId+RuntimeLaunchIntentId, and exact conversation-continuity proof for an Agent | two-phase restart intent: stop the live attempt once, then create one replacement under the same Tool Node or same Agent Node/instance/conversation; absent Agent continuity refuses and offers separate Fresh Start, never a fallback |
| `prepare_bulk_idle_restart` / `get_bulk_idle_restart` | LocalDesktopForegroundAuthority, exact Workspace/policy/inventory/Attention/interaction/input-lease revisions and closed candidate bound≤256; daemon derives rather than accepts the ordered eligible set and reserves one bounded preview / local authority and exact Workspace+BulkIdleRestartIntentId or original operation id | `bulk_idle_restart_preview` / durable intent+ordered summary |
| `commit_bulk_idle_restart` / `cancel_bulk_idle_restart` / `reconcile_bulk_idle_restart` | LocalDesktopForegroundAuthority, operation id, exact unexpired preview id/revision/digest+all pinned revisions and preassigned per-candidate restart operation ids / new operation id+intent revision; cancellation fences before the next candidate and never interrupts an already-dispatched one / new operation id+intent revision+original receipt; lookup-only against each named restart receipt | `bulk_idle_restart_intent`+overall and per-instance receipts |
| `set_agent_eco_policy` | LocalDesktopForegroundAuthority, operation id, exact Workspace/settings/inventory revision and closed disabled-or-enabled policy with idle threshold≥15 minutes, batch rate≤2/minute and maximum candidates≤256 | resolved settings+policy receipt; enabling never hibernates in the same operation |
| `get_agent_eco_operation` / `cancel_agent_eco_operation` / `reconcile_agent_eco_operation` | exact Workspace+EcoHibernateIntentId or original operation id / foreground new operation id+prepared revision / new operation id+nonterminal revision+original receipt and exact runtime/session correlation; recovery is lookup-only | `eco_hibernate_intent`+receipt |
| `branch_agent_instance` | foreground surface, operation id, exact source Node/AgentInstance/attempt/conversation/binding+lineage revisions, destination Workspace/Session/optional parent/tree/target/adapter/account/checkout/capability-catalogue/launch-policy revisions and closed branch launch spec; daemon preassigns and reserves new NodeId+AgentInstanceId+RuntimeAttemptId+RuntimeLaunchIntentId plus receipt/replay/recovery before effect | one distinct lineage-linked Agent Node/Instance/Attempt plus durable `RuntimeLaunchReceipt(created|refused|uncertain)`; source is unchanged and uncertain reconciliation never relaunches |
| `switch_agent_configuration` | foreground surface, operation id, exact Node/AgentInstance/current attempt/binding/configuration+adapter-capability generations, target/backend/durable-handle/process-start identities and generations, closed patch `{model?,mode?}` with at least one field, optional exact ModelEndpointProfile revision, accepted same-conversation/in-place preflight and preassigned RuntimeAttemptId+RuntimeConfigurationReceiptId | same AgentInstance plus one configuration-epoch attempt/receipt only after proved effective configuration; refusal/uncertainty leaves prior attempt current and never branches/restarts |
| `get_runtime_configuration_operation` / `reconcile_runtime_configuration_operation` | exact RuntimeConfigurationReceiptId or original operation id / foreground new operation id, exact dispatching/uncertain receipt+old attempt/binding/target/backend/handle/process-start/provider correlation and generations; lookup only | current receipt / evidence-refined receipt; reconcile never resends configuration or launches work |
| `get_runtime_launch_operation` / `reconcile_runtime_launch_operation` | exact RuntimeLaunchIntentId or original operation id / foreground new operation id, exact dispatching/uncertain/reconcile-required intent+receipt, owner/preassigned attempt/launch-spec/target/backend and provider/process correlation; lookup/probe only | current receipt / evidence-refined receipt; reconciliation never spawns, retries or remints an attempt |
| `terminate_runtime_owner` / `kill_runtime_owner` | LocalDesktopForegroundAuthority, operation id, preassigned RuntimeLifecycleIntentId, exact AttemptOwner/current RuntimeAttempt/binding, target/trust/backend/handle/process-start/owner/attempt/binding/lifecycle/input-safety generations and reviewed graceful timeout / same exact vector plus forceful consequence review; each atomically reserves intent/receipt/recovery before effect | typed lifecycle intent+receipt; terminate never escalates to kill, kill is never inferred from timeout |
| `recycle_runtime_owner` | LocalDesktopForegroundAuthority, operation id, preassigned RuntimeLifecycleIntentId, exact AttemptOwner/current attempt/binding/target/backend/handle/process-start and all generations, accepted replacement/isolation preflight, preassigned replacement RuntimeAttemptId+RuntimeLaunchIntentId and exact continuity proof when preserving AgentInstance/ConversationKey | two-phase lifecycle intent: old infrastructure is ended once, replacement starts only after definite old-stop evidence; uncertainty never creates a duplicate and refusal offers separate Fresh Start |
| `detach_runtime_view` | operation id, monotonic Surface operation sequence and exact owning connection+Surface+Installation stream/Surface revision+PaneAttachment/attachment generation, AttemptOwner/RuntimeAttempt/PTY/buffer generations and no lifecycle disposition | atomically retires the exact existing PaneAttachment/baseline/batch generation and commits `RuntimeViewReplayFence(kind=detach)`; replay returns `ack` without a lifecycle receipt, and runtime, attempt, input, terminal bytes and semantic ownership are unchanged |
| `destroy_runtime_owner` | LocalDesktopForegroundAuthority, operation id, exact Node+tagged owner/instance/current attempt generations and explicit `terminate|kill|leave_running_recovery` runtime disposition; optional preferred same-Session reparent/rehome destinations are hints only. The daemon serially derives a total per-child/semantic-survivor/RuntimeLifecycleIntent+receipt/replacement RuntimeLaunchIntent/terminal park+detach+wake+shadow+writer state/MediaImport/CommitProposal+Attempt/RepositoryPublishIntent/WebPreviewLoadIntent/BrowserNodeCreationIntent/BrowserNavigationIntent/BrowserDownloadQuarantine/AgentBrowserActionIntent/DocumentPrintIntent/BulkIdleRestartIntent/EcoHibernateIntent/CompanionAgentLaunchIntent/TransferTicket/PortableExport/PortableImport-destination/Attention/Team/Flow/dependency disposition and falls back to tombstone+owning Workspace SemanticRecoveryInventory instead of refusing removal | `deletion_result` with applied disposition vector, durable tombstone and cleanup survivors |
| `get_runtime_attachment_operation` / `reconcile_runtime_attachment_operation` | exact RuntimeAttachmentReceiptId or original operation id / foreground new operation id, exact uncertain receipt+owner/attempt/binding/target/backend/handle/process-start/endpoint-correlation revisions and original receipt; authorised lookup/probe only | original attachment receipt / evidence-refined receipt; reconcile never launches, stops, creates an attempt or repeats backend attach |
| `get_runtime_lifecycle_operation` / `reconcile_runtime_lifecycle_operation` | exact RuntimeLifecycleIntentId or original operation id / LocalDesktopForegroundAuthority, new operation id, exact nonterminal intent+original receipt and target/backend/handle/process-start/provider correlations; lookup/probe only | original lifecycle intent+receipt / evidence-refined receipt; reconcile never signals, stops or launches again |
| `list_execution_targets` / `get_execution_target` | visible owner scope / exact target id | `execution_target_list` / `execution_target` |
| `create_execution_target` / `adopt_execution_target` | foreground surface, operation id, preassigned monotonic ExecutionTargetId, expected Installation catalogue revision and closed local/ssh/custom inert descriptor / same plus exact discovered descriptor and provenance | `execution_target`+receipt |
| `probe_execution_target` | foreground surface, operation id, target id/revision, bounded non-mutating probe policy | `execution_target`+observation receipt |
| `get_pty_capacity` | exact ExecutionTarget/trust/generation and known observation revision | current `pty_capacity_observation` with measured-at, coverage and freshness; never guessed zero/healthy |
| `prepare_pty_capacity_remediation` | LocalDesktopForegroundAuthority, operation id, exact ExecutionTarget/trust/target generation, current complete fresh observation+revision and provider capability/policy revisions; daemon preassigns RemediationIntentId and freezes the current kernel ceiling, persistent-config identity hash-or-proved-absence, fixed helper/provider identity, proposed ceiling and all intent/receipt/replay/journal/correlation/recovery/rollback capacity before review; caller supplies no command/path/config bytes | durable prepared `pty_capacity_remediation_intent` plus one consequence-review projection |
| `apply_pty_capacity_remediation` / `cancel_pty_capacity_remediation` / `reconcile_pty_capacity_remediation` | LocalDesktopForegroundAuthority, exact prepared intent/revision/review digest/current observation+target+provider capability/policy revisions; immediately before dispatch daemon rereads and matches kernel ceiling, persistent-config identity hash-or-absence and fixed helper/provider identity / exact target generation+prepared intent/revision / new operation id, exact target generation+nonterminal intent/revision+original receipt+provider correlation; daemon rereads exact kernel/config/helper/provider journal+rollback proof and never trusts caller observations | remediation intent+receipt with exact before/after/persistent-config/rollback disposition; drift refuses pre-effect and reconcile never reruns elevation |
| `trust_execution_target` / `rotate_execution_target_trust` | LocalDesktopForegroundAuthority, operation id, target id/revision, expected fingerprint/trust generation and independently observed accepted fingerprint | `execution_target`+receipt |
| `bind_execution_target_workspace` / `unbind_execution_target_workspace` | foreground surface, operation id, exact target+Workspace revisions and closed read/runtime/file/repository scopes | `execution_target_binding`+receipt |
| `retire_execution_target` / `delete_execution_target` | foreground surface, operation id, exact target/trust/catalogue revision and explicit survivor/profile/backend/reference disposition | `execution_target_result`+receipt |
| `begin_workspace_onboarding` / `resume_workspace_onboarding` / `get_workspace_onboarding` | foreground surface, operation id, target generation and exact `create_directory`, `open_directory`, `clone_repository` or `adopt_ssh_target` intent / WorkspaceOnboardingId+revision / id | `workspace_onboarding` |
| `cancel_workspace_onboarding` / `reconcile_workspace_onboarding` | foreground surface, onboarding id/revision and expected phase / same plus exact phase receipt and observed external identity | `workspace_onboarding` |
| `publish_repository` | LocalDesktopForegroundAuthority, operation id, preassigned RepositoryPublishIntentId, exact hosted RepositoryAuthority, target/repository/destination/visibility/branch/tree/commit/expected remote ref/upstream/config/credential-reference revisions, canonical `non_primary` classification plus Turn-owned isolated-worktree generation/active lease and consequence review; reserves active/terminal/journal/correlation/recovery before the first effect | `repository_publish_intent`+phase receipt |
| `cancel_repository_publish` | LocalDesktopForegroundAuthority, new operation id, exact RepositoryPublishIntentId+prepared revision and complete no-effect proof; refuses after creating_remote begins | cancelled publish receipt |
| `get_repository_publish` / `reconcile_repository_publish` | RepositoryPublishIntentId or original operation id / LocalDesktopForegroundAuthority, new operation id, exact intent/authority/destination/object/ref/config/CheckoutScope/lease/provider-correlation revisions and original receipt; lookup only, never create/push/write/rotate/reacquire | publish intent+published/no-effect/partial/reconcile-required receipt |
| `create_runtime_node` | foreground surface, `operation_id`, Session, closed non-agent NodeKind and launch spec | `node_view` |
| `create_resource_node` | foreground surface, operation id, preassigned NodeId, exact Workspace/Session revisions, optional same-Session GroupId+revision and GroupTreeRevision, closed non-Group resource kind and typed payload | `node_view`+creation receipt |
| `create_group` | foreground surface, operation id, preassigned Group NodeId, exact Workspace/Session revision, optional parent GroupId+revision, expected GroupTreeRevision and declared position; same-Session/depth≤128/cycle CAS before insertion | `group_tree`+creation receipt |
| `load_web_preview` | foreground owning connection+Surface, operation id, preassigned WebPreviewLoadIntentId, exact WebPreview Node/source revision, private canonical HTTPS URL≤4 KiB/2,048 scalars+hash, policy/DNS generation, closed UTF-8 plain/Markdown/HTML MIME,≤10 redirects,≤8-MiB transferred/16-MiB decoded/20:1/30-second limits and active/receipt/replay/journal/recovery/body/renderer/shared-memory reservations; daemon fetches only the top-level body and passes one sanitised bundle to a socketless renderer | `web_preview_load_intent`+ephemeral state |
| `get_web_preview_load` / `reconcile_web_preview_load` | WebPreviewLoadIntentId or original operation id / new operation id, exact intent/Node/source/URL/policy/Surface/HTTP-correlation revisions and original receipt; lookup only, never DNS/fetch/read/render | loaded/failed/fetch-unconfirmed/reconcile-required receipt |
| `close_web_preview` | exact owning connection+Surface+WebPreviewLoadStateId/revision; atomically fences HTTP/renderer correlation, requests stop and retains all count/body/renderer/shared charges until proved quiescent | `closed(no_result)` after socket/worker/buffer cessation proof, or `reconcile_required` with the original intent/correlation and no refetch |
| `create_browser_node` | foreground surface, operation id, preassigned Browser NodeId, exact Workspace/Session/optional Group graph revisions, isolated partition policy and exact initial inert/no-load BrowserUrl≤4 KiB/2,048 scalars; reserves one of100,000 installation-lifetime≤512-byte/48-MiB creation replay fences and persists BrowserNodeCreationIntent | `node_view`+creation receipt |
| `adopt_agent_browser_control` / `revoke_agent_browser_control` | LocalDesktopForegroundAuthority, operation id, exact Workspace/capability-policy revision, expiry≤24 hours and≤64 reviewed public-HTTPS origin rules / exact AgentBrowserControlGrantId+revision; cloned/shared flags are evidence only and cannot select either transition | active-or-rejected grant receipt / terminal revoke receipt |
| `agent_browser_create` / `agent_browser_navigate` / `agent_browser_read` / `agent_browser_click` / `agent_browser_type` | authenticated exact AgentInstance+RuntimeAttempt+attempt/binding/adapter generations, active grant+Workspace policy revision, agent-created Browser Node ownership or preassigned isolated logged-out Node/partition, expected Browser/navigation/DOM-accessibility revision and one closed typed payload; create/navigate reuse named Browser creation/navigation sub-intents, read is≤256 KiB, click names one stable element id, and type carries≤4 KiB inert text to one named non-secret field | `agent_browser_action_receipt` plus canonical Browser subreceipt or request-only bounded read result |
| `stop_agent_browser_control` / `get_agent_browser_action` / `reconcile_agent_browser_action` | foreground Surface Stop or LocalDesktopForegroundAuthority revoke with exact grant/owner/Node/action revisions / exact action id or original operation id / new operation id, exact action+grant+owner+Browser generations and original receipt; lookup-only, never clicks/types/navigates/creates | fenced Node/action state / action receipt |
| `navigate_browser` / `browser_back` / `browser_forward` / `reload_browser` / `stop_browser` | foreground surface, operation id, preassigned BrowserNavigationIntentId, exact Browser Node/partition/navigation revision and public BrowserUrl≤4 KiB/2,048 scalars or history entry≤8 KiB, or reviewed local-HTML canonical root+regular descriptor+file identity/hash+declared bytes≤8 MiB and BrowserLocalSnapshot count/aggregate reservation;≤10 redirects and active/terminal/replay/journal/recovery plus worst-case history/renderer/partition/shared-RSS capacity reserve; dispatching is durable before any load/history/stop effect | `browser_navigation_intent`+receipt, with oversize/eleventh redirect stopped as `bounded_redirect(url_oversize|redirect_count)` before follow/history commit |
| `open_reviewed_browser_popup` | foreground surface, operation id, source Browser/partition/navigation revision, exact popup origin/target/consequence review, isolated policy generation, preassigned Browser NodeId and destination Workspace/Session/optional Group graph revisions; daemon persists BrowserNodeCreationIntent before load | Browser Node+creation receipt |
| `accept_reviewed_browser_download` | foreground surface, operation id, exact sealed BrowserDownloadQuarantineId/revision and Browser/partition/navigation/response/descriptor/size/type/hash identity, preassigned TransferTicketId+File NodeId/blob id and destination Workspace/Session/optional Group graph revisions; reserves a ticket slot then atomically transfers descriptor+shared byte charge into a `browser_download→turn_file_resource` create-new ticket without copy/redownload | `transfer_ticket`+creation receipt |
| `get_browser_download_quarantine` / `discard_browser_download_quarantine` / `reconcile_browser_download_quarantine` | exact Workspace+quarantine id/revision / foreground operation id, exact sealed-or-pre-effect revision and no-ticket disposition / new operation id, exact transferring-ownership-or-reconcile revision, original receipt, quarantine/ticket descriptors and reserved identities; reconcile is lookup-only | quarantine+receipt / terminal-or-retained quarantine / quarantine-or-ticket receipt |
| `get_browser_node_creation` / `reconcile_browser_node_creation` | BrowserNodeCreationIntentId or original operation id / new operation id, exact intent/source/policy/destination revisions, original receipt and preassigned NodeId; lookup only, never reload | creation receipt with created/proved-absent/reconcile-required |
| `get_browser_navigation` / `reconcile_browser_navigation` | BrowserNavigationIntentId or original operation id / new operation id, exact intent/Node/partition/navigation/policy/source-or-history/renderer-correlation revisions and original receipt; lookup only, never load, traverse, reload or stop | navigation intent+applied/no-effect/dispatched-unconfirmed/reconcile-required receipt |
| `clear_browser_storage` | foreground surface, operation id, exact Browser Node/partition generation and consequence review | `browser_storage_receipt` |
| `update_resource_node` | foreground surface, exact node, expected revision, typed patch | `node_view` |
| `delete_resource_node` | foreground surface, operation id, exact node/content revision, expected context-graph+Attention revisions, closed per-ContextLink revoke vector, in-flight-read fence and per-live-reference tombstone/provisional-route disposition; Group additionally requires `refuse`, `promote_children` or `move_children_to_session`, GroupTreeRevision and `binding=some(CheckoutScopeBindingId,binding_revision,scope_revision)|none(proved_at_GroupTreeRevision)` | `deletion_result` |
| `set_group_membership` / `move_group_subtree` | foreground surface, operation id, exact same-Session node or Group, parent Group?, expected Node+GroupTreeRevision | `node_view` / `group_tree` |
| `repair_group_tree` | local foreground surface, exact corrupt GroupTreeRevision, bounded proved prefix and closed edge-removal/reparent repair plan | `group_tree` |
| `create_checkout_scope` / `adopt_checkout_scope` | foreground surface, exact existing-or-preassigned Session, target/trust/repository/worktree, creator `turn_created` or `adopted`, optional preassigned Group projection; one composite operation id | `checkout_scope_provisioning_receipt` |
| `bind_group_checkout_scope` | foreground surface, operation id, preassigned CheckoutScopeBindingId, same-Session Group+CheckoutScope and exact Group/GroupTree/scope revisions | `checkout_scope_binding` |
| `unbind_group_checkout_scope` | foreground surface, operation id, exact CheckoutScopeBindingId+binding revision, same-Session Group+CheckoutScope and exact Group/GroupTree/scope revisions | `checkout_scope_binding` |
| `move_and_rehome` | foreground surface, exact subtree, target CheckoutScope, GroupTree revision and complete stopped-descriptor preflight | `group_tree_rehome_receipt` |
| `unbind_checkout_scope` | foreground surface, operation id, exact scope+revision, retained-worktree disposition and `binding=some(CheckoutScopeBindingId,binding_revision,GroupTreeRevision)|none(proved_at_GroupTreeRevision)` | `checkout_scope` |
| `remove_checkout_scope` | local foreground consequence review, operation id, exact scope+revision, dirty/unpublished/owner/survivor inventory and the same required tagged binding proof | `checkout_scope` |
| `reconcile_checkout_scope` | foreground surface, operation id, exact scope/reconciliation revision, original operation id/origin/desired terminal/possible effect, target inventory revision and the same required tagged binding proof | `checkout_scope` |
| `get_display_name_facts` | exact Node/Group and revision | `display_name_facts` |
| `set_local_display_name` / `unpin_local_display_name` | foreground surface, exact Node/Group revision and bounded sanitised alias / expected pinned fact revision | `display_name_facts` |
| `generate_name_proposal` / `apply_name_proposal` | foreground surface, exact target/source/redaction/generator/bounds / exact NameProposalId, target revision and expiry | `name_proposal` / `display_name_facts` |
| `create_work_item` | foreground surface, operation id, exact destination Session/optional Group and expected tree/session/group revisions, bounded initial title/body/link/priority/due/tags/assignee; state is always backlog | `node_view` with WorkItemId+NodeId |
| `update_work_item_metadata` | foreground surface, operation id, exact WorkItemId+NodeId/revision and closed patch containing only current Turn-authority fields | `node_view` |
| `archive_work_item_projection` / `forget_work_item_projection` / `restore_work_item_projection` | foreground surface, operation id, exact WorkItemId+NodeId, expected projection/binding revisions; local CAS only | `work_item_projection_receipt` |
| `delete_work_item` | local foreground consequence review, operation id, exact WorkItemId+NodeId/item/projection/binding+Attention revisions, zero active mutation/conflict and closed per-live-reference tombstone/provisional-route disposition; provider effect forbidden | `deletion_result` |
| `create_work_item_source` / `update_work_item_source` / `validate_work_item_source` | local foreground surface, operation id, exact source/target, RepositoryHostProfileId+revision, active RepositoryHostCapabilityGrantId(kind=work_item_source)+revision/scope/expiry, credential generation, mapping/filter/rate/bounds generations / source+revision and same grant fences plus patch / source+revision probe with same grant fences | `work_item_source` |
| `list_work_item_sources` / `get_work_item_source` / `query_work_items` | visible source/profile scope / exact source+revision / exact source/profile/project/source+mapping+filter+credential generations, filters/sort and closed `begin(max_rows≤500,max_bytes≤1 MiB)` or `continue(authenticated cursor≤512 bytes,page ordinal,predecessor digest)` | `work_item_source_list` / `work_item_source` / request-only `work_item_page` with≤500 safe summaries/≤2 KiB each/≤1 MiB logical, exact source revision+ordinal+predecessor and `complete|partial(next_cursor)|gapped(minimum_revision)` coverage |
| `refresh_work_item_source` / `get_work_item_sync_run` / `cancel_work_item_sync_run` | foreground surface, operation id, exact source/mapping/filter/credential generations and bounded scope / SyncRunId+revision / same expected nonterminal revision; applying cancellation fences the next page and returns last applied cursor/coverage after the current atomic page | `work_item_sync_run` |
| `delete_work_item_source` | local foreground surface, operation id, exact source+generation/revision and one closed disposition (`detach_bindings` or `delete_local_items`); zero provider item mutation | `work_item_source_result` |
| `import_external_work_item` | foreground surface, operation id, exact fresh WorkItemKey/source+sync+mapping revision, destination Session/optional Group and expected tree revisions; installation-wide ownership CAS | `node_view` or existing authorised reference/refusal |
| `rebind_external_work_item` / `detach_external_work_item` | foreground surface, operation id, exact WorkItemId+NodeId+BindingId/binding revision, new fresh WorkItemKey+source revisions / detach disposition | `work_item_binding_receipt` |
| `create_external_work_item` | foreground surface, operation id, destination Session/optional Group and expected tree revisions, exact source/profile/project/generations, proved creation-correlation mode and mapped initial fields; daemon reserves WorkItemId+NodeId+CreationId+intent before dispatch | `work_item_creation_receipt` |
| `cancel_external_work_item_creation` | foreground surface, operation id, exact WorkItemId+NodeId+WorkItemCreationId and expected prepared intent revision; zero source effect after CAS | `work_item_creation_receipt` |
| `edit_external_work_item` | foreground surface, operation id, exact WorkItemId+NodeId+BindingId+WorkItemKey, binding/source/mapping/item revisions and closed non-comment/non-assignee/non-state field patch | `work_item_mutation_receipt` |
| `comment_external_work_item` / `assign_external_work_item` | foreground surface, operation id, same exact binding fences plus bounded comment with client CommentId / mapped assignee | `work_item_mutation_receipt` |
| `transition_external_work_item` / `close_external_work_item` / `reopen_external_work_item` | foreground surface, operation id, same exact binding fences plus exact mapped transition/reason | `work_item_mutation_receipt` |
| `get_work_item_mutation` / `reconcile_external_work_item_mutation` | exact WorkItemCreationId or MutationIntentId+original operation id / new operation id, same tagged subject, expected intent/binding/source revisions and provider correlation; lookup only | `work_item_creation_receipt` / `work_item_mutation_receipt` |
| `resolve_work_item_conflict` | foreground surface, operation id, exact WorkItemId+BindingId+ConflictId and expected conflict/local/source/mapping revisions with closed per-field choices | `work_item_mutation_receipt` |
| `open_file_for_edit` / `close_file_edit` | foreground connection+Surface, exact FileBackend target/root/path, byte/encoding bounds; daemon reserves per-connection/global count+aggregate bytes before descriptor read / exact owning connection+Surface+FileEditSnapshotId+revision | daemon-minted `file_edit_snapshot` / `ack` |
| `save_file_edit` | foreground surface, operation id, preassigned FileSaveIntentId, exact owning FileEditSnapshotId/revision+target/root/path/file identity/hash/revision, intended bytes≤8 MiB+after hash and daemon-minted owner-only sibling temporary identity; daemon reserves active/temp/terminal capacity before byte one | `file_save_intent`+receipt and advanced-or-unchanged snapshot revision |
| `get_file_save` / `reconcile_file_save` | FileSaveIntentId+revision or original operation id / new operation id, exact intent/target/root/file/temp identities, before+after hashes and original receipt; lookup only and never writes, renames, deletes or repeats replace | `file_save_intent`+receipt |
| `open_document_view` / `close_document_view` | foreground owning connection+Surface, exact local-or-remote FileBackend target/trust/root/regular descriptor/file identity/revision/hash, declared closed MIME and source size≤256 MiB; reserve view/blob/decode/cache/index/shared-memory bounds before read / exact DocumentViewId+revision; close fences decoder/object-URL/search/print-spool generations and waits for quiescence | daemon-minted isolated `document_view_state` or typed refusal / `closed|cleanup_pending` |
| `control_document_view` | exact owning connection+Surface+DocumentView/source/blob revisions and one closed `page(1..=page_count≤10,000)|zoom(250..=8000 permille)|rotate(0|90|180|270)|fit(width|page)|search(query≤4 KiB,next|previous,wrap)|clear_search`; no content bytes or print effect | advanced ephemeral state plus bounded request-only `TextSearchResultPage` where applicable |
| `prepare_document_print` / `commit_document_print` / `cancel_document_print` / `get_document_print` / `reconcile_document_print` | LocalDesktopForegroundAuthority, operation id, exact DocumentView/source/blob/page-selection/layout revisions and printer capability digest; prepare persists no spool and returns one consequence review / exact prepared intent+review digest and a≤64-MiB isolated spool reservation / new operation id+pre-dispatch revision / id or original operation id / new operation id+intent/printer correlation+original receipt; recovery is lookup-only | `document_print_intent`+body-free receipt with `printed|not_printed|submitted_unconfirmed|reconcile_required` |
| `list_directory` / `close_directory_scan` | closed `begin(FileBackend target/trust/root/directory identity, expected observed revision, page bounds)` with no ScanId, or `continue(daemon-minted DirectoryScanId,pinned directory revision,next page sequence,predecessor cursor digest,opaque cursor)` / exact connection+DirectoryScanId+revision | `directory_page` with minted scan id/pinned revision and complete/partial/gapped coverage / `ack` |
| `watch_directory` / `unwatch_directory` | exact complete directory revision, target/root generations and event/byte/backpressure bounds / DirectoryWatchId+generation | `directory_watch` / `ack` |
| `get_commit_graph` | exact RepositoryId/revision, traversal root/order, cursor and ≤500-page/10,000-scan bounds | `commit_graph_page` with parent ids/coverage |
| `get_commit_changed_files` | exact RepositoryId/revision, commit object id, cursor and ≤1,000-page bound | `commit_changed_files_page` |
| `get_repository_status` / `get_repository_diff` / `list_repository_branches` / `get_repository_conflicts` | exact `RepositoryAuthority=filesystem(backend,target/trust,RepositoryId,CheckoutScopeId+revision)|hosted(the same plus RepositoryHostProfileId+revision and active RepositoryHostCapabilityGrantId(kind=repository_backend)+revision/scope/expiry)`, canonical checkout identity, `primary|non_primary` classification and expected repository/index/worktree revisions plus bounded query/cursor | bounded revisioned status / diff / branch / conflict page with complete/partial/gapped coverage |
| `stage_repository_paths` / `unstage_repository_paths` | foreground surface, operation id, preassigned RepositoryMutationIntentId, same exact filesystem-or-hosted RepositoryAuthority/canonical-classification fences, required `non_primary` proof, owned isolated-worktree generation+lease when Turn-managed, expected index/worktree revisions and bounded descriptor-identity path set plus sealed postcondition | repository mutation intent+receipt with new revisions |
| `commit_repository` / `commit_and_push_repository` | local or authorised remote foreground surface, operation id, preassigned RepositoryMutationIntentId, filesystem-or-hosted RepositoryAuthority with required `non_primary` proof, staged-index revision/hash, sealed tree+parent vector+expected commit object id, reviewed bounded message, author policy and expected branch revision / LocalDesktopForegroundAuthority plus required hosted authority, exact upstream/destination/credential generation, old/new remote ref ids and correlation | repository mutation intent with commit receipt / product-state commit and separately correlated push outcomes |
| `fetch_repository` / `pull_repository` / `push_repository` | hosted RepositoryAuthority only, local or authorised remote foreground surface, operation id, preassigned RepositoryMutationIntentId, exact profile/grant/CheckoutScope/canonical-classification fences with required `non_primary` proof, remote/refspec, expected local/remote observation revisions and finite sealed postcondition / LocalDesktopForegroundAuthority, same fences, exact upstream and closed merge-or-rebase plan / LocalDesktopForegroundAuthority, same fences and exact upstream/refspec old/new object ids | repository mutation intent+network receipt with applied/no-effect/reconcile-required evidence |
| `create_repository_branch` / `switch_repository_branch` | foreground surface, operation id, preassigned RepositoryMutationIntentId, filesystem-or-hosted RepositoryAuthority/canonical-classification fences with required `non_primary` proof, validated branch name, start object where creating and exact current HEAD/index/worktree revisions plus sealed postcondition | repository mutation intent+branch receipt |
| `initialize_repository` | LocalDesktopForegroundAuthority, operation id, preassigned RepositoryMutationIntentId+RepositoryId, exact confined empty regular directory descriptor under a Turn-created non-primary isolated CheckoutScope, target/trust/root/lease revisions, initial branch name and sealed empty-tree/config postcondition; aliases, existing repository metadata and primary checkout are refused | repository mutation intent+new canonical RepositoryAuthority receipt |
| `checkout_repository_commit` / `rename_repository_branch` / `delete_repository_branch` | LocalDesktopForegroundAuthority, operation id, preassigned RepositoryMutationIntentId, exact non-primary RepositoryAuthority/fence/lease plus HEAD/index/worktree/ref revisions and respectively immutable commit id+detached disposition / old+new ref names and expected object / branch ref+object, merged-or-reviewed-unmerged proof and survivor/upstream disposition | repository mutation intent+typed ref/HEAD receipt |
| `stash_repository_changes` / `pop_repository_stash` | LocalDesktopForegroundAuthority, operation id, preassigned RepositoryMutationIntentId, exact non-primary authority/fence/lease, HEAD/index/worktree/stash-ref revisions, bounded included path identities and preflighted expected stash object+post-state / exact stash object/ref plus expected clean-or-reviewed conflict plan and separate `worktree × index × stash_ref` postconditions | repository mutation intent+product-state stash receipt; pop never drops stash until every apply postcondition is proved |
| `merge_repository_branch` / `rebase_repository_branch` / `revert_repository_commit` | LocalDesktopForegroundAuthority, operation id, preassigned RepositoryMutationIntentId, exact non-primary authority/fence/lease and bounded finite preflight plan≤1,000 commits/10,000 paths with all HEAD/ref/index/worktree/object revisions, strategy from a closed enum and expected applied-or-conflicted postcondition | repository mutation intent+phase receipt preserving exact applied/conflicted/aborted/reconcile-required evidence |
| `force_push_repository` | LocalDesktopForegroundAuthority, operation id, preassigned RepositoryMutationIntentId, hosted non-primary RepositoryAuthority, active host grant, exact local source object, remote name/ref and mandatory observed expected remote object (`force_with_lease` only), protected-branch policy revision and consequence review; raw force, wildcard refspec and unknown remote tip are unrepresentable | repository mutation intent+remote receipt `applied|lease_rejected|no_effect|reconcile_required` |
| `resolve_repository_conflict` / `discard_repository_changes` / `cleanup_repository_worktree` | LocalDesktopForegroundAuthority and consequence review, operation id, preassigned RepositoryMutationIntentId, filesystem-or-hosted RepositoryAuthority/canonical-classification fences with required `non_primary` proof, immutable conflict ids and per-file resolutions / exact descriptor identities+expected hashes / exact Turn-owned worktree identity+lease/process/branch disposition, each with finite sealed postcondition | repository mutation intent+destructive receipt with post revisions and survivor evidence |
| `get_repository_mutation` / `reconcile_repository_mutation` | RepositoryMutationIntentId+revision or original operation id / new operation id, exact intent/authority/fence/precondition/postcondition/suboutcome vector and original receipt; lookup-only, never Git mutation, network retry, credential change or cleanup | repository mutation intent+receipt with applied/no-effect/reconcile-required evidence |
| `search_view_text` / `move_text_search_cursor` / `close_text_search` | closed `begin(exact Surface, source=terminal(AttemptOwner,buffer generation,first/last seq,cell-grid revision)|text(document revision,content hash), query≤4 KiB, page≤200 matches/200 KiB)` with no id, or `continue(TextSearchSessionId+revision,page ordinal,predecessor digest,authenticated cursor≤512 bytes)`; observed-scan cap≤10,000 and source cap `terminal(cells≤1,000,000,logical_lines≤100,000)|text(utf8_bytes≤16 MiB)` / TextSearchSessionId+revision, direction=`next|previous` and wrap=`allow|stop` / id+revision | request-only `TextSearchResultPage` with≤200 typed matches/≤1 KiB each/≤200 KiB logical, exact ordinal/predecessor and `coverage=complete|bounded(observed_count,next_cursor)`; cursor result `match(index,observed_count,wrapped)|no_match|stale`, with no_match only under complete coverage / same result / `ack` |
| `prepare_media_import` / `get_media_import` | foreground surface, operation id, destination Session/Group revisions, reviewed pinned backend descriptor or pasted-stream total/hash, declared MIME and≤256-MiB item; atomically reserves active+terminal+recovery and owning-Workspace physical blob/temp bytes before read/chunk / MediaImportId+revision | `media_import` including reserved MediaImportStreamId when streamed |
| `put_media_import_chunk` | authenticated full-GUI surface, operation id, exact MediaImportId/revision+MediaImportStreamId/generation, monotonic chunk index/offset, bytes≤4 MiB, chunk hash and optional final total/hash; bounded backpressure | `media_import_chunk_receipt` |
| `commit_media_import` / `cancel_media_import` / `reconcile_media_import` | foreground surface, new operation id, MediaImportId+validated revision+sealed hash+reserved Node/blob identities and destination revisions / new operation id+id+any nonterminal revision / new operation id+id+committing-or-reconcile revision, original operation receipt and reserved identities | `node_view`+receipt / `media_import` / `media_import`+receipt |
| `control_media_playback` | foreground owning connection+Surface and closed `begin(exact Media Node/blob generation,expected no current,state/family/shared-memory reservation,play)|current(MediaPlaybackStateId+revision+playback generation,play|pause|stop|seek_ms|set_muted|set_volume_millipercent(0..1000)|select_caption_track(id≤64 bytes/32 scalars or none))`; same-Surface replacement reserves before atomic swap | ephemeral state/revision≤32 KiB with codec/container ids≤64 ASCII bytes or closed error enum, elapsed/known-or-unknown duration, mute/volume and≤64 tracks of BCP-47 language≤35 ASCII bytes, closed kind and inert label≤128 bytes/64 scalars, or stopped ack |
| `list_repository_host_profiles` / `get_repository_host_profile` | exact target/host scope / profile id+revision | list / `repository_host_profile` |
| `create_repository_host_profile` / `adopt_repository_host_profile` | local foreground surface, operation id, exact target/trust/canonical host/account/scopes and external credential reference | `repository_host_profile` |
| `authenticate_repository_host_profile` / `rotate_repository_host_profile` | LocalDesktopForegroundAuthority, operation id, preassigned RepositoryHostCredentialIntentId, exact profile/target/trust/canonical-host/account/scopes/profile+old-credential generations, pre-reserved next generation, broker policy, provider correlation and expiry; rotate atomically degrades profile and revokes all active grants before dispatch | credential intent+profile/grant receipt |
| `get_repository_host_credential_operation` / `cancel_repository_host_credential_operation` / `reconcile_repository_host_credential_operation` | intent id+revision or original operation id / LocalDesktopForegroundAuthority, new operation id and exact prepared revision / same authority, new operation id, exact dispatching/awaiting/reconcile revision, original operation+provider correlation; lookup only | intent / terminal intent / intent+profile receipt |
| `validate_repository_host_profile` | LocalDesktopForegroundAuthority, operation id, exact profile/target/trust/current credential generation and correlated credential-intent receipt plus bounded validation policy | `repository_host_profile`+validation receipt |
| `grant_repository_host_capability` / `revoke_repository_host_capability` | local foreground surface, operation id, exact profile/target/trust/host/account/credential revisions and one RepositoryBackend-or-WorkItemSource repository/project scope with expiry / exact RepositoryHostCapabilityGrantId+revision | independent scoped grant / terminal grant receipt |
| `revoke_repository_host_profile` / `delete_repository_host_profile` | LocalDesktopForegroundAuthority, profile+revision, operation id and exact credential-intent/grant/credential-reference survivor disposition; revoke fences late callbacks and delete requires terminal intents | profile result |
| `list_commit_proposal_provider_profiles` / `get_commit_proposal_provider_profile` | installation scope / profile id+revision | safe profile list / profile |
| `create_commit_proposal_provider_profile` / `adopt_commit_proposal_provider_profile` / `update_commit_proposal_provider_profile` / `validate_commit_proposal_provider_profile` / `retire_commit_proposal_provider_profile` / `delete_commit_proposal_provider_profile` | LocalDesktopForegroundAuthority, operation id, exact expected profile revision where present, sandboxed executable canonical descriptor+SHA-256 or ModelEndpointProfile+route revision, sandbox-policy revision and wall/cpu/process/RSS/stdout/stderr limits; no secret bytes | profile+receipt |
| `generate_commit_proposal` | foreground surface, CommitProposalId/operation id, exact filesystem-or-hosted RepositoryAuthority, RepositoryId+revision, staged-index revision/hash and CommitProposalProviderProfile revision; daemon reads that exact index via RepositoryBackend and creates the bounded redacted snapshot | `commit_proposal` |
| `get_commit_proposal` / `apply_commit_proposal_to_editor` / `discard_commit_proposal` | id+revision / id+ready revision and exact editor draft revision / id+revision | proposal / editor draft receipt / proposal |
| `prepare_transfer` / `get_transfer` | foreground review under the caller's normal local/full-GUI capability, exact owning WorkspaceId+revision, TransferTicketId/operation id, source and destination each as independently fenced backend descriptor (target/trust/root/descriptor generations), authenticated client stream (client/session/surface generation), source-only Browser download identity or destination-only reserved Turn File-Resource Node/blob+graph revisions, size≤2 GiB, hash, chunk≤4 MiB, create-new policy and expiry≤30m / owning Workspace+id+revision | `transfer_ticket` |
| `start_transfer` / `pause_transfer` / `resume_transfer` / `cancel_transfer` / `reconcile_transfer` | exact ticket/revision/chunk ledger and operation id; reconcile adds original receipt and destination identity | `transfer_ticket`+receipt |
| `put_transfer_chunk` / `get_transfer_chunk` | authenticated full-GUI surface, operation id, exact TicketId/revision, client/session/surface generation, client-stream endpoint role, monotonic chunk index/offset, expected chunk hash and put bytes≤4 MiB; get returns only reviewed source chunk | `transfer_chunk_receipt` / bounded chunk+receipt |
| `set_content_projection` / `clear_content_projection` | owning connection+Surface, operation id, exact bounded Terminal/Note/editor source id+revision+hash and plain-or-markdown mode; daemon reserves count/bytes, selects current trusted sanitizer and atomically replaces only after success / exact owning connection+Surface+ContentProjectionId/revision | `content_projection` / `ack` |
| `open_reviewed_content_projection_link` | foreground surface, operation id, exact ContentProjectionId/revision+source id/revision/hash, LinkId+normalised URL/text hash, isolated Browser policy generation, preassigned Browser NodeId and destination Workspace/Session/optional Group graph revisions; persists the same BrowserNodeCreationIntent before any load | reviewed Browser Node+creation receipt |
| `get_command_catalogue` / `search_command_catalogue` / `close_command_catalogue_scan` | closed begin(surface/connection, evaluation scope=`installation_zero_state(installation revision, optional ExecutionTarget/trust)` or `workspace(Workspace id+revision, optional selected ViewTarget+revision, ExecutionTarget/trust)`, StateWatermark, known catalogue revision, page≤200/response≤1 MiB) without scan id, or continue(daemon-minted CatalogueScanId,pinned scope+catalogue+evaluation watermark,next page sequence,predecessor cursor digest,opaque cursor) / same scope plus revision, normalised query≤256 bytes, scan≤10,000 and result≤200/1 MiB / exact connection+CatalogueScanId+revision | entries (label≤512 bytes/128 scalars, ≤32 keywords×64 bytes, schema≤16 KiB, reason≤2 KiB) with daemon-evaluated availability+watermark and complete/partial/gapped cursor / bounded search results / `ack` |
| `get_signing_trust_store` | one exact `command_extension|product_announcement|update_manifest|update_package|voice_model_manifest` domain and known revision | safe domain/store revision, active key ids+epochs, revocation/high-water digests; never key material |
| `install_signed_command_extension` / `revoke_signed_command_extension` | LocalDesktopForegroundAuthority, operation id, exact command-extension SigningTrustStore revision, closed SignedEnvelopeV1+structured payload / extension id+revision and current store/revocation fence; raw executable/path/new-operation fields are unrepresentable | catalogue mutation+signature receipt |
| `invoke_command_catalogue_entry` | foreground surface, operation id, exact catalogue revision+CommandEntryId, typed schema values and every target/capability revision | canonical typed operation receipt |
| `register_local_command_catalogue_entry` / `update_local_command_catalogue_entry` / `revoke_local_command_catalogue_entry` | LocalDesktopForegroundAuthority, operation id, exact catalogue revision, stable local entry id, category, bounded label/keywords, registered operation/schema and declared capability/consequence / same id+revision patch / same id+revision | catalogue mutation receipt |
| `set_command_shortcut_binding` / `revoke_command_shortcut_binding` | LocalDesktopForegroundAuthority, operation id, exact catalogue/entry revisions, ShortcutBindingId, platform, global-or-Workspace scope, normalised chord and closed `admit_disabled_conflict|explicit_replace(chosen and displaced binding ids/revisions)` policy / exact binding+revision | command shortcut receipt |
| `list_announcements` / `get_announcement` | exact signed channel/platform audience, expected product-announcement trust-store revision and known feed revision / AnnouncementId+revision+accepted envelope identity | bounded announcements / `announcement` |
| `dismiss_announcement` / `open_reviewed_announcement_link` | surface, operation id, exact announcement revision/state / foreground surface, operation id, exact signed AnnouncementId/revision/audience+LinkId/normalised URL hash, HTTPS consequence review, isolated Browser policy generation, preassigned Browser NodeId and destination Workspace/Session/optional Group graph revisions; persists the same BrowserNodeCreationIntent before any load | announcement receipt / reviewed Browser Node+creation receipt |
| `discover_application_update` / `get_application_update` | foreground surface, operation id, exact channel/platform/architecture/current-version, expected update-manifest/update-package trust-store revisions and current anti-rollback high-water; caller cannot select key/epoch and discovery reserves the one current intent plus receipt / UpdateIntentId+revision | `application_update` |
| `download_application_update` / `resume_application_update` / `cancel_application_update` | foreground surface, exact intent/manifest/revision/chunk ledger, operation id and declared-size reservation in the one ≤2-GiB package allocation | `application_update` |
| `verify_application_update` / `stage_application_update` / `discard_application_update` | exact intent, canonical signed manifest+package envelope identities, parent-manifest hash, both current trust-store revisions, package/revision and operation id | `application_update` |
| `apply_application_update` / `rollback_application_update` / `reconcile_application_update` | LocalDesktopForegroundAuthority, exact staged revision, daemon-derived LiveUpdatePlan plus daemon/protocol/PTY inventory revisions, anti-rollback epoch and operation id / exact rollback_required revision, backup/install identity and operation id / exact apply-or-rollback reconcile revision plus original operation/receipt and installation/backup identity; lookup only | durable apply / rollback / reconcile receipt |
| `query_work_item_activity` / `subscribe_work_item_activity` / `unsubscribe_work_item_activity` | exact WorkItemId/item revision, permission scope, authenticated cursor≤512 bytes and page≤200 events/1 MiB / same plus event/byte bounds / subscription id | request-only `work_item_activity_page` with≤200 events/≤8 KiB each/≤1 MiB logical and complete/partial(next_cursor)/gapped(checkpoint) coverage / subscription / `ack` |
| `get_presentation_history` | exact Workspace, current surface/connection and current remote-session generation when remote, history generation and bounded cursor; daemon derives PresentationHistoryOwner from authenticated principal+surface | only that derived owner's undo/redo stack metadata |
| `undo_presentation_operation` / `redo_presentation_operation` | foreground surface, operation id, exact current surface/connection/remote-session generation, whitelisted history entry, Workspace/history/object generations and expected inverse/pre/post revisions; daemon derives and matches owner | `presentation_history_receipt` |
| `list_status_events` / `get_status_event` / `dismiss_status_event` | exact StatusEventOwner (`Installation`, `Workspace` or `ExecutionTarget`), owner StateStream revision, bounded cursor≤200 / owner+StatusEventId+revision / foreground surface, operation id, exact owner/event/revision and warning-only dismissal | bounded ordered status page with complete/gapped coverage / `status_event` / status receipt |
| `query_diagnostic_log` / `subscribe_diagnostic_log` / `unsubscribe_diagnostic_log` | LocalDesktopForegroundAuthority, exact daemon/log generation, earliest/last sequence, closed source/severity/text-key filter, page≤256/1 MiB and cursor≤512 bytes / same generation/filter and declared event/byte bounds / exact LiveSubscriptionId+revision | request-only `DiagnosticLogPage` with complete-or-gapped coverage / bounded live subscription / `ack` |
| `clear_diagnostic_log` | LocalDesktopForegroundAuthority, operation id, exact daemon/log generation+revision, closed `all|source(DiagnosticSourceKey)` scope and through-sequence; reserves clear receipt before mutation | new log revision+`diagnostic_log_clear_receipt` |
| `get_settings_registry` / `search_settings` / `resolve_settings_route` | exact registry revision and begin-or-authenticated continuation cursor, page≤500/1 MiB / same plus normalised query≤256 bytes, result≤200/400 KiB / exact SettingsSectionId+optional setting key and registry revision | request-only `SettingsRegistryPage` / `SettingsSearchPage` / canonical section+row route or typed unavailable/not-found |
| `preview_reset_settings_section` / `cancel_settings_reset_preview` | LocalDesktopForegroundAuthority, exact Surface/connection, registry revision, SettingsSectionId, tagged persistent SettingsOwnerKey+record/resolved revisions or local TemporarySettings owner+revision / exact preview id+revision | one bounded `settings_reset_preview` / `ack` |
| `apply_reset_settings_section` | LocalDesktopForegroundAuthority, operation id, exact preview id/revision/digest, registry/owner/record/resolved revisions and ordered key/schema digest set; no caller-supplied patch | authoritative resolved settings+`settings_mutation_receipt` or revision conflict |
| `get_corrupt_store_recovery` | LocalDesktopForegroundAuthority and exact StoreOwnerKey+quarantine revision | safe failure/descriptor-size/hash/omission/recovery metadata; never raw bytes |
| `recover_corrupt_store` / `start_fresh_store` / `export_corrupt_store_quarantine` / `discard_corrupt_store_quarantine` | LocalDesktopForegroundAuthority, operation id, exact StoreOwnerKey+quarantine id/revision/hash and respectively validated replacement schema/hash / reviewed omission inventory+new-document revision / exact create-new confined descriptor / separate destructive privacy review with proved no open recovery reference; each preassigns CorruptStoreRecoveryIntentId and reserves terminal/recovery capacity | `corrupt_store_recovery_intent`+receipt; ambiguity is lookup-only and never repeats replace/export/delete |
| `prepare_bug_report` | LocalDesktopForegroundAuthority, exact Surface, preassigned BugReportDraftId, diagnostic daemon/log revision+sequence selection, closed safe system-field allowlist and client draft/family reservation | request-only `BugReportDraftSeed`; after atomic transfer the client owns the sole local editable draft and the daemon retains zero body bytes |
| `review_bug_report` | LocalDesktopForegroundAuthority, operation id, exact local draft id/revision, canonical body≤1 MiB+digest, inclusion/redaction manifest+digest, source daemon/log revision and closed `copy_only|copy_and_open_support_page(ProductSupportDestinationId)|create_new_file(exact descriptor)` disposition; daemon revalidates/redacts, reserves review plus named Browser/File receipt before effect | `bug_report_review_receipt` plus exact Browser/File subreceipt where applicable |
| `mark_node_result_read` | foreground `surface_id`, exact node/instance, result revision | `ack` |
| `list_attention_queue` / `get_attention` / `get_pending_interaction` | authorised exact scope, queue/subject StateWatermark and cursor≤200 / AttentionId+queue+route revision / exact Workspace+PendingInteractionId+interaction revision | bounded ordered queue page / attention entry+route / safe typed interaction metadata |
| `acknowledge_attention` / `snooze_attention` / `dismiss_attention` | authorised surface, operation id, immutable AttentionId, expected queue+subject+route revisions and ack / bounded deadline / dismiss disposition | `attention_mutation_receipt` |
| `create_context_link` | foreground surface, `operation_id`, tagged AgentInstance/Note source, destination instance, purpose, revision policy, scopes, cumulative limits, required expiry | `context_link` |
| `update_context_link` | foreground `surface_id`, link id, expected generation, purpose/scopes/limits/expiry patch, `operation_id` | `context_link` |
| `revoke_context_link` | foreground `surface_id`, `operation_id`, `context_link_id`, expected generation | `ack` |
| `prepare_context_packet` | foreground surface, generations, source, existing/new target spec, intent, separate next instruction?, context-aware budget capped at 1-MiB canonical UTF-8 body+1-MiB inert review, expiry≤600 s, optional reviewed grant and active/metadata/family/shared-memory reservations | `context_packet` |
| `deliver_context_packet` | `operation_id`, opaque reviewed packet capability | `context_packet_delivery` |
| `get_context_packet_delivery` | `operation_id` or packet id | `context_packet_delivery` |
| `prepare_portable_export` / `commit_portable_export` / `cancel_portable_export` / `reconcile_portable_export` | local foreground surface, operation id, exact owning source Workspace/revision, exportable object revisions including optional ContextPacket body/framing hashes, selection/redaction/omission manifest and reserved package≤64 MiB / PortableExportId+review revision, create-new regular-file descriptor identity and reviewed package hash / id+pre-effect revision / id+committing-or-reconcile revision and original receipt | `portable_export` / terminal-or-reconcile export receipt |
| `prepare_portable_import` / `commit_portable_import` / `cancel_portable_import` / `reconcile_portable_import` | local foreground surface, operation id, regular-file descriptor identity, exact package hash/schema/size≤64 MiB and validation bounds / PortableImportId+fresh review revision and PortableImportDestination=`new_workspace(preassigned WorkspaceId,Installation revision)` or `existing_container(WorkspaceId,optional SessionId,optional GroupId,exact graph revisions)` / id+pre-effect revision / id+committing-or-reconcile revision and original receipt | inert import review / reminted ids+receipt / terminal-or-reconcile import receipt |
| `respond_to_agent_interaction` | foreground surface, exact node/instance/attempt/generation/pending id and user-selected response | `ack` |
| `prepare_agent_message` | foreground surface, generations, source/destination instance, purpose, exact UTF-8 body/recipe≤4 KiB, expiry≤600 s and item/destination/global/terminal/family/shared-memory reservations | `agent_message` |
| `deliver_agent_message` | `operation_id`, opaque reviewed message capability | `agent_message_delivery` |
| `set_dependency_edge` | foreground Surface/connection, `operation_id`, exact WorkspaceId+Workspace revision, source NodeId+Node revision, target NodeId+Node revision, DependencyGraphRevision, typed result contract+schema digest and tagged expected edge state `absent|current(DependencyEdgeId,EdgeGeneration,EdgeRevision)`; every field enters the canonical fingerprint | `dependency_edge` plus exact graph/edge-generation mutation receipt |
| `remove_dependency_edge` | foreground Surface/connection, `operation_id`, exact WorkspaceId+Workspace revision, DependencyEdgeId+EdgeGeneration+EdgeRevision, source/target NodeId+Node revisions and DependencyGraphRevision | terminal dependency-edge removal receipt plus next graph revision; never generic `ack` |
| `create_team` | foreground surface, `operation_id`, Session, members/roles, policy | `team` |
| `update_team` | foreground surface, team id, expected generation, member/role/policy patch | `team` |
| `delete_team` | foreground surface, team id, expected generation | `ack` |
| `create_flow_definition` | foreground surface, operation id, portable schema, expected catalogue/policy revisions | `flow_definition` |
| `get_flow_definition` | exact definition id and immutable revision | `flow_definition` |
| `version_flow_definition` | foreground surface, definition id/revision, operation id, immutable replacement | `flow_definition` |
| `archive_flow_definition` | foreground surface, definition id/revision, operation id | `ack` |
| `preflight_flow_run` | foreground Surface/connection, `operation_id`, exact FlowDefinitionId+immutable DefinitionRevision, canonical typed inputs+digest, target/trust/profile/adapter/capability revisions, isolation/checkout receipt+revision, DependencyGraphRevision+dependency-policy revision and requested run bounds; reserves one exact preflight id/body/result/run-capacity vector before evaluation | `flow_preflight` with immutable revision/digest, closed accepted/refused result and the exact reserved capacity vector consumed only by `start_flow_run` |
| `start_flow_run` | foreground surface, operation id, accepted preflight revision | `flow_run` |
| `start_flow_step` | foreground surface, exact FlowRun/step, immutable manual StepStartPolicy revision, expected run/step revision and operation id | `step_attempt` |
| `get_flow_run` | run id/revision | `flow_run` |
| `pause_flow_run` / `resume_flow_run` | foreground surface, run id, expected revision, operation id | `flow_run` |
| `cancel_flow_run` / `abort_flow_run` | foreground surface, run id, expected revision, operation id, declared dispositions | `flow_run` |
| `retry_flow_step` / `reconcile_flow_run` | foreground surface, run/step/attempt, expected revision, operation id | `flow_run` |
| `issue_delegation_grant` / `revoke_delegation_grant` | foreground surface, run/current agent attempt, exact scopes/budgets/expiry, operation id/generation | `delegation_grant` |
| `get_delegation_grant` | exact grant id and generation; body visible only to its authorised operator scope | `delegation_grant` |
| `submit_delegated_operation` | grant capability, exact agent attempt/generation, operation id, closed typed operation | `operation_receipt` |
| `get_runtime_continuity` | exact node/instance | `runtime_continuity` |
| `revalidate_runtime_endpoint_continuity` | authenticated endpoint broker, operation id, exact RuntimeEndpoint/target/root fingerprint, prior+candidate endpoint generations, current endpoint-binding inventory revision and one `RuntimeEndpointContinuityProofV1` covering the exact complete sorted≤64 `BindingContinuityClaim`s, including explicit unavailable claims; reserve request-buffer/receipt/replay capacity before candidate observation | durable continuity receipt plus one atomic endpoint-generation and per-binding result vector; an invalid root proof leaves every binding stale, while one invalid/unavailable claim cannot block valid siblings; failure never mints/rebinds an identity |
| `get_runtime_endpoint_continuity_operation` / `reconcile_runtime_endpoint_continuity_operation` | exact RuntimeEndpointContinuityReceiptId or original operation id / same exact receipt/proof digest and current endpoint/key-epoch metadata; verification/lookup only | original receipt/current endpoint state; reconcile never rotates a key, creates a scope or changes a binding beyond returning the already committed atomic result |
| `rotate_runtime_endpoint_continuity_key` | LocalDesktopForegroundAuthority, operation id, preassigned RuntimeEndpointContinuityReceiptId, exact target/RuntimeEndpoint/root fingerprint/current key epoch and new broker-generated non-exportable key reference+preassigned epoch; no caller key bytes | new key epoch receipt; old-epoch proofs are immediately invalid and affected bindings require fresh proof |
| `rebind_runtime_endpoint_conversation_profile` | LocalDesktopForegroundAuthority, operation id, preassigned ConversationProfileRebindReceiptId+RuntimeEndpointBindingId, exact endpoint/root/thread/unscoped scope+binding+owner+target+continuity-proof generations, destination AccountProfile/read+control grant revisions, ConversationOwnershipRegistry revision and daemon-derived new profiled ConversationKey; no body/input/context payload | atomic old-binding retired + new-binding current + immutable old→new scope lineage receipt after global uniqueness CAS, or zero-effect refusal |
| `get_conversation_profile_rebind` / `reconcile_conversation_profile_rebind` | exact receipt id or original operation id / same exact receipt/fingerprint and current old/new binding+ownership-registry revisions; lookup only | original committed/refused receipt; never copies data, launches, sends input or repeats binding mutation |
| `query_conversation_inventory` | exact `ProviderAccountScope=profiled` provider/Profile/Target/namespace+generations and read grant, declared predicates/normalisation and closed `begin(page≤500/1 MiB,scan≤10,000)` or `continue(authenticated cursor≤512 bytes,page ordinal,predecessor digest,source revision)` | request-only `conversation_inventory_page` with≤500 private safe descriptors/≤2 KiB each/≤1 MiB logical and complete/partial(next_cursor)/gapped(minimum_revision)/unavailable coverage |
| `set_private_transcript_search_policy` | LocalDesktopForegroundAuthority, operation id, preassigned PrivateTranscriptSearchOperationId, exact `ProviderAccountScope=profiled` provider/Profile/Target/namespace+adapter/transcript-root/policy generations and closed `enabled|disabled(current_index_generation,current_key_generation,current_descriptor_revision)`; caller supplies no filesystem root, glob, parser, key or retention override and reserves operation receipt/fence plus worker or unlink capacity before effect | durable private-index operation receipt; enable schedules one bounded rebuild, disable cryptographically retires that exact index/key generation before unlink and never deletes provider transcripts |
| `get_private_transcript_search_state` | LocalDesktopForegroundAuthority, exact authorised profiled provider/Profile/Target/namespace and known index revision? | body-free index state with generation, document/time bounds, coverage, freshness, last complete refresh and gap/unavailable reason |
| `query_private_transcript_search` | LocalDesktopForegroundAuthority and exact owning Surface/connection, exact profiled provider/Profile/Target/namespace+index/transcript-source generations, UTF-8 query 2..256 scalars and closed `begin(limit≤20,scan≤10,000)` or `continue(authenticated cursor≤512 bytes,ordinal,predecessor digest,index revision)` | request-only `private_transcript_search_page` with≤20 exact-key hits,≤4 KiB/hit,≤80 KiB logical, complete/partial(next_cursor)/gapped/unavailable coverage and one authenticated≤512-byte selection seal per hit binding query/page digest+ordinal+all identity/revision fields |
| `select_private_transcript_search_hit` | LocalDesktopForegroundAuthority and exact owning Surface/connection, operation id, current Surface/ViewTarget revision, exact profiled provider/Profile/Target/namespace+index/source generations, ConversationKey+transcript revision, page digest+hit ordinal and daemon selection seal; daemon reauthorises and rereads only identity/currentness | atomically revised `surface_state` whose closed read-only `historical_conversation` ViewTarget matches the sealed hit plus its first bounded view page, or typed stale/forged/cross-scope refusal with zero Surface/ownership/runtime effect |
| `get_historical_conversation_view` | LocalDesktopForegroundAuthority, exact owning Surface/connection+active historical-conversation ViewTarget/revision and closed `continue(authenticated cursor≤512 bytes,index/source revision,page ordinal,predecessor digest)`; begin occurs only inside successful hit selection | request-only read-only page≤100 normalised user/assistant segments and≤64 KiB from that hit's encrypted indexed≤200-KiB tail, with complete/partial(next_cursor)/gapped/unavailable coverage; no provider file read, Node or authority |
| `rebuild_private_transcript_search_index` / `delete_private_transcript_search_index` | LocalDesktopForegroundAuthority, operation id, preassigned PrivateTranscriptSearchOperationId, exact profiled provider/Profile/Target/namespace+policy/index/source generations / same plus consequence review and current index-key generation | operation receipt; rebuild reads only adapter-declared transcript sources, while delete revokes the per-index encryption key before unlink and leaves provider data untouched |
| `get_private_transcript_search_operation` / `reconcile_private_transcript_search_operation` | LocalDesktopForegroundAuthority, exact PrivateTranscriptSearchOperationId or original operation id / LocalDesktopForegroundAuthority, new operation id, exact nonterminal/uncertain receipt, policy/index/key/source generations, sealed index descriptor identity and worker/unlink correlation; lookup/descriptor/quiescence inspection only | original operation receipt / evidence-refined receipt; never rereads transcripts, rebuilds, rotates/revokes a key again or repeats unlink |
| `adopt_conversation` | foreground surface, operation id, preassigned ConversationAdoptionReceiptId+NodeId+AgentInstanceId+RuntimeEndpointBindingId, exact profiled ConversationKey and descriptor digest, complete inventory/source revision, destination Session/tree/Workspace revisions, AccountProfile/read grant/target/adapter/capability/endpoint generations, global ConversationOwnershipRegistry revision and no-current-owner proof; reserves Node/instance/binding/receipt/replay/recovery capacity before CAS | one stopped Agent Node+AgentInstance, exact semantic ownership plus a `proposed` endpoint binding and terminal adoption receipt in one local transaction, or zero-effect typed refusal; adoption never makes a runtime binding current and emits no launch, input or provider request |
| `get_conversation_adoption` / `reconcile_conversation_adoption` | exact ConversationAdoptionReceiptId or original operation id / same exact receipt, fingerprint and current registry/Session/tree revisions; lookup only | original committed/refused adoption receipt; never queries the provider, creates another identity, binds, launches or sends input |
| `read_conversation_title` | exact current ConversationKey and provider revision; requires `title_read` | `conversation_title_observation` |
| `rename_conversation` | LocalDesktopForegroundAuthority, operation id, preassigned ConversationRenameIntentId, exact ConversationKey/account-scope/target/endpoint/adapter/capability generations, expected provider-title revision, tagged owned-or-unowned ownership proof (unowned requires profiled inventory), requested single-line title≤512 UTF-8 bytes/200 scalars+hash and lookup-capable provider correlation; requires `conversation_rename` | `conversation_rename_intent`+receipt |
| `get_conversation_rename` / `cancel_conversation_rename` / `reconcile_conversation_rename` | intent id+revision or original operation id / LocalDesktopForegroundAuthority, new operation id and exact prepared revision / same authority, new operation id, exact dispatching/submitted/reconcile revision, original operation, subject proof and provider correlation; lookup only | intent / terminal intent / intent+receipt |
| `list_native_jobs` | closed `begin(exact provider/Profile/Target/namespace/adapter generations,predicates,page≤500/1 MiB,scan≤10,000,known coverage watermark)` with no scan id, or `continue(daemon-minted NativeJobScanId+revision,next ordinal,fixed provider snapshot watermark,predecessor digest,authenticated cursor≤512 bytes)` | request-only chained `native_job_page` with≤500 safe jobs/≤2 KiB each/≤1 MiB logical and complete/partial(next_cursor)/gapped(minimum watermark) coverage |
| `get_native_job` | exact NativeJobKey, known job/presence revision and profile/target generations | `native_job` |
| `adopt_native_job` | foreground surface, operation id, exact complete inventory/get observation and NativeJobKey, destination Session/optional Group plus expected key-registry/tree/session/group/profile/target revisions; local global-ownership CAS only | existing authorised owner reference or one new `native_job` Node receipt; zero provider effect |
| `create_native_job` | foreground surface, operation id, destination Session/optional Group and expected tree/session/group revisions, exact provider/Profile/Target/namespace generations, proved creation-correlation mode, NativeJobDefinitionSpec, schedule/time-zone/model/safe flags; daemon atomically reserves Job Node+CreationId+intent before dispatch | `native_job_creation_receipt` with reserved NodeId, CreationId and intent revision |
| `cancel_native_job_creation` | foreground surface, operation id, exact reserved NodeId+NativeJobCreationId and expected prepared intent revision; CAS refuses after dispatch begins and emits zero provider request | `native_job_creation_receipt` |
| `update_native_job` | foreground surface, operation id, exact NativeJobKey/job/effective-config revision and profile/target generations, closed definition/schedule/model/flag patch and lookup-capable correlation; atomically reserves MutationIntentId/replay capacity | `native_job_mutation_receipt` naming intent/requested/effective state |
| `pause_native_job` / `resume_native_job` | foreground surface, operation id, exact NativeJobKey/job+schedule revision, profile/target generations and lookup-capable correlation; atomically reserves MutationIntentId | `native_job_mutation_receipt` naming intent |
| `run_native_job_now` | foreground surface, operation id, Turn-minted NativeJobInvocationId, exact NativeJobKey/job+schedule revision, profile/target generations and lookup-capable invocation correlation; atomically reserves independent MutationIntentId | `native_job_mutation_receipt` naming intent/invocation |
| `cancel_native_job_iteration` | foreground surface, operation id, exact NativeJobKey/job revision, NativeJobIterationKey/iteration revision in queued/running, profile/target generations and lookup-capable correlation; atomically reserves MutationIntentId | `native_job_mutation_receipt` naming intent |
| `delete_native_job` | local foreground consequence review, operation id, exact NativeJobKey/job+presence revision, profile/target generations, closed tombstone disposition and lookup-capable correlation; atomically reserves exclusive MutationIntentId | `native_job_mutation_receipt` naming intent |
| `cancel_native_job_mutation` | foreground surface, operation id, exact NodeId+NativeJobMutationIntentId and expected prepared intent revision; local CAS refuses once dispatching and emits zero provider request | `native_job_mutation_receipt` |
| `reconcile_native_job_mutation` | foreground surface, new operation id, original operation id, tagged CreationId/InvocationId/NativeJobKey/NativeJobIterationKey subject, exact MutationIntentId when applicable, expected intent+presence revisions, profile/target generations and exact lookup correlation receipt/mode; lookup only, never redispatch | `native_job_creation_receipt` / `native_job_mutation_receipt` |
| `hide_native_job_activity` / `forget_native_job_projection` / `restore_native_job_projection` | foreground surface, operation id, exact NodeId plus tagged job-key or creation-id projection subject and expected projection/Attention revisions; local CAS only | `native_job_projection_receipt` |
| `delete_native_job_local_data` | local foreground privacy review, operation id, exact NodeId plus tagged job-key or creation-id subject, terminal intent/receipt/Attention revisions and expected replay-fence capacity; zero provider effect | local privacy receipt plus retained minimal replay/visibility fence |
| `get_runtime_inventory` | foreground surface, exact ExecutionTarget/fingerprint/generation, known watermark? | `runtime_inventory` |
| `get_resource_inventory` / `subscribe_resource_inventory` / `unsubscribe_resource_inventory` | exact ResourceScopeKey and coverage watermark / same plus byte,row,cadence bounds / subscription id | `resource_inventory` / `resource_inventory_subscription` / `ack` |
| `terminate_resource_owner` | local foreground consequence review, exact target/trust/handle generations, process start identity and expected resource observation; delegates to the same exact RuntimeInventory termination authority | `runtime_inventory_termination_receipt` |
| `get_target_recovery_view` / `subscribe_target_recovery_view` / `unsubscribe_target_recovery_view` | local administrative surface, exact ExecutionTarget and target-stream revision / bounded subscription / subscription id | `target_recovery_view` / `target_recovery_subscription` / `ack` |
| `get_semantic_recovery_inventory` / `get_semantic_recovery_page` | LocalDesktopForegroundAuthority, exact `workspace(WorkspaceId)|installation` inventory key, inventory revision, redacted filter `all|status_kind|close_receipt(ContainerCloseReceiptId,semantic_survivor_root)` and requested row/byte bounds≤500/1 MiB / same authority, exact inventory key+revision, same closed filter, authenticated cursor≤512 bytes binding filter+close root, page ordinal and predecessor digest | `semantic_recovery_inventory` summary plus first `semantic_recovery_page` / next page; revision change returns `gapped(current_revision)`, never a mixed snapshot |
| `get_container_close_survivor_inventory` / `get_container_close_survivor_page` | LocalDesktopForegroundAuthority, exact ContainerCloseReceiptId+receipt revision+container tombstone generation+close serialisation point+all three typed survivor counts/roots, closed `all|semantic|target_runtime|process_cleanup` filter and requested row/byte bounds≤500/1 MiB / same authority, same immutable receipt/root vector and filter, authenticated cursor≤512 bytes binding typed root+page ordinal+predecessor digest | `container_close_survivor_inventory` summary plus first `container_close_survivor_page` / next page; every typed leaf is verified and revision/root loss returns `gapped`, never an incomplete success |
| `adopt_runtime_inventory_item` | foreground surface, operation id, exact target/handle/inventory revision, destination Session and Node kind | `node_view` |
| `ignore_runtime_inventory_item` / `terminate_runtime_inventory_item` | foreground surface, operation id, exact target/handle/inventory revision and expiry? / disposition | `operation_receipt` |
| `attach_runtime_attempt` | foreground surface, `operation_id`, preassigned RuntimeAttachmentReceiptId, exact tagged AttemptOwner plus an already-existing live/orphaned RuntimeAttempt, target/backend/durable-handle/process-start identity, `proposed|stale|current` binding and all generations, current global ownership-registry revision and verified endpoint-continuity receipt/correlation; never accepts `no_prior_attempt` and creates no Pane | durable `runtime_attachment_receipt` with `attached|recovered|refused|uncertain` plus current `node_view`; it may atomically promote the proved binding and mark the same attempt reconnected but never launches, stops or creates a RuntimeAttempt |
| `interrupt_runtime_owner` | authorised control surface, operation id, exact AttemptOwner/RuntimeAttempt/binding, target/backend/durable-handle/process-start identities and owner/attempt/binding/target/backend/handle/lifecycle/input-safety generations; one interrupt only, never broad stop | `runtime_interrupt_receipt` |
| `get_runtime_interrupt_operation` / `reconcile_runtime_interrupt_operation` | exact RuntimeInterruptReceiptId or original operation id / authorised control surface, new operation id, exact dispatching/uncertain/reconcile-required receipt, owner/attempt/binding/target/backend/handle/process-start/input-safety generations and signal correlation; lookup/probe only | current receipt / evidence-refined receipt; reconciliation never emits another interrupt |
| `acquire_input_lease` / `renew_input_lease` / `handoff_input_lease` / `release_input_lease` | exact AttemptOwner/attempt/binding, client/surface/connection and expected lease generation | `input_lease_receipt` |
| `request_input_lease_handoff` | authorised surface, operation id, exact AttemptOwner/attempt/binding, current lease+generation, requesting client/surface/connection and expiry; creates a visible proposal and never grants a lease or accepts bytes | `input_lease_handoff_proposal` |
| `write_runtime_input` | tagged `ordinary_bytes(source=typed|clipboard_paste|path_drop)` or `verified_local_permission_fallback`; exact AttemptOwner/attempt/binding, lease id/generation, client/surface/connection, expected InputSafetyState revision+route, monotonic input sequence and bounded bytes; clipboard paste additionally binds a current local user-gesture generation and≤64-KiB UTF-8 payload digest, while path drop binds≤128 canonical local paths each≤4 KiB inside the same≤64-KiB manifest and refuses every remote target; fallback additionally carries operation id, full PermissionAuthorityVector, PendingInteraction id/revision, permission-fact revision, verified encoder/transport generation, local-desktop foreground generation and exact option id and atomically acquires the shared interaction claim before effect-armed and derived-byte enqueue | `runtime_input_receipt`; fallback also names its `permission_response_receipt` and ClaimId |
| `resize_runtime_input` | same exact owner/attempt/binding/lease/client fences, monotonic input sequence and bounded rows/columns/pixels | `runtime_input_receipt` |
| `create_account_profile` / `adopt_account_profile` | foreground surface, operation id, provider+ExecutionTarget and isolated non-secret config/auth reference | `account_profile` |
| `list_account_profiles` / `get_account_profile` | exact provider/ExecutionTarget scope / profile id | `account_profile_list` / `account_profile` |
| `begin_account_authentication` | LocalDesktopForegroundAuthority, operation id, preassigned AccountAuthenticationIntentId, exact profile/provider/target/trust/profile/config-reference/broker-policy generations, provider correlation, expiry and root count/byte/terminal/recovery reservation | `account_authentication_intent`+profile receipt |
| `get_account_authentication` / `cancel_account_authentication` / `reconcile_account_authentication` | intent id+revision or original operation id / LocalDesktopForegroundAuthority, new operation id and exact prepared revision / same authority, new operation id, exact dispatching/awaiting/reconcile revision, original operation+provider correlation; lookup only | intent / terminal intent / intent+profile receipt |
| `validate_account_profile` | LocalDesktopForegroundAuthority, operation id, exact profile/provider/target/trust revision, effective credential generation, correlated authentication receipt and bounded validation policy | `account_profile`+validation receipt |
| `rename_account_profile` | foreground surface, profile id/revision and bounded display name | `account_profile` |
| `set_default_account_profile` | foreground surface, exact Workspace-or-target/provider scope, profile id and expected revision | `account_default` |
| `retire_account_profile` / `delete_account_profile` | foreground surface, profile id/revision and operation id / same plus closed `external_data=retain_external_provider_data` and `private_root=retain|delete_if_turn_created_and_fingerprint_current` disposition | `account_profile_result` |
| `get_account_activity` / `subscribe_account_activity` / `unsubscribe_account_activity` | exact provider/Profile/Target, filters, cursor/item/byte bounds / same plus subscription / subscription id | `account_activity` / `account_activity_subscription` / `ack` |
| `list_model_endpoint_profiles` / `get_model_endpoint_profile` | exact ExecutionTarget scope / profile id+revision | `model_endpoint_profile_list` / `model_endpoint_profile` |
| `create_model_endpoint_profile` / `update_model_endpoint_profile` | local foreground surface, exact target/trust, canonical HTTPS origin, protocol/pin policy, eligibility and credential reference+generation / profile+revision patch | `model_endpoint_profile` |
| `validate_model_endpoint_profile` / `discover_model_endpoint_models` | local foreground surface, exact profile/target/trust revision and bounded network/catalogue policy | `model_endpoint_profile` / `model_catalogue` |
| `set_default_model_endpoint_profile` | local foreground surface, exact Workspace-or-target/provider scope, profile revision and expected default revision | `model_endpoint_default` |
| `retire_model_endpoint_profile` / `delete_model_endpoint_profile` | local foreground surface, exact profile revision, operation id and survivor/default/secret-reference disposition | `model_endpoint_profile_result` |
| `list_notification_endpoints` / `get_notification_endpoint` | local administrative scope / endpoint id+generation | `notification_endpoint_list` / `notification_endpoint` |
| `pair_notification_endpoint` | LocalDesktopForegroundAuthority, operation id, preassigned NotificationPairingIntentId+NotificationEndpointId+initial DeliveryGrantId, exact endpoint-catalogue revision, endpoint public key/token reference, device/profile, peer correlation, scopes/classes/privacy/rate/batch bounds and expiry; reserves all bounded control/delivery capacity before peer dispatch | `notification_pairing_intent`+endpoint/grant receipt |
| `get_notification_pairing` / `cancel_notification_pairing` / `reconcile_notification_pairing` | intent id+revision or original operation id / LocalDesktopForegroundAuthority, new operation id and exact prepared revision / LocalDesktopForegroundAuthority, new operation id, exact dispatching/awaiting/reconcile revision, original operation+peer correlation; lookup only | pairing intent / terminal intent / pairing intent+receipt |
| `issue_delivery_grant` / `revoke_delivery_grant` | LocalDesktopForegroundAuthority, operation id, preassigned DeliveryGrantId, exact endpoint/catalogue/generation, scope fingerprint, key/token reference, classes/privacy/rate/batch/expiry / same authority, operation id, exact endpoint/grant/generation and expected revision | immutable grant+receipt / terminal grant receipt |
| `retire_notification_endpoint` / `delete_notification_endpoint` | LocalDesktopForegroundAuthority, operation id, exact endpoint/catalogue/generation and grant/outbox/live cascade revision / same authority, operation id, exact retired endpoint revision and proved terminal pairing/grant/delivery survivors | retired endpoint+receipt / deleted tombstone receipt |
| `get_notification_outbox` / `flush_notification_outbox` | local administrative exact endpoint/grant generation and bounded cursor / same plus current queue+presence revision | `notification_outbox` / `notification_flush_receipt` |
| `subscribe_live_notification_status` / `unsubscribe_live_notification_status` | exact authorised endpoint/scope and bounded subscription / subscription id | `live_notification_subscription` / `ack` |
| `create_remote_invitation` / `list_remote_invitations` / `get_remote_invitation` / `revoke_remote_invitation` | LocalDesktopForegroundAuthority plus exact Workspace/Session, closed client role/scope, origin/device policy, bounded expiry and operation id / local administrative scope / invitation id+revision / id+revision+operation id | invitation with preassigned RemoteClientId+RemoteSessionId / list / item / receipt |
| `redeem_remote_invitation` | stable RemoteRedemptionId, single-use invitation id+revision, preassigned client/session ids, exact authenticated origin+device public key, protocol role and manifest hash; no ambient credential | durable `remote_redemption_receipt` plus exact client/session |
| `open_remote_session` | stable RemoteSessionOpenId, authenticated disconnected client id+revision/device+origin, same-or-narrower role/scope, current manifest hash and operation id; refuses while another child session is negotiating/active | durable `remote_session_open_receipt` plus reserved session id |
| `list_remote_clients` / `get_remote_client` / `revoke_remote_client` | LocalDesktopForegroundAuthority and bounded scope / exact client id+revision / client id+revision+operation id | list / `remote_client` / receipt |
| `list_remote_sessions` / `get_remote_session` / `revoke_remote_session` | LocalDesktopForegroundAuthority and bounded scope / exact session id+revision / session id+revision+operation id | list / `remote_session` / receipt |
| `update_remote_presence` | active client/session/surface/connection generations, exact Workspace scope, expected presence revision, state, optional authorised ViewTarget and expiry≤30s | ephemeral `remote_presence` or tombstone receipt |
| `send_presence_chat` / `retract_presence_chat` | authenticated encrypted `full_gui` client, active client/session/surface/connection generations, exact Workspace scope, anti-replay nonce, expected current sender-chat revision, preassigned MessageGeneration, sanitised UTF-8 body≤512 bytes/256 scalars and expiry≤30s / same exact owner+current message generation+revision+nonce | ephemeral `presence_chat_message` / live tombstone `ack` |
| `list_remote_permission_response_grants` / `get_remote_permission_response_grant` | LocalDesktopForegroundAuthority, exact client/interaction scope and bounded cursor / exact grant id+revision | list / `remote_permission_response_grant` |
| `issue_remote_permission_response_grant` | LocalDesktopForegroundAuthority, operation id, full PermissionAuthorityVector, exact current sensitive class=permission, remote client role/id+revision, RemoteSessionId+revision+expiry, surface/connection generation, provider/profile/Workspace/Session/Node/instance/attempt/input route+binding generations, PendingInteraction/options/fact/transport revisions and expiry; atomically CASes GrantIssueKey and claim absence | `remote_permission_response_grant` plus delivery receipt |
| `ack_remote_permission_response_grant` | exact grantee client+revision, RemoteSessionId+revision, surface/connection generation, grant id/revision and encrypted delivery nonce | `remote_permission_response_grant` |
| `revoke_remote_permission_response_grant` | LocalDesktopForegroundAuthority, operation id and exact grant id/revision | `remote_permission_response_grant` |
| `submit_local_permission_response` | LocalDesktopForegroundAuthority, operation id, full PermissionAuthorityVector, exact Node/instance/AttemptOwner/attempt/InputRoute/binding generations, PendingInteraction/option, InputSafetyState revision, permission-fact revision and typed transport generation; atomically acquires the interaction claim | `permission_response_receipt` with ClaimId |
| `submit_remote_permission_response` | operation id, full PermissionAuthorityVector, exact grant id/revision, grantee client+revision, RemoteSessionId+revision, surface/connection generation, anti-replay nonce, exact PendingInteraction/option/InputSafetyState/fact/typed-transport revisions and repeated owner/route/binding fences; atomically acquires the same interaction claim | `permission_response_receipt` with ClaimId |
| `get_permission_response_receipt` / `reconcile_permission_response` | exact original operation id/receipt id/ClaimId and subject / same plus expected receipt/evidence revisions and provider correlation; lookup only, never redispatch | `permission_response_receipt` |

`select_tree_node` continues to persist only surface-scoped navigation. The client derives a `ViewTarget`
and requests its content separately. A Node selection never opens, zooms or focuses a Pane, but it may
replace the WorkSurface content; that distinction supersedes ADR-048. `get_node_view` repeats the requested
key and carries a monotonic node-view revision so late answers can be discarded. Selection never launches,
domain-attaches, cold-resumes, acknowledges or marks a result read. For a selected already-live attempt the
client automatically uses presentation `attach_pane`; domain `attach_runtime_attempt` remains a separately
authorised continuity mutation and is never inferred from selection.

`activate_session` is the separate ADR-049 user intent. One Session-row click may issue selection followed
by activation, but an Agent/child selection, `route_attention`, notification or automatic Focus never issues
it. One accepted operation restores the saved Layout, attaches proved live attempts and materialises every
eligible stopped runtime descriptor in the exact bounded preflighted plan. Before any spawn the daemon
persists one activation operation and its deterministic descriptor→Node/Attempt reservations. Lost-response
retry with the same operation id returns the same per-reservation receipt; a competing operation at the same
Session activation generation cannot mint a second attempt, and reconcile probes only reserved identities.
If the Session has no runtime
descriptor, the same operation may create/start exactly its configured default Shell. The plan fixes Session
revision, descriptor ids and each target/profile/cwd/isolation/command/authority generation; every element is
revalidated before the first spawn. A stale/changed/ambiguous/unsafe element rejects the whole materialisation
before external effect and returns one consolidated typed recovery result. Restore, background clients,
child selection and Attention routing never activate, and activation cannot resume an unverified provider
conversation. No generic follow-up “Start pane” operation exists.

`WorkspaceOnboarding` is one resumable operation family, not four unrelated wizards. Its closed intent is
`create_directory|open_directory|clone_repository|adopt_ssh_target`. Before its first effect it freezes the
operation id, intended Workspace identity, ExecutionTarget, canonical path, repository/remote identity,
authentication reference and target generation. Each directory, fetched object, checkout, trust decision
and cleanup result receives an exact receipt. Cancellation and daemon loss preserve partial/uncertain facts;
resume reconciles by operation id plus repository/remote identity and cannot clone twice. SSH identity/path
remain pinned and no remote failure falls back to a same-named local path. Open/adopt is inert until required
local capability consent is decided. `publish_repository` is a separate local-foreground, consequence-
reviewed operation fixing destination, visibility, branch/upstream and credential reference; onboarding
never publishes implicitly. Every writer receives an isolated checkout and the operator's primary `main`
checkout remains unoccupied and switchable.

```text
WorkspaceOnboardingState = prepared
                | running(phase)
                | cancel_requested(last_proved_phase)
                | reconcile_required(last_proved_phase, possible_effect)
                | completed | cancelled | failed(reason, residuals[])
phase = preflight | path_probe | directory | target_adoption |
        remote_fetch | checkout | workspace_commit | cleanup
```

Each intent freezes one finite ordered phase plan before `prepared → running(preflight)`; execution may move
only to the next phase in that plan. Any possible-but-unproved external effect enters `reconcile_required`
and no resume repeats it. `running → cancel_requested` fences new effects;
`cancel_requested → running(cleanup)|cancelled|reconcile_required`, and cleanup may end only cancelled or
reconcile-required until every created artifact is classified. Reconciliation may return to the proved next
phase, cleanup or one terminal only from an exact phase receipt and observed external identity. Completed,
cancelled and failed are terminal for the WorkspaceOnboardingId; continuing work requires a new id. Every
state revision retains the full bounded phase-receipt vector and residual disposition.

WorkspaceOnboarding is Installation-stream owned from reservation through terminal receipt. At most 100 are
nonterminal installation-wide; begin reserves its active record and terminal-receipt capacity before any
filesystem, target or network effect, and N+1 refuses with no path, target, repository or Workspace change.
The optional preassigned Workspace is committed through one exact Installation+Workspace revision vector.
Terminal rich receipts compact after 180 days inside the 10,000-receipt bound; nonterminal, possible-effect,
residual-cleanup and reconcile-required evidence never ages out.

`ExecutionTarget` is a stable installation-owned object, not a Workspace-shaped hostname. Its closed record
is `{ ExecutionTargetId, kind=local|ssh|custom, display_label, endpoint_descriptor,
authenticated_fingerprint, trust_generation, path_namespace, backend_capability_revisions,
connectivity_freshness, non_secret_credential_reference, state, revision }`. State is closed:

| From | May transition to |
| --- | --- |
| `proposed` | `probing`, `retired`, `deleted` |
| `probing` | `trust_pending`, `connected`, `disconnected`, `mismatch`, `retired` |
| `trust_pending` | `connected`, `mismatch`, `retired` |
| `connected` | `disconnected`, `mismatch`, `retired` |
| `disconnected` | `probing`, `connected`, `mismatch`, `retired` |
| `mismatch` | `trust_pending`, `retired` |
| `retired` | `probing`, `deleted` |
| `deleted` | none; permanent id tombstone |

Create stores inert endpoint text as proposed. Adopt/probe is bounded and cannot mutate the remote; local
foreground trust pins the proved fingerprint, and rotation is a separate consequence-labelled operation.
Reconnect or same-named host discovery never changes identity/trust implicitly. Exactly one installation
owns a target; revisioned Workspace grants expose a closed subset of `observe|runtime|file|repository` and
removing one neither deletes target state nor another grant. Retire prevents new launch while retaining
survivors/evidence. Delete requires zero active runtimes, profiles, jobs, Workspace defaults, backend/
Recovery items and retained audit/authority references; no lifecycle operation broad-kills a host.

Runtime inventory and recovery are owned by the `ExecutionTarget` state stream. Every unmatched handle is
addressable from one installation-level `TargetRecoveryView` even if no Workspace is bound or its former
Workspace was deleted. A Workspace may receive only references permitted by its binding; it never owns or
hides the canonical item. Enumeration, adopt, ignore and terminate are local-administrative operations.
Adopt alone names an explicit destination Session and mints a Node/AttemptOwner; ignore is exact-revision and
bounded; terminate revalidates target/fingerprint/generation/handle/inventory revision. The canonical view
survives Workspace deletion and never fabricates a Session merely to place a row.

`ResourceInventoryObservation` extends that same target snapshot family. Its canonical keys are
`ResourceScopeKey=(ExecutionTargetId,target_generation)` and
`RuntimeResourceRowKey=(ExecutionTargetId,target_generation,backend_handle,handle_generation)`. The host
observation carries physical memory total/available/used, swap total/free, measured pressure, accounting
method, observed time and `complete|partial|gapped|unavailable|unsupported|stale` coverage, plus an explicit
`measured_nonempty|measured_empty|unmeasured` result. An absent optional fact, collector error or remote read
failure never serialises as numeric zero.

Each process row uses reuse-safe `(target_boot_id,pid,process_start_time)`, bounded parent edges, own RSS and
deduplicated descendant RSS. Attribution names exact RuntimeAttempt, Node and Session when proved, with
`owned_current|owned_closed_session|unmatched_survivor|ambiguous`; a surviving process of a closed Session
keeps the closed owner. Cycles, inaccessible processes, shared RuntimeEndpoints and overlapping trees are
partial/shared buckets, never double-counted or guessed. Aggregates repeat numerator, denominator, coverage
and revision. Observation has no effect. `terminate_resource_owner` re-probes and revalidates target/trust,
handle generation, process start identity and expected resource observation before invoking the exact
RuntimeInventory termination route. PID/name-only, host-wide and remote-to-local fallback shapes do not
exist; a late target-generation response affects no sibling.

Ordinary create and branch atomically insert their one-to-one Node + AgentInstance pair in `provisioning`
inside the store, then run external launch as a visible idempotent saga. They cannot accept or smuggle an
initial packet. A new handoff target uses the same pair-creation primitive only inside
`deliver_context_packet`, after the exact packet and optional grant have been reviewed.

`subscribe_node_view` is surface-scoped and valid only for the currently selected exact subject. The answer
contains a subscription id, subject, content kind, initial revision and negotiated bounds. Every
`node_view_changed` push repeats the subscription id, subject and monotonic revision and contains only a
serialized≤180-KiB metadata/delta payload in one complete≤256-KiB frame. Content or a delta that would exceed
that ceiling becomes the pre-reserved `gap(resnapshot_required)`; the client protocol runtime automatically
issues exact `get_node_view`, receives any large logical body through `ChunkedResponseStream` and verifies all
generations+digest before one atomic replacement. This is not an operator interaction, and partial content is
never rendered. Reselection, replacement connection, disconnect or explicit unsubscribe retires it; bounded
backpressure emits the same typed gap. Clients discard late pushes from any retired subscription or different
subject.

`route_attention` is the vNext successor to v4 `goto_attention`. The daemon chooses an omitted-id “next” or
an aggregate-badge `scope` using the one global queue order and returns `AttentionRoute`: `surface_id`,
`surface_connection_generation`, daemon generation, `attention_id`, Workspace/Session and a tagged subject:
exact node/AgentInstance with optional
current attempt/generation/pending id, authenticated provisional parent/external-worker scope, or unassigned
Session demand. Only an exact subject may carry a verified interaction owner and bounded NodeView bootstrap;
the others carry a `ProvisionalAttentionView` and never borrow input. Notification deep links carry only an
opaque attention id plus daemon generation and are revalidated through this operation. The
same route is embedded in governor-approved `focus` effects; deferred focus retains it, while denied focus
never navigates. Thus shortcut, exact/aggregate badge, notification and automatic focus cannot land merely
on a plausible Session or Pane. Routing is navigation only: even when it crosses Sessions, it never invokes
`activate_session` or starts/resumes a runtime.

`update_surface_activity` replaces the surface-less v4 activity shape. The daemon serially tracks exactly
one `focus_target_surface_id`: the focused connected surface, or when the application is backgrounded, the
most recently focused still-connected surface. Automatic Focus binds its route to that stored surface and
connection generation. If it is absent, replaced or disconnected before application, the effect is denied/
degraded to queue/badge and never transferred to another window. User-invoked routing always names the
surface that received the gesture.

Linked-context reads do not exist on the administrative protocol. A foreground operator surface issues,
expands or renews root authority and may create/revoke the durable grant directly. A current agent may create
only an exact pre-authorised link/packet through `submit_delegated_operation`; the daemon derives issuer,
FlowRun, source/destination, bounds and generation from that capability and refuses widening. Other agent
events are proposals. This cannot distinguish a same-uid process that stole the administrative capability
and impersonates a surface. The destination
adapter reads through a distinct local-only `ContextBroker` with a short-
lived capability bound to link generation, destination instance, current attempt, allowed source/scopes,
purpose, cumulative limits and expiry. The broker derives destination from the capability, rotates it for
every attempt, validates a local/remote source-host jail and never exposes the daemon control token.
`update_context_link` is the only root renew/expand operation: it requires a foreground surface, expected
generation and operation id, creates a new generation/capability and never edits a grant in place.

No direct mutation/prepare endpoint accepts a `DelegationGrantExercise`. Delegated variants enter only
through `submit_delegated_operation`, whose closed payload is
`CreateAgent|CreateRuntime|CreateResource|UpdateResource|CreateContextLink|PrepareContextPacket|
DeliverContextPacket|PrepareAgentMessage|DeliverAgentMessage|SetDependency|CreateOrUpdateTeam|
PublishProgress`. The daemon ignores caller-supplied
authority fields and derives exact source/destination/kind/schema/path/budget/revision/generation from the
immutable Flow grant; a mismatch refuses before disclosure or effect. Root update/renew/expand/revoke,
resource delete/reparent, permissions, credentials, destructive lifecycle and repository integration have
no delegated variant.
`DeliverContextPacket` and `DeliverAgentMessage` contain only the prepared object id, its preparation receipt
id and canonical content hash. The referenced preparation must have been created by the same FlowRun, grant,
AgentInstance, RuntimeAttempt and authority generation and must carry a still-current preauthorised
deterministic recipe; an ad-hoc/operator draft can never be delegated. The daemon retains the sealed
single-use delivery capability, revalidates destination readiness and all cumulative budgets, and charges it
exactly once before the external write. The agent receives only a durable accepted/refused/uncertain receipt,
never the bearer. A different body, destination, recipe/policy revision, generation or second delivery is
refused before effect. Thus prepare without deliver is harmless, while a current immutable grant can actually
complete the bounded send promised by the Flow.
Root issue/update/revoke and delegated exercise are operation-id idempotent and retain operator operation,
grant and authority generation, so a lost answer cannot duplicate authority or budget. Each live link counts
against `records.active_context_links_per_agent` at both endpoints and create
is refused at either bound. End/delete/expiry revocations and direct-Archive suspension+bearer revocation are
internal lifecycle transitions and require no extra UI surface; only End/delete is permanent, while Archive
retains the suspended link for an explicit fully revalidated restore that performs no read.

The broker request contains only that capability, a closed content kind, selector/range and requested byte/
token bound; its response repeats grant generation, provenance, redaction/truncation and remaining budgets.
It returns quoted untrusted data, never a control-protocol object that an agent can replay as authority.
At most 10,000 ContextLinks are active installation-wide as well as 64 at either endpoint Agent; each current
destination attempt has exactly one≤4-KiB `ContextBrokerBearer`, with≤10,000 bearers and≤32 MiB memory-only
aggregate. A read is≤1 MiB, has a 30-second wall deadline and admits at most four in-flight buffers per
destination attempt,16 per Link and256 installation-wide, with a≤256-MiB buffer aggregate. Bearer and read
buffers charge the shared variable-RSS pool. Count/item/family/shared/TTL N+1 refuses before source open or
remote helper request and does not consume cumulative link budget. Responses are bounded/non-streaming: the
broker atomically reserves the maximum budget and exact read-buffer slot, buffers data, then
revalidates generation/expiry/endpoints and commits actual budget + audit immediately before exposing bytes.
A revoke committed first yields no body; a read committed first is already disclosed and remains charged.
Pre-commit failure/revoke/expiry/attempt end releases the buffer after descriptor/network quiescence; after
commit, disconnect cannot refund disclosure but the bytes release after the one bounded destination write is
quiescent. Bearer rotation/revocation destroys its memory slot, while a live/uncertain remote helper transfers
only its existing AuxiliaryWorker reservation to ProcessCleanupCharge and retains no response body.
`ContextSource` is `AgentInstanceSource` or `NoteResourceSource`. The latter carries exact NodeId plus
`pinned_revision` or a `follow_reviewed_revisions` policy with allowed authors/grants, schema and cumulative
revision/byte/token limits. Every response repeats the Note revision; edits never reset a limit or redirect
the link, and other Resource kinds are rejected on this data plane.
Parallel reservations count against the cumulative limit. The high-entropy bearer prevents accidental/stale
cross-wiring and other-user access, not theft by a malicious same-uid process absent per-agent OS isolation.
If an offline source host prevents physical removal of a catalogued remote artifact, revoke/delete returns
`remote_residual` with `pending_purge`, persists a bounded non-secret host/generation cleanup tombstone and
keeps authority revoked. An authenticated purge result for that exact host/generation is the only transition
to `removed`; reaching the tombstone bound refuses creation of new remote artifacts rather than forgetting
cleanup evidence.

Resource operations are M14 names, not v4 claims. Groups form one same-Session forest with maximum depth
128. A Group may name one parent Group or the owning Session; every create/reparent/subtree move/remove is
one compare-and-swap over the Session-scoped `GroupTreeRevision` and revalidates same-Session ownership,
uniqueness, depth and acyclicity after concurrent changes. `set_group_membership` changes only one
same-Session presentation membership and never rewrites runtime parentage. Removing a non-empty Group accepts
exactly `refuse|promote_children|move_children_to_session` and never cascades into runtime, context,
Attention or checkout deletion. Deterministic display chooses explicit Group, strongest verified Spawn,
strongest verified Process, then Session; equal strongest parents remain unassigned rather than using an id
tie-break. Traversal has visited-set plus depth/node bounds; existing duplicate/cycle/depth corruption returns
typed `group_tree_corrupt`, exposes only a bounded proved prefix and blocks ordinary mutation. Only a separate
exact foreground repair operation may change it. Note stores bounded Turn-private text;
File/Diff accept canonical checkout-confined references. `WebPreview` accepts only HTTPS without userinfo/query/
fragment, treats the complete URL as private content, validates all DNS answers and pins every connection/
redirect to an approved public IP; it does not fetch during create/restore and renders with script, forms,
navigation, popups, downloads, ambient credentials, daemon access and local files disabled. Deletion removes only the Turn
record. Every operation is revision/idempotency fenced and enters ADR-057 inventory/export/delete coverage
before it ships.

One Group may project one `CheckoutScopeBinding`, but `CheckoutScopeBindingId`, `CheckoutScopeId`, GroupId
and canonical repository/worktree identity remain distinct and Session-owned. `CheckoutScopeBindingState` is
closed: `proposed → current|refused|unbound`, `current → stale|unbound`, `stale → current|unbound`; `refused|unbound`
are terminal for that binding id. `unbind_group_checkout_scope` drops only that projection and leaves an
active CheckoutScope unchanged; `unbind_checkout_scope` is the separate scope lifecycle operation and
retains the worktree. Only fresh `active → missing|conflicted` inventory makes a current binding `stale`;
scope unbind/remove carries
the expected scope, `GroupTreeRevision` and binding revision and atomically terminalises any `proposed|current|stale`
binding as `unbound`, so no Group retains a default to a released or removed scope.
The binding grants no runtime or repository authority;
it supplies default cwd/isolation only for new descendants or an explicit `move_and_rehome`. A presentation
move alone never changes a running cwd. Rehome atomically preflights every affected stopped descriptor and
refuses live writers. `CheckoutScopeState` is closed:

```text
provisioning -> active | reconcile_required
active -> missing | conflicted | unbinding | removing
missing | conflicted -> active | unbinding
unbinding -> unbound | reconcile_required
removing -> removed | reconcile_required
reconcile_required(origin=provisioning, last_proved=none, desired_terminal=none,
                   possible_effect=created_or_adopted, operation_id, receipt)
  -> active | unbound | reconcile_required
reconcile_required(origin=unbinding, last_proved=active|missing|conflicted,
                   desired_terminal=unbound, possible_effect=ownership_release,
                   operation_id, receipt)
  -> unbound | reconcile_required
reconcile_required(origin=removing, last_proved=active, desired_terminal=removed,
                   possible_effect=worktree_delete, operation_id, receipt)
  -> removed | reconcile_required
unbound | removed -> terminal for that CheckoutScopeId
```

Every reconcile transition requires fresh complete identity-bound inventory. Provisioning absence with no
survivor may terminalise as `unbound`; an unbind/remove terminal desire is monotonic and cannot return to its
last-proved operational state or change terminal kind. Create/adopt/bind/unbind/remove/reconcile name exact
Session, target/trust generation, repository/worktree identity, `creator=turn_created|adopted`, scope
revision and operation id. Missing or foreign worktrees become `missing|conflicted`, never a
same-looking local fallback. Unbind and Group deletion preserve the worktree. Remove is separately
consequence-labelled and requires fresh dirty, unpublished, path-owner, repository and live-writer proof;
adopted scopes default to unbind. Agent-per-branch Flow members keep separately owned scopes and a Group only
projects one of them. A single create/adopt request may preassign the CheckoutScope, Session and optional
Group/binding ids and drive their external-effect saga under one composite operation id. Its provisioning
receipt records each worktree/Session/Group boundary; a partial effect remains `reconcile_required` and can
neither duplicate the worktree nor expose an ownerless invisible resource.

Group deletion with a projection must carry `binding=some(CheckoutScopeBindingId,binding_revision,
scope_revision)|none(proved_at_GroupTreeRevision)`. In the same GroupTree CAS it terminalises any proposed,
current or stale binding as `unbound`, preserves the scope/worktree, then applies the requested Group-child
disposition. It cannot silently orphan or delete the scope.

`Browser` is a different explicitly created Node and capability, never a mode selected from WebPreview. It
owns one process-isolated cookie/storage partition and revisioned address/history/load/TLS/error/permission/
popup/download state. Only the typed operations in the target table may navigate or mutate it; page content,
links and script messages are hostile data and cannot become protocol operations, Attention resolution or
authority. Popups are blocked until a foreground exact-origin/consequence review creates a separate Browser
Node without opener authority. Downloads are quarantined non-executable and become an inert File Resource
only after exact size/type/hash/confined-path review; they never auto-open. Device/clipboard/filesystem and
ambient Turn/provider credentials are denied. Local HTML is descriptor-copied from one reviewed confined root
into a synthetic origin; `file://`, live Workspace access and symlink/hardlink/mount escape are invalid. A
loopback URL requires a short-lived capability binding scheme, resolved IP set, port, target fingerprint/
generation and expiry; every navigation/redirect revalidates against DNS rebinding and host/port/fallback.
Restore/history never loads a page automatically, and destroy clears only the partition, not server data.

`WorkItemId` owns exactly one canonical WorkItem Node in one Session. Local `create_work_item` atomically mints
WorkItemId+NodeId under an exact Session/optional Group with initial state `backlog`; every other initial local
state is refused. Other Workspaces may hold an authorised activatable reference, never a duplicate Node.
The closed schema is bounded title/body/link,
`WorkItemState=backlog|ready|active|blocked|review|done|cancelled`,
`Priority=unset|low|normal|high|urgent`, optional UTC due instant, unique normalised tags, append-only comments
`{comment_id, external_comment_key?, author_identity, body, created_at, revision}` and
`Assignee=none|AgentInstance(id)|TeamRole(team_id,role)|ExternalIdentity(key,unmapped_reason?)`.
`update_work_item_metadata` changes only fields whose current authority is `turn`; a state omission does not
move and its current value is a no-op. The only non-self local-operation transitions are:

| From | May transition to |
| --- | --- |
| `backlog` | `ready`, `cancelled` |
| `ready` | `backlog`, `active`, `blocked`, `cancelled` |
| `active` | `blocked`, `review`, `done`, `cancelled` |
| `blocked` | `ready`, `active`, `cancelled` |
| `review` | `active`, `blocked`, `done`, `cancelled` |
| `done` | `ready` only with an explicit bounded reopen reason |
| `cancelled` | `backlog` only with an explicit bounded reopen reason |

The daemon compare-and-swaps complete Node/WorkItem revision; comments never overwrite, and assignee is display
responsibility rather than control authority. Fields never mutate runtime/turn/dependency, satisfy Flow or
emit Attention directly. Projection state is closed `visible → archived|forgotten`, `archived → visible|
forgotten`, `forgotten → visible|deleted`, `visible|archived → deleted`; deleted is terminal. Archive/forget/
restore/delete are local CAS operations with zero provider effect. Forgotten/deleted retain the minimum
WorkItemId/NodeId/WorkItemKey tombstone fence against sync resurrection; delete refuses active mutation/conflict.

`WorkItemBinding` is durable Workspace-owned state keyed by the canonical WorkItem/Node owner; the separate
Installation-owned `WorkItemKeyRegistryEntry` enforces cross-Workspace uniqueness without moving binding
lifecycle into the Installation stream. External identity is only
`WorkItemKey=(source_id,source_profile_id,project_namespace,external_item_id)`. Installation-wide one key maps
to at most one WorkItemId+NodeId. `WorkItemBindingId` has immutable lineage and closed state
`proposed → current|refused`, `current → stale|detached|tombstoned`, `stale → current|detached|tombstoned`,
`detached → proposed|tombstoned`; refused/tombstoned are terminal. Import installs the exact fresh key and
destination atomically; concurrent/cross-Workspace import has one owner CAS winner. Rebind requires detached;
detach/source deletion cannot mutate the provider. Late events consult the binding/tombstone revision.

Before an external create has a provider id, `create_external_work_item` atomically reserves WorkItemId,
NodeId, `WorkItemCreationId` and `WorkItemCreateIntent` at its exact Session/optional Group. Intent state is
`prepared → dispatching|cancelled`, `dispatching → bound|refused|reconcile_required`,
`reconcile_required → bound|not_created|reconcile_required`; bound/refused/cancelled/not_created are terminal.
Only cancel-create CASes prepared→cancelled with zero source request. An adapter advertises create only with
`create_correlation=idempotency_key_lookup|provider_receipt_lookup`; both expose a side-effect-free exact
outcome query and a write-only idempotency key is insufficient. A correlated receipt binds one
WorkItemKey exactly once to the reserved Node. `reconcile_external_work_item_mutation` queries only tagged
CreationId/MutationIntentId plus original operation/correlation and never redispatches or matches display data.
`WorkItemCreateIntent` is durable Workspace-owned state because its preassigned WorkItemId, NodeId and exact
Session/optional Group are all owned by that Workspace.

`WorkItemSourceState` is closed: `draft → validating|revoked|deleted`, `validating → active|degraded|revoked`,
`active → validating|degraded|revoked`, `degraded → validating|active|revoked`, `revoked → validating|deleted`,
and deleted terminal. Mapping/filter/credential changes mint a source generation. `WorkItemSyncRun` is durable
Installation-owned state under the exact global WorkItemSource generation; each applied page separately CASes
the affected Workspace stream revisions, so sibling Workspace runs cannot bypass the one source cursor/fence.
One `WorkItemSyncRunId` uses
`prepared → fetching|cancelled`, `fetching → applying|partial|gapped|failed|cancelled`,
`applying → complete|partial|gapped|failed|cancelled`; terminals do not resume. Applying cancellation blocks
the next page, lets the current page commit-or-not atomically and records last applied cursor plus honest
partial/gapped coverage; it never rolls back an applied page or claims unseen data. Independent observation axes are
`SyncCoverage=complete|partial|gapped|unavailable`, `SyncFreshness=fresh|stale|expired`,
`SyncBackoff=ready|rate_limited(retry_at)|offline|auth_required` and
`WorkItemPresenceState=observed|stale|missing|source_deleted|unknown`. Snapshot/delta/webhook input repeats
run/source/mapping/filter/credential generations, cursor/watermark and item revision. Old generations and
post-detach/tombstone input have zero effect. Only fresh complete exact-scope absence proves missing;
source_deleted requires an exact provider tombstone/event. Filters, permission loss, partial/gapped, stale,
offline and rate limits never prove deletion or exact zero.

The source declares exhaustive native mappings and per-field `external|turn|reviewed_merge` authority.
External observations store native+mapped values, source/mapping revision and provenance. A fresh externally
authoritative state may map directly to any WorkItemState even when no local-operation edge exists; it records
`source_observed`, never fabricates a Turn transition. Reviewed-merge stores both revisions. External edit,
comment, assign, transition, close and reopen have disjoint field schemas and exact binding/source/mapping/item
CAS; creation has no nonexistent item ETag. Stable provider comment/assignee subidentities deduplicate sync
echoes.

Every external write owns a durable Workspace-owned `WorkItemMutationIntent`, identified by
`WorkItemMutationIntentId`, with closed state `prepared → dispatching|cancelled`,
`dispatching → submitted|refused|reconcile_required`, `submitted → resolved|reconcile_required`,
`reconcile_required → resolved|not_applied|reconcile_required`; terminals never retry. A conflict owns
`WorkItemConflictId`, immutable per-field local/external revisions and `active → resolved|superseded|abandoned`.
A newer external revision supersedes an old conflict; concurrent resolvers have one revision CAS winner.
Active intent/conflict/receipt evidence cannot compact. Credentials remain broker-only, and no local projection,
source-config or Attention operation closes/reopens/deletes a provider item.

WorkItemSources are Installation-stream records, not Workspace records. The installation admits at most 64
non-deleted sources across every Workspace reference; N+1 create refuses before credential, provider or
configuration effect. A deleted source frees that slot only after credential/generation revocation and after
all current binding, mutation, conflict, identity and resurrection fences have moved to their bounded owners;
nonterminal evidence never compacts for admission.

Source ids are monotonic installation ids and never reused. `WorkItemKeyRegistry` is Installation-owned and
holds at most1,000,000 current/binding/tombstone entries or480 MiB, each≤512 bytes; exactly983,040 maximum
entries reach the independent byte boundary while the count boundary uses smaller entries. Known-key import reserves
that key; external create reserves an unbound slot before dispatch and binds its exact correlated result once.
N+1 count/byte refusal precedes provider or Node effect. An exact key fence may compact only after its source
is terminally deleted, every operation settles and the monotonic source-id/generation fence can reject all
late events without id reuse.

One of 10,000 installation-wide WorkItemSource operation slots is reserved before each sync/create/mutation
provider request and retained through nonterminal or uncompacted terminal state. N+1 refuses before dispatch.
Terminal richness compacts after 30 days only after operation replay, key/binding, source-generation and
conflict fences are durable; possible-effect/reconcile/nonterminal/active-conflict evidence never ages out.

`ProgressUpdate` is also a closed record, not an alternate StepAttempt state:

```text
ProgressUpdate = {
  progress_id, flow_run_id, step_id, step_attempt_id, grant_id, operation_id,
  producer_instance_id, producer_attempt_id, producer_generation,
  sequence, expected_previous_revision, phase, percent?,
  message_key?, bounded_message_arguments?,
  bounded_artifact_or_result_refs[], observed_at
}
phase = queued | running | blocked | succeeded | failed | cancelled
```

`sequence` is strictly increasing within one `progress_id`; duplicate sequence with identical bytes returns
the first receipt and different bytes conflicts. Legal non-self phase transitions are `queued →
running|blocked|failed|cancelled`, `running → blocked|succeeded|failed|cancelled` and `blocked →
running|failed|cancelled`; terminal phases do not transition. `percent`, when present, is an integer `0..100`
and cannot decrease within one uninterrupted running phase. After `blocked → running`, the next value must
retain the prior floor or carry a closed explicit reset reason. A higher sequence commits only with the exact
expected prior revision, atomically replaces the current projection and retains bounded receipt history.
Result references are exact closed artifact/operation hashes, never raw output. Message key/arguments,
percent, references and terminal phase are untrusted evidence: a ProgressUpdate cannot transition FlowRun,
StepAttempt, Lifecycle, TurnState, DependencyResult or Attention; the corresponding authoritative reducer
must independently consume its own typed result/receipt.

File edit snapshots and saves are FileBackend capabilities, bind exact target/root/path/file identity/hash/
revision, enforce descriptor/root confinement and return conflict without overwrite when any external fact
changed. Open mints a connection+Surface-owned id after reserving 16/connection, 128 installation, 8-MiB item
and 1,024-MiB aggregate memory limits before read. Exact activity refreshes a 60-minute idle deadline; close,
source/root/target invalidation, Surface/connection loss or expiry releases and reconnect never inherits.
External content change keeps the base for conflict; applied save advances its revision and conflict does not.
They never use terminal input, persist snapshot bytes or restore them after loss.

Runtime inventory is a target-wide snapshot/delta stream with endpoint fingerprint/generation and closed
coverage, independent of Workspace-owned attempts. Unmatched handles remain recovery items rather than
invented Nodes. Adopt/ignore/terminate revalidate exact target+handle+inventory revision; only adopt mints a
Node and tagged AttemptOwner, and terminate can affect no sibling/host-wide pattern.

AccountProfile records are non-secret provider+ExecutionTarget identities pointing to isolated external
auth/config storage. Their closed state is
`draft|authenticating|validating|active|auth_failed|expired|revoked|retired|deleted`; deleted retains a
permanent id tombstone. Legal transitions are:

| From | May transition to |
| --- | --- |
| `draft` | `authenticating`, `validating`, `retired`, `deleted` |
| `authenticating` | `validating`, `auth_failed`, `retired` |
| `validating` | `active`, `auth_failed`, `expired`, `revoked`, `retired` |
| `active` | `validating`, `expired`, `revoked`, `retired` |
| `auth_failed` | `authenticating`, `validating`, `retired`, `deleted` |
| `expired` | `authenticating`, `validating`, `revoked`, `retired` |
| `revoked` | `authenticating`, `retired` |
| `retired` | `authenticating`, `deleted` |
| `deleted` | none |

Create allocates an empty isolated credential reference in `draft`; adopt binds an exact existing provider
config reference into `draft` without reading credentials. `AccountAuthenticationIntent` is Installation-
owned and durable before broker/helper/Browser launch. It freezes operation/fingerprint, exact profile/
provider/target/trust/profile/config-reference/broker-policy generations, provider correlation, origin state,
expiry and possible credential-reference effect. Its state is `prepared→dispatching|cancelled|expired`,
`dispatching→awaiting_provider|refused|reconcile_required`,
`awaiting_provider→authenticated|auth_failed|reconcile_required`, and
`reconcile_required→authenticated|not_applied|auth_failed|reconcile_required`; terminals never reactivate.
Post-dispatch timeout/crash remains reconcile-required. Get/reconcile uses correlation lookup only and never
relaunches or rewrites; cancel is prepared-only. Correlated authenticated atomically records effective
credential generation and moves the profile to validating; validation separately proves provider/account
identity and capability before active. Retire/revoke fences callbacks and delete refuses live/possible-effect
intents. One nonterminal intent exists per profile.

Turn-created roots are broker-confined to 64 MiB each/2,048 MiB aggregate, reserved prelaunch. Unsupported
write confinement/quota makes authentication unsupported. Exactly 10,000 nonterminal-or-uncompacted intents,
each≤4 KiB/32 MiB aggregate, and 180-day terminal richness are admitted; exactly8,192 maximum records reach
the independent byte boundary while the count boundary uses smaller records. Count/bytes/terminal/root/recovery N+1
refuses before broker/Browser/provider/root effect. Folding preserves operation/profile/target/credential/
correlation fences and never removes possible-effect evidence. Rename changes only bounded display metadata under CAS. A default may
reference only an `active` profile on the exact trusted target generation with proved isolation and required
capability. Default resolution is explicit
launch, immutable Flow/Template, Workspace, then target/provider; LaunchReceipt pins the result, and changing
a default never migrates active instances. When a default becomes ineligible it is explicitly unset; Turn
never selects another profile silently. `expired|revoked|auth_failed` never falls back to another account.
Retire removes launch/default eligibility and preserves evidence; delete is allowed only from the declared
draft/auth-failed/retired transitions with zero active attempts, current bindings, defaults, grants, auth intents or
retained required references. Its only external-data disposition is `retain_external_provider_data`; the
optional private-root disposition may delete only a still-fingerprint-matching Turn-created isolated root.
No AccountProfile operation can authorise provider-side account, conversation, transcript, activity or quota
data deletion.
Every verb has its own operation id, expected profile/target generation and receipt. Cross-profile attach/
transcript/quota/config-root reuse and missing-credential fallback are server-side refusals.

`AccountActivityProjection` is keyed by exact provider, AccountProfile and ExecutionTarget. Its bounded
cursor page keeps independently timestamped conversation-context observations, each provider quota window/
reset and exact conversation/NativeJobIteration/Attention inbox references. Every value carries source,
coverage, confidence, freshness and `unknown|unsupported|stale|rate_limited|fetch_failed`; absence or partial
page is never numeric zero or an authoritative empty inbox, and caches/cursors never cross profiles. Filter is
read-only. `mark_result_read`/`dismiss` mutate only the exact named projection or canonical Attention revision;
they do not acknowledge provider work, resolve a prompt or retire/delete a native job.

Display names are local metadata, not identity or provider authority. `DisplayNameFact` names one Node/Group,
source revision, exact source `declared|structured_task|provider_observed|generated|operator_alias|fallback`,
confidence, observed time and bounded sanitised label. `NameMode=follow_source|pinned`; an operator edit or
exact `apply_name_proposal` pins until explicit unpin, so provider/reconnect/generated facts cannot overwrite
it. Resolution precedence is pinned `operator_alias`, otherwise newest current fact at
`declared > structured_task > provider_observed > generated > fallback`; source revision then observation
time resolves one tier, and an unresolved equality remains explicit. Local rename emits neither
`conversation_rename` nor terminal bytes. Migrated v4 `rename_node` is exactly a local pinned
`set_local_display_name` for its resolved Node; it is not a second vNext mutation or provider operation.

`NameProposalId` binds the bounded captured source bytes/hash, target scope, Node/Group revision, generator
identity/model, redaction policy and expiry. Generation is on-demand unless a reviewed local policy sets its
exact bounds. Target-aware acquisition cannot send raw remote output to an undeclared local/network generator;
controls, bidi/invisible injection, paths, secrets and multiline labels are rejected. Applying a stale
proposal has zero effect. Group proposals consume bounded member summaries, never concatenated transcripts.
Captured source bytes are memory-only; persistence retains only proposal id, bounded metadata, content hash,
expiry and accepted/refused receipt.

The M13 coordination names are likewise reserved, not v4 claims. Direct endpoints accept create/change only
from foreground operator operations. An exact current Agent attempt carrying an unexpired ADR-061
`DelegationGrant` can exercise only the matching closed variant through `submit_delegated_operation`; all
other authenticated agent events may only propose them and create an Attention review. This is not
protection from an unsandboxed same-uid process that steals the administrative capability
and impersonates a UI. Message preparation holds the exact body in one expiring client-bound draft outside
an already reviewed Flow; a Flow stores only its reviewed deterministic body recipe. Message state is the
following closed product; decoder and store migration reject every unlisted combination:

```text
BodyAuthority = AdHoc(body=live|consumed|lost,
                      review=pending|reviewed|review_required)
              | FlowRecipe(policy_revision,
                           body=reassemblable|consumed|lost,
                           review=preauthorised|review_required)
Transport     = prepared | queued | submitted | submitted_unconfirmed | refused | failed | expired
Evidence      = { received?: EvidenceFact, read?: EvidenceFact, acted?: EvidenceFact }
```

| BodyAuthority | Legal transport/evidence |
| --- | --- |
| ad-hoc `live/pending` | `prepared`, empty evidence |
| ad-hoc `live/reviewed` | `prepared\|refused\|failed\|expired`, empty evidence |
| ad-hoc `consumed/reviewed` | `queued\|submitted\|submitted_unconfirmed\|refused\|failed(queue_body_lost)\|expired`; evidence empty unless submitted/unconfirmed |
| ad-hoc `lost/review_required` | only `failed(body_lost)`, empty evidence |
| Flow `reassemblable/preauthorised` | `prepared\|refused\|failed\|expired`, empty evidence |
| Flow `consumed/preauthorised` | `queued\|submitted\|submitted_unconfirmed\|refused\|failed\|expired`; evidence empty unless submitted/unconfirmed |
| Flow `lost/review_required` | only `failed(policy_invalid\|policy_reassembly_required)`, empty evidence |

Transport transitions are only `prepared → queued|refused|failed|expired`, `queued →
submitted|submitted_unconfirmed|refused|failed|expired`, and `submitted_unconfirmed → submitted` from
independently correlated evidence; every other transport terminal has no successor. Queue acceptance consumes
the body. `received`, `read` and `acted` are independent monotonic optional facts permitted only after a
submitted or unconfirmed write and never imply one another. Delivery assigns FIFO order under exact
256-item/1-MiB-per-destination and 600-second TTL bounds. One source connection has≤16 unaccepted drafts;
installation-wide there are≤10,000 live bodies,≤32 MiB body bytes (8,192 maximum bodies),≤64 MiB including queue/encoder overhead
and 100,000 pre-reserved body-free terminal metadata/replay slots. All family bytes also charge
`runtime.turn_variable_rss_mib`. Queue acceptance atomically moves the body and existing reservations from
connection+Surface to `(daemon generation,owning Workspace,AgentMessageDeliveryId,destination AgentInstance/
current-attempt generation)`; source disconnect cannot free it. Definite pre-write terminal/expiry releases
only after queue/encoder/transport-buffer quiescence, possible write stays charged until that proof, and daemon
death proves memory reclamation before recovery classification. Every item/destination/global/family/shared/
terminal N+1 refuses before effect. Delivery refuses overflow, requires a structured idle endpoint with no pending
interaction/human draft, never falls back to PTY, submits once and persists only hash/metadata/evidence.

Recovery classifies the exact external-effect boundary. Proven submission stays `submitted`; possible write
is `submitted_unconfirmed` and is never replayed. A definitely unstarted ad-hoc draft whose bytes die becomes
`lost/review_required/failed(body_lost)`; an accepted queued ad-hoc body whose bytes die before any possible
write preserves `consumed/reviewed` and becomes `failed(queue_body_lost)`. A queued Flow operation whose
assembled bytes die becomes `lost/review_required/failed(policy_reassembly_required)`; only a new message and
operation id may reassemble its still-current recipe after policy/destination/grant revalidation. An invalid
recipe becomes `failed(policy_invalid)`. The old operation is terminal and a hash is never replay material.
A destination disconnect before any possible write leaves Transport exactly `queued` with separate
`dispatch_ineligible(disconnected)` only while the body and authority remain available/current; reconnect
revalidates endpoint generation, while a mismatch reaches the declared `refused|failed` result. There is no
`suspended` AgentMessage state. Independently correlated late evidence may refine
`submitted_unconfirmed → submitted` without writing.
Dependency edges store only closed bounded state, producer/revision
ids, hashes/verified references, timestamped provenance/confidence and an optional bounded stripped/redacted
summary; raw output, PTY, transcripts, diff/file bodies, environment and arbitrary provider payloads are
invalid. Outside a FlowRun they emit no start/retry operation; inside one only the immutable reviewed start
policy may consume a current result through a separately idempotent operation. Teams store roles/policy only
and confer no socket, context, checkout, approval or focus capability beyond an explicit DelegationGrant.

Inside a FlowRun, `DependencyResultKey=(FlowRunId,DependencyEdgeId,producer StepId,producer StepAttemptId,
attempt ordinal,attempt generation,result revision)` is immutable and exact. Its closed schema separates
`outcome=succeeded|failed|cancelled|aborted` from
`origin=step_terminal(producer terminal receipt/hash)|verified_external(canonical source-receipt kind/key/
revision/digest)`, plus observed/effective times, provenance/confidence, bounded typed artifact/resource
references and an optional≤4-KiB stripped, redacted summary. The publication reducer is
`reserved→published|refused`; published/refused are terminal and
no terminal line, hook `done`, idle observation, deleted Node or absent producer can invoke it. A StepAttempt
terminal transition atomically publishes its edge results. The only non-StepAttempt path is the internal
`VerifiedExternalResultEvent`; it is not a client request, adapter bearer or remotely callable operation. An
immutable FlowDefinition revision may name only a closed existing Turn receipt kind, exact subject predicate,
accepted outcome derivation and edge. Preflight resolves that source and reserves the exact DependencyResultKey.
When the canonical source receipt itself commits a terminal revision, the daemon atomically derives outcome,
publishes the result and records one FlowOperationReceipt keyed by `(source receipt key+revision,FlowRun,edge)`.
No caller summary, provider event or hook may choose success. A forged client event is unrepresentable; stale,
cross-run, cross-edge, wrong-kind/subject/generation, changed digest and replayed source receipts publish nothing.
Body, parse or capacity failure publishes no fabricated value and leaves each downstream step blocked or
policy-failed.

One current result per edge is derived without mutation: the lowest attempt ordinal that published
`outcome=succeeded` wins, but a verified-external origin participates only when that edge's immutable policy
explicitly accepts its exact source kind; otherwise it is nonmatching. Otherwise only the final terminal attempt
after the retry budget/decision is exhausted supplies the
terminal failure result. Creating a retry makes an earlier non-success ineligible before readiness evaluation;
late evidence cannot replace a winning success or reopen a readiness decision. An `any_of` tie uses stable
edge id after current-result derivation. Each run admits≤4,096 DependencyResults and≤10,000 existing
FlowOperationReceipts; results are≤8 KiB each/≤32 MiB per run and≤100,000/256 MiB installation-wide. Capacity
reserves before the producer terminal commit so a terminal transition never becomes an unrecorded dependency
signal; N+1 leaves explicit policy-failure/recovery evidence and starts no dependant.

Deleting or losing a producer before publication is not a result and makes an otherwise required edge
impossible according to policy. Deleting it after one immutable current result was published neither revokes
that result nor republishes it: readiness consumes only the recorded DependencyResultKey and a committed
StepReadinessReceipt remains final. Idle/done labels, hook text and deletion are never alternate success paths.

`FlowRunState` is `preflighting|provisioning|running|paused(resume_state)|failing|cancelling|
reconcile_required(last_proved,desired_terminal?)|completed|failed|cancelled|aborted`, with the legal
transition, desired-terminal, StepAttempt and deterministic result-aggregation rules in the master contract.
Definition revisions are immutable; FlowRun receipts are append-only. Pausing one FlowRun starts no new
StepAttempt in that run and does not freeze existing runtimes; it has no effect on the separate recurrence
schedule/FlowRunTrigger of the immutable definition. Cancel revokes grants before
applying each persisted runtime disposition; abort is a separate foreground force action and remains
`reconcile_required(desired_terminal=aborted)` while any disposition is uncertain. Step retry creates
one new bounded attempt and whole-run retry creates a lineage-linked run. Every Flow/Grant request includes
the expected definition/run/authority revision plus operation id; a stale request fails before effects and a
duplicate returns the original receipt. The exact recurrence timezone/DST/missed/overlap/occurrence bounds in
the product contract are wire fields, not free-form settings.

`FlowRunTrigger` is tagged `manual{accepted_preflight_revision}` or
`bounded_recurrence{definition_revision,occurrence_id,scheduled_instant,schedule_policy_revision}`. The
scheduler journals the latter before creating its at-most-one FlowRun; it is not a StepAttempt trigger.
`StepStartPolicy` is tagged `manual`, `with_run`, `after_success{edge_id}`,
`after_result{edge_id,predicate}`, `all_of{edge_ids,predicates}` or
`any_of{edge_ids,predicates}`. Manual requires `start_flow_step`; with-run consumes the exact transition of
its owning run to running; only the four dependency variants accept `DependencyResult` revisions. A
`StepReadinessReceipt` includes run, step, policy revision and the foreground/run-transition/result-set
trigger identity; `any_of` records the first committed match and stable edge id resolves a same-transaction
tie. It is the closed `step_readiness` variant embedded in the already manifest-owned durable
`FlowOperationReceipt`, not a separate state family. It also carries StepAttempt reservation id, expected run/
step revisions, the canonical ordered DependencyResultKey set and its digest, decision time and one
`ready|refused_stale|refused_nonmatching|refused_impossible` outcome. The `ready` receipt and
`blocked→ready` transition commit atomically before any start effect; a separately idempotent start consumes
that exact receipt once. Duplicate or stale trigger identities return the original receipt or a zero-effect
refusal, and compaction cannot remove it while the run/attempt or a recovery/replay fence refers to it.

For `manual`, that consumer is the foreground `start_flow_step` request. For every dependency policy it is the
daemon-only `ReadyStepDispatchEvent=(StepReadinessReceiptId,receipt revision+digest,preassigned StepAttemptId,
preassigned RuntimeLaunchIntentId)`; it is not a client request or adapter event. The existing
`FlowOperationReceipt` embeds a `step_dispatch` variant with `reserved→launch_reserved|refused`,
`launch_reserved→started|reconcile_required`, and `reconcile_required→started|failed|reconcile_required` by
lookup of the exact RuntimeLaunchIntent only. Receipt/attempt/launch/recovery capacity reserves in the same
transaction that commits ready. A daemon crash before launch consumes the still-current reserved dispatch once;
after any possible launch it reconciles and never creates a second StepAttempt or RuntimeLaunchIntent. Changed
run/step/policy/result digest, duplicate dispatch and a client-authored imitation refuse before effect.

`refused_impossible` or any definitively impossible required dependency atomically sets the exact Step/FlowRun
Status to Blocked with reason and creates one deduplicated actionable Attention demand keyed by
`(FlowRunId,StepId,StepReadinessReceiptId)`, unless the immutable `fail_run` policy immediately enters its
already specified failing reducer, in which case that exact failure/recovery demand is used. Navigation never
acknowledges or resolves it; a later immutable result cannot reopen that terminal readiness decision.

A required failure with `on_failure=fail_run` never commits terminal `failed` while another step/effect is
active or unclassified. It enters `failing`, atomically prevents new starts and revokes every run grant. The immutable definition supplies each active
step's `leave_running|interrupt_then_terminate|terminate` failure disposition and each not-started step's
`skip|cancel` disposition. Reconciliation records every step as terminal, policy-skipped/cancelled, or an
explicit detached survivor with exact Node/AttemptOwner and last evidence; every external disposition has a
definite receipt. Only then may `failing → failed`. An uncertain disposition enters
`reconcile_required(last_proved=failing,desired_terminal=failed)`; it cannot infer success or retry. A `leave_running` survivor remains visible and may
continue to emit runtime-continuity evidence, but it is detached from scheduling and later evidence cannot
rewrite the immutable failed run result. Cancel/abort may strengthen a pending failed desire to
`cancelled|aborted` according to the master transition table, never clear the failure or restart work.

The adapter capability schema is versioned and closed to exactly 23 facts:
`launch|resume|branch|stop|structured_status|questions|permissions|subagents|transcript|context_usage|
provider_quota|model_switch|mode_switch|messaging|context_transfer|shared_identity|durable_attach|delegated_control|
native_jobs|conversation_inventory|title_read|conversation_rename|model_gateway`. Each fact is independently
keyed by adapter/CLI version, provider, AccountProfile, ExecutionTarget, endpoint, attempt/generation and
observation epoch as applicable and reports `supported|unsupported|degraded|unknown`, mechanism, bounds,
freshness and expiry. Claude Code, Codex, Gemini, OpenCode, GitHub Copilot and Grok each use a dedicated
adapter and the complete matrix; provider-name branches and executable-name inference are invalid substitutes.
Kimi and MiniMax are permanently scoped as first-class profile-scoped quota/activity connectors, not launch
adapters; those connectors expose no launch, transcript, conversation or control authority.
The generic terminal adapter advertises only facts it proves. RuntimeBackend, broker, endpoint and delegated-
authority capabilities remain separate and an operation uses their intersection, never their union.
The `permissions` fact additionally has a fact revision and closed
`response_transport=typed(schema_id,schema_version,transport_generation)|verified_local_pty(encoder_id,
encoder_version,transport_generation)|none`. Typed permission operations require current `supported+typed`;
the local PTY fallback requires current `supported+verified_local_pty`. Unsupported, degraded, unknown,
stale/expired and none fail closed for semantic permission response. An opaque TUI cannot infer a transport.
The vNext `welcome` reports `adapter_capability_schema_version` and its canonical registry hash. The frozen
`docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv` source-capability ledger is release authority, not a runtime payload:
no request may read, import or mutate it, and its rationale/evidence digests never cross the wire.

`RuntimeContinuityView` reports endpoint kind/host, stable non-secret fingerprint/generation, integration
capability and a list of independently keyed `RuntimeEndpointBinding`s: provider/account-scope/host,
AgentInstance/RuntimeAttempt, conversation, last observation and attach/resume confidence—never a bearer
token or descriptor. Semantic conversation ownership is keyed independently of transport:

```text
ProviderAccountScope=profiled(AccountProfileId,AccountProfileRevision)
                   | endpoint_unscoped(UnscopedRuntimeScopeId)
ConversationKey=(provider_id, ProviderAccountScope, ExecutionTargetId, provider_namespace, normalized_provider_conversation_id)
BindingState = proposed | current | refused | stale | unbound | retired
```

`normalized_provider_conversation_id` is exact private identity and the adapter declares the namespace that makes it
unique. A profiled scope prevents cross-host/account aliasing. For a source with deliberately absent account
metadata, the daemon mints one opaque `UnscopedRuntimeScopeId` into the durable RuntimeEndpoint and preserves it
only across signed continuity of that exact provider/target/endpoint root; it prevents aliasing inside that
root but creates no AccountProfile, credential, quota, activity, inventory or cross-endpoint authority. A
failed continuity proof mints no replacement binding and leaves the old binding stale. Across all endpoint
records and generations,
one `ConversationKey` has at most one `current` AgentInstance owner and one AgentInstance has at most one
current binding. Endpoint generation fences the transport only; it cannot make the same conversation a
second semantic identity. Binding transitions are closed: `proposed → current|refused`, `current →
stale|unbound|retired`, `stale → current|unbound|retired`, `unbound → proposed|retired`, and
`refused|retired` are terminal for that binding id. A duplicate current claim is rejected before input,
transcript or context authority and cannot displace the proved owner. Endpoint mismatch, generation
discontinuity, ownership conflict or stale proof changes only BindingState/connectivity, never
`Lifecycle::Lost`; only an independent bounded RuntimeBackend/provider absence proof may produce Lost.

`RuntimeEndpointContinuityProofV1` is a closed authenticated value, not prose evidence:

```text
RuntimeEndpointContinuityProofV1 = {
  algorithm = hmac_sha256,
  key_epoch, root_proof_sequence, nonce_128,
  provider_id, ExecutionTargetId, TargetGeneration, RuntimeEndpointId,
  endpoint_root_fingerprint,
  prior_endpoint_generation, candidate_endpoint_generation,
  UnscopedRuntimeScopeId,
  endpoint_binding_inventory_revision,
  binding_claim_count, binding_claims_digest_256,
  issued_at, expires_at,
  authenticator_256
}

BindingContinuityClaim = {
  RuntimeEndpointBindingId, binding_generation,
  provider_namespace, ConversationKeyHash,
  AttemptOwner, owner_generation,
  continuity_evidence = present(RuntimeAttemptId, AttemptGeneration, binding_proof_sequence)
                      | unavailable(reason=not_observed|backend_unavailable|owner_unavailable)
}
```

The authenticated preimage is ASCII domain separator `turn/runtime-endpoint-continuity/v1`, followed by every
field above except `authenticator_256` as fixed-width unsigned big-endian scalars or a four-byte-length-prefixed
UTF-8/opaque byte string in exactly the displayed order. `binding_claims_digest_256` is SHA-256 over
`binding_claim_count` followed by each claim encoded by the same rule, sorted strictly by BindingId; count is
1..64 and duplicate/missing/extra claims relative to the pinned endpoint binding inventory are a malformed
batch and refuse. `present` and `unavailable` have distinct fixed tag bytes; an unavailable claim still names
the exact inventory member and is authenticated rather than silently omitted. Overlong, noncanonical UTF-8,
unknown/duplicate fields, unknown tags/algorithm or duplicate encodings refuse the entire batch before any
candidate observation. `expires_at-issued_at` is1..300
seconds and accepted clock skew is≤30 seconds.
The signer is the exact endpoint broker/adapter holding one restart-stable non-exportable 256-bit secret in the
OS keystore or target-host secret broker; Turn stores only its broker reference, root fingerprint, current key
epoch, root proof-sequence high-water and per-binding monotonic proof-sequence high-water inside the existing durable `RuntimeEndpoint`/
`RuntimeEndpointBinding` families. No key enters config, argv, protocol reads, diagnostics or export.

One RuntimeEndpoint admits≤64 non-retired bindings; the 65th admission refuses before a binding, owner or
authority mutation. Observing a new candidate endpoint generation first atomically records `revalidating`,
transitions every current binding in that pinned inventory to stale and freezes the candidate/root/key epoch;
that observation grants no thread authority. Verification has two layers. The root layer requires current
target trust, exact root fingerprint, candidate generation=prior+1, current key epoch, strictly increasing root
proof sequence, unexpired time, constant-time MAC equality and the exact complete inventory digest. A root,
MAC, key, endpoint, target, generation, time or batch-shape failure leaves every binding stale and records one
closed root reason; it never evaluates claims as authority. Once the root is authenticated, each claim is
independently checked against its exact binding generation, namespace, ConversationKey, owner, attempt and
monotonic binding proof sequence. One transaction advances the endpoint generation/root high-water and every
applicable per-binding high-water, records the ordered 1..64 result vector and promotes only valid `present`
claims stale→current. An `unavailable`, stale, replayed, mismatched or ownership-conflicted claim remains stale
with one closed per-claim reason and cannot block or alter a valid sibling. Thus a valid batch containing 63
valid claims and one invalid claim commits 63 current bindings and one stale binding under one inventory
revision. No path leaves old-generation authority current. Rotation
mints a higher nonreused epoch in the broker, atomically revokes the old epoch and stales affected bindings until
new proof; deleting an endpoint revokes its key reference and
retains epoch/sequence high-waters. Keystore loss is explicit `continuity_unavailable`, never a fresh scope.
Creating a different endpoint root/scope is a separate foreground endpoint-create operation and cannot replace,
repair or auto-rebind the failed root.

`RuntimeEndpointContinuityReceipt` has closed terminal variants
`committed_results(results[1..64])|committed_stale(root_reason)|refused(reason)`. Each result is exactly
`current|stale_unavailable|stale_invalid|stale_replay|stale_conflict`, names the BindingId/generation and carries
no transcript/body. `continuity_unavailable` is a closed `committed_stale` root reason (for example keystore or
broker loss), never another receipt state. Verification and the endpoint/binding CAS are one local transaction
after the candidate-observation stale edge, so there is no dispatching/possible-effect state. Its Rotate variant
is likewise `rotated|refused`: the broker creates an unadopted key reference first, and one local CAS adopts the
higher epoch and distrusts the old epoch; an orphan pre-adoption key has no Turn authority and is broker cleanup,
while a committed rotation never rolls back. Get/reconcile only retrieves these receipts.

`RuntimeEndpointContinuityVerificationBuffer` is request-only ephemeral state owned by
`EndpointBrokerConnectionGeneration+OperationId+ExecutionTargetId+TargetGeneration+RuntimeEndpointId+
PriorEndpointGeneration+CandidateEndpointGeneration+ProofDigest`. `EndpointBrokerConnectionGeneration` is
the exact authenticated endpoint-broker channel generation; its loss releases the buffer after verification
work quiesces. At most128 continuity verification buffers exist installation-wide, one/endpoint, each≤256 KiB
and≤32 MiB aggregate with a five-second deadline; they survive no broker disconnect.
Rich receipts
are≤100,000/≤4 KiB each under≤256 MiB, whose independent byte bound admits65,536 maximum records. Receipt,
operation-replay and refusal capacity reserves before candidate observation/MAC/high-water mutation. Each
operation also pre-reserves an exact≤512-byte installation-lifetime fence containing operation id,
fingerprint, endpoint/candidate/key epoch/proof sequence and the complete terminal result digest; a scalar
epoch or sequence high-water cannot substitute because it cannot reject changed request bytes. This independent
pool admits1,000,000 smaller fences or exactly983,040 maximum fences/480 MiB. Terminal richness retains180
days behind that exact fence, while revalidating candidate evidence never ages out. Rich or minimal count/byte
N+1 returns capacity refusal without changing endpoint or binding state; same id/fingerprint replays the
identical result and changed bytes conflict after compaction.

`ConversationProfileRebindReceipt` is Workspace-owned with only terminal `committed|refused` variants because
the operation has no external effect. Commit atomically reserves and inserts the new exact
profiled ConversationKey/BindingId under the same AgentInstance, records immutable old-unscoped→new-profiled
scope lineage, retires the old binding and advances the global uniqueness registry. Any stale proof/profile/
grant/owner/registry generation or duplicate owner refuses with no partial state. No transcript/index/cache/
credential/quota/activity/context/input/Attention bytes or authority move; subsequent reads independently use
the new profile grants. `ConversationProfileRebindBuffer` is request-only ephemeral state owned by
`LocalClientInstanceId+ConnectionGeneration+SurfaceId+RequestId+WorkspaceId+RuntimeEndpointBindingId+
BindingGeneration`; there is≤1/old binding and128 installation-wide, each≤64 KiB and≤8 MiB aggregate. It
survives no local foreground-authority, Surface or connection loss and releases only after in-flight work
quiesces.
Rich receipts are
≤100,000/≤4 KiB under≤256 MiB, retain180 days and leave a permanent operation/fingerprint/old+new key/result
fence. Each mutation pre-reserves that separate≤512-byte fence before the ownership CAS; its pool admits
1,000,000 smaller records or exactly983,040 maximum records/480 MiB. Rich and minimal count/byte boundaries are
independent, their N+1 refuses before mutation, same id/fingerprint replays the terminal result and changed
bytes conflict. Get/reconcile only returns the atomic receipt.

Every input/transcript/context/Attention operation names binding id and binding generation as well as the
ConversationKey hash. One endpoint may multiplex many current bindings, but duplicate or cross-scope/
target claims fail without changing siblings. Domain `attach_runtime_attempt` binds one already-existing,
reuse-safe live/orphaned attempt to its exact semantic owner after endpoint proof and launches nothing; it may
promote `proposed|stale→current` only in the same CAS as global uniqueness and attempt reattachment. It never
creates a Pane: presentation uses `attach_pane`, automatic resync and `detach_runtime_view`. A cold
continuation is only `resume_agent_instance`. Reconnect first creates/revalidates a
`proposed|stale|unbound` binding and promotes it atomically only after the global uniqueness check.
A configured provider-runtime/multiplexer is part of the continuity seam; general Remote/SSH Session creation
is M16. Acceptance creates competing endpoint records and generations for one ConversationKey and proves
that no observation can produce two current owners.

`RuntimeAttachmentReceipt` is Workspace-owned and freezes operation/fingerprint, owner, the pre-existing
RuntimeAttempt and its prior lifecycle, target/backend/durable-handle/process-start identity, prior/resulting
binding and generations, endpoint-continuity receipt/correlation, global ownership-registry revisions and one
closed outcome `attached|recovered|refused|uncertain`. `attached` means a proved live current attempt was bound;
`recovered` means the exact orphaned/disconnected attempt became reconnected; neither creates an attempt nor
causes a process lifecycle effect, while `recovered` alone advances observed Lifecycle under exact proof. A possible backend attach with an unproved result is `uncertain` and reconciliation
is lookup/probe-only; same operation/fingerprint returns the identical receipt. Rich receipts are≤100,000,
each≤8 KiB and≤512 MiB (65,536 maximum receipts at the independent byte bound), retain180 days behind an
installation-lifetime minimal operation/fingerprint/owner/attempt/result fence. That fence family independently
admits1,000,000 smaller records or exactly983,040 maximum≤512-byte records/480 MiB; count and bytes never
saturate together. Receipt, replay, correlation and semantic-recovery capacity reserve before any backend attach or CAS.

`RuntimeEffectOperationKind=launch|lifecycle|configuration|interrupt` is one shared bounded durability pool,
not four separately spendable limits. One charged rich record is the complete intent+receipt bundle for one
operation,≤8 KiB;≤100,000 small bundles or≤512 MiB installation-wide, with the maximum-item byte fixture
admitting exactly65,536 bundles. Every operation simultaneously reserves one independent permanent minimal
fence before identity/CAS/signal/spawn/provider effect; that pool admits either1,000,000 smaller fences or
exactly983,040 maximum≤512-byte fences/480 MiB. Terminal rich metadata retains180 days and compacts only behind
its reserved operation/fingerprint/kind/subject/result fence. Nonterminal, possible-effect,
reconcile-required and cleanup-pending evidence never ages out. Count and byte N+1 for either pool refuse
before effect. Runtime-launch reservation follows the destination's state, not an origin label: when the
destination Node already owns an eligible `node_aggregate`, Resume/Restart/Recycle or any other launch into
that Node inherits it and allocates no second semantic slot; otherwise the launch reserves its own subject
before identity/spawn and transfers that same ReservationId one-to-one when its preassigned Node becomes live.
This covers inert saved descriptors, `session_activation`, `runtime_node`, Fresh, Branch and each new Flow
step without an origin-shaped gap. The FlowRun keeps its separate coordinator reservation and no child
inherits it; a step targeting an already-reserved Node instead inherits that Node. A successful Flow runtime
therefore never exists without Node coverage. A Companion child is the explicit exception: it inherits the
Companion launch slot and that parent transfers on registration. Runtime-effect intent/receipt replay remains
independently bounded in every case. Each child of a bulk restart likewise follows its own Node/lifecycle rule
and never inherits the bulk coordinator reservation.

`RuntimeLaunchIntent` freezes operation/fingerprint, closed origin
`session_activation|runtime_node|fresh_agent|resume_agent|branch_agent|restart_replacement|
recycle_replacement|flow_step|companion_agent`, AttemptOwner, preassigned Node/AgentInstance when applicable,
preassigned RuntimeAttemptId+generation and RuntimeLaunchReceiptId, exact LaunchSpec and Workspace/Session/
graph/target/trust/backend/adapter/account/catalogue/checkout/launch-policy/conversation/binding generations,
plus lookup-capable provider/process correlation. Its closed reducer is
`prepared→dispatching|cancelled|refused`,
`dispatching→created|refused|submitted_unconfirmed|reconcile_required`, and
`submitted_unconfirmed|reconcile_required→created|not_created|reconcile_required` by lookup/probe only.
Terminal internal states are `created|refused|cancelled|not_created`; the closed wire receipt remains
`created|refused|uncertain`, mapping cancelled to `refused(reason=cancelled)` and not-created proof to
`refused(reason=proved_not_created)`, while `submitted_unconfirmed|reconcile_required` projects as `uncertain`.
No reconciliation edge spawns, retries, remints or changes origin. Created publishes
the exact preassigned attempt once; proved refusal/absence publishes none; uncertainty retains all identities
for `get_runtime_launch_operation`/`reconcile_runtime_launch_operation`.

`RuntimeInterruptReceipt` is the intent and receipt for one narrow interrupt. It freezes operation/fingerprint,
AttemptOwner/RuntimeAttempt/binding/target/backend/durable-handle/process-start identity, every associated
lifecycle/input-safety generation and signal correlation.
Its closed reducer is `prepared→dispatching|cancelled|refused`,
`dispatching→sent|not_sent|submitted_unconfirmed|reconcile_required`, and
`submitted_unconfirmed|reconcile_required→sent|not_sent|reconcile_required` by observation/probe only.
`sent|not_sent|cancelled|refused` are terminal; the remaining two states project as `uncertain`. Prepared
receipt, rich/minimal replay, correlation and semantic-recovery reservation commit before the one signal.
`get_runtime_interrupt_operation` and `reconcile_runtime_interrupt_operation` never signal; same
operation/fingerprint returns the identical state and changed bytes conflict.

Runtime lifecycle is a closed operation algebra, not a boolean on restart:

```text
RuntimeLifecycleOperation = bind_existing_attempt | resume | restart_live | interrupt |
                            terminate_graceful | kill_force | recycle_infrastructure |
                            detach_view | destroy_semantic_owner
```

`bind_existing_attempt` is wire `attach_runtime_attempt`: it may change binding/continuity state for the same
proved existing attempt but never process lifecycle. Presentation attach/detach are exclusively
`attach_pane`/automatic resync and `detach_runtime_view` and retain the exact RuntimeAttempt. Resume consumes
either a terminal/stopped attempt or committed adopted `no_prior_attempt`, plus verified ConversationKey
ownership/binding, and creates one new attempt under the same AgentInstance without stopping a live process.
Restart consumes an exact live attempt, stops it once and then
creates one replacement attempt; a Tool keeps its Node, while an Agent keeps its Node/instance/conversation only
with exact continuity proof and otherwise refuses while offering separate `create_agent_instance` Fresh Start.
Adapter support can refuse either tag but can never convert Resume, Restart or Fresh Start into one another.
Interrupt uses only its existing one-shot receipt. Terminate requests
the declared graceful backend action and reports `still_running` at timeout without escalation; Kill is a
separate reviewed force action. The source-style `ptyKill` subscriber release maps only to `detach_runtime_view`,
never force-kill. Recycle preserves a Node—and an AgentInstance/conversation only with exact continuity proof—
while replacing runtime infrastructure; Destroy runs the total semantic deletion reducer and its runtime
disposition without conflating provider/user-data deletion.

`RuntimeLifecycleIntent` is tagged `restart|terminate|kill|recycle`. Terminate and Kill use the closed reducer
`prepared→dispatching|cancelled|refused`, `dispatching→exited|still_running|submitted_unconfirmed|
reconcile_required`, and `submitted_unconfirmed|reconcile_required→exited|still_running|reconcile_required`
from correlation/probe evidence only; no edge changes kind or dispatches again. Restart and Recycle use
`prepared→stopping_old|cancelled|refused`, `stopping_old→old_stopped|old_still_running|reconcile_required`,
`old_stopped→starting_replacement`, `starting_replacement→recycled|replacement_failed|reconcile_required`,
and `reconcile_required→old_stopped|old_still_running|recycled|replacement_failed|reconcile_required` by
lookup/probe only. It cannot enter `starting_replacement` until exact old handle/process absence is definite.
For Restart, the corresponding terminal label is `restarted` rather than `recycled`; its frozen tag and
preflight distinguish task relaunch from infrastructure replacement and reconciliation cannot swap them.
Every state and `RuntimeLifecycleReceipt` freezes kind, AttemptOwner, old/new attempt ids, target/backend/
handle/process-start identities, all generations, consequence/preflight digest, external correlation and
possible-effect boundary. Same operation/fingerprint returns the original receipt; changed bytes conflict.

There is at most one nonterminal lifecycle intent per AttemptOwner and 10,000 installation-wide. Its complete
intent/receipt bundle and permanent replay fence consume the shared RuntimeEffectOperationKind pool above.
Intent, receipt, operation replay, journal, provider/runtime correlation and cleanup-recovery capacity reserve
atomically before the first signal or launch. Restart/Recycle validate and attach the replacement
RuntimeLaunchIntent as an inherited child of the existing node reservation before the first old-runtime signal,
so runtime-effect-capacity failure leaves the old runtime untouched and no second semantic slot can collide.
Destroy reserves every per-runtime lifecycle intent together with the
total semantic survivor vector; a signal failure or unreachable cleanup is inventoried and never vetoes row
removal or resurrects the semantic owner.

`ConversationInventory` is a private bounded adapter query available only to
`ProviderAccountScope=profiled` with the exact AccountProfile read grant, over one exact provider, AccountProfile,
ExecutionTarget and provider namespace. Pages contain ConversationKey, optional provider title, created/
updated time, native status, model/mode hints, ownership/resumability evidence, source revision, coverage and
freshness—never ambient transcript bodies. The adapter declares provider-side versus complete-cache search,
predicates/normalisation, cursor/page/scan/cache/rate bounds; gaps, partial pages, rate limiting and
unsupported search cannot prove absence or exact zero. Exact-key proof may bind; title/text similarity is
advisory only.

`adopt_conversation` is a local atomic ownership CAS, not a provider action. Before mutation it reserves the
preassigned NodeId, AgentInstanceId, RuntimeEndpointBindingId, `ConversationAdoptionReceiptId`, operation replay
and semantic-recovery slots and freezes the exact complete inventory descriptor/digest, profiled
ConversationKey, source/profile/grant/target/adapter/capability/endpoint, destination Session/tree and global
ownership-registry revisions. Commit creates exactly one stopped Agent Node/AgentInstance, its semantic owner
and a `proposed` endpoint binding only to the preassigned identities; adoption never promotes that binding to
`current`—the first legal runtime transition is `resume_agent_instance(no_prior_attempt)` under an independent
continuity proof; domain attach becomes legal only after a real attempt independently exists and is proved. It launches, resumes, sends input,
reads a transcript and calls the provider zero times. Any incomplete/gapped inventory, stale field, capacity
failure, duplicate current owner or concurrent destination/registry change returns `refused` with no partial
identity. `ConversationAdoptionReceipt` is Workspace-owned and terminal-only `committed|refused`; same
operation/fingerprint returns the same receipt and changed bytes conflict. Get/reconcile are pure lookup. Rich
receipts are≤100,000/≤4 KiB each under≤256 MiB, retain180 days and then fold only after a permanent
operation/fingerprint/ConversationKey/preassigned-id/result fence exists. Before the ownership CAS, adoption
atomically reserves both one rich-receipt slot and one independent≤512-byte minimal-fence slot. The rich count
boundary uses smaller receipts; the byte boundary admits exactly65,536 maximum receipts. The permanent fence
independently admits either1,000,000 smaller records or exactly983,040 maximum records/480 MiB and preserves
the terminal `committed(no_prior_attempt)|refused` outcome because only the exact committed outcome can
authorise first Resume. Same id/fingerprint replays it after rich compaction; changed bytes conflict. Any rich
or minimal count/byte N+1 refuses before the CAS, so no Node, instance, proposed binding or ownership appears.

The UI phrase “Resume conversation” is not a wire operation and creates no alternate reducer: it resolves an
already owned stopped Agent Node and emits exactly `resume_agent_instance` with that operation's complete
preflight, preassigned attempt/launch identities and continuity fences. An unowned inventory row offers Adopt,
not Resume; adoption never chains into an automatic resume. Title observation requires `title_read`; provider mutation requires
the distinct capability `conversation_rename` plus a pre-effect ExecutionTarget-owned ConversationRenameIntent.
It freezes operation/fingerprint, exact key/profile/target/adapter/capability generations, expected provider-
title revision, single-line requested title≤512 UTF-8 bytes/200 scalars+hash, lookup-capable correlation and
tagged ownership proof: owned identity/current optional attempt/binding generations, or unowned global-
registry+inventory revision only when no owner exists. State is `prepared→dispatching|cancelled`,
`dispatching→submitted|refused|reconcile_required`, `submitted→resolved|reconcile_required`, and
`reconcile_required→resolved|not_applied|reconcile_required`. Resolved requires correlated effective title/new
revision; same-title observation never proves it. Cancel is prepared-only and reconciliation is lookup-only.
One nonterminal/key and 10,000 nonterminal-or-uncompacted≤4-KiB intents/32 MiB are hard; the independent
byte boundary admits exactly8,192 maximum records, while the count boundary uses smaller records. The 180-day terminal
folding retains operation/key/correlation/result fences and N+1 capacity refuses pre-dispatch. Unsupported or
uncertain rename changes no local/pinned/effective title; a separate explicit local alias remains available.

An `endpoint_unscoped` key is visible only in its exact RuntimeContinuityView and can carry typed operations on
that already-bound thread when endpoint and adapter capabilities prove them. It cannot list/search/adopt
arbitrary conversations, query quota/activity, resolve a profile credential or migrate across endpoints.
Association with a profile requires a separate foreground rebind, exact endpoint/thread/profile proof, a new
profiled ConversationKey and retained lineage; a default-profile change or reconnect never casts the scope.

Provider-native work is keyed by
`NativeJobKey=(provider_id, AccountProfileId, ExecutionTargetId, provider_namespace, provider_job_id,
provider_job_incarnation)`. Incarnation is stable/non-reused provider evidence; otherwise identity support is
absent. Exactly one installation-wide Job Node owns a key. `adopt_native_job` locally CASes a current complete
inventory/get observation into one destination Session/optional Group and emits zero provider request. It
refuses while a create intent is nonterminal in that profile/namespace; competing Workspaces get one owner or
a non-disclosing conflict, never duplicate Nodes.

`create_native_job` first atomically reserves an installation-minted
Job Node, `NativeJobCreationId` and `NativeJobCreateIntent` under its exact destination Session/optional Group,
before any provider effect; replay by operation id returns the same reservation. Its closed state is
`prepared → dispatching|cancelled`,
`dispatching → bound|refused|reconcile_required`, and `reconcile_required → bound|not_created|
reconcile_required`; its terminal bound receipt installs NativeJobKey on the same Node. Thus uncertainty has
a stable WorkSurface/Attention route. Only `cancel_native_job_creation` may CAS a still-`prepared` intent to
cancelled; it refuses after dispatch starts and emits zero provider request. That Turn-minted id is the only creation correlation. `create` is advertised only with proved
`create_correlation=idempotency_key_lookup|provider_receipt_lookup`; both expose side-effect-free exact
applied/not-applied lookup and a write-only idempotency key is insufficient. Its receipt maps creation id once
to NativeJobKey, and timeout reconciles only that correlation. `run_now` uses a distinct Turn-minted
`NativeJobInvocationId` with the same lookup-capable correlation and maps it exactly once to the
resulting NativeJobIterationKey. Label, schedule, definition or temporal proximity never binds or retries
either operation.

`NativeJobDefinitionSpec=provider_template_ref(template_id,revision)|reviewed_instruction(content_type,
byte_count<=65536,sha256,private_bytes)`. Create/update freeze
`NativeJobConfigurationIntent(requested_definition,schedule,time_zone,model,safe_flags,
pre_effective_revision)`, and every field has `pending|accepted(receipt,effective_hash?)|refused(reason)|
uncertain(correlation)` requested state. The independent read shape is
`NativeJobEffectiveConfigurationObservation(last_proved_definition,schedule,model,flags,native_values,
revision,freshness)|unavailable(reason,last_hash?)`. JobNodeView always keeps last proved A, requested B and B's
state distinct across refusal, timeout, coercion and restart; requested never means effective. Exact Job/
creation-scoped storage may retain the requested definition up to 64 KiB. Provider-observed bodies share that
cap; oversized/opaque evidence yields unavailable plus safe count/hash. Logs, diagnostics, broad exports,
terminal/context and control decoding receive no body. Schedule state and iteration state are
separate: `NativeJobScheduleState=scheduled|paused|completed|failed|cancelled|unknown`, while stable
`NativeJobIterationKey=(NativeJobKey,normalized_provider_iteration_id)` has its own revision and closed
`NativeJobIterationState=queued|running|succeeded|failed|cancelled|unknown`. Schedule permits scheduled→paused/
completed/failed/cancelled/unknown, paused→scheduled/completed/failed/cancelled/unknown and unknown→any;
iteration permits queued→running/succeeded/failed/cancelled/unknown, running→succeeded/failed/cancelled/unknown
and unknown→any. Terminal schedule/iteration states never regress for one key/incarnation. Each axis retains
its native value and total mapping. An iteration carries scheduled/started/finished time, safe result/error
metadata ≤32 KiB (summary≤4 KiB, error code/message≤4 KiB, original count/hash, ≤16 inert refs≤512 bytes), and optional
exact AgentInstance/RuntimeAttempt reference; cancel requires its exact key/revision and queued/running state.

Each NativeJob materialises at most 1,100 iteration keys: 1,000 `queued|running|unknown` active rows plus 100
newest unreferenced terminal rows. Active rows retain their cancel/control key and are never evicted for
terminal history. Turn refuses a 1,001st active run before provider effect; unpausable external excess records
a coverage gap and disables exact absence/control instead of overwriting a key. Terminal row 101 compacts only
the oldest eligible unreferenced terminal row, otherwise coverage gaps.

`NativeJobPage` includes NativeJobScanId, daemon scan ordinal, profile/target/adapter generations, optional fixed
provider snapshot watermark, page sequence, predecessor-cursor digest, next cursor, terminal flag,
`complete|partial|gapped` coverage and freshness. Pages apply idempotently only in one complete cursor chain;
only the greatest-started scan reaching a complete terminal page may replace inventory or prove absence.
Later-started scans fence older pages. A provider without a stable snapshot/watermark cannot produce complete
absence. Versioned provider events advance only a newer exact key; unversioned webhooks merely trigger get/list.
Ordering is target generation, profile revision, adapter generation, provider revision/scan ordinal, event id.
Stale evidence cannot regress terminal state or tombstone.

The projection separately records `NativeJobPresenceState=observed|stale|missing|provider_deleted`, local
`NativeJobProjectionState=visible|activity_hidden|forgotten`. Complete fresh exact-key absence without exact
deletion evidence is `missing`. `provider_deleted` requires either a correlated Turn delete intent plus proved
absence or an authenticated stable provider tombstone/event naming exact key/incarnation/revision; it is terminal
and reuse mints a new incarnation. Presence permits observed→stale/missing/provider_deleted, stale→observed/
missing/provider_deleted and missing→observed/provider_deleted. Partial/filter/gap/auth loss proves no absence.

Every update/pause/resume/run-now/iteration-cancel/provider-delete owns immutable
`NativeJobMutationIntentId(operation_id,payload_fingerprint,tagged_subject,requested_configuration?,
expected_revisions/generations,possible_effect,lookup_correlation)`. Its reducer is
`prepared→dispatching|cancelled`, `dispatching→submitted|refused|reconcile_required`,
`submitted→resolved|reconcile_required`, `reconcile_required→resolved|not_applied|reconcile_required`; terminals
are cancelled/refused/resolved/not_applied. The intent is durable before dispatch, dispatching recovery is
lookup-only and `cancel_native_job_mutation` affects only prepared. Definition/schedule/delete conflicts
serialize, iteration cancellation serializes by iteration, and independent InvocationIds permit independent
run-now intents. Delete conflicts with all nonterminal intents. The View derives a bounded set of active/
reconcile intent ids rather than one lossy mutation flag.

The projection reducer is exactly `visible → activity_hidden|forgotten`,
`activity_hidden → visible|forgotten`, `forgotten → visible`: activity-hidden suppresses only a local unread/activity
card and retains the canonical row/View. Forgotten removes the row/View but retains a
NodeId plus `NativeJobProjectionSubject=job(NativeJobKey)|creation(NativeJobCreationId)` visibility fence;
failed/cancelled/not-created creation Nodes therefore remain manageable without a fabricated key. Sync cannot
resurrect it. Forget atomically reroutes all current subject Attention to the provisional view and serializes
with new demand creation. Later actionable evidence
extends the general `ProvisionalAttentionView` key with immutable AttentionId, owning Session/profile/target,
the tagged projection subject and observation revision; iteration/input-owner references are present only when proved. Only
`restore_native_job_projection` returns that same NodeId to visible.

`delete_native_job_local_data` is a zero-provider-effect privacy operation requiring terminal subject evidence.
It retains NodeId plus tagged key/creation fence and an installation-lifetime minimal operation-id/fingerprint/
result replay fence, sets privacy_suppressed and erases private definition/history. Automatic sync/get may then
refresh only identity/presence/deletion evidence; only explicit Restore may admit fresh content again.

`native_jobs` independently advertises the closed normalized keys `list`, `get`, `create`, `update`, `pause`,
`resume`, `run_now`, `cancel_iteration` and `delete_job`. The request table maps those keys one-to-one to
`list_native_jobs`, `get_native_job`, `create_native_job`, `update_native_job`, `pause_native_job`,
`resume_native_job`, `run_native_job_now`, `cancel_native_job_iteration` and `delete_native_job`; no cancel-job
or delete-iteration alias exists. Read operations return observations; every mutation carries operation id,
exact creation/job/iteration subject, profile/target generation and expected revisions. Dismissing Attention,
hiding/forgetting the Turn projection or ending a Session never mutates provider work. Job-iteration Attention
routes into the owning JobNodeView record/reference or exact provisional view, not a separate tree row. Portable packages carry only
inert configuration, never provider ids, active schedule or authority.

`reconcile_native_job_mutation` names the original operation id, one tagged creation/invocation/job/iteration
subject, exact MutationIntentId where applicable, expected intent/presence revisions, profile/target generations
and the proved lookup correlation receipt; it performs lookup only and never redispatches. Every provider
mutation reserves its durable intent and replay fence before effect; saturation or missing lookup correlation
refuses before dispatch. The local operations `adopt_native_job`, `cancel_native_job_creation`,
`cancel_native_job_mutation`, `hide_native_job_activity`, `forget_native_job_projection`,
`restore_native_job_projection` and `delete_native_job_local_data` are revision-fenced over the exact NodeId+
tagged projection subject and emit zero provider request.

Session/Workspace End or deletion has closed, daemon-derived
`NativeJobContainerDisposition=rehome(destination Session,optional Group)|delete_terminal_local_data|
recovery_inventory`. A surviving/nonterminal/uncertain job, intent or Attention is rehomed when its exact
destination is valid and otherwise moves with last proof and desired cleanup to the owning Workspace
SemanticRecoveryInventory;
it never refuses container removal or requires a caller-supplied plan. Rehome preserves NodeId, subject
ownership, receipts and reroutes current Attention atomically; terminal local deletion obeys the privacy
fence. No disposition emits a provider request, and late receipt/page/demand evidence always resolves through
the retained destination.

`ModelEndpointProfileId` is a non-secret stable id scoped to one ExecutionTarget. Its
`ModelEndpointProfile` record contains canonical
HTTPS origin, target/trust generation, TLS/pin policy, supported wire protocols, bounded discovered model
catalogue, provider/AccountProfile eligibility, health/freshness, revision and one non-secret credential
reference. The closed wire kind is
`CredentialReferenceKind=environment|os_keystore|target_host_agent|external_broker`; a read can reveal its
kind/name and availability, never its value.
`ModelEndpointProfileState` is closed:

```text
draft -> validating | retired | deleted
validating -> active | invalid | retired
active -> validating | degraded | retired
degraded -> validating | active | retired
invalid -> validating | retired | deleted
retired -> validating | deleted
deleted -> terminal tombstone
```

Create/update/validate/set-default/retire/delete are distinct operation-idempotent, revision-fenced local-
foreground mutations. Validation bounds redirects, response bytes/time/model count, pins network identity and
rejects non-HTTPS, userinfo, DNS rebinding, loopback/private/metadata destinations unless target policy has
explicitly adopted that exact origin. Raw credentials are write-only at the secret broker and absent from
protocol reads, argv, durable environment, logs, diagnostics and exports. Launch preflight intersects
`model_gateway`, endpoint protocol, current profile/target/account/model and credential reference. LaunchSpec
freezes requested route/model; LaunchReceipt records effective endpoint revision, wire route/model and
redacted credential-reference kind. Discovery labels are bounded untrusted data and cannot inject flags or
environment. Profile/default/health changes affect only future attempts and never cause provider, endpoint,
model, AccountProfile or local/remote fallback.
`switch_agent_configuration` is the sole in-place model/mode-route mutation. It requires the independently
current capability facts for every changed field (`model_switch` and `mode_switch`)
plus `model_gateway` where applicable, exact instance/attempt/binding/configuration/profile generations and a
fresh preflight, target/backend/durable-handle/process-start identity and generations.
`RuntimeConfigurationReceipt` is the durable operation state: it freezes operation fingerprint,
preassigned next RuntimeAttemptId, previous/requested model+mode, exact adapter/provider correlation and the
closed reducer `prepared→dispatching|refused|cancelled`, `dispatching→applied|not_applied|submitted_unconfirmed|
reconcile_required`, and `submitted_unconfirmed|reconcile_required→applied|not_applied|reconcile_required` by
lookup only. Only proof of the same conversation/process binding and effective configuration may atomically
close the old epoch and make the preassigned attempt current. Any refusal, timeout or uncertain proof leaves the
prior attempt/input authority current and visible; it never silently branches, resumes, restarts, changes
defaults or selects another route. A provider that cannot prove continuity offers an explicit Branch/new-instance
operation instead. One nonterminal receipt/instance and10,000 installation-wide share the existing≤8-KiB/item,
100,000-rich-record/512-MiB lifecycle-operation envelope; reservation,180-day rich retention, permanent replay
fence and possible-effect non-age-out are identical to `RuntimeLifecycleIntent`.

`respond_to_agent_interaction` is not permission automation. It exists only for a foreground, explicit user
action against an exact non-authorising question/decision id and current attempt. A local foreground operator
uses `submit_local_permission_response` for a recognised typed permission only under fresh supported typed transport.
The daemon refuses stale ids and adapter/
prompt mismatches, records submission as pending, and waits for provider evidence before resolving
Attention. A local operator may continue only when a fresh adapter fact explicitly proves
`response_transport=verified_local_pty(encoder_id,encoder_version,transport_generation)`, through
LocalDesktopForegroundAuthority and the shared PermissionResponseClaim/effect-armed path. Mere absence of typed
capability is insufficient. Remote bytes do not inherit that fallback: the remote InputSafetyState policy below refuses a
sensitive or unclassifiable prompt.

### Accepted multi-client, companion and remote-backend target (not in v4)

State ownership uses a closed tagged stream key, not a fictitious Workspace for global/target state:

```text
StateStreamKey = Installation(daemon_generation)
               | Workspace(daemon_generation, WorkspaceId)
               | ExecutionTarget(daemon_generation, ExecutionTargetId, target_generation)
StateWatermark = sorted[{ StateStreamKey, revision }]
```

`StateDeclarationCensus.vNext` is the independent exhaustive declaration denominator. Every state-bearing
schema/reducer block must begin with `StateFamilyDeclaration|Name`; every named projection that retains no
state must begin with `RequestValueDeclaration|Name`. The next two lines are exactly one matching
`@protocol_decl` and exactly one `@state_family` or `@request_value` classification. Orphan, duplicate,
missing or additional annotations are gate errors. The presentation manifest below is derived from and
checked against this census; it is never used to generate the census.

```text
StateDeclarationCensus.vNext = {
  StateFamilyDeclaration|AccountActivityProjection
  @protocol_decl|vNext|AccountActivityProjection
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|AccountAuthenticationIntent
  @protocol_decl|vNext|AccountAuthenticationIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AccountProfile
  @protocol_decl|vNext|AccountProfile
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|ActivityPreview
  @protocol_decl|vNext|ActivityPreview
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|AgentBrowserActionIntent
  @protocol_decl|vNext|AgentBrowserActionIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|AgentBrowserActionReceipt
  @protocol_decl|vNext|AgentBrowserActionReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|AgentBrowserControlGrant
  @protocol_decl|vNext|AgentBrowserControlGrant
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  RequestValueDeclaration|AgentBrowserReadPage
  @protocol_decl|vNext|AgentBrowserReadPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|AgentInstance
  @protocol_decl|vNext|AgentInstance
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|AgentMessageAdHocDraft
  @protocol_decl|vNext|AgentMessageAdHocDraft
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+AgentMessageId
  StateFamilyDeclaration|AgentMessageDeliveryReceipt
  @protocol_decl|vNext|AgentMessageDeliveryReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|AgentMessageDeliveryView
  @protocol_decl|vNext|AgentMessageDeliveryView
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|AgentMessageEffectIntent
  @protocol_decl|vNext|AgentMessageEffectIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|AgentMessageQueuedBody
  @protocol_decl|vNext|AgentMessageQueuedBody
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+AgentMessageDeliveryId+DestinationAgentInstanceId+DestinationAttemptGeneration
  StateFamilyDeclaration|AgentTopologyObservation
  @protocol_decl|vNext|AgentTopologyObservation
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|Announcement
  @protocol_decl|vNext|Announcement
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AnnouncementDismissal
  @protocol_decl|vNext|AnnouncementDismissal
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AnnouncementHighWater
  @protocol_decl|vNext|AnnouncementHighWater
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AttentionAudioCue
  @protocol_decl|vNext|AttentionAudioCue
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+AttentionSubject+SubjectRevision+CueGeneration
  StateFamilyDeclaration|AttentionEntry
  @protocol_decl|vNext|AttentionEntry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AttentionQueue
  @protocol_decl|vNext|AttentionQueue
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AttentionQueueOrder
  @protocol_decl|vNext|AttentionQueueOrder
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AttentionRouteMutationReceipt
  @protocol_decl|vNext|AttentionRouteMutationReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|AuxiliaryWorker
  @protocol_decl|vNext|AuxiliaryWorker
  @state_family|ephemeral|ephemeral|DaemonGeneration+AuxiliaryWorkerOwnerKey+WorkerGeneration
  StateFamilyDeclaration|BrowserDownloadQuarantine
  @protocol_decl|vNext|BrowserDownloadQuarantine
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|BrowserHistory
  @protocol_decl|vNext|BrowserHistory
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration
  StateFamilyDeclaration|BrowserLocalSnapshot
  @protocol_decl|vNext|BrowserLocalSnapshot
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+SnapshotId
  StateFamilyDeclaration|BrowserMemorySaverState
  @protocol_decl|vNext|BrowserMemorySaverState
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+PolicyRevision
  StateFamilyDeclaration|BrowserNavigationIntent
  @protocol_decl|vNext|BrowserNavigationIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|BrowserNodeCreationIntent
  @protocol_decl|vNext|BrowserNodeCreationIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|BrowserPage
  @protocol_decl|vNext|BrowserPage
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+NavigationRevision
  StateFamilyDeclaration|BrowserPartition
  @protocol_decl|vNext|BrowserPartition
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration
  StateFamilyDeclaration|BrowserRenderer
  @protocol_decl|vNext|BrowserRenderer
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+RendererGeneration
  StateFamilyDeclaration|BugReportDraft
  @protocol_decl|vNext|BugReportDraft
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+BugReportDraftId
  RequestValueDeclaration|BugReportDraftSeed
  @protocol_decl|vNext|BugReportDraftSeed
  @request_value|request_value|request|none
  StateFamilyDeclaration|BugReportReviewReceipt
  @protocol_decl|vNext|BugReportReviewReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|BulkIdleRestartInstanceReceipt
  @protocol_decl|vNext|BulkIdleRestartInstanceReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|BulkIdleRestartIntent
  @protocol_decl|vNext|BulkIdleRestartIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|BulkIdleRestartPreview
  @protocol_decl|vNext|BulkIdleRestartPreview
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+WorkspaceId+PreviewGeneration
  StateFamilyDeclaration|BulkIdleRestartReceipt
  @protocol_decl|vNext|BulkIdleRestartReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|CheckoutFence
  @protocol_decl|vNext|CheckoutFence
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CheckoutFenceRegistry
  @protocol_decl|vNext|CheckoutFenceRegistry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CheckoutLeaseHighWater
  @protocol_decl|vNext|CheckoutLeaseHighWater
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CheckoutScope
  @protocol_decl|vNext|CheckoutScope
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|CheckoutScopeBinding
  @protocol_decl|vNext|CheckoutScopeBinding
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ChunkedResponseStream
  @protocol_decl|vNext|ChunkedResponseStream
  @state_family|ephemeral|ephemeral|ConnectionGeneration+RequestId+ResponseStreamGeneration+ContentKind
  StateFamilyDeclaration|ClientAwaitingRequestRegistry
  @protocol_decl|vNext|ClientAwaitingRequestRegistry
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+ConnectionGeneration
  StateFamilyDeclaration|ClientInboundQueue
  @protocol_decl|vNext|ClientInboundQueue
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+ConnectionGeneration
  StateFamilyDeclaration|ClientOutboundIntentQueue
  @protocol_decl|vNext|ClientOutboundIntentQueue
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+ConnectionGeneration
  StateFamilyDeclaration|CommandCatalogue
  @protocol_decl|vNext|CommandCatalogue
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CommandCatalogueEntry
  @protocol_decl|vNext|CommandCatalogueEntry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CommandCatalogueScan
  @protocol_decl|vNext|CommandCatalogueScan
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+CatalogueScanId+EvaluationScopeKey+StateWatermark+CatalogueRevision
  RequestValueDeclaration|CommandSearchResult
  @protocol_decl|vNext|CommandSearchResult
  @request_value|request_value|request|none
  StateFamilyDeclaration|CommandShortcutBinding
  @protocol_decl|vNext|CommandShortcutBinding
  @state_family|durable|Installation|Installation(daemon_generation)
  RequestValueDeclaration|CommitChangedFilesPage
  @protocol_decl|vNext|CommitChangedFilesPage
  @request_value|request_value|request|none
  RequestValueDeclaration|CommitGraphPage
  @protocol_decl|vNext|CommitGraphPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|CommitProposal
  @protocol_decl|vNext|CommitProposal
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|CommitProposalAttempt
  @protocol_decl|vNext|CommitProposalAttempt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CommitProposalProviderProfile
  @protocol_decl|vNext|CommitProposalProviderProfile
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CommitProposalProviderRevision
  @protocol_decl|vNext|CommitProposalProviderRevision
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CommitProposalRevision
  @protocol_decl|vNext|CommitProposalRevision
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|CommitProposalSandboxHelper
  @protocol_decl|vNext|CommitProposalSandboxHelper
  @state_family|ephemeral|ephemeral|DaemonGeneration+CommitProposalAttemptId+AttemptGeneration+WorkerGeneration
  StateFamilyDeclaration|CompanionActionDispatchQueue
  @protocol_decl|vNext|CompanionActionDispatchQueue
  @state_family|ephemeral|ephemeral|ConnectionGeneration+RemoteClientId+RemoteSessionId
  StateFamilyDeclaration|CompanionActionIntent
  @protocol_decl|vNext|CompanionActionIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CompanionActionReceipt
  @protocol_decl|vNext|CompanionActionReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|ContainerCloseReceipt
  @protocol_decl|vNext|ContainerCloseReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|ContainerCloseSurvivorMembership
  @protocol_decl|vNext|ContainerCloseSurvivorMembership
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|CompanionAgentLaunchGrant
  @protocol_decl|vNext|CompanionAgentLaunchGrant
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|CompanionAgentLaunchIntent
  @protocol_decl|vNext|CompanionAgentLaunchIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|CompanionAgentLaunchReceipt
  @protocol_decl|vNext|CompanionAgentLaunchReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ContentProjection
  @protocol_decl|vNext|ContentProjection
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+ContentProjectionId
  StateFamilyDeclaration|ContextBrokerBearer
  @protocol_decl|vNext|ContextBrokerBearer
  @state_family|ephemeral|ephemeral|ContextLinkId+LinkGeneration+DestinationAgentInstanceId+RuntimeAttemptId+AttemptGeneration
  StateFamilyDeclaration|ContextBrokerReadBuffer
  @protocol_decl|vNext|ContextBrokerReadBuffer
  @state_family|ephemeral|ephemeral|ContextLinkId+LinkGeneration+DestinationAgentInstanceId+RuntimeAttemptId+AttemptGeneration+ReadId
  StateFamilyDeclaration|ContextLink
  @protocol_decl|vNext|ContextLink
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ContextPacketAdHocDraft
  @protocol_decl|vNext|ContextPacketAdHocDraft
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+ContextPacketId
  StateFamilyDeclaration|ContextPacketDeliveryReceipt
  @protocol_decl|vNext|ContextPacketDeliveryReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ContextPacketDeliveryView
  @protocol_decl|vNext|ContextPacketDeliveryView
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ContextPacketEffectIntent
  @protocol_decl|vNext|ContextPacketEffectIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ContextPacketLiveBody
  @protocol_decl|vNext|ContextPacketLiveBody
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+ContextPacketDeliveryId+TargetGeneration
  StateFamilyDeclaration|ContextReadAudit
  @protocol_decl|vNext|ContextReadAudit
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ContextScope
  @protocol_decl|vNext|ContextScope
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ContextUsageSnapshot
  @protocol_decl|vNext|ContextUsageSnapshot
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ConversationAdoptionReceipt
  @protocol_decl|vNext|ConversationAdoptionReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ConversationBinding
  @protocol_decl|vNext|ConversationBinding
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ConversationInventory
  @protocol_decl|vNext|ConversationInventory
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  RequestValueDeclaration|ConversationInventoryPage
  @protocol_decl|vNext|ConversationInventoryPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|ConversationInventoryQueryBuffer
  @protocol_decl|vNext|ConversationInventoryQueryBuffer
  @state_family|ephemeral|ephemeral|ConnectionGeneration+RequestId+AccountProfileId+ExecutionTargetId+TargetGeneration+ProviderNamespace+QueryGeneration
  StateFamilyDeclaration|ConversationOwnershipRegistry
  @protocol_decl|vNext|ConversationOwnershipRegistry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|ConversationProfileRebindBuffer
  @protocol_decl|vNext|ConversationProfileRebindBuffer
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+ConnectionGeneration+SurfaceId+RequestId+WorkspaceId+RuntimeEndpointBindingId+BindingGeneration
  StateFamilyDeclaration|ConversationProfileRebindReceipt
  @protocol_decl|vNext|ConversationProfileRebindReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ConversationRenameIntent
  @protocol_decl|vNext|ConversationRenameIntent
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|ConversationRenameReceipt
  @protocol_decl|vNext|ConversationRenameReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|ConversationTitleObservation
  @protocol_decl|vNext|ConversationTitleObservation
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|CorruptStoreQuarantine
  @protocol_decl|vNext|CorruptStoreQuarantine
  @state_family|durable|TaggedOwner|StoreOwnerKey
  StateFamilyDeclaration|CorruptStoreRecoveryIntent
  @protocol_decl|vNext|CorruptStoreRecoveryIntent
  @state_family|durable|TaggedOwner|StoreOwnerKey
  StateFamilyDeclaration|CorruptStoreRecoveryReceipt
  @protocol_decl|vNext|CorruptStoreRecoveryReceipt
  @state_family|durable|TaggedOwner|StoreOwnerKey
  StateFamilyDeclaration|DelegationExerciseReceipt
  @protocol_decl|vNext|DelegationExerciseReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|DelegationGrant
  @protocol_decl|vNext|DelegationGrant
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|DeliveryGrant
  @protocol_decl|vNext|DeliveryGrant
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|DependencyEdge
  @protocol_decl|vNext|DependencyEdge
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|DependencyResult
  @protocol_decl|vNext|DependencyResult
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|DiagnosticClearHighWater
  @protocol_decl|vNext|DiagnosticClearHighWater
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|DiagnosticLogClearReceipt
  @protocol_decl|vNext|DiagnosticLogClearReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RuntimeViewReplayFence
  @protocol_decl|vNext|RuntimeViewReplayFence
  @state_family|durable|Installation|Installation(daemon_generation)
  RequestValueDeclaration|DiagnosticLogPage
  @protocol_decl|vNext|DiagnosticLogPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|DiagnosticLogRing
  @protocol_decl|vNext|DiagnosticLogRing
  @state_family|ephemeral|ephemeral|DaemonGeneration+DiagnosticLogGeneration
  StateFamilyDeclaration|DictationTarget
  @protocol_decl|vNext|DictationTarget
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+CaptureGeneration
  RequestValueDeclaration|DirectoryPage
  @protocol_decl|vNext|DirectoryPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|DirectoryScan
  @protocol_decl|vNext|DirectoryScan
  @state_family|ephemeral|ephemeral|ConnectionGeneration+DirectoryScanId
  StateFamilyDeclaration|DirectoryWatch
  @protocol_decl|vNext|DirectoryWatch
  @state_family|ephemeral|ephemeral|ConnectionGeneration+DirectoryWatchId
  StateFamilyDeclaration|DisplayNameFact
  @protocol_decl|vNext|DisplayNameFact
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|DocumentBlob
  @protocol_decl|vNext|DocumentBlob
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+DocumentViewId+BlobGeneration
  StateFamilyDeclaration|DocumentDecodeWorkingSet
  @protocol_decl|vNext|DocumentDecodeWorkingSet
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+DocumentViewId+DecoderGeneration
  StateFamilyDeclaration|DocumentPageCache
  @protocol_decl|vNext|DocumentPageCache
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+DocumentViewId+BlobGeneration
  StateFamilyDeclaration|DocumentPrintIntent
  @protocol_decl|vNext|DocumentPrintIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|DocumentPrintReceipt
  @protocol_decl|vNext|DocumentPrintReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|DocumentPrintSpool
  @protocol_decl|vNext|DocumentPrintSpool
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+DocumentPrintIntentId+SpoolGeneration
  StateFamilyDeclaration|DocumentTextIndex
  @protocol_decl|vNext|DocumentTextIndex
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+DocumentViewId+BlobGeneration
  StateFamilyDeclaration|DocumentViewState
  @protocol_decl|vNext|DocumentViewState
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+DocumentViewId+ViewGeneration
  StateFamilyDeclaration|EcoHibernateIntent
  @protocol_decl|vNext|EcoHibernateIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|EcoHibernateReceipt
  @protocol_decl|vNext|EcoHibernateReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|EcoSchedulerQueue
  @protocol_decl|vNext|EcoSchedulerQueue
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+PolicyRevision+SchedulerGeneration
  StateFamilyDeclaration|ExecutionTarget
  @protocol_decl|vNext|ExecutionTarget
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|FileEditSnapshot
  @protocol_decl|vNext|FileEditSnapshot
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+FileEditSnapshotId
  StateFamilyDeclaration|FileSaveIntent
  @protocol_decl|vNext|FileSaveIntent
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|FileSaveReceipt
  @protocol_decl|vNext|FileSaveReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|FlowDefinition
  @protocol_decl|vNext|FlowDefinition
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|FlowDefinitionRevision
  @protocol_decl|vNext|FlowDefinitionRevision
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|FlowOperationReceipt
  @protocol_decl|vNext|FlowOperationReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|FlowRun
  @protocol_decl|vNext|FlowRun
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|FlowRunTrigger
  @protocol_decl|vNext|FlowRunTrigger
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|GlobalSettingsRecord
  @protocol_decl|vNext|GlobalSettingsRecord
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|GroupMembershipEdge
  @protocol_decl|vNext|GroupMembershipEdge
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|GroupTree
  @protocol_decl|vNext|GroupTree
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|HierarchyFilterBitmap
  @protocol_decl|vNext|HierarchyFilterBitmap
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+HierarchyRevision+FilterRevision
  StateFamilyDeclaration|HierarchyIndexSnapshot
  @protocol_decl|vNext|HierarchyIndexSnapshot
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+HierarchyRevision+FilterRevision+IncludeArchived
  StateFamilyDeclaration|HierarchyPage
  @protocol_decl|vNext|HierarchyPage
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+HierarchyScanId+HierarchyRevision+FilterRevision+PageOrdinal+PredecessorDigest
  StateFamilyDeclaration|HierarchyReveal
  @protocol_decl|vNext|HierarchyReveal
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+HierarchyKey+HierarchyRevision+FilterRevision
  StateFamilyDeclaration|HierarchyScan
  @protocol_decl|vNext|HierarchyScan
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+HierarchyScanId
  StateFamilyDeclaration|HistoricalConversationViewBuffer
  @protocol_decl|vNext|HistoricalConversationViewBuffer
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+RequestId+ViewTarget+ViewRevision+IndexGeneration+SourceRevision
  RequestValueDeclaration|HistoricalConversationViewPage
  @protocol_decl|vNext|HistoricalConversationViewPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|ImeComposition
  @protocol_decl|vNext|ImeComposition
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+InputTarget+CompositionGeneration
  StateFamilyDeclaration|InputLease
  @protocol_decl|vNext|InputLease
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|InputLeaseHandoffProposal
  @protocol_decl|vNext|InputLeaseHandoffProposal
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|InstallationMigrationReservation
  @protocol_decl|vNext|InstallationMigrationReservation
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|InstallationSemanticRecoveryEntry
  @protocol_decl|vNext|InstallationSemanticRecoveryEntry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|InstallationSemanticRecoveryInventory
  @protocol_decl|vNext|InstallationSemanticRecoveryInventory
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|Layout
  @protocol_decl|vNext|Layout
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|LineageEdge
  @protocol_decl|vNext|LineageEdge
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|LiveSubscription
  @protocol_decl|vNext|LiveSubscription
  @state_family|ephemeral|ephemeral|ConnectionGeneration+LiveSubscriptionSubjectKey
  StateFamilyDeclaration|LiveSubscriptionRegistry
  @protocol_decl|vNext|LiveSubscriptionRegistry
  @state_family|ephemeral|ephemeral|DaemonGeneration
  StateFamilyDeclaration|LocalInputDraft
  @protocol_decl|vNext|LocalInputDraft
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+LocalInputDraftId
  StateFamilyDeclaration|MediaBlob
  @protocol_decl|vNext|MediaBlob
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|MediaDecoder
  @protocol_decl|vNext|MediaDecoder
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+MediaPlaybackStateId+DecoderGeneration
  StateFamilyDeclaration|MediaImportIntent
  @protocol_decl|vNext|MediaImportIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|MediaImportReceipt
  @protocol_decl|vNext|MediaImportReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|MediaPlaybackState
  @protocol_decl|vNext|MediaPlaybackState
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+MediaPlaybackStateId
  StateFamilyDeclaration|MicrophoneLease
  @protocol_decl|vNext|MicrophoneLease
  @state_family|ephemeral|ephemeral|PhysicalOperatorDeviceId
  StateFamilyDeclaration|ModelDiscoveryObservation
  @protocol_decl|vNext|ModelDiscoveryObservation
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|ModelEndpointProfile
  @protocol_decl|vNext|ModelEndpointProfile
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|ModelEndpointProfileRevision
  @protocol_decl|vNext|ModelEndpointProfileRevision
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|ModelValidationReceipt
  @protocol_decl|vNext|ModelValidationReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|NameMutationReceipt
  @protocol_decl|vNext|NameMutationReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|NameProposalMetadata
  @protocol_decl|vNext|NameProposalMetadata
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|NativeDialogQueue
  @protocol_decl|vNext|NativeDialogQueue
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+WindowGeneration
  StateFamilyDeclaration|NativeJob
  @protocol_decl|vNext|NativeJob
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|NativeJobCreateIntent
  @protocol_decl|vNext|NativeJobCreateIntent
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|NativeJobInvocationReceipt
  @protocol_decl|vNext|NativeJobInvocationReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|NativeJobIteration
  @protocol_decl|vNext|NativeJobIteration
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|NativeJobMutationIntent
  @protocol_decl|vNext|NativeJobMutationIntent
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|NativeJobProjection
  @protocol_decl|vNext|NativeJobProjection
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  RequestValueDeclaration|NativeJobPage
  @protocol_decl|vNext|NativeJobPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|NativeJobPageBuffer
  @protocol_decl|vNext|NativeJobPageBuffer
  @state_family|ephemeral|ephemeral|ConnectionGeneration+RequestId+NativeJobScanId+PageGeneration
  StateFamilyDeclaration|NativeJobScan
  @protocol_decl|vNext|NativeJobScan
  @state_family|ephemeral|ephemeral|ConnectionGeneration+NativeJobScanId+AccountProfileId+ExecutionTargetId+TargetGeneration+ProviderNamespace+AdapterGeneration+SnapshotWatermark
  StateFamilyDeclaration|Node
  @protocol_decl|vNext|Node
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|NodeViewSubscription
  @protocol_decl|vNext|NodeViewSubscription
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+ViewTarget+ContentKind
  StateFamilyDeclaration|NoteRevision
  @protocol_decl|vNext|NoteRevision
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|NotificationAudit
  @protocol_decl|vNext|NotificationAudit
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|NotificationControlReceipt
  @protocol_decl|vNext|NotificationControlReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|NotificationDelivery
  @protocol_decl|vNext|NotificationDelivery
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|NotificationEndpoint
  @protocol_decl|vNext|NotificationEndpoint
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|NotificationIdentityHighWater
  @protocol_decl|vNext|NotificationIdentityHighWater
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|NotificationPairingIntent
  @protocol_decl|vNext|NotificationPairingIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|Pane
  @protocol_decl|vNext|Pane
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PaneAttachment
  @protocol_decl|vNext|PaneAttachment
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration
  StateFamilyDeclaration|PaneNodeBinding
  @protocol_decl|vNext|PaneNodeBinding
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PendingInteraction
  @protocol_decl|vNext|PendingInteraction
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PendingInteractionReceipt
  @protocol_decl|vNext|PendingInteractionReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PermissionCapabilityFact
  @protocol_decl|vNext|PermissionCapabilityFact
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|PermissionResponseClaim
  @protocol_decl|vNext|PermissionResponseClaim
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PermissionResponseReceipt
  @protocol_decl|vNext|PermissionResponseReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PermissionResponseTransportFact
  @protocol_decl|vNext|PermissionResponseTransportFact
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|PhysicalDiskLedger
  @protocol_decl|vNext|PhysicalDiskLedger
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|PortableExportIntent
  @protocol_decl|vNext|PortableExportIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PortableExportReceipt
  @protocol_decl|vNext|PortableExportReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PortableImportIntent
  @protocol_decl|vNext|PortableImportIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|PortableImportReceipt
  @protocol_decl|vNext|PortableImportReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|PresenceChatMessage
  @protocol_decl|vNext|PresenceChatMessage
  @state_family|ephemeral|ephemeral|RemoteClientId+RemoteSessionId+WorkspaceId+SurfaceId+ConnectionGeneration+ViewTarget+ViewRevision+MessageGeneration
  StateFamilyDeclaration|PresentationHistory
  @protocol_decl|vNext|PresentationHistory
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|PrivateTranscriptSearchIndex
  @protocol_decl|vNext|PrivateTranscriptSearchIndex
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|PrivateTranscriptSearchIndexReceipt
  @protocol_decl|vNext|PrivateTranscriptSearchIndexReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  RequestValueDeclaration|PrivateTranscriptSearchPage
  @protocol_decl|vNext|PrivateTranscriptSearchPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|PrivateTranscriptSearchQueryBuffer
  @protocol_decl|vNext|PrivateTranscriptSearchQueryBuffer
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+RequestId+ExecutionTargetId+TargetGeneration+AccountProfileId+ProviderNamespace+IndexGeneration+QueryGeneration
  StateFamilyDeclaration|PrivateTranscriptSearchRefreshQueue
  @protocol_decl|vNext|PrivateTranscriptSearchRefreshQueue
  @state_family|ephemeral|ephemeral|DaemonGeneration+ExecutionTargetId+TargetGeneration+AccountProfileId+ProviderNamespace+IndexGeneration
  StateFamilyDeclaration|ProcessCleanupCharge
  @protocol_decl|vNext|ProcessCleanupCharge
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|ProcessEdge
  @protocol_decl|vNext|ProcessEdge
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ProgressUpdate
  @protocol_decl|vNext|ProgressUpdate
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ProtocolConnectionOutbox
  @protocol_decl|vNext|ProtocolConnectionOutbox
  @state_family|ephemeral|ephemeral|ConnectionGeneration
  StateFamilyDeclaration|ProviderProfileObservation
  @protocol_decl|vNext|ProviderProfileObservation
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|PtyCapacityMonitor
  @protocol_decl|vNext|PtyCapacityMonitor
  @state_family|ephemeral|ephemeral|DaemonGeneration+ExecutionTargetId+TargetGeneration+MonitorGeneration
  StateFamilyDeclaration|PtyCapacityObservation
  @protocol_decl|vNext|PtyCapacityObservation
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|PtyCapacityRemediationIntent
  @protocol_decl|vNext|PtyCapacityRemediationIntent
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|PtyCapacityRemediationReceipt
  @protocol_decl|vNext|PtyCapacityRemediationReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|QuotaScope
  @protocol_decl|vNext|QuotaScope
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|QuotaSnapshot
  @protocol_decl|vNext|QuotaSnapshot
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RemoteCleanupTombstone
  @protocol_decl|vNext|RemoteCleanupTombstone
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemoteClient
  @protocol_decl|vNext|RemoteClient
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemoteInvitation
  @protocol_decl|vNext|RemoteInvitation
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemotePermissionGrantIssueFence
  @protocol_decl|vNext|RemotePermissionGrantIssueFence
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemotePermissionResponseGrant
  @protocol_decl|vNext|RemotePermissionResponseGrant
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemotePresence
  @protocol_decl|vNext|RemotePresence
  @state_family|ephemeral|ephemeral|RemoteClientId+RemoteSessionId+WorkspaceId+SurfaceId+ConnectionGeneration
  StateFamilyDeclaration|RemoteRedemptionReceipt
  @protocol_decl|vNext|RemoteRedemptionReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemoteReplayFence
  @protocol_decl|vNext|RemoteReplayFence
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemoteReplayNonce
  @protocol_decl|vNext|RemoteReplayNonce
  @state_family|ephemeral|ephemeral|RemoteClientId+RemoteSessionId+ConnectionGeneration+NonceHash
  StateFamilyDeclaration|RemoteSession
  @protocol_decl|vNext|RemoteSession
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RemoteSessionOpenReceipt
  @protocol_decl|vNext|RemoteSessionOpenReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RepositoryBackendHandle
  @protocol_decl|vNext|RepositoryBackendHandle
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RepositoryHostCapabilityGrant
  @protocol_decl|vNext|RepositoryHostCapabilityGrant
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RepositoryHostCredentialIntent
  @protocol_decl|vNext|RepositoryHostCredentialIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RepositoryHostProfile
  @protocol_decl|vNext|RepositoryHostProfile
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|RepositoryMutationIntent
  @protocol_decl|vNext|RepositoryMutationIntent
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RepositoryMutationReceipt
  @protocol_decl|vNext|RepositoryMutationReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RepositoryPublishIntent
  @protocol_decl|vNext|RepositoryPublishIntent
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RepositoryPublishReceipt
  @protocol_decl|vNext|RepositoryPublishReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|Resource
  @protocol_decl|vNext|Resource
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|ResourceInventory
  @protocol_decl|vNext|ResourceInventory
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|ResourceRevision
  @protocol_decl|vNext|ResourceRevision
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeAttemptDetail
  @protocol_decl|vNext|RuntimeAttemptDetail
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeConfigurationReceipt
  @protocol_decl|vNext|RuntimeConfigurationReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeEndpoint
  @protocol_decl|vNext|RuntimeEndpoint
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RuntimeEndpointBinding
  @protocol_decl|vNext|RuntimeEndpointBinding
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RuntimeEndpointContinuityReceipt
  @protocol_decl|vNext|RuntimeEndpointContinuityReceipt
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RuntimeEndpointContinuityVerificationBuffer
  @protocol_decl|vNext|RuntimeEndpointContinuityVerificationBuffer
  @state_family|ephemeral|ephemeral|EndpointBrokerConnectionGeneration+OperationId+ExecutionTargetId+TargetGeneration+RuntimeEndpointId+PriorEndpointGeneration+CandidateEndpointGeneration+ProofDigest
  StateFamilyDeclaration|RuntimeInputReceipt
  @protocol_decl|vNext|RuntimeInputReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeInterruptReceipt
  @protocol_decl|vNext|RuntimeInterruptReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeInventory
  @protocol_decl|vNext|RuntimeInventory
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|RuntimeLaunchIntent
  @protocol_decl|vNext|RuntimeLaunchIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeLaunchReceipt
  @protocol_decl|vNext|RuntimeLaunchReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeAttachmentReceipt
  @protocol_decl|vNext|RuntimeAttachmentReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeLifecycleIntent
  @protocol_decl|vNext|RuntimeLifecycleIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|RuntimeLifecycleReceipt
  @protocol_decl|vNext|RuntimeLifecycleReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|Session
  @protocol_decl|vNext|Session
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|SessionActivationReceipt
  @protocol_decl|vNext|SessionActivationReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|SessionSettingsRecord
  @protocol_decl|vNext|SessionSettingsRecord
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  RequestValueDeclaration|RecoveryInventoryPage
  @protocol_decl|vNext|RecoveryInventoryPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|SettingsMutationReceipt
  @protocol_decl|vNext|SettingsMutationReceipt
  @state_family|durable|TaggedOwner|SettingsOwnerKey
  RequestValueDeclaration|SettingsRegistryPage
  @protocol_decl|vNext|SettingsRegistryPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|SettingsResetPreview
  @protocol_decl|vNext|SettingsResetPreview
  @state_family|ephemeral|ephemeral|ConnectionGeneration+LocalClientInstanceId+SurfaceId+SettingsResetPreviewId
  RequestValueDeclaration|SettingsSearchPage
  @protocol_decl|vNext|SettingsSearchPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|SigningAudienceHighWater
  @protocol_decl|vNext|SigningAudienceHighWater
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SigningTrustStore
  @protocol_decl|vNext|SigningTrustStore
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SpawnEdge
  @protocol_decl|vNext|SpawnEdge
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|SpeechModelArtifact
  @protocol_decl|vNext|SpeechModelArtifact
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SpeechModelCatalogueEntry
  @protocol_decl|vNext|SpeechModelCatalogueEntry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SpeechModelInstallIntent
  @protocol_decl|vNext|SpeechModelInstallIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SpeechModelInstallPartial
  @protocol_decl|vNext|SpeechModelInstallPartial
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SpeechModelInstallReceipt
  @protocol_decl|vNext|SpeechModelInstallReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SpeechWorker
  @protocol_decl|vNext|SpeechWorker
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+DeviceId+WorkerGeneration
  StateFamilyDeclaration|StateStreamSubscription
  @protocol_decl|vNext|StateStreamSubscription
  @state_family|ephemeral|ephemeral|ConnectionGeneration+StateStreamKey
  StateFamilyDeclaration|StatusEvent
  @protocol_decl|vNext|StatusEvent
  @state_family|durable|TaggedOwner|StateStreamKey
  StateFamilyDeclaration|StepAttempt
  @protocol_decl|vNext|StepAttempt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|Surface
  @protocol_decl|vNext|Surface
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SurfaceConnectionBinding
  @protocol_decl|vNext|SurfaceConnectionBinding
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId
  StateFamilyDeclaration|SurfaceHistoryIndex
  @protocol_decl|vNext|SurfaceHistoryIndex
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SurfaceOwnerHighWater
  @protocol_decl|vNext|SurfaceOwnerHighWater
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|SurfaceRegistry
  @protocol_decl|vNext|SurfaceRegistry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|TargetConnectivityState
  @protocol_decl|vNext|TargetConnectivityState
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|TargetIndependentPolicy
  @protocol_decl|vNext|TargetIndependentPolicy
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|TargetRuntimeRecoveryEntry
  @protocol_decl|vNext|TargetRuntimeRecoveryEntry
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|TargetRuntimeRecoveryInventory
  @protocol_decl|vNext|TargetRuntimeRecoveryInventory
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|TargetTrustState
  @protocol_decl|vNext|TargetTrustState
  @state_family|durable|ExecutionTarget|ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)
  StateFamilyDeclaration|Team
  @protocol_decl|vNext|Team
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|TeamMembershipEdge
  @protocol_decl|vNext|TeamMembershipEdge
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|TeamRevision
  @protocol_decl|vNext|TeamRevision
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|Template
  @protocol_decl|vNext|Template
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|TemplateSettingsRecord
  @protocol_decl|vNext|TemplateSettingsRecord
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|TemporaryPane
  @protocol_decl|vNext|TemporaryPane
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+PaneId
  StateFamilyDeclaration|TemporarySettingsRecord
  @protocol_decl|vNext|TemporarySettingsRecord
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId
  StateFamilyDeclaration|TerminalBackgroundWriteChannel
  @protocol_decl|vNext|TerminalBackgroundWriteChannel
  @state_family|ephemeral|ephemeral|DaemonGeneration+ExecutionTargetId+TargetGeneration+RuntimeBackendId+DurableSessionHandle+HandleGeneration+ChannelGeneration
  StateFamilyDeclaration|TerminalByteRing
  @protocol_decl|vNext|TerminalByteRing
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration
  StateFamilyDeclaration|TerminalClipboardGesture
  @protocol_decl|vNext|TerminalClipboardGesture
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+GestureGeneration
  StateFamilyDeclaration|TerminalHistoryMetadata
  @protocol_decl|vNext|TerminalHistoryMetadata
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|TerminalImageChunkAssembly
  @protocol_decl|vNext|TerminalImageChunkAssembly
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageSequenceGeneration
  StateFamilyDeclaration|TerminalImageClientCache
  @protocol_decl|vNext|TerminalImageClientCache
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration
  StateFamilyDeclaration|TerminalImageDecodeWorkingSet
  @protocol_decl|vNext|TerminalImageDecodeWorkingSet
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageSequenceGeneration
  StateFamilyDeclaration|TerminalImageFetch
  @protocol_decl|vNext|TerminalImageFetch
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageId+FetchGeneration
  StateFamilyDeclaration|TerminalImageScanBuffer
  @protocol_decl|vNext|TerminalImageScanBuffer
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageSequenceGeneration
  StateFamilyDeclaration|TerminalImageStore
  @protocol_decl|vNext|TerminalImageStore
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration
  StateFamilyDeclaration|TerminalOffscreenClientDetach
  @protocol_decl|vNext|TerminalOffscreenClientDetach
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+OffscreenGeneration
  StateFamilyDeclaration|TerminalOutputQueue
  @protocol_decl|vNext|TerminalOutputQueue
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+OutputQueueGeneration
  StateFamilyDeclaration|TerminalPumpBatch
  @protocol_decl|vNext|TerminalPumpBatch
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+BatchGeneration
  StateFamilyDeclaration|TerminalRuntimeState
  @protocol_decl|vNext|TerminalRuntimeState
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration
  StateFamilyDeclaration|TerminalScreen
  @protocol_decl|vNext|TerminalScreen
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration
  StateFamilyDeclaration|TerminalScreenProjectionBaseline
  @protocol_decl|vNext|TerminalScreenProjectionBaseline
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+BaselineGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration
  StateFamilyDeclaration|TerminalShadowObserver
  @protocol_decl|vNext|TerminalShadowObserver
  @state_family|ephemeral|ephemeral|DaemonGeneration+ExecutionTargetId+TargetGeneration+RuntimeBackendId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+DurableSessionHandle+HandleGeneration+ShadowGeneration
  StateFamilyDeclaration|TerminalWakeInputBuffer
  @protocol_decl|vNext|TerminalWakeInputBuffer
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+SessionId+PaneId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+InputLeaseId+InputLeaseGeneration+WakeGeneration
  StateFamilyDeclaration|TerminalWarmViewPark
  @protocol_decl|vNext|TerminalWarmViewPark
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+SessionId+PaneId+ViewTarget+WarmParkGeneration
  RequestValueDeclaration|TextSearchResultPage
  @protocol_decl|vNext|TextSearchResultPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|TextSearchSession
  @protocol_decl|vNext|TextSearchSession
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+TextSearchSessionId
  StateFamilyDeclaration|TopologyObservationQueue
  @protocol_decl|vNext|TopologyObservationQueue
  @state_family|ephemeral|ephemeral|DaemonGeneration+WorkspaceId+SourceId+ObservationEpoch
  StateFamilyDeclaration|TransferTicket
  @protocol_decl|vNext|TransferTicket
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|TreeSurfaceState
  @protocol_decl|vNext|TreeSurfaceState
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|UpdateIntent
  @protocol_decl|vNext|UpdateIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|VoiceHypothesis
  @protocol_decl|vNext|VoiceHypothesis
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+WorkerGeneration+CaptureGeneration
  StateFamilyDeclaration|VoicePcmBuffer
  @protocol_decl|vNext|VoicePcmBuffer
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+DeviceId+CaptureGeneration
  StateFamilyDeclaration|VoiceTranscriptDraft
  @protocol_decl|vNext|VoiceTranscriptDraft
  @state_family|ephemeral|ephemeral|LocalClientInstanceId+SurfaceId+VoiceDraftId
  StateFamilyDeclaration|WebPreviewBody
  @protocol_decl|vNext|WebPreviewBody
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+WebPreviewLoadStateId
  StateFamilyDeclaration|WebPreviewFetchCorrelation
  @protocol_decl|vNext|WebPreviewFetchCorrelation
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+WebPreviewLoadIntentId
  StateFamilyDeclaration|WebPreviewLoadIntent
  @protocol_decl|vNext|WebPreviewLoadIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WebPreviewLoadState
  @protocol_decl|vNext|WebPreviewLoadState
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+WebPreviewLoadStateId
  StateFamilyDeclaration|WebPreviewRenderer
  @protocol_decl|vNext|WebPreviewRenderer
  @state_family|ephemeral|ephemeral|ConnectionGeneration+SurfaceId+WebPreviewLoadStateId+RendererGeneration
  StateFamilyDeclaration|WorkItemActivity
  @protocol_decl|vNext|WorkItemActivity
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  RequestValueDeclaration|WorkItemActivityPage
  @protocol_decl|vNext|WorkItemActivityPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|WorkItemBinding
  @protocol_decl|vNext|WorkItemBinding
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkItemConflict
  @protocol_decl|vNext|WorkItemConflict
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkItemCreateIntent
  @protocol_decl|vNext|WorkItemCreateIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkItemKeyRegistry
  @protocol_decl|vNext|WorkItemKeyRegistry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|WorkItemKeyRegistryEntry
  @protocol_decl|vNext|WorkItemKeyRegistryEntry
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|WorkItemMutationIntent
  @protocol_decl|vNext|WorkItemMutationIntent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkItemMutationReceipt
  @protocol_decl|vNext|WorkItemMutationReceipt
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  RequestValueDeclaration|WorkItemPage
  @protocol_decl|vNext|WorkItemPage
  @request_value|request_value|request|none
  StateFamilyDeclaration|WorkItemProjection
  @protocol_decl|vNext|WorkItemProjection
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkItemSource
  @protocol_decl|vNext|WorkItemSource
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|WorkItemSourceQueryBuffer
  @protocol_decl|vNext|WorkItemSourceQueryBuffer
  @state_family|ephemeral|ephemeral|ConnectionGeneration+RequestId+WorkItemSourceId+SourceGeneration+QueryGeneration
  StateFamilyDeclaration|WorkItemSyncRun
  @protocol_decl|vNext|WorkItemSyncRun
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|Workspace
  @protocol_decl|vNext|Workspace
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkspaceEvent
  @protocol_decl|vNext|WorkspaceEvent
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkspaceOnboardingIntent
  @protocol_decl|vNext|WorkspaceOnboardingIntent
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|WorkspaceOnboardingReceipt
  @protocol_decl|vNext|WorkspaceOnboardingReceipt
  @state_family|durable|Installation|Installation(daemon_generation)
  StateFamilyDeclaration|WorkspaceSemanticRecoveryEntry
  @protocol_decl|vNext|WorkspaceSemanticRecoveryEntry
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkspaceSemanticRecoveryInventory
  @protocol_decl|vNext|WorkspaceSemanticRecoveryInventory
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkspaceSemanticReservation
  @protocol_decl|vNext|WorkspaceSemanticReservation
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkspaceSettingsRecord
  @protocol_decl|vNext|WorkspaceSettingsRecord
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkspaceTargetBinding
  @protocol_decl|vNext|WorkspaceTargetBinding
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
  StateFamilyDeclaration|WorkspaceWriteLease
  @protocol_decl|vNext|WorkspaceWriteLease
  @state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)
}
```

`StateFamilyManifest.vNext` below is the sole normative closed ownership presentation. Its denominator is the
independent exhaustive `StateDeclarationCensus.vNext`, not this block. Every `StateFamilyDeclaration` has
exactly one matching `@protocol_decl` plus `@state_family`; every stateless named request/response projection has
exactly one `RequestValueDeclaration`, matching `@protocol_decl` and `@request_value`. An unannotated,
unclassified, orphaned or multiply classified declaration is a gate error, and there is no default owner,
nesting inference, wildcard or "miscellaneous" family. Request values survive no request and retain no authority,
bytes or identity. Every other product document incorporates the resulting manifest by reference and cannot add,
omit or reassign a family.

`docs/STATE_FAMILY_MANIFEST_VNEXT.tsv` is emitted only from the declaration census. The verifier separately
parses and compares declaration census↔presentation and declaration census↔TSV by name, declaration class,
lifetime, stream owner and exact owner key, then checks the independently reviewed 345-row/328-family/
17-request-value count and canonical digest. Its mutation suite proves missing annotations, missing or duplicate
classifications, new unannotated reducers, presentation/TSV drift and coordinated deletion or coordinate changes.
Deleting or changing declaration, presentation and TSV together still fails the oracle. When vNext schemas
become compiled sources, typed attributes replace this prose census as declaration input; the presentation block
never becomes that input.

```text
StateFamilyManifest.vNext = {
  durable.Installation = {
    SurfaceRegistry, Surface, TreeSurfaceState, SurfaceOwnerHighWater, SurfaceHistoryIndex, PhysicalDiskLedger,
    Template, GlobalSettingsRecord, TemplateSettingsRecord,
    ExecutionTarget, AccountProfile, AccountAuthenticationIntent,
    CommandCatalogue, CommandCatalogueEntry, CommandShortcutBinding,
    AttentionQueue, AttentionEntry, AttentionQueueOrder, AttentionRouteMutationReceipt,
    WorkItemSource, WorkItemKeyRegistry, WorkItemKeyRegistryEntry, WorkItemSyncRun,
    RepositoryHostProfile, RepositoryHostCredentialIntent, RepositoryHostCapabilityGrant,
    Announcement, AnnouncementDismissal, AnnouncementHighWater,
    UpdateIntent,
    SigningTrustStore, SigningAudienceHighWater,
    SpeechModelCatalogueEntry, SpeechModelArtifact, SpeechModelInstallIntent,
    SpeechModelInstallReceipt, SpeechModelInstallPartial,
    InstallationSemanticRecoveryInventory, InstallationSemanticRecoveryEntry, InstallationMigrationReservation,
    CommitProposalProviderProfile, CommitProposalProviderRevision, CommitProposalAttempt,
    WorkspaceOnboardingIntent, WorkspaceOnboardingReceipt,
    PortableImportIntent, PortableImportReceipt,
    NotificationEndpoint, NotificationPairingIntent, DeliveryGrant,
    NotificationControlReceipt, NotificationDelivery, NotificationAudit,
    NotificationIdentityHighWater,
    CheckoutFenceRegistry, CheckoutFence, CheckoutLeaseHighWater,
    RemoteInvitation, RemoteClient, RemoteSession, RemoteRedemptionReceipt,
    RemoteSessionOpenReceipt, RemoteReplayFence,
    CompanionActionIntent, CompanionActionReceipt,
    RemotePermissionResponseGrant, RemotePermissionGrantIssueFence,
    TargetIndependentPolicy, ConversationOwnershipRegistry,
    RemoteCleanupTombstone, ProcessCleanupCharge, ContainerCloseReceipt, ContainerCloseSurvivorMembership,
    RuntimeViewReplayFence,
    DiagnosticClearHighWater, DiagnosticLogClearReceipt, BugReportReviewReceipt
  },
  durable.Workspace = {
    Workspace, WorkspaceSettingsRecord, Session, SessionSettingsRecord,
    Node, NoteRevision, AgentInstance, NativeJobProjection, RuntimeAttemptDetail,
    RuntimeLaunchIntent, RuntimeLaunchReceipt, RuntimeAttachmentReceipt, RuntimeConfigurationReceipt,
    RuntimeLifecycleIntent, RuntimeLifecycleReceipt,
    SessionActivationReceipt, RuntimeInputReceipt, RuntimeInterruptReceipt,
    CompanionAgentLaunchGrant, CompanionAgentLaunchIntent, CompanionAgentLaunchReceipt,
    BulkIdleRestartIntent, BulkIdleRestartReceipt, BulkIdleRestartInstanceReceipt,
    EcoHibernateIntent, EcoHibernateReceipt,
    Pane, Layout, PaneNodeBinding, TerminalHistoryMetadata,
    SpawnEdge, ProcessEdge, GroupMembershipEdge, TeamMembershipEdge,
    DependencyEdge, DependencyResult, LineageEdge, GroupTree, CheckoutScope, CheckoutScopeBinding,
    DisplayNameFact, NameProposalMetadata, NameMutationReceipt,
    Team, TeamRevision, DelegationGrant, DelegationExerciseReceipt,
    FlowDefinition, FlowDefinitionRevision, FlowRun, FlowRunTrigger,
    StepAttempt, FlowOperationReceipt,
    Resource, ResourceRevision, ProgressUpdate,
    ContextScope, ContextUsageSnapshot, ContextLink, ContextReadAudit, ContextPacketDeliveryView,
    ContextPacketEffectIntent, ContextPacketDeliveryReceipt,
    AgentMessageDeliveryView, AgentMessageEffectIntent, AgentMessageDeliveryReceipt,
    WorkItemProjection, WorkItemActivity, WorkItemBinding, WorkItemConflict,
    WorkItemCreateIntent, WorkItemMutationIntent, WorkItemMutationReceipt,
    MediaImportIntent, MediaImportReceipt, MediaBlob,
    DocumentPrintIntent, DocumentPrintReceipt,
    CommitProposal, CommitProposalRevision,
    WebPreviewLoadIntent, BrowserNodeCreationIntent, BrowserNavigationIntent,
    BrowserDownloadQuarantine, TransferTicket,
    AgentBrowserControlGrant, AgentBrowserActionIntent, AgentBrowserActionReceipt,
    PortableExportIntent, PortableExportReceipt,
    WorkspaceSemanticRecoveryInventory, WorkspaceSemanticRecoveryEntry, WorkspaceSemanticReservation,
    PresentationHistory, PendingInteraction, PendingInteractionReceipt,
    PermissionResponseClaim, PermissionResponseReceipt,
    InputLease, InputLeaseHandoffProposal, WorkspaceWriteLease,
    WorkspaceEvent, ActivityPreview, AgentTopologyObservation,
    WorkspaceTargetBinding, ConversationBinding, ConversationAdoptionReceipt, ConversationProfileRebindReceipt
  },
  durable.ExecutionTarget = {
    TargetConnectivityState, TargetTrustState,
    RuntimeInventory, ResourceInventory, PtyCapacityObservation,
    PtyCapacityRemediationIntent, PtyCapacityRemediationReceipt,
    TargetRuntimeRecoveryInventory, TargetRuntimeRecoveryEntry,
    NativeJob, NativeJobIteration, NativeJobCreateIntent, NativeJobMutationIntent,
    NativeJobInvocationReceipt,
    RuntimeEndpoint, RuntimeEndpointBinding, RuntimeEndpointContinuityReceipt,
    RepositoryBackendHandle, FileSaveIntent, FileSaveReceipt,
    RepositoryMutationIntent, RepositoryMutationReceipt,
    RepositoryPublishIntent, RepositoryPublishReceipt,
    ModelEndpointProfile, ModelEndpointProfileRevision,
    ModelDiscoveryObservation, ModelValidationReceipt,
    ConversationInventory, ConversationTitleObservation, ConversationRenameIntent,
    ConversationRenameReceipt, PrivateTranscriptSearchIndex, PrivateTranscriptSearchIndexReceipt,
    AccountActivityProjection,
    QuotaScope, QuotaSnapshot,
    PermissionCapabilityFact, PermissionResponseTransportFact,
    ProviderProfileObservation
  },
  durable.TaggedOwner = {
    StatusEvent(owner_key=StateStreamKey),
    SettingsMutationReceipt(owner_key=SettingsOwnerKey),
    CorruptStoreQuarantine(owner_key=StoreOwnerKey),
    CorruptStoreRecoveryIntent(owner_key=StoreOwnerKey),
    CorruptStoreRecoveryReceipt(owner_key=StoreOwnerKey)
  },
  ephemeral = {
    LiveSubscriptionRegistry(owner_key=DaemonGeneration),
    SurfaceConnectionBinding(owner_key=ConnectionGeneration+SurfaceId),
    TemporarySettingsRecord(owner_key=LocalClientInstanceId+SurfaceId),
    TemporaryPane(owner_key=ConnectionGeneration+SurfaceId+PaneId),
    StateStreamSubscription(owner_key=ConnectionGeneration+StateStreamKey),
    LiveSubscription(owner_key=ConnectionGeneration+LiveSubscriptionSubjectKey),
    NodeViewSubscription(owner_key=ConnectionGeneration+SurfaceId+ViewTarget+ContentKind),
    PaneAttachment(owner_key=ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration),
    TerminalScreenProjectionBaseline(owner_key=ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+BaselineGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration),
    TerminalRuntimeState(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration),
    TerminalByteRing(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration),
    TerminalScreen(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration),
    TerminalOutputQueue(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+OutputQueueGeneration),
    TerminalPumpBatch(owner_key=ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+BatchGeneration),
    TerminalWarmViewPark(owner_key=LocalClientInstanceId+SurfaceId+SessionId+PaneId+ViewTarget+WarmParkGeneration),
    TerminalOffscreenClientDetach(owner_key=LocalClientInstanceId+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+OffscreenGeneration),
    TerminalWakeInputBuffer(owner_key=LocalClientInstanceId+SurfaceId+SessionId+PaneId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+InputLeaseId+InputLeaseGeneration+WakeGeneration),
    TerminalShadowObserver(owner_key=DaemonGeneration+ExecutionTargetId+TargetGeneration+RuntimeBackendId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+DurableSessionHandle+HandleGeneration+ShadowGeneration),
    TerminalBackgroundWriteChannel(owner_key=DaemonGeneration+ExecutionTargetId+TargetGeneration+RuntimeBackendId+DurableSessionHandle+HandleGeneration+ChannelGeneration),
    TerminalImageStore(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration),
    TerminalImageScanBuffer(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageSequenceGeneration),
    TerminalImageChunkAssembly(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageSequenceGeneration),
    TerminalImageDecodeWorkingSet(owner_key=DaemonGeneration+WorkspaceId+SessionId+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageSequenceGeneration),
    TerminalImageFetch(owner_key=ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration+ImageId+FetchGeneration),
    TerminalImageClientCache(owner_key=ConnectionGeneration+SurfaceId+SessionId+PaneId+PaneAttachmentId+AttachmentGeneration+AttemptOwner+RuntimeAttemptId+AttemptGeneration+PtyGeneration+BufferGeneration),
    ProtocolConnectionOutbox(owner_key=ConnectionGeneration),
    ClientInboundQueue(owner_key=LocalClientInstanceId+ConnectionGeneration),
    ClientOutboundIntentQueue(owner_key=LocalClientInstanceId+ConnectionGeneration),
    ClientAwaitingRequestRegistry(owner_key=LocalClientInstanceId+ConnectionGeneration),
    NativeDialogQueue(owner_key=LocalClientInstanceId+WindowGeneration),
    CompanionActionDispatchQueue(owner_key=ConnectionGeneration+RemoteClientId+RemoteSessionId),
    TopologyObservationQueue(owner_key=DaemonGeneration+WorkspaceId+SourceId+ObservationEpoch),
    DiagnosticLogRing(owner_key=DaemonGeneration+DiagnosticLogGeneration),
    SettingsResetPreview(owner_key=ConnectionGeneration+LocalClientInstanceId+SurfaceId+SettingsResetPreviewId),
    BugReportDraft(owner_key=LocalClientInstanceId+SurfaceId+BugReportDraftId),
    BulkIdleRestartPreview(owner_key=ConnectionGeneration+SurfaceId+WorkspaceId+PreviewGeneration),
    EcoSchedulerQueue(owner_key=DaemonGeneration+WorkspaceId+PolicyRevision+SchedulerGeneration),
    PtyCapacityMonitor(owner_key=DaemonGeneration+ExecutionTargetId+TargetGeneration+MonitorGeneration),
    TerminalClipboardGesture(owner_key=LocalClientInstanceId+SurfaceId+GestureGeneration),
    AttentionAudioCue(owner_key=LocalClientInstanceId+AttentionSubject+SubjectRevision+CueGeneration),
    ChunkedResponseStream(owner_key=ConnectionGeneration+RequestId+ResponseStreamGeneration+ContentKind),
    DirectoryScan(owner_key=ConnectionGeneration+DirectoryScanId),
    DirectoryWatch(owner_key=ConnectionGeneration+DirectoryWatchId),
    FileEditSnapshot(owner_key=ConnectionGeneration+SurfaceId+FileEditSnapshotId),
    TextSearchSession(owner_key=ConnectionGeneration+SurfaceId+TextSearchSessionId),
    ContentProjection(owner_key=ConnectionGeneration+SurfaceId+ContentProjectionId),
    CommandCatalogueScan(owner_key=ConnectionGeneration+SurfaceId+CatalogueScanId+EvaluationScopeKey+StateWatermark+CatalogueRevision),
    HierarchyIndexSnapshot(owner_key=ConnectionGeneration+SurfaceId+HierarchyRevision+FilterRevision+IncludeArchived),
    HierarchyPage(owner_key=ConnectionGeneration+SurfaceId+HierarchyScanId+HierarchyRevision+FilterRevision+PageOrdinal+PredecessorDigest),
    HierarchyScan(owner_key=ConnectionGeneration+SurfaceId+HierarchyScanId),
    HierarchyReveal(owner_key=ConnectionGeneration+SurfaceId+HierarchyKey+HierarchyRevision+FilterRevision),
    HierarchyFilterBitmap(owner_key=ConnectionGeneration+SurfaceId+HierarchyRevision+FilterRevision),
    WebPreviewLoadState(owner_key=ConnectionGeneration+SurfaceId+WebPreviewLoadStateId),
    WebPreviewBody(owner_key=ConnectionGeneration+SurfaceId+WebPreviewLoadStateId),
    WebPreviewRenderer(owner_key=ConnectionGeneration+SurfaceId+WebPreviewLoadStateId+RendererGeneration),
    WebPreviewFetchCorrelation(owner_key=ConnectionGeneration+SurfaceId+WebPreviewLoadIntentId),
    BrowserLocalSnapshot(owner_key=DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+SnapshotId),
    BrowserRenderer(owner_key=DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+RendererGeneration),
    BrowserPartition(owner_key=DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration),
    BrowserPage(owner_key=DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+NavigationRevision),
    BrowserHistory(owner_key=DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration),
    BrowserMemorySaverState(owner_key=DaemonGeneration+WorkspaceId+BrowserNodeId+PartitionId+PartitionGeneration+PolicyRevision),
    DocumentViewState(owner_key=ConnectionGeneration+SurfaceId+DocumentViewId+ViewGeneration),
    DocumentBlob(owner_key=ConnectionGeneration+SurfaceId+DocumentViewId+BlobGeneration),
    DocumentDecodeWorkingSet(owner_key=ConnectionGeneration+SurfaceId+DocumentViewId+DecoderGeneration),
    DocumentPageCache(owner_key=ConnectionGeneration+SurfaceId+DocumentViewId+BlobGeneration),
    DocumentTextIndex(owner_key=ConnectionGeneration+SurfaceId+DocumentViewId+BlobGeneration),
    DocumentPrintSpool(owner_key=DaemonGeneration+WorkspaceId+DocumentPrintIntentId+SpoolGeneration),
    MediaPlaybackState(owner_key=ConnectionGeneration+SurfaceId+MediaPlaybackStateId),
    MediaDecoder(owner_key=ConnectionGeneration+SurfaceId+MediaPlaybackStateId+DecoderGeneration),
    RemotePresence(owner_key=RemoteClientId+RemoteSessionId+WorkspaceId+SurfaceId+ConnectionGeneration),
    PresenceChatMessage(owner_key=RemoteClientId+RemoteSessionId+WorkspaceId+SurfaceId+ConnectionGeneration+ViewTarget+ViewRevision+MessageGeneration),
    RemoteReplayNonce(owner_key=RemoteClientId+RemoteSessionId+ConnectionGeneration+NonceHash),
    ContextBrokerBearer(owner_key=ContextLinkId+LinkGeneration+DestinationAgentInstanceId+RuntimeAttemptId+AttemptGeneration),
    ContextBrokerReadBuffer(owner_key=ContextLinkId+LinkGeneration+DestinationAgentInstanceId+RuntimeAttemptId+AttemptGeneration+ReadId),
    ContextPacketAdHocDraft(owner_key=ConnectionGeneration+SurfaceId+ContextPacketId),
    ContextPacketLiveBody(owner_key=DaemonGeneration+WorkspaceId+ContextPacketDeliveryId+TargetGeneration),
    AgentMessageAdHocDraft(owner_key=ConnectionGeneration+SurfaceId+AgentMessageId),
    AgentMessageQueuedBody(owner_key=DaemonGeneration+WorkspaceId+AgentMessageDeliveryId+DestinationAgentInstanceId+DestinationAttemptGeneration),
    WorkItemSourceQueryBuffer(owner_key=ConnectionGeneration+RequestId+WorkItemSourceId+SourceGeneration+QueryGeneration),
    ConversationInventoryQueryBuffer(owner_key=ConnectionGeneration+RequestId+AccountProfileId+ExecutionTargetId+TargetGeneration+ProviderNamespace+QueryGeneration),
    PrivateTranscriptSearchRefreshQueue(owner_key=DaemonGeneration+ExecutionTargetId+TargetGeneration+AccountProfileId+ProviderNamespace+IndexGeneration),
    PrivateTranscriptSearchQueryBuffer(owner_key=ConnectionGeneration+SurfaceId+RequestId+ExecutionTargetId+TargetGeneration+AccountProfileId+ProviderNamespace+IndexGeneration+QueryGeneration),
    HistoricalConversationViewBuffer(owner_key=ConnectionGeneration+SurfaceId+RequestId+ViewTarget+ViewRevision+IndexGeneration+SourceRevision),
    RuntimeEndpointContinuityVerificationBuffer(owner_key=EndpointBrokerConnectionGeneration+OperationId+ExecutionTargetId+TargetGeneration+RuntimeEndpointId+PriorEndpointGeneration+CandidateEndpointGeneration+ProofDigest),
    ConversationProfileRebindBuffer(owner_key=LocalClientInstanceId+ConnectionGeneration+SurfaceId+RequestId+WorkspaceId+RuntimeEndpointBindingId+BindingGeneration),
    NativeJobScan(owner_key=ConnectionGeneration+NativeJobScanId+AccountProfileId+ExecutionTargetId+TargetGeneration+ProviderNamespace+AdapterGeneration+SnapshotWatermark),
    NativeJobPageBuffer(owner_key=ConnectionGeneration+RequestId+NativeJobScanId+PageGeneration),
    DictationTarget(owner_key=LocalClientInstanceId+SurfaceId+CaptureGeneration),
    MicrophoneLease(owner_key=PhysicalOperatorDeviceId),
    VoicePcmBuffer(owner_key=LocalClientInstanceId+SurfaceId+DeviceId+CaptureGeneration),
    VoiceHypothesis(owner_key=LocalClientInstanceId+SurfaceId+WorkerGeneration+CaptureGeneration),
    VoiceTranscriptDraft(owner_key=LocalClientInstanceId+SurfaceId+VoiceDraftId),
    SpeechWorker(owner_key=LocalClientInstanceId+SurfaceId+DeviceId+WorkerGeneration),
    CommitProposalSandboxHelper(owner_key=DaemonGeneration+CommitProposalAttemptId+AttemptGeneration+WorkerGeneration),
    AuxiliaryWorker(owner_key=DaemonGeneration+AuxiliaryWorkerOwnerKey+WorkerGeneration),
    LocalInputDraft(owner_key=LocalClientInstanceId+SurfaceId+LocalInputDraftId),
    ImeComposition(owner_key=LocalClientInstanceId+SurfaceId+InputTarget+CompositionGeneration)
  }
}
```

`TerminalWarmViewPark` and `TerminalOffscreenClientDetach` are independent local presentation mechanisms for
`PRD-RUN-022`, never runtime lifecycle states. Switching away immediately creates at most one warm generation
and may retain its renderer, screen projection and image cache for five minutes. Its closed reducer is
`warm→restored|evicted|expired`; all three outcomes are terminal. A Surface holds at most twelve warm records
in LRU order. The thirteenth or memory pressure evicts the oldest renderer only after its references quiesce;
it never changes PaneAttachment, RuntimeAttempt, PTY, terminal bytes, drafts or Attention. Thus the hard cache
bound never needs to sacrifice work or claim that a renderer is runtime authority.

Independently, the same view switch may create one exact `TerminalOffscreenClientDetach` in `observing` while
the PaneAttachment remains live. Only after ten continuous off-screen minutes may `observing→detaching`, and
only when the daemon revalidates exact attachment/attempt/handle generations and proves the client attachment
can disappear without closing, signalling, pausing or replacing the RuntimeAttempt/durable session and while
lifecycle, output-gap and Attention observation remain daemon-owned. A backend without that proof—including a
live plain-shell client whose view owns its PTY—is ineligible. Selected/focused Views, active input or resize
leases, IME/dictation/local drafts, a wake already in flight, `Working|AwaitingUser|Blocked|Unknown` evidence,
an exact Attention route/subject and any uncertain detach are protected and cancel that generation.

The closed detach reducer is `observing→detaching|selected|ineligible|expired`,
`detaching→detached|selected|cleanup_pending`, `cleanup_pending→detached|selected|cleanup_pending`,
`detached→attaching|expired`, and `attaching→selected|blocked|cleanup_pending`; `selected|ineligible|blocked|
expired` are terminal for that OffscreenGeneration. Every detach/attach retires the prior attachment generation,
preserves the same ViewTarget and Attempt identity and uses the ordinary terminal gap/resync contract. A selected
row, keyboard reveal or accepted Attention route starts `attaching` automatically; the client may prewarm on
approach but never starts/resumes/replaces a runtime. If the exact durable handle is unavailable it shows precise
offline/recovery status in the bottom status bar, not a generic start control. Restore, reconnect and daemon
restart still obey their normal no-implicit-launch rules.

A ten-minute zero-watcher sweep is only reconciliation for an already eligible or interrupted off-screen
detach: it requires the exact tmux-equivalent handle, no painter/relay/shadow/background-writer watcher and ten
continuous unwatched minutes, then releases only Turn's stale client PTY/attachment. It never kills the durable
session, process tree or scrollback and cannot originate a detach for an otherwise ineligible view. No shadow
PTY, shell command or second input owner may be created merely to keep an off-screen View observable.

For a RuntimeBackend that proves an exact tmux-equivalent durable session, a fixed binary may instead own one
zero-PTY `TerminalShadowObserver` while its painter is detached. It pins target/backend/socket/session handle,
handle and attempt generations, grid, output watermark and ShadowGeneration. Its reducer is
`reserved→attaching|retired`, `attaching→observing|failed|cleanup_pending`,
`observing→retiring|gapped|failed`, `gapped→observing|retiring|failed`,
`retiring→retired|cleanup_pending`, and `cleanup_pending→retired|cleanup_pending`; terminal failure publishes
the exact observation gap and leaves the runtime untouched. A gap first performs the one bounded capture/resync
against the same handle. If that cannot restore exact sequence coverage before its ten-second/8-MiB bound, the
Attempt observation capability becomes `Unknown` and one pre-reserved deduplicated actionable Attention is
materialised for `(AttemptOwner,RuntimeAttemptId,ShadowGeneration,gap watermark)`; it never fabricates a prompt,
completion or process exit. Recovery of a later exact generation ends that demand only through the canonical
evidence reducer, not navigation. Attach uses fixed argv/direct pipes and a
backend-verified owner-only socket—never a caller command, shell, PATH lookup, remote-to-local fallback or PTY.
Only one Turn painter, shadow observer or background-write control client may attach as Turn's client to that
durable session at a time. Viewer attach first fences and retires the shadow; it cannot publish cells under the
new attachment generation until the old control stream quiesces.

Shadow output enters the same Attempt-owned TerminalByteRing with the backend's monotonic output watermark;
it creates no second scrollback. Per-shadow input is≤512 chunks/8 MiB and buffer-first overflow records one
exact gap, stops further projection, and performs bounded backend capture/resync against the same handle. It
never pauses, kills or detaches the durable session. Death, timeout, malformed control frames, socket/target/
handle change or sequence regression retires the client and exposes partial observation; reconnect may create
one fresh generation only while the Park still requires it and a side-effect-free handle probe succeeds.

Background input to a parked durable session uses a separate at-most-one-per-target
`TerminalBackgroundWriteChannel`. It is created lazily from the same fixed control protocol, binds one exact
session handle at a time and retires before a painter or per-session shadow attaches. It accepts bytes only
after the ordinary `write_runtime_input` path validates the exact InputLease/InputSafety/Attempt generations,
encodes them as bounded literal bytes without shell evaluation and commits the existing RuntimeInputReceipt.
One command is≤64 KiB;≤64 queued commands/1 MiB, a five-second command deadline and a thirty-second idle linger
are hard. Lost reply becomes the receipt's possible-effect state and is never resent. The channel cannot mint,
renew or steal an input lease, answer a permission, write to another session or make output text executable.

Its closed reducer is `reserved→connecting|retired`, `connecting→idle|failed|cleanup_pending`,
`idle→dispatching|retiring`, `dispatching→idle|possible_effect|retiring|cleanup_pending`,
`possible_effect→idle|retiring|cleanup_pending` only after the exact RuntimeInputReceipt reconciles, and
`retiring→retired|cleanup_pending`, `cleanup_pending→retired|cleanup_pending`; `retired|failed` are terminal.
The transition to dispatching atomically transfers the command's byte/effect reservation into the existing
RuntimeInputReceipt before the first control write. `possible_effect` cannot accept another command for that
session and never resends. Owner/handle/target/lease loss, painter/shadow attach, End/Delete or daemon shutdown
fences new writes and enters retiring; queued definitely-unwritten commands cancel, while possible writes stay
with their receipts until lookup/quiescence.

There are≤128 live-or-cleanup shadow observers per ExecutionTarget and≤256 installation-wide; state is≤128 KiB
each/≤32 MiB, control input≤8 MiB each/≤256 MiB and measured process RSS≤8 MiB each/≤512 MiB, all charged before
spawn. There is≤1 background-write channel/target and≤64 globally,≤1 MiB queued bytes/channel/≤64 MiB and the
same≤8-MiB process ceiling/≤256-MiB family. Count, queue, process, output, cleanup and shared-RSS reservations
are atomic; saturation refuses the optimisation/write before effect and preserves the existing runtime. A
process/descriptor oracle proves every control client consumes zero PTY devices and cleanup uncertainty retains
its original count/byte/process charge. Before either fixed child spawns, Turn also reserves one Installation
`ProcessCleanupCharge` slot carrying exact target, executable descriptor, process-start identity and PID-reuse
fence. Owner/target/daemon/End/Delete loss atomically transfers—never duplicates—the original count/RSS/queue/
output charge to it until child-tree/pipe/descriptor quiescence or OS-reclamation proof. Parent-death/descriptor
oracles are defence in depth, not substitutes for this recovery evidence.

Input arriving during the automatic attach window is offered only to the exact `TerminalWakeInputBuffer`
fenced by Attempt and InputLease generations. It holds whole ordered input frames up to4,096 UTF-8 bytes for
at most ten seconds. A frame that would exceed the bound is refused whole with `queue_full`; successful attach,
fresh input-safety revalidation and lease confirmation flush once in order, while timeout, target/attempt/lease/
selection change, attach failure or cancellation expires the bytes and reports the loss in bottom status.
Nothing can splice into a resume/launch line, fall through to an old attachment or require another click.
The Wake buffer's closed reducer is `reserved→holding|cancelled`,
`holding→transferred_once|expired|cancelled`; all three outcomes are terminal. `transferred_once` is one atomic
ownership move into the exact generation-fenced `write_runtime_input`/RuntimeInputReceipt, not a replayable
body. End/Delete, reconnect, Surface/selection/attempt/lease/input-safety change and attach failure take
expired/cancelled, wipe bytes and emit one bottom-status receipt; no successor generation inherits them.
There are≤12 warm Park records/Surface and≤256/client, each≤256 KiB/≤32 MiB per client. There is≤1 current
off-screen-detach record/Pane,≤64/client and≤256 installation-wide, each≤4 KiB/≤1 MiB. There is≤1 wake
buffer/Pane,≤64/client and≤256 installation-wide/≤1 MiB. Reservation precedes retain, detach tracking or
buffering; saturation refuses the optimisation/input frame, never the runtime or Attention.

`PRD-SAF-021` is represented by an intentional absence: no detached-session count, elapsed-time threshold,
memory watermark, pressure signal, stale viewer or missing attachment can authorise End, kill, delete, reap,
detach a runtime, or remove its scrollback/recovery evidence. Resource pressure may retire reconstructible
presentation state, slow background observation, raise status/Attention, or offer an explicit exact-owner
consequence review. The independently enabled Eco reducer may hibernate only its already specified eligible
resumable instances. Every other termination uses the foreground typed lifecycle operation and exact handle;
there is therefore no automatic detached-session reaper, queue, timer, setting, receipt or hidden kill path in
the state manifest or operation registry.

`PrivateTranscriptSearchIndex` is the body-search capability of `PRD-OBS-012`; it is distinct from metadata-
only `ConversationInventory`. It exists only for `ProviderAccountScope=profiled` and is keyed by exact provider,
AccountProfile revision, ExecutionTarget generation, provider namespace, adapter/parser revision, transcript-
source generation and index generation. Only an enabled local-desktop policy may build it. Transcript roots are
closed adapter evidence already confined to that profile and target; the caller cannot supply paths, globs,
parsers, symlink policy or credentials. Each source read pins a regular descriptor/file identity and refuses
alias/root escape, device/FIFO/socket input, profile ambiguity and local fallback for a remote target.

The index is encrypted at rest with one non-exportable per-index key held through the local secret boundary;
its postings, titles, cwd labels, snippets, source locators and final≤200-KiB normalised user/assistant segment
tail per indexed document are all transcript-derived sensitive data. The segment tail is part of the same
encrypted index generation and its existing≤512-MiB profile/target and≤1-GiB installation bounds, deletion,
redaction and key-revocation policy; it is not a second cache or a provider-file fallback.
`absent→building|disabled`, `building→ready|partial|gapped|unavailable|deleting`,
`ready|partial|gapped|unavailable→building|deleting`, `deleting→deleted|delete_uncertain`, and
`delete_uncertain→deleted|delete_uncertain`; a new policy/source/parser generation creates a new index
generation, and deleted is terminal. `disabled` denotes only a policy for which no index generation was ever
built; an existing generation has no direct disabled edge. Disable/delete first
revokes the key and fences readers, then unlinks the encrypted index after descriptor/worker quiescence;
uncertain cleanup retains body-free evidence and never claims deletion. Provider transcript files are never
modified or deleted. Profile/target retirement, consent revocation and transcript-root loss take the same
fenced deletion path; no index, query or snippet enters sync, export, diagnostics, telemetry, ContextPacket,
Attention, Agent input or another profile/target.

`PrivateTranscriptSearchIndexReceipt` is the durable operation state keyed by
`PrivateTranscriptSearchOperationId`. Enable/Rebuild follows `prepared→building|cancelled|refused`,
`building→sealed|partial|gapped|unavailable|reconcile_required`, and `sealed→applied|reconcile_required`;
the prior current generation stays readable until one fully sealed descriptor/hash/key generation atomically
becomes current, and partial files are never published as ready. Disable/Delete follows
`prepared→revoking_key|cancelled|refused`, `revoking_key→key_revoked|reconcile_required`,
`key_revoked→unlinking`, `unlinking→deleted|delete_uncertain`, and
`delete_uncertain|reconcile_required→deleted|delete_uncertain|reconcile_required` from key/descriptor/worker
evidence only. Once `key_revoked` is proved, no path restores that index generation or revokes again; late
build output is fenced and unlinked after quiescence. Reconcile never opens a transcript, rebuilds, mints a key,
repeats revocation/unlink or converts missing evidence to success.

There is≤1 nonterminal search operation per exact profile/target/namespace and≤8 installation-wide. Rich
operation receipts are≤10,000 and≤8 KiB each under≤64 MiB, so the independent byte bound admits exactly8,192
maximum records. Operation/receipt/replay/journal/worker-correlation/recovery capacity reserves before source
read or key effect. Rich terminal details retain180 days behind a body-free installation-lifetime operation/
fingerprint/scope/key-generation/result fence; nonterminal, possible-key-effect and delete-uncertain records
never age out. N+1 reads no transcript, changes no policy/key/index and leaves coverage explicit.

A refresh runs at most once per five minutes and incrementally rereads changed exact identities. It scans at
most10,000 documents, reads at most the final5 MiB of one transcript and indexes at most the final200 KiB of
normalised user/assistant text per document. Each hit retains exact ConversationKey/source revision, bounded
safe title/project label, update time and a≤160-scalar query-centred snippet; it never infers ownership or
resumability from text. Coverage is `complete|partial(next_cursor)|gapped(minimum_revision)|unavailable|
disabled` with scanned/eligible/indexed/skipped counts and refresh time. Missing roots, unreadable/oversize
files, parse loss, stale index or saturation can never become a complete empty result.

One target/profile has≤1 refresh and the installation≤8, with≤256 queued source identities/2 MiB each and a
≤64-MiB refresh family. The encrypted durable index admits≤10,000 documents and≤512 MiB per profile/target,
≤1 GiB installation-wide inside the existing `account_private_root` disk class; N+1 marks partial before reading another body. One connection/Surface has≤2 search
buffers and the installation≤32, each≤2 MiB/≤64 MiB family with a30-second deadline.
`PrivateTranscriptSearchPage` is an explicit `request_value` with≤20 hits,≤4 KiB/hit and≤80 KiB logical;
query length is2..256 scalars and one query examines≤10,000 current
entries. Completion/cancel/deadline/index-generation change/disconnect releases query bytes. Selecting a hit
only selects the canonical read-only historical-conversation ViewTarget in the existing Surface; it never
creates a Node, adopts, resumes, starts, sends input, creates Attention or acknowledges anything. The selection
seal binds exact query/page digest, row ordinal, ConversationKey, profile/target/index/source generations and
expires with any of them. Selection first reserves one `HistoricalConversationViewBuffer`, the first-page
result and connection-outbox/chunk capacity for the exact response, then reauthorises the caller and atomically
CASes the current Surface/ViewTarget revision. N+1 or byte pressure refuses before that CAS and leaves the old
Surface and query intact. After a successful CAS, the already reserved first page returns in the same response,
so selection can never commit a blank historical view or require a second operator action. Forged, stale,
replayed against a changed Surface or cross-profile seals refuse before mutation.

Historical-target invalidation is an automatic Surface CAS, not a user action. It fences all outstanding page
cursors, increments the ViewTarget revision and selects the exact current `TreeSurfaceState.selected` target;
if that key was deleted, the standard following/previous/Session fallback runs, and if its container vanished
the owning Workspace overview is used. Concurrent index delete/profile retire and page response races have one
winner by Surface/index revisions; every late page is discarded. This fallback starts, resumes, focuses,
acknowledges and sends input to nothing.

Successful hit selection automatically returns the first `HistoricalConversationViewPage`; this explicit
`request_value` survives no request, and later pages use
`get_historical_conversation_view` against the exact active Surface/ViewTarget. Pages are derived only from the
already encrypted indexed≤200-KiB normalised tail, never by reopening provider transcript files. Each contains
≤100 user/assistant segments and≤64 KiB plus an authenticated≤512-byte cursor binding profile/target/index/
source/ConversationKey/ViewTarget revision, ordinal and predecessor digest. `HistoricalConversationViewBuffer`
is ephemeral state owned by
`ConnectionGeneration+SurfaceId+RequestId+ViewTarget+ViewRevision+IndexGeneration+SourceRevision`. At most one
request runs per Surface and16 installation-wide, each with≤2 MiB/≤32 MiB request-only working bytes and a
30-second deadline; bytes
release at response/failed/cancel/deadline/Surface or target loss and survive no reconnect. The 101st segment or
next byte yields partial, never truncates to complete. Changed/deleted index, source gap, revoked profile grant or
ViewTarget replacement returns explicit gapped/unavailable and zero fallback read. The page is display data only:
it cannot become ContextPacket, clipboard, export, diagnostics, Attention body or provider/runtime input.

`BrowserMemorySaverState` is daemon-owned ephemeral lifecycle evidence, never a renderer-local timer or a
durable page snapshot. The `lifecycle_behavior` setting defaults it off. One exact policy generation uses the
closed reducer `visible→hidden_waiting|terminated`, `hidden_waiting→visible|discarding|terminated`,
`discarding→discarded|visible|cleanup_pending`, `cleanup_pending→discarded|blocked`,
`discarded→rehydrating|blocked|terminated`, `rehydrating→visible|blocked|cleanup_pending`, and
`blocked→rehydrating|visible|terminated`; `terminated` is final and a new policy revision creates a new owner.
Only a continuously hidden current Browser revision for at least five minutes may enter `discarding`, and the
daemon revalidates immediately before the edge that it is not loading, audible, agent-controlled, opening a
popup, receiving or reviewing a download, printing, executing an action, or holding an unsubmitted form/POST.
`discarded` is committed only after renderer, page and partition quiescence or OS-reclamation proof releases
their original charges; uncertainty remains `cleanup_pending` with the same charges and cannot claim savings.
It retains only the canonical current public-HTTPS URL, origin/policy/address revisions and a
`history_lost=true` marker—never DOM, body, form/POST, script, cookie, credential, storage or history bodies.

Selecting that same discarded Node in the same daemon generation automatically preassigns exactly one new
idempotent `BrowserNavigationIntent` and enters `rehydrating` only if the current reviewed public-navigation
policy still permits the exact origin and address. This derived selection edge cannot acquire ambient
credentials, cannot open localhost/private or local-file content and cannot reuse a previous possible-effect
intent. A policy/address/origin mismatch enters `blocked(reason)` and exposes the precise reason in bottom
status; it never emits a generic start action. Crash recovery is lookup-only for the already durable new
navigation intent. Restore, reconnect or daemon restart still performs no automatic load. At most one state
exists per Browser Node, 10,000 installation-wide; each is≤4 KiB and the family is≤32 MiB, reached by8,192
maximum-size states independently of count. Count, item, family and shared-memory capacity reserve before
tracking; N+1 leaves the current renderer untouched, and Node/Workspace/daemon or policy-owner loss terminates
only after any cleanup-pending charge is transferred unchanged to `ProcessCleanupCharge`.

`PtyCapacityObservation` is the one current durable observation per exact ExecutionTarget generation. It
contains `used?`, `ceiling?`, backend-declared `required_headroom?`, measurement source, measured-at,
`complete|partial|unavailable|unsupported` coverage and `fresh|stale`; absent facts are never encoded as zero.
Fresh means no older than two minutes. The target-owned `PtyCapacityMonitor` samples no faster than once per
minute through the existing ResourceInventory collector and stores only the latest reading plus last-announced
level/time. The closed level is `healthy|elevated|critical|unknown`: elevated begins at exact
`used*5≥ceiling*4`; critical begins at `used≥ceiling-required_headroom`; unknown covers every absent, stale or
non-complete fact. A level edge emits one target-scoped status and, for elevated/critical, one deduplicated
resource-pressure Attention entry before any capacity-refused RuntimeLaunchReceipt. A held level may remind no
more than once per five minutes. Clear retires only that pressure demand and never another Attention subject.
There are≤256 observations and monitors, each≤4 KiB/≤1 MiB per family. A target/daemon generation change makes
the old observation stale immediately; target retirement removes it only after referenced launch/remediation
evidence is terminal. Sampling failure changes coverage to unavailable and never claims health.

The local macOS backend declares exact headroom four; another RuntimeBackend must publish and version its own
measured safe headroom or report unsupported. Before PTY launch the daemon refreshes or revalidates the exact
target observation. Fresh critical evidence refuses before opening a PTY. If measurement is unavailable, Turn
does not fabricate room: the existing launch may proceed only under its normal backend resource contract, and
an exact OS PTY-exhaustion result atomically publishes critical/unknown evidence plus Attention before the
failure receipt. Pressure cleanup can close only already-ended, unreferenced Turn-owned handles through their
normal exact lifecycle; it never kills, detaches, reaps or relabels live, watched, remote, tmux or unowned work.

`PtyCapacityRemediationIntent` exists only when a target advertises a closed privileged provider with durable
correlation, exact reread and rollback proof. Its prepared record freezes target/trust/provider/policy,
observation revision, before ceiling and persistent-config identity/hash-or-absence, daemon-derived proposed
ceiling, fixed helper identity, consequence text and rollback postcondition. The caller cannot supply shell,
argv, path, service label, config bytes or credential. Its reducer is
`prepared→dispatching|cancelled|expired|refused`,
`dispatching→kernel_applied|failed|reconcile_required`,
`kernel_applied→persisting|rollback_dispatching|reconcile_required`,
`persisting→verifying|rollback_dispatching|reconcile_required`,
`verifying→applied|rollback_dispatching|reconcile_required`,
`rollback_dispatching→rolled_back|rollback_failed|reconcile_required`, and
`reconcile_required→applied|rolled_back|partial_uncertain|failed|reconcile_required`; the exact terminal set is
`cancelled|expired|refused|applied|rolled_back|partial_uncertain|failed|rollback_failed`. Both
`partial_uncertain|rollback_failed` retain their original recovery evidence/charge and actionable Attention;
all intermediate phases remain nonterminal. Foreground Apply commits dispatching before invoking the fixed platform
privilege broker. Turn never receives an administrator secret. Cancellation is pre-dispatch only. Provider
failure attempts the already-reviewed rollback within the same correlated transaction; crash/lost reply uses
only provider journal, exact kernel reread and confined persistent-config identity, never a second privileged
dispatch. Unproved partial state remains `partial_uncertain` with recovery Attention.

At most64 remediation intents are nonterminal and one per target;10,000 nonterminal-plus-rich-terminal records
each≤8 KiB share≤64 MiB, reached by8,192 maximum records independently of count. One of100,000≤512-byte/
48-MiB operation/provider/before/after/rollback replay fences plus terminal, journal and recovery capacity
reserves before dispatch. Rich terminal evidence retains180 days only behind that fence; nonterminal,
reconcile-required, partial or rollback-failed evidence never ages out. Count/byte/fence N+1 leaves the system
setting untouched and exposes manual target-specific guidance. A backend without the complete provider,
receipt, reread and rollback contract exposes no automatic fix.

`AuxiliaryWorkerOwnerKey` is not an extension point. Its complete tagged union is:

```text
NotificationHost(NotificationEndpointId,EndpointGeneration,HostGeneration)
| NotificationDelivery(NotificationDeliveryId,DeliveryAttemptGeneration)
| RemoteTransport(RemoteClientId,RemoteSessionId,TransportGeneration)
| ContextBrokerRemoteRead(ContextLinkId,LinkGeneration,ReadId)
| Transfer(TransferTicketId,TicketGeneration)
| Updater(UpdateIntentId,IntentGeneration)
| ProviderBroker(ModelEndpointProfileId,ProfileRevision,BrokerGeneration)
| ProviderCollector(AccountProfileId,SourceGeneration,CollectorGeneration)
| Watchdog(ExecutionTargetId,TargetGeneration,WatchdogGeneration)
```

`AuxiliaryWorker` means only a Turn-owned process/task/socket worker in that union; `SpeechWorker`, Browser/
Media workers and `CommitProposalSandboxHelper` retain their stricter families and cannot be relabelled into
it. Per-kind live-or-cleanup-pending counts are respectively 1,32,128,128,32,1,32,32,64 and the shared
AuxiliaryWorker count is128. Each worker reserves its declared effective RSS/owned working set≤128 MiB, the
family is≤1,024 MiB, and both charges also consume `runtime.turn_variable_rss_mib`. Counts are independently
reachable with small workers; the byte boundary is independently reachable with eight maximum workers.
Reservation of kind+global count, item bytes, family bytes, shared bytes and one `ProcessCleanupCharge` slot is
atomic and precedes process/task/socket creation, source open or network dispatch; N+1 parks or refuses with
zero effect and never borrows another kind's unused per-kind limit.

The closed worker reducer is `reserved→starting|released`, `starting→running|stopping`,
`running→stopping`, `stopping→quiescent`; `released|quiescent` are terminal. Completion, cancellation, deadline,
owner/generation loss or daemon shutdown revokes authority and enters `stopping`; no new I/O/effect may start.
After the purpose-specific grace (at most two seconds), Turn terminates the owned process/task/socket tree.
Only quiescence or OS-reclamation proof releases count and bytes. If ownership ends first, the identical
kind/global slot and family/shared byte charge transfers—not duplicates—to `ProcessCleanupCharge`, so cycling
owners cannot admit a 129th worker or exceed1,024 MiB.

The following named wire projections are explicitly annotated `request_value`, survive no request and are
therefore absent from the state manifest: `DirectoryPage`, `CommitGraphPage`,
`CommitChangedFilesPage`, `CommandSearchResult`, `TextSearchResultPage`, `WorkItemPage`,
`ConversationInventoryPage`, `PrivateTranscriptSearchPage`, `HistoricalConversationViewPage`, `NativeJobPage`,
`WorkItemActivityPage`, `AgentBrowserReadPage`, `DiagnosticLogPage`,
`RecoveryInventoryPage`, `SettingsRegistryPage`, `SettingsSearchPage` and `BugReportDraftSeed`. Their logical response bodies are
owned only by the already-accounted `ChunkedResponseStream`/`ProtocolConnectionOutbox` while in flight; after
atomic transfer or failure they retain zero bytes, authority, cursor or identity. A backend implementation
that caches one must declare a new closed state family and cannot ship under this manifest.

`RecoveryInventoryPage` is the shared request-only envelope used by both typed inventory APIs.
`semantic_recovery_inventory` contains exact inventory key+revision, the closed redacted filter, counts by
subject kind/status, optional verified ContainerCloseReceiptId+semantic-survivor count/root and its first
page. `semantic_recovery_page` repeats that identity, revision and filter and carries page ordinal,
predecessor/page digest, coverage `complete|partial(next_cursor)|gapped(current_revision)` and≤500 entries/
≤1 MiB. Each≤2-KiB entry contains ReservationId, subject kind, redacted canonical-key hash, current state and
revision, immutable close receipt/serial ordinal/leaf when filtered by a close root, and one safe `ViewTarget`;
it contains no subject body, output, transcript, credential, provider payload or process command. The
authenticated≤512-byte cursor binds inventory, filter, root, page ordinal and predecessor digest. One
connection owns≤4 requests, the installation≤32 and≤32 MiB of the existing response-stream/outbox pool, with
one≤1-MiB request buffer and a30-second deadline; timeout, disconnect, transfer or gap releases it completely.
`container_close_survivor_inventory/page` uses the same bounds but binds the immutable close receipt, serial
point and all three typed roots. Each row is exactly
`semantic(ReservationId,inventory key+revision)|target_runtime(ExecutionTargetId,target inventory revision,
runtime recovery key)|process_cleanup(ProcessCleanupChargeId,cleanup revision)`, plus receipt ordinal, leaf
digest, current redacted status and one safe `ViewTarget`. Pages order first by the closed typed-root order and
then stable ordinal; the final page proves each advertised count/root. This is one consolidated local view
even when one close spans many ExecutionTargets, and it grants no lifecycle, input, filesystem or provider
authority.

`SettingsOwnerKey` is the closed union
`Global|Workspace(WorkspaceId)|Template(TemplateId)|Session(SessionId)`. Temporary values remain in the
exact `LocalClientInstanceId+SurfaceId` record and never acquire a daemon owner. There is no Node settings
scope. The stable `SettingsSectionId` union is exactly:

```text
agents | accounts | custom_agents | model_endpoints | usage | commit_proposals
| terminal | shell | runtime_backends | work_items | lifecycle_behavior
| appearance | attention_hud | notifications | voice
| operator_presence | companion | remote_access | collaboration_access | ssh_targets
| updates | privacy | diagnostics
```

These 23 ids are grouped, in order, as `agent_work`, `workspace`, `interface`, `connectivity` and
`application`. Every platform renders every id; an unsupported platform capability renders a typed unavailable
row instead of deleting the route. The authoritative allowed-scope matrix is:

```text
agents                 global workspace template session temporary
accounts               global workspace
custom_agents          global workspace template session
model_endpoints        global workspace template session
usage                   global workspace temporary
commit_proposals        global workspace template session
terminal                global workspace template session temporary
shell                   global workspace template session
runtime_backends        global workspace template session
work_items              global workspace template session
lifecycle_behavior      global workspace template session temporary
appearance              global workspace template session temporary
attention_hud           global workspace template session temporary
notifications           global workspace template session
voice                   global workspace template session temporary
operator_presence       global
companion               global workspace
remote_access           global workspace
collaboration_access    global workspace
ssh_targets             global workspace template session
updates                 global
privacy                 global workspace
diagnostics             global temporary
```

The `appearance.hidden_optional_controls` setting is a canonical ordered set of zero to eleven values from
this complete `HideableControlId` union:

```text
group_selection | remove_from_group | colour | duplicate | collapse_expand
| markdown_projection | refresh_terminal
| header_refresh | voice_dictation | generate_name | comments
```

Its default is empty. It is merely one≤256-byte value inside the existing SettingsRecord and introduces no
state family or operation. Writes with an unknown, duplicate or non-canonical value are rejected; legacy or
hostile stored values outside the union are ignored fail-visible and reported as an invalid setting. Every
other control id is structurally unhideable, including Attention/Next Attention, blocked/recovery/status,
Delete/End, Restart, Search, Close and every destructive consequence-review action. A hidden optional control
is removed only from its optional menu/header slot: the same current `CommandCatalogueEntryId`, typed action,
availability reason and receipt remain reachable through the canonical palette and keyboard route. Resolution
changes no command registration, authority, state, focus, accessibility tree outside that optional slot or
action result. The generated setting definition, closed union, menu/header projections, command entries and
accessibility inventory must form one exact bijection; a new hideable id cannot ship by editing only a client.

The generated product registry must equal this independently frozen union+matrix; it cannot define its own
denominator. A generated setting definition has one stable key, exactly one section,
one value schema/default/redaction class, a nonempty subset of `global|workspace|template|session`, and an
explicit `temporary_allowed` bit. A section may also expose typed management actions or read-only facts, but
section reset can contain only setting definitions. The registry admits≤2,048 definitions, each≤2 KiB, and
pages at≤500 rows/≤1 MiB with authenticated cursor≤512 bytes; search query is≤256 bytes and returns≤200
canonical row references/≤400 KiB. Search and deep link resolve to the same section+key editor and never copy
a setting or create another resolver. No commercial licence, seat, upgrade, subscription or entitlement id,
key, row, action, scope or feature gate exists in this union or registry. No product-telemetry, analytics,
install-count, unique-installation identifier, usage-reporting consent or always-on statistics setting, state
family, request, push, worker or destination exists either. Signed update discovery is a purpose-bound
foreground updater operation with channel/platform/version and anti-rollback state; it carries no stable
client/install id and cannot be reused as telemetry transport.

One persistent settings record exists lazily per `SettingsOwnerKey`, with≤256 overrides,≤4 KiB/value and
≤64 KiB encoded. Across all owners there are≤16,384 nonempty records/≤1,024 MiB; a failed N+1 setting write
does not prevent its Workspace/Template/Session from existing. A `SettingsMutationReceipt` is≤4 KiB; the
installation retains≤100,000/≤384 MiB for180 days, with exactly98,304 maximum receipts at the independent
byte boundary. Unknown values persisted by a newer build count toward record bytes and remain individually
resettable, but cannot be assigned to or removed by a section reset without a current exact definition.

`SettingsResetPreview` is one memory-only record per Surface, eight/local client and64 installation-wide. It
freezes registry revision, section, exact persistent `SettingsOwnerKey` or local temporary owner, settings
record revision, resolved-settings revision, and an ordered patch of≤256 changed keys with before/default/
revealed-origin safe values and per-row schema digest. One row is≤2 KiB, the complete preview is≤1 MiB, the
family is≤64 MiB and expiry is60 seconds. Preparing a replacement reserves before atomic swap; cancel,
apply, expiry, scope/section/record/registry change, Surface/window/client/connection or daemon loss releases
it. Apply matches exact preview id/revision/digest and changes only those keys in one transaction. A retry with
the same operation/fingerprint returns the same receipt; any changed fingerprint or concurrent settings/
registry revision conflicts with current truth. The inline preview's Apply action is the only consequence
review—there is no second modal for this non-destructive reset—and management actions, secrets, credentials,
profiles, trust, runtimes, Attention and data-deletion controls are structurally absent from the patch.

`StoreOwnerKey` is the closed union `Installation|Workspace(WorkspaceId)`. A parse, checksum or schema failure
never turns a store into an empty authoritative document. Before any default/new save, Turn atomically renames
the exact regular descriptor to an owner-only non-reused `CorruptStoreQuarantine`, fsyncs file and parent,
records original identity/size≤64 MiB/SHA-256/failure class and publishes one recovery StatusEvent. If atomic
rename, descriptor confinement, capacity or fsync cannot be proved, the original remains untouched and that
owner opens read-only unavailable; no empty/default file is written. A changed descriptor/race restarts from
current truth and never quarantines the wrong bytes.

There is at most one current quarantine per StoreOwnerKey,1,024 installation-wide and≤2,048 MiB physical
aggregate. A second failure first preserves the current exact file under a new identity; it never evicts an
unresolved quarantine, and capacity N+1 leaves the failing source untouched/read-only. Quarantines have no
time-based deletion. `recover_corrupt_store` names one exact quarantine id/hash and a reviewed validated
replacement document; `start_fresh_store` explicitly acknowledges omission inventory and reserves recovery
receipt plus new-document capacity; `export_corrupt_store_quarantine` is create-new only; and
`discard_corrupt_store_quarantine` is a separate destructive local-foreground privacy review. All use
`CorruptStoreRecoveryIntent` under `prepared→committing|cancelled`,
`committing→recovered|started_fresh|exported|discarded|reconcile_required`, and lookup-only reconciliation.
The quarantined bytes remain until a proved recovered/start-fresh/exported-plus-explicit-discard disposition;
a lost reply never repeats replace/export/delete. At most one nonterminal intent/owner,64 globally and10,000
receipts≤4 KiB/≤32 MiB are hard, giving8,192 maximum receipts at the independent byte boundary.

The internal Installation/Workspace store is daemon-single-writer and has no filesystem-watch mutation path.
What appears as an external Workspace change from another device is an authenticated typed mutation in the
same `Workspace(StateStreamKey)` plus its operation receipt. The originating client suppresses only the exact
same operation/revision echo; every other client applies the strictly sequenced event or gaps and automatically
resnapshots. A newly created Session/Node is additive canonical state and becomes visible without replacing a
locally selected ViewTarget, stopping a runtime or rebuilding from a client-authored document. Domain field
conflicts remain CAS conflicts. The whitelisted presentation-history fields may merge only by their declared
object/field revisions; a dirty `LocalInputDraft`, editor snapshot or settings-reset preview remains locally
owned and is marked stale/conflicted rather than overwritten.

Direct filesystem modification, replacement or watcher notification for an internal store never becomes a
domain event. Parse/checksum/descriptor failure follows `CorruptStoreQuarantine`; otherwise an unexpected
external identity/change makes that owner read-only and requires recovery. A user-selected external package
enters only through the existing reviewed `PortableImportIntent` with reminted ids. There is no watcher rearm,
mtime heuristic, additive JSON merge, hidden device registry or operation that can bypass StateStream revision,
authentication, idempotency, authority or recovery capacity.

`DiagnosticSourceKey` is the closed union
`daemon(DaemonGeneration)|local_client(LocalClientInstanceId)|adapter(AdapterRegistrationId,AdapterGeneration)|execution_target(ExecutionTargetId,TargetGeneration)|auxiliary_worker(AuxiliaryWorkerOwnerKey,WorkerGeneration)`.
`DiagnosticClearHighWater` admits≤4,096 exact source rows, each≤256 bytes/≤1 MiB aggregate; source registration
reserves its row before diagnostic admission, and an excess source may operate normally but contributes only
to one already-reserved global diagnostic gap until an eligible retired source row folds into the aggregate
non-replay digest. It never makes runtime, input or Attention admission depend on logging.
The one memory-only `DiagnosticLogRing` belongs to the current daemon/log generation and admits≤2,048
structured entries, each≤4 KiB/≤8 MiB total. Redaction and bulk-member removal happen before reservation;
an entry contains only sequence/time/severity, exact source, text key+safe arguments, bounded correlation ids
and coverage. It can contain no credential/environment value, raw terminal/transcript/file/HTTP/provider body,
command payload or unauthorised absolute path. Ring overflow advances `earliest_sequence` and records gapped
coverage; daemon restart creates a fresh empty generation labelled restart-gap, never a claim of complete
history. A `DiagnosticLogPage` is request-only, pinned to exact daemon/log generation, has≤256 entries/≤1 MiB
and cursor≤512 bytes. Filter and copy are client-pure projections of those exact returned rows.

Clear scope is exactly `all|source(DiagnosticSourceKey)` through an exact sequence. It requires
LocalDesktopForegroundAuthority, operation id and current daemon/log generation; it atomically removes matching
payload rows, advances the clear high-water, increments log revision and invalidates affected pages/subscriptions.
It cannot clear StatusEvent, Attention, audit, operation receipt, security, recovery or terminal evidence. One
durable body-free `DiagnosticLogClearReceipt`≤4 KiB records operation/fingerprint, old/new log revisions,
scope, through-sequence and removed count; at most10,000/≤32 MiB are rich for30 days, with exactly8,192
maximum receipts at the independent byte boundary and a minimal non-replay high-water thereafter. Same
operation/fingerprint is idempotent, changed fingerprint conflicts, and old payload cannot return through
restart, reconnect, stale page or subscription.

`BugReportDraft` is distinct from agent input, Notes and diagnostics. One memory-only editable draft exists per
Surface, eight/local client and64 installation-wide, each≤1 MiB/≤64 MiB family and expires after30 minutes.
It freezes draft id/revision, exact diagnostic daemon/log revision+sequence selection, title≤256 bytes,
description/reproduction text, an allowlisted system-version summary, ordered inclusion manifest, redaction/
omission report and canonical export digest. The reducer is `editing→reviewing|discarded|expired`,
`reviewing→editing|consumed|discarded|expired`; terminals never reactivate. Replacement, discard, consume,
expiry or Surface/window/client loss releases. Daemon/log generation change leaves the visible draft locally
editable but stale and ineligible for review until a fresh preparation binds current source evidence; reconnect
never grants old delivery authority. Preparation and editing perform no network, Browser, file, clipboard,
issue or provider effect.

A separate LocalDesktopForegroundAuthority review names exact draft/revision/digest and one closed disposition:
`copy_only`, `copy_and_open_support_page(ProductSupportDestinationId)` or
`create_new_file(exact create-new FileBackend descriptor)`. The compiled support-destination registry contains
only fixed HTTPS origin/path identities; the URL query/fragment carries no draft or diagnostic data, opening uses
the normal isolated reviewed Browser-Node intent, and report bytes are never uploaded automatically. Copy/file
content is exactly the reviewed canonical body. One body-free `BugReportReviewReceipt`≤4 KiB records digest,
destination/action and Browser/File subreceipt; at most10,000/≤32 MiB are retained30 days, with exactly8,192
maximum receipts at the independent byte boundary. Same operation/fingerprint is idempotent; ambiguity
reconciles the named Browser/File intent and never repeats or widens the effect.

`DocumentViewState` is a non-editing projection over one exact FileBackend descriptor/revision/hash, never a
Browser, Media playback or editor mode. Its admitted MIME union is exactly `image/png|image/jpeg|image/webp|
application/pdf`; SVG, animation, embedded files, forms, scripts and active content are unsupported. Source
bytes are≤256 MiB, a PDF is≤10,000 pages, an image or PDF page is≤16,384 pixels on either axis and≤64 million
decoded source pixels, and every visible raster is produced as≤8-million-pixel tiles. MIME disagreement,
polyglot, encryption, corrupt cross-reference, recursive object, decompression/decoder bomb or changed
descriptor/hash refuses before a ready view. The reducer is `reserved→reading|closed`,
`reading→ready|refused|failed|closing`, `ready→closing|failed`, `refused|failed→closing`, and
`closing→closed|cleanup_pending`; `closed` is terminal and `cleanup_pending→closed|cleanup_pending` only after
quiescence evidence. Restore, reconnect and selection never read, decode, print or auto-open.

One `DocumentViewState`≤64 KiB may exist per Surface, four/connection and64 installation-wide, for≤4 MiB.
Its exact-source `DocumentBlob` is memory-only,≤256 MiB/item,≤64 count and≤512 MiB aggregate. At most two
`DocumentDecodeWorkingSet`s are live-or-cleanup-pending, each≤256 MiB/≤512 MiB family. One view retains≤4
decoded tiles, each≤32 MiB; across all views the `DocumentPageCache` is≤256 tiles/≤512 MiB. A
`DocumentTextIndex` is≤16 MiB/view,≤64 count/≤64 MiB family and indexes only bounded extracted plain text;
it uses `TextSearchSession` and never retains a second result set. Every byte also charges
`runtime.turn_variable_rss_mib`. Count, source, decoder, cache, index and shared-byte reservations occur before
descriptor read/worker start; N+1 performs no read or decode. Page replacement releases only after renderer
references quiesce. Close, source/revision/ViewTarget/Surface/connection/daemon loss immediately revokes the
object URL and decoder generation, while lingering worker/buffer charges transfer without duplication to
`ProcessCleanupCharge`; cycling a view cannot bypass a bound.

Printing is the sole document external effect. `DocumentPrintIntent` freezes operation/fingerprint, exact
Workspace/View/source/blob/page-selection/layout/printer-capability revisions and review digest under
`prepared→spooling|cancelled|expired`, `spooling→dispatching|failed|cancelled`,
`dispatching→printed|not_printed|submitted_unconfirmed|reconcile_required`, and
`reconcile_required→printed|not_printed|submitted_unconfirmed|reconcile_required`; terminal outcomes never
reactivate. `dispatching` is durable before the native print call. Lookup-only recovery uses the exact native
job correlation and never prints again. One nonterminal intent/view and32 installation-wide are allowed;
10,000 rich intents/receipts≤4 KiB each share≤32 MiB, giving exactly8,192 maximum records at the independent
byte boundary, and retain30-day richness behind an installation-lifetime operation/result replay fence.
`DocumentPrintSpool` is isolated,≤64 MiB/item,≤2 count/≤128 MiB plus shared RSS; it contains only the reviewed
page raster/vector subset, has no links/scripts/attachments, and is destroyed after native descriptor/job
quiescence. Prepared cancellation produces zero spool/printer effect; ambiguity remains visible and never
auto-retries.

`TerminalClipboardGesture` is local-client ephemeral state, not daemon or terminal authority. Its closed kind
is `copy_selection|paste_text|drop_paths`. It binds the exact LocalClientInstanceId, Surface, current
PaneAttachment/attachment generation, terminal buffer/grid-or-input-lease revision, OS user-gesture generation
and digest. Copy accepts only the currently rendered explicit selection≤64 KiB, writes the local OS clipboard
once, records `applied|failed|ambiguous` locally and has no wire request; it cannot ask the terminal for data.
Paste contains≤64 KiB UTF-8. Drop contains≤128 canonical local paths, each≤4 KiB, inside the same≤64-KiB
manifest and is unrepresentable for a remote ExecutionTarget or remote/Companion client. Paste/drop commit only
as the `clipboard_paste|path_drop` variant of one current lease- and InputSafety-fenced
`write_runtime_input`; its existing `RuntimeInputReceipt` is the only PTY effect receipt. Failure, stale focus,
non-ordinary safety, lease loss or cancellation writes zero bytes; a lost reply reconciles that receipt and
never resends.

One gesture/Surface, eight/local client and64 installation-wide are admitted, each≤64 KiB and≤4 MiB total,
with a30-second hard expiry. Replacement reserves before atomically wiping the predecessor. Commit, cancel,
expiry, Pane/attachment/grid/lease/Surface/client loss wipes body bytes and releases the record; no body enters
store, journal, diagnostics, context, sync, crash data or Attention. The terminal parser consumes every OSC 52
read query and write payload—including fragmented, nested, encoded and oversized forms—before it can reach the
OS clipboard and emits no response bytes. Neither direction has an allow setting or remote fallback.

`AttentionAudioCue` is a derived local accessibility supplement. The only cue kinds are `done` and
`needs_you`: `done` is admitted from a fresh canonical working→idle-or-completed edge carrying a new result
revision and no current actionable demand; `needs_you` is admitted only from insertion of a fresh current
PendingInteraction/Attention demand revision. Settings provide visible `enabled`, `muted`,
`volume_millipercent=0..1000` and per-subject cooldown≥2 seconds. A client holds≤16 queued/current cues,
the installation≤128, each≤2 KiB/≤256 KiB total; one subject sounds at most once per revision and a client at
most eight cues per rolling10 seconds. Admission after a duplicate/stale/replayed edge, while muted, inside
cooldown, or more than2 seconds after the source edge drops the cue rather than playing late. Built-in local
assets are fixed, signed and≤64 KiB each; there is no network or content-derived audio.

A supported cue starts within300 ms, is≤2 seconds long and never blocks input, terminal output or Attention.
Playback completion, source invalidation, mute, settings revision, client exit or deadline releases the
ephemeral record; daemon restart/reconnect restores none and cannot autoplay. Audio failure is a labelled local
capability fact, not a state transition. Cue enqueue/playback carries no task text, credential or provider data
and creates, routes, focuses, acknowledges, snoozes, dismisses or resolves zero Attention records.

`BulkIdleRestartPreview` is daemon-derived from one exact Workspace inventory/policy/Attention/interaction/
input-lease watermark and contains≤256 ordered candidate rows, each≤512 bytes, inside≤256 KiB. One/Surface and
64 installation-wide share≤16 MiB; it expires after60 seconds and candidate/state revision drift invalidates
it. Eligibility is closed to an AgentInstance whose latest RuntimeAttempt is exactly `Idle`, locally owned,
stoppable, resume-capable and not background/recurrent, with no live subagent, nonterminal delegated effect,
PendingInteraction, actionable Attention, input lease, unresolved recovery or primary-checkout binding.
`Working|AwaitingUser|Blocked|Unknown|Disconnected|Lost`, remote, missing-session and unresumable candidates are
reported as excluded with one closed reason and can never be caller-added.

`BulkIdleRestartIntent` freezes that preview digest,≤256 candidate identities/revisions and a preassigned
restart operation id per eligible candidate. Its reducer is `prepared→running|cancelled|expired`,
`running→completed|partial|cancelling|reconcile_required`, `cancelling→cancelled|partial|reconcile_required`,
and `reconcile_required→running|completed|partial|cancelled|reconcile_required`; terminal states never
reactivate. Exactly one candidate is dispatched at a time through canonical `restart_runtime_owner`; its
`BulkIdleRestartInstanceReceipt` records `restarted|skipped(reason)|failed|reconcile_required` and the named
canonical receipt once. Cancellation fences before the next candidate and does not repeat or interrupt the
current operation. Crash recovery first performs lookup-only reconciliation of every dispatched id, then may
continue only still-exact undispatched candidates. The final ordered summary accounts once for every preview
row and never substitutes a new candidate.

One nonterminal bulk intent/Workspace and64 installation-wide are hard. Up to10,000 overall records≤64 KiB
share≤128 MiB, giving2,048 maximum records at the independent byte boundary; up to100,000 per-instance
receipts≤2 KiB share≤128 MiB, giving65,536 maximum records at that boundary. Active/terminal/replay/journal/
recovery and every per-instance receipt reserve before the first stop. Rich terminal state is retained180 days
behind minimal non-replay fences; nonterminal/possible-effect evidence never ages out. N+1 has zero runtime,
input, Attention, checkout or graph effect.

Eco hibernation is a separate opt-in lifecycle policy, never resource-pressure eviction. `EcoSchedulerQueue`
pins one Workspace policy/inventory/Attention/interaction watermark, contains≤256 candidates/≤128 KiB,
one/Workspace and64/≤8 MiB installation-wide, and emits at most two attempts in any rolling minute. A candidate
must remain continuously Idle for the configured threshold≥15 minutes and pass the same bulk eligibility plus
`not visible on any Surface`, `no live descendant`, `no recurring flow`, `no background task`, `local target`
and exact adapter `hibernate_resume` support. Any `Working|AwaitingUser|Blocked|Unknown`, visibility, prompt,
Attention, child, background, remote, unresumable or stale evidence removes it before dispatch.

Each automatic attempt first persists Workspace-owned `EcoHibernateIntent` with exact instance/attempt/session/
multiplexer/scrollback/policy/eligibility revisions. Its reducer is `prepared→exiting|cancelled|ineligible`,
`exiting→hibernated|failed|reconcile_required`, `hibernated→waking`,
`waking→awake|failed|reconcile_required`, and `reconcile_required→hibernated|awake|failed|
reconcile_required`; terminal `cancelled|ineligible|awake|failed` never reactivate. The adapter performs one
typed graceful hibernate operation—never injected shell text—and proves the provider process ended while the
same resumable session and bounded scrollback remain. Selection, Attention route or a new eligible Flow step
automatically persists and dispatches wake without a `Start pane` action. Wake failure creates one actionable
Attention demand; hibernate failure creates only a visible lifecycle failure unless work now needs input.

At most one nonterminal Eco intent/AgentInstance,64 installation-wide and10,000 rich records are admitted;
each is≤4 KiB/≤32 MiB, so exactly8,192 maximum records hit the independent byte boundary. Admission reserves
terminal/replay/journal/recovery before exit. Same operation/fingerprint is idempotent, recovery is lookup-only,
possible exit/wake evidence never retries automatically, and30-day compaction retains instance/attempt/
session/policy/outcome replay proof. Hibernation preserves hierarchy, Session, terminal scrollback, unread and
Attention truth; it creates no false completion and no other candidate is sacrificed to meet pressure.

`AgentBrowserControlGrant` is Workspace-owned and locally adopted; repository/shared/cloned flags are
untrusted discovery facts and cannot activate it. Its state is `proposed→active|rejected|expired`,
`active→revoked|expired`; terminal states never reactivate. An active grant binds exact Workspace policy,
adopting local operator,≤64 canonical public-HTTPS origin rules, capability revision and expiry≤24 hours.
There are≤256 active grants installation-wide, each≤8 KiB/≤2 MiB; N+1 or expiry/revoke fences every child
action before releasing. It grants no filesystem, `file:`, localhost/private/link-local address, daemon/control
origin, clipboard, credential store, ambient cookie, device, download acceptance, popup acceptance or human
browser access.

An agent-controlled Browser Node uses the normal isolated logged-out Browser kind and immutably records its
creating AgentInstance+RuntimeAttempt+attempt/binding/adapter generations and grant revision. Only that exact
current owner may submit the closed `create|navigate|read|click|type` action union. Navigate accepts one
grant-allowed BrowserUrl. Read returns a request-only `AgentBrowserReadPage` of≤256 inert accessibility rows,
≤1 KiB/row and≤256 KiB total from a scan of≤10,000 current-revision nodes. Click names one stable element id;
type names one non-secret editable element plus≤4 KiB UTF-8. Raw script, selector, key sequence, arbitrary DOM,
upload/path, password/payment field and caller-chosen network request are unrepresentable. Every resulting
navigation/redirect reuses the existing Browser policy; popup/download remains blocked or quarantined for a
separate human review and cannot be accepted by the agent.

Each action first persists `AgentBrowserActionIntent` with operation/fingerprint, exact grant/owner/Node/
partition/navigation/accessibility revisions and typed payload hash. Its reducer is
`prepared→dispatching|refused|cancelled`, `dispatching→applied|no_effect|reconcile_required`, and
`reconcile_required→applied|no_effect|dispatched_unconfirmed|reconcile_required`; terminals never reactivate.
Dispatching precedes Browser creation/navigation/interaction; creation/navigation names their canonical
subintent/receipt. Recovery inspects only those ids and renderer interaction journal and never recreates,
navigates, clicks, types or reads a newer page. One action may be nonterminal per owned Node,≤256 globally;
10,000 rich action records≤8 KiB share≤64 MiB, giving8,192 maximum records at the byte boundary, and100,000
minimal≤512-byte replay fences share≤48 MiB. Rich terminals retain30 days; nonterminal/possible-effect evidence
never ages out.

Every controlled Node shows a non-content-overlay badge naming the owning agent and one foreground `Stop`
action. Stop, grant revoke/expiry, owner/attempt/binding/adapter termination, Workspace close or Node close
fences queued and late actions, revokes the partition's agent channel and leaves uncertain dispatched evidence
charged for reconciliation. Untrusted page content cannot obscure the badge, alter the grant or emit a Turn
operation. A human may continue or close the isolated Browser only after agent authority is revoked; no action
can cross into a Browser Node created by a human or another agent.

Variant lists inside these families are separately closed by their tagged unions; for example `Node` contains
every declared NodeKind, while the six relationship families above are the complete graph-family set rather
than an open `relationships` bucket. Global/Template settings are Installation-owned, Workspace/Session
settings are Workspace-owned and Temporary settings are Surface-bound ephemeral. `SurfaceRegistry` is the
collection, `Surface` its durable entry and `TreeSurfaceState` its state-bearing nested schema; the generated
manifest checks all three declarations and their shared Installation owner rather than inferring nesting.
The same rule makes `CommandCatalogue`, `AttentionQueue`, `WorkItemKeyRegistry`, each of the three semantic-
recovery inventories and `LiveSubscriptionRegistry` explicit state-bearing collections alongside their named
entries/order/reservations. Aggregate reservations, revisions, gap capacity, source high-water and tombstone-
fold state belong to those named families and cannot hide in an unlisted implementation index. A read-only
projection over them is annotated `request_value` and owns no independent state. `TerminalRuntimeState` is the memory-only PTY lifecycle entry;
`TerminalByteRing`, `TerminalScreen` and `TerminalImageStore` are its separately accounted state-bearing
children, while `TerminalImageClientCache` is connection/Surface-local and never owns terminal truth.
For every ephemeral declaration the exact `owner_key` expression above is part of the manifest checksum and
the ownership-bijection gate; lifetime-only grouping is insufficient. A projection may reference
another stream id but cannot mutate or revision it. A client
subscribes to an authorised set. Each `state_snapshot` closes its named stream at revision `R`; that stream's
`state_event` is strictly sequenced from `R+1`, and
`ack_state_revision` advances only the named cursor.

`LocalClientInstanceId` is a cryptographically random 128-bit id minted once by a native client process,
never reused and held only in that process. It is an ownership coordinate for non-authoritative local UI state,
not authentication or daemon authority. `LocalInputDraftId`, `VoiceDraftId`, `CaptureGeneration`,
`CompositionGeneration` and `WorkerGeneration` are monotonically non-reused inside that client id. A new
daemon/transport connection cannot claim an earlier connection's authority merely by repeating the local id.
It may only let the still-open native window keep its own draft/settings bytes while every commit is
revalidated against a newly obtained InputTarget.

`SurfaceConnectionBinding` is exact authenticated authority metadata,≤4 KiB, one per active Surface,≤4 per
connection and≤64/256 KiB installation-wide. Open/resume atomically reserves and replaces only after owner+
revision validation; predecessor disconnect/replacement immediately revokes and releases the binding before
the Surface becomes dormant, while any child-worker cleanup remains separately charged. A fifth/65th/oversize
binding mints or transfers nothing, and reconnect cannot inherit the predecessor generation.

`RemotePresence` reserves≤4 KiB/item and≤128/512 KiB family plus shared RSS before accepting an update;
disconnect or its≤30-second expiry emits one bounded tombstone and releases. `RemoteReplayNonce` reserves
≤256 bytes/item,≤10,000 count and≤2 MiB family before any remote mutation; independent fixtures reach10,000
small records or8,192 maximum records. Receipt/expiry/revocation/session or connection loss releases at the
exact10-minute boundary, restart inherits no ephemeral presence or nonce, and N+1 performs no mutation.

One `TemporarySettingsRecord` exists per LocalClientInstanceId+Surface, contains at most 256 declared keys,
one value at most 4 KiB and a complete encoded record at most 64 KiB. Eight records/512 KiB per local client
and 64 records/4 MiB installation-wide are hard. They survive daemon/transport reconnect in the same open
native window, but Surface retirement or client/window process exit drops them; they never sync, persist or
move to another Surface. One `LocalInputDraft` per Surface is at most 32 KiB, with eight/256 KiB per local
client and 64/2 MiB installation-wide. Selection/target/generation change marks it stale and disables commit;
it remains visibly editable until explicit send/insert/discard or Surface/window exit, and reconnect never
silently retargets it. `VoiceTranscriptDraft` is bounded provenance/review metadata for that same draft body,
not a second copy. One `ImeComposition` per Surface is at most 32 KiB under the same 8/256-KiB client and
64/2-MiB installation bounds; it commits zero target bytes while active and is cancelled on target/focus/
Surface/window loss. A transport/daemon reconnect may preserve its local text but requires a fresh exact
InputTarget before the one composition commit. Every count/byte N+1 refuses the new local state before
replacing an existing record.

Core semantic admission is a cross-family transaction, not an unbounded side effect hidden behind these
streams. The daemon simultaneously enforces 1,024 Workspaces; 1,024 Sessions/Workspace and 10,000/installation;
10,000 base Nodes/Session, 50,000/Workspace and 100,000/installation; 50,000 AgentInstances; 10,000 nonterminal
RuntimeAttempts and 100,000 current+ended RuntimeAttempt details; and 64 Panes/Session plus 4,096/installation.
Every durable Workspace, Session, base Node (including Group/WorkItem shell), AgentInstance, retained Attempt
detail, Pane, Layout, TeamMembership, DependencyEdge and Flow/relationship core additionally charges the shared
200,000-record/1,024-MiB encoded envelope:≤64 KiB/ordinary item and≤256 KiB/Layout. A create/adopt/activation/
launch mutation reserves every applicable count, worst-case bytes, replay/journal/recovery slot and, for a
history-enabled terminal Pane, one of 256 complete 8-MiB-journal+4-MiB-checkpoint allowances before graph,
filesystem, PTY, process, provider, file, renderer or network effect. Eligible ended Attempt detail may fold
first; current/live/referenced state never evicts. Exact N+1 refusal names the saturated key and changes nothing.
External observation overflow emits a bounded coverage gap and disables authoritative absence/control. Delete/
tombstone releases only after its nonreuse/reference fence commits; End uses pre-reserved recovery and cannot be
vetoed by a full core.

`PhysicalDiskLedger` is the sole-writer Installation record over ten closed Turn-owned classes:
`operational_store|state_sync_journal|terminal_history|file_save_temporary|portable_temporary|
account_private_root|speech_model|media_pool|transfer_or_quarantine|update_package`. Their hard physical caps
are 8/4/3/2/2/2/8/100/4/2 GiB and their sum is the 135-GiB total. Every file create/extend reserves family plus
total using the greater of worst-case logical reservation and filesystem allocated bytes+declared overhead.
Rename/seal/refcount/ownership-transfer moves one charge; copy reserves both; cleanup releases only after
absence proof. Boot reconciliation charges unknown Turn-root bytes to the
`operational_store(unclassified_quarantine)` substate before writes; that substate consumes both the 8-GiB
operational cap and the total, raises system Attention and permits no new write until every entry is classified
or removed. It is not an eleventh class. External user/provider roots are separately reported and never scratch. Every disk report returns class
logical_reserved, physical_allocated, reclaim_pending and total, so sparse/COW/compression cannot satisfy an
oracle by hiding ownership.

`ProcessCleanupCharge` is an Installation-owned, body-free cleanup record for a Turn worker whose ephemeral
connection/Surface/Node owner is being removed before quiescence. Spawn pre-reserves one of 4,096 records and
≤4 KiB inside a 16-MiB aggregate. Owner loss atomically revokes view/control authority and transfers—not
duplicates—the exact worker correlation, family slot and family/shared-RSS byte reservations to this record;
dormancy/retire/End/delete still completes. Only descriptor/process/thread/socket/shared-buffer quiescence or
OS-reclamation proof releases the inherited slot/bytes and terminalises the record. A hung worker remains
charged, reconnect cannot inherit it and cycling owners cannot exceed the original family/shared caps.

Permission issue/claim/dispatch carries
`PermissionAuthorityVector=(InstallationRevision,WorkspaceRevision,ExecutionTargetRevision)`. The grant/session
component is Installation-owned, interaction/claim/receipt Workspace-owned and permission fact/transport
ExecutionTarget-owned. One store transaction locks/revalidates the vector and commits one shared barrier or
none. `RemotePresence` is ephemeral connection state outside durable snapshots/journal; its live projection is
explicitly not an authority revision.

A transaction touching multiple streams has one operation/transaction id and repeats its complete sorted
pre/post `StateWatermark` on every fragment. The client applies it atomically only after every authorised
fragment arrives; a hidden unauthorised fragment is represented by a non-content causal marker so order is
preserved without disclosure. A missing fragment, compacted cursor, `state_gap`, target/daemon generation
change or mismatched watermark blocks mutation and forces a bounded snapshot at the affected vector. Every
mutation carries every affected stream and object revision through `MutationEnvelope`. Edge add/remove
commutes by edge id, scalar state is compare-and-swap, FlowDefinition revisions are immutable, FlowRun
receipts are append-only, deletion wins over older mutation and lifecycle effects converge by operation id.
Updates never upsert and ids are never reused, so each stream's compacted minimum accepted revision remains a
permanent resurrection fence. There is no invented total order across independent streams. Global Attention
routing validates the Installation revision plus the exact subject-stream revision; a Workspace default
pointing to an AccountProfile or ModelEndpointProfile validates Workspace and ExecutionTarget revisions.
CheckoutScope create/adopt/rehome likewise validates its Workspace/GroupTree and exact target inventory vector.
Target Recovery and profile changes therefore converge without being smuggled into whichever Workspace
happened to be open.

Journal storage is hard-bounded to 512 MiB for Installation, 256 MiB per Workspace, 128 MiB per
ExecutionTarget and 4,096 MiB across all streams. Every local MutationEnvelope reserves its event and all
cross-stream barrier fragments before effect. Eligible segments compact behind a published minimum accepted
revision; if the whole vector still cannot fit, the mutation refuses before effect. External observation
overflow uses the producer's reserved current-state+`state_gap` record. Thirty-day/byte compaction preserves
current records, nonterminal/uncertain intents, operation replay, deletion/nonreuse fences and unresolved
transaction barriers. A slower cursor cannot pin storage or mutate and must resnapshot its affected vector.

`acquire_input_lease`, `renew_input_lease`, `handoff_input_lease` and `release_input_lease` carry exact
runtime/input owner, client/surface/generation and expected lease generation. A lease expires after 15 seconds
and renews no more often than five; handoff changes generation atomically and both clients observe it before
new bytes are accepted. `write_runtime_input` and `resize_runtime_input` repeat the exact AttemptOwner,
RuntimeAttempt id/generation, optional current RuntimeEndpointBinding id/generation, lease id/generation,
client id, surface/connection generation and a monotonically increasing per-lease input sequence inside their
`MutationEnvelope`; write additionally repeats the current InputSafety revision/route and its ordinary-bytes or
verified-local-permission-fallback tag. Validation of safety, permission fact/transport when applicable and
byte/resize enqueue are one serial action; any mismatch accepts zero bytes
and returns a typed receipt. A duplicate operation/sequence with identical bytes returns the first receipt;
different bytes conflict. Lease handoff/expiry closes the old sequence space before the new owner is visible.
The v4 `write_pty`/`resize_pty` shape is available only to a negotiated local single-client v4 connection.
Negotiating vNext state streams, any remote role or multi-client operation makes those legacy requests
`unsupported_protocol`; they cannot bypass lease, attempt, binding or surface fences. Draft bodies are not
protocol state and never transfer between clients.

Background notification is an Installation-stream projection of canonical Attention keyed by
`NotificationEndpointId`, never a second queue. `NotificationEndpointState` is
`reserved→active|retired|deleted`, `active→retired`, `retired→deleted`; deleted terminal. Ids are installation-
monotonic/non-reused. Retire atomically revokes proposed/active grants, expires outbox, emits live tombstones
and removes only the local secret reference; delete requires retired plus terminal pairing/grant/delivery
survivors and retains high-water/replay fences. Re-pair always mints a new endpoint.

`NotificationPairingIntent` freezes operation/fingerprint, preassigned endpoint+initial grant ids and
generations, catalogue revision, peer correlation, policy and times. Its closed state is
`prepared→dispatching|cancelled|expired`, `dispatching→awaiting_peer|paired|refused|reconcile_required`,
`awaiting_peer→paired|refused|reconcile_required`, and
`reconcile_required→paired|not_paired|reconcile_required`. Prepared expires at `created_at+600s`; an awaiting-
peer 600-second deadline and every post-dispatch timeout/crash never expire safely but enter reconcile-required;
lookup-only reconciliation uses peer correlation and never redispatches. Paired activates endpoint+grant in
one transaction. Cancel accepts prepared only. Cancelled/expired pre-dispatch atomically creates an endpoint
deleted tombstone and expired initial grant; refused/not_paired does the same with invalid grant, releasing
live count without cleanup interaction while preserving id/correlation/replay fences.

A `DeliveryGrantId` binds endpoint public key/token reference, device/profile, allowed Workspaces/
ExecutionTargets, event classes, privacy detail, rate/batch bounds, scope fingerprint, generation and expiry.
Its state is `proposed→active|invalid|revoked|expired`, `active→expired|invalid|revoked`; terminals never
reactivate. Every issue/regrant uses a non-reused id and monotonic endpoint generation, with one active
equivalent scope. Widen/rekey/policy change is revoke+new grant. Tokens/private keys remain in the keystore or
target agent and are absent from reads, exports, logs and diagnostics. A 401/403 invalidates only the exact
generation.

The installation admits 64 non-deleted endpoints, 32 proposed/active grants per endpoint and 2,048 total.
Pair/issue/revoke/retire/delete share 10,000 nonterminal-or-uncompacted≤4-KiB control records/32 MiB; the
independent byte boundary admits exactly8,192 maximum records, while the count boundary uses smaller records.
They retain 180-day terminal richness. Each operation reserves count/bytes/terminal/recovery before peer/gateway effect; N+1
refuses pre-effect. Nonterminal/possible-effect evidence never ages out; folding preserves endpoint/grant
high-water, scope fingerprint, operation replay and peer correlation.

```text
NotificationDeliveryState:
eligible -> held_present | queued | superseded | expired
held_present -> queued | superseded | expired
queued -> submitted | superseded | expired
submitted -> accepted | failed_retryable | failed_terminal | superseded | expired
failed_retryable -> queued | failed_terminal | superseded | expired
accepted | failed_terminal | superseded | expired -> terminal
```

One `NotificationDeliveryId` survives exactly eight total gateway submissions including the first; each
return to queued increments an attempt counter and applies declared jitter/rate limits with backoff≤15
minutes. A ninth is unrepresentable and exhaustion forces `failed_terminal`. Gateway
`accepted` proves neither device delivery, reading nor demand resolution. Collapse identity is split:
`CollapseFamilyKey=(NotificationEndpointId,complete AttentionSubject identity,demand_kind)` is stable across
revisions, and `CollapseKey=(CollapseFamilyKey,subject_revision)` identifies one delivery. Only a newer
current revision in the same family may supersede an older delivery. Insert and flush both revalidate grant,
queue revision, resolution and presence. The bounded E2EE payload excludes transcript/path/command/secret;
failure changes no Attention, unread or runtime state. Deep links carry opaque ids and must resnapshot and
route the exact current Attention before display or action.

The encrypted outbox is bounded to 10,000 live deliveries/16 MiB, each ciphertext≤16 KiB and≤24 hours.
Terminal delivery audit is bounded to 100,000 records/256 MiB, each≤4 KiB, for seven days. Eligibility reserves
outbox and terminal slots together. Overflow emits a bounded gap without mutating Attention; it never silently
evicts a still-current higher-priority delivery. Folding retains delivery id, endpoint/grant generation,
collapse family, retry counter and replay fences.

Live status uses `LiveStreamKey=(NotificationEndpointId,AttentionSubject identity,attempt_generation)` plus
monotonic event revision. Start/update/end are collapse-aware and end/tombstone fences every late tick.
Presence may hold an alert but never pauses the authoritative stream; release enqueues only a still-current
demand. `NotificationHostMode=owner_local|loopback_observer` accepts authenticated owner-local or loopback
observation input and makes outbound HTTPS delivery only. It ignores public bind host/port and exposes zero
public inbound listeners. It is not a `RemoteOperatorSurface`; notification or headless delivery grants no
input/control/credential authority, and remote GUI/headless protocol clients remain separately authenticated.

`CompanionAction` is a closed wire-to-domain mapping, not an alias namespace:

```text
route_attention            -> route_attention
mark_result_read            -> mark_node_result_read
acknowledge                 -> acknowledge_attention
snooze                      -> snooze_attention
dismiss                     -> dismiss_attention
submit_free_text_response   -> respond_to_agent_interaction
submit_permission_response  -> submit_remote_permission_response
interrupt                   -> interrupt_runtime_owner
request_writer_lease        -> request_input_lease_handoff
launch_allowlisted_agent    -> launch_companion_agent
```

Every action carries a common `CompanionActionEnvelope=(action_id,operation_id,issued_at,expires_at,
RemoteClientId/client_revision,RemoteSessionId/session_revision,surface_id,connection_generation,
WorkspaceId,SessionId?,registry_manifest_hash)` plus the complete exact fields of its mapped operation.
Its expiry is at most 30 seconds. Before the mapped operation can have any effect, the daemon reserves hard
nonterminal/terminal-receipt capacity and immutable
`CompanionActionIntent(action_id,operation_id,mapped_operation,canonical_request_hash,principal/client/session/
surface/scope/revisions,expiry)`. Its closed reducer is `prepared→dispatching|refused|expired`,
`dispatching→submitted|reconcile_required`, `submitted→resolved|reconcile_required`, and
`reconcile_required→resolved|not_applied|reconcile_required`; terminals are refused/expired/resolved/not_applied.
The canonical request reuses the fixed operation id. Recovery queries its canonical receipt and never
redispatches. `CompanionActionReceipt` projects the intent plus canonical receipt/outcome; identical replay
recovers it and changed-payload, expired, offline or old-session replay is denied.
Each right-hand operation uses its canonical exact fields and revisions; an unknown tag or
direct alias is denied. Free text is valid only for a verified non-authorising question/decision. Interrupt
targets one exact AttemptOwner/attempt/binding generation. Writer-lease request creates only a visible expiring
handoff proposal and can neither seize a lease nor write bytes. Credential/password entry, grant administration,
host trust/rotation, broad stop/kill/destroy, checkout integration and publish/merge have no Companion shape.
Non-sensitive free-text drafts may remain encrypted only on that client pending resnapshot; permission drafts,
grants and permission mutations are destroyed/invalidated on disconnect and never replay offline.

`launch_allowlisted_agent` is the one deliberate creation exception and does not widen Companion into a
general creator. A local foreground operator must first persist `CompanionAgentLaunchGrant` for one exact
Workspace/Session/ExecutionTarget and≤32 immutable entries, each binding Template revision, dedicated adapter,
AccountProfile revision, optional fixed model, canonical safe-cwd root and read-only-or-new-isolated-worktree
checkout policy. The grant expires within24 hours and is `active→revoked|expired`; cloned/shared grant bytes,
an absent entry, changed revision, primary checkout, arbitrary cwd, command, env, flags, parent, model override
or provider credential is unrepresentable. At most64 active grants and≤2 MiB encoded state are installation-
wide; grant N+1 or revoke/expiry changes no runtime.

An accepted launch preassigns NodeId, AgentInstanceId, RuntimeAttemptId and CheckoutScopeId and persists one
Workspace-owned `CompanionAgentLaunchIntent` before checkout or process effect. Its reducer is
`prepared→provisioning|refused|cancelled`, `provisioning→launching|failed|reconcile_required`,
`launching→registered|failed|reconcile_required`, and
`reconcile_required→registered|failed|reconcile_required`; terminals never reactivate. Graph registration and
launch use the canonical create-agent/checkout receipts under those same reserved ids; recovery only looks up
those receipts and never launches/registers twice. One nonterminal intent/grant,64 globally and10,000 rich
records≤8 KiB/≤64 MiB are hard, giving8,192 maximum records at the independent byte boundary; active,
terminal, replay, checkout, graph, runtime, journal and recovery capacity reserve atomically. Revocation fences
new launches but does not kill an already registered agent. The resulting Node is ordinary canonical hierarchy
state with visible `created_via=companion`; it has no hidden mobile mirror or parallel project registry.

`RemoteOperatorSurface` is a full protocol client role, not a CompanionAction superset and not a WebPreview or Browser Node
origin. Its invitation/session capability is distinct from the local administrative token, binds authenticated
origin, operator, Workspace/scope, surface/connection generation, expiry and negotiated view/mutation/input
capabilities, and is transported only over authenticated encryption with anti-replay. HTTP mutations require
same-origin CSRF proof; WebSocket upgrade validates the exact Origin and capability before any snapshot.
Tokens never enter URL/query, logs, referrers or browser persistent storage. WebPreview and Browser Node
content runs in separate isolated origins and cannot reach this channel.

Remote authority has closed lifecycle. `RemoteInvitationState` is `prepared|active|consumed|expired|revoked|
invalidated`, with prepared→active/revoked/expired/invalidated, active→consumed/revoked/expired/invalidated;
consumed/expired/revoked/invalidated are terminal. `RemoteClientState=connecting|active|disconnected|revoked|expired` permits only
connecting→active/disconnected/revoked/expired, active→disconnected/revoked/expired and disconnected→
connecting/revoked/expired; revoked/expired are terminal. `RemoteSessionState=negotiating|active|disconnected|
revoked|expired` permits only negotiating→active/disconnected/revoked/expired and active→disconnected/revoked/
expired; disconnected/revoked/expired are terminal. Reconnect mints a new RemoteSessionId and connection
generation. Client revoke/expiry cascades to every child session, grant, lease and subscription. Any child
session transition from negotiating or active to disconnected/revoked/expired—unless caused by a terminal-
client cascade—invalidates descendants of that session and atomically moves an otherwise connecting/active
reusable client to disconnected; it does not terminalise the client.
LocalDesktopForegroundAuthority alone may
create/revoke invitations and list/get/revoke client/session records; the two authenticated bootstrap sagas
below are the only remote paths that create/advance their preassigned client/session identities.

`create_remote_invitation` persists prepared with the one-time-secret verifier, safe public metadata and hard
deadline no later than 600 seconds; successful keystore-backed secret generation atomically activates it before the secret is returned
once. Crash/restart while prepared invalidates it. No client or timer activates an invitation.

Invitation creation preassigns RemoteClientId and RemoteSessionId. `redeem_remote_invitation` carries stable
RemoteRedemptionId, those ids, invitation id/revision, authenticated origin, device public key, closed
`full_gui|headless_status|companion` role, Workspace/Session scope and manifest hash. One transaction consumes
the invitation, creates/advances the preassigned client/session and persists `RemoteRedemptionReceipt`; only
then may it return session material. Concurrent redemption has one winner, identical replay recovers the same
redacted receipt/session, and a lost response never consumes another invitation or mints another identity.

Exactly one negotiating/active RemoteSession may belong to a RemoteClient. After disconnect,
`open_remote_session` carries RemoteSessionOpenId, current RemoteClientId/revision, authenticated origin/device
proof, same-or-narrower role/scope and current manifest hash. It atomically reserves a new RemoteSessionId,
moves the client disconnected→connecting and persists `RemoteSessionOpenReceipt`; successful handshake moves
client/session active together. Same-id replay recovers it. Concurrent opens, a live session, widened scope/role
or stale client fail before reservation; the predecessor remains terminal and its descendants stay invalid.

Redemption/open starts a 60-second device-key challenge. Verified proof plus manifest negotiation atomically
moves client connecting/session negotiating→active. Failure, timeout, daemon-generation change or transport
loss moves both to disconnected and terminalises the session; active-session disconnect atomically moves an
otherwise nonterminal client active→disconnected. Only device public key and credential-verifier hash/generation
are durable, never bearer material. Same redemption/open id with fresh device signature may rotate the verifier
and return one replacement ephemeral credential for the same reserved identity, invalidating the prior
generation without minting a new client/session.
An active session expires no later than 86,400 seconds; expiry or explicit session revoke atomically moves an
otherwise active client to disconnected, after which a new device-key-authenticated open is required. Client
revoke/expiry remains terminal and cascades instead.

`RemotePresence=(RemoteClientId,RemoteSessionId,surface_id,WorkspaceId)` is ephemeral and revisioned. Its state
is `present|idle|typing`, with optional authorised selected ViewTarget and expiry ≤30 seconds.
`update_remote_presence` revalidates the active session, scope and target. Snapshot and
`remote_presence_changed` expose it only to authorised peers; disconnect/expiry emits a live tombstone. It is
not durable/offline-replayed and cannot navigate another surface, mark read, acknowledge, type, lease or control.

`PresenceChatMessage` is a separate ephemeral human-collaboration overlay, not Presence state, AgentMessage,
ContextPacket, input, StatusEvent or Attention. The `send_presence_chat` Workspace scope mandatorily includes
one exact currently authorised `ViewTarget+view_revision`; retract and every push repeat it. One current message
may exist per exact active full-GUI remote client/session/Workspace/Surface/connection+ViewTarget/revision; a new same-owner message atomically replaces it only after the
new count/byte reservation, and explicit retract names its exact generation/revision. The body is sanitised
single-paragraph UTF-8≤512 bytes/256 scalars inside a≤1-KiB complete item; the installation admits≤128
messages/≤128 KiB. Each client may accept at most four sends per rolling10 seconds and consecutive sends are
at least500 ms apart; excess is typed rate-limit with zero peer projection. Hard expiry is30 seconds.
Disconnect, session/client revoke/expiry, Workspace/scope/Surface/connection/ViewTarget-revision loss,
replacement, retract or TTL
emits an authorised live tombstone then releases. Reconnect/offline history inherits nothing. Authenticated
encryption and the existing nonce fence precede admission; content is absent from store, journal, diagnostics,
export, notifications and crash data. Rendering grants no navigation, focus, input, context, resolution,
acknowledgement or authority, and muted/hidden overlays do not change canonical Presence or Attention.

After negotiation a remote client consumes daemon pushes and sends separately named requests. Remote dispatch
is default-deny. The global versioned `OperationRegistry.vNext` gives every exact name a
`direction=client_request|daemon_push` and, for requests, one
`effect_class=pure_read|subscription|navigation|ephemeral_collaboration|cursor_mutation|domain_mutation|input|denied`.
Its canonical sorted manifest hash is returned in the handshake and covered by compatibility tests. An absent/
new name is denied until a new version classifies it. The block below is only the closed
`RemoteOperatorSurfaceNonDenied.vNext` projection; it is neither the global registry nor evidence that a
LocalDesktop-, broker-, verifier- or policy-only operation is unreachable. Pure reads omit MutationEnvelope; subscriptions bind
scope/generation/bounds; navigation carries the full MutationEnvelope and exact owning Surface/connection plus
Installation StateStream/Surface revision even when its only effect is presentation; `ephemeral_collaboration` carries active encrypted
client/session/Workspace/Surface generations, anti-replay nonce, bounded TTL/rate and no durable/offline replay;
cursor/domain/input classes likewise carry the complete applicable revisions and MutationEnvelope. The complete remote
non-denied client-request sets are:

```text
pure_read = get_state_snapshot, get_hierarchy, get_hierarchy_page, reveal_hierarchy_key,
            get_node_view, get_flow_definition, get_flow_run,
            get_runtime_continuity, get_conversation_adoption,
            reconcile_conversation_adoption, open_file_for_edit, list_work_item_sources,
            query_work_items, query_conversation_inventory, read_conversation_title,
            get_runtime_attachment_operation, get_runtime_lifecycle_operation, get_runtime_configuration_operation,
            get_runtime_interrupt_operation, get_runtime_launch_operation,
            get_pty_capacity,
            list_native_jobs, get_native_job, get_account_activity,
            get_permission_response_receipt, list_directory, get_commit_graph,
            get_commit_changed_files, get_repository_status, get_repository_diff,
            list_repository_branches, get_repository_conflicts, get_file_save,
            get_runtime_inventory, get_resource_inventory,
            get_repository_mutation, get_web_preview_load,
            get_browser_node_creation, get_browser_navigation,
            get_browser_download_quarantine,
            get_media_import, get_document_print,
            list_repository_host_profiles,
            get_repository_host_profile, get_commit_proposal, get_transfer,
            get_command_catalogue, search_command_catalogue, list_announcements,
            get_announcement, get_application_update, query_work_item_activity,
            get_presentation_history
subscription = subscribe_state_stream, unsubscribe_state_stream,
               subscribe_node_view, unsubscribe_node_view,
               subscribe_resource_inventory, unsubscribe_resource_inventory,
               subscribe_account_activity, unsubscribe_account_activity,
               subscribe_live_notification_status, unsubscribe_live_notification_status,
               watch_directory, unwatch_directory,
               subscribe_work_item_activity, unsubscribe_work_item_activity
navigation = set_tree_expanded, set_tree_expanded_all, select_tree_node, attach_pane, resync_pane,
             set_tree_presentation,
             set_surface_view_mode, set_inspector_width, set_board_presentation,
             set_terminal_appearance, update_surface_activity, route_attention,
             search_view_text, move_text_search_cursor, close_text_search,
             detach_runtime_view,
             open_document_view, control_document_view,
             control_media_playback, set_content_projection, clear_content_projection
ephemeral_collaboration = update_remote_presence, send_presence_chat, retract_presence_chat
cursor_mutation = open_surface, retire_surface, ack_state_revision, close_hierarchy_scan, close_web_preview,
                  close_document_view,
                  close_directory_scan, close_command_catalogue_scan, close_file_edit
domain_mutation = activate_session, create_agent_instance, adopt_conversation, attach_runtime_attempt,
                  resume_agent_instance,
                  branch_agent_instance, switch_agent_configuration,
                  reconcile_runtime_attachment_operation, reconcile_runtime_configuration_operation,
                  reconcile_runtime_interrupt_operation, reconcile_runtime_launch_operation,
                  create_runtime_node, create_resource_node, create_group, create_browser_node,
                  update_resource_node, set_group_membership, move_group_subtree,
                  load_web_preview, reconcile_web_preview_load,
                  navigate_browser, browser_back, browser_forward, reload_browser, stop_browser,
                  clear_browser_storage,
                  open_reviewed_browser_popup, accept_reviewed_browser_download,
                  reconcile_browser_node_creation, reconcile_browser_navigation,
                  discard_browser_download_quarantine,
                  reconcile_browser_download_quarantine,
                  create_work_item, update_work_item_metadata,
                  save_file_edit, reconcile_file_save,
                  stage_repository_paths, unstage_repository_paths,
                  commit_repository, fetch_repository, create_repository_branch,
                  switch_repository_branch, reconcile_repository_mutation,
                  mark_node_result_read, acknowledge_attention,
                  snooze_attention, dismiss_attention, set_dependency_edge,
                  remove_dependency_edge, create_team, update_team,
                  create_flow_definition, version_flow_definition, preflight_flow_run,
                  start_flow_run, start_flow_step, pause_flow_run, resume_flow_run,
                  retry_flow_step, respond_to_agent_interaction,
                  launch_companion_agent,
                  ack_remote_permission_response_grant,
                  submit_remote_permission_response, interrupt_runtime_owner,
                  request_input_lease_handoff, prepare_media_import, commit_media_import,
                  put_media_import_chunk, cancel_media_import, reconcile_media_import,
                  generate_commit_proposal,
                  apply_commit_proposal_to_editor, discard_commit_proposal,
                  prepare_transfer, start_transfer, pause_transfer, resume_transfer,
                  put_transfer_chunk, get_transfer_chunk, cancel_transfer,
                  reconcile_transfer, invoke_command_catalogue_entry,
                  open_reviewed_content_projection_link, dismiss_announcement,
                  open_reviewed_announcement_link,
                  undo_presentation_operation,
                  redo_presentation_operation
input = acquire_input_lease, renew_input_lease, handoff_input_lease,
        release_input_lease, write_runtime_input, resize_runtime_input
daemon_push = state_snapshot, state_event, node_view_changed, terminal_output,
              status_event_changed, attention_changed, pending_interaction_changed,
              remote_permission_response_grant_changed,
              permission_response_receipt_changed, input_lease_handoff_changed,
              remote_presence_changed, presence_chat_changed, directory_changed, resource_inventory_changed,
              account_activity_changed, live_notification_status_changed,
              web_preview_changed, browser_download_changed, media_import_changed,
              commit_proposal_changed, transfer_changed, announcement_changed,
              application_update_changed, work_item_activity_changed,
              presentation_history_changed
```

The eight non-Watch subscribe families use daemon-minted memory-only `LiveSubscriptionId`s in one registry.
Canonical ownership is connection generation plus tagged state-stream, Surface+NodeView, ResourceScopeKey,
local-admin Surface+TargetRecovery, AccountProfile source, NotificationEndpoint+scope, WorkItem source key or
local-foreground DiagnosticLogGeneration+filter.
Identical subscribe returns the existing id; a changed request replaces atomically only
after reservation. Hard limits are 64/connection, 4,096 installation,≤4-KiB metadata/record and≤16 MiB metadata
aggregate; queues are≤64 events or1 MiB/subscription,≤256 events or8 MiB/connection and≤4,096 events or64 MiB
installation-wide, also charged to the shared variable-RSS pool. Admission reserves every count/byte and one
gap marker before producer registration. Terminal gap, unsubscribe, scope/source/owner/Surface/session/
connection/process loss releases; reconnect inherits none. DirectoryWatch retains its separate count limits but
charges the same queue/shared-memory pools.

`invoke_command_catalogue_entry` is only a typed indirection: after resolving the exact current entry, the
daemon applies the canonical operation's own effect class, role allowlist, LocalDesktopForegroundAuthority,
consequence review, object/capability revisions and every other policy as if called directly. The wrapper can
never lower authority or make an unavailable operation invocable. Credential, destructive and integration
entries therefore remain denied to headless/Companion and require the same local/full-GUI authority they
normally require.

`daemon_push` names are never valid client requests. Registry membership is necessary but insufficient. The
closed role matrix is: `full_gui` may use every non-denied registry member that is also in its invitation
capability set; `headless_status` may use pure_read, subscription, cursor_mutation and navigation sets only;
`companion` may use `open_surface`, `retire_surface`, `get_state_snapshot`, state subscribe/unsubscribe,
`ack_state_revision`, the nine
CompanionAction mappings, scoped `get_account_activity` and account-activity subscribe/unsubscribe, its own
`get_permission_response_receipt`, `update_remote_presence`, and `ack_remote_permission_response_grant` only
for its exact grant/session. A
Companion supplies only its action tag/envelope and cannot bypass translation by naming the right-hand
operation directly. The invitation's exact Workspace/stream/object/capability scope, role matrix,
MutationEnvelope and server policy must all allow the request. In particular,
the six scoped recovery families for WebPreview load, Browser creation/navigation/quarantine, FileSaveIntent
and RepositoryMutationIntent are not authority escalation: a `get_*` requires the exact object scope, while a
`reconcile_*` additionally requires the original creator or explicit invitation recovery capability, original
operation/owner/target revisions and performs lookup only. Reconnect may recover the same record; it cannot
name another client's intent, repeat the underlying effect or widen endpoint/repository/file authority.
The only pre-session bootstrap frames are `redeem_remote_invitation` and `open_remote_session`. They sit outside
the post-negotiation registry, use their exact invitation/device or disconnected-client proofs, stable id,
canonical payload fingerprint, hard rate/attempt/expiry bounds and durable replay receipt, and grant no
registry access until client/session negotiation becomes active. Every other pre-session name is denied.
`respond_to_agent_interaction` is limited to a verified non-authorising schema and
`submit_remote_permission_response` to the exact grant below. Every other operation—including credential/
secret entry, permission-response grant issue/revoke, AccountProfile/auth/default or
ModelEndpointProfile/default/discovery, DeliveryGrant/notification administration,
DelegationGrant/root-context/message authority, ExecutionTarget trust/administration, WorkspaceOnboarding,
CheckoutScope removal/rehome, destructive container/provider/job/runtime delete/cancel/abort/stop/terminate,
RuntimeInventory/ResourceInventory/`WorkspaceSemanticRecoveryInventory`/`InstallationSemanticRecoveryInventory`/
`TargetRuntimeRecoveryInventory` enumeration or control, repository credential/profile/grant
administration, push/pull/commit-and-push, merge/conflict resolution, discard/cleanup and `publish_repository`—
is server-refused even if a client forges its wire shape. Scoped stage/unstage/commit/fetch/branch operations
explicitly present in the registry are not part of that denied set. This denial does not include the explicitly listed
`cancel_media_import` or `cancel_transfer`: an authorised full-GUI client may cancel only its exact scoped,
revision-fenced saga, which publishes nothing and grants no lifecycle/provider authority.

`LocalDesktopForegroundAuthority` is an authenticated visible native desktop surface/client/connection that
remains OS-foreground at the daemon's serial validation point. Voice capture/worker, hook, NotificationHost,
HUD/headless, RemoteOperatorSurface, Companion, Browser and WebPreview are excluded. It alone may issue,
inspect or revoke a response grant and call `submit_local_permission_response`.

`RemotePermissionResponseGrant` is immutable. It binds GrantId/revision, remote role/client id+revision,
RemoteSessionId/session revision/session expiry, surface/connection generation, provider/profile,
Workspace/Session, Node/AgentInstance, exact AttemptOwner/RuntimeAttempt/
InputRoute/binding generations, PendingInteraction id/revision, permission-fact revision, typed transport
generation, closed offered option ids, minimal bounded consequence metadata, issued time and hard-bounded expiry.
Its closed state is `active → consumed|revoked|expired|invalidated`; all destinations are terminal. Expansion
has no operation: revoke and issue a new id. Independent delivery state is `pending_ack|acknowledged|failed`.
Issue returns a local receipt and E2EE-pushes the capability/metadata only to the exact grantee generation;
`ack_remote_permission_response_grant` activates its use. Delivery failure, disconnect/reconnect, interaction/
attempt/binding change or capability downgrade invalidates it. Remote clients cannot list/get grants. A grant
may target only full_gui or companion, never headless_status. Issue atomically checks that no
PermissionResponseClaim exists and loses to a concurrent claim. It also CASes unique
`RemotePermissionGrantIssueKey=(PendingInteractionId,interaction_revision,RemoteClientId,client_revision,
RemoteSessionId,session_revision)`; concurrent issues for one grantee yield one active grant, same-operation
replay returns it and changed operation/payload conflicts. It requires the exact current
`InputSafetyState=sensitive_interaction(class=permission)`; credential, host-trust,
grant or destructive-confirmation classes cannot be relabelled into a permission grant.

Delivery has the closed reducer `pending_ack→acknowledged|failed`; acknowledged and failed are terminal.
The only valid grant/delivery pairs are `(active,pending_ack|acknowledged)`, `(consumed,acknowledged)` and
`(revoked|expired|invalidated,acknowledged|failed)`. Ack races terminal grant transitions atomically, failed
delivery invalidates the grant, consumption requires acknowledged, and no terminal grant remains pending_ack.

`submit_local_permission_response` carries no grant. `submit_remote_permission_response` repeats the exact
grant/client/session/surface/connection, nonce, owner/route/binding, PendingInteraction/option, InputSafety
revision, permission-fact revision and typed-transport generation in its authenticated encrypted envelope.
The daemon revalidates fresh `permissions=supported+typed` and all revisions.

PendingInteractionId is installation-minted and never reused. Each current RuntimeAttempt pre-reserves eight
Workspace-owned≤8-KiB nonterminal interaction slots and their Attention entries before spawn. The installation
admits at most100,000 nonterminal interactions/768 MiB and100,000 terminal receipts, each≤4 KiB/384 MiB;
the independent terminal byte boundary admits exactly98,304 maximum receipts while the count boundary uses
smaller receipts. There are80,000 interaction slots
and their full625-MiB worst-case encoding are a dedicated reservation partition for the10,000 admitted live
RuntimeAttempts, while the remaining20,000 count slots/143 MiB serve non-attempt and admission-race headroom
(18,304 additional records at the full8-KiB item bound). A
reservation and its later materialised record are one charge: materialisation consumes the token without
double-counting or seeking new capacity. A distinct ninth prompt
cannot overwrite another. It is backpressured or, for an unpausable provider, produces the attempt's reserved
typed-response-disabled observability-gap Attention while preserving provider/terminal evidence. Terminal
richness compacts after 180 days only after id/attempt/input-route/option/claim/replay fences are durable;
nonterminal/claimed/submitted/possible-effect records never age out.

The Installation Attention Queue admits200,000 unresolved/snoozed/dismissible≤4-KiB entries/768 MiB and
200,000 terminal route/mutation receipt slots/768 MiB retained180 days. Count and byte caps are independently
reachable:196,608 maximum-size records fill bytes before count, while smaller records fill count. Every
effect-capable demand producer reserves its entry+receipt before admission;90,000 entries/351.5625 MiB are a dedicated reservation partition for each admitted
RuntimeAttempt's eight normal routes plus one observability-gap route, leaving110,000 count slots/416.4375 MiB for all
other declared producers and admission races. Reservation and materialisation are one charge. N+1 producer
admission refuses before effect, exact dedup alone replaces in place, and active entries never compact. A
provider cardinality overrun uses only its reserved actionable gap and never drops or fabricates an exact
interaction.

Local typed, remote typed and
`verified_local_permission_fallback` all contend on the durable unique
`PermissionResponseClaimKey=(AttemptOwner,RuntimeAttemptId,attempt_generation,InputRouteId,route_generation,
PendingInteractionId,interaction_revision)`. The immutable claim
contains ClaimId, operation id, path, selected provider option, every route/binding/safety/fact/transport
generation and optional exact GrantId. Before the first possible provider effect, one transaction reserves one
hard nonterminal slot and one terminal-receipt slot, CASes absence→claimed, persists a prepared
PermissionResponseReceipt and, for remote, CASes active→consumed; every
sibling grant for that interaction becomes invalidated. Local-vs-local, local-vs-PTY, two-client and
different-option races therefore have exactly one winner. Same-id replay returns the first receipt; every
other operation/path/option loses. Rejection, disconnect, uncertainty or crash never releases the claim, and
claim/tombstone compaction waits for terminal interaction+receipt and expiry of the anti-replay retention.
Capacity exhaustion refuses before claim/effect and raises one bounded system Attention; active/uncertain
records never exceed the hard bound.

`PermissionResponseReceipt` has independent closed axes:
`PermissionDispatchState=prepared|effect_armed|definite_no_effect|submitted|possible_effect` and
`PermissionEvidenceState=not_started|pending|not_applied|resolved|cancelled|attempt_ended|reconcile_required`. Prepared
moves through a durable pre-effect boundary. The only valid pairs/transitions are
`(prepared,not_started)→(effect_armed,not_started)`;
`(effect_armed,not_started)→(definite_no_effect,not_applied)|(submitted,pending)|
(possible_effect,reconcile_required)`; `(submitted,pending)→(submitted,resolved|cancelled|attempt_ended)`;
provider-proved rejection may also move submitted/pending→submitted/not_applied; and
`(possible_effect,reconcile_required)→(possible_effect,not_applied|resolved|cancelled|attempt_ended)`. No other
cross-product is representable and only correlated provider evidence may leave pending/reconcile-required.
Provider call or PTY enqueue is forbidden before effect_armed commits. Recovery of effect_armed after a daemon
generation change first writes possible_effect/reconcile_required and performs provider lookup or PTY input-
sequence journal reconciliation without dispatch. Prepared can continue only through identical explicit replay.
`get_permission_response_receipt`
recovers state and `reconcile_permission_response` performs lookup only, never redispatch. Attention closes
only from evidence, not transport submission. Denial is simply one provider-offered typed option; there is no
approval alias or free-form credential path.
Definite-no-effect/not-applied retains the claim tombstone; retry requires a fresh provider-observed
PendingInteraction revision and cannot reuse the old claim.

Input has a separate revisioned guard. `InputSafetyState=ordinary|non_authorising_interaction|
sensitive_interaction|unknown`; every variant includes exact InputRoute, AttemptOwner, RuntimeAttempt/binding
generations, safety revision, permission-fact revision and transport generation, and interaction variants add
id/revision. Sensitive class is `permission|credential|host_trust|grant|destructive_confirmation`. Reconnect,
stream gap, owner/binding/attempt replacement, stale/degraded/unknown permission fact or unclassified semantic
state becomes unknown until fresh evidence. `write_runtime_input` atomically revalidates safety with byte enqueue.
Ordinary accepts only tagged lease-fenced bytes. A recognised permission with fresh supported typed transport
blocks raw bytes everywhere and uses the two typed operations. Fresh supported verified-local-PTY transport
allows only the tagged LocalDesktopForegroundAuthority fallback: request carries the exact option and revisions,
acquires that same PermissionResponseClaim before enqueue, and the versioned daemon encoder rederives/compares
bytes. Typed↔PTY losers accept zero bytes. Unsupported,
degraded, unknown, stale/expired or none enables neither semantic response. Every remote, Companion, voice,
hook and background path lacks PTY fallback. A provider that cannot classify prompts advertises no semantic
permission transport.
Generic Shell/TUI raw input cannot promise command-level safety: granting it is explicitly labelled
`raw_terminal_execution_authority` and permits every command available inside that runtime's enforced
sandbox. Server refusal guarantees apply to Turn's typed control-plane operations, not to arbitrary commands
the invited terminal writer can execute. The UI and audit must disclose that distinction before grant.

Subscriptions retain normal byte/count/backpressure bounds; reconnect/gap requires resnapshot, offline drafts
never replay, local revocation closes streams and leases, and every mutation/denial is visible in the local
audit surface. Headless clients use the same role and objects without claiming rendering support.

`RuntimeBackend`, `FileBackend` and `RepositoryBackend` requests use disjoint capability tags. File and
repository operations bind ExecutionTarget host/generation, confined root/repository id, expected revision
and operation id; repository verbs are the closed list in the product contract. Wrong/offline remote targets
return scoped refusal/uncertain receipts and cannot resolve or mutate a same-named local target.

### Bounded tool, distribution and presentation records

The following vNext shapes are closed and use the request table above:

- Turn-owned memory admission has a non-borrowable≤512-MiB daemon/GUI/client-core reservation and one≤1,024-
  MiB shared variable pool. Every snapshot, projection, queue/buffer, renderer/partition, decoder, SpeechWorker
  and helper working set atomically reserves its family limit and this shared pool before effect. Family maxima
  are independent ceilings, not simultaneously additive; shared N+1 refuses or parks before read/spawn/load/
  subscribe/decode/dispatch and no post-hoc process kill counts as admission control.
- `AuxiliaryWorkerOwnerKey` is exactly the nine-tag union declared beside `StateFamilyManifest.vNext`.
  NotificationHost/NotificationDelivery/RemoteTransport/ContextBrokerRemoteRead/Transfer/Updater/
  ProviderBroker/ProviderCollector/Watchdog allow at most1/32/128/128/32/1/32/32/64 respectively, while the
  cross-kind family allows128 live-or-cleanup-pending workers,≤128 MiB each and≤1,024 MiB total. All kind,
  family and shared reservations plus cleanup capacity precede any process/task/socket/source/network effect;
  cancellation or owner loss revokes I/O and releases only after quiescence, otherwise the exact reservation
  remains in `ProcessCleanupCharge`.
- A RuntimeAttempt is not automatically a resident terminal. At most 128 live-or-retained
  `TerminalRuntimeState`s exist installation-wide. PTY launch reserves one state, its complete 2-MiB raw ring
  and a 4-MiB current-grid allowance before process effect, so the 128-state boundary is reachable within the
  shared pool; the 129th launch first drops only an eligible stopped/unpinned state with a complete durable
  checkpoint, otherwise it refuses without a process. `TerminalScreen` retains≤5,000 scrollback rows and≤8 MiB
  per state/≤512 MiB family-wide, including the reserved current grid; pressure evicts oldest unpinned
  scrollback before current cells and never reports discarded history as complete. Raw rings are exactly≤2 MiB
  each/≤256 MiB aggregate and retain their explicit truncated boundary. One daemon `TerminalImageStore` holds
  ≤16 payloads/16 MiB per terminal and≤512 MiB aggregate. Incoming image growth reserves family+shared bytes,
  evicts only unplaced least-recent payloads and, if still full, emits the bounded visible image-refusal notice
  while continuing PTY text; it never blocks or kills the producer. One client cache per visible Surface/Pane
  holds≤12 payloads/12 MiB, with≤4 caches/connection,≤64 installation-wide and≤256 MiB aggregate; eviction
  yields a placeholder/refetch and owns no terminal truth. Every terminal family also charges the shared pool.
  Attempt end keeps a stopped state only while a Pane/view/search pin or incomplete checkpoint requires it;
  final checkpoint+unpin, Pane deletion, exact owner deletion or daemon loss releases it, while a live PTY is
  never evicted merely to admit another.
- Terminal image processing never hides pre-retention memory. At most eight `TerminalImageScanBuffer`s retain
  one≤8-MiB encoded sequence (≤64 MiB aggregate); at most eight multipart `TerminalImageChunkAssembly`s retain
  one≤8-MiB Kitty body (≤64 MiB); and at most two `TerminalImageDecodeWorkingSet`s reserve≤128 MiB each/
  ≤256 MiB family-wide before inflate/header decode/raster/downsample. That 128 MiB is the measured high-water
  for the complete operation while encoded/chunk input is still live: inflater output, decoder allocator,
  decoded raster, resize scratch and final RGBA are sub-allocations of it, not additive allowances; the decoder
  receives only the remaining budget and must reuse/free phases or refuse before exceeding it. A sequence start/header that cannot
  reserve enters≤512-byte discard-until-terminator state and emits one coalesced visible refusal; raw PTY text,
  input and Attention continue. Partial sequences, abort, malformed terminator, Attempt/buffer generation loss
  or process end release; decoded success atomically transfers only the final≤4-MiB RGBA payload into
  TerminalImageStore and frees every scan/chunk/decode byte. Races with 128 partial sequences or simultaneous
  bombs therefore cannot exceed family/shared RSS.
- One `PaneAttachment` is≤8 KiB;≤64/Surface,≤256/connection and≤4,096 installation-wide/32 MiB. A cells
  attachment owns one≤2-MiB `TerminalScreenProjectionBaseline` under a≤256-MiB family cap; byte pressure refuses
  a new/replacement attachment before changing the current baseline, while small baselines can reach the count
  cap. The authoritative per-terminal `TerminalOutputQueue` holds≤512 shared Arc chunks or8 MiB and all queues
  share≤4,096 chunks/256 MiB. Reader output always updates TerminalBuffer first; queue overflow drops oldest
  delivery chunks, records the exact first-missing sequence and makes every lagging attachment consume a
  terminal gap+automatic streamed resync. At most128 `TerminalPumpBatch`es exist, each≤16 frames/1 MiB and≤128 MiB aggregate;
  lack of a batch parks the projection, never the PTY, until it gaps/resyncs. Attach/detach/reselection/resize/
  gap/owner or connection loss retires its exact PaneAttachmentId+AttachmentGeneration, baseline and batch
  generation. Once its critical gap is admitted, that old generation emits no further cells or bytes. The
  client protocol runtime—not the operator—automatically requests `resync_pane`; success atomically installs a
  fresh attachment/baseline generation through `ChunkedResponseStream` when needed, and failure remains a
  labelled reconnecting projection while retry policy runs without a Start/repair control.
- `RuntimeViewReplayFence` is the closed `attach|resync|detach` union, Installation-owned and keyed by
  SurfaceId+SurfaceOwnerGeneration+monotonic SurfaceOperationSequence. Every variant freezes operation id,
  canonical request fingerprint, connection/Surface owner and the exact AttemptOwner/RuntimeAttempt/binding/
  PTY/buffer identities and generations. `attach` additionally freezes the current-or-absent attachment CAS
  input, committed new PaneAttachmentId+AttachmentGeneration+BaselineGeneration, exact screen snapshot
  sequence/range, stream kind, logical length/digest and sealed `ChunkedResponseStream` result descriptor.
  `resync` freezes the retired→new attachment/baseline identity transition and the same exact snapshot and
  sealed-result fields. `detach` freezes
  the retired attachment/baseline/batch identities and terminal `ack`; it creates no lifecycle receipt.
  Attach/resync CAS and fence commit, or detach retirement and fence commit, are one transaction. The same
  sequence+fingerprint returns the identical variant/receipt and never repeats CAS; changed request bytes
  conflict. While the exact baseline/chunk result remains live, attach or resync replay fetches and verifies
  that same logical body from its seal. After daemon restart or eviction it returns that same receipt/seal with explicit
  `body_unavailable`, discards every partial chunk, and the client runtime—not the operator—starts a new
  operation id against current generations: `attach_pane` when the old connection/attachment no longer exists,
  otherwise `resync_pane` (including a committed attach whose body alone was lost). That new operation owns
  its own CAS and fence; no replay substitutes a newer screen
  under the old digest and no Start/retry control appears.

  Exact minimal fingerprints do not fold into a scalar high-water, because that could not distinguish altered
  old bytes. The independent bounds cover all three variants and admit either 1,000,000 smaller installation-
  lifetime fences or exactly983,040 maximum≤512-byte fences/480 MiB; count and byte saturation are tested
  separately. Each admitted PaneAttachment simultaneously reserves its future detach variant, so≤4,096 live
  reservations are non-borrowable: saturation refuses attach/resync before its CAS and raises bounded Status,
  but never blocks eventual detach. Replay at the oldest retained sequence returns only the exact frozen
  variant for the exact fingerprint. Lost reply with a live body, restart/eviction with an unavailable body,
  wrong seal/digest, changed bytes, N+1 and automatic fresh-operation recovery are mandatory mutation cases.
- Every authenticated `ProtocolConnectionOutbox` is≤256 frames or8 MiB and all outboxes share≤4,096 frames/
  128 MiB; one frame is≤256 KiB including envelope. A logical large result uses `ChunkedResponseStream` above:
  ≤4 streams/connection,≤16 installation-wide,≤7,680 KiB/item and≤120 MiB aggregate. `pane_image` additionally
  admits≤8 fetches/Surface,≤32/connection and≤128 installation-wide, one logical RGBA body≤4 MiB and≤128 MiB
  aggregate. Stream bytes and TerminalImageFetch body are one allocation charged once to shared RSS; successful
  verification transfers it into the already-reserved≤12-MiB client cache without a second copy. Gap, hash/
  generation mismatch, image eviction, reselection, detach or disconnect discards it. One automatic refetch may
  occur for the same still-visible image/view generation; another failure leaves a labelled placeholder rather
  than looping or asking the user to press a start/retry control.
- GUI/client crossing state is not an implicit toolkit queue. Per `LocalClientInstanceId+ConnectionGeneration`,
  `ClientInboundQueue` admits≤64 messages, `ClientOutboundIntentQueue`≤256 not-yet-written intents and
  `ClientAwaitingRequestRegistry`≤512 written request identities. Each family is independently capped at
  4,096 installation-wide items,≤4 KiB/item and≤16 MiB; every item/family/shared-byte reservation precedes
  enqueue or socket write. Inbound reserves16/64 slots per client and1,024/4,096 globally for lifecycle,
  Attention, input receipts and scoped gaps; presentation deltas coalesce or become a scoped gap plus automatic
  snapshot. An outbound N+1 is not written and returns typed local backpressure; a written request is never
  silently removed to admit another. Disconnect/window loss drops inbound and not-written outbound state;
  awaiting mutation identities become reconciliation-required and are looked up by operation id, never replayed,
  while pure reads resnapshot automatically. A fresh connection inherits none of the three generations.
  `NativeDialogQueue` admits one≤4-KiB descriptor per local window/64 installation-wide/256 KiB and
  `CompanionActionDispatchQueue` one≤8-KiB descriptor per active RemoteSession/64 installation-wide/512 KiB;
  replacement is same-id CAS only, owner/session/window loss revokes it, and N+1 opens no dialog or remote effect.
- `TopologyObservationQueue` is owned by exact Workspace+source+observation epoch, admits≤1,024 events/source
  and≤4,096 installation-wide,≤4 KiB/event and≤16 MiB total, all additionally charged to shared variable RSS.
  Reservation precedes adapter/hook/process-inventory delivery. The first per-source/global/item/byte excess
  atomically records one durable gapped observation for that exact source/epoch, fences exact coverage and
  schedules a bounded asynchronous resnapshot; it neither blocks the producer nor keeps reporting an exact
  child count. Drain, gap retirement, source/epoch/attempt/Workspace loss or daemon loss releases after the
  in-flight event apply quiesces; a new epoch inherits no queued event.
- One `WebPreviewFetchCorrelation`≤8 KiB is reserved with each of≤32 live-or-cleanup
  WebPreviewLoadStates, under a≤256-KiB family bound. It contains only exact intent/URL-hash/policy/DNS/socket/
  renderer generations; bodies remain in the WebPreview body family. Close/source/ViewTarget/Node/Surface/
  connection loss or expiry fences new I/O, but correlation count/bytes release only with socket/worker/buffer
  quiescence and otherwise transfer to the existing cleanup charge. N+1 performs no DNS/network/read/render.
- `BrowserPage` is safe metadata, not DOM/page storage. Each of≤8 active renderers may own one current and one
  pending page generation, for≤16 records,≤32 KiB each and≤512 KiB family-wide plus shared RSS. A third
  generation refuses before renderer/network dispatch. Successful navigation atomically replaces current and
  releases the prior after renderer references quiesce; stop/failure/Node/partition/renderer/daemon loss
  discards pending state. DOM/script/storage/body bytes remain solely inside BrowserPartition/renderer caps,
  and a lingering renderer transfers the identical page slot/bytes to ProcessCleanupCharge.
- `BrowserLocalSnapshot` is memory-only and exactly
  `reserved→reading|discarded`, `reading→sealed|discarded`, `sealed→loaded|discarded`,
  `loaded→discarded`, with discarded terminal. Cancel/stop/open/read/hash/descriptor drift, replacement or
  Node/scope/owner/renderer/daemon loss takes the legal discard edge and releases its≤8-MiB item reservation
  from both the 32/256-MiB family limits and shared variable-RSS pool. No loss path restores bytes.

- `query_work_items` admits at most four `WorkItemSourceQueryBuffer`s/connection and32 installation-wide;
  each reserves≤2 MiB raw-provider/sanitisation working bytes under a≤64-MiB family plus shared-RSS charge
  before provider read. A page is a request-only safe summary projection:≤500 rows,≤2 KiB/row and≤1 MiB
  logical, whichever limit arrives first; body/comment/credential/provider payloads are absent. Its≤512-byte
  authenticated cursor binds source/project/all generations/filter/sort/page ordinal/predecessor digest and
  coverage is `complete|partial(next_cursor)|gapped(minimum_revision)`. The 501st/successor byte continues,
  never truncates to complete. Success frees raw bytes as the independently reserved response stream/outbox
  takes the safe body; failure/cancel/gap/30-second request deadline/connection loss frees them. N+1, oversize
  provider page or shared pressure refuses/gaps before further provider read and no WorkItemPage survives the
  request.

- `query_conversation_inventory` admits≤4 `ConversationInventoryQueryBuffer`s/connection and≤32 globally,
  each≤2 MiB/≤64 MiB family plus shared RSS before provider/cache read and with a30-second request deadline.
  One request-only page is≤500 safe descriptors,≤2 KiB/item and≤1 MiB logical; its authenticated≤512-byte
  cursor fixes provider/Profile/Target/namespace+generations, predicates, source revision, ordinal and
  predecessor digest. One query scans≤10,000 descriptors. The501st/next byte is partial; oversize raw provider
  page,10,001st candidate or stale cursor is partial/gapped/unavailable, never complete zero. Success frees raw
  bytes as response capacity takes the sanitised page; failure/cancel/deadline/disconnect releases after I/O
  quiescence and retains no query text/raw page.
- Private transcript body search is local-desktop-only and does not reuse the metadata inventory buffer.
  One `PrivateTranscriptSearchRefreshQueue` exists per enabled exact profile/target/namespace index and≤8
  globally; it admits≤256 source identities/2 MiB each and≤64 MiB across the family before opening a source.
  One document read is≤5 MiB and contributes≤200 KiB normalised text; one index is≤10,000 documents/512 MiB,
  with≤1 GiB installation-wide encrypted storage inside `account_private_root`. `PrivateTranscriptSearchQueryBuffer` is≤2/Surface and≤32
  globally,≤2 MiB each/≤64 MiB family plus shared RSS, and lives≤30 seconds. A request-only page is≤20 hits,
  ≤4 KiB/hit and≤80 KiB logical. N+1 or any source/index/query byte boundary changes coverage to partial or
  refuses before another transcript body read; it never evicts another profile's index, reports a complete
  empty result, starts a conversation or takes capacity from terminal input/Attention.
- `list_native_jobs.begin` mints a `NativeJobScanId`;≤8 scans/connection and≤512 installation-wide retain
  only≤32-KiB source/generation/watermark/cursor metadata each/≤16 MiB family for60 seconds idle. Each page read
  also reserves one of≤4 `NativeJobPageBuffer`s/connection and≤32 globally,≤2 MiB/item/≤64 MiB family plus
  shared RSS for≤30 seconds before provider read. A request-only page/scan is≤500 safe jobs,≤2 KiB/item,
  ≤1 MiB logical and≤10,000 observed jobs with an authenticated≤512-byte chained cursor. Final complete page,
  close-by-finalisation, gap/source generation loss/disconnect/TTL releases the scan; response completion/
  failure releases page bytes. N+1 and oversize provider input have zero new observation and cannot evict a
  live scan or claim complete absence.
- `WorkItemActivityPage` is request-only:≤200 events,≤8 KiB/event and≤1 MiB logical, whichever limit arrives
  first, with an authenticated≤512-byte WorkItem/revision/checkpoint/order cursor and
  `complete|partial(next_cursor)|gapped(checkpoint)`. Count saturation uses small events; byte saturation uses
  exactly128 maximum events. Page bytes are owned only by response stream/outbox and retain zero state.

- `DirectoryPage.begin` has no client scan id; the daemon mints DirectoryScanId and pins the actual target/
  trust/root/directory identity+revision. `continue` fixes that id/revision, next page sequence, predecessor
  cursor digest and opaque cursor under complete/partial/gapped coverage. `DirectoryWatchId` begins at a
  complete revision and gaps on overflow,
  cursor loss or generation change. Entries never follow aliases. One directory entry is≤2 KiB and a page is
  ≤2,000 entries/4 MiB logical including envelope; the 2,001st uses a continuation cursor, never truncates to
  complete. The state-bearing `DirectoryScan` is metadata only:≤16 KiB each,≤16/connection and≤1,024/16 MiB
  installation-wide. Its current pinned revision, next sequence and predecessor digest replace in place after
  response commit; page bytes exist only in the generic response stream/outbox.
- `CommitGraphPage` is a request-only value of≤500 nodes,≤2 KiB/node and≤1 MiB logical from a≤10,000 traversal;
  `CommitChangedFilesPage` is a request-only value of≤1,000 rows,≤2 KiB/row and≤2 MiB logical. Their≤512-byte
  authenticated cursors encode exact RepositoryId/revision/root-or-commit/order/next offset and retain no
  server state. Both report cycles, missing objects, oversize rows and stale revisions as explicit gaps with
  zero mutation; bodies above192 KiB use the generic atomic response stream.
- Live pins are exact-owner bounded: 16 DirectoryScans per connection/1,024 installation-wide with 60-second
  idle TTL and≤16-KiB item/16-MiB family metadata; eight CatalogueScans per connection/512 installation-wide
  with≤32-KiB item/16-MiB family metadata and 30-second idle TTL; and eight TextSearchSessions per Surface/512
  installation-wide with≤16-KiB item/8-MiB family metadata and 15-minute idle TTL. Final complete page, explicit
  close, invalidation, owner disconnect or TTL expiry releases the pin. N+1 refuses after eligible expiry;
  later use of a released id is gapped/stale and reconnect never inherits it.
- DirectoryWatch admits 32 per connection/2,048 installation-wide only from a complete current scan and
  reserves its≤8-KiB metadata/16-MiB family bytes plus count+gap-event capacity before backend subscription.
  Explicit unwatch, first overflow/cursor-loss/
  source-or-target invalidation/generation-change gap or connection loss releases it. The terminal gap requires
  full resnapshot; a reconnect cannot inherit the id. Each N+1 refuses pre-subscription without evicting a live
  watcher.
- `TextSearchSession` fixes a surface and either TerminalTextRevision (owner, buffer generation, retained seq
  interval, cell-grid revision) with logical-line/cell ranges or text document revision+hash with UTF-8 scalar-
  boundary ranges. Query≤4 KiB, results≤10,000, text scan≤16 MiB and terminal scan≤1,000,000 cells plus
  ≤100,000 logical lines, with cancellation and a scheduler yield after each ≤25 ms CPU slice. It retains only
  query/source/cursor/count metadata inside its≤16-KiB item, never the result set. Each request-only result page
  contains≤200 matches,≤1 KiB/match and≤200 KiB logical; the10,000 limit counts matches observed across the
  pinned scan and cannot allocate10,000 retained objects. Coverage is
  `complete|bounded(limit,cursor)` and continuation pins the identical source revision; only complete coverage
  may return global `no_match`. Movement carries `next|previous` plus `allow|stop` wrap and returns
  `match(index,count,wrapped)|no_match|stale`; eviction/reflow/source change invalidates it and no
  request writes terminal bytes or Attention.
- `CommitProposalProviderProfile` is installation-owned and bounded to 64 profiles with current plus 31
  historical revisions each; a referenced revision is retained and N+1 create/update refuses before current
  state changes. `CommitProposalAttempt` reserves one of 10,000 installation-wide nonterminal+terminal slots
  before helper/broker dispatch. Executable profiles share exactly two live-or-cleanup-pending sandbox-helper
  slots, each≤512 MiB and≤1,024 MiB family aggregate, also charged to shared variable RSS before spawn; a third
  Attempt waits without a process. Termination releases only after process-tree/descriptor/buffer quiescence or
  OS reclamation and otherwise transfers the same charge to ProcessCleanupCharge. A terminal Attempt compacts only after 30 days, terminal Proposal state and
  durable minimal operation/profile/repository/snapshot/result replay proof; nonterminal Attempts and terminal
  Attempts whose Proposal is nonterminal never age out, and delete cannot erase a referenced provider revision.
- `MediaImport` is `prepared→reading|cancelled`, `reading→validated|cancelled|refused|failed`,
  `validated→committing|cancelled`, `committing→committed|failed|reconcile_required`, and
  `reconcile_required→committed|failed|cancelled|reconcile_required`, with terminals final. It freezes
  descriptor-or-bytes, destination, preassigned NodeId, blob reservation/capacity, MIME/size≤256 MiB/hash/temp
  identity and rejects alias/TOCTOU/spoof/bomb evidence. `committing` is durable before publication; cancel or
  crash there records intent and reconciles the sealed temp/blob/Node binding without reread, recopied bytes or
  redispatch. Media playback is daemon-minted and ephemeral, owned by exact connection+Surface+Node/blob
  generation and contains `stopped|loading|ready|playing|paused|ended|error`, codec/container ids≤64 ASCII
  bytes or the closed error enum, elapsed/known-or-unknown duration, mute, 0..1000 volume and≤64 stable caption
  tracks (id≤64 bytes/32 scalars, BCP-47≤35 ASCII bytes, closed kind, label≤128 bytes/64 scalars)/selection,
  all within≤32 KiB encoded. One state/
  Surface, four/connection and 32 installation-wide are hard; decoder state is≤64 MiB/item and≤512 MiB family
  aggregate while also charging the shared variable-RSS pool. Begin/replacement reserves count+both byte pools
  before decoder read/spawn and swaps atomically. Stop/ended/error/source/selection/Node/Surface/connection
  invalidation fences the exact decoder generation and requests termination but retains state/count/family/
  shared charges in `cleanup_pending` until descriptor/process/thread/shared-buffer quiescence or OS-reclamation
  proof; decoder exit with that proof releases. Hung/uncertain cleanup remains recovery-owned and charged,
  cycling a Surface/connection cannot bypass it, End/delete uses its existing reservation, reconnect inherits
  no authority and restore never auto-loads.
  A pasted/client source uses one authenticated MediaImportStreamId with ≤4-MiB indexed/hash-checked chunks,
  exact total/hash, idempotent same-hash replay and explicit gap/backpressure; a descriptor source carries no
  wire body.
- `RepositoryPublishIntent` is ExecutionTarget-owned, one nonterminal per RepositoryId or canonical host/
  account/destination key, and freezes hosted authority, non-primary CheckoutScope/worktree/lease, destination/
  visibility/credential, source branch/tree/commit, expected remote ref, upstream/config and provider correlation.
  Every receipt carries monotonic highest_applied_phase. Its phase machine is prepared→creating_remote→
  remote_created→pushing→remote_published→configuring_upstream→published, with prepared cancellation/refusal,
  no_effect legal only before any remote creation, phase-specific reconcile-required, and terminal
  `partial(highest_applied_phase,reason)` after any proved prefix whose later phase cannot apply.
  Each phase persists its sealed postcondition before one effect; lookup-only reconcile never create/push/config/
  rotate/reacquire. Exactly256 nonterminal and10,000 total records, each≤8 KiB/64 MiB aggregate with8,192
  maximum records at the independent byte boundary, plus100,000 installation-lifetime replay fences each≤512
  bytes/48 MiB with98,304 maximum fences at the independent byte boundary reserve terminal/journal/correlation/
  recovery before effect; fence saturation refuses pre-effect, 180-day rich folding preserves every replay
  fence and partial/possible-effect evidence never ages out.
- `RepositoryHostProfile` carries canonical host, target/trust, account/scopes and credential reference under
  `draft→authenticating|revoked|deleted`, `authenticating→validating|degraded|revoked`,
  `validating→active|degraded|revoked`, `active→validating|degraded|revoked`,
  `degraded→authenticating|validating|active|revoked`, `revoked→authenticating|deleted`; deleted is terminal.
  Authenticate/rotate uses one Installation-owned `RepositoryHostCredentialIntent(kind)` per profile. It
  freezes operation/fingerprint, profile/host/account/scopes/target/trust/old+reserved-next credential
  generations, broker policy and provider correlation before effect under
  `prepared→dispatching|cancelled|expired`, `dispatching→awaiting_provider|refused|reconcile_required`,
  `awaiting_provider→credential_received|auth_failed|reconcile_required`, and
  `reconcile_required→credential_received|not_applied|auth_failed|reconcile_required`. Recovery is lookup-only;
  no correlation means unsupported. Rotate atomically degrades to rotation_pending and revokes all grants
  before dispatch; correlated receipt moves only to validating, explicit validation reaches active, and no
  grant auto-reactivates. Revoke fences late callbacks and delete waits for terminal intent. Exactly 10,000
  nonterminal+uncompacted intents, each≤4 KiB/32 MiB aggregate, with exactly8,192 maximum records at the
  independent byte boundary while the count boundary uses smaller records. They retain 180-day richness; N+1 count/bytes/
  terminal/recovery/next-generation refuses pre-effect.
  `RepositoryHostCapabilityGrantId` binds exact profile/target/trust/host/account/credential revisions, one
  `repository_backend|work_item_source` repository/project scope and expiry under
  `active→revoked|expired`; terminals never reactivate. At most 128 are active/profile; terminal ids fold into
  a durable non-reused id/generation/scope high-water under 100,000-record/256-MiB/180-day bounds.
  RepositoryBackend handles and host-backed
  WorkItemSources carry the matching profile+grant ids/revisions on every operation. The two grant kinds are
  not substitutable and profile revoke/delete atomically revokes both.
- `CommitProposalProviderProfile` fixes one sandboxed executable descriptor+SHA-256 or exact
  ModelEndpointProfile broker route, sandbox-policy revision and wall≤30s/cpu≤10s/processes≤4/RSS≤512 MiB/
  stdout≤8 KiB/stderr≤8 KiB limits. Each durable attempt precedes dispatch. Executables run in a new empty
  non-repository cwd with allowlisted non-secret env, fds 0..2 only, sealed redacted stdin, no network and an
  enforced denial of workspace/repository/home/arbitrary files, daemon/control, keychain, clipboard, PTY and
  devices; model-gateway attempts spawn no helper and use only the pinned broker. Unsupported enforcement,
  crash, timeout or limit breach is terminal and never retries the operation.
- `CommitProposalAttemptState` is exactly
  `prepared→dispatching|failed(cancelled_or_expired_pre_dispatch)` and
  `dispatching→succeeded|failed(timeout|crash|signal|limit|ambiguous_broker|expired_during_dispatch|
  invalid_output)`; terminals never transition and no reconcile state exists. Proposal
  prepared→generating and Attempt prepared→dispatching commit together before dispatch; terminal success+
  Proposal ready or terminal failure+Proposal failed/expired commit together. Expiry kills/fences dispatch and
  cannot strand or reuse an Attempt.
- `CommitProposal` is `prepared→generating|refused|expired`, `generating→ready|failed|expired`,
  `ready→applied_to_editor|discarded|expired`. It fixes repository/staged revision+hash, redacted diff≤128 KiB,
  omission manifest, exact provider-profile/attempt revisions and output≤8 KiB. Apply CASes only the editor draft.
- `TransferTicket` is `prepared→transferring|cancelled|expired`,
  `transferring→paused|completed|failed|reconcile_required|cancelled|expired`,
  `paused→transferring|cancelled|expired`,
  `reconcile_required→completed|failed|paused|cancelled|expired|reconcile_required`.
  It fixes direction plus separate source/destination endpoint identities and generations, ≤2 GiB size/hash,
  ≤4 MiB chunks, create-new policy and ≤30m
  expiry. Completion is owner-only temp plus full hash and non-executable atomic rename; cancel/expiry during
  possible publication terminalises only after lookup proves no output, while proved output is completed and
  uncertainty remains reconcile-required. Client-stream chunks additionally bind client/session/surface and
  endpoint role; other endpoint pairs use only their backend transports. Reconcile never copies twice.
- `ContentProjection` fixes surface/source id+revision+hash, plain-or-markdown, sanitizer version and source≤2
  MiB. One current projection/Surface, four/connection, 64 installation-wide and 128-MiB aggregate are hard;
  set reserves/validates before atomic replacement and clear/source invalidation/Surface-or-connection loss
  releases. Failure/N+1 preserves the old projection, reconnect inherits none and bodies remain memory-only.
  Markdown has no raw HTML/script/events/images/network/forms/control/unsafe schemes and cannot mutate source.
- `SignedEnvelopeV1` is closed to domain=`command_extension|product_announcement|update_manifest|
  update_package|voice_model_manifest`, schema_version=1, payload type/SHA-256, signer key id+epoch, issued/expiry, exact channel/
  platform/architecture/cohort audience, monotonic sequence, algorithm=ed25519, signature and a parent-
  manifest SHA-256 required only for packages. Preimage is `TURN-SIGNED-V1\0`, length-prefixed domain and
  length-prefixed RFC-8785 canonical envelope bytes without signature. Structured payload hashes use
  schema-validated duplicate/unknown-key-rejecting RFC-8785 bytes; packages use exact streamed bytes. Five
  revisioned installation trust stores are domain-disjoint. Old/revoked epoch, wrong store/audience/domain,
  expiry, lower sequence, same-sequence/different-hash and package/manifest mismatch fail before effect;
  idempotent exact replay is harmless and high-water survives compaction/rotation.
- `CommandCatalogue` is the sole catalogue revision/store. Its stable entries carry creation-or-general
  category, `built_in|signed_extension|local_operator` provenance, typed schema, registered typed operation,
  declared capability predicate and consequence. `CreationCatalog` means only its `category=creation` filter.
  Local-operator admission is a foreground validated mutation; repository/import/process output cannot admit
  entries. Labels≤512 bytes/128 scalars, keywords≤32×64 bytes, schema≤16 KiB and reason≤2 KiB. Get pages≤200/
  1 MiB through a daemon-minted scan pinned to catalogue+evaluation context; search query≤256 bytes scans
  ≤10,000 and returns≤200/1 MiB by deterministic score/id. Invoke accepts only an exact current entry and typed
  values, never label/output/arbitrary command.
  Local mutation is exposed only through the three entry-CRUD and two shortcut-binding
  LocalDesktopForegroundAuthority operations above. A signed-extension entry also pins its accepted envelope
  and command-extension trust-store revision; invocation revalidates revocation/high-water and it can name
  only operations/schemas/capabilities already registered in this build, never executable code or a new verb.
  `ShortcutBindingId` binds platform/scope/chord/entry/provenance under
  `active|disabled_conflict|revoked`; one slot has at most one active binding. A second viable binding without
  explicit local resolution disables the entire slot, sorts contenders by provenance/stable id/revision for
  display only and chord invocation executes nothing. Only explicit local replace names the chosen/displaced
  revisions and restores one winner; built-in/signed updates never override an active local resolution,
  arrival order never selects and revoke never auto-promotes a shadow.
- `Announcement` fixes accepted product-announcement envelope/store revision, signed audience/revision/expiry, inert text≤16 KiB and ≤3 reviewed HTTPS links;
  `active→dismissed|expired|superseded` and terminals never reactivate. AnnouncementOperatorIdentity is
  daemon-derived as local LocalOperatorIdentityId or full-GUI RemoteClientId; dismissal binds it and cannot be
  caller-selected. Installation high-water by channel/audience/key epoch plus terminal-id fence rejects older
  replay after compaction. It cannot emit StatusEvent/Attention/focus/command/setup/update authority.
- `ApplicationUpdate` is one current Installation-stream intent. Its `UpdateQuery` freezes operation,
  channel/platform/architecture/current-version, both expected trust-store revisions and anti-rollback
  high-water. `QueryOnly` is legal only at idle/no-update/discovery-failed; `ManifestAccepted` is mandatory at
  available/downloading/downloaded and pins the accepted manifest plus declared package fields; and
  `ReleaseAccepted` is mandatory from verified onward and also pins the separately accepted package envelope,
  current store revision and exact parent-manifest hash. A later failed/discarded state retains its highest
  evidence+phase; every other state/evidence pairing is rejected. Matching channel/platform/architecture/
  version/size≤2 GiB/digest/minimum/anti-rollback fields remain mandatory. Its reducer is `idle→no_update|available|failed`,
  `available→downloading|discarded`, `downloading→downloaded|available|failed|discarded`,
  `downloaded→verified|failed|discarded`, `verified→staged|discarded`, `staged→applying|discarded`,
  `applying→applied|rollback_required|apply_reconcile_required`,
  `apply_reconcile_required→applied|rollback_required|apply_reconcile_required`,
  `rollback_required→rolling_back`, `rolling_back→rolled_back|failed|rollback_reconcile_required`, and
  `rollback_reconcile_required→rolled_back|failed|rollback_reconcile_required`. Apply is local-desktop
  foreground and uses daemon-derived `daemon_absent_install|compatible_daemon_preserve|
  incompatible_daemon_with_live_ptys_refuse|incompatible_idle_daemon_refuse` under exact live revisions; only
  the first two apply. Same-query discovery is idempotent; a different query while current/non-clean refuses
  before network. One declared-size allocation is reserved before the first byte and is shared, not duplicated,
  by temporary/downloaded/verified/staged states; a second intent or aggregate byte N+1 refuses before effect.
  Terminal replacement folds into at most 100 rich receipts only after bytes are absent and independent
  signing/anti-rollback/replay fences are durable. Reconcile is lookup-only and never repeats replace/rollback.
  Discovery/download/failure never blocks terminals or installs.
- `WorkItemActivityEvent` fixes event/work-item ids, actor/provenance, operation/source receipt, pre/post
  revisions, observed time+clock source/freshness, optional provider effective time/external echo id and one
  matching closed kind/delta: `created|imported` refs, state from/to, metadata field tags+redacted safe values,
  comment ref only, assignee refs, sync revision+coverage, conflict id/field choices, projection from/to or
  source tombstone. Unknown/mismatched variants fail and delta≤8 KiB. Order is post revision/event sequence/
  event id—not timestamp; pages≤200, echo dedup is exact and compaction emits checkpoint/gap. It is evidence only.
- `ReversiblePresentationOperation` is exactly `set_tree_expanded`, `set_tree_expanded_all`, `set_tree_presentation`,
  `select_tree_node`, `set_surface_view_mode`, `set_inspector_width`, `set_board_presentation` or
  `set_terminal_appearance`; those exact wire requests create history and no other request may be encoded as
  one. The `set_tree_expanded_all` inverse stores the prior `expansion_default` and complete bounded exception
  set, never a hierarchy-row enumeration. LocalOperatorIdentityId is installation-minted/non-PII/stable until installation deletion; local
  connections bind it. `PresentationHistoryOwner=(LocalOperatorIdentityId,surface_id)|
  (RemoteClientId,RemoteSessionId,surface_id)` is daemon-derived from the authenticated connection and cannot
  be caller-selected. It partitions a Workspace history. Each record has operation/owner, Workspace history/object generations,
  pre/post/inverse and receipt; the Workspace's owner-partitioned stacks total ≤200. Undo/redo requires the
  same current owner/surface/session, so one remote or surface cannot undo another. New edit after undo clears
  only that owner's redo, CAS conflict invalidates and excluded domain/runtime/input/provider/source/SCM/
  Attention/authority/destructive effects are structurally unrepresentable.

Portable export carries package-local ids only. Import creates one fresh package map. A `new_workspace`
destination remints the package Workspace and every child id; an `existing_container` destination maps only
the package root to the exact selected Workspace/Session/Group and remints every imported child id. A
`FlowRun` is never portable: attempts,
provider conversation/NativeJob/runtime/process/PTY ids, revisions, operation receipts, grants, tombstones, credentials and machine/
host identity are constitutive run authority/evidence and are invalid package fields. An optional
`PortableRunReport` is a different inert type containing only package-local definition/step references,
origin content hash, terminal label, bounded redacted summaries, artifact content hashes and timestamps with
untrusted provenance. Import may display/link that report, but cannot decode it as FlowRun, satisfy a
dependency/result, emit Attention, authorise work, resume/retry/reconcile, or supply a launch receipt. To run
again the operator adopts the reminted FlowDefinition and creates a fresh preflight/FlowRun id. Collisions and
unresolved references create inert errors, never update/create by caller-selected local id.

`PortableContextPacketArtifact` is a closed optional package member with package-local artifact/source refs,
schema/sanitizer versions, bounded redacted older digest, exact recent tail, optional inert artifact bytes/
refs, selection/budget/omission/redaction manifests, body/framing hashes and untrusted provenance time. Local
packet/destination/runtime/conversation/operation/revision/grant/credential/host ids and executable fields are
invalid. Import remints `ImportedContextArtifactId` and exposes inert content only; normal
`prepare_context_packet` must perform a fresh current destination/budget/review before delivery.

`PortableExportState` is `prepared→assembling|cancelled`, `assembling→review_required|failed`,
`review_required→committing|cancelled|stale`, `committing→written|reconcile_required`,
`reconcile_required→written|not_written|reconcile_required`. `PortableImportState` is
`prepared→validating|cancelled`, `validating→review_required|refused|failed`,
`review_required→committing|cancelled|stale`, `committing→committed|reconcile_required`,
`reconcile_required→committed|not_imported|reconcile_required`. Terminal states never resume. Both pin exact
regular-file identity, package hash/schema/≤64-MiB size and review revision; durable intent precedes create-new
atomic write or remint transaction and reconciliation is lookup-only.

PortableExport is owned by its exact source Workspace stream; PortableImport remains Installation-stream
owned before and after its revision-vector-fenced destination commit. The installation admits at most 16
nonterminal exports and 16 nonterminal imports. Prepare reserves an active slot, terminal receipt and full
declared package allowance before read/write/remint; N+1 refuses before effect. All assembly/validation
temporaries share one owner-only 2-GiB installation cap and each package is≤64 MiB. Cancel/terminal cleanup
releases only proved-absent bytes; committing/reconcile state never ages out. Up to 10,000 rich terminal
receipts remain 30 days, then compact only after minimal operation/package/path/destination/result replay and
collision fences are durable.

### Accepted local dictation protocol target (not in v4)

ADR-060 adds no request that can open a microphone, capture PCM or run transcription. Those are foreground
native-client acts. The reserved M15 operations manage only trusted model artifacts and an already reviewed
text commit:

| Planned `op` | Principal fields | Planned answer |
| --- | --- | --- |
| `list_local_speech_models` | none | `local_speech_models` |
| `install_local_speech_model` | foreground surface, `operation_id`, closed `model_id`, exact accepted `SignedEnvelopeV1(voice_model_manifest)` identity, expected voice-model SigningTrustStore+catalogue revisions and declared artifact size/hash/origin; caller cannot choose key/root | `local_speech_model_state` |
| `cancel_local_speech_model_install` | foreground surface, `operation_id`, model id/generation | `local_speech_model_state` |
| `remove_local_speech_model` | foreground surface, `operation_id`, model id/generation | `local_speech_model_state` |
| `commit_operator_text` | foreground surface/connection/daemon generation, `operation_id`, exact `InputTarget`, expected input revision, `insert` or `submit`, bounded UTF-8 text | `operator_text_delivery` |

`InputTarget` repeats exact Workspace/Session/Node, optional AgentInstance, current RuntimeAttempt/generation,
verified input owner and optional pending free-text interaction/revision. The daemon revalidates all of it
immediately before one fenced write. Permission, credential/password, provisional/unassigned, raw-TTY and
unverified alternate-screen targets are invalid. Text is independently control-stripped and bounded;
`insert` cannot append Enter, while `submit` performs exactly one reviewed send. Dictation provenance grants
no authority or confidence. An uncertain partial write is `submitted_unconfirmed` and is never replayed.

Model list/state/progress contains closed model id, exact accepted envelope and trust-store revisions, signer/
epoch/audience/sequence/high-water identity, expected/observed digest and size, catalogue/engine compatibility,
licence, generation and safe error code. Current store/revocation is revalidated before download, rename and
native load. It never contains PCM, transcript, device identity or
arbitrary URL/path. Audio, hypotheses and the inline draft never cross this protocol. The full target,
settings, privacy and acceptance contract is `docs/LOCAL_VOICE_INPUT.md`.

The local state still obeys the closed manifest and global admission. M15 owns one device MicrophoneLease, one
DictationTarget, two≤10-MiB PCM buffers (active capture plus pending inference), one≤32-KiB hypothesis and one
live-or-cleanup-pending≤512-MiB SpeechWorker. It reuses one≤32-KiB LocalInputDraft/Surface (eight/client,
64/2 MiB installation) and adds only≤4-KiB VoiceTranscriptDraft metadata (64/256 KiB). Capture≤300 seconds;
inference≤300 seconds and stop waits≤2 seconds before forced termination. Family+shared reservation precedes
microphone open/spawn, all N+1 paths have zero target/process effect, and a hung worker transfers its exact
reservation to ProcessCleanupCharge until OS reclamation. LocalClientInstanceId continuity preserves only the
same open window's non-authoritative draft/settings; it never preserves or widens daemon InputTarget authority.

### Release/update preflight

| `op` | Fields | Answers with |
| --- | --- | --- |
| `get_update_status` | — | `update_status` |

`update_status` carries `daemon_version`, `protocol_min`, `protocol_max` and `active_ptys`.
It is an authenticated read used before replacing an installed app bundle. `active_ptys` counts the live
PTY handles owned by this daemon; it is not reconstructed from saved Session or lifecycle state. An updater
may replace only the UI while the release and daemon protocol windows overlap. If the windows do not
overlap, any positive PTY count defers the update, and a zero count still requires an explicit daemon stop —
there is no protocol request which silently restarts the daemon for an installer.

### Local-data privacy

| `op` | Fields | Answers with |
| --- | --- | --- |
| `get_privacy_report` | `scope: PrivacyScope` | `privacy_report` |
| `export_privacy_data` | `scope: PrivacyScope`, absolute `path` | `privacy_exported` |
| `delete_privacy_data` | foreground surface, operation id, `scope: PrivacyScope`, exact scope owner/content/graph/Attention/authority revisions applicable to its tag and closed tag-specific revoke/rehome/tombstone/retain-or-refuse disposition | `privacy_deleted` with per-category fences, survivors and residuals |
| `compact_privacy_data` | — | `privacy_compacted` |

`PrivacyScope` is the closed tagged union `installation`, `workspace { workspace_id }`,
`session { session_id }`, `agent { session_id, node_id }`, `note { note_id, node_id }`,
`work_item { work_item_id, node_id }`, `work_item_source { source_id }`,
`resource { resource_id, node_id }`, `native_job { session_id, node_id, job_key_or_creation_id }`,
`flow_run { flow_run_id }`, `account_profile { profile_id }` or `remote_client { client_id }`.
Reports enumerate counts and bytes by stored type, the resolved retention
policy, and the explicit no-telemetry facts. Exports are owner-only, create-new JSON documents; every datum
names its origin/type/timestamp and carries redacted content or an explanation that its filesystem payload
was omitted. Existing destinations and symlinks are refused.

Every scoped delete performs one CAS over its declared revisions. Note/Resource deletion first revokes every
current ContextLink, fences in-flight reads and reroutes exact live references/Attention before content
removal. WorkItem deletion similarly fences the independent Attention revision before its canonical Node
leaves. A newly arriving link, read, demand, mutation or binding makes the stale request refuse with zero
partial deletion. Selective deletion then stops the named work according to the required disposition, removes Turn-owned database
and filesystem records, compacts SQLite and reports any escaped process ids. `keep_processes` is invalid.
Installation scope is refused by the live daemon and must use the lock-protected offline
`turnd --delete-installation-data` operation. Compact applies retention, bounds/scrubs the diagnostic log,
checkpoints the WAL and vacuums the database.

### Workspaces

This section explicitly overlays the accepted target on the implemented v4 compatibility table. Rows whose
Fields begin with `foreground surface, operation id` are **planned vNext target operations, not v4 wire**.
For compatibility auditing, their implemented v4 shapes remain exactly:
`create_workspace(name,root)`, `rename_workspace(workspace_id,name)`,
`archive_workspace(workspace_id,archived)`, `duplicate_workspace(workspace_id,name?)`,
`close_workspace(workspace_id,disposition)` and `delete_workspace(workspace_id,disposition)`.
Unmarked list/lease rows below remain implemented v4. A v4 peer cannot send the added target fields.

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_workspaces` | `include_archived?` | `workspaces` |
| `create_workspace` | foreground surface, operation id, preassigned WorkspaceId, exact Installation stream/catalogue revision, name and confined target/trust/root descriptor; reserves identity before any filesystem effect | `workspace`+creation receipt |
| `rename_workspace` | foreground surface, operation id, exact Workspace id/revision and name | `workspace` |
| `archive_workspace` | foreground surface, operation id, exact Workspace/graph/context-authority/Attention revisions and `archived`; hide-only archive stops/deletes nothing, suspends every ContextLink owned by or incident to any descendant in that Workspace, revokes its bearer and preserves Attention routes, while restore fully revalidates with fresh bearers and performs no broker read | `workspace` plus link-suspension/revalidation and Attention-route receipt |
| `duplicate_workspace` | foreground surface, operation id, preassigned WorkspaceId, exact source Workspace/settings and Installation stream revisions, `name?`; settings only, no Sessions, processes or authority | `workspace`+creation receipt |
| `close_workspace` | LocalDesktopForegroundAuthority, operation id, exact Workspace identity/generation and survivor-preserving `keep_processes` or `terminate|kill`; caller supplies no child/graph/Attention/survivor revision, and the daemon derives the current complete vector inside the serial transaction; Workspace row remains, but every active Session is ended under the same total per-Session reducer | durable `closed`+ContainerCloseReceipt with daemon-derived dispositions and cleanup survivors |
| `delete_workspace` | LocalDesktopForegroundAuthority, operation id, exact Workspace identity/generation and `terminate|kill`; caller supplies no child/graph/Attention/survivor revision. The serial daemon transaction derives total per-subject tombstone/rehome/InstallationSemanticRecoveryInventory and relationship/MediaImport/CommitProposal+Attempt/RepositoryPublishIntent/WebPreviewLoadIntent/BrowserNodeCreationIntent/BrowserNavigationIntent/BrowserDownloadQuarantine/AgentBrowserActionIntent/DocumentPrintIntent/BulkIdleRestartIntent/EcoHibernateIntent/CompanionAgentLaunchIntent/TransferTicket/PortableExport/PortableImport-destination disposition, atomically migrates existing Workspace recovery entries, and deletes only Turn container data, never provider/user data | durable `closed`+ContainerCloseReceipt with daemon-derived dispositions and cleanup survivors |
| `get_workspace_write_lease` | exact Workspace identity/generation | `workspace_write_lease` |
| `acquire_workspace_write_lease` | LocalDesktopForegroundAuthority, operation id, exact Workspace/Session/checkout identities+generations, current lease state+generation or proved none, canonical checkout identity and host-lock generation; target policy permits only a dedicated non-primary worktree | `workspace_write_lease` or typed conflict; admission reserves its future ended-owner/Installation fence before writer authority |
| `release_workspace_write_lease` | LocalDesktopForegroundAuthority, operation id, exact Workspace, lease id/generation, canonical checkout identity+host-lock generation and tagged `current_session(SessionId,SessionGeneration)|ended_session(SessionId,SessionGeneration,CheckoutId,ending_generation)` owner; release/reconcile requires exact no-owned-live-runtime plus OS lock/process-start quiescence or reclamation proof | incremented-generation `released` lease/fence receipt; lookup/reconciliation never signals a process or grants a new writer |

`archive_*` takes a flag rather than existing as two operations, so undo is the
same code path as do.

The four destructive operations answer `closed` rather than `ack`, and the difference is the point:
`closed` carries `escaped`, the processes Turn could not stop. A destructive act is authoritative — it
does not fail because a process survived the daemon that started it, since refusing would leave that
process running anyway and the user holding a Session they had finished with (ADR-050). `escaped` is
empty in the ordinary case; each entry names `node_id`, `session_id`, `title` and the last observed
`pid`, which is what a user needs in order to find it in a process list. Nothing in that path claims
the process exited: its `Lifecycle` stays `orphaned`.

`ContainerCloseReceipt` is Installation-owned and durable for
`close_session|delete_session|close_workspace|delete_workspace`. Each Session admission pre-reserves one
terminal close-or-delete receipt+minimal fence. Each Workspace admission reserves one delete slot and one
current nonempty close-epoch slot; `close_workspace` consumes the epoch, an empty repeat returns
`closed_already` after admitting its separate redundant-operation fence, and admitting the next Session first rearms another close epoch plus that Session's terminal
slot. Thus the joint 10,000-Session+1,024-Workspace maximum owns12,048 slots before terminal-history headroom,
and End/delete never allocates this capacity. The receipt freezes operation id and
canonical fingerprint, exact container tag/id/identity generation, action/disposition, serialisation point,
daemon-derived current graph/Attention/authority/subject revision-vector digest, resulting container tombstone
generation, ordered `ContainerSagaDisposition` root/digest, three fixed-size
`semantic|target_runtime|process_cleanup` survivor counts+Merkle roots and terminal result
`closed|closed_already`. It never copies or individually lists an unbounded survivor set. In the same serial
commit, one immutable Installation-owned `ContainerCloseSurvivorMembership` records the exact
ContainerCloseReceiptId, close serialisation point, typed stable survivor key, inventory/revision locator at
close, stable ordinal and leaf digest under its typed root. This is a many-to-many historical index: one
survivor may have a different membership under each close through which it is rehomed, and a later Workspace
delete may add another membership without rewriting an older root. Later inventory additions cannot enter an
old receipt; later resolution remains visible through the membership's stable typed key. Same operation/fingerprint
always reconstructs the identical `closed`; changed bytes conflict. A new operation against its exact
tombstone or an already-empty Workspace performs no container mutation. Before returning `closed_already` it
must reserve a new≤512-byte operation/fingerprint/container/tombstone/result fence from the independent
minimal pool; same id/fingerprint then replays `closed_already` and changed bytes conflict. At minimal-pool N+1
it returns typed `replay_capacity_refused_already_closed` with zero mutation: the requested outcome is already
true and no further operator interaction is required. It never returns generic `ack` or `not_found`.
At most16,384 root-only rich receipts≤16 KiB share≤256 MiB exactly and retain180 days; each then folds to one≤512-byte installation-lifetime
operation/fingerprint/container/tombstone/result fence. Its independent bounds admit either1,000,000 smaller
fences or exactly983,040 maximum≤512-byte fences/480 MiB; count and bytes never saturate in the same fixture.
Rich and minimal capacity reserve under the rules above; retained-terminal saturation refuses only a future
Workspace/Session admission, never closure of an admitted one. Minimal saturation may refuse only a new
redundant operation whose container outcome is already committed; it cannot affect the original close/delete
or its pre-reserved lost-reply fence. Large subject details remain in the bounded recovery inventories.
Selecting a receipt uses `get_container_close_survivor_inventory/page` to verify and page all three typed
roots in one consolidated local WorkSurface, even when runtime survivors span targets. Every row carries a
one-action redacted `ViewTarget`; the WorkSurface verifies receipt id, serial point, ordinal, leaf and root
before navigation. A missing page, root mismatch or inventory revision change is
shown as `gapped`, never silently as a complete survivor set.

Every subject/runtime/helper admission that can become a close survivor simultaneously reserves its next
`ContainerCloseSurvivorMembership`; semantic admission also pre-reserves the distinct future Workspace-delete
migration membership. End/close only consumes those rows and performs zero membership allocation. A valid
rehome reserves the destination's next-close membership before it commits; at membership N+1 it falls back to
the already-reserved Workspace/Installation recovery disposition instead of refusing close. Runtime launch and
helper spawn apply the same rule before effect. Membership richness retains180 days with its close receipt and
may fold only when that receipt has folded and no page can be requested; its stable survivor key is not the
survivor body. Thus a subject may traverse arbitrarily many Sessions over time without rebinding or overwriting
any historical receipt root, while saturation can refuse only new work/rehome before effect, never End/delete.

This post-commit process rule does not erase semantic survivor evidence or trap the operator. End
(`close_session` with `keep_processes`, `terminate` or `kill`), Workspace stop-all and Turn-container delete are total daemon
reducers: at the serial commit point they enumerate the current graph/Attention/authority, discard
uncommitted Session-owned drafts, revoke Session authority, apply any valid predeclared rehome and otherwise
retain/tombstone independently live or uncertain semantic subjects in the WorkspaceSemanticRecoveryInventory,
or atomically in InstallationSemanticRecoveryInventory when the Workspace itself is deleted. Runtime handles
remain separately in their TargetRuntimeRecoveryInventory. No missing/stale client plan
exists to refuse. A concurrent subject is either included before the ending tombstone or reduced against it
afterwards. Semantic dispositions and active-row removal commit atomically; later cleanup or `escaped`
reporting cannot veto or resurrect the container. Provider/user-data delete remains a separate operation.

Semantic recovery is a closed typed registry, never a prose catch-all. For ordinary admission,
`SemanticRecoveryReservationId` is a freshly daemon-minted, non-reused 128-bit id; schema migration instead
derives a deterministic 128-bit id from the stable legacy tuple plus registry namespace and rejects any
collision before insertion. `ReservationRevision` is a monotonic unsigned 64-bit revision.
`SemanticRecoverySubjectKey=(subject_kind,canonical_key)` is stable across the subject's mutable revision and uses one of exactly
the following 26 kinds; unknown/default/misc/other/wildcard kinds are unrepresentable:

The registry's fence vocabulary is typed: `AgentInstanceRevision` and `FlowRunRevision` are monotonic u64
revisions of their named records; `FlowDefinitionId` and `MediaBlobId` are daemon-minted, non-reused 128-bit
identities. They are not aliases for Node, operation, receipt, path or provider ids, and every canonical
fingerprint encodes the tagged type.

```text
SemanticRecoverySubjectRegistry.vNext = {
  @semantic_subject|agent_browser_action
  @semantic_subject|browser_download_quarantine
  @semantic_subject|browser_navigation
  @semantic_subject|browser_node_creation
  @semantic_subject|bulk_idle_restart
  @semantic_subject|commit_proposal
  @semantic_subject|companion_agent_launch
  @semantic_subject|document_print
  @semantic_subject|eco_hibernate
  @semantic_subject|effect_delivery
  @semantic_subject|flow_run
  @semantic_subject|media_import
  @semantic_subject|native_job_create
  @semantic_subject|native_job_mutation
  @semantic_subject|native_job_projection
  @semantic_subject|node_aggregate
  @semantic_subject|portable_export
  @semantic_subject|portable_import_destination
  @semantic_subject|repository_publish
  @semantic_subject|runtime_launch
  @semantic_subject|runtime_lifecycle
  @semantic_subject|transfer
  @semantic_subject|web_preview_load
  @semantic_subject|work_item_create
  @semantic_subject|work_item_mutation
  @semantic_subject|work_item_projection
}
```

`docs/SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv` is the authority-hashed schema for every key, required fence,
family bundle, eligibility predicate, allocate/inherit/transfer rule and release proof above. Its paired
`docs/SEMANTIC_RECOVERY_FAMILY_CLASSIFICATION_VNEXT.tsv` independently classifies every durable Workspace
family exactly once as a typed bundle member or a deterministic exclusion. Cross-owner dependencies remain in
their Installation/ExecutionTarget stream and are referenced by exact key; ephemeral families are forbidden
from a semantic bundle. The verifier rejects a missing/duplicate family, unconditional all-Node eligibility,
an invented relationship subject, child double-allocation, transfer without the same reservation id, or any
unclassified future Workspace family.

An allocating subject owns the pair below. An inherited child is represented only inside the owning parent's
declared `family_bundle`, required fences and revision vector; it has no second SubjectKey or reservation.
`WorkspaceSemanticReservation=(SemanticRecoveryReservationId,SemanticRecoverySubjectKey,WorkspaceId,
ReservationRevision,subject_revision,reservation_fingerprint,state,metadata_bound)` where state is exactly
`reserved|inventoried|resolved` and metadata_bound is≤16 KiB. The paired
`InstallationMigrationReservation=(same reservation id,same subject key,former WorkspaceId,
ReservationRevision,reservation_fingerprint,state)` exists from initial admission. Unique subject and
reservation-id constraints plus foreign keys enforce a one-to-one bijection between the Workspace and
Installation rows; their fingerprint includes the registry digest and cannot be rebound ad hoc. A declared
one-to-one transfer is an exact CAS that keeps ReservationId, increments ReservationRevision and commits a new
binding fingerprint over prior SubjectKey+prior fingerprint+declared transfer edge+new SubjectKey; the source
key/fingerprint retires behind replay and no other transition may rebind it. End changes an existing row to
inventoried, rehome preserves it, and transfer keeps that complete lineage. Workspace delete consumes the pre-existing InstallationMigrationReservation
and inserts/activates the corresponding InstallationSemanticRecoveryEntry under that same reserved id, byte
charge and capacity; it allocates no new slot and cannot refuse for capacity. Inherited child
work consumes no second slot. Only definite terminal/no-survivor proof plus durable replay/nonreuse fences and
quiescent helpers/buffers/temp may resolve/release; live, possible-effect, reconcile-required or cleanup-pending
subjects never age out. Attention reroutes; grants/links/leases revoke or use their own typed fence; internal
Team/Group/Spawn/Process/Lineage relations retain deterministic history; presentation terminal state retires;
OS/PTY/renderer/helper handles use TargetRuntimeRecoveryInventory or ProcessCleanupCharge. None invents a
semantic subject at End.

`NativeJobProjection=(NodeId,NativeJobProjectionSubject,ProjectionRevision,PresenceRevision?,profile/target/
namespace/adapter generations,provider_job_incarnation?)` is Workspace-owned. Its subject is exactly
`job(NativeJobKey)|creation(NativeJobCreationId)` and remains addressable even when forgotten. The daemon also
mints `BrowserNodeCreationIntentId`, `AgentBrowserActionIntentId`, `DocumentPrintIntentId`,
`BulkIdleRestartIntentId`, `EcoHibernateIntentId` and `CompanionAgentLaunchIntentId` before their first declared
effect; these ids are stable keys, never aliases for operation id.

The current protocol-v4 store baseline is SQLite schema11; vNext semantic reservations are append-only
schema12. Before opening SQLite, the daemon takes the exclusive canonical data-directory lock. Migration12 is
one `BEGIN IMMEDIATE` transaction containing DDL, closed legacy census, validation, paired backfill, invariant
marker+registry digest and `PRAGMA user_version=12`; no post-backfill step occurs outside that transaction.
The legacy census admits only `process_nodes` whose parsed lifecycle is `spawning|alive|orphaned|reconnected`,
plus Agent/Subagent rows with a stable external/conversation identity even when process state is terminal/lost.
Terminal inert Tools/Shells/tests, presentation, Attention, relations and checkout handles receive no slot.
Each candidate gets canonical `(WorkspaceId,node_aggregate,NodeId)` and a deterministic 128-bit reservation id
derived from that stable tuple+registry namespace with collision validation; missing provider/attempt/
generation data is never guessed and runtime probing is forbidden.

Before insertion migration validates every JSON/enum/UTF-8 value, owner join, foreign key, unique subject
fingerprint,≤16-KiB encoded row and the independent 4,096/64-MiB per-Workspace and32,768/512-MiB installation
bounds. Unknown/corrupt/dangling/duplicate/oversize/N+1 or I/O failure rolls back DDL, rows, marker and
user_version; schema11 remains reopenable by the prior binary and no daemon restore, process, file or network
effect occurs. The vNext binary exposes a bounded read-only migration diagnostic naming the exact validation
class, candidate count/encoded-byte boundary and non-secret owner/key hashes, plus explicit copy-backup and
offline export guidance; it never opens the v11 store writable, substitutes defaults, prunes candidates or
asks End to make room. Crash injection before/after DDL, every candidate, between paired inserts, after each pair,
marker, user_version and commit yields exactly schema11 with no new table/row or schema12 with complete
bijection. Re-running schema12 with the same registry digest changes zero bytes; a different digest refuses
write-open. Stores older than11 may advance to the already supported schema11, but migration12 atomicity is
promised specifically for the 11→12 transaction, not retroactively for an earlier migration chain.

After schema12 open, restore and every writer admission remain blocked until pair counts, unique keys,
fingerprints, registry digest and census marker validate. End/delete at full capacity only consumes, transfers
or resolves an existing ReservationId and performs zero reservation INSERT. The exact-zero, exact count/byte
caps, N+1, malformed lifecycle, orphan owner, dangling provenance, live/orphaned/lost/exited matrix, Attention,
legacy primary-checkout lease and every crash boundary are mandatory migration fixtures.

The same serial enumeration includes every referencing `RuntimeLifecycleIntent`/receipt and any reserved
replacement `RuntimeLaunchIntent`, MediaImport, CommitProposal plus its Installation-owned
CommitProposalAttempt, RepositoryPublishIntent, WebPreviewLoadIntent, BrowserNodeCreationIntent, BrowserNavigationIntent,
BrowserDownloadQuarantine, AgentBrowserActionIntent, DocumentPrintIntent, BulkIdleRestartIntent,
EcoHibernateIntent, CompanionAgentLaunchIntent,
TransferTicket, Workspace-owned PortableExport and Installation-owned PortableImport
destination. Its closed `ContainerSagaDisposition` is `cancel_pre_effect|terminalise_no_effect|
rehome(exact compatible destination)|workspace_recovery|installation_recovery`. Cancellation requires proved
no effect. Generating proposal/attempt linkage survives while editor-apply authority is revoked; prepared
publication cancels only with no-effect proof, while remote-created/remote-published/configuring/reconcile
evidence preserves destination/object/ref/config/correlation identity, loses the ended checkout lease and never
deletes provider data or resumes another phase implicitly; a prepared WebPreview load cancels only with proved
zero request, fetching/reconcile-required/fetch-unconfirmed evidence preserves source/URL-hash/policy/HTTP
correlation in recovery; End revokes presentation immediately and either releases proved-quiescent allocations
or transfers the exact socket/worker/buffer charge to recovery-owned cleanup until quiescence, and late evidence neither renders into a
deleted Session nor refetches; prepared Browser
creation/navigation cancels only with no-effect proof, while dispatching/reconcile-required/dispatched-unconfirmed
identity, partition, renderer token and possible-load evidence rehomes or enters semantic recovery and can never
late-publish into a deleted Session or redispatch; Browser quarantine
response/descriptor/sealed-hash/ownership-transfer uncertainty and transfer temp, endpoint/chunk/publication
uncertainty survive; committing export output identity and committing import
destination/remint uncertainty survive without rewrite/reimport. A precommit deleted destination becomes
stale/cancelled. Every saga reserved its semantic-recovery slot before effect, so End/delete never allocates or
refuses; late receipts reduce only against the retained exact identity.

For lifecycle races, a `prepared` intent with proof that no signal, stop or launch escaped takes
`cancel_pre_effect`. Once the old attempt is definitely stopped, the ending tombstone atomically fences any
not-yet-effectful replacement launch; it cannot start after the Session row leaves. A signal/stop/launch in
`dispatching|possible_effect|reconcile_required`, or a replacement whose absence is not proved, takes the
applicable semantic recovery disposition with its pre-reserved identity and `ProcessCleanupCharge`; later work
is lookup/probe-only. Definite replacement publication before the tombstone is either rehomed with the same
Node/instance/conversation identity when an exact compatible destination was already declared or retained as
runtime/recovery evidence; it is never recreated. The container tombstone rejects every late launch/receipt
publish and cannot resurrect a Node, Session, input lease or authority.

The same commit expires every `TerminalWarmViewPark`, cancels and wipes every Session-owned
`TerminalWakeInputBuffer`, fences each `TerminalOffscreenClientDetach` and revokes the ended Session's input
lease. Target/handle-owned `TerminalShadowObserver`/`TerminalBackgroundWriteChannel` generations follow the
runtime disposition: an exact live rehome/recovery survivor atomically inherits the same observer/channel and
charge without inheriting input authority or buffered wake bytes; a stopped/ended runtime orders them to
retire. Proved-quiescent renderer, observer, writer and buffer charges release immediately; hung or uncertain
cleanup transfers the already reserved `ProcessCleanupCharge` and exact handle/generation to recovery without
vetoing row removal. The profile/target-
owned `PrivateTranscriptSearchIndex` is not Session data and is neither revoked nor deleted by End/delete;
only the ended Surface's query/view buffers, cursors and historical `ViewTarget` are invalidated and wiped.

Every canonical semantic-subject reservation is durable before subject admission or external effect and binds
its exact subject/Workspace to one ≤16-KiB recovery record, a 4,096-subject/64-MiB Workspace budget and a
pre-reserved 32,768-subject/512-MiB installation migration slot. N+1 admission refuses before effect; End/delete
uses or moves the existing reservation and can never allocate/refuse on this path. Late evidence joins only its
reserved tombstoned identity; unmatched runtime handles remain target-runtime inventory. A resolved terminal
entry releases its reservation only after the 180-day recovery boundary and independent non-reuse/replay fence
are durable; live, uncertain and cleanup-required entries do not age out.

### Sessions

This table uses the same explicit overlay rule: creation/duplicate/archive/close/delete rows whose Fields begin
with `foreground surface, operation id` are **planned vNext target operations, not v4 wire**. Their implemented
v4 counterparts remain the simple shapes documented by protocol source: the six `create_*session*` requests
accept their legacy Workspace/name/template/cwd/branch/panes/note/tags fields, `archive_session` accepts
`session_id,archived`, `duplicate_session` accepts only `session_id`, and close/delete accept
`session_id,disposition`. Unmarked list/rename/favourite/pinned/get/tree rows remain implemented v4. A v4 peer
cannot send a preassigned id, MutationEnvelope or target survivor fields.

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_sessions` | `workspace_id?` (absent = all), `include_archived?` | `sessions` |
| `create_session` | foreground surface, operation id, preassigned SessionId, exact Workspace/Workspace-stream/target/trust/catalogue revisions, `name`, confined `cwd?`, bounded descriptor-bearing `panes?`, `note?`, `tags?`; reserves the Session before any create/spawn | `session`+creation receipt |
| `create_session_from_template` | foreground surface, operation id, preassigned SessionId, exact Workspace/Workspace-stream/target/trust/catalogue/Template revisions, `name?`, confined `cwd?`, `branch?`, `task?`; template expansion is frozen before reservation | `session`+creation receipt |
| `create_read_only_session` | foreground surface, operation id, preassigned SessionId, exact Workspace/Workspace-stream/target/trust/catalogue revisions, `name`, confined `cwd?`, `panes?`, `note?`, `tags?` and enforced read-only policy revision | `session`+creation receipt |
| `create_read_only_session_from_template` | foreground surface, operation id, preassigned SessionId, exact Workspace/Workspace-stream/target/trust/catalogue/Template revisions, `name?`, confined `cwd?`, `branch?`, `task?` and enforced read-only policy revision | `session`+creation receipt |
| `create_worktree_session` | foreground surface, operation id, preassigned SessionId+CheckoutScopeId, exact Workspace/Workspace-stream/target/trust/repository/canonical-checkout revisions, `name`, non-primary `branch`, confined `worktree_path?`, `panes?`, `note?`, `tags?`; reserves one isolated worktree generation/lease before spawn | `session`+checkout creation receipt |
| `create_worktree_session_from_template` | foreground surface, operation id, preassigned SessionId+CheckoutScopeId, exact Workspace/Workspace-stream/target/trust/repository/canonical-checkout/catalogue/Template revisions, `name?`, confined `cwd?`, `template_branch?`, `task?`, non-primary `branch`, confined `worktree_path?`; reserves one isolated worktree generation/lease before spawn | `session`+checkout creation receipt |
| `rename_session` | `session_id`, `name` | `session` |
| `archive_session` | foreground surface, operation id, `session_id`, `archived`, exact Session/graph/context-authority/Attention revisions; `archived=true` atomically suspends every ContextLink and revokes bearers while preserving exact Attention routes, and `archived=false` may revalidate links with fresh bearers but performs no broker read | `session` plus link-suspension/revalidation and Attention-route receipt |
| `duplicate_session` | foreground surface, operation id, preassigned SessionId, exact source Session/Layout/Workspace revisions and explicit read-only-or-new-isolated-worktree mode; copies inert shape only, never processes, handles, leases or authority | `session`+creation receipt |
| `set_session_favourite` | `session_id`, `favourite` | `session` |
| `set_session_pinned` | `session_id`, `pinned` | `session` |
| `close_session` | LocalDesktopForegroundAuthority, operation id, exact Session identity/generation and survivor-preserving `keep_processes` or `terminate|kill`; caller supplies no graph/Attention/authority/survivor revision. The daemon snapshots their current complete values after serialisation and derives a total per-child/survivor/relation/RuntimeLifecycleIntent+receipt/replacement RuntimeLaunchIntent/terminal park+detach+wake+shadow+writer state/MediaImport/CommitProposal+Attempt/RepositoryPublishIntent/WebPreviewLoadIntent/BrowserNodeCreationIntent/BrowserNavigationIntent/BrowserDownloadQuarantine/AgentBrowserActionIntent/DocumentPrintIntent/BulkIdleRestartIntent/EcoHibernateIntent/CompanionAgentLaunchIntent/TransferTicket/PortableExport/PortableImport-destination vector in the same transaction; every disposition removes the Session row, and presentation-only detach is `detach_runtime_view` | durable `closed`+ContainerCloseReceipt with daemon-derived dispositions and cleanup survivors |
| `delete_session` | LocalDesktopForegroundAuthority, operation id, exact Session identity/generation and `terminate|kill`; caller supplies no graph/Attention/authority/survivor revision. The serial daemon transaction derives total per-subject tombstone/rehome/WorkspaceSemanticRecoveryInventory and relation/RuntimeLifecycleIntent+receipt/replacement RuntimeLaunchIntent/terminal park+detach+wake+shadow+writer state/MediaImport/CommitProposal+Attempt/RepositoryPublishIntent/WebPreviewLoadIntent/BrowserNodeCreationIntent/BrowserNavigationIntent/BrowserDownloadQuarantine/AgentBrowserActionIntent/DocumentPrintIntent/BulkIdleRestartIntent/EcoHibernateIntent/CompanionAgentLaunchIntent/TransferTicket/PortableExport/PortableImport-destination disposition, deleting only Turn container data and never provider/user data | durable `closed`+ContainerCloseReceipt with daemon-derived dispositions and cleanup survivors |
| `get_session` | `session_id` | `session_details` |
| `get_process_tree` | `session_id` | `tree` |

### Settings

| `op` | Fields | Answers with |
| --- | --- | --- |
| `get_settings` | `session_id?` (absent = the Global level alone) | `settings` |
| `set_setting` | `scope`, `owner_id?`, `key`, `value` | `settings` |
| `reset_setting` | `scope`, `owner_id?`, `key` | `settings` |

Those three compact shapes are protocol v4 only. In vNext, `get_settings` adds exact registry plus every
applicable Global/Workspace/Template/Session record and resolved revision; `set_setting` and individual
`reset_setting` require LocalDesktopForegroundAuthority, operation id, `SettingsOwnerKey`, exact registry/
owner-record/resolved revisions, key+schema digest and bounded value where present, reserve one
`SettingsMutationReceipt`, and return the authoritative resolved document plus receipt. Same operation/
fingerprint is idempotent; a changed fingerprint or stale revision conflicts with current truth before mutation.
Temporary writes/resets use the identical generated key schema and revision reducer in the owning local Surface
but never cross IPC. Registry/search/deep-link and section-preview/apply use the vNext table above.

`scope` is one of `global`, `workspace`, `template`, `session`, `temporary`, in that precedence order:
later beats earlier. `owner_id` names the Workspace, Template or Session; it is ignored for `global`,
which has one owner.

All three answer with the whole resolved set rather than an ack, for the same reason a pane operation
answers with the layout: one write can move what is in force for more than the key that was written —
removing a Session override reveals the Workspace's value — and a client that patched its own copy
would be a second resolver able to disagree with the daemon's. **The daemon is the only resolver of
persistent levels** (ADR-051): the window receives Global through Session with their origin and
never re-resolves them. It may only append its own Temporary value, which cannot exist outside that
window.

`set_setting` refuses an unknown key (`not_found`), a level the key does not belong to (`refused`,
with the levels it does belong to in the detail), a value of the wrong shape (`invalid_argument`,
naming what would be accepted), an owner that does not exist (`not_found`), and the `temporary` level,
which lives in the window and is never persisted. For persistent scopes, `reset_setting` does not
require the key to be known: resetting a key this build does not define is how a user removes a value
a newer Turn wrote. Temporary set and reset never cross the protocol.

A `settings` response carries `levels` — the persistent levels that exist for this Session, each
with the `owner_id` a write quotes back — and one `entries` row per preference, with the resolved
value, the level it came from (`null` for Turn's own default, which is distinguishable from a level
having set the same value), every level it shadowed, its compiled default, and the levels it may be
set at. The window adds `temporary` to the editable levels and keeps that layer only in memory. A
secret arrives already replaced with `<redacted>` and `hidden: true`; the daemon keeps the real value
because it needs it, and this is the boundary past which nothing does.

The `keyboard.bindings` preference is the one the **window** applies rather than the daemon: it is a
map from command id to chord, and the daemon never sees a keystroke. An empty chord unbinds a command;
an absent entry inherits, which is why resetting a binding removes its entry rather than writing the
default into it. `keymap.json` still loads at startup and the stored preference wins over it per
command, so a command the preference does not mention keeps what the file said.

ADR-060 reserves `keyboard.bindings["input.dictate"]` plus `input.dictation.model` (Global, default
`none`), `input.dictation.language` (all levels, default `auto`) and `input.dictation.max_seconds` (Global,
default 120, range 15–300), plus Global `records.local_speech_models_mib` (default 8,192). They remain
unknown/refused in protocol v4 until M15 ships end to end; `none` means no microphone feature/model download,
and no setting can enable cloud, auto-send or voice commands.

`branch` and `task` fill `{branch}` and `{task}` in the template's name pattern.
`panes` is a list of `NewPane`; absent means a single shell.

In the legacy v4 wire shape only, `create_session` and `create_session_from_template` requested the primary
checkout; callers could not smuggle a generic mode or checkout through either shape. Creating `main_checkout` persisted the Session
assignment and acquires the lease in one atomic store
transaction, before init commands, processes or Panes existed. A conflict rolled the transaction back, returned
the typed context in §5 and left no partial Session. Duplicate never inherited an active lease; it had to
choose/reconcile a mode. Read-only replies include `read_only_enforced`, because metadata and agent guidance
were not enforcement. True meant the daemon constructed the platform process guard and could launch the
configured Layout; false meant commands remained stopped. Worktree replies included the new checkout and
declared shared resources. These sentences are migration history, not an available vNext writer path.

**Known v4 conflict and required vNext migration:** the primary-checkout default above is not accepted
post-v0.1 behavior. vNext has no operation that creates or acquires a writable `main_checkout`; every direct,
Template, Flow, add-node/pane, activate, restore, resume, restart, recycle, switch and branch request selects
an enforced read-only target or provisions a unique isolated worktree before spawn. The legacy enum is
decode-only for migration. Stopped rows convert; live legacy workers become `migration_required`, accept no
new input/effects and prevent release compliance until the operator stops them or explicitly recreates them
in isolation. Completion requires a scan proving zero Turn-owned leases/processes on the primary path and
zero secondary worktrees holding its `main` branch.

When the failed request was `create_session_from_template`, the client retains only its original Template
identity and interpolation inputs, then uses the matching `*_from_template` alternative. The daemon reloads
the authoritative Template and applies the same Layout, commands, relative cwd, environment, Attention,
tmux and name-pattern rules inside the selected safe checkout. A read-only alternative preserves that
configuration and launches it only after enforcement is active; while enforcement is unavailable it starts
no process. A worktree alternative remaps
absolute primary-checkout Session/Pane cwd values to the same repository-relative location in the isolated
checkout. Clients must not reconstruct a Template from `TemplateSummary`. The non-Template alternatives
remain the explicit blank/shell path.

### Templates

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_templates` | — | `templates` |
| `get_template` | `template_id` | `template_details` |
| `create_layout_template` | `name`, `layout`, `description?` | `template` |
| `create_template` | complete `template` draft | `template` |
| `save_layout_as_template` | `session_id`, `name`, `description?`, `hotkey?` | `template` |
| `update_template` | `template_id`, complete `template` draft | `template` |
| `duplicate_template` | `template_id`, `name` | `template` |
| `delete_template` | `template_id` | `templates` |
| `set_workspace_default_template` | `workspace_id`, `template_id?` | `workspace` |
| `apply_template_to_session` | `session_id`, `template_id` | `session` |

Both creation paths strip process bindings: a template describes what to start, never which
instance it was captured from. `create_layout_template` is the visual editor path. Its bounded
Layout is validated and normalised by the daemon before persistence; the client-side Pane ids are
draft identity only, and every Session instantiation mints a fresh set. Capturing a live Layout with
`save_layout_as_template` resets transient Automatic presentation `kind` to `launch_kind()` and preserves a
valid manual operational display pin. Create/update drafts containing a manual pin to an internal or
renderer-less kind are rejected with `invalid_argument`; they are never persisted as operational Panes.

Built-in Templates are immutable but may be duplicated. Rich create/update drafts preserve Layout
geometry, Pane command/argv/cwd/environment/restore behaviour, Template environment and init actions,
Attention policy, naming metadata and tmux preference. Client-supplied identity, timestamps, built-in
ownership and runtime bindings are ignored. `TemplateSummary.missing_commands` names unresolved
executables using the daemon's PATH so a picker can warn before launch.

Applying refuses a Session that still has a running process; it never turns layout replacement into an
implicit stop operation. A successful application materialises the complete Template and starts safe
Pane commands without a second confirmation. Deleting clears Global/Workspace defaults and Session
references to that Template, but Sessions retain their independently persisted layout and configuration.

### Panes

| `op` | Fields | Answers with |
| --- | --- | --- |
| `split_pane` | `session_id`, `pane_id`, `direction`, `pane` | `layout` |
| `create_pane` | `session_id`, `target_pane_id`, `placement`, `pane` | `layout` |
| `close_pane` | v4 only: `session_id,pane_id,disposition`; vNext rejects this generic shape, uses `detach_runtime_view` for view-only close and the exact typed runtime-owner terminate/kill/destroy operations for lifecycle | v4 `layout`; vNext `unsupported_protocol` |
| `resize_pane` | `session_id`, `pane_id`, `delta` | `layout` |
| `resize_divider` | `session_id`, `before`, `after`, `delta` | `layout` |
| `equalize_divider` | `session_id`, `before`, `after` | `layout` |
| `apply_layout_preset` | `session_id`, `preset` | `layout` |
| `focus_pane` | `session_id`, `target` | `layout` |
| `relocate_pane` | `session_id`, `moved`, `target`, `zone` | `layout` |
| `swap_panes` | `session_id`, `a`, `b` — superseded by `relocate_pane` | `layout` |
| `zoom_pane` | `session_id`, `pane_id` — **toggles** | `layout` |
| `duplicate_pane` | `session_id`, `pane_id` | `layout` |
| `change_pane_kind` | `session_id`, `pane_id`, operational display `kind` | `layout` |
| `reset_pane_kind` | `session_id`, `pane_id` | `layout` |
| `float_pane` | `session_id`, `pane_id`, `geometry` | `layout` |
| `dock_pane` | `session_id`, `pane_id` | `layout` |
| `set_floating_pane_geometry` | `session_id`, `pane_id`, `geometry` | `layout` |
| `attach_pane` | v4 only shape `session_id,pane_id,size,stream?`; vNext uses operation id+Surface operation sequence, exact connection/Surface/Installation-stream/Surface/Session/Pane/PaneNodeBinding revisions, AttemptOwner+live RuntimeAttempt/binding/PTY/buffer generations, bounded size/`stream=cells|bytes`, preassigned PaneAttachmentId+AttachmentGeneration+BaselineGeneration and current/no-current attachment CAS; reserves attach replay plus future detach fence before replacement | v4 `attached`; vNext `attached` with exact attachment/baseline/attempt/PTY/buffer generations and `RuntimeViewReplayFence(kind=attach)` |
| `resync_pane` | v4 only shape `session_id,pane_id`; vNext uses operation id+Surface operation sequence, exact owning connection/Surface/Installation-stream/Surface revision, retired PaneAttachmentId+AttachmentGeneration, AttemptOwner+RuntimeAttemptId+AttemptGeneration+binding/PtyGeneration/BufferGeneration and `stream=cells|bytes`; reserves resync replay plus future detach fence before CAS | v4 `screen`; vNext logical `terminal_screen` with fresh attachment/baseline identities and `RuntimeViewReplayFence(kind=resync)`, automatically chunked when>192 KiB |
| `pane_image` | `surface_id`, `session_id`, `pane_id`, exact attachment/attempt/PTY/buffer generations, `image_id`, fetch generation | v4 `pane_image`; vNext automatic bounded response stream |
| `detach_pane` | v4 only `session_id,pane_id`; rejected after vNext negotiation, whose sole view-detach wire operation is `detach_runtime_view` | v4 `ack`; vNext `unsupported_protocol` |
| `get_pane_history` | `session_id`, `pane_id`, `offset?` | `pane_history` |
| `search_pane` | `session_id`, `pane_id`, `query` | `pane_matches` |

Every Layout-mutating pane operation above answers with the resulting `layout` rather than an ack, so
the UI renders the daemon's arrangement instead of its own optimistic guess at what
a split, a collapse or a clamped resize did. Attach/detach and process-control operations acknowledge or
return their own runtime result without pretending they changed Layout.

Pane identity and runtime identity are many-to-one through `PaneNodeBinding`: one Pane binds at most one
node, one node may have zero or many Panes. `ProcessNode.pane_id` is not a v3 authority. Opening a semantic
subagent with no independent PTY yields a Preview/Process Details pane capability, not a terminal that
cannot work. Detach or `ClosePane(KeepProcesses)` never terminates the node; explicit `Terminate`/`Kill`
dispositions apply only where process-control rules allow them, and an Agent requires a separate node action.

#### Pane kind wire contract

`PaneKind` is a closed snake-case JSON enum. Its documentation slug is not a wire value:

| Wire value | Documentation slug | Operator label | Allowed by `change_pane_kind` |
| --- | --- | --- | --- |
| `terminal` | `terminal` | Terminal | yes |
| `agent` | `agent-terminal` | Agent terminal | yes |
| `shell` | `shell` | Shell | yes |
| `tui` | `terminal-app` | Terminal app | yes |
| `logs` | `logs` | Logs | yes |
| `test_output` | `test-output` | Test output | yes |
| `server` | `server` | Server | yes |
| `event_log` | `event-log` | Event log | no |
| `agent_tree` | `agent-tree` | Agent tree | no |
| `process_details` | `process-details` | Process details | no |
| `preview` | `preview` | Preview | no |
| `tmux_terminal` | `tmux-terminal` | tmux terminal | yes |
| `placeholder` | `placeholder` | Placeholder | no |

The [Pane and view type catalog](VIEW_TYPES.md) gives the renderer, input, detection, restore and fallback
contract for every row.

The five `no` values remain readable for migration, reserved and daemon-selected semantic views; they are
not operator-selectable saved-Pane overrides. `change_pane_kind` returns `invalid_argument` for them.
Automatic detection may still use `process_details` for a semantic Node without its own terminal, and a
durable tiled or floating Pane with that exact Node binding renders the bounded Node WorkSurface rather than
an empty terminal placeholder.

A persisted `Pane` separates launch from presentation while keeping the meaning an existing v4 client
assigns to `kind`:

- required `kind: PaneKind` is the current presentation and historical v4 renderer field.
  `Pane::presentation_kind()` returns it directly;
- optional `launch_kind?: PaneKind` is immutable launch intent when it differs from `kind`. When absent,
  `Pane::launch_kind()` falls back to `kind`; Template materialisation, restore and relaunch use this accessor;
- `kind_is_user_set: bool` is presentation provenance. It defaults to false when absent. False means
  Automatic and allows daemon detection to update `kind` while preserving launch intent; true means an
  operator pin that detection cannot overwrite;
- `detected_kind?: PaneKind` is the latest daemon-detected capability while a manual pin owns `kind`.
  It is absent for Automatic Panes and defaults to visible `kind` for legacy payloads. A terminal-shaped pin
  is attachable only when both its selected presentation and `detected_kind` are terminal-backed; the pin
  cannot borrow another semantic Node's PTY;
- provenance is authoritative independently of optional-field presence. A manual pin to the same value as
  launch intent has `launch_kind` absent and `kind_is_user_set: true`;
- `title`, `title_is_user_set`, `command`, `args`, `launch_profile`, `cwd`, `env`, `node_id` and `restore`
  retain their existing meanings. Presentation and provenance are not process, PTY, binding or lifecycle
  authority; `launch_kind` affects only a later authorised materialisation/restore.

For example, a Shell launch currently presenting a shell-hosted Agent is
`{"kind":"agent","launch_kind":"shell","kind_is_user_set":false,...}`. `NewPane.kind` is launch intent
on the creation request; the returned Pane begins Automatic with that same value in presentation `kind` and
no `launch_kind`. `change_pane_kind` changes presentation `kind` and provenance while preserving the effective
launch intent. `reset_pane_kind` clears manual provenance and derives `kind` from the bound semantic Node plus
terminal capability, or from `Pane::launch_kind()` when unbound.

`launch_kind`, `kind_is_user_set` and `detected_kind` are additive within protocol v4. A new client receiving an old Pane
defaults to Automatic and interprets historical `kind` as both presentation and launch. An old client
receiving a new Pane ignores the additive fields but continues to render the correct presentation from
`kind`. It cannot represent the split axes: if it rewrites a complete Layout, the rewritten Pane loses manual
provenance and collapses launch intent to its visible `kind`. That lossy old-writer round trip may therefore
change what a later restore launches, although merely reading the payload changes no command, current runtime
or lifecycle. Clients that edit complete Layouts must understand the additive fields to preserve the split.

`placement` is `replace_current`, `split_right`, `split_below` or `temporary`. Opening an existing node and
promoting its temporary view reuse the same vocabulary; only the temporary choice leaves the saved Layout
untouched. `create_pane` accepts the complete `NewPane`, including an arbitrary executable and argv without
shell evaluation. `duplicate_pane` creates another view of the same node rather than another process.
`change_pane_kind`, `reset_pane_kind`, floating, docking and geometry updates are likewise view-only
operations. `change_pane_kind` pins one of the eight operational presentations without changing the semantic
node, PTY or saved launch intent; daemon detection leaves that pin intact. `reset_pane_kind` removes the pin
and immediately derives the presentation from the bound semantic node and its terminal capability; an
unbound Pane returns to its immutable launch intent.

For Shell-hosted Agents, `hosted` is the daemon's lifecycle/relaunch and authenticated-hook receipt while
`observed_subject` is independent foreground terminal authority. Ctrl+Z can therefore remove A's terminal
capability without ending A; a detected B may own the same Shell PTY; and `fg` restores A's adapter and
integration tier without relaunching it. Before publishing output for a changed foreground process group,
the daemon reconciles and generation-fences durable and surface-scoped temporary exact views. The eager
barrier runs at most once per distinct job during a deferred sweep request. `attach_pane` fails with
`conflict` for every bound Node without a proved live/recovered terminal; only `node_id:null` may return a
blank terminal attachment.

Supervisor-originated `process.spawned_child` events keep `executable` and `args` as separate fields.
`executable` is the OS executable path (falling back to the OS process name only when the platform exposes no
path); `args` preserves argument boundaries and `command` remains the bounded display projection. Legacy
events omit `executable` and replay through the conservative command fallback. Live identity never tokenises
an executable path, never promotes mutable `argv[0]` over it and never searches arbitrary argv text. An
adapter-declared observation alias additionally requires argv[0] to name one of that same adapter's launch
executables; a filesystem alias must canonicalise to the kernel executable. Observation aliases never enter
the launch catalogue. Existing
wrapper paths are canonicalised before adapter suffix matching. Hosted PID corroboration binds the observed
adapter to the adapter recorded in the Node's launch metadata, rather than accepting a provider word in a
prompt. A same-provider interpreter child is coalesced only when its canonical wrapper subject equals its
parent's; this covers Gemini's intentional same-bundle Node relaunch without hiding a distinct Agent instance.

A floating Pane retains its split-tree position and exact point geometry, so docking restores it without
reparenting its node.
The wire-level `detach_pane` remains v4-only and is unrepresentable after vNext negotiation; vNext clients use
only `detach_runtime_view`, so they cannot bypass its exact generations or `RuntimeViewReplayFence`.
Likewise the v4 `close_pane` shape cannot carry Terminate/Kill into vNext: presentation close routes to
`detach_runtime_view`, while lifecycle routes to the exact reviewed runtime-owner operation. `float_pane`
remains the distinct saved-Layout operation.

- `direction`: `"horizontal"` \| `"vertical"`
- `target`: `{"kind":"pane","pane_id":…}` \| `{"kind":"next"}` \| `{"kind":"previous"}`
- `delta`: fraction of the parent split, positive to grow. Clamped so no pane can
  be resized out of existence.
- `resize_divider` supersedes the ambiguous leaf-only resize for interactive dividers. The ordered
  `before`/`after` pair identifies the exact boundary even when it separates nested subtrees.
- `equalize_divider` is the double-click operation and gives every sibling in that split an equal
  share. `preset` is one of `balanced`, `columns`, `rows`, `main_left`, `grid`; presets preserve Pane
  and Process identity and change geometry only.
- `relocate_pane` **moves** a pane: it leaves where it was and arrives beside `target`,
  so the layout changes shape. `zone` is `"left"` \| `"right"` \| `"above"` \| `"below"` \|
  `"centre"`. The four edges make `moved` a sibling of `target` on that side —
  left/right in a horizontal split, above/below in a vertical one — and `centre`
  exchanges the two panes in place. A pane relocated next to a target that already sits
  in a split of the required direction **joins** that split as a sibling rather than
  nesting a new two-way split inside it, so repeated rearrangement cannot turn a flat row
  into a staircase of nested splits and the dividers keep lining up. The space the moved
  pane vacates goes to its former siblings, a split left with one child collapses, and no
  pane is left below the minimum visible share. `moved == target`, an unknown pane and a
  single-pane layout are refused (`conflict`/`not_found`) rather than approximated.
- A relocation **starts and stops no process.** The pane keeps its id and its node
  binding, so a session full of running agents can be rearranged freely; only the Layout
  is written back, and only `layout_changed` is pushed.
- `swap_panes` is the older spelling of `relocate_pane` with `zone: "centre"`. It remains
  on the wire because a shipped client already sends it, and the daemon serves it through
  the same relocation — one behaviour with two names, not two implementations. New clients
  should send `relocate_pane`; nothing else it can express is missing from `relocate_pane`.
- `zoom_pane` leaves the layout tree untouched, so un-zooming restores the exact
  previous geometry. Zoom and focus both survive a relocation: moving a pane must not
  change what the user is looking at or typing into.
- `pane` (a `NewPane`): required launch `kind`; optional `title`, `command`, `args`, `launch_profile`, `cwd`
  and `env`; and `restore` defaulting to `reattach_only`. `launch_profile`, when present, is
  `{adapter_id,profile_id}` semantic launch policy; it never substitutes provider-specific flags into
  `args`. A `NewPane` has no `launch_kind`, `kind_is_user_set` or `detected_kind`: presentation starts Automatic and is
  changed only through the separate view operation. The daemon mints the `PaneId` — it is the only writer of
  state, and a client minting its own would collide with a second client on the same daemon.
- `stream`: `"cells"` (absent means cells) \| `"bytes"`. See §2. `size` is applied to
  the pty before the screen or replay is taken, so what comes back matches the geometry
  the client is about to render at. `rows * cols` over `max_screen_cells` is
  `invalid_argument`.
- In v4, `resync_pane(session_id,pane_id)` asks for a cells pane's whole screen after a missed update. It is
  read-only, requires a live cells attachment (`pane_not_attached` otherwise), and answers `conflict` for
  `bytes`, whose v4 recovery is a fresh attach/replay. In vNext, the client protocol runtime automatically
  invokes the versioned shape only for an exact retired generation after `terminal_output.gap`; it repeats
  Surface, stream and every owner generation and works for both cells and bytes. An omitted/stale/wrong-stream,
  still-active or already-replaced identity fails without creating state. Success mints fresh attachment/
  baseline identities and returns logical `terminal_screen`, streamed if>192 KiB and applied atomically only
  after digest/generation verification. No operator control or user retry is involved.
- `pane_image` fetches the pixels of one inline image the pane's screen refers to (§2.3). It
  is read-only, and `not_found` is a normal answer: an image that has scrolled out of the
  daemon's bounded store is gone, and saying so is better than handing back a different
  picture. The vNext shape validates the exact current cells attachment and reserves one of eight
  fetches/Surface,32/connection and128 installation-wide plus the≤4-MiB body/≤128-MiB family/shared allowance.
  It then uses the automatic chunked-response framing from §1; no encoded application frame exceeds256 KiB.
  Contiguous offsets and the content-derived ImageId/digest must agree before one atomic transfer to the
  reserved client cache. Gap, detach, reselection, generation change or disconnect discards all partial bytes.
- `get_pane_history` reads a screen-shaped window of the pane's **scrollback**, as cells.
  The scrollback belongs to the daemon because the daemon is the only thing that has it: a
  client is sent the screen, and a pane that printed five hundred lines between two
  coalesced updates never sent the four hundred and eighty in the middle. `offset` is rows
  above the top of the live screen and is **clamped** to what the daemon still holds, so
  scrolling past the beginning answers with the oldest window rather than an error. The
  answer says which offset it actually is (`scrollback_offset`) and how deep the record goes
  (`scrollback_len`). Read-only: the daemon borrows its parser's viewport and puts it back,
  so one client's history read cannot move another client's screen. A `bytes` attachment
  answers `conflict` — history as cells is not what that client is rendering.
- `search_pane` searches everything the daemon retains for the pane: the history, then the
  live screen. `query` is `{"text":…,"mode":"literal"|"regex","case_sensitive":bool}`, where
  absent `mode` is `literal` and absent `case_sensitive` is false — case-insensitive is what
  a user means by default. Every part of it is bounded, and the bounds are the reason a
  pattern from a text field is safe to accept:

  | Bound | Value | Why |
  | --- | --- | --- |
  | `text` length | 256 characters | a search box is not a program; longer is `invalid_argument` |
  | compiled pattern | 1 MiB of automaton, same cap on its lazy DFA | a generated pattern cannot make the daemon allocate |
  | match time | linear in the row | a finite automaton, so `(a+)+$` cannot be made to backtrack |
  | rows read | 8,192 | above the 5,000-row scrollback, so a real search is never cut short |
  | matches returned | 1,000, and 64 from any one row | `a` against a row of `a`s is a real thing a user types |

  A pattern that will not compile is `invalid_argument` **with the reason in the message**,
  because "invalid regular expression" with nothing else is a dead end. When a cap stops the
  scan the answer sets `truncated`, so a UI says "1000+" rather than implying it counted them
  all. Read-only, and it moves nobody's viewport.

### PTY

| `op` | Fields | Answers with |
| --- | --- | --- |
| `write_pty` | `session_id`, `node_id`, `data` (base64) | `ack` |
| `resize_pty` | `session_id`, `node_id`, `size` | `ack` |

Addressed to the **node**, not the pane: the pty belongs to the process, and one
process may be shown in more than one place.
These are v4-only local single-client operations. A vNext, remote or multi-client peer must use the
lease/attempt/binding-fenced `write_runtime_input` and `resize_runtime_input` target operations above; the
daemon refuses this legacy shape after such negotiation.

### Agent context handoff

| `op` | Fields | Answers with |
| --- | --- | --- |
| `prepare_context_handoff` | `session_id`, `source_node_id`, `target_node_id`, `mode`, `instruction?` | `context_handoff` |
| `deliver_context_handoff` | `session_id`, `handoff_id` | `ack` |

This is the **implemented v4** two-request, review-before-send capability. `prepare_context_handoff` verifies two distinct
agentic nodes in one active Session and one of four explicit intents: `continue_with`, `review_handoff`,
`second_opinion` or `promote_to_main`. It assembles a bounded packet containing Session objective/summary,
real Git root/branch/HEAD/status/diff evidence, relevant files, stable Activity Preview decisions, pending
work, commands and exit codes, tests, subagents, recent events, active processes and prior handoff metadata.
Typed credential fields are omitted, known-secret/secret-shaped text is redacted, and the response carries
the **exact** text the daemon retains for operator review. No pattern detector proves arbitrary free text
secret-free. It never reads raw terminal scrollback and never writes a PTY. Hidden previews remain hidden.

The returned `handoff_id` is an opaque, short-lived capability bound to the preparing client, Session and
destination. `deliver_context_handoff` accepts only that id; a client cannot replace the reviewed body on
the delivery request. The daemon revalidates that the destination is an idle, controllable Agent with a
live Turn-owned PTY and no pending question or permission, then submits one bracketed paste. Closing or
opening panes is irrelevant and no layout operation is implied.

A successful same-connection retry is idempotent. A possibly partial PTY write is fenced as `conflict` and
is never replayed automatically. Drafts expire after ten minutes and are discarded when their client
disconnects. Success means the payload was submitted to the PTY; it is not proof that the Agent accepted or
acted on it. Handoffs deliberately cannot answer permission or question prompts; those remain explicit
`write_pty` input. A metadata-only `context_handoff.finished` event records source, destination, intent and
submitted/uncertain outcome under normal event-retention policy. The handoff subsystem creates no dedicated
durable semantic body record: its draft is memory-only. Once submitted, the same bytes may persist in the
destination provider transcript, visible terminal/scrollback and ADR-052 terminal journal; Turn's review
must disclose that downstream retention and revocation cannot recall it.

ADR-059 retains this security boundary and specifies the planned `ContextPacket` successor. Its
`prepare_context_packet`/`deliver_context_packet` operations replace rather than alias the v4 pair because
their schemas and guarantees differ. Preparation creates only an expiring draft and optional target spec.
Delivery can include adapter-normalised turns and reviewed short-lived pull grants, target a newly created
stable Node + AgentInstance, carry a context-window-aware budget manifest and record evidence-backed
submitted/received/read/acted observations independently. New-target provisioning/launch/delivery
is a fenced idempotent saga; an uncertain write is never retried. It never creates a dedicated semantic
packet-body record, substitutes
an unreviewed delivery payload, writes into a pending interaction or treats submission as receipt;
downstream provider/terminal retention still applies. `docs/AGENT_NODE_VIEWS_AND_CONTEXT.md` is normative
for that target; the richer fields are not part of protocol v4.

The vNext packet has one closed control state, an independent body-authority variant and append-only semantic
evidence:

```text
PacketAuthority = AdHocDraft(body=live|consumed|lost,
                             review=pending|reviewed|review_required)
                | FlowRecipe(policy_revision, recipe_hash,
                             body=reassemblable|live|consumed|lost,
                             review=preauthorised|review_required)
DeliveryState = draft | reviewed |
                delivery_started(phase) |
                launch_unconfirmed | grant_install_unconfirmed |
                submitted_unconfirmed | finished |
                failed(reason) | draft_lost
phase = provisioning | launching | grant_pending | submitting | awaiting_evidence
reason = expired | refused | target_incompatible | launch_failed | grant_failed |
         write_definitely_failed | policy_invalid | operator_cancelled
Evidence = { submitted?: EvidenceFact, received?: EvidenceFact,
             read?: EvidenceFact, acted?: EvidenceFact }
```

The legal transitions are total:

| From | May transition to |
| --- | --- |
| `draft` | `reviewed`, `draft_lost`, `failed(expired\|refused\|policy_invalid\|operator_cancelled)` |
| `reviewed` | `delivery_started(provisioning\|grant_pending\|submitting)`, `draft_lost`, `failed(expired\|refused\|target_incompatible\|policy_invalid\|operator_cancelled)` |
| `delivery_started(provisioning)` | `delivery_started(launching\|grant_pending\|submitting)`, `failed(target_incompatible\|launch_failed\|operator_cancelled)` |
| `delivery_started(launching)` | `delivery_started(grant_pending\|submitting)`, `launch_unconfirmed`, `failed(launch_failed\|operator_cancelled)` |
| `delivery_started(grant_pending)` | `delivery_started(submitting)`, `grant_install_unconfirmed`, `failed(grant_failed\|operator_cancelled)` |
| `delivery_started(submitting)` | `delivery_started(awaiting_evidence)`, `submitted_unconfirmed`, `failed(write_definitely_failed)` |
| `delivery_started(awaiting_evidence)` | `finished`, `submitted_unconfirmed` |
| `launch_unconfirmed` | `delivery_started(launching\|grant_pending\|submitting)` after explicit exact reconciliation with the live body, or `draft_lost\|failed(launch_failed\|operator_cancelled)` |
| `submitted_unconfirmed` | `finished` only from independently correlated submission/receipt evidence |
| terminal `grant_install_unconfirmed\|finished\|failed\|draft_lost` | none for the same operation id |

Cancellation after an external-effect intent can select `operator_cancelled` only after that effect is
proved not to have started; otherwise the corresponding unconfirmed state is mandatory.
Body-authority transitions are also closed. Ad-hoc preparation is `live/pending + draft`; review makes it
`live/reviewed`. Flow preparation is `reassemblable/preauthorised + draft|reviewed`; accepting delivery
materialises the deterministic bytes as `live/preauthorised` before the first effect. Pre-write phases require
a live reviewed/preauthorised body. Committing the write intent atomically changes `live → consumed`; only a
consumed body may reach awaiting-evidence, submitted-unconfirmed or finished. A terminal pre-write refusal,
expiry, cancellation, draft loss or uncertain/revoked grant discards bytes as `lost/review_required`.
`launch_unconfirmed` may retain a live body only in the same daemon generation; after body loss it may only
reconcile the process and then enter draft-lost/failed, never a write phase. `failed(write_definitely_failed)`
and every submitted state retain `consumed`; `failed(policy_invalid)` requires Flow `lost/review_required`.
Evidence is empty through `delivery_started(submitting)` except for durable effect intents. It may accrue
only in `awaiting_evidence|submitted_unconfirmed|finished`; every field has its own source/revision/time and
none implies another. `finished` requires independently proved submission but does not require receipt/read/
acted. The decoder/store rejects any state/phase/reason/evidence combination not listed above.

One packet body is≤1 MiB and its inert review≤1 MiB, for a≤2-MiB working-set reservation. One source connection
holds≤16 unaccepted drafts; installation-wide there are≤128 live draft-or-accepted bodies and≤256 MiB packet
working sets, all also charged to `runtime.turn_variable_rss_mib`, with TTL≤600 seconds and one of 10,000
body-free metadata/replay slots reserved before preparation. Delivery acceptance atomically changes the owner
from connection+Surface to `(daemon generation,owning Workspace,ContextPacketDeliveryId,target generation)`.
Item/count/family/shared/metadata N+1 refuses before read/assembly/provision/launch/grant/write.

An ad-hoc draft is client/surface/daemon-generation bound and memory-only. A disconnect before delivery
acceptance changes `draft|reviewed → draft_lost`; after acceptance the same daemon may hold the bounded body
only for the in-flight saga and source disconnect cannot release it. Definite pre-write terminal/expiry releases
only after encoder/write-buffer quiescence; possible write remains charged until the same proof, and daemon
death proves memory reclamation before recovery. Durable hash/manifest metadata cannot reconstruct it. A still-valid Flow recipe
may be reassembled only into a new packet and operation id; the old delivery never advances from a hash.
Daemon restart first classifies each durable effect intent: proven submission becomes `finished`, a possible
write becomes `submitted_unconfirmed`, ambiguous launch becomes `launch_unconfirmed`, and ambiguous bearer
installation becomes terminal `grant_install_unconfirmed` after revocation. Every definitely pre-write state
whose body died becomes `draft_lost`; a Flow recipe may then create a new operation only if its exact policy,
grant and destination remain current. Restart itself never launches, installs or writes. Within one daemon
generation, `launch_unconfirmed` may advance only after an explicit operator reconciliation proves/adopts the
preassigned process or proves no launch and the same reviewed body remains live. No ambiguous effect is
replayed.

`ContextPacketDeliveryView` is durable and queryable by operation id. It repeats packet/target identities,
preassigned attempt/launch nonce, target generation, PacketAuthority metadata without body, the exact closed
DeliveryState and independent Evidence. Launch, bearer installation and context write are distinct external
effects with a durable intent before each.

An optional reviewed ContextLink is committed `pending_activation` only after the target attempt is fenced.
Its short-lived broker bearer is installed only through the adapter's inherited descriptor/owner-only
attempt file; the packet envelope carries a non-secret link descriptor. A PTY-only adapter without that
channel refuses grant-bearing delivery rather than writing authority into a terminal/transcript. It becomes
usable immediately before the one write; launch failure, definite write failure or uncertain write revokes
it, while evidenced submission keeps it only to its reviewed expiry/budget. A failed `provisioning` target
remains visible with the exact phase/error and semantic retry or `destroy_runtime_owner` action; no timeout
silently deletes it.

### Node control

| `op` | Fields | Answers with |
| --- | --- | --- |
| `interrupt_node` | v4 only `session_id,node_id`; vNext uses exact `interrupt_runtime_owner` | v4 `ack`; vNext `unsupported_protocol` |
| `terminate_node` | v4 only `session_id,node_id`; vNext uses reviewed exact `terminate_runtime_owner` | v4 `ack`; vNext `unsupported_protocol` |
| `kill_node` | v4 only `session_id,node_id`; vNext uses reviewed exact `kill_runtime_owner` | v4 `ack`; vNext `unsupported_protocol` |
| `relaunch_node` | v4 only `session_id,node_id,resume?`; vNext separates `resume_agent_instance|restart_runtime_owner|create_agent_instance` and accepts no resume boolean | v4 `node`; vNext `unsupported_protocol` |

These four shapes are decode-only v4 compatibility and are rejected after vNext negotiation; no dispatcher
alias may fill their missing AttemptOwner/attempt/binding/target/handle/generation, consequence review or
receipt fields. In historical v4, `interrupt_node` writes the interrupt character through the tty so it reaches the
whole foreground process group, not only the process Turn spawned. `resume` asks
the adapter to continue the agent's previous conversation where the tool supports
it; that boolean has no vNext meaning.

### Attention

| `op` | Fields | Answers with |
| --- | --- | --- |
| `next_attention` | — (a read-only peek) | `attention` |
| `list_attention` | `session_id?` | `attention_list` |
| `goto_attention` | `attention_id?` (absent = next) | `effects` |
| `acknowledge_attention` | `attention_id` | `ack` |
| `snooze_attention` | `attention_id`, `until_ms` | `ack` |
| `set_attention_priority` | `attention_id`, `priority_boost` (-100..100) | `ack` |
| `dismiss_attention` | `attention_id` | `ack` |
| `mute_session` | `session_id`, `until_ms?` (absent = unmute) | `ack` |
| `correct_state` | `session_id`, `node_id`, `lifecycle?`, `turn?`, `note?` | `node` |

`goto_attention` is a user-initiated move, so it bypasses the focus governor's
guards — pressing the shortcut is consent. It still resets the rate limiter so
automatic focus does not immediately fight manual navigation.

This paragraph is the implemented v4 behavior. ADR-059 does not silently extend this shape: incompatible
vNext replaces it with surface-scoped `route_attention`, whose answer carries a tagged `exact`, `provisional`
or `unassigned` subject. Only `exact` may include a verified interaction owner and Node View bootstrap; the
other tags carry their exact provisional demand view and never borrow input. Selection/rendering still never
acknowledges the entry, and a response remains pending until adapter evidence confirms that exact prompt
ended.

A muted session still badges. Muting silences the interruption, not the evidence, and its
deadline is restored after a daemon restart. Snooze, dismiss, acknowledgement and explicit
queue priority are stored with the queue before its reordered projection is pushed.

```jsonc
// The user fixing a state Turn got wrong. Recorded with
// EventSource::UserCorrection at explicit confidence: on the question of what is
// actually happening in their terminal, the human outranks every heuristic.
{"v":4,"type":"request","id":"r-6","request":{
  "op":"correct_state","session_id":"sess_4b71e0","node_id":"proc_7a12ff",
  "turn":{"kind":"active"},"note":"still working"}}
```

### User activity

| `op` | Fields | Answers with |
| --- | --- | --- |
| `update_user_activity` | `context` | `effects` |

```jsonc
{"v":4,"type":"request","id":"r-4","request":{
  "op":"update_user_activity","context":{
    "last_keystroke_ms":1700000000000,"app_foreground":true,
    "active_session":"sess_4b71e0","sensitive_operation":false}}}
```

This is `turn_core::attention::UserContext`, sent as itself. It is what the focus
governor needs to decide whether it may move the user.

This is likewise the v4 shape. ADR-059 replaces it with `update_surface_activity`, binding activity and any
automatic route to one connected surface/connection generation as specified in the accepted target table;
vNext never guesses a window from `active_session` alone.

- Send state transitions immediately: the first keystroke of a burst, window focus,
  active Session and sensitive-operation changes. During a continuing burst, coalesce
  a bounded heartbeat carrying the latest timestamp before `TYPING_GRACE_MS` can
  expire at the daemon. Do not send one request per character, and do not send idle
  periodic traffic.
- The daemon derives "is typing" from `last_keystroke_ms` rather than trusting a
  boolean a client might forget to clear.
- Set `sensitive_operation` while something must not be interrupted: a permission
  prompt being read, a paste in flight, a modal open.
- It answers with `effects` because the governor may release a deferred focus jump
  the moment the user's hands leave the keyboard.

### `disposition` — closing

Required, with no default. "Close" is ambiguous — the whole point of the daemon is
that processes outlive the UI — and a daemon guessing would either kill work the
user wanted kept or leak processes they thought were gone.

| Value | Meaning |
| --- | --- |
| `keep_processes` | Send no process stop. For Session End the active Session row still leaves and every live process is rehomed or recovery-inventoried; presentation-only detach is a different operation. `close_workspace` retains its Workspace container while applying its per-Session contract. |
| `terminate` | Ask them to stop, the way closing a terminal would. |
| `kill` | Stop them without asking. |

Closing only a view and archiving a Session retain its row and therefore retain a live writer lease. End
Session is different: the Session row always leaves. In the same serial transaction, if the exact surviving
writer is validly rehomed to an existing destination Session over the same canonical checkout identity, the
lease owner transfers atomically, its generation increments and the host lock remains held. Otherwise the
lease owner becomes the closed
`ended_session(SessionId,SessionGeneration,CheckoutId,ending_generation)` tombstone with state
`recovery_required`; there is no dangling Session foreign key. The tombstone grants no input, process,
filesystem or writer authority and `keep_processes` sends no signal, but it preserves the exact process-start/
host-lock evidence and continues to fence a second writer. `release_workspace_write_lease` is the only target
reducer for that tombstone and succeeds only under its exact generation plus no-owned-live-runtime and OS
lock/process-start quiescence or reclamation proof; it never force-takes over or stops work. Deleting the
Workspace atomically replaces any unresolved lease with its pre-reserved Installation-owned
checkout-identity/generation/high-water fence before deleting the Workspace row, so neither Session nor
Workspace deletion can leave a dangling owner or refuse for fence capacity. Before restoring Sessions or
emitting any heartbeat, a new daemon changes every non-`released` current-owner lease to `recovery_required`
while preserving its id, generation and previous heartbeat. Loading the former owner never auto-adopts that
authority. **Historical protocol-v4 clause:** `acquire_workspace_write_lease` could
promote a stopped read-only Session to `main_checkout`. Target clients cannot invoke that transition;
write escalation provisions a dedicated worktree, and primary lease records are decode/quarantine/cleanup
evidence only.

The wire lease is also the owner record for a uid-scoped host checkout lock independent of the daemon's
data directory. Acquisition takes that lock before committing SQLite; heartbeat and launch require both.
Symlink aliases collide and distinct checkout/worktree directory identities do not. If contention comes from
another daemon, `workspace_write_lease_conflict` carries that daemon's owner but omits `focus_owner`, because
the current socket cannot focus a Session owned by another daemon; the remaining alternatives are
`create_read_only`, `create_isolated_worktree`, and `cancel`. A surviving writer process can retain this lock
after daemon loss, so timeout or daemon death alone never authorises takeover.

### Examples

```jsonc
{"v":4,"type":"request","id":"r-1","request":{
  "op":"get_hierarchy","surface_id":"main-window","include_archived":false}}

// Cells, because the field is absent. What a renderer wants.
{"v":4,"type":"request","id":"r-2","request":{
  "op":"attach_pane","session_id":"sess_4b71e0","pane_id":"pane_11c3d8",
  "size":{"rows":40,"cols":120}}}

// The escape stream instead, for something that needs the bytes themselves.
{"v":4,"type":"request","id":"r-2b","request":{
  "op":"attach_pane","session_id":"sess_4b71e0","pane_id":"pane_11c3d8",
  "size":{"rows":40,"cols":120},"stream":"bytes"}}

// Answering an agent's y/n prompt. There is no "approve" request; this is it.
{"v":4,"type":"request","id":"r-3","request":{
  "op":"write_pty","session_id":"sess_4b71e0","node_id":"proc_7a12ff","data":"eQ0="}}

{"v":4,"type":"request","id":"r-5","request":{
  "op":"close_session","session_id":"sess_4b71e0","disposition":"keep_processes"}}

// Review first. This response writes no PTY.
{"v":4,"type":"request","id":"r-7","request":{
  "op":"prepare_context_handoff","session_id":"sess_4b71e0",
  "source_node_id":"proc_source","target_node_id":"proc_reviewer",
  "mode":"review_handoff",
  "instruction":"Check the assumptions before continuing."}}

// After displaying the exact context_handoff.body and receiving explicit consent:
{"v":4,"type":"request","id":"r-8","request":{
  "op":"deliver_context_handoff","session_id":"sess_4b71e0",
  "handoff_id":"handoff_86d451"}}
```

---

## 7. Responses — daemon → UI

Result shapes are tagged `result`. Each request names exactly one
(`Request::expected_result`), and a test asserts that every name it produces exists
in this catalogue, so the pairing above is load-bearing rather than documentation
that might be stale. Failures never arrive as a response; they arrive as an
`error` frame (§5).

| `result` | Payload |
| --- | --- |
| `ack` | — |
| `workspaces` | `workspaces: [WorkspaceSummary]` |
| `workspace` | `workspace: WorkspaceSummary` |
| `hierarchy_index` | vNext `snapshot: HierarchyIndexSnapshot≤6 MiB`, first daemon-minted HierarchyScanId and first≤500-row/1-MiB page |
| `hierarchy_page` | vNext exact scan/revision/page ordinal/predecessor digest,≤500 summaries/1 MiB and complete/partial/gapped coverage |
| `hierarchy_reveal` | vNext exact target/path plus≤1-MiB materialising pages or typed stale/gap |
| `hierarchy` | `snapshot: HierarchySnapshot` |
| `inspector` | `details: InspectorDetails` |
| `surface_state` | daemon-minted SurfaceId, owner, state revision, connection generation, dormant deadline and bounded TreeSurfaceState |
| `tree_state` | `state: TreeSurfaceState` |
| `workspace_write_lease` | `workspace_id`, `lease?: WorkspaceWriteLease` |
| `sessions` | `sessions: [SessionSummary]` |
| `session` | `session: SessionSummary` |
| `session_details` | `details: SessionDetails` |
| `templates` | `templates: [TemplateSummary]` |
| `template` | `template: TemplateSummary` |
| `template_details` | `template: Template` |
| `layout` | `session_id`, `layout: Layout` |
| `attached` | `attachment: PaneAttachment` |
| `screen` | `session_id`, `pane_id`, `node_id?`, `next_seq`, `grid: Grid` |
| `terminal_screen` | vNext exact retired identity plus fresh PaneAttachmentId+AttachmentGeneration+BaselineGeneration, all owner generations, stream and one complete cells grid or parsed byte replay as an atomic logical result |
| `tree` | `session_id`, `nodes: [TreeNodeView]` |
| `node` | `node: TreeNodeView` |
| `node_pane` | `pane: NodePaneView` |
| `pane_focus` | `focus?: PaneFocusView` — absent when no safe existing Pane can receive focus |
| `attention` | `entry?: AttentionView` — absent when the queue is empty |
| `attention_list` | `entries: [AttentionView]` |
| `effects` | `effects: [Effect]` |
| `preview_history` | `session_id`, `node_id`, `entries: [ActivityPreview]` (newest first) |
| `context_handoff` | `handoff: ContextHandoffView` — ids, mode, safe labels, exact redacted `body`, preview/history counts, repository evidence flag and redaction flag |

```jsonc
{"v":4,"type":"response","id":"r-3","response":{"result":"ack"}}
```

### `attached` — the feature made visible

```jsonc
{"v":4,"type":"response","id":"r-2","response":{"result":"attached","attachment":{
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "stream":"cells",
  "screen":{"rows":40,"cols":120,"cursor":[1,0],
            "runs":[[{"t":"ready","n":5,"f":[0,205,0]},{"n":115}],
                    [{"n":120}],"…"]},
  "scrollback":{"cols":120,"rows":[[{"t":"previous output","n":15},{"n":105}]]},
  "size":{"rows":40,"cols":120},
  "scrollback_truncated":false,"bytes_seen":12,"next_seq":0}}}
```

This is what makes "processes survive UI restarts" a demonstrable feature rather than a
claim. The daemon held the pty the whole time; the screen it hands over reproduces the
pane exactly as the user left it.

Exactly one payload is present, decided by `stream`:

- **`screen`** for a cells attachment — a `Grid` (§2.2), plus bounded styled
  `scrollback` rows when history exists. `replay` is absent.
- **`replay`** for a byte attachment — the **parsed screen re-emitted**, not the raw
  scrollback, because a truncated raw ring can begin mid-escape-sequence and corrupt the
  receiving terminal. `screen` is absent.

Sending both would double the cost of every attach to serve a client that asked for one.

- `size` is the applied live PTY size. For a display-only terminal recovered after a
  daemon restart it is the historical size the archived grid was recorded at.
- `scrollback` is oldest-first compact `CellRun` rows, validated against its `cols`.
  It is capped at 5,000 rows and a 3 MiB serialized budget so the whole attachment
  stays inside the frame limit.
- `scrollback_truncated` means output was discarded by memory/disk rotation or the
  attachment budget. The screen is still correct; the history above it is incomplete,
  and the UI must mark the boundary rather than let the user scroll into a lie.
- `next_seq` is the `seq` the next update for this attachment will carry — a
  `pane_screen` for cells, a `pane_output` for bytes — so a client can detect a gap
  between what it was handed and the live stream.
- `node_id` is absent for a pane with no process — an empty slot after a partial restore,
  or one of Turn's own views. A cells attachment to one still gets a `screen`: a blank
  grid at the client's size, because a renderer with nothing to draw is worse than one
  drawing an empty pane.

### `screen` — the answer to `resync_pane`

```jsonc
{"v":4,"type":"response","id":"r-7","response":{"result":"screen",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "next_seq":312,"grid":{"rows":40,"cols":120,"…":"…"}}}
```

The same `Grid` an attach would return, so a client's recovery path and its first-render
path are one piece of code. `next_seq` is the sequence number the next `pane_screen` will
carry, and the grid is the state as of just before it — the daemon answers with the exact
screen its next diff is computed against, not with a fresher read of the pty. A fresher
one would look more helpful and be wrong: a row that changed and changed back in between
would never be corrected.

### `pane_image` — the answer to `pane_image`

```jsonc
{"v":3,"type":"response","id":"r-7","response":{"result":"pane_image",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8",
  "image":{"id":6023794128384115081,"width":240,"height":160,"pixels":"<base64 RGBA>"}}}
```

That object is the implemented v4 answer. The incompatible vNext answer carries the same logical image
metadata in `response_stream_begin(content_kind=pane_image)` and its RGBA through≤180-KiB raw chunks under the
declared total/digest. A client never accepts a mixture of the two shapes under one negotiated protocol.

`pixels` is `width * height * 4` bytes of RGBA — unassociated alpha, sRGB, row-major, no
padding — and it is by a wide margin the largest thing this protocol carries. `id` is derived
from the contents, so the same picture printed twice is one payload and a client's cache
survives a re-attach.

A receiver **must** check the three things before trusting it, and
`turn_proto::ImagePayload`'s decoder does: `width * height` against `max_image_pixels`, the
byte length against those dimensions, and the id against the hash of the pixels. The last
one matters because a cache keyed by id would otherwise be poisonable by a payload arriving
under somebody else's name.

### `pane_history` — the answer to `get_pane_history`

```jsonc
{"v":3,"type":"response","id":"r-8","response":{"result":"pane_history",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "grid":{"rows":40,"cols":120,"scrollback_offset":1240,"scrollback_len":5000,"…":"…"}}}
```

The same `Grid` shape a live screen arrives in, so a client paints history with the code it
already has rather than with a second renderer. It carries no cursor: the cursor is on the
live screen, and drawing it at the same coordinates over history would put it on an
unrelated character. `scrollback_offset` is the offset actually served after clamping, and
`scrollback_len` is the depth of the record — together they say which absolute rows the
window holds, so a client can file them and know when it has reached the beginning.

### `pane_matches` — the answer to `search_pane`

```jsonc
{"v":3,"type":"response","id":"r-9","response":{"result":"pane_matches",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "outcome":{"matches":[{"line":1240,"col":4,"cols":5},{"line":4812,"col":0,"cols":5}],
    "scanned_lines":5040,"total_lines":5040,"screen_rows":40,"scrollback_len":5000}}}
```

A match is a **line index** and a **column range**:

- `line` `0` is the oldest row the daemon still holds; `scrollback_len` is the line index of
  the live screen's top row, and `scrollback_len + screen_rows - 1` is the bottom of it. That
  is the only coordinate both ends can compute. `offset = scrollback_len - line` is the
  `get_pane_history` offset that puts the match on the top row; centring it is
  `scrollback_len - (line - screen_rows/2)`, clamped — `turn_proto::search::viewport_offset`.
- `col`/`cols` are **columns, not characters**. A wide glyph is one character of text and two
  columns of screen, so a character offset would highlight the wrong cells on every row
  containing an ideograph or an emoji.
- Matches are ordered oldest first, so "next" moves towards the live screen.
- `scrollback_len` is the depth the search was taken at. A client that later sees a different
  one knows the line indices may have moved — rows drop off the top once the ring is full —
  and re-runs the query rather than scrolling to a line that has since shifted.
- `truncated` (absent when false) means a cap stopped the scan and the count is a floor.
- While a full-screen application is in front there is no history to search: the alternate
  screen has no scrollback of its own, so `scrollback_len` is `0` and the search covers what
  is on screen. Turn stands down from scrolling for the same reason.

### `effects`

`turn_core::attention::Effect`, passed through unchanged. The manager already
decided what may happen, including whether a focus change was granted, deferred or
refused; re-describing that verdict here would be a second place to get it wrong.

```jsonc
{"result":"effects","effects":[
  {"effect":"badge","session_id":"sess_4b71e0","count":1},
  {"effect":"focus_deferred","session_id":"sess_4b71e0","until_ms":1700000001500,
   "reason":"user_typing"},
  {"effect":"focus_denied","session_id":"sess_4b71e0","reason":"rate_limited"}]}
```

A client **must not** treat `focus_deferred` or `focus_denied` as a jump. Only
`focus` moves the user. Tags: `badge`, `highlight`, `play_sound`, `notify`,
`enqueued`, `focus`, `focus_deferred`, `focus_denied`, `run_custom`, `cleared`.

In the incompatible ADR-059 protocol, `notify`, `focus` and `focus_deferred` additionally carry the opaque
`attention_id`, daemon generation and daemon-resolved `AttentionRoute` (or a route token resolving to the
same immutable subject). Notification activation revalidates that id. A deferred effect keeps the route;
clients never reconstruct a Node, Pane or Session target from notification text.

---

## 8. Server pushes — daemon → UI

Pushes are tagged `event`, wrapped in `{"type":"event","event":{…}}`. They carry no
request id because no request caused them.

| `event` | Payload |
| --- | --- |
| `pane_screen` | `session_id`, `pane_id`, `node_id?`, `seq`, `update: ScreenUpdate` |
| `pane_output` | `session_id`, `pane_id`, `node_id?`, `seq`, `data` |
| `pane_output_gap` | `session_id`, `pane_id`, `dropped`, `resume_seq` |
| `node_state_changed` | `session_id`, `node_id`, `lifecycle`, `turn?`, `display_state`, `caused_by?` |
| `session_state_changed` | `session: SessionSummary` |
| `session_removed` | `session_id`, `workspace_id` |
| `turn_event_emitted` | `turn_event: TurnEvent` |
| `attention_effect` | `effect: Effect` |
| `attention_queue_changed` | `entries: [AttentionView]` |
| `tree_changed` | `session_id`, `nodes: [TreeNodeView]` |
| `hierarchy_changed` | v4 `snapshot: HierarchySnapshot` full replacement; vNext ordered `HierarchyDelta`≤4,096 compact topology/flag/RowMetricClass ops and≤180 KiB serialized with a complete encoded frame≤256 KiB, or `gap(affected_scope,minimum_revision)` requiring automatic compact-index+affected-visible-page refresh—never a vNext full detailed replacement or chunked unsolicited push |
| `activity_preview_changed` | `hierarchy_revision`, `session_id`, `node_id`, `preview?: ActivityPreview` |
| `pane_bindings_changed` | `hierarchy_revision`, `session_id`, `node_id`, `bindings: [PaneNodeBinding]` |
| `workspace_write_lease_changed` | `hierarchy_revision`, `workspace_id`, `lease?: WorkspaceWriteLease` |
| `layout_changed` | `session_id`, `layout` |
| `pty_resized` | `session_id`, `node_id`, `size` |
| `restore_result` | `session_id`, `state`, `needs_explanation`, `panes` |
| `status_event_changed` | common push envelope, exact StatusEventOwner, owner stream revision, StatusEventId/revision or `gap(minimum_revision)`, severity/state and≤4-KiB safe text-key/arguments/progress/recovery metadata |
| `diagnostic_log_changed` | local-foreground subscribed common push envelope, exact daemon/log generation+revision, sequence and one≤4-KiB structured redacted row or `gap(earliest_sequence)`; never a raw body, environment value, credential or absolute path |
| `presence_chat_changed` | authorised encrypted full-GUI peer envelope, exact client/session/Workspace/Surface/connection+message generation/revision, expiry and sanitised body≤512 bytes/256 scalars or live tombstone; never durable/offline replay or control |
| `attention_changed` | common push envelope, Installation queue revision, AttentionId, exact tagged subject+subject stream revision, route/state or `gap(minimum_revision)`; never reconstructs a destination from display text |
| `pending_interaction_changed` | common push envelope, Workspace/attempt/input-route and PendingInteraction id/revision/state plus bounded safe option metadata or `gap`; never prompt/credential body |
| `directory_changed` | common push envelope, DirectoryWatchId/generation, target/root/directory identity+revision, watch sequence and bounded `delta|gap(reason,resnapshot_required)` |
| `resource_inventory_changed` | common push envelope, LiveSubscriptionId, exact ResourceScopeKey/target generation/coverage watermark and bounded process/resource delta or `gap(resnapshot_required)`; never argv/environment/body |
| `target_recovery_changed` | local-administrative common push envelope, LiveSubscriptionId, exact ExecutionTarget/target-stream revision and bounded recovery identity/state delta or `gap(resnapshot_required)`; absent from remote registry |
| `account_activity_changed` | common push envelope, LiveSubscriptionId, exact provider/Profile/Target/source generation, coverage/freshness and bounded quota/context/activity delta or `gap(resnapshot_required)`; never raw provider body |
| `live_notification_status_changed` | common push envelope, LiveSubscriptionId, exact endpoint/scope/grant/live generation and bounded start/update/end or `gap(resnapshot_required)`; never plaintext notification body |
| `web_preview_changed` | common push envelope, exact owning connection generation+Surface+WebPreviewLoadStateId/revision and Node/source revision, closed load state, bounded transferred/decoded progress and outcome or `gap`; never private URL path, response/header/body, DOM or renderer bytes |
| `browser_download_changed` | common push envelope, Workspace+BrowserDownloadQuarantineId/revision, Browser/partition/navigation/response identity, closed state, expiry and bounded byte progress or `gap`; sealed may expose reviewed size/type/hash but never payload/path bytes |
| `media_import_changed` | common push envelope, MediaImportId/revision, destination, reserved Node/blob ids, closed state, bounded progress/error and no source bytes/path beyond display policy |
| `commit_proposal_changed` | common push envelope, CommitProposalId/revision, repository/index hashes, closed state and bounded omission/output metadata only for an authorised subscriber |
| `transfer_changed` | common push envelope, TransferTicketId/revision, target generation, state, expiry, bounded byte/chunk progress and `gap|reconcile_required`; never file bytes |
| `announcement_changed` | common push envelope, AnnouncementId/revision, audience and active/dismissed/expired/superseded state; content is fetched under normal read scope |
| `application_update_changed` | common push envelope, UpdateIntentId/revision, signed-manifest identity, state and bounded download/apply/rollback progress; never package bytes |
| `work_item_activity_changed` | common push envelope, subscription id, WorkItemId/item revision, activity event id/sequence or `gap(checkpoint,cursor)` and bounded safe delta |
| `presentation_history_changed` | common push envelope, WorkspaceId, exact PresentationHistoryOwner, history generation, undo/redo top metadata or invalidation/gap; never another owner's entries |

Every typed common-push event row above carries `StateStreamKey`, stream revision, object revision, daemon/
connection generation and monotonically increasing subscription/event sequence. A client applies it only after
the matching snapshot/watermark. Overflow, coalescing loss, compacted cursor, authorisation change or sequence
gap uses that event's closed `gap` payload, stops incremental application and requires resnapshot; it never
silently truncates to complete. Producers reserve bounded queue/byte capacity, may coalesce only intermediate
progress for the same object revision lineage, and never drop terminal state or a gap marker. The shared
LiveSubscriptionRegistry reserves its terminal gap slot before producer registration; a gap stops delivery and
releases count/queue bytes. Unsubscribed or unauthorised clients receive none of the object payload.

For vNext, every unsolicited common push, `node_view_changed` and non-gap `hierarchy_changed` payload is≤180
KiB serialized and its complete encoded frame is≤256 KiB. This bound is independent of the64-event/1-MiB
subscription queue: one event cannot consume that whole queue or escape framing. If the next typed delta/
metadata event would exceed180 KiB, the producer consumes its pre-reserved terminal gap, stops that stream and
the client protocol runtime automatically performs the exact snapshot/read request; any large response is then
chunked and applied atomically. Unsolicited pushes are never fragmented, and no user is asked to reload.

The incompatible ADR-059 protocol adds `node_view_changed`, `runtime_attempt_changed`,
`context_usage_changed`, `quota_scope_changed`, `context_link_changed` and `context_packet_changed`. Large
content pushes are sent only for an active `NodeViewSubscription` and repeat its subscription id, exact
subject and monotonic revision; a gap is explicit and cancels the stream. Context/quota pushes name their
stable scope ids rather than duplicating a sample per Agent.

M13 additionally reserves `agent_message_changed`, `dependency_edge_changed`, `team_changed` and
`runtime_continuity_changed`. Each repeats its stable id/generation and carries bounded metadata/evidence,
never a message body, runtime bearer or implicit execution instruction.

In v4, `hierarchy_changed` sends the whole projection, not a structural diff. In vNext it sends only the
bounded ordered compact delta or scoped gap defined above; count and byte boundaries are independent and the
first operation that would make the serialized delta exceed180 KiB is represented by the scoped gap instead.
No hierarchy push is fragmented and no complete encoded push exceeds256 KiB. A client applies a delta
exclusively to the exact prior revision and never patches stale ownership. Both accept only a strictly newer
revision from the same daemon; a gap, reversal or daemon identity change automatically issues `get_hierarchy`,
with vNext discarding scans and atomically rebuilding the compact index plus affected visible pages. Preview/binding pushes are bounded
replacements for one node and may be coalesced; they are not `TurnEvent`s. Selection/expansion produces no
broadcast.

### 8.1 The screen: `pane_screen`

The default terminal push. It carries **what changed**, in one of two shapes, tagged
`mode`.

```jsonc
// The rows that differ. The everyday case.
{"v":4,"type":"event","event":{"event":"pane_screen",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":312,"update":{
    "mode":"rows","size":{"rows":40,"cols":120},"cursor":[7,18],
    "rows":[{"row":6,"runs":[{"t":"$ cargo test","n":12},{"n":108}]},
            {"row":7,"runs":[{"n":120}]}]}}}

// The whole screen. Sent on resync, after a resize, and when a diff would not be
// smaller.
{"v":4,"type":"event","event":{"event":"pane_screen",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":313,"update":{"mode":"full","grid":{"rows":40,"cols":120,"…":"…"}}}}
```

**Applying a `rows` update**: replace each named row's cells outright, then take
`cursor`, `alternate_screen` and `scrollback_len` from the update. Rows are whole — there is
no partial-row addressing, so a client can never leave a row half written. A row is `runs` in
exactly the grid encoding of §2.2.

`scrollback_len` — absent when zero, which is every update for a pane that has not scrolled
yet — is how much history now sits above the screen. It is on every update rather than only
on the `full` ones because a client needs two things from it that it cannot work out for
itself: that there is history to scroll into at all, and **how many rows just left the top**.
The second is what keeps a scrolled viewport still. A client's scroll offset is measured from
the live screen, so when rows scroll off, an unchanged offset would show newer content and
the line the user was reading would slide away a row at a time. The increase in
`scrollback_len` is exactly the number of rows that left, however many screens' worth arrived
at once — which is the case a client cannot prove for itself, since a burst bigger than the
screen leaves no overlap to compare.

`size` is carried so a client can **refuse** an update meant for a geometry it is no
longer rendering, rather than writing rows into the wrong shape. That happens if it
missed a resize; the answer is `resync_pane`.

An update with an empty `rows` list is normal and cheap: it means only the cursor moved.
A screen where nothing changed at all produces no frame — a bell, or a mode change that
leaves the cells alone, is not a redraw.

**Why rows and not cell runs.** Cell-level addressing would be smaller for one character
changing inside a dense 120-column row. Measured on realistic screens it does not pay for
the addressing it needs: a keystroke echo touches a mostly-blank prompt row, which is
133 bytes as a row diff, and a scroll touches every row either way. Rows also make
application trivially idempotent, which matters more than the last few bytes.

**The cap on one update.** Past the point where more than half the rows differ, the whole
grid is the smaller message, so the daemon sends `full`. A single update therefore never
carries more than one screen, and one screen is bounded by `max_screen_cells`. A resize
is always `full`: rows do not correspond across one.

### 8.2 Sequence and resync

`seq` is per-attachment and increases by one per update — for `pane_screen` and
`pane_output` alike. A client that sees a jump has missed an update, and must not apply
what follows: rows applied to a stale screen leave the two disagreeing silently, which is
the one failure this design exists to remove.

Two repairs, and they fail differently, which is why both exist:

1. **The client asks.** `resync_pane` answers with the whole screen and the sequence to
   continue from. Available immediately, whether or not the pane produces anything else.
2. **The daemon notices.** When a push cannot be delivered — its channel is bounded on
   purpose — the attachment is marked, and its next update is `full` rather than rows.
   Costs nothing and needs no client cooperation, but only lands when the pane next
   produces output.

A cells client cannot suffer the *other* kind of loss. The screen is rebuilt from the
daemon's authoritative buffer every time, so a pty read the daemon's own pump missed is
already accounted for in the next screen it takes. That is why there is no
`pane_screen_gap`.

### 8.3 Bytes: `pane_output`

```jsonc
{"v":4,"type":"event","event":{"event":"pane_output",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":41,"data":"b2sNCg=="}}

{"v":4,"type":"event","event":{"event":"pane_output_gap",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","dropped":12,"resume_seq":53}}
```

Only for attachments that asked for `bytes`. A read larger than
`max_output_chunk_bytes` is split across several frames, in order.

The daemon's output channel is bounded — buffering an unbounded amount for a slow
client is a memory leak that looks like a feature — so a client that falls far
behind loses frames. `pane_output_gap` admits it, so the UI can re-attach and
replay rather than render a terminal that silently missed a screenful. There is no
equivalent for cells because there is nothing to admit: see §8.2.

The incompatible vNext push is one closed `terminal_output` union. Every variant repeats daemon-minted
PaneAttachmentId, AttachmentGeneration, ConnectionGeneration, SurfaceId, SessionId, PaneId, AttemptOwner,
RuntimeAttemptId, AttemptGeneration, PtyGeneration, BufferGeneration, stream=`cells|bytes` and
per-attachment sequence. Its payload is `cells(ScreenDelta,current_baseline_revision,next_baseline_revision)`,
`bytes(raw≤64 KiB)` or `gap(first_missing_seq,last_buffer_seq,resync_required=true)`. A `cells` delta is admitted
only when its complete encoded event is≤256 KiB; an update that would exceed the envelope becomes `gap` rather
than a partial/full push. No title/path/provider body enters the envelope.

Every vNext gap is terminal for that PaneAttachmentId+AttachmentGeneration: after the reserved critical gap is
queued the daemon retires its exact baseline and batch, and no subsequent event can reuse their identities.
For both cells and bytes the client protocol runtime automatically sends `resync_pane` with the retired exact
identity and stream; this is never an operator interaction. The vNext `screen` response repeats every owner
generation, mints the replacement PaneAttachmentId+AttachmentGeneration and BaselineGeneration and carries a
complete cells grid or parsed byte replay as one logical response. If it exceeds192 KiB it uses
`ChunkedResponseStream`; offsets, final digest and all generations are verified before the replacement
attachment/screen is applied atomically, and failure applies zero cells/bytes and follows bounded automatic
retry/backoff. A stale generation or noncontiguous sequence is discarded and cannot be repaired by a same-named
Pane in another Session.

TerminalBuffer is updated before queue admission. If one terminal's 512-chunk/8-MiB queue or the 4,096-chunk/
256-MiB global pool is full, only that producing terminal drops its own oldest queued chunk—or, if none can be
dropped, declines the new delivery chunk—and marks its attachments gapped. It never evicts a sibling terminal,
blocks the PTY reader or loses the already-applied authoritative screen/ring. Detach, attachment/Surface/
connection/Attempt/PTY/buffer-generation loss or process exit releases that exact attachment, baseline and
pump batch; terminal end releases its output queue after the last subscriber/gap fence, and connection loss
releases its outbox/partial response streams. Outbox admission reserves 32 frames/1 MiB per connection and
512 frames/16 MiB globally exclusively for input receipts, Attention, lifecycle/control and gaps; terminal/
content traffic can consume only the remaining224 frames/7 MiB per connection and3,584 frames/112 MiB global.
Thus a saturated output producer cannot occupy the capacity required to report or act on operator-critical work.

The v4 `pane_screen`/`pane_output` rows and §8.2 statement that cells has no `pane_screen_gap` apply only to
negotiated v4, whose≤8-MiB line permits its bounded full-screen response/update. VNext never emits those event
names: it uses `terminal_output.gap` plus the automatic chunked `resync_pane` replacement above, so even a
65,536-cell maximum grid produces no frame above256 KiB and no partially applied snapshot.

### 8.4 Coalescing, and panes nobody is watching

Both streams are batched a few milliseconds before being sent, so a program writing line
by line produces one update per screenful rather than one per line. For cells this is
worth more than it was for bytes: one batch is one diff, so two hundred lines of build
output become a handful of updates describing the rows that ended up different, instead of
two hundred describing rows that had already scrolled away.

**A pane nobody attached produces no frames at all** — no screens, no bytes. Its output
has already reached the daemon's buffer, which is what a later attach reads from, so
nothing is lost by not sending it. With thirty sessions open and one on screen, that is
the difference between a daemon that idles and one that does not.

### State changes

```jsonc
{"v":4,"type":"event","event":{"event":"node_state_changed",
  "session_id":"sess_4b71e0","node_id":"proc_7a12ff",
  "lifecycle":{"kind":"alive"},"turn":{"kind":"done"},
  "display_state":"completed_turn"}}
```

Both axes plus the derived projection travel together. `display_state` is a pure
function of the other two, and it is sent anyway: a client deriving it would be a
second implementation of the one rule this product cannot afford to get wrong. The
frame above is the headline case — the turn is over, the process is still alive,
and the two are not the same claim.

`turn` is absent for a node with no agent axis; a shell owes the user nothing.
`caused_by` is absent for a change Turn made itself (a user correction, a
supervisor sweep) rather than being filled in with a fabricated cause.

### The event stream

```jsonc
{"v":4,"type":"event","event":{"event":"turn_event_emitted","turn_event":{
  "id":"evt_c41b90","timestamp_ms":1700000000000,"workspace_id":null,
  "session_id":"sess_4b71e0","node_id":"proc_7a12ff","parent_node_id":null,
  "agent":{"provider":null,"tool":null,"model":null},
  "kind":{"event":"agent.turn_completed","last_message":"tests running",
          "background_tasks":2},
  "confidence":"explicit",
  "source":{"hook":{"tool":"claude-code","event_name":"Stop"}},
  "severity":"notice","dedup_key":"sess_4b71e0|agent.turn_completed","raw":null}}}
```

`turn_core::event::TurnEvent`, normalised, whatever produced it. `confidence` and
`source` are what let a UI render a heuristic's opinion as provisional:
`inferred_low` and `inferred_high` must be drawn as guesses, `integrated` and
`explicit` as facts. `background_tasks` is why the notification for this event says
*"Turn complete · 2 still running"* rather than "done".

### The unified hierarchy

```jsonc
{"v":4,"type":"event","event":{"event":"hierarchy_changed","snapshot":{
  "revision":18,"tree_state":{"surface_id":"main-window","selected":null,"expanded":[]},
  "workspaces":[{"workspace":{"id":"ws_9f2a1c","name":"turn",
      "lease_reconciliation_required":false},"sessions":[{"session":{
      "id":"sess_4b71e0","name":"Fix restore","mode":"main_checkout"},"nodes":[
  {"node_id":"proc_7a12ff","parent":null,"depth":0,"child_count":1,
   "kind":"agent","is_agentic":true,"title":"claude","command":"claude",
   "lifecycle":{"kind":"alive"},"turn":{"kind":"active"},
   "display_state":"running","state_label":"running","severity":20,
   "needs_user":false,"runtime_ms":3000, "…":"…"},
  {"node_id":"proc_e5c308","parent":"proc_7a12ff",
   "relationship":{"kind":"spawned_by","confidence":"explicit"},
   "depth":1,"child_count":0,
   "kind":"subagent","is_agentic":true,
   "agent":{"name":{"declared_name":"Reviewer","display_name":"Reviewer",
                      "source":"explicit_parent_event","confidence":"explicit"}},
   "activity_preview":{"normalized_text":"Reviewing restore path",
                       "source":"semantic_event","confidence":"explicit",
                       "stable":true,"redacted":true},
   "pane_bindings":[],"pane_capability":"preview_details"}]}]}]}}}
```

Elided fields are listed in §9. This is what a reported background subagent changes: one revisioned
projection, a declared name only when the integration supplied it, an explicit `spawned_by` edge, safe
preview and no Pane binding. It does not mutate Layout, selection, focus or Attention.

An OS parent observation could carry the same relationship kind with `inferred_high`; event confidence that
the scan occurred remains a separate field. An unknown runtime parent leaves the node directly contained by
its Session rather than inventing a process edge. A client renders uncertainty supplied by the daemon and
never recomputes it.

### Restore

```jsonc
{"v":4,"type":"event","event":{"event":"restore_result",
  "session_id":"sess_4b71e0","state":"partially_restored","needs_explanation":true,
  "panes":[
    {"pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
     "lifecycle":{"kind":"orphaned"},"can_relaunch":false},
    {"pane_id":"pane_66ba04","node_id":"proc_910bc2","lifecycle":{"kind":"lost"},
     "can_relaunch":true,"command":"cargo watch -x test"}]}}
```

Pushed rather than answered, because a restore happens when the daemon decides — on
its own start, or when it re-adopts processes — and the UI may not have asked
anything yet.

- `state`: `live` \| `reattached` \| `partially_restored` \| `layout_only`
- `needs_explanation` is true when the user must be told, rather than left to
  notice a dead pane.
- `lifecycle`: current daemon restart reports `orphaned` (the stored PID may still be alive but the PTY is
  out of reach) or `lost` (the former runtime cannot be found). `reconnected` is reserved for a future
  backend that can prove it reattached the original PTY; this build does not emit that claim on restore.
- **Nothing in this v4 restore event has been relaunched.** Every outcome retains its durable `node_id`; `can_relaunch: true` is
  an offer to relaunch that exact node, not a newly invented process. `command` is descriptive so accepting
  is informed. The user answers with `relaunch_node` or does not, and nothing happens until they do.
- A UI reconnect to the same still-running daemon is a different path: it reattaches to the daemon-owned
  live screen and bindings. It must not be described as PTY survival across a daemon restart.

---

## 9. View models

The daemon owns every product rule. If the client derived `display_state` itself,
or decided whether a parent link is a guess, or worked out which of thirty sessions
is shouting loudest, those rules would exist twice. Sharing Rust types does not make two derivations agree;
the GUI renders daemon projections.

Two rules the projections keep: turn-core types are **embedded** rather than
re-described, and extra fields are strictly *derived* values a client would
otherwise need a copy of the rules to compute.

### `HierarchySnapshot` / `HierarchyIndexSnapshot` — the one navigation projection

Implemented v4 `HierarchySnapshot` is `revision`, `tree_state`, `workspaces` and remains a full replacement.
There is no duplicate top-level `surface_id`. The vNext replacement is `HierarchyIndexSnapshot { revision,
coverage, tree_state, coordinates, filter_bitmap }`:≤111,024 compact coordinates/6 MiB, no detailed rows or
bodies, and daemon-minted row scans/reveal as specified above.

`tree_state` is v4 `TreeSurfaceState { surface_id,...,expanded,... }`; vNext migrates that field losslessly to
`TreeSurfaceState { surface_id, owner, state_revision, selected?, expansion_default, expansion_exceptions,
manual_order, filters, visibility_mode, scroll_anchor?, dormant_deadline? }`. The daemon-minted id is non-reused; one record
is≤256 KiB and includes≤2,000 unique expansion-exception keys,≤2,000 unique manual-order keys and≤32 closed filters of
≤256 encoded bytes each. Keys use the sole vNext `HierarchyKey=workspace|session|node` union; `node` carries the
closed NodeKind and therefore covers Agent, Group and every Resource/Browser/Job kind without a process alias. Interaction state from
another surface is never merged into it. The search query is intentionally transient; the durable fields
restore the navigational context without persisting arbitrary repository or task text.

In v4, each `WorkspaceTreeView` contains `workspace`, `checkouts`, `write_lease?` and ordered `sessions`. Each
`SessionTreeView` contains `session` and ordered node rows. The daemon supplies parent/depth/order, derived
state and badges, relationship confidence, preview, bindings and capability; the GUI does not join separate
lists or infer missing values. In vNext those detailed row summaries are≤2 KiB and arrive only through the
pinned≤500-row/1-MiB page/reveal path; the compact coordinate supplies parent/order/kind/flags/RowMetricClass
needed for total projection and exact spacers. `revision` rejects stale/out-of-order delivery and tells a
client when to resynchronise; a vNext delta applies only to its named predecessor or becomes a gap.

### `InspectorDetails` — optional contextual detail

Tagged `workspace`, `session`, `agent` or `process`, and always returned only for one typed
`HierarchyKey`:

- Workspace detail carries the summary, checkout paths/branches/shared resources, write lease,
  configuration, environment **names** and Attention policy.
- Session detail carries mode, checkout, branch, Template, process/Attention counts, environment names and
  bounded recent event history.
- Agent detail carries the exact `TreeNodeView`, provider/tool/model/name, work and permission context,
  metrics, a readable parent link, relationship confidence, process facts, origin, handoff metadata and
  bounded recent event history.
- Process detail carries the exact `TreeNodeView`, readable parent and confidence, PID/PPID/process group,
  argv, cwd, lifecycle/exit, origin and bounded recent event history.

Raw hook bodies, raw PTY output and environment values are not representable here. Parent links and origins
carry `Confidence`; clients must label provisional values as inferred and navigate through the accompanying
stable key rather than guessing from a display name.

### Planned `NodeView` — one visible semantic subject

Not present in v4. The ADR-059 projection is tagged by node/content kind and carries `surface_id`, the exact
`HierarchyKey`, node-view revision, semantic `AgentInstanceId?`, tagged `AttemptOwner?`,
`active_runtime_attempt_id?`, `latest_runtime_attempt_id?`, safe content capability and bounded content/
subscription descriptor. It composes rather than duplicates:

- the exact subject's Attention entries and verified interaction owner;
- stable identity, provider conversation capability and attempt history;
- attempt start cause plus immutable launch/resume receipt or verified in-place configuration-transition
  receipt, requested/effective/current runtime facts, provenance and fallback reasons;
- conversation-scoped context usage and separately scoped provider/account quota samples;
- hierarchy, context-link and lineage references;
- the active terminal attachment, structured activity/transcript or truthful unavailable state.

Every dynamic observation carries source, observed time and freshness. Shared quota names its account/host
scope and is never projected as node-attributed consumption. Missing facts are absent with a typed
unavailable reason, not zero/default values. Selecting the node does not acknowledge Attention; the daemon
marks a result read only after `mark_node_result_read` names the rendered result revision.

### Planned `AttentionRoute` and Node View subscription

Not present in v4. `AttentionRoute` contains the requesting `surface_id`, its
`surface_connection_generation`, daemon generation, exact `attention_id`, Workspace/Session and tagged
`subject`. An exact subject contains `NodeId`, agentic
`AgentInstanceId?`, active/latest `RuntimeAttemptId?`/generation, tagged
`demand_ref: pending_interaction|result|condition`, optional verified input-owner node and a bounded `NodeView`
bootstrap with revision. An exact subject without an action owner still opens its Node View with the action
disabled; it is not provisional. A provisional subject contains only the authenticated
parent/external-worker correlation scope; an unassigned subject contains only the owning Session. Both open
a demand view and expose no borrowed input. If later evidence binds a node, the daemon preserves
`AttentionId`, atomically replaces subject/dedup key, increments route revision and retires the prior route.
The UI applies the route as one visual transaction and revalidates it after any generation change. An
aggregate Workspace/Session badge never chooses locally; it asks the daemon for the first queue entry in
that scope.

`NodeViewSubscription` contains `subscription_id`, surface, exact key/subject, content kind, current revision
and byte/item bounds. Its pushes carry the same identity and revision. Backpressure is bounded and explicit;
a gap, reselection, disconnect or replacement connection retires the stream and cannot leak content from the
previously selected node into the current view.

### `SessionSummary` — one Session projection

Identity: `id`, `workspace_id`, `name`, `note`, `cwd`, `status`
(`active`\|`paused`\|`archived`).

Checkout safety: `mode` (`main_checkout`\|`read_only`\|`isolated_worktree`), `checkout_id?`,
`worktree_path?`, `read_only_enforced`. A read-only badge must say whether the guard is enforced; when the
last field is false it must also explain that process launch is disabled.

Derived state — **the client renders these, it never computes them**:

| Field | From |
| --- | --- |
| `display_state` | `DisplayState::derive` over the session's process tree |
| `state_label` | the display-state label, except outstanding Attention promotes the Session row to `"YOUR TURN"` |
| `severity` | Ranking weight, so a client sorting locally sorts as the daemon would |
| `needs_user` | Whether the runtime tree itself is blocked on the human; `badge_count` independently exposes exact or scoped Attention |

Counts: `subagent_count`, `running_count`, `orphaned_count`, `node_count`, `pane_count`. Subagents
and running processes are counted separately because "the agent finished its turn"
and "nothing is running any more" are different claims. `orphaned_count` is the subset of
`running_count` that survived a previous daemon and that Turn cannot stop; it travels with the summary
so a confirmation dialog for any row can say what ending it will not achieve without holding that
Session's whole tree.

**Known v4 defect:** these bare integers cannot distinguish confirmed zero from no/partial/stale topology
observation, and the current `subagent_count` counts `NodeKind::Subagent` rows rather than a covered semantic
graph. The post-v0.1 protocol replaces agent-child aggregates with `ObservedCountView { metric:
semantic_children|live_children|completed_children, scope: direct|descendants, parent_scope:
current_attempt|instance_lifetime, value:
exact(n)|lower_bound(n)|unknown|unsupported, coverage:
complete_snapshot|sequenced_complete|best_effort|gap_detected|unavailable, source_epochs,
snapshot_watermark?, graph_revision, observed_ms, freshness, reason, remediation? }`.

Topology pushes use `snapshot_begin|snapshot_item|snapshot_end|delta|heartbeat|gap` and carry source id,
observation epoch, parent instance/attempt/generation, source sequence and covered domain/metrics. A matching
`snapshot_end` closes coverage only when item count/watermark match and no gap occurred. `HookStats.dropped`,
sequence gaps, receiver/adapter restart, stale heartbeat and generation change emit a scoped gap, immediately
invalidate exactness and schedule bounded asynchronous resync. Exact zero additionally requires a closed
authoritative coverage set and no matching graph node. An orphaned/conflicted child prevents exact live
coverage. Best-effort can add positive nodes but yields only a lower bound or unknown. `current_attempt`
filters verified SpawnEdges by each traversed parent's active attempt/generation;
`instance_lifetime` includes all retained attempts. Every Workspace/Session/Agent aggregate comes from the same graph revision, and
acceptance compares it with an independent expected-event manifest rather than calling the same graph query.
A vNext client must never convert an absent legacy field or unavailable adapter into exact zero; during
mixed-version negotiation it labels the legacy value `coverage_unknown`.

Every TurnState fact additionally carries daemon-canonical `turn_id`, monotonic `turn_revision` and an exact
pending-interaction/result correlation when applicable. Native revisions are compared only within their
source; adapters map them into the canonical turn epoch. A new non-terminal state after
`Done|TaskDone|Failed` requires a new TurnId, while `Done → TaskDone` may refine the same turn.

Timing: `idle_ms` (never negative, even under clock skew), `last_activity_ms`,
`created_ms`.

Restore: `restore_state`, `restore_needs_explanation`.

Attention: `badge_count`, `muted`. A muted session still reports its badge count.

Organisation: `pinned`, `favourite`, `tags`, `git_branch`, `linked_ref`,
`template_id`, `parent_session`, `tmux`.

`primary_agent`: an `AgentSummary` when the session has one — `node_id`, `agent`
(`provider`/`tool`/`model`), `external_id`, `agent_type`, `turn`, `current_task`,
`last_message`, `pending_permission`, `pending_question`, `tokens_used`,
`cost_usd`, `permission_mode`, `git_branch`, `resumable`.

`pending_permission` carries `summary`, `command`, `tool_name`, `risk`
(`low`\|`medium`\|`high`), `requested_ms` and **`cwd`** — shown verbatim, because
approving something in the wrong repository is the mistake that field exists to
prevent.

`SessionSummary::sidebar_rank()` mirrors `Session::sidebar_rank()`
(`pinned`, `needs_user`, `severity`, `last_activity_ms`; higher first) so a client
can re-sort a list it already holds after a push, without a round trip and without
inventing its own ordering.

### `TreeNodeView` — one runtime node projection

Flat with a `depth` rather than nested, for the same reason `SessionTree` stores
parent pointers: the shape changes as subagents come and go, and re-rendering a
list is cheaper and less error-prone than diffing a recursive structure. Rows
arrive in draw order — each root followed by its subtree, depth-first, siblings in
insertion order.

Placement: `node_id`, `session_id`, `parent`, `relationship { kind, confidence }`,
`relationship_is_provisional`, `depth`, `child_count`.
Event confidence does not substitute for `relationship.confidence`.

Identity: `kind`, `is_agentic`, `title`, `command`, `args`, `cwd`, `pid`, `ppid`, `ephemeral`,
`terminal_runtime_host`. Ephemeral process-table plumbing remains searchable but is hidden outside Technical
mode unless a search reveals it. `terminal_runtime_host: true` marks the Shell that owns the PTY used by a
semantic Agent child; it is routing metadata, not a second Agent or an instruction to display the Shell in
Normal navigation.

State: `lifecycle`, `turn` (absent for a non-agent), `display_state`,
`state_label`, `severity`, `needs_user`, `interaction_pending`.

Views and timing: `pane_bindings: [PaneNodeBinding]`, `pane_capability`, `started_ms`, `ended_ms`,
`runtime_ms` (freezes at exit), `exit_code`. There is no privileged single `pane_id`.

`agent`: an `AgentSummary`, for agentic nodes, including lossless `name` (`declared_name?`, `display_name`,
source/confidence, `user_renamed`). `activity_preview?` and `preview_visibility` are bounded/redacted current
state, never raw output.

A node whose parent is not in the tree is reported at `depth: 0` with its `parent`
still set — orphaned, not hidden, and not silently re-attached elsewhere.

### `Grid` — one pane's screen

Not a projection but a *reading*: the daemon's `vt100` screen, cell by cell, with palette
indices already resolved (§2.2). `rows`, `cols`, `cursor` (absent when the program hid
it), `alternate_screen`, `modes`, `scrollback_offset`, `scrollback_len`, and the `runs`
themselves.

`modes` is what the program set with its own escape sequences, and a client **must not**
guess at it: `application_cursor` decides whether an arrow key sends `ESC O A` or
`ESC [ A`, `bracketed_paste` decides whether a paste is wrapped so an editor can tell it
from typing, and `mouse` (`none` \| `press` \| `button_motion` \| `any_motion`) decides
whether a wheel notch is a mouse report or Turn's own scrollback. Getting any of them
wrong breaks arrow keys inside `vim`.

The attached live `Grid` starts with `scrollback_offset = 0`; `PaneAttachment.scrollback`
seeds the client's transcript. The client then reports that transcript's length in
`scrollback_len` and changes `scrollback_offset` as the user scrolls. A resync replaces
the live grid, while a fresh attach is the operation that re-seeds durable history.

`notices` is what the pane **refused to draw**, and it is the one field that is not the
program's: a list of `{text, count}` where each `text` is a complete sentence Turn
generated, already bracketed and containing nothing the process supplied. It travels here,
beside the cells, rather than as cells, because a sentence written into the grid lands at
the program's cursor and corrupts a layout the program repaints without ever overwriting it
(ADR-045). A client shows it in its own furniture — never in the screen — and may render
the text as-is. At most eight entries, each at most 160 characters, each
with a `count` of at least 1; a peer sending otherwise is refused. Absent on the wire for
every pane that refused nothing, which is nearly all of them.

### Archiving, closing and deleting

Three verbs, and the difference between them is what the client must put in front of the user:

| Operation/disposition | Stops processes | Active navigation result | Record kept | Reversible |
| --- | --- | --- | --- | --- |
| `archive_*` | no | named row leaves active tree | yes | yes |
| `close_session(keep_processes)` / End Session | no; survivors are rehomed/recovery-inventoried | Session row leaves atomically | yes, minimal tombstone plus exact survivor identity | original Session is not restored |
| `close_session(terminate|kill)` / End Session | yes after daemon-derived total survivor reduction | Session row leaves atomically | yes, minimal tombstone plus rehomed/recovery survivors | work is not |
| `close_workspace(keep_processes)` | no; survivors are rehomed/recovery-inventoried | Workspace row stays; ended Session rows leave | yes, Workspace plus minimal Session tombstones/survivors | Sessions are not restored |
| `close_workspace(terminate|kill)` / Stop all Sessions | yes after daemon-derived total per-Session reductions | Workspace row stays; ended Session rows leave | yes | work is not |
| `delete_session|delete_workspace` | yes after daemon-derived total survivor reduction | deleted container row leaves | no active/rich container domain row; the reference-only ContainerCloseReceipt plus mandatory tombstones/rehome/Workspace-or-Installation SemanticRecoveryInventory survive | no |

End/Stop-all/delete serially derives and commits every survivor disposition without a second user action;
there is no semantic precondition that can leave navigation stuck. `delete_session` and `delete_workspace`
then remove Turn-owned container records.
Ordinary terminal process rows, layout, compactable event log, scratch directory and per-window tree
state may be removed. Provider-native Jobs, nonterminal creation/mutation intents and current Attention never
disappear with the container: valid predeclared `NativeJobContainerDisposition` may rehome them atomically to
an exact destination Session/optional Group, otherwise the total fallback retains them in the Workspace
SemanticRecoveryInventory (or InstallationSemanticRecoveryInventory when deleting that Workspace);
only terminal local-data disposition may erase rich content while preserving replay/visibility fences. A Workspace applies this rule to every Session before
its own row can disappear.

They do **not** touch the user's disk. The checkout is a directory the user chose and Turn does
not own it: no file is removed, no branch and no worktree is deleted. Every surface that offers
a delete has to say so, and naming the exact path is better than promising in the abstract.

`keep_processes` is not a `delete_session|delete_workspace` request variant; the operator uses
`close_session|close_workspace(keep_processes)` when preservation is intended. A delete still cannot strand an
unkillable, offline or uncertain survivor: the total reducer names it in the applicable recovery inventory
before the container row leaves. Replay after a lost reply returns the identical durable `closed`; a new
operation against the exact tombstone first reserves its bounded redundant-operation fence and then returns
`closed_already` with the same disposition reference, or at that independent pool's N+1 returns
`replay_capacity_refused_already_closed` with zero effect and no required operator follow-up.

### `SessionDetails`

`summary`, `layout` (the domain `Layout`), `tree` (`[TreeNodeView]`), `attention`
(the `AttentionPolicy` in force), `env`.

### `AttentionView`

`entry` is the whole `turn_core::attention::AttentionEntry` — id, session, node,
optional `parent_node_id` and `subject_external_id` correlation scope,
`reason` (`permission`\|`credentials`\|`question`\|`input`), `summary`,
`confidence`, timestamps, `state` (`pending`\|`snoozed`\|`acknowledged`),
`priority_boost`. Plus:

- `session_name` — the queue reads as a task, not an id. Falls back to the id.
- `provisional` — derived from `entry.confidence`. A heuristic's demand must be
  visibly a guess.
- `score` — the queue score at projection time, so a client can keep a list ordered
  locally. Not stable across time: the age bonus grows.
- `actionable` — a snoozed entry is still **listed**, so the snooze does not feel
  like a deletion, and marked unactionable.

`node_id` is the exact subject when known. The parent/external fields are not a substitute node to focus:
they preserve an unresolved callback's authenticated boundary so deduplication, restart and resolution do
not broaden it to every worker in the Session. They are additive optional fields; older persisted/protocol
entries decode with no provisional scope.

A client applies the same rule to permission detail. It may join `pending_permission` only to the exact
`node_id`; a matching `SessionSummary.primary_agent.node_id` is an exact join when the Session projection
arrives before the tree. A scoped node-less entry or a stale exact id must never borrow the primary Agent's
command, cwd or risk. Only a legacy entry with all three subject fields absent may use that compatibility
fallback.

### `WorkspaceSummary`

`id`, `name`, `root`, `git_remote`, `colour`, `icon`, `archived`,
`session_count`, `sessions_needing_user`, `badge_count`, `default_agent`,
`default_shell`, `default_template`, `tmux_enabled`, `lease_reconciliation_required`, `created_ms`,
`last_used_ms`.

The counts are the sum of the workspace's `SessionSummary` values, so a workspace
badge and its session badges can never disagree.

### Lease, binding and preview views

`WorkspaceWriteLease` carries Workspace/checkout identity, tagged
`owner=current_session(SessionId,SessionGeneration)|ended_session(SessionId,SessionGeneration,CheckoutId,
ending_generation)`, `mode: exclusive_write`, state,
acquisition/heartbeat/release timestamps and fencing generation. `recovery_required` and `stale` still block
a new owner; only fenced release/reconciliation makes the claim non-blocking. Canonical checkout paths live
on `WorkspaceCheckout`, not on the lease.

`PaneNodeBinding` carries Pane, Session and node identity, temporary/durable ownership, optional
`surface_id` and open time. `pane_capability` is the closed
`NodePaneCapability::{Terminal, PreviewDetails}`, so a semantic-only subagent cannot be opened as a fake
terminal. `PaneFocusView.node_id` always names the Pane's real node;
`attention_subject_node_id?` preserves the distinct AgentNode whose Attention caused a runtime-owner route.

`ActivityPreview` carries normalised text, source, confidence, stability/redaction flags, update time and
optional source sequence for replacement ordering. It never carries raw bytes or an unredacted source.

### `TemplateSummary`

`id`, `name`, `description`, `icon`, `hotkey`, `built_in`, `pane_count`,
`commands`, `name_pattern`, `tmux`, `created_ms`. `commands` lists what the
template would start, in pane order — materialising a template launches processes,
and choosing one should be an informed decision.

---

## 10. What the protocol refuses to express

Product guarantees appear here as **absences**, which is the strongest
enforcement a type definition can offer. A future request matching these
descriptions has to argue with a test first.

1. **A heuristic can never move the user.** Focus is not something a client is told
   to do directly; it arrives as an `Effect` the attention manager already cleared
   through the focus governor. `EventSource::PtyHeuristic` caps `Confidence` at
   `inferred_high`, and `AttentionPolicy::resolve` degrades any focus action from a
   provisional event to a badge.
2. **Turn never chooses an operator response.** Legacy local v4 has no typed semantic-response operation;
   ordinary PTY input carries no permission guarantee. vNext may transport
   an exact operator-selected non-authorising response through `respond_to_agent_interaction`, or an exact
   provider-offered permission option through distinct `submit_local_permission_response` or
   grant-bound `submit_remote_permission_response`. Neither path infers, widens or
   automatically selects an answer, and a context
   handoff is refused while the destination has a pending interaction.
3. **Turn never runs a command it inferred from agent output.** There is no "run this" verb. A process can
   start only from an explicit lifecycle intent, an immutable reviewed Flow/Template, or the exact bounded
   eligible descriptor set (or one configured default Shell) accepted by `activate_session`; agent output
   never becomes any of those authorities.
4. **Restore does not require pane ceremony and does not invent work.** Boot restore only reconstructs state
   and attaches proved-live attempts. A foreground `activate_session` may materialise its exact bounded
   eligible descriptor set—or exactly one configured default Shell when that set is empty—after atomic
   preflight. A stopped individual runtime outside that operation still requires its specific explicit
   recovery intent. No generic “Start pane” gate exists, and consequential or ambiguous descriptors remain
   stopped with one consolidated recovery result.
5. **No client can request a primary-checkout writer.** Writable primary-checkout mode is decode-only legacy
   state and unreachable in vNext; every create/activate/restore/resume/restart/Flow/Template path proves an
   isolated worktree before spawn. There is no arbitration, force/steal or fallback that can make primary
   `main` writable, and migration cannot pass until its Turn-owned writer/process inventory is zero.
6. **Navigation cannot fabricate ownership.** There is no unconstrained `move_node`, no client-supplied
   confidence promotion and no `tree.node_selected` domain event. Protocol v4 refuses to approximate
   relationship correction with a local-only mutation; vNext accepts only its audited, cycle-checked exact
   operation.
7. **The daemon protocol cannot listen.** M15 adds no start-microphone, PCM, audio-file, transcription or
   background-listening request. An authenticated client can manage a closed local model id and commit text
   the foreground operator already reviewed; it cannot make another client capture audio.
8. **Dictation cannot become a permission or Attention shortcut.** `commit_operator_text` accepts only an
   exact `FreeText` input target. It cannot approve/deny, route/acknowledge/dismiss Attention, activate a
   Session, launch work or retarget to the current selection.
9. **Resource pressure cannot reap detached work.** No request, setting, timer, state family or background
   reducer authorises termination from age, memory, count, invisibility or missing attachment. View parking
   may retire only proved-reconstructible client presentation; Eco is separately opt-in and exact-eligibility
   fenced; every other End/terminate/kill/delete remains an explicit typed lifecycle operation.
10. **Transcript search cannot become conversation authority.** The private local index is separately enabled,
    encrypted and profile/target scoped. A query or selected hit cannot adopt, bind, resume, launch, send,
    transfer context, create/resolve Attention or infer ownership/resumability from text.

One more, on the transport, which this crate does not implement but assumes:
`$SOCKET` is owner-only, the kernel peer UID is checked, at most 32 connections are admitted and the
per-generation token file is owner-only. The hook server is a separate listener that binds `127.0.0.1`
with independent per-node tokens. Never `0.0.0.0` and never reuse an IPC token as hook authority.

---

## 11. Implementing a client

1. Read the owner-only `$SOCKET.token`, connect to `$SOCKET`, and send `hello` with that capability as the
   first frame. Re-read it for every reconnect because daemon restart revokes the old generation.
2. Read frames with a decoder matching §1. On an `unauthorized` rejection, discard the presented token,
   re-read `$SOCKET.token` and reconnect with normal backoff; never resend the same credential on the same
   connection. On every other `rejected`, show `error.message` and stop — do not retry.
3. Store `agreed_version`, `daemon_pid` and `limits` from `welcome`. Stamp `v` on
   every frame you send.
4. Call `open_surface` to ask the daemon to mint a new Surface or resume the exact daemon-minted id/revision
   previously returned for this authenticated owner; never invent an id. Then call `get_hierarchy`. Render the
   v4 full snapshot or vNext compact index+automatic viewport pages as the one navigation projection; vNext
   selection/restore/Attention uses exact reveal and never exposes Load more. Use list/detail operations only for their named administrative
   purpose. Send `retire_surface` when the window is permanently discarded; disconnect alone leaves only its
   bounded 30-day dormant navigation state.
5. `attach_pane` for each visible pane. Draw `screen`, then apply each `pane_screen`
   in `seq` order (§8.1). On a `seq` jump, `resync_pane`. A `rows` update whose `size`
   is not what you are rendering means you missed a resize: resync.
   Ask for `stream: "bytes"` only if you need the escape stream itself, and then feed
   `replay` into your emulator and apply `pane_output`, re-attaching on
   `pane_output_gap`.
6. On v4 local single-client connections, forward keystrokes as `write_pty` and window resizes as
   `resize_pty`. On vNext use only the current input-lease-fenced `write_runtime_input`/
   `resize_runtime_input` operations and preserve their per-lease sequence; never fall back across versions.
7. Send `update_user_activity` immediately on activity transitions. During continuous typing, send only a
   coalesced bounded heartbeat before the typing grace can expire; never one request per character or idle
   periodic traffic.
8. Handle every push in §8. Apply terminal sequence rules independently from hierarchy revision rules.
   On an invalid hierarchy revision, request `get_hierarchy`; do not patch stale ownership.
9. On a decode error, reply with an `error` frame built from
   `FrameError::to_proto_error()` and **keep the connection**.
10. On reconnect, re-handshake and compare `daemon_pid`: unchanged means the runtime may still be there,
    but hierarchy/lease state is still resynchronised before enabling write actions. A different daemon
    identity triggers restore/reconciliation and never automatic lease takeover.

---

*Protocol version 4. The Rust request/response catalogue is authoritative for the exact variant count;
this document is authoritative for hierarchy, checkout-safety, revision and recovery semantics.*
