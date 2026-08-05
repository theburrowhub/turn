//! Template persistence.

use crate::codec::{from_json, from_json_opt, json};
use crate::error::Result;
use crate::redact::{redact_layout, redact_pairs};
use rusqlite::{params, Connection, Row};
use turn_core::ids::TemplateId;
use turn_core::model::{Layout, PaneKind, Template};
use turn_core::AttentionPolicy;

const COLUMNS: &str = "id, name, description, icon, layout_json, attention_json, \
     init_commands_json, name_pattern, hotkey, env_json, tmux, built_in, created_ms";

pub struct TemplateRepo<'a> {
    conn: &'a Connection,
}

impl<'a> TemplateRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn save(&self, template: &Template) -> Result<()> {
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
                template.id.as_str(),
                template.name,
                template.description,
                template.icon,
                json("layout", &redact_layout(&template.layout))?,
                template
                    .attention
                    .as_ref()
                    .map(|p| json("attention policy", p))
                    .transpose()?,
                json("template init commands", &template.init_commands)?,
                template.name_pattern,
                template.hotkey,
                json("template env", &redact_pairs(&template.env))?,
                template.tmux,
                template.built_in,
                template.created_ms,
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

    /// Writes missing built-ins and upgrades an obsolete navigation Pane in a
    /// shipped built-in, returning how many rows changed.
    ///
    /// Matching is by name, not by id: `Template::built_ins` mints fresh ids on
    /// every call, so an id comparison would install four duplicates on every
    /// launch. Existing rows are normally left alone so user overrides survive.
    /// `AgentTree` is the exception: the unified hierarchy makes a second
    /// persistent navigator a product invariant violation, not a visual choice.
    /// Only each obsolete Pane is replaced in place; ids, split geometry and
    /// user-owned panes/policy/commands/environment remain stable.
    pub fn install_built_ins(&self, now_ms: i64) -> Result<usize> {
        let mut installed = 0;
        for shipped in Template::built_ins(now_ms) {
            match self.find_by_name(&shipped.name)? {
                None => {
                    self.save(&shipped)?;
                    installed += 1;
                }
                Some(existing)
                    if existing.built_in
                        && existing
                            .layout
                            .panes()
                            .iter()
                            .any(|pane| pane.kind == PaneKind::AgentTree) =>
                {
                    let mut upgraded = existing;
                    let obsolete: Vec<_> = upgraded
                        .layout
                        .panes()
                        .iter()
                        .filter(|pane| pane.kind == PaneKind::AgentTree)
                        .map(|pane| pane.id.clone())
                        .collect();
                    for pane_id in obsolete {
                        let pane = upgraded
                            .layout
                            .get_mut(&pane_id)
                            .expect("the Pane id came from this Layout");
                        pane.kind = PaneKind::Tui;
                        pane.title = Some("fang (files)".into());
                        pane.command = Some("fang".into());
                        pane.args.clear();
                        pane.cwd = None;
                        pane.env.clear();
                        pane.node_id = None;
                        pane.restore = turn_core::model::RestoreBehaviour::Relaunch;
                    }
                    upgraded.description = shipped.description;
                    self.save(&upgraded)?;
                    installed += 1;
                }
                Some(_) => {}
            }
        }
        Ok(installed)
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
        assert_eq!(store.templates().install_built_ins(T0).unwrap(), 4);
        assert_eq!(store.templates().install_built_ins(T0 + 60_000).unwrap(), 0);
        assert_eq!(store.templates().count().unwrap(), 4);

        let names: Vec<String> = store
            .templates()
            .list()
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            names,
            vec!["Blank", "Coding", "PR Review", "Pair of Agents"]
        );
    }

    #[test]
    fn an_edited_built_in_is_not_overwritten_by_a_later_launch() {
        let store = testing::store();
        store.templates().install_built_ins(T0).unwrap();
        let mut coding = store.templates().find_by_name("Coding").unwrap().unwrap();
        coding.init_commands = vec!["nvm use".into()];
        store.templates().save(&coding).unwrap();

        store.templates().install_built_ins(T0 + 1).unwrap();

        let back = store.templates().get(&coding.id).unwrap().unwrap();
        assert_eq!(back.init_commands, vec!["nvm use".to_string()]);
    }

    #[test]
    fn the_legacy_coding_tree_is_replaced_without_changing_identity_or_user_settings() {
        use turn_core::model::{Direction, Pane};

        let store = testing::store();
        let agent = Pane::new(PaneKind::Agent).with_command("claude");
        let mut layout = Layout::single(agent);
        let agent_id = layout.panes()[0].id.clone();
        layout.split(
            &agent_id,
            Direction::Horizontal,
            Pane::new(PaneKind::AgentTree).with_title("agents"),
        );
        let tree_id = layout.active.clone().unwrap();
        layout.split(
            &tree_id,
            Direction::Vertical,
            Pane::new(PaneKind::Server)
                .with_command("cargo run")
                .with_title("custom server"),
        );
        let mut legacy = Template::from_layout("Coding", &layout, T0);
        legacy.built_in = true;
        legacy.init_commands = vec!["nvm use".into()];
        let original_id = legacy.id.clone();
        store.templates().save(&legacy).unwrap();

        assert_eq!(store.templates().install_built_ins(T0 + 1).unwrap(), 4);
        let coding = store.templates().find_by_name("Coding").unwrap().unwrap();
        assert_eq!(coding.id, original_id);
        assert_eq!(coding.created_ms, T0);
        assert_eq!(coding.init_commands, vec!["nvm use"]);
        assert!(coding
            .layout
            .panes()
            .iter()
            .all(|pane| pane.kind != PaneKind::AgentTree));
        assert!(coding
            .layout
            .panes()
            .iter()
            .any(|pane| { pane.kind == PaneKind::Tui && pane.command.as_deref() == Some("fang") }));
        assert!(coding.layout.panes().iter().any(|pane| {
            pane.kind == PaneKind::Server && pane.command.as_deref() == Some("cargo run")
        }));
        assert_eq!(store.templates().install_built_ins(T0 + 2).unwrap(), 0);
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
