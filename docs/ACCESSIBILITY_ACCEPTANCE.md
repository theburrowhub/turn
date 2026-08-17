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

## Accepted operator-control-plane surface matrix

These post-v0.1 rows become required as their verticals ship. Automated accessibility-tree snapshots prove
roles/names/state/order; packaged VoiceOver and Orca runs prove the platform bridge. Every pointer drag has a
keyboard move/action-menu equivalent, and closing a view/dialog restores the exact invoking element.

| Surface | Required semantic/focus oracle |
| --- | --- |
| Canonical tree | Workspace, Session and every `Agent`, `Subagent`, `Shell`, `Command`, `Tui`, `Service`, `Process`, `Log`, `Group`, `Team`, `Flow`, `Job`, `NativeJob`, `NativeJobIteration`, `WorkItem`, `Note`, `File`, `Diff`, `Web`, `Browser`, `Media` row is one named tree item with level, expanded/selected, lifecycle/turn and badge described independently; references activate the one row rather than duplicate it. |
| Agent/Subagent WorkSurface | Heading names role/task/provider and distinguishes local alias from observed provider title; attempt, turn, children, context/quota and unavailable/stale state use labelled regions; transcript/activity order is stable; action focus never jumps when observations update. |
| Terminal/runtime WorkSurface | Terminal keeps application/document semantics as appropriate; Service health, Process ancestry and Log filters are named; alternate-screen/IME remains primary input and status updates do not steal focus. |
| Flow/Team/Group | Definition/run state, step/member/reference lists, dependency result and grant limits are named; pause/cancel/abort/retry consequences are descriptions; add/remove/reorder/member-role editing works without drag. |
| Resource WorkSurface | Note editor, inert File/Diff/Web preview, interactive isolated Browser and Media are distinct; reviewed origin/local-file scope, loading/blocked navigation, history and popup request state are named, and untrusted content cannot create invisible focus targets or escape its labelled region. |
| CreationCatalog/setup | Search, grouped entries, capability/disabled reason, effective defaults and target Workspace are announced; keyboard order follows visible grouping; progress/cancel returns to the invoking tree item or specified new target. Foreground Session selection automatically activates its exact bounded eligible saved-runtime set—or one default Shell when empty—after preflight and never exposes a generic secondary start-pane control. |
| Integration diagnostic | Provider/version/mechanism/freshness/downgrade and self-test consequences are labelled; starting a self-test is explicit, progress is one live region and cleanup receipt returns focus. |
| Bottom status and HUD | Highest event plus overflow count has `status`; history has deterministic severity/time order; progress announces start/terminal only; ordinary working rows never appear as actionable Attention. |
| Attention/provisional view | Next routes in one command; permission/question/result controls expose the exact subject/owner; node-less evidence opens a named provisional document with no enabled input; mark-read/ack/resolve remain separate. |
| File explorer/SCM/conflict | Tree/table rows expose host/repository/path and selected/staged/conflict state; stage/unstage/commit/history/conflict resolution all work by keyboard; discard/history rewrite identify destructive scope. |
| Remote writer handoff | Current viewer/writer, lease expiry and requested recipient are announced on both clients; accept/refuse is modal and focus returns to the runtime input only after the new generation is active. |
| Companion | Observe/respond/control scope and offline/stale state are named; usage/context/activity are grouped by AccountProfile with observation/expiry state; every allowed action, including a specifically granted typed permission response, has a native accessibility role; desktop-only actions are absent or expose a disabled reason, never an inert icon. |

For each row, the packaged record names OS/session, VoiceOver or Orca version, zoom, contrast, reduced-motion
and IME state. The minimum/maximum window sizes are tested at 50%, 100%, 200% and 300% zoom. Focus-order
snapshots and screen-reader speech logs are retained as artifacts rather than replaced by visual screenshots.

### ADR-063 independent capability rows

The following are eight independent acceptance rows. Passing another row, or the broader surface row above,
cannot stand in for one of them. Each row needs an automated accessibility-tree/focus-order snapshot and a
packaged VoiceOver and Orca run on every surface on which that capability is advertised.

| Capability | Required semantic, keyboard and focus oracle |
| --- | --- |
| Board and `WorkItem` projection | The board is one named region whose columns/groups and revision are exposed; every item announces title, closed state, assignee, labels, dependencies and stale/conflict state independently. Keyboard commands cover create, edit, move, close, filter and opening the canonical Node. Reordering never requires drag, selection remains stable after a revision, and board metadata is never announced as Lifecycle, dependency-result or Attention authority. |
| Delegated `Resource` and `ProgressUpdate` | A delegated resource is a normal named tree item with creator, owning attempt, bounded kind/size and accepted/refused receipt. Progress is attached to the exact Flow/step/attempt and announces start, meaningful state changes and terminal state once, not every sample. Limit refusal and partial acceptance are labelled, do not steal focus, and expose no enabled edit/control action outside the grant. |
| Shared `RuntimeEndpoint` multiplexing | Endpoint health is a labelled group separate from each bound AgentInstance. Every binding announces its unique conversation owner, profile/target scope, connection generation and current/stale/recovery state; moving among siblings never changes another sibling's input focus or transcript position. A binding failure updates only that binding plus endpoint summary, and reconnect/fallback actions return focus to the same instance. |
| Target-wide runtime inventory and reconciliation | The recovery view is a named table/tree with provider, target/host, account profile, endpoint generation, runtime identity and bound/unbound/ignored state per row. Adopt, Ignore and Terminate are distinct keyboard actions with exact-scope confirmation, disabled reasons and uncertain-effect status; refresh preserves selection by runtime identity and never silently activates the first discovered runtime. |
| `FileBackend` editing | The editor announces backend, target/host, jailed root, path, encoding, dirty state and exact base revision. Save is a named keyboard action; revision mismatch opens a labelled conflict with Reload, Compare and Cancel, preserves the draft and returns focus to the conflicted editor. Read-only, disconnected and unsupported states remove or disable mutation with a stated reason and never substitute a same-named local file. |
| Note-backed live brief `ContextLink` | A Note announces author, current reviewed revision, consumers and whether each link is pinned or follows reviewed revisions. Editing, reviewing, pinning, advancing, expanding, renewing and revoking have separate named actions and keyboard paths. A newer unreviewed revision produces one non-disruptive stale/update status; it never changes a consumer's effective context or steals editor/terminal focus. |
| `AccountProfile` lifecycle and isolation | Distinct create, adopt, external-authenticate, validate, rename, set/unset-default, retire and delete controls identify provider plus ExecutionTarget, expose the exact closed state and explicitly state that the record is non-secret. Launch/setup announces requested, resolved and effective profile; an active attempt keeps its frozen profile when defaults change. Auth failure, expired/revoked/retired profile, delete blocker and unavailable isolation each expose a distinct disabled reason, and no credential value appears in accessible names, descriptions or speech logs. |
| Full remote GUI operator surface | This is a complete browser GUI rendering of the canonical tree, one selected WorkSurface, status history, Attention queue and capability-gated controls; it is tested independently from the reduced Companion and headless client. Browser landmarks, page/view titles, reconnect/stale state, writer lease and network-lag status are named; keyboard routing survives reconnect without duplicate activation or focus loss. Desktop-only sensitive actions are absent or explicitly disabled server-side, while every advertised remote-GUI action remains operable with a screen reader at all zoom levels. |
| Headless structured client | The same objects, revisions, routes, disabled reasons and receipts are emitted as bounded documented structured output with deterministic ordering, stable field names and machine-readable errors. Rendering, zoom, focus, keyboard and screen-reader claims are explicitly not applicable; schema/golden/CLI-help tests replace packaged assistive-technology evidence and cannot approve the remote GUI. |

The full remote GUI, headless client and Companion are three different surfaces and require separate records.
No pass approves another: the GUI needs packaged browser/assistive-technology evidence, headless needs
structured schema/order/error evidence with rendering marked not applicable, and Companion needs its reduced
action-set evidence.

### ADR-064 independent product-gap rows

These eight rows are independent requirements. Each needs automated tree/focus/speech snapshots plus the
packaged VoiceOver and Orca record on every advertised desktop, full-remote GUI and Companion surface. A pass for
one row cannot approve another, and an unsupported capability must expose its reason rather than disappearing.

| Capability | Required semantic, keyboard and focus oracle |
| --- | --- |
| Foreground Session activation (`ACP-LIF-009`) | Selecting the canonical Session once announces selection and the exact bounded eligible saved-runtime set—or one default Shell when empty—then attaches/starts that plan with no secondary action or generic “start pane” control. Existing runtimes, multi-descriptor plan, empty/default-Shell, preflight refusal and uncertain launch are distinct status values. Expansion, preview, restore, reconnect and Attention navigation never announce or cause a launch. A refusal keeps focus on the selected Session, exposes the exact bottom-status remediation and performs no surprise focus jump; successful asynchronous readiness transfers terminal focus only to the selected WorkSurface's exact declared owner. |
| External WorkItemSource (`ACP-VIE-012`) | Source, external identity, field authority, sync coverage, observed/expiry time and conflict state are labelled independently from the WorkItem's local fields. Saved filters and bounded next/previous page controls are keyboard operable and retain the exact item across refresh. Create/edit/close/reopen, Reload source, Compare and Keep proposal have distinct names and CAS consequences. An unmapped state/assignee, rate limit, stale cache, missing page or conflict is announced rather than rendered as empty/done; no credential value enters accessible text. |
| Typed remote permission response (`ACP-ATT-011`) | The full remote and Companion surfaces announce exact Agent/attempt, permission kind/risk, bounded detail, allowed typed options, grant scope, expiry and freshness. Options use native radio/button semantics and one activation; raw terminal input is disabled with a named reason while that recognised sensitive interaction is pending. Stale/replayed/refused/uncertain receipts return focus to the same interaction without duplicate submission. Credential entry, host trust and administration stay absent/local-only. An opaque generic TUI is explicitly described as not classifiable rather than falsely protected. |
| Provider-native jobs (`ACP-ADP-011`) | `Job`, `NativeJob` and `NativeJobIteration` are separate levels with provider/profile, schedule/time zone, enabled state, current/last iteration, freshness and survival/recovery state. Turn Flow recurrence is named as a different object. Dismiss, cancel current iteration, enable/disable schedule and delete job are separate keyboard actions with exact scope and confirmation; dismiss never sounds like deletion, and a replaced iteration never steals focus. |
| Conversation inventory (`ACP-CTX-013`) | History/search is a labelled, bounded result list scoped to provider, AccountProfile and ExecutionTarget. Each result announces stable identity evidence, time/state, coverage/freshness and title availability without reading transcript content by default. Search scope and page controls are named. Ambiguous/duplicate/stale matches disable Adopt and Resume with the exact reason; Adopt existing and Resume new attempt are distinct actions whose completion returns focus to the canonical Agent node. |
| Inert Web versus interactive Browser (`ACP-RUN-011`) | Web preview exposes document semantics only and has no script-generated focus targets. Browser is a separately named application/region with reviewed origin or local-HTML identity, isolation state, address, Back/Forward/Reload, loading/error and popup request. Localhost/private-origin and local-file review are explicit AlertDialogs; redirects and popups never move focus or open a surface without approval. Untrusted page focus is contained, Escape returns to Browser chrome, and leaving the node returns to its exact tree row. |
| Provider title read and rename (`ACP-OBS-009`) | Local alias, observed provider title, provider revision and freshness are separately labelled. Unsupported/stale title read never appears as an empty title. Provider Rename is a named dialog/action with requested and effective values plus accepted/refused/uncertain receipt; local Rename is a different action. Degrading either capability preserves the other and keeps the tree row's stable identity/focus. |
| Companion usage/context/activity inbox (`ACP-SCL-010`) | AccountProfile is the first grouping and remains in every item description. Usage/context announces value, unit/window, reset when known, observation time, expiry and the distinct `unavailable`, `stale` or `rate-limited` state rather than a false zero. Activity announces provider event identity, bounded summary, time/freshness and unread/handled state; inbox unread is never announced as Attention unless the typed Attention reducer created a demand. Paging, profile switch, offline cache and reconnect preserve selection without mixing sibling profiles. |

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

| Date | Commit/build | OS and session | Surface and browser/client version | Network profile | Assistive technology/version | Input method | Result | Follow-up issue |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| _pending_ | | | | | | | | |

An automated green run proves the application-owned contract. The manual rows prove that
the packaged platform bridge and current assistive-technology release expose that contract
as intended; neither is a substitute for the other. `Surface` is one of desktop, full remote GUI or
Companion and names its app/browser build. Headless uses a separate automated structured-output record and
never appears as a packaged screen-reader pass. `Network profile` records local/LAN/WAN plus injected latency,
loss, offline/reconnect and endpoint location, so a local desktop record cannot silently approve a remote
focus/reconnect path.
