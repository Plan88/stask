mod db;
mod task;

pub use db::Db;
pub use task::Task;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),
}
