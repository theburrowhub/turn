# The Turn daemon protocol

Normative contract for the implemented `turn-proto` version **3**. Version 2 is historical context for the
retained terminal-cell transport, not a supported navigation mode. Operations explicitly labelled planned
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
{"v": 3, "type": "request", "id": "r-1", "request": {"op": "..."}}
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
- per-surface `set_tree_expanded` and `select_tree_node`, which are acknowledged but not broadcast;
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

The cell protocol introduced in v2 is retained unchanged. This pre-release codebase serves v3 only:
`MIN_PROTOCOL_VERSION == PROTOCOL_VERSION == 3`. Legacy list/detail operations may remain as administrative
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
{"v":3,"type":"hello","client":"turn-gui","client_version":"0.1.0"}
```

`accepts_encoding` may be included (`["base64"]`); an empty or absent list means
base64.

```jsonc
// daemon → UI
{"v":3,"type":"welcome","protocol_version":3,"min_protocol_version":3,
 "agreed_version":3,"daemon_version":"0.1.0","daemon_pid":51234,
 "daemon_started_ms":1700000000000,
 "limits":{"max_line_bytes":8388608,"max_output_chunk_bytes":262144,
           "max_screen_cells":65536,"max_image_pixels":1048576,
           "max_placed_images":8},
 "output_encoding":"base64"}
```

`daemon_pid` and `daemon_started_ms` are how a reconnecting UI tells "my socket
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
// daemon → UI — a daemon that has moved on to protocol 3..=4, client speaks 2
{"v":2,"type":"rejected","error":{
  "code":"unsupported_version",
  "message":"This Turn app is too old for the daemon it is talking to (app speaks protocol 2, daemon needs 3 or newer). Quit Turn and start it again to pick up the matching app",
  "detail":"client=2 supported=3..=4"}}
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
{"v":3,"type":"error","id":"r-9","error":{
  "code":"not_found","message":"No such session","detail":"sess_gone"}}

{"v":3,"type":"error","error":{
  "code":"malformed_message","message":"A message could not be understood"}}
```

| Code | Meaning | Retryable | Fatal to connection |
| --- | --- | --- | --- |
| `unsupported_version` | Version windows do not overlap | no | **yes** |
| `handshake_required` | A request arrived before `hello` | no | **yes** |
| `already_handshaked` | A second `hello` on one connection | no | no |
| `malformed_message` | Not valid JSON, or not a message this protocol defines | no | no |
| `line_too_long` | Over the frame limit; the line was discarded | no | no |
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
{"v":3,"type":"error","id":"r-lease","error":{
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
`HierarchyKey` is tagged `workspace`, `session` or `process`; a raw string is never accepted where the
kind matters.

### Unified hierarchy

| `op` | Fields | Answers with |
| --- | --- | --- |
| `get_hierarchy` | `surface_id`, `include_archived?` | `hierarchy` |
| `set_tree_expanded` | `surface_id`, `key: HierarchyKey`, `expanded` | `tree_state` |
| `select_tree_node` | `surface_id`, `selected: HierarchyKey?` | `tree_state` |
| `get_preview_history` | `session_id`, `node_id`, `limit?` (clamped to 20) | `preview_history` |
| `set_preview_visibility` | `session_id`, `node_id`, `visibility` | `ack` |
| `open_node_as_temporary_pane` | `surface_id`, `session_id`, `node_id` | `node_pane` |
| `focus_pane_for_node` | `surface_id`, `session_id`, `node_id` | `pane_focus` |
| `focus_pane_for_attention` | `surface_id`, `session_id`, `subject_node_id` | `pane_focus` |

`get_hierarchy` is navigation bootstrap. `list_workspaces`, `list_sessions`, `get_session` and
`get_process_tree` remain useful to administration, search and details, but composing them into a second
navigation tree is a client bug. `HierarchySnapshot.revision` is monotonic for the daemon lifetime; after a
revision gap or daemon identity change, request a full snapshot.

Expansion/selection writes are per stable `surface_id`. They are not `TurnEvent`s, do not change active
Session or Pane focus, and do not produce a broadcast. There is deliberately no unconstrained `move_node`.

`surface_id` is immutable for one connected client. The first `get_hierarchy` on a replacement connection
claims that surface, retires any older connection's surface ownership and removes its temporary Pane before
the snapshot is built. Permanent Layout bindings and tree expansion/selection remain. Temporary bindings
are also removed when their last client disconnects and when the daemon restarts; they are ephemeral view
state, not restorable process state.

`rename_node`, audited `correct_relationship` and tree visibility/filter mutations are accepted product
APIs but are **not protocol-v3 operations in this build**. A client must not send those operation names or
pretend a local rename changed daemon state. Their eventual contracts must verify the old edge, refuse
cycles and cross-Session moves, and record the user's correction at explicit confidence before this table
can list them as implemented.

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

### Workspaces

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_workspaces` | `include_archived?` | `workspaces` |
| `create_workspace` | `name`, `root` | `workspace` |
| `rename_workspace` | `workspace_id`, `name` | `workspace` |
| `archive_workspace` | `workspace_id`, `archived` | `workspace` |
| `duplicate_workspace` | `workspace_id`, `name?` — settings only, no sessions | `workspace` |
| `close_workspace` | `workspace_id`, `disposition` | `ack` |
| `delete_workspace` | `workspace_id`, `disposition` | `ack` |
| `get_workspace_write_lease` | `workspace_id` | `workspace_write_lease` |
| `acquire_workspace_write_lease` | `workspace_id`, `session_id`, `checkout_id` | `workspace_write_lease` |
| `release_workspace_write_lease` | `workspace_id`, `lease_id`, `expected_generation` | `workspace_write_lease` |

`archive_*` takes a flag rather than existing as two operations, so undo is the
same code path as do.

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
| `close_session` | `session_id`, `disposition` | `ack` |
| `delete_session` | `session_id`, `disposition` | `ack` |
| `get_session` | `session_id` | `session_details` |
| `get_process_tree` | `session_id` | `tree` |

`branch` and `task` fill `{branch}` and `{task}` in the template's name pattern.
`panes` is a list of `NewPane`; absent means a single shell.

`create_session` and `create_session_from_template` always request the primary checkout; callers cannot
smuggle a generic mode or checkout through either shape. Creating `main_checkout` persists the Session
assignment and acquires the lease in one atomic store
transaction, before init commands, processes or Panes exist. A conflict rolls the transaction back, returns
the typed context in §5 and leaves no partial Session. Duplicate never inherits an active lease; it must
choose/reconcile a mode. Read-only replies include `read_only_enforced`, because metadata and agent guidance
are not enforcement. Worktree replies include the new checkout and declared shared resources.

When the failed request was `create_session_from_template`, the client retains only its original Template
identity and interpolation inputs, then uses the matching `*_from_template` alternative. The daemon reloads
the authoritative Template and applies the same Layout, commands, relative cwd, environment, Attention,
tmux and name-pattern rules inside the selected safe checkout. A read-only alternative preserves that
configuration but launches no process while enforcement is unavailable; a worktree alternative remaps
absolute primary-checkout Session/Pane cwd values to the same repository-relative location in the isolated
checkout. Clients must not reconstruct a Template from `TemplateSummary`. The non-Template alternatives
remain the explicit blank/shell path.

### Templates

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_templates` | — | `templates` |
| `create_layout_template` | `name`, `layout`, `description?` | `template` |
| `save_layout_as_template` | `session_id`, `name`, `description?`, `hotkey?` | `template` |

Both creation paths strip process bindings: a template describes what to start, never which
instance it was captured from. `create_layout_template` is the visual editor path. Its bounded
Layout is validated and normalised by the daemon before persistence; the client-side Pane ids are
draft identity only, and every Session instantiation mints a fresh set.

### Panes

| `op` | Fields | Answers with |
| --- | --- | --- |
| `split_pane` | `session_id`, `pane_id`, `direction`, `pane` | `layout` |
| `close_pane` | `session_id`, `pane_id`, `disposition` | `layout` |
| `resize_pane` | `session_id`, `pane_id`, `delta` | `layout` |
| `resize_divider` | `session_id`, `before`, `after`, `delta` | `layout` |
| `equalize_divider` | `session_id`, `before`, `after` | `layout` |
| `apply_layout_preset` | `session_id`, `preset` | `layout` |
| `focus_pane` | `session_id`, `target` | `layout` |
| `relocate_pane` | `session_id`, `moved`, `target`, `zone` | `layout` |
| `swap_panes` | `session_id`, `a`, `b` — superseded by `relocate_pane` | `layout` |
| `zoom_pane` | `session_id`, `pane_id` — **toggles** | `layout` |
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

### Agent context handoff

| `op` | Fields | Answers with |
| --- | --- | --- |
| `prepare_context_handoff` | `session_id`, `source_node_id`, `target_node_id`, `instruction?` | `context_handoff` |
| `deliver_context_handoff` | `session_id`, `handoff_id` | `ack` |

This is a two-request, review-before-send capability. `prepare_context_handoff` verifies two distinct
agentic nodes in one active Session, assembles a bounded packet from the source's stable Activity Preview
history plus an optional user instruction, redacts secrets, and returns the **exact** text the daemon retains.
It never reads raw terminal scrollback and never writes a PTY. Hidden previews remain hidden.

The returned `handoff_id` is an opaque, short-lived capability bound to the preparing client, Session and
destination. `deliver_context_handoff` accepts only that id; a client cannot replace the reviewed body on
the delivery request. The daemon revalidates that the destination is an idle, controllable Agent with a
live Turn-owned PTY and no pending question or permission, then submits one bracketed paste. Closing or
opening panes is irrelevant and no layout operation is implied.

A successful same-connection retry is idempotent. A possibly partial PTY write is fenced as `conflict` and
is never replayed automatically. Drafts expire after ten minutes and are discarded when their client
disconnects. Success means the payload was submitted to the PTY; it is not proof that the Agent accepted or
acted on it. Handoffs deliberately cannot answer permission or question prompts; those remain explicit
`write_pty` input.

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
| `dismiss_attention` | `attention_id` | `ack` |
| `mute_session` | `session_id`, `until_ms?` (absent = unmute) | `ack` |
| `correct_state` | `session_id`, `node_id`, `lifecycle?`, `turn?`, `note?` | `node` |

`goto_attention` is a user-initiated move, so it bypasses the focus governor's
guards — pressing the shortcut is consent. It still resets the rate limiter so
automatic focus does not immediately fight manual navigation.

A muted session still badges. Muting silences the interruption, not the evidence.

```jsonc
// The user fixing a state Turn got wrong. Recorded with
// EventSource::UserCorrection at explicit confidence: on the question of what is
// actually happening in their terminal, the human outranks every heuristic.
{"v":3,"type":"request","id":"r-6","request":{
  "op":"correct_state","session_id":"sess_4b71e0","node_id":"proc_7a12ff",
  "turn":{"kind":"active"},"note":"still working"}}
```

### User activity

| `op` | Fields | Answers with |
| --- | --- | --- |
| `update_user_activity` | `context` | `effects` |

```jsonc
{"v":3,"type":"request","id":"r-4","request":{
  "op":"update_user_activity","context":{
    "last_keystroke_ms":1700000000000,"app_foreground":true,
    "active_session":"sess_4b71e0","sensitive_operation":false}}}
```

This is `turn_core::attention::UserContext`, sent as itself. It is what the focus
governor needs to decide whether it may move the user.

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
never auto-adopts that authority; release/reacquisition remains explicit.

### Examples

```jsonc
{"v":3,"type":"request","id":"r-1","request":{
  "op":"get_hierarchy","surface_id":"main-window","include_archived":false}}

// Cells, because the field is absent. What a renderer wants.
{"v":3,"type":"request","id":"r-2","request":{
  "op":"attach_pane","session_id":"sess_4b71e0","pane_id":"pane_11c3d8",
  "size":{"rows":40,"cols":120}}}

// The escape stream instead, for something that needs the bytes themselves.
{"v":3,"type":"request","id":"r-2b","request":{
  "op":"attach_pane","session_id":"sess_4b71e0","pane_id":"pane_11c3d8",
  "size":{"rows":40,"cols":120},"stream":"bytes"}}

// Answering an agent's y/n prompt. There is no "approve" request; this is it.
{"v":3,"type":"request","id":"r-3","request":{
  "op":"write_pty","session_id":"sess_4b71e0","node_id":"proc_7a12ff","data":"eQ0="}}

{"v":3,"type":"request","id":"r-5","request":{
  "op":"close_session","session_id":"sess_4b71e0","disposition":"keep_processes"}}

// Review first. This response writes no PTY.
{"v":3,"type":"request","id":"r-7","request":{
  "op":"prepare_context_handoff","session_id":"sess_4b71e0",
  "source_node_id":"proc_source","target_node_id":"proc_reviewer",
  "instruction":"Check the assumptions before continuing."}}

// After displaying the exact context_handoff.body and receiving explicit consent:
{"v":3,"type":"request","id":"r-8","request":{
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
| `tree_state` | `state: TreeSurfaceState` |
| `workspace_write_lease` | `workspace_id`, `lease?: WorkspaceWriteLease` |
| `sessions` | `sessions: [SessionSummary]` |
| `session` | `session: SessionSummary` |
| `session_details` | `details: SessionDetails` |
| `templates` | `templates: [TemplateSummary]` |
| `template` | `template: TemplateSummary` |
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
| `context_handoff` | `handoff: ContextHandoffView` — ids, safe labels, exact redacted `body`, fact count and redaction flag |

```jsonc
{"v":3,"type":"response","id":"r-3","response":{"result":"ack"}}
```

### `attached` — the feature made visible

```jsonc
{"v":3,"type":"response","id":"r-2","response":{"result":"attached","attachment":{
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "stream":"cells",
  "screen":{"rows":40,"cols":120,"cursor":[1,0],
            "runs":[[{"t":"ready","n":5,"f":[0,205,0]},{"n":115}],
                    [{"n":120}],"…"]},
  "size":{"rows":40,"cols":120},
  "scrollback_truncated":false,"bytes_seen":12,"next_seq":0}}}
```

This is what makes "processes survive UI restarts" a demonstrable feature rather than a
claim. The daemon held the pty the whole time; the screen it hands over reproduces the
pane exactly as the user left it.

Exactly one payload is present, decided by `stream`:

- **`screen`** for a cells attachment — a `Grid` (§2.2). `replay` is absent.
- **`replay`** for a byte attachment — the **parsed screen re-emitted**, not the raw
  scrollback, because a truncated raw ring can begin mid-escape-sequence and corrupt the
  receiving terminal. `screen` is absent.

Sending both would double the cost of every attach to serve a client that asked for one.

- `size` is the size the client asked for, applied to the pty *before* the screen or
  replay was taken.
- `scrollback_truncated` means output was dropped from the daemon's ring before this
  point. The screen is still correct; the history above it is incomplete, and the UI
  should say so rather than let the user scroll up into a lie.
- `next_seq` is the `seq` the next update for this attachment will carry — a
  `pane_screen` for cells, a `pane_output` for bytes — so a client can detect a gap
  between what it was handed and the live stream.
- `node_id` is absent for a pane with no process — an empty slot after a partial restore,
  or one of Turn's own views. A cells attachment to one still gets a `screen`: a blank
  grid at the client's size, because a renderer with nothing to draw is worse than one
  drawing an empty pane.

### `screen` — the answer to `resync_pane`

```jsonc
{"v":3,"type":"response","id":"r-7","response":{"result":"screen",
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
{"v":3,"type":"event","event":{"event":"pane_screen",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":312,"update":{
    "mode":"rows","size":{"rows":40,"cols":120},"cursor":[7,18],
    "rows":[{"row":6,"runs":[{"t":"$ cargo test","n":12},{"n":108}]},
            {"row":7,"runs":[{"n":120}]}]}}}

// The whole screen. Sent on resync, after a resize, and when a diff would not be
// smaller.
{"v":3,"type":"event","event":{"event":"pane_screen",
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
{"v":3,"type":"event","event":{"event":"pane_output",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":41,"data":"b2sNCg=="}}

{"v":3,"type":"event","event":{"event":"pane_output_gap",
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
{"v":3,"type":"event","event":{"event":"node_state_changed",
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
{"v":3,"type":"event","event":{"event":"turn_event_emitted","turn_event":{
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
{"v":3,"type":"event","event":{"event":"hierarchy_changed","snapshot":{
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
{"v":3,"type":"event","event":{"event":"restore_result",
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
- **Nothing here has been relaunched.** Every outcome retains its durable `node_id`; `can_relaunch: true` is
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

`tree_state` is `TreeSurfaceState { surface_id, selected?, expanded }`. Keys are tagged
`workspace`/`session`/`process`; expansion and selection from another surface are never merged into it.

Each `WorkspaceTreeView` contains `workspace`, `checkouts`, `write_lease?` and ordered `sessions`. Each
`SessionTreeView` contains `session` and ordered node rows. The daemon supplies parent/depth/order, derived
state and badges, relationship confidence, preview, bindings and capability; the GUI does not join separate
lists or infer missing values. A snapshot is a full replacement. `revision` rejects stale/out-of-order
delivery and tells a client when to resynchronise.

### `SessionSummary` — one Session projection

Identity: `id`, `workspace_id`, `name`, `note`, `cwd`, `status`
(`active`\|`paused`\|`archived`).

Checkout safety: `mode` (`main_checkout`\|`read_only`\|`isolated_worktree`), `checkout_id?`,
`worktree_path?`, `read_only_enforced`. A read-only badge must not imply technical enforcement when the last
field is false.

Derived state — **the client renders these, it never computes them**:

| Field | From |
| --- | --- |
| `display_state` | `DisplayState::derive` over the session's process tree |
| `state_label` | the display-state label, except outstanding Attention promotes the Session row to `"YOUR TURN"` |
| `severity` | Ranking weight, so a client sorting locally sorts as the daemon would |
| `needs_user` | Whether the runtime tree itself is blocked on the human; `badge_count` independently exposes exact or scoped Attention |

Counts: `subagent_count`, `running_count`, `node_count`, `pane_count`. Subagents
and running processes are counted separately because "the agent finished its turn"
and "nothing is running any more" are different claims.

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

Identity: `kind`, `is_agentic`, `title`, `command`, `args`, `cwd`, `pid`, `ppid`.

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

`scrollback_offset` and `scrollback_len` are reported as 0 today: the daemon serves the
live screen only, and there is no request that asks for history. A client should treat
them as "no history offered" rather than as "no history exists".

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
2. **Turn never approves a permission.** No request says so — not
   `approve_permission`, not `respond_permission`. Pending questions and permissions
   are answered only through `write_pty`: the human typing. A context handoff is
   refused while the destination has a pending interaction.
3. **Turn never runs a command it inferred from agent output.** There is no "run
   this" verb. Processes start from a template, a pane definition, or
   `relaunch_node`, all of which the user chose.
4. **Turn never relaunches on its own.** `relaunch_node` is the only path back, and
   it always originates with a human. `restore_result` reports and offers.
5. **A client cannot request an unarbitrated primary-checkout writer.** Session mode is closed, creation
   goes through daemon lease arbitration, and conflict recovery is one of the typed alternatives. There is
   no force/steal flag; generation mismatch is a conflict, not a retry loop.
6. **Navigation cannot fabricate ownership.** There is no unconstrained `move_node`, no client-supplied
   confidence promotion and no `tree.node_selected` domain event. Audited, cycle-checked relationship
   correction is planned; protocol v3 refuses to approximate it with a local-only mutation.

One more, on the transport, which this crate does not implement but assumes:
`$SOCKET` is owner-only, and the hook server binds `127.0.0.1` with a per-node
token. Never `0.0.0.0`.

---

## 11. Implementing a client

1. Connect to `$SOCKET`. Send `hello` as the first frame.
2. Read frames with a decoder matching §1. On `rejected`, show
   `error.message` and stop — do not retry.
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
6. Forward keystrokes as `write_pty` and window resizes as `resize_pty`. Pipeline
   freely.
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

*Protocol version 3. The Rust request/response catalogue is authoritative for the exact variant count;
this document is authoritative for hierarchy, checkout-safety, revision and recovery semantics.*
