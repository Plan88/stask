use std::path::Path;

use rusqlite::Connection;

use crate::{Error, Status, StatusKind, StatusMove, Task, TaskMove};

const MIGRATION_V1: &str = "
BEGIN;
CREATE TABLE statuses (
  id            INTEGER PRIMARY KEY,
  label         TEXT    NOT NULL,
  kind          TEXT    NOT NULL,
  color         TEXT    NOT NULL,
  key           TEXT    NOT NULL,
  display_order INTEGER NOT NULL,
  is_default    INTEGER NOT NULL DEFAULT 0
);

INSERT INTO statuses (label, kind, color, key, display_order, is_default) VALUES
  ('未着手',   'open',      'gray',      't', 0, 1),
  ('着手可能', 'open',      'cyan',      'r', 1, 0),
  ('進行中',   'open',      'yellow',    'd', 2, 0),
  ('完了',     'done',      'green',     'x', 3, 0),
  ('破棄',     'cancelled', 'dark_gray', 'c', 4, 0);

CREATE TABLE tasks (
  id            INTEGER PRIMARY KEY,
  parent_id     INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
  display_order INTEGER NOT NULL,
  status_id     INTEGER NOT NULL REFERENCES statuses(id),
  title         TEXT    NOT NULL,
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

    /// Creates a task under `parent_id`. When `after` is given, the new task
    /// is inserted right after that display_order; otherwise it is appended.
    /// `status_id` must reference a row in `statuses` (FK-enforced).
    pub fn create_task(
        &self,
        parent_id: Option<i64>,
        title: &str,
        after: Option<i64>,
        status_id: i64,
    ) -> Result<Task, Error> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        // Shift + insert must be atomic, or a failure in between would leave a
        // gap in the sibling ordering.
        let tx = self.conn.unchecked_transaction()?;
        let display_order = match after {
            Some(after) => {
                tx.execute(
                    "UPDATE tasks SET display_order = display_order + 1
                     WHERE parent_id IS ?1 AND display_order > ?2",
                    rusqlite::params![parent_id, after],
                )?;
                after + 1
            }
            // MAX + 1 instead of COUNT: subtree deletion leaves the sibling
            // orders untouched, so gaps exist and COUNT could collide with
            // an existing order.
            None => tx.query_row(
                "SELECT COALESCE(MAX(display_order) + 1, 0) FROM tasks WHERE parent_id IS ?1",
                [parent_id],
                |row| row.get(0),
            )?,
        };
        tx.execute(
            "INSERT INTO tasks (parent_id, display_order, title, status_id, log, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, '', ?5, ?5)",
            rusqlite::params![parent_id, display_order, title, status_id, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Task {
            id,
            parent_id,
            display_order,
            title: title.to_string(),
            status_id,
            due: None,
            log: String::new(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn rename_task(&self, id: i64, title: &str) -> Result<(), Error> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let changed = self.conn.execute(
            "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![title, now, id],
        )?;
        if changed == 0 {
            return Err(Error::TaskNotFound(id));
        }
        Ok(())
    }

    /// Sets the task's status. `status_id` must reference a row in
    /// `statuses` (FK-enforced).
    pub fn set_status(&self, id: i64, status_id: i64) -> Result<(), Error> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let changed = self.conn.execute(
            "UPDATE tasks SET status_id = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![status_id, now, id],
        )?;
        if changed == 0 {
            return Err(Error::TaskNotFound(id));
        }
        Ok(())
    }

    pub fn list_statuses(&self) -> Result<Vec<Status>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, kind, color, key, display_order, is_default
             FROM statuses ORDER BY display_order",
        )?;
        let statuses = stmt
            .query_map([], status_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(statuses)
    }

    /// Returns the id of the status applied to newly created tasks. The app
    /// keeps exactly one row flagged as default at all times.
    pub fn default_status_id(&self) -> Result<i64, Error> {
        let id =
            self.conn
                .query_row("SELECT id FROM statuses WHERE is_default = 1", [], |row| {
                    row.get(0)
                })?;
        Ok(id)
    }

    /// Creates a status appended at the end of the display order. New
    /// statuses are never the default; that flag only moves explicitly.
    pub fn create_status(
        &self,
        label: &str,
        kind: StatusKind,
        color: &str,
        key: char,
    ) -> Result<Status, Error> {
        // Reading the tail position and inserting must be atomic so two
        // writers cannot end up sharing a display_order.
        let tx = self.conn.unchecked_transaction()?;
        // MAX + 1 instead of COUNT: deletions leave gaps, so COUNT could
        // collide with an existing order.
        let display_order: i64 = tx.query_row(
            "SELECT COALESCE(MAX(display_order) + 1, 0) FROM statuses",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO statuses (label, kind, color, key, display_order, is_default)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![label, kind.as_str(), color, key.to_string(), display_order],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Status {
            id,
            label: label.to_string(),
            kind,
            color: color.to_string(),
            key,
            display_order,
            is_default: false,
        })
    }

    pub fn update_status_label(&self, id: i64, label: &str) -> Result<(), Error> {
        self.update_status_column(id, "label", &label)
    }

    pub fn update_status_kind(&self, id: i64, kind: StatusKind) -> Result<(), Error> {
        self.update_status_column(id, "kind", &kind.as_str())
    }

    pub fn update_status_color(&self, id: i64, color: &str) -> Result<(), Error> {
        self.update_status_column(id, "color", &color)
    }

    pub fn update_status_key(&self, id: i64, key: char) -> Result<(), Error> {
        self.update_status_column(id, "key", &key.to_string())
    }

    fn update_status_column(
        &self,
        id: i64,
        column: &str,
        value: &dyn rusqlite::ToSql,
    ) -> Result<(), Error> {
        // `column` only ever comes from the fixed set above, never from
        // user input, so interpolating it is safe.
        let changed = self.conn.execute(
            &format!("UPDATE statuses SET {column} = ?1 WHERE id = ?2"),
            rusqlite::params![value, id],
        )?;
        if changed == 0 {
            return Err(Error::StatusNotFound(id));
        }
        Ok(())
    }

    pub fn count_tasks_with_status(&self, id: i64) -> Result<i64, Error> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Deletes a status unless tasks still reference it, it is the last row,
    /// or it is the current default. Each refusal carries its reason so the
    /// UI can tell the user what to change first.
    pub fn delete_status(&self, id: i64) -> Result<(), Error> {
        // Guard checks and the delete must see one consistent snapshot.
        let tx = self.conn.unchecked_transaction()?;
        let is_default: bool = tx
            .query_row(
                "SELECT is_default FROM statuses WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| status_not_found(e, id))?;
        let in_use: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        if in_use > 0 {
            return Err(Error::StatusInUse { count: in_use });
        }
        let total: i64 = tx.query_row("SELECT COUNT(*) FROM statuses", [], |row| row.get(0))?;
        if total <= 1 {
            return Err(Error::CannotDeleteLastStatus);
        }
        if is_default {
            return Err(Error::CannotDeleteDefaultStatus);
        }
        tx.execute("DELETE FROM statuses WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Swaps the status with its display-order neighbour; a no-op at either
    /// end of the list.
    pub fn move_status(&self, id: i64, direction: StatusMove) -> Result<(), Error> {
        use rusqlite::OptionalExtension;
        // Both UPDATEs must land together or the orders would collide.
        let tx = self.conn.unchecked_transaction()?;
        let order: i64 = tx
            .query_row(
                "SELECT display_order FROM statuses WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| status_not_found(e, id))?;
        let neighbour_sql = match direction {
            StatusMove::Down => {
                "SELECT id, display_order FROM statuses
                 WHERE display_order > ?1 ORDER BY display_order ASC LIMIT 1"
            }
            StatusMove::Up => {
                "SELECT id, display_order FROM statuses
                 WHERE display_order < ?1 ORDER BY display_order DESC LIMIT 1"
            }
        };
        let neighbour: Option<(i64, i64)> = tx
            .query_row(neighbour_sql, [order], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        if let Some((neighbour_id, neighbour_order)) = neighbour {
            tx.execute(
                "UPDATE statuses SET display_order = ?1 WHERE id = ?2",
                rusqlite::params![neighbour_order, id],
            )?;
            tx.execute(
                "UPDATE statuses SET display_order = ?1 WHERE id = ?2",
                rusqlite::params![order, neighbour_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Moves the default flag to `id`. The old flag is only dropped in the
    /// same transaction that sets the new one, so exactly one default row
    /// survives any outcome.
    pub fn set_default_status(&self, id: i64) -> Result<(), Error> {
        let tx = self.conn.unchecked_transaction()?;
        // A blanket UPDATE with a nonexistent id would clear every flag.
        let exists: i64 =
            tx.query_row("SELECT COUNT(*) FROM statuses WHERE id = ?1", [id], |row| {
                row.get(0)
            })?;
        if exists == 0 {
            return Err(Error::StatusNotFound(id));
        }
        tx.execute("UPDATE statuses SET is_default = (id = ?1)", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Swaps the task with its display-order neighbour within the same
    /// sibling group; a no-op at either end of the group.
    pub fn move_task(&self, id: i64, direction: TaskMove) -> Result<(), Error> {
        use rusqlite::OptionalExtension;
        // Both UPDATEs must land together or the orders would collide.
        let tx = self.conn.unchecked_transaction()?;
        let (parent_id, order): (Option<i64>, i64) = tx
            .query_row(
                "SELECT parent_id, display_order FROM tasks WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| task_not_found(e, id))?;
        let neighbour_sql = match direction {
            TaskMove::Down => {
                "SELECT id, display_order FROM tasks
                 WHERE parent_id IS ?1 AND display_order > ?2
                 ORDER BY display_order ASC LIMIT 1"
            }
            TaskMove::Up => {
                "SELECT id, display_order FROM tasks
                 WHERE parent_id IS ?1 AND display_order < ?2
                 ORDER BY display_order DESC LIMIT 1"
            }
        };
        let neighbour: Option<(i64, i64)> = tx
            .query_row(neighbour_sql, rusqlite::params![parent_id, order], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        if let Some((neighbour_id, neighbour_order)) = neighbour {
            tx.execute(
                "UPDATE tasks SET display_order = ?1 WHERE id = ?2",
                rusqlite::params![neighbour_order, id],
            )?;
            tx.execute(
                "UPDATE tasks SET display_order = ?1 WHERE id = ?2",
                rusqlite::params![order, neighbour_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Counts the tasks in the subtree rooted at `id`, the root included.
    pub fn count_subtree(&self, id: i64) -> Result<i64, Error> {
        self.conn
            .query_row("SELECT id FROM tasks WHERE id = ?1", [id], |_| Ok(()))
            .map_err(|e| task_not_found(e, id))?;
        Ok(collect_subtree_ids(&self.conn, id)?.len() as i64)
    }

    /// Moves the task under `new_parent` (None = root level). Within the new
    /// sibling group it lands right after display_order `after`, or at the
    /// tail when `after` is None. The old sibling group is compacted.
    pub fn reparent(
        &self,
        id: i64,
        new_parent: Option<i64>,
        after: Option<i64>,
    ) -> Result<(), Error> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        // Compaction, shift and the move itself must be atomic or a failure
        // in between would corrupt the sibling orderings.
        let tx = self.conn.unchecked_transaction()?;
        let (old_parent, old_order): (Option<i64>, i64) = tx
            .query_row(
                "SELECT parent_id, display_order FROM tasks WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| task_not_found(e, id))?;
        // Attaching a task inside its own subtree would detach that subtree
        // into a cycle unreachable from any root, so refuse up front.
        if let Some(parent_id) = new_parent
            && (parent_id == id || is_in_subtree(&tx, id, parent_id)?)
        {
            return Err(Error::CycleDetected {
                task: id,
                new_parent: parent_id,
            });
        }
        tx.execute(
            "UPDATE tasks SET display_order = display_order - 1
             WHERE parent_id IS ?1 AND display_order > ?2",
            rusqlite::params![old_parent, old_order],
        )?;
        // The moving row is excluded below so its stale display_order can
        // neither be shifted nor counted; it is overwritten at the end.
        let new_order = match after {
            Some(after) => {
                tx.execute(
                    "UPDATE tasks SET display_order = display_order + 1
                     WHERE parent_id IS ?1 AND display_order > ?2 AND id != ?3",
                    rusqlite::params![new_parent, after, id],
                )?;
                after + 1
            }
            // MAX + 1 for the same reason as in create_task: deletions leave
            // gaps, so COUNT could collide with an existing order.
            None => tx.query_row(
                "SELECT COALESCE(MAX(display_order) + 1, 0) FROM tasks
                 WHERE parent_id IS ?1 AND id != ?2",
                rusqlite::params![new_parent, id],
                |row| row.get(0),
            )?,
        };
        tx.execute(
            "UPDATE tasks SET parent_id = ?1, display_order = ?2, updated_at = ?3 WHERE id = ?4",
            rusqlite::params![new_parent, new_order, now, id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes the subtree rooted at `id` and returns how many tasks were
    /// removed. Rows are deleted one by one, deepest first, and each task's
    /// tags are deleted explicitly: relying on ON DELETE CASCADE would tie
    /// correctness (and future trigger-based undo recording) to SQLite's
    /// recursive-trigger settings and their depth limit.
    pub fn delete_subtree(&self, id: i64) -> Result<i64, Error> {
        // Collection and deletion must see one consistent snapshot.
        let tx = self.conn.unchecked_transaction()?;
        tx.query_row("SELECT id FROM tasks WHERE id = ?1", [id], |_| Ok(()))
            .map_err(|e| task_not_found(e, id))?;
        let ids = collect_subtree_ids(&tx, id)?;
        let mut deleted = 0i64;
        // Preorder reversed puts every task before its ancestors, so no
        // DELETE ever triggers a cascade onto a still-pending row.
        for &task_id in ids.iter().rev() {
            tx.execute("DELETE FROM tags WHERE task_id = ?1", [task_id])?;
            deleted += tx.execute("DELETE FROM tasks WHERE id = ?1", [task_id])? as i64;
        }
        tx.commit()?;
        Ok(deleted)
    }

    pub fn list_children(&self, parent_id: Option<i64>) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, display_order, title, status_id, due, log, created_at, updated_at
             FROM tasks WHERE parent_id IS ?1 ORDER BY display_order",
        )?;
        let tasks = stmt
            .query_map([parent_id], task_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    /// Returns every task. Sibling groups are kept contiguous and in display
    /// order so callers can build tree views without further sorting.
    pub fn list_all(&self) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, display_order, title, status_id, due, log, created_at, updated_at
             FROM tasks ORDER BY parent_id, display_order",
        )?;
        let tasks = stmt
            .query_map([], task_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }
}

/// Turns the no-rows case of a status lookup into StatusNotFound while
/// passing every other sqlite failure through unchanged.
fn status_not_found(err: rusqlite::Error, id: i64) -> Error {
    match err {
        rusqlite::Error::QueryReturnedNoRows => Error::StatusNotFound(id),
        other => Error::Sqlite(other),
    }
}

/// Collects the ids of the subtree rooted at `id`, the root first and every
/// parent before its descendants (preorder). Walks with an explicit stack
/// because tree depth is unbounded and recursion would tie stack usage to
/// user data.
fn collect_subtree_ids(conn: &Connection, id: i64) -> Result<Vec<i64>, Error> {
    let mut stmt =
        conn.prepare("SELECT id FROM tasks WHERE parent_id = ?1 ORDER BY display_order DESC")?;
    let mut ids = Vec::new();
    let mut stack = vec![id];
    while let Some(current) = stack.pop() {
        ids.push(current);
        let children = stmt
            .query_map([current], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        stack.extend(children);
    }
    Ok(ids)
}

/// Whether `candidate` lies strictly inside the subtree rooted at `root`.
fn is_in_subtree(conn: &Connection, root: i64, candidate: i64) -> Result<bool, Error> {
    let mut stmt = conn.prepare("SELECT id FROM tasks WHERE parent_id = ?1")?;
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let children = stmt
            .query_map([current], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if children.contains(&candidate) {
            return Ok(true);
        }
        stack.extend(children);
    }
    Ok(false)
}

/// Turns the no-rows case of a task lookup into TaskNotFound while passing
/// every other sqlite failure through unchanged.
fn task_not_found(err: rusqlite::Error, id: i64) -> Error {
    match err {
        rusqlite::Error::QueryReturnedNoRows => Error::TaskNotFound(id),
        other => Error::Sqlite(other),
    }
}

/// Maps invalid stored text (kind, key) to a conversion error instead of
/// panicking; such rows can only appear through outside edits of the file.
fn column_error(index: usize, message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        message.to_string().into(),
    )
}

fn status_from_row(row: &rusqlite::Row<'_>) -> Result<Status, rusqlite::Error> {
    let kind_text: String = row.get(2)?;
    let kind = StatusKind::parse(&kind_text)
        .ok_or_else(|| column_error(2, &format!("unknown status kind `{kind_text}`")))?;
    let key_text: String = row.get(4)?;
    let key = key_text
        .chars()
        .next()
        .ok_or_else(|| column_error(4, "empty status key"))?;
    Ok(Status {
        id: row.get(0)?,
        label: row.get(1)?,
        kind,
        color: row.get(3)?,
        key,
        display_order: row.get(5)?,
        is_default: row.get(6)?,
    })
}

fn task_from_row(row: &rusqlite::Row<'_>) -> Result<Task, rusqlite::Error> {
    Ok(Task {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        display_order: row.get(2)?,
        title: row.get(3)?,
        status_id: row.get(4)?,
        due: row.get(5)?,
        log: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
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

    fn default_status(db: &Db) -> i64 {
        db.default_status_id().unwrap()
    }

    /// Picks a seeded status other than the default, for tests that must
    /// observe a status actually changing.
    fn non_default_status(db: &Db) -> i64 {
        db.list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| !s.is_default)
            .unwrap()
            .id
    }

    // Tests that creating a task with an unknown status id is rejected.
    // Given: a fresh database (seeded status ids do not include 999)
    // When: create_task is called with status_id = 999
    // Then: it fails with a foreign key violation instead of storing a task
    //       that references no status
    #[test]
    fn create_task_with_unknown_status_id_violates_fk() {
        let db = Db::open_in_memory().unwrap();

        let result = db.create_task(None, "t", None, 999);

        assert!(
            matches!(result, Err(Error::Sqlite(_))),
            "create with unknown status id should fail, got {result:?}"
        );
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "no task row must be left behind");
    }

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

    // Tests that a fresh database is seeded with the stock statuses.
    // Given: a brand-new in-memory database
    // When: Db::open_in_memory is called
    // Then: the statuses table holds exactly 5 seeded rows, of which exactly
    //       one is marked as the default
    #[test]
    fn open_seeds_statuses_with_single_default() {
        let db = Db::open_in_memory().unwrap();

        let total: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM statuses", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 5);

        let defaults: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM statuses WHERE is_default = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(defaults, 1);
    }

    // Tests that list_statuses returns the seeded rows in display order.
    // Given: a fresh database whose seeded display orders are rearranged so
    //        that row order and display order disagree
    // When: list_statuses is called
    // Then: statuses come back sorted by display_order with every field
    //       (label/kind/color/key/is_default) populated from the table
    #[test]
    fn list_statuses_returns_all_in_display_order() {
        let db = Db::open_in_memory().unwrap();
        // Move the last seeded status ("破棄") to the front.
        db.conn
            .execute(
                "UPDATE statuses SET display_order = -1 WHERE label = '破棄'",
                [],
            )
            .unwrap();

        let statuses = db.list_statuses().unwrap();

        assert_eq!(statuses.len(), 5);
        let labels: Vec<&str> = statuses.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["破棄", "未着手", "着手可能", "進行中", "完了"]);
        let first = &statuses[0];
        assert_eq!(first.kind, StatusKind::Cancelled);
        assert_eq!(first.color, "dark_gray");
        assert_eq!(first.key, 'c');
        assert!(!first.is_default);
        assert!(statuses[1].is_default, "未着手 is the seeded default");
    }

    // Tests that default_status_id resolves the is_default row.
    // Given: a fresh database, then the default flag moved to another row
    // When: default_status_id is called before and after the move
    // Then: it returns the id of whichever row currently has is_default = 1
    #[test]
    fn default_status_id_follows_the_default_flag() {
        let db = Db::open_in_memory().unwrap();
        let statuses = db.list_statuses().unwrap();
        let seeded_default = statuses.iter().find(|s| s.is_default).unwrap().id;
        assert_eq!(db.default_status_id().unwrap(), seeded_default);

        let other = statuses.iter().find(|s| !s.is_default).unwrap().id;
        db.conn
            .execute("UPDATE statuses SET is_default = (id = ?1)", [other])
            .unwrap();

        assert_eq!(db.default_status_id().unwrap(), other);
    }

    // Tests that foreign key enforcement is enabled on the connection.
    // Given: an open database with no rows in tasks
    // When: a task is inserted with a parent_id that does not exist
    // Then: the insert fails with a foreign key violation
    #[test]
    fn insert_with_missing_parent_violates_fk() {
        let db = Db::open_in_memory().unwrap();

        let result = db.conn.execute(
            "INSERT INTO tasks (parent_id, display_order, title, status_id, created_at, updated_at)
             VALUES (999, 0, 't', 1, '', '')",
            [],
        );

        assert!(result.is_err(), "insert with missing parent should fail");
    }

    // Tests that root-level tasks are appended with sequential display orders.
    // Given: an empty database
    // When: three tasks are created under the root (parent_id = None) with no
    //       insertion position (after = None)
    // Then: their display_order values are 0, 1, 2 in creation order
    #[test]
    fn create_task_appends_display_order_at_tail() {
        let db = Db::open_in_memory().unwrap();

        let a = db
            .create_task(None, "a", None, default_status(&db))
            .unwrap();
        let b = db
            .create_task(None, "b", None, default_status(&db))
            .unwrap();
        let c = db
            .create_task(None, "c", None, default_status(&db))
            .unwrap();

        assert_eq!(a.display_order, 0);
        assert_eq!(b.display_order, 1);
        assert_eq!(c.display_order, 2);
    }

    // Tests that a task can be inserted between existing siblings.
    // Given: three root tasks a(0), b(1), c(2)
    // When: a task is created with after = b's display_order
    // Then: the new task gets display_order b+1 (= 2), the following sibling
    //       c is shifted to 3, and the resulting order is a, b, new, c
    #[test]
    fn create_task_inserts_after_given_display_order() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "a", None, default_status(&db))
            .unwrap();
        let b = db
            .create_task(None, "b", None, default_status(&db))
            .unwrap();
        db.create_task(None, "c", None, default_status(&db))
            .unwrap();

        let new = db
            .create_task(None, "new", Some(b.display_order), default_status(&db))
            .unwrap();

        assert_eq!(new.display_order, b.display_order + 1);
        let roots = db.list_children(None).unwrap();
        let titles: Vec<&str> = roots.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["a", "b", "new", "c"]);
        let orders: Vec<i64> = roots.iter().map(|t| t.display_order).collect();
        assert_eq!(orders, [0, 1, 2, 3], "orders must stay dense after shift");
    }

    // Tests that inserting after a sibling only shifts that sibling group.
    // Given: root tasks r1(0), r2(1) and children of r1: x(0), y(1)
    // When: a task is created under r1 with after = x's display_order
    // Then: r1's children become x, new, y while root orders are untouched
    #[test]
    fn create_task_insert_shifts_only_same_sibling_group() {
        let db = Db::open_in_memory().unwrap();
        let r1 = db
            .create_task(None, "r1", None, default_status(&db))
            .unwrap();
        db.create_task(None, "r2", None, default_status(&db))
            .unwrap();
        let x = db
            .create_task(Some(r1.id), "x", None, default_status(&db))
            .unwrap();
        db.create_task(Some(r1.id), "y", None, default_status(&db))
            .unwrap();

        db.create_task(
            Some(r1.id),
            "new",
            Some(x.display_order),
            default_status(&db),
        )
        .unwrap();

        let children = db.list_children(Some(r1.id)).unwrap();
        let titles: Vec<&str> = children.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["x", "new", "y"]);
        let root_orders: Vec<i64> = db
            .list_children(None)
            .unwrap()
            .iter()
            .map(|t| t.display_order)
            .collect();
        assert_eq!(root_orders, [0, 1], "root siblings must not shift");
    }

    // Tests the field values of a newly created task.
    // Given: a fresh database and a non-default status picked from the seeds
    // When: a task is created with a title and that status id
    // Then: the caller-provided status id is stored and returned, due is
    //       NULL, log is empty, and created_at/updated_at are equal RFC 3339
    //       UTC timestamps
    #[test]
    fn create_task_stores_given_status_and_defaults() {
        let db = Db::open_in_memory().unwrap();
        let ready = non_default_status(&db);

        let task = db.create_task(None, "t", None, ready).unwrap();

        assert_eq!(task.status_id, ready);
        let stored: i64 = db
            .conn
            .query_row(
                "SELECT status_id FROM tasks WHERE id = ?1",
                [task.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, ready);
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

    // Tests that renaming updates the title and bumps updated_at only.
    // Given: a task whose created_at/updated_at are backdated to a fixed
    //        past timestamp (second-precision timestamps would otherwise be
    //        indistinguishable within one test run)
    // When: rename_task is called with a new title
    // Then: the stored title changes, updated_at moves off the old value,
    //       and created_at keeps the old value
    #[test]
    fn rename_task_updates_title_and_updated_at() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "old", None, default_status(&db))
            .unwrap();
        let past = "2000-01-01T00:00:00Z";
        db.conn
            .execute(
                "UPDATE tasks SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
                rusqlite::params![past, task.id],
            )
            .unwrap();

        db.rename_task(task.id, "new").unwrap();

        let renamed = db
            .list_children(None)
            .unwrap()
            .into_iter()
            .find(|t| t.id == task.id)
            .unwrap();
        assert_eq!(renamed.title, "new");
        assert_eq!(renamed.created_at, past);
        assert_ne!(renamed.updated_at, past);
    }

    // Tests that renaming a nonexistent task is reported as an error.
    // Given: an empty database
    // When: rename_task is called with an id that matches no row
    // Then: it fails with TaskNotFound instead of silently updating nothing,
    //       so callers holding a stale id notice immediately
    #[test]
    fn rename_task_with_unknown_id_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.rename_task(999, "new");

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
    }

    // Tests that set_status updates the status and bumps updated_at only.
    // Given: a task whose created_at/updated_at are backdated to a fixed
    //        past timestamp (second-precision timestamps would otherwise be
    //        indistinguishable within one test run)
    // When: set_status is called with a non-default seeded status id
    // Then: the stored status id becomes that id, updated_at moves off the
    //       old value, and created_at keeps the old value
    #[test]
    fn set_status_updates_status_and_updated_at() {
        let db = Db::open_in_memory().unwrap();
        let doing = non_default_status(&db);
        let task = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let past = "2000-01-01T00:00:00Z";
        db.conn
            .execute(
                "UPDATE tasks SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
                rusqlite::params![past, task.id],
            )
            .unwrap();

        db.set_status(task.id, doing).unwrap();

        let updated = db
            .list_children(None)
            .unwrap()
            .into_iter()
            .find(|t| t.id == task.id)
            .unwrap();
        assert_eq!(updated.status_id, doing);
        assert_eq!(updated.created_at, past);
        assert_ne!(updated.updated_at, past);
    }

    // Tests that setting the status of a nonexistent task is an error.
    // Given: an empty database
    // When: set_status is called with an id that matches no row
    // Then: it fails with TaskNotFound instead of silently updating nothing,
    //       so callers holding a stale id notice immediately
    #[test]
    fn set_status_with_unknown_id_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.set_status(999, 1);

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
    }

    // Tests that list_all returns every task across all depths.
    // Given: two root tasks, a child under the first root, and a grandchild
    //        under that child
    // When: list_all is called
    // Then: all four tasks are returned, ordered by (parent_id, display_order)
    //       so sibling groups come out contiguous and in display order
    #[test]
    fn list_all_returns_every_task() {
        let db = Db::open_in_memory().unwrap();
        let r1 = db
            .create_task(None, "r1", None, default_status(&db))
            .unwrap();
        db.create_task(None, "r2", None, default_status(&db))
            .unwrap();
        let child = db
            .create_task(Some(r1.id), "child", None, default_status(&db))
            .unwrap();
        db.create_task(Some(child.id), "grandchild", None, default_status(&db))
            .unwrap();

        let all = db.list_all().unwrap();

        let titles: Vec<&str> = all.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["r1", "r2", "child", "grandchild"]);
    }

    // Tests that list_children(None) returns root tasks ordered by display_order.
    // Given: three root tasks whose display orders are rearranged to c(0), a(1), b(2),
    //        plus one child task under "a"
    // When: list_children(None) is called
    // Then: it returns exactly [c, a, b] (display_order, no child tasks)
    #[test]
    fn list_children_returns_roots_in_display_order() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .create_task(None, "a", None, default_status(&db))
            .unwrap();
        let b = db
            .create_task(None, "b", None, default_status(&db))
            .unwrap();
        let c = db
            .create_task(None, "c", None, default_status(&db))
            .unwrap();
        db.create_task(Some(a.id), "child", None, default_status(&db))
            .unwrap();
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

    /// Creates a small tree for structure-editing tests:
    /// roots a(0), b(1), c(2); children of b: x(0), y(1); child of x: leaf.
    /// Returns (a, b, c, x, y, leaf).
    fn structure_fixture(db: &Db) -> (Task, Task, Task, Task, Task, Task) {
        let status = default_status(db);
        let a = db.create_task(None, "a", None, status).unwrap();
        let b = db.create_task(None, "b", None, status).unwrap();
        let c = db.create_task(None, "c", None, status).unwrap();
        let x = db.create_task(Some(b.id), "x", None, status).unwrap();
        let y = db.create_task(Some(b.id), "y", None, status).unwrap();
        let leaf = db.create_task(Some(x.id), "leaf", None, status).unwrap();
        (a, b, c, x, y, leaf)
    }

    fn titles_of(tasks: &[Task]) -> Vec<String> {
        tasks.iter().map(|t| t.title.clone()).collect()
    }

    fn orders_of(tasks: &[Task]) -> Vec<i64> {
        tasks.iter().map(|t| t.display_order).collect()
    }

    // Tests moving a task down within its sibling group.
    // Given: roots a(0), b(1), c(2) where b has children (which must not move)
    // When: a is moved down
    // Then: the root order becomes b, a, c with dense orders 0, 1, 2, and
    //       b's children keep their own display orders
    #[test]
    fn move_task_down_swaps_with_next_sibling() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, ..) = structure_fixture(&db);

        db.move_task(a.id, TaskMove::Down).unwrap();

        let roots = db.list_children(None).unwrap();
        assert_eq!(titles_of(&roots), ["b", "a", "c"]);
        assert_eq!(orders_of(&roots), [0, 1, 2]);
        let children = db.list_children(Some(b.id)).unwrap();
        assert_eq!(orders_of(&children), [0, 1], "child group must not shift");
    }

    // Tests moving a task up within its sibling group.
    // Given: children of b: x(0), y(1)
    // When: y is moved up
    // Then: the child order becomes y, x while the root group is untouched
    #[test]
    fn move_task_up_swaps_with_previous_sibling() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, _, _, y, _) = structure_fixture(&db);

        db.move_task(y.id, TaskMove::Up).unwrap();

        let children = db.list_children(Some(b.id)).unwrap();
        assert_eq!(titles_of(&children), ["y", "x"]);
        assert_eq!(titles_of(&db.list_children(None).unwrap()), ["a", "b", "c"]);
    }

    // Tests moving at the edges of a sibling group.
    // Given: roots a(0), b(1), c(2)
    // When: a is moved up and c is moved down
    // Then: both calls succeed as no-ops and the order is unchanged
    #[test]
    fn move_task_at_edges_is_noop() {
        let db = Db::open_in_memory().unwrap();
        let (a, _, c, ..) = structure_fixture(&db);

        db.move_task(a.id, TaskMove::Up).unwrap();
        db.move_task(c.id, TaskMove::Down).unwrap();

        let roots = db.list_children(None).unwrap();
        assert_eq!(titles_of(&roots), ["a", "b", "c"]);
        assert_eq!(orders_of(&roots), [0, 1, 2]);
    }

    // Tests that sibling movement never crosses into another group.
    // Given: root b's last child y, with root c following b at the root level
    // When: y is moved down (no next sibling within b)
    // Then: the call is a no-op; y does not swap with anything outside b
    #[test]
    fn move_task_does_not_cross_sibling_groups() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, _, _, y, _) = structure_fixture(&db);

        db.move_task(y.id, TaskMove::Down).unwrap();

        assert_eq!(
            titles_of(&db.list_children(Some(b.id)).unwrap()),
            ["x", "y"]
        );
        assert_eq!(titles_of(&db.list_children(None).unwrap()), ["a", "b", "c"]);
    }

    // Tests moving a nonexistent task.
    // Given: an id (999) that matches no task row
    // When: move_task is called
    // Then: it fails with TaskNotFound
    #[test]
    fn move_unknown_task_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.move_task(999, TaskMove::Down);

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
    }

    /// Builds a parent chain of `depth` tasks and returns (topmost, deepest).
    /// Deep enough that recursive traversal would risk the call stack, which
    /// is why the engine walks trees with an explicit stack.
    fn deep_chain(db: &Db, depth: usize) -> (Task, Task) {
        let status = default_status(db);
        let top = db.create_task(None, "level 0", None, status).unwrap();
        let mut current = top.clone();
        for level in 1..depth {
            current = db
                .create_task(Some(current.id), &format!("level {level}"), None, status)
                .unwrap();
        }
        (top, current)
    }

    // Tests counting a subtree of mixed shapes.
    // Given: roots a, b, c where b > {x > leaf, y}
    // When: count_subtree is called for a leaf, for x and for b
    // Then: it returns 1 (self only), 2 (x + leaf) and 4 (b, x, y, leaf);
    //       unrelated roots are never counted
    #[test]
    fn count_subtree_includes_self_and_all_descendants() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, _, x, ..) = structure_fixture(&db);

        assert_eq!(db.count_subtree(a.id).unwrap(), 1);
        assert_eq!(db.count_subtree(x.id).unwrap(), 2);
        assert_eq!(db.count_subtree(b.id).unwrap(), 4);
    }

    // Tests counting a nonexistent subtree.
    // Given: an id (999) that matches no task row
    // When: count_subtree is called
    // Then: it fails with TaskNotFound
    #[test]
    fn count_subtree_of_unknown_task_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.count_subtree(999);

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
    }

    // Tests re-parenting to the tail of another sibling group.
    // Given: roots a(0), b(1), c(2) where b has children x(0), y(1)
    // When: c is re-parented under b with after = None
    // Then: c becomes b's last child (order 2), the root group compacts to
    //       a(0), b(1), and c keeps its subtree-free fields intact
    #[test]
    fn reparent_appends_at_tail_and_compacts_old_group() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, c, ..) = structure_fixture(&db);

        db.reparent(c.id, Some(b.id), None).unwrap();

        let roots = db.list_children(None).unwrap();
        assert_eq!(titles_of(&roots), ["a", "b"]);
        assert_eq!(orders_of(&roots), [0, 1], "old group must be compacted");
        let children = db.list_children(Some(b.id)).unwrap();
        assert_eq!(titles_of(&children), ["x", "y", "c"]);
        assert_eq!(orders_of(&children), [0, 1, 2]);
    }

    // Tests re-parenting into the middle of another sibling group.
    // Given: roots a(0), b(1), c(2) where b has children x(0), y(1)
    // When: a is re-parented under b with after = x's display_order (0)
    // Then: b's children become x(0), a(1), y(2) and the root group
    //       compacts to b(0), c(1)
    #[test]
    fn reparent_inserts_after_given_order_and_shifts_new_group() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, _, x, ..) = structure_fixture(&db);

        db.reparent(a.id, Some(b.id), Some(x.display_order))
            .unwrap();

        let children = db.list_children(Some(b.id)).unwrap();
        assert_eq!(titles_of(&children), ["x", "a", "y"]);
        assert_eq!(orders_of(&children), [0, 1, 2]);
        let roots = db.list_children(None).unwrap();
        assert_eq!(titles_of(&roots), ["b", "c"]);
        assert_eq!(orders_of(&roots), [0, 1]);
    }

    // Tests re-parenting up to the root level (the outdent shape).
    // Given: roots a(0), b(1), c(2) where b has children x(0), y(1)
    // When: x is re-parented to the root level right after b (after = 1)
    // Then: the roots become a(0), b(1), x(2), c(3), x keeps its own child,
    //       and b's remaining child compacts to y(0)
    #[test]
    fn reparent_to_root_level_lands_after_old_parent() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, _, x, ..) = structure_fixture(&db);

        db.reparent(x.id, None, Some(b.display_order)).unwrap();

        let roots = db.list_children(None).unwrap();
        assert_eq!(titles_of(&roots), ["a", "b", "x", "c"]);
        assert_eq!(orders_of(&roots), [0, 1, 2, 3]);
        assert_eq!(titles_of(&db.list_children(Some(b.id)).unwrap()), ["y"]);
        assert_eq!(orders_of(&db.list_children(Some(b.id)).unwrap()), [0]);
        let x_children = db.list_children(Some(x.id)).unwrap();
        assert_eq!(titles_of(&x_children), ["leaf"], "subtree moves with x");
    }

    // Tests the self-cycle guard.
    // Given: any task b
    // When: b is re-parented under itself
    // Then: it fails with CycleDetected and nothing changes
    #[test]
    fn reparent_under_itself_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, ..) = structure_fixture(&db);

        let result = db.reparent(b.id, Some(b.id), None);

        assert!(matches!(
            result,
            Err(Error::CycleDetected { task, new_parent }) if task == b.id && new_parent == b.id
        ));
        assert_eq!(titles_of(&db.list_children(None).unwrap()), ["a", "b", "c"]);
    }

    // Tests the descendant-cycle guard.
    // Given: b > x > leaf
    // When: b is re-parented under its grandchild leaf
    // Then: it fails with CycleDetected and the tree is unchanged
    #[test]
    fn reparent_under_own_descendant_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, _, x, _, leaf) = structure_fixture(&db);

        let result = db.reparent(b.id, Some(leaf.id), None);

        assert!(matches!(result, Err(Error::CycleDetected { .. })));
        assert_eq!(titles_of(&db.list_children(None).unwrap()), ["a", "b", "c"]);
        assert_eq!(titles_of(&db.list_children(Some(x.id)).unwrap()), ["leaf"]);
    }

    // Tests the cycle guard on a chain deep enough to threaten a recursive
    // implementation's call stack.
    // Given: a 3000-level parent chain
    // When: the topmost task is re-parented under the deepest one
    // Then: it fails with CycleDetected without overflowing (the descendant
    //       check walks with an explicit stack)
    #[test]
    fn reparent_cycle_check_survives_deep_hierarchy() {
        let db = Db::open_in_memory().unwrap();
        let (top, deepest) = deep_chain(&db, 3000);

        let result = db.reparent(top.id, Some(deepest.id), None);

        assert!(matches!(result, Err(Error::CycleDetected { .. })));
    }

    // Tests that a deep subtree can itself be re-parented.
    // Given: a 3000-level chain and a separate root task
    // When: the chain's second level (a 2999-task subtree) moves under that
    //       root
    // Then: the move succeeds; the subtree is not part of the new parent's
    //       ancestry, so no cycle is reported
    #[test]
    fn reparent_deep_subtree_under_unrelated_task_succeeds() {
        let db = Db::open_in_memory().unwrap();
        let (top, _) = deep_chain(&db, 3000);
        let other = db
            .create_task(None, "other", None, default_status(&db))
            .unwrap();

        db.reparent(top.id, Some(other.id), None).unwrap();

        let children = db.list_children(Some(other.id)).unwrap();
        assert_eq!(titles_of(&children), ["level 0"]);
    }

    // Tests re-parenting a nonexistent task.
    // Given: an id (999) that matches no task row
    // When: reparent is called
    // Then: it fails with TaskNotFound
    #[test]
    fn reparent_unknown_task_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.reparent(999, None, None);

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
    }

    // Tests creating at the tail after a deletion left an order gap.
    // Given: roots a(0), b(1), c(2) with the middle sibling b deleted
    //        (deletion leaves the survivors' orders 0 and 2 untouched)
    // When: a task is created at the root tail (after = None)
    // Then: it comes last and no two roots share a display_order
    #[test]
    fn create_task_after_delete_still_lands_at_tail() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, ..) = structure_fixture(&db);
        db.delete_subtree(b.id).unwrap();

        let created = db
            .create_task(None, "new", None, default_status(&db))
            .unwrap();

        let roots = db.list_children(None).unwrap();
        assert_eq!(roots.last().unwrap().id, created.id);
        let mut orders = orders_of(&roots);
        orders.dedup();
        assert_eq!(orders.len(), roots.len(), "orders must be unique");
    }

    // Tests re-parenting to the tail of a group with an order gap.
    // Given: roots a(0), b(1), c(2) with the middle sibling b deleted
    //        (survivor orders 0 and 2), where b had child x with child leaf
    //        — recreated as root "extra" with child "child" for this test
    // When: the nested child is re-parented to the root tail (after = None)
    // Then: it comes last and no two roots share a display_order
    #[test]
    fn reparent_to_tail_after_delete_keeps_orders_unique() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let (_, b, ..) = structure_fixture(&db);
        db.delete_subtree(b.id).unwrap();
        let extra = db.create_task(None, "extra", None, status).unwrap();
        let child = db
            .create_task(Some(extra.id), "child", None, status)
            .unwrap();

        db.reparent(child.id, None, None).unwrap();

        let roots = db.list_children(None).unwrap();
        assert_eq!(roots.last().unwrap().id, child.id);
        let mut orders = orders_of(&roots);
        orders.dedup();
        assert_eq!(orders.len(), roots.len(), "orders must be unique");
    }

    // Tests deleting a 3-level subtree while its siblings survive.
    // Given: roots a(0), b(1), c(2) where b > {x > leaf, y}
    // When: delete_subtree is called on b
    // Then: it reports 4 deleted tasks (each row removed individually, not
    //       via cascade), only a and c remain, and their display orders are
    //       untouched (a keeps 0, c keeps 2)
    #[test]
    fn delete_subtree_removes_three_levels_and_keeps_siblings() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, c, ..) = structure_fixture(&db);

        let deleted = db.delete_subtree(b.id).unwrap();

        assert_eq!(deleted, 4);
        let remaining = db.list_all().unwrap();
        let ids: Vec<i64> = remaining.iter().map(|t| t.id).collect();
        assert_eq!(ids, [a.id, c.id]);
        assert_eq!(
            orders_of(&remaining),
            [0, 2],
            "sibling display orders must be left untouched"
        );
    }

    // Tests that a subtree's tags are removed together with its tasks.
    // Given: the subtree b > {x > leaf, y} where b and leaf carry tags, and
    //        an unrelated tagged root a
    // When: delete_subtree is called on b
    // Then: every tag of the deleted tasks is gone while a's tag survives
    #[test]
    fn delete_subtree_removes_tags_of_deleted_tasks_only() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, _, _, _, leaf) = structure_fixture(&db);
        for (task_id, tag) in [(a.id, "keep"), (b.id, "work"), (leaf.id, "deep")] {
            db.conn
                .execute(
                    "INSERT INTO tags (task_id, tag) VALUES (?1, ?2)",
                    rusqlite::params![task_id, tag],
                )
                .unwrap();
        }

        db.delete_subtree(b.id).unwrap();

        let tags: Vec<(i64, String)> = db
            .conn
            .prepare("SELECT task_id, tag FROM tags")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(tags, [(a.id, "keep".to_string())]);
    }

    // Tests deleting a single leaf.
    // Given: the leaf task under b > x
    // When: delete_subtree is called on it
    // Then: it reports 1 deleted task and its parent x survives
    #[test]
    fn delete_subtree_on_leaf_deletes_one() {
        let db = Db::open_in_memory().unwrap();
        let (_, _, _, x, _, leaf) = structure_fixture(&db);

        let deleted = db.delete_subtree(leaf.id).unwrap();

        assert_eq!(deleted, 1);
        assert!(db.list_all().unwrap().iter().any(|t| t.id == x.id));
    }

    // Tests deleting a nonexistent subtree.
    // Given: an id (999) that matches no task row
    // When: delete_subtree is called
    // Then: it fails with TaskNotFound and nothing is deleted
    #[test]
    fn delete_subtree_of_unknown_task_fails() {
        let db = Db::open_in_memory().unwrap();
        structure_fixture(&db);

        let result = db.delete_subtree(999);

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
        assert_eq!(db.list_all().unwrap().len(), 6);
    }

    // Tests deleting a chain deep enough to threaten a recursive
    // implementation's call stack.
    // Given: a 3000-level parent chain
    // When: delete_subtree is called on the topmost task
    // Then: all 3000 tasks are reported deleted and none remain (the walk
    //       and the deepest-first deletion use an explicit stack)
    #[test]
    fn delete_subtree_survives_deep_hierarchy() {
        let db = Db::open_in_memory().unwrap();
        let (top, _) = deep_chain(&db, 3000);

        let deleted = db.delete_subtree(top.id).unwrap();

        assert_eq!(deleted, 3000);
        assert!(db.list_all().unwrap().is_empty());
    }

    /// A seeded status that is neither the default nor referenced by any
    /// task, i.e. one that delete_status must accept.
    fn deletable_status(db: &Db) -> Status {
        db.list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| !s.is_default)
            .unwrap()
    }

    // Tests that a created status is appended at the end of the list.
    // Given: a fresh database with the 5 seeded statuses
    // When: create_status is called with label/kind/color/key
    // Then: the returned status carries those fields, is not the default,
    //       and list_statuses shows it as the last of now 6 rows
    #[test]
    fn create_status_appends_at_tail() {
        let db = Db::open_in_memory().unwrap();

        let created = db
            .create_status("レビュー待ち", StatusKind::Open, "magenta", 'w')
            .unwrap();

        assert_eq!(created.label, "レビュー待ち");
        assert_eq!(created.kind, StatusKind::Open);
        assert_eq!(created.color, "magenta");
        assert_eq!(created.key, 'w');
        assert!(!created.is_default);
        let statuses = db.list_statuses().unwrap();
        assert_eq!(statuses.len(), 6);
        assert_eq!(statuses.last().unwrap().id, created.id);
    }

    // Tests that creation still appends at the tail after a deletion left a
    // gap in the display orders.
    // Given: the seeds with one middle status deleted
    // When: a new status is created
    // Then: it comes last in list_statuses and no two rows share a
    //       display_order (orders must stay unique for reordering to work)
    #[test]
    fn create_status_after_delete_still_lands_at_tail() {
        let db = Db::open_in_memory().unwrap();
        db.delete_status(deletable_status(&db).id).unwrap();

        let created = db
            .create_status("new", StatusKind::Open, "gray", 'z')
            .unwrap();

        let statuses = db.list_statuses().unwrap();
        assert_eq!(statuses.last().unwrap().id, created.id);
        let mut orders: Vec<i64> = statuses.iter().map(|s| s.display_order).collect();
        orders.dedup();
        assert_eq!(orders.len(), statuses.len(), "orders must be unique");
    }

    // Tests that every per-field status update persists.
    // Given: a seeded non-default status
    // When: label, kind, color and key are each updated
    // Then: the reloaded status carries all four new values
    #[test]
    fn update_status_fields_persist() {
        let db = Db::open_in_memory().unwrap();
        let id = deletable_status(&db).id;

        db.update_status_label(id, "renamed").unwrap();
        db.update_status_kind(id, StatusKind::Done).unwrap();
        db.update_status_color(id, "blue").unwrap();
        db.update_status_key(id, 'z').unwrap();

        let status = db
            .list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap();
        assert_eq!(status.label, "renamed");
        assert_eq!(status.kind, StatusKind::Done);
        assert_eq!(status.color, "blue");
        assert_eq!(status.key, 'z');
    }

    // Tests that updating a nonexistent status is reported as an error.
    // Given: an id (999) that matches no status row
    // When: each update method is called with it
    // Then: every call fails with StatusNotFound instead of silently
    //       updating nothing
    #[test]
    fn update_status_with_unknown_id_fails() {
        let db = Db::open_in_memory().unwrap();

        let results = [
            db.update_status_label(999, "x"),
            db.update_status_kind(999, StatusKind::Done),
            db.update_status_color(999, "red"),
            db.update_status_key(999, 'x'),
        ];

        for result in results {
            assert!(matches!(result, Err(Error::StatusNotFound(999))));
        }
    }

    // Tests the per-status task count used by the delete guard.
    // Given: two tasks on the default status and one on another status
    // When: count_tasks_with_status is called for each of those and for an
    //       unused third status
    // Then: it returns 2, 1 and 0 respectively
    #[test]
    fn count_tasks_with_status_counts_only_that_status() {
        let db = Db::open_in_memory().unwrap();
        let default = default_status(&db);
        let other = deletable_status(&db).id;
        let unused = db
            .list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| !s.is_default && s.id != other)
            .unwrap()
            .id;
        db.create_task(None, "a", None, default).unwrap();
        db.create_task(None, "b", None, default).unwrap();
        db.create_task(None, "c", None, other).unwrap();

        assert_eq!(db.count_tasks_with_status(default).unwrap(), 2);
        assert_eq!(db.count_tasks_with_status(other).unwrap(), 1);
        assert_eq!(db.count_tasks_with_status(unused).unwrap(), 0);
    }

    // Tests deleting a status that nothing protects.
    // Given: a seeded non-default status with no tasks referencing it
    // When: delete_status is called
    // Then: the row is gone and the other 4 seeded rows remain
    #[test]
    fn delete_status_removes_unused_non_default_row() {
        let db = Db::open_in_memory().unwrap();
        let victim = deletable_status(&db);

        db.delete_status(victim.id).unwrap();

        let statuses = db.list_statuses().unwrap();
        assert_eq!(statuses.len(), 4);
        assert!(statuses.iter().all(|s| s.id != victim.id));
    }

    // Tests the in-use guard of delete_status.
    // Given: a non-default status referenced by two tasks
    // When: delete_status is called
    // Then: it fails with StatusInUse carrying the count 2, and the status
    //       row survives
    #[test]
    fn delete_status_in_use_is_rejected_with_count() {
        let db = Db::open_in_memory().unwrap();
        let status = deletable_status(&db);
        db.create_task(None, "a", None, status.id).unwrap();
        db.create_task(None, "b", None, status.id).unwrap();

        let result = db.delete_status(status.id);

        assert!(matches!(result, Err(Error::StatusInUse { count: 2 })));
        assert_eq!(db.list_statuses().unwrap().len(), 5);
    }

    // Tests the default-row guard of delete_status.
    // Given: the seeded default status, unused by any task
    // When: delete_status is called on it
    // Then: it fails with CannotDeleteDefaultStatus and the row survives
    #[test]
    fn delete_default_status_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        let default = default_status(&db);

        let result = db.delete_status(default);

        assert!(matches!(result, Err(Error::CannotDeleteDefaultStatus)));
        assert_eq!(db.default_status_id().unwrap(), default);
    }

    // Tests the last-row guard of delete_status.
    // Given: all statuses deleted down to a single remaining row
    // When: delete_status is called on that row
    // Then: it fails with CannotDeleteLastStatus (reported as "last", the
    //       more fundamental reason, even though the survivor is also the
    //       default) and the row survives
    #[test]
    fn delete_last_status_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        for status in db.list_statuses().unwrap() {
            if !status.is_default {
                db.delete_status(status.id).unwrap();
            }
        }
        let last = db.list_statuses().unwrap();
        assert_eq!(last.len(), 1);

        let result = db.delete_status(last[0].id);

        assert!(matches!(result, Err(Error::CannotDeleteLastStatus)));
        assert_eq!(db.list_statuses().unwrap().len(), 1);
    }

    // Tests deleting a nonexistent status.
    // Given: an id (999) that matches no status row
    // When: delete_status is called
    // Then: it fails with StatusNotFound
    #[test]
    fn delete_unknown_status_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.delete_status(999);

        assert!(matches!(result, Err(Error::StatusNotFound(999))));
    }

    // Tests moving a status one place down.
    // Given: the 5 seeded statuses in seed order
    // When: the first status is moved down
    // Then: it swaps places with the second; everything else stays put
    #[test]
    fn move_status_down_swaps_with_next() {
        let db = Db::open_in_memory().unwrap();
        let first = db.list_statuses().unwrap()[0].id;

        db.move_status(first, StatusMove::Down).unwrap();

        let labels: Vec<String> = db
            .list_statuses()
            .unwrap()
            .into_iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(labels, ["着手可能", "未着手", "進行中", "完了", "破棄"]);
    }

    // Tests moving a status one place up.
    // Given: the 5 seeded statuses in seed order
    // When: the last status is moved up
    // Then: it swaps places with the one before it
    #[test]
    fn move_status_up_swaps_with_previous() {
        let db = Db::open_in_memory().unwrap();
        let last = db.list_statuses().unwrap().last().unwrap().id;

        db.move_status(last, StatusMove::Up).unwrap();

        let labels: Vec<String> = db
            .list_statuses()
            .unwrap()
            .into_iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(labels, ["未着手", "着手可能", "進行中", "破棄", "完了"]);
    }

    // Tests reordering at the list boundaries.
    // Given: the 5 seeded statuses in seed order
    // When: the first is moved up and the last is moved down
    // Then: both calls succeed as no-ops and the order is unchanged
    #[test]
    fn move_status_at_edges_is_noop() {
        let db = Db::open_in_memory().unwrap();
        let statuses = db.list_statuses().unwrap();

        db.move_status(statuses[0].id, StatusMove::Up).unwrap();
        db.move_status(statuses.last().unwrap().id, StatusMove::Down)
            .unwrap();

        let ids: Vec<i64> = db.list_statuses().unwrap().iter().map(|s| s.id).collect();
        let expected: Vec<i64> = statuses.iter().map(|s| s.id).collect();
        assert_eq!(ids, expected);
    }

    // Tests moving a nonexistent status.
    // Given: an id (999) that matches no status row
    // When: move_status is called
    // Then: it fails with StatusNotFound
    #[test]
    fn move_unknown_status_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.move_status(999, StatusMove::Down);

        assert!(matches!(result, Err(Error::StatusNotFound(999))));
    }

    // Tests that changing the default status keeps the invariant of exactly
    // one default row.
    // Given: the seeded statuses with 未着手 as the default
    // When: set_default_status is called with another status id
    // Then: that status becomes the default, exactly one row carries the
    //       flag, and default_status_id agrees
    #[test]
    fn set_default_status_moves_flag_and_keeps_single_default() {
        let db = Db::open_in_memory().unwrap();
        let new_default = deletable_status(&db).id;

        db.set_default_status(new_default).unwrap();

        let statuses = db.list_statuses().unwrap();
        let defaults: Vec<i64> = statuses
            .iter()
            .filter(|s| s.is_default)
            .map(|s| s.id)
            .collect();
        assert_eq!(defaults, [new_default], "exactly one default row");
        assert_eq!(db.default_status_id().unwrap(), new_default);
    }

    // Tests that a failed default change leaves the old default intact.
    // Given: an id (999) that matches no status row
    // When: set_default_status is called with it
    // Then: it fails with StatusNotFound and the previous default row still
    //       carries the flag (the flag is never dropped without a successor)
    #[test]
    fn set_default_status_with_unknown_id_keeps_old_default() {
        let db = Db::open_in_memory().unwrap();
        let old_default = default_status(&db);

        let result = db.set_default_status(999);

        assert!(matches!(result, Err(Error::StatusNotFound(999))));
        assert_eq!(db.default_status_id().unwrap(), old_default);
    }
}
