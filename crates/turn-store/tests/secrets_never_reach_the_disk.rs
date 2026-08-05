//! Proof, at the level of raw bytes, that credentials and opaque hook free text
//! do not end up in the file.
//!
//! Every other secret-hygiene test checks what comes back out of the store, which
//! only proves redaction happened somewhere in the read path. This one reads the
//! database file — and its write-ahead log, and every other file SQLite created —
//! and asserts the secret is simply not in there.

use std::collections::HashMap;
use std::path::Path;
use turn_core::attention::{AttentionEntry, AttentionPolicy, EntryState};
use turn_core::event::{AgentRef, Risk};
use turn_core::ids::{AttentionId, CheckoutId, WorkspaceId};
use turn_core::model::layout::{Direction, Pane, PaneKind};
use turn_core::model::node::{NodeKind, PendingPermission};
use turn_core::model::{
    ActivityPreview, AgentName, Layout, PreviewSource, ProcessNode, Session, SessionMode, Template,
    Workspace, WorkspaceCheckout,
};
use turn_core::state::{AwaitingReason, Lifecycle, Turn};
use turn_core::{Confidence, EventKind, EventSource, TurnEvent};
use turn_store::{Store, REDACTED};

const T0: i64 = 1_700_000_000_000;

/// Values that must never appear on disk, one per write path that touches an
/// environment or a captured payload.
/// A correctly-shaped classic GitHub token. The same value is deliberately planted
/// in every durable free-text field so a single missed field fails the byte scan.
const DURABLE_SECRET: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";

const SECRETS: [&str; 9] = [
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
    DURABLE_SECRET,
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
const TABLES_THIS_TEST_WRITES: [&str; 10] = [
    "activity_previews",
    "attention_entries",
    "events",
    "process_nodes",
    "session_layouts",
    "sessions",
    "settings",
    "templates",
    "workspaces",
    "workspace_checkouts",
];

/// Tables holding nothing an agent or an environment ever supplied, so there is
/// nothing here for a secret to hide in.
///
/// `tree_ui_state` holds structural UI identity. Checkout/lease tables hold
/// filesystem identity, typed ids, states and timestamps; free checkout labels are
/// nevertheless planted above. Pane bindings are ids. Workspace audit events are
/// restricted to structured lease/tree facts. The `events` table stores typed facts
/// and provenance, never raw hook callbacks.
const TABLES_WITH_NOTHING_TO_LEAK: [&str; 5] = [
    "checkout_write_fences",
    "pane_node_bindings",
    "tree_ui_state",
    "workspace_audit_events",
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

fn tainted(label: &str) -> String {
    format!("{label} {DURABLE_SECRET}")
}

fn write_everything(store: &Store) -> (WorkspaceId, Session) {
    let root = store
        .path()
        .unwrap()
        .parent()
        .unwrap()
        .join("workspace-root");
    std::fs::create_dir_all(&root).unwrap();
    let mut workspace = Workspace::new(tainted("turn"), root.to_string_lossy(), T0);
    workspace.git_remote = Some(tainted("https://github.com/example/turn.git?credential="));
    workspace.env = vec![
        ("PATH".into(), "/usr/bin".into()),
        ("GITHUB_TOKEN".into(), SECRETS[0].into()),
        ("INNOCENT_WORKSPACE_VALUE".into(), tainted("workspace env")),
    ];
    workspace.default_shell = Some(tainted("zsh"));
    workspace.default_agent = Some(tainted("claude"));
    workspace.init_commands = vec![tainted("mise install")];
    workspace.attention.custom_command = Some(tainted("notify-send"));
    workspace.colour = Some(tainted("violet"));
    workspace.icon = Some(tainted("terminal"));
    store.workspaces().save(&workspace).unwrap();

    // A pane whose environment carries a token: the layout is stored as one JSON
    // document, which is the easiest place for a secret to hide.
    let mut agent_pane = Pane::new(PaneKind::Agent)
        .with_title(tainted("Claude Code"))
        .with_command(tainted("claude"))
        .with_cwd(tainted("pane cwd"));
    agent_pane.args = vec![tainted("--resume")];
    agent_pane.env = vec![
        ("NPM_TOKEN".into(), SECRETS[2].into()),
        ("PANE_INNOCENT".into(), tainted("pane env")),
    ];
    let mut layout = Layout::single(agent_pane);
    let first = layout.panes()[0].id.clone();
    layout.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));

    let mut session = Session::new(
        workspace.id.clone(),
        tainted("Fix bugs"),
        tainted("/repos/turn"),
        layout,
        T0,
    );
    session.note = Some(tainted("Investigate the failing test"));
    session.env = vec![
        ("ANTHROPIC_API_KEY".into(), SECRETS[1].into()),
        ("SESSION_INNOCENT".into(), tainted("session env")),
    ];
    session.attention.custom_command = Some(tainted("osascript"));
    session.tags = vec![tainted("urgent")];
    session.git_branch = Some(tainted("fix/redaction"));
    session.linked_ref = Some(tainted("https://github.com/example/turn/pull/1"));

    let mut node = ProcessNode::agent(
        session.id.clone(),
        tainted("claude"),
        tainted("/repos/turn"),
        T0,
    );
    node.title = tainted("Claude Code");
    node.args = vec![tainted("--resume")];
    node.lifecycle = Lifecycle::Signaled {
        signal: tainted("SIGTERM"),
    };
    node.turn = Some(Turn::Failed {
        reason: tainted("adapter failure"),
    });
    let agent = node.agent.as_mut().expect("agent metadata");
    agent.agent = AgentRef {
        provider: Some(tainted("anthropic")),
        tool: Some(tainted("claude-code")),
        model: Some(tainted("claude-sonnet")),
        external_id: Some(tainted("event-agent-external")),
    };
    agent.name = AgentName::declared(tainted("Reviewer"));
    agent.name.rename(tainted("Review specialist"));
    agent.external_id = Some(tainted("agent-external"));
    agent.agent_type = Some(tainted("code-reviewer"));
    agent.current_task = Some(tainted("Review current diff"));
    agent.last_message = Some(tainted("Found an issue"));
    agent.pending_permission = Some(PendingPermission {
        summary: tainted("Run release command"),
        command: Some(tainted("git push")),
        tool_name: Some(tainted("Bash")),
        risk: Risk::High,
        requested_ms: T0,
        cwd: Some(tainted("/repos/turn")),
    });
    agent.pending_question = Some(tainted("Should I continue?"));
    agent.permission_mode = Some(tainted("default"));
    agent.git_branch = Some(tainted("fix/redaction"));
    let mut highlights = HashMap::new();
    highlights.insert("AWS_SESSION_TOKEN".to_string(), SECRETS[3].to_string());
    highlights.insert("NODE_ENV".to_string(), "development".to_string());
    highlights.insert("NODE_LABEL".to_string(), tainted("node env"));
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
    let mut template = Template::from_layout(tainted("Captured"), &session.layout, T0);
    template.description = Some(tainted("Reusable coding layout"));
    template.icon = Some(tainted("template icon"));
    template.attention = Some(AttentionPolicy {
        custom_command: Some(tainted("template notification")),
        ..AttentionPolicy::default()
    });
    template.init_commands = vec![tainted("cargo fetch")];
    template.name_pattern = Some(tainted("Review {branch}"));
    template.hotkey = Some(tainted("cmd+shift+1"));
    template.env = vec![
        ("CI_JOB_TOKEN".into(), SECRETS[0].into()),
        ("TEMPLATE_INNOCENT".into(), tainted("template env")),
    ];
    store.templates().save(&template).unwrap();

    // Generic settings are another durable default/configuration route. The key
    // is structural and remains stable; its arbitrary JSON string value is not.
    store
        .settings()
        .set("test.durable-secret", &tainted("global default"), T0)
        .unwrap();

    // Isolated checkout labels are Workspace metadata too. Paths are operational
    // fencing identities and stay clean; branch/resource labels cross the same
    // redaction boundary as the Workspace itself.
    let worktree_root = root.parent().unwrap().join("isolated-worktree");
    std::fs::create_dir_all(&worktree_root).unwrap();
    let worktree_root = std::fs::canonicalize(worktree_root).unwrap();
    let checkout_id = CheckoutId::new();
    let branch = tainted("turn/durable-redaction");
    let checkout = WorkspaceCheckout {
        id: checkout_id.clone(),
        workspace_id: workspace.id.clone(),
        path: worktree_root.to_string_lossy().into_owned(),
        canonical_path: worktree_root.to_string_lossy().into_owned(),
        branch: Some(branch.clone()),
        primary: false,
        shared_resources: vec![tainted("docker")],
        created_ms: T0,
    };
    let mut isolated = Session::new(
        workspace.id.clone(),
        "Isolated",
        worktree_root.to_string_lossy(),
        Layout::single(Pane::new(PaneKind::Shell)),
        T0,
    );
    isolated.mode = SessionMode::IsolatedWorktree;
    isolated.checkout_id = checkout_id;
    isolated.worktree_path = Some(checkout.path.clone());
    isolated.git_branch = Some(branch);
    store
        .hierarchy()
        .create_worktree_session(&isolated, &checkout)
        .unwrap();

    // A raw Claude hook payload with arbitrary free text under an innocent key.
    // It is intentionally not recognisable by the redactor: only the durable
    // boundary (drop the callback, keep the typed fact) can make this safe.
    let mut event = TurnEvent::new(
        session.id.clone(),
        EventKind::AgentTurnStarted {
            prompt_excerpt: Some(tainted("fix the failing test")),
        },
        EventSource::Hook {
            tool: tainted("claude-code"),
            event_name: tainted("UserPromptSubmit"),
        },
        Confidence::Explicit,
        T0 + 10,
    )
    .with_agent(AgentRef {
        provider: Some(tainted("anthropic")),
        tool: Some(tainted("claude-code")),
        model: Some(tainted("claude-sonnet")),
        external_id: Some(tainted("event external")),
    })
    .with_raw(format!(
        r#"{{"cwd":"/repos/turn","diagnostic_note":"{}","prompt":"fix the failing test"}}"#,
        SECRETS[4]
    ));
    event.dedup_key = tainted("hook-submit");
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
        summary: Some(tainted(&format!(
            "Run `curl -H 'Authorization: Bearer {}'`",
            SECRETS[5]
        ))),
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
        subject_external_id: Some(tainted("queued-worker")),
        reason: AwaitingReason::Question,
        summary: Some(tainted(&format!("Should I push with {}?", SECRETS[6]))),
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

fn assert_scrubbed(label: &str, value: &str) {
    assert!(
        !value.contains(DURABLE_SECRET),
        "{label} returned the credential: {value}"
    );
    assert!(
        value.contains(REDACTED),
        "{label} was not visibly redacted: {value}"
    );
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

    // What the store hands back is the safe durable projection too. Assert every
    // free-text family, rather than relying only on the byte scan to tell us which
    // route lost the credential.
    let restored_workspace = store
        .workspaces()
        .get(&session.workspace_id)
        .unwrap()
        .expect("workspace");
    assert_scrubbed("workspace.name", &restored_workspace.name);
    assert_scrubbed(
        "workspace.git_remote",
        restored_workspace.git_remote.as_deref().unwrap(),
    );
    assert_scrubbed(
        "workspace.default_shell",
        restored_workspace.default_shell.as_deref().unwrap(),
    );
    assert_scrubbed(
        "workspace.default_agent",
        restored_workspace.default_agent.as_deref().unwrap(),
    );
    assert_scrubbed(
        "workspace.init_commands",
        &restored_workspace.init_commands[0],
    );
    assert_scrubbed(
        "workspace.attention.custom_command",
        restored_workspace
            .attention
            .custom_command
            .as_deref()
            .unwrap(),
    );
    assert_scrubbed(
        "workspace.colour",
        restored_workspace.colour.as_deref().unwrap(),
    );
    assert_scrubbed(
        "workspace.icon",
        restored_workspace.icon.as_deref().unwrap(),
    );
    assert_eq!(restored_workspace.env[1].1, REDACTED);
    assert_scrubbed("workspace.env", &restored_workspace.env[2].1);

    let restored = store.sessions().get(&session.id).unwrap().unwrap();
    assert_eq!(restored.id, session.id, "redaction must not rewrite IDs");
    assert_eq!(
        restored.layout.panes()[0].id,
        session.layout.panes()[0].id,
        "redaction must not rewrite Pane IDs"
    );
    assert_eq!(restored.env[0].0, "ANTHROPIC_API_KEY");
    assert_eq!(restored.env[0].1, REDACTED);
    assert_scrubbed("session.name", &restored.name);
    assert_scrubbed("session.note", restored.note.as_deref().unwrap());
    assert_scrubbed("session.cwd", &restored.cwd);
    assert_eq!(restored.worktree_path, None);
    assert_scrubbed("session.env", &restored.env[1].1);
    assert_scrubbed(
        "session.attention.custom_command",
        restored.attention.custom_command.as_deref().unwrap(),
    );
    assert_scrubbed("session.tags", &restored.tags[0]);
    assert_scrubbed(
        "session.git_branch",
        restored.git_branch.as_deref().unwrap(),
    );
    assert_scrubbed(
        "session.linked_ref",
        restored.linked_ref.as_deref().unwrap(),
    );

    let pane = restored.layout.panes()[0];
    assert_scrubbed("pane.title", pane.title.as_deref().unwrap());
    assert_scrubbed("pane.command", pane.command.as_deref().unwrap());
    assert_scrubbed("pane.args", &pane.args[0]);
    assert_scrubbed("pane.cwd", pane.cwd.as_deref().unwrap());
    assert_eq!(pane.env[0].1, REDACTED);
    assert_scrubbed("pane.env", &pane.env[1].1);

    let node = restored.tree.iter().next().expect("process node");
    assert_eq!(
        node.id,
        session.tree.iter().next().unwrap().id,
        "redaction must not rewrite Node IDs"
    );
    assert_scrubbed("node.title", &node.title);
    assert_scrubbed("node.command", &node.command);
    assert_scrubbed("node.args", &node.args[0]);
    assert_scrubbed("node.cwd", &node.cwd);
    match &node.lifecycle {
        Lifecycle::Signaled { signal } => assert_scrubbed("node.lifecycle.signal", signal),
        other => panic!("unexpected lifecycle {other:?}"),
    }
    match node.turn.as_ref().unwrap() {
        Turn::Failed { reason } => assert_scrubbed("node.turn.reason", reason),
        other => panic!("unexpected turn {other:?}"),
    }
    assert_eq!(node.env_highlights["AWS_SESSION_TOKEN"], REDACTED);
    assert_scrubbed("node.env_highlights", &node.env_highlights["NODE_LABEL"]);
    assert_scrubbed(
        "node.preview",
        &node.activity_preview.as_ref().unwrap().normalized_text,
    );

    let agent = node.agent.as_ref().expect("agent metadata");
    for (label, value) in [
        ("agent.provider", agent.agent.provider.as_deref().unwrap()),
        ("agent.tool", agent.agent.tool.as_deref().unwrap()),
        ("agent.model", agent.agent.model.as_deref().unwrap()),
        (
            "agent.ref.external_id",
            agent.agent.external_id.as_deref().unwrap(),
        ),
        (
            "agent.name.declared",
            agent.name.declared_name.as_deref().unwrap(),
        ),
        ("agent.name.display", agent.name.display_name.as_str()),
        ("agent.external_id", agent.external_id.as_deref().unwrap()),
        ("agent.agent_type", agent.agent_type.as_deref().unwrap()),
        ("agent.current_task", agent.current_task.as_deref().unwrap()),
        ("agent.last_message", agent.last_message.as_deref().unwrap()),
        (
            "agent.pending_question",
            agent.pending_question.as_deref().unwrap(),
        ),
        (
            "agent.permission_mode",
            agent.permission_mode.as_deref().unwrap(),
        ),
        ("agent.git_branch", agent.git_branch.as_deref().unwrap()),
    ] {
        assert_scrubbed(label, value);
    }
    let permission = agent.pending_permission.as_ref().unwrap();
    assert_scrubbed("permission.summary", &permission.summary);
    assert_scrubbed("permission.command", permission.command.as_deref().unwrap());
    assert_scrubbed(
        "permission.tool_name",
        permission.tool_name.as_deref().unwrap(),
    );
    assert_scrubbed("permission.cwd", permission.cwd.as_deref().unwrap());

    let template = store.templates().list().unwrap().remove(0);
    assert_scrubbed("template.name", &template.name);
    assert_scrubbed(
        "template.description",
        template.description.as_deref().unwrap(),
    );
    assert_scrubbed("template.icon", template.icon.as_deref().unwrap());
    assert_scrubbed("template.init_commands", &template.init_commands[0]);
    assert_scrubbed(
        "template.name_pattern",
        template.name_pattern.as_deref().unwrap(),
    );
    assert_scrubbed("template.hotkey", template.hotkey.as_deref().unwrap());
    assert_eq!(template.env[0].1, REDACTED);
    assert_scrubbed("template.env", &template.env[1].1);
    assert_scrubbed(
        "template.attention.custom_command",
        template
            .attention
            .as_ref()
            .unwrap()
            .custom_command
            .as_deref()
            .unwrap(),
    );
    let template_pane = template.layout.panes()[0];
    assert_scrubbed(
        "template.layout.pane.command",
        template_pane.command.as_deref().unwrap(),
    );

    let checkout = store
        .hierarchy()
        .checkouts_for_workspace(&session.workspace_id)
        .unwrap()
        .into_iter()
        .find(|checkout| !checkout.primary)
        .expect("isolated checkout");
    assert_scrubbed("checkout.branch", checkout.branch.as_deref().unwrap());
    assert_scrubbed("checkout.shared_resources", &checkout.shared_resources[0]);

    let setting: String = store
        .settings()
        .get("test.durable-secret")
        .unwrap()
        .expect("setting");
    assert_scrubbed("settings.value", &setting);

    let event = store
        .events()
        .list_for_session(&session.id, 10)
        .unwrap()
        .remove(0);
    match &event.kind {
        EventKind::AgentTurnStarted {
            prompt_excerpt: Some(prompt),
        } => assert_scrubbed("event.kind", prompt),
        other => panic!("unexpected event kind {other:?}"),
    }
    for (label, value) in [
        (
            "event.agent.provider",
            event.agent.provider.as_deref().unwrap(),
        ),
        ("event.agent.tool", event.agent.tool.as_deref().unwrap()),
        ("event.agent.model", event.agent.model.as_deref().unwrap()),
        (
            "event.agent.external_id",
            event.agent.external_id.as_deref().unwrap(),
        ),
        ("event.dedup_key", event.dedup_key.as_str()),
    ] {
        assert_scrubbed(label, value);
    }
    match &event.source {
        EventSource::Hook { tool, event_name } => {
            assert_scrubbed("event.source.tool", tool);
            assert_scrubbed("event.source.event_name", event_name);
        }
        other => panic!("unexpected event source {other:?}"),
    }

    let entries = store.attention().list_for_session(&session.id).unwrap();
    assert!(!entries.is_empty());
    for entry in entries {
        assert_scrubbed("attention.summary", entry.summary.as_deref().unwrap());
        if let Some(external_id) = entry.subject_external_id.as_deref() {
            assert_scrubbed("attention.subject_external_id", external_id);
        }
    }
}

#[test]
fn a_credential_shaped_workspace_root_is_refused_before_sqlite_sees_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path()).unwrap();
    let root = temp.path().join(DURABLE_SECRET);
    std::fs::create_dir(&root).unwrap();
    let workspace = Workspace::new("unsafe structural path", root.to_string_lossy(), T0);

    let error = store
        .workspaces()
        .save(&workspace)
        .expect_err("rewriting a checkout identity would be unsafe");
    assert!(matches!(
        error,
        turn_store::StoreError::SecretInStructuralField {
            what: "workspace root",
            ..
        }
    ));
    assert_eq!(store.workspaces().count().unwrap(), 0);
    assert_absent(
        temp.path(),
        "after rejecting a credential-shaped structural path",
    );
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
    assert!(
        store
            .settings()
            .keys()
            .unwrap()
            .contains(&"test.durable-secret".to_string()),
        "settings"
    );
    assert!(store.templates().count().unwrap() > 0, "templates");
    assert!(
        store.workspaces().get(&workspace).unwrap().is_some(),
        "workspaces"
    );
    assert!(
        store
            .hierarchy()
            .checkouts_for_workspace(&workspace)
            .unwrap()
            .len()
            > 1,
        "workspace_checkouts"
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
    let mut layout = restored.layout.clone();
    let pane_id = layout.panes()[0].id.clone();
    layout.get_mut(&pane_id).unwrap().command = Some(tainted("direct layout save"));
    store
        .sessions()
        .save_layout(&session_id, &layout, T0 + 1)
        .unwrap();
    let mut node = restored.tree.iter().next().cloned().expect("an agent node");
    node.command = tainted("claude --resume");
    node.agent.as_mut().unwrap().pending_question = Some(tainted("direct node upsert"));
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
