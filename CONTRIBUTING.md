# Contributing to Turn

Short on purpose. Read `ARCHITECTURE.md` for how the system fits together and `DECISIONS.md` for why it is
shaped the way it is.

## The rules a change must not break

**1. A heuristic can never move the user's focus.**

This is the product. Terminal-output inference is capped at `Confidence::InferredHigh` by its
`EventSource`, and `AttentionPolicy::resolve` degrades any focus action to `Badge` when the confidence
fails `may_steal_focus()`. Both points are enforced independently and tested separately
(`event::tests::a_heuristic_cannot_promote_itself_to_explicit`,
`policy::tests::a_guessed_permission_badges_instead_of_stealing_focus`,
`manager::tests::a_guessed_permission_never_produces_a_focus_effect`). Belt and braces is the point; do
not remove either.

A missed notification costs the user a glance. A false focus change costs them the thought they were
holding, and teaches them not to trust the tool. Guesses may badge, highlight, notify and enqueue — every
channel the user consults on their own schedule. They may not use the one channel that consults the user on
Turn's schedule.

**2. Turn never acts on the user's behalf.** No auto-approving a permission. No auto-relaunching a process
on restore — Turn offers, the user decides. No executing a command inferred from agent output. These are
enforced structurally: `risk::assess` authorises nothing, the hook server always replies with an empty 200,
and `turn-proto` has no request that approves, no request that runs an inferred command, and exactly one
(`RelaunchNode`) that restarts anything.

**3. Never invent a parent-child relationship.** `Relation::Confirmed` only when a tool reported it,
`Relation::Inferred` for process-table guesses, `Relation::Unknown` otherwise — which renders at the
Session root rather than under a plausible-looking parent. Confirmed links are never downgraded.

**4. Bind local servers to `127.0.0.1` only, never `0.0.0.0`,** with a per-node random token.

**5. The window renders; it never decides.** `state_label`, `severity`, `score`, `provisional` and
`relation_is_provisional` all arrive computed from the daemon (ADR-032). Now that the client is Rust,
`turn-core` is importable from it and calling `DisplayState::derive` yourself is one line away — do not. A
client that computes is a client that can disagree with the daemon, and the symptom is a sidebar that
contradicts the terminal next to it.

**6. State is never signalled by colour alone.** Every state carries a glyph as well as a hue, and
`Theme::state_marker` returns both together so that a caller cannot take one without the other. Two tests
hold it structurally: `every_state_has_a_glyph_as_well_as_a_colour` and
`the_attention_colour_is_reserved_for_states_that_block_the_user`. If you add a `DisplayState`, the first one
fails until you give it a glyph, which is the intended experience.

## Code style — match the existing code exactly

- **English** for comments and identifiers.
- **Doc comments explain *why*, not *what*.** `///` and `//!` justify a design choice; they do not narrate
  the line below. Read `crates/turn-core/src/state.rs` and `crates/turn-core/src/attention/focus.rs` for
  the register: calm, specific, no marketing.
- **Test names are full sentences describing the guaranteed behaviour.** For example
  `fn a_crashed_process_never_keeps_claiming_it_awaits_you()`. Tests document the contract, so
  `cargo test -- --list` reads as a specification.
- **Test the real behaviour, including adversarial and malformed input.** `turn-pty` spawns real processes
  on real ptys and asks the tty itself via `stty size`. `turn-agents` asserts against payloads recorded
  from a live agent run and then mangles them. `turn-store` writes real SQLite files and greps them for
  secrets. No trivial filler.
- **Time is a parameter, never a clock read.** Every function that needs the time takes `now_ms: i64`, so
  the attention rules are deterministic and "an hour later" is an integer rather than a `sleep`.
  `turn_core::now_ms()` exists for the edges only.
- **No `TODO`, `unimplemented!()`, `panic!("not implemented")`, or stubs returning fake values.**
- **No `unwrap()` / `expect()` in non-test code**, except where a lock poison is genuinely unrecoverable.
  Return typed errors (`thiserror`).
- **Prefer small focused files.** Past roughly 600 lines, split into a module directory.

## Changing the window

`crates/turn-gui` is `eframe`/`egui` over `wgpu` — native, on the GPU, with no webview, no HTML and no CSS
(ADR-039). Two things follow that are easy to get wrong:

- **A visual change needs a snapshot.** `cargo test -p turn-gui` renders the real widget tree through `wgpu`
  with no display attached and diffs it against the PNGs in `tests/snapshots/`. If your change is meant to
  alter the picture, re-record with `UPDATE_SNAPSHOTS=1 cargo test -p turn-gui` and commit the image, so the
  diff a reviewer sees is the diff a user would see. If it is not meant to alter the picture and the test
  fails, that is the test doing its job — the first one caught two labels drawn on top of each other.
- **Accessible names are not optional and are not free.** There is no DOM, so anything a sighted user can
  read has to be put into the AccessKit tree deliberately via `widget_info`. This is currently a known gap:
  `every_session_row_is_reachable_by_its_accessible_name` is committed failing and `#[ignore]`d because the
  rows are painted rather than composed from widgets. Work that moves it toward passing is welcome; work that
  adds another painted-only region makes it worse.

## Dependencies

Shared dependency versions live in the **root** `Cargo.toml` under `[workspace.dependencies]`. Consume them
with `{ workspace = true }`.

Do not edit the root `Cargo.toml` or another crate's `Cargo.toml` as a side effect of your change. If you
genuinely need a new third-party crate, add it to your own crate's `Cargo.toml` with an explicit version and
say why in the commit message. `turn-hook` has **no dependencies on purpose** (ADR-026) — keep it that way.

## Running the tests

```sh
cargo test --workspace -- --test-threads=4
```

`--test-threads=4` matters: the `turn-pty` tests open real ptys, and a machine that exhausts the pty table
fails with a confusing `openpty` error rather than a test failure.

While `turnd` is being written, scope your runs to the crate you touched (`cargo test -p turn-agents`) so a
failure in the daemon is not mistaken for a failure in yours.

Before opening a change:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=4
```

All three pass on `main` as of 2026-08-04. They have not always, and the way they broke both times was a new
crate rather than a change to an old one — so if you are adding one, run them before you are asked to.

`cargo test --workspace` includes `turn-gui`'s snapshot tests, which need a GPU or a software rasteriser. On
macOS that is Metal and it just works. On a headless Linux box you need `mesa-vulkan-drivers` and
`libvulkan1`, and the committed baselines were recorded on macOS/Metal so they will not match there yet —
CI runs the comparison on macOS only and `.github/workflows/ci.yml` says why.

## Definition of done

1. `cargo test -p <your-crate>` passes, with real assertions.
2. `cargo clippy -p <your-crate> --all-targets` produces no warnings you introduced.
3. Zero `TODO`s and zero stubs.
4. Report the exact test count you got to green.

Run the commands. Do not claim success without having seen the output.

## Adding an agent adapter

One `impl AgentAdapter` in `crates/turn-agents/src/`, registered in `registry.rs`. Before writing it:

1. **Establish the tool's real contract empirically**, not from its documentation. Turn's own history is the
   argument: the documented Claude Code field is `user_prompt` and the real one is `prompt` (ADR-012), and
   the documented-looking Codex `hooks="/path"` form is rejected outright (ADR-013).
2. **Commit a fixture recorded from a live run** under `tests/fixtures/`, and a contract test asserting
   every field your adapter reads. That test failing after a tool upgrade is the system working.
3. **Report the level you achieved, not the one you wanted.** `LaunchPlan::level` may be lower than
   `best_level()`, with a `note` the Session details panel shows. See
   `codex::tests::without_hooks_the_launch_degrades_to_notify_and_says_so`, and the contract-level version
   in `crates/turn-agents/tests/contract_codex.rs`,
   `without_hook_trust_the_adapter_reports_wrapper_and_says_what_is_missing`.
4. **Never panic in `normalise`.** An adapter that panics takes the daemon's event loop with it. Drop
   unrecognised events rather than guessing at them: new releases add events, and they must not become noise
   or wrong states.
5. **Distinguish clearly between what you verified and what you assumed,** in the module's own doc comment.
   `codex.rs` does this explicitly, and it is why the one unverified assumption there is routed around
   rather than depended on.
