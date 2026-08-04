//! Workspace persistence.

use crate::codec::{from_json, json};
use crate::error::Result;
use crate::redact::redact_pairs;
use rusqlite::{params, Connection, Row};
use turn_core::ids::{TemplateId, WorkspaceId};
use turn_core::model::Workspace;
use turn_core::AttentionPolicy;

const COLUMNS: &str = "id, name, root, git_remote, env_json, default_shell, default_agent, \
     init_commands_json, default_template, attention_json, colour, icon, created_ms, \
     last_used_ms, tmux_enabled, archived";

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
        let env = redact_pairs(&workspace.env);
        self.conn.execute(
            "INSERT INTO workspaces (id, name, root, git_remote, env_json, default_shell, \
                 default_agent, init_commands_json, default_template, attention_json, colour, \
                 icon, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
             ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, root = excluded.root, git_remote = excluded.git_remote, \
                 env_json = excluded.env_json, default_shell = excluded.default_shell, \
                 default_agent = excluded.default_agent, \
                 init_commands_json = excluded.init_commands_json, \
                 default_template = excluded.default_template, \
                 attention_json = excluded.attention_json, colour = excluded.colour, \
                 icon = excluded.icon, created_ms = excluded.created_ms, \
                 last_used_ms = excluded.last_used_ms, tmux_enabled = excluded.tmux_enabled, \
                 archived = excluded.archived",
            params![
                workspace.id.as_str(),
                workspace.name,
                workspace.root,
                workspace.git_remote,
                json("workspace env", &env)?,
                workspace.default_shell,
                workspace.default_agent,
                json("workspace init commands", &workspace.init_commands)?,
                workspace.default_template.as_ref().map(|t| t.as_str()),
                json("attention policy", &workspace.attention)?,
                workspace.colour,
                workspace.icon,
                workspace.created_ms,
                workspace.last_used_ms,
                workspace.tmux_enabled,
                workspace.archived,
            ],
        )?;
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
    pub fn delete(&self, id: &WorkspaceId) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM workspaces WHERE id = ?1", params![id.as_str()])?;
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::REDACTED;
    use crate::testing;
    use turn_core::attention::Action;

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn a_workspace_round_trips_with_every_field_intact() {
        let store = testing::store();
        let mut ws = Workspace::new("turn", "/Users/x/turn", T0);
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
        let mut ws = Workspace::new("turn", "/repo", T0);
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
        let mut ws = Workspace::new("turn", "/repo", T0);
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
        let old = Workspace::new("old", "/a", T0);
        let mut recent = Workspace::new("recent", "/b", T0);
        recent.last_used_ms = T0 + 10_000;
        let mut archived = Workspace::new("archived", "/c", T0);
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
        let ws = Workspace::new("turn", "/repo", T0);
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
    fn deleting_a_workspace_takes_its_sessions_with_it() {
        let store = testing::store();
        let ws = testing::saved_workspace(&store, "turn");
        let session = testing::saved_session(&store, &ws.id, "Fix bug");

        assert!(store.workspaces().delete(&ws.id).unwrap());
        assert!(store.sessions().get(&session.id).unwrap().is_none());
    }
}
