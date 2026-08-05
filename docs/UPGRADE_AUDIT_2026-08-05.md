# Unified hierarchy upgrade audit — 2026-08-05

## Verdict

The upgrade is implemented as a domain and authority correction for the first reproducible vertical. Turn
now has one persistent Workspace → Session → Agent/Tool → Child navigator, one canonical writer for a
primary checkout inside one canonical Turn data directory, background subagents independent of panes, durable semantic previews and node-specific
Attention. The shared primary Git checkout was not modified during this work; implementation happened on the
dedicated `codex/unified-hierarchy-20260805` worktree.

This verdict means the normative vertical is automated and regression-tested. It does **not** mean the app
is release-ready: authenticated live-Claude acceptance, migration-reconciliation UX, performance budgets,
packaging and platform accessibility sign-off remain open.

## Normative surface audited

| Requirement | Delivered invariant | Reproducible evidence |
| --- | --- | --- |
| One persistent hierarchy | The left AccessKit tree is the only persistent navigator; no Session tabs, thumbnail strip or second Agent tree | 20 native GUI tests, including 12 PNG baselines and a dense 30-Session fixture |
| One primary-checkout writer | One process owns the canonical data directory before Store/restore; canonical checkout identity, store-wide monotonic fence, atomic Session+lease creation and typed alternatives enforce one writer inside that authority domain | Same-data-dir/different-socket, data-dir alias, crash recovery, rollback, stale generation and conflict tests |
| Safe parallel alternatives | Read-only Sessions refuse a write-capable launch without a technical guard; isolated Sessions create a real independent Git worktree and retain shared-resource declarations | Session request and real-worktree integration tests |
| Agent independent of Pane | A node has zero-to-many bindings; closing a view never stops the Agent | Reviewer vertical and temporary-pane tests |
| Background subagents | `Reviewer` is inserted under Claude with declared name and relationship confidence, without layout/focus mutation | Hook-transport vertical and tree projection tests |
| Honest worker attribution | Worker callbacks without `agent_id` target one live child only as `inferred_high`; ambiguous callbacks and explicit unknown ids remain node-less/`unknown` under their authenticated parent | Single-child, out-of-order-id and two-parent hook-transport tests |
| Safe Activity Preview | Semantic/stable text is ANSI/CR/noise-normalised, redacted before persistence, bounded and returned newest-first | Preview, secret-on-disk, restart and Quick Preview tests |
| Attention coordination | Entries are per exact node or durable parent/external-id scope; lifecycle cleanup, deferred focus and queue triage preserve that same subject boundary across mute/snooze/dismiss/restart | Core, store, restart, daemon and GUI Attention suites |
| Honest permission detail | The banner resolves an exact Agent (including a primary Agent projected before the tree); scoped node-less and stale identities never borrow command, cwd or risk from the primary Agent | GUI permission-attribution tests |
| Honest restart | A daemon fences every unreleased lease before loading, relaunches nothing, restores exact interaction entries only while their runtime owner is corroborated, preserves marked postmortem evidence and does not open Panes | Store and daemon restart suites |
| Checkout launch boundary | Session, template, Pane, init and relaunch cwd values must canonically remain inside the assigned checkout at creation and immediately before PTY spawn | Absolute, `..`, symlink, persisted-Layout and worktree-to-primary adversarial tests |
| Template intent survives conflict | Safe read-only/worktree retries retain only Template identity/inputs; the daemon reapplies the authoritative Layout/env/Attention/tmux/naming contract and maps worktree cwd values | GUI request-shape, daemon unit and real-Git end-to-end tests |
| Surface-scoped temporary views | Temporary Pane bindings are visible only to their live UI surface and expire on replacement/disconnect/restart without stopping the node or changing Layout | Two-surface, reconnect and daemon-restart tests |
| Crash-consistent runtime projection | Session/tree/preview, event log and resulting Attention Queue commit together before any push/effect; failed checkpoints form one FIFO runtime barrier | Fault-injection, exit-order and real-restart tests |
| Hostile/durable text boundary | Navigation identities reject control/bidi/invisible input; discovered Agent/process projections are sanitised/bounded; every durable free-text write is credential-redacted | Reducer/protocol/restore tests plus byte scans of SQLite and WAL |

## Findings corrected during the audit

1. **Launch-directory escape.** A stored or requested Pane could name another checkout with an absolute,
   relative or symlink path. Creation and final PTY launch now fail before lease, Layout or process side
   effects when canonical containment is false.
2. **Subagent attention misattribution and over-broad resolution.** Claude worker notifications delivered
   through a parent hook endpoint could blame the parent, assign an explicit unknown id to a different
   unique child, or clear ambiguous demands belonging to another parent in the Session. Correlation now
   preserves an exact node or durable `(parent, external id?)` scope; ambiguity remains `unknown`, survives
   migration 007/restart and only its matching response resolves it.
3. **Two daemons could mutate one data directory.** Socket exclusivity did not stop another daemon using a
   different socket from opening the same SQLite database and fencing the live owner's leases. `turnd` now
   acquires a non-blocking OS lock on canonical `data_dir` before Store, migration or restore. Symlink aliases
   contend, and a real `SIGKILL` test proves the kernel releases ownership for recovery.
4. **Lease auto-adoption after daemon restart.** A new daemon could treat persisted authority as its own.
   Restore now fences every unreleased lease as `recovery_required` before loading Sessions; heartbeat and
   launch accept only `active` authority acquired by the current explicit flow.
5. **Lossy Attention restoration.** Replaying stored demands minted new ids and lost age, snooze,
   acknowledgement and priority. Restore now retains the exact durable queue, removes non-surviving
   interaction demands whose runtime owner no longer runs, and preserves entries explicitly marked
   `survives_owner_exit` as postmortem failure/completion evidence.
6. **Reversed Quick Preview history.** The limit was applied to the newest rows but a daemon reversal made
   the client highlight the wrong end. Store, protocol and GUI now use newest-first consistently.
7. **Hostile hook retention.** Raw Claude callback bodies no longer enter the event log, and migration 005
   physically clears historical hook bodies from SQLite and its WAL while retaining typed facts.
8. **Legacy lease ambiguity.** Migration 006 never grants authority to pre-existing rows, canonicalises
   identity and marks affected Workspaces for explicit reconciliation instead of selecting a recent Session.
9. **Orphaned lifecycle Attention and deferred focus.** A node-less worker demand could survive the death
   or stop of the runtime that owned it, and direct retirement paths changed memory without durably updating
   SQLite or clients. Lifecycle resolution now distinguishes owner scopes from exact live children, correlates
   declared worker ids under the authenticated parent, persists every queue mutation and keeps failure itself
   as a new demand. Snooze, dismiss and mute cancel only their matching deferred jump; one sibling demand can
   no longer authorise another subject's delayed focus.
10. **Permission-detail substitution.** When an unresolved or stale worker demand headed the queue, the client
    could fall back to the primary Agent and display its command, cwd and risk under the worker's attention id.
    The primary summary is now used only when its node id is the exact target, or as an explicit compatibility
    fallback for a fully unscoped legacy entry. Modern provisional scopes remain visible in the queue without
    invented permission detail.
11. **Typing protection expired during a long burst.** The client reported only the first key transition, while
    the daemon independently expired `last_keystroke_ms` after the 1.5-second grace. Continuous typing could
    therefore release a deferred focus into the wrong terminal. Activity reporting now coalesces a bounded
    heartbeat while the timestamp advances and schedules its own wake-up before the daemon's grace can expire;
    it remains far below one request per character.
12. **Lease ownership and tree truth could diverge.** Acquisition accepted a Session assigned to another
    checkout, archive paths could hide a still-blocking owner, and hierarchy projection could omit that owner.
    Lease validation now binds Workspace, Session and checkout atomically; Session/Workspace archive refuses
    unreleased ownership, and every live/recovery owner remains navigable even if stale metadata says archived.
13. **The removed overview still existed as hidden runtime.** The command/shortcut was gone visually, but
    Session-overview feed state and repaint work still formed a second navigator. Command, feed, module,
    repaint cadence and route are deleted; the parsed terminal screen now serves attached panes, on-demand
    preview and heuristics only.
14. **Node-less Attention was either invisible or falsely assigned.** A legitimate Session-scoped demand
    with no exact Agent did not raise aggregate status; making the parent `RUNNING/YOUR TURN` would have lied.
    Workspace/session badges and queue now surface it while the parent node retains its own state.
15. **Temporary panes survived their UI owner as phantoms.** A persisted-looking binding could appear after
    reconnect but could not be focused by the new surface. Temporary bindings are surface-owned and cleared
    on connection replacement, final disconnect and daemon restart; permanent bindings and Agent lifetime
    are untouched.
16. **Runtime state, event log and Attention were separate commits.** Disk failure could leave a permission
    without its Agent, a tombstone without its Stop or a stale queue after restart. One Store checkpoint now
    writes Session → event → Attention transactionally and publishes only after commit. A unified FIFO barrier
    also keeps later exits unapplied, and virtual descendants retire inside the same checkpoint. All protocol
    requests retry that checkpoint and fail `unavailable` while it remains blocked, preventing reads of the
    speculative projection and rejected mutations that could otherwise hitch a ride on recovery.
17. **Secret redaction covered only selected routes.** Commands, names, Template/Layout metadata, settings,
    Attention and Agent/process details could retain a recognisable credential. Every repository now builds
    from a redacted durable copy; ids/FKs are stable, and authority-bearing paths fail instead of being
    rewritten. Byte-level tests seed every free-text route and scan both database and WAL.
18. **Typed hierarchy metadata was treated as safe display text.** C1 CSI/OSC tails, bidi/zero-width labels,
    huge Agent tasks and unbounded process argv could forge or exhaust navigation, inspector and SQLite.
    User navigation names are rejected; discovered fields are normalised once before state/push/store and
    argv has count, per-item and aggregate caps. The supervisor retains raw values only for classification.
19. **A lease-conflict fallback lost its Template.** The GUI flattened no authoritative details and could
    silently create a blank/shell read-only or worktree Session instead of Coding. New typed Template-safe
    requests preserve the original intent; the daemon loads the Template, keeps an unenforced read-only
    launch stopped, and a
    real Git test proves worktree Panes start only under the isolated checkout while the primary stays clean.
20. **Current-write redaction did not clean historical durable bytes.** A credential written by an older
    build could survive in a live cell, free SQLite page or WAL. Migration 009 leaves a retry marker; open
    classifies every `TEXT` column, redacts free text in one transaction, rejects unsafe structural rewrites
    or correlation collisions, rebuilds the database and verifies WAL truncation before clearing the marker.
21. **A long permission command could become a misleading approval.** Display truncation could hide a
    destructive suffix while preserving an apparently actionable permission. Over-limit semantic commands
    are now refused whole; the compact summary may be shortened, but the command the user judges never is.
22. **The legacy credential purge was not exclusive or failure-safe enough.** Migration repair now restricts
    DB/WAL/SHM/journal permissions before reading, holds an exclusive SQLite lock across logical scrub,
    checkpoint and `VACUUM`, works in bounded batches, preserves unknown typed JSON fields through generic
    redaction, and clears its retry marker only after physical verification. Busy sidecars, unsafe
    operational identities, redaction collisions or permission failures abort closed and leave the marker.
23. **Restore results could offer a relaunch with no durable target and overclaim reconnection.** Every
    `PaneRestoreOutcome` now carries a required `node_id`; current daemon restart emits `Orphaned`/`Lost`,
    reserves `Reconnected` for a backend that can prove PTY reattachment, and keeps relaunch node-addressed.
24. **Session Attention and Agent runtime state were visually conflated.** An exact Agent waiting for input
    now remains `WAITING` (or `PERMISSION`) while the owning Session is labelled `YOUR TURN`; a scoped
    node-less child demand badges the Session/Workspace without relabelling its running parent. Obsolete
    overview/event-log toggles were removed and the remaining command explicitly focuses the unified tree.
25. **Hierarchy pushes could mix surfaces or stay at a stale revision.** Pane-binding replacements are now
    projected separately for each connected `surface_id`, filtering temporary views while retaining durable
    bindings. Workspace create/rename/archive/duplicate and Session archive/unarchive increment and publish a
    hierarchy revision, with two-surface and stale-revision regressions.
26. **Archived entities and lease release left authority gaps.** Archived Workspaces/Sessions are rejected
    for every Session-creation or lease-acquisition path. Fenced release and the owner's transition to
    `ReadOnly` now commit in one `IMMEDIATE` transaction, update memory only after success and roll both
    changes back on an injected failure.
27. **Semantic Attention had no honest input route for a background Agent without a PTY.** `Next Attention`
    now keeps/selects the exact subject (for example Reviewer) while resolving a separate existing Pane for
    the authentic runtime that can receive input. It traverses only integrated/explicit `spawned_by` or
    `owns_process` ancestry, never crosses a distinct child runtime or provisional edge, never attributes
    the demand to the parent and never opens a Pane implicitly. `PaneFocusView` returns both identities.

## Validation performed on the integrated branch

All commands below completed successfully on macOS, serialising OS-resource tests where required:

```sh
cargo test -p turnd --test agents \
  the_reviewer_vertical_crosses_the_real_claude_hook_and_survives_a_ui_restart \
  -- --exact --test-threads=1 --nocapture

cargo test -p turnd --test agents \
  an_idless_worker_permission_round_trips_through_hooks_to_the_reviewer \
  -- --exact --test-threads=1 --nocapture

cargo test -p turnd \
  two_hook_parents_keep_out_of_order_and_idless_attention_in_their_own_scopes \
  -- --test-threads=1 --nocapture

cargo test -p turnd \
  data_directory_and_socket_ownership_are_independent_and_recoverable \
  -- --test-threads=1 --nocapture

cargo test -p turnd \
  a_data_directory_rejects_another_socket_and_recovers_after_sigkill \
  -- --test-threads=1 --nocapture

cargo test --workspace --all-targets --no-fail-fast -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The workspace run includes 20 native GUI tests, 12 of which compare committed PNG baselines without
regenerating them. The exact Reviewer test crosses the real loopback Claude hook transport and production
normaliser, closes a temporary view without stopping the worker, reconnects a replacement UI to the same
daemon and verifies that relationship, name, preview, live process and Layout facts survive. Separate daemon
restart tests restore durable metadata as `Orphaned`/`Lost` and prove that nothing is relaunched.

## Known limits and next gates

- **Authenticated Claude smoke:** the deterministic and transport proofs do not exercise paid credentials,
  the currently installed binary, signing or a human permission exchange in the packaged window.
- **Migration 006 reconciliation:** upgraded Workspaces intentionally remain fail-closed until an audited
  user flow proves the legacy writer stopped and clears `lease_reconciliation_required`.
- **Read-only execution:** when no technical guard exists, the mode is honestly
  `read_only_enforced=false` and launches no Template process. A platform/tool sandbox is still required
  before it can run write-capable CLIs rather than remain a layout/review record.
- **Containment is not a sandbox:** Turn constrains the initial cwd. Same-user code can later `chdir` or open
  another path; containers/sandbox profiles are a separate security boundary.
- **Process lock trust boundary:** the canonical-data-directory `flock` is advisory on a supported local
  filesystem and protects cooperating Turn daemons. It is not a sandbox against the same account deliberately
  unlinking/replacing the lock inode or mutating SQLite outside Turn.
- **Cross-installation checkout authority:** two deliberately separate `--data-dir` stores do not share a
  checkout-scoped OS lock and can each claim the same path. Host-global exclusivity remains a hardening gate.
- **Opaque credentials:** every durable free-text class is covered by sensitive-key and known-shape
  redaction, but an unlabelled credential with no distinctive shape cannot be identified reliably without
  unacceptable false positives. Raw terminal streams remain ephemeral; arbitrary diagnostic persistence
  requires a separate opt-in threat review.
- **Recovered Preview marker:** timestamps and confidence survive, but the UI still needs a distinct stale /
  recovered marker instead of merely showing the old timestamp.
- **Reconnection identity:** stored PIDs/commands support conservative restore diagnostics, but successful
  PTY reattachment/PID-reuse proof is incomplete; `Lifecycle::Reconnected` is not yet a general restore path.
- **Product hardening:** tree search/filter/manual ordering, relationship correction, full context menus,
  Expand/Collapse all, technical process mode, IME and screen-reader acceptance, performance measurement at
  30 Workspaces/100 Sessions/hundreds of processes, Linux visual sign-off and packaging remain.

See `docs/UNIFIED_HIERARCHY_UPGRADE.md` for the normative model, `DECISIONS.md` ADR-040 for the accepted
architecture and `docs/PROTOCOL.md` for protocol-v3 recovery and ordering contracts.
