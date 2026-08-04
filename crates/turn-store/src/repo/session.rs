//! Session persistence, including the layout tree and the process tree.
//!
//! A save is one transaction over three tables — the session row, its layout
//! document and its nodes — because a session whose layout survived but whose
//! nodes did not is not a session anybody can restore.

use crate::codec::{from_json, from_tag, json, tag};
use crate::error::{Result, StoreError};
use crate::redact::{redact_layout, redact_pairs};
use crate::repo::node::{build_tree, from_row as node_from_row, upsert_node};
use rusqlite::{params, Connection, Row};
use turn_core::ids::{CheckoutId, SessionId, TemplateId, WorkspaceId};
use turn_core::model::session::{RestoreState, SessionStatus};
use turn_core::model::{Layout, ProcessNode, Session, SessionMode, SessionTree};
use turn_core::state::Lifecycle;
use turn_core::AttentionPolicy;

const COLUMNS: &str = "s.id, s.workspace_id, s.name, s.note, s.cwd, s.mode, s.checkout_id, \
     s.worktree_path, s.read_only_enforced, s.env_json, \
     s.attention_json, s.template_id, s.status, s.restore_state, s.tags_json, s.git_branch, \
     s.linked_ref, s.favourite, s.pinned, s.sort_key, s.parent_session, s.created_ms, \
     s.last_activity_ms, s.tmux, l.layout_json";

const NODE_COLUMNS: &str = "id, session_id, seq, kind, title, command, args_json, cwd, pid, \
     ppid, lifecycle_json, turn_json, agent_json, external_id, parent, relation, pane_id, \
     declared_name, display_name, name_source, name_confidence, user_renamed, \
     relationship_kind, relationship_confidence, preview_visibility, activity_preview_json, \
     started_ms, ended_ms, exit_code, env_highlights_json, interaction_pending";

pub struct SessionRepo<'a> {
    conn: &'a Connection,
}

impl<'a> SessionRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Inserts or updates a session, its layout and its whole process tree.
    ///
    /// Nodes that are no longer in the tree are deleted, so a session that has
    /// dropped a pane does not accumulate ghosts. Environments — the session's
    /// own and every pane's — are redacted on the way in.
    pub fn save(&self, session: &Session) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        save_in(&tx, session)?;
        tx.commit()?;
        Ok(())
    }

    /// Inserts a session as part of a wider store transaction. This is the seam
    /// the lease arbiter uses so a Main-checkout Session and its write lease either
    /// both exist or neither does.
    pub(crate) fn save_in_transaction(conn: &Connection, session: &Session) -> Result<()> {
        save_in(conn, session)
    }

    /// Loads a session exactly as it was stored.
    pub fn get(&self, id: &SessionId) -> Result<Option<Session>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM sessions s \
             LEFT JOIN session_layouts l ON l.session_id = s.id WHERE s.id = ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let mut session = from_row(row)?;
        session.tree = self.tree(&session.id)?;
        Ok(Some(session))
    }

    /// Loads a session for a daemon that has just started.
    ///
    /// Every node stored as `Alive`, `Spawning` or `Reconnected` is downgraded to
    /// [`Lifecycle::Orphaned`], because a stored "alive" only ever meant "alive
    /// when we last wrote". It is not downgraded to `Lost`: deciding that a
    /// process is gone requires looking at the process table, which is the
    /// supervisor's job, not the store's. Nothing is relaunched and nothing is
    /// rewritten on disk — this is a projection at read time.
    pub fn load_for_restore(&self, id: &SessionId) -> Result<Option<Session>> {
        let Some(mut session) = self.get(id)? else {
            return Ok(None);
        };
        let nodes: Vec<ProcessNode> = session
            .tree
            .iter()
            .cloned()
            .map(|mut node| {
                if matches!(
                    node.lifecycle,
                    Lifecycle::Alive | Lifecycle::Spawning | Lifecycle::Reconnected
                ) {
                    node.lifecycle = Lifecycle::Orphaned;
                }
                node
            })
            .collect();
        session.tree = build_tree(nodes);
        Ok(Some(session))
    }

    /// A workspace's sessions, most recently active first.
    pub fn list_for_workspace(&self, workspace: &WorkspaceId) -> Result<Vec<Session>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM sessions s \
                 LEFT JOIN session_layouts l ON l.session_id = s.id \
                 WHERE s.workspace_id = ?1 ORDER BY s.last_activity_ms DESC"
            ),
            params![workspace.as_str()],
        )
    }

    /// Sessions in one status, most recently active first.
    pub fn list_with_status(&self, status: SessionStatus) -> Result<Vec<Session>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM sessions s \
                 LEFT JOIN session_layouts l ON l.session_id = s.id \
                 WHERE s.status = ?1 ORDER BY s.last_activity_ms DESC"
            ),
            params![tag("session status", &status)?],
        )
    }

    pub fn list_all(&self) -> Result<Vec<Session>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM sessions s \
                 LEFT JOIN session_layouts l ON l.session_id = s.id \
                 ORDER BY s.last_activity_ms DESC"
            ),
            params![],
        )
    }

    /// Stores a layout on its own.
    ///
    /// Splitting and resizing panes happens constantly and has nothing to do with
    /// the row the sidebar reads, so it gets its own write path.
    pub fn save_layout(&self, id: &SessionId, layout: &Layout, now_ms: i64) -> Result<()> {
        write_layout(self.conn, id, layout, now_ms)
    }

    pub fn layout(&self, id: &SessionId) -> Result<Option<Layout>> {
        let mut stmt = self
            .conn
            .prepare("SELECT layout_json FROM session_layouts WHERE session_id = ?1")?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_json(
                "layout",
                id.as_str(),
                &row.get::<_, String>(0)?,
            )?)),
            None => Ok(None),
        }
    }

    /// Records activity without rewriting the session.
    pub fn touch(&self, id: &SessionId, now_ms: i64) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE sessions SET last_activity_ms = ?2 WHERE id = ?1",
            params![id.as_str(), now_ms],
        )?;
        Ok(changed > 0)
    }

    /// Moves a session between active, paused and archived.
    pub fn set_status(&self, id: &SessionId, status: SessionStatus) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE sessions SET status = ?2 WHERE id = ?1",
            params![id.as_str(), tag("session status", &status)?],
        )?;
        Ok(changed > 0)
    }

    /// Records how much of a session came back after a restart.
    pub fn set_restore_state(&self, id: &SessionId, state: RestoreState) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE sessions SET restore_state = ?2 WHERE id = ?1",
            params![id.as_str(), tag("restore state", &state)?],
        )?;
        Ok(changed > 0)
    }

    /// Deletes a session with its layout, nodes, events and attention entries.
    pub fn delete(&self, id: &SessionId) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id.as_str()])?;
        Ok(changed > 0)
    }

    pub fn count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    fn tree(&self, id: &SessionId) -> Result<SessionTree> {
        let sql = format!(
            "SELECT {NODE_COLUMNS} FROM process_nodes WHERE session_id = ?1 ORDER BY seq ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        let mut nodes = Vec::new();
        while let Some(row) = rows.next()? {
            nodes.push(node_from_row(row)?);
        }
        Ok(build_tree(nodes))
    }

    fn query(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(args)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(from_row(row)?);
        }
        for session in out.iter_mut() {
            session.tree = self.tree(&session.id)?;
        }
        Ok(out)
    }
}

fn save_in(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, workspace_id, name, note, cwd, env_json, attention_json, \
             mode, checkout_id, worktree_path, read_only_enforced, template_id, status, \
             restore_state, tags_json, git_branch, linked_ref, favourite, pinned, sort_key, \
             parent_session, created_ms, last_activity_ms, tmux) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24) \
         ON CONFLICT(id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, \
             note = excluded.note, cwd = excluded.cwd, env_json = excluded.env_json, \
             mode = excluded.mode, checkout_id = excluded.checkout_id, \
             worktree_path = excluded.worktree_path, \
             read_only_enforced = excluded.read_only_enforced, \
             attention_json = excluded.attention_json, template_id = excluded.template_id, \
             status = excluded.status, restore_state = excluded.restore_state, \
             tags_json = excluded.tags_json, git_branch = excluded.git_branch, \
             linked_ref = excluded.linked_ref, favourite = excluded.favourite, \
             pinned = excluded.pinned, sort_key = excluded.sort_key, \
             parent_session = excluded.parent_session, created_ms = excluded.created_ms, \
             last_activity_ms = excluded.last_activity_ms, tmux = excluded.tmux",
        params![
            session.id.as_str(),
            session.workspace_id.as_str(),
            session.name,
            session.note,
            session.cwd,
            json("session env", &redact_pairs(&session.env))?,
            json("attention policy", &session.attention)?,
            tag("session mode", &session.mode)?,
            session.checkout_id.as_str(),
            session.worktree_path,
            session.read_only_enforced,
            session.template_id.as_ref().map(|t| t.as_str()),
            tag("session status", &session.status)?,
            tag("restore state", &session.restore_state)?,
            json("session tags", &session.tags)?,
            session.git_branch,
            session.linked_ref,
            session.favourite,
            session.pinned,
            session.sort_key,
            session.parent_session.as_ref().map(|s| s.as_str()),
            session.created_ms,
            session.last_activity_ms,
            session.tmux,
        ],
    )
    .map_err(|error| StoreError::from_write("session", session.workspace_id.as_str(), error))?;

    write_layout(conn, &session.id, &session.layout, session.last_activity_ms)?;

    // Replace the node set wholesale: the tree in memory is authoritative.
    let mut keep: Vec<String> = Vec::new();
    for (index, node) in session.tree.iter().enumerate() {
        upsert_node(conn, node, index as i64)?;
        keep.push(node.id.to_string());
    }
    prune_nodes(conn, &session.id, &keep)?;
    sync_layout_bindings(conn, session)?;
    Ok(())
}

fn write_layout(conn: &Connection, id: &SessionId, layout: &Layout, now_ms: i64) -> Result<()> {
    let safe = redact_layout(layout);
    conn.execute(
        "INSERT INTO session_layouts (session_id, layout_json, updated_ms) VALUES (?1, ?2, ?3) \
         ON CONFLICT(session_id) DO UPDATE SET \
             layout_json = excluded.layout_json, updated_ms = excluded.updated_ms",
        params![id.as_str(), json("layout", &safe)?, now_ms],
    )
    .map_err(|error| StoreError::from_write("layout", id.as_str(), error))?;
    Ok(())
}

/// Replaces only durable Layout bindings. Temporary preview panes live outside
/// the saved Layout and survive an unrelated session save until explicitly closed.
fn sync_layout_bindings(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "DELETE FROM pane_node_bindings WHERE session_id = ?1 AND temporary = 0",
        params![session.id.as_str()],
    )?;
    for pane in session.layout.panes() {
        let Some(node_id) = &pane.node_id else {
            continue;
        };
        conn.execute(
            "INSERT INTO pane_node_bindings \
                 (pane_id, session_id, node_id, temporary, surface_id, opened_ms) \
             VALUES (?1, ?2, ?3, 0, NULL, ?4)",
            params![
                pane.id.as_str(),
                session.id.as_str(),
                node_id.as_str(),
                session.last_activity_ms
            ],
        )?;
    }
    Ok(())
}

/// Deletes stored nodes the in-memory tree no longer contains.
fn prune_nodes(conn: &Connection, session: &SessionId, keep: &[String]) -> Result<()> {
    if keep.is_empty() {
        conn.execute(
            "DELETE FROM process_nodes WHERE session_id = ?1",
            params![session.as_str()],
        )?;
        return Ok(());
    }
    // The id list comes from the tree, never from user input, and SQLite has no
    // way to bind a variable-length list.
    let placeholders = std::iter::repeat_n("?", keep.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql =
        format!("DELETE FROM process_nodes WHERE session_id = ?1 AND id NOT IN ({placeholders})");
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(keep.len() + 1);
    let session_id = session.as_str();
    params.push(&session_id);
    for id in keep {
        params.push(id);
    }
    conn.execute(&sql, params.as_slice())?;
    Ok(())
}

fn from_row(row: &Row<'_>) -> Result<Session> {
    let id: String = row.get("id")?;
    let layout_json: Option<String> = row.get("layout_json")?;
    let Some(layout_json) = layout_json else {
        return Err(StoreError::MissingLayout { id });
    };
    Ok(Session {
        id: SessionId::from_stored(id.clone()),
        workspace_id: WorkspaceId::from_stored(row.get::<_, String>("workspace_id")?),
        name: row.get("name")?,
        note: row.get("note")?,
        cwd: row.get("cwd")?,
        mode: from_tag::<SessionMode>("session mode", &id, &row.get::<_, String>("mode")?)?,
        checkout_id: row
            .get::<_, Option<String>>("checkout_id")?
            .map(CheckoutId::from_stored)
            .unwrap_or_else(|| {
                CheckoutId::primary_for(&WorkspaceId::from_stored(
                    row.get::<_, String>("workspace_id").unwrap_or_default(),
                ))
            }),
        worktree_path: row.get("worktree_path")?,
        read_only_enforced: row.get("read_only_enforced")?,
        env: from_json("session env", &id, &row.get::<_, String>("env_json")?)?,
        layout: from_json::<Layout>("layout", &id, &layout_json)?,
        tree: SessionTree::new(),
        attention: from_json::<AttentionPolicy>(
            "attention policy",
            &id,
            &row.get::<_, String>("attention_json")?,
        )?,
        template_id: row
            .get::<_, Option<String>>("template_id")?
            .map(TemplateId::from_stored),
        status: from_tag::<SessionStatus>("session status", &id, &row.get::<_, String>("status")?)?,
        restore_state: from_tag::<RestoreState>(
            "restore state",
            &id,
            &row.get::<_, String>("restore_state")?,
        )?,
        tags: from_json("session tags", &id, &row.get::<_, String>("tags_json")?)?,
        git_branch: row.get("git_branch")?,
        linked_ref: row.get("linked_ref")?,
        favourite: row.get("favourite")?,
        pinned: row.get("pinned")?,
        sort_key: row.get("sort_key")?,
        parent_session: row
            .get::<_, Option<String>>("parent_session")?
            .map(SessionId::from_stored),
        created_ms: row.get("created_ms")?,
        last_activity_ms: row.get("last_activity_ms")?,
        tmux: row.get("tmux")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::REDACTED;
    use crate::testing;
    use turn_core::model::layout::{Direction, Pane, PaneKind, RestoreBehaviour};
    use turn_core::model::node::{NodeKind, Relation};
    use turn_core::state::{AwaitingReason, DisplayState, Turn};
    use turn_core::AttentionPolicy;

    const T0: i64 = 1_700_000_000_000;

    fn session_with_three_panes(workspace: &WorkspaceId) -> Session {
        let agent = Pane::new(PaneKind::Agent)
            .with_command("claude")
            .with_title("claude");
        let mut layout = Layout::single(agent);
        let agent_id = layout.panes()[0].id.clone();
        layout.split(
            &agent_id,
            Direction::Horizontal,
            Pane::new(PaneKind::Shell)
                .with_command("zsh")
                .with_restore(RestoreBehaviour::Relaunch),
        );
        let shell_id = layout.active.clone().unwrap();
        layout.split(
            &shell_id,
            Direction::Vertical,
            Pane::new(PaneKind::AgentTree),
        );
        layout.active = Some(agent_id);

        Session::new(workspace.clone(), "Fix climbing bugs", "/repo", layout, T0)
    }

    #[test]
    fn a_session_round_trips_with_its_layout_and_every_scalar_field() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = session_with_three_panes(&ws.id);
        session.note = Some("blocked on review".into());
        session.tags = vec!["bug".into(), "climbing".into()];
        session.git_branch = Some("fix/climbing".into());
        session.linked_ref = Some("#104".into());
        session.favourite = true;
        session.pinned = true;
        session.sort_key = -3;
        session.tmux = true;
        session.attention = AttentionPolicy::silent();
        session.template_id = Some(TemplateId::from_stored("tpl_coding"));

        store.sessions().save(&session).unwrap();
        let back = store.sessions().get(&session.id).unwrap().expect("stored");

        assert_eq!(back.name, session.name);
        assert_eq!(back.note, session.note);
        assert_eq!(back.tags, session.tags);
        assert_eq!(back.git_branch, session.git_branch);
        assert_eq!(back.linked_ref, session.linked_ref);
        assert!(back.favourite && back.pinned && back.tmux);
        assert_eq!(back.sort_key, -3);
        assert_eq!(back.attention, AttentionPolicy::silent());
        assert_eq!(back.template_id, session.template_id);
        assert_eq!(
            back.layout, session.layout,
            "the pane tree is byte-identical"
        );
        assert_eq!(back.layout.pane_count(), 3);
        assert_eq!(back.layout.active, session.layout.active);
        assert!(back.layout.sizes_are_normalised());
    }

    #[test]
    fn a_sessions_process_tree_comes_back_with_its_shape_and_both_state_axes() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = session_with_three_panes(&ws.id);

        let mut agent = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::Done);
        let agent_id = session.tree.insert(agent);

        let mut tests = ProcessNode::process(
            session.id.clone(),
            NodeKind::TestRunner,
            "cargo test",
            "/repo",
            T0 + 10,
        );
        tests.lifecycle = Lifecycle::Alive;
        tests.link_to(agent_id.clone(), Relation::Confirmed);
        session.tree.insert(tests);

        store.sessions().save(&session).unwrap();
        let back = store.sessions().get(&session.id).unwrap().unwrap();

        assert_eq!(back.tree.len(), 2);
        assert_eq!(back.tree.children(&agent_id).len(), 1);
        assert_eq!(back.tree.running_count(), 2);
        // Case E survives storage: the turn is over, the child is not.
        assert_eq!(back.display_state(), DisplayState::CompletedTurn);
    }

    /// The whole point of the crate: close the daemon, open it again, and the
    /// user's desk is where they left it — with an honest account of what is
    /// still running.
    #[test]
    fn a_restart_restores_the_layout_and_reports_live_processes_as_orphaned() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn.db");

        let (workspace_id, session_id, pane_count) = {
            let store = crate::Store::open_at(&path).unwrap();
            let ws = testing::saved_workspace(&store, "turn");
            let mut session = session_with_three_panes(&ws.id);
            let mut agent = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
            agent.lifecycle = Lifecycle::Alive;
            agent.pid = Some(4242);
            agent.turn = Some(Turn::AwaitingUser {
                reason: AwaitingReason::Permission,
            });
            session.tree.insert(agent);
            store.sessions().save(&session).unwrap();
            (ws.id, session.id, session.layout.pane_count())
        };

        let store = crate::Store::open_at(&path).unwrap();
        let sessions = store.sessions().list_for_workspace(&workspace_id).unwrap();
        assert_eq!(sessions.len(), 1);

        let restored = store
            .sessions()
            .load_for_restore(&session_id)
            .unwrap()
            .expect("the session survived the restart");
        assert_eq!(restored.layout.pane_count(), pane_count);

        let agent = restored
            .tree
            .primary_agent()
            .expect("the agent node is back");
        assert_eq!(
            agent.lifecycle,
            Lifecycle::Orphaned,
            "a stored `alive` only ever meant alive when we last wrote"
        );
        assert_eq!(
            agent.pid,
            Some(4242),
            "which is what makes re-attaching possible"
        );
        assert_ne!(
            agent.lifecycle,
            Lifecycle::Lost,
            "declaring it lost is the supervisor's call, not the store's"
        );

        // The verbatim read is unchanged: the projection is a read-time view.
        let verbatim = store.sessions().get(&session_id).unwrap().unwrap();
        assert_eq!(
            verbatim.tree.primary_agent().unwrap().lifecycle,
            Lifecycle::Alive
        );
    }

    #[test]
    fn a_session_that_dropped_a_process_does_not_keep_a_ghost_of_it() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = session_with_three_panes(&ws.id);
        let keeper = session.tree.insert(ProcessNode::agent(
            session.id.clone(),
            "claude",
            "/repo",
            T0,
        ));
        let doomed = session.tree.insert(ProcessNode::process(
            session.id.clone(),
            NodeKind::Shell,
            "zsh",
            "/repo",
            T0,
        ));
        store.sessions().save(&session).unwrap();
        assert_eq!(store.nodes().count_for_session(&session.id).unwrap(), 2);

        session.tree.remove(&doomed);
        store.sessions().save(&session).unwrap();

        assert_eq!(store.nodes().count_for_session(&session.id).unwrap(), 1);
        assert!(store.nodes().get(&keeper).unwrap().is_some());
        assert!(store.nodes().get(&doomed).unwrap().is_none());
    }

    #[test]
    fn a_session_whose_last_process_ended_stores_an_empty_tree() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = session_with_three_panes(&ws.id);
        let only = session.tree.insert(ProcessNode::agent(
            session.id.clone(),
            "claude",
            "/repo",
            T0,
        ));
        store.sessions().save(&session).unwrap();

        session.tree.remove(&only);
        store.sessions().save(&session).unwrap();

        let back = store.sessions().get(&session.id).unwrap().unwrap();
        assert!(back.tree.is_empty());
        assert_eq!(
            back.display_state(),
            DisplayState::Idle,
            "an empty session is idle, not a mystery"
        );
    }

    /// `INSERT OR REPLACE` would delete the session row and cascade its children
    /// away. Renaming a session must not erase its history.
    #[test]
    fn updating_a_session_keeps_its_nodes_events_and_attention_entries() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = session_with_three_panes(&ws.id);
        session.tree.insert(ProcessNode::agent(
            session.id.clone(),
            "claude",
            "/repo",
            T0,
        ));
        store.sessions().save(&session).unwrap();
        testing::save_event(&store, &session.id, T0);
        testing::save_attention(&store, &session.id, T0);

        session.name = "Fix climbing bugs (v2)".into();
        store.sessions().save(&session).unwrap();

        assert_eq!(store.sessions().count().unwrap(), 1);
        assert_eq!(store.nodes().count_for_session(&session.id).unwrap(), 1);
        assert_eq!(store.events().count_for_session(&session.id).unwrap(), 1);
        assert_eq!(
            store
                .attention()
                .list_for_session(&session.id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_layout_change_can_be_saved_without_touching_the_session_row() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let session = session_with_three_panes(&ws.id);
        store.sessions().save(&session).unwrap();

        let mut layout = session.layout.clone();
        let target = layout.panes()[0].id.clone();
        layout.split(&target, Direction::Vertical, Pane::new(PaneKind::Logs));
        store
            .sessions()
            .save_layout(&session.id, &layout, T0 + 1_000)
            .unwrap();

        let back = store.sessions().get(&session.id).unwrap().unwrap();
        assert_eq!(back.layout.pane_count(), 4);
        assert_eq!(back.last_activity_ms, T0, "geometry is not activity");
    }

    #[test]
    fn a_secret_pasted_into_a_pane_environment_never_survives_the_save() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut pane = Pane::new(PaneKind::Agent).with_command("claude");
        pane.env = vec![("ANTHROPIC_API_KEY".into(), "sk-ant-do-not-store".into())];
        let session = Session::new(ws.id.clone(), "leaky", "/repo", Layout::single(pane), T0);

        store.sessions().save(&session).unwrap();
        let back = store.sessions().get(&session.id).unwrap().unwrap();
        assert_eq!(back.layout.panes()[0].env[0].1, REDACTED);
    }

    #[test]
    fn a_session_environment_is_redacted_too() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = session_with_three_panes(&ws.id);
        session.env = vec![
            ("LANG".into(), "en_GB.UTF-8".into()),
            ("NPM_TOKEN".into(), "npm_secret".into()),
        ];
        store.sessions().save(&session).unwrap();

        let back = store.sessions().get(&session.id).unwrap().unwrap();
        assert_eq!(back.env[0].1, "en_GB.UTF-8");
        assert_eq!(back.env[1].1, REDACTED);
    }

    #[test]
    fn sessions_are_listed_per_workspace_most_recent_first() {
        let store = testing::store();
        let a = testing::saved_workspace(&store, "a");
        let b = testing::saved_workspace(&store, "b");

        let mut old = session_with_three_panes(&a.id);
        old.name = "old".into();
        let mut recent = session_with_three_panes(&a.id);
        recent.name = "recent".into();
        recent.last_activity_ms = T0 + 5_000;
        let mut elsewhere = session_with_three_panes(&b.id);
        elsewhere.name = "elsewhere".into();

        for session in [&old, &recent, &elsewhere] {
            store.sessions().save(session).unwrap();
        }

        let names: Vec<String> = store
            .sessions()
            .list_for_workspace(&a.id)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["recent", "old"]);
    }

    #[test]
    fn archiving_and_status_filters_agree_with_each_other() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let session = session_with_three_panes(&ws.id);
        store.sessions().save(&session).unwrap();

        assert!(store
            .sessions()
            .set_status(&session.id, SessionStatus::Archived)
            .unwrap());

        assert!(store
            .sessions()
            .list_with_status(SessionStatus::Active)
            .unwrap()
            .is_empty());
        let archived = store
            .sessions()
            .list_with_status(SessionStatus::Archived)
            .unwrap();
        assert_eq!(archived.len(), 1);
        assert!(archived[0].is_archived());
    }

    #[test]
    fn a_restore_state_that_needs_explaining_is_persisted_verbatim() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let session = session_with_three_panes(&ws.id);
        store.sessions().save(&session).unwrap();

        store
            .sessions()
            .set_restore_state(&session.id, RestoreState::PartiallyRestored)
            .unwrap();

        let back = store.sessions().get(&session.id).unwrap().unwrap();
        assert_eq!(back.restore_state, RestoreState::PartiallyRestored);
        assert!(back.restore_state.needs_explanation());
    }

    #[test]
    fn a_duplicated_session_keeps_its_parent_link_and_survives_the_parents_deletion() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let original = session_with_three_panes(&ws.id);
        store.sessions().save(&original).unwrap();
        let copy = original.duplicate(T0 + 1_000);
        store.sessions().save(&copy).unwrap();

        let back = store.sessions().get(&copy.id).unwrap().unwrap();
        assert_eq!(back.parent_session, Some(original.id.clone()));

        assert!(store.sessions().delete(&original.id).unwrap());
        let orphaned = store
            .sessions()
            .get(&copy.id)
            .unwrap()
            .expect("a copy outlives its origin");
        assert_eq!(orphaned.parent_session, None, "the dead link is cleared");
    }

    #[test]
    fn a_session_in_an_unknown_workspace_is_refused_with_a_named_error() {
        let store = testing::store();
        let session = session_with_three_panes(&WorkspaceId::from_stored("ws_ghost"));
        let error = store
            .sessions()
            .save(&session)
            .expect_err("no such workspace");
        match error {
            StoreError::UnknownReference { what, missing } => {
                assert_eq!(what, "session");
                assert_eq!(missing, "ws_ghost");
            }
            other => panic!("expected UnknownReference, got {other:?}"),
        }
        assert_eq!(
            store.sessions().count().unwrap(),
            0,
            "nothing was half-written"
        );
    }

    /// Adversarial: a session row whose layout row was removed behind Turn's
    /// back. The store must say so rather than invent a layout.
    #[test]
    fn a_session_with_no_stored_layout_reports_the_inconsistency() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let session = session_with_three_panes(&ws.id);
        store.sessions().save(&session).unwrap();
        store
            .connection()
            .execute("DELETE FROM session_layouts", [])
            .unwrap();

        let error = store.sessions().get(&session.id).expect_err("no layout");
        match error {
            StoreError::MissingLayout { id } => assert_eq!(id, session.id.to_string()),
            other => panic!("expected MissingLayout, got {other:?}"),
        }
    }

    #[test]
    fn touching_a_session_only_moves_its_activity_timestamp() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let session = session_with_three_panes(&ws.id);
        store.sessions().save(&session).unwrap();

        assert!(store.sessions().touch(&session.id, T0 + 42_000).unwrap());
        let back = store.sessions().get(&session.id).unwrap().unwrap();
        assert_eq!(back.last_activity_ms, T0 + 42_000);
        assert_eq!(back.created_ms, T0);
        assert_eq!(back.idle_for_ms(T0 + 42_000), 0);
    }
}
