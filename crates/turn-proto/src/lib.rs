//! # turn-proto
//!
//! The contract between `turnd` and a UI client.
//!
//! The daemon owns every pty, all state and the attention manager. The client
//! renders and forwards keystrokes. That division is what makes processes survive a
//! UI restart — the pty never belonged to the window — and it only holds if the
//! boundary is described precisely enough that a second frontend could be written
//! against it. This crate is that description: types, framing and nothing else. It
//! has no I/O, no tokio, no socket.
//!
//! ## The shape of a connection
//!
//! ```text
//! UI                                             turnd
//!  │  {"v":3,"type":"hello",…}                      │
//!  │ ─────────────────────────────────────────────► │
//!  │                    {"v":3,"type":"welcome",…}  │   negotiate()
//!  │ ◄───────────────────────────────────────────── │
//!  │  {"v":3,"type":"request","id":"r-1",…}         │
//!  │ ─────────────────────────────────────────────► │
//!  │                   {"v":3,"type":"response",…}  │   correlated by id
//!  │ ◄───────────────────────────────────────────── │
//!  │                      {"v":3,"type":"event",…}  │   unsolicited, any time
//!  │ ◄───────────────────────────────────────────── │
//! ```
//!
//! * [`ClientFrame`] / [`ServerFrame`] — the versioned envelope ([`envelope`]).
//! * [`Request`] — what the UI may ask for ([`request`]).
//! * [`Response`] and [`ProtoError`] — one typed result per request, one error
//!   shape ([`response`], [`error`]).
//! * [`ServerEvent`] — what the daemon pushes without being asked ([`events`]).
//! * [`view`] — projections that keep product rules out of the client.
//! * [`cells`] — a pane's screen as cells, and the compact form it travels in.
//! * [`screen`] — screen diffs, the sequence rule, and which stream a pane carries.
//! * [`framing`] — newline-delimited JSON, robust to partial reads and bad lines.
//! * [`bytes`] — binary payloads, and an honest note about what base64 costs.
//!
//! ## A terminal is cells, not bytes
//!
//! The daemon keeps an authoritative `vt100`-parsed screen for every PTY-backed runtime
//! node — it has to, because on-demand previews and output heuristics work with no client
//! attached. A bound pane's screen therefore crosses this boundary already parsed:
//! [`Grid`] of [`Cell`], with
//! palette indices already resolved to concrete [`Rgb`]. There is one VT emulator in
//! the system, which removes the entire class of bug where the daemon's screen and
//! the client's disagree.
//!
//! The byte stream is still here, because some things genuinely need it — capturing a
//! log, a client that has its own emulator — but a client gets cells unless it asks
//! for [`PaneStream::Bytes`].
//!
//! ## Four rules the protocol enforces by omission
//!
//! Some of the product's guarantees are visible here as things that are *absent*,
//! which is the strongest form of enforcement available to a type definition:
//!
//! 1. **A heuristic cannot move the user.** Focus is never something a client is
//!    told to do directly; it arrives as an [`Effect`](turn_core::Effect) the
//!    attention manager already cleared through the focus governor, and
//!    [`Confidence`](turn_core::Confidence) travels with every event so a guess
//!    stays a guess.
//! 2. **Turn never approves a permission.** No request says so. Pending questions
//!    and permissions are answered only by [`Request::WritePty`] — the human
//!    typing. A context handoff can target only an idle or done Agent.
//! 3. **Turn never runs a command it inferred.** Processes start from a template, a
//!    pane definition, or [`Request::RelaunchNode`]. There is no "run this" verb.
//! 4. **Turn never relaunches on its own.** A restore *reports* what it found and
//!    marks what could be started again; the client turns that into an offer.
//!
//! ## Compatibility
//!
//! Nothing here uses `deny_unknown_fields`, deliberately: a newer daemon may add a
//! field and an older client must ignore it rather than fail. Changes that would
//! make an older client *misread* a message bump [`PROTOCOL_VERSION`], and the
//! handshake refuses the connection instead of letting it half work.

pub mod bytes;
pub mod cells;
#[cfg(test)]
mod contract;
pub mod envelope;
pub mod error;
pub mod events;
pub mod framing;
pub mod geometry;
pub mod request;
pub mod response;
pub mod screen;
pub mod view;

pub use bytes::{decode_base64, encode_base64, Base64Error, TerminalBytes};
#[cfg(feature = "vt100")]
pub use cells::from_screen;
pub use cells::{
    indexed_rgb, Cell, CellAttrs, CellRun, Grid, GridError, Modes, MouseMode, Rgb, MAX_SCREEN_CELLS,
};
pub use envelope::{
    negotiate, negotiate_within, peek_version, version_refusal, ClientFrame, ClientMessage, Hello,
    Limits, OutputEncoding, ServerFrame, ServerMessage, Welcome, MIN_PROTOCOL_VERSION,
    PROTOCOL_VERSION,
};
pub use error::{
    ErrorCode, ProtoError, ProtoErrorContext, SessionConflictAlternative, WriteLeaseOwnerView,
};
pub use events::{PaneRestoreOutcome, ServerEvent};
pub use framing::{
    encode, encode_checked, encode_into, FrameError, LineDecoder, MAX_LINE_BYTES,
    MAX_OUTPUT_CHUNK_BYTES,
};
pub use geometry::PtySize;
pub use request::{CloseDisposition, FocusTarget, NewPane, Request, RequestId};
pub use response::{PaneAttachment, Response};
pub use screen::{GridRow, PaneStream, ScreenUpdate};
pub use view::{
    AgentSummary, AttentionView, ContextHandoffText, ContextHandoffView, HierarchyKey,
    HierarchySnapshot, NodePaneCapability, NodePaneView, PaneFocusView, SessionDetails,
    SessionSummary, SessionTreeView, TemplateSummary, TreeNodeView, TreeSurfaceState,
    WorkspaceSummary, WorkspaceTreeView,
};
