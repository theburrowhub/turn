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
| Canonical tree | Workspace, Session and every `Agent`, `Subagent`, `Shell`, `Command`, `Tui`, `Service`, `Process`, `Log`, `Group`, `Team`, `Flow`, `Job`, `WorkItem`, `Note`, `File`, `Diff`, `WebPreview`, `Browser`, `Media` Node row is one named tree item with level, expanded/selected, applicable lifecycle/turn and badge described independently; NativeJobIteration and other references remain inside the owning Node View and activate that one row rather than becoming NodeKinds or duplicate tree rows. |
| Agent/Subagent WorkSurface | Heading names role/task/provider and distinguishes local alias from observed provider title; attempt, turn, children, context/quota and unavailable/stale state use labelled regions; transcript/activity order is stable; action focus never jumps when observations update. |
| Terminal/runtime WorkSurface | Terminal keeps application/document semantics as appropriate; Service health, Process ancestry and Log filters are named; alternate-screen/IME remains primary input and status updates do not steal focus. |
| Terminal appearance Settings and preview | Font family/size, theme, foreground/background contrast, line spacing and zoom are individually named, keyboard operable and reflected immediately in one labelled non-interactive TerminalPreview. Preview and a real terminal under the same resolved settings have matching glyph metrics, wrapping, clipping and palette snapshots at min/max zoom; preview exposes no terminal input/control action, screen readers do not mistake it for a live prompt and every combination retains the declared text/control contrast. |
| Flow/Team/Group | Definition/run state, step/member/reference lists, dependency result and grant limits are named; recursive Group level/expanded/order and separately owned CheckoutScope state are explicit; pause/cancel/abort/retry and promote/unbind/remove consequences are descriptions; add/remove/subtree-move/reorder/member-role editing works without drag. |
| Resource WorkSurface | Note editor, inert File/Diff/WebPreview, interactive isolated Browser and Media are distinct; reviewed origin/local-file scope, loading/blocked navigation, history and popup request state are named, and untrusted content cannot create invisible focus targets or escape its labelled region. |
| CommandCatalogue creation filter/setup | Search, grouped entries, capability/disabled reason, effective defaults and target Workspace are announced; New/Open/Clone/SSH onboarding phase, target/path/repository and partial/reconcile state use one keyboard path while Publish remains a separate labelled consequence; progress/cancel returns to the invoking tree item or specified new target. Foreground Session selection automatically activates its exact bounded eligible saved-runtime set—or one default Shell when empty—after preflight and never exposes a generic secondary start-pane control. |
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

The following are nine independent acceptance rows. Passing another row, or the broader surface row above,
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
| Typed permission response (`ACP-ATT-011`) | Desktop, full remote and Companion announce exact Agent/attempt/route, bounded redacted consequence, every provider option (not only allow/deny), transport/freshness, grant delivery/expiry and receipt evidence. Typed response disables raw input with a named reason; verified-local-PTY labels deterministic desktop fallback. Consumed/revoked/expired/invalidated grants and definite-no-effect/submitted/possible-effect/evidence states are distinct. Stale/raced/refused receipts return focus without duplicate dispatch. Credential, grant administration, trust and opaque-TUI semantic claims remain absent. |
| Provider-native jobs (`ACP-ADP-011`) | The one `Job` Node presents creation/key/incarnation and owning Session; requested versus last-proved effective definition/schedule/model/flags; scan coverage; independent schedule, iteration, presence, projection and per-intent reconciliation states; and bounded iteration records as labelled sections, never tree rows. Adopt discovered, List/Get/Create/Update/Pause/Resume/Run now/Cancel exact iteration/Delete provider job have distinct labels/consequences. Cancel prepared create/mutation, Hide activity, Forget, Restore and Delete local data are named local zero-provider actions. Missing versus external tombstone, truncated metadata, privacy-suppressed content and control-disabled coverage gap are announced. Forget preserves/reroutes current Attention and container rehome retains focus on the same Node. |
| Conversation inventory (`ACP-CTX-013`) | History/search is a labelled, bounded result list scoped to provider, AccountProfile and ExecutionTarget. Each result announces stable identity evidence, time/state, coverage/freshness and title availability without reading transcript content by default. Search scope and page controls are named. Ambiguous/duplicate/stale matches disable Adopt and Resume with the exact reason; Adopt existing and Resume new attempt are distinct actions whose completion returns focus to the canonical Agent node. |
| Inert WebPreview versus interactive Browser (`ACP-RUN-011`) | WebPreview exposes document semantics only and has no script-generated focus targets. Browser is a separately named application/region with reviewed origin or local-HTML identity, isolation state, address, Back/Forward/Reload, loading/error and popup request. Localhost/private-origin and local-file review are explicit AlertDialogs; redirects and popups never move focus or open a surface without approval. Untrusted page focus is contained, Escape returns to Browser chrome, and leaving the node returns to its exact tree row. |
| Provider title read and rename (`ACP-OBS-009`) | Local alias, observed provider title, provider revision and freshness are separately labelled. Unsupported/stale title read never appears as an empty title. Provider Rename is a named dialog/action with requested and effective values plus accepted/refused/uncertain receipt; local Rename is a different action. Degrading either capability preserves the other and keeps the tree row's stable identity/focus. |
| Companion usage/context/activity inbox (`ACP-SCL-010`) | AccountProfile is the first grouping and remains in every item description. Usage/context announces value, unit/window, reset when known, observation time, expiry and the distinct `unavailable`, `stale` or `rate-limited` state rather than a false zero. Activity announces provider event identity, bounded summary, time/freshness and unread/handled state; inbox unread is never announced as Attention unless the typed Attention reducer created a demand. Paging, profile switch, offline cache and reconnect preserve selection without mixing sibling profiles. |

### ADR-066 independent utility and presentation rows

These nine rows are independent. Each needs the same automated semantic/focus/keyboard evidence and packaged
VoiceOver/Orca record on every advertised GUI surface; a combined WorkSurface screenshot cannot approve them.

| Capability | Required semantic, keyboard and focus oracle |
| --- | --- |
| Directory, commit history and find (`ACP-RUN-004`, `ACP-RUN-006`) | Directory and commit pages expose target/root/repository, revision, position and complete/partial/gapped coverage; watches announce rename/delete/gap without stealing focus. Graph parent relations and changed-file status remain keyboard navigable. Find announces pinned source revision, match count/current position, Next/Previous/Wrap/No match, and closes without terminal input or selection change. |
| Media import/playback (`ACP-RUN-012`) | Import progress/refusal names source type, safe size/MIME/hash state and destination without reading a private path unnecessarily. A committed Media Node exposes named Play/Pause/Seek/Mute/Volume/Caption controls, elapsed/duration and codec/error; restore never auto-loads/plays or moves focus. Unsupported content remains a labelled inert region. |
| Repository host and commit proposal (`ACP-RUN-013`, `ACP-RUN-014`) | Profile host/target/account/safe scopes/state and independent Repository/WorkItem grants are labelled; secret values are absent. Authenticate/Validate/Rotate/Revoke/Delete return focus with exact state. Proposal view announces staged revision/hash, omissions/redactions, generating/ready/refused/failed/expired state and editable draft; Apply to editor is distinct from Commit/Push and never triggers them. |
| Transfer tickets (`ACP-RUN-015`) | Direction, reviewed source/destination, target/root, size/hash/progress, expiry and prepared/transferring/paused/reconcile/completed/failed/cancelled state are named. Pause/Resume/Cancel/Retry reconciliation are keyboard actions, progress is throttled for speech and a destination conflict never silently overwrites or changes focus. |
| Plain/Markdown projection (`ACP-RUN-016`) | Plain and Sanitised Markdown are a named per-surface toggle with pinned source revision and unsupported/stale reason. Semantic headings/lists/links remain navigable, reviewed links require a separate action, and switching modes restores logical selection without editing source or injecting terminal bytes. |
| Command catalogue (`ACP-CRE-009`) | Toolbar, palette, menus and shortcuts expose the same entry names, ids, availability/consequence and current catalogue revision. Search result count/order and disabled reason are announced; invoking an entry returns focus to its canonical result. No terminal/agent-output label becomes an actionable command. |
| Application update and announcements (`ACP-CRE-010`, `ACP-VIE-013`) | Announcement source/revision/expiry/dismiss and reviewed-link action are labelled separately from Status/Attention. Update channel/version/platform/architecture, download/verify/stage/apply/rollback states and errors use one live region with bounded progress. Apply is a distinct foreground confirmation; failure leaves terminals operable and never traps focus. |
| WorkItem activity (`ACP-VIE-014`) | Events form an ordered labelled list grouped by WorkItem, with actor/provenance/kind/time/safe delta and page/gap state. Selecting an event routes the same WorkItem without treating it as Attention or repeating the mutation; echo-deduplicated events are not announced twice. |
| Presentation undo/redo (`ACP-SAF-015`) | Undo/Redo names the exact whitelisted presentation change and disabled/invalidated reason, is keyboard operable and restores the expected surface focus. New edit clears redo visibly; concurrent invalidation never silently undoes another client. Excluded domain/runtime/provider/input/Attention actions never appear in history. |

### ADR-065 independent final-gap rows

The eight user-facing rows below require automated tree/focus/speech snapshots and packaged evidence on every
advertised GUI surface. `ACP-SAF-014` is intentionally non-visual: its TSV/schema/mutation oracle is the only
applicable evidence, and a screen-reader pass cannot approve the source-coverage ledger.

| Capability | Required semantic, keyboard and focus oracle |
| --- | --- |
| Recursive Groups and CheckoutScope (`ACP-HIE-009`) | Each Group exposes exact tree level, expanded/selected state, parent and stable sibling position through native tree semantics up to depth 128. Create, group siblings, move subtree, promote/ungroup, reorder and non-empty delete disposition have keyboard/action-menu equivalents and return focus to the surviving exact row. Cycle/cross-Session/depth/corruption refusal is an alert with no partial move. CheckoutScope exposes provisioning/active/missing/conflicted/unbinding/removing/reconcile-required/unbound/removed separately from binding proposed/current/refused/stale/unbound and creator provenance. **Unbind Group projection**, **Unbind CheckoutScope**, **Reconcile**, **Move and rehome** and **Remove CheckoutScope** name distinct cwd/runtime/worktree/disk/branch consequences and never rely on drag or an icon alone. |
| Automatic hierarchy arrangement (`ACP-HIE-010`) | Dense nested, collapsed, filtered and variable-height `ProjectedRows` remain non-overlapping in exact logical/native-accessibility preorder; `MaterializedRows` is only the viewport subset. Every adjacent projected pair uses exactly the declared 0–8 logical-pixel `TreeRowGap` at 100% zoom and every virtual spacer equals the omitted projected prefix sum. Restore, resize, zoom and row failure expose no Tidy/Arrange control, inaccessible gap/overlay or focus/selection move; a selected/focused projected row is pinned materialized with its anchor. Collapse retains hidden selection and focuses the collapsed ancestor, filtering focuses the filter control and later restores the surviving row, and deletion falls forward, backward, then to Session. The initiating visibility/topology action—not layout reflow—owns that announced focus change. |
| WorkspaceOnboarding (`ACP-CRE-008`) | New directory, Open directory, Clone repository and Adopt SSH target are one labelled stepper with target/path/repository/auth-reference, current phase and cancellation/recovery. Partial clone and host mismatch retain a named resumable recovery action and invoking focus. Publish repository is a separate AlertDialog naming destination, visibility and branch; onboarding success cannot focus or activate it implicitly. |
| Six adapters and quota-only connectors (`ACP-ADP-012`) | Provider/version/integration and every capability state are read as a uniform labelled table for all six dedicated adapters. Unsupported/degraded/unknown states stay present with reason/remediation. Kimi/MiniMax usage rows announce AccountProfile/target/window/reset/coverage and are explicitly quota-only; no disabled phantom launch/control buttons appear. Slow updates preserve row and selected-node focus. |
| Model endpoint routing (`ACP-ADP-013`) | Endpoint profile, target, canonical origin, protocol, health, credential-reference kind/availability, discovery coverage and requested/effective model are labelled without exposing a secret. Create/Edit/Validate/Set default/Retire/Delete and model choice work by keyboard; invalid TLS/redirect/private origin, stale catalogue, missing secret and failed switch return focus with exact zero-effect reason. The launch receipt distinguishes requested from effective route/model. |
| ResourceInventory (`ACP-OBS-010`) | The target resource view announces host total/available/used memory, swap and pressure only when measured, plus coverage/observed age. `unmeasured`, `partial`, `gapped`, `unavailable`, `unsupported` and measured zero are distinct speech values. Process rows identify current/closed Session owner, unmatched survivor or ambiguous attribution and own/children/total RSS without duplicate labels. Exact Terminate is a separate foreground confirmation; refresh preserves row identity and failed remote collection never moves to a local row. |
| Local name proposals (`ACP-OBS-011`) | Local alias, provider title, proposed label, source/confidence/time and `follow source|pinned` mode are separate. Generate, Apply, Pin/Unpin and local Rename are named actions; Provider Rename remains elsewhere. A rejected/stale/redacted proposal announces why and returns focus without replacing the current label. Group proposals identify their bounded member-summary scope. |
| Background Attention delivery (`ACP-ATT-012`) | Endpoint/grant scope, expiry, privacy level, queued/submitted/accepted/failed/revoked state and live start/update/end are named in settings/history without claiming delivered/read/resolved. Pair/Revoke/Retry-test are keyboard-operable and never reveal token or payload body. Notification activation opens the exact current route after resync; stale routes expose a named resolved/expired state. NotificationHostMode has structured status/log evidence only and makes no invisible GUI or screen-reader claim. |

`ACP-SAF-014` passes only when the neutral ledger parser and adversarial mutations prove exact snapshot/count,
unique feature ids, allowed dispositions, nonempty rationale, evidence digest and live PRD/ACP/ADR links. If a
future UI displays the ledger, that view becomes an additional accessibility surface; it cannot replace the
machine oracle.

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

## ADR-067 document, sound, browser-control, lifecycle and recovery rows

These rows require independent automated semantics plus packaged VoiceOver/Orca evidence; sound is
supplementary and cannot be the sole signal.

| Capability | Required semantic, keyboard and focus oracle |
| --- | --- |
| Document view/print (`ACP-RUN-017`) | Image/PDF View names source revision, MIME, page/count, zoom/fit/rotation, search coverage and stale/offline/error state. Page/zoom/search/close are fully keyboard operable and preserve tree selection. Print is a distinct consequence-labelled desktop action with selected pages/layout/printer and `submitted-unconfirmed` announced separately; restore never opens, decodes or prints. |
| Attention sounds (`ACP-ATT-013`) | `done` and `needs-you` have distinct optional cues plus complete visual live-region/state text. Enable, mute, volume and cooldown are named controls. Muted, unsupported, failed or rate-limited audio changes no focus/Attention and leaves every demand/result discoverable without hearing. |
| Agent Browser control (`ACP-ADP-014`) | The Browser region announces controlling agent/attempt, logged-out isolation, origin grant and control state outside page content. Badge and Stop are always keyboard/screen-reader reachable before untrusted page focus; Escape returns to chrome. Revoke/expiry/owner loss is announced and page content cannot hide or rename controls. |
| Browser Memory Saver (`ACP-RUN-020`) | Opt-in/off, five-minute eligibility, `discarding`, `cleanup pending`, `discarded — history lost`, `rehydrating` and exact blocked reason are named independently of page content. Selecting a discarded Node preserves tree focus and automatically rehydrates when still safe; otherwise bottom status announces the policy/origin/address reason. There is never a generic `Start pane` button or a second action needed. |
| Safe control visibility (`ACP-VIE-015`) | The eleven optional switches name both action and slot. Every combination keeps Attention/Next Attention, blocked/recovery, Delete/End, Restart, Search, Close and destructive consequence controls in logical keyboard/screen-reader order. Hiding an allowed slot leaves its same named action in the completely keyboard-operable command palette; a dedicated global shortcut is not required. Invalid ids are announced as ignored and never remove focus or action authority. |
| PTY capacity (`ACP-RUN-021`) | Target, used, ceiling, headroom, measured time and complete/partial/unavailable/stale state have text labels and never rely on warning colour. Elevated and critical route through the canonical status/Attention path without focus theft; repeat dedupe is silent to the current task. Automatic remediation appears only when supported and opens one keyboard-operable consequence review naming current/proposed persistent value, privilege and rollback. Cancel/partial/uncertain/unsupported states return focus and give precise target-specific guidance in bottom status, without an unsolicited password loop. |
| Bulk restart/Eco (`ACP-LIF-010`, `ACP-LIF-011`) | Bulk preview exposes included and excluded rows/reasons, exact count, sequential progress, Cancel-before-next and complete restarted/skipped/failed/reconcile summary without focus theft. Eco opt-in/status/eligibility and hibernating/waking/failure are named; returning selection wakes automatically without a `Start pane`, and wake failure routes an accessible recovery demand. |
| Companion launch (`ACP-CRE-011`) | Companion exposes only reviewed allowlist entries and their Workspace/Session/template/adapter/account/safe-target identity, never a free-form command/path/flags editor. Submitted/refused/reconciling/registered states preserve the exact operation and route the one canonical new Node. |
| Corrupt store recovery (`ACP-SAF-019`) | One assertive scoped status names that original bytes were preserved and offers Inspect metadata, create-new Export, reviewed Recover/Start fresh and destructive Discard as distinct keyboard actions. Dismiss does not imply recovery/deletion; read-only/capacity/fsync uncertainty is announced instead of an empty successful tree. |
| Cross-client convergence (`ACP-SAF-020`) | A Session/Node added by another authenticated client appears once in logical tree order without moving current focus/selection or announcing a reload. Conflict and gap/resnapshot are named; a dirty local draft stays present with stale/conflict state rather than disappearing. |
