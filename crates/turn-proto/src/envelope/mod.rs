//! The envelope every frame is wrapped in, and the handshake that opens a
//! connection.
//!
//! ## Why the version is on every frame
//!
//! It would be enough to negotiate once and trust the connection afterwards. The
//! version rides on every frame anyway, for two reasons that cost a handful of
//! bytes each. A frame captured out of a log or a bug report is
//! self-describing — you can tell what dialect it is without the handshake that
//! preceded it. And a peer that reconnects with different code, which is exactly
//! what happens while developing the UI against a running daemon, is caught by
//! [`ClientFrame::expect_version`] instead of misbehaving quietly.
//!
//! ## The failure this prevents
//!
//! A stale UI attached to a newer daemon is the dangerous case, because the
//! symptoms are subtle: an unrecognised enum variant deserialises as an error the
//! UI reports as "unknown", a renamed field arrives as `None`, and the user sees a
//! terminal that looks fine while the sidebar quietly lies about what is running.
//! Refusing the connection outright, with a message that says which side is old,
//! is strictly better than a UI that half works.

use serde::{Deserialize, Serialize};

use crate::cells::MAX_SCREEN_CELLS;
use crate::error::{ErrorCode, ProtoError};
use crate::events::ServerEvent;
use crate::framing::{MAX_LINE_BYTES, MAX_OUTPUT_CHUNK_BYTES};
use crate::request::{Request, RequestId};
use crate::response::Response;

/// The protocol version this build speaks.
///
/// Bumped whenever a change would make an older client misread a message:
/// removing or renaming a field, changing a field's meaning, removing a variant.
/// *Adding* a request, a response variant, a push or an optional field does not
/// bump it — nothing here uses `deny_unknown_fields`, so an older client ignores
/// what it does not know and a newer client tolerates a daemon that omits it.
///
/// **3** replaces the singular reverse Pane pointer with authoritative
/// `pane_bindings` and adds the unified Workspace hierarchy/lease contracts. A
/// v2 client would silently misread a live node's view bindings, so this is
/// deliberately not an additive rollout.
///
/// **2** is the version where a pane's screen became cells. `attach_pane` gained a
/// `stream` field whose default is [`OutputEncoding`]-independent and is
/// [`crate::PaneStream::Cells`], so a version 1 client — which omits the field and
/// expects `pane_output` bytes — would attach and then be sent `pane_screen` frames
/// it has no code for. That is a change of meaning for an existing request rather
/// than an addition, which is exactly what this constant is for.
pub const PROTOCOL_VERSION: u32 = 3;

/// The oldest version this build still accepts from a peer.
///
/// Kept separate from [`PROTOCOL_VERSION`] so a daemon can support a window of
/// versions during a rollout rather than requiring both sides to move at once. The
/// window is a single version at the moment because both v1→v2 and v2→v3 changed
/// the meaning of existing fields. Half-compatible supervision is less safe than a
/// loud upgrade requirement.
pub const MIN_PROTOCOL_VERSION: u32 = 3;

/// How binary payloads are carried.
///
/// Present in the handshake from version 1 so a future length-prefixed side
/// channel can be introduced by agreement rather than by a protocol break. A
/// client asks for what it can handle; the daemon answers with what will actually
/// be used, and a client must honour the answer rather than its own request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputEncoding {
    /// Standard base64 inside JSON strings. Costs 33% in size — see
    /// [`crate::bytes`].
    #[default]
    Base64,
}

/// A client introducing itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Hello {
    /// What the client is: `"turn-ui"`, `"turn-cli"`, someone's script. Used for
    /// logging and for telling two attached clients apart, never for access
    /// control.
    pub client: String,
    /// The client's own release version, which is what a support message quotes.
    pub client_version: String,
    /// Encodings the client can decode, best first. An empty list means base64.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepts_encoding: Vec<OutputEncoding>,
}

impl Hello {
    pub fn new(client: impl Into<String>, client_version: impl Into<String>) -> Self {
        Self {
            client: client.into(),
            client_version: client_version.into(),
            accepts_encoding: Vec::new(),
        }
    }
}

/// Limits a client must respect. Sent rather than assumed, so a client built
/// against a different build does not have to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Limits {
    pub max_line_bytes: usize,
    /// Raw bytes per output frame before the daemon splits it.
    pub max_output_chunk_bytes: usize,
    /// Most cells a pane's screen may have, `rows * cols`.
    ///
    /// Announced rather than assumed because it is a limit a client can *hit*: it
    /// decides the largest geometry `attach_pane` will accept, so a client can clamp
    /// its own layout instead of discovering the refusal after the user resized the
    /// window. Defaulted on the way in so a daemon that predates the field is read as
    /// meaning the same number rather than as meaning zero.
    #[serde(default = "default_max_screen_cells")]
    pub max_screen_cells: usize,
    /// Most pixels one inline image may carry.
    ///
    /// Announced for the same reason as the cell limit: it is one a client can hit — a
    /// `pane_image` payload is checked against it on the way in — and it is how a client
    /// knows how much memory to be ready for per picture. Defaulted so a daemon that
    /// predates the field reads as meaning the number rather than as meaning zero.
    #[serde(default = "default_max_image_pixels")]
    pub max_image_pixels: u32,
    /// Most inline images one screen may place at a time.
    #[serde(default = "default_max_placed_images")]
    pub max_placed_images: usize,
}

fn default_max_screen_cells() -> usize {
    MAX_SCREEN_CELLS
}

fn default_max_image_pixels() -> u32 {
    crate::images::MAX_IMAGE_PIXELS
}

fn default_max_placed_images() -> usize {
    crate::images::MAX_PLACED_IMAGES
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_line_bytes: MAX_LINE_BYTES,
            max_output_chunk_bytes: MAX_OUTPUT_CHUNK_BYTES,
            max_screen_cells: MAX_SCREEN_CELLS,
            max_image_pixels: crate::images::MAX_IMAGE_PIXELS,
            max_placed_images: crate::images::MAX_PLACED_IMAGES,
        }
    }
}

/// The daemon accepting a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Welcome {
    /// The newest version this daemon speaks.
    pub protocol_version: u32,
    /// The oldest it accepts.
    pub min_protocol_version: u32,
    /// The version this connection will use. Both sides send this from here on.
    pub agreed_version: u32,
    pub daemon_version: String,
    /// So a UI can tell "the daemon restarted" from "my socket hiccupped", which
    /// decides whether it must re-attach every pane.
    pub daemon_pid: u32,
    pub daemon_started_ms: i64,
    pub limits: Limits,
    /// The encoding that will actually be used.
    pub output_encoding: OutputEncoding,
}

impl Welcome {
    pub fn new(
        agreed_version: u32,
        daemon_version: impl Into<String>,
        daemon_pid: u32,
        daemon_started_ms: i64,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            min_protocol_version: MIN_PROTOCOL_VERSION,
            agreed_version,
            daemon_version: daemon_version.into(),
            daemon_pid,
            daemon_started_ms,
            limits: Limits::default(),
            output_encoding: OutputEncoding::Base64,
        }
    }
}

/// Decides whether a client speaking `client_version` may proceed.
///
/// Returns the version the connection will use, or the refusal to send back. The
/// message names both sides and says which one to update, because "protocol
/// mismatch" alone leaves the user with nothing to do about it.
pub fn negotiate(client_version: u32) -> Result<u32, ProtoError> {
    negotiate_within(client_version, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION)
}

/// The negotiation rule, with the supported window as parameters.
///
/// Split out so the window logic is testable at versions this build does not
/// happen to have — the interesting cases only exist once the protocol has moved
/// on, and they are exactly the cases that must not be got wrong later.
pub fn negotiate_within(
    client_version: u32,
    min_supported: u32,
    max_supported: u32,
) -> Result<u32, ProtoError> {
    debug_assert!(
        min_supported <= max_supported,
        "the supported window is inverted"
    );

    if client_version < min_supported {
        return Err(ProtoError::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "This Turn app is too old for the daemon it is talking to \
                 (app speaks protocol {client_version}, daemon needs \
                 {min_supported} or newer). Quit Turn and start it again to pick \
                 up the matching app"
            ),
        )
        .with_detail(format!(
            "client={client_version} supported={min_supported}..={max_supported}"
        )));
    }

    if client_version > max_supported {
        return Err(ProtoError::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "The running Turn daemon is older than this app \
                 (app speaks protocol {client_version}, daemon speaks up to \
                 {max_supported}). Stop the daemon so Turn can start a current one"
            ),
        )
        .with_detail(format!(
            "client={client_version} supported={min_supported}..={max_supported}"
        )));
    }

    // Inside the window: speak the client's dialect. Choosing the client's version
    // rather than the daemon's newest is what makes a rollout window mean anything.
    Ok(client_version)
}

/// Reads the `v` out of a frame without parsing anything else in it.
///
/// A frame written by a build this one does not understand is unparsable by
/// definition: an `op` that did not exist yet, a variant this build has no name
/// for. The version is the one field that stays readable anyway, because the
/// compatibility rules exist to keep it so — and reading it is what turns
/// "a message could not be understood", which leaves the user with nothing to do,
/// into "this app is too old for the daemon", which tells them exactly what to do.
///
/// Takes the raw line, because that is what a caller has left after
/// [`crate::LineDecoder::next_line`] hands over bytes that
/// [`serde_json`] then refuses: [`crate::FrameError`] deliberately keeps only a
/// short excerpt, so the version has to be recovered from the line itself.
///
/// `None` when the frame is not a JSON object or carries no numeric `v`. That is
/// genuinely malformed and must be reported as such rather than guessed at.
pub fn peek_version(frame: &[u8]) -> Option<u32> {
    /// Everything else in the frame is ignored — no `deny_unknown_fields`
    /// anywhere in this protocol is what makes that safe.
    #[derive(Deserialize)]
    struct VersionOnly {
        v: u32,
    }

    serde_json::from_slice::<VersionOnly>(frame)
        .ok()
        .map(|probe| probe.v)
}

/// The refusal an unparsable frame deserves, when its version is the reason.
///
/// `None` means the version is one this build speaks, so the frame really is
/// nonsense rather than merely foreign and stays a
/// [`ErrorCode::MalformedMessage`]. `Some` is the same error
/// [`negotiate`] would have produced at the handshake, which is the message the
/// user needs: a frame from a build outside the window is the case where the
/// handshake itself may never have happened.
pub fn version_refusal(frame: &[u8]) -> Option<ProtoError> {
    negotiate(peek_version(frame)?).err()
}

/// A frame from the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ClientFrame {
    /// Protocol version this frame is written against.
    pub v: u32,
    #[serde(flatten)]
    pub message: ClientMessage,
}

impl ClientFrame {
    /// Wraps a message at this build's version.
    pub fn new(message: ClientMessage) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            message,
        }
    }

    pub fn hello(hello: Hello) -> Self {
        Self::new(ClientMessage::Hello(hello))
    }

    pub fn request(id: RequestId, request: Request) -> Self {
        Self::new(ClientMessage::Request { id, request })
    }

    /// Negotiates from this frame's version. Called on the opening `Hello`.
    pub fn negotiate(&self) -> Result<u32, ProtoError> {
        negotiate(self.v)
    }

    /// Checks a post-handshake frame against the agreed version.
    ///
    /// Catches a peer that changed code without re-handshaking, which is the
    /// everyday case while developing a UI against a long-running daemon.
    pub fn expect_version(&self, agreed: u32) -> Result<(), ProtoError> {
        if self.v == agreed {
            return Ok(());
        }
        Err(ProtoError::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "This connection agreed on protocol {agreed} but a frame arrived \
                 marked {}. Reconnect to negotiate again",
                self.v
            ),
        ))
    }

    pub fn request_id(&self) -> Option<&RequestId> {
        match &self.message {
            ClientMessage::Request { id, .. } => Some(id),
            ClientMessage::Hello(_) => None,
        }
    }
}

/// What a client can send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Always the first frame. A request before it is refused with
    /// [`ErrorCode::HandshakeRequired`].
    Hello(Hello),
    /// A request and the id to answer it with.
    ///
    /// Requests may be pipelined: a client need not wait for one response before
    /// sending the next, which matters for `write_pty` at typing speed. Responses
    /// are correlated by `id`, not by arrival order.
    Request { id: RequestId, request: Request },
}

/// A frame from the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServerFrame {
    pub v: u32,
    #[serde(flatten)]
    pub message: ServerMessage,
}

impl ServerFrame {
    pub fn new(message: ServerMessage) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            message,
        }
    }

    pub fn welcome(welcome: Welcome) -> Self {
        // A welcome is stamped with the agreed version, not the daemon's newest:
        // from here on the whole connection speaks the agreed dialect.
        Self {
            v: welcome.agreed_version,
            message: ServerMessage::Welcome(welcome),
        }
    }

    /// A refused handshake. The daemon sends this and closes the connection.
    pub fn rejected(error: ProtoError) -> Self {
        Self::new(ServerMessage::Rejected { error })
    }

    pub fn response(id: RequestId, response: Response) -> Self {
        Self::new(ServerMessage::Response { id, response })
    }

    /// A failed request. `id` is `None` for a failure that belongs to no
    /// request — a malformed frame, an over-length line.
    pub fn error(id: Option<RequestId>, error: ProtoError) -> Self {
        Self::new(ServerMessage::Error { id, error })
    }

    pub fn event(event: ServerEvent) -> Self {
        Self::new(ServerMessage::Event { event })
    }

    /// The request this frame answers, if any. The client's correlation hook.
    pub fn request_id(&self) -> Option<&RequestId> {
        match &self.message {
            ServerMessage::Response { id, .. } => Some(id),
            ServerMessage::Error { id, .. } => id.as_ref(),
            _ => None,
        }
    }

    /// Whether this frame means the connection is over.
    pub fn is_terminal(&self) -> bool {
        matches!(self.message, ServerMessage::Rejected { .. })
    }
}

/// What a daemon can send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The handshake succeeded.
    Welcome(Welcome),
    /// The handshake failed. Nothing else will arrive on this connection.
    Rejected {
        error: ProtoError,
    },
    Response {
        id: RequestId,
        response: Response,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<RequestId>,
        error: ProtoError,
    },
    /// An unsolicited push.
    Event {
        event: ServerEvent,
    },
}

#[cfg(test)]
mod tests;
