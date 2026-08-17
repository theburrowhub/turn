# Accessibility acceptance

This is the reproducible acceptance artifact for zoom, contrast, reduced motion,
keyboard-only operation, AccessKit semantics and terminal text composition. The automated
half does not open a window:

```sh
make accessibility-acceptance
```

## Automated contract

| Requirement | Reproducible evidence |
| --- | --- |
| One `Workspace → Session → Agent/Tool → Child` navigator, with no duplicate legacy navigation | `every_hierarchy_level_is_a_reachable_tree_item` |
| State, selection, pane focus and attention are separate live regions; the selected row also keeps its own selected state | `accessibility_announces_state_selection_focus_and_attention_separately` |
| Dialogs and alerts are named and modal, background tree focus cannot escape through the Command Palette, and close returns focus to the selected tree row | the Settings, Keyboard shortcuts, Attention Queue, New Pane, pane placement, write-conflict and Command Palette snapshot tests plus `closing_a_modal_returns_accessibility_focus_to_the_selected_tree_row` |
| State never relies on colour alone | `theme::tests::every_state_has_a_glyph_as_well_as_a_colour` and `the_attention_colour_is_reserved_for_states_that_block_the_user` |
| High-contrast text clears 4.5:1 and control boundaries clear 3:1 | `theme::tests::the_high_contrast_palette_clears_text_and_control_thresholds` |
| An explicit Turn choice overrides the desktop; otherwise macOS Reduce Motion and Increase Contrast are inherited live | `theme::tests::explicit_accessibility_values_override_the_live_desktop_preferences` and `app::tests::appearance_settings_are_installed_into_the_live_context_without_a_restart` |
| Reduced motion disables cursor blink, egui transitions and the loading spinner, then lets the window settle | `theme::tests::reduced_motion_removes_egui_transitions_from_the_installed_style` and `reduced_motion_keeps_loading_static_and_allows_the_window_to_settle` |
| At 300% zoom and the native 900×560 minimum, the hierarchy and active terminal remain navigable | `maximum_zoom_keeps_the_minimum_window_navigable` |
| A committed composed character reaches the PTY once; preedit reaches it zero times | `terminal::tests::a_composed_accent_reaches_the_program` and `a_composition_in_progress_is_not_sent_to_the_program` |

The Settings sheet exposes terminal font size, interface font size, whole-window zoom,
standard/high contrast and reduced motion. Empty Global contrast or motion values follow
the live macOS accessibility preference; an explicit value is stable and portable to
Linux.

## Manual VoiceOver acceptance on macOS

Build the same packaged sibling layout a release uses:

```sh
make macos-app
open dist/Turn.app
```

Use a Workspace with a running terminal, an Agent, a child process and one attention
demand. Turn VoiceOver on with Command-F5, then record each item below.

1. Navigate the window without a pointer. VoiceOver finds one hierarchy tree and its
   Workspace, Session, Agent/Tool and child levels in that order; it does not find a
   second Session or Agent navigator.
2. Move tree selection and pane focus separately. VoiceOver announces `Selection:` and
   `Focus:` separately. Trigger and clear an attention demand; `Attention:` is distinct
   from `Application state:` and `Connection:`.
3. Open Settings, Keyboard shortcuts, Attention Queue, Command Palette, New Pane and a
   destructive confirmation using only the keyboard. VoiceOver names each Dialog or
   AlertDialog, Tab stays inside it, Escape closes it and focus returns to the hierarchy.
4. Inspect every state shown in the tree and status bars. Each has a word or glyph in
   addition to colour. Enable High contrast and verify text, selected rows, input borders
   and disabled controls remain distinguishable.
5. Set zoom successively to 50%, 100%, 200% and 300%. At the native minimum window size,
   the compact permission actions, hierarchy Actions menu, selected row and active
   terminal remain reachable. Reset restores the inherited value.
6. Enable Reduce Motion in macOS with Turn's value unset, then reopen an inspector while
   details load. There is no spinner, transition or blinking terminal cursor. Set Turn's
   value explicitly off and verify that the explicit override takes effect.
7. Type a dead-key accent such as `á`, then use an installed CJK input method. Preedit and
   the candidate window stay at the terminal cursor; committing inserts the text once,
   without duplicate or missing characters.
8. Read the active terminal. VoiceOver announces its live/history state, rows and columns,
   and screen value; switching to retained history does not describe it as the live prompt.

## Manual Orca acceptance on Linux

Run the packaged or release build in a GNOME session with accessibility enabled and start
Orca (`orca --replace` when appropriate for the test desktop). Use the same fixture and
repeat the eight checks above, substituting the desktop's high-contrast and input-method
controls. Record whether the session is Wayland or X11, because both the accessibility
bridge and candidate-window placement can differ.

Linux has no single desktop API equivalent to the two AppKit preference properties.
Therefore High contrast and Reduce motion must always be testable through Turn's explicit
Global controls even when the desktop cannot be inherited.

## Accepted M15 local dictation checks

These checks apply when ADR-060 ships and do not claim current support:

1. The microphone control beside an eligible input is a named toggle with idle/recording/transcribing/error
   state, exact target and keyboard shortcut. It is fully usable without holding a key; Escape always cancels.
2. VoiceOver/Orca announces capture start, semantic Agent plus real input owner, elapsed-time milestones,
   transcription completion, truncation/error and cancellation. It does not continuously announce waveform/
   level updates.
3. The bottom status-bar state and inline draft never rely on colour or motion. Reduced motion replaces every
   pulsing/animated meter with static state plus elapsed text and does not hide microphone activity.
4. The memory-only draft has a normal multiline text role, label and description; focus arrives once after
   transcription. Insert, Send, Return to target, Copy and Discard have keyboard equivalents and disabled
   reasons when target identity is stale.
5. Permission, credential, provisional/unassigned and other ineligible targets expose neither a misleading
   enabled mic control nor an unnamed placeholder. The reason is discoverable without pointer hover.
6. OS microphone consent is announced by the platform and focus returns to the exact initiating control.
   Denial leaves one accessible recovery action and never loops the prompt.
7. At 300% zoom and the minimum window, target, on-device/model state, timer, draft and cancel/send controls
   remain reachable; microphone state never covers the Attention status.

Packaged macOS and Linux run records add microphone device, local model/engine and hold-versus-toggle result.
The full acceptance matrix is `docs/LOCAL_VOICE_INPUT.md`.

## Run record

Do not mark the manual pass complete without a row that another person can reproduce:

| Date | Commit/build | OS and session | Assistive technology/version | Input method | Result | Follow-up issue |
| --- | --- | --- | --- | --- | --- | --- |
| _pending_ | | | | | | |

An automated green run proves the application-owned contract. The manual rows prove that
the packaged platform bridge and current assistive-technology release expose that contract
as intended; neither is a substitute for the other.
