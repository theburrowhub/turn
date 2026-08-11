//! What settings exist, what they mean, and what they are worth by default.
//!
//! A catalogue rather than a struct with a field per preference, for the reason the store's
//! own key/value table gives: preferences are read one at a time, written one at a time, and
//! grow every release. What the catalogue adds on top of the storage is the part a key/value
//! table cannot hold — a default, a type, an area, and whether the value is a secret.
//!
//! ## Every key here is read by something
//!
//! Deliberately, and it is the rule to keep when adding to this file. A catalogue is the
//! easiest place in a codebase to write a hundred plausible entries that nothing consumes,
//! and the result is a settings sheet full of controls that do nothing — which is worse than
//! a short one, because the user cannot tell which half is real. If a key is added here
//! before its reader exists, the settings sheet grows a lie.
//!
//! The areas, on the other hand, are the complete list from the product's own specification.
//! An area with no keys yet is honest: it says the section exists and is empty.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The section of Settings a key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    ShellAndAgents,
    Environment,
    Layout,
    Appearance,
    Keyboard,
    Attention,
    Security,
    Records,
    Adapters,
    Worktrees,
}

impl Area {
    pub const ALL: [Area; 10] = [
        Area::ShellAndAgents,
        Area::Environment,
        Area::Layout,
        Area::Appearance,
        Area::Keyboard,
        Area::Attention,
        Area::Security,
        Area::Records,
        Area::Adapters,
        Area::Worktrees,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Area::ShellAndAgents => "Shell, agents and models",
            Area::Environment => "Environment and init commands",
            Area::Layout => "Layout",
            Area::Appearance => "Theme, fonts, cursor and zoom",
            Area::Keyboard => "Keyboard",
            Area::Attention => "Attention, sounds and notifications",
            Area::Security => "Security and previews",
            Area::Records => "Logs, scrollback, journals and restore",
            Area::Adapters => "Adapters and updates",
            Area::Worktrees => "Worktrees and shared resources",
        }
    }
}

/// Whether a value may be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Safe to render, log and export.
    Plain,
    /// A token, a password, or anything else that must never reach a log, a preview or an
    /// export. Resolved and used normally; replaced by everything that renders.
    Secret,
    /// A key this build does not define, so nothing is known about it — including whether it
    /// is a secret. Treated as one by anything that would display it: a value written by a
    /// newer build may be a token this build has never heard of.
    Unknown,
}

/// The shape a value must have, so a bad write is refused where it happens rather than
/// panicking in whatever reads it three layers away.
/// Serialised on the way out only. A `Deserialize` here would need the choice list to be
/// owned, and a definition is never received: it is compiled in, and what travels is the
/// resolution.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueKind {
    Bool,
    /// An integer with inclusive bounds. Bounds rather than a bare integer because every
    /// numeric preference here has a range outside which the UI is unusable — a font size of
    /// zero, a scrollback of minus one.
    Integer {
        min: i64,
        max: i64,
    },
    /// A number with inclusive bounds, for the zoom factor.
    Number {
        min: f64,
        max: f64,
    },
    Text,
    /// One of a fixed set, which is what makes a control a menu rather than a field.
    Choice {
        options: &'static [&'static str],
    },
    /// Any subset of a fixed set. Used when several independent behaviours may be enabled
    /// together, while still refusing misspelled values at the daemon boundary.
    ChoiceList {
        options: &'static [&'static str],
    },
    /// A list of strings — init commands, shared paths.
    TextList,
    /// A string-to-string map — environment variables, keyboard overrides.
    TextMap,
}

impl ValueKind {
    /// Whether a value is of this shape.
    ///
    /// `null` is accepted for every kind: it is the deliberate "nothing here" a level uses to
    /// override an inherited value with an absence, and refusing it would make that
    /// impossible to express.
    pub fn accepts(&self, value: &Value) -> bool {
        if value.is_null() {
            return true;
        }
        match self {
            ValueKind::Bool => value.is_boolean(),
            ValueKind::Integer { min, max } => value
                .as_i64()
                .is_some_and(|number| number >= *min && number <= *max),
            ValueKind::Number { min, max } => value
                .as_f64()
                .is_some_and(|number| number >= *min && number <= *max),
            ValueKind::Text => value.is_string(),
            ValueKind::Choice { options } => {
                value.as_str().is_some_and(|text| options.contains(&text))
            }
            ValueKind::ChoiceList { options } => value.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|text| options.contains(&text)))
            }),
            ValueKind::TextList => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            ValueKind::TextMap => value.as_object().is_some_and(|pairs| {
                pairs
                    .values()
                    .all(|value| value.is_string() || value.is_null())
            }),
        }
    }

    /// Why a value was refused, in the words a settings surface can show.
    pub fn describe(&self) -> String {
        match self {
            ValueKind::Bool => "on or off".to_string(),
            ValueKind::Integer { min, max } => format!("a whole number from {min} to {max}"),
            ValueKind::Number { min, max } => format!("a number from {min} to {max}"),
            ValueKind::Text => "text".to_string(),
            ValueKind::Choice { options } => format!("one of {}", options.join(", ")),
            ValueKind::ChoiceList { options } => {
                format!("any of {}", options.join(", "))
            }
            ValueKind::TextList => "a list of lines".to_string(),
            ValueKind::TextMap => "a set of name/value pairs".to_string(),
        }
    }
}

/// One preference.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub key: &'static str,
    pub area: Area,
    /// What the user reads as the control's name.
    pub title: &'static str,
    /// One sentence on what it changes. Shown under the control, because a title alone
    /// leaves the user guessing at what a preference actually does.
    pub description: &'static str,
    pub kind: ValueKind,
    pub default: Value,
    pub sensitivity: Sensitivity,
    /// The levels at which setting this makes sense.
    ///
    /// Not every preference belongs everywhere. A keyboard binding is the user's, the same in
    /// every Workspace, and offering it per Session would be five places to look for one
    /// answer. Enforced by the writer, and the reason a `Scope` is a parameter here rather
    /// than a free-for-all.
    pub scopes: &'static [crate::settings::Scope],
}

use crate::settings::Scope;

const EVERYWHERE: &[Scope] = &[
    Scope::Global,
    Scope::Workspace,
    Scope::Template,
    Scope::Session,
    Scope::Temporary,
];
/// Persisted levels only. For a preference a temporary override would make no sense of —
/// one read once when something is created rather than continuously.
const PERSISTED: &[Scope] = &[
    Scope::Global,
    Scope::Workspace,
    Scope::Template,
    Scope::Session,
];
/// The user's own, the same everywhere.
const GLOBAL_ONLY: &[Scope] = &[Scope::Global];

const ATTENTION_ACTIONS: &[&str] = &[
    "badge",
    "highlight",
    "sound",
    "notify",
    "enqueue",
    "focus",
    "focus_if_idle",
    "focus_if_background",
    "custom",
];

/// Every preference this build knows about.
#[derive(Debug, Clone)]
pub struct Catalogue {
    definitions: Vec<Definition>,
}

impl Default for Catalogue {
    fn default() -> Self {
        Self::built_in()
    }
}

impl Catalogue {
    /// Turn's own catalogue.
    pub fn built_in() -> Self {
        Self {
            definitions: vec![
                // ---------------------------------------------- shell, agents and models
                Definition {
                    key: "shell.command",
                    area: Area::ShellAndAgents,
                    title: "Shell",
                    description: "The program a Shell pane opens. Empty means the login shell \
                                  the operating system says is yours.",
                    kind: ValueKind::Text,
                    default: Value::Null,
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                // ---------------------------------------------- environment
                Definition {
                    key: "environment.variables",
                    area: Area::Environment,
                    title: "Environment variables",
                    description: "Added to the environment of every process Turn starts here. \
                                  A Session's own entries are added to the Workspace's rather \
                                  than replacing them.",
                    kind: ValueKind::TextMap,
                    default: json!({}),
                    sensitivity: Sensitivity::Secret,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "environment.init_commands",
                    area: Area::Environment,
                    title: "Init commands",
                    description: "Run once when a Session is created, before its panes. Read at \
                                  creation, so changing this does not affect Sessions that exist.",
                    kind: ValueKind::TextList,
                    default: json!([]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                // ------------------------------------------------------- layout
                Definition {
                    key: "layout.open_pane_placement",
                    area: Area::Layout,
                    title: "Open Process panes",
                    description: "The placement Turn reuses after you open an Agent or Process: \
                                  replace the current Pane, split right, split below or keep it \
                                  temporary.",
                    kind: ValueKind::Choice {
                        options: &["replace_current", "split_right", "split_below", "temporary"],
                    },
                    default: json!("split_right"),
                    sensitivity: Sensitivity::Plain,
                    scopes: GLOBAL_ONLY,
                },
                // ---------------------------------------------- appearance
                Definition {
                    key: "appearance.font_size",
                    area: Area::Appearance,
                    title: "Terminal font size",
                    description: "Point size of the monospaced font in every pane.",
                    kind: ValueKind::Integer { min: 6, max: 32 },
                    default: json!(13),
                    sensitivity: Sensitivity::Plain,
                    scopes: EVERYWHERE,
                },
                Definition {
                    key: "appearance.ui_font_size",
                    area: Area::Appearance,
                    title: "Interface font size",
                    description: "Point size of Turn's own text: the tree, the headers, the \
                                  dialogs.",
                    kind: ValueKind::Integer { min: 8, max: 28 },
                    default: json!(13),
                    sensitivity: Sensitivity::Plain,
                    scopes: EVERYWHERE,
                },
                Definition {
                    key: "appearance.zoom",
                    area: Area::Appearance,
                    title: "Zoom",
                    description: "Scales the whole window, text and controls together. \
                                  Independent of the two font sizes, which set the ratio \
                                  between them.",
                    kind: ValueKind::Number { min: 0.5, max: 3.0 },
                    default: json!(1.0),
                    sensitivity: Sensitivity::Plain,
                    scopes: EVERYWHERE,
                },
                Definition {
                    key: "appearance.cursor",
                    area: Area::Appearance,
                    title: "Cursor",
                    description: "The shape Turn draws at the live prompt: a filled block, \
                                  narrow bar or underline.",
                    kind: ValueKind::Choice {
                        options: &["block", "bar", "underline"],
                    },
                    default: json!("block"),
                    sensitivity: Sensitivity::Plain,
                    scopes: EVERYWHERE,
                },
                Definition {
                    key: "appearance.cursor_blink",
                    area: Area::Appearance,
                    title: "Blink the cursor",
                    description: "Off is not only a preference: a blinking cursor is a motion \
                                  trigger for some people, and this is the switch that stops it.",
                    kind: ValueKind::Bool,
                    default: json!(true),
                    sensitivity: Sensitivity::Plain,
                    scopes: EVERYWHERE,
                },
                Definition {
                    key: "appearance.ligatures",
                    area: Area::Appearance,
                    title: "Font ligatures",
                    description: "Visually joins programming operators such as -> and != \
                                  without changing their cells, search text or clipboard. Off \
                                  by default so every character remains visually explicit.",
                    kind: ValueKind::Bool,
                    default: json!(false),
                    sensitivity: Sensitivity::Plain,
                    scopes: EVERYWHERE,
                },
                Definition {
                    key: "appearance.reduced_motion",
                    area: Area::Appearance,
                    title: "Reduce motion",
                    description: "Stops Turn's own animation and cursor blink. Follows the \
                                  operating system when unset.",
                    kind: ValueKind::Bool,
                    default: Value::Null,
                    sensitivity: Sensitivity::Plain,
                    scopes: GLOBAL_ONLY,
                },
                // ---------------------------------------------- keyboard
                Definition {
                    key: "keyboard.bindings",
                    area: Area::Keyboard,
                    title: "Keyboard shortcuts",
                    description: "Command id to chord. An empty chord unbinds a command, which \
                                  is how a shortcut is removed rather than replaced.",
                    kind: ValueKind::TextMap,
                    default: json!({}),
                    sensitivity: Sensitivity::Plain,
                    scopes: GLOBAL_ONLY,
                },
                // ------------------------------------------------------- attention
                Definition {
                    key: "attention.on_turn_complete",
                    area: Area::Attention,
                    title: "Turn completed",
                    description: "Actions performed when an Agent finishes a turn.",
                    kind: ValueKind::ChoiceList {
                        options: ATTENTION_ACTIONS,
                    },
                    default: json!(["badge", "enqueue"]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.on_question",
                    area: Area::Attention,
                    title: "Question asked",
                    description: "Actions performed when an Agent asks a question.",
                    kind: ValueKind::ChoiceList {
                        options: ATTENTION_ACTIONS,
                    },
                    default: json!(["badge", "enqueue", "notify"]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.on_permission_required",
                    area: Area::Attention,
                    title: "Permission required",
                    description: "Actions performed when an Agent is blocked on permission.",
                    kind: ValueKind::ChoiceList {
                        options: ATTENTION_ACTIONS,
                    },
                    default: json!(["enqueue", "focus_if_idle", "sound"]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.on_task_complete",
                    area: Area::Attention,
                    title: "Task completed",
                    description: "Actions performed when an Agent reports task completion.",
                    kind: ValueKind::ChoiceList {
                        options: ATTENTION_ACTIONS,
                    },
                    default: json!(["badge", "enqueue", "notify"]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.on_failure",
                    area: Area::Attention,
                    title: "Failure",
                    description: "Actions performed when an Agent or Process fails.",
                    kind: ValueKind::ChoiceList {
                        options: ATTENTION_ACTIONS,
                    },
                    default: json!(["badge", "enqueue", "notify", "highlight"]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.on_waiting_for_user",
                    area: Area::Attention,
                    title: "Waiting for you",
                    description: "Actions performed when a Session explicitly needs input.",
                    kind: ValueKind::ChoiceList {
                        options: ATTENTION_ACTIONS,
                    },
                    default: json!(["badge", "enqueue"]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.on_subagent_appeared",
                    area: Area::Attention,
                    title: "Subagent appeared",
                    description: "Actions performed when an Agent starts a subagent.",
                    kind: ValueKind::ChoiceList {
                        options: ATTENTION_ACTIONS,
                    },
                    default: json!(["badge"]),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.do_not_interrupt_while_typing",
                    area: Area::Attention,
                    title: "Typing guard",
                    description:
                        "Never move focus while you are typing, even if a trigger asks to focus.",
                    kind: ValueKind::Bool,
                    default: json!(true),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.focus_only_if_idle",
                    area: Area::Attention,
                    title: "Focus only when idle",
                    description: "Treat every focus action as requiring an idle user.",
                    kind: ValueKind::Bool,
                    default: json!(false),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.cooldown_seconds",
                    area: Area::Attention,
                    title: "Interruption cooldown",
                    description: "Minimum seconds between attention effects for one Session.",
                    kind: ValueKind::Integer { min: 0, max: 3600 },
                    default: json!(10),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.sound",
                    area: Area::Attention,
                    title: "Sound",
                    description: "Sound used by trigger action ‘sound’; none keeps it silent.",
                    kind: ValueKind::Choice {
                        options: &["none", "subtle", "alert"],
                    },
                    default: json!("subtle"),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.custom_command",
                    area: Area::Attention,
                    title: "Custom action command",
                    description:
                        "Shell command spawned by trigger action ‘custom’. Empty disables it.",
                    kind: ValueKind::Text,
                    default: Value::Null,
                    sensitivity: Sensitivity::Secret,
                    scopes: PERSISTED,
                },
                Definition {
                    key: "attention.priority_boost",
                    area: Area::Attention,
                    title: "Default queue priority",
                    description:
                        "Signed ranking adjustment applied when this Session enters the queue.",
                    kind: ValueKind::Integer {
                        min: -100,
                        max: 100,
                    },
                    default: json!(0),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                // ---------------------------------------------- records
                Definition {
                    key: "records.scrollback_lines",
                    area: Area::Records,
                    title: "Scrollback",
                    description: "How many lines of a pane's output Turn keeps above the \
                                  screen. Larger costs memory per pane, and a Session can have \
                                  many.",
                    kind: ValueKind::Integer {
                        min: 0,
                        max: 200_000,
                    },
                    default: json!(10_000),
                    sensitivity: Sensitivity::Plain,
                    scopes: PERSISTED,
                },
                // ---------------------------------------------- security and previews
                Definition {
                    key: "security.previews",
                    area: Area::Security,
                    title: "Activity previews",
                    description: "Whether Turn shows a line of what each process is doing in \
                                  the tree. Off means the tree shows state and no content, \
                                  which is what a shared screen wants.",
                    kind: ValueKind::Bool,
                    default: json!(true),
                    sensitivity: Sensitivity::Plain,
                    scopes: EVERYWHERE,
                },
            ],
        }
    }

    /// A catalogue with nothing in it, for a test that wants to control every key.
    pub fn empty() -> Self {
        Self {
            definitions: Vec::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .find(|definition| definition.key == key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.definitions.iter().map(|definition| definition.key)
    }

    pub fn in_area(&self, area: Area) -> impl Iterator<Item = &Definition> + '_ {
        self.definitions
            .iter()
            .filter(move |definition| definition.area == area)
    }

    pub fn all(&self) -> impl Iterator<Item = &Definition> + '_ {
        self.definitions.iter()
    }

    /// Whether this key may be written at this level, and whether the value fits.
    ///
    /// One function rather than two so a caller cannot check the shape and forget the level.
    /// An unknown key is refused: a build that accepted writes to keys it does not define
    /// would let a typo persist for ever, looking to the user exactly like a preference that
    /// does not work.
    pub fn check(&self, key: &str, scope: Scope, value: &Value) -> Result<(), Refusal> {
        let Some(definition) = self.get(key) else {
            return Err(Refusal::UnknownKey {
                key: key.to_string(),
            });
        };
        if !definition.scopes.contains(&scope) {
            return Err(Refusal::WrongScope {
                key: key.to_string(),
                scope,
                allowed: definition.scopes,
            });
        }
        if !definition.kind.accepts(value) {
            return Err(Refusal::WrongShape {
                key: key.to_string(),
                wanted: definition.kind.describe(),
            });
        }
        Ok(())
    }
}

/// Why a write was refused.
///
/// A typed refusal rather than a string, because the daemon turns each of these into a
/// different protocol error and the UI shows a different control state for each.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("{key} is not a setting this version of Turn knows about")]
    UnknownKey { key: String },
    #[error("{key} cannot be set at the {} level", scope.label())]
    WrongScope {
        key: String,
        scope: Scope,
        allowed: &'static [Scope],
    },
    #[error("{key} must be {wanted}")]
    WrongShape { key: String, wanted: String },
}
