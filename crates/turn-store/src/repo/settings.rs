//! Settings and preferences.
//!
//! A key/value table rather than one column per preference: preferences are read
//! individually, written individually, and grow with every release. A wide table
//! would need a migration for each one, and a migration is a thing that can fail
//! on a user's machine.
//!
//! Values are JSON, so a preference can be a bool today and a struct next
//! release; a value this build cannot parse is reported as a decode error naming
//! the key, never silently replaced with a default.

use crate::codec::{from_json, json};
use crate::error::Result;
use rusqlite::{params, Connection};
use serde::{de::DeserializeOwned, Serialize};

pub struct SettingsRepo<'a> {
    conn: &'a Connection,
}

impl<'a> SettingsRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Stores a preference, replacing any previous value.
    pub fn set<T: Serialize>(&self, key: &str, value: &T, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value_json, updated_ms) VALUES (?1, ?2, ?3) \
             ON CONFLICT(key) DO UPDATE SET \
                 value_json = excluded.value_json, updated_ms = excluded.updated_ms",
            params![key, json("setting", value)?, now_ms],
        )?;
        Ok(())
    }

    /// Reads a preference, or `None` if it has never been set.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value_json FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_json("setting", key, &row.get::<_, String>(0)?)?)),
            None => Ok(None),
        }
    }

    /// Reads a preference, falling back to a default when it was never set.
    ///
    /// A stored value that cannot be decoded is still an error: silently falling
    /// back would reset a user's configuration without telling them.
    pub fn get_or<T: DeserializeOwned>(&self, key: &str, fallback: T) -> Result<T> {
        Ok(self.get(key)?.unwrap_or(fallback))
    }

    /// When a preference was last written.
    pub fn updated_ms(&self, key: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT updated_ms FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn remove(&self, key: &str) -> Result<bool> {
        let removed = self
            .conn
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(removed > 0)
    }

    /// Every key that has a value, alphabetically.
    pub fn keys(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM settings ORDER BY key ASC")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row.get(0)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::testing;
    use turn_core::attention::{Action, Sound};
    use turn_core::AttentionPolicy;

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn a_scalar_preference_round_trips_and_can_be_overwritten() {
        let store = testing::store();
        store.settings().set("ui.theme", &"dark", T0).unwrap();
        assert_eq!(
            store.settings().get::<String>("ui.theme").unwrap(),
            Some("dark".to_string())
        );

        store
            .settings()
            .set("ui.theme", &"light", T0 + 100)
            .unwrap();
        assert_eq!(
            store.settings().get::<String>("ui.theme").unwrap(),
            Some("light".to_string())
        );
        assert_eq!(
            store.settings().updated_ms("ui.theme").unwrap(),
            Some(T0 + 100)
        );
        assert_eq!(store.settings().keys().unwrap(), vec!["ui.theme"]);
    }

    #[test]
    fn a_structured_preference_keeps_its_shape() {
        let store = testing::store();
        let policy = AttentionPolicy {
            sound: Sound::Alert,
            cooldown_seconds: 45,
            ..AttentionPolicy::default()
        };
        store
            .settings()
            .set("attention.default", &policy, T0)
            .unwrap();

        let back: AttentionPolicy = store
            .settings()
            .get("attention.default")
            .unwrap()
            .expect("stored");
        assert_eq!(back, policy);
        assert_eq!(back.sound, Sound::Alert);
        assert!(back.on_permission_required.contains(&Action::FocusIfIdle));
    }

    #[test]
    fn an_unset_preference_falls_back_without_being_written() {
        let store = testing::store();
        assert_eq!(store.settings().get::<bool>("ui.compact").unwrap(), None);
        assert!(store.settings().get_or("ui.compact", true).unwrap());
        assert!(
            store.settings().keys().unwrap().is_empty(),
            "reading a default must not persist it"
        );
    }

    /// Silently returning the default would reset a user's configuration and tell
    /// them nothing. The failure is surfaced instead.
    #[test]
    fn a_value_this_build_cannot_parse_is_an_error_rather_than_a_silent_reset() {
        let store = testing::store();
        store
            .settings()
            .set("ui.compact", &"not a bool", T0)
            .unwrap();

        let error = store
            .settings()
            .get::<bool>("ui.compact")
            .expect_err("a type change must be visible");
        let rendered = error.to_string();
        assert!(rendered.contains("ui.compact"), "got {rendered}");
    }

    #[test]
    fn removing_a_preference_reports_whether_there_was_one() {
        let store = testing::store();
        store.settings().set("ui.theme", &"dark", T0).unwrap();
        assert!(store.settings().remove("ui.theme").unwrap());
        assert!(!store.settings().remove("ui.theme").unwrap());
        assert_eq!(store.settings().get::<String>("ui.theme").unwrap(), None);
    }
}
