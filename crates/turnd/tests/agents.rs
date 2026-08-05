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
use turn_core::model::{NodeKind, PaneKind};
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
                if session_id == &agent.session && nodes.len() == 2 =>
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
    let parents: Vec<_> = details
        .tree
        .iter()
        .filter(|node| node.kind == NodeKind::Agent && node.parent.is_none())
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
            "hook_event_name": "SubagentStart",
            "agent_name": "Reviewer",
            "agent_type": "Explore",
            "agent_id": "sub-reviewer",
            "task": "Review the climbing logic changes",
            "session_id": fixture_session_id(),
            "cwd": "/private/tmp"
        }),
    )
    .await;

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

    // And it leaves again when the tool says so.
    post_hook(
        &agent.hook,
        &serde_json::json!({
            "hook_event_name": "SubagentStop",
            "agent_id": "sub-reviewer",
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
