# Authenticated Reviewer acceptance

Issue [#6](https://github.com/theburrowhub/turn/issues/6) is complete only after the
whole scenario below passes against an installed, authenticated Claude Code. Fixture
and recorded-payload tests remain the deterministic CI floor; they are not substitutes
for this run.

This document proves one historical provider vertical only. It is not evidence of cross-provider parity,
the ADR-064 capability set or completion of the operator control plane. Provider-specific observations below
must remain scoped to the recorded adapter/version/account rather than becoming product-wide claims.

## Local macOS bundle

`make macos-app` builds all three release binaries, verifies their versions/protocols,
lays them out as packaged siblings, copies the checked-in icon and `Info.plist`, applies
ad-hoc hardened-runtime signatures and verifies the sealed bundle. The default output
is `dist/Turn.app`; an existing bundle is never overwritten. This is the credential-free
acceptance form of the same topology the Developer ID/notarized tag workflow publishes;
see [RELEASE.md](RELEASE.md) for the production archive and updater contract.

```sh
make macos-app
codesign --verify --deep --strict --verbose=2 dist/Turn.app
```

## Reproducible live harness

The opt-in test connects to the daemon already launched by the packaged app. It never
runs in normal CI and refuses to use an account unless `TURN_LIVE_CLAUDE=1` is present.
Use a disposable Git repository so the run leaves no project history behind. The test
keeps Claude in `default` permission mode, never bypasses its permission system, and
enables experimental Agent Teams only in the launched Pane's environment. The project
root is test-data isolation, not a host security boundary.

The harness submits no permission response. ADR-064's separately accepted remote typed-response path does
not weaken that property: it must use an exact foreground-desktop-issued single-use grant and provider
evidence, and must never masquerade as raw PTY input or a bypass mode.

```sh
acceptance_root="$(mktemp -d /tmp/turn-reviewer.XXXXXX)"
mkdir -p "$acceptance_root/data" "$acceptance_root/project"
git -C "$acceptance_root/project" init -b main
cp README.md "$acceptance_root/project/README.md"
git -C "$acceptance_root/project" add README.md
git -C "$acceptance_root/project" \
  -c user.name='Turn Acceptance' \
  -c user.email='turn-acceptance@example.invalid' \
  commit -m 'fixture: add acceptance readme'

open -n -F \
  --env "TURN_DATA_DIR=$acceptance_root/data" \
  --env "TURN_SOCKET=$acceptance_root/turn.sock" \
  "$PWD/dist/Turn.app" --args --socket "$acceptance_root/turn.sock"

TURN_LIVE_CLAUDE=1 \
TURN_LIVE_CLAUDE_SOCKET="$acceptance_root/turn.sock" \
TURN_LIVE_CLAUDE_PROJECT="$acceptance_root/project" \
TURN_LIVE_CLAUDE_BIN="$(command -v claude)" \
TURN_LIVE_CLAUDE_EVIDENCE="$acceptance_root/live-evidence.json" \
TURN_LIVE_CLAUDE_DEBUG="$acceptance_root/claude-debug.log" \
cargo test -p turnd --test live_claude \
  packaged_app_runs_authenticated_claude_reviewer_vertical \
  -- --ignored --exact --nocapture --test-threads=1
```

The first test must pass before closing the GUI. Close only `Turn.app`, verify that the
same `turnd` and Claude PIDs still own the socket/PTY, reopen the same bundle and run:

```sh
TURN_LIVE_CLAUDE=1 \
TURN_LIVE_CLAUDE_SOCKET="$acceptance_root/turn.sock" \
TURN_LIVE_CLAUDE_EVIDENCE="$acceptance_root/live-evidence.json" \
cargo test -p turnd --test live_claude \
  reopened_packaged_app_restores_live_claude_reviewer_vertical \
  -- --ignored --exact --nocapture --test-threads=1
```

The harness proves the main-checkout lease, real PTY, explicit parent/name/relation,
stable preview, absence of an invented subagent PID or Pane, Quick Preview data,
temporary Pane close semantics, saved Layout and the live UI-reconnect boundary. It
also records the terminal modes reported by the actual Claude TUI. Paste and submit
are deliberately separate PTY writes. The one-line acceptance prompt is inserted as
plain text so delivery is independent of when Claude enables bracketed paste; the
reported terminal mode is still captured and verified independently.

## Manual window checklist

Automation does not replace the visible interaction check. During the same run:

1. Confirm the app was opened from `Turn.app` and `turnd`/`turn-hook` resolve from
   `Contents/MacOS`, not Cargo or `PATH`.
2. Confirm the Workspace and `Live Claude → Reviewer` Session show one active
   main-checkout write lease.
3. Type the Reviewer prompt into Claude's visible PTY and submit it.
4. In the unified tree, verify Reviewer has the declared name, Claude parent,
   `spawned_by` relationship, explicit confidence, live state and activity preview.
5. Confirm no iTerm window, permanent Pane or OS-focus change occurred.
6. Select Reviewer and press Space. Quick Preview must open without changing Pane
   focus or Layout.
7. Press Cmd+Enter, inspect the temporary Preview/Details Pane, then close it with
   `Keep processes`. Reviewer must remain in the tree and the saved Layout must remain
   one Pane.
8. Exercise selection/copy and a bracketed paste in Claude, resize the Turn window and
   interact with one mouse-aware Claude control. Confirm colour and alternate-screen
   rendering remain intact.
9. Close only Turn.app. Confirm `turnd` and Claude remain alive, then reopen the bundle.
10. Confirm the same tree edge, Layout, preview, write lease and live terminal return.

## Accepted successor live-capability matrix

ADR-064 adds a provider-neutral acceptance layer above this historical vertical. Every adapter for Claude
Code, Codex, Gemini, OpenCode, future/custom agents and the generic terminal fallback runs the same
deterministic capability contract. Each capability that depends on a live provider additionally needs one
current authenticated packaged smoke for that exact adapter/CLI/provider/AccountProfile/ExecutionTarget;
one provider's pass cannot bless another provider or an unknown version.

The successor record must include:

| Capability area | Required packaged evidence |
| --- | --- |
| Foreground activation | Select one inactive safe Session once and observe exactly one idempotent activation, restored/attached attempts and the exact bounded eligible saved-runtime set—or one configured default Shell when empty—with no second action. Repeat with changed account/target/command/authority and observe zero launch plus one recovery action. Selecting child/history/resource rows remains launch-free. |
| WorkItemSource | Project one externally sourced card through bounded pages; preserve exact source/project/item identity, mapping, assignee and field authority; exercise compare-and-swap update, conflict and timeout reconciliation. Dismiss/archive/local deletion sends no source close/delete. Credentials never enter evidence. |
| Native jobs | Enumerate a stable Job and ordered iterations, correlate one exact runtime/result and record provider schedule/revision/survival evidence. Advertised pause/resume/run-now/cancel/delete operations each produce their own receipt; local hide/delete and daemon restart send none. A Turn Flow is visibly different. |
| Conversation inventory | Query bounded private history/search for two profiles/targets with truncation and similar titles. Search creates no Nodes or runtimes. Adopt creates one stopped exact-key owner and sends no input; separately preflighted resume creates one new attempt. Cross-endpoint duplicate ownership is refused installation-wide. |
| Title and profile observations | Exercise `title_read` with rename absent and rename with read stale/absent, proving independent capability and failure states. Record requested/effective model/flags, context windows, quota windows and bounded conversation/job/Attention activity per exact profile/target with source, coverage and freshness; partial/error data is never zero and never crosses profiles. |
| Web and Browser | Select an inert Web Resource and prove zero script/network/navigation. In a separately created isolated Browser, exercise typed address/history and reviewed local HTML/localhost, reject origin rebinding, popup/download escalation and page/script control messages, then restore metadata with zero automatic reload. |
| Remote permission | From the foreground desktop issue one narrow expiring encrypted grant for one known typed provider option. A full remote GUI or companion sends one allow/deny and waits for provider evidence. Replay, widening, stale/offline/cross-profile use and raw remote PTY bytes at that sensitive interaction fail server-side. Credentials, grant changes, administration and host trust stay local; generic unclassifiable TUIs receive no fabricated guarantee. |
| Client-class separation | Replace/reconnect a full remote GUI using its revisioned WorkSurface and input lease, then independently reconnect a headless status client and companion. Captures prove the latter two expose only their closed projections/allowlists and cannot inherit full GUI or arbitrary terminal authority. |

The canonical left tree, one selected WorkSurface and one logical Attention Queue must remain visible
throughout. The run records effective capabilities and unavailable/degraded states, not provider marketing
names. A screenshot or a provider title without protocol receipts and negative side-effect checks is not
passing evidence.

## Passing run record — 2026-08-10

Environment:

- Turn base commit `f494817`, local ad-hoc signed arm64 bundle built from the isolated
  issue worktree.
- macOS 26.5.2 (25F84), arm64.
- Claude Code 2.1.226, authenticated with the installed first-party Team account.
- Claude `default` permission mode with Agent Teams enabled only in the Pane. No
  permission was bypassed or approved by the harness.
- Fresh persistent data directory, socket and one-file Git repository.

Both ignored tests passed. The authenticated run produced one uniquely declared live
Reviewer with explicit `spawned_by` confidence, the real Claude Agent as parent, a
stable activity preview, no PID, no Pane binding and an `Alive` lifecycle after its
temporary Pane was closed. The main-checkout write lease remained active and the saved
Layout remained one Pane. Real hook-derived events covered `SessionStart`,
`UserPromptSubmit`, the Agent Teams declaration, subagent start/stop callbacks and
completed turns; SQLite retained zero raw hook payloads.

The terminal evidence was 44×132 cells with 1,449 styled cells and bracketed paste
enabled after the editor settled. Claude 2.1.226 rendered this run on the primary
screen (`alternate_screen=false`) and requested no terminal mouse reporting
(`mouse_mode=None`), unlike the 2.1.224 run below. Those values are recorded as real
version differences rather than fabricated capabilities. Resize, paste/submit and the
restored primary-screen terminal all passed.

Closing GUI PID 36573 left daemon PID 36575, hosting shell PID 36596 and Claude PID
36653 alive. Reopening produced GUI PID 36919 against the same daemon, socket, PTY,
Session id and write lease; the restoration test passed immediately. Targeted window
captures before and after reopening were inspected locally and showed the packaged
app, one Pane, styled Claude output and Reviewer's report with no external terminal or
extra Turn Pane. The captures are not checked in because they include account-local UI
labels; the redacted facts above are the durable evidence.

Claude emitted both the Agent Teams declaration and the tool's terminal subagent
callback lifecycle. Turn kept the uniquely declared Reviewer as the live semantic
teammate and the callback record as an exited child, matching the two distinct external
identities rather than aliasing them.

After review hardening, the final harness was repeated against the same signed bundle
with plain-text prompt insertion. The authenticated vertical passed in 14.39 seconds,
recorded 1,438 styled cells and the same explicit live Reviewer invariants, then passed
the GUI-only close/reopen restoration test against the surviving daemon and Claude
processes. Every process created exclusively for that confirmation was stopped after
the restoration check.

## Run record — 2026-08-07

Environment:

- Turn commit `9c7320e`, local ad-hoc signed arm64 bundle.
- macOS 26.5.2 (25F84), arm64.
- Claude Code 2.1.224, authenticated with the installed first-party Team account.
- Isolated persistent data directory, socket and one-file Git repository.

Passed before the account stopped the scenario:

- LaunchServices opened the packaged `turn`; it launched the packaged sibling `turnd`
  and found the packaged `turn-hook`. The GUI connected on protocol 4.
- The harness created a main-checkout Session with an active write lease and launched
  `/opt/homebrew/.../2.1.224/claude` at native/structured integration level.
- The actual Claude grid reported 40×120 cells, alternate screen, bracketed paste,
  `any_motion` mouse reporting, a visible cursor and 76 styled runs. Resizing and the
  bracketed-paste path both succeeded.
- Real hook traffic produced these redacted, durable facts (raw callback bodies are
  intentionally never persisted):
  - `SessionStart` → `agent.started`, external id present, model
    `claude-sonnet-5`, tool `claude-code`.
  - `UserPromptSubmit` → `agent.turn_started`, with the bounded Reviewer prompt
    excerpt.
  - `StopFailure` → `agent.failed`, reason `rate_limit`.
- Claude's hook debug log recorded both loopback HTTP callbacks returning 200 with an
  empty body. No callback URL or token is retained in this document.
- Closing GUI PID 27849 left daemon PID 27871 and Claude PID 30176 alive. Reopening
  produced GUI PID 31313, which reconnected to daemon PID 27871 and recovered the live
  alternate-screen terminal. These PIDs are run evidence, not stable expectations.

Blocked acceptance point:

Claude rejected the first paid turn with `You've hit your individual spend limit` and
offered to notify an administrator. That request was cancelled; Turn did not act on the
user's behalf. No Reviewer was created, so the relationship/preview/temporary-Pane
half of this authenticated run is **not passed** and issue #6 must remain open. Re-run
the two ignored tests and the manual checklist after account capacity is restored.

## Deterministic floor

While the external account is unavailable, these commands still prove the full
in-product semantics without making the live claim:

```sh
cargo test -p turnd --test agents \
  the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart \
  -- --exact --test-threads=1 --nocapture
cargo test -p turn-gui --test snapshots -- --test-threads=1
cargo test -p turn-proto \
  a_full_screen_program_reports_its_alternate_screen_and_its_input_modes \
  -- --test-threads=1
```
