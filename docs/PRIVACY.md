# Local data and privacy

Turn has no telemetry transport. It does not send terminal contents, process output, environment values,
credentials, local records, or usage measurements anywhere. Every privacy report and export states
`telemetry_enabled: false` and `telemetry_endpoints: 0`, so this is an inspectable result rather than an
assumption based on a missing settings screen.

## What Turn stores

The authoritative inventory has two closed catalogues:

- SQLite rows: Workspaces, Sessions, Layouts, Process/Agent nodes, Events, Attention, Activity Previews,
  Pane bindings, Settings layers, Templates, tree presentation state, checkout identities/leases/fences and
  workspace audit events. A schema migration adding a table without adding it to the inventory makes export
  fail rather than silently omit the table.
- Private files: SQLite and its WAL/SHM/journal sidecars, the owner-only daemon log, injected Agent scratch
  configuration, bounded terminal journals/checkpoints and runtime control files. Any future file under the
  Turn data directory is reported as `unclassified_local_file` until it receives a semantic category.

Daemon-managed Git worktree roots are listed with their aggregate size, but their payload is never exported
or deleted. They contain user work. Likewise, file payloads which may contain terminal output, log text or
injected configuration are represented by metadata only. Their `content` explains the omission.

Every exported datum contains `origin`, `data_type`, `timestamp_ms` (explicitly `null` when the backing
format has no timestamp), `bytes` and redacted `content`. Known credentials are scrubbed again at the export
boundary. Unknown or secret Settings values are replaced by `<redacted>`.

## Inspect and export without opening a window

These commands authenticate to the already-running daemon and never start a companion or GUI:

```sh
turn --privacy-report
turn --privacy-report session:sess_ab12
turn --privacy-export /absolute/path/turn-export.json --scope workspace:ws_ab12
turn --privacy-compact
```

Scopes are `installation`, `workspace:<id>`, `session:<id>` and
`agent:<session-id>:<node-id>`. Report output includes per-category and total item/byte counts plus the policy
currently in force. Export uses owner-only, create-new file semantics: it never follows an existing
destination symlink and never overwrites a file.

## Delete

Selective deletion is authenticated through the live daemon:

```sh
turn --privacy-delete session:sess_ab12
turn --privacy-delete workspace:ws_ab12 --kill
turn --privacy-delete agent:sess_ab12:proc_ab12
```

The default disposition is a polite termination; `--kill` requests a hard stop. Keeping processes while
deleting their identity is refused. Session and Workspace deletion removes SQLite records, Settings owners,
scratch configuration, journals, checkpoints, previews and bindings. Agent deletion removes its subtree,
Attention/Event references, previews, bindings, scratch and history. The response reports records/files and
bytes removed, compaction, and any process identity that escaped Turn's control.

Installation deletion is offline because a process must not unlink its own open database:

```sh
turnd --delete-installation-data
# Add --data-dir and --socket when using non-default locations.
```

The command acquires the same canonical data-directory lock as normal daemon startup. It exits with code 3
without deleting anything if a daemon is live. Once exclusive, it removes SQLite and every sidecar, logs,
scratch, terminal history, tokens, stale sockets and unclassified future entries without following symlinks.
It deliberately retains the stable `.turnd.lock` inode and `worktrees/` user checkout roots.

## Retention controls

The Settings hierarchy exposes these controls under **Records**:

| Key | Default | Scope |
| --- | ---: | --- |
| `records.event_retention_days` | 30 days | Global |
| `records.event_limit` | 50,000 | Global |
| `records.event_session_floor` | 50 | Global |
| `records.preview_retention_days` | 30 days | Global |
| `records.previews_per_agent` | 20 | Global |
| `records.preview_limit` | 2,000 | Global |
| `records.terminal_history` | on | Global, Workspace, Template, Session |
| `records.terminal_journal_mib` | 8 MiB per Pane | Global |
| `records.terminal_checkpoint_mib` | 4 MiB per Pane | Global |
| `records.daemon_log_mib` | 4 MiB | Global |

Event/Preview retention and log bounds are enforced immediately after a related setting write and at least
once per minute while the daemon runs. Turning terminal history off stops journalling and removes existing
history without stopping the live Session; turning it back on applies to newly launched Panes. Journal and
checkpoint size changes apply when a Pane next starts. `privacy-compact` applies retention immediately,
truncates the WAL, vacuums SQLite and reports before/after storage.

## Reproducible acceptance

```sh
make privacy-acceptance
```

The target covers the closed SQLite catalogue, redacted export, credential-free log/export output,
create-new/symlink safety, scoped deletion, dynamic retention/history controls, authenticated protocol/CLI
shapes, offline physical purge, preservation of checkout work and refusal while a daemon owns the lock.
