mod db;
mod status;
mod task;

pub use db::Db;
pub use status::{Status, StatusKind, StatusMove};
pub use task::Task;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),
    /// The referenced task does not exist. Surfaced instead of silently
    /// updating zero rows so callers holding a stale id notice the bug.
    #[error("task {0} not found")]
    TaskNotFound(i64),
    /// The referenced status does not exist. Same rationale as TaskNotFound.
    #[error("status {0} not found")]
    StatusNotFound(i64),
    /// Deleting this status would orphan the tasks that reference it. The
    /// count lets the UI tell the user the size of the conflict.
    #[error("status is used by {count} task(s)")]
    StatusInUse { count: i64 },
    /// Tasks always need a status to be created with, so at least one status
    /// row must survive.
    #[error("the last remaining status cannot be deleted")]
    CannotDeleteLastStatus,
    /// Exactly one status must stay flagged as default; the user has to move
    /// the default elsewhere before deleting this row.
    #[error("the default status cannot be deleted")]
    CannotDeleteDefaultStatus,
}
