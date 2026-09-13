mod db;
mod query;
mod status;
mod task;

pub use db::{Db, UndoOutcome};
pub use query::{Filter, Query, Sort};
pub use status::{Status, StatusKind, StatusMove};
pub use task::{Task, TaskMove};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),
    /// The referenced task does not exist. Surfaced instead of silently
    /// updating zero rows so callers holding a stale id notice the bug.
    #[error("task {0} not found")]
    TaskNotFound(i64),
    /// Re-parenting a task under itself or one of its descendants would
    /// disconnect the subtree into a cycle unreachable from any root.
    #[error(
        "cannot move task {task} under {new_parent}: it is the task itself or one of its descendants"
    )]
    CycleDetected { task: i64, new_parent: i64 },
    /// Due dates must be real calendar days written exactly as YYYY-MM-DD.
    /// The canonical form is enforced on write so stored dates compare
    /// correctly as plain strings (ordering, overdue checks).
    #[error("invalid date `{0}` (expected YYYY-MM-DD)")]
    InvalidDate(String),
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
    /// Undo scopes do not nest. Opening a second one would silently make the
    /// matching end fold edits from the wrong boundary, so it fails loudly
    /// at the call that broke the pairing.
    #[error("an undo scope is already active")]
    UndoScopeAlreadyActive,
    /// Closing an undo scope requires one to be open; same rationale as
    /// UndoScopeAlreadyActive.
    #[error("no undo scope is active")]
    UndoScopeNotActive,
}
