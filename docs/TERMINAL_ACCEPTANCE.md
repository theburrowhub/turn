# Terminal interaction acceptance

This is the reproducible acceptance artifact for search and scrollback, links, path drops,
appearance preferences, text input and full-screen terminal modes. It deliberately does not
open Turn or a browser.

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
| Apply terminal/UI font size, zoom, block/bar/underline cursor, cursor blink/reduced motion and optional programming ligatures live | `theme::tests::every_appearance_control_changes_the_values_the_renderer_reads`, `app::tests::appearance_settings_are_installed_into_the_live_context_without_a_restart` and the cursor/ligature renderer tests |
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
terminal contract.

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

