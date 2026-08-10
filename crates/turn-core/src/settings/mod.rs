//! Settings, and the five places a value can come from.
//!
//! One preference is asked for in one place and answered from up to five: a Global default, a
//! Workspace's opinion, the Template a Session was made from, the Session itself, and a
//! temporary override that lasts as long as the window is open. This module is the resolution
//! and nothing else — no storage, no UI, no daemon. Those consume [`Resolution`].
//!
//! ## The three things that are load-bearing
//!
//! **1. Setting a level never touches another level.** [`Layer`]s are separate maps, and
//! resolution reads them without merging them. This is the criterion that is easy to get
//! wrong by storing the resolved value: a Workspace that saved "font size 14" as the
//! resolved answer would silently swallow the Session's 18, and the user would discover it
//! the next time they changed the Workspace. Here, changing a Workspace changes exactly one
//! entry in exactly one map, and a Session override that was there before is still there
//! afterwards.
//!
//! **2. A value knows where it came from.** [`Resolution::origin`] is the level that won and
//! [`Resolution::shadowed`] is every level that was overridden, in precedence order. Both are
//! needed by the surface that offers "reset to inherited": without the first it cannot say
//! whether this value is the user's or a default, and without the second it cannot say what
//! resetting would reveal.
//!
//! **3. Unset and set-to-null are different.** A key absent from a layer does not
//! participate; a key present with `null` is a deliberate "no value here", which is how a
//! Session says "no init commands" over a Workspace that has some. [`Layer::clear`] removes
//! the entry — that is the reset — and [`Layer::set`] with `Value::Null` keeps it.
//!
//! ## Secrets
//!
//! A key whose [`Catalogue`] entry is marked [`Sensitivity::Secret`] never appears in a
//! [`Resolution`]'s debug output, an export, or anything else built from
//! [`Resolution::for_display`]. The value is still resolved and still usable — the daemon
//! needs it — but every path that produces text for a human or a file goes through the
//! display form, which replaces it. Redaction at the point of *rendering* rather than of
//! storage, because a secret that was redacted on the way in is a secret the daemon cannot
//! use.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

mod catalogue;
pub use catalogue::{Area, Catalogue, Definition, Refusal, Sensitivity, ValueKind};

#[cfg(test)]
mod tests;

/// Where a value was set, in precedence order: later beats earlier.
///
/// The order is the product decision and the reason this is an enum with an explicit rank
/// rather than a `Vec` position. `Template` sits between `Workspace` and `Session` because a
/// Template is a considered choice about a *kind* of task — narrower than the project, wider
/// than one run of it — and a Session made from it must be able to disagree.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Turn's own default. Always present, never editable away — resetting the Global level
    /// of a key returns it to the value compiled in, which is why this scope and the built-in
    /// default are distinguishable.
    ///
    /// The default variant, because a `Layer` deserialised without one is the widest possible
    /// claim rather than the narrowest: a level whose scope was lost must not silently
    /// outrank the levels below it.
    #[default]
    Global,
    Workspace,
    Template,
    Session,
    /// This window, until it closes. Never persisted: a temporary override is how a user
    /// tries a bigger font for ten minutes without deciding anything.
    Temporary,
}

impl Scope {
    /// Every scope, weakest first. The iteration order resolution depends on.
    pub const ALL: [Scope; 5] = [
        Scope::Global,
        Scope::Workspace,
        Scope::Template,
        Scope::Session,
        Scope::Temporary,
    ];

    /// What the user reads. Sentence case, because it appears mid-sentence in "inherited
    /// from the Workspace".
    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Workspace => "Workspace",
            Scope::Template => "Template",
            Scope::Session => "Session",
            Scope::Temporary => "this window",
        }
    }

    /// Whether a value at this level outlives the process that set it.
    ///
    /// Asked by the daemon to decide what to write, and by the UI to decide whether to warn
    /// that a change will be lost. `Temporary` is the only one that answers false.
    pub fn is_persistent(self) -> bool {
        self != Scope::Temporary
    }
}

/// One level's own opinions. Absent keys do not participate in resolution.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub scope: Scope,
    values: BTreeMap<String, Value>,
}

impl Layer {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            values: BTreeMap::new(),
        }
    }

    /// Builds a layer from stored pairs. Used by the store, which holds JSON it did not
    /// interpret.
    pub fn from_pairs(scope: Scope, pairs: impl IntoIterator<Item = (String, Value)>) -> Self {
        Self {
            scope,
            values: pairs.into_iter().collect(),
        }
    }

    /// Records a value at this level. `Value::Null` is a value — "nothing, deliberately" —
    /// and is not the same as [`Self::clear`].
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.values.insert(key.into(), value);
    }

    /// Removes this level's opinion, so the level below is seen again. This is "reset to
    /// inherited", and it is a removal rather than a write of the inherited value — writing
    /// it would freeze today's inherited answer as tomorrow's override.
    pub fn clear(&mut self, key: &str) -> bool {
        self.values.remove(key).is_some()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn pairs(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.values.iter().map(|(key, value)| (key.as_str(), value))
    }
}

/// One key's answer, and the whole story behind it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub key: String,
    /// The value in force.
    pub value: Value,
    /// The level that set it, or `None` when nothing did and this is the built-in default.
    ///
    /// Distinguished from `Some(Scope::Global)` on purpose: "Turn's default" and "somebody
    /// set this globally to the same thing" look identical in the value and are different
    /// questions when deciding whether there is anything to reset.
    pub origin: Option<Scope>,
    /// The levels this value overrode, weakest first. Empty when nothing was shadowed.
    ///
    /// What makes "reset to inherited" honest: the surface can say what would come back.
    pub shadowed: Vec<Shadowed>,
    /// Whether this key's value is a secret, carried with the resolution so a caller cannot
    /// render one without knowing.
    pub sensitivity: Sensitivity,
}

/// A value that lost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shadowed {
    pub scope: Scope,
    pub value: Value,
}

impl Resolution {
    /// Whether any level has an opinion about this key.
    ///
    /// False means the answer is the built-in default and there is nothing to reset anywhere.
    pub fn is_overridden(&self) -> bool {
        self.origin.is_some()
    }

    /// What the level below the winner would give if the winner were reset.
    ///
    /// `None` when nothing is below it, in which case resetting returns the built-in default.
    pub fn inherited(&self) -> Option<&Shadowed> {
        self.shadowed.last()
    }

    /// The same resolution with any secret replaced.
    ///
    /// Every path that turns settings into text a human or a file will see goes through
    /// this: a log line, an export, a support bundle, a screenshot of the settings sheet.
    /// The replacement is a fixed string rather than a length-preserving mask, because the
    /// length of a token is itself worth knowing and not worth leaking.
    /// Whether this value must not be rendered.
    ///
    /// `Unknown` counts. A key this build does not define may be a token a newer one wrote,
    /// and the failure of guessing wrong is a secret in a log file — so the unknown case
    /// resolves to the cautious answer rather than the convenient one.
    pub fn must_be_hidden(&self) -> bool {
        matches!(self.sensitivity, Sensitivity::Secret | Sensitivity::Unknown)
    }

    pub fn for_display(&self) -> Resolution {
        if !self.must_be_hidden() {
            return self.clone();
        }
        Resolution {
            key: self.key.clone(),
            value: Value::String(REDACTED.to_string()),
            origin: self.origin,
            shadowed: self
                .shadowed
                .iter()
                .map(|shadowed| Shadowed {
                    scope: shadowed.scope,
                    value: Value::String(REDACTED.to_string()),
                })
                .collect(),
            sensitivity: self.sensitivity,
        }
    }
}

/// What a secret reads as once it is fit to be seen.
pub const REDACTED: &str = "<redacted>";

/// The levels in force for one Session, and the catalogue that gives them meaning.
///
/// Assembled per question rather than held as one global object: a second Session in another
/// Workspace has a different Workspace layer and the same Global one, and a resolver that
/// cached the answer would be a resolver that has to be invalidated.
#[derive(Debug, Clone)]
pub struct Settings<'a> {
    catalogue: &'a Catalogue,
    layers: Vec<&'a Layer>,
}

impl<'a> Settings<'a> {
    /// Takes the layers in any order and sorts them into precedence order.
    ///
    /// Sorted here rather than trusted from the caller: the caller assembles them from four
    /// different places — a settings table, a Workspace row, a Template, a window's own
    /// state — and a resolution that depended on the order they happened to be pushed in is
    /// a bug that would only show up for whichever level was listed last.
    pub fn new(catalogue: &'a Catalogue, layers: impl IntoIterator<Item = &'a Layer>) -> Self {
        let mut layers: Vec<&Layer> = layers.into_iter().collect();
        layers.sort_by_key(|layer| layer.scope);
        Self { catalogue, layers }
    }

    /// Resolves one key.
    ///
    /// A key the catalogue does not define still resolves — the value is whatever the layers
    /// say, with no default and no type — because a build that dropped a key must not throw
    /// away a value a newer build wrote. The caller sees `sensitivity` of
    /// [`Sensitivity::Unknown`] and, if it is going to display it, must treat it as a secret.
    pub fn resolve(&self, key: &str) -> Resolution {
        let definition = self.catalogue.get(key);
        let mut winner: Option<(Scope, &Value)> = None;
        let mut shadowed: Vec<Shadowed> = Vec::new();
        for layer in &self.layers {
            if let Some(value) = layer.get(key) {
                if let Some((scope, previous)) = winner.replace((layer.scope, value)) {
                    shadowed.push(Shadowed {
                        scope,
                        value: previous.clone(),
                    });
                }
            }
        }
        let (origin, value) = match winner {
            Some((scope, value)) => (Some(scope), value.clone()),
            None => (
                None,
                definition
                    .map(|definition| definition.default.clone())
                    .unwrap_or(Value::Null),
            ),
        };
        Resolution {
            key: key.to_string(),
            value,
            origin,
            shadowed,
            sensitivity: definition
                .map(|definition| definition.sensitivity)
                .unwrap_or(Sensitivity::Unknown),
        }
    }

    /// Resolves every key the catalogue defines, plus any a layer holds that it does not.
    ///
    /// The second half is what stops a downgrade from looking like a data loss: a value
    /// written by a newer build appears, unknown and unexplained, rather than vanishing from
    /// the surface that would let the user delete it.
    pub fn resolve_all(&self) -> Vec<Resolution> {
        let mut keys: Vec<&str> = self.catalogue.keys().collect();
        for layer in &self.layers {
            for key in layer.keys() {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        keys.sort_unstable();
        keys.into_iter().map(|key| self.resolve(key)).collect()
    }

    /// Resolves every key in one area, for a settings surface built a section at a time.
    pub fn resolve_area(&self, area: Area) -> Vec<Resolution> {
        self.catalogue
            .in_area(area)
            .map(|definition| self.resolve(definition.key))
            .collect()
    }
}
