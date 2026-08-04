//! What the window says about its connection, and how long it waits before trying
//! again.
//!
//! The protocol makes the interesting question decidable rather than guessable:
//! `welcome` carries `daemon_pid` and `daemon_started_ms`, so a reconnecting window
//! can tell "my socket hiccupped and every pty is exactly where I left it" from "a
//! new daemon is running and nothing was inherited". Those need different
//! recoveries — the first only needs re-attaching, the second means the user must be
//! told what was lost — so the distinction is computed once, here.

use turn_proto::{ProtoError, Welcome};

/// Where the connection is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// The window is up and the supervisor has not run yet.
    ///
    /// A state of its own rather than `Connecting { attempt: 0 }`, so the first real
    /// attempt is a transition the window is told about. Collapsing the two would
    /// make the very first "connecting" the one status change nobody ever sees.
    Starting,
    /// Trying, and expected to succeed. `attempt` drives the "retrying…" copy.
    Connecting { attempt: u32 },
    /// Handshaked. Requests may be sent.
    Connected {
        daemon_version: String,
        daemon_pid: u32,
        agreed_version: u32,
        max_line_bytes: usize,
        /// True when this is a *different* daemon from the one we last spoke to. The
        /// window must re-fetch the world and cannot assume its attachments survived.
        daemon_restarted: bool,
        /// True for the first connection of this window's life, where "restarted"
        /// would be a meaningless claim.
        first_connection: bool,
    },
    /// The socket went away. Retrying, with the reason to show meanwhile.
    Disconnected { message: String, retrying: bool },
    /// The daemon refused this build. Retrying cannot help; the user must act.
    Incompatible {
        message: String,
        detail: Option<String>,
    },
}

impl ConnectionState {
    /// Whether requests can be sent right now.
    pub fn is_live(&self) -> bool {
        matches!(self, ConnectionState::Connected { .. })
    }

    /// The short word the status bar shows beside its glyph. Never colour alone.
    pub fn word(&self) -> &'static str {
        match self {
            ConnectionState::Starting => "starting",
            ConnectionState::Connecting { .. } => "connecting",
            ConnectionState::Connected { .. } => "connected",
            ConnectionState::Disconnected { .. } => "no daemon",
            ConnectionState::Incompatible { .. } => "wrong version",
        }
    }

    /// The sentence to show under the word.
    pub fn detail(&self) -> String {
        match self {
            ConnectionState::Starting => "looking for turnd".to_string(),
            ConnectionState::Connecting { attempt } if *attempt <= 1 => {
                "looking for turnd".to_string()
            }
            ConnectionState::Connecting { attempt } => format!("retrying · attempt {attempt}"),
            ConnectionState::Connected {
                daemon_pid,
                daemon_version,
                ..
            } => format!("turnd {daemon_version} · pid {daemon_pid}"),
            ConnectionState::Disconnected { message, .. } => message.clone(),
            ConnectionState::Incompatible { message, .. } => message.clone(),
        }
    }
}

/// Tracks which daemon we were last talking to.
///
/// Deliberately not a boolean "was connected before": the question is not whether
/// this window has connected before but whether it is connected to the *same*
/// process, and a pid alone is not enough because pids are reused. The pair
/// (pid, started_at) is.
#[derive(Debug, Default)]
pub struct DaemonIdentity {
    last_seen: Option<(u32, i64)>,
}

impl DaemonIdentity {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a successful handshake and reports what it means for the work the
    /// window had on screen.
    pub fn observe(&mut self, welcome: &Welcome) -> ConnectionState {
        let identity = (welcome.daemon_pid, welcome.daemon_started_ms);
        let first_connection = self.last_seen.is_none();
        let daemon_restarted = match self.last_seen {
            Some(previous) => previous != identity,
            None => false,
        };
        self.last_seen = Some(identity);

        ConnectionState::Connected {
            daemon_version: welcome.daemon_version.clone(),
            daemon_pid: welcome.daemon_pid,
            agreed_version: welcome.agreed_version,
            max_line_bytes: welcome.limits.max_line_bytes,
            daemon_restarted,
            first_connection,
        }
    }

    pub fn last_seen(&self) -> Option<(u32, i64)> {
        self.last_seen
    }
}

/// Backoff between connection attempts.
///
/// Doubling from a quarter of a second to a ceiling of five. The first attempt is
/// immediate because the overwhelmingly common case is a daemon three hundred
/// milliseconds from being ready — the user launched Turn and the daemon is still
/// binding — and making them wait a whole second for the window to come alive would
/// be a worse trade than three wasted syscalls.
pub const FIRST_RETRY_MS: u64 = 250;
pub const MAX_RETRY_MS: u64 = 5_000;

/// The delay before attempt number `attempt`, counting from one.
pub fn retry_delay_ms(attempt: u32) -> u64 {
    if attempt <= 1 {
        return 0;
    }
    let doublings = attempt.saturating_sub(2).min(16);
    (FIRST_RETRY_MS << doublings).min(MAX_RETRY_MS)
}

/// How long a connection has to last before the backoff counts as spent.
///
/// A handshake is not health. A daemon that is crash-looping accepts the socket,
/// answers `welcome` and dies again, and treating that as recovery resets the delay
/// to zero every time — which turns the supervisor into a hot loop that spins a core
/// and refills the status line for as long as the crash lasts. Ten seconds is longer
/// than any restart the daemon does on purpose and far shorter than a working day.
pub const HEALTHY_CONNECTION_MS: u64 = 10_000;

/// The attempt count the next connection starts from, after one that lasted
/// `lived_ms`.
///
/// Returning `attempt` unchanged is what makes the delay keep growing across a crash
/// loop; returning 0 is what makes an ordinary daemon restart reconnect at once.
pub fn attempt_after_connection(attempt: u32, lived_ms: u64) -> u32 {
    if lived_ms >= HEALTHY_CONNECTION_MS {
        0
    } else {
        attempt
    }
}

/// Turns a refusal into the terminal state for it.
pub fn incompatible(error: &ProtoError) -> ConnectionState {
    ConnectionState::Incompatible {
        message: error.message.clone(),
        detail: error.detail.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcome(pid: u32, started_ms: i64) -> Welcome {
        Welcome::new(1, "0.1.0", pid, started_ms)
    }

    #[test]
    fn the_first_connection_reports_no_restart_because_there_is_nothing_to_compare() {
        let mut identity = DaemonIdentity::new();
        match identity.observe(&welcome(900, 1_700_000_000_000)) {
            ConnectionState::Connected {
                daemon_restarted,
                first_connection,
                ..
            } => {
                assert!(
                    !daemon_restarted,
                    "there was no previous daemon to differ from"
                );
                assert!(first_connection);
            }
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    /// The case the protocol added `daemon_pid` for: the socket blipped, the same
    /// daemon answered, and every pty is still exactly where the window left it.
    #[test]
    fn reconnecting_to_the_same_daemon_reports_that_the_work_survived() {
        let mut identity = DaemonIdentity::new();
        identity.observe(&welcome(900, 1_700_000_000_000));
        match identity.observe(&welcome(900, 1_700_000_000_000)) {
            ConnectionState::Connected {
                daemon_restarted,
                first_connection,
                ..
            } => {
                assert!(!daemon_restarted);
                assert!(!first_connection, "this window has connected before");
            }
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    #[test]
    fn a_new_daemon_is_reported_as_a_restart_so_the_window_refetches_everything() {
        let mut identity = DaemonIdentity::new();
        identity.observe(&welcome(900, 1_700_000_000_000));
        match identity.observe(&welcome(901, 1_700_000_500_000)) {
            ConnectionState::Connected {
                daemon_restarted, ..
            } => assert!(daemon_restarted),
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    /// Pids are reused. A daemon that happens to land on the old pid after a reboot
    /// is still a different daemon, and the start time is what proves it.
    #[test]
    fn the_same_pid_with_a_different_start_time_is_still_a_different_daemon() {
        let mut identity = DaemonIdentity::new();
        identity.observe(&welcome(900, 1_700_000_000_000));
        match identity.observe(&welcome(900, 1_700_009_999_000)) {
            ConnectionState::Connected {
                daemon_restarted, ..
            } => assert!(
                daemon_restarted,
                "a recycled pid must not be mistaken for the daemon that owned our ptys"
            ),
            other => panic!("expected Connected, got {other:?}"),
        }
        assert_eq!(identity.last_seen(), Some((900, 1_700_009_999_000)));
    }

    #[test]
    fn the_first_attempt_does_not_wait_and_the_backoff_is_bounded() {
        assert_eq!(retry_delay_ms(1), 0);
        assert_eq!(retry_delay_ms(2), FIRST_RETRY_MS);
        assert_eq!(retry_delay_ms(3), 500);
        assert_eq!(retry_delay_ms(4), 1_000);
        assert_eq!(retry_delay_ms(60), MAX_RETRY_MS);
        // A window left open overnight against a stopped daemon must not overflow
        // its way back into an instant reconnect loop.
        assert_eq!(retry_delay_ms(u32::MAX), MAX_RETRY_MS);
    }

    /// A handshake is not health. A daemon that answers `welcome` and then dies must
    /// not buy back the whole backoff, or the supervisor reconnects to a crash loop
    /// as fast as the kernel will let it.
    #[test]
    fn a_connection_that_died_on_arrival_does_not_reset_the_backoff() {
        assert_eq!(attempt_after_connection(6, 0), 6);
        assert_eq!(attempt_after_connection(6, 40), 6);
        assert_eq!(
            attempt_after_connection(6, HEALTHY_CONNECTION_MS - 1),
            6,
            "one millisecond short is still a crash loop"
        );
        assert!(retry_delay_ms(attempt_after_connection(6, 40) + 1) > 0);
    }

    /// The other half of the same rule: an ordinary restart — a daemon that ran for
    /// hours, was upgraded and came back — must reconnect at once rather than being
    /// punished for a delay it earned days ago.
    #[test]
    fn a_connection_that_was_healthy_starts_the_next_attempt_from_scratch() {
        assert_eq!(attempt_after_connection(9, HEALTHY_CONNECTION_MS), 0);
        assert_eq!(attempt_after_connection(9, 3_600_000), 0);
        assert_eq!(
            retry_delay_ms(attempt_after_connection(9, 3_600_000) + 1),
            0,
            "the attempt after a healthy connection is immediate"
        );
    }

    #[test]
    fn a_refused_handshake_becomes_a_state_the_window_cannot_retry_out_of() {
        let error = ProtoError::new(
            turn_proto::ErrorCode::UnsupportedVersion,
            "This Turn app is too old for the daemon it is talking to",
        )
        .with_detail("client=2 supported=3..=4");
        match incompatible(&error) {
            ConnectionState::Incompatible { message, detail } => {
                assert_eq!(message, error.message);
                assert_eq!(detail.as_deref(), Some("client=2 supported=3..=4"));
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    /// Every state has to be sayable in words, because the status bar must never
    /// distinguish "connected" from "no daemon" by colour alone.
    #[test]
    fn every_connection_state_can_be_said_in_words() {
        let mut identity = DaemonIdentity::new();
        let states = [
            ConnectionState::Starting,
            ConnectionState::Connecting { attempt: 1 },
            ConnectionState::Connecting { attempt: 4 },
            identity.observe(&welcome(51234, 1_700_000_000_000)),
            ConnectionState::Disconnected {
                message: "the daemon connection ended".into(),
                retrying: true,
            },
            incompatible(&ProtoError::new(
                turn_proto::ErrorCode::UnsupportedVersion,
                "too old",
            )),
        ];
        for state in states {
            assert!(!state.word().is_empty(), "{state:?} has no word");
            assert!(!state.detail().is_empty(), "{state:?} has no detail");
        }
        assert_eq!(
            ConnectionState::Connecting { attempt: 4 }.detail(),
            "retrying · attempt 4"
        );
        assert!(!ConnectionState::Starting.is_live());
    }
}
