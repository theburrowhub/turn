//! Moving domain types in and out of text columns.
//!
//! Two shapes are used, and the choice is deliberate per column:
//!
//! * [`tag`] for enums with no payload, which land as a bare `awaiting_user`
//!   rather than a quoted JSON scalar. Those columns are filtered and grouped
//!   in SQL, and a bare word keeps queries and `sqlite3` dumps readable.
//! * [`json`] for anything with structure — a `Lifecycle::Exited { code }`, a
//!   layout tree, a policy. Turn always reads these whole, so decomposing them
//!   into columns would buy nothing and cost a migration every time the domain
//!   grows a field.

use crate::error::{Result, StoreError};
use serde::{de::DeserializeOwned, Serialize};

/// Encodes a value as JSON for a text column.
pub(crate) fn json<T: Serialize>(what: &'static str, value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|cause| StoreError::encode(what, cause))
}

/// Decodes JSON from a text column, naming the row if it fails.
pub(crate) fn from_json<T: DeserializeOwned>(what: &'static str, id: &str, raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|cause| StoreError::Decode {
        what,
        id: id.to_string(),
        cause,
    })
}

/// Same as [`from_json`], for a nullable column.
pub(crate) fn from_json_opt<T: DeserializeOwned>(
    what: &'static str,
    id: &str,
    raw: Option<String>,
) -> Result<Option<T>> {
    match raw {
        Some(text) => Ok(Some(from_json(what, id, &text)?)),
        None => Ok(None),
    }
}

/// Encodes a payload-free enum as its bare serde name.
pub(crate) fn tag<T: Serialize>(what: &'static str, value: &T) -> Result<String> {
    let encoded = serde_json::to_string(value).map_err(|cause| StoreError::encode(what, cause))?;
    Ok(encoded.trim_matches('"').to_string())
}

/// Decodes a bare serde name back into its enum.
pub(crate) fn from_tag<T: DeserializeOwned>(what: &'static str, id: &str, raw: &str) -> Result<T> {
    // The value was written by [`tag`], so quoting it is enough to make it JSON
    // again. A hand-edited column containing a quote fails the decode below
    // rather than being silently accepted.
    let quoted = format!("\"{raw}\"");
    serde_json::from_str(&quoted).map_err(|cause| StoreError::Decode {
        what,
        id: id.to_string(),
        cause,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::state::{AwaitingReason, Lifecycle};
    use turn_core::Confidence;

    #[test]
    fn payload_free_enums_are_stored_as_bare_greppable_words() {
        assert_eq!(
            tag("confidence", &Confidence::InferredHigh).unwrap(),
            "inferred_high"
        );
        assert_eq!(
            tag("reason", &AwaitingReason::Permission).unwrap(),
            "permission"
        );
    }

    #[test]
    fn bare_words_decode_back_into_the_original_enum() {
        let value: Confidence = from_tag("confidence", "evt_1", "explicit").unwrap();
        assert_eq!(value, Confidence::Explicit);
    }

    #[test]
    fn an_unrecognised_stored_word_is_a_decode_error_naming_the_row() {
        let failure = from_tag::<Confidence>("confidence", "evt_42", "extremely_sure");
        let error = failure.expect_err("an unknown variant must not be accepted");
        let rendered = error.to_string();
        assert!(rendered.contains("evt_42"), "got {rendered}");
        assert!(rendered.contains("confidence"), "got {rendered}");
    }

    #[test]
    fn structured_enums_keep_their_payload_through_json() {
        let stored = json("lifecycle", &Lifecycle::Exited { code: 3 }).unwrap();
        let back: Lifecycle = from_json("lifecycle", "proc_1", &stored).unwrap();
        assert_eq!(back, Lifecycle::Exited { code: 3 });
    }

    #[test]
    fn a_null_column_decodes_to_none_rather_than_failing() {
        let none = from_json_opt::<Lifecycle>("lifecycle", "proc_1", None).unwrap();
        assert!(none.is_none());
    }
}
