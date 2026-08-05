//! The two-axis state model.
//!
//! Turn deliberately refuses to collapse "the process is running" and "the agent
//! owes me a reply" into one enum. They answer different questions and they
//! change independently: Claude Code can finish its turn while a `npm test` it
//! spawned keeps running for another minute, and a shell can stay alive forever
//! without ever owing the user anything.
//!
//! * [`Lifecycle`] tracks the OS process.
//! * [`Turn`] tracks the conversational turn, and only exists for agents.
//!
//! [`DisplayState`] is the *derived* projection the UI renders. It is never
//! stored and never assigned directly — it is a pure function of the two axes,
//! which is what keeps them from drifting apart.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Where an OS process is in its life. Independent of any agent semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Lifecycle {
    /// We have asked the OS for a process but have no pid yet.
    Spawning,
    /// Running, and we hold its pty/handle.
    Alive,
    /// Exited on its own terms.
    Exited { code: i32 },
    /// Killed by a signal nobody asked for. Carries the platform's own name for
    /// it ("Killed", "Terminated") rather than a number: that is what the OS
    /// gives us, and inventing a numeric mapping would only lose information.
    Signaled { signal: String },
    /// Ended by a signal the user asked Turn to send.
    ///
    /// Distinct from [`Lifecycle::Signaled`] because stopping something on
    /// purpose is not a failure, and showing it in red teaches people to ignore
    /// red. It is a separate variant rather than a flag on `Signaled`, and
    /// certainly rather than a fabricated `Exited { code: 0 }`, because the
    /// signal name is real information: overwriting it to make the state look
    /// tidy would lose the platform's own account of how the process died, which
    /// ADR-010 exists to preserve.
    Stopped { signal: String },
    /// Still running after a UI restart, but we no longer own its handle.
    /// Turn can see it in the process table and nothing more.
    Orphaned,
    /// Running and successfully re-attached after a daemon or UI restart.
    Reconnected,
    /// Was running before a restart and cannot be found or re-attached.
    /// This is an honest "we don't know", never a silent respawn.
    Lost,
}

impl Lifecycle {
    /// Whether the process is believed to be executing right now.
    pub fn is_running(&self) -> bool {
        matches!(
            self,
            Lifecycle::Spawning | Lifecycle::Alive | Lifecycle::Orphaned | Lifecycle::Reconnected
        )
    }

    /// Whether the process reached a terminal state we observed.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Lifecycle::Exited { .. }
                | Lifecycle::Signaled { .. }
                | Lifecycle::Stopped { .. }
                | Lifecycle::Lost
        )
    }

    /// Whether this ended badly. A non-zero exit or a signal counts; a clean
    /// exit does not.
    pub fn is_failure(&self) -> bool {
        match self {
            Lifecycle::Exited { code } => *code != 0,
            Lifecycle::Signaled { .. } => true,
            // A stop the user asked for is not a failure.
            Lifecycle::Stopped { .. } => false,
            _ => false,
        }
    }
}

/// Why an agent is waiting on the human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingReason {
    /// Asked an open question and stopped.
    Question,
    /// Wants approval to run a tool or command.
    Permission,
    /// Generic "your move" — the prompt is idle and it expects input.
    Input,
    /// Needs credentials or an auth flow completed.
    Credentials,
}

impl AwaitingReason {
    /// Baseline urgency. A blocked permission costs the agent more wall-clock
    /// than an idle prompt, so it outranks it in the attention queue.
    pub fn base_priority(&self) -> u8 {
        match self {
            AwaitingReason::Permission => 90,
            AwaitingReason::Credentials => 85,
            AwaitingReason::Question => 70,
            AwaitingReason::Input => 50,
        }
    }
}

/// Where an agent is in the conversational loop. Only agents have this axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Turn {
    /// Launched, not yet in a turn.
    Idle,
    /// Actively working on the user's request.
    Active,
    /// Stopped and waiting on the human. This is what `YOUR TURN` means.
    AwaitingUser { reason: AwaitingReason },
    /// Finished this turn. The agent is not necessarily done with the task, and
    /// its child processes may well still be running.
    Done,
    /// Reported the whole task complete, not merely the turn.
    TaskDone,
    /// Errored out. The process may still be alive.
    Failed { reason: String },
    /// No adapter could tell us anything. Never guessed at.
    Unknown,
}

impl Turn {
    /// Whether the human is being waited on.
    pub fn needs_user(&self) -> bool {
        matches!(self, Turn::AwaitingUser { .. })
    }
}

/// The flattened state the UI shows. Derived, never stored.
///
/// This is the vocabulary from the product brief; deriving it rather than
/// storing it means the sidebar can never show `completed_turn` for a process
/// that actually crashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayState {
    Starting,
    Running,
    WaitingForUser,
    NeedsPermission,
    AskingQuestion,
    CompletedTurn,
    CompletedTask,
    Failed,
    Stopped,
    Idle,
    Unknown,
}

impl DisplayState {
    /// Projects the two axes onto the flat vocabulary.
    ///
    /// Order matters. A dead process outranks any stale turn state, because a
    /// crashed agent that last reported `AwaitingUser` must not sit in the queue
    /// forever pretending it is still waiting for you.
    pub fn derive(lifecycle: &Lifecycle, turn: Option<&Turn>) -> Self {
        // A failed process is failed regardless of what the agent last said.
        if lifecycle.is_failure() {
            return DisplayState::Failed;
        }
        if lifecycle.is_terminal() {
            // A clean exit after reporting the task done is a completed task,
            // not merely a stopped process.
            if matches!(turn, Some(Turn::TaskDone)) {
                return DisplayState::CompletedTask;
            }
            return DisplayState::Stopped;
        }

        match turn {
            None => match lifecycle {
                Lifecycle::Spawning => DisplayState::Starting,
                _ => DisplayState::Running,
            },
            Some(Turn::Failed { .. }) => DisplayState::Failed,
            Some(Turn::AwaitingUser { reason }) => match reason {
                AwaitingReason::Permission => DisplayState::NeedsPermission,
                AwaitingReason::Question => DisplayState::AskingQuestion,
                AwaitingReason::Credentials | AwaitingReason::Input => DisplayState::WaitingForUser,
            },
            Some(Turn::Active) => DisplayState::Running,
            Some(Turn::Done) => DisplayState::CompletedTurn,
            Some(Turn::TaskDone) => DisplayState::CompletedTask,
            Some(Turn::Idle) => match lifecycle {
                Lifecycle::Spawning => DisplayState::Starting,
                _ => DisplayState::Idle,
            },
            Some(Turn::Unknown) => DisplayState::Unknown,
        }
    }

    /// Whether this state is one the user must act on.
    pub fn demands_user(&self) -> bool {
        matches!(
            self,
            DisplayState::WaitingForUser
                | DisplayState::NeedsPermission
                | DisplayState::AskingQuestion
        )
    }

    /// Short label for one process/node. Session-level attention is projected
    /// separately as `YOUR TURN`; a waiting Agent remains honestly `WAITING`.
    pub fn label(&self) -> &'static str {
        match self {
            DisplayState::Starting => "starting",
            DisplayState::Running => "running",
            DisplayState::WaitingForUser => "WAITING",
            DisplayState::NeedsPermission => "PERMISSION",
            DisplayState::AskingQuestion => "QUESTION",
            DisplayState::CompletedTurn => "turn done",
            DisplayState::CompletedTask => "done",
            DisplayState::Failed => "failed",
            DisplayState::Stopped => "stopped",
            DisplayState::Idle => "idle",
            DisplayState::Unknown => "unknown",
        }
    }

    /// Rank used when several sessions compete for the sidebar's attention.
    /// Higher sorts first.
    pub fn severity(&self) -> u8 {
        match self {
            DisplayState::Failed => 100,
            DisplayState::NeedsPermission => 90,
            DisplayState::AskingQuestion => 80,
            DisplayState::WaitingForUser => 70,
            DisplayState::CompletedTask => 40,
            DisplayState::CompletedTurn => 35,
            DisplayState::Running => 20,
            DisplayState::Starting => 15,
            DisplayState::Idle => 10,
            DisplayState::Stopped => 5,
            DisplayState::Unknown => 1,
        }
    }
}

impl fmt::Display for DisplayState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headline case from the product brief: an agent finishes its turn
    /// while the process it started is still running. These must not collapse.
    #[test]
    fn finishing_a_turn_is_not_the_same_as_exiting() {
        let state = DisplayState::derive(&Lifecycle::Alive, Some(&Turn::Done));
        assert_eq!(state, DisplayState::CompletedTurn);
        assert!(Lifecycle::Alive.is_running());

        let exited = DisplayState::derive(&Lifecycle::Exited { code: 0 }, Some(&Turn::Done));
        assert_eq!(exited, DisplayState::Stopped);
    }

    #[test]
    fn a_crashed_process_never_keeps_claiming_it_awaits_you() {
        // The agent's last word was "waiting for you", then it died.
        let turn = Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        };
        let state = DisplayState::derive(&Lifecycle::Exited { code: 1 }, Some(&turn));
        assert_eq!(state, DisplayState::Failed);
        assert!(!state.demands_user(), "a dead agent must leave the queue");
    }

    #[test]
    fn a_clean_exit_after_task_done_reads_as_done_not_stopped() {
        let state = DisplayState::derive(&Lifecycle::Exited { code: 0 }, Some(&Turn::TaskDone));
        assert_eq!(state, DisplayState::CompletedTask);
    }

    #[test]
    fn signals_count_as_failure() {
        let killed = Lifecycle::Signaled {
            signal: "Killed".into(),
        };
        assert!(killed.is_failure());
        assert!(killed.is_terminal());
        assert_eq!(DisplayState::derive(&killed, None), DisplayState::Failed);
    }

    #[test]
    fn each_awaiting_reason_maps_to_its_own_display_state() {
        let cases = [
            (AwaitingReason::Permission, DisplayState::NeedsPermission),
            (AwaitingReason::Question, DisplayState::AskingQuestion),
            (AwaitingReason::Input, DisplayState::WaitingForUser),
            (AwaitingReason::Credentials, DisplayState::WaitingForUser),
        ];
        for (reason, expected) in cases {
            let turn = Turn::AwaitingUser { reason };
            let got = DisplayState::derive(&Lifecycle::Alive, Some(&turn));
            assert_eq!(got, expected, "reason {reason:?}");
            assert!(got.demands_user());
        }
    }

    #[test]
    fn a_waiting_agent_is_not_itself_labelled_your_turn() {
        assert_eq!(DisplayState::WaitingForUser.label(), "WAITING");
        assert!(DisplayState::WaitingForUser.demands_user());
    }

    /// Stopping something on purpose is not a failure, and the distinction must
    /// not be made by fabricating an exit code: the signal name is real
    /// information the platform gave us, and ADR-010 exists to keep it.
    #[test]
    fn a_stop_the_user_asked_for_is_not_a_failure_and_still_names_its_signal() {
        let stopped = Lifecycle::Stopped {
            signal: "Terminated".into(),
        };
        assert!(!stopped.is_failure(), "the user asked for this");
        assert!(stopped.is_terminal());
        assert!(!stopped.is_running());
        assert_eq!(
            DisplayState::derive(&stopped, None),
            DisplayState::Stopped,
            "a deliberate stop must not read as red"
        );

        // And an unasked-for kill of the same signal still does.
        let killed = Lifecycle::Signaled {
            signal: "Terminated".into(),
        };
        assert!(killed.is_failure());
        assert_eq!(DisplayState::derive(&killed, None), DisplayState::Failed);

        // The signal survives in both cases; nothing is overwritten to look tidy.
        match stopped {
            Lifecycle::Stopped { signal } => assert_eq!(signal, "Terminated"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_plain_process_with_no_agent_axis_is_just_running() {
        assert_eq!(
            DisplayState::derive(&Lifecycle::Alive, None),
            DisplayState::Running
        );
        assert_eq!(
            DisplayState::derive(&Lifecycle::Spawning, None),
            DisplayState::Starting
        );
    }

    #[test]
    fn a_lost_process_is_reported_rather_than_assumed_dead_or_alive() {
        // Lost is terminal for display purposes but carries its own label so the
        // UI can say "we could not re-attach" instead of inventing an exit code.
        assert!(Lifecycle::Lost.is_terminal());
        assert!(!Lifecycle::Lost.is_failure());
        assert_eq!(
            DisplayState::derive(&Lifecycle::Lost, None),
            DisplayState::Stopped
        );
    }

    #[test]
    fn permission_outranks_a_merely_idle_prompt() {
        assert!(AwaitingReason::Permission.base_priority() > AwaitingReason::Input.base_priority());
        assert!(DisplayState::NeedsPermission.severity() > DisplayState::WaitingForUser.severity());
        assert!(DisplayState::Failed.severity() > DisplayState::NeedsPermission.severity());
    }

    #[test]
    fn reconnected_processes_are_treated_as_running() {
        assert!(Lifecycle::Reconnected.is_running());
        assert!(Lifecycle::Orphaned.is_running());
        assert_eq!(
            DisplayState::derive(&Lifecycle::Reconnected, Some(&Turn::Active)),
            DisplayState::Running
        );
    }
}
