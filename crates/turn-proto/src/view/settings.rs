//! The settings projection: what is in force, where it came from, and what may be changed.
//!
//! The persistent levels are computed by the daemon and rendered by the window, like every
//! other projection here. The rule matters more than usual for this one: resolution *is* the
//! feature, and a client that re-resolved those levels locally would be a second implementation
//! of the precedence order — the symptom being a sheet that shows one value while the terminal
//! uses another.
//!
//! So the window receives resolved values, the level each came from, the levels each shadowed,
//! and enough about the definition to draw a control. It adds only the Temporary layer that
//! belongs to that window and therefore cannot come from the daemon.

use serde::{Deserialize, Serialize};
use turn_core::ids::{SessionId, TemplateId, WorkspaceId};
use turn_core::settings::{Area, Resolution, Scope};

/// Everything the preferences surface needs for one Session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SettingsView {
    /// The Session this was resolved for, when one was named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Which levels exist to be edited, in precedence order.
    ///
    /// Not the same as [`Scope::ALL`]: a Session made without a Template has no Template
    /// level, and offering one would be a control that writes to nothing. Each carries the
    /// owner id a write has to quote back.
    pub levels: Vec<SettingsLevel>,
    /// One entry per preference, alphabetically by key.
    ///
    /// Secrets arrive already replaced — the daemon calls
    /// [`Resolution::for_display`](turn_core::settings::Resolution::for_display) on the way
    /// out — so a window cannot render one by accident, and a screenshot of this sheet is
    /// safe to attach to a bug report.
    pub entries: Vec<SettingsEntry>,
}

/// One level that exists for this Session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SettingsLevel {
    pub scope: Scope,
    /// What a write quotes back as `owner_id`. Empty for the Global level.
    pub owner_id: String,
    /// What the user reads: the Workspace's name, the Template's name, the Session's name.
    /// "Global" and "this window" for the two that are not owned by a named thing.
    pub label: String,
}

impl SettingsLevel {
    pub fn global() -> Self {
        Self {
            scope: Scope::Global,
            owner_id: String::new(),
            label: Scope::Global.label().to_string(),
        }
    }

    pub fn workspace(id: &WorkspaceId, name: &str) -> Self {
        Self {
            scope: Scope::Workspace,
            owner_id: id.as_str().to_string(),
            label: name.to_string(),
        }
    }

    pub fn template(id: &TemplateId, name: &str) -> Self {
        Self {
            scope: Scope::Template,
            owner_id: id.as_str().to_string(),
            label: name.to_string(),
        }
    }

    pub fn session(id: &SessionId, name: &str) -> Self {
        Self {
            scope: Scope::Session,
            owner_id: id.as_str().to_string(),
            label: name.to_string(),
        }
    }

    /// The only level owned by the window rather than by a stored object.
    pub fn temporary() -> Self {
        Self {
            scope: Scope::Temporary,
            owner_id: String::new(),
            label: "This window".to_string(),
        }
    }
}

/// What kind of control a preference wants.
///
/// The projection of [`ValueKind`](turn_core::settings::ValueKind), with its choice list
/// owned rather than borrowed from the catalogue. A separate type on purpose: the catalogue's
/// version is the daemon's own validation vocabulary, and this one is a drawing instruction.
/// The window renders whichever it is told and validates nothing — the daemon refuses a bad
/// write, and a window that reimplemented the bounds would be a second validator able to
/// disagree about which values exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "control", rename_all = "snake_case")]
pub enum SettingsControl {
    Toggle,
    Integer {
        min: i64,
        max: i64,
    },
    Number {
        min: f64,
        max: f64,
    },
    Text,
    Choice {
        options: Vec<String>,
    },
    MultiChoice {
        options: Vec<String>,
    },
    /// A list of lines, edited as text: one per line, which is how the user already thinks of
    /// init commands.
    TextList,
    /// Name/value pairs, edited as `NAME=value` lines for the same reason.
    TextMap,
    /// A key this build does not define. Shown as it was stored, with no control: the only
    /// thing that can honestly be offered for it is a reset.
    Unknown,
}

impl SettingsControl {
    /// The drawing instruction for a catalogue definition's shape.
    pub fn from_kind(kind: &turn_core::settings::ValueKind) -> Self {
        use turn_core::settings::ValueKind;
        match kind {
            ValueKind::Bool => SettingsControl::Toggle,
            ValueKind::Integer { min, max } => SettingsControl::Integer {
                min: *min,
                max: *max,
            },
            ValueKind::Number { min, max } => SettingsControl::Number {
                min: *min,
                max: *max,
            },
            ValueKind::Text => SettingsControl::Text,
            ValueKind::Choice { options } => SettingsControl::Choice {
                options: options.iter().map(|option| option.to_string()).collect(),
            },
            ValueKind::ChoiceList { options } => SettingsControl::MultiChoice {
                options: options.iter().map(|option| option.to_string()).collect(),
            },
            ValueKind::TextList => SettingsControl::TextList,
            ValueKind::TextMap => SettingsControl::TextMap,
        }
    }
}

/// One preference, resolved, with enough of its definition to draw a control for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SettingsEntry {
    /// The resolved value, its origin and everything it shadowed.
    pub resolution: Resolution,
    /// Turn's compiled-in answer, used when editing a wider level than every stored
    /// opinion. Kept separately from the effective value so a Session override cannot
    /// leak into the Workspace control beneath it.
    #[serde(default)]
    pub default_value: serde_json::Value,
    pub area: Area,
    /// The section heading this belongs under.
    pub area_title: String,
    pub title: String,
    pub description: String,
    /// What would be accepted, as a sentence, for the control to show while the user types.
    pub accepts: String,
    /// Which control to draw.
    pub control: SettingsControl,
    /// The levels this key may be set at, of the ones that exist. A control offered at a level
    /// that would refuse the write is a control that lies.
    pub settable_at: Vec<Scope>,
    /// Whether the value shown has been replaced because it is a secret.
    pub hidden: bool,
    /// Whether this key is one this build defines.
    ///
    /// False for a value written by a newer Turn. The surface shows it so the user can delete
    /// it, and says it does not know what it is rather than drawing a control that would
    /// refuse every write.
    pub known: bool,
}

impl SettingsEntry {
    /// This level's own opinion, including one currently shadowed by a narrower level.
    pub fn override_at(&self, scope: Scope) -> Option<&serde_json::Value> {
        if self.resolution.origin == Some(scope) {
            return Some(&self.resolution.value);
        }
        self.resolution
            .shadowed
            .iter()
            .find(|opinion| opinion.scope == scope)
            .map(|opinion| &opinion.value)
    }

    /// The value a control at this level should begin with.
    ///
    /// It includes this level and everything wider, but deliberately ignores narrower
    /// opinions: editing Workspace while Session wins must not copy the Session value down
    /// into Workspace and thereby change every sibling Session.
    pub fn value_for_editing_at(&self, scope: Scope) -> &serde_json::Value {
        if let Some(value) = self.override_at(scope) {
            return value;
        }
        let mut inherited: Option<(Scope, &serde_json::Value)> = None;
        for opinion in &self.resolution.shadowed {
            if opinion.scope < scope && inherited.is_none_or(|(current, _)| opinion.scope > current)
            {
                inherited = Some((opinion.scope, &opinion.value));
            }
        }
        if let Some(origin) = self.resolution.origin {
            if origin < scope && inherited.is_none_or(|(current, _)| origin > current) {
                inherited = Some((origin, &self.resolution.value));
            }
        }
        inherited
            .map(|(_, value)| value)
            .unwrap_or(&self.default_value)
    }

    /// Every level with its own value, weakest first.
    pub fn override_scopes(&self) -> Vec<Scope> {
        let mut scopes: Vec<Scope> = self
            .resolution
            .shadowed
            .iter()
            .map(|opinion| opinion.scope)
            .collect();
        if let Some(origin) = self.resolution.origin {
            scopes.push(origin);
        }
        scopes.sort();
        scopes.dedup();
        scopes
    }

    /// The winning level after removing one opinion, or the built-in default.
    pub fn origin_without(&self, removed: Scope) -> Option<Scope> {
        self.override_scopes()
            .into_iter()
            .filter(|scope| *scope != removed)
            .max()
    }
}

impl SettingsView {
    /// Whether any level has an opinion about this key, for a surface offering "reset".
    pub fn entry(&self, key: &str) -> Option<&SettingsEntry> {
        self.entries
            .iter()
            .find(|entry| entry.resolution.key == key)
    }

    /// The entries in one area, in the order they arrived.
    pub fn in_area(&self, area: Area) -> impl Iterator<Item = &SettingsEntry> {
        self.entries.iter().filter(move |entry| entry.area == area)
    }

    /// The level, if any, this Session can write at for the given scope.
    pub fn level(&self, scope: Scope) -> Option<&SettingsLevel> {
        self.levels.iter().find(|level| level.scope == scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turn_core::settings::{Sensitivity, Shadowed};

    fn layered_entry() -> SettingsEntry {
        SettingsEntry {
            resolution: Resolution {
                key: "appearance.font_size".into(),
                value: json!(19),
                origin: Some(Scope::Session),
                shadowed: vec![
                    Shadowed {
                        scope: Scope::Global,
                        value: json!(14),
                    },
                    Shadowed {
                        scope: Scope::Workspace,
                        value: json!(16),
                    },
                ],
                sensitivity: Sensitivity::Plain,
            },
            default_value: json!(13),
            area: Area::Appearance,
            area_title: "Appearance".into(),
            title: "Terminal font size".into(),
            description: String::new(),
            accepts: String::new(),
            control: SettingsControl::Integer { min: 6, max: 32 },
            settable_at: Scope::ALL.to_vec(),
            hidden: false,
            known: true,
        }
    }

    #[test]
    fn editing_a_level_uses_its_own_value_not_a_narrower_winner() {
        let entry = layered_entry();
        assert_eq!(entry.value_for_editing_at(Scope::Global), &json!(14));
        assert_eq!(entry.value_for_editing_at(Scope::Workspace), &json!(16));
        assert_eq!(entry.value_for_editing_at(Scope::Template), &json!(16));
        assert_eq!(entry.value_for_editing_at(Scope::Session), &json!(19));
        assert_eq!(entry.value_for_editing_at(Scope::Temporary), &json!(19));
    }

    #[test]
    fn a_shadowed_override_remains_independently_resettable() {
        let entry = layered_entry();
        assert_eq!(entry.override_at(Scope::Workspace), Some(&json!(16)));
        assert_eq!(
            entry.override_scopes(),
            vec![Scope::Global, Scope::Workspace, Scope::Session]
        );
        assert_eq!(entry.origin_without(Scope::Workspace), Some(Scope::Session));
        assert_eq!(entry.origin_without(Scope::Session), Some(Scope::Workspace));
    }
}
