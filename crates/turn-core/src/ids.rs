//! Typed identifiers.
//!
//! Every entity gets its own newtype so that a `SessionId` can never be passed
//! where a `PaneId` is expected. IDs are prefixed strings (`sess_ab12cd34`) so
//! they stay readable in logs, event payloads and the SQLite store.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! typed_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Mints a fresh random id.
            pub fn new() -> Self {
                let raw = uuid::Uuid::new_v4().simple().to_string();
                Self(format!("{}_{}", $prefix, &raw[..12]))
            }

            /// Rebuilds an id from storage. No validation: the store is trusted.
            pub fn from_stored(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub const PREFIX: &'static str = $prefix;
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }
    };
}

typed_id!(WorkspaceId, "ws");
typed_id!(SessionId, "sess");
typed_id!(NodeId, "proc");
typed_id!(PaneId, "pane");
typed_id!(TemplateId, "tpl");
typed_id!(EventId, "evt");
typed_id!(AttentionId, "attn");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_unique() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert!(a.as_str().starts_with("sess_"), "got {a}");
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), "sess_".len() + 12);
    }

    #[test]
    fn ids_round_trip_through_serde_as_plain_strings() {
        let id = NodeId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id));
        let back: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn from_stored_preserves_the_original_value() {
        let id = WorkspaceId::from_stored("ws_deadbeef1234");
        assert_eq!(id.as_str(), "ws_deadbeef1234");
    }
}
