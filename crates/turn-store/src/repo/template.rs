//! Template persistence.

use crate::codec::{from_json, from_json_opt, json};
use crate::error::Result;
use crate::redact::template_for_persistence;
use rusqlite::{params, Connection, Row};
use turn_core::ids::TemplateId;
use turn_core::model::{Layout, PaneKind, Template};
use turn_core::AttentionPolicy;

const COLUMNS: &str = "id, name, description, icon, layout_json, attention_json, \
     init_commands_json, name_pattern, hotkey, env_json, tmux, built_in, created_ms";
const LEGACY_BUILT_IN_NAMES: &[&str] = &["Blank", "Coding", "PR Review", "Pair of Agents"];

pub struct TemplateRepo<'a> {
    conn: &'a Connection,
}

impl<'a> TemplateRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn save(&self, template: &Template) -> Result<()> {
        let safe = template_for_persistence(template);
        self.conn.execute(
            "INSERT INTO templates (id, name, description, icon, layout_json, attention_json, \
                 init_commands_json, name_pattern, hotkey, env_json, tmux, built_in, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
             ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, description = excluded.description, \
                 icon = excluded.icon, layout_json = excluded.layout_json, \
                 attention_json = excluded.attention_json, \
                 init_commands_json = excluded.init_commands_json, \
                 name_pattern = excluded.name_pattern, hotkey = excluded.hotkey, \
                 env_json = excluded.env_json, tmux = excluded.tmux, \
                 built_in = excluded.built_in, created_ms = excluded.created_ms",
            params![
                safe.id.as_str(),
                safe.name,
                safe.description,
                safe.icon,
                json("layout", &safe.layout)?,
                safe.attention
                    .as_ref()
                    .map(|p| json("attention policy", p))
                    .transpose()?,
                json("template init commands", &safe.init_commands)?,
                safe.name_pattern,
                safe.hotkey,
                json("template env", &safe.env)?,
                safe.tmux,
                safe.built_in,
                safe.created_ms,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &TemplateId) -> Result<Option<Template>> {
        let sql = format!("SELECT {COLUMNS} FROM templates WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Every template: the built-ins first, then the user's own, alphabetically.
    pub fn list(&self) -> Result<Vec<Template>> {
        let sql = format!("SELECT {COLUMNS} FROM templates ORDER BY built_in DESC, name ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(from_row(row)?);
        }
        Ok(out)
    }

    pub fn find_by_name(&self, name: &str) -> Result<Option<Template>> {
        let sql = format!("SELECT {COLUMNS} FROM templates WHERE name = ?1 LIMIT 1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![name])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Reconciles the shipped preset set and upgrades obsolete navigation Panes,
    /// returning how many Template rows changed.
    ///
    /// The original four presets launched optional third-party tools, so rows
    /// carrying those names are retired only when they are still marked built-in.
    /// A user-owned Template with the same name is preserved. Workspace defaults
    /// pointing at a retired row are moved to the portable starter preset.
    ///
    /// `AgentTree` cannot coexist with the unified hierarchy. Retained Templates
    /// keep their identity, geometry and other Panes, but that obsolete Pane is
    /// replaced by an ordinary Shell with no declared command.
    pub fn install_built_ins(&self, now_ms: i64) -> Result<usize> {
        let mut changed = 0;

        for mut template in self.list()? {
            if replace_obsolete_navigation_panes(&mut template) {
                self.save(&template)?;
                changed += 1;
            }
        }

        let mut starter_id = None;
        for shipped in Template::built_ins(now_ms) {
            match self.find_built_in_by_name(&shipped.name)? {
                None => {
                    self.save(&shipped)?;
                    starter_id = Some(shipped.id.clone());
                    changed += 1;
                }
                Some(existing) => starter_id = Some(existing.id),
            }
        }

        let legacy: Vec<TemplateId> = self
            .list()?
            .into_iter()
            .filter(|template| {
                template.built_in && LEGACY_BUILT_IN_NAMES.contains(&template.name.as_str())
            })
            .map(|template| template.id)
            .collect();
        for id in &legacy {
            if self.delete(id)? {
                changed += 1;
            }
        }

        if let Some(starter_id) = starter_id {
            for retired_id in legacy {
                self.conn.execute(
                    "UPDATE workspaces SET default_template = ?1 WHERE default_template = ?2",
                    params![starter_id.as_str(), retired_id.as_str()],
                )?;
            }
        }

        Ok(changed)
    }

    fn find_built_in_by_name(&self, name: &str) -> Result<Option<Template>> {
        let sql =
            format!("SELECT {COLUMNS} FROM templates WHERE name = ?1 AND built_in = 1 LIMIT 1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![name])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn delete(&self, id: &TemplateId) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM templates WHERE id = ?1", params![id.as_str()])?;
        Ok(changed > 0)
    }

    pub fn count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM templates", [], |row| row.get(0))?;
        Ok(count as usize)
    }
}

fn replace_obsolete_navigation_panes(template: &mut Template) -> bool {
    let obsolete: Vec<_> = template
        .layout
        .panes()
        .iter()
        .filter(|pane| {
            pane.presentation_kind() == PaneKind::AgentTree
                || pane.launch_kind() == PaneKind::AgentTree
        })
        .map(|pane| pane.id.clone())
        .collect();
    for pane_id in &obsolete {
        let pane = template
            .layout
            .get_mut(pane_id)
            .expect("the Pane id came from this Layout");
        pane.kind = PaneKind::Shell;
        pane.launch_kind = None;
        pane.kind_is_user_set = false;
        pane.title = Some("shell".into());
        pane.command = None;
        pane.args.clear();
        pane.launch_profile = None;
        pane.cwd = None;
        pane.env.clear();
        pane.node_id = None;
        pane.restore = turn_core::model::RestoreBehaviour::Relaunch;
    }
    !obsolete.is_empty()
}

fn from_row(row: &Row<'_>) -> Result<Template> {
    let id: String = row.get("id")?;
    Ok(Template {
        id: TemplateId::from_stored(id.clone()),
        name: row.get("name")?,
        description: row.get("description")?,
        icon: row.get("icon")?,
        layout: from_json::<Layout>("layout", &id, &row.get::<_, String>("layout_json")?)?,
        attention: from_json_opt::<AttentionPolicy>(
            "attention policy",
            &id,
            row.get("attention_json")?,
        )?,
        init_commands: from_json(
            "template init commands",
            &id,
            &row.get::<_, String>("init_commands_json")?,
        )?,
        name_pattern: row.get("name_pattern")?,
        hotkey: row.get("hotkey")?,
        env: from_json("template env", &id, &row.get::<_, String>("env_json")?)?,
        tmux: row.get("tmux")?,
        built_in: row.get("built_in")?,
        created_ms: row.get("created_ms")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::REDACTED;
    use crate::testing;

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn a_template_round_trips_with_its_geometry_and_commands() {
        let store = testing::store();
        let template = Template::coding(T0);
        store.templates().save(&template).unwrap();

        let back = store
            .templates()
            .get(&template.id)
            .unwrap()
            .expect("stored");
        assert_eq!(back, template);
        assert_eq!(back.layout.pane_count(), 3);
        assert!(back.layout.sizes_are_normalised());
        assert_eq!(back.hotkey.as_deref(), Some("cmd+shift+1"));
    }

    #[test]
    fn a_template_with_its_own_policy_keeps_it_and_one_without_stays_none() {
        let store = testing::store();
        let mut own = Template::blank(T0);
        own.attention = Some(AttentionPolicy::silent());
        let inherited = Template::pr_review(T0);

        store.templates().save(&own).unwrap();
        store.templates().save(&inherited).unwrap();

        assert_eq!(
            store.templates().get(&own.id).unwrap().unwrap().attention,
            Some(AttentionPolicy::silent())
        );
        assert_eq!(
            store
                .templates()
                .get(&inherited.id)
                .unwrap()
                .unwrap()
                .attention,
            None,
            "no policy means inherit, and must not become a default"
        );
    }

    #[test]
    fn installing_the_built_ins_twice_does_not_duplicate_them() {
        let store = testing::store();
        assert_eq!(store.templates().install_built_ins(T0).unwrap(), 1);
        assert_eq!(store.templates().install_built_ins(T0 + 60_000).unwrap(), 0);
        assert_eq!(store.templates().count().unwrap(), 1);

        let templates = store.templates().list().unwrap();
        assert_eq!(templates[0].name, "Two Shells");
        assert!(templates[0].built_in);
        assert!(templates[0]
            .layout
            .panes()
            .iter()
            .all(|pane| { pane.kind == PaneKind::Shell && pane.command.is_none() }));
    }

    #[test]
    fn legacy_built_ins_are_retired_but_user_templates_with_those_names_survive() {
        let store = testing::store();
        for legacy in [
            Template::blank(T0),
            Template::coding(T0),
            Template::pr_review(T0),
            Template::pair_of_agents(T0),
        ] {
            store.templates().save(&legacy).unwrap();
        }
        let mut own_coding = Template::coding(T0);
        own_coding.id = TemplateId::from_stored("tpl_user_coding");
        own_coding.built_in = false;
        own_coding.init_commands = vec!["nvm use".into()];
        store.templates().save(&own_coding).unwrap();

        assert_eq!(store.templates().install_built_ins(T0 + 1).unwrap(), 5);

        let back = store.templates().get(&own_coding.id).unwrap().unwrap();
        assert!(!back.built_in);
        assert_eq!(back.init_commands, vec!["nvm use".to_string()]);
        let built_ins: Vec<_> = store
            .templates()
            .list()
            .unwrap()
            .into_iter()
            .filter(|template| template.built_in)
            .map(|template| template.name)
            .collect();
        assert_eq!(built_ins, vec!["Two Shells"]);
    }

    #[test]
    fn an_obsolete_tree_becomes_a_commandless_shell_without_changing_user_template_identity() {
        use turn_core::model::{Direction, Pane};

        let store = testing::store();
        let agent = Pane::new(PaneKind::Agent).with_command("claude");
        let mut layout = Layout::single(agent);
        let agent_id = layout.panes()[0].id.clone();
        layout.split(
            &agent_id,
            Direction::Horizontal,
            Pane::new(PaneKind::AgentTree)
                .with_launch_profile(turn_core::model::AgentLaunchProfileRef::new(
                    "claude",
                    "autonomous",
                ))
                .with_title("agents"),
        );
        let tree_id = layout.active.clone().unwrap();
        layout.split(
            &tree_id,
            Direction::Vertical,
            Pane::new(PaneKind::Server)
                .with_command("cargo run")
                .with_title("custom server"),
        );
        let mut legacy = Template::from_layout("My layout", &layout, T0);
        legacy.init_commands = vec!["nvm use".into()];
        let original_id = legacy.id.clone();
        store.templates().save(&legacy).unwrap();

        assert_eq!(store.templates().install_built_ins(T0 + 1).unwrap(), 2);
        let restored = store.templates().get(&original_id).unwrap().unwrap();
        assert_eq!(restored.id, original_id);
        assert!(!restored.built_in);
        assert_eq!(restored.created_ms, T0);
        assert_eq!(restored.init_commands, vec!["nvm use"]);
        assert!(restored
            .layout
            .panes()
            .iter()
            .all(|pane| pane.kind != PaneKind::AgentTree));
        assert!(restored.layout.panes().iter().any(|pane| {
            pane.kind == PaneKind::Shell
                && pane.launch_kind() == PaneKind::Shell
                && pane.command.is_none()
                && pane.launch_profile.is_none()
        }));
        assert!(restored.layout.panes().iter().any(|pane| {
            pane.kind == PaneKind::Server && pane.command.as_deref() == Some("cargo run")
        }));
        assert_eq!(store.templates().install_built_ins(T0 + 2).unwrap(), 0);
    }

    #[test]
    fn an_obsolete_navigation_launch_intent_is_migrated_even_after_a_view_override() {
        let mut pane = turn_core::model::Pane::new(PaneKind::AgentTree).with_launch_profile(
            turn_core::model::AgentLaunchProfileRef::new("codex", "autonomous"),
        );
        pane.override_kind(PaneKind::Shell);
        let pane_id = pane.id.clone();
        let mut template = Template::from_layout("Legacy navigator", &Layout::single(pane), T0);

        assert!(replace_obsolete_navigation_panes(&mut template));
        let migrated = template.layout.get(&pane_id).unwrap();
        assert_eq!(migrated.presentation_kind(), PaneKind::Shell);
        assert_eq!(migrated.launch_kind(), PaneKind::Shell);
        assert!(!migrated.kind_is_user_set);
        assert!(migrated.launch_profile.is_none());
        assert!(!replace_obsolete_navigation_panes(&mut template));
    }

    #[test]
    fn a_workspace_default_is_repointed_from_a_retired_builtin_to_two_shells() {
        let store = testing::store();
        let coding = Template::coding(T0);
        store.templates().save(&coding).unwrap();
        let mut workspace = testing::saved_workspace(&store, "turn");
        workspace.default_template = Some(coding.id.clone());
        store.workspaces().save(&workspace).unwrap();

        store.templates().install_built_ins(T0 + 1).unwrap();

        let starter = store
            .templates()
            .find_by_name("Two Shells")
            .unwrap()
            .unwrap();
        let restored = store.workspaces().get(&workspace.id).unwrap().unwrap();
        assert_eq!(restored.default_template, Some(starter.id));
        assert!(store.templates().get(&coding.id).unwrap().is_none());
    }

    #[test]
    fn a_stored_template_still_produces_independent_sessions() {
        let store = testing::store();
        let template = Template::pair_of_agents(T0);
        store.templates().save(&template).unwrap();
        let back = store.templates().get(&template.id).unwrap().unwrap();

        let first = back.instantiate();
        let second = back.instantiate();
        let ids: Vec<_> = first.panes().iter().map(|p| p.id.clone()).collect();
        for pane in second.panes() {
            assert!(!ids.contains(&pane.id), "pane id {} leaked", pane.id);
        }
        assert_eq!(first.pane_count(), 2);
    }

    #[test]
    fn a_secret_in_a_templates_environment_is_redacted_like_any_other() {
        let store = testing::store();
        let mut template = Template::blank(T0);
        template.env = vec![("CI_JOB_TOKEN".into(), "glcit-secret".into())];
        store.templates().save(&template).unwrap();

        let back = store.templates().get(&template.id).unwrap().unwrap();
        assert_eq!(back.env[0].1, REDACTED);
    }

    #[test]
    fn deleting_a_template_leaves_the_sessions_made_from_it_alone() {
        let store = testing::store();
        let template = Template::coding(T0);
        store.templates().save(&template).unwrap();
        let ws = testing::saved_workspace(&store, "turn");
        let mut session = testing::saved_session(&store, &ws.id, "from a template");
        session.template_id = Some(template.id.clone());
        store.sessions().save(&session).unwrap();

        assert!(store.templates().delete(&template.id).unwrap());

        let back = store
            .sessions()
            .get(&session.id)
            .unwrap()
            .expect("still here");
        assert_eq!(
            back.template_id,
            Some(template.id),
            "the provenance is kept even though the template is gone"
        );
    }
}
