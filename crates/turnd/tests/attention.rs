//! Attention coordination, end to end.
//!
//! The queue's ordering, what a mute does and does not silence, and the moment the focus
//! governor decides it may not move the user. These are the product's reason for
//! existing, so they are asserted against a running daemon rather than in isolation.

mod common;

use common::agent::*;
use common::*;
use turn_core::attention::Effect;
use turn_core::state::DisplayState;
use turn_proto::{Request, ServerEvent};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_sessions_blocking_at_once_are_ordered_and_walking_the_queue_visits_both() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let first = agent_session(&daemon, &mut ui, "asks a question").await;
    let second = agent_session(&daemon, &mut ui, "needs permission").await;

    // The idle prompt lands first, so age cannot be what decides the order.
    post_hook(
        &first.hook,
        &notification("agent_needs_input", "Which approach do you want?"),
    )
    .await;
    wait_for_state(&mut ui, &first.session, DisplayState::WaitingForUser).await;
    post_hook(
        &second.hook,
        &notification("permission_prompt", "Run the migration"),
    )
    .await;
    wait_for_state(&mut ui, &second.session, DisplayState::NeedsPermission).await;

    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(entries.len(), 2, "{entries:#?}");
    assert_eq!(
        entries[0].entry.session_id, second.session,
        "a blocked permission outranks an idle prompt, however long the prompt has waited"
    );
    assert_eq!(entries[1].entry.session_id, first.session);
    assert!(entries[0].score > entries[1].score);

    // Walking the queue reaches both, in that order, and each visit acknowledges the one
    // it landed on so the next press moves on rather than going back.
    let effects = effects_of(ui.ask(Request::GotoAttention { attention_id: None }).await);
    let visited_first = match effects.first() {
        Some(Effect::Focus { session_id, .. }) => session_id.clone(),
        other => panic!("expected a focus effect, got {other:?}"),
    };
    assert_eq!(visited_first, second.session);

    let effects = effects_of(ui.ask(Request::GotoAttention { attention_id: None }).await);
    let visited_second = match effects.first() {
        Some(Effect::Focus { session_id, .. }) => session_id.clone(),
        other => panic!("expected a focus effect, got {other:?}"),
    };
    assert_eq!(visited_second, first.session);
    assert_ne!(visited_first, visited_second, "both sessions were visited");

    // Both are still listed, acknowledged rather than resolved: the user has seen them,
    // and neither agent has been answered yet.
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| matches!(
        entry.entry.state,
        turn_core::attention::EntryState::Acknowledged
    )));

    // Filtering narrows to one session.
    let mine = attention_list_of(
        ui.ask(Request::ListAttention {
            session_id: Some(first.session.clone()),
        })
        .await,
    );
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].entry.session_id, first.session);

    // Dismissing takes one out of the queue for good.
    let doomed = entries[0].entry.id.clone();
    ui.ask(Request::DismissAttention {
        attention_id: doomed.clone(),
    })
    .await;
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(entries.len(), 1);
    assert!(entries.iter().all(|entry| entry.entry.id != doomed));

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_muted_session_still_badges_and_a_snooze_takes_a_demand_out_of_the_way() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "chatty").await;
    let now = turn_core::now_ms();

    ui.ask(Request::MuteSession {
        session_id: agent.session.clone(),
        until_ms: Some(now + 60_000),
    })
    .await;

    post_hook(
        &agent.hook,
        &notification("permission_prompt", "Run something loud"),
    )
    .await;

    // Muting silences the interruption, not the evidence: a badge still arrives, and
    // nothing else does.
    let effect = ui
        .wait_for("a badge", |event| match event {
            ServerEvent::AttentionEffect { effect } => Some(effect.clone()),
            _ => None,
        })
        .await;
    assert!(matches!(effect, Effect::Badge { .. }), "{effect:?}");
    ui.poll_events().await;
    assert!(
        !ui.buffered().any(|event| matches!(
            event,
            ServerEvent::AttentionEffect {
                effect: Effect::Focus { .. } | Effect::PlaySound { .. } | Effect::Notify { .. }
            }
        )),
        "a muted session must not make a sound or move anybody"
    );

    ui.ask(Request::MuteSession {
        session_id: agent.session.clone(),
        until_ms: None,
    })
    .await;
    post_hook(
        &agent.hook,
        &notification("agent_needs_input", "Still waiting"),
    )
    .await;
    wait_for_state(&mut ui, &agent.session, DisplayState::WaitingForUser).await;

    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    let entry = entries[0].entry.id.clone();

    // A snooze in the past would come back immediately and read as a broken button.
    let error = ui
        .try_ask(Request::SnoozeAttention {
            attention_id: entry.clone(),
            until_ms: now - 1,
        })
        .await
        .expect_err("a snooze must end in the future");
    assert_eq!(error.code, turn_proto::ErrorCode::InvalidArgument);

    ui.ask(Request::SnoozeAttention {
        attention_id: entry.clone(),
        until_ms: now + 3_600_000,
    })
    .await;
    assert!(
        attention_of(ui.ask(Request::NextAttention).await).is_none(),
        "a snoozed demand is not the next thing to do"
    );
    // Still listed, though: hiding it would make a snooze feel like a deletion.
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].actionable);

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_activity_is_what_the_governor_uses_and_it_answers_with_effects() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let agent = agent_session(&daemon, &mut ui, "typing").await;
    let now = turn_core::now_ms();

    // The user is mid-keystroke and the window is theirs.
    let effects = effects_of(
        ui.ask(Request::UpdateUserActivity {
            context: turn_core::UserContext {
                last_keystroke_ms: Some(now),
                app_foreground: true,
                // Looking at something else: focus is only interesting when it would
                // actually take the user somewhere.
                active_session: None,
                sensitive_operation: false,
            },
        })
        .await,
    );
    assert!(effects.is_empty(), "nothing was pending: {effects:#?}");

    // A permission arrives while they are typing. Focus is deferred, not denied: the
    // signal is not lost, only delayed until their hands stop.
    post_hook(
        &agent.hook,
        &notification("permission_prompt", "Run the thing"),
    )
    .await;
    let deferral = ui
        .wait_for("focus to be deferred", |event| match event {
            ServerEvent::AttentionEffect {
                effect:
                    Effect::FocusDeferred {
                        session_id, reason, ..
                    },
            } if session_id == &agent.session => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(deferral, turn_core::attention::DeferReason::UserTyping);

    let response = match ui
        .try_ask(Request::MuteSession {
            session_id: turn_core::ids::SessionId::from_stored("sess_nope"),
            until_ms: Some(now + 1000),
        })
        .await
    {
        Err(error) => error.code,
        Ok(other) => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(response, turn_proto::ErrorCode::NotFound);

    daemon.shutdown().await;
}
