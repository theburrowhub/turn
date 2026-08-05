//! Processes ending, and the guesses made about the ones we never held.

use crate::core::{Core, FINISHED_PTY_RETENTION_MS};
use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::Relation;
use turn_core::state::Lifecycle;
use turn_pty::ExitInfo;

impl Core {
    /// Records a process ending.
    ///
    /// The pty handle is kept for now. Its buffer still holds what the process printed,
    /// and a user whose build just failed wants to read the error — throwing the screen
    /// away the moment the process dies would be the least useful possible moment. It is
    /// let go later, by [`Self::reap_finished_processes`], once nobody is watching and
    /// enough time has passed that nobody is about to.
    pub(crate) fn node_exited(&mut self, node: &NodeId, info: ExitInfo, now_ms: i64) {
        let Some(process) = self.processes.get_mut(node) else {
            return;
        };
        if process.exited_ms.is_some() {
            // Already accounted for — a watcher's report arriving after
            // [`Self::reap_finished_processes`] noticed the same death. Recording it twice
            // would put two endings in the log for one process.
            return;
        }
        let session_id = process.session_id.clone();
        // When the retention window starts, for the reaper below.
        process.exited_ms = Some(now_ms);
        if let Some(token) = process.hook_token.take() {
            // Nothing may report as this node any more.
            self.hooks.unregister(&token);
        }
        if let Some(pump) = self.pumps.remove(node) {
            pump.abort();
        }

        self.record_exit(&session_id, node, info, now_ms);
    }

    /// Lets go of the ptys of processes that have ended and nobody is watching.
    ///
    /// Without this the daemon's memory is a function of everything that has ever run
    /// in it: every exited pane, and every start-up command — which has no pane at all,
    /// so no close and no relaunch would ever have reclaimed it. Thirty sessions over
    /// several days is precisely the shape the product promises to survive.
    ///
    /// A process being watched is never reclaimed, however long ago it ended: somebody
    /// has that terminal on screen.
    ///
    /// A death nobody reported is recorded here first. Letting the pty go while the node
    /// still claims to be running would leave the daemon asserting something it has no
    /// evidence for and no handle behind — the one thing the restore path is built to
    /// avoid — and the pty knows how the process ended, so there is nothing to guess.
    pub(crate) fn reap_finished_processes(&mut self, now_ms: i64) {
        // Ended, and never written down: the exit watcher lost its channel, or its task
        // was gone before the status arrived. Collected first because recording an exit
        // takes `&mut self`.
        let unreported: Vec<(NodeId, ExitInfo)> = self
            .processes
            .iter()
            .filter(|(_, process)| process.exited_ms.is_none())
            .filter_map(|(node, process)| {
                // `None` while it runs; `Some` from the moment it does not, which is the
                // same fact `is_running` reports and the status we need with it.
                process.pty.exit_info().map(|info| (node.clone(), info))
            })
            .collect();
        for (node, info) in unreported {
            tracing::debug!(%node, "a process ended without its exit being reported");
            // The ordinary path: it stamps `exited_ms`, revokes the node's hook token and
            // writes the lifecycle, the exit code and the event.
            self.node_exited(&node, info, now_ms);
        }

        let mut finished: Vec<NodeId> = Vec::new();
        for (node, process) in self.processes.iter_mut() {
            if process.pty.is_running() {
                continue;
            }
            // Every ended process was stamped above — a pty that is not running always
            // has its status — so this is a fallback that keeps the retention clock
            // starting now rather than never, without an unwrap.
            let ended = *process.exited_ms.get_or_insert(now_ms);
            if now_ms.saturating_sub(ended) >= FINISHED_PTY_RETENTION_MS {
                finished.push(node.clone());
            }
        }

        for node in finished {
            if self.is_watched(&node) {
                continue;
            }
            self.discard_process(&node);
            tracing::debug!(%node, "let go of a finished process's terminal");
        }
    }

    /// Writes an exit into the tree and the event log.
    ///
    /// Separate from [`Self::node_exited`] because a pane being closed takes the pty with
    /// it: there is then no handle left to look the session up through, and the exit still
    /// has to be recorded rather than leaving a node that claims to be running.
    pub(crate) fn record_exit(
        &mut self,
        session_id: &SessionId,
        node: &NodeId,
        info: ExitInfo,
        now_ms: i64,
    ) {
        let session_id = session_id.clone();
        // Spent whether or not it still applies — the exit it was waiting for has now
        // happened, one way or another — but it only *excuses* the exit while the stop
        // request can plausibly explain it. A `SIGTERM` the process caught and ignored
        // an hour ago explains nothing about the crash that finally killed it, and
        // filing that crash as a deliberate stop would raise no failure at all.
        let expected = self
            .expected_exits
            .remove(node)
            .is_some_and(|applies_until| now_ms <= applies_until);
        let lifecycle = exit_lifecycle(&info, expected);

        if let Some(session) = self.sessions.get_mut(&session_id) {
            if let Some(node) = session.tree.get_mut(node) {
                node.lifecycle = lifecycle.clone();
                node.ended_ms = Some(now_ms);
                node.exit_code = Some(info.code);
            }
        }

        tracing::info!(
            %node, session = %session_id, code = info.code, signal = ?info.signal,
            expected, "a process ended"
        );

        // A process the user asked to stop has not failed, so it does not raise the
        // failure trigger and does not notify them about something they did on purpose.
        let kind = if expected || (info.code == 0 && info.signal.is_none()) {
            EventKind::ProcessExited { code: info.code }
        } else {
            EventKind::ProcessFailed {
                // `None` for a signal death: `portable-pty` reports one with a
                // meaningless exit code of 1 (ADR-010), and passing that on would
                // record a status the process never returned.
                code: info.signal.is_none().then_some(info.code),
                // The event vocabulary types this as a signal *number*, and there is no
                // number to put in it — see `signal_note` for where the name goes.
                signal: None,
            }
        };
        let mut event = TurnEvent::new(
            session_id.clone(),
            kind,
            EventSource::Supervisor,
            Confidence::Explicit,
            now_ms,
        )
        .with_node(node.clone());
        if let Some(note) = signal_note(&info) {
            event = event.with_raw(note);
        }
        self.ingest(event, now_ms);

        // Children we only ever saw through the process table go with their parent:
        // we cannot see them any more and will not claim they are running.
        self.orphan_inferred_children(&session_id, node, now_ms);
        self.request_sweep(now_ms);
    }

    /// Marks inferred children of a dead node as no longer observable.
    pub(super) fn orphan_inferred_children(
        &mut self,
        session_id: &SessionId,
        parent: &NodeId,
        now_ms: i64,
    ) {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        let children: Vec<NodeId> = session
            .tree
            .children(parent)
            .into_iter()
            .filter(|node| node.relation == Relation::Inferred && node.is_running())
            .map(|node| node.id.clone())
            .collect();
        if children.is_empty() {
            return;
        }
        for child in &children {
            if let Some(node) = session.tree.get_mut(child) {
                // `Lost`, not `Exited`: we never held this process and did not see it
                // end. Claiming a clean exit would be inventing an exit code.
                node.lifecycle = Lifecycle::Lost;
                node.ended_ms = Some(now_ms);
            }
            self.attention.resolve_node(child);
        }
        self.push_tree(session_id, now_ms);
    }

    /// Runs output inference for the panes that have it, and feeds anything it
    /// concludes through the same pipeline as a hook callback.
    pub(crate) fn observe_heuristics(&mut self, now_ms: i64) {
        let mut inferred = Vec::new();
        for (node, process) in self.processes.iter_mut() {
            let Some(heuristic) = process.heuristic.as_mut() else {
                continue;
            };
            if !process.pty.is_running() {
                continue;
            }
            let Some(snapshot) = process.pty.snapshot() else {
                continue;
            };
            let ctx = turn_agents::EventContext {
                session_id: process.session_id.clone(),
                node_id: node.clone(),
                timestamp_ms: now_ms,
            };
            inferred.extend(heuristic.observe(&snapshot, now_ms, &ctx));
        }
        for event in inferred {
            self.ingest(event, now_ms);
        }
    }
}

/// The lifecycle to record for an exit, given whether the user asked for it.
///
/// A signal death Turn *requested* is recorded as a clean end, and that is the whole
/// point: [`turn_core::state::DisplayState`] derives `Failed` from any signal, so
/// leaving it as [`Lifecycle::Signaled`] paints a red "failed" row for something the
/// user did deliberately — and a badge that fires when nothing is wrong teaches people
/// to ignore the one that matters. Nothing is lost by it. The exit code the platform
/// reported stays on the node, and the signal's own name is in the event log.
///
/// Only signal deaths are neutralised. A process the user asked to stop that chose to
/// exit non-zero on its way out keeps that status, because that is the process's own
/// word about itself rather than an artefact of how it was stopped — `portable-pty`
/// reports a signal death with a meaningless code of 1 (ADR-010), and
/// [`Core::stop_and_release`](crate::core::Core::stop_and_release) synthesises 137 or
/// 143 for a pty it closed, so in the signalled case there is no real status to keep.
fn exit_lifecycle(info: &ExitInfo, expected: bool) -> Lifecycle {
    // A stop the user asked for is recorded as `Stopped`, which is not a failure,
    // rather than rewritten into a clean exit. Fabricating `Exited { code: 0 }`
    // would have made a deliberate stop indistinguishable from a process that
    // really did exit successfully, and would have discarded the signal name —
    // exactly the invented information ADR-010 refuses.
    match (&info.signal, expected) {
        (Some(signal), true) => Lifecycle::Stopped {
            signal: signal.clone(),
        },
        _ => info.lifecycle(),
    }
}

/// How a signalled process died, for the event log.
///
/// [`EventKind::ProcessFailed`] types its `signal` as a number and the platform gives a
/// name ("Killed", "Terminated") — ADR-010 is explicit that converting one to the other
/// would invent information — so the name travels in the event's `raw` field, which
/// exists for exactly this: the source's own account, kept verbatim. Without it the log
/// cannot say whether a process was killed or merely exited, which is the difference
/// between two very different mornings.
fn signal_note(info: &ExitInfo) -> Option<String> {
    let signal = info.signal.as_deref()?;
    Some(format!("signal={signal} code={}", info.code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::clients::Attachment;
    use crate::core::testing::Harness;
    use crate::core::Command;
    use turn_core::ids::{PaneId, SessionId};

    const NOW: i64 = 1_775_000_000_000;

    /// Waits for the exit watcher to report, then applies it the way the loop does.
    async fn run_until_it_exits(harness: &mut Harness, node: &NodeId, now_ms: i64) {
        let info = loop {
            match harness.commands.recv().await {
                Some(Command::Exited { node: exited, info }) if &exited == node => break info,
                Some(_) => continue,
                None => panic!("the exit watcher went away"),
            }
        };
        harness.core.node_exited(node, info, now_ms);
    }

    /// Takes the exit report off the channel and throws it away.
    ///
    /// This is what a lost watcher looks like from the state owner's side: the process
    /// really has ended, and nothing ever told the core about it. Taking the report is how
    /// the test knows the process is finished without waiting on a clock.
    async fn discard_the_exit_report(harness: &mut Harness, node: &NodeId) -> ExitInfo {
        loop {
            match harness.commands.recv().await {
                Some(Command::Exited { node: exited, info }) if &exited == node => return info,
                Some(_) => continue,
                None => panic!("the exit watcher went away"),
            }
        }
    }

    /// Counts the endings the log holds for a session.
    fn endings(harness: &Harness, session: &SessionId) -> usize {
        harness
            .core
            .store
            .events()
            .list_for_session(session, 50)
            .expect("the log must be readable")
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    EventKind::ProcessExited { .. } | EventKind::ProcessFailed { .. }
                )
            })
            .count()
    }

    /// The reaper lets go of a pty, and the node it belonged to has to be able to answer
    /// for itself afterwards. If the exit was never reported, the node still carries the
    /// lifecycle it was launched with — so reaping alone would leave the daemon presenting
    /// a running process with no process and no terminal behind it. Honesty about what we
    /// know is the rule the restore path is built around, and it does not stop applying
    /// because the news arrived by an unusual route.
    #[tokio::test]
    async fn a_process_reaped_without_its_exit_being_reported_still_says_what_happened() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_unreported");
        harness.add_session(session.clone(), PaneId::from_stored("pane_unreported"), NOW);
        harness.allow_test_processes(&session);

        let node = harness
            .core
            .spawn_init_command(&session, "exit 7", NOW)
            .expect("the start-up command must run");
        let report = discard_the_exit_report(&mut harness, &node).await;
        assert_eq!(
            report.code, 7,
            "the process really did end with that status"
        );
        assert!(
            harness
                .core
                .sessions
                .get(&session)
                .expect("the session")
                .tree
                .get(&node)
                .expect("the node")
                .is_running(),
            "nothing has told the core about this exit yet"
        );

        harness.core.reap_finished_processes(NOW + 1_000);

        {
            let recorded = harness
                .core
                .sessions
                .get(&session)
                .expect("the session")
                .tree
                .get(&node)
                .expect("the node")
                .clone();
            assert_eq!(
                recorded.lifecycle,
                Lifecycle::Exited { code: 7 },
                "a node whose pty is being let go must not claim to be running"
            );
            assert_eq!(recorded.exit_code, Some(7));
            assert_eq!(recorded.ended_ms, Some(NOW + 1_000));
        }
        assert_eq!(endings(&harness, &session), 1);

        // A report that finds its way in afterwards changes nothing: one process ends once,
        // and a second ending in the log would be an event the user never lived through.
        harness.core.node_exited(&node, report, NOW + 2_000);
        assert_eq!(
            endings(&harness, &session),
            1,
            "one process must not end twice"
        );
        assert_eq!(
            harness
                .core
                .sessions
                .get(&session)
                .expect("the session")
                .tree
                .get(&node)
                .expect("the node")
                .ended_ms,
            Some(NOW + 1_000),
            "the moment it ended must not be rewritten by a late report"
        );

        // And the terminal is still reclaimed on the ordinary schedule.
        harness
            .core
            .reap_finished_processes(NOW + 1_000 + FINISHED_PTY_RETENTION_MS);
        assert!(
            !harness.core.processes.contains_key(&node),
            "a finished process nobody is watching must not be held for the daemon's lifetime"
        );
    }

    /// The daemon is meant to hold thirty sessions for days. A pty handle kept for every
    /// process that ever ran is a memory leak with a schedule, and a start-up command is
    /// the worst case: it has no pane, so no close and no relaunch would ever reach it.
    #[tokio::test]
    async fn a_finished_process_nobody_is_watching_gives_its_terminal_back() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_reaped");
        let pane = PaneId::from_stored("pane_reaped");
        harness.add_session(session.clone(), pane.clone(), NOW);
        harness.allow_test_processes(&session);

        let node = harness
            .core
            .spawn_init_command(&session, "exit 0", NOW)
            .expect("the start-up command must run");
        run_until_it_exits(&mut harness, &node, NOW).await;

        // Kept at first, on purpose: the buffer still holds why it failed.
        assert!(harness.core.processes.contains_key(&node));
        harness.core.reap_finished_processes(NOW + 1_000);
        assert!(
            harness.core.processes.contains_key(&node),
            "a terminal must not be taken away while the user is still reading it"
        );

        // Somebody watching keeps it, however long ago it ended.
        let (client, _frames) = harness.add_client(8);
        harness
            .core
            .clients
            .get_mut(&client)
            .expect("the client")
            .attachments
            .insert(
                (session.clone(), pane.clone()),
                Attachment {
                    node_id: Some(node.clone()),
                    stream: turn_proto::PaneStream::Cells,
                    next_seq: 0,
                    owed_gap: 0,
                    owes_full_screen: false,
                },
            );
        harness
            .core
            .reap_finished_processes(NOW + FINISHED_PTY_RETENTION_MS * 10);
        assert!(
            harness.core.processes.contains_key(&node),
            "somebody has that terminal on screen"
        );

        harness.core.client_closed(client);
        harness
            .core
            .reap_finished_processes(NOW + FINISHED_PTY_RETENTION_MS);
        assert!(
            !harness.core.processes.contains_key(&node),
            "a finished process nobody is watching must not be held for the daemon's lifetime"
        );

        // What happened to it is still on the record: only the scrollback went.
        let node_view = harness
            .core
            .sessions
            .get(&session)
            .expect("the session")
            .tree
            .get(&node)
            .expect("the node stays in the tree");
        assert!(node_view.lifecycle.is_terminal());
        assert_eq!(node_view.exit_code, Some(0));
    }

    /// A signal death is the one exit where the code is meaningless, so the name is the
    /// only account of it there is. Losing it means the log cannot answer "was it killed,
    /// or did it fail?" — and applying the event must not wipe what the platform said.
    #[tokio::test]
    async fn the_log_says_how_a_signalled_process_died_and_the_node_keeps_its_status() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_signalled");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        harness.allow_test_processes(&session);

        let node = harness
            .core
            .spawn_init_command(&session, "sleep 30", NOW)
            .expect("the start-up command must run");
        // A death nobody asked for: an out-of-memory kill, not a stop request.
        harness.core.record_exit(
            &session,
            &node,
            turn_pty::ExitInfo {
                code: 1,
                signal: Some("Killed".to_string()),
            },
            NOW + 5_000,
        );

        let recorded = harness
            .core
            .sessions
            .get(&session)
            .expect("the session")
            .tree
            .get(&node)
            .expect("the node");
        assert_eq!(
            recorded.lifecycle,
            Lifecycle::Signaled {
                signal: "Killed".to_string()
            }
        );
        assert_eq!(
            recorded.exit_code,
            Some(1),
            "applying the event must not erase what the platform reported"
        );

        let logged = harness
            .core
            .store
            .events()
            .list_for_session(&session, 20)
            .expect("the log must be readable");
        let failure = logged
            .iter()
            .find(|event| matches!(event.kind, EventKind::ProcessFailed { .. }))
            .expect("a signal death nobody asked for is a failure");
        assert!(
            failure
                .raw
                .as_deref()
                .is_some_and(|raw| raw.contains("Killed")),
            "the log cannot say how it died: {:?}",
            failure.raw
        );
    }

    #[test]
    fn a_process_the_user_asked_to_stop_is_not_recorded_as_a_failure() {
        // The failure this prevents: the user kills a build on purpose and the sidebar
        // shows it in red, alongside the crashes that actually need them.
        let killed = ExitInfo {
            code: 137,
            signal: Some("Killed".to_string()),
        };
        let lifecycle = exit_lifecycle(&killed, true);
        assert!(!lifecycle.is_failure(), "{lifecycle:?}");
        assert!(lifecycle.is_terminal());
        assert_eq!(
            turn_core::state::DisplayState::derive(&lifecycle, None),
            turn_core::state::DisplayState::Stopped
        );

        // The same death nobody asked for is still a failure, and still says how it died.
        let unexpected = exit_lifecycle(&killed, false);
        assert_eq!(
            unexpected,
            Lifecycle::Signaled {
                signal: "Killed".to_string()
            }
        );
        assert!(unexpected.is_failure());
    }

    #[test]
    fn a_process_that_exits_on_its_own_keeps_its_status_whoever_asked() {
        // Stopping something on purpose does not rewrite what it said on the way out.
        let failed = ExitInfo {
            code: 3,
            signal: None,
        };
        let kept = Lifecycle::Exited { code: 3 };
        assert_eq!(exit_lifecycle(&failed, true), kept);
        assert_eq!(exit_lifecycle(&failed, false), kept);

        let clean = ExitInfo {
            code: 0,
            signal: None,
        };
        assert_eq!(exit_lifecycle(&clean, true), Lifecycle::Exited { code: 0 });
    }

    #[test]
    fn the_log_records_the_platforms_own_name_for_the_signal() {
        let terminated = ExitInfo {
            code: 143,
            signal: Some("Terminated".to_string()),
        };
        let note = signal_note(&terminated).expect("a signal death has a note");
        assert!(note.contains("Terminated"), "{note}");
        assert!(note.contains("143"), "{note}");
        // An ordinary exit has no signal to describe, and inventing a note for one
        // would put "signal=" in the log for every process that ever ended.
        assert!(signal_note(&ExitInfo {
            code: 1,
            signal: None
        })
        .is_none());
    }
}
