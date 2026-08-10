//! The settings projection: what is in force, where it came from, and what may be changed.
//!
//! Computed by the daemon and rendered by the window, like every other projection here. The
//! rule matters more than usual for this one: resolution *is* the feature, and a client that
//! resolved locally would be a second implementation of the precedence order — the symptom
//! being a sheet that shows one value while the terminal uses another.
//!
//! So the window receives resolved values, the level each came from, the levels each shadowed,
//! and enough about the definition to draw a control. It decides nothing.

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
