//! Workspace persistence.

use crate::codec::{from_json, json};
use crate::error::{Result, StoreError};
use crate::redact::{redact_secrets, workspace_for_persistence};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use turn_core::ids::{CheckoutId, TemplateId, WorkspaceId};
use turn_core::model::Workspace;
use turn_core::AttentionPolicy;

const COLUMNS: &str = "id, name, root, git_remote, env_json, default_shell, default_agent, \
     init_commands_json, default_template, attention_json, colour, icon, created_ms, \
     last_used_ms, tmux_enabled, archived, lease_reconciliation_required";

pub struct WorkspaceRepo<'a> {
    conn: &'a Connection,
}

impl<'a> WorkspaceRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Inserts or updates a workspace.
    ///
    /// The environment is redacted on the way in, so what comes back out of
    /// [`get`](Self::get) is what was safe to keep — not necessarily what was
    /// passed in.
    pub fn save(&self, workspace: &Workspace) -> Result<()> {
        let mut safe = workspace_for_persistence(workspace);
        if redact_secrets(&workspace.root) != workspace.root {
            return Err(StoreError::SecretInStructuralField {
                what: "workspace root",
                owner_id: workspace.id.to_string(),
            });
        }
        // Take the database writer lock before resolving filesystem identity. A
        // concurrent Store must not register the canonical spelling while this
        // one is still comparing a legacy symlink spelling.
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let canonical = canonical_workspace_root(&workspace.root)?;
        let canonical = canonical.to_string_lossy().into_owned();
        if redact_secrets(&canonical) != canonical {
            return Err(StoreError::SecretInStructuralField {
                what: "workspace root",
                owner_id: workspace.id.to_string(),
            });
        }
        safe.root = canonical.clone();
        let existing: Option<(String, bool)> = tx
            .query_row(
                "SELECT root, lease_reconciliation_required FROM workspaces WHERE id = ?1",
                params![workspace.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if existing.as_ref().is_some_and(|(root, required)| {
            root != &canonical || (*required && !safe.lease_reconciliation_required)
        }) {
            return Err(StoreError::LeaseReconciliationRequired {
                workspace_id: workspace.id.to_string(),
                checkout_id: CheckoutId::primary_for(&workspace.id).to_string(),
            });
        }
        let alias: Option<String> = tx
            .query_row(
                "SELECT id FROM workspaces WHERE root = ?1 AND id != ?2 \
                 UNION \
                 SELECT workspace_id FROM workspace_checkouts \
                 WHERE canonical_path = ?1 AND workspace_id != ?2 LIMIT 1",
                params![&canonical, workspace.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_workspace_id) = alias {
            return Err(StoreError::WorkspaceRootAlias {
                canonical_path: canonical,
                existing_workspace_id,
            });
        }
        // Legacy rows may still carry a non-canonical checkout spelling because
        // v6 could not prove it was safe to rewrite an active claim. Compare the
        // real filesystem identities as well as the stored strings so a symlink,
        // case alias, or historically drifted checkout cannot mint a new fence.
        let live_alias = {
            let mut stmt = tx.prepare(
                "SELECT id, root FROM workspaces WHERE id != ?1 \
                 UNION ALL \
                 SELECT workspace_id, path FROM workspace_checkouts \
                 WHERE workspace_id != ?1 AND is_primary = 1",
            )?;
            let candidates = stmt
                .query_map(params![workspace.id.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            candidates.into_iter().find_map(|(workspace_id, path)| {
                match canonical_workspace_root(&path) {
                    Ok(identity) if identity.to_string_lossy() == canonical => Some(workspace_id),
                    Ok(_) | Err(StoreError::WorkspaceRoot { .. }) => None,
                    Err(_) => None,
                }
            })
        };
        if let Some(existing_workspace_id) = live_alias {
            return Err(StoreError::WorkspaceRootAlias {
                canonical_path: canonical,
                existing_workspace_id,
            });
        }
        tx.execute(
            "INSERT INTO workspaces (id, name, root, git_remote, env_json, default_shell, \
                 default_agent, init_commands_json, default_template, attention_json, colour, \
                 icon, created_ms, last_used_ms, tmux_enabled, archived, \
                 lease_reconciliation_required) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
             ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, root = excluded.root, git_remote = excluded.git_remote, \
                 env_json = excluded.env_json, default_shell = excluded.default_shell, \
                 default_agent = excluded.default_agent, \
                 init_commands_json = excluded.init_commands_json, \
                 default_template = excluded.default_template, \
                 attention_json = excluded.attention_json, colour = excluded.colour, \
                 icon = excluded.icon, created_ms = excluded.created_ms, \
                 last_used_ms = excluded.last_used_ms, tmux_enabled = excluded.tmux_enabled, \
                 archived = excluded.archived, \
                 lease_reconciliation_required = excluded.lease_reconciliation_required",
            params![
                workspace.id.as_str(),
                safe.name,
                safe.root,
                safe.git_remote,
                json("workspace env", &safe.env)?,
                safe.default_shell,
                safe.default_agent,
                json("workspace init commands", &safe.init_commands)?,
                safe.default_template.as_ref().map(|t| t.as_str()),
                json("attention policy", &safe.attention)?,
                safe.colour,
                safe.icon,
                safe.created_ms,
                safe.last_used_ms,
                safe.tmux_enabled,
                safe.archived,
                safe.lease_reconciliation_required,
            ],
        )?;
        // The fence is global and deliberately outlives a Workspace. A second
        // caller cannot alias this checkout because the IMMEDIATE transaction
        // checked the canonical identity before inserting the Workspace.
        tx.execute(
            "INSERT INTO checkout_write_fences (canonical_path, generation) \
             VALUES (?1, 0) ON CONFLICT(canonical_path) DO NOTHING",
            params![canonical],
        )?;
        tx.execute(
            "INSERT INTO workspace_checkouts \
                 (id, workspace_id, path, canonical_path, branch, is_primary, \
                  shared_resources_json, created_ms) \
             VALUES (?1, ?2, ?3, ?4, NULL, 1, '[]', ?5) \
             ON CONFLICT(id) DO UPDATE SET path = excluded.path, \
                 canonical_path = excluded.canonical_path",
            params![
                CheckoutId::primary_for(&workspace.id).as_str(),
                workspace.id.as_str(),
                canonical,
                canonical,
                safe.created_ms
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, id: &WorkspaceId) -> Result<Option<Workspace>> {
        let sql = format!("SELECT {COLUMNS} FROM workspaces WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Every workspace, most recently used first.
    pub fn list(&self) -> Result<Vec<Workspace>> {
        self.query(&format!(
            "SELECT {COLUMNS} FROM workspaces ORDER BY last_used_ms DESC, name ASC"
        ))
    }

    /// The workspaces the sidebar shows: everything not archived.
    pub fn list_active(&self) -> Result<Vec<Workspace>> {
        self.query(&format!(
            "SELECT {COLUMNS} FROM workspaces WHERE archived = 0 \
             ORDER BY last_used_ms DESC, name ASC"
        ))
    }

    /// Records that a workspace was used, without rewriting the whole row.
    pub fn touch(&self, id: &WorkspaceId, now_ms: i64) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE workspaces SET last_used_ms = ?2 WHERE id = ?1",
            params![id.as_str(), now_ms],
        )?;
        Ok(changed > 0)
    }

    /// Deletes a workspace and, by cascade, its sessions and everything hanging
    /// off them. Archiving is the reversible option; this one is not.
    ///
    /// It deletes Turn's *record* of the workspace. The checkout it pointed at is a directory
    /// the user chose and Turn does not own: nothing on disk is removed, no branch and no
    /// worktree is touched, and that promise is what makes this action safe to offer at all.
    ///
    /// The per-window tree state and scroll anchors of the Workspace, its Sessions and their
    /// process nodes are cleared here because the presentation tables have no foreign keys to
    /// cascade through — see [`super::session::SessionRepo::delete`]. They are removed before
    /// the Workspace row, while the ownership subqueries can still identify every descendant.
    pub fn delete(&self, id: &WorkspaceId) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM tree_ui_state WHERE \
                 (node_kind = 'workspace' AND node_id = ?1) OR \
                 (node_kind = 'session' AND node_id IN (\
                     SELECT id FROM sessions WHERE workspace_id = ?1)) OR \
                 (node_kind IN ('agent', 'process') AND node_id IN (\
                     SELECT id FROM process_nodes WHERE session_id IN (\
                         SELECT id FROM sessions WHERE workspace_id = ?1)))",
            params![id.as_str()],
        )?;
        tx.execute(
            "UPDATE tree_surface_preferences \
             SET scroll_node_kind = NULL, scroll_node_id = NULL \
             WHERE (scroll_node_kind = 'workspace' AND scroll_node_id = ?1) OR \
                   (scroll_node_kind = 'session' AND scroll_node_id IN (\
                       SELECT id FROM sessions WHERE workspace_id = ?1)) OR \
                   scroll_node_id IN (\
                       SELECT id FROM process_nodes WHERE session_id IN (\
                           SELECT id FROM sessions WHERE workspace_id = ?1))",
            params![id.as_str()],
        )?;
        let changed = tx.execute("DELETE FROM workspaces WHERE id = ?1", params![id.as_str()])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    fn query(&self, sql: &str) -> Result<Vec<Workspace>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(from_row(row)?);
        }
        Ok(out)
    }
}

/// Resolves a Workspace root to the filesystem identity used for checkout
/// fencing. It must already exist and be a directory: using the spelling of a
/// missing path would let a later symlink/rename create a second fence namespace
/// for the same checkout.
pub(crate) fn canonical_workspace_root(path: &str) -> Result<PathBuf> {
    let resolved =
        std::fs::canonicalize(Path::new(path)).map_err(|cause| StoreError::WorkspaceRoot {
            path: path.to_string(),
            cause,
        })?;
    let metadata = std::fs::metadata(&resolved).map_err(|cause| StoreError::WorkspaceRoot {
        path: path.to_string(),
        cause,
    })?;
    if !metadata.is_dir() {
        return Err(StoreError::WorkspaceRoot {
            path: path.to_string(),
            cause: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace root is not a directory",
            ),
        });
    }
    Ok(resolved)
}

/// Canonicalises roots written by pre-v6 builds without ever declaring them
/// reconciled. This runs after SQL migrations because SQLite cannot resolve
/// symlinks or prove that a path exists.
///
/// Multiple legacy aliases remain visible, but are all marked reconciliation
/// required. Their checkout rows may share one canonical fence only when doing
/// so cannot collide two unreleased leases; otherwise their old identities are
/// left intact and every writer stays blocked.
pub(crate) fn canonicalize_persisted_roots(conn: &Connection) -> Result<()> {
    // Begin before the first read. Without this lock, another Store could insert
    // the real path between our legacy-spelling snapshot and its rewrite.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let roots = {
        let mut stmt = tx.prepare("SELECT id, root FROM workspaces ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut unresolved = Vec::new();
    for (workspace_id, root) in roots {
        match canonical_workspace_root(&root) {
            Ok(canonical) => groups
                .entry(canonical.to_string_lossy().into_owned())
                .or_default()
                .push((workspace_id, root)),
            Err(StoreError::WorkspaceRoot { .. }) => unresolved.push(workspace_id),
            Err(error) => return Err(error),
        }
    }

    for workspace_id in unresolved {
        tx.execute(
            "UPDATE workspaces SET lease_reconciliation_required = 1 WHERE id = ?1",
            params![&workspace_id],
        )?;
        tx.execute(
            "UPDATE workspace_write_leases SET state = 'recovery_required' \
             WHERE workspace_id = ?1 AND state != 'released'",
            params![&workspace_id],
        )?;
    }

    for (canonical, members) in groups {
        let aliases = members.len() > 1;
        let mut old_canonicals = Vec::new();
        let mut identity_changed = false;
        let mut unreleased = 0_i64;
        let mut checkout_identity_proven = true;

        for (workspace_id, root) in &members {
            let checkout: Option<(String, String)> = tx
                .query_row(
                    "SELECT path, canonical_path FROM workspace_checkouts \
                     WHERE workspace_id = ?1 AND is_primary = 1",
                    params![workspace_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            match checkout {
                Some((path, old)) => {
                    let same_live_identity = canonical_workspace_root(&path)
                        .ok()
                        .is_some_and(|resolved| resolved.to_string_lossy() == canonical);
                    checkout_identity_proven &= same_live_identity;
                    identity_changed |=
                        !same_live_identity || old != canonical || root != &canonical;
                    old_canonicals.push(old);
                }
                None => {
                    checkout_identity_proven = false;
                    identity_changed = true;
                }
            }
            unreleased += tx.query_row(
                "SELECT COUNT(*) FROM workspace_write_leases \
                 WHERE workspace_id = ?1 AND state != 'released'",
                params![workspace_id],
                |row| row.get::<_, i64>(0),
            )?;
        }

        let requires_reconciliation = aliases || identity_changed || !checkout_identity_proven;
        for (workspace_id, _) in &members {
            tx.execute(
                "UPDATE workspaces SET root = ?2, \
                        lease_reconciliation_required = CASE \
                            WHEN ?3 THEN 1 ELSE lease_reconciliation_required END \
                 WHERE id = ?1",
                params![workspace_id, &canonical, requires_reconciliation],
            )?;
            if requires_reconciliation {
                tx.execute(
                    "UPDATE workspace_write_leases SET state = 'recovery_required' \
                     WHERE workspace_id = ?1 AND state != 'released'",
                    params![workspace_id],
                )?;
            }
        }

        if !checkout_identity_proven || unreleased > 1 {
            // Two historical claims cannot be merged without violating the
            // canonical unique index. Likewise, a checkout that resolves away
            // from its Workspace root may still have a live legacy writer. Keep
            // that old claim intact; all affected Workspaces are gated above.
            continue;
        }

        let mut generation = tx
            .query_row(
                "SELECT generation FROM checkout_write_fences WHERE canonical_path = ?1",
                params![&canonical],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        for old in &old_canonicals {
            generation = generation.max(
                tx.query_row(
                    "SELECT generation FROM checkout_write_fences WHERE canonical_path = ?1",
                    params![old],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0),
            );
        }
        tx.execute(
            "INSERT INTO checkout_write_fences (canonical_path, generation) VALUES (?1, ?2) \
             ON CONFLICT(canonical_path) DO UPDATE SET \
                 generation = MAX(generation, excluded.generation)",
            params![&canonical, generation],
        )?;

        for (workspace_id, _) in &members {
            tx.execute(
                "UPDATE workspace_write_leases SET canonical_path = ?2 \
                 WHERE workspace_id = ?1 AND state != 'released'",
                params![workspace_id, &canonical],
            )?;
            tx.execute(
                "UPDATE workspace_checkouts SET path = ?2, canonical_path = ?2 \
                 WHERE workspace_id = ?1 AND is_primary = 1",
                params![workspace_id, &canonical],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn from_row(row: &Row<'_>) -> Result<Workspace> {
    let id: String = row.get("id")?;
    let attention: AttentionPolicy = from_json(
        "attention policy",
        &id,
        &row.get::<_, String>("attention_json")?,
    )?;
    Ok(Workspace {
        id: WorkspaceId::from_stored(id.clone()),
        name: row.get("name")?,
        root: row.get("root")?,
        git_remote: row.get("git_remote")?,
        env: from_json("workspace env", &id, &row.get::<_, String>("env_json")?)?,
        default_shell: row.get("default_shell")?,
        default_agent: row.get("default_agent")?,
        init_commands: from_json(
            "workspace init commands",
            &id,
            &row.get::<_, String>("init_commands_json")?,
        )?,
        default_template: row
            .get::<_, Option<String>>("default_template")?
            .map(TemplateId::from_stored),
        attention,
        colour: row.get("colour")?,
        icon: row.get("icon")?,
        created_ms: row.get("created_ms")?,
        last_used_ms: row.get("last_used_ms")?,
        tmux_enabled: row.get("tmux_enabled")?,
        archived: row.get("archived")?,
        lease_reconciliation_required: row.get("lease_reconciliation_required")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::REDACTED;
    use crate::testing;
    use turn_core::attention::Action;

    const T0: i64 = 1_700_000_000_000;

    fn workspace(name: &str) -> Workspace {
        let id = WorkspaceId::new();
        let root = std::env::temp_dir()
            .join("turn-workspace-repo-tests")
            .join(id.as_str());
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let mut workspace = Workspace::new(name, root.to_string_lossy(), T0);
        workspace.id = id;
        workspace
    }

    #[test]
    fn a_workspace_round_trips_with_every_field_intact() {
        let store = testing::store();
        let mut ws = workspace("turn");
        ws.git_remote = Some("git@github.com:x/turn.git".into());
        ws.default_shell = Some("/bin/zsh".into());
        ws.default_agent = Some("claude".into());
        ws.init_commands = vec!["nvm use".into(), "make deps".into()];
        ws.default_template = Some(TemplateId::from_stored("tpl_coding0001"));
        ws.colour = Some("#ff8800".into());
        ws.icon = Some("rocket".into());
        ws.tmux_enabled = true;
        ws.attention = AttentionPolicy::silent();
        ws.env = vec![("PATH".into(), "/usr/bin".into())];

        store.workspaces().save(&ws).unwrap();
        let back = store.workspaces().get(&ws.id).unwrap().expect("stored");

        assert_eq!(back, ws);
        assert_eq!(
            back.attention.on_permission_required,
            vec![Action::Badge],
            "the policy survives as a policy, not as prose"
        );
    }

    #[test]
    fn saving_the_same_workspace_twice_updates_it_instead_of_duplicating() {
        let store = testing::store();
        let mut ws = workspace("turn");
        store.workspaces().save(&ws).unwrap();

        ws.name = "turn (renamed)".into();
        ws.archived = true;
        store.workspaces().save(&ws).unwrap();

        assert_eq!(store.workspaces().count().unwrap(), 1);
        let back = store.workspaces().get(&ws.id).unwrap().unwrap();
        assert_eq!(back.name, "turn (renamed)");
        assert!(back.archived);
    }

    #[test]
    fn a_secret_in_a_workspace_environment_is_redacted_before_it_is_written() {
        let store = testing::store();
        let mut ws = workspace("turn");
        ws.env = vec![
            ("PATH".into(), "/usr/bin".into()),
            ("GITHUB_TOKEN".into(), "ghp_verysecret".into()),
        ];
        store.workspaces().save(&ws).unwrap();

        let back = store.workspaces().get(&ws.id).unwrap().unwrap();
        assert_eq!(back.env[0].1, "/usr/bin");
        assert_eq!(back.env[1].0, "GITHUB_TOKEN");
        assert_eq!(back.env[1].1, REDACTED);
    }

    #[test]
    fn listing_puts_the_most_recently_used_first_and_hides_archived_ones() {
        let store = testing::store();
        let old = workspace("old");
        let mut recent = workspace("recent");
        recent.last_used_ms = T0 + 10_000;
        let mut archived = workspace("archived");
        archived.last_used_ms = T0 + 20_000;
        archived.archived = true;

        for ws in [&old, &recent, &archived] {
            store.workspaces().save(ws).unwrap();
        }

        let all: Vec<String> = store
            .workspaces()
            .list()
            .unwrap()
            .into_iter()
            .map(|w| w.name)
            .collect();
        assert_eq!(all, vec!["archived", "recent", "old"]);

        let active: Vec<String> = store
            .workspaces()
            .list_active()
            .unwrap()
            .into_iter()
            .map(|w| w.name)
            .collect();
        assert_eq!(active, vec!["recent", "old"]);
    }

    #[test]
    fn touching_a_workspace_only_moves_its_timestamp() {
        let store = testing::store();
        let ws = workspace("turn");
        store.workspaces().save(&ws).unwrap();

        assert!(store.workspaces().touch(&ws.id, T0 + 5_000).unwrap());
        let back = store.workspaces().get(&ws.id).unwrap().unwrap();
        assert_eq!(back.last_used_ms, T0 + 5_000);
        assert_eq!(back.created_ms, T0);
        assert_eq!(back.name, "turn");

        assert!(
            !store
                .workspaces()
                .touch(&WorkspaceId::from_stored("ws_ghost"), T0)
                .unwrap(),
            "touching a workspace that is not there reports it instead of inserting one"
        );
    }

    #[test]
    fn asking_for_an_unknown_workspace_yields_none_rather_than_an_error() {
        let store = testing::store();
        assert!(store
            .workspaces()
            .get(&WorkspaceId::from_stored("ws_nope"))
            .unwrap()
            .is_none());
        assert!(!store
            .workspaces()
            .delete(&WorkspaceId::from_stored("ws_nope"))
            .unwrap());
    }

    #[test]
    fn a_missing_root_is_refused_without_minting_a_textual_fence() {
        let store = testing::store();
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("not-cloned-yet");
        let workspace = Workspace::new("missing", missing.to_string_lossy(), T0);

        let error = store.workspaces().save(&workspace).unwrap_err();
        assert!(matches!(error, StoreError::WorkspaceRoot { .. }));
        assert_eq!(store.workspaces().count().unwrap(), 0);
        let fences: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM checkout_write_fences", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fences, 0);
    }

    #[test]
    fn generic_save_cannot_move_a_checkout_or_reconcile_it_implicitly() {
        let store = testing::store();
        let mut workspace = workspace("fixed-root");
        store.workspaces().save(&workspace).unwrap();
        let original = workspace.root.clone();

        workspace.lease_reconciliation_required = true;
        store.workspaces().save(&workspace).unwrap();
        workspace.lease_reconciliation_required = false;
        let error = store.workspaces().save(&workspace).unwrap_err();
        assert!(matches!(
            error,
            StoreError::LeaseReconciliationRequired { .. }
        ));
        workspace.lease_reconciliation_required = true;

        let other = std::env::temp_dir()
            .join("turn-workspace-repo-tests")
            .join(WorkspaceId::new().as_str());
        std::fs::create_dir_all(&other).unwrap();
        workspace.root = other.to_string_lossy().into_owned();

        let error = store.workspaces().save(&workspace).unwrap_err();
        assert!(matches!(
            error,
            StoreError::LeaseReconciliationRequired { .. }
        ));
        let stored = store.workspaces().get(&workspace.id).unwrap().unwrap();
        assert_eq!(stored.root, original);
        assert!(stored.lease_reconciliation_required);
    }

    #[test]
    fn deleting_a_workspace_takes_its_sessions_with_it() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = testing::saved_session(&store, &ws.id, "Fix bug");
        let node = turn_core::model::ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        let node_id = node.id.clone();
        session.tree.insert(node);
        store.sessions().save(&session).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tree_ui_state \
                 (surface_id, node_kind, node_id, updated_ms) VALUES ('window', 'process', ?1, ?2)",
                params![node_id.as_str(), T0],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tree_surface_preferences \
                 (surface_id, scroll_node_kind, scroll_node_id, updated_ms) \
                 VALUES ('window', 'process', ?1, ?2)",
                params![node_id.as_str(), T0],
            )
            .unwrap();

        assert!(store.workspaces().delete(&ws.id).unwrap());
        assert!(store.sessions().get(&session.id).unwrap().is_none());
        assert!(testing::rows_mentioning(&store, "node_id", node_id.as_str()).is_empty());
        let anchor: Option<String> = store
            .connection()
            .query_row(
                "SELECT scroll_node_id FROM tree_surface_preferences WHERE surface_id = 'window'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(anchor.is_none(), "the scroll anchor outlived its Workspace");
    }
}
