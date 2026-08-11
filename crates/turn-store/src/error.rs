//! The store's error type.
//!
//! Every failure is typed rather than an `anyhow` blob, because the daemon has
//! to react differently to each: a schema written by a newer build must stop the
//! daemon cold, a decode failure on one row should not take the whole workspace
//! list down, and a missing data directory is something the user can fix.

use std::path::Path;

/// Anything that can go wrong while persisting or loading Turn's state.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database on disk is newer than this build understands.
    ///
    /// Turn refuses to open it at all. Running an old binary against a new
    /// schema would either fail on unknown columns or, worse, silently write
    /// rows the newer build can no longer read.
    #[error(
        "this database was written by a newer version of Turn (schema v{found}, \
         this build understands v{supported}); refusing to open it"
    )]
    SchemaTooNew { found: i64, supported: i64 },

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A domain value could not be turned into JSON for a column.
    #[error("could not encode {what} for storage: {cause}")]
    Encode {
        what: &'static str,
        #[source]
        cause: serde_json::Error,
    },

    /// A column held something this build cannot parse. Carries the row id so a
    /// corrupt entity can be named in a log line instead of being a mystery.
    #[error("stored {what} for {id} could not be decoded: {cause}")]
    Decode {
        what: &'static str,
        id: String,
        #[source]
        cause: serde_json::Error,
    },

    /// A write referenced a parent row that is not in the store.
    ///
    /// Foreign keys are on, so this surfaces as a clean error instead of an
    /// orphaned event nobody will ever be able to attribute to a session.
    #[error("{what} refers to {missing}, which is not in the store")]
    UnknownReference { what: &'static str, missing: String },

    /// A session row exists but its layout row does not. Only reachable if the
    /// file was edited or truncated behind Turn's back; both are written in one
    /// transaction.
    #[error("session {id} has no stored layout; the database is inconsistent")]
    MissingLayout { id: String },

    /// Another unreconciled Session already owns the checkout. Structured fields
    /// let the daemon build a typed conflict response rather than parse this text.
    #[error("checkout {checkout_id} is held by session {owner_session_id} (lease {lease_id})")]
    WriteLeaseHeld {
        checkout_id: String,
        owner_session_id: String,
        lease_id: String,
    },

    /// A launch-time authority check found no active lease owned by the Session.
    /// This is distinct from a conflict: there may be no current owner at all,
    /// but process launch must never acquire ownership as a side effect.
    #[error(
        "session {session_id} does not hold an active write lease for checkout {checkout_id} in workspace {workspace_id}"
    )]
    WriteLeaseNotActive {
        workspace_id: String,
        session_id: String,
        checkout_id: String,
    },

    /// The checkout came from state written before Turn could prove exclusive
    /// ownership. Only an explicit reconciliation flow may clear this gate; a
    /// lease acquisition must never "repair" it as a side effect.
    #[error("workspace {workspace_id} checkout {checkout_id} requires write-lease reconciliation")]
    LeaseReconciliationRequired {
        workspace_id: String,
        checkout_id: String,
    },

    /// Workspace roots are filesystem identities, not caller-provided labels.
    /// A missing/unresolvable/non-directory root cannot safely participate in
    /// checkout fencing.
    #[error("could not resolve workspace root {path}: {cause}")]
    WorkspaceRoot {
        path: String,
        #[source]
        cause: std::io::Error,
    },

    /// Two Workspace records must not name the same checkout. Historical
    /// aliases remain visible for reconciliation, but no new alias is accepted.
    #[error(
        "workspace root {canonical_path} is already registered by workspace {existing_workspace_id}"
    )]
    WorkspaceRootAlias {
        canonical_path: String,
        existing_workspace_id: String,
    },

    /// The requested Session or Checkout does not belong to the named Workspace.
    /// Keeping all three ids makes the programming error diagnosable without
    /// weakening it into a generic missing-row response.
    #[error(
        "session {session_id} and checkout {checkout_id} are not both owned by workspace {workspace_id}"
    )]
    InvalidLeaseOwnership {
        workspace_id: String,
        session_id: String,
        checkout_id: String,
    },

    /// Archived navigation state cannot become new checkout authority. The
    /// daemon checks this before calling the store for a useful UI error, while
    /// the transaction repeats the check so another writer cannot archive an
    /// entity between the projection read and the fenced write.
    #[error("archived workspace {workspace_id} cannot create or acquire Session authority")]
    ArchivedWorkspace { workspace_id: String },

    /// An archived Session is deliberately outside normal navigation and may
    /// not be promoted into a hidden primary-checkout writer.
    #[error("archived session {session_id} cannot acquire checkout authority")]
    ArchivedSession { session_id: String },

    /// A specialised Session creation API received a shape that would make the
    /// persisted mode ambiguous or unsafe. The caller must fix the domain object;
    /// the store never silently converts a writer into another mode.
    #[error("cannot create session {session_id}: {reason}")]
    InvalidSessionCreation { session_id: String, reason: String },

    /// A worktree must already exist so its filesystem identity can be fenced
    /// against the primary checkout and every other registered checkout.
    #[error("could not resolve checkout path {path}: {cause}")]
    CheckoutPath {
        path: String,
        #[source]
        cause: std::io::Error,
    },

    /// The supplied checkout metadata disagrees with its workspace, Session, or
    /// canonical filesystem identity.
    #[error("checkout {checkout_id} is invalid: {reason}")]
    InvalidCheckout { checkout_id: String, reason: String },

    /// Isolated worktrees may not alias any existing checkout. Primary checkout
    /// aliases are supported, but an isolated writer must have its own directory.
    #[error("checkout path {canonical_path} is already registered as {existing_checkout_id}")]
    CheckoutPathConflict {
        canonical_path: String,
        existing_checkout_id: String,
    },

    /// A filesystem identity or other structural key cannot be rewritten without
    /// changing its meaning. If it contains something shaped like a credential,
    /// fail before SQLite sees it instead of storing the credential or inventing
    /// a redacted identity that aliases a different checkout.
    #[error("{what} for {owner_id} contains credential-shaped text; refusing to persist it")]
    SecretInStructuralField {
        what: &'static str,
        owner_id: String,
    },

    /// A one-shot security cleanup could not complete. Its durable marker stays
    /// in place, so the next open retries rather than pretending the old bytes
    /// are gone.
    #[error("legacy credential cleanup is incomplete and will retry on next open: {reason}")]
    SecurityMaintenanceIncomplete { reason: String },

    #[error("could not create the data directory {path}: {cause}")]
    DataDir {
        path: String,
        #[source]
        cause: std::io::Error,
    },

    /// A schema table was added without declaring how privacy export scopes it.
    /// Refusing is safer than producing an export that silently omits durable data.
    #[error("privacy inventory does not account for these SQLite tables: {tables:?}")]
    PrivacyInventoryIncomplete { tables: Vec<String> },

    #[error("could not write privacy export {path}: {cause}")]
    PrivacyExport {
        path: String,
        #[source]
        cause: std::io::Error,
    },

    /// No platform data directory could be resolved (a stripped container, a
    /// user with no home). The message names the escape hatch.
    #[error("no platform data directory could be resolved; set TURN_DATA_DIR")]
    NoDataDir,
}

impl StoreError {
    pub(crate) fn encode(what: &'static str, cause: serde_json::Error) -> Self {
        StoreError::Encode { what, cause }
    }

    pub(crate) fn data_dir(path: &Path, cause: std::io::Error) -> Self {
        StoreError::DataDir {
            path: path.display().to_string(),
            cause,
        }
    }

    /// Translates a SQLite foreign-key violation into a domain-level "the parent
    /// is not there", leaving every other SQLite failure untouched.
    pub(crate) fn from_write(
        what: &'static str,
        missing: impl Into<String>,
        error: rusqlite::Error,
    ) -> Self {
        if is_foreign_key_violation(&error) {
            StoreError::UnknownReference {
                what,
                missing: missing.into(),
            }
        } else {
            StoreError::Sqlite(error)
        }
    }
}

/// Whether a SQLite failure is specifically a foreign-key violation.
fn is_foreign_key_violation(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(err, _) => {
            err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
        }
        _ => false,
    }
}

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, StoreError>;
