use std::path::Path;

use rusqlite::Connection;

use crate::{Error, Task};

const MIGRATION_V1: &str = "
BEGIN;
CREATE TABLE tasks (
  id            INTEGER PRIMARY KEY,
  parent_id     INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
  display_order INTEGER NOT NULL,
  title         TEXT    NOT NULL,
  status        TEXT    NOT NULL,
  due           TEXT,
  log           TEXT    NOT NULL DEFAULT '',
  created_at    TEXT    NOT NULL,
  updated_at    TEXT    NOT NULL
);
CREATE INDEX idx_tasks_parent ON tasks(parent_id, display_order);

CREATE TABLE tags (
  task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  tag     TEXT    NOT NULL,
  PRIMARY KEY (task_id, tag)
);
PRAGMA user_version = 1;
COMMIT;
";

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, Error> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, Error> {
        // FK enforcement is per-connection opt-in in SQLite; the bundled build
        // happens to default it on, but we must not depend on that.
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn create_task(&self, parent_id: Option<i64>, title: &str) -> Result<Task, Error> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        // display_order is a dense 0..n sequence per sibling group, so the
        // tail position equals the current sibling count.
        let display_order: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE parent_id IS ?1",
            [parent_id],
            |row| row.get(0),
        )?;
        // Default status is hardcoded until user-defined statuses become configurable.
        self.conn.execute(
            "INSERT INTO tasks (parent_id, display_order, title, status, log, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'todo', '', ?4, ?4)",
            rusqlite::params![parent_id, display_order, title, now],
        )?;
        Ok(Task {
            id: self.conn.last_insert_rowid(),
            parent_id,
            display_order,
            title: title.to_string(),
            status: "todo".to_string(),
            due: None,
            log: String::new(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn list_children(&self, parent_id: Option<i64>) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, display_order, title, status, due, log, created_at, updated_at
             FROM tasks WHERE parent_id IS ?1 ORDER BY display_order",
        )?;
        let tasks = stmt
            .query_map([parent_id], |row| {
                Ok(Task {
                    id: row.get(0)?,
                    parent_id: row.get(1)?,
                    display_order: row.get(2)?,
                    title: row.get(3)?,
                    status: row.get(4)?,
                    due: row.get(5)?,
                    log: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }
}

fn migrate(conn: &Connection) -> Result<(), Error> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        conn.execute_batch(MIGRATION_V1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that opening a fresh database applies migration v1.
    // Given: a brand-new in-memory database
    // When: Db::open_in_memory is called
    // Then: user_version is 1 and both the tasks and tags tables exist
    #[test]
    fn open_migrates_fresh_db_to_v1() {
        let db = Db::open_in_memory().unwrap();

        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);

        for table in ["tasks", "tags"] {
            let count: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table `{table}` should exist");
        }
    }

    // Tests that foreign key enforcement is enabled on the connection.
    // Given: an open database with no rows in tasks
    // When: a task is inserted with a parent_id that does not exist
    // Then: the insert fails with a foreign key violation
    #[test]
    fn insert_with_missing_parent_violates_fk() {
        let db = Db::open_in_memory().unwrap();

        let result = db.conn.execute(
            "INSERT INTO tasks (parent_id, display_order, title, status, created_at, updated_at)
             VALUES (999, 0, 't', 'todo', '', '')",
            [],
        );

        assert!(result.is_err(), "insert with missing parent should fail");
    }

    // Tests that root-level tasks are appended with sequential display orders.
    // Given: an empty database
    // When: three tasks are created under the root (parent_id = None)
    // Then: their display_order values are 0, 1, 2 in creation order
    #[test]
    fn create_task_appends_display_order_at_tail() {
        let db = Db::open_in_memory().unwrap();

        let a = db.create_task(None, "a").unwrap();
        let b = db.create_task(None, "b").unwrap();
        let c = db.create_task(None, "c").unwrap();

        assert_eq!(a.display_order, 0);
        assert_eq!(b.display_order, 1);
        assert_eq!(c.display_order, 2);
    }

    // Tests the default field values of a newly created task.
    // Given: an empty database
    // When: a task is created with only a title
    // Then: status is "todo", due is NULL, log is empty, and
    //       created_at/updated_at are equal RFC 3339 UTC timestamps
    #[test]
    fn create_task_sets_defaults() {
        let db = Db::open_in_memory().unwrap();

        let task = db.create_task(None, "t").unwrap();

        assert_eq!(task.status, "todo");
        assert_eq!(task.due, None);
        assert_eq!(task.log, "");
        assert_eq!(task.created_at, task.updated_at);
        let parsed = chrono::DateTime::parse_from_rfc3339(&task.created_at).unwrap();
        assert_eq!(
            parsed.offset().local_minus_utc(),
            0,
            "timestamp must be UTC"
        );
    }

    // Tests that list_children(None) returns root tasks ordered by display_order.
    // Given: three root tasks whose display orders are rearranged to c(0), a(1), b(2),
    //        plus one child task under "a"
    // When: list_children(None) is called
    // Then: it returns exactly [c, a, b] (display_order, no child tasks)
    #[test]
    fn list_children_returns_roots_in_display_order() {
        let db = Db::open_in_memory().unwrap();
        let a = db.create_task(None, "a").unwrap();
        let b = db.create_task(None, "b").unwrap();
        let c = db.create_task(None, "c").unwrap();
        db.create_task(Some(a.id), "child").unwrap();
        for (id, key) in [(c.id, 0), (a.id, 1), (b.id, 2)] {
            db.conn
                .execute(
                    "UPDATE tasks SET display_order = ?1 WHERE id = ?2",
                    rusqlite::params![key, id],
                )
                .unwrap();
        }

        let roots = db.list_children(None).unwrap();

        let titles: Vec<&str> = roots.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["c", "a", "b"]);
    }
}
