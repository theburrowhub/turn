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
| `max_output_chunk_bytes` | 256 KiB | Raw bytes per `pane_output` frame before the daemon splits it. |
| `max_screen_cells` | 65,536 | Largest `rows * cols` `attach_pane` accepts, and the most cells any grid may describe. |
| `max_image_pixels` | 1,048,576 | Most pixels one inline image may carry — 4 MiB of RGBA, so one image is always one frame. |
| `max_placed_images` | 8 | Most inline images one screen may place at a time. |

Nothing legitimate approaches 8 MiB — the largest message is a pane's screen, a few
kilobytes (§2.2). The limit exists so a peer cannot exhaust memory by opening a socket
and writing bytes without a newline.

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
- revisioned `hierarchy_changed` full replacements and bounded preview/binding/lease pushes.

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

### Unified hierarchy

| `op` | Fields | Answers with |
| --- | --- | --- |
| `get_hierarchy` | `surface_id`, `include_archived?` | `hierarchy` |
| `get_inspector` | `key: HierarchyKey` | `inspector` |
| `set_tree_expanded` | `surface_id`, `key: HierarchyKey`, `expanded` | `tree_state` |
| `set_tree_expanded_all` | `surface_id`, `expanded` | `tree_state` |
| `select_tree_node` | `surface_id`, `selected: HierarchyKey?` | `tree_state` |
| `set_tree_presentation` | `surface_id`, `filters: [TreeFilter]`, `visibility_mode`, `scroll_anchor?` | `tree_state` |
| `move_tree_node` | `surface_id`, `key`, `before?` | `tree_state` |
| `rename_node` | `session_id`, `node_id`, `name` | `node` |
| `correct_relationship` | `session_id`, `node_id`, `parent_node_id?`, `relationship_kind` | `node` |
| `get_preview_history` | `session_id`, `node_id`, `limit?` (clamped to 20) | `preview_history` |
| `set_preview_visibility` | `session_id`, `node_id`, `visibility` | `ack` |
| `open_node_as_temporary_pane` | `surface_id`, `session_id`, `node_id` | `node_pane` |
| `open_node_as_pane` | `surface_id`, `session_id`, `node_id`, `target_pane_id`, `placement` | `layout` |
| `promote_temporary_pane` | `surface_id`, `session_id`, `pane_id`, `target_pane_id`, `placement` | `layout` |
| `focus_pane_for_node` | `surface_id`, `session_id`, `node_id` | `pane_focus` |
| `focus_pane_for_attention` | `surface_id`, `session_id`, `subject_node_id` | `pane_focus` |

`get_hierarchy` is navigation bootstrap. `list_workspaces`, `list_sessions`, `get_session` and
`get_process_tree` remain useful to administration, search and details, but composing them into a second
navigation tree is a client bug. `HierarchySnapshot.revision` is monotonic for the daemon lifetime; after a
revision gap or daemon identity change, request a full snapshot.

`get_inspector` is an on-demand read for the selected hierarchy row. Its response identity must equal the
requested `HierarchyKey`; a client discards a late answer after selection changes. Inspector history is
bounded and redacted, environment values are never projected, and an inferred parent or origin retains its
confidence instead of becoming a fact merely because it appears in a detail panel. Inspector data is not
part of `HierarchySnapshot`, so opening one row does not make every hierarchy refresh carry logs and
configuration.

Presentation writes are per stable `surface_id`. They are not `TurnEvent`s, do not change active Session or
Pane focus, and do not produce a broadcast. `move_tree_node` changes only the stable order of siblings; it
cannot reparent a node or move selection.

`surface_id` is immutable for one connected client. The first `get_hierarchy` on a replacement connection
claims that surface, retires any older connection's surface ownership and removes its temporary Pane before
the snapshot is built. Permanent Layout bindings and tree expansion/selection remain. Temporary bindings
are also removed when their last client disconnects and when the daemon restarts; they are ephemeral view
state, not restorable process state.

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
not an input channel; a client may close that ephemeral view with `keep_processes` before focusing the
resolved runtime Pane.

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
advance a cursor; such an acknowledgement is a mutation. No operation-specific row may weaken these rules,
and a newly registered mutation is unreachable until the protocol registry declares its streams, revisions,
authority class, idempotency fingerprint and remote policy.

| Planned `op` | Principal fields | Planned answer |
| --- | --- | --- |
| `get_node_view` | `surface_id`, `key: HierarchyKey`, `known_revision?` | `node_view` |
| `subscribe_node_view` | `surface_id`, exact key/revision, content kind, byte/item bounds | `node_view_subscription` |
| `unsubscribe_node_view` | `surface_id`, subscription id | `ack` |
| `route_attention` | `surface_id`, `surface_connection_generation`, `attention_id?`, `scope?`, daemon generation | `attention_route` |
| `activate_session` | foreground surface/connection, Session id/revision, exact preflighted activation-policy revision and exact bounded eligible descriptor set, or exactly one configured default Shell when that set is empty | `session_activation` |
| `update_surface_activity` | `surface_id`, connection generation, focused/foreground/typing/session/sensitive state | `effects` |
| `create_agent_instance` | foreground surface, `operation_id`, `session_id`, launch spec, `parent?` | `agent_instance` |
| `restart_agent_instance` | foreground surface, `operation_id`, node/instance, expected generation, resume, launch overrides | `agent_instance` |
| `branch_agent_instance` | foreground surface, `operation_id`, source instance/attempt, target launch spec | `agent_instance` |
| `switch_agent_model` | foreground surface, exact node/instance/current attempt+generation, requested model, optional ModelEndpointProfile+revision and accepted preflight | `agent_instance` |
| `delete_agent_instance` | foreground surface, exact node/instance, expected generation, process disposition | `deletion_result` |
| `list_execution_targets` / `get_execution_target` | visible owner scope / exact target id | `execution_target_list` / `execution_target` |
| `create_execution_target` / `adopt_execution_target` | foreground surface, closed local/ssh/custom inert descriptor / exact discovered descriptor and provenance | `execution_target` |
| `probe_execution_target` | foreground surface, target id/revision, bounded non-mutating probe policy | `execution_target` |
| `trust_execution_target` / `rotate_execution_target_trust` | local foreground surface, target id, expected fingerprint/trust generation, independently observed accepted fingerprint | `execution_target` |
| `bind_execution_target_workspace` / `unbind_execution_target_workspace` | foreground surface, target id, Workspace id, closed read/runtime/file/repository scopes | `execution_target_binding` |
| `retire_execution_target` / `delete_execution_target` | foreground surface, target id and explicit survivor/profile/backend disposition | `execution_target_result` |
| `begin_workspace_onboarding` / `resume_workspace_onboarding` / `get_workspace_onboarding` | foreground surface, operation id, target generation and exact `create_directory`, `open_directory`, `clone_repository` or `adopt_ssh_target` intent / WorkspaceOnboardingId+revision / id | `workspace_onboarding` |
| `cancel_workspace_onboarding` / `reconcile_workspace_onboarding` | foreground surface, onboarding id/revision and expected phase / same plus exact phase receipt and observed external identity | `workspace_onboarding` |
| `publish_repository` | local foreground surface, exact target/repository/destination/visibility/branch/upstream/credential-reference revisions and consequence review | `repository_publish_receipt` |
| `create_runtime_node` | foreground surface, `operation_id`, Session, closed non-agent NodeKind and launch spec | `node_view` |
| `restart_runtime_owner` | foreground surface, `operation_id`, tagged AttemptOwner, expected active/latest attempt and generation | `node_view` |
| `create_resource_node` | foreground surface, `session_id`, `group_id?`, closed kind, typed payload, `operation_id` | `node_view` |
| `create_browser_node` | foreground surface, Session/Group, isolated partition policy, exact initial inert/no-load address spec | `node_view` |
| `navigate_browser` / `browser_back` / `browser_forward` / `reload_browser` / `stop_browser` | foreground surface, exact Browser Node/partition/navigation revision and URL/history entry where applicable | `browser_navigation_receipt` |
| `open_reviewed_browser_popup` | foreground surface, source Browser/navigation revision, exact popup origin/target/consequence review | `node_view` |
| `accept_reviewed_browser_download` | foreground surface, Browser/navigation/download id, expected size/type/hash and confined destination policy | `node_view` |
| `clear_browser_storage` | foreground surface, exact Browser Node/partition generation and consequence review | `browser_storage_receipt` |
| `update_resource_node` | foreground surface, exact node, expected revision, typed patch | `node_view` |
| `delete_resource_node` | foreground surface, exact node/revision; Group requires `refuse`, `promote_children` or `move_children_to_session` and GroupTreeRevision | `ack` |
| `set_group_membership` / `move_group_subtree` | foreground surface, exact same-Session node or Group, parent Group?, expected Node+GroupTreeRevision | `node_view` / `group_tree` |
| `repair_group_tree` | local foreground surface, exact corrupt GroupTreeRevision, bounded proved prefix and closed edge-removal/reparent repair plan | `group_tree` |
| `create_checkout_scope` / `adopt_checkout_scope` | foreground surface, exact existing-or-preassigned Session, target/trust/repository/worktree, creator `turn_created` or `adopted`, optional preassigned Group projection; one composite operation id | `checkout_scope_provisioning_receipt` |
| `bind_group_checkout_scope` / `unbind_group_checkout_scope` | foreground surface, same-Session Group+CheckoutScope and exact GroupTree/scope revisions | `checkout_scope_binding` |
| `move_and_rehome` | foreground surface, exact subtree, target CheckoutScope, GroupTree revision and complete stopped-descriptor preflight | `group_tree_rehome_receipt` |
| `unbind_checkout_scope` / `remove_checkout_scope` / `reconcile_checkout_scope` | foreground exact scope/revision and retained-worktree disposition / local consequence review with dirty/unpublished/owner/survivor inventory / exact scope+target inventory revisions | `checkout_scope` |
| `get_display_name_facts` | exact Node/Group and revision | `display_name_facts` |
| `set_local_display_name` / `unpin_local_display_name` | foreground surface, exact Node/Group revision and bounded sanitised alias / expected pinned fact revision | `display_name_facts` |
| `generate_name_proposal` / `apply_name_proposal` | foreground surface, exact target/source/redaction/generator/bounds / exact NameProposalId, target revision and expiry | `name_proposal` / `display_name_facts` |
| `update_work_item_metadata` | foreground surface, exact node/revision, closed state/priority/due/tags/comment/assignee patch | `node_view` |
| `list_work_item_sources` / `query_work_items` | visible source/profile scope / exact WorkItemSource+profile+project, filters/sort, cursor and page bounds | `work_item_source_list` / `work_item_page` |
| `create_external_work_item` / `edit_external_work_item` | foreground surface, exact source/profile/project and mapped fields / WorkItemKey, source revision/ETag and field patch | `work_item_mutation_receipt` |
| `comment_external_work_item` / `assign_external_work_item` | foreground surface, WorkItemKey, source revision and bounded comment / mapped assignee | `work_item_mutation_receipt` |
| `transition_external_work_item` / `close_external_work_item` / `reopen_external_work_item` | foreground surface, WorkItemKey, exact source revision and mapped transition/reason | `work_item_mutation_receipt` |
| `resolve_work_item_conflict` | foreground surface, WorkItemKey, conflict/source/local revisions and closed per-field choices | `work_item_mutation_receipt` |
| `open_file_for_edit` | foreground surface, exact FileBackend target/root/path, byte/encoding bounds | `file_edit_snapshot` |
| `save_file_edit` | foreground surface, operation id, exact target/root/path/file identity/hash/revision and bounded bytes | `file_edit_receipt` |
| `mark_node_result_read` | foreground `surface_id`, exact node/instance, result revision | `ack` |
| `create_context_link` | foreground surface, `operation_id`, tagged AgentInstance/Note source, destination instance, purpose, revision policy, scopes, cumulative limits, required expiry | `context_link` |
| `update_context_link` | foreground `surface_id`, link id, expected generation, purpose/scopes/limits/expiry patch, `operation_id` | `context_link` |
| `revoke_context_link` | foreground `surface_id`, `operation_id`, `context_link_id`, expected generation | `ack` |
| `prepare_context_packet` | foreground surface, generations, source, existing/new target spec, intent, separate next instruction?, budget, optional reviewed grant | `context_packet` |
| `deliver_context_packet` | `operation_id`, opaque reviewed packet capability | `context_packet_delivery` |
| `get_context_packet_delivery` | `operation_id` or packet id | `context_packet_delivery` |
| `respond_to_agent_interaction` | foreground surface, exact node/instance/attempt/generation/pending id and user-selected response | `ack` |
| `prepare_agent_message` | foreground surface, generations, source/destination instance, purpose, exact bounded body/recipe, required expiry | `agent_message` |
| `deliver_agent_message` | `operation_id`, opaque reviewed message capability | `agent_message_delivery` |
| `set_dependency_edge` | foreground surface, `operation_id`, source/target node, typed result contract, expected generation? | `dependency_edge` |
| `remove_dependency_edge` | foreground surface, edge id, expected generation | `ack` |
| `create_team` | foreground surface, `operation_id`, Session, members/roles, policy | `team` |
| `update_team` | foreground surface, team id, expected generation, member/role/policy patch | `team` |
| `delete_team` | foreground surface, team id, expected generation | `ack` |
| `create_flow_definition` | foreground surface, operation id, portable schema, expected catalogue/policy revisions | `flow_definition` |
| `get_flow_definition` | exact definition id and immutable revision | `flow_definition` |
| `version_flow_definition` | foreground surface, definition id/revision, operation id, immutable replacement | `flow_definition` |
| `archive_flow_definition` | foreground surface, definition id/revision, operation id | `ack` |
| `preflight_flow_run` | foreground surface, definition revision, inputs, target/isolation receipts | `flow_preflight` |
| `start_flow_run` | foreground surface, operation id, accepted preflight revision | `flow_run` |
| `get_flow_run` | run id/revision | `flow_run` |
| `pause_flow_run` / `resume_flow_run` | foreground surface, run id, expected revision, operation id | `flow_run` |
| `cancel_flow_run` / `abort_flow_run` | foreground surface, run id, expected revision, operation id, declared dispositions | `flow_run` |
| `retry_flow_step` / `reconcile_flow_run` | foreground surface, run/step/attempt, expected revision, operation id | `flow_run` |
| `issue_delegation_grant` / `revoke_delegation_grant` | foreground surface, run/current agent attempt, exact scopes/budgets/expiry, operation id/generation | `delegation_grant` |
| `get_delegation_grant` | exact grant id and generation; body visible only to its authorised operator scope | `delegation_grant` |
| `submit_delegated_operation` | grant capability, exact agent attempt/generation, operation id, closed typed operation | `operation_receipt` |
| `get_runtime_continuity` | exact node/instance | `runtime_continuity` |
| `query_conversation_inventory` | exact provider/Profile/Target/namespace, declared predicates, cursor and page/scan bounds | `conversation_inventory_page` |
| `adopt_conversation` | foreground surface, exact ConversationKey/inventory revision, destination Session and ownership proof | `agent_instance` |
| `resume_conversation` | foreground surface, exact ConversationKey/inventory revision and launch/preflight inputs | `agent_instance` |
| `read_conversation_title` | exact current ConversationKey and provider revision; requires `title_read` | `conversation_title_observation` |
| `rename_conversation` | foreground surface, exact ConversationKey/provider revision and bounded requested title; requires `conversation_rename` | `conversation_rename_receipt` |
| `list_native_jobs` / `get_native_job` | exact provider/Profile/Target/namespace plus cursor bounds / NativeJobKey and revision | `native_job_page` / `native_job` |
| `create_native_job` / `update_native_job` | foreground surface, exact provider/Profile/Target/namespace, closed schedule/model/flag spec / NativeJobKey and revision | `native_job_receipt` |
| `pause_native_job` / `resume_native_job` / `run_native_job_now` / `cancel_native_job_iteration` / `delete_native_job` | foreground surface, NativeJobKey/revision and exact iteration/disposition where applicable | `native_job_receipt` |
| `get_runtime_inventory` | foreground surface, exact ExecutionTarget/fingerprint/generation, known watermark? | `runtime_inventory` |
| `get_resource_inventory` / `subscribe_resource_inventory` / `unsubscribe_resource_inventory` | exact ResourceScopeKey and coverage watermark / same plus byte,row,cadence bounds / subscription id | `resource_inventory` / `resource_inventory_subscription` / `ack` |
| `terminate_resource_owner` | local foreground consequence review, exact target/trust/handle generations, process start identity and expected resource observation; delegates to the same exact RuntimeInventory termination authority | `runtime_inventory_termination_receipt` |
| `get_target_recovery_view` / `subscribe_target_recovery_view` / `unsubscribe_target_recovery_view` | local administrative surface, exact ExecutionTarget and target-stream revision / bounded subscription / subscription id | `target_recovery_view` / `target_recovery_subscription` / `ack` |
| `adopt_runtime_inventory_item` | foreground surface, operation id, exact target/handle/inventory revision, destination Session and Node kind | `node_view` |
| `ignore_runtime_inventory_item` / `terminate_runtime_inventory_item` | foreground surface, operation id, exact target/handle/inventory revision and expiry? / disposition | `operation_receipt` |
| `attach_runtime_attempt` | foreground surface, `operation_id`, tagged AttemptOwner, expected active/latest attempt/generation, verified endpoint receipt | `node_view` |
| `acquire_input_lease` / `renew_input_lease` / `handoff_input_lease` / `release_input_lease` | exact AttemptOwner/attempt/binding, client/surface/connection and expected lease generation | `input_lease_receipt` |
| `write_runtime_input` | exact AttemptOwner/attempt/binding, lease id/generation, client/surface/connection, monotonic input sequence and bounded bytes | `runtime_input_receipt` |
| `resize_runtime_input` | same exact owner/attempt/binding/lease/client fences, monotonic input sequence and bounded rows/columns/pixels | `runtime_input_receipt` |
| `create_account_profile` / `adopt_account_profile` | foreground surface, operation id, provider+ExecutionTarget and isolated non-secret config/auth reference | `account_profile` |
| `list_account_profiles` / `get_account_profile` | exact provider/ExecutionTarget scope / profile id | `account_profile_list` / `account_profile` |
| `begin_account_authentication` / `validate_account_profile` | foreground surface, profile id/revision; external provider flow receipt / bounded validation policy | `account_profile` |
| `rename_account_profile` | foreground surface, profile id/revision and bounded display name | `account_profile` |
| `set_default_account_profile` | foreground surface, exact Workspace-or-target/provider scope, profile id and expected revision | `account_default` |
| `retire_account_profile` / `delete_account_profile` | foreground surface, profile id/revision, operation id and explicit external-data disposition | `account_profile_result` |
| `get_account_activity` / `subscribe_account_activity` / `unsubscribe_account_activity` | exact provider/Profile/Target, filters, cursor/item/byte bounds / same plus subscription / subscription id | `account_activity` / `account_activity_subscription` / `ack` |
| `list_model_endpoint_profiles` / `get_model_endpoint_profile` | exact ExecutionTarget scope / profile id+revision | `model_endpoint_profile_list` / `model_endpoint_profile` |
| `create_model_endpoint_profile` / `update_model_endpoint_profile` | local foreground surface, exact target/trust, canonical HTTPS origin, protocol/pin policy, eligibility and credential reference+generation / profile+revision patch | `model_endpoint_profile` |
| `validate_model_endpoint_profile` / `discover_model_endpoint_models` | local foreground surface, exact profile/target/trust revision and bounded network/catalogue policy | `model_endpoint_profile` / `model_catalogue` |
| `set_default_model_endpoint_profile` | local foreground surface, exact Workspace-or-target/provider scope, profile revision and expected default revision | `model_endpoint_default` |
| `retire_model_endpoint_profile` / `delete_model_endpoint_profile` | local foreground surface, exact profile revision, operation id and survivor/default/secret-reference disposition | `model_endpoint_profile_result` |
| `list_notification_endpoints` / `get_notification_endpoint` | local administrative scope / endpoint id+generation | `notification_endpoint_list` / `notification_endpoint` |
| `pair_notification_endpoint` | local foreground surface, endpoint public key/token reference, device/profile, exact scopes/classes/privacy/rate/batch bounds and expiry | `delivery_grant` |
| `revoke_delivery_grant` | local foreground surface, exact endpoint/grant/generation and expected revision | `delivery_grant` |
| `get_notification_outbox` / `flush_notification_outbox` | local administrative exact endpoint/grant generation and bounded cursor / same plus current queue+presence revision | `notification_outbox` / `notification_flush_receipt` |
| `subscribe_live_notification_status` / `unsubscribe_live_notification_status` | exact authorised endpoint/scope and bounded subscription / subscription id | `live_notification_subscription` / `ack` |
| `issue_remote_permission_response_grant` / `revoke_remote_permission_response_grant` | local foreground surface, exact client/provider/profile/Session/Node/instance/attempt/generation/interaction/options/scope/expiry | `remote_permission_response_grant` |
| `submit_permission_response` | remote/Companion capability, exact grant/client/interaction/option/revisions/binding+connection generation and anti-replay nonce | `permission_response_receipt` |

`select_tree_node` continues to persist only surface-scoped navigation. The client derives a `ViewTarget`
and requests its content separately. A Node selection never opens, zooms or focuses a Pane, but it may
replace the WorkSurface content; that distinction supersedes ADR-048. `get_node_view` repeats the requested
key and carries a monotonic node-view revision so late answers can be discarded. Selection never launches,
cold-resumes, acknowledges or marks a result read; warm attachment only connects to an already live runtime.

`activate_session` is the separate ADR-049 user intent. One Session-row click may issue selection followed
by activation, but an Agent/child selection, `route_attention`, notification or automatic Focus never issues
it. One accepted operation restores the saved Layout, attaches proved live attempts and materialises every
eligible stopped runtime descriptor in the exact bounded preflighted plan. If the Session has no runtime
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
`node_view_changed` push repeats the subscription id, subject and monotonic revision. Reselection,
replacement connection, disconnect or explicit unsubscribe retires it; bounded backpressure emits a typed
gap and requires resubscription. Clients discard late pushes from any retired subscription or different
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
is refused at either bound. Archive/end/delete/expiry revocations are internal lifecycle transitions and
require no UI surface.

The broker request contains only that capability, a closed content kind, selector/range and requested byte/
token bound; its response repeats grant generation, provenance, redaction/truncation and remaining budgets.
It returns quoted untrusted data, never a control-protocol object that an agent can replay as authority.
Responses are bounded/non-streaming: the broker atomically reserves the maximum budget, buffers data, then
revalidates generation/expiry/endpoints and commits actual budget + audit immediately before exposing bytes.
A revoke committed first yields no body; a read committed first is already disclosed and remains charged.
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
exactly
`bound → unbound`, terminal for that binding id. `unbind_group_checkout_scope` drops only that projection and
leaves an active CheckoutScope unchanged; `unbind_checkout_scope` is the separate scope lifecycle operation.
The binding grants no runtime or repository authority;
it supplies default cwd/isolation only for new descendants or an explicit `move_and_rehome`. A presentation
move alone never changes a running cwd. Rehome atomically preflights every affected stopped descriptor and
refuses live writers. `CheckoutScopeState` is closed:

```text
provisioning -> active | reconcile_required
active -> missing | conflicted | unbinding | removing
missing | conflicted -> active | unbinding | reconcile_required
unbinding -> unbound | reconcile_required
removing -> removed | reconcile_required
reconcile_required -> only a state proved by fresh exact inventory
unbound | removed -> terminal for that CheckoutScopeId
```

Create/adopt/bind/unbind/remove/reconcile name exact Session, target/trust generation, repository/worktree
identity, `creator=turn_created|adopted`, scope revision and operation id. Missing or foreign worktrees become `missing|conflicted`, never a
same-looking local fallback. Unbind and Group deletion preserve the worktree. Remove is separately
consequence-labelled and requires fresh dirty, unpublished, path-owner, repository and live-writer proof;
adopted scopes default to unbind. Agent-per-branch Flow members keep separately owned scopes and a Group only
projects one of them. A single create/adopt request may preassign the CheckoutScope, Session and optional
Group/binding ids and drive their external-effect saga under one composite operation id. Its provisioning
receipt records each worktree/Session/Group boundary; a partial effect remains `reconcile_required` and can
neither duplicate the worktree nor expose an ownerless invisible resource.

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

`update_work_item_metadata` changes canonical Node metadata only. Its schema is closed:
`WorkItemState=backlog|ready|active|blocked|review|done|cancelled`,
`Priority=unset|low|normal|high|urgent`, optional UTC due instant, bounded unique normalised tags, append-only
bounded comments `{ comment_id, author, body, created_at, revision }`, and
`Assignee=none|AgentInstance(id)|TeamRole(team_id,role)`. A state field omitted from a patch does not move;
supplying its current value is a metadata no-op. The only non-self transitions are:

| From | May transition to |
| --- | --- |
| `backlog` | `ready`, `cancelled` |
| `ready` | `backlog`, `active`, `blocked`, `cancelled` |
| `active` | `blocked`, `review`, `done`, `cancelled` |
| `blocked` | `ready`, `active`, `cancelled` |
| `review` | `active`, `blocked`, `done`, `cancelled` |
| `done` | `ready` only with an explicit bounded reopen reason |
| `cancelled` | `backlog` only with an explicit bounded reopen reason |

The daemon compare-and-swaps the complete Node/WorkItem revision; comments never overwrite, and assignee is
display responsibility rather than control authority. These fields never mutate runtime/turn/dependency
state, satisfy a Flow result or emit Attention directly. Board/list/search clients project the returned
canonical Node revision.

An external binding is keyed only by
`WorkItemKey=(source_id, source_profile_id, project_namespace, external_item_id)`. One key maps to at most
one canonical Node, and one Node has zero-or-one current external binding plus immutable rebinding lineage;
title, URL and page order are never identity. A versioned `WorkItemSource` declares field/state/assignee
mappings, per-field `external|turn|overlay` authority, supported predicates/sorts, cursor/page/cache bounds,
webhook/poll mechanism, rate budget and a target-keystore credential reference. Snapshot/delta pages carry
source epoch, cursor/watermark, item revision/ETag, coverage and freshness. Partial/rate-limited/gapped data
may add or stale items but cannot prove deletion or exact zero.

Create, edit, comment, assign, transition, close and reopen are separate advertised capabilities and exact
operation variants. Every write compare-and-swaps the source revision and waits for an external receipt
before advancing the local projection. Timeout after a possible effect becomes
`reconcile_required(WorkItemKey,operation_id)` and is probed by key/revision, never replayed. Divergent field
revisions preserve external and local values in one conflict record until foreground per-field resolution or
a previously declared deterministic policy commits a new source revision. Deleting Turn's projection,
dismissing Attention, permission loss, unknown assignee, mapping change or stale cache never closes/deletes
the source item or invents a successful sync.

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
changed. They never use terminal input.

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
config reference into `draft` without reading credentials. Authentication opens a provider-owned flow,
stores only its receipt and moves to `authenticating`; validation separately proves provider/account identity
and capability before `active`. Rename changes only bounded display metadata under CAS. A default may
reference only an `active` profile on the exact trusted target generation with proved isolation and required
capability. Default resolution is explicit
launch, immutable Flow/Template, Workspace, then target/provider; LaunchReceipt pins the result, and changing
a default never migrates active instances. When a default becomes ineligible it is explicitly unset; Turn
never selects another profile silently. `expired|revoked|auth_failed` never falls back to another account.
Retire removes launch/default eligibility and preserves evidence; delete is allowed only from the declared
draft/auth-failed/retired transitions with zero active attempts, current bindings, defaults, grants or
retained required references, and never deletes provider-side data without a separately typed disposition.
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
submitted or unconfirmed write and never imply one another. Delivery assigns FIFO order under declared
per-destination count/byte/TTL bounds, refuses overflow, requires a structured idle endpoint with no pending
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

`FlowRunState` is `preflighting|provisioning|running|paused(resume_state)|failing|cancelling|
reconcile_required(last_proved,desired_terminal?)|completed|failed|cancelled|aborted`, with the legal
transition, desired-terminal, StepAttempt and deterministic result-aggregation rules in the master contract.
Definition revisions are immutable; FlowRun receipts are append-only. Pause starts no
new step and suspends recurrence evaluation without freezing existing runtimes. Cancel revokes grants before
applying each persisted runtime disposition; abort is a separate foreground force action and remains
`reconcile_required(desired_terminal=aborted)` while any disposition is uncertain. Step retry creates
one new bounded attempt and whole-run retry creates a lineage-linked run. Every Flow/Grant request includes
the expected definition/run/authority revision plus operation id; a stale request fails before effects and a
duplicate returns the original receipt. The exact recurrence timezone/DST/missed/overlap/occurrence bounds in
the product contract are wire fields, not free-form settings.

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

The adapter capability schema is versioned and closed to exactly 22 facts:
`launch|resume|branch|stop|structured_status|questions|permissions|subagents|transcript|context_usage|
provider_quota|model_switch|messaging|context_transfer|shared_identity|durable_attach|delegated_control|
native_jobs|conversation_inventory|title_read|conversation_rename|model_gateway`. Each fact is independently
keyed by adapter/CLI version, provider, AccountProfile, ExecutionTarget, endpoint, attempt/generation and
observation epoch as applicable and reports `supported|unsupported|degraded|unknown`, mechanism, bounds,
freshness and expiry. Claude Code, Codex, Gemini, OpenCode, GitHub Copilot and Grok each use a dedicated
adapter and the complete matrix; provider-name branches and executable-name inference are invalid substitutes.
Kimi and MiniMax are first-class profile-scoped quota/activity connectors only until a separate launch
adapter is advertised, so those connectors expose no launch, transcript, conversation or control authority.
The generic terminal adapter advertises only facts it proves. RuntimeBackend, broker, endpoint and delegated-
authority capabilities remain separate and an operation uses their intersection, never their union.
The vNext `welcome` reports `adapter_capability_schema_version` and its canonical registry hash. The frozen
`docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv` source-capability ledger is release authority, not a runtime payload:
no request may read, import or mutate it, and its rationale/evidence digests never cross the wire.

`RuntimeContinuityView` reports endpoint kind/host, stable non-secret fingerprint/generation, integration
capability and a list of independently keyed `RuntimeEndpointBinding`s: provider/account/host,
AgentInstance/RuntimeAttempt, conversation, last observation and attach/resume confidence—never a bearer
token or descriptor. Semantic conversation ownership is keyed independently of transport:

```text
ConversationKey=(provider_id, AccountProfileId, ExecutionTargetId, provider_namespace, normalized_provider_conversation_id)
BindingState = proposed | current | refused | stale | unbound | retired
```

`normalized_provider_conversation_id` is exact private identity and the adapter declares the namespace that makes it
unique; target and profile prevent cross-host/account aliasing. Across all endpoint records and generations,
one `ConversationKey` has at most one `current` AgentInstance owner and one AgentInstance has at most one
current binding. Endpoint generation fences the transport only; it cannot make the same conversation a
second semantic identity. Binding transitions are closed: `proposed → current|refused`, `current →
stale|unbound|retired`, `stale → current|unbound|retired`, `unbound → proposed|retired`, and
`refused|retired` are terminal for that binding id. A duplicate current claim is rejected before input,
transcript or context authority and cannot displace the proved owner. Endpoint mismatch, generation
discontinuity, ownership conflict or stale proof changes only BindingState/connectivity, never
`Lifecycle::Lost`; only an independent bounded RuntimeBackend/provider absence proof may produce Lost.

Every input/transcript/context/Attention operation names binding id and binding generation as well as the
ConversationKey hash. One endpoint may multiplex many current bindings, but duplicate or cross-account/
target claims fail without changing siblings. `attach_runtime_attempt` is warm attachment to a verified
still-live endpoint and launches nothing; a cold resume remains `restart_agent_instance`. Reconnect first
creates/revalidates a `proposed|stale|unbound` binding and promotes it atomically only after the global uniqueness check.
A configured provider-runtime/multiplexer is part of the continuity seam; general Remote/SSH Session creation
is M16. Acceptance creates competing endpoint records and generations for one ConversationKey and proves
that no observation can produce two current owners.

`ConversationInventory` is a private bounded adapter query over one exact provider, AccountProfile,
ExecutionTarget and provider namespace. Pages contain ConversationKey, optional provider title, created/
updated time, native status, model/mode hints, ownership/resumability evidence, source revision, coverage and
freshness—never ambient transcript bodies. The adapter declares provider-side versus complete-cache search,
predicates/normalisation, cursor/page/scan/cache/rate bounds; gaps, partial pages, rate limiting and
unsupported search cannot prove absence or exact zero. Exact-key proof may bind; title/text similarity is
advisory only. Adopt creates one stopped Node/AgentInstance and `proposed|current` binding without launch.
Resume is a separate preflighted operation that creates a RuntimeAttempt only after resumability and global
ConversationKey ownership revalidation. Title observation requires `title_read`; provider mutation requires
the distinct capability `conversation_rename`, exact expected provider revision and an idempotent receipt with
requested/effective title. Unsupported or uncertain rename may create only an explicitly local alias and
never claims provider mutation.

Provider-native work is keyed by
`NativeJobKey=(provider_id, AccountProfileId, ExecutionTargetId, provider_namespace, provider_job_id)`.
Exactly one current Job Node owns a key; ordered `NativeJobIteration`s carry ordinal/native id, scheduled/
started/finished time, result/error and optional exact AgentInstance/RuntimeAttempt reference. The normalised
job state is `scheduled|running|paused|completed|failed|cancelled|unknown`, with total adapter mapping and
native value retained. `native_jobs` independently advertises list/create/update/pause/resume/run-now/cancel-
iteration/delete. Every mutation carries exact job revision and profile/target generation; an ambiguous
effect reconciles by NativeJobKey before retry. Dismissing Attention, hiding/deleting the Turn projection or
ending a Session never cancels/deletes provider work. Portable packages may carry only inert job
configuration text, never provider_job_id, activation or authority.

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
`switch_agent_model` is the sole in-place model-route mutation. It requires current `model_switch` and
`model_gateway` facts as applicable, exact instance/attempt/profile generations and a fresh preflight. Only a
provider proof of the same conversation may atomically close the old configuration epoch and create the new
RuntimeAttempt/receipt. Any refusal, timeout or uncertain configuration proof leaves the prior attempt/input
authority current and visible; it never silently branches, restarts, changes defaults or selects another
route. A provider that cannot prove continuity offers an explicit Branch/new-instance operation instead.

`respond_to_agent_interaction` is not permission automation. It exists only for a foreground, explicit user
action against the exact typed prompt id and current attempt. The daemon refuses stale ids and adapter/
prompt mismatches, records submission as pending, and waits for provider evidence before resolving
Attention. A local operator may continue through verified PTY input when the provider lacks the typed
capability. Remote bytes do not inherit that fallback: the remote InputSafetyState policy below refuses a
sensitive or unclassifiable prompt.

### Accepted multi-client, companion and remote-backend target (not in v4)

State ownership uses a closed tagged stream key, not a fictitious Workspace for global/target state:

```text
StateStreamKey = Installation(daemon_generation)
               | Workspace(daemon_generation, WorkspaceId)
               | ExecutionTarget(daemon_generation, ExecutionTargetId, target_generation)
StateWatermark = sorted[{ StateStreamKey, revision }]
```

Installation owns the ExecutionTarget/AccountProfile catalogue, one logical Attention order, notification
endpoints/grants/outbox and target-independent policy; Workspace owns its Sessions/Nodes/relationships/Flows,
GroupTree/CheckoutScope projections, display-name facts and Workspace defaults; ExecutionTarget owns backend
connectivity, RuntimeInventory/ResourceInventory/Recovery, NativeJobs, RuntimeEndpoints,
ModelEndpointProfiles and target/profile observations. A projection may reference another stream id but
cannot mutate or revision it. A client
subscribes to an authorised set. Each `state_snapshot` closes its named stream at revision `R`; that stream's
`state_event` is strictly sequenced from `R+1`, and
`ack_state_revision` advances only the named cursor.

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

`acquire_input_lease`, `renew_input_lease`, `handoff_input_lease` and `release_input_lease` carry exact
runtime/input owner, client/surface/generation and expected lease generation. A lease expires after 15 seconds
and renews no more often than five; handoff changes generation atomically and both clients observe it before
new bytes are accepted. `write_runtime_input` and `resize_runtime_input` repeat the exact AttemptOwner,
RuntimeAttempt id/generation, optional current RuntimeEndpointBinding id/generation, lease id/generation,
client id, surface/connection generation and a monotonically increasing per-lease input sequence inside their
`MutationEnvelope`. Validation and byte/resize enqueue are one serial action; any mismatch accepts zero bytes
and returns a typed receipt. A duplicate operation/sequence with identical bytes returns the first receipt;
different bytes conflict. Lease handoff/expiry closes the old sequence space before the new owner is visible.
The v4 `write_pty`/`resize_pty` shape is available only to a negotiated local single-client v4 connection.
Negotiating vNext state streams, any remote role or multi-client operation makes those legacy requests
`unsupported_protocol`; they cannot bypass lease, attempt, binding or surface fences. Draft bodies are not
protocol state and never transfer between clients.

Background notification is an Installation-stream projection of canonical Attention keyed by
`NotificationEndpointId`, never a second queue. A foreground-paired `DeliveryGrantId` binds endpoint public
key/token reference, device/profile, allowed Workspaces/ExecutionTargets, event classes, privacy detail,
rate/batch bounds, generation and expiry. `DeliveryGrantState` is closed:
`proposed → active|invalid|revoked`,
`active → expired|invalid|revoked`; terminal states never reactivate. Tokens/private keys remain in the
keystore or target agent and are absent from reads, exports, logs and diagnostics. A 401/403 or revocation
invalidates only the exact generation.

```text
NotificationDeliveryState:
eligible -> held_present | queued | superseded | expired
held_present -> queued | superseded | expired
queued -> submitted | superseded | expired
submitted -> accepted | failed_retryable | failed_terminal | superseded | expired
failed_retryable -> queued | failed_terminal | superseded | expired
accepted | failed_terminal | superseded | expired -> terminal
```

One `NotificationDeliveryId` survives the bounded retry edge; each return to queued increments an attempt
counter and applies declared jitter/rate limits, with exhaustion forcing `failed_terminal`. Gateway
`accepted` proves neither device delivery, reading nor demand resolution. Collapse identity is split:
`CollapseFamilyKey=(NotificationEndpointId,complete AttentionSubject identity,demand_kind)` is stable across
revisions, and `CollapseKey=(CollapseFamilyKey,subject_revision)` identifies one delivery. Only a newer
current revision in the same family may supersede an older delivery. Insert and flush both revalidate grant,
queue revision, resolution and presence. The bounded E2EE payload excludes transcript/path/command/secret;
failure changes no Attention, unread or runtime state. Deep links carry opaque ids and must resnapshot and
route the exact current Attention before display or action.

Live status uses `LiveStreamKey=(NotificationEndpointId,AttentionSubject identity,attempt_generation)` plus
monotonic event revision. Start/update/end are collapse-aware and end/tombstone fences every late tick.
Presence may hold an alert but never pauses the authoritative stream; release enqueues only a still-current
demand. `NotificationHostMode=owner_local|loopback_observer` accepts authenticated owner-local or loopback
observation input and makes outbound HTTPS delivery only. It ignores public bind host/port and exposes zero
public inbound listeners. It is not a `RemoteOperatorSurface`; notification or headless delivery grants no
input/control/credential authority, and remote GUI/headless protocol clients remain separately authenticated.

`CompanionAction` is closed to `route_attention|mark_result_read|acknowledge|snooze|dismiss|
submit_free_text_response|submit_permission_response|interrupt|request_writer_lease`. Each request names
capability, expected queue/subject/interaction/authority revisions, expiry and operation id. Free text is
valid only for a verified non-sensitive question/decision. `submit_permission_response` is the only permission
variant and requires the single-use grant below; it is distinct from legacy `approve_permission` and never
accepts free-form/credential bytes. Credential/password entry, response-grant administration, host trust/
rotation, force kill/destroy, checkout integration and publish/merge have no companion operation. An offline
client stores only an encrypted local draft; stale reconnect refuses rather than replaying it. Every accepted/
refused/uncertain action returns a receipt.

`RemoteOperatorSurface` is a full protocol client role, not a CompanionAction superset and not a WebPreview or Browser Node
origin. Its invitation/session capability is distinct from the local administrative token, binds authenticated
origin, operator, Workspace/scope, surface/connection generation, expiry and negotiated view/mutation/input
capabilities, and is transported only over authenticated encryption with anti-replay. HTTP mutations require
same-origin CSRF proof; WebSocket upgrade validates the exact Origin and capability before any snapshot.
Tokens never enter URL/query, logs, referrers or browser persistent storage. WebPreview and Browser Node
content runs in separate isolated origins and cannot reach this channel.

After negotiation it consumes the same `state_snapshot/state_event`, NodeView subscriptions, terminal stream,
AttentionRoute and input-lease operations as desktop. Remote dispatch is default-deny. The versioned protocol
registry assigns every exact operation name one `remote_class=read|mutate|input|denied`; its canonical sorted
manifest hash is returned in the handshake and covered by compatibility tests. An absent/new name is
`denied` until a new protocol version classifies it. For this target the complete non-denied sets are:

```text
read = get_node_view, subscribe_node_view, unsubscribe_node_view, route_attention,
       state_snapshot, state_event, ack_state_revision,
       get_flow_definition, get_flow_run, get_runtime_continuity, open_file_for_edit,
       list_work_item_sources, query_work_items, query_conversation_inventory,
       read_conversation_title, list_native_jobs, get_native_job,
       get_account_activity, subscribe_account_activity, unsubscribe_account_activity
mutate = select_tree_node, update_surface_activity, activate_session,
         create_agent_instance, restart_agent_instance, branch_agent_instance,
         create_runtime_node, create_resource_node, update_resource_node,
         set_group_membership, update_work_item_metadata, save_file_edit,
         mark_node_result_read, set_dependency_edge, remove_dependency_edge,
         create_team, update_team, create_flow_definition, version_flow_definition,
         preflight_flow_run, start_flow_run, pause_flow_run, resume_flow_run,
         retry_flow_step, respond_to_agent_interaction(non_authorising_only),
         submit_permission_response(single_use_grant_only)
input = acquire_input_lease, renew_input_lease, handoff_input_lease,
        release_input_lease, write_runtime_input, resize_runtime_input
```

Membership is necessary but not sufficient: the invitation's exact Workspace/stream/object/capability scope,
MutationEnvelope and server policy must also allow the request. Every other operation—including credential/
secret entry, permission-response grant issue/expand/revoke, AccountProfile/auth/default or
ModelEndpointProfile/default/discovery, DeliveryGrant/notification administration,
DelegationGrant/root-context/message authority, ExecutionTarget trust/administration, WorkspaceOnboarding,
CheckoutScope removal/rehome, destructive delete/cancel/abort/stop/terminate,
RuntimeInventory/ResourceInventory/Recovery enumeration or control and repository integration/publish/merge—
is server-refused even if a client forges its wire shape.

`RemotePermissionResponseGrantId` names one `RemotePermissionResponseGrant` issued/revoked only by a local
foreground operator. It is single-use and
binds client, provider/profile, Workspace/Session, Node/AgentInstance/RuntimeAttempt/generation, exact
PendingInteraction id/revision, provider-offered closed response ids, maximum scope/duration and expiry.
`submit_permission_response` repeats that grant, exact typed option, every interaction/authority revision,
binding/connection generation, operation id and anti-replay nonce inside an authenticated encrypted envelope.
The daemon revalidates the current adapter `permissions` capability immediately before dispatch; the option
cannot widen the provider offer. Denial is a typed option. The durable accepted/refused/uncertain receipt is
provider-correlated, and Attention closes only from later provider evidence. This vNext operation is distinct
from legacy local `approve_permission`; no remote alias or PTY fallback exists.

Input has a separate guard. Each agent attempt exposes
`InputSafetyState=ordinary|non_authorising_interaction(id,revision)|sensitive_interaction(class,id,revision)|
unknown`, where sensitive class is `permission|credential|host_trust|grant|destructive_confirmation`.
Remote raw bytes are accepted only in `ordinary`; a non-authorising question/decision uses the exact typed
`respond_to_agent_interaction` route. A recognised permission accepts only matching
`submit_permission_response` under the grant above; credential, host-trust, grant, destructive-confirmation
and unknown states accept neither a remote response nor bytes. A provider that cannot classify prompts with
a verified structured adapter advertises no remote input for that attempt.
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

Portable export carries package-local ids only. Import remints Workspace, Session, inert Node shape, Team,
FlowDefinition and relationship ids through one package map. A `FlowRun` is never portable: attempts,
provider conversation/NativeJob/runtime/process/PTY ids, revisions, operation receipts, grants, tombstones, credentials and machine/
host identity are constitutive run authority/evidence and are invalid package fields. An optional
`PortableRunReport` is a different inert type containing only package-local definition/step references,
origin content hash, terminal label, bounded redacted summaries, artifact content hashes and timestamps with
untrusted provenance. Import may display/link that report, but cannot decode it as FlowRun, satisfy a
dependency/result, emit Attention, authorise work, resume/retry/reconcile, or supply a launch receipt. To run
again the operator adopts the reminted FlowDefinition and creates a fresh preflight/FlowRun id. Collisions and
unresolved references create inert errors, never update/create by caller-selected local id.

### Accepted local dictation protocol target (not in v4)

ADR-060 adds no request that can open a microphone, capture PCM or run transcription. Those are foreground
native-client acts. The reserved M15 operations manage only trusted model artifacts and an already reviewed
text commit:

| Planned `op` | Principal fields | Planned answer |
| --- | --- | --- |
| `list_local_speech_models` | none | `local_speech_models` |
| `install_local_speech_model` | foreground surface, `operation_id`, closed `model_id` | `local_speech_model_state` |
| `cancel_local_speech_model_install` | foreground surface, `operation_id`, model id/generation | `local_speech_model_state` |
| `remove_local_speech_model` | foreground surface, `operation_id`, model id/generation | `local_speech_model_state` |
| `commit_operator_text` | foreground surface/connection/daemon generation, `operation_id`, exact `InputTarget`, expected input revision, `insert` or `submit`, bounded UTF-8 text | `operator_text_delivery` |

`InputTarget` repeats exact Workspace/Session/Node, optional AgentInstance, current RuntimeAttempt/generation,
verified input owner and optional pending free-text interaction/revision. The daemon revalidates all of it
immediately before one fenced write. Permission, credential/password, provisional/unassigned, raw-TTY and
unverified alternate-screen targets are invalid. Text is independently control-stripped and bounded;
`insert` cannot append Enter, while `submit` performs exactly one reviewed send. Dictation provenance grants
no authority or confidence. An uncertain partial write is `submitted_unconfirmed` and is never replayed.

Model list/state/progress contains closed model id, expected/observed digest and size, catalogue/engine
compatibility, licence, generation and safe error code. It never contains PCM, transcript, device identity or
arbitrary URL/path. Audio, hypotheses and the inline draft never cross this protocol. The full target,
settings, privacy and acceptance contract is `docs/LOCAL_VOICE_INPUT.md`.

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
| `delete_privacy_data` | `scope: PrivacyScope`, `disposition` | `privacy_deleted` |
| `compact_privacy_data` | — | `privacy_compacted` |

`PrivacyScope` is tagged as `installation`, `workspace { workspace_id }`, `session { session_id }` or
`agent { session_id, node_id }`. Reports enumerate counts and bytes by stored type, the resolved retention
policy, and the explicit no-telemetry facts. Exports are owner-only, create-new JSON documents; every datum
names its origin/type/timestamp and carries redacted content or an explanation that its filesystem payload
was omitted. Existing destinations and symlinks are refused.

Selective deletion stops the named work according to the required disposition, removes Turn-owned database
and filesystem records, compacts SQLite and reports any escaped process ids. `keep_processes` is invalid.
Installation scope is refused by the live daemon and must use the lock-protected offline
`turnd --delete-installation-data` operation. Compact applies retention, bounds/scrubs the diagnostic log,
checkpoints the WAL and vacuums the database.

### Workspaces

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_workspaces` | `include_archived?` | `workspaces` |
| `create_workspace` | `name`, `root` | `workspace` |
| `rename_workspace` | `workspace_id`, `name` | `workspace` |
| `archive_workspace` | `workspace_id`, `archived` | `workspace` |
| `duplicate_workspace` | `workspace_id`, `name?` — settings only, no sessions | `workspace` |
| `close_workspace` | `workspace_id`, `disposition` | `closed` |
| `delete_workspace` | `workspace_id`, `disposition` | `closed` |
| `get_workspace_write_lease` | `workspace_id` | `workspace_write_lease` |
| `acquire_workspace_write_lease` | `workspace_id`, `session_id`, `checkout_id` | `workspace_write_lease` |
| `release_workspace_write_lease` | `workspace_id`, `lease_id`, `expected_generation` | `workspace_write_lease` |

`archive_*` takes a flag rather than existing as two operations, so undo is the
same code path as do.

The four destructive operations answer `closed` rather than `ack`, and the difference is the point:
`closed` carries `escaped`, the processes Turn could not stop. A destructive act is authoritative — it
does not fail because a process survived the daemon that started it, since refusing would leave that
process running anyway and the user holding a Session they had finished with (ADR-050). `escaped` is
empty in the ordinary case; each entry names `node_id`, `session_id`, `title` and the last observed
`pid`, which is what a user needs in order to find it in a process list. Nothing in that path claims
the process exited: its `Lifecycle` stays `orphaned`.

### Sessions

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_sessions` | `workspace_id?` (absent = all), `include_archived?` | `sessions` |
| `create_session` | `workspace_id`, `name`, `cwd?`, `panes?`, `note?`, `tags?` | `session` |
| `create_session_from_template` | `workspace_id`, `template_id`, `name?`, `cwd?`, `branch?`, `task?` | `session` |
| `create_read_only_session` | `workspace_id`, `name`, `cwd?`, `panes?`, `note?`, `tags?` | `session` |
| `create_read_only_session_from_template` | `workspace_id`, `template_id`, `name?`, `cwd?`, `branch?`, `task?` | `session` |
| `create_worktree_session` | `workspace_id`, `name`, `branch`, `worktree_path?`, `panes?`, `note?`, `tags?` | `session` |
| `create_worktree_session_from_template` | `workspace_id`, `template_id`, `name?`, `cwd?`, `template_branch?`, `task?`, `branch`, `worktree_path?` | `session` |
| `rename_session` | `session_id`, `name` | `session` |
| `archive_session` | `session_id`, `archived` | `session` |
| `duplicate_session` | `session_id` — same shape, new identity, no processes | `session` |
| `set_session_favourite` | `session_id`, `favourite` | `session` |
| `set_session_pinned` | `session_id`, `pinned` | `session` |
| `close_session` | `session_id`, `disposition` | `closed` |
| `delete_session` | `session_id`, `disposition` | `closed` |
| `get_session` | `session_id` | `session_details` |
| `get_process_tree` | `session_id` | `tree` |

### Settings

| `op` | Fields | Answers with |
| --- | --- | --- |
| `get_settings` | `session_id?` (absent = the Global level alone) | `settings` |
| `set_setting` | `scope`, `owner_id?`, `key`, `value` | `settings` |
| `reset_setting` | `scope`, `owner_id?`, `key` | `settings` |

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

`create_session` and `create_session_from_template` always request the primary checkout; callers cannot
smuggle a generic mode or checkout through either shape. Creating `main_checkout` persists the Session
assignment and acquires the lease in one atomic store
transaction, before init commands, processes or Panes exist. A conflict rolls the transaction back, returns
the typed context in §5 and leaves no partial Session. Duplicate never inherits an active lease; it must
choose/reconcile a mode. Read-only replies include `read_only_enforced`, because metadata and agent guidance
are not enforcement. True means the daemon constructed the platform process guard and may launch the
configured Layout; false means commands remain stopped. Worktree replies include the new checkout and
declared shared resources.

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
draft identity only, and every Session instantiation mints a fresh set.

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
| `close_pane` | `session_id`, `pane_id`, `disposition` | `layout` |
| `resize_pane` | `session_id`, `pane_id`, `delta` | `layout` |
| `resize_divider` | `session_id`, `before`, `after`, `delta` | `layout` |
| `equalize_divider` | `session_id`, `before`, `after` | `layout` |
| `apply_layout_preset` | `session_id`, `preset` | `layout` |
| `focus_pane` | `session_id`, `target` | `layout` |
| `relocate_pane` | `session_id`, `moved`, `target`, `zone` | `layout` |
| `swap_panes` | `session_id`, `a`, `b` — superseded by `relocate_pane` | `layout` |
| `zoom_pane` | `session_id`, `pane_id` — **toggles** | `layout` |
| `duplicate_pane` | `session_id`, `pane_id` | `layout` |
| `change_pane_kind` | `session_id`, `pane_id`, `kind` | `layout` |
| `float_pane` | `session_id`, `pane_id`, `geometry` | `layout` |
| `dock_pane` | `session_id`, `pane_id` | `layout` |
| `set_floating_pane_geometry` | `session_id`, `pane_id`, `geometry` | `layout` |
| `attach_pane` | `session_id`, `pane_id`, `size`, `stream?` | `attached` |
| `resync_pane` | `session_id`, `pane_id` | `screen` |
| `pane_image` | `session_id`, `pane_id`, `image_id` | `pane_image` |
| `detach_pane` | `session_id`, `pane_id` | `ack` |
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

`placement` is `replace_current`, `split_right`, `split_below` or `temporary`. Opening an existing node and
promoting its temporary view reuse the same vocabulary; only the temporary choice leaves the saved Layout
untouched. `create_pane` accepts the complete `NewPane`, including an arbitrary executable and argv without
shell evaluation. `duplicate_pane` creates another view of the same node rather than another process.
`change_pane_kind`, floating, docking and geometry updates are likewise view-only operations. A floating Pane
retains its split-tree position and exact point geometry, so docking restores it without reparenting its node.
The wire-level `detach_pane` remains the older operation that detaches a client's output stream; `float_pane`
is the distinct saved-Layout operation.

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
- `pane` (a `NewPane`): `kind` plus optional `title`, `command`, `args`, `cwd`,
  `env`, `restore`. The daemon mints the `PaneId` — it is the only writer of state,
  and a client minting its own would collide with a second client on the same
  daemon.
- `stream`: `"cells"` (absent means cells) \| `"bytes"`. See §2. `size` is applied to
  the pty before the screen or replay is taken, so what comes back matches the geometry
  the client is about to render at. `rows * cols` over `max_screen_cells` is
  `invalid_argument`.
- `resync_pane` asks for a pane's whole screen again after a missed update (§8.1). It is
  read-only, it requires an attachment (`pane_not_attached` otherwise), and on a `bytes`
  attachment it answers `conflict`: what that client lost was bytes, and its way back is
  to attach again for the replay.
- `pane_image` fetches the pixels of one inline image the pane's screen refers to (§2.3). It
  is read-only, and `not_found` is a normal answer: an image that has scrolled out of the
  daemon's bounded store is gone, and saying so is better than handing back a different
  picture.
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

An ad-hoc draft is client/surface/daemon-generation bound and memory-only. A disconnect before delivery
acceptance changes `draft|reviewed → draft_lost`; after acceptance the same daemon may hold the bounded body
only for the in-flight saga. Durable hash/manifest metadata cannot reconstruct it. A still-valid Flow recipe
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
remains visible with the exact phase/error and semantic retry or `delete_agent_instance` action; no timeout
silently deletes it.

### Node control

| `op` | Fields | Answers with |
| --- | --- | --- |
| `interrupt_node` | `session_id`, `node_id` | `ack` |
| `terminate_node` | `session_id`, `node_id` | `ack` |
| `kill_node` | `session_id`, `node_id` | `ack` |
| `relaunch_node` | `session_id`, `node_id`, `resume?` | `node` |

`interrupt_node` writes the interrupt character through the tty so it reaches the
whole foreground process group, not only the process Turn spawned. `resume` asks
the adapter to continue the agent's previous conversation where the tool supports
it.

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
| `keep_processes` | Detach only. Processes keep running under the daemon. |
| `terminate` | Ask them to stop, the way closing a terminal would. |
| `kill` | Stop them without asking. |

`keep_processes`, closing the UI and archiving a Session with a live writer do not release its checkout
lease. A fenced explicit release is valid only after no runtime node owned by the Session remains running;
the same atomic operation demotes the Session to `read_only`. Before restoring Sessions or emitting any
heartbeat, a new daemon changes every non-`released` lease
to `recovery_required` while preserving its id, generation and previous heartbeat. Loading the former owner
never auto-adopts that authority. An explicit `acquire_workspace_write_lease` may promote a read-only
Session only after all of its runtime nodes have ended; success acquires the lease and changes the Session to
`main_checkout` in the same durable transition.

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
| `hierarchy` | `snapshot: HierarchySnapshot` |
| `inspector` | `details: InspectorDetails` |
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
| `hierarchy_changed` | `snapshot: HierarchySnapshot` — full replacement with monotonic revision |
| `activity_preview_changed` | `hierarchy_revision`, `session_id`, `node_id`, `preview?: ActivityPreview` |
| `pane_bindings_changed` | `hierarchy_revision`, `session_id`, `node_id`, `bindings: [PaneNodeBinding]` |
| `workspace_write_lease_changed` | `hierarchy_revision`, `workspace_id`, `lease?: WorkspaceWriteLease` |
| `layout_changed` | `session_id`, `layout` |
| `pty_resized` | `session_id`, `node_id`, `size` |
| `restore_result` | `session_id`, `state`, `needs_explanation`, `panes` |

The incompatible ADR-059 protocol adds `node_view_changed`, `runtime_attempt_changed`,
`context_usage_changed`, `quota_scope_changed`, `context_link_changed` and `context_packet_changed`. Large
content pushes are sent only for an active `NodeViewSubscription` and repeat its subscription id, exact
subject and monotonic revision; a gap is explicit and cancels the stream. Context/quota pushes name their
stable scope ids rather than duplicating a sample per Agent.

M13 additionally reserves `agent_message_changed`, `dependency_edge_changed`, `team_changed` and
`runtime_continuity_changed`. Each repeats its stable id/generation and carries bounded metadata/evidence,
never a message body, runtime bearer or implicit execution instruction.

`hierarchy_changed` sends the whole projection, not a structural diff. A client accepts only a strictly
newer revision from the same daemon; a gap, reversal or daemon identity change requires `get_hierarchy`.
Applying a diff to stale ownership is how a sidebar invents an edge. Preview/binding pushes are bounded
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

### `HierarchySnapshot` — the one navigation projection

`revision`, `tree_state`, `workspaces`. There is no duplicate top-level `surface_id`.

`tree_state` is `TreeSurfaceState { surface_id, selected?, expanded, manual_order, filters,
visibility_mode, scroll_anchor? }`. Keys are tagged `workspace`/`session`/`process`; interaction state from
another surface is never merged into it. The search query is intentionally transient; the durable fields
restore the navigational context without persisting arbitrary repository or task text.

Each `WorkspaceTreeView` contains `workspace`, `checkouts`, `write_lease?` and ordered `sessions`. Each
`SessionTreeView` contains `session` and ordered node rows. The daemon supplies parent/depth/order, derived
state and badges, relationship confidence, preview, bindings and capability; the GUI does not join separate
lists or infer missing values. A snapshot is a full replacement. `revision` rejects stale/out-of-order
delivery and tells a client when to resynchronise.

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

Identity: `kind`, `is_agentic`, `title`, `command`, `args`, `cwd`, `pid`, `ppid`, `ephemeral`. Ephemeral
process-table plumbing remains searchable but is hidden outside Technical mode unless a search reveals it.

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

| | Stops processes | Leaves the tree | Record kept | Reversible |
| --- | --- | --- | --- | --- |
| `archive_*` | no | yes | yes | yes |
| `close_*` | yes | no | yes | the work is not |
| `delete_*` | yes | yes | **no** | **no** |

`delete_session` and `delete_workspace` remove Turn's *record*: the Session or Workspace row,
its layout, its process tree, its event log, its attention entries, its scratch directory and
its per-window tree state. A Workspace takes its Sessions with it.

They do **not** touch the user's disk. The checkout is a directory the user chose and Turn does
not own it: no file is removed, no branch and no worktree is deleted. Every surface that offers
a delete has to say so, and naming the exact path is better than promising in the abstract.

`keep_processes` is refused for both: nothing would name those processes once the record is
gone. Deleting something already gone answers `ack` rather than `not_found`, so a client that
lost a reply can retry.

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

`WorkspaceWriteLease` carries Workspace/Session/checkout identity, `mode: exclusive_write`, state,
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
2. **Turn never chooses an operator response.** Legacy local v4 has no
   `approve_permission` or `respond_permission`; the operator types through `write_pty`. vNext may transport
   an exact operator-selected non-authorising response through `respond_to_agent_interaction`, or an exact
   provider-offered permission option through `submit_permission_response` under its single-use local-
   foreground-issued grant. Neither path infers, widens or automatically selects an answer, and a context
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
5. **A client cannot request an unarbitrated primary-checkout writer.** Session mode is closed, creation
   goes through daemon lease arbitration, and conflict recovery is one of the typed alternatives. There is
   no force/steal flag; generation mismatch is a conflict, not a retry loop.
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
4. Mint or restore a stable `surface_id`, then call `get_hierarchy`. Render that snapshot as the one
   navigation projection; use list/detail operations only for their named administrative purpose.
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
