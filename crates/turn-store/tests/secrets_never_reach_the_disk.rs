//! Proof, at the level of raw bytes, that credentials and opaque hook free text
//! do not end up in the file.
//!
//! Every other secret-hygiene test checks what comes back out of the store, which
//! only proves redaction happened somewhere in the read path. This one reads the
//! database file — and its write-ahead log, and every other file SQLite created —
//! and asserts the secret is simply not in there.

use std::collections::HashMap;
use std::path::Path;
use turn_core::attention::{AttentionEntry, EntryState};
use turn_core::ids::{AttentionId, WorkspaceId};
use turn_core::model::layout::{Direction, Pane, PaneKind};
use turn_core::model::node::NodeKind;
use turn_core::model::{
    ActivityPreview, Layout, PreviewSource, ProcessNode, Session, Template, Workspace,
};
use turn_core::state::AwaitingReason;
use turn_core::{Confidence, EventKind, EventSource, TurnEvent};
use turn_store::{Store, REDACTED};

const T0: i64 = 1_700_000_000_000;

/// Values that must never appear on disk, one per write path that touches an
/// environment or a captured payload.
const SECRETS: [&str; 8] = [
    "ghp_workspace_level_secret",
    "sk-ant-session-level-secret",
    "npm_pane_level_secret",
    "aws-node-level-secret",
    // Deliberately has no recognisable credential prefix and lives under an
    // innocent key. A scanner cannot save us here; the hook body must be absent.
    "free-text-hook-secret-with-no-recognisable-shape-8675309",
    // A shape the scanner has to recognise on its own: an attention summary has
    // no key beside it to give the game away.
    "sk-ant-api03-attention-summary-secret",
    "ghp_QUEUEDSUMMARYSECRET0123456789ABCDEF",
    "sk-ant-api03-preview-secret-ABCDEFGHIJKLMNOPQRSTUVWXYZ",
];

/// Tables `write_everything` puts a row in, because each of them can hold text an
/// agent or the user's environment supplied.
///
/// Checked against the schema below rather than trusted, because the gap this list
/// closes is exactly how `attention_entries` — the table holding the command line
/// of every permission an agent asked for — stayed outside this file for its whole
/// existence. A table added to a migration lands in neither list and fails
/// [`every_table_in_the_schema_is_accounted_for`] until someone decides which side
/// it belongs on.
const TABLES_THIS_TEST_WRITES: [&str; 8] = [
    "activity_previews",
    "attention_entries",
    "events",
    "process_nodes",
    "session_layouts",
    "sessions",
    "templates",
    "workspaces",
];

/// Tables holding nothing an agent or an environment ever supplied, so there is
/// nothing here for a secret to hide in.
///
/// `settings` and `tree_ui_state` are UI preferences. Checkout/lease tables hold
/// filesystem identity, typed ids, states and timestamps. Pane bindings are ids.
/// Workspace audit events are restricted to structured lease/tree facts. The
/// `events` table stores typed facts and provenance, never raw hook callbacks.
const TABLES_WITH_NOTHING_TO_LEAK: [&str; 7] = [
    "checkout_write_fences",
    "pane_node_bindings",
    "settings",
    "tree_ui_state",
    "workspace_audit_events",
    "workspace_checkouts",
    "workspace_write_leases",
];

/// The tables the schema actually declares, read out of the migrations so a new
/// one cannot be missed by anybody's memory.
fn tables_in_the_schema() -> Vec<String> {
    turn_store::migrations::MIGRATIONS
        .iter()
        .flat_map(|migration| migration.statements.split("CREATE TABLE ").skip(1))
        .filter_map(|tail| tail.split_whitespace().next())
        .map(|name| name.trim_matches('(').to_string())
        .collect()
}

fn write_everything(store: &Store) -> (WorkspaceId, Session) {
    let root = store
        .path()
        .unwrap()
        .parent()
        .unwrap()
        .join("workspace-root");
    std::fs::create_dir_all(&root).unwrap();
    let mut workspace = Workspace::new("turn", root.to_string_lossy(), T0);
    workspace.env = vec![
        ("PATH".into(), "/usr/bin".into()),
        ("GITHUB_TOKEN".into(), SECRETS[0].into()),
    ];
    store.workspaces().save(&workspace).unwrap();

    // A pane whose environment carries a token: the layout is stored as one JSON
    // document, which is the easiest place for a secret to hide.
    let mut agent_pane = Pane::new(PaneKind::Agent).with_command("claude");
    agent_pane.env = vec![("NPM_TOKEN".into(), SECRETS[2].into())];
    let mut layout = Layout::single(agent_pane);
    let first = layout.panes()[0].id.clone();
    layout.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));

    let mut session = Session::new(workspace.id.clone(), "Fix bugs", "/repos/turn", layout, T0);
    session.env = vec![("ANTHROPIC_API_KEY".into(), SECRETS[1].into())];

    let mut node = ProcessNode::agent(session.id.clone(), "claude", "/repos/turn", T0);
    let mut highlights = HashMap::new();
    highlights.insert("AWS_SESSION_TOKEN".to_string(), SECRETS[3].to_string());
    highlights.insert("NODE_ENV".to_string(), "development".to_string());
    node.env_highlights = highlights;
    node.activity_preview = Some(ActivityPreview {
        node_id: node.id.clone(),
        raw_source_sequence: Some(1),
        normalized_text: format!("Waiting with {}", SECRETS[7]),
        source: PreviewSource::SemanticEvent,
        confidence: Confidence::Explicit,
        stable: true,
        contains_sensitive_data: false,
        redacted: false,
        updated_ms: T0 + 1,
    });
    session.tree.insert(node);

    store.sessions().save(&session).unwrap();

    // A template captured from the same shape, with its own environment.
    let mut template = Template::from_layout("Captured", &session.layout, T0);
    template.env = vec![("CI_JOB_TOKEN".into(), SECRETS[0].into())];
    store.templates().save(&template).unwrap();

    // A raw Claude hook payload with arbitrary free text under an innocent key.
    // It is intentionally not recognisable by the redactor: only the durable
    // boundary (drop the callback, keep the typed fact) can make this safe.
    let event = TurnEvent::new(
        session.id.clone(),
        EventKind::AgentTurnStarted {
            prompt_excerpt: Some("fix the failing test".into()),
        },
        EventSource::Hook {
            tool: "claude-code".into(),
            event_name: "UserPromptSubmit".into(),
        },
        Confidence::Explicit,
        T0 + 10,
    )
    .with_raw(format!(
        r#"{{"cwd":"/repos/turn","diagnostic_note":"{}","prompt":"fix the failing test"}}"#,
        SECRETS[4]
    ));
    store.events().append(&event).unwrap();

    // A blocked agent, waiting for the user. Its summary is the command line the
    // model wrote, which is why it belongs here: the permission a user is asked to
    // approve is routinely `gh auth login --with-token …`.
    let demand = AttentionEntry {
        id: AttentionId::new(),
        session_id: session.id.clone(),
        node_id: None,
        parent_node_id: None,
        subject_external_id: None,
        reason: AwaitingReason::Permission,
        summary: Some(format!(
            "Run `curl -H 'Authorization: Bearer {}'`",
            SECRETS[5]
        )),
        confidence: Confidence::Explicit,
        created_ms: T0 + 20,
        updated_ms: T0 + 20,
        state: EntryState::Pending,
        priority_boost: 0,
        survives_owner_exit: false,
        demand_kind: Default::default(),
    };
    store.attention().upsert(&demand).unwrap();

    // And the other way in: the whole queue written at a checkpoint.
    let mut queue = turn_core::attention::AttentionQueue::new();
    queue.upsert(AttentionEntry {
        id: AttentionId::new(),
        session_id: session.id.clone(),
        node_id: None,
        parent_node_id: None,
        subject_external_id: None,
        reason: AwaitingReason::Question,
        summary: Some(format!("Should I push with {}?", SECRETS[6])),
        confidence: Confidence::Explicit,
        created_ms: T0 + 30,
        updated_ms: T0 + 30,
        state: EntryState::Pending,
        priority_boost: 0,
        survives_owner_exit: false,
        demand_kind: Default::default(),
    });
    queue.upsert(demand);
    store.attention().replace_all(&queue).unwrap();

    (workspace.id, session)
}

/// Reads every file SQLite left in the directory: the database, the write-ahead
/// log and the shared-memory index. A secret parked in the WAL is still a secret
/// on the user's disk.
fn all_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            out.push((name, std::fs::read(entry.path()).unwrap()));
        }
    }
    assert!(!out.is_empty(), "the store wrote no files at all");
    out
}

fn assert_absent(dir: &Path, moment: &str) {
    for (name, bytes) in all_bytes(dir) {
        for secret in SECRETS {
            assert!(
                !contains(&bytes, secret.as_bytes()),
                "{secret} was found in {name} {moment}"
            );
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn no_secret_value_is_present_anywhere_in_the_files_on_disk() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path()).unwrap();
    let (_workspace, session) = write_everything(&store);

    // Before any checkpoint: the rows may still live in the write-ahead log.
    assert_absent(temp.path(), "while the write-ahead log is still hot");

    // And after the pages have been folded into the main file.
    store.compact().unwrap();
    assert_absent(temp.path(), "after a checkpoint and vacuum");

    // The control: the harmless values did land, so the test is looking in the
    // right place and is not passing because nothing was written.
    let bytes = all_bytes(temp.path());
    let found_marker = bytes
        .iter()
        .any(|(_, bytes)| contains(bytes, b"fix the failing test"));
    assert!(
        found_marker,
        "the typed event fact never reached the file, so the absence of callback text proves nothing"
    );

    let sqlite = rusqlite::Connection::open(temp.path().join(turn_store::DATABASE_FILE)).unwrap();
    let raw: Option<String> = sqlite
        .query_row(
            "SELECT raw FROM events WHERE source_json LIKE '{\"hook\":%' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, None, "a hook callback must not occupy the raw column");

    let keys_kept = bytes
        .iter()
        .any(|(_, bytes)| contains(bytes, b"GITHUB_TOKEN"));
    assert!(
        keys_kept,
        "the variable names are supposed to survive; only the values are dropped"
    );

    // And what the store hands back names the variable and marks it redacted.
    let restored = store.sessions().get(&session.id).unwrap().unwrap();
    assert_eq!(restored.env[0].0, "ANTHROPIC_API_KEY");
    assert_eq!(restored.env[0].1, REDACTED);
}

#[test]
fn upgrading_physically_removes_historical_hook_free_text_from_sqlite_and_its_wal() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(turn_store::DATABASE_FILE);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    turn_store::migrations::apply_to(&conn, 1).unwrap();
    conn.execute(
        "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
         attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
         VALUES ('ws_old', 'legacy', '/repo', '[]', '[]', '{}', 1, 1, 0, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions \
             (id, workspace_id, name, cwd, env_json, attention_json, status, \
              restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
              last_activity_ms, tmux) \
         VALUES ('sess_old', 'ws_old', 'work', '/repo', '[]', '{}', 'active', \
                 'live', '[]', 0, 0, 0, 1, 1, 0)",
        [],
    )
    .unwrap();
    let secret = "historical-hook-free-text-with-no-secret-shape-8675309";
    conn.execute(
        "INSERT INTO events \
             (id, timestamp_ms, session_id, kind_slug, kind_json, agent_json, \
              confidence, source_json, severity, dedup_key, raw) \
         VALUES ('evt_hook', 1, 'sess_old', 'agent.idle', '{}', '{}', 'explicit', \
                 '{\"hook\":{\"tool\":\"claude-code\",\"event_name\":\"Stop\"}}', \
                 'debug', 'hook', ?1)",
        [secret],
    )
    .unwrap();
    turn_store::migrations::apply_to(&conn, 4).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    assert!(
        all_bytes(temp.path())
            .iter()
            .any(|(_, bytes)| contains(bytes, secret.as_bytes())),
        "the v4 fixture never contained the historical callback"
    );
    drop(conn);

    let store = Store::open_at(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), turn_store::LATEST_VERSION);
    for (name, bytes) in all_bytes(temp.path()) {
        assert!(
            !contains(&bytes, secret.as_bytes()),
            "the historical callback survived the security migration in {name}"
        );
    }

    let sqlite = rusqlite::Connection::open(&path).unwrap();
    let raw: Option<String> = sqlite
        .query_row("SELECT raw FROM events WHERE id = 'evt_hook'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(raw, None);
    let pending: bool = sqlite
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM settings \
             WHERE key = 'security.hook_raw_purge_pending')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !pending,
        "a successful physical purge must clear its retry marker"
    );
}

/// The control for the control: absence of a secret from a table proves nothing
/// unless the table has rows in it.
///
/// `attention_entries` was empty for every run of this file until now, which is
/// how a summary written straight to disk unredacted survived a reviewer, a test
/// suite and a byte-level grep at the same time.
#[test]
fn every_table_this_test_claims_to_cover_actually_has_rows_in_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path()).unwrap();
    let (workspace, session) = write_everything(&store);

    // One assertion per table in TABLES_THIS_TEST_WRITES, in the same order.
    let node_id = session.tree.iter().next().unwrap().id.clone();
    assert!(
        !store
            .hierarchy()
            .preview_history(&node_id, 20)
            .unwrap()
            .is_empty(),
        "activity_previews"
    );
    assert!(store.attention().count().unwrap() > 0, "attention_entries");
    assert!(store.events().count().unwrap() > 0, "events");
    assert!(
        store.nodes().count_for_session(&session.id).unwrap() > 0,
        "process_nodes"
    );
    assert!(
        store.sessions().layout(&session.id).unwrap().is_some(),
        "session_layouts"
    );
    assert!(store.sessions().count().unwrap() > 0, "sessions");
    assert!(store.templates().count().unwrap() > 0, "templates");
    assert!(
        store.workspaces().get(&workspace).unwrap().is_some(),
        "workspaces"
    );
}

/// A table nobody classified is a table nobody checked for secrets.
#[test]
fn every_table_in_the_schema_is_accounted_for() {
    let mut declared: Vec<String> = TABLES_THIS_TEST_WRITES
        .iter()
        .chain(TABLES_WITH_NOTHING_TO_LEAK.iter())
        .map(|name| name.to_string())
        .collect();
    declared.sort();

    let mut found = tables_in_the_schema();
    found.sort();

    assert_eq!(
        found, declared,
        "a table was added to the schema without deciding whether it can hold \
         agent-supplied text; if it can, write a secret into it in write_everything, \
         and if it cannot, say so in TABLES_WITH_NOTHING_TO_LEAK"
    );
}

#[test]
fn a_secret_survives_nowhere_even_after_the_daemon_restarts_and_prunes() {
    let temp = tempfile::tempdir().unwrap();
    let session_id = {
        let store = Store::open_in(temp.path()).unwrap();
        let (_, session) = write_everything(&store);
        session.id
    };

    let store = Store::open_in(temp.path()).unwrap();
    // Touch every read and write path a restarted daemon uses.
    let restored = store
        .sessions()
        .load_for_restore(&session_id)
        .unwrap()
        .expect("the session came back");
    store.sessions().save(&restored).unwrap();
    let mut node = restored.tree.iter().next().cloned().expect("an agent node");
    node.command = "claude --resume".into();
    store.nodes().upsert(&node).unwrap();
    store
        .events()
        .prune(&turn_store::Retention::default(), T0 + 1_000)
        .unwrap();
    store.compact().unwrap();

    assert_absent(temp.path(), "after a restart, a re-save and a prune");
}

#[test]
fn a_process_environment_is_not_persisted_wholesale_even_when_it_looks_innocent() {
    // The rule is not "redact the obvious names"; it is that whole environments
    // are never written. A node carries only the highlights an adapter chose.
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path()).unwrap();
    let root = temp.path().join("workspace-root");
    std::fs::create_dir(&root).unwrap();
    let workspace = Workspace::new("turn", root.to_string_lossy(), T0);
    store.workspaces().save(&workspace).unwrap();
    let session = Session::new(
        workspace.id.clone(),
        "plain",
        "/repos/turn",
        Layout::single(Pane::new(PaneKind::Shell)),
        T0,
    );
    store.sessions().save(&session).unwrap();

    let mut node = ProcessNode::process(session.id.clone(), NodeKind::Shell, "zsh", "/", T0);
    node.env_highlights
        .insert("VIRTUAL_ENV".into(), "/repos/turn/.venv".into());
    store.nodes().upsert(&node).unwrap();

    let back = store.nodes().get(&node.id).unwrap().unwrap();
    assert_eq!(
        back.env_highlights.len(),
        1,
        "only the highlights are stored, and nothing is added on the way in or out"
    );
    assert_eq!(back.env_highlights["VIRTUAL_ENV"], "/repos/turn/.venv");
}

/// The store's contents are only as private as its file mode.
///
/// It records every command line an agent proposed, the directories it ran in and
/// hook payloads. The platform default of 0644 in a 0755 directory makes all of
/// that readable by every other account on the machine — a poor default for a
/// single-user desktop app, and the reason `ensure_dir` and `open_at` narrow both.
#[cfg(unix)]
#[test]
fn the_database_and_its_sidecars_are_readable_only_by_their_owner() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("turn-data");
    let store = turn_store::Store::open_in(&root).unwrap();
    // Force a write so the write-ahead log actually exists.
    store
        .workspaces()
        .save(&turn_core::model::Workspace::new("w", "/tmp", 0))
        .unwrap();
    drop(store);

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

    assert_eq!(
        mode(&root),
        0o700,
        "the data directory must not be world-readable"
    );

    let db = root.join(turn_store::DATABASE_FILE);
    assert_eq!(mode(&db), 0o600, "the database must not be world-readable");

    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from({
            let mut s = db.clone().into_os_string();
            s.push(suffix);
            s
        });
        if sidecar.exists() {
            assert_eq!(
                mode(&sidecar),
                0o600,
                "{} holds uncheckpointed rows and must be just as private",
                sidecar.display()
            );
        }
    }
}
