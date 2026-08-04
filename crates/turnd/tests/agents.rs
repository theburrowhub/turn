//! What agents report, and what Turn does about it.
//!
//! The hook payloads are posted to the daemon's own loopback server, with the token it
//! issued, exactly as an agent's hook engine does — and translated by the production
//! Claude Code adapter. Nothing about the signal path is simulated.

mod common;

use common::agent::*;
use common::*;
use turn_core::attention::Effect;
use turn_core::event::Confidence;
use turn_core::model::PaneKind;
use turn_core::state::{AwaitingReason, DisplayState, Turn};
use turn_proto::{NewPane, Request, ServerEvent};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notification_that_the_agent_needs_input_puts_the_session_in_the_queue() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "needs input").await;

    post_hook(
        &agent.hook,
        &notification("agent_needs_input", "Waiting for your reply"),
    )
    .await;

    wait_for_state(&mut ui, &agent.session, DisplayState::WaitingForUser).await;

    // The node itself says why, on the axis that means it.
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert_eq!(
        details.tree[0].turn,
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Input
        })
    );
    assert!(details.summary.needs_user);
    assert!(
        details.tree[0].lifecycle.is_running(),
        "the process is fine; it is the turn that is waiting"
    );

    // And it is in the queue, at explicit confidence, because the tool said so.
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(entries.len(), 1, "{entries:#?}");
    assert_eq!(entries[0].entry.session_id, agent.session);
    assert_eq!(entries[0].entry.reason, AwaitingReason::Input);
    assert!(
        !entries[0].provisional,
        "a hook callback is a fact, not a guess"
    );
    assert_eq!(entries[0].session_name, "needs input");

    let next = attention_of(ui.ask(Request::NextAttention).await).expect("a next demand");
    assert_eq!(next.entry.session_id, agent.session);
    assert_eq!(next.entry.node_id.as_ref(), Some(&agent.node));

    // The user answers. `UserPromptSubmit` is the agent telling us a new turn began, and
    // that is what clears the demand — Turn never decides it has been dealt with.
    post_hook(&agent.hook, &fixtures()["UserPromptSubmit"]).await;
    wait_for_state(&mut ui, &agent.session, DisplayState::Running).await;

    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert!(entries.is_empty(), "the demand must clear: {entries:#?}");
    assert!(attention_of(ui.ask(Request::NextAttention).await).is_none());

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert_eq!(details.tree[0].turn, Some(Turn::Active));
    assert_eq!(
        details.tree[0]
            .agent
            .as_ref()
            .unwrap()
            .current_task
            .as_deref(),
        Some("Reply with exactly: OK"),
        "the prompt from the recorded payload"
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_request_is_carried_in_full_and_never_answered_by_turn() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "needs permission").await;

    post_hook(
        &agent.hook,
        &notification("permission_prompt", "Claude wants to run make verify"),
    )
    .await;
    wait_for_state(&mut ui, &agent.session, DisplayState::NeedsPermission).await;

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    let pending = details.tree[0]
        .agent
        .as_ref()
        .and_then(|agent| agent.pending_permission.clone())
        .expect("the pending permission");
    assert_eq!(pending.summary, "Claude wants to run make verify");
    assert!(
        pending.cwd.is_some(),
        "the directory travels with the request: approving in the wrong repo is the \
         mistake this field prevents"
    );

    // A permission is the one moment the agent is burning wall-clock on us, so a *fact*
    // may move the user. The governor cleared it, and it arrived as an effect.
    let focused = ui
        .wait_for("a focus effect", |event| match event {
            ServerEvent::AttentionEffect {
                effect:
                    Effect::Focus {
                        session_id,
                        node_id,
                    },
            } => Some((session_id.clone(), node_id.clone())),
            _ => None,
        })
        .await;
    assert_eq!(focused.0, agent.session);
    assert_eq!(focused.1.as_ref(), Some(&agent.node));

    // Nothing Turn can do resolves it. The permission is still pending after the daemon
    // has had every chance to be clever, and it can only be answered by typing.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert!(
        details.tree[0]
            .agent
            .as_ref()
            .and_then(|agent| agent.pending_permission.as_ref())
            .is_some(),
        "Turn must not have approved anything on the user's behalf"
    );
    assert_eq!(details.summary.display_state, DisplayState::NeedsPermission);

    daemon.shutdown().await;
}

/// Case D from the brief: a subagent appearing is information, not an interruption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subagent_joins_the_tree_with_a_confirmed_link_and_does_not_move_the_user() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "spawning subagents").await;

    post_hook(&agent.hook, &subagent_start("Explore", "sub-1")).await;

    let nodes = ui
        .wait_for("the tree to gain a subagent", |event| match event {
            ServerEvent::TreeChanged { session_id, nodes }
                if session_id == &agent.session && nodes.len() == 2 =>
            {
                Some(nodes.clone())
            }
            _ => None,
        })
        .await;

    let subagent = nodes
        .iter()
        .find(|node| node.kind == turn_core::model::NodeKind::Subagent)
        .expect("the subagent");
    assert_eq!(subagent.parent.as_ref(), Some(&agent.node));
    assert_eq!(
        subagent.relationship.kind,
        turn_core::model::RelationshipKind::SpawnedBy,
        "the tool reported this itself; it is not a guess"
    );
    assert_eq!(subagent.relationship.confidence, Confidence::Explicit);
    assert!(!subagent.relationship_is_provisional);
    assert_eq!(subagent.depth, 1);
    assert_eq!(subagent.title, "Explore");
    assert_eq!(
        subagent.pid, None,
        "a subagent runs inside its parent, so inventing a pid would name nothing"
    );
    assert_eq!(nodes[0].child_count, 1);

    // Nothing was allowed to take the user anywhere. The badge is the whole response.
    ui.poll_events().await;
    let effects: Vec<&Effect> = ui
        .buffered()
        .filter_map(|event| match event {
            ServerEvent::AttentionEffect { effect } => Some(effect),
            _ => None,
        })
        .collect();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Badge { .. })),
        "a subagent appearing is worth a badge: {effects:#?}"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Focus { .. })),
        "a subagent appearing must never move the user: {effects:#?}"
    );

    // And it leaves again when the tool says so.
    post_hook(
        &agent.hook,
        &serde_json::json!({
            "hook_event_name": "SubagentStop",
            "agent_id": "sub-1",
            "session_id": fixture_session_id(),
        }),
    )
    .await;
    let nodes = ui
        .wait_for("the subagent to finish", |event| match event {
            ServerEvent::TreeChanged { session_id, nodes } if session_id == &agent.session => nodes
                .iter()
                .find(|node| node.kind == turn_core::model::NodeKind::Subagent)
                .filter(|node| !node.lifecycle.is_running())
                .map(|_| nodes.clone()),
            _ => None,
        })
        .await;
    assert_eq!(nodes.len(), 2, "a finished subagent stays visible");

    daemon.shutdown().await;
}

/// Case E: the turn is over and the work is not, and those are different claims.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_ending_while_work_continues_does_not_read_as_finished() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "still building").await;

    // The recorded `Stop` payload, with the background task list it really carries.
    let mut stop = fixtures()["Stop"].clone();
    stop["background_tasks"] = serde_json::json!([
        { "id": "task-1", "description": "cargo test" },
        { "id": "task-2", "description": "vite dev" },
    ]);
    post_hook(&agent.hook, &stop).await;

    let event = ui
        .wait_for("the turn to complete", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event } => match &turn_event.kind {
                turn_core::event::EventKind::AgentTurnCompleted {
                    background_tasks, ..
                } => Some(*background_tasks),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(
        event, 2,
        "Claude Code tells us how much work it left running; Turn must keep the number"
    );

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert_eq!(details.tree[0].turn, Some(Turn::Done));
    assert_eq!(
        details.summary.display_state,
        DisplayState::CompletedTurn,
        "`turn done`, not `done`"
    );
    assert_ne!(details.summary.display_state, DisplayState::CompletedTask);
    assert!(
        details.tree[0].lifecycle.is_running(),
        "the process is still alive; the turn ending said nothing about it"
    );
    assert_eq!(details.summary.running_count, 1);

    daemon.shutdown().await;
}

/// Cases B and F: a guess stays a guess, and the user outranks it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guess_never_moves_the_user_and_never_overrides_them() {
    let daemon = TestDaemon::start_with(inferring_registry).await;
    let mut ui = daemon.connect().await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "inferred".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "a tool that cannot report".to_string(),
            cwd: None,
            // `sh` is claimed by an adapter that only ever infers, so the daemon attaches
            // the output heuristic and the test can put any screen in front of it.
            panes: Some(vec![NewPane::new(PaneKind::Agent).with_command("sh")]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let node = details.tree[0].node_id.clone();

    // A confirmation box on screen. The heuristic reports it as a demand for the user.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: node.clone(),
        data: turn_proto::TerminalBytes::new(b"echo 'Apply this change? (y/n)'\n".to_vec()),
    })
    .await;

    let event = ui
        .wait_for("the heuristic to reach a verdict", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event } => matches!(
                turn_event.source,
                turn_core::event::EventSource::PtyHeuristic { .. }
            )
            .then(|| turn_event.clone()),
            _ => None,
        })
        .await;
    assert_eq!(
        event.confidence,
        turn_core::Confidence::InferredHigh,
        "a heuristic cannot promote itself"
    );
    assert!(!event.confidence.may_steal_focus());

    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(entries.len(), 1, "{entries:#?}");
    assert!(
        entries[0].provisional,
        "the queue must show a guess as a guess"
    );

    // Nothing the heuristic said was allowed to move the user.
    ui.poll_events().await;
    assert!(
        !ui.buffered().any(|event| matches!(
            event,
            ServerEvent::AttentionEffect {
                effect: Effect::Focus { .. }
            }
        )),
        "a heuristic must never take the user anywhere"
    );

    // The user says it is wrong: the shell is not asking them anything.
    let corrected = node_of(
        ui.ask(Request::CorrectState {
            session_id: session.id.clone(),
            node_id: node.clone(),
            lifecycle: None,
            turn: Some(Turn::Active),
            note: Some("that is just an echo".to_string()),
        })
        .await,
    );
    assert_eq!(corrected.turn, Some(Turn::Active));
    assert_eq!(corrected.display_state, DisplayState::Running);
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert!(
        entries.is_empty(),
        "correcting the state clears what it had raised: {entries:#?}"
    );

    // The correction is recorded as the user's, at explicit confidence.
    let recorded = ui
        .wait_for("the correction in the event log", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event } => matches!(
                turn_event.source,
                turn_core::event::EventSource::UserCorrection
            )
            .then(|| turn_event.clone()),
            _ => None,
        })
        .await;
    assert_eq!(recorded.confidence, turn_core::Confidence::Explicit);
    assert!(
        recorded
            .raw
            .as_deref()
            .is_some_and(|raw| raw.contains("that is just an echo")),
        "the user's own words are kept so a misfiring rule can be found later"
    );

    // Now the screen changes, and the heuristic reaches a different verdict. It must not
    // be able to put its own guess back over the user's correction.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: node.clone(),
        data: turn_proto::TerminalBytes::new(b"echo 'Do you want to continue?'\n".to_vec()),
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    assert_eq!(
        details.tree[0].turn,
        Some(Turn::Active),
        "the user's correction stands"
    );
    assert_eq!(details.summary.display_state, DisplayState::Running);
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert!(
        entries.is_empty(),
        "a refused guess raises nothing either: {entries:#?}"
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hook_post_without_a_valid_token_is_refused_and_counted() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "guarded").await;

    // Another process on the machine, guessing. Nothing else may report as this session.
    let forged = format!(
        "{}/hook/{}",
        daemon.handle().hook_base_url(),
        "0".repeat(64)
    );
    let response = reqwest::Client::new()
        .post(&forged)
        .json(&notification("agent_needs_input", "let me in"))
        .send()
        .await
        .expect("the hook server must answer");
    assert_eq!(response.status().as_u16(), 404);
    assert_eq!(daemon.handle().hook_stats().refused, 1);

    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert!(entries.is_empty(), "a forged post must change nothing");

    // The real token still works.
    post_hook(&agent.hook, &notification("agent_needs_input", "real")).await;
    wait_for_state(&mut ui, &agent.session, DisplayState::WaitingForUser).await;
    assert_eq!(daemon.handle().hook_stats().accepted, 1);

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agents_process_ending_takes_its_demand_out_of_the_queue() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "about to die").await;

    post_hook(&agent.hook, &notification("agent_needs_input", "Answer me")).await;
    wait_for_state(&mut ui, &agent.session, DisplayState::WaitingForUser).await;
    assert_eq!(
        attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await).len(),
        1
    );

    ui.ask(Request::TerminateNode {
        session_id: agent.session.clone(),
        node_id: agent.node.clone(),
    })
    .await;

    // A dead agent cannot still be waiting for you. This is the queue entry the brief
    // says must not sit there for the rest of the day.
    ui.wait_for("the process to end", |event| match event {
        ServerEvent::NodeStateChanged {
            node_id,
            lifecycle,
            display_state,
            ..
        } if node_id == &agent.node && !lifecycle.is_running() => Some(*display_state),
        _ => None,
    })
    .await;
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert!(entries.is_empty(), "{entries:#?}");

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert!(!details.summary.needs_user);
    assert!(!details.summary.display_state.demands_user());

    // A stopped process can be started again, and only because the user asked.
    let relaunched = node_of(
        ui.ask(Request::RelaunchNode {
            session_id: agent.session.clone(),
            node_id: agent.node.clone(),
            resume: false,
        })
        .await,
    );
    assert!(relaunched.lifecycle.is_running());
    assert_ne!(
        relaunched.node_id, agent.node,
        "a new process is a new node"
    );
    assert!(pid_is_alive(relaunched.pid.expect("a pid")));

    daemon.shutdown().await;
}

/// The one thing a client is told it must never do, checked from the other side: the
/// protocol offers no way to approve, and the daemon offers no way either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answering_an_agent_is_a_keystroke_and_nothing_else() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "asking").await;

    post_hook(
        &agent.hook,
        &notification("permission_prompt", "Delete the branch?"),
    )
    .await;
    wait_for_state(&mut ui, &agent.session, DisplayState::NeedsPermission).await;

    // `cat` echoes, which is exactly what a terminal does with what the user types. The
    // point is that the only way to say "yes" is to send the bytes for it.
    let pane = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    )
    .layout
    .panes()[0]
        .id
        .clone();
    ui.attach_cells(&agent.session, &pane, turn_proto::PtySize::new(24, 80))
        .await;
    ui.ask(Request::WritePty {
        session_id: agent.session.clone(),
        node_id: agent.node.clone(),
        data: turn_proto::TerminalBytes::new(b"y\n".to_vec()),
    })
    .await;
    let echoed = ui.wait_for_screen("y").await;
    assert!(echoed.contains('y'));

    // The agent reports the outcome; Turn does not decide it. Until it does, the
    // permission is still pending.
    post_hook(
        &agent.hook,
        &serde_json::json!({
            "hook_event_name": "PermissionDenied",
            "session_id": fixture_session_id(),
        }),
    )
    .await;
    ui.wait_for("the permission to resolve", |event| match event {
        ServerEvent::TurnEventEmitted { turn_event } => matches!(
            turn_event.kind,
            turn_core::event::EventKind::AgentPermissionResolved { allowed: false }
        )
        .then_some(()),
        _ => None,
    })
    .await;

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert!(
        details.tree[0]
            .agent
            .as_ref()
            .and_then(|agent| agent.pending_permission.as_ref())
            .is_none(),
        "the agent said it was resolved, so the pending permission is gone"
    );

    daemon.shutdown().await;
}
