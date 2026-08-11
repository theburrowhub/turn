# Attention policy acceptance

This checklist exercises the user-visible boundary. The automated equivalents run with
`make verify`; the named tests below make a failure easy to reproduce in isolation.

## Automated proof

```sh
cargo test -p turnd attention_policy_resolves_all_four_persistent_levels_and_sessions_can_differ
cargo test -p turnd queue_priority_is_reordered_and_persisted
cargo test -p turnd a_session_mute_is_restored_after_attention_runtime_restarts
cargo test -p turn-core a_configured_custom_action_emits_the_exact_command
cargo test -p turn-core simultaneous_demands_never_become_a_timed_focus_cascade
cargo test -p turn-core typing_defers_focus_rather_than_dropping_the_signal
cargo test -p turn-gui a_custom_action_reaches_the_command_runner_exactly_once
cargo test -p turn-gui selecting_a_tree_node_never_acknowledges_or_resolves_attention
```

## Manual sound, notification and custom action

1. Open Settings with a Session selected. In “Attention, sounds and notifications”, select
   the Session level.
2. Enable `sound`, `notify` and `custom` for “Question asked”. Choose the alert sound. Replace
   the custom command with `touch /tmp/turn-attention-custom-accepted`.
3. Make that Session's Agent ask a question. Confirm that the OS notification names the
   demand, the alert sounds once, and the file is created. Turn never displays the stored
   command again; replacing it is deliberately blind because it may contain credentials.
4. Delete the acceptance file when finished.

## Manual hierarchy, focus and persistence

1. Give two Sessions different “Question asked” actions at the Session level. Trigger both
   and confirm their effects differ. Reset one Session value and confirm the Template,
   Workspace or Global value shown underneath becomes effective again.
2. Enable the typing guard and a focus action. Type continuously in another Session while the
   demand arrives: it must queue without moving focus, then may move only after typing stops.
3. Raise one demand and lower another with the queue's priority controls. Snooze one, mute its
   Session and dismiss another. Restart Turn: priority, snooze and mute remain; the dismissed
   demand does not return.
4. Select a Process row that has an outstanding permission. Selection alone must not remove,
   acknowledge or reorder the demand. Its permission banner must show the exact command, cwd,
   risk, Agent and Process before “Go to this session” is used.

