//! What agents report, and what Turn does about it.
//!
//! The hook payloads are posted to the daemon's own loopback server, with the token it
//! issued, exactly as an agent's hook engine does — and translated by the production
//! Claude Code adapter. Nothing about the signal path is simulated.

mod common;

use common::agent::*;
use common::*;
use turn_core::attention::Effect;
use turn_core::event::{Confidence, EventKind, EventSource};
use turn_core::model::{NodeKind, PaneKind, PanePlacement};
use turn_core::state::{AwaitingReason, DisplayState, Turn};
use turn_proto::{CloseDisposition, HierarchyKey, NewPane, Request, Response, ServerEvent};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notification_that_the_agent_needs_input_puts_the_session_in_the_queue() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "needs input").await;

    post_hook(
        &agent.hook,
        &notification("idle_prompt", "Waiting for your reply"),
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
        agent_row(&details).turn,
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Input
        })
    );
    assert!(details.summary.needs_user);
    assert!(
        agent_row(&details).lifecycle.is_running(),
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
    assert_eq!(agent_row(&details).turn, Some(Turn::Active));
    assert_eq!(
        agent_row(&details)
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
    let pending = agent_row(&details)
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
        agent_row(&details)
            .agent
            .as_ref()
            .and_then(|agent| agent.pending_permission.as_ref())
            .is_some(),
        "Turn must not have approved anything on the user's behalf"
    );
    assert_eq!(details.summary.display_state, DisplayState::NeedsPermission);

    daemon.shutdown().await;
}

/// P1 regression: Claude delivers worker notifications through the parent's hook
/// and may omit `agent_id`. With one declared child the tree is enough to correlate
/// at inferred-high confidence; answering clears that child only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idless_worker_permission_round_trips_through_hooks_to_the_reviewer() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "idless worker attention").await;

    post_hook(
        &agent.hook,
        &serde_json::json!({
            "hook_event_name": "SubagentStart",
            "agent_name": "Reviewer",
            "agent_type": "Explore",
            "agent_id": "sub-reviewer-idless",
            "task": "Review the climbing diff",
            "session_id": fixture_session_id(),
            "cwd": "/private/tmp"
        }),
    )
    .await;
    let reviewer = ui
        .wait_for("Reviewer in the hierarchy", |event| match event {
            ServerEvent::TreeChanged { session_id, nodes }
                // Three rows: the pane's shell, the agent Turn started in it, and the
                // worker the agent declared.
                if session_id == &agent.session && nodes.len() == 3 =>
            {
                nodes
                    .iter()
                    .find(|node| node.title == "Reviewer")
                    .map(|node| node.node_id.clone())
            }
            _ => None,
        })
        .await;

    let payload = notification(
        "worker_permission_prompt",
        "Reviewer needs permission to run tests",
    );
    assert!(
        payload.get("agent_id").is_none(),
        "the regression requires the real id-less hook shape"
    );
    post_hook(&agent.hook, &payload).await;

    let permission = ui
        .wait_for(
            "the correlated worker permission event",
            |event| match event {
                ServerEvent::TurnEventEmitted { turn_event }
                    if turn_event.session_id == agent.session
                        && matches!(
                            &turn_event.kind,
                            EventKind::AgentPermissionRequired { .. }
                        ) =>
                {
                    Some(turn_event.clone())
                }
                _ => None,
            },
        )
        .await;
    assert_eq!(permission.node_id.as_ref(), Some(&reviewer));
    assert_eq!(permission.parent_node_id.as_ref(), Some(&agent.node));
    assert_eq!(permission.confidence, Confidence::InferredHigh);
    assert!(matches!(
        permission.source,
        EventSource::Hook {
            ref tool,
            ref event_name
        } if tool == "claude-code" && event_name == "Notification"
    ));

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert_eq!(
        details
            .tree
            .iter()
            .find(|node| node.node_id == reviewer)
            .unwrap()
            .turn,
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission
        })
    );
    let parent = details
        .tree
        .iter()
        .find(|node| node.node_id == agent.node)
        .unwrap();
    assert!(
        parent.turn.as_ref().is_some_and(|turn| !turn.needs_user()),
        "the parent did not ask for this permission: {:?}",
        parent.turn
    );
    assert!(
        parent
            .agent
            .as_ref()
            .and_then(|agent| agent.pending_permission.as_ref())
            .is_none(),
        "the permission detail belongs only to Reviewer"
    );
    let queued = attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(agent.session.clone()),
        })
        .await,
    );
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].entry.node_id.as_ref(), Some(&reviewer));
    assert!(queued[0].provisional);

    let next = attention_of(ui.ask(Request::NextAttention).await).expect("Reviewer is next");
    assert_eq!(next.entry.node_id.as_ref(), Some(&reviewer));
    let goto = effects_of(
        ui.ask(Request::GotoAttention {
            attention_id: Some(next.entry.id.clone()),
        })
        .await,
    );
    assert!(goto.iter().any(|effect| matches!(
        effect,
        Effect::Focus {
            session_id,
            node_id: Some(node_id),
        } if session_id == &agent.session && node_id == &reviewer
    )));

    let surface = "semantic-attention-window".to_string();
    ui.ask(Request::GetHierarchy {
        surface_id: surface.clone(),
        include_archived: false,
    })
    .await;
    let routed = ui
        .ask(Request::FocusPaneForAttention {
            surface_id: surface,
            session_id: agent.session.clone(),
            subject_node_id: reviewer.clone(),
        })
        .await;
    assert!(
        matches!(
            routed,
            Response::PaneFocus {
                focus: Some(ref focus)
            } if focus.attention_subject_node_id.as_ref() == Some(&reviewer)
                // The Pane represents the parent Agent while its terminal resolver sends
                // input to the Shell-owned PTY. Reviewer remains the exact demand subject.
                && focus.node_id == agent.node
        ),
        "unexpected Attention focus route: {routed:?}"
    );

    let prompt = fixtures()["UserPromptSubmit"].clone();
    assert!(prompt.get("agent_id").is_none());
    post_hook(&agent.hook, &prompt).await;
    let resumed = ui
        .wait_for("the correlated worker resume event", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event }
                if turn_event.session_id == agent.session
                    && matches!(&turn_event.kind, EventKind::AgentTurnStarted { .. }) =>
            {
                Some(turn_event.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(resumed.node_id.as_ref(), Some(&reviewer));
    assert_eq!(resumed.parent_node_id.as_ref(), Some(&agent.node));
    assert_eq!(resumed.confidence, Confidence::InferredHigh);

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert_eq!(
        details
            .tree
            .iter()
            .find(|node| node.node_id == reviewer)
            .unwrap()
            .turn,
        Some(Turn::Active)
    );
    assert!(attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(agent.session.clone()),
        })
        .await
    )
    .is_empty());

    // Claude's other id-less worker hand-off follows the same path. This is a
    // separate hook subtype, so exercise it through the transport as well as the
    // adapter unit contract.
    post_hook(
        &agent.hook,
        &notification("agent_needs_input", "Reviewer needs a decision"),
    )
    .await;
    let waiting = ui
        .wait_for("the correlated worker input event", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event }
                if turn_event.session_id == agent.session
                    && matches!(&turn_event.kind, EventKind::AgentWaitingForUser { .. }) =>
            {
                Some(turn_event.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(waiting.node_id.as_ref(), Some(&reviewer));
    assert_eq!(waiting.parent_node_id.as_ref(), Some(&agent.node));
    assert_eq!(waiting.confidence, Confidence::InferredHigh);
    let queued = attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(agent.session.clone()),
        })
        .await,
    );
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].entry.node_id.as_ref(), Some(&reviewer));
    assert_eq!(queued[0].entry.reason, AwaitingReason::Input);

    post_hook(&agent.hook, &prompt).await;
    ui.wait_for("Reviewer to resume a second time", |event| match event {
        ServerEvent::TurnEventEmitted { turn_event }
            if turn_event.session_id == agent.session
                && turn_event.node_id.as_ref() == Some(&reviewer)
                && matches!(&turn_event.kind, EventKind::AgentTurnStarted { .. }) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    assert!(attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(agent.session.clone()),
        })
        .await
    )
    .is_empty());

    daemon.shutdown().await;
}

/// Adversarial correlation regression: two authenticated parent runtimes share a
/// Session, callbacks arrive before declarations, and id-less resumes interleave.
/// No event may borrow a sibling's identity or clear another parent's demand.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_hook_parents_keep_out_of_order_and_idless_attention_in_their_own_scopes() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let root = daemon
        .data_dir()
        .join("workspaces")
        .join(turn_core::ids::WorkspaceId::new().as_str());
    std::fs::create_dir_all(&root).expect("two-parent workspace root");
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "two-parent-correlation".into(),
            root: root.display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "two authenticated parents".into(),
            cwd: None,
            panes: Some(vec![
                NewPane::new(PaneKind::Agent).with_command("cat"),
                NewPane::new(PaneKind::Agent).with_command("cat"),
            ]),
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
    // Each agent hangs off the shell of its own pane, so neither is a root — what makes
    // them independent is that they are different agents in different terminals, which
    // is the only thing this test needs of them.
    let parents: Vec<_> = details
        .tree
        .iter()
        .filter(|node| node.kind == NodeKind::Agent)
        .map(|node| node.node_id.clone())
        .collect();
    assert_eq!(
        parents.len(),
        2,
        "the test needs two independent hook parents"
    );
    let parent_a = parents[0].clone();
    let parent_b = parents[1].clone();
    let hook_a = hook_url(daemon.data_dir(), &session.id, &parent_a);
    let hook_b = hook_url(daemon.data_dir(), &session.id, &parent_b);

    // A different child already exists when a callback names Future Reviewer.
    // The old bug assigned that callback to Existing simply because it was the
    // only child visible at that instant.
    post_hook(
        &hook_a,
        &serde_json::json!({
            "hook_event_name": "SubagentStart",
            "agent_name": "Existing",
            "agent_type": "Explore",
            "agent_id": "worker-existing",
            "session_id": fixture_session_id(),
        }),
    )
    .await;
    let existing = ui
        .wait_for("Existing under parent A", |event| match event {
            ServerEvent::TreeChanged { session_id, nodes } if session_id == &session.id => nodes
                .iter()
                .find(|node| node.title == "Existing")
                .map(|node| node.node_id.clone()),
            _ => None,
        })
        .await;

    let mut out_of_order = notification(
        "worker_permission_prompt",
        "Future Reviewer needs permission",
    );
    out_of_order["agent_id"] = serde_json::json!("worker-future-reviewer");
    post_hook(&hook_a, &out_of_order).await;
    let unresolved = ui
        .wait_for("the out-of-order worker demand", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event }
                if turn_event.session_id == session.id
                    && matches!(
                        &turn_event.kind,
                        EventKind::AgentPermissionRequired { summary, .. }
                            if summary == "Future Reviewer needs permission"
                    ) =>
            {
                Some(turn_event.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(unresolved.node_id, None);
    assert_eq!(unresolved.parent_node_id.as_ref(), Some(&parent_a));
    assert_eq!(
        unresolved.agent.external_id.as_deref(),
        Some("worker-future-reviewer")
    );
    assert_eq!(unresolved.confidence, Confidence::Unknown);
    let queued = attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(session.id.clone()),
        })
        .await,
    );
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].entry.node_id, None);
    assert_eq!(queued[0].entry.parent_node_id.as_ref(), Some(&parent_a));
    assert_eq!(
        queued[0].entry.subject_external_id.as_deref(),
        Some("worker-future-reviewer")
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    assert_eq!(
        details
            .tree
            .iter()
            .find(|node| node.node_id == existing)
            .unwrap()
            .turn,
        Some(Turn::Active),
        "an explicit unknown id must not fall through to the unique other child"
    );

    // The missing declarations arrive. Parent A now has two children and parent
    // B gets two as well, making every following id-less worker callback
    // deliberately ambiguous within its own subtree.
    post_hook(
        &hook_a,
        &serde_json::json!({
            "hook_event_name": "SubagentStart",
            "agent_name": "Future Reviewer",
            "agent_type": "Explore",
            "agent_id": "worker-future-reviewer",
            "session_id": fixture_session_id(),
        }),
    )
    .await;
    let future_reviewer = ui
        .wait_for("Future Reviewer under parent A", |event| match event {
            ServerEvent::TreeChanged { session_id, nodes } if session_id == &session.id => nodes
                .iter()
                .find(|node| node.title == "Future Reviewer")
                .map(|node| node.node_id.clone()),
            _ => None,
        })
        .await;
    for (name, id) in [
        ("B Reviewer", "worker-b-reviewer"),
        ("B Tests", "worker-b-tests"),
    ] {
        post_hook(
            &hook_b,
            &serde_json::json!({
                "hook_event_name": "SubagentStart",
                "agent_name": name,
                "agent_type": "Explore",
                "agent_id": id,
                "session_id": fixture_session_id(),
            }),
        )
        .await;
        ui.wait_for(name, |event| match event {
            ServerEvent::TreeChanged { session_id, nodes } if session_id == &session.id => {
                nodes.iter().any(|node| node.title == name).then_some(())
            }
            _ => None,
        })
        .await;
    }

    post_hook(
        &hook_a,
        &notification("worker_permission_prompt", "an A worker needs permission"),
    )
    .await;
    ui.wait_for("A's id-less demand", |event| match event {
        ServerEvent::TurnEventEmitted { turn_event }
            if turn_event.session_id == session.id
                && turn_event.parent_node_id.as_ref() == Some(&parent_a)
                && turn_event.node_id.is_none()
                && matches!(&turn_event.kind, EventKind::AgentPermissionRequired { .. }) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    post_hook(
        &hook_b,
        &notification("worker_permission_prompt", "a B worker needs permission"),
    )
    .await;
    ui.wait_for("B's id-less demand", |event| match event {
        ServerEvent::TurnEventEmitted { turn_event }
            if turn_event.session_id == session.id
                && turn_event.parent_node_id.as_ref() == Some(&parent_b)
                && turn_event.node_id.is_none()
                && matches!(&turn_event.kind, EventKind::AgentPermissionRequired { .. }) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    let queued = attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(session.id.clone()),
        })
        .await,
    );
    assert_eq!(queued.len(), 3, "external A + id-less A + id-less B");

    // A's anonymous answer clears A's anonymous demand only. It cannot clear
    // the explicit unknown-id flow under A, nor B's anonymous flow.
    post_hook(&hook_a, &fixtures()["UserPromptSubmit"]).await;
    let resumed_a = ui
        .wait_for("A's scoped id-less resume", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event }
                if turn_event.session_id == session.id
                    && turn_event.parent_node_id.as_ref() == Some(&parent_a)
                    && matches!(&turn_event.kind, EventKind::AgentTurnStarted { .. }) =>
            {
                Some(turn_event.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(resumed_a.node_id, None);
    assert_eq!(resumed_a.confidence, Confidence::Unknown);
    let queued = attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(session.id.clone()),
        })
        .await,
    );
    assert_eq!(queued.len(), 2);
    assert!(queued.iter().any(|view| {
        view.entry.parent_node_id.as_ref() == Some(&parent_a)
            && view.entry.subject_external_id.as_deref() == Some("worker-future-reviewer")
    }));
    assert!(queued.iter().any(|view| {
        view.entry.parent_node_id.as_ref() == Some(&parent_b)
            && view.entry.subject_external_id.is_none()
    }));

    // Once the delayed identity exists, an exact callback closes only its old
    // external-id scope. B still waits until B's own parent reports a resume.
    let mut exact_resume = fixtures()["UserPromptSubmit"].clone();
    exact_resume["agent_id"] = serde_json::json!("worker-future-reviewer");
    post_hook(&hook_a, &exact_resume).await;
    let exact = ui
        .wait_for("Future Reviewer's exact resume", |event| match event {
            ServerEvent::TurnEventEmitted { turn_event }
                if turn_event.session_id == session.id
                    && matches!(&turn_event.kind, EventKind::AgentTurnStarted { .. })
                    && turn_event.agent.external_id.as_deref()
                        == Some("worker-future-reviewer") =>
            {
                Some(turn_event.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(exact.node_id.as_ref(), Some(&future_reviewer));
    let queued = attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(session.id.clone()),
        })
        .await,
    );
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].entry.parent_node_id.as_ref(), Some(&parent_b));

    post_hook(&hook_b, &fixtures()["UserPromptSubmit"]).await;
    ui.wait_for("B's scoped id-less resume", |event| match event {
        ServerEvent::TurnEventEmitted { turn_event }
            if turn_event.session_id == session.id
                && turn_event.parent_node_id.as_ref() == Some(&parent_b)
                && matches!(&turn_event.kind, EventKind::AgentTurnStarted { .. }) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    assert!(attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(session.id.clone()),
        })
        .await
    )
    .is_empty());

    daemon.shutdown().await;
}

/// The upgraded first vertical over the real hook transport. A declared Reviewer is a
/// background AgentNode first; its preview, relationship and tree interaction survive a
/// UI reconnect, while its temporary Pane never mutates the saved Layout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "spawning subagents").await;
    let layout_before = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    )
    .layout;

    post_hook(
        &agent.hook,
        &serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Agent",
            "tool_use_id": "toolu_reviewer",
            "tool_input": {
                "name": "Reviewer",
                "subagent_type": "Explore",
                "description": "Review the climbing logic changes",
                "prompt": "Inspect the movement implementation and report defects"
            },
            "tool_response": {
                "status": "teammate_spawned",
                "agent_id": "Reviewer@session-test",
                "teammate_id": "Reviewer@session-test",
                "name": "Reviewer",
                "agent_type": "Explore",
                "team_name": "session-test",
                "is_splitpane": false
            },
            "session_id": fixture_session_id(),
            "cwd": "/private/tmp"
        }),
    )
    .await;

    let nodes = ui
        .wait_for("the tree to gain a subagent", |event| match event {
            ServerEvent::TreeChanged { session_id, nodes }
                // Three rows: the pane's shell, the agent Turn started in it, and the
                // worker the agent declared.
                if session_id == &agent.session && nodes.len() == 3 =>
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
    assert_eq!(
        subagent.depth, 2,
        "the pane's shell, then its agent, then the worker"
    );
    assert_eq!(subagent.title, "Reviewer");
    assert_eq!(
        subagent
            .agent
            .as_ref()
            .and_then(|agent| agent.name.declared_name.as_deref()),
        Some("Reviewer")
    );
    assert_eq!(
        subagent
            .activity_preview
            .as_ref()
            .map(|preview| preview.normalized_text.as_str()),
        Some("Review the climbing logic changes")
    );
    assert!(subagent.pane_bindings.is_empty());
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

    let surface = "reviewer-window".to_string();
    let reviewer_key = HierarchyKey::process(subagent.node_id.clone());
    let root_key = HierarchyKey::process(agent.node.clone());
    ui.ask(Request::GetHierarchy {
        surface_id: surface.clone(),
        include_archived: false,
    })
    .await;
    ui.ask(Request::SetTreeExpanded {
        surface_id: surface.clone(),
        key: root_key.clone(),
        expanded: true,
    })
    .await;
    ui.ask(Request::SelectTreeNode {
        surface_id: surface.clone(),
        selected: Some(reviewer_key.clone()),
    })
    .await;

    let history = ui
        .ask(Request::GetPreviewHistory {
            session_id: agent.session.clone(),
            node_id: subagent.node_id.clone(),
            limit: Some(5),
        })
        .await;
    assert!(matches!(
        history,
        Response::PreviewHistory { ref entries, .. }
            if entries.iter().any(|entry| entry.normalized_text == "Review the climbing logic changes")
    ));

    let temporary = match ui
        .ask(Request::OpenNodeAsTemporaryPane {
            surface_id: surface.clone(),
            session_id: agent.session.clone(),
            node_id: subagent.node_id.clone(),
        })
        .await
    {
        Response::NodePane { pane } => pane,
        other => panic!("expected a temporary Reviewer pane, got {other:?}"),
    };
    assert!(temporary.binding.temporary);
    assert!(matches!(
        temporary.capability,
        turn_proto::NodePaneCapability::PreviewDetails
    ));
    let layout_while_open = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    )
    .layout;
    assert_eq!(layout_while_open, layout_before, "Cmd+Enter must not split");

    ui.ask(Request::ClosePane {
        session_id: agent.session.clone(),
        pane_id: temporary.binding.pane_id.clone(),
        disposition: CloseDisposition::KeepProcesses,
    })
    .await;
    let after_close = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    let reviewer = after_close
        .tree
        .iter()
        .find(|node| node.node_id == subagent.node_id)
        .expect("Reviewer remains in the tree");
    assert!(
        reviewer.lifecycle.is_running(),
        "closing a view cannot stop it"
    );
    assert_eq!(after_close.layout, layout_before);

    // Dropping and reconnecting this client is the UI-restart boundary: turnd and its
    // Processes stay alive, while the next window restores only persisted tree state.
    drop(ui);
    let mut ui = daemon.connect().await;
    let restored = match ui
        .ask(Request::GetHierarchy {
            surface_id: surface,
            include_archived: false,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected the restored hierarchy, got {other:?}"),
    };
    assert_eq!(restored.tree_state.selected, Some(reviewer_key));
    assert!(restored.tree_state.expanded.contains(&root_key));
    let restored_reviewer = restored
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .flat_map(|session| &session.nodes)
        .find(|node| node.node_id == subagent.node_id)
        .expect("Reviewer survives the UI restart");
    assert_eq!(restored_reviewer.parent.as_ref(), Some(&agent.node));
    assert_eq!(
        restored_reviewer.relationship.confidence,
        Confidence::Explicit
    );
    assert!(restored_reviewer.activity_preview.is_some());
    assert!(restored_reviewer.pane_bindings.is_empty());

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
    assert_eq!(agent_row(&details).turn, Some(Turn::Done));
    assert_eq!(
        details.summary.display_state,
        DisplayState::CompletedTurn,
        "`turn done`, not `done`"
    );
    assert_ne!(details.summary.display_state, DisplayState::CompletedTask);
    assert!(
        agent_row(&details).lifecycle.is_running(),
        "the process is still alive; the turn ending said nothing about it"
    );
    assert_eq!(
        details.summary.running_count, 2,
        "the pane's shell and the agent running in it"
    );

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
    let node = agent_row(&details).node_id.clone();

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
        agent_row(&details).turn,
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
        .json(&notification("idle_prompt", "let me in"))
        .send()
        .await
        .expect("the hook server must answer");
    assert_eq!(response.status().as_u16(), 404);
    assert_eq!(daemon.handle().hook_stats().refused, 1);

    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert!(entries.is_empty(), "a forged post must change nothing");

    // The real token still works.
    post_hook(&agent.hook, &notification("idle_prompt", "real")).await;
    wait_for_state(&mut ui, &agent.session, DisplayState::WaitingForUser).await;
    assert_eq!(daemon.handle().hook_stats().accepted, 1);

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agents_process_ending_takes_its_demand_out_of_the_queue() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "about to die").await;

    post_hook(&agent.hook, &notification("idle_prompt", "Answer me")).await;
    wait_for_state(&mut ui, &agent.session, DisplayState::WaitingForUser).await;
    assert_eq!(
        attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await).len(),
        1
    );

    // Stopping the agent is a signal to the agent's own process, so Turn has to know
    // which process that is. The pane's shell is not the target: it survives this, which
    // is the whole point of the agent running inside one.
    let agent_pid = wait_for_agent_pid(&mut ui, &agent.session, &agent.node).await;
    assert!(pid_is_alive(agent_pid));
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
    assert_eq!(
        relaunched.parent.as_ref(),
        Some(&agent.shell),
        "the shell it was typed into is the same shell as before"
    );
    let fresh_pid = wait_for_agent_pid(&mut ui, &agent.session, &relaunched.node_id).await;
    assert!(pid_is_alive(fresh_pid));
    assert_ne!(fresh_pid, agent_pid, "a new process is a new pid");

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
        agent_row(&details)
            .agent
            .as_ref()
            .and_then(|agent| agent.pending_permission.as_ref())
            .is_none(),
        "the agent said it was resolved, so the pending permission is gone"
    );

    daemon.shutdown().await;
}

/// A workspace with a root of its own, so each fixture has a distinct canonical
/// checkout identity and the production lease arbiter is not fighting the test.
async fn own_workspace(
    daemon: &TestDaemon,
    ui: &mut Client,
    name: &str,
) -> turn_proto::WorkspaceSummary {
    let root = daemon
        .data_dir()
        .join("workspaces")
        .join(turn_core::ids::WorkspaceId::new().as_str());
    std::fs::create_dir_all(&root).expect("agent test Workspace root");
    workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: name.to_string(),
            root: root.display().to_string(),
        })
        .await,
    )
}

// ------------------------------------- an agent runs inside the pane's shell

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_hosted_agent_keeps_lifecycle_authority_while_another_agent_uses_its_terminal() {
    let daemon = TestDaemon::start_with(inferring_registry).await;
    let mut ui = daemon.connect().await;
    let hosted = agent_session(&daemon, &mut ui, "hosted foreground handoff").await;
    wait_for_agent_pid(&mut ui, &hosted.session, &hosted.node).await;
    let before = details_of(
        ui.ask(Request::GetSession {
            session_id: hosted.session.clone(),
        })
        .await,
    );
    let pane_id = before.layout.panes()[0].id.clone();

    ui.ask(Request::WritePty {
        session_id: hosted.session.clone(),
        node_id: hosted.node.clone(),
        data: turn_proto::TerminalBytes::new(vec![0x1a]),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: hosted.session.clone(),
            })
            .await,
        );
        if details.layout.get(&pane_id).unwrap().node_id.as_ref() == Some(&hosted.shell) {
            assert!(row(&details, &hosted.node).lifecycle.is_running());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hosted Agent did not yield terminal presentation after Ctrl-Z"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    ui.ask(Request::WritePty {
        session_id: hosted.session.clone(),
        node_id: hosted.shell.clone(),
        data: turn_proto::TerminalBytes::new(b"sh -c 'sleep 30; :'\r".to_vec()),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let replacement = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: hosted.session.clone(),
            })
            .await,
        );
        if let Some(subject) = details
            .layout
            .get(&pane_id)
            .unwrap()
            .node_id
            .as_ref()
            .filter(|subject| *subject != &hosted.shell && *subject != &hosted.node)
        {
            assert_eq!(row(&details, subject).kind, NodeKind::Agent);
            break subject.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the replacement Agent never took foreground presentation"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };

    // A's authenticated lifecycle channel remains A's while B owns only the screen.
    post_hook(&hosted.hook, &fixtures()["Stop"]).await;
    let turn = ui
        .wait_for("the background hosted Agent's hook", |event| match event {
            ServerEvent::NodeStateChanged { node_id, turn, .. }
                if node_id == &hosted.node && turn.as_ref() == Some(&Turn::Done) =>
            {
                turn.clone()
            }
            _ => None,
        })
        .await;
    assert_eq!(turn, Turn::Done);
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: hosted.session.clone(),
        })
        .await,
    );
    assert_eq!(
        details.layout.get(&pane_id).unwrap().node_id,
        Some(replacement.clone())
    );
    assert!(row(&details, &hosted.node).lifecycle.is_running());

    ui.ask(Request::WritePty {
        session_id: hosted.session.clone(),
        node_id: replacement,
        data: turn_proto::TerminalBytes::new(vec![0x03]),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: hosted.session.clone(),
            })
            .await,
        );
        if details.layout.get(&pane_id).unwrap().node_id.as_ref() == Some(&hosted.shell) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the replacement Agent did not return control to the Shell"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    ui.attach_cells(&hosted.session, &pane_id, turn_proto::PtySize::new(20, 80))
        .await;
    ui.ask(Request::WritePty {
        session_id: hosted.session.clone(),
        node_id: hosted.shell.clone(),
        data: turn_proto::TerminalBytes::new(
            b"printf 'JOBS-BEGIN\\n'; jobs -l; printf 'JOBS-END\\n'\r".to_vec(),
        ),
    })
    .await;
    ui.wait_for_screen("JOBS-END").await;
    ui.poll_screens().await;
    let jobs = ui.screen(&hosted.session, &pane_id).text();
    assert!(
        jobs.contains("suspended") || jobs.contains("Stopped"),
        "the original hosted job vanished while backgrounded:\n{jobs}"
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: hosted.session.clone(),
        })
        .await,
    );
    assert!(row(&details, &hosted.node).lifecycle.is_running());
    assert_eq!(row(&details, &hosted.node).turn, Some(Turn::Done));

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_agent_does_not_steal_the_shell_pane() {
    let daemon = TestDaemon::start_with(inferring_registry).await;
    let mut ui = daemon.connect().await;
    let root = daemon.data_dir().join("background-agent");
    std::fs::create_dir_all(&root).unwrap();
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "background agent workspace".into(),
            root: root.display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "background agent".into(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Shell)]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let before = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pane_id = before.layout.panes()[0].id.clone();
    let shell_id = before.layout.panes()[0]
        .node_id
        .clone()
        .expect("the automatic shell runtime");

    // An interactive shell puts the job in its own background process group. The
    // fixture's `sh` executable is a heuristic Agent and waits on `sleep` without
    // reading the terminal, so it remains alive for both sides of the job-control
    // transition without ever owning the foreground here.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: shell_id.clone(),
        data: turn_proto::TerminalBytes::new(b"sh -c 'sleep 30; :' &\r".to_vec()),
    })
    .await;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let (background_id, detected) =
        loop {
            let details = details_of(
                ui.ask(Request::GetSession {
                    session_id: session.id.clone(),
                })
                .await,
            );
            let pane = details.layout.get(&pane_id).unwrap();
            assert_eq!(
                pane.node_id.as_ref(),
                Some(&shell_id),
                "a background Agent must never become the Pane subject"
            );
            assert_eq!(pane.presentation_kind(), PaneKind::Shell);
            if let Some(agent) = details.tree.iter().find(|node| {
                node.kind == NodeKind::Agent && node.parent.as_ref() == Some(&shell_id)
            }) {
                break (agent.node_id.clone(), details);
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the background Agent was not discovered within the debounced sweep"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
    assert_eq!(row(&detected, &background_id).kind, NodeKind::Agent);
    assert_eq!(
        detected.layout.get(&pane_id).unwrap().node_id.as_ref(),
        Some(&shell_id)
    );

    // Job control changes foreground ownership without creating another PID. The
    // already-known Agent must take the Pane when `fg` resumes it rather than being
    // skipped forever merely because the discovery sweep saw it once in background.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: shell_id.clone(),
        data: turn_proto::TerminalBytes::new(b"fg\r".to_vec()),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane_id).unwrap();
        if pane.node_id.as_ref() == Some(&background_id) {
            assert_eq!(pane.presentation_kind(), PaneKind::Agent);
            assert!(row(&details, &background_id)
                .pane_bindings
                .iter()
                .any(|binding| binding.pane_id == pane_id));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "an Agent moved to foreground never became the Pane subject"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let duplicated = layout_of(
        ui.ask(Request::DuplicatePane {
            session_id: session.id.clone(),
            pane_id: pane_id.clone(),
        })
        .await,
    );
    let exact_background_pane = duplicated
        .panes()
        .into_iter()
        .find(|pane| pane.id != pane_id && pane.node_id.as_ref() == Some(&background_id))
        .expect("the duplicate is an exact view of Agent A")
        .id
        .clone();
    let pinned = layout_of(
        ui.ask(Request::ChangePaneKind {
            session_id: session.id.clone(),
            pane_id: exact_background_pane.clone(),
            kind: PaneKind::Terminal,
        })
        .await,
    );
    let pinned = pinned.get(&exact_background_pane).unwrap();
    assert_eq!(pinned.presentation_kind(), PaneKind::Terminal);
    assert!(pinned.kind_is_user_set);
    assert_eq!(pinned.detected_kind, Some(PaneKind::Agent));
    assert!(pinned.has_terminal_capability());
    ui.attach_cells(&session.id, &pane_id, turn_proto::PtySize::new(20, 80))
        .await;
    ui.attach_cells(
        &session.id,
        &exact_background_pane,
        turn_proto::PtySize::new(20, 80),
    )
    .await;

    // Ctrl-Z is the inverse transition and carries no newline. Turn schedules a sweep
    // for the unchanged control byte, sees the Shell's process group regain the PTY and
    // returns presentation without pretending the stopped Agent exited.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: background_id.clone(),
        data: turn_proto::TerminalBytes::new(vec![0x1a]),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let after_background = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane_id).unwrap();
        if pane.node_id.as_ref() == Some(&shell_id) {
            assert_eq!(pane.presentation_kind(), PaneKind::Shell);
            assert!(row(&details, &shell_id)
                .pane_bindings
                .iter()
                .any(|binding| binding.pane_id == pane_id));
            assert!(
                row(&details, &background_id).lifecycle.is_running(),
                "backgrounding changes foreground ownership, not lifecycle"
            );
            break details;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the Shell did not regain its Pane after Ctrl-Z"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let exact = after_background.layout.get(&exact_background_pane).unwrap();
    assert_eq!(exact.node_id.as_ref(), Some(&background_id));
    assert_eq!(exact.presentation_kind(), PaneKind::Terminal);
    assert!(exact.kind_is_user_set);
    assert_eq!(exact.detected_kind, Some(PaneKind::ProcessDetails));
    assert!(!exact.has_terminal_capability());

    // Agent B now owns the same Shell PTY. Neither resetting nor explicitly opening
    // the still-live Agent A may infer that shared runtime from its old relationship.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: shell_id.clone(),
        data: turn_proto::TerminalBytes::new(b"sh -c 'sleep 30; :'\r".to_vec()),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let (replacement_id, with_replacement) = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        let subject = details.layout.get(&pane_id).unwrap().node_id.as_ref();
        if let Some(subject) = subject.filter(|node| *node != &shell_id && *node != &background_id)
        {
            break (subject.clone(), details);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Agent B never became the Shell's foreground subject"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        with_replacement
            .layout
            .get(&pane_id)
            .unwrap()
            .presentation_kind(),
        PaneKind::Agent
    );

    let reset = layout_of(
        ui.ask(Request::ResetPaneKind {
            session_id: session.id.clone(),
            pane_id: exact_background_pane.clone(),
        })
        .await,
    );
    let reset_exact = reset.get(&exact_background_pane).unwrap();
    assert_eq!(reset_exact.node_id.as_ref(), Some(&background_id));
    assert_eq!(reset_exact.presentation_kind(), PaneKind::ProcessDetails);
    assert!(!reset_exact.kind_is_user_set);
    assert!(!reset_exact.has_terminal_capability());

    let surface_id = "background-binding-guard".to_string();
    let hierarchy = match ui
        .ask(Request::GetHierarchy {
            surface_id: surface_id.clone(),
            include_archived: false,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected hierarchy, got {other:?}"),
    };
    let branch = hierarchy
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .find(|branch| branch.session.id == session.id)
        .unwrap();
    assert_eq!(
        branch
            .nodes
            .iter()
            .find(|node| node.node_id == background_id)
            .unwrap()
            .pane_capability,
        turn_proto::NodePaneCapability::PreviewDetails
    );
    assert!(matches!(
        branch
            .nodes
            .iter()
            .find(|node| node.node_id == replacement_id)
            .unwrap()
            .pane_capability,
        turn_proto::NodePaneCapability::Terminal { .. }
    ));

    let opened = layout_of(
        ui.ask(Request::OpenNodeAsPane {
            surface_id,
            session_id: session.id.clone(),
            node_id: background_id.clone(),
            target_pane_id: pane_id.clone(),
            placement: PanePlacement::SplitBelow,
        })
        .await,
    );
    let opened_background_pane = opened
        .panes()
        .into_iter()
        .find(|pane| {
            pane.id != exact_background_pane
                && pane.node_id.as_ref() == Some(&background_id)
                && pane.presentation_kind() == PaneKind::ProcessDetails
        })
        .expect("opening suspended Agent A creates a ProcessDetails view")
        .id
        .clone();
    for guarded in [&exact_background_pane, &opened_background_pane] {
        let error = ui
            .try_ask(Request::AttachPane {
                session_id: session.id.clone(),
                pane_id: guarded.clone(),
                size: turn_proto::PtySize::new(20, 80),
                stream: turn_proto::PaneStream::Cells,
            })
            .await
            .expect_err("Agent A must not borrow Agent B's Shell terminal");
        assert_eq!(error.code, turn_proto::ErrorCode::Conflict);
    }

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_typed_into_a_shell_is_detected_and_becomes_the_pane_subject() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let root = daemon.data_dir().join("manually-started-agent");
    std::fs::create_dir_all(&root).unwrap();
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "manual agent workspace".into(),
            root: root.display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "manual agent".into(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Shell)]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let before = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pane_id = before.layout.panes()[0].id.clone();
    let shell_id = before.layout.panes()[0]
        .node_id
        .clone()
        .expect("the automatic shell runtime");

    // `cat` is the fixture registry's structured agent. It is started exactly as a
    // person starts Claude/Codex/Gemini/OpenCode in an existing terminal.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: shell_id.clone(),
        data: turn_proto::TerminalBytes::new(b"cat\r".to_vec()),
    })
    .await;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let (agent_id, detected) = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane_id).unwrap();
        if pane.node_id.as_ref().is_some_and(|node| node != &shell_id) {
            break (pane.node_id.clone().unwrap(), details);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the submitted command was not classified within the debounced sweep"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let pane = detected.layout.get(&pane_id).unwrap();
    assert_eq!(pane.kind, PaneKind::Agent);
    assert_eq!(
        pane.launch_kind(),
        PaneKind::Shell,
        "launch intent stays a Shell"
    );
    assert_eq!(pane.presentation_kind(), PaneKind::Agent);
    assert!(!pane.kind_is_user_set);
    let agent = row(&detected, &agent_id);
    assert_eq!(agent.kind, NodeKind::Agent);
    assert_eq!(agent.parent.as_ref(), Some(&shell_id));
    let info = agent.agent.as_ref().expect("detected agent metadata");
    assert_eq!(info.agent.tool.as_deref(), Some("claude-code"));
    assert_eq!(info.agent.provider.as_deref(), Some("anthropic"));

    let hierarchy = match ui
        .ask(Request::GetHierarchy {
            surface_id: "agent-runtime-host".into(),
            include_archived: false,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected hierarchy, got {other:?}"),
    };
    let branch = hierarchy
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .find(|branch| branch.session.id == session.id)
        .unwrap();
    assert!(
        branch
            .nodes
            .iter()
            .find(|node| node.node_id == shell_id)
            .unwrap()
            .terminal_runtime_host,
        "GetHierarchy must expose the Shell that owns the detected Agent's PTY"
    );

    // Input addressed to semantic identity still reaches the Shell-owned PTY.
    ui.attach_cells(&session.id, &pane_id, turn_proto::PtySize::new(20, 80))
        .await;
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: agent_id.clone(),
        data: turn_proto::TerminalBytes::new(b"semantic-agent-input\n".to_vec()),
    })
    .await;
    ui.wait_for_screen("semantic-agent-input").await;

    // Leaving the manually started agent returns automatic presentation to the same
    // live Shell, ready to detect the next foreground agent.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: agent_id,
        data: turn_proto::TerminalBytes::new(vec![0x04]),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane_id).unwrap();
        if pane.node_id.as_ref() == Some(&shell_id) {
            assert_eq!(pane.kind, PaneKind::Shell);
            assert_eq!(pane.presentation_kind(), PaneKind::Shell);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Pane stayed on an exited inferred Agent"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    daemon.shutdown().await;
}

/// A fast command replacement is a single supervisor observation: the old Agent has
/// vanished and the new one already exists before retirement runs. The Pane must move
/// directly A -> B rather than returning to Shell and then permanently skipping B as an
/// already-known PID.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replacement_agent_becomes_the_subject_in_the_same_sweep() {
    let daemon = TestDaemon::start_with(inferring_registry).await;
    let mut ui = daemon.connect().await;
    let root = daemon.data_dir().join("replacement-agent");
    std::fs::create_dir_all(&root).unwrap();
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "replacement agent workspace".into(),
            root: root.display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "replacement agent".into(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Shell)]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let before = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pane_id = before.layout.panes()[0].id.clone();
    let shell_id = before.layout.panes()[0]
        .node_id
        .clone()
        .expect("the automatic shell runtime");
    let shell_pid = row(&before, &shell_id).pid.expect("the Shell pid");

    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: shell_id.clone(),
        data: turn_proto::TerminalBytes::new(b"sh -c 'sleep 30; :'\r".to_vec()),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let first_agent = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane_id).unwrap();
        if let Some(subject) = pane
            .node_id
            .as_ref()
            .filter(|subject| *subject != &shell_id)
        {
            break subject.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first manual Agent was not detected"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let first_pid = wait_for_agent_pid(&mut ui, &session.id, &first_agent).await;
    let duplicated = layout_of(
        ui.ask(Request::DuplicatePane {
            session_id: session.id.clone(),
            pane_id: pane_id.clone(),
        })
        .await,
    );
    let exact_first_agent_pane = duplicated
        .panes()
        .into_iter()
        .find(|pane| pane.id != pane_id && pane.node_id.as_ref() == Some(&first_agent))
        .expect("the explicit duplicate is an exact view of Agent A")
        .id
        .clone();
    let surface_id = "replacement-first-byte-guard".to_string();
    assert!(matches!(
        ui.ask(Request::GetHierarchy {
            surface_id: surface_id.clone(),
            include_archived: false,
        })
        .await,
        Response::Hierarchy { .. }
    ));
    let temporary = match ui
        .ask(Request::OpenNodeAsTemporaryPane {
            surface_id: surface_id.clone(),
            session_id: session.id.clone(),
            node_id: first_agent.clone(),
        })
        .await
    {
        Response::NodePane { pane } => pane,
        other => panic!("expected Agent A's temporary exact pane, got {other:?}"),
    };
    assert!(temporary.binding.temporary);
    assert_eq!(temporary.binding.node_id, first_agent);
    assert!(matches!(
        temporary.capability,
        turn_proto::NodePaneCapability::Terminal { .. }
    ));
    let temporary_first_agent_pane = temporary.binding.pane_id;
    ui.attach_cells(&session.id, &pane_id, turn_proto::PtySize::new(20, 80))
        .await;
    ui.attach_cells(
        &session.id,
        &exact_first_agent_pane,
        turn_proto::PtySize::new(20, 80),
    )
    .await;
    ui.attach_cells(
        &session.id,
        &temporary_first_agent_pane,
        turn_proto::PtySize::new(20, 80),
    )
    .await;

    // Schedule one debounced sweep, then make A disappear and B appear before it runs.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: first_agent.clone(),
        data: turn_proto::TerminalBytes::new(vec![0x03]),
    })
    .await;
    let exit_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while pid_is_alive(first_pid) {
        assert!(
            tokio::time::Instant::now() < exit_deadline,
            "the first Agent fixture did not consume Ctrl-C"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: shell_id.clone(),
        // The echoed command does not contain the contiguous marker. Seeing
        // `B-FIRST` therefore proves the first output batch from Agent B arrived.
        data: turn_proto::TerminalBytes::new(
            b"sh -c \"printf 'B%s\\n' '-FIRST'; sleep 30; :\"\r".to_vec(),
        ),
    })
    .await;
    ui.wait_for_screen("B-FIRST").await;
    ui.poll_screens().await;
    assert!(ui.screen(&session.id, &pane_id).text().contains("B-FIRST"));
    assert!(
        !ui.screen(&session.id, &exact_first_agent_pane)
            .text()
            .contains("B-FIRST"),
        "Agent A's durable exact feed consumed Agent B's first output"
    );
    assert!(
        !ui.screen(&session.id, &temporary_first_agent_pane)
            .text()
            .contains("B-FIRST"),
        "Agent A's temporary exact feed consumed Agent B's first output"
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let (replacement, after) = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane_id).unwrap();
        if let Some(subject) = pane
            .node_id
            .as_ref()
            .filter(|subject| *subject != &shell_id && *subject != &first_agent)
        {
            break (subject.clone(), details);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the replacement was discovered but never became the Pane subject: {:#?}",
            details.layout
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        after.layout.get(&pane_id).unwrap().presentation_kind(),
        PaneKind::Agent
    );
    assert_eq!(row(&after, &replacement).kind, NodeKind::Agent);
    assert!(row(&after, &replacement).lifecycle.is_running());
    assert!(!row(&after, &first_agent).lifecycle.is_running());
    assert_eq!(
        after
            .layout
            .get(&exact_first_agent_pane)
            .and_then(|pane| pane.node_id.as_ref()),
        Some(&first_agent),
        "an exact duplicate of A must not follow the Shell foreground to B"
    );
    let exact = after.layout.get(&exact_first_agent_pane).unwrap();
    assert_eq!(exact.presentation_kind(), PaneKind::ProcessDetails);
    assert!(!exact.kind_is_user_set);
    assert!(!exact.has_terminal_capability());
    assert_eq!(exact.launch_kind(), PaneKind::Shell);
    let attach_error = ui
        .try_ask(Request::AttachPane {
            session_id: session.id.clone(),
            pane_id: exact_first_agent_pane.clone(),
            size: turn_proto::PtySize::new(20, 80),
            stream: turn_proto::PaneStream::Cells,
        })
        .await
        .expect_err("semantic ProcessDetails must not borrow Agent B's Shell");
    assert_eq!(attach_error.code, turn_proto::ErrorCode::Conflict);
    let temporary_attach_error = ui
        .try_ask(Request::AttachPane {
            session_id: session.id.clone(),
            pane_id: temporary_first_agent_pane.clone(),
            size: turn_proto::PtySize::new(20, 80),
            stream: turn_proto::PaneStream::Cells,
        })
        .await
        .expect_err("Agent A's temporary exact view must not borrow Agent B's Shell");
    assert_eq!(temporary_attach_error.code, turn_proto::ErrorCode::Conflict);
    let hierarchy = match ui
        .ask(Request::GetHierarchy {
            surface_id,
            include_archived: false,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected hierarchy, got {other:?}"),
    };
    let first_row = hierarchy
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .flat_map(|branch| &branch.nodes)
        .find(|node| node.node_id == first_agent)
        .expect("Agent A remains a semantic row");
    assert_eq!(
        first_row.pane_capability,
        turn_proto::NodePaneCapability::PreviewDetails
    );
    assert!(first_row
        .pane_bindings
        .iter()
        .any(|binding| binding.pane_id == temporary_first_agent_pane && binding.temporary));

    let mut reconnected = daemon.connect().await;
    let restored = details_of(
        reconnected
            .ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
    );
    let exact = restored.layout.get(&exact_first_agent_pane).unwrap();
    assert_eq!(exact.node_id.as_ref(), Some(&first_agent));
    assert_eq!(exact.presentation_kind(), PaneKind::ProcessDetails);
    assert!(!exact.has_terminal_capability());

    // Once B releases the Shell, output from that same PTY may update the foreground
    // Pane but can never repaint the exact view still labelled A.
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: replacement.clone(),
        data: turn_proto::TerminalBytes::new(vec![0x03]),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
        );
        if details.layout.get(&pane_id).unwrap().node_id.as_ref() == Some(&shell_id) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the replacement Agent did not release the Shell"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let marker = "exact-feed-guard-53";
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: shell_id.clone(),
        data: turn_proto::TerminalBytes::new(format!("printf '{marker}\\n'\r").into_bytes()),
    })
    .await;
    ui.wait_for_screen(marker).await;
    ui.poll_screens().await;
    assert!(ui.screen(&session.id, &pane_id).text().contains(marker));
    assert!(
        !ui.screen(&session.id, &exact_first_agent_pane)
            .text()
            .contains(marker),
        "Agent A's exact view consumed later output from the shared Shell"
    );
    assert_eq!(row(&after, &shell_id).pid, Some(shell_pid));
    assert!(pid_is_alive(shell_pid));

    daemon.shutdown().await;
}

/// The report: leaving Claude with `/exit` left the pane flickering, because the pane's
/// process *was* Claude. It is not any more. The pane runs the user's shell, the agent
/// runs in it, and quitting the agent gives the prompt back — which is what quitting an
/// agent in iTerm2 does.
///
/// `cat` stands in for the agent, so its EOF stands in for `/exit`: a clean exit the
/// agent chose. What proves the shell is really still working is arithmetic — `turn-42`
/// is something only a shell can print, where a program still holding the terminal would
/// merely echo the characters back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quitting_an_agent_returns_the_live_shell_and_allows_the_next_agent() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "quits cleanly").await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    let pane = details.layout.panes()[0].id.clone();
    let shell_pid = row(&details, &agent.shell)
        .pid
        .expect("the pane's own process has a pid from the moment it is spawned");
    wait_for_agent_pid(&mut ui, &agent.session, &agent.node).await;

    // End-of-transmission on the pane's tty: the agent reads it, decides it is done and
    // exits. Nothing about this is Turn stopping anything.
    ui.ask(Request::WritePty {
        session_id: agent.session.clone(),
        node_id: agent.shell.clone(),
        data: turn_proto::TerminalBytes::new(vec![0x04]),
    })
    .await;

    let ended = ui
        .wait_for("the agent to end", |event| match event {
            ServerEvent::NodeStateChanged {
                node_id,
                lifecycle,
                display_state,
                ..
            } if node_id == &agent.node && !lifecycle.is_running() => Some(*display_state),
            _ => None,
        })
        .await;
    assert!(
        !ended.demands_user(),
        "an agent that quit is not waiting for anybody: {ended:?}"
    );

    // The Pane's semantic subject returns to its runtime Shell automatically. The
    // lifecycle event and layout push are separate frames, so observe the durable
    // session state rather than relying on their delivery order.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let after = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: agent.session.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane).unwrap();
        if pane.node_id.as_ref() == Some(&agent.shell)
            && pane.presentation_kind() == PaneKind::Shell
        {
            break details;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Pane stayed bound to the hosted Agent after it exited: {:#?}",
            details.layout
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };

    // The pane never blinked: same process, same pid, still running.
    let shell = row(&after, &agent.shell);
    assert!(
        shell.lifecycle.is_running(),
        "the pane's shell outlives the agent: {:?}",
        shell.lifecycle
    );
    assert_eq!(shell.pid, Some(shell_pid), "and it is the same shell");
    assert!(
        pid_is_alive(shell_pid),
        "the kernel agrees, not only the daemon"
    );
    assert!(
        !shell.lifecycle.is_failure() && !after.summary.display_state.demands_user(),
        "quitting an agent is not a failure and demands nothing: {:#?}",
        after.summary
    );

    // And the prompt is really a prompt. Arithmetic no echo could produce.
    ui.attach_cells(&agent.session, &pane, turn_proto::PtySize::new(30, 100))
        .await;
    ui.ask(Request::WritePty {
        session_id: agent.session.clone(),
        node_id: agent.shell.clone(),
        data: turn_proto::TerminalBytes::new(b"echo turn-$((6 * 7))\n".to_vec()),
    })
    .await;
    let screen = ui.wait_for_screen("turn-42").await;
    assert!(screen.contains("turn-42"), "the screen reads {screen:?}");

    // The returned prompt is not a terminal state. Starting another recognised
    // executable manually must create a fresh Agent and hand the Pane to it, without
    // replacing the long-lived Shell process underneath.
    ui.ask(Request::WritePty {
        session_id: agent.session.clone(),
        node_id: agent.shell.clone(),
        data: turn_proto::TerminalBytes::new(b"cat\r".to_vec()),
    })
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let (next_agent, detected) = loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: agent.session.clone(),
            })
            .await,
        );
        let pane = details.layout.get(&pane).unwrap();
        if let Some(subject) = pane
            .node_id
            .as_ref()
            .filter(|subject| *subject != &agent.shell && *subject != &agent.node)
        {
            break (subject.clone(), details);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the Shell did not detect the next foreground Agent"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let next = row(&detected, &next_agent);
    assert_eq!(next.kind, NodeKind::Agent);
    assert_eq!(next.parent.as_ref(), Some(&agent.shell));
    assert_eq!(
        row(&detected, &agent.shell).pid,
        Some(shell_pid),
        "detecting the next Agent must retain the Pane's Shell runtime"
    );
    assert!(pid_is_alive(shell_pid));

    daemon.shutdown().await;
}

/// The agent is still an agent, which is the part that had to be got right: its node is
/// no longer the pane's process, so everything that makes it an agent has to travel with
/// it deliberately. Its adapter, its integration level, its turn axis, its hook endpoint
/// — and an edge to the shell that is *confirmed*, because Turn wrote the command line
/// itself. An inferred edge here would be Turn guessing about its own launch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_started_in_a_pane_shell_is_still_an_agent_in_the_tree() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "still an agent").await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );

    let shell = row(&details, &agent.shell);
    assert_eq!(shell.kind, NodeKind::Shell, "the pane's process is a shell");
    assert!(shell.turn.is_none(), "a shell owes the user nothing");
    assert_eq!(shell.depth, 0);
    assert_eq!(
        details.layout.panes()[0].node_id.as_ref(),
        Some(&agent.node),
        "the Pane represents the Agent while its terminal runtime remains the shell"
    );
    assert_eq!(details.layout.panes()[0].kind, PaneKind::Agent);
    assert!(
        !details.layout.panes()[0].kind_is_user_set,
        "the Agent view is detected, not a manual renderer label"
    );

    let row = row(&details, &agent.node);
    assert_eq!(row.kind, NodeKind::Agent);
    assert!(row.is_agentic, "it carries the turn axis");
    assert_eq!(
        row.turn,
        Some(Turn::Idle),
        "idle until it reports otherwise"
    );
    assert_eq!(row.depth, 1);
    assert_eq!(row.parent.as_ref(), Some(&agent.shell));
    assert_eq!(
        row.relationship.kind,
        turn_core::model::RelationshipKind::SpawnedBy
    );
    assert_eq!(
        row.relationship.confidence,
        Confidence::Explicit,
        "Turn typed this command itself; there is nothing to infer"
    );
    assert!(
        !row.relationship_is_provisional,
        "the edge must not be drawn as a guess"
    );
    let info = row.agent.as_ref().expect("the agent detail");
    assert_eq!(info.agent.tool.as_deref(), Some("claude-code"));
    assert_eq!(info.agent.provider.as_deref(), Some("anthropic"));
    assert!(
        matches!(
            row.pane_capability,
            turn_proto::NodePaneCapability::Terminal { .. }
        ),
        "the shell's terminal is the agent's terminal: {:?}",
        row.pane_capability
    );

    // Its hook endpoint is its own, issued to its own node, with its own scratch
    // directory — the shell has neither, and nothing may report as the shell.
    assert!(!agent.hook.is_empty());
    assert!(
        !turnd::paths::node_scratch(daemon.data_dir(), &agent.session, &agent.shell).exists(),
        "a shell needs no injected configuration and is issued no token"
    );

    daemon.shutdown().await;
}

/// A payload captured from a real Claude Code run, posted to the URL the adapter was
/// handed, still lands on the agent — not on the shell it happens to be running in, and
/// not nowhere. This is the whole integration crossing the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_captured_claude_payload_still_reaches_the_agent_under_its_pane_shell() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "hooked through a shell").await;

    post_hook(&agent.hook, &fixtures()["Stop"]).await;
    let reported = ui
        .wait_for("the turn ending", |event| match event {
            ServerEvent::NodeStateChanged { node_id, turn, .. }
                if node_id == &agent.node && turn.is_some() =>
            {
                turn.clone()
            }
            _ => None,
        })
        .await;
    assert_eq!(reported, Turn::Done);

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    assert_eq!(agent_row(&details).turn, Some(Turn::Done));
    assert_eq!(
        agent_row(&details).node_id,
        agent.node,
        "the callback resolved to the agent's own node"
    );
    assert!(
        row(&details, &agent.shell).turn.is_none(),
        "and gave the shell around it no turn state it could never fill"
    );

    daemon.shutdown().await;
}

/// The report: `+ Pane Agent` does nothing. With no default agent configured and none on
/// the machine, the pane is still the user's shell and it says why it is only that.
/// Silence was the bug; an error nobody sees would be the same bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_an_agent_pane_with_no_agent_anywhere_opens_a_shell_that_says_so() {
    // A registry with no agent adapters at all, so "nothing installed" is a fact about
    // this test rather than about the machine it runs on.
    let daemon = TestDaemon::start_with(turn_agents::AdapterRegistry::bare).await;
    let mut ui = daemon.connect().await;
    let workspace = own_workspace(&daemon, &mut ui, "no agents here").await;
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "nothing to run".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Shell)]),
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
    let first = details.layout.panes()[0].id.clone();

    let layout = layout_of(
        ui.ask(Request::SplitPane {
            session_id: session.id.clone(),
            pane_id: first.clone(),
            direction: turn_core::model::Direction::Horizontal,
            // No command, and the workspace names no default agent: exactly the
            // "+ Pane > Agent" the owner pressed.
            pane: NewPane::new(PaneKind::Agent),
        })
        .await,
    );
    let added = layout
        .panes()
        .into_iter()
        .find(|pane| pane.id != first)
        .cloned()
        .expect("the new pane");
    let node = added
        .node_id
        .clone()
        .expect("an agent pane always starts something: an empty pane is the bug");

    ui.attach_cells(&session.id, &added.id, turn_proto::PtySize::new(30, 100))
        .await;
    let screen = ui.wait_for_screen("no agent CLI on your PATH").await;
    assert!(
        screen.contains("this pane is your shell"),
        "the pane has to say what it is instead: {screen:?}"
    );

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let opened = row(&details, &node);
    assert_eq!(opened.kind, NodeKind::Shell);
    assert!(opened.lifecycle.is_running());
    assert!(
        details.tree.iter().all(|view| !view.is_agentic),
        "no agent was invented: {:#?}",
        details.tree
    );

    daemon.shutdown().await;
}

/// And with an agent CLI on the PATH but no default configured, the same press launches
/// it. Falling back to what is installed is the difference between a pane that works and
/// a pane that explains itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_an_agent_pane_with_no_configured_default_still_launches_one() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let workspace = own_workspace(&daemon, &mut ui, "no default configured").await;
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "falls back".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Shell)]),
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
    let first = details.layout.panes()[0].id.clone();

    let layout = layout_of(
        ui.ask(Request::SplitPane {
            session_id: session.id.clone(),
            pane_id: first.clone(),
            direction: turn_core::model::Direction::Horizontal,
            pane: NewPane::new(PaneKind::Agent),
        })
        .await,
    );
    let added = layout
        .panes()
        .into_iter()
        .find(|pane| pane.id != first)
        .cloned()
        .expect("the new pane");
    let subject = added
        .node_id
        .clone()
        .expect("the pane represents the launched agent");
    assert_eq!(added.kind, PaneKind::Agent);
    assert!(!added.kind_is_user_set);

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let started = details
        .tree
        .iter()
        .find(|view| view.node_id == subject)
        .expect("an agent was started without one being configured");
    assert!(started.is_agentic);
    let shell = started.parent.as_ref().expect("the agent's PTY host");
    assert_eq!(row(&details, shell).kind, NodeKind::Shell);
    assert_eq!(started.relationship.confidence, Confidence::Explicit);
    assert_eq!(
        started
            .agent
            .as_ref()
            .and_then(|info| info.agent.tool.clone()),
        Some("claude-code".to_string()),
        "the strongest integration the registry can actually find"
    );

    daemon.shutdown().await;
}

/// The other way the old pane died: ctrl-C. The interrupt goes through the tty, so it
/// reaches the agent's whole foreground process group — and the pane's shell is
/// interactive, which is what makes it carry on rather than exit alongside what it was
/// running. A non-interactive shell would take the pane down with the agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupting_a_hosted_agent_leaves_its_pane_alive() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "interrupted").await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    let shell_pid = row(&details, &agent.shell).pid.expect("the pane's own pid");
    let agent_pid = wait_for_agent_pid(&mut ui, &agent.session, &agent.node).await;

    // Addressed to the agent, delivered through the tty it is reading from. `cat` does
    // not catch it, so it dies: the observable proof that the signal was delivered.
    ui.ask(Request::InterruptNode {
        session_id: agent.session.clone(),
        node_id: agent.node.clone(),
    })
    .await;
    ui.wait_for("the agent to end", |event| match event {
        ServerEvent::NodeStateChanged {
            node_id, lifecycle, ..
        } if node_id == &agent.node && !lifecycle.is_running() => Some(()),
        _ => None,
    })
    .await;
    assert!(!pid_is_alive(agent_pid), "the agent really is gone");

    let after = details_of(
        ui.ask(Request::GetSession {
            session_id: agent.session.clone(),
        })
        .await,
    );
    let shell = row(&after, &agent.shell);
    assert!(shell.lifecycle.is_running(), "{:?}", shell.lifecycle);
    assert_eq!(shell.pid, Some(shell_pid));
    assert!(pid_is_alive(shell_pid), "the pane is still there");

    daemon.shutdown().await;
}
