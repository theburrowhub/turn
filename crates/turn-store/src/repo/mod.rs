//! Repositories: one small, explicit API per entity.
//!
//! Each repository borrows the connection rather than owning it, so a caller can
//! hold several at once and still have every write land in the same database with
//! the same pragmas. None of them start their own threads or take locks; the
//! daemon calls them from a blocking context and controls the ordering itself.
//!
//! Writes are `INSERT ... ON CONFLICT DO UPDATE` rather than `INSERT OR REPLACE`.
//! `REPLACE` deletes the old row first, which for a row anything else references
//! would fire `ON DELETE CASCADE` and take a session's nodes, events and pending
//! attention with it — renaming a session must not erase its history.

pub mod attention;
pub mod event;
pub mod hierarchy;
pub mod node;
pub mod session;
pub mod settings;
pub mod template;
pub mod workspace;

pub use attention::AttentionRepo;
pub use event::{EventRepo, PruneOutcome, Retention};
pub use hierarchy::HierarchyRepo;
pub use node::NodeRepo;
pub use session::SessionRepo;
pub use settings::SettingsRepo;
pub use template::TemplateRepo;
pub use workspace::WorkspaceRepo;
