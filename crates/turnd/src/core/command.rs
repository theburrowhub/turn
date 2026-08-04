//! The one channel into the state owner.
//!
//! Everything that wants to change state — a UI request, a pty exiting, a pump with
//! output to deliver, a signal handler — sends one of these. Nothing else may touch
//! a session, which is why there is no lock anywhere in [`crate::core`]: the
//! serialisation is the channel.

use tokio::sync::{mpsc, oneshot};
use turn_core::ids::NodeId;
use turn_proto::{ProtoError, Request, RequestId, Response, ServerFrame};

/// A connected client, numbered in accept order.
///
/// A counter rather than a random id: it appears in log lines, and "client 3" is
/// easier to follow through a debugging session than a uuid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client-{}", self.0)
    }
}

/// Work for the core task.
pub enum Command {
    /// A connection finished its handshake and is ready for pushes.
    ClientOpened {
        client: ClientId,
        agreed_version: u32,
        frames: mpsc::Sender<ServerFrame>,
        /// Answered once the client is registered, so the connection task knows its
        /// pushes will not be dropped before its first request is served.
        ready: oneshot::Sender<()>,
    },
    /// A connection ended. Its attachments go with it; its processes do not.
    ClientClosed { client: ClientId },
    /// One request, with the channel its answer travels back on.
    Request {
        client: ClientId,
        id: RequestId,
        request: Box<Request>,
        reply: oneshot::Sender<std::result::Result<Response, ProtoError>>,
    },
    /// Coalesced output from one node's pty.
    Output {
        node: NodeId,
        data: Vec<u8>,
        /// Frames the pump itself lost because it fell behind the pty's broadcast.
        /// Reported rather than hidden: the UI needs to re-attach.
        dropped: u64,
    },
    /// A process ended. Carries the exit as `turn-pty` observed it, so a signal
    /// death is not flattened into an exit code.
    Exited {
        node: NodeId,
        info: turn_pty::ExitInfo,
    },
    /// Stop: flush state to the store and end the loop.
    Shutdown { done: oneshot::Sender<()> },
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::ClientOpened { client, .. } => write!(f, "ClientOpened({client})"),
            Command::ClientClosed { client } => write!(f, "ClientClosed({client})"),
            Command::Request {
                client,
                id,
                request,
                ..
            } => {
                write!(f, "Request({client}, {id}, {})", request.op())
            }
            // Never the bytes: pty traffic contains whatever the user typed.
            Command::Output {
                node,
                data,
                dropped,
            } => {
                write!(f, "Output({node}, {} bytes, {dropped} dropped)", data.len())
            }
            Command::Exited { node, info } => write!(f, "Exited({node}, {info:?})"),
            Command::Shutdown { .. } => f.write_str("Shutdown"),
        }
    }
}
