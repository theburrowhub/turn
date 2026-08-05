//! Process node persistence: the metadata a restart needs to be honest.
//!
//! What is stored here is deliberately *only* metadata — pid, command, cwd,
//! lifecycle, relation, exit code and the tool's own external id. That is enough
//! to look a process up in the process table and try to re-attach, and enough to
//! say "this was running and we can no longer find it" when re-attaching fails.
//!
//! What is not stored: the pty, the scrollback, the terminal grid, the output
//! channel. Those are ephemeral by nature — a pty master cannot outlive the
//! process, and a restored scrollback would be a screenshot of a conversation the
//! agent no longer remembers.

use crate::codec::{from_json, from_json_opt, from_tag, json, tag};
use crate::error::{Result, StoreError};
use crate::redact::{redact_map, redact_secrets};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashSet;
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::node::{AgentInfo, NodeKind, ProcessNode, Relation, SessionTree};
use turn_core::model::{AgentName, NameSource, PreviewVisibility, Relationship, RelationshipKind};
use turn_core::state::{Lifecycle, Turn};
use turn_core::Confidence;

const COLUMNS: &str = "id, session_id, seq, kind, title, command, args_json, cwd, pid, ppid, \
     lifecycle_json, turn_json, agent_json, external_id, parent, relation, pane_id, \
     declared_name, display_name, name_source, name_confidence, user_renamed, \
     relationship_kind, relationship_confidence, preview_visibility, activity_preview_json, \
     started_ms, ended_ms, exit_code, env_highlights_json, interaction_pending";

pub struct NodeRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodeRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Inserts or updates one node, keeping its place in the session's order.
    ///
    /// A node seen for the first time goes to the end of the tree; a node already
    /// stored keeps the position it had, so a status update does not reshuffle
    /// the user's view.
    pub fn upsert(&self, node: &ProcessNode) -> Result<()> {
        let seq = match self.seq_of(&node.id)? {
            Some(existing) => existing,
            None => self.next_seq(&node.session_id)?,
        };
        upsert_node(self.conn, node, seq)
    }

    pub fn get(&self, id: &NodeId) -> Result<Option<ProcessNode>> {
        let sql = format!("SELECT {COLUMNS} FROM process_nodes WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Every node of a session, in the order it was inserted.
    pub fn list_for_session(&self, session: &SessionId) -> Result<Vec<ProcessNode>> {
        let sql =
            format!("SELECT {COLUMNS} FROM process_nodes WHERE session_id = ?1 ORDER BY seq ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![session.as_str()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(from_row(row)?);
        }
        Ok(out)
    }

    /// Rebuilds a session's tree.
    pub fn tree_for_session(&self, session: &SessionId) -> Result<SessionTree> {
        Ok(build_tree(self.list_for_session(session)?))
    }

    /// Finds a node by the agent tool's own session or thread id.
    ///
    /// This is the lookup a hook callback needs: the payload identifies itself
    /// with Claude Code's `session_id` or Codex's `thread-id`, not with Turn's.
    pub fn find_by_external_id(&self, external_id: &str) -> Result<Option<ProcessNode>> {
        let sql = format!("SELECT {COLUMNS} FROM process_nodes WHERE external_id = ?1 LIMIT 1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![external_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Every node that has ever claimed this pid, most recent first.
    ///
    /// Returns a list rather than one node on purpose: the OS recycles pids, so a
    /// single answer would be a guess. The caller decides, usually by checking
    /// the start time against the process table.
    pub fn find_by_pid(&self, pid: u32) -> Result<Vec<ProcessNode>> {
        let sql =
            format!("SELECT {COLUMNS} FROM process_nodes WHERE pid = ?1 ORDER BY started_ms DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![pid])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(from_row(row)?);
        }
        Ok(out)
    }

    /// Deletes a node, promoting its children to roots.
    ///
    /// Children are kept and explicitly un-parented, matching
    /// `SessionTree::remove`: a process whose parent record is gone is still a
    /// real process, and hiding it would be worse than showing it at the root
    /// with an unknown relation.
    pub fn delete(&self, id: &NodeId) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE process_nodes SET parent = NULL, relation = ?2 WHERE parent = ?1",
            params![id.as_str(), tag("relation", &Relation::Unknown)?],
        )?;
        let removed = tx.execute(
            "DELETE FROM process_nodes WHERE id = ?1",
            params![id.as_str()],
        )?;
        tx.commit()?;
        Ok(removed > 0)
    }

    pub fn count_for_session(&self, session: &SessionId) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM process_nodes WHERE session_id = ?1",
            params![session.as_str()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn seq_of(&self, id: &NodeId) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq FROM process_nodes WHERE id = ?1")?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    fn next_seq(&self, session: &SessionId) -> Result<i64> {
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM process_nodes WHERE session_id = ?1",
            params![session.as_str()],
            |row| row.get(0),
        )?;
        Ok(next)
    }
}

/// Writes one node at an explicit position. Shared with [`SessionRepo`], which
/// saves a whole tree in one transaction.
///
/// [`SessionRepo`]: crate::repo::SessionRepo
pub(crate) fn upsert_node(conn: &Connection, node: &ProcessNode, seq: i64) -> Result<()> {
    let external_id = node
        .agent
        .as_ref()
        .and_then(|agent| agent.external_id.clone());
    let env = redact_map(&node.env_highlights);
    let name = node.agent.as_ref().map(|agent| &agent.name);
    let safe_preview = node.activity_preview.as_ref().map(|preview| {
        let mut safe = preview.clone();
        let redacted = redact_secrets(&safe.normalized_text);
        if redacted != safe.normalized_text {
            safe.normalized_text = redacted;
            safe.contains_sensitive_data = true;
            safe.redacted = true;
        }
        safe
    });
    let preview_json = safe_preview
        .as_ref()
        .map(|preview| json("activity preview", preview))
        .transpose()?;
    let previous_preview: Option<String> = conn
        .query_row(
            "SELECT activity_preview_json FROM process_nodes WHERE id = ?1",
            params![node.id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();

    conn.execute(
        "INSERT INTO process_nodes (id, session_id, seq, kind, title, command, args_json, cwd, \
             pid, ppid, lifecycle_json, turn_json, agent_json, external_id, parent, relation, \
             pane_id, declared_name, display_name, name_source, name_confidence, user_renamed, \
             relationship_kind, relationship_confidence, preview_visibility, activity_preview_json, \
             started_ms, ended_ms, exit_code, env_highlights_json, interaction_pending) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
             ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31) \
         ON CONFLICT(id) DO UPDATE SET \
             session_id = excluded.session_id, seq = excluded.seq, kind = excluded.kind, \
             title = excluded.title, command = excluded.command, args_json = excluded.args_json, \
             cwd = excluded.cwd, pid = excluded.pid, ppid = excluded.ppid, \
             lifecycle_json = excluded.lifecycle_json, turn_json = excluded.turn_json, \
             agent_json = excluded.agent_json, external_id = excluded.external_id, \
             parent = excluded.parent, relation = excluded.relation, \
             pane_id = excluded.pane_id, declared_name = excluded.declared_name, \
             display_name = excluded.display_name, name_source = excluded.name_source, \
             name_confidence = excluded.name_confidence, user_renamed = excluded.user_renamed, \
             relationship_kind = excluded.relationship_kind, \
             relationship_confidence = excluded.relationship_confidence, \
             preview_visibility = excluded.preview_visibility, \
             activity_preview_json = excluded.activity_preview_json, \
             started_ms = excluded.started_ms, \
             ended_ms = excluded.ended_ms, exit_code = excluded.exit_code, \
             env_highlights_json = excluded.env_highlights_json, \
             interaction_pending = excluded.interaction_pending",
        params![
            node.id.as_str(),
            node.session_id.as_str(),
            seq,
            tag("node kind", &node.kind)?,
            node.title,
            node.command,
            json("node args", &node.args)?,
            node.cwd,
            node.pid,
            node.ppid,
            json("lifecycle", &node.lifecycle)?,
            node.turn.as_ref().map(|t| json("turn", t)).transpose()?,
            node.agent
                .as_ref()
                .map(|a| json("agent info", a))
                .transpose()?,
            external_id,
            node.parent.as_ref().map(|p| p.as_str()),
            tag("relation", &node.relation)?,
            Option::<&str>::None,
            name.and_then(|name| name.declared_name.as_deref()),
            name.map(|name| name.display_name.as_str()),
            tag(
                "agent name source",
                &name.map(|name| name.source).unwrap_or(NameSource::Fallback),
            )?,
            tag(
                "agent name confidence",
                &name
                    .map(|name| name.confidence)
                    .unwrap_or(Confidence::Unknown),
            )?,
            name.is_some_and(|name| name.user_renamed),
            tag("relationship kind", &node.relationship.kind)?,
            tag("relationship confidence", &node.relationship.confidence)?,
            tag("preview visibility", &node.preview_visibility)?,
            preview_json,
            node.started_ms,
            node.ended_ms,
            node.exit_code,
            json("node env highlights", &env)?,
            node.interaction_pending,
        ],
    )
    .map_err(|error| StoreError::from_write("process node", node.session_id.as_str(), error))?;

    if previous_preview != preview_json {
        if let Some(preview) = safe_preview {
            conn.execute(
                "INSERT INTO activity_previews (node_id, raw_source_sequence, normalized_text, \
                     source_type, confidence, stable, contains_sensitive_data, redacted, created_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    node.id.as_str(),
                    preview.raw_source_sequence.map(|value| value as i64),
                    preview.normalized_text,
                    tag("preview source", &preview.source)?,
                    tag("preview confidence", &preview.confidence)?,
                    preview.stable,
                    preview.contains_sensitive_data,
                    preview.redacted,
                    preview.updated_ms,
                ],
            )?;
            // A preview is navigation state, not scrollback. Bound both per node
            // and globally so a noisy agent cannot grow the database forever.
            conn.execute(
                "DELETE FROM activity_previews WHERE node_id = ?1 AND id NOT IN ( \
                     SELECT id FROM activity_previews WHERE node_id = ?1 \
                     ORDER BY created_ms DESC, id DESC LIMIT 20)",
                params![node.id.as_str()],
            )?;
            conn.execute(
                "DELETE FROM activity_previews WHERE id NOT IN ( \
                     SELECT id FROM activity_previews ORDER BY created_ms DESC, id DESC LIMIT 2000)",
                [],
            )?;
        }
    }
    Ok(())
}

/// Assembles nodes into a tree, repairing links whose parent is not present.
///
/// A dangling parent is not hypothetical: a node can be deleted directly, or a
/// database can be restored from a partial copy. `SessionTree` finds children by
/// matching parent ids, so a node pointing at a parent that is not in the set
/// would appear neither as a root nor as anyone's child — invisible, while its
/// process is still running. Clearing the link puts it back at the root with an
/// honest `Relation::Unknown`.
pub(crate) fn build_tree(nodes: Vec<ProcessNode>) -> SessionTree {
    let known: HashSet<NodeId> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut tree = SessionTree::new();
    for mut node in nodes {
        if let Some(parent) = &node.parent {
            if !known.contains(parent) {
                node.parent = None;
                node.relation = Relation::Unknown;
            }
        }
        tree.insert(node);
    }
    tree
}

pub(crate) fn from_row(row: &Row<'_>) -> Result<ProcessNode> {
    let id: String = row.get("id")?;
    let title: String = row.get("title")?;
    let mut agent = from_json_opt::<AgentInfo>("agent info", &id, row.get("agent_json")?)?;
    if let Some(info) = agent.as_mut() {
        info.name = AgentName {
            declared_name: row.get("declared_name")?,
            display_name: row
                .get::<_, Option<String>>("display_name")?
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| title.clone()),
            source: from_tag::<NameSource>(
                "agent name source",
                &id,
                &row.get::<_, String>("name_source")?,
            )?,
            confidence: from_tag::<Confidence>(
                "agent name confidence",
                &id,
                &row.get::<_, String>("name_confidence")?,
            )?,
            user_renamed: row.get("user_renamed")?,
        };
    }
    Ok(ProcessNode {
        id: NodeId::from_stored(id.clone()),
        session_id: SessionId::from_stored(row.get::<_, String>("session_id")?),
        kind: from_tag::<NodeKind>("node kind", &id, &row.get::<_, String>("kind")?)?,
        title,
        command: row.get("command")?,
        args: from_json("node args", &id, &row.get::<_, String>("args_json")?)?,
        cwd: row.get("cwd")?,
        pid: row.get("pid")?,
        ppid: row.get("ppid")?,
        lifecycle: from_json::<Lifecycle>(
            "lifecycle",
            &id,
            &row.get::<_, String>("lifecycle_json")?,
        )?,
        turn: from_json_opt::<Turn>("turn", &id, row.get("turn_json")?)?,
        agent,
        parent: row
            .get::<_, Option<String>>("parent")?
            .map(NodeId::from_stored),
        relation: from_tag::<Relation>("relation", &id, &row.get::<_, String>("relation")?)?,
        relationship: Relationship {
            kind: from_tag::<RelationshipKind>(
                "relationship kind",
                &id,
                &row.get::<_, String>("relationship_kind")?,
            )?,
            confidence: from_tag::<Confidence>(
                "relationship confidence",
                &id,
                &row.get::<_, String>("relationship_confidence")?,
            )?,
        },
        activity_preview: from_json_opt(
            "activity preview",
            &id,
            row.get("activity_preview_json")?,
        )?,
        preview_visibility: from_tag::<PreviewVisibility>(
            "preview visibility",
            &id,
            &row.get::<_, String>("preview_visibility")?,
        )?,
        started_ms: row.get("started_ms")?,
        ended_ms: row.get("ended_ms")?,
        exit_code: row.get("exit_code")?,
        env_highlights: from_json(
            "node env highlights",
            &id,
            &row.get::<_, String>("env_highlights_json")?,
        )?,
        interaction_pending: row.get("interaction_pending")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::REDACTED;
    use crate::testing;
    use turn_core::event::{AgentRef, Risk};
    use turn_core::model::node::PendingPermission;
    use turn_core::state::AwaitingReason;

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn an_agent_node_round_trips_with_its_turn_axis_and_agent_detail() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "Fix climbing bugs");

        let mut node = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        node.pid = Some(4242);
        node.ppid = Some(1);
        node.lifecycle = Lifecycle::Alive;
        node.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        node.interaction_pending = true;
        node.args = vec!["--resume".into()];
        node.agent = Some(AgentInfo {
            agent: AgentRef {
                provider: Some("anthropic".into()),
                tool: Some("claude-code".into()),
                model: Some("opus".into()),
                external_id: None,
            },
            name: AgentName::fallback("Claude Code"),
            external_id: Some("claude-abc123".into()),
            agent_type: None,
            current_task: Some("run the tests".into()),
            last_message: Some("May I run make verify?".into()),
            pending_permission: Some(PendingPermission {
                summary: "run make verify".into(),
                command: Some("make verify".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
                requested_ms: T0 + 500,
                cwd: Some("/repo".into()),
            }),
            pending_question: None,
            tokens_used: Some(12_345),
            cost_usd: Some(0.42),
            permission_mode: Some("default".into()),
            git_branch: Some("fix/climbing".into()),
            resumable: true,
        });

        store.nodes().upsert(&node).unwrap();
        let back = store.nodes().get(&node.id).unwrap().expect("stored");

        assert_eq!(back, node);
        assert_eq!(
            back.display_state().label(),
            "PERMISSION",
            "the two axes survive the round trip as two axes"
        );
    }

    #[test]
    fn a_shell_node_keeps_no_turn_axis_after_a_round_trip() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "shell only");
        let node = ProcessNode::process(session.id.clone(), NodeKind::Shell, "zsh", "/repo", T0);

        store.nodes().upsert(&node).unwrap();
        let back = store.nodes().get(&node.id).unwrap().unwrap();

        assert!(back.turn.is_none(), "a shell owes the user nothing");
        assert!(back.agent.is_none());
    }

    #[test]
    fn an_exit_code_and_a_signal_are_both_preserved_exactly() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "runs");

        let mut exited = ProcessNode::process(
            session.id.clone(),
            NodeKind::TestRunner,
            "cargo test",
            "/",
            T0,
        );
        exited.lifecycle = Lifecycle::Exited { code: 101 };
        exited.exit_code = Some(101);
        exited.ended_ms = Some(T0 + 9_000);

        let mut killed = ProcessNode::process(
            session.id.clone(),
            NodeKind::Server,
            "node server.js",
            "/",
            T0,
        );
        killed.lifecycle = Lifecycle::Signaled {
            signal: "Terminated".into(),
        };

        store.nodes().upsert(&exited).unwrap();
        store.nodes().upsert(&killed).unwrap();

        let back_exited = store.nodes().get(&exited.id).unwrap().unwrap();
        assert_eq!(back_exited.lifecycle, Lifecycle::Exited { code: 101 });
        assert_eq!(back_exited.exit_code, Some(101));
        assert_eq!(back_exited.ended_ms, Some(T0 + 9_000));

        let back_killed = store.nodes().get(&killed.id).unwrap().unwrap();
        assert_eq!(
            back_killed.lifecycle,
            Lifecycle::Signaled {
                signal: "Terminated".into()
            },
            "the platform's own signal name is kept rather than a number"
        );
    }

    #[test]
    fn a_hook_can_find_its_node_by_the_tools_own_session_id() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "agent");
        let mut node = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        node.agent.as_mut().unwrap().external_id = Some("claude-2f9c".into());
        store.nodes().upsert(&node).unwrap();

        let found = store
            .nodes()
            .find_by_external_id("claude-2f9c")
            .unwrap()
            .expect("the external id is indexed");
        assert_eq!(found.id, node.id);
        assert!(store
            .nodes()
            .find_by_external_id("codex-does-not-exist")
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_recycled_pid_returns_every_candidate_newest_first_rather_than_a_guess() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "pids");

        let mut old = ProcessNode::process(session.id.clone(), NodeKind::Shell, "zsh", "/", T0);
        old.pid = Some(500);
        let mut new = ProcessNode::process(
            session.id.clone(),
            NodeKind::Build,
            "make",
            "/",
            T0 + 60_000,
        );
        new.pid = Some(500);
        store.nodes().upsert(&old).unwrap();
        store.nodes().upsert(&new).unwrap();

        let matches = store.nodes().find_by_pid(500).unwrap();
        assert_eq!(matches.len(), 2, "the store must not pick a winner");
        assert_eq!(matches[0].id, new.id, "newest first");
    }

    #[test]
    fn a_confirmed_parent_link_survives_and_stays_confirmed() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "subagents");
        let parent = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        let mut child = ProcessNode::agent(session.id.clone(), "explore", "/repo", T0 + 10);
        child.kind = NodeKind::Subagent;
        child.link_to(parent.id.clone(), Relation::Confirmed);

        store.nodes().upsert(&parent).unwrap();
        store.nodes().upsert(&child).unwrap();

        let tree = store.nodes().tree_for_session(&session.id).unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree.roots().len(), 1);
        assert_eq!(tree.children(&parent.id).len(), 1);
        assert_eq!(tree.get(&child.id).unwrap().relation, Relation::Confirmed);
    }

    #[test]
    fn deleting_a_node_promotes_its_children_instead_of_hiding_them() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "tree");
        let parent = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        let mut child = ProcessNode::process(
            session.id.clone(),
            NodeKind::TestRunner,
            "npm test",
            "/",
            T0,
        );
        child.link_to(parent.id.clone(), Relation::Confirmed);
        store.nodes().upsert(&parent).unwrap();
        store.nodes().upsert(&child).unwrap();

        assert!(store.nodes().delete(&parent.id).unwrap());

        let back = store.nodes().get(&child.id).unwrap().expect("still stored");
        assert!(back.parent.is_none());
        assert_eq!(
            back.relation,
            Relation::Unknown,
            "the link is dropped honestly, not re-pointed at a guess"
        );
        assert_eq!(store.nodes().count_for_session(&session.id).unwrap(), 1);
    }

    /// Adversarial: a database where a parent row is missing entirely. The child
    /// must still be visible, because its process may well still be running.
    #[test]
    fn a_node_whose_parent_vanished_loads_at_the_root_rather_than_disappearing() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "corrupt");
        let mut orphan =
            ProcessNode::process(session.id.clone(), NodeKind::Unknown, "mystery", "/", T0);
        orphan.link_to(
            NodeId::from_stored("proc_never_stored"),
            Relation::Confirmed,
        );
        store.nodes().upsert(&orphan).unwrap();

        let tree = store.nodes().tree_for_session(&session.id).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.roots().len(), 1, "it must not become invisible");
        assert_eq!(tree.get(&orphan.id).unwrap().relation, Relation::Unknown);
    }

    #[test]
    fn insertion_order_is_preserved_across_a_reload() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "ordered");
        let mut ids = Vec::new();
        for i in 0..8 {
            let node = ProcessNode::process(
                session.id.clone(),
                NodeKind::Shell,
                format!("cmd{i}"),
                "/",
                T0 + i,
            );
            ids.push(node.id.clone());
            store.nodes().upsert(&node).unwrap();
        }

        let tree = store.nodes().tree_for_session(&session.id).unwrap();
        let loaded: Vec<NodeId> = tree.iter().map(|n| n.id.clone()).collect();
        assert_eq!(loaded, ids);
    }

    #[test]
    fn updating_a_node_keeps_its_position_in_the_tree() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "ordered");
        let first = ProcessNode::process(session.id.clone(), NodeKind::Shell, "first", "/", T0);
        let mut second =
            ProcessNode::process(session.id.clone(), NodeKind::Shell, "second", "/", T0 + 1);
        store.nodes().upsert(&first).unwrap();
        store.nodes().upsert(&second).unwrap();

        second.lifecycle = Lifecycle::Exited { code: 0 };
        store.nodes().upsert(&second).unwrap();

        let order: Vec<String> = store
            .nodes()
            .list_for_session(&session.id)
            .unwrap()
            .into_iter()
            .map(|n| n.command)
            .collect();
        assert_eq!(order, vec!["first", "second"]);
    }

    #[test]
    fn env_highlights_are_redacted_before_they_reach_the_database() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "env");
        let mut node = ProcessNode::process(session.id.clone(), NodeKind::Shell, "zsh", "/", T0);
        node.env_highlights
            .insert("AWS_SECRET_ACCESS_KEY".into(), "abc/secret+value".into());
        node.env_highlights
            .insert("NODE_ENV".into(), "development".into());
        store.nodes().upsert(&node).unwrap();

        let back = store.nodes().get(&node.id).unwrap().unwrap();
        assert_eq!(back.env_highlights["AWS_SECRET_ACCESS_KEY"], REDACTED);
        assert_eq!(back.env_highlights["NODE_ENV"], "development");
    }

    #[test]
    fn a_node_for_an_unknown_session_is_refused_with_a_named_error() {
        let store = testing::store();
        let node = ProcessNode::process(
            SessionId::from_stored("sess_ghost"),
            NodeKind::Shell,
            "zsh",
            "/",
            T0,
        );
        let error = store
            .nodes()
            .upsert(&node)
            .expect_err("an unattributable node must not be stored");
        match error {
            StoreError::UnknownReference { what, missing } => {
                assert_eq!(what, "process node");
                assert_eq!(missing, "sess_ghost");
            }
            other => panic!("expected UnknownReference, got {other:?}"),
        }
    }
}
