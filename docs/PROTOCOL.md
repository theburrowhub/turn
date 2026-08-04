# The Turn daemon protocol

`turn-proto` version **2**.

This is the contract between `turnd` — which owns every pty, all state and the
attention manager — and a UI client, which renders and forwards keystrokes.

Every message shown below was produced by serialising the real types; the field
sets are exact, not illustrative. The source of truth is
`crates/turn-proto/src/`, and tests in that crate assert that the operation names,
the request-to-response pairing and the catalogue sizes documented here match the
code.

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
thumbnails and the output heuristics work with no client attached, so a `vt100` screen
per pane exists whether or not anybody is looking. Sending that screen means there is
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

### 2.3 What is *not* sanitised

Screen cells are the terminal's own contents, passed through as the program wrote them.
That is the opposite of the rule for **labels** — a pane title or a thumbnail line is
stripped of control, bidirectional and invisible characters, because those end up in
Turn's chrome where text could lie about itself. Inside a pane the client paints cell by
cell, so no cell can reorder its neighbours, and filtering would mean a terminal that
does not show what the program printed.

---

## 3. Envelope

Every frame, in both directions, carries `v` — the protocol version it is written
against — alongside a `type` discriminator.

```jsonc
{"v": 2, "type": "request", "id": "r-1", "request": {"op": "..."}}
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
{"v":2,"type":"hello","client":"turn-gui","client_version":"0.1.0"}
```

`accepts_encoding` may be included (`["base64"]`); an empty or absent list means
base64.

```jsonc
// daemon → UI
{"v":2,"type":"welcome","protocol_version":2,"min_protocol_version":2,
 "agreed_version":2,"daemon_version":"0.1.0","daemon_pid":51234,
 "daemon_started_ms":1700000000000,
 "limits":{"max_line_bytes":8388608,"max_output_chunk_bytes":262144,
           "max_screen_cells":65536},
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
{"v":2,"type":"error","id":"r-9","error":{
  "code":"not_found","message":"No such session","detail":"sess_gone"}}

{"v":2,"type":"error","error":{
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

---

## 6. Requests — UI → daemon

> **Protocol v3 upgrade in progress (ADR-040).** The canonical navigation surface is a unified Workspace
> hierarchy, not independently rendered Workspace, Session and process lists. New Tree, Preview, Pane
> binding and Lease operations are specified in `UNIFIED_HIERARCHY_UPGRADE.md`. Until the v3 types land,
> the v2 endpoints below describe the legacy wire contract, not the accepted product navigation model.

41 operations, tagged `op`. Every request carries a client-supplied `id`; the
daemon echoes it untouched. Ids are client-supplied so the UI can key its pending
map on something it already has, without a round trip to learn the key.

`session_id`, `workspace_id`, `pane_id`, `node_id`, `template_id` and
`attention_id` are the prefixed string ids from `turn_core::ids`.

### Workspaces

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_workspaces` | `include_archived?` | `workspaces` |
| `create_workspace` | `name`, `root` | `workspace` |
| `rename_workspace` | `workspace_id`, `name` | `workspace` |
| `archive_workspace` | `workspace_id`, `archived` | `workspace` |
| `duplicate_workspace` | `workspace_id`, `name?` — settings only, no sessions | `workspace` |
| `close_workspace` | `workspace_id`, `disposition` | `ack` |

`archive_*` takes a flag rather than existing as two operations, so undo is the
same code path as do.

### Sessions

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_sessions` | `workspace_id?` (absent = all), `include_archived?` | `sessions` |
| `create_session` | `workspace_id`, `name`, `cwd?`, `panes?`, `note?`, `tags?` | `session` |
| `create_session_from_template` | `workspace_id`, `template_id`, `name?`, `cwd?`, `branch?`, `task?` | `session` |
| `rename_session` | `session_id`, `name` | `session` |
| `archive_session` | `session_id`, `archived` | `session` |
| `duplicate_session` | `session_id` — same shape, new identity, no processes | `session` |
| `close_session` | `session_id`, `disposition` | `ack` |
| `get_session` | `session_id` | `session_details` |
| `get_process_tree` | `session_id` | `tree` |

`branch` and `task` fill `{branch}` and `{task}` in the template's name pattern.
`panes` is a list of `NewPane`; absent means a single shell.

### Templates

| `op` | Fields | Answers with |
| --- | --- | --- |
| `list_templates` | — | `templates` |
| `save_layout_as_template` | `session_id`, `name`, `description?`, `hotkey?` | `template` |

Saving strips process bindings: a template describes what to start, never which
instance it was captured from.

### Panes

| `op` | Fields | Answers with |
| --- | --- | --- |
| `split_pane` | `session_id`, `pane_id`, `direction`, `pane` | `layout` |
| `close_pane` | `session_id`, `pane_id`, `disposition` | `layout` |
| `resize_pane` | `session_id`, `pane_id`, `delta` | `layout` |
| `focus_pane` | `session_id`, `target` | `layout` |
| `swap_panes` | `session_id`, `a`, `b` | `layout` |
| `zoom_pane` | `session_id`, `pane_id` — **toggles** | `layout` |
| `attach_pane` | `session_id`, `pane_id`, `size`, `stream?` | `attached` |
| `resync_pane` | `session_id`, `pane_id` | `screen` |
| `detach_pane` | `session_id`, `pane_id` | `ack` |

Every pane operation answers with the resulting `layout` rather than an ack, so
the UI renders the daemon's arrangement instead of its own optimistic guess at what
a split, a collapse or a clamped resize did.

- `direction`: `"horizontal"` \| `"vertical"`
- `target`: `{"kind":"pane","pane_id":…}` \| `{"kind":"next"}` \| `{"kind":"previous"}`
- `delta`: fraction of the parent split, positive to grow. Clamped so no pane can
  be resized out of existence.
- `zoom_pane` leaves the layout tree untouched, so un-zooming restores the exact
  previous geometry.
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

### PTY

| `op` | Fields | Answers with |
| --- | --- | --- |
| `write_pty` | `session_id`, `node_id`, `data` (base64) | `ack` |
| `resize_pty` | `session_id`, `node_id`, `size` | `ack` |

Addressed to the **node**, not the pane: the pty belongs to the process, and one
process may be shown in more than one place.

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
{"v":2,"type":"request","id":"r-6","request":{
  "op":"correct_state","session_id":"sess_4b71e0","node_id":"proc_7a12ff",
  "turn":{"kind":"active"},"note":"still working"}}
```

### User activity

| `op` | Fields | Answers with |
| --- | --- | --- |
| `update_user_activity` | `context` | `effects` |

```jsonc
{"v":2,"type":"request","id":"r-4","request":{
  "op":"update_user_activity","context":{
    "last_keystroke_ms":1700000000000,"app_foreground":true,
    "active_session":"sess_4b71e0","sensitive_operation":false}}}
```

This is `turn_core::attention::UserContext`, sent as itself. It is what the focus
governor needs to decide whether it may move the user.

- Send on a **change**, not on a timer. The interesting transitions are the first
  keystroke of a burst, the window losing focus, and a modal opening.
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

### Examples

```jsonc
{"v":2,"type":"request","id":"r-1","request":{
  "op":"list_sessions","workspace_id":"ws_9f2a1c","include_archived":false}}

// Cells, because the field is absent. What a renderer wants.
{"v":2,"type":"request","id":"r-2","request":{
  "op":"attach_pane","session_id":"sess_4b71e0","pane_id":"pane_11c3d8",
  "size":{"rows":40,"cols":120}}}

// The escape stream instead, for something that needs the bytes themselves.
{"v":2,"type":"request","id":"r-2b","request":{
  "op":"attach_pane","session_id":"sess_4b71e0","pane_id":"pane_11c3d8",
  "size":{"rows":40,"cols":120},"stream":"bytes"}}

// Answering an agent's y/n prompt. There is no "approve" request; this is it.
{"v":2,"type":"request","id":"r-3","request":{
  "op":"write_pty","session_id":"sess_4b71e0","node_id":"proc_7a12ff","data":"eQ0="}}

{"v":2,"type":"request","id":"r-5","request":{
  "op":"close_session","session_id":"sess_4b71e0","disposition":"keep_processes"}}
```

---

## 7. Responses — daemon → UI

16 result shapes, tagged `result`. Each request names exactly one
(`Request::expected_result`), and a test asserts that every name it produces exists
in this catalogue, so the pairing above is load-bearing rather than documentation
that might be stale. Failures never arrive as a response; they arrive as an
`error` frame (§5).

| `result` | Payload |
| --- | --- |
| `ack` | — |
| `workspaces` | `workspaces: [WorkspaceSummary]` |
| `workspace` | `workspace: WorkspaceSummary` |
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
| `attention` | `entry?: AttentionView` — absent when the queue is empty |
| `attention_list` | `entries: [AttentionView]` |
| `effects` | `effects: [Effect]` |

```jsonc
{"v":2,"type":"response","id":"r-3","response":{"result":"ack"}}
```

### `attached` — the feature made visible

```jsonc
{"v":2,"type":"response","id":"r-2","response":{"result":"attached","attachment":{
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
{"v":2,"type":"response","id":"r-7","response":{"result":"screen",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "next_seq":312,"grid":{"rows":40,"cols":120,"…":"…"}}}
```

The same `Grid` an attach would return, so a client's recovery path and its first-render
path are one piece of code. `next_seq` is the sequence number the next `pane_screen` will
carry, and the grid is the state as of just before it — the daemon answers with the exact
screen its next diff is computed against, not with a fresher read of the pty. A fresher
one would look more helpful and be wrong: a row that changed and changed back in between
would never be corrected.

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

13 pushes, tagged `event`, wrapped in `{"type":"event","event":{…}}`. They carry no
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
| `layout_changed` | `session_id`, `layout` |
| `pty_resized` | `session_id`, `node_id`, `size` |
| `restore_result` | `session_id`, `state`, `needs_explanation`, `panes` |

`session_state_changed`, `tree_changed` and `layout_changed` send the whole thing
rather than a diff. Each is small, and a client applying diffs to a stale copy is a
class of bug that shows up as a sidebar disagreeing with the terminal — or, for the
tree, as an invented parent link.

### 8.1 The screen: `pane_screen`

The default terminal push. It carries **what changed**, in one of two shapes, tagged
`mode`.

```jsonc
// The rows that differ. The everyday case.
{"v":2,"type":"event","event":{"event":"pane_screen",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":312,"update":{
    "mode":"rows","size":{"rows":40,"cols":120},"cursor":[7,18],
    "rows":[{"row":6,"runs":[{"t":"$ cargo test","n":12},{"n":108}]},
            {"row":7,"runs":[{"n":120}]}]}}}

// The whole screen. Sent on resync, after a resize, and when a diff would not be
// smaller.
{"v":2,"type":"event","event":{"event":"pane_screen",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":313,"update":{"mode":"full","grid":{"rows":40,"cols":120,"…":"…"}}}}
```

**Applying a `rows` update**: replace each named row's cells outright, then take
`cursor` and `alternate_screen` from the update. Rows are whole — there is no partial-row
addressing, so a client can never leave a row half written. A row is `runs` in exactly
the grid encoding of §2.2.

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
{"v":2,"type":"event","event":{"event":"pane_output",
  "session_id":"sess_4b71e0","pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
  "seq":41,"data":"b2sNCg=="}}

{"v":2,"type":"event","event":{"event":"pane_output_gap",
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
{"v":2,"type":"event","event":{"event":"node_state_changed",
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
{"v":2,"type":"event","event":{"event":"turn_event_emitted","turn_event":{
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

### The tree

```jsonc
{"v":2,"type":"event","event":{"event":"tree_changed",
  "session_id":"sess_4b71e0","nodes":[
  {"node_id":"proc_7a12ff","parent":null,"relation":"unknown",
   "relation_is_provisional":true,"depth":0,"child_count":1,
   "kind":"agent","is_agentic":true,"title":"claude","command":"claude",
   "lifecycle":{"kind":"alive"},"turn":{"kind":"active"},
   "display_state":"running","state_label":"running","severity":20,
   "needs_user":false,"runtime_ms":3000, "…":"…"},
  {"node_id":"proc_e5c308","parent":"proc_7a12ff","relation":"confirmed",
   "relation_is_provisional":false,"depth":1,"child_count":0,
   "kind":"subagent","is_agentic":true,"title":"explore", "…":"…"}]}}
```

(Elided fields are listed in §9; the wire form carries all of them.)

This is the push a subagent appearing produces. `relation` and
`relation_is_provisional` are the point: Claude Code's `SubagentStart` hook gives a
**confirmed** edge, a pid whose ppid happens to match gives an **inferred** one, and
the UI must draw the second differently. Anything Turn cannot place stays
`unknown`, renders at `depth: 0`, and is never hidden.

### Restore

```jsonc
{"v":2,"type":"event","event":{"event":"restore_result",
  "session_id":"sess_4b71e0","state":"partially_restored","needs_explanation":true,
  "panes":[
    {"pane_id":"pane_11c3d8","node_id":"proc_7a12ff",
     "lifecycle":{"kind":"reconnected"},"can_relaunch":false},
    {"pane_id":"pane_66ba04","lifecycle":{"kind":"lost"},
     "can_relaunch":true,"command":"cargo watch -x test"}]}}
```

Pushed rather than answered, because a restore happens when the daemon decides — on
its own start, or when it re-adopts processes — and the UI may not have asked
anything yet.

- `state`: `live` \| `reattached` \| `partially_restored` \| `layout_only`
- `needs_explanation` is true when the user must be told, rather than left to
  notice a dead pane.
- `lifecycle`: `reconnected` (the pty survived and Turn owns it again), `orphaned`
  (alive but out of reach), `lost` (was running, cannot be found).
- **Nothing here has been relaunched.** `can_relaunch: true` with `node_id` absent
  is an *offer*: there is no process to point at. `command` is shown verbatim so
  accepting is an informed choice. The user answers with `relaunch_node` or does
  not, and nothing happens until they do.

---

## 9. View models

The daemon owns every product rule. If the client derived `display_state` itself,
or decided whether a parent link is a guess, or worked out which of thirty sessions
is shouting loudest, those rules would exist twice — and the second copy would be
written in TypeScript by someone reading a screenshot.

Two rules the projections keep: turn-core types are **embedded** rather than
re-described, and extra fields are strictly *derived* values a client would
otherwise need a copy of the rules to compute.

### `SessionSummary` — one sidebar row

Identity: `id`, `workspace_id`, `name`, `note`, `cwd`, `status`
(`active`\|`paused`\|`archived`).

Derived state — **the client renders these, it never computes them**:

| Field | From |
| --- | --- |
| `display_state` | `DisplayState::derive` over the session's process tree |
| `state_label` | `"YOUR TURN"`, `"PERMISSION"`, `"QUESTION"`, `"running"`, `"failed"`, … |
| `severity` | Ranking weight, so a client sorting locally sorts as the daemon would |
| `needs_user` | Whether anything here is blocked on the human |

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

### `TreeNodeView` — one row of the process tree

Flat with a `depth` rather than nested, for the same reason `SessionTree` stores
parent pointers: the shape changes as subagents come and go, and re-rendering a
list is cheaper and less error-prone than diffing a recursive structure. Rows
arrive in draw order — each root followed by its subtree, depth-first, siblings in
insertion order.

Placement: `node_id`, `session_id`, `parent`, `relation`
(`confirmed`\|`inferred`\|`unknown`), **`relation_is_provisional`**, `depth`,
`child_count`.

Identity: `kind`, `is_agentic`, `title`, `command`, `args`, `cwd`, `pid`, `ppid`.

State: `lifecycle`, `turn` (absent for a non-agent), `display_state`,
`state_label`, `severity`, `needs_user`, `interaction_pending`.

Placement and timing: `pane_id`, `started_ms`, `ended_ms`, `runtime_ms`
(freezes at the exit), `exit_code`.

`agent`: an `AgentSummary`, for agentic nodes.

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

### `SessionDetails`

`summary`, `layout` (the domain `Layout`), `tree` (`[TreeNodeView]`), `attention`
(the `AttentionPolicy` in force), `env`.

### `AttentionView`

`entry` is the whole `turn_core::attention::AttentionEntry` — id, session, node,
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

### `WorkspaceSummary`

`id`, `name`, `root`, `git_remote`, `colour`, `icon`, `archived`,
`session_count`, `sessions_needing_user`, `badge_count`, `default_agent`,
`default_shell`, `default_template`, `tmux_enabled`, `created_ms`, `last_used_ms`.

The counts are the sum of the workspace's `SessionSummary` values, so a workspace
badge and its session badges can never disagree.

### `TemplateSummary`

`id`, `name`, `description`, `icon`, `hotkey`, `built_in`, `pane_count`,
`commands`, `name_pattern`, `tmux`, `created_ms`. `commands` lists what the
template would start, in pane order — materialising a template launches processes,
and choosing one should be an informed decision.

---

## 10. What the protocol refuses to express

Four product guarantees appear here as **absences**, which is the strongest
enforcement a type definition can offer. A future request matching these
descriptions has to argue with a test first.

1. **A heuristic can never move the user.** Focus is not something a client is told
   to do directly; it arrives as an `Effect` the attention manager already cleared
   through the focus governor. `EventSource::PtyHeuristic` caps `Confidence` at
   `inferred_high`, and `AttentionPolicy::resolve` degrades any focus action from a
   provisional event to a badge.
2. **Turn never approves a permission.** No request says so — not
   `approve_permission`, not `respond_permission`. Answering an agent is
   `write_pty`: the human typing.
3. **Turn never runs a command it inferred from agent output.** There is no "run
   this" verb. Processes start from a template, a pane definition, or
   `relaunch_node`, all of which the user chose.
4. **Turn never relaunches on its own.** `relaunch_node` is the only path back, and
   it always originates with a human. `restore_result` reports and offers.

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
4. `list_workspaces`, `list_sessions`, `get_session`. Render from the view models;
   compute nothing the daemon already computed.
5. `attach_pane` for each visible pane. Draw `screen`, then apply each `pane_screen`
   in `seq` order (§8.1). On a `seq` jump, `resync_pane`. A `rows` update whose `size`
   is not what you are rendering means you missed a resize: resync.
   Ask for `stream: "bytes"` only if you need the escape stream itself, and then feed
   `replay` into your emulator and apply `pane_output`, re-attaching on
   `pane_output_gap`.
6. Forward keystrokes as `write_pty` and window resizes as `resize_pty`. Pipeline
   freely.
7. Send `update_user_activity` on activity transitions, not on a timer.
8. Handle every push in §8. Treat each as the current truth about what it names.
9. On a decode error, reply with an `error` frame built from
   `FrameError::to_proto_error()` and **keep the connection**.
10. On reconnect, re-handshake and compare `daemon_pid`: unchanged means your
    processes are still there and re-attaching restores them.

---

*Protocol version 2. Catalogue: 41 requests, 16 response shapes, 13 pushes, 13
error codes.*
