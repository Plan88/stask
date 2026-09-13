use std::path::Path;

use rusqlite::Connection;

use crate::{Error, Filter, Query, Sort, Status, StatusKind, StatusMove, Task, TaskMove};

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
  note          TEXT    NOT NULL DEFAULT '',
  created_at    TEXT    NOT NULL,
  updated_at    TEXT    NOT NULL
);
CREATE INDEX idx_tasks_parent ON tasks(parent_id, display_order);
PRAGMA user_version = 1;
COMMIT;
";

/// One undoable user action: the closed, mutually disjoint ranges of undolog
/// entries it wrote, plus a human-readable description shown when it is
/// undone or redone. A plain action writes a single range; closing an undo
/// scope folds the session's surviving steps into one step carrying all of
/// their ranges.
struct UndoStep {
    description: String,
    ranges: Vec<(i64, i64)>,
}

/// What an undo or redo just did, so the UI can show the change: name it in
/// a message and bring the touched tasks into view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    /// Description of the original action, e.g. `delete "design the API"`.
    pub description: String,
    /// Ids of the task rows the replay touched (deduplicated). Some may no
    /// longer exist — undoing a create removes the task again.
    pub affected_task_ids: Vec<i64>,
}

/// An open undo sub-session (e.g. while a modal editor is showing). It only
/// scopes the in-memory history bookkeeping; the database never sees it, so
/// a scope abandoned by a crash cannot affect stored data.
struct UndoScope {
    /// undo_stack length when the scope opened; scoped undo never pops
    /// below this boundary.
    boundary: usize,
    /// The outer redo stack, stashed away so that redo inside the scope can
    /// only reach steps undone inside it.
    saved_redo: Vec<UndoStep>,
}

pub struct Db {
    conn: Connection,
    // RefCell because history bookkeeping is interior to mutation methods
    // that take &self; the connection is single-threaded anyway.
    undo_stack: std::cell::RefCell<Vec<UndoStep>>,
    redo_stack: std::cell::RefCell<Vec<UndoStep>>,
    undo_scope: std::cell::RefCell<Option<UndoScope>>,
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
        create_undo_log(&conn)?;
        Ok(Self {
            conn,
            undo_stack: std::cell::RefCell::new(Vec::new()),
            redo_stack: std::cell::RefCell::new(Vec::new()),
            undo_scope: std::cell::RefCell::new(None),
        })
    }

    /// Runs one user action as one transaction and, if it changed any rows,
    /// one undo step. A failure inside rolls the transaction back, which
    /// also discards the undolog entries it wrote (the TEMP log is
    /// transactional like any table), so no half-recorded step can exist.
    fn with_action<T>(
        &self,
        description: String,
        action: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let seq_before = self.max_undolog_seq()?;
        let tx = self.conn.unchecked_transaction()?;
        let value = action(&tx)?;
        tx.commit()?;
        let seq_after = self.max_undolog_seq()?;
        // An action that touched no rows (e.g. a move at the edge of its
        // group) is not recorded: undoing it would visibly do nothing,
        // which reads as a broken undo. It does not clear the redo stack
        // either, since no data changed.
        if seq_after > seq_before {
            self.undo_stack.borrow_mut().push(UndoStep {
                description,
                ranges: vec![(seq_before + 1, seq_after)],
            });
            self.redo_stack.borrow_mut().clear();
        }
        Ok(value)
    }

    fn max_undolog_seq(&self) -> Result<i64, Error> {
        let seq = self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM undolog", [], |row| {
                row.get(0)
            })?;
        Ok(seq)
    }

    /// The task's title, for building action descriptions.
    fn task_title(&self, id: i64) -> Result<String, Error> {
        self.conn
            .query_row("SELECT title FROM tasks WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .map_err(|e| task_not_found(e, id))
    }

    /// The status's label, for building action descriptions.
    fn status_label(&self, id: i64) -> Result<String, Error> {
        self.conn
            .query_row("SELECT label FROM statuses WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .map_err(|e| status_not_found(e, id))
    }

    /// Opens an undo sub-session: until the matching end_undo_scope, undo
    /// and redo only reach steps recorded inside the scope. Scopes do not
    /// nest.
    pub fn begin_undo_scope(&self) -> Result<(), Error> {
        let mut scope = self.undo_scope.borrow_mut();
        if scope.is_some() {
            return Err(Error::UndoScopeAlreadyActive);
        }
        *scope = Some(UndoScope {
            boundary: self.undo_stack.borrow().len(),
            saved_redo: std::mem::take(&mut *self.redo_stack.borrow_mut()),
        });
        Ok(())
    }

    /// Closes the current undo scope. The edits that survived it (were not
    /// undone) are folded into a single outer undo step described by
    /// `description`; if none survived, the history is left as if the scope
    /// had never been opened.
    pub fn end_undo_scope(&self, description: &str) -> Result<(), Error> {
        let Some(scope) = self.undo_scope.borrow_mut().take() else {
            return Err(Error::UndoScopeNotActive);
        };
        let mut undo_stack = self.undo_stack.borrow_mut();
        let surviving = undo_stack.split_off(scope.boundary);
        let mut redo_stack = self.redo_stack.borrow_mut();
        if surviving.is_empty() {
            // Everything the session did was undone again, so to the outer
            // history it never happened — the stashed redo is still valid.
            *redo_stack = scope.saved_redo;
        } else {
            // The session counts as one fresh edit: like any edit it
            // discards the outer redo branch, and the in-scope redo
            // counters must not leak outside their scope.
            redo_stack.clear();
            undo_stack.push(UndoStep {
                description: description.to_string(),
                ranges: surviving.into_iter().flat_map(|s| s.ranges).collect(),
            });
        }
        Ok(())
    }

    /// Reverts the most recent action and moves it to the redo stack.
    /// Returns None when there is nothing to undo — inside an undo scope,
    /// that includes every step older than the scope.
    pub fn undo(&self) -> Result<Option<UndoOutcome>, Error> {
        if let Some(scope) = self.undo_scope.borrow().as_ref()
            && self.undo_stack.borrow().len() <= scope.boundary
        {
            return Ok(None);
        }
        self.apply_step(&self.undo_stack, &self.redo_stack)
    }

    /// Re-applies the most recently undone action and moves it back to the
    /// undo stack. Returns None when there is nothing to redo.
    pub fn redo(&self) -> Result<Option<UndoOutcome>, Error> {
        self.apply_step(&self.redo_stack, &self.undo_stack)
    }

    /// Pops the top step off `source`, replays its logged reverse SQL, and
    /// pushes the range that replay itself logged onto `target`. That
    /// captured counter-range is what makes undo and redo symmetric: undoing
    /// a step records exactly how to redo it, and vice versa.
    fn apply_step(
        &self,
        source: &std::cell::RefCell<Vec<UndoStep>>,
        target: &std::cell::RefCell<Vec<UndoStep>>,
    ) -> Result<Option<UndoOutcome>, Error> {
        let Some(step) = source.borrow_mut().pop() else {
            return Ok(None);
        };
        match self.replay_step(&step) {
            Ok((first_seq, last_seq, affected_task_ids)) => {
                target.borrow_mut().push(UndoStep {
                    description: step.description.clone(),
                    ranges: vec![(first_seq, last_seq)],
                });
                Ok(Some(UndoOutcome {
                    description: step.description,
                    affected_task_ids,
                }))
            }
            Err(err) => {
                // The transaction rolled back, so the step still applies
                // cleanly; keep it instead of silently losing history.
                source.borrow_mut().push(step);
                Err(err)
            }
        }
    }

    /// Replays the entries of all the step's log ranges, newest-first across
    /// every range, in one transaction. Reverse order is what keeps the
    /// parent_id foreign key intact: a subtree is deleted children-first, so
    /// its reversal inserts every parent before its children. Returns the
    /// log range the replay wrote and the ids of the task rows it touched.
    fn replay_step(&self, step: &UndoStep) -> Result<(i64, i64, Vec<i64>), Error> {
        let seq_before = self.max_undolog_seq()?;
        let tx = self.conn.unchecked_transaction()?;
        let mut entries: Vec<(i64, String, String, i64)> = {
            let mut stmt =
                tx.prepare("SELECT seq, sql, tbl, rid FROM undolog WHERE seq BETWEEN ?1 AND ?2")?;
            let mut entries = Vec::new();
            for &(first_seq, last_seq) in &step.ranges {
                let rows = stmt.query_map([first_seq, last_seq], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
                for row in rows {
                    entries.push(row?);
                }
            }
            entries
        };
        entries.sort_unstable_by_key(|&(seq, ..)| std::cmp::Reverse(seq));
        let mut affected_task_ids: Vec<i64> = Vec::new();
        for (_, sql, tbl, rid) in &entries {
            tx.execute(sql, [])?;
            if tbl == "tasks" && !affected_task_ids.contains(rid) {
                affected_task_ids.push(*rid);
            }
        }
        tx.commit()?;
        let seq_after = self.max_undolog_seq()?;
        Ok((seq_before + 1, seq_after, affected_task_ids))
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
        self.with_action(format!("create \"{title}\""), |tx| {
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
            "INSERT INTO tasks (parent_id, display_order, title, status_id, note, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, '', ?5, ?5)",
            rusqlite::params![parent_id, display_order, title, status_id, now],
        )?;
        let id = tx.last_insert_rowid();
        Ok(Task {
            id,
            parent_id,
            display_order,
            title: title.to_string(),
            status_id,
            due: None,
            note: String::new(),
            created_at: now.clone(),
            updated_at: now,
        })
        })
    }

    pub fn rename_task(&self, id: i64, title: &str) -> Result<(), Error> {
        let old = self.task_title(id)?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.with_action(format!("rename \"{old}\" → \"{title}\""), |tx| {
            let changed = tx.execute(
                "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![title, now, id],
            )?;
            if changed == 0 {
                return Err(Error::TaskNotFound(id));
            }
            Ok(())
        })
    }

    /// Sets the task's status. `status_id` must reference a row in
    /// `statuses` (FK-enforced).
    pub fn set_status(&self, id: i64, status_id: i64) -> Result<(), Error> {
        use rusqlite::OptionalExtension;
        let title = self.task_title(id)?;
        // A missing status is reported by the FK check below, not here, so
        // the label lookup must not fail first.
        let label: Option<String> = self
            .conn
            .query_row(
                "SELECT label FROM statuses WHERE id = ?1",
                [status_id],
                |row| row.get(0),
            )
            .optional()?;
        let label = label.unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.with_action(format!("set status of \"{title}\" to \"{label}\""), |tx| {
            let changed = tx.execute(
                "UPDATE tasks SET status_id = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![status_id, now, id],
            )?;
            if changed == 0 {
                return Err(Error::TaskNotFound(id));
            }
            Ok(())
        })
    }

    /// Sets or clears the task's due date. Dates must be real calendar days
    /// written exactly as `YYYY-MM-DD`; anything else is rejected with
    /// InvalidDate before the database is touched.
    pub fn set_due(&self, id: i64, due: Option<&str>) -> Result<(), Error> {
        if let Some(date) = due {
            // Round-tripping through the parsed date also rejects valid but
            // non-canonical spellings like `2026-9-5`, which chrono accepts;
            // only canonical dates keep string comparison of dates correct.
            let canonical = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map(|d| d.format("%Y-%m-%d").to_string());
            if canonical.as_deref() != Ok(date) {
                return Err(Error::InvalidDate(date.to_string()));
            }
        }
        let title = self.task_title(id)?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.with_action(format!("set due \"{title}\""), |tx| {
            let changed = tx.execute(
                "UPDATE tasks SET due = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![due, now, id],
            )?;
            if changed == 0 {
                return Err(Error::TaskNotFound(id));
            }
            Ok(())
        })
    }

    /// Replaces the task's whole note, e.g. after an external-editor session.
    pub fn set_note(&self, id: i64, text: &str) -> Result<(), Error> {
        let title = self.task_title(id)?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.with_action(format!("edit note \"{title}\""), |tx| {
            let changed = tx.execute(
                "UPDATE tasks SET note = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![text, now, id],
            )?;
            if changed == 0 {
                return Err(Error::TaskNotFound(id));
            }
            Ok(())
        })
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
        self.with_action(format!("create status \"{label}\""), |tx| {
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
            Ok(Status {
                id,
                label: label.to_string(),
                kind,
                color: color.to_string(),
                key,
                display_order,
                is_default: false,
            })
        })
    }

    pub fn update_status_label(&self, id: i64, label: &str) -> Result<(), Error> {
        let old = self.status_label(id)?;
        self.update_status_column(
            id,
            "label",
            &label,
            format!("rename status \"{old}\" → \"{label}\""),
        )
    }

    pub fn update_status_kind(&self, id: i64, kind: StatusKind) -> Result<(), Error> {
        let label = self.status_label(id)?;
        self.update_status_column(
            id,
            "kind",
            &kind.as_str(),
            format!("update status \"{label}\""),
        )
    }

    pub fn update_status_color(&self, id: i64, color: &str) -> Result<(), Error> {
        let label = self.status_label(id)?;
        self.update_status_column(id, "color", &color, format!("update status \"{label}\""))
    }

    pub fn update_status_key(&self, id: i64, key: char) -> Result<(), Error> {
        let label = self.status_label(id)?;
        self.update_status_column(
            id,
            "key",
            &key.to_string(),
            format!("update status \"{label}\""),
        )
    }

    fn update_status_column(
        &self,
        id: i64,
        column: &str,
        value: &dyn rusqlite::ToSql,
        description: String,
    ) -> Result<(), Error> {
        self.with_action(description, |tx| {
            // `column` only ever comes from the fixed set above, never from
            // user input, so interpolating it is safe.
            let changed = tx.execute(
                &format!("UPDATE statuses SET {column} = ?1 WHERE id = ?2"),
                rusqlite::params![value, id],
            )?;
            if changed == 0 {
                return Err(Error::StatusNotFound(id));
            }
            Ok(())
        })
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
        let label = self.status_label(id)?;
        self.with_action(format!("delete status \"{label}\""), |tx| {
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
            Ok(())
        })
    }

    /// Swaps the status with its display-order neighbour; a no-op at either
    /// end of the list.
    pub fn move_status(&self, id: i64, direction: StatusMove) -> Result<(), Error> {
        use rusqlite::OptionalExtension;
        let label = self.status_label(id)?;
        self.with_action(format!("move status \"{label}\""), |tx| {
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
            Ok(())
        })
    }

    /// Moves the default flag to `id`. The old flag is only dropped in the
    /// same transaction that sets the new one, so exactly one default row
    /// survives any outcome.
    pub fn set_default_status(&self, id: i64) -> Result<(), Error> {
        // Also guards the blanket UPDATE below: run with a nonexistent id,
        // it would clear every flag.
        let label = self.status_label(id)?;
        self.with_action(format!("set default status \"{label}\""), |tx| {
            tx.execute("UPDATE statuses SET is_default = (id = ?1)", [id])?;
            Ok(())
        })
    }

    /// Swaps the task with its display-order neighbour within the same
    /// sibling group; a no-op at either end of the group.
    pub fn move_task(&self, id: i64, direction: TaskMove) -> Result<(), Error> {
        use rusqlite::OptionalExtension;
        let title = self.task_title(id)?;
        self.with_action(format!("move \"{title}\""), |tx| {
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
            Ok(())
        })
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
        let title = self.task_title(id)?;
        self.with_action(format!("move \"{title}\""), |tx| {
            let (old_parent, old_order): (Option<i64>, i64) = tx
                .query_row(
                    "SELECT parent_id, display_order FROM tasks WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| task_not_found(e, id))?;
            // Attaching a task inside its own subtree would detach that
            // subtree into a cycle unreachable from any root, so refuse up
            // front.
            if let Some(parent_id) = new_parent
                && (parent_id == id || is_in_subtree(tx, id, parent_id)?)
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
                // MAX + 1 for the same reason as in create_task: deletions
                // leave gaps, so COUNT could collide with an existing order.
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
            Ok(())
        })
    }

    /// Deletes the subtree rooted at `id` and returns how many tasks were
    /// removed. Rows are deleted one by one, deepest first: relying on
    /// ON DELETE CASCADE would tie correctness (and trigger-based undo
    /// recording) to SQLite's recursive-trigger settings and their depth
    /// limit.
    pub fn delete_subtree(&self, id: i64) -> Result<i64, Error> {
        let title = self.task_title(id)?;
        self.with_action(format!("delete \"{title}\""), |tx| {
            let ids = collect_subtree_ids(tx, id)?;
            let mut deleted = 0i64;
            // Preorder reversed puts every task before its ancestors, so no
            // DELETE ever triggers a cascade onto a still-pending row — and
            // replaying the logged reverse INSERTs backwards restores every
            // parent before its children, keeping the FK satisfied.
            for &task_id in ids.iter().rev() {
                deleted += tx.execute("DELETE FROM tasks WHERE id = ?1", [task_id])? as i64;
            }
            Ok(deleted)
        })
    }

    pub fn list_children(&self, parent_id: Option<i64>) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, display_order, title, status_id, due, note, created_at, updated_at
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
            "SELECT id, parent_id, display_order, title, status_id, due, note, created_at, updated_at
             FROM tasks ORDER BY parent_id, display_order",
        )?;
        let tasks = stmt
            .query_map([], task_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    /// Runs one search/filter/sort request and returns the matching tasks as
    /// a flat list. Text matching is a plain LIKE scan over titles and notes
    /// (wildcards escaped, so the text is always literal); at personal-tool
    /// scale that costs milliseconds and needs no index to keep in sync.
    /// `today` (as `YYYY-MM-DD`) is passed in so overdue filtering stays a
    /// pure function of its inputs.
    pub fn search(&self, query: &Query, today: &str) -> Result<Vec<Task>, Error> {
        let mut sql = String::from(
            "SELECT t.id, t.parent_id, t.display_order, t.title, t.status_id, t.due, t.note,
                    t.created_at, t.updated_at
             FROM tasks t JOIN statuses s ON s.id = t.status_id WHERE 1 = 1",
        );
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(text) = &query.text {
            let pattern = format!("%{}%", escape_like(text));
            sql.push_str(" AND (t.title LIKE ? ESCAPE '\\' OR t.note LIKE ? ESCAPE '\\')");
            params.push(pattern.clone().into());
            params.push(pattern.into());
        }
        match query.filter {
            Filter::All => {}
            Filter::Open => sql.push_str(" AND s.kind = 'open'"),
            Filter::Status(status_id) => {
                sql.push_str(" AND t.status_id = ?");
                params.push(status_id.into());
            }
            // Only tasks that can still be worked on count as overdue; a
            // finished task's past due date is history, not a problem.
            Filter::Overdue => {
                sql.push_str(" AND t.due IS NOT NULL AND t.due < ? AND s.kind = 'open'");
                params.push(today.to_string().into());
            }
        }
        // Stored dates and timestamps are canonical fixed-width strings, so
        // plain string ordering is correct for all of them. The id tiebreak
        // keeps equal-key orders stable.
        match query.sort {
            Sort::TreeOrder => {}
            Sort::Due => sql.push_str(" ORDER BY t.due IS NULL, t.due, t.id"),
            Sort::Updated => sql.push_str(" ORDER BY t.updated_at DESC, t.id"),
            Sort::Created => sql.push_str(" ORDER BY t.created_at DESC, t.id"),
            Sort::Title => sql.push_str(" ORDER BY t.title, t.id"),
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let mut tasks = stmt
            .query_map(rusqlite::params_from_iter(params), task_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if query.sort == Sort::TreeOrder {
            // Matches can sit at any depth, so their relative order is the
            // whole tree's depth-first order, not anything SQL can sort by.
            let position = self.tree_order_positions()?;
            tasks.sort_by_key(|task| position.get(&task.id).copied().unwrap_or(usize::MAX));
        }
        Ok(tasks)
    }

    /// Maps every task id to its position in a depth-first walk of the whole
    /// tree (parents before children, siblings by display order). Walks with
    /// an explicit stack because tree depth is unbounded.
    fn tree_order_positions(&self) -> Result<std::collections::HashMap<i64, usize>, Error> {
        let mut children: std::collections::HashMap<Option<i64>, Vec<i64>> =
            std::collections::HashMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT id, parent_id FROM tasks ORDER BY display_order DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
        })?;
        for row in rows {
            let (id, parent_id) = row?;
            children.entry(parent_id).or_default().push(id);
        }
        let mut position = std::collections::HashMap::new();
        // Each group was collected in descending display order, so popping
        // off the stack yields siblings in ascending display order.
        let mut stack = children.remove(&None).unwrap_or_default();
        while let Some(id) = stack.pop() {
            position.insert(id, position.len());
            if let Some(group) = children.remove(&Some(id)) {
                stack.extend(group);
            }
        }
        Ok(position)
    }
}

/// Escapes LIKE wildcards (and the escape character itself) so search text
/// always matches literally.
fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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
        note: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// Creates the session-scoped undo machinery: an `undolog` table collecting
/// reverse SQL for every row change, filled by one INSERT/UPDATE/DELETE
/// trigger per undoable table. All of it is TEMP, so the log is connection-
/// local (concurrent app instances cannot see each other's history) and
/// vanishes with the connection — no startup cleanup, no migration.
///
/// Seeding in the migration runs before this, so a fresh database starts
/// with an empty log.
fn create_undo_log(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("CREATE TEMP TABLE undolog (seq INTEGER PRIMARY KEY, sql TEXT NOT NULL, tbl TEXT NOT NULL, rid INTEGER NOT NULL)")?;
    // Both tables expose their rowid as `id`. Logging under the rowid lets
    // the reverse INSERT restore it, so ids survive a delete/undo round
    // trip and later log entries referring to the same row stay valid.
    conn.execute_batch(&undo_triggers_sql(
        "tasks",
        "id",
        &[
            "parent_id",
            "display_order",
            "status_id",
            "title",
            "due",
            "note",
            "created_at",
            "updated_at",
        ],
    ))?;
    conn.execute_batch(&undo_triggers_sql(
        "statuses",
        "id",
        &[
            "label",
            "kind",
            "color",
            "key",
            "display_order",
            "is_default",
        ],
    ))?;
    Ok(())
}

/// Builds the three TEMP triggers recording reverse SQL for one table.
/// Column values are rendered with quote(), which escapes text and turns
/// NULL into the literal NULL, so any row round-trips through the log.
fn undo_triggers_sql(table: &str, key: &str, columns: &[&str]) -> String {
    let insert_columns = columns.join(",");
    let insert_values: String = columns
        .iter()
        .map(|c| format!("||','||quote(OLD.{c})"))
        .collect();
    let update_assignments = columns
        .iter()
        .map(|c| format!("{c}='||quote(OLD.{c})||'"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "CREATE TEMP TRIGGER undo_{table}_insert AFTER INSERT ON {table} BEGIN
           INSERT INTO undolog (sql, tbl, rid)
           VALUES ('DELETE FROM {table} WHERE {key}='||NEW.{key}, '{table}', NEW.{key});
         END;
         CREATE TEMP TRIGGER undo_{table}_update AFTER UPDATE ON {table} BEGIN
           INSERT INTO undolog (sql, tbl, rid)
           VALUES ('UPDATE {table} SET {update_assignments} WHERE {key}='||OLD.{key}, '{table}', OLD.{key});
         END;
         CREATE TEMP TRIGGER undo_{table}_delete AFTER DELETE ON {table} BEGIN
           INSERT INTO undolog (sql, tbl, rid)
           VALUES ('INSERT INTO {table}({key},{insert_columns}) VALUES('||OLD.{key}{insert_values}||')', '{table}', OLD.{key});
         END;"
    )
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
    // Then: user_version is 1 and the tasks table exists
    #[test]
    fn open_migrates_fresh_db_to_v1() {
        let db = Db::open_in_memory().unwrap();

        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);

        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'tasks'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "table `tasks` should exist");
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
    //       NULL, note is empty, and created_at/updated_at are equal RFC 3339
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
        assert_eq!(task.note, "");
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

    // Tests that set_due stores the date and bumps updated_at only.
    // Given: a task whose created_at/updated_at are backdated to a fixed
    //        past timestamp (second-precision timestamps would otherwise be
    //        indistinguishable within one test run)
    // When: set_due is called with a valid YYYY-MM-DD date
    // Then: the stored due becomes that date, updated_at moves off the old
    //       value, and created_at keeps the old value
    #[test]
    fn set_due_stores_date_and_updates_updated_at() {
        let db = Db::open_in_memory().unwrap();
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

        db.set_due(task.id, Some("2026-09-15")).unwrap();

        let updated = db
            .list_children(None)
            .unwrap()
            .into_iter()
            .find(|t| t.id == task.id)
            .unwrap();
        assert_eq!(updated.due.as_deref(), Some("2026-09-15"));
        assert_eq!(updated.created_at, past);
        assert_ne!(updated.updated_at, past);
    }

    // Tests that set_due(None) clears an existing due date.
    // Given: a task with due = 2026-09-15
    // When: set_due is called with None
    // Then: the stored due is NULL again
    #[test]
    fn set_due_with_none_clears_the_date() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        db.set_due(task.id, Some("2026-09-15")).unwrap();

        db.set_due(task.id, None).unwrap();

        assert_eq!(db.list_all().unwrap()[0].due, None);
    }

    // Tests that malformed or impossible dates are rejected up front.
    // Given: a task with no due date
    // When: set_due is called with a wrong separator, a day that does not
    //       exist, and a non-zero-padded (non-canonical) form
    // Then: each fails with InvalidDate carrying the input, and the stored
    //       due stays NULL (canonical storage keeps string comparison of
    //       dates correct)
    #[test]
    fn set_due_rejects_invalid_dates() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();

        for bad in ["2026/09/15", "2026-02-30", "2026-9-5", "someday"] {
            let result = db.set_due(task.id, Some(bad));
            assert!(
                matches!(result, Err(Error::InvalidDate(ref d)) if d == bad),
                "`{bad}` should be rejected, got {result:?}"
            );
        }
        assert_eq!(db.list_all().unwrap()[0].due, None);
    }

    // Tests that setting the due date of a nonexistent task is an error.
    // Given: an empty database
    // When: set_due is called with an id that matches no row
    // Then: it fails with TaskNotFound instead of silently updating nothing,
    //       so callers holding a stale id notice immediately
    #[test]
    fn set_due_with_unknown_id_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.set_due(999, Some("2026-09-15"));

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
    }

    // Tests that set_note replaces the whole note.
    // Given: a task with an existing note and backdated timestamps
    // When: set_note is called with a new full text
    // Then: the stored note is exactly the new text, updated_at moves off
    //       the old value, and created_at keeps it
    #[test]
    fn set_note_replaces_note_and_updates_updated_at() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        db.set_note(task.id, "old entry\n").unwrap();
        let past = "2000-01-01T00:00:00Z";
        db.conn
            .execute(
                "UPDATE tasks SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
                rusqlite::params![past, task.id],
            )
            .unwrap();

        db.set_note(task.id, "# rewritten\n").unwrap();

        let updated = db.list_all().unwrap()[0].clone();
        assert_eq!(updated.note, "# rewritten\n");
        assert_eq!(updated.created_at, past);
        assert_ne!(updated.updated_at, past);
    }

    // Tests that replacing the note of a nonexistent task is an error.
    // Given: an empty database
    // When: set_note is called with an id that matches no row
    // Then: it fails with TaskNotFound instead of silently updating nothing
    #[test]
    fn set_note_with_unknown_id_fails() {
        let db = Db::open_in_memory().unwrap();

        let result = db.set_note(999, "text");

        assert!(matches!(result, Err(Error::TaskNotFound(999))));
    }

    // Tests that note edits are undoable actions.
    // Given: a task whose note was written once and then rewritten
    // When: undo runs twice
    // Then: the first undo restores the first text, the second restores the
    //       empty note, and each outcome names the task
    #[test]
    fn undo_of_note_edits_restores_previous_text() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "設計", None, default_status(&db))
            .unwrap();
        db.set_note(task.id, "entry\n").unwrap();
        db.set_note(task.id, "rewritten\n").unwrap();

        let outcome = db.undo().unwrap().expect("undo the rewrite");
        assert_eq!(outcome.description, "edit note \"設計\"");
        assert_eq!(db.list_all().unwrap()[0].note, "entry\n");

        let outcome = db.undo().unwrap().expect("undo the first write");
        assert_eq!(outcome.description, "edit note \"設計\"");
        assert_eq!(db.list_all().unwrap()[0].note, "");
    }

    // Tests that a due-date change is an undoable action.
    // Given: a task whose due was set to one date and then another
    // When: undo runs once
    // Then: the first date is back and the outcome names the task; a second
    //       undo restores the original NULL
    #[test]
    fn undo_of_set_due_restores_previous_value() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "設計", None, default_status(&db))
            .unwrap();
        db.set_due(task.id, Some("2026-09-15")).unwrap();
        db.set_due(task.id, Some("2026-10-01")).unwrap();

        let outcome = db.undo().unwrap().expect("undo the second set_due");
        assert_eq!(db.list_all().unwrap()[0].due.as_deref(), Some("2026-09-15"));
        assert_eq!(outcome.description, "set due \"設計\"");
        assert_eq!(outcome.affected_task_ids, [task.id]);

        db.undo().unwrap().expect("undo the first set_due");
        assert_eq!(db.list_all().unwrap()[0].due, None);
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

    /// Undo-log entries recorded after `from_seq`, oldest first.
    fn undolog_after(db: &Db, from_seq: i64) -> Vec<(String, String, i64)> {
        db.conn
            .prepare("SELECT sql, tbl, rid FROM undolog WHERE seq > ?1 ORDER BY seq")
            .unwrap()
            .query_map([from_seq], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn max_undolog_seq(db: &Db) -> i64 {
        db.conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM undolog", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    // Tests that every kind of task change is recorded as reverse SQL.
    // Given: a fresh database with the undo-log position noted
    // When: a task row is inserted, updated and deleted through raw SQL
    // Then: the log gains one entry per change — a DELETE undoing the
    //       insert, an UPDATE restoring the old values, and an INSERT
    //       undoing the delete — each tagged with tbl='tasks' and the row id
    #[test]
    fn undolog_records_reverse_sql_for_tasks() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let before = max_undolog_seq(&db);

        db.conn
            .execute(
                "INSERT INTO tasks (parent_id, display_order, status_id, title, created_at, updated_at)
                 VALUES (NULL, 0, ?1, 'old', 't0', 't0')",
                [status],
            )
            .unwrap();
        let id = db.conn.last_insert_rowid();
        db.conn
            .execute("UPDATE tasks SET title = 'new' WHERE id = ?1", [id])
            .unwrap();
        db.conn
            .execute("DELETE FROM tasks WHERE id = ?1", [id])
            .unwrap();

        let entries = undolog_after(&db, before);
        assert_eq!(entries.len(), 3);
        for (_, tbl, rid) in &entries {
            assert_eq!(tbl, "tasks");
            assert_eq!(*rid, id);
        }
        assert_eq!(entries[0].0, format!("DELETE FROM tasks WHERE id={id}"));
        assert!(entries[1].0.starts_with("UPDATE tasks SET "));
        assert!(entries[1].0.contains("title='old'"));
        assert!(entries[1].0.ends_with(&format!(" WHERE id={id}")));
        assert!(entries[2].0.starts_with("INSERT INTO tasks("));
    }

    // Tests that status changes are recorded as reverse SQL.
    // Given: a fresh database with the undo-log position noted
    // When: a status row is inserted, updated and deleted through raw SQL
    // Then: the log gains a reverse entry per change, tagged with
    //       tbl='statuses' and the row id
    #[test]
    fn undolog_records_reverse_sql_for_statuses() {
        let db = Db::open_in_memory().unwrap();
        let before = max_undolog_seq(&db);

        db.conn
            .execute(
                "INSERT INTO statuses (label, kind, color, key, display_order, is_default)
                 VALUES ('w', 'open', 'red', 'w', 9, 0)",
                [],
            )
            .unwrap();
        let id = db.conn.last_insert_rowid();
        db.conn
            .execute("UPDATE statuses SET color = 'blue' WHERE id = ?1", [id])
            .unwrap();
        db.conn
            .execute("DELETE FROM statuses WHERE id = ?1", [id])
            .unwrap();

        let entries = undolog_after(&db, before);
        assert_eq!(entries.len(), 3);
        for (_, tbl, rid) in &entries {
            assert_eq!(tbl, "statuses");
            assert_eq!(*rid, id);
        }
        assert_eq!(entries[0].0, format!("DELETE FROM statuses WHERE id={id}"));
        assert!(entries[1].0.contains("color='red'"));
        assert!(entries[2].0.starts_with("INSERT INTO statuses("));
    }

    // Tests that the logged reverse SQL restores deleted rows exactly,
    // including NULLs and text needing quote escaping.
    // Given: two deleted tasks — one with due = NULL and a title containing
    //        a single quote, one with a due date
    // When: the logged reverse INSERT statements are executed oldest-last
    //       (parents were deleted last, so they are restored first)
    // Then: the restored rows equal the originals column for column
    #[test]
    fn undolog_reverse_insert_round_trips_rows() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "it's quoted", None, status).unwrap();
        let b = db
            .create_task(Some(a.id), "with due", None, status)
            .unwrap();
        db.conn
            .execute("UPDATE tasks SET due = '2026-09-30' WHERE id = ?1", [b.id])
            .unwrap();
        let original_tasks = db.list_all().unwrap();
        let before = max_undolog_seq(&db);

        db.conn
            .execute("DELETE FROM tasks WHERE id = ?1", [b.id])
            .unwrap();
        db.conn
            .execute("DELETE FROM tasks WHERE id = ?1", [a.id])
            .unwrap();
        for (sql, ..) in undolog_after(&db, before).iter().rev() {
            db.conn.execute(sql, []).unwrap();
        }

        assert_eq!(db.list_all().unwrap(), original_tasks);
    }

    fn undo_descriptions(db: &Db) -> Vec<String> {
        db.undo_stack
            .borrow()
            .iter()
            .map(|step| step.description.clone())
            .collect()
    }

    // Tests that each mutating call becomes one undo step with a
    // description naming what it did.
    // Given: a fresh database
    // When: a task is created, renamed and deleted
    // Then: the undo stack holds three steps whose descriptions carry the
    //       operation and the task title involved
    #[test]
    fn mutations_push_described_undo_steps() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "設計", None, default_status(&db))
            .unwrap();

        db.rename_task(task.id, "実装").unwrap();
        db.delete_subtree(task.id).unwrap();

        assert_eq!(
            undo_descriptions(&db),
            [
                "create \"設計\"",
                "rename \"設計\" → \"実装\"",
                "delete \"実装\""
            ]
        );
    }

    // Tests replaying logged SQL whose text values resemble SQL syntax.
    // Given: a task renamed away from a title containing a quote, a
    //        parameter marker and a comment marker
    // When: the rename is undone
    // Then: the original title is restored verbatim — logged values are
    //       quoted literals that the replay never re-interprets as
    //       parameters or comments
    #[test]
    fn undo_restores_titles_containing_sql_syntax() {
        let db = Db::open_in_memory().unwrap();
        let tricky = "it's ?1 -- 100%";
        let task = db
            .create_task(None, tricky, None, default_status(&db))
            .unwrap();
        db.rename_task(task.id, "plain").unwrap();

        db.undo().unwrap().expect("undo the rename");

        assert_eq!(db.list_all().unwrap()[0].title, tricky);
    }

    // Tests that an action changing no rows leaves the history alone.
    // Given: a single root task (moving it up has no neighbour to swap with)
    // When: move_task(Up) runs as a no-op
    // Then: no undo step is pushed — undoing it would visibly do nothing,
    //       which reads as a broken undo
    #[test]
    fn noop_action_pushes_no_undo_step() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "only", None, default_status(&db))
            .unwrap();
        let depth_before = db.undo_stack.borrow().len();

        db.move_task(task.id, TaskMove::Up).unwrap();

        assert_eq!(db.undo_stack.borrow().len(), depth_before);
    }

    // Tests that a failed action leaves no trace in the history.
    // Given: a task chain a > b (re-parenting a under b is a cycle)
    // When: the reparent fails with CycleDetected
    // Then: no undo step is pushed and no orphaned log entries remain
    //       referenced (the transaction rollback discards them)
    #[test]
    fn failed_action_pushes_no_undo_step() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap();
        let b = db.create_task(Some(a.id), "b", None, status).unwrap();
        let depth_before = db.undo_stack.borrow().len();
        let seq_before = max_undolog_seq(&db);

        let result = db.reparent(a.id, Some(b.id), None);

        assert!(matches!(result, Err(Error::CycleDetected { .. })));
        assert_eq!(db.undo_stack.borrow().len(), depth_before);
        assert_eq!(max_undolog_seq(&db), seq_before);
    }

    // Tests that status edits are undoable actions too.
    // Given: a fresh database
    // When: a status is created and then relabelled
    // Then: both actions land on the undo stack with descriptions naming
    //       the labels involved
    #[test]
    fn status_mutations_push_described_undo_steps() {
        let db = Db::open_in_memory().unwrap();
        let created = db
            .create_status("review", StatusKind::Open, "magenta", 'w')
            .unwrap();

        db.update_status_label(created.id, "waiting").unwrap();

        assert_eq!(
            undo_descriptions(&db),
            [
                "create status \"review\"",
                "rename status \"review\" → \"waiting\""
            ]
        );
    }

    /// Builds the trickiest shape for delete/undo: a three-level subtree
    /// with a sibling display-order gap on the middle level and a mix of
    /// NULL and non-NULL due dates. Returns the subtree root's id.
    fn subtree_fixture(db: &Db) -> i64 {
        let status = default_status(db);
        let root = db.create_task(None, "root", None, status).unwrap();
        db.create_task(None, "keep", None, status).unwrap();
        let x = db.create_task(Some(root.id), "x", None, status).unwrap();
        let y = db.create_task(Some(root.id), "y", None, status).unwrap();
        let z = db.create_task(Some(root.id), "z", None, status).unwrap();
        db.create_task(Some(x.id), "leaf", None, status).unwrap();
        // A gap in the middle sibling group: orders become x(0), z(2).
        db.delete_subtree(y.id).unwrap();
        db.conn
            .execute("UPDATE tasks SET due = '2026-10-01' WHERE id = ?1", [z.id])
            .unwrap();
        root.id
    }

    // Tests the core promise of trigger-based undo: a deleted subtree comes
    // back exactly as it was.
    // Given: a three-level subtree with a sibling display-order gap and
    //        mixed NULL/non-NULL due dates (plus an untouched outside
    //        task), fully snapshotted
    // When: the subtree is deleted and the deletion is undone
    // Then: every task row (ids, orders, timestamps included) equals the
    //       pre-delete snapshot exactly
    #[test]
    fn undo_of_subtree_delete_restores_all_rows_exactly() {
        let db = Db::open_in_memory().unwrap();
        let root_id = subtree_fixture(&db);
        let tasks_before = db.list_all().unwrap();

        db.delete_subtree(root_id).unwrap();
        let outcome = db.undo().unwrap().expect("there is a step to undo");

        assert_eq!(db.list_all().unwrap(), tasks_before);
        assert_eq!(outcome.description, "delete \"root\"");
        assert!(outcome.affected_task_ids.contains(&root_id));
    }

    // Tests undoing a creation.
    // Given: a single created task
    // When: undo runs
    // Then: the task row is gone again, and the outcome still reports its
    //       id (the UI uses this to notice nothing is left to select)
    #[test]
    fn undo_of_create_removes_the_task() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();

        let outcome = db.undo().unwrap().expect("there is a step to undo");

        assert!(db.list_all().unwrap().is_empty());
        assert_eq!(outcome.description, "create \"t\"");
        assert_eq!(outcome.affected_task_ids, [task.id]);
    }

    // Tests that undo and redo are symmetric.
    // Given: the subtree fixture, deleted
    // When: undo, redo and undo again run
    // Then: redo removes the subtree exactly as the delete did, the second
    //       undo restores the full snapshot again, and every outcome carries
    //       the original action's description
    #[test]
    fn undo_redo_undo_round_trips() {
        let db = Db::open_in_memory().unwrap();
        let root_id = subtree_fixture(&db);
        let tasks_before = db.list_all().unwrap();
        db.delete_subtree(root_id).unwrap();
        let tasks_deleted = db.list_all().unwrap();

        db.undo().unwrap().expect("undo the delete");
        let redone = db.redo().unwrap().expect("redo the delete");
        assert_eq!(db.list_all().unwrap(), tasks_deleted);
        assert_eq!(redone.description, "delete \"root\"");

        let undone = db.undo().unwrap().expect("undo the redone delete");
        assert_eq!(db.list_all().unwrap(), tasks_before);
        assert_eq!(undone.description, "delete \"root\"");
    }

    // Tests linear history: editing after an undo discards the redo branch.
    // Given: a renamed task whose rename was undone
    // When: a new edit (another rename) happens
    // Then: redo returns None — the undone rename is no longer reachable
    #[test]
    fn new_edit_after_undo_discards_redo() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "a", None, default_status(&db))
            .unwrap();
        db.rename_task(task.id, "b").unwrap();
        db.undo().unwrap().expect("undo the rename");

        db.rename_task(task.id, "c").unwrap();

        assert!(db.redo().unwrap().is_none());
        assert_eq!(db.list_all().unwrap()[0].title, "c");
    }

    // Tests undoing a status deletion.
    // Given: the seeded statuses with one non-default row deleted
    // When: undo runs
    // Then: the full status list equals the pre-delete snapshot (same id,
    //       label, kind, color, key, order) and no task ids are reported
    #[test]
    fn undo_of_status_delete_restores_the_row() {
        let db = Db::open_in_memory().unwrap();
        let statuses_before = db.list_statuses().unwrap();
        let victim = deletable_status(&db);

        db.delete_status(victim.id).unwrap();
        let outcome = db.undo().unwrap().expect("there is a step to undo");

        assert_eq!(db.list_statuses().unwrap(), statuses_before);
        assert_eq!(
            outcome.description,
            format!("delete status \"{}\"", victim.label)
        );
        assert!(outcome.affected_task_ids.is_empty());
    }

    // Tests undoing a default-status change.
    // Given: the default flag moved from the seeded default to another row
    // When: undo runs
    // Then: the old default carries the flag again and exactly one row is
    //       flagged (the blanket flag update is replayed row by row)
    #[test]
    fn undo_of_set_default_keeps_exactly_one_default() {
        let db = Db::open_in_memory().unwrap();
        let old_default = default_status(&db);
        db.set_default_status(deletable_status(&db).id).unwrap();

        db.undo().unwrap().expect("there is a step to undo");

        assert_eq!(db.default_status_id().unwrap(), old_default);
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

    // Tests undo and redo on empty stacks.
    // Given: a fresh database with no actions performed
    // When: undo and redo run
    // Then: both return None and change nothing
    #[test]
    fn undo_and_redo_on_empty_stacks_return_none() {
        let db = Db::open_in_memory().unwrap();

        assert!(db.undo().unwrap().is_none());
        assert!(db.redo().unwrap().is_none());
        assert_eq!(db.list_statuses().unwrap().len(), 5);
    }

    // Tests that a scope's surviving edits fold into one outer undo step.
    // Given: an undo scope in which a status is relabelled and recolored
    // When: the scope ends with the description "edit statuses" and a
    //       single undo runs afterwards
    // Then: that one undo reverts both edits (full status snapshot match)
    //       and its outcome carries the scope's description
    #[test]
    fn scope_folds_surviving_edits_into_one_outer_step() {
        let db = Db::open_in_memory().unwrap();
        let statuses_before = db.list_statuses().unwrap();
        let victim = deletable_status(&db).id;

        db.begin_undo_scope().unwrap();
        db.update_status_label(victim, "renamed").unwrap();
        db.update_status_color(victim, "blue").unwrap();
        db.end_undo_scope("edit statuses").unwrap();
        let outcome = db.undo().unwrap().expect("the folded step is undoable");

        assert_eq!(db.list_statuses().unwrap(), statuses_before);
        assert_eq!(outcome.description, "edit statuses");
        assert!(outcome.affected_task_ids.is_empty());
    }

    // Tests that the folded scope step redoes atomically.
    // Given: a closed scope holding two status edits, undone from outside
    // When: redo runs
    // Then: both edits are applied again in one step, and undoing once more
    //       reverts both again (multi-range replay is symmetric)
    #[test]
    fn outer_redo_reapplies_the_whole_scope_session() {
        let db = Db::open_in_memory().unwrap();
        let statuses_before = db.list_statuses().unwrap();
        let victim = deletable_status(&db).id;
        db.begin_undo_scope().unwrap();
        db.update_status_label(victim, "renamed").unwrap();
        db.update_status_color(victim, "blue").unwrap();
        db.end_undo_scope("edit statuses").unwrap();
        let statuses_edited = db.list_statuses().unwrap();
        db.undo().unwrap().expect("undo the folded step");

        let redone = db.redo().unwrap().expect("the folded step is redoable");
        assert_eq!(db.list_statuses().unwrap(), statuses_edited);
        assert_eq!(redone.description, "edit statuses");

        db.undo().unwrap().expect("undo the redone step");
        assert_eq!(db.list_statuses().unwrap(), statuses_before);
    }

    // Tests that scoped undo never descends into pre-scope history.
    // Given: a task rename done before the scope and one status edit inside
    // When: undo runs twice inside the scope
    // Then: the first undo reverts the in-scope edit, the second returns
    //       None, and the pre-scope rename stays applied
    #[test]
    fn scoped_undo_stops_at_the_scope_boundary() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "old", None, default_status(&db))
            .unwrap();
        db.rename_task(task.id, "new").unwrap();
        let victim = deletable_status(&db);

        db.begin_undo_scope().unwrap();
        db.update_status_label(victim.id, "renamed").unwrap();
        db.undo().unwrap().expect("the in-scope edit is undoable");
        assert!(db.undo().unwrap().is_none(), "must stop at the boundary");

        assert_eq!(db.list_all().unwrap()[0].title, "new");
        let label = db
            .list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| s.id == victim.id)
            .unwrap()
            .label;
        assert_eq!(label, victim.label);
    }

    // Tests fine-grained undo/redo inside a scope.
    // Given: an open scope with one status edit
    // When: the edit is undone and then redone inside the scope
    // Then: the undo reverts it, the redo reapplies it, and both outcomes
    //       name the original edit
    #[test]
    fn scoped_undo_then_redo_is_symmetric() {
        let db = Db::open_in_memory().unwrap();
        let victim = deletable_status(&db);
        db.begin_undo_scope().unwrap();
        db.update_status_label(victim.id, "renamed").unwrap();

        let undone = db.undo().unwrap().expect("undo the in-scope edit");
        let label_after_undo = db
            .list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| s.id == victim.id)
            .unwrap()
            .label;
        let redone = db.redo().unwrap().expect("redo the in-scope edit");
        let label_after_redo = db
            .list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| s.id == victim.id)
            .unwrap()
            .label;

        assert_eq!(label_after_undo, victim.label);
        assert_eq!(label_after_redo, "renamed");
        let expected = format!("rename status \"{}\" → \"renamed\"", victim.label);
        assert_eq!(undone.description, expected);
        assert_eq!(redone.description, expected);
    }

    // Tests that a scope whose edits were all undone vanishes entirely.
    // Given: an outer rename undone before the scope (so the redo stack is
    //        non-empty), then a scope whose only edit gets undone inside
    // When: the scope ends
    // Then: nothing is pushed onto the undo stack and the stashed outer
    //       redo comes back: redo reapplies the pre-scope rename
    #[test]
    fn empty_scope_result_restores_the_stashed_redo() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "old", None, default_status(&db))
            .unwrap();
        db.rename_task(task.id, "new").unwrap();
        db.undo().unwrap().expect("undo the rename");
        let victim = deletable_status(&db).id;

        db.begin_undo_scope().unwrap();
        db.update_status_label(victim, "renamed").unwrap();
        db.undo().unwrap().expect("undo the in-scope edit");
        db.end_undo_scope("edit statuses").unwrap();

        let redone = db.redo().unwrap().expect("outer redo must be restored");
        assert_eq!(redone.description, "rename \"old\" → \"new\"");
        assert_eq!(db.list_all().unwrap()[0].title, "new");
    }

    // Tests a scope in which nothing was edited at all.
    // Given: one pre-scope task creation, then a scope opened and closed
    //        without any edit
    // When: undo runs after the scope ends
    // Then: it reverts the pre-scope creation, not an empty scope step
    #[test]
    fn scope_without_edits_pushes_nothing() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "t", None, default_status(&db))
            .unwrap();

        db.begin_undo_scope().unwrap();
        db.end_undo_scope("edit statuses").unwrap();
        let outcome = db.undo().unwrap().expect("the creation is undoable");

        assert_eq!(outcome.description, "create \"t\"");
        assert!(db.list_all().unwrap().is_empty());
    }

    // Tests that closing a scope with surviving edits leaves no redo.
    // Given: an outer rename undone before the scope (redo stack non-empty)
    //        and a scope whose edit survives, with one in-scope undo/redo
    //        cycle producing in-scope redo entries along the way
    // When: the scope ends
    // Then: redo returns None — the session counts as a fresh edit, so the
    //       outer redo branch is gone, and in-scope counters do not leak out
    #[test]
    fn redo_after_closing_a_scope_with_edits_is_empty() {
        let db = Db::open_in_memory().unwrap();
        let task = db
            .create_task(None, "old", None, default_status(&db))
            .unwrap();
        db.rename_task(task.id, "new").unwrap();
        db.undo().unwrap().expect("undo the rename");
        let victim = deletable_status(&db).id;

        db.begin_undo_scope().unwrap();
        db.update_status_label(victim, "renamed").unwrap();
        db.undo().unwrap().expect("undo the in-scope edit");
        db.redo().unwrap().expect("redo the in-scope edit");
        db.end_undo_scope("edit statuses").unwrap();

        assert!(db.redo().unwrap().is_none());
    }

    // Tests that undo scopes refuse to nest.
    // Given: an already open undo scope
    // When: begin_undo_scope is called again
    // Then: it fails with UndoScopeAlreadyActive, and the original scope is
    //       still functional (its end succeeds)
    #[test]
    fn begin_undo_scope_twice_is_an_error() {
        let db = Db::open_in_memory().unwrap();
        db.begin_undo_scope().unwrap();

        let result = db.begin_undo_scope();

        assert!(matches!(result, Err(Error::UndoScopeAlreadyActive)));
        db.end_undo_scope("edit statuses").unwrap();
    }

    // Tests closing a scope that was never opened.
    // Given: a database with no active undo scope
    // When: end_undo_scope is called
    // Then: it fails with UndoScopeNotActive
    #[test]
    fn end_undo_scope_without_begin_is_an_error() {
        let db = Db::open_in_memory().unwrap();

        let result = db.end_undo_scope("edit statuses");

        assert!(matches!(result, Err(Error::UndoScopeNotActive)));
    }

    // Tests that replaying a big deletion backwards never breaks the
    // parent_id foreign key, which stays enforced during undo.
    // Given: a 3000-level parent chain, deleted deepest-first
    // When: undo runs (replay inserts each parent before its children)
    // Then: all 3000 tasks are back and redo removes them again
    #[test]
    fn undo_of_deep_subtree_delete_respects_foreign_keys() {
        let db = Db::open_in_memory().unwrap();
        let (top, _) = deep_chain(&db, 3000);
        db.delete_subtree(top.id).unwrap();

        db.undo().unwrap().expect("there is a step to undo");
        assert_eq!(db.list_all().unwrap().len(), 3000);

        db.redo().unwrap().expect("there is a step to redo");
        assert!(db.list_all().unwrap().is_empty());
    }

    /// A fixed "today" for the search tests, so they never depend on the
    /// clock.
    const TODAY: &str = "2026-09-13";

    fn search_titles(db: &Db, query: &Query) -> Vec<String> {
        db.search(query, TODAY)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect()
    }

    fn text_query(text: &str) -> Query {
        Query {
            text: Some(text.to_string()),
            filter: Filter::All,
            ..Query::default()
        }
    }

    /// The seeded status id whose kind is done ("完了").
    fn done_status(db: &Db) -> i64 {
        db.list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| s.kind == StatusKind::Done)
            .unwrap()
            .id
    }

    // Tests that search text matches both titles and notes.
    // Given: one task whose title contains "設計" and another whose note
    //       contains the same word (a two-character Japanese word, which a
    //       tokenizing search would miss), plus an unrelated task
    // When: searching for "設計" without a filter
    // Then: exactly the title match and the note match come back
    #[test]
    fn search_matches_japanese_word_in_title_or_note() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        db.create_task(None, "API 設計", None, status).unwrap();
        let noted = db.create_task(None, "auth", None, status).unwrap();
        db.set_note(noted.id, "JWT の設計を検討した").unwrap();
        db.create_task(None, "unrelated", None, status).unwrap();

        let titles = search_titles(&db, &text_query("設計"));

        assert_eq!(titles, ["API 設計", "auth"]);
    }

    // Tests that LIKE wildcards in the search text are taken literally.
    // Given: tasks titled "off 50%" / "off 50x" and "a_b" / "axb"
    // When: searching for "50%" and for "a_b"
    // Then: each search matches only the task containing the literal
    //       characters; % and _ do not act as wildcards
    #[test]
    fn search_treats_like_wildcards_literally() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        db.create_task(None, "off 50%", None, status).unwrap();
        db.create_task(None, "off 50x", None, status).unwrap();
        db.create_task(None, "a_b", None, status).unwrap();
        db.create_task(None, "axb", None, status).unwrap();

        assert_eq!(search_titles(&db, &text_query("50%")), ["off 50%"]);
        assert_eq!(search_titles(&db, &text_query("a_b")), ["a_b"]);
    }

    // Tests that the escape character itself is matched literally.
    // Given: a task whose title contains a backslash and one without
    // When: searching for the backslash-containing fragment
    // Then: only the task with the literal backslash matches
    #[test]
    fn search_treats_escape_character_literally() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        db.create_task(None, "path C:\\dir", None, status).unwrap();
        db.create_task(None, "path C:dir", None, status).unwrap();

        assert_eq!(search_titles(&db, &text_query("C:\\dir")), ["path C:\\dir"]);
    }

    // Tests the default open filter.
    // Given: one task on the default (open) status and one on a done status
    // When: searching with no text and Filter::Open
    // Then: only the open task comes back; Filter::All returns both
    #[test]
    fn search_open_filter_hides_finished_tasks() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "open task", None, default_status(&db))
            .unwrap();
        db.create_task(None, "done task", None, done_status(&db))
            .unwrap();

        let open_only = search_titles(&db, &Query::default());
        assert_eq!(open_only, ["open task"]);

        let all = search_titles(
            &db,
            &Query {
                filter: Filter::All,
                ..Query::default()
            },
        );
        assert_eq!(all, ["open task", "done task"]);
    }

    // Tests filtering by one specific status.
    // Given: tasks on two different open statuses
    // When: searching with Filter::Status of one of them
    // Then: only the task on that exact status comes back
    #[test]
    fn search_status_filter_matches_exactly_that_status() {
        let db = Db::open_in_memory().unwrap();
        let ready = non_default_status(&db);
        db.create_task(None, "default", None, default_status(&db))
            .unwrap();
        db.create_task(None, "ready", None, ready).unwrap();

        let titles = search_titles(
            &db,
            &Query {
                filter: Filter::Status(ready),
                ..Query::default()
            },
        );

        assert_eq!(titles, ["ready"]);
    }

    // Tests the overdue filter.
    // Given: open tasks due before/on/after the fixed today, an open task
    //        without a due date, and a done task due in the past
    // When: searching with Filter::Overdue
    // Then: only the open task whose due date lies strictly before today
    //       comes back
    #[test]
    fn search_overdue_filter_keeps_only_open_past_due_tasks() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let overdue = db.create_task(None, "overdue", None, status).unwrap();
        db.set_due(overdue.id, Some("2026-09-12")).unwrap();
        let today_task = db.create_task(None, "due today", None, status).unwrap();
        db.set_due(today_task.id, Some(TODAY)).unwrap();
        let future = db.create_task(None, "future", None, status).unwrap();
        db.set_due(future.id, Some("2026-09-14")).unwrap();
        db.create_task(None, "no due", None, status).unwrap();
        let done = db
            .create_task(None, "done late", None, done_status(&db))
            .unwrap();
        db.set_due(done.id, Some("2026-09-01")).unwrap();

        let titles = search_titles(
            &db,
            &Query {
                filter: Filter::Overdue,
                ..Query::default()
            },
        );

        assert_eq!(titles, ["overdue"]);
    }

    // Tests the due-date sort.
    // Given: tasks due late, early, and without a due date
    // When: searching sorted by due
    // Then: dated tasks come first in ascending order, undated ones last
    #[test]
    fn search_due_sort_is_ascending_with_nulls_last() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let late = db.create_task(None, "late", None, status).unwrap();
        db.set_due(late.id, Some("2026-12-01")).unwrap();
        db.create_task(None, "undated", None, status).unwrap();
        let early = db.create_task(None, "early", None, status).unwrap();
        db.set_due(early.id, Some("2026-09-20")).unwrap();

        let titles = search_titles(
            &db,
            &Query {
                sort: Sort::Due,
                ..Query::default()
            },
        );

        assert_eq!(titles, ["early", "late", "undated"]);
    }

    // Tests the updated/created sorts.
    // Given: three tasks whose created_at/updated_at are backdated so that
    //        creation order and update order disagree
    // When: searching sorted by updated and by created
    // Then: both orders are newest-first of their respective timestamp
    #[test]
    fn search_updated_and_created_sorts_are_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        for (title, created, updated) in [
            ("a", "2026-01-01T00:00:00Z", "2026-03-01T00:00:00Z"),
            ("b", "2026-02-01T00:00:00Z", "2026-01-01T00:00:00Z"),
            ("c", "2026-03-01T00:00:00Z", "2026-02-01T00:00:00Z"),
        ] {
            let task = db.create_task(None, title, None, status).unwrap();
            db.conn
                .execute(
                    "UPDATE tasks SET created_at = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![created, updated, task.id],
                )
                .unwrap();
        }

        let by_updated = search_titles(
            &db,
            &Query {
                sort: Sort::Updated,
                ..Query::default()
            },
        );
        assert_eq!(by_updated, ["a", "c", "b"]);

        let by_created = search_titles(
            &db,
            &Query {
                sort: Sort::Created,
                ..Query::default()
            },
        );
        assert_eq!(by_created, ["c", "b", "a"]);
    }

    // Tests the title sort.
    // Given: tasks titled out of alphabetical order
    // When: searching sorted by title
    // Then: they come back in ascending title order
    #[test]
    fn search_title_sort_is_ascending() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        for title in ["banana", "apple", "cherry"] {
            db.create_task(None, title, None, status).unwrap();
        }

        let titles = search_titles(
            &db,
            &Query {
                sort: Sort::Title,
                ..Query::default()
            },
        );

        assert_eq!(titles, ["apple", "banana", "cherry"]);
    }

    // Tests the tree-order sort.
    // Given: roots B(order 0) and A(order 1), each with two children whose
    //        display orders disagree with their creation order
    // When: searching everything sorted by tree order
    // Then: results follow a depth-first walk: each root immediately
    //       followed by its children in display order
    #[test]
    fn search_tree_order_flattens_depth_first_by_display_order() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let b = db.create_task(None, "B", None, status).unwrap();
        let a = db.create_task(None, "A", None, status).unwrap();
        db.create_task(Some(a.id), "A2", None, status).unwrap();
        let a2 = db.list_children(Some(a.id)).unwrap()[0].clone();
        db.create_task(Some(a.id), "A1", None, status).unwrap();
        // Swap the children so display order disagrees with insertion order.
        db.move_task(a2.id, TaskMove::Down).unwrap();
        db.create_task(Some(b.id), "B1", None, status).unwrap();

        let titles = search_titles(&db, &Query::default());

        assert_eq!(titles, ["B", "B1", "A", "A1", "A2"]);
    }

    // Tests combining text, filter and sort in one query.
    // Given: tasks "x report" (open, due late), "y report" (open, due
    //        early), "z report" (done) and "other" (open)
    // When: searching for "report" with the open filter, sorted by due
    // Then: only the open report tasks come back, earliest due first
    #[test]
    fn search_combines_text_filter_and_sort() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let x = db.create_task(None, "x report", None, status).unwrap();
        db.set_due(x.id, Some("2026-10-01")).unwrap();
        let y = db.create_task(None, "y report", None, status).unwrap();
        db.set_due(y.id, Some("2026-09-20")).unwrap();
        db.create_task(None, "z report", None, done_status(&db))
            .unwrap();
        db.create_task(None, "other", None, status).unwrap();

        let titles = search_titles(
            &db,
            &Query {
                text: Some("report".to_string()),
                filter: Filter::Open,
                sort: Sort::Due,
            },
        );

        assert_eq!(titles, ["y report", "x report"]);
    }
}
