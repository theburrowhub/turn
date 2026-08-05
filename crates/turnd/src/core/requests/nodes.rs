//! Writing to processes, stopping them, and the one way one comes back.

use super::Answer;
use crate::core::{ClientId, Core};
use turn_core::ids::{NodeId, SessionId};
use turn_proto::{ErrorCode, ProtoError, PtySize, Response, ServerEvent};
use turn_pty::ScreenSize;

impl Core {
    /// Sends keystrokes or pasted text to a process.
    ///
    /// This is also how an agent's permission prompt is answered. There is no request
    /// that approves anything: the only thing that reaches a pty is what the human
    /// typed, and that is the point.
    pub(super) fn write_pty(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        data: &[u8],
        now_ms: i64,
    ) -> Answer {
        self.node_of(session_id, node_id)?;
        let process = self.running_process(node_id)?;
        process
            .pty
            .write(data)
            .map_err(|error| pty_failure("write to", error))?;
        if let Some(session) = self.sessions.get_mut(session_id) {
            // Recorded in memory only. A write happens per keystroke, and a database
            // transaction per keystroke would be a strange way to spend a session.
            session.touch(now_ms);
        }
        Ok(Response::Ack)
    }

    pub(super) fn resize_pty(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        node_id: &NodeId,
        size: PtySize,
    ) -> Answer {
        self.node_of(session_id, node_id)?;
        let requested = PtySize::new(size.rows, size.cols);
        let screen = ScreenSize::new(requested.rows, requested.cols);
        let process = self.running_process(node_id)?;
        process
            .pty
            .resize(screen)
            .map_err(|error| pty_failure("resize", error))?;
        process.size = screen;
        // A second client showing the same session has to follow, or it renders at a
        // width the process is no longer drawing for.
        self.push_others(
            client,
            ServerEvent::PtyResized {
                session_id: session_id.clone(),
                node_id: node_id.clone(),
                size: requested,
            },
        );
        // Everyone rendering cells gets the whole screen at the new geometry, the
        // resizing client included: rows do not correspond across a resize, so a diff
        // would be meaningless, and a program that does not redraw would otherwise
        // leave the client with nothing at its new size.
        self.push_full_screen(node_id, None);
        Ok(Response::Ack)
    }

    /// Sends the interrupt character through the tty.
    ///
    /// Not `kill(pid)`: the tty delivers the signal to the whole foreground process
    /// group, which is what reaches the `cargo test` an agent started rather than only
    /// the agent itself.
    pub(super) fn interrupt_node(&mut self, session_id: &SessionId, node_id: &NodeId) -> Answer {
        self.node_of(session_id, node_id)?;
        let process = self.running_process(node_id)?;
        process
            .pty
            .interrupt()
            .map_err(|error| pty_failure("interrupt", error))?;
        Ok(Response::Ack)
    }

    /// Stops a process, politely or not.
    pub(super) fn stop_node(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        hard: bool,
        now_ms: i64,
    ) -> Answer {
        self.node_of(session_id, node_id)?;
        self.running_process(node_id)?;
        self.signal_node(node_id, hard, now_ms);
        Ok(Response::Ack)
    }

    /// Starts a pane's process again, because the user asked.
    ///
    /// Turn never relaunches on its own — not on restore, not after a crash — so this
    /// is the only path back into a running process, and it always begins with a human.
    ///
    /// The old node record leaves the tree. Two nodes cannot both claim one pane, and
    /// the event log keeps the whole history of the one that ended.
    pub(super) fn relaunch_node(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        resume: bool,
        now_ms: i64,
    ) -> Answer {
        self.require_session_launch_allowed(session_id)?;
        let node = self.node_of(session_id, node_id)?.clone();
        if node.is_running() {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "That process is still running",
            ));
        }
        let Some(pane_id) = self
            .session(session_id)?
            .layout
            .panes()
            .into_iter()
            .find(|pane| pane.node_id.as_ref() == Some(node_id))
            .map(|pane| pane.id.clone())
        else {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "Turn did not start this process, so it cannot start it again",
            )
            .with_detail(
                "only a pane's own process can be relaunched; this node was reported by \
                 a tool or seen in the process table",
            ));
        };
        let session = self.session(session_id)?;
        if session.layout.get(&pane_id).is_none() {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "The pane this process belonged to is gone",
            ));
        }

        let extra = if resume {
            resume_arguments(&self.resume_target(&node))?
        } else {
            Vec::new()
        };

        // Retire the old record and everything that hung off it before the new process
        // takes the pane.
        if let Ok(session) = self.session_mut(session_id) {
            let descendants: Vec<NodeId> = session
                .tree
                .descendants(node_id)
                .into_iter()
                .map(|node| node.id.clone())
                .collect();
            for id in descendants {
                session.tree.remove(&id);
            }
            session.tree.remove(node_id);
            if let Some(pane) = session.layout.get_mut(&pane_id) {
                pane.node_id = None;
            }
        }
        self.attention.resolve_node(node_id);
        self.turn_authority.remove(node_id);
        self.background_tasks.remove(node_id);
        self.expected_exits.remove(node_id);
        self.discard_process(node_id);
        // The old launch's injected configuration goes with it. Its token was revoked
        // when the process was discarded, so what would be left on disk is a settings
        // file telling an agent to report to a URL that answers nothing — and the new
        // process writes its own, under its own node id.
        crate::paths::remove_node_scratch(&self.data_dir, session_id, node_id);

        let started = self.materialise_pane_with(session_id, &pane_id, &extra, now_ms)?;
        let Some(new_node) = started else {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "That pane has no command to run",
            ));
        };
        self.persist_session(session_id)?;
        self.push_tree(session_id, now_ms);
        self.push_session_state(session_id, now_ms);

        let view = self
            .node_view(session_id, &new_node, now_ms)
            .ok_or_else(|| ProtoError::internal("the relaunched node is missing from the tree"))?;
        Ok(Response::Node {
            node: Box::new(view),
        })
    }

    /// Stops a process and records that we asked for it.
    ///
    /// The distinction matters downstream: a process the user stopped has not failed, so
    /// it does not raise the failure trigger and does not produce a notification about
    /// something the user did on purpose.
    ///
    /// The expectation is recorded only once a signal has actually been delivered, and
    /// only for as long as that delivery can plausibly explain an exit. A signal that
    /// failed stopped nothing; a signal that landed on a program which catches it stopped
    /// nothing either. Either way the process is still running and whatever ends it later
    /// ends it for its own reasons — and an expectation left behind would make that death,
    /// a genuine crash, report nothing at all. See
    /// [`EXPECTED_EXIT_GRACE_MS`](crate::core::EXPECTED_EXIT_GRACE_MS).
    ///
    /// The pty handle is kept. The pane is still there, the buffer still holds what the
    /// process printed, and a program that chooses to ignore the signal goes on showing as
    /// running — which is the truth, and leaves the user a kill to reach for.
    pub(crate) fn signal_node(&mut self, node: &NodeId, hard: bool, now_ms: i64) {
        let Some(process) = self.processes.get(node) else {
            return;
        };
        let outcome = if hard {
            process.pty.kill()
        } else {
            process.pty.terminate()
        };
        match outcome {
            Ok(()) => {
                self.expect_exit(node, now_ms);
            }
            Err(error) => tracing::warn!(%node, hard, %error, "could not signal a process"),
        }
    }

    /// Records that this node's exit, if it comes soon, is one the user asked for.
    pub(crate) fn expect_exit(&mut self, node: &NodeId, now_ms: i64) {
        self.expected_exits
            .insert(node.clone(), now_ms + crate::core::EXPECTED_EXIT_GRACE_MS);
    }

    /// Stops a process and lets go of its pty, because its pane is going away.
    ///
    /// This is what closing a terminal window does, and the difference from
    /// [`Self::signal_node`] is the part that matters: an interactive shell *ignores*
    /// `SIGTERM`, so a polite signal alone would leave it running with nothing on screen
    /// to show it and no way for the user to reach it again. Dropping the handle closes
    /// the pty master, which delivers `SIGHUP` to the whole foreground process group —
    /// the actual mechanism by which closing a terminal ends what was in it.
    ///
    /// The exit is recorded from what we did rather than waited for: once the master is
    /// closed there is no watcher left to report it, and a node that stayed `alive`
    /// forever would be the daemon lying about a pane the user just closed.
    pub(crate) fn stop_and_release(
        &mut self,
        session_id: &SessionId,
        node: &NodeId,
        hard: bool,
        now_ms: i64,
    ) {
        // Recorded here rather than left to `signal_node`, because this death is one the
        // user asked for however it arrives: the signal, or — for a program that ignores
        // it — the pty master closing underneath it a moment later. Both happen inside
        // this function, so the expectation is spent immediately and its deadline never
        // has a chance to matter.
        self.expect_exit(node, now_ms);
        self.signal_node(node, hard, now_ms);
        let observed = self.discard_process(node);
        let info = observed.unwrap_or_else(|| {
            // The shell convention for a signal death, which is what this is: the
            // process was signalled and its terminal taken away.
            let signal = if hard { "Killed" } else { "Terminated" };
            turn_pty::ExitInfo {
                code: if hard { 137 } else { 143 },
                signal: Some(signal.to_string()),
            }
        });
        self.record_exit(session_id, node, info, now_ms);
    }

    /// What a resume would continue, if anything.
    fn resume_target(&self, node: &turn_core::model::ProcessNode) -> ResumeTarget {
        let agent = node.agent.as_ref();
        ResumeTarget {
            adapter: agent
                .and_then(|agent| agent.agent.tool.clone())
                .unwrap_or_default(),
            external_id: agent.and_then(|agent| agent.external_id.clone()),
            resumable: agent.is_some_and(|agent| agent.resumable),
        }
    }

    /// The node, if it belongs to this session.
    pub(crate) fn node_of(
        &self,
        session_id: &SessionId,
        node_id: &NodeId,
    ) -> std::result::Result<&turn_core::model::ProcessNode, ProtoError> {
        self.session(session_id)?
            .tree
            .get(node_id)
            .ok_or_else(|| ProtoError::not_found("process", node_id.as_str()))
    }

    /// The live process for a node, or a refusal that names why there is none.
    fn running_process(
        &mut self,
        node_id: &NodeId,
    ) -> std::result::Result<&mut crate::core::Process, ProtoError> {
        match self.processes.get_mut(node_id) {
            Some(process) if process.pty.is_running() => Ok(process),
            Some(_) => Err(ProtoError::new(
                ErrorCode::ProcessNotRunning,
                "That process has ended",
            )),
            None => Err(ProtoError::new(
                ErrorCode::ProcessNotRunning,
                "Turn does not hold this process",
            )
            .with_detail(
                "it was reported by a tool, seen in the process table, or lost when the \
                 daemon restarted",
            )),
        }
    }
}

/// What a resume request would need to honour.
struct ResumeTarget {
    adapter: String,
    external_id: Option<String>,
    resumable: bool,
}

/// The arguments that resume an agent's previous conversation.
///
/// Only spelled out for tools where the flag is known to exist. Guessing at one would
/// mean launching an agent with an argument it does not understand, and the failure
/// would look like Turn breaking the tool. When a resume cannot be honoured the request
/// is refused rather than quietly starting a fresh conversation: the user asked to
/// continue, and silently not continuing is the wrong kind of surprise.
fn resume_arguments(target: &ResumeTarget) -> std::result::Result<Vec<String>, ProtoError> {
    let refuse = |reason: &str| {
        Err(
            ProtoError::new(ErrorCode::Conflict, reason).with_detail(format!(
                "adapter={} external_id={:?}",
                target.adapter, target.external_id
            )),
        )
    };
    if !target.resumable {
        return refuse(
            "This agent cannot be resumed. Relaunch without resume to start a fresh conversation",
        );
    }
    let Some(external) = target.external_id.clone() else {
        return refuse(
            "Turn never learned this agent's own session id, so there is nothing to resume",
        );
    };
    match target.adapter.as_str() {
        "claude-code" => Ok(vec!["--resume".to_string(), external]),
        _ => refuse(
            "Turn does not know how to resume this tool. Relaunch without resume to start \
             a fresh conversation",
        ),
    }
}

/// Turns a pty failure into an answer the user can read.
fn pty_failure(what: &str, error: turn_pty::PtyError) -> ProtoError {
    ProtoError::new(
        ErrorCode::Unavailable,
        format!("Could not {what} that process"),
    )
    .with_detail(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::ids::PaneId;
    use turn_core::model::{NodeKind, ProcessNode};
    use turn_core::state::Lifecycle;

    const NOW: i64 = 1_775_000_000_000;

    /// A stop whose signal never reached the process stopped nothing. Whatever ends that
    /// process later ends it for its own reasons, and a crash that reports nothing —
    /// no failure state, no notification — is the worst kind of silence this daemon has.
    #[tokio::test]
    async fn a_signal_that_never_landed_does_not_excuse_a_later_crash() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_stubborn");
        harness.add_session(session.clone(), PaneId::new(), NOW);

        // A node Turn holds no pty for: the shape of a stop request that reaches nothing.
        let mut node = ProcessNode::process(
            session.clone(),
            NodeKind::Server,
            "server --serve",
            "/tmp",
            NOW,
        );
        node.lifecycle = Lifecycle::Alive;
        let node_id = node.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session)
            .expect("the session")
            .tree
            .insert(node);

        harness.core.signal_node(&node_id, false, NOW);
        assert!(
            harness.core.expected_exits.is_empty(),
            "nothing was signalled, so no exit was asked for"
        );

        // It then dies on its own. That is a crash, and a crash has to say so.
        harness.core.record_exit(
            &session,
            &node_id,
            turn_pty::ExitInfo {
                code: 1,
                signal: Some("Killed".to_string()),
            },
            NOW + 60_000,
        );
        let after = harness
            .core
            .sessions
            .get(&session)
            .expect("the session")
            .tree
            .get(&node_id)
            .expect("the node")
            .lifecycle
            .clone();
        assert!(
            after.is_failure(),
            "a crash recorded as an expected stop raises nothing: {after:?}"
        );
        let logged = harness
            .core
            .store
            .events()
            .list_for_session(&session, 20)
            .expect("the log must be readable");
        assert!(
            logged.iter().any(|event| matches!(
                event.kind,
                turn_core::event::EventKind::ProcessFailed { .. }
            )),
            "the log must record the failure: {logged:#?}"
        );
    }

    /// A delivered signal is not a death. `SIGTERM` is a request, and a program that
    /// catches it — an interactive shell, or routinely a child an agent spawned — goes on
    /// running. An expectation with no bound then waits for an exit that has nothing to do
    /// with the stop, so the crash that eventually kills that process is filed as
    /// something the user asked for: `Stopped`, which is not a failure, raising no trigger
    /// and no notification. The log stops being quiet and starts being wrong.
    #[tokio::test]
    async fn a_signal_a_process_ignored_does_not_excuse_a_crash_much_later() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_ignores_term");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        harness.allow_test_processes(&session);

        // A shell that refuses `SIGTERM`, which is the ordinary case rather than an
        // exotic one: `kill` will report success and the process will still be there.
        let node_id = harness
            .core
            .spawn_init_command(&session, "trap '' TERM; sleep 30", NOW)
            .expect("the start-up command must run");
        harness.core.signal_node(&node_id, false, NOW);

        // Four hours later it dies for its own reasons. That is a crash, whatever was
        // asked of it this morning, and a crash has to say so.
        harness.core.record_exit(
            &session,
            &node_id,
            turn_pty::ExitInfo {
                code: 1,
                signal: Some("Killed".to_string()),
            },
            NOW + 4 * 60 * 60 * 1_000,
        );
        let after = harness
            .core
            .sessions
            .get(&session)
            .expect("the session")
            .tree
            .get(&node_id)
            .expect("the node")
            .lifecycle
            .clone();
        assert!(
            after.is_failure(),
            "a crash recorded as a stop the user asked for raises nothing: {after:?}"
        );
        let logged = harness
            .core
            .store
            .events()
            .list_for_session(&session, 20)
            .expect("the log must be readable");
        assert!(
            logged.iter().any(|event| matches!(
                event.kind,
                turn_core::event::EventKind::ProcessFailed { .. }
            )),
            "the log must record the failure: {logged:#?}"
        );
    }

    /// The other half of the same rule: an exit that never comes must not leave the
    /// expectation sitting in the table waiting to be spent on something else.
    #[tokio::test]
    async fn a_stop_request_the_process_outlived_is_forgotten_rather_than_saved_up() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_outlived");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        harness.allow_test_processes(&session);

        let node_id = harness
            .core
            .spawn_init_command(&session, "trap '' TERM; sleep 30", NOW)
            .expect("the start-up command must run");
        harness.core.signal_node(&node_id, false, NOW);
        assert!(
            harness.core.expected_exits.contains_key(&node_id),
            "a signal that was delivered is a stop the user asked for"
        );

        // Still within the window: the process may yet be shutting down, and honouring
        // the request is the whole point of recording it.
        harness
            .core
            .forget_stale_stop_requests(NOW + crate::core::EXPECTED_EXIT_GRACE_MS - 1);
        assert!(
            harness.core.expected_exits.contains_key(&node_id),
            "a process given a moment to shut down must still count as stopped on purpose"
        );

        harness
            .core
            .forget_stale_stop_requests(NOW + crate::core::EXPECTED_EXIT_GRACE_MS + 1);
        assert!(
            harness.core.expected_exits.is_empty(),
            "a process that outlived the signal owns its next exit"
        );
    }

    #[test]
    fn resuming_claude_code_passes_its_own_session_id() {
        let target = ResumeTarget {
            adapter: "claude-code".into(),
            external_id: Some("84cde77e-f54f-41e7-bb05-2716cb61b6bf".into()),
            resumable: true,
        };
        assert_eq!(
            resume_arguments(&target).unwrap(),
            vec![
                "--resume".to_string(),
                "84cde77e-f54f-41e7-bb05-2716cb61b6bf".to_string()
            ]
        );
    }

    #[test]
    fn a_resume_that_cannot_be_honoured_is_refused_rather_than_started_fresh() {
        // The failure this prevents: the user asks to continue a conversation, Turn
        // starts a new one, and the agent has forgotten everything without saying so.
        let unknown_tool = ResumeTarget {
            adapter: "some-other-agent".into(),
            external_id: Some("thread-1".into()),
            resumable: true,
        };
        let error = resume_arguments(&unknown_tool).expect_err("must refuse");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(
            error.message.contains("without resume"),
            "{}",
            error.message
        );

        let no_id = ResumeTarget {
            adapter: "claude-code".into(),
            external_id: None,
            resumable: true,
        };
        assert!(resume_arguments(&no_id).is_err());

        let not_resumable = ResumeTarget {
            adapter: "claude-code".into(),
            external_id: Some("x".into()),
            resumable: false,
        };
        assert!(resume_arguments(&not_resumable).is_err());
    }
}
