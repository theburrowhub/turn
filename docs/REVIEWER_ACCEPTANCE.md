# Authenticated Reviewer acceptance

Issue [#6](https://github.com/theburrowhub/turn/issues/6) is complete only after the
whole scenario below passes against an installed, authenticated Claude Code. Fixture
and recorded-payload tests remain the deterministic CI floor; they are not substitutes
for this run.

## Local macOS bundle

`make macos-app` builds all three release binaries, lays them out as packaged siblings,
copies the checked-in icon and `Info.plist`, ad-hoc signs the development bundle and
verifies its nested signatures. The default output is `dist/Turn.app`; an existing
bundle is never overwritten.

This is deliberately an acceptance artifact, not the release artifact tracked by
#19. It is not Developer ID signed or notarized and has no updater.

```sh
make macos-app
codesign --verify --deep --strict --verbose=2 dist/Turn.app
```

## Reproducible live harness

The opt-in test connects to the daemon already launched by the packaged app. It never
runs in normal CI and refuses to use an account unless `TURN_LIVE_CLAUDE=1` is present.
Use a disposable Git repository: the test grants Claude bypass permissions only inside
that explicit root so an unattended permission prompt cannot make the result
ambiguous.

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
cargo test -p turnd --test live_claude \
  reopened_packaged_app_restores_live_claude_reviewer_vertical \
  -- --ignored --exact --nocapture --test-threads=1
```

The harness proves the main-checkout lease, real PTY, explicit parent/name/relation,
stable preview, absence of an invented subagent PID or Pane, Quick Preview data,
temporary Pane close semantics, saved Layout and the live UI-reconnect boundary. It
also records the terminal modes reported by the actual Claude TUI. Paste and submit
are deliberately separate PTY writes: Claude correctly keeps Enter inside the editor
when it arrives in the same read as a bracketed paste.

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
