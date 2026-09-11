mod db;
mod task;

pub use db::Db;
pub use task::Task;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),
    /// The referenced task does not exist. Surfaced instead of silently
    /// updating zero rows so callers holding a stale id notice the bug.
    #[error("task {0} not found")]
    TaskNotFound(i64),
}
