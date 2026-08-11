//! # turnd
//!
//! The daemon. It owns every pty, all persistent state and the attention manager;
//! a UI attaches over a unix socket, renders and forwards keystrokes.
//!
//! That split is the whole reason the daemon exists. A pty belongs to the process
//! that opened it, so as long as the window owns the terminal, closing the window
//! ends the work. Here the window owns nothing: it asks for a pane's current screen
//! ([`turn_proto::Request::AttachPane`]) and gets bytes that rebuild it, because the
//! process never stopped running.
//!
//! ## Shape
//!
//! * [`config`] and [`paths`] — where the socket, the database and the throwaway
//!   agent configuration live.
//! * [`instance`] — refusing to be the second daemon, and clearing away the socket
//!   of a daemon that died.
//! * [`server`] — the accept loop and one task pair per connection.
//! * [`core`] — the single owner of state. Every mutation happens in one task, fed
//!   by four sources: UI requests, agent hook callbacks, pty exits and a modest
//!   timer. Nothing else holds a lock on a session.
//!
//! ## Four product rules this crate is built around
//!
//! 1. **A heuristic never moves the user.** Turn-axis changes carry the confidence
//!    of the event that caused them, and a provisional event cannot overwrite a
//!    state a hook or the user established ([`core::Core`]'s turn authority).
//! 2. **Turn never answers a pending agent interaction autonomously.** Permission
//!    and question prompts are answered only by [`turn_proto::Request::WritePty`]
//!    carrying human input. A reviewed context handoff is a separate explicit
//!    operation accepted only while its destination Agent is idle or done.
//! 3. **Turn never runs a command it inferred.** Processes start from a template, a
//!    pane definition or an explicit relaunch.
//! 4. **Turn never relaunches on restore.** Startup *reports* what it found and
//!    marks what could be started again. Nothing runs until the user asks.

pub(crate) mod checkout_lock;
pub mod config;
pub mod core;
pub mod error;
pub mod instance;
pub mod logging;
pub mod options;
pub mod paths;
pub mod privacy;
pub mod server;

pub use config::Config;
pub use error::{DaemonError, Result};
pub use options::Options;
pub use server::{
    start, DaemonHandle, IpcStats, MAX_IPC_CONNECTIONS, REQUESTS_PER_SECOND, REQUEST_BURST,
};

/// The daemon's own version, reported in the handshake.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
