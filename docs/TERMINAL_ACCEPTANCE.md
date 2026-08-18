# Terminal interaction acceptance

This is the reproducible acceptance artifact for search and scrollback, links, path drops,
appearance preferences, text input and full-screen terminal modes. It deliberately does not
open the Turn desktop application or an interactive Browser Node.

Run from the repository root:

```sh
make terminal-acceptance
```

The target uses real PTYs where the boundary matters and deterministic grids/input events where
desktop automation would add timing noise. It covers the following contract.

| Requirement | Reproducible evidence |
| --- | --- |
| Search the live screen and retained scrollback; next/previous wrap and reach a result that left the viewport | `turn-gui/tests/scrollback.rs`, `terminal::search` and `turnd::requests::scrollback` |
| OSC-8 and detected links are clickable without treating output as markup or a command | `turn-gui/tests/links.rs`, `terminal::links` and `turn-pty::links` |
| Refuse executable schemes, normalise confusable hosts and confirm declared text that names a different host | `a_hyperlink_pointing_at_a_scheme_that_executes_is_refused_the_whole_way_through` and the malicious-link cases in `terminal::links` |
| Drop paths as one non-submitting bracketed paste, quoted for zsh/bash/fish syntax; refuse filenames containing a newline | `terminal::paths` and `collect_dropped_paths` |
| Apply terminal/UI font size, zoom, high contrast, block/bar/underline cursor, cursor blink/reduced motion and optional programming ligatures live | `theme::tests::every_appearance_control_changes_the_values_the_renderer_reads`, `app::tests::appearance_settings_are_installed_into_the_live_context_without_a_restart` and the cursor/ligature renderer tests |
| Preserve original cells, search text and clipboard while ligatures are enabled | `terminal::tests::ligatures_join_only_known_pairs_and_never_change_the_grid_text` |
| IME commits and dead-key composition reach the PTY once, while an in-progress composition reaches it zero times | `terminal::tests::a_composed_accent_reaches_the_program` and `a_composition_in_progress_is_not_sent_to_the_program` |
| Mouse press/drag/hover reporting, bracketed paste and alternate-screen ownership follow the program's advertised modes | `terminal::mouse`, `terminal::keys`, `terminal::feed` and `turn-proto::cells` |
| True colour, wide Unicode, resize, clipboard and normal/block/wrapped selection preserve terminal geometry | `terminal`, `turn-proto::cells` and `turnd/tests/cells.rs` |
| Output remains bounded; a lagging client gets a gap plus a replay instead of unbounded backpressure | `turn-pty::buffer`, `turn-pty::process`, `turnd::output` and `a_client_that_dropped_an_update_recovers_the_whole_screen_and_carries_on` |

## Why the application matrix is one terminal contract

Claude Code, Codex CLI, Gemini CLI, OpenCode, shells, test watchers, editors, `lazygit` and
`btop` do not use vendor-specific rendering paths. They all run behind the same PTY and express
input modes, colour, hyperlinks and full-screen ownership with the byte sequences asserted above.
Adapters may improve semantic state, but they cannot change terminal input or rendering. This is
why the acceptance target tests the PTY/VT contract once instead of requiring authenticated network
sessions from four vendors on every run.

The authenticated packaged-Claude vertical remains separately reproducible in
[`REVIEWER_ACCEPTANCE.md`](REVIEWER_ACCEPTANCE.md). A release candidate should additionally smoke-test
whatever current vendor binaries and shells are installed on both supported desktop platforms; that
manual smoke is evidence about packaging and upstream releases, not a second implementation of the
terminal contract. Screen-reader, contrast, motion and zoom evidence is recorded separately in
[`ACCESSIBILITY_ACCEPTANCE.md`](ACCESSIBILITY_ACCEPTANCE.md).

## Accepted successor terminal and remote-input boundary

ADR-064/065/066 do not add provider-specific terminal rendering paths. They add authority and projection
tests around the same PTY:

1. A foreground Session with a current proved-safe activation plan can restore/attach and start its exact
   bounded eligible saved-runtime descriptor set—or, when empty, its configured default Shell—as part of
   selecting that Session. This is one typed idempotent Session
   activation, not terminal output inference. Selecting a terminal child, historical conversation, WebPreview
   Resource, Browser, WorkItem, Job row or iteration record inside JobNodeView starts and submits zero bytes.
2. An inert `WebPreview` is not a terminal or a browser: its renderer has no script, forms, navigation,
   download, ambient credentials, daemon socket or file access. An interactive Browser is a separate Node
   with an isolated storage partition and typed navigation/history/popup/download operations. Browser page
   content and script messages never become PTY bytes, Turn control operations or Attention evidence.
3. Provider-native jobs and ConversationInventory come only from typed adapter capabilities. Terminal text,
   process titles and shell history cannot fabricate a job, conversation ownership, resumability, title-read
   capability or rename receipt. A generic terminal remains fully functional while those states are
   explicitly unsupported or unknown.
4. A full remote GUI may acquire a revision-fenced input/resize lease and use ordinary PTY input subject to
   the same owner/attempt/surface/connection generations as the local WorkSurface. A headless status client
   and a companion are not terminal clients and receive no arbitrary PTY input operation.
5. When adapter evidence identifies an exact typed sensitive permission interaction, all raw remote PTY
   writes for its input owner are refused while it is pending. A remote provider-offered option is accepted only through
   the closed typed operation and a single-use expiring foreground-desktop-issued encrypted grant bound to
   the exact provider option, AccountProfile, Session/Node/instance/attempt/generation and interaction/
   authority revisions; provider evidence, not the write receipt alone, closes it. Credentials, grant
   issuance/expansion, administration and host trust remain local. For an unclassifiable generic TUI Turn
   makes no guarantee that arbitrary bytes are a permission response and never fabricates a sensitive-state
   block from heuristics.
6. Provider/account context usage, quota and activity data never enter terminal title, scrollback or prompt
   parsing as authoritative facts. Their profile/target scope, coverage and freshness remain visible in the
   selected view, and missing/partial/error states never render as zero.
7. Recursive Group move/reorder/ungroup changes only presentation. It never types `cd`, relaunches a process or
   claims its cwd changed. A separately explicit CheckoutScope `move_and_rehome` refuses live writers and
   rewrites only stopped descriptors after current target/repository/worktree proof; missing inventory never
   falls back to the terminal or primary checkout.
8. ModelEndpointProfile discovery, selection and credential resolution are typed adapter/broker operations.
   Terminal output cannot advertise a route/model/capability or request a secret. Raw endpoint secrets are
   absent from argv, PTY, title, scrollback and durable environment; a missing/stale route writes zero terminal
   bytes and never launches a generic/native fallback.
9. Automatic name input is one bounded captured output/task summary under explicit NameProposal identity. An
   arbitrary OSC title, prompt line or provider `/rename` text cannot pin a local alias, rename a Group or send
   a provider command. Control/bidi/multiline/secret output is rejected, and applying a current proposal writes
   no PTY bytes.
10. Notification delivery, live status, host resource metrics, quota-only connectors and WorkspaceOnboarding
    phases come only from typed state/receipts. Terminal text cannot fabricate endpoint pairing, delivery
    acceptance, memory zero/pressure, clone completion, publish success or adapter support; displaying their
    status never injects input. NotificationHostMode opens no terminal/public listener implicitly.

Successor terminal acceptance therefore combines the existing byte/geometry suite with protocol tests for
input-lease fencing and remote-operation allowlists; a headless render test alone cannot prove remote GUI
control.

The successor resource harness additionally treats terminal residency and projection as closed state rather
than multiplying per-Pane constants by the 10,000 RuntimeAttempt cap. It must:

1. admit exactly128 live-or-retained PTY states with 2-MiB raw-ring and4-MiB current-grid reservations, refuse
   the 129th before spawn unless a stopped/unpinned/checkpointed state releases, and never evict a live sibling;
2. saturate parsed screen8 MiB/item/512 MiB,5,000 rows, retained images16 payloads/16 MiB/item/512 MiB and client
   caches12 payloads/12 MiB/item/256 MiB while preserving current cells and explicit truncation/placeholders;
3. hold128 partial hostile image sequences while proving only eight8-MiB scan buffers, eight8-MiB multipart
   assemblies and two complete128-MiB decode high-waters exist; allocator/RSS canaries cover input, inflate,
   decoder, raster, resize and RGBA together, and excess work becomes discard+one visible refusal;
4. fill PaneAttachment, projection-baseline, output-queue and pump-batch count/byte bounds independently. A
   buffer-first overflow gaps only the producing terminal; each vNext gap retires exactly the old attachment/
   baseline/batch generation and the client runtime, without operator action, performs a streamed resync that
   atomically installs a fresh generation. Exact detach/reselection/resize/owner/generation/connection/process
   loss releases every child state and no subscriber copy multiplies Arc payload bytes;
5. slow the connection writer until its256-frame/8-MiB and global4,096-frame/128-MiB outboxes fill, proving the
   reserved critical partitions still deliver input receipts, Attention, lifecycle/control and gap frames;
6. stream the maximum hierarchy-bootstrap, 65,536-cell resync and4-MiB image responses through≤180-KiB raw/
   ≤256-KiB encoded frames; independently fill a4,096-operation/180-KiB hierarchy delta and prove the first
   excess emits one scoped gap+automatic atomic refresh with no fragmented push. Reject a129-byte RequestId,
   discontinuity/duplicate-different chunk/bad digest/stale generation and disconnect, and apply zero partial
   cells or bytes. Image fetch reserves/transfers into cache without a duplicate shared charge,
   retries automatically at most once for the same visible generation and then leaves a labelled placeholder.

## Manual release smoke

On a packaged build, open one pane for each installed shell/agent/TUI and check:

1. Type a composed accent or use an IME, resize the pane, paste several lines and select/copy a
   wrapped line containing a wide Unicode character.
2. Run a full-screen program, verify arrows and mouse input, leave it, then search output that has
   scrolled off the screen.
3. Print a safe OSC-8 HTTPS link and a disguised one; the safe target opens directly and the
   disguised target shows both hosts before any external action.
4. Drop a path containing spaces and shell metacharacters. It appears quoted at the prompt and is
   not submitted.
5. Change each Appearance control at the temporary level. The visible pane updates without a
   restart; resetting it restores the inherited value.

## ADR-067 reviewed terminal clipboard oracle

`ACP-RUN-018` extends both automated PTY fixtures and the packaged check; a generic “paste works” result is
insufficient:

1. Keyboard/menu copy binds the exact current PaneAttachment, grid generation and explicit selection≤64 KiB,
   attempts one local OS clipboard write and never asks the PTY for content. Focus/grid/selection change before
   commit yields zero clipboard effect; client failure is labelled ambiguous and never retried.
2. Keyboard paste, X PRIMARY middle-click paste and path drop all use the same
   `TerminalClipboardGesture→write_runtime_input` path. They bind a current OS gesture, Surface, attachment,
   InputLease, InputSafety revision and monotonic input sequence. X primary-selection configuration cannot
   bypass those checks. Stale/background/permission-sensitive/unleased/cancelled input writes zero bytes.
3. Paste is≤64 KiB UTF-8. Drop is≤128 canonical local paths,≤4 KiB/path and≤64 KiB total; it remains quoted
   and unsubmitted. Every remote target, full-remote GUI and Companion fixture refuses before reading or
   transmitting clipboard/path bodies.
4. The local gesture family reaches one/Surface,eight/client,64/4 MiB and30-second expiry. Commit/cancel/expiry/
   detach/grid/lease/Surface/client loss wipes all body bytes; store, journal, diagnostics, context, sync and
   crash artifacts contain only the body-free RuntimeInputReceipt metadata.
5. Raw, fragmented, nested, base64, query, set and oversized OSC 52 sequences are injected across ordinary and
   alternate-screen streams. Both read and write directions are consumed before OS clipboard access, no reply
   bytes reach the PTY and following ordinary text/cells remain exact. No setting enables an exception.

## ADR-067 target PTY-capacity oracle

`ACP-RUN-021` extends the launch/resource harness independently of the resident-terminal memory caps:

1. Local macOS, remote-capable, unsupported, partial, failed and stale fixtures expose target/generation,
   used, ceiling, required headroom, measured-at and coverage; absent facts are never rendered or serialised
   as zero/healthy and a target-generation change invalidates the prior observation.
2. Exact `<80%`, `80%`, `ceiling−headroom−1` and `ceiling−headroom` readings drive healthy/elevated/critical
   edges. Level status and deduplicated Attention commit before the first capacity-refused launch receipt;
   held pressure reminds no faster than five minutes and clearing it resolves only that subject.
3. Critical fresh evidence refuses before opening a PTY. Unknown evidence follows the ordinary backend launch
   contract and an exact OS exhaustion failure publishes honest critical/unknown evidence before its receipt.
   Pressure handling never kills or terminates a tmux Session and releases a viewer/PTY client only through
   the independent CAP-107 durable-survival and zero-watcher proof; unknown or live work remains untouched.
4. Automatic remediation is present only for a capability-declared fixed provider. One foreground review pins
   exact before/proposed/persistent/rollback consequences; durable dispatch precedes the privilege broker and
   caller input can supply no shell, argv, path or config. Cancel and every stale/capacity/unsupported case have
   zero system effect.
5. Crash/lost reply is injected before and after kernel apply, persistent write, verify and rollback. Exact
   provider correlation plus reread yields applied, rolled back, failed or visibly partial/uncertain without a
   second elevation. Count/byte/replay-fence N+1 leaves the kernel/config untouched and terminal input remains
   inside its p99 budget during all256 target monitors.

## ADR-067 off-screen terminal parking oracle

`ACP-RUN-022` is independent of Eco: it never exits an agent or claims provider resume.

1. A view switch immediately creates one `TerminalWarmViewPark`. It retains renderer/projection/cache for at
   most five minutes without changing PaneAttachment or runtime; selection before expiry restores the same
   generation. Exactly twelve warm parks fit one Surface. The thirteenth and memory-pressure mutations evict
   only the oldest quiescent renderer, keep the hard bound and leave PTY, bytes, drafts and Attention unchanged.
2. A separate generated matrix pins Node, Surface, viewer, PaneAttachment, RuntimeAttempt, durable handle and
   generations plus visible/selected/watched/input/Attention/agent-state evidence. At `10m−1` the attachment
   remains; at `≥10m` only a continuously off-screen current local tmux-backed or equivalently durable client
   may detach. Plain-shell, remote-uncertain, working, waiting, blocked, unknown, Attention-bearing, selected,
   watched and input-active rows cancel or refuse that detach generation.
3. The zero-watcher safety sweep is reconciliation for the same eligible detach, requires exact durable backing,
   zero live painter/relay/shadow/writer watcher and ten continuous unwatched minutes. It releases only Turn's
   stale client PTY/attachment and never the durable Session, process tree, scrollback, runtime identity or
   recovery evidence. CAP-105 pressure may request the sweep but cannot originate or widen eligibility.
4. A zero-PTY control/shadow client observes ordered bounded output and accepts background input only through
   the same current InputLease/InputSafety/backpressure sequence as an attached painter. Wrong session,
   generation, owner, duplicate sequence, overflow and reconnect gap write no bytes and resnapshot explicitly.
   An unrecoverable bounded observation gap sets the exact Attempt to Unknown and emits one deduplicated
   observability Attention without inventing prompt/completion. Background lost reply transfers possible effect
   to its RuntimeInputReceipt and is never resent; wake input reaches exactly `transferred_once|expired|cancelled`.
5. Selection, approach, Attention route or current input automatically attaches/repaints the same runtime once;
   no launch, process respawn, generic `Start pane` or second gesture exists. Crashes/lost replies around park,
   release and attach reconcile by exact handle and publish truthful xterm/viewer/PTY/tmux/process charges.
   End/Delete/target loss/daemon shutdown fences Park/Detach/Wake and retires shadow/writer; a hung fixed child
   transfers its exact count/RSS/queue/output charge to ProcessCleanupCharge and never vetoes row removal.

## ADR-067 rejection of automatic durable-session reaping

`ACP-SAF-021` mutates every source `session-budget.ts` trigger—age, detached count, memory, swap, PSI,
external/PTY pressure, grace, batch and LRU order—and asserts that the resulting termination plan is always
empty. The only permitted automatic effects are canonical Status/Attention and a CAP-107-proved reconstructible
viewer/client trim. No trigger may call tmux kill-session, terminate a process, End/Delete a Session, fabricate
an Eco grant or turn unknown evidence into eligibility. A separate foreground typed termination must pin exact
target, handle, start identity and generation, show consequences and durably receipt/reconcile one effect.
