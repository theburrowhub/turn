//! The single error shape every failed request comes back as.
//!
//! One shape rather than a per-request error enum, because the UI's error
//! handling is generic — show the message, log the code — and a client written in
//! another language should not have to model forty failure types to be correct.
//! The machine-readable [`ErrorCode`] is what code branches on; `message` is what
//! a human reads and is never parsed.

use serde::{Deserialize, Serialize};
use std::fmt;
use turn_core::ids::{CheckoutId, LeaseId, SessionId, WorkspaceId};
use turn_core::model::{SessionMode, WorkspaceWriteLease};

/// The current writer shown when a main-checkout Session cannot be created or
/// promoted. Kept smaller than a complete Session view so an error never leaks
/// environment/configuration fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WriteLeaseOwnerView {
    pub session_id: SessionId,
    pub session_name: String,
    pub mode: SessionMode,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub last_activity_ms: i64,
}

/// Safe next actions for a checkout conflict. The daemon supplies the set that
/// is actually available; the UI does not parse an error string to invent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionConflictAlternative {
    FocusOwner,
    CreateReadOnly,
    CreateIsolatedWorktree,
    Cancel,
}

/// Machine-readable detail for failures on which a client has a product flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtoErrorContext {
    /// Exactly one Session may own the primary checkout write lease. This shape
    /// is also used when a stale/recovery-required owner must be reconciled; the
    /// lease state tells the UI which explanation to show.
    WorkspaceWriteLeaseConflict {
        workspace_id: WorkspaceId,
        checkout_id: CheckoutId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requesting_session_id: Option<SessionId>,
        lease: Box<WorkspaceWriteLease>,
        owner: Box<WriteLeaseOwnerView>,
        alternatives: Vec<SessionConflictAlternative>,
    },
    /// Optimistic fencing rejected a client that retained an older generation.
    /// The client must refresh the lease; it may not retry the stale release.
    StaleLeaseGeneration {
        workspace_id: WorkspaceId,
        lease_id: LeaseId,
        expected_generation: u64,
        actual_generation: u64,
    },
}

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
    /// The peer did not present the current daemon capability. Reconnect only
    /// after re-reading the token file; repeating the same credential is refused.
    Unauthorized,
    /// A request arrived before the handshake completed.
    HandshakeRequired,
    /// A second `Hello` on a connection that already completed one.
    AlreadyHandshaked,
    /// The line was not valid JSON, or not a message this protocol defines.
    MalformedMessage,
    /// The line exceeded the frame limit and was discarded. The connection
    /// survives; whatever was being sent did not.
    LineTooLong,
    /// This authenticated client exceeded its request budget. Other clients and
    /// the daemon remain healthy; the caller may retry after backing off.
    RateLimited,
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
        matches!(
            self,
            ErrorCode::RateLimited | ErrorCode::Unavailable | ErrorCode::Internal
        )
    }

    /// Whether the connection itself is unusable and must be re-established.
    pub fn is_fatal_to_connection(&self) -> bool {
        matches!(
            self,
            ErrorCode::UnsupportedVersion | ErrorCode::Unauthorized | ErrorCode::HandshakeRequired
        )
    }

    /// The stable wire string. Kept as a method so the documented catalogue and
    /// the code cannot drift.
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::HandshakeRequired => "handshake_required",
            ErrorCode::AlreadyHandshaked => "already_handshaked",
            ErrorCode::MalformedMessage => "malformed_message",
            ErrorCode::LineTooLong => "line_too_long",
            ErrorCode::RateLimited => "rate_limited",
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
    /// Typed context required to render a recovery flow. `detail` remains only
    /// diagnostic text and must never be parsed for control flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Box<ProtoErrorContext>>,
}

impl ProtoError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            context: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_context(mut self, context: ProtoErrorContext) -> Self {
        self.context = Some(Box::new(context));
        self
    }

    pub fn workspace_write_lease_conflict(context: ProtoErrorContext) -> Self {
        debug_assert!(matches!(
            &context,
            ProtoErrorContext::WorkspaceWriteLeaseConflict { .. }
        ));
        Self::new(
            ErrorCode::Conflict,
            "The primary checkout already has a write owner",
        )
        .with_context(context)
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
        assert!(!json.contains("context"), "got {json}");
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
    fn all_codes() -> [ErrorCode; 15] {
        [
            ErrorCode::UnsupportedVersion,
            ErrorCode::Unauthorized,
            ErrorCode::HandshakeRequired,
            ErrorCode::AlreadyHandshaked,
            ErrorCode::MalformedMessage,
            ErrorCode::LineTooLong,
            ErrorCode::RateLimited,
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
            15,
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

    #[test]
    fn a_write_conflict_carries_owner_and_alternatives_as_typed_context() {
        let workspace_id = WorkspaceId::from_stored("ws_a");
        let checkout_id = CheckoutId::from_stored("checkout_a");
        let owner_id = SessionId::from_stored("sess_owner");
        let lease = WorkspaceWriteLease::active(
            workspace_id.clone(),
            owner_id.clone(),
            checkout_id.clone(),
            10,
        );
        let context = ProtoErrorContext::WorkspaceWriteLeaseConflict {
            workspace_id,
            checkout_id,
            requesting_session_id: None,
            lease: Box::new(lease),
            owner: Box::new(WriteLeaseOwnerView {
                session_id: owner_id,
                session_name: "Fix auth".into(),
                mode: SessionMode::MainCheckout,
                cwd: "/repo".into(),
                branch: Some("fix/auth".into()),
                last_activity_ms: 12,
            }),
            alternatives: vec![
                SessionConflictAlternative::FocusOwner,
                SessionConflictAlternative::CreateReadOnly,
                SessionConflictAlternative::CreateIsolatedWorktree,
                SessionConflictAlternative::Cancel,
            ],
        };
        let error = ProtoError::workspace_write_lease_conflict(context.clone());
        let json = serde_json::to_string(&error).unwrap();

        assert!(json.contains("\"kind\":\"workspace_write_lease_conflict\""));
        assert!(json.contains("\"focus_owner\""));
        assert!(json.contains("\"create_isolated_worktree\""));
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(error.context, Some(Box::new(context)));
        assert_eq!(serde_json::from_str::<ProtoError>(&json).unwrap(), error);
    }

    #[test]
    fn a_stale_fencing_generation_is_machine_readable() {
        let error = ProtoError::new(
            ErrorCode::Conflict,
            "The write lease changed before it could be released",
        )
        .with_context(ProtoErrorContext::StaleLeaseGeneration {
            workspace_id: WorkspaceId::from_stored("ws_a"),
            lease_id: LeaseId::from_stored("lease_a"),
            expected_generation: 4,
            actual_generation: 5,
        });
        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"kind\":\"stale_lease_generation\""));
        assert!(json.contains("\"expected_generation\":4"));
        assert!(json.contains("\"actual_generation\":5"));
        assert_eq!(serde_json::from_str::<ProtoError>(&json).unwrap(), error);
    }
}
