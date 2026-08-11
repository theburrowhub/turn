//! Preferences, one row per level.
//!
//! The flat [`SettingsRepo`](super::settings::SettingsRepo) beside this one holds Turn's own
//! runtime deadlines and maintenance flags, read by key with no notion of a level. This one
//! holds the user's preferences, and the level is half of the identity: a
//! Workspace and a Session may both have an opinion about the font size, and neither may
//! overwrite the other.
//!
//! That separation is the acceptance criterion "changing one level does not destroy overrides
//! below it", and it is enforced by the primary key `(scope, owner_id, key)` rather than by
//! the code above. A resolver reads the levels and picks; nothing merges them, so nothing can
//! merge them wrongly.
//!
//! ## Reset is a delete
//!
//! [`Self::clear`] removes the row. It does not write the inherited value, which would freeze
//! today's inherited answer as tomorrow's override — the user would have "reset" a preference
//! into permanence, and the next change to the level above would not reach them.
//!
//! ## Secrets
//!
//! `value_json` goes through the same redaction as `workspaces.env_json`, because it holds the
//! same kind of thing. Turn does not keep a credential in the clear on disk, and a settings
//! table is not a way around that: an environment variable that looks like a token is stored
//! redacted, and the user sees that it was.

use crate::codec::json;
use crate::error::{Result, StoreError};
use crate::redact::{redact_json, redact_secrets};
use rusqlite::{params, Connection};
use serde_json::Value;
use turn_core::settings::{Layer, Scope};

pub struct SettingLayerRepo<'a> {
    conn: &'a Connection,
}

impl<'a> SettingLayerRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Records one preference at one level.
    ///
    /// `owner` names the Workspace, Template or Session; it is ignored for
    /// [`Scope::Global`], which has exactly one owner and stores the empty string.
    pub fn set(
        &self,
        scope: Scope,
        owner: &str,
        key: &str,
        value: &Value,
        now_ms: i64,
    ) -> Result<()> {
        let owner = owner_of(scope, owner);
        reject_secret_in_identity(key)?;
        reject_secret_in_identity(&owner)?;
        // A temporary override belongs to a window and dies with it. Writing one would make
        // "try a bigger font for ten minutes" a decision the user did not make, and it would
        // outlive the window that made it.
        if !scope.is_persistent() {
            return Err(StoreError::SecurityMaintenanceIncomplete {
                reason: format!(
                    "a {} setting is not persisted and must not be written",
                    scope.label()
                ),
            });
        }
        self.conn.execute(
            "INSERT INTO setting_layers (scope, owner_id, key, value_json, updated_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(scope, owner_id, key) DO UPDATE SET \
                 value_json = excluded.value_json, updated_ms = excluded.updated_ms",
            params![
                scope_name(scope),
                owner,
                key,
                redact_json(&json("setting", value)?),
                now_ms
            ],
        )?;
        Ok(())
    }

    /// Removes one level's opinion, so the level below is seen again. `false` when there was
    /// nothing to remove, which is not an error: resetting an inherited value is a no-op the
    /// user cannot tell apart from success, and refusing it would be a dialog about nothing.
    pub fn clear(&self, scope: Scope, owner: &str, key: &str) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM setting_layers WHERE scope = ?1 AND owner_id = ?2 AND key = ?3",
            params![scope_name(scope), owner_of(scope, owner), key],
        )?;
        Ok(removed > 0)
    }

    /// Removes everything one owner set, for a Workspace or Session being deleted.
    ///
    /// Not a foreign key, deliberately: `owner_id` is one column holding ids from three
    /// different tables, and a constraint cannot say "references whichever table this scope
    /// names". So the cleanup is explicit, and a row left behind by a crash is inert — it
    /// belongs to an id nothing resolves any more.
    pub fn forget_owner(&self, scope: Scope, owner: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM setting_layers WHERE scope = ?1 AND owner_id = ?2",
            params![scope_name(scope), owner_of(scope, owner)],
        )?)
    }

    /// One level's whole opinion, ready to hand to the resolver.
    pub fn layer(&self, scope: Scope, owner: &str) -> Result<Layer> {
        let mut statement = self.conn.prepare(
            "SELECT key, value_json FROM setting_layers \
             WHERE scope = ?1 AND owner_id = ?2 ORDER BY key ASC",
        )?;
        let mut rows = statement.query(params![scope_name(scope), owner_of(scope, owner)])?;
        let mut pairs = Vec::new();
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let raw: String = row.get(1)?;
            // A value this build cannot parse is a decode error naming its key, never a
            // silent default: silently resetting a preference looks to the user exactly like
            // a preference that does not save.
            let value: Value = serde_json::from_str(&raw).map_err(|error| StoreError::Decode {
                what: "setting",
                id: key.clone(),
                cause: error,
            })?;
            pairs.push((key, value));
        }
        Ok(Layer::from_pairs(scope, pairs))
    }

    /// When one preference at one level was last written, for a surface that wants to say so.
    pub fn updated_ms(&self, scope: Scope, owner: &str, key: &str) -> Result<Option<i64>> {
        let mut statement = self.conn.prepare(
            "SELECT updated_ms FROM setting_layers \
             WHERE scope = ?1 AND owner_id = ?2 AND key = ?3",
        )?;
        let mut rows = statement.query(params![scope_name(scope), owner_of(scope, owner), key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Every owner that has set anything at one level, for maintenance and for a settings
    /// surface that lists which Workspaces have overrides.
    pub fn owners(&self, scope: Scope) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT owner_id FROM setting_layers WHERE scope = ?1 ORDER BY owner_id ASC",
        )?;
        let rows =
            statement.query_map(params![scope_name(scope)], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// The Global level has one owner, stored as the empty string rather than NULL: SQLite treats
/// NULLs as distinct in a unique index, so a nullable owner would allow two Global rows for
/// one key and the resolver would see whichever it read first.
fn owner_of(scope: Scope, owner: &str) -> String {
    if scope == Scope::Global {
        String::new()
    } else {
        owner.to_string()
    }
}

/// The stored spelling of a level. Written out rather than derived from `Debug`, so renaming
/// the Rust variant cannot silently orphan every row that used the old name.
fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Global => "global",
        Scope::Workspace => "workspace",
        Scope::Template => "template",
        Scope::Session => "session",
        Scope::Temporary => "temporary",
    }
}

/// A key or an owner id is structural: it is compared, indexed and logged, so a credential in
/// one would survive every redaction that only looks at values.
fn reject_secret_in_identity(value: &str) -> Result<()> {
    if redact_secrets(value) != value {
        return Err(StoreError::SecretInStructuralField {
            what: "setting key",
            owner_id: "setting_layers".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use serde_json::json;

    const T0: i64 = 1_700_000_000_000;

    /// Two levels holding the same key are two rows, and writing one leaves the other exactly
    /// as it was. The acceptance criterion, at the level that enforces it.
    #[test]
    fn a_workspace_and_a_session_hold_the_same_key_without_overwriting_each_other() {
        let store = testing::store();
        let layers = store.setting_layers();

        layers
            .set(
                Scope::Workspace,
                "ws_a",
                "appearance.font_size",
                &json!(13),
                T0,
            )
            .unwrap();
        layers
            .set(
                Scope::Session,
                "sess_a",
                "appearance.font_size",
                &json!(17),
                T0,
            )
            .unwrap();
        // And the Workspace is edited afterwards, which is when a design that stored the
        // resolved value would swallow the Session's override.
        layers
            .set(
                Scope::Workspace,
                "ws_a",
                "appearance.font_size",
                &json!(9),
                T0 + 1,
            )
            .unwrap();

        assert_eq!(
            layers
                .layer(Scope::Workspace, "ws_a")
                .unwrap()
                .get("appearance.font_size"),
            Some(&json!(9))
        );
        assert_eq!(
            layers
                .layer(Scope::Session, "sess_a")
                .unwrap()
                .get("appearance.font_size"),
            Some(&json!(17)),
            "the Session's own override is untouched"
        );
    }

    /// Two Workspaces are two owners.
    #[test]
    fn one_workspaces_preference_is_not_another_workspaces() {
        let store = testing::store();
        let layers = store.setting_layers();
        layers
            .set(
                Scope::Workspace,
                "ws_a",
                "appearance.cursor",
                &json!("bar"),
                T0,
            )
            .unwrap();
        assert!(layers.layer(Scope::Workspace, "ws_b").unwrap().is_empty());
        assert_eq!(
            layers.owners(Scope::Workspace).unwrap(),
            vec!["ws_a".to_string()]
        );
    }

    /// The Global level has one owner whatever the caller passes, so two callers spelling it
    /// differently cannot end up with two Global values for one key.
    #[test]
    fn the_global_level_has_exactly_one_owner() {
        let store = testing::store();
        let layers = store.setting_layers();
        layers
            .set(Scope::Global, "", "appearance.font_size", &json!(12), T0)
            .unwrap();
        layers
            .set(
                Scope::Global,
                "ignored",
                "appearance.font_size",
                &json!(14),
                T0 + 1,
            )
            .unwrap();

        let layer = layers.layer(Scope::Global, "anything").unwrap();
        assert_eq!(layer.get("appearance.font_size"), Some(&json!(14)));
        assert_eq!(
            layers.owners(Scope::Global).unwrap(),
            vec![String::new()],
            "one row, not two"
        );
    }

    /// Reset removes the row rather than writing what was inherited.
    #[test]
    fn resetting_removes_the_row_so_the_level_below_is_seen_again() {
        let store = testing::store();
        let layers = store.setting_layers();
        layers
            .set(
                Scope::Session,
                "sess_a",
                "appearance.font_size",
                &json!(17),
                T0,
            )
            .unwrap();

        assert!(layers
            .clear(Scope::Session, "sess_a", "appearance.font_size")
            .unwrap());
        assert!(layers.layer(Scope::Session, "sess_a").unwrap().is_empty());
        assert!(
            !layers
                .clear(Scope::Session, "sess_a", "appearance.font_size")
                .unwrap(),
            "resetting an inherited value is a no-op, not a failure"
        );
    }

    /// A deliberate `null` is a value and survives the round trip, because it is how a level
    /// says "nothing here" over an inherited something.
    #[test]
    fn a_deliberate_nothing_round_trips_as_a_value() {
        let store = testing::store();
        let layers = store.setting_layers();
        layers
            .set(
                Scope::Session,
                "sess_a",
                "environment.init_commands",
                &Value::Null,
                T0,
            )
            .unwrap();
        assert_eq!(
            layers
                .layer(Scope::Session, "sess_a")
                .unwrap()
                .get("environment.init_commands"),
            Some(&Value::Null),
            "absent and null are different, and only one of them is stored"
        );
    }

    /// Deleting a Session takes its preferences with it.
    #[test]
    fn forgetting_an_owner_takes_every_preference_it_set() {
        let store = testing::store();
        let layers = store.setting_layers();
        layers
            .set(
                Scope::Session,
                "sess_a",
                "appearance.font_size",
                &json!(17),
                T0,
            )
            .unwrap();
        layers
            .set(
                Scope::Session,
                "sess_a",
                "appearance.cursor",
                &json!("bar"),
                T0,
            )
            .unwrap();
        layers
            .set(
                Scope::Session,
                "sess_b",
                "appearance.cursor",
                &json!("bar"),
                T0,
            )
            .unwrap();

        assert_eq!(layers.forget_owner(Scope::Session, "sess_a").unwrap(), 2);
        assert!(layers.layer(Scope::Session, "sess_a").unwrap().is_empty());
        assert!(
            !layers.layer(Scope::Session, "sess_b").unwrap().is_empty(),
            "and only that owner's"
        );
    }

    /// A temporary override is refused rather than quietly persisted. It belongs to a window,
    /// and a stored one would outlive the window that made it.
    #[test]
    fn a_temporary_override_is_never_written_to_disk() {
        let store = testing::store();
        let refused = store.setting_layers().set(
            Scope::Temporary,
            "surface_a",
            "appearance.font_size",
            &json!(24),
            T0,
        );
        assert!(refused.is_err(), "got {refused:?}");
        assert!(store
            .setting_layers()
            .layer(Scope::Temporary, "surface_a")
            .unwrap()
            .is_empty());
    }

    /// A credential inside a *value* is redacted, on the same terms as a Workspace's
    /// environment. Turn does not hold a token in the clear on disk, and the settings table
    /// is not a way around that.
    #[test]
    fn a_credential_in_a_value_does_not_reach_the_column_in_the_clear() {
        let store = testing::store();
        let layers = store.setting_layers();
        layers
            .set(
                Scope::Workspace,
                "ws_a",
                "environment.variables",
                &json!({"GITHUB_TOKEN": "ghp_0123456789abcdef0123456789abcdef0123"}),
                T0,
            )
            .unwrap();

        let stored = format!(
            "{:?}",
            layers
                .layer(Scope::Workspace, "ws_a")
                .unwrap()
                .get("environment.variables")
        );
        assert!(
            !stored.contains("ghp_0123456789abcdef0123456789abcdef0123"),
            "a token reached the column: {stored}"
        );
        assert!(
            stored.contains("GITHUB_TOKEN"),
            "the name stays, so the user can see what is set: {stored}"
        );
    }

    /// A credential used as a *key* is refused outright. A key is compared, indexed and
    /// logged, so redacting the value would not help.
    #[test]
    fn a_credential_used_as_a_key_is_refused() {
        let store = testing::store();
        let refused = store.setting_layers().set(
            Scope::Workspace,
            "ws_a",
            "ghp_0123456789abcdef0123456789abcdef0123",
            &json!(1),
            T0,
        );
        assert!(
            matches!(refused, Err(StoreError::SecretInStructuralField { .. })),
            "got {refused:?}"
        );
    }

    /// A stored value this build cannot parse is an error naming its key, not a silent reset.
    #[test]
    fn an_unreadable_value_is_reported_rather_than_replaced_with_a_default() {
        let store = testing::store();
        store
            .setting_layers()
            .set(
                Scope::Workspace,
                "ws_a",
                "appearance.cursor",
                &json!("bar"),
                T0,
            )
            .unwrap();
        store
            .connection()
            .execute(
                "UPDATE setting_layers SET value_json = 'not json at all' WHERE key = ?1",
                params!["appearance.cursor"],
            )
            .unwrap();

        let error = store
            .setting_layers()
            .layer(Scope::Workspace, "ws_a")
            .expect_err("a value that cannot be read is not a value");
        assert!(
            format!("{error}").contains("appearance.cursor"),
            "the error names the key so the user can fix it: {error}"
        );
    }
}
