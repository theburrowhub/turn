# Functional v0.1.0 acceptance

This is the release gate for Turn's **functional v0.1.0 MVP**. It closes the
consolidated backlog tracked by issue
[#21](https://github.com/theburrowhub/turn/issues/21) without claiming that every
post-MVP distribution or platform sign-off is complete.

Run the complete automated gate from a clean checkout:

```sh
make mvp-acceptance
```

The target runs serially because PTY and loopback-hook tests use real operating-system
resources. It performs the full format, Clippy and workspace test gate, the measured
30-Session/120-Process performance harness, privacy/data-control acceptance, and the
macOS bundle/version/update acceptance path. On non-macOS hosts the bundle verification
continues to run in the macOS CI job, as documented in [RELEASE.md](RELEASE.md).

## Global definition of done

| Requirement | Evidence |
| --- | --- |
| Real `Workspace → Session → Claude → Reviewer` vertical from the packaged app | The passing authenticated Claude Code 2.1.226 run, exact environment, hook deviations, terminal modes and close/reopen PIDs are recorded in [REVIEWER_ACCEPTANCE.md](REVIEWER_ACCEPTANCE.md). The ignored harness remains reproducible against a current authenticated installation. |
| Closing and reopening the UI preserves the daemon, PTYs, hierarchy, Layout, Previews and terminal | The two packaged live-Claude harness phases plus the deterministic `agents`, `desk`, `conversation` and restart suites. Daemon death is deliberately a different, post-MVP boundary and never claims PTY reattachment. |
| Read-only is technically enforced and a primary checkout never has two writers | The Seatbelt integration tests, canonical checkout lock tests, fenced lease/reconciliation suites and real daemon-loss cases run by `make verify`. |
| Selection, active Session, Pane focus and Attention remain distinct | Domain, protocol, native-window and AccessKit tests; the consolidated visual/keyboard evidence is in [ACCESSIBILITY_ACCEPTANCE.md](ACCESSIBILITY_ACCEPTANCE.md). |
| Subagents never open Panes or change Layout without an explicit user action | Reviewer vertical, hierarchy projection, Quick Preview and permanent/temporary Pane tests. |
| Each backlog delivery has reproducible evidence | Issues #1–#20 are closed. Focused artifacts live in `docs/*ACCEPTANCE.md`, [PERFORMANCE.md](PERFORMANCE.md), [RELEASE.md](RELEASE.md) and [PRIVACY.md](PRIVACY.md); their code paths are also covered by the full workspace gate. |
| Behaviour-changing decisions and protocols are documented | `DECISIONS.md`, `ARCHITECTURE.md`, `PRODUCT.md`, `ROADMAP.md`, `docs/PROTOCOL.md` and `docs/SECURITY.md` describe the implemented v0.1.0 contract. |
| Suite, lint, snapshots and release binaries remain green | `make verify`, PR CI on macOS/Ubuntu, and `make release-acceptance`. |

## Automated gate composition

| Command | What it proves |
| --- | --- |
| `make verify TEST_THREADS=1` | Formatting, all-target Clippy, every workspace/integration/snapshot/doc test. |
| `make performance-acceptance` | Bounded queues/storage and measured responsiveness at 30 Sessions and 120 Processes. |
| `make privacy-acceptance` | Complete/redacted inventory, scoped and installation deletion, retention, telemetry-free operation and CLI protocol. |
| `make release-acceptance` | Three compatible release binaries, bundle topology, signatures, version rejection and daemon-safe update semantics. |

The focused terminal, Template, inspector, lifecycle, adapter, Attention and
accessibility targets remain useful while changing those areas. They are not repeated
by `mvp-acceptance` because `make verify` already runs their test binaries; the release
gate adds only the specialised performance, privacy and package-level proofs that the
ordinary workspace suite does not fully reproduce.

## Successor product-spec gate is separate

Passing this historical v0.1.0 gate is not completion evidence for ADR-059–ADR-064 or the operator
control-plane goal. The successor completion gate must additionally dispatch independent production-path
oracles for all of these ADR-064 obligations:

- one safe foreground Session selection performs at most one idempotent preflighted activation without a
  second start action, while every stale/unsafe case and every child/resource/history selection launches
  nothing;
- external WorkItemSource identity, mapping, bounded pagination/cache coverage, compare-and-swap writes,
  conflict/reconciliation, close/reopen, assignee/rate/credential isolation and source receipts;
- provider-native scheduled/background Job identity, ordered iteration history, survival evidence and
  independent list/create/update/pause/resume/run-now/cancel/delete capabilities, distinct from Flow
  recurrence and local projection deletion;
- private bounded ConversationInventory search/history with installation-wide exact ownership, advisory
  matching, stopped adoption and separately preflighted resume;
- inert Web preview versus an isolated interactive Browser with reviewed local HTML/localhost, typed
  navigation/history/popup/download/storage operations and no automatic restore load;
- independent provider `title_read` and `conversation_rename` capability/receipt/degradation paths;
- an exact single-use foreground-desktop-issued encrypted remote permission-response grant, provider-evidence
  closure and server-side blocking of raw remote PTY input during that known sensitive interaction, without
  extending the claim to an unclassifiable generic TUI; and
- per-provider/AccountProfile/ExecutionTarget context, quota and bounded activity inbox projections whose
  independent source, coverage and freshness prevent false zero or cross-profile leakage.

Those proofs must use the same canonical tree, one selected WorkSurface and one logical Attention Queue.
Full remote GUI, headless status and companion clients have distinct authenticated sync/input/action
contracts and must be exercised independently. A PR may report this document's v0.1 gate green while the
successor product completion report remains correctly rejected; the two results must never be collapsed.

## Explicitly outside this functional gate

- Surviving a **daemon** crash with a live PTY. v0.1.0 preserves live processes across
  UI replacement; after daemon loss it reports `Orphaned`/`Lost` honestly and never
  fabricates `Reconnected`.
- A published Developer ID/notarized production tag and clean-machine installation.
  The workflow and local ad-hoc equivalent are implemented; publishing credentials and
  release-channel operation are distribution work.
- Linux archive/update distribution and reviewed Linux GPU baselines.
- Recorded VoiceOver/Orca and current input-method runs on every release platform. The
  application-owned keyboard, AccessKit, contrast, zoom, reduced-motion and IME
  contracts are automated, and the manual checklist is reproducible.
- tmux, Windows, SSH, collaboration, containers, plugins and marketplace support.

Those exclusions match issue #21's scope clarification. They remain visible work, but
none is silently promoted into the functional MVP or used to weaken its completed
acceptance criteria. They describe only the frozen v0.1.0 release gate; later accepted control-plane
requirements for remote ExecutionTargets, full remote GUI and companions supersede the relevant product
scope without retroactively changing what this historical gate proved.
