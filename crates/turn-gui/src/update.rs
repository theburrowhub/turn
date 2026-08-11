//! Safe update preflight for the split UI/daemon product.
//!
//! Replacing `Turn.app` does not replace the executable image of an already-running
//! `turnd`. That is useful: a compatible UI update can land while the daemon keeps
//! every PTY. It is also a trap if an installer assumes the daemon changed with the
//! files on disk. This module asks the live daemon for its authoritative PTY count and
//! turns protocol compatibility into one of three explicit plans. None of them stops a
//! process; an incompatible daemon restart is always a separate user action.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use turn_proto::{
    ClientFrame, Hello, LineDecoder, Request, RequestId, Response, RuntimeUpdateStatus,
    ServerFrame, ServerMessage, Welcome, MAX_LINE_BYTES,
};

const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(2);

/// Release metadata needed to decide whether its UI can speak to the live daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCompatibility {
    pub version: String,
    pub protocol_min: u32,
    pub protocol_max: u32,
}

/// Authoritative daemon facts printed by `turn --update-status` for the installer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonUpdateReport {
    pub daemon_running: bool,
    pub daemon_version: String,
    pub daemon_pid: u32,
    pub protocol_min: u32,
    pub protocol_max: u32,
    pub active_ptys: u32,
}

/// The only safe outcomes of comparing a release with a live daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePlan {
    /// Install the new app bundle and leave the current daemon entirely alone.
    InstallUiKeepDaemon {
        daemon_version: String,
        active_ptys: u32,
        agreed_protocol: u32,
    },
    /// The new UI cannot speak to the current daemon and PTYs still depend on it.
    DeferUntilPtysExit {
        daemon_version: String,
        active_ptys: u32,
    },
    /// No PTY would be lost, but restarting the daemon remains an explicit action.
    RequireExplicitDaemonRestart { daemon_version: String },
}

/// Chooses a safe install plan. This function never performs it.
pub fn plan_update(release: &ReleaseCompatibility, daemon: &DaemonUpdateReport) -> UpdatePlan {
    let overlap_min = release.protocol_min.max(daemon.protocol_min);
    let overlap_max = release.protocol_max.min(daemon.protocol_max);
    if overlap_min <= overlap_max {
        return UpdatePlan::InstallUiKeepDaemon {
            daemon_version: daemon.daemon_version.clone(),
            active_ptys: daemon.active_ptys,
            agreed_protocol: overlap_max,
        };
    }

    if daemon.active_ptys > 0 {
        UpdatePlan::DeferUntilPtysExit {
            daemon_version: daemon.daemon_version.clone(),
            active_ptys: daemon.active_ptys,
        }
    } else {
        UpdatePlan::RequireExplicitDaemonRestart {
            daemon_version: daemon.daemon_version.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateStatusError {
    #[error("no Turn daemon is listening on {socket}: {cause}")]
    Unavailable {
        socket: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("could not read the live daemon capability beside {socket}: {cause}")]
    Capability {
        socket: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("the update preflight connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the daemon sent an invalid update-preflight frame: {0}")]
    Frame(#[from] turn_proto::FrameError),
    #[error("the daemon refused update preflight: {0}")]
    Refused(turn_proto::ProtoError),
    #[error("the daemon answered update preflight with {0}")]
    Unexpected(String),
}

impl UpdateStatusError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

/// Queries a daemon without launching, stopping or adopting it.
pub fn query_update_status(socket: &Path) -> Result<DaemonUpdateReport, UpdateStatusError> {
    let mut stream =
        UnixStream::connect(socket).map_err(|cause| UpdateStatusError::Unavailable {
            socket: socket.to_path_buf(),
            cause,
        })?;
    stream.set_read_timeout(Some(PREFLIGHT_TIMEOUT))?;
    stream.set_write_timeout(Some(PREFLIGHT_TIMEOUT))?;

    let token =
        turn_proto::read_ipc_auth_token(socket).map_err(|cause| UpdateStatusError::Capability {
            socket: socket.to_path_buf(),
            cause,
        })?;
    let hello = ClientFrame::hello(Hello::new("turn-updater", env!("CARGO_PKG_VERSION"), token));
    stream.write_all(&turn_proto::encode_checked(&hello, MAX_LINE_BYTES)?)?;

    let mut decoder = LineDecoder::new();
    let welcome = match read_frame(&mut stream, &mut decoder)?.message {
        ServerMessage::Welcome(welcome) => welcome,
        ServerMessage::Rejected { error } | ServerMessage::Error { error, .. } => {
            return Err(UpdateStatusError::Refused(error))
        }
        other => {
            return Err(UpdateStatusError::Unexpected(format!(
                "{other:?} before welcome"
            )))
        }
    };

    let request_id = RequestId::new("update-preflight");
    let request = ClientFrame::request(request_id.clone(), Request::GetUpdateStatus);
    stream.write_all(&turn_proto::encode_checked(
        &request,
        welcome.limits.max_line_bytes,
    )?)?;

    let status = loop {
        let frame = read_frame(&mut stream, &mut decoder)?;
        match frame.message {
            ServerMessage::Response { id, response } if id == request_id => match response {
                Response::UpdateStatus { status } => break status,
                other => {
                    return Err(UpdateStatusError::Unexpected(format!(
                        "{} response",
                        other.result_name()
                    )))
                }
            },
            ServerMessage::Error {
                id: Some(id),
                error,
            } if id == request_id => return Err(UpdateStatusError::Refused(error)),
            ServerMessage::Event { .. } => continue,
            other => {
                return Err(UpdateStatusError::Unexpected(format!(
                    "unrelated {other:?}"
                )))
            }
        }
    };

    validate_status(&welcome, &status)?;
    Ok(DaemonUpdateReport {
        daemon_running: true,
        daemon_version: status.daemon_version,
        daemon_pid: welcome.daemon_pid,
        protocol_min: status.protocol_min,
        protocol_max: status.protocol_max,
        active_ptys: status.active_ptys,
    })
}

fn validate_status(
    welcome: &Welcome,
    status: &RuntimeUpdateStatus,
) -> Result<(), UpdateStatusError> {
    if status.daemon_version != welcome.daemon_version {
        return Err(UpdateStatusError::Unexpected(format!(
            "welcome named daemon {} but update status named {}",
            welcome.daemon_version, status.daemon_version
        )));
    }
    if status.protocol_min > status.protocol_max
        || welcome.agreed_version < status.protocol_min
        || welcome.agreed_version > status.protocol_max
    {
        return Err(UpdateStatusError::Unexpected(format!(
            "invalid daemon protocol window {}..={} for agreed protocol {}",
            status.protocol_min, status.protocol_max, welcome.agreed_version
        )));
    }
    Ok(())
}

fn read_frame(
    stream: &mut UnixStream,
    decoder: &mut LineDecoder,
) -> Result<ServerFrame, UpdateStatusError> {
    let mut bytes = [0u8; 8 * 1024];
    loop {
        if let Some(frame) = decoder.next_message::<ServerFrame>() {
            return frame.map_err(UpdateStatusError::Frame);
        }
        let read = stream.read(&mut bytes)?;
        if read == 0 {
            return Err(UpdateStatusError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the daemon closed the update-preflight connection",
            )));
        }
        decoder.feed(&bytes[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(min: u32, max: u32) -> ReleaseCompatibility {
        ReleaseCompatibility {
            version: "0.2.0".into(),
            protocol_min: min,
            protocol_max: max,
        }
    }

    fn daemon(min: u32, max: u32, active_ptys: u32) -> DaemonUpdateReport {
        DaemonUpdateReport {
            daemon_running: true,
            daemon_version: "0.1.0".into(),
            daemon_pid: 42,
            protocol_min: min,
            protocol_max: max,
            active_ptys,
        }
    }

    #[test]
    fn a_compatible_ui_update_keeps_the_daemon_even_with_live_ptys() {
        assert_eq!(
            plan_update(&release(4, 5), &daemon(3, 4, 27)),
            UpdatePlan::InstallUiKeepDaemon {
                daemon_version: "0.1.0".into(),
                active_ptys: 27,
                agreed_protocol: 4,
            }
        );
    }

    #[test]
    fn an_incompatible_update_is_deferred_while_any_pty_is_alive() {
        assert_eq!(
            plan_update(&release(5, 5), &daemon(4, 4, 1)),
            UpdatePlan::DeferUntilPtysExit {
                daemon_version: "0.1.0".into(),
                active_ptys: 1,
            }
        );
    }

    #[test]
    fn even_an_idle_incompatible_daemon_is_never_restarted_silently() {
        assert_eq!(
            plan_update(&release(5, 5), &daemon(4, 4, 0)),
            UpdatePlan::RequireExplicitDaemonRestart {
                daemon_version: "0.1.0".into(),
            }
        );
    }

    #[test]
    fn a_status_cannot_disagree_with_the_authenticated_welcome() {
        let welcome = Welcome::new(4, "0.1.0", 42, 1);
        let wrong_version = RuntimeUpdateStatus {
            daemon_version: "0.2.0".into(),
            protocol_min: 4,
            protocol_max: 4,
            active_ptys: 0,
        };
        assert!(validate_status(&welcome, &wrong_version).is_err());

        let inverted = RuntimeUpdateStatus {
            daemon_version: "0.1.0".into(),
            protocol_min: 5,
            protocol_max: 4,
            active_ptys: 0,
        };
        assert!(validate_status(&welcome, &inverted).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preflight_crosses_the_authenticated_socket_without_starting_or_stopping_the_daemon() {
        let state = tempfile::tempdir().expect("an isolated daemon directory");
        let daemon = turnd::start(
            turnd::Config::in_dir(state.path())
                .with_checkout_lock_dir(state.path().join("checkout-locks"))
                .without_persistence(),
        )
        .await
        .expect("the isolated daemon starts");
        let socket = daemon.socket_path().to_path_buf();
        let report = tokio::task::spawn_blocking(move || query_update_status(&socket))
            .await
            .expect("the blocking preflight task joins")
            .expect("the authenticated preflight succeeds");

        assert!(report.daemon_running);
        assert_eq!(report.daemon_pid, daemon.info().pid);
        assert_eq!(report.daemon_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(report.protocol_min, turn_proto::MIN_PROTOCOL_VERSION);
        assert_eq!(report.protocol_max, turn_proto::PROTOCOL_VERSION);
        assert_eq!(report.active_ptys, 0);

        daemon.shutdown().await;
    }
}
