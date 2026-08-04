//! The single error shape every failed request comes back as.
//!
//! One shape rather than a per-request error enum, because the UI's error
//! handling is generic — show the message, log the code — and a client written in
//! another language should not have to model forty failure types to be correct.
//! The machine-readable [`ErrorCode`] is what code branches on; `message` is what
//! a human reads and is never parsed.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Machine-readable failure classes.
///
/// Deliberately coarse. A code exists when a client would plausibly behave
/// differently on it — retry, re-handshake, refresh its state, or give up — not
/// merely to describe what went wrong in more words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The client speaks a protocol version this daemon cannot serve. The client
    /// must not retry; one of the two binaries has to be updated.
    UnsupportedVersion,
    /// A request arrived before the handshake completed.
    HandshakeRequired,
    /// A second `Hello` on a connection that already completed one.
    AlreadyHandshaked,
    /// The line was not valid JSON, or not a message this protocol defines.
    MalformedMessage,
    /// The line exceeded the frame limit and was discarded. The connection
    /// survives; whatever was being sent did not.
    LineTooLong,
    /// The id in the request does not exist (or no longer does).
    NotFound,
    /// The request is well-formed but its arguments make no sense — a resize
    /// delta of NaN, an empty workspace name, a snooze in the past.
    InvalidArgument,
    /// The request contradicts current state: closing the last pane of a
    /// session, attaching a pane that has no process.
    Conflict,
    /// Output was requested for a pane the client never attached to.
    PaneNotAttached,
    /// The target process is not running, so there is nothing to write to,
    /// resize, interrupt or kill.
    ProcessNotRunning,
    /// Turn structurally refuses this. Not a permission check that could be
    /// relaxed by configuration: these are the product's own limits — Turn never
    /// auto-approves an agent's permission request and never runs a command it
    /// inferred from agent output.
    Refused,
    /// A dependency the request needs is unavailable — the store, the pty layer,
    /// an agent binary that is no longer on `PATH`.
    Unavailable,
    /// A bug in the daemon. Always worth reporting.
    Internal,
}

impl ErrorCode {
    /// Whether re-sending the identical request could plausibly succeed later.
    ///
    /// The UI uses this to decide between offering a retry and reporting a
    /// failure the user has to act on.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ErrorCode::Unavailable | ErrorCode::Internal)
    }

    /// Whether the connection itself is unusable and must be re-established.
    pub fn is_fatal_to_connection(&self) -> bool {
        matches!(
            self,
            ErrorCode::UnsupportedVersion | ErrorCode::HandshakeRequired
        )
    }

    /// The stable wire string. Kept as a method so the documented catalogue and
    /// the code cannot drift.
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::HandshakeRequired => "handshake_required",
            ErrorCode::AlreadyHandshaked => "already_handshaked",
            ErrorCode::MalformedMessage => "malformed_message",
            ErrorCode::LineTooLong => "line_too_long",
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::Conflict => "conflict",
            ErrorCode::PaneNotAttached => "pane_not_attached",
            ErrorCode::ProcessNotRunning => "process_not_running",
            ErrorCode::Refused => "refused",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProtoError {
    pub code: ErrorCode,
    /// Sentence-case, no trailing full stop, safe to show to the user verbatim.
    pub message: String,
    /// Extra context for logs and bug reports: the offending value, the id that
    /// was not found. Never required for the UI to render something sensible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ProtoError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn not_found(what: &str, id: &str) -> Self {
        Self::new(ErrorCode::NotFound, format!("No such {what}")).with_detail(id.to_string())
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// Something Turn will not do on principle. The message is shown to the
    /// user, so it explains the rule rather than just refusing.
    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Refused, message)
    }
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProtoError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_round_trips_with_its_machine_readable_code() {
        let error = ProtoError::not_found("session", "sess_abc123");
        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"not_found\""), "got {json}");
        assert_eq!(serde_json::from_str::<ProtoError>(&json).unwrap(), error);
    }

    #[test]
    fn an_error_without_detail_omits_the_field_rather_than_sending_null() {
        let json = serde_json::to_string(&ProtoError::invalid("bad delta")).unwrap();
        assert!(!json.contains("detail"), "got {json}");
    }

    #[test]
    fn a_version_mismatch_is_never_presented_as_retryable() {
        assert!(!ErrorCode::UnsupportedVersion.is_retryable());
        assert!(ErrorCode::UnsupportedVersion.is_fatal_to_connection());
        // A transient dependency failure is the opposite on both counts.
        assert!(ErrorCode::Unavailable.is_retryable());
        assert!(!ErrorCode::Unavailable.is_fatal_to_connection());
    }

    /// One of each code, checked for completeness by the count assertion below.
    fn all_codes() -> [ErrorCode; 13] {
        [
            ErrorCode::UnsupportedVersion,
            ErrorCode::HandshakeRequired,
            ErrorCode::AlreadyHandshaked,
            ErrorCode::MalformedMessage,
            ErrorCode::LineTooLong,
            ErrorCode::NotFound,
            ErrorCode::InvalidArgument,
            ErrorCode::Conflict,
            ErrorCode::PaneNotAttached,
            ErrorCode::ProcessNotRunning,
            ErrorCode::Refused,
            ErrorCode::Unavailable,
            ErrorCode::Internal,
        ]
    }

    #[test]
    fn every_code_has_a_wire_name_matching_its_serialised_form() {
        for code in all_codes() {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
            assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), code);
        }
    }

    /// The wire names are the vocabulary two independent implementations share; a
    /// duplicate would make one code unreachable, and a silent addition would make
    /// docs/PROTOCOL.md wrong.
    #[test]
    fn the_code_catalogue_is_the_documented_size_with_no_duplicates() {
        let unique: std::collections::HashSet<&'static str> =
            all_codes().iter().map(|c| c.as_str()).collect();
        assert_eq!(
            unique.len(),
            13,
            "the error catalogue changed size: {unique:?}"
        );
    }

    #[test]
    fn the_display_form_carries_code_message_and_detail() {
        let error = ProtoError::refused("Turn never approves a permission on your behalf")
            .with_detail("agent.permission_required");
        let shown = error.to_string();
        assert!(shown.starts_with("[refused] Turn never approves"));
        assert!(shown.ends_with("(agent.permission_required)"));
    }
}
