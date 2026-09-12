mod color;
mod command;
mod footer;
mod input;
mod key;
mod keymap;
mod render;
mod status_cycle;
mod status_manage;
mod tree;

use std::collections::HashSet;
use std::error::Error;
use std::path::PathBuf;

use engine::{Db, Status, Task};
use ratatui::crossterm::event;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListState, Paragraph};

use crate::command::id;
use crate::key::key_from_event;

fn main() -> Result<(), Box<dyn Error>> {
    // XDG path resolution for the database is not implemented yet; until
    // then --db is mandatory.
    let db_path = parse_path_flag("--db").ok_or("usage: dandori --db <path>")?;
    let db = Db::open(&db_path)?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &db);
    ratatui::restore();
    result
}

fn parse_path_flag(name: &str) -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == name {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

/// What submitting the input line should do. Decided when the input opens,
/// so every key that opens it shares one submit path.
enum InputAction {
    Create(tree::CreateTarget),
    Rename(i64),
}

impl InputAction {
    fn prompt(&self) -> &'static str {
        match self {
            Self::Create(_) => "New task: ",
            Self::Rename(_) => "Rename: ",
        }
    }
}

enum Mode {
    Tree,
    Input {
        editor: input::Editor,
        action: InputAction,
    },
    StatusSelect {
        task_id: i64,
    },
    StatusManage(status_manage::ManageState),
}

struct App {
    /// Status definitions loaded from the database at startup, in display
    /// order. Reloaded whenever they are edited (management UI, undo).
    statuses: Vec<Status>,
    /// Status applied to newly created tasks.
    default_status_id: i64,
    tasks: Vec<Task>,
    expanded: HashSet<i64>,
    rows: Vec<tree::Row>,
    /// Index into `rows`, i.e. cursor position among visible lines.
    selected: usize,
    /// Task shown as the view root; its children render at depth 0. View
    /// state only, never persisted.
    zoom_root: Option<i64>,
    mode: Mode,
    /// One-line notice shown above the footer (e.g. why a delete was
    /// refused). Cleared by the next key press.
    status_line: Option<String>,
    keymap: keymap::Keymap,
    dispatcher: keymap::Dispatcher,
    should_quit: bool,
}

impl App {
    fn new(tasks: Vec<Task>, statuses: Vec<Status>, default_status_id: i64) -> Self {
        let expanded = HashSet::new();
        let rows = tree::build_visible_rows(&tasks, &expanded, None);
        Self {
            statuses,
            default_status_id,
            tasks,
            expanded,
            rows,
            selected: 0,
            zoom_root: None,
            mode: Mode::Tree,
            status_line: None,
            keymap: keymap::Keymap::default(),
            dispatcher: keymap::Dispatcher::default(),
            should_quit: false,
        }
    }

    fn context(&self) -> command::Context {
        match self.mode {
            Mode::Tree => command::Context::Tree,
            Mode::Input { .. } => command::Context::Input,
            Mode::StatusSelect { .. } => command::Context::StatusSelect,
            // While a cell edit or the new-status label is being typed, the
            // active keys are the text-input ones, so show those hints.
            Mode::StatusManage(ref state) => match state.editing {
                status_manage::Editing::Cell(_) | status_manage::Editing::NewStatus(_) => {
                    command::Context::Input
                }
                _ => command::Context::StatusManage,
            },
        }
    }

    fn rebuild_rows(&mut self) {
        // A zoom root that vanished from the task list would render an
        // empty, inescapable view; dropping the zoom is the safe fallback.
        if let Some(id) = self.zoom_root
            && !self.tasks.iter().any(|task| task.id == id)
        {
            self.zoom_root = None;
        }
        self.rows = tree::build_visible_rows(&self.tasks, &self.expanded, self.zoom_root);
        // Collapsing can shrink the row list past the cursor.
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn reload(&mut self, db: &Db) -> Result<(), engine::Error> {
        self.tasks = db.list_all()?;
        self.rebuild_rows();
        Ok(())
    }

    /// Moves the cursor to the visible row showing `task_id`, if any.
    fn select_task(&mut self, task_id: i64) {
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| self.tasks[row.task_index].id == task_id)
        {
            self.selected = index;
        }
    }

    fn handle_key(&mut self, db: &Db, key: key::Key) -> Result<(), engine::Error> {
        // Notices live until the next key press; handlers below may set a
        // fresh one for this key.
        self.status_line = None;
        match self.mode {
            Mode::Tree => {
                if let Some(command) =
                    self.dispatcher
                        .key(&self.keymap, command::Context::Tree, key)
                {
                    // These write to the database, which run_command (shared
                    // with pure view commands and their tests) cannot reach.
                    match command {
                        id::STATUS_NEXT => self.cycle_status(db, status_cycle::Direction::Next)?,
                        id::STATUS_PREV => self.cycle_status(db, status_cycle::Direction::Prev)?,
                        _ => self.run_command(command),
                    }
                }
            }
            Mode::Input { .. } => {
                // The editor consumes the state, so take it out of the mode
                // first; every branch below decides the next mode explicitly.
                let Mode::Input { editor, action } = std::mem::replace(&mut self.mode, Mode::Tree)
                else {
                    unreachable!("mode was just matched as Input");
                };
                match editor.handle_key(key) {
                    input::EditResult::Continue(editor) => {
                        self.mode = Mode::Input { editor, action }
                    }
                    input::EditResult::Submitted(title) => {
                        let title = title.trim();
                        // An empty title is treated as a cancel; a blank
                        // title would only produce noise to clean up.
                        if !title.is_empty() {
                            self.submit(db, action, title)?;
                        }
                    }
                    input::EditResult::Cancelled => {}
                }
            }
            Mode::StatusManage(_) => {
                // The handler needs the state and &mut self at once, so take
                // the state out; it is restored unless the modal closes.
                let Mode::StatusManage(mut state) = std::mem::replace(&mut self.mode, Mode::Tree)
                else {
                    unreachable!("mode was just matched as StatusManage");
                };
                if self.handle_manage_key(db, &mut state, key)? {
                    self.mode = Mode::StatusManage(state);
                }
            }
            Mode::StatusSelect { task_id } => match key {
                key::Key::Esc => self.mode = Mode::Tree,
                key::Key::Char(c) => {
                    // Keys come from the user's status definitions, not the
                    // keymap; unbound keys are ignored so a typo cannot
                    // close the menu or change anything.
                    let status_id = self
                        .statuses
                        .iter()
                        .find(|status| status.key == c)
                        .map(|status| status.id);
                    if let Some(status_id) = status_id {
                        db.set_status(task_id, status_id)?;
                        self.mode = Mode::Tree;
                        self.reload(db)?;
                        self.select_task(task_id);
                    }
                }
                _ => {}
            },
        }
        Ok(())
    }

    /// Moves the selected task's status one step through the display order,
    /// wrapping at both ends, then reloads to reflect the change.
    fn cycle_status(
        &mut self,
        db: &Db,
        direction: status_cycle::Direction,
    ) -> Result<(), engine::Error> {
        let Some(row) = self.rows.get(self.selected) else {
            return Ok(());
        };
        let task = &self.tasks[row.task_index];
        let Some(status_id) =
            status_cycle::adjacent_status_id(&self.statuses, task.status_id, direction)
        else {
            return Ok(());
        };
        let task_id = task.id;
        db.set_status(task_id, status_id)?;
        self.reload(db)?;
        self.select_task(task_id);
        Ok(())
    }

    /// Handles one key inside the status-management modal. Returns whether
    /// the modal stays open.
    fn handle_manage_key(
        &mut self,
        db: &Db,
        state: &mut status_manage::ManageState,
        key: key::Key,
    ) -> Result<bool, engine::Error> {
        // An active capture consumes the state; every branch below decides
        // the next editing state explicitly.
        match std::mem::take(&mut state.editing) {
            status_manage::Editing::None => {
                if let Some(command) =
                    self.dispatcher
                        .key(&self.keymap, command::Context::StatusManage, key)
                {
                    return self.run_manage_command(db, state, command);
                }
            }
            status_manage::Editing::Cell(editor) => match editor.handle_key(key) {
                input::EditResult::Continue(editor) => {
                    state.editing = status_manage::Editing::Cell(editor)
                }
                input::EditResult::Submitted(text) => {
                    self.submit_cell_edit(db, state, text.trim())?
                }
                input::EditResult::Cancelled => {}
            },
            status_manage::Editing::NewStatus(editor) => match editor.handle_key(key) {
                input::EditResult::Continue(editor) => {
                    state.editing = status_manage::Editing::NewStatus(editor)
                }
                input::EditResult::Submitted(text) => {
                    let label = text.trim();
                    // A blank label is treated as a cancel, like elsewhere.
                    if !label.is_empty() {
                        self.submit_new_status(db, state, label)?;
                    }
                }
                input::EditResult::Cancelled => {}
            },
            status_manage::Editing::KeyCapture => match key {
                key::Key::Esc => {}
                key::Key::Char(c) => {
                    let status = &self.statuses[state.row];
                    if status_manage::key_taken(&self.statuses, c, status.id) {
                        self.status_line =
                            Some(format!("key `{c}` is already used by another status"));
                    } else {
                        db.update_status_key(status.id, c)?;
                        self.reload_statuses(db)?;
                    }
                }
                // Non-character keys cannot be a status key; keep waiting.
                _ => state.editing = status_manage::Editing::KeyCapture,
            },
        }
        Ok(true)
    }

    /// Runs a dispatched modal command. Returns whether the modal stays
    /// open.
    fn run_manage_command(
        &mut self,
        db: &Db,
        state: &mut status_manage::ManageState,
        command: command::CommandId,
    ) -> Result<bool, engine::Error> {
        match command {
            id::MANAGE_CLOSE => return Ok(false),
            id::MANAGE_ROW_NEXT => state.move_down(self.statuses.len()),
            id::MANAGE_ROW_PREV => state.move_up(),
            id::MANAGE_COL_PREV => state.col = state.col.left(),
            id::MANAGE_COL_NEXT => state.col = state.col.right(),
            id::MANAGE_EDIT => {
                let status = &self.statuses[state.row];
                match state.col {
                    status_manage::Column::Label => {
                        state.editing =
                            status_manage::Editing::Cell(input::Editor::with_text(&status.label))
                    }
                    status_manage::Column::Color => {
                        state.editing =
                            status_manage::Editing::Cell(input::Editor::with_text(&status.color))
                    }
                    // Three fixed variants make a toggle faster than any
                    // input prompt.
                    status_manage::Column::Kind => {
                        let next = status_manage::toggle_kind(status.kind);
                        db.update_status_kind(status.id, next)?;
                        self.reload_statuses(db)?;
                    }
                    status_manage::Column::Key => {
                        state.editing = status_manage::Editing::KeyCapture
                    }
                }
            }
            id::MANAGE_ADD => {
                // Refuse up front: without a free key the new status could
                // never be reached from the status-select menu.
                if status_manage::free_key(&self.statuses).is_some() {
                    state.editing = status_manage::Editing::NewStatus(input::Editor::new());
                } else {
                    self.status_line =
                        Some("cannot add a status: every key a-z is already in use".to_string());
                }
            }
            id::MANAGE_DELETE => match db.delete_status(self.statuses[state.row].id) {
                Ok(()) => {
                    self.reload_statuses(db)?;
                    state.clamp_row(self.statuses.len());
                }
                // Refusals are user-visible outcomes, not failures: show why
                // and keep the modal open.
                Err(engine::Error::StatusInUse { count }) => {
                    self.status_line =
                        Some(format!("cannot delete: {count} task(s) use this status"));
                }
                Err(engine::Error::CannotDeleteLastStatus) => {
                    self.status_line = Some("cannot delete the last status".to_string());
                }
                Err(engine::Error::CannotDeleteDefaultStatus) => {
                    self.status_line = Some("cannot delete the default status".to_string());
                }
                Err(other) => return Err(other),
            },
            id::MANAGE_MOVE_DOWN => self.move_status_row(db, state, engine::StatusMove::Down)?,
            id::MANAGE_MOVE_UP => self.move_status_row(db, state, engine::StatusMove::Up)?,
            id::MANAGE_SET_DEFAULT => {
                db.set_default_status(self.statuses[state.row].id)?;
                self.reload_statuses(db)?;
            }
            _ => {}
        }
        Ok(true)
    }

    fn submit_cell_edit(
        &mut self,
        db: &Db,
        state: &status_manage::ManageState,
        text: &str,
    ) -> Result<(), engine::Error> {
        // A blank value is treated as a cancel, like elsewhere.
        if text.is_empty() {
            return Ok(());
        }
        let id = self.statuses[state.row].id;
        match state.col {
            status_manage::Column::Label => db.update_status_label(id, text)?,
            status_manage::Column::Color => {
                // Unknown names are stored anyway (they only degrade to the
                // default color at render time), but warn about the typo.
                db.update_status_color(id, text)?;
                if color::color_from_name(text) == ratatui::style::Color::Reset {
                    self.status_line = Some(format!(
                        "unknown color `{text}`; it will render as the terminal default"
                    ));
                }
            }
            // Kind and key cells never open a text edit.
            _ => {}
        }
        self.reload_statuses(db)
    }

    fn submit_new_status(
        &mut self,
        db: &Db,
        state: &mut status_manage::ManageState,
        label: &str,
    ) -> Result<(), engine::Error> {
        let Some(key) = status_manage::free_key(&self.statuses) else {
            // Guarded when the input opened; only reachable if the statuses
            // changed underneath, so just report it.
            self.status_line =
                Some("cannot add a status: every key a-z is already in use".to_string());
            return Ok(());
        };
        let created = db.create_status(label, engine::StatusKind::Open, "gray", key)?;
        self.reload_statuses(db)?;
        // Land on the new row so follow-up edits (kind, color) are a single
        // keystroke away.
        state.row = self
            .statuses
            .iter()
            .position(|s| s.id == created.id)
            .unwrap_or(0);
        state.col = status_manage::Column::Label;
        Ok(())
    }

    fn move_status_row(
        &mut self,
        db: &Db,
        state: &mut status_manage::ManageState,
        direction: engine::StatusMove,
    ) -> Result<(), engine::Error> {
        let id = self.statuses[state.row].id;
        db.move_status(id, direction)?;
        self.reload_statuses(db)?;
        // The cursor follows the status it was on, not the table position.
        if let Some(position) = self.statuses.iter().position(|s| s.id == id) {
            state.row = position;
        }
        Ok(())
    }

    /// Re-reads the status list and the default after any status edit, so
    /// the table, the tree rows and new-task creation all see the change.
    fn reload_statuses(&mut self, db: &Db) -> Result<(), engine::Error> {
        self.statuses = db.list_statuses()?;
        self.default_status_id = db.default_status_id()?;
        Ok(())
    }

    fn submit(&mut self, db: &Db, action: InputAction, title: &str) -> Result<(), engine::Error> {
        match action {
            InputAction::Create(target) => {
                let task = db.create_task(
                    target.parent_id,
                    title,
                    target.after,
                    self.default_status_id,
                )?;
                // Ensure the new task is visible: a new child may sit under
                // a still-collapsed parent (for siblings the parent is
                // already expanded).
                if let Some(parent_id) = target.parent_id {
                    self.expanded.insert(parent_id);
                }
                self.reload(db)?;
                self.select_task(task.id);
            }
            InputAction::Rename(id) => {
                db.rename_task(id, title)?;
                self.reload(db)?;
                // Reloading rebuilds the rows; put the cursor back on the
                // task that was just renamed.
                self.select_task(id);
            }
        }
        Ok(())
    }

    fn run_command(&mut self, command: command::CommandId) {
        match command {
            id::QUIT => self.should_quit = true,
            id::SELECT_NEXT => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
            }
            id::SELECT_PREV => self.selected = self.selected.saturating_sub(1),
            id::SELECT_FIRST => self.selected = 0,
            id::SELECT_LAST => self.selected = self.rows.len().saturating_sub(1),
            id::CREATE_TASK => {
                self.mode = Mode::Input {
                    editor: input::Editor::new(),
                    action: InputAction::Create(tree::sibling_target(
                        &self.tasks,
                        &self.rows,
                        self.selected,
                        self.zoom_root,
                    )),
                }
            }
            id::CREATE_CHILD => {
                self.mode = Mode::Input {
                    editor: input::Editor::new(),
                    action: InputAction::Create(tree::child_target(
                        &self.tasks,
                        &self.rows,
                        self.selected,
                        self.zoom_root,
                    )),
                }
            }
            id::RENAME_TASK => {
                if let Some(row) = self.rows.get(self.selected) {
                    let task = &self.tasks[row.task_index];
                    self.mode = Mode::Input {
                        editor: input::Editor::with_text(&task.title),
                        action: InputAction::Rename(task.id),
                    };
                }
            }
            id::STATUS_MANAGE => {
                // Statuses can never be empty (the last row is undeletable),
                // but an empty table would leave the cursor nowhere to sit.
                if !self.statuses.is_empty() {
                    self.mode = Mode::StatusManage(status_manage::ManageState::new());
                }
            }
            id::SET_STATUS => {
                if let Some(row) = self.rows.get(self.selected) {
                    self.mode = Mode::StatusSelect {
                        task_id: self.tasks[row.task_index].id,
                    };
                }
            }
            id::ZOOM_IN => {
                if let Some(row) = self.rows.get(self.selected) {
                    self.zoom_root = Some(self.tasks[row.task_index].id);
                    self.rebuild_rows();
                    self.selected = 0;
                }
            }
            id::ZOOM_OUT => {
                if let Some(old_root) = self.zoom_root {
                    self.zoom_root = self
                        .tasks
                        .iter()
                        .find(|task| task.id == old_root)
                        .and_then(|task| task.parent_id);
                    self.rebuild_rows();
                    // Landing back on the subtree we just left keeps the
                    // cursor where the user's attention is.
                    self.select_task(old_root);
                }
            }
            id::TOGGLE_EXPAND => {
                if let Some(row) = self.rows.get(self.selected)
                    && row.has_children
                {
                    let id = self.tasks[row.task_index].id;
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                    self.rebuild_rows();
                }
            }
            _ => {}
        }
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, db: &Db) -> Result<(), Box<dyn Error>> {
    let mut app = App::new(db.list_all()?, db.list_statuses()?, db.default_status_id()?);
    while !app.should_quit {
        terminal.draw(|frame| draw(frame, &app))?;
        if let event::Event::Key(key_event) = event::read()?
            && key_event.kind == event::KeyEventKind::Press
            && let Some(key) = key_from_event(&key_event)
        {
            app.handle_key(db, key)?;
        }
    }
    Ok(())
}

fn draw(frame: &mut ratatui::Frame, app: &App) {
    // The line above the footer doubles as the text input and the
    // status-select candidate list.
    let input_height = match app.mode {
        Mode::Tree => 0,
        Mode::Input { .. } | Mode::StatusSelect { .. } => 1,
        Mode::StatusManage(ref state) => match state.editing {
            status_manage::Editing::None => 0,
            _ => 1,
        },
    };
    let header_height = if app.zoom_root.is_some() { 1 } else { 0 };
    let message_height = if app.status_line.is_some() { 1 } else { 0 };
    let [
        header_area,
        list_area,
        input_area,
        message_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(message_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    if let Some(zoom_root) = app.zoom_root {
        frame.render_widget(
            Paragraph::new(tree::breadcrumb(&app.tasks, zoom_root))
                .style(Style::default().add_modifier(Modifier::DIM)),
            header_area,
        );
    }

    if let Mode::StatusManage(state) = &app.mode {
        // The modal replaces the task list wholesale; tree keys are inert
        // while it is open, so showing stale tree state would only mislead.
        frame.render_widget(
            Paragraph::new(render::manage_table_lines(
                &app.statuses,
                state.row,
                state.col,
            )),
            list_area,
        );
    } else {
        let list = List::new(app.rows.iter().map(|row| {
            let task = &app.tasks[row.task_index];
            let is_expanded = app.expanded.contains(&task.id);
            render::task_line(
                tree::row_prefix(row.depth, row.has_children, is_expanded),
                task,
                &app.statuses,
            )
        }))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut list_state = ListState::default();
        if !app.rows.is_empty() {
            list_state.select(Some(app.selected));
        }
        frame.render_stateful_widget(list, list_area, &mut list_state);
    }

    if let Mode::Input { editor, action } = &app.mode {
        frame.render_widget(
            Paragraph::new(input_line(action.prompt(), editor)),
            input_area,
        );
    }
    if let Mode::StatusSelect { .. } = &app.mode {
        frame.render_widget(
            Paragraph::new(render::status_menu_line(&app.statuses)),
            input_area,
        );
    }
    if let Mode::StatusManage(state) = &app.mode {
        match &state.editing {
            status_manage::Editing::Cell(editor) => {
                let prompt = match state.col {
                    status_manage::Column::Label => "Label: ",
                    status_manage::Column::Color => "Color: ",
                    _ => "Edit: ",
                };
                frame.render_widget(Paragraph::new(input_line(prompt, editor)), input_area);
            }
            status_manage::Editing::NewStatus(editor) => {
                frame.render_widget(
                    Paragraph::new(input_line("New status: ", editor)),
                    input_area,
                );
            }
            status_manage::Editing::KeyCapture => {
                frame.render_widget(
                    Paragraph::new("Press a key for this status (Esc cancels)"),
                    input_area,
                );
            }
            status_manage::Editing::None => {}
        }
    }

    if let Some(message) = &app.status_line {
        frame.render_widget(Paragraph::new(message.as_str()), message_area);
    }

    let hints = footer::footer_line(
        command::COMMANDS,
        &app.keymap,
        app.context(),
        footer_area.width as usize,
    );
    frame.render_widget(
        Paragraph::new(hints).style(Style::default().add_modifier(Modifier::DIM)),
        footer_area,
    );
}

/// Renders the input field with a block cursor. Highlighting the char at the
/// cursor avoids display-width math for positioning a real terminal cursor.
fn input_line<'a>(prompt: &'a str, editor: &'a input::Editor) -> Line<'a> {
    let (before, after) = editor.text().split_at(editor.cursor());
    let cursor_len = after.chars().next().map_or(0, char::len_utf8);
    let (at_cursor, rest) = after.split_at(cursor_len);
    let cursor_display = if at_cursor.is_empty() { " " } else { at_cursor };
    Line::from(vec![
        Span::raw(prompt),
        Span::raw(before),
        Span::styled(
            cursor_display,
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        Span::raw(rest),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: i64, parent_id: Option<i64>, display_order: i64) -> Task {
        Task {
            id,
            parent_id,
            display_order,
            title: format!("task {id}"),
            status_id: 1,
            due: None,
            log: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn default_status(db: &Db) -> i64 {
        db.default_status_id().unwrap()
    }

    /// Builds an App wired to `db`'s statuses, mirroring startup.
    fn app_for(db: &Db, tasks: Vec<Task>) -> App {
        App::new(
            tasks,
            db.list_statuses().unwrap(),
            db.default_status_id().unwrap(),
        )
    }

    /// Builds an App over plain task fixtures; statuses come from a fresh
    /// seeded database because they always originate there.
    fn test_app(tasks: Vec<Task>) -> App {
        app_for(&Db::open_in_memory().unwrap(), tasks)
    }

    fn selected_id(app: &App) -> Option<i64> {
        app.rows
            .get(app.selected)
            .map(|row| app.tasks[row.task_index].id)
    }

    // Tests zooming in on the selected task.
    // Given: roots 1 and 2 where root 2 (selected) has a child 21
    // When: the zoom-in command runs
    // Then: task 2 becomes the zoom root and the cursor moves to the first
    //       row of the zoomed view (its child 21)
    #[test]
    fn zoom_in_zooms_on_selection_and_selects_first_row() {
        let mut app = test_app(vec![
            task(1, None, 0),
            task(2, None, 1),
            task(21, Some(2), 0),
        ]);
        app.selected = 1;

        app.run_command(id::ZOOM_IN);

        assert_eq!(app.zoom_root, Some(2));
        assert_eq!(app.selected, 0);
        assert_eq!(selected_id(&app), Some(21));
    }

    // Tests zooming out one level.
    // Given: a chain 1 > 11 > 111 zoomed on the middle task 11
    // When: the zoom-out command runs
    // Then: the zoom root becomes the parent task 1, and the cursor lands
    //       on the row showing the previous zoom root 11
    #[test]
    fn zoom_out_steps_to_parent_and_reselects_old_root() {
        let mut app = test_app(vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ]);
        app.zoom_root = Some(11);
        app.rebuild_rows();

        app.run_command(id::ZOOM_OUT);

        assert_eq!(app.zoom_root, Some(1));
        assert_eq!(selected_id(&app), Some(11));
    }

    // Tests zooming out from a top-level zoom root.
    // Given: roots 1 and 2 zoomed on root 1
    // When: the zoom-out command runs
    // Then: the zoom is cleared entirely and the cursor lands on the row
    //       showing the previous zoom root 1
    #[test]
    fn zoom_out_from_root_level_clears_zoom() {
        let mut app = test_app(vec![
            task(1, None, 0),
            task(2, None, 1),
            task(11, Some(1), 0),
        ]);
        app.zoom_root = Some(1);
        app.rebuild_rows();

        app.run_command(id::ZOOM_OUT);

        assert_eq!(app.zoom_root, None);
        assert_eq!(selected_id(&app), Some(1));
    }

    // Tests zoom-out when not zoomed.
    // Given: an unzoomed view with the cursor on the second root
    // When: the zoom-out command runs
    // Then: nothing changes (no zoom, cursor stays put)
    #[test]
    fn zoom_out_without_zoom_is_a_no_op() {
        let mut app = test_app(vec![task(1, None, 0), task(2, None, 1)]);
        app.selected = 1;

        app.run_command(id::ZOOM_OUT);

        assert_eq!(app.zoom_root, None);
        assert_eq!(app.selected, 1);
    }

    // Tests zoom-in on an empty view.
    // Given: no tasks at all
    // When: the zoom-in command runs
    // Then: nothing changes (there is no task to zoom on)
    #[test]
    fn zoom_in_on_empty_view_is_a_no_op() {
        let mut app = test_app(vec![]);

        app.run_command(id::ZOOM_IN);

        assert_eq!(app.zoom_root, None);
    }

    // Tests that the rename command opens a prefilled input.
    // Given: two root tasks with the cursor on the second one ("task 2")
    // When: the rename command runs
    // Then: the mode becomes Input targeting task 2 for rename, with the
    //       editor prefilled with the current title and the cursor at its end
    #[test]
    fn rename_command_opens_input_prefilled_with_title() {
        let mut app = test_app(vec![task(1, None, 0), task(2, None, 1)]);
        app.selected = 1;

        app.run_command(id::RENAME_TASK);

        let Mode::Input { editor, action } = &app.mode else {
            panic!("rename should enter input mode");
        };
        assert!(matches!(action, InputAction::Rename(2)));
        assert_eq!(editor.text(), "task 2");
        assert_eq!(editor.cursor(), "task 2".len());
    }

    // Tests the rename command on an empty view.
    // Given: no tasks at all
    // When: the rename command runs
    // Then: the mode stays Tree (there is nothing to rename)
    #[test]
    fn rename_command_on_empty_view_is_a_no_op() {
        let mut app = test_app(vec![]);

        app.run_command(id::RENAME_TASK);

        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests submitting a rename end to end.
    // Given: a database task titled "設計" opened for rename
    // When: "する" is typed and the input is confirmed
    // Then: the task is renamed to "設計する" both in the database and in
    //       the reloaded view, and the cursor stays on the same task
    #[test]
    fn submitting_rename_updates_task_and_keeps_selection() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "other", None, default_status(&db))
            .unwrap();
        let target = db
            .create_task(None, "設計", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(target.id);

        app.run_command(id::RENAME_TASK);
        for c in "する".chars() {
            app.handle_key(&db, key::Key::Char(c)).unwrap();
        }
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(selected_id(&app), Some(target.id));
        let titles: Vec<String> = db
            .list_all()
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, ["other", "設計する"]);
    }

    // Tests that submitting a blank rename cancels instead of renaming.
    // Given: a database task titled "keep" opened for rename
    // When: the prefilled title is erased down to a single space and confirmed
    // Then: the task keeps its original title and the mode returns to Tree
    #[test]
    fn submitting_blank_rename_is_a_cancel() {
        let db = Db::open_in_memory().unwrap();
        let target = db
            .create_task(None, "keep", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        app.run_command(id::RENAME_TASK);
        for _ in 0.."keep".len() {
            app.handle_key(&db, key::Key::Backspace).unwrap();
        }
        app.handle_key(&db, key::Key::Char(' ')).unwrap();
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        let reloaded = db.list_all().unwrap();
        assert_eq!(reloaded[0].id, target.id);
        assert_eq!(reloaded[0].title, "keep");
    }

    // Tests that the input prompt names the action being performed.
    // Given: a create action and a rename action
    // When: asking each for its prompt
    // Then: they differ so the user can tell what confirming will do
    #[test]
    fn prompt_distinguishes_create_from_rename() {
        let create = InputAction::Create(tree::CreateTarget {
            parent_id: None,
            after: None,
        });

        assert_eq!(create.prompt(), "New task: ");
        assert_eq!(InputAction::Rename(1).prompt(), "Rename: ");
    }

    // Tests that the set-status command opens the status-select menu.
    // Given: two root tasks with the cursor on the second one
    // When: the set-status command runs
    // Then: the mode becomes StatusSelect targeting the selected task, and
    //       the active context switches accordingly
    #[test]
    fn set_status_command_opens_menu_for_selected_task() {
        let mut app = test_app(vec![task(1, None, 0), task(2, None, 1)]);
        app.selected = 1;

        app.run_command(id::SET_STATUS);

        assert!(matches!(app.mode, Mode::StatusSelect { task_id: 2 }));
        assert_eq!(app.context(), command::Context::StatusSelect);
    }

    // Tests the set-status command on an empty view.
    // Given: no tasks at all
    // When: the set-status command runs
    // Then: the mode stays Tree (there is nothing to change)
    #[test]
    fn set_status_command_on_empty_view_is_a_no_op() {
        let mut app = test_app(vec![]);

        app.run_command(id::SET_STATUS);

        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests applying a status from the menu end to end.
    // Given: two database tasks with the status menu open for the second,
    //        where "d" is the seeded key for the status labelled "進行中"
    // When: the "d" key is pressed
    // Then: the task's status id becomes that status in the database, the
    //       mode returns to Tree, and the cursor stays on the same task
    #[test]
    fn status_key_applies_status_and_keeps_selection() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "other", None, default_status(&db))
            .unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(target.id);
        app.run_command(id::SET_STATUS);

        app.handle_key(&db, key::Key::Char('d')).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(selected_id(&app), Some(target.id));
        let doing = app.statuses.iter().find(|s| s.key == 'd').unwrap().id;
        let status_ids: Vec<i64> = db
            .list_all()
            .unwrap()
            .into_iter()
            .map(|t| t.status_id)
            .collect();
        assert_eq!(status_ids, [default_status(&db), doing]);
    }

    // Tests cancelling the status menu.
    // Given: a database task with the status menu open for it
    // When: Esc is pressed
    // Then: the mode returns to Tree and the status is unchanged
    #[test]
    fn esc_cancels_status_menu_without_change() {
        let db = Db::open_in_memory().unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.run_command(id::SET_STATUS);

        app.handle_key(&db, key::Key::Esc).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(db.list_all().unwrap()[0].status_id, default_status(&db));
        assert_eq!(selected_id(&app), Some(target.id));
    }

    // Tests that keys not bound to any status are ignored.
    // Given: a database task with the status menu open, where "z" is no
    //        seeded status key
    // When: "z" is pressed
    // Then: the menu stays open and the status is unchanged
    #[test]
    fn unbound_key_in_status_menu_is_ignored() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.run_command(id::SET_STATUS);

        app.handle_key(&db, key::Key::Char('z')).unwrap();

        assert!(matches!(app.mode, Mode::StatusSelect { .. }));
        assert_eq!(db.list_all().unwrap()[0].status_id, default_status(&db));
    }

    // Tests cycling the selected task's status forward without the menu.
    // Given: two database tasks on the default status, cursor on the second
    // When: Shift+j ("J") is pressed in the Tree context
    // Then: that task moves to the status following the default one in
    //       display order, the mode stays Tree, and the cursor stays put
    #[test]
    fn shift_j_advances_status_to_next_in_display_order() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "other", None, default_status(&db))
            .unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(target.id);

        app.handle_key(&db, key::Key::Char('J')).unwrap();

        let position = app
            .statuses
            .iter()
            .position(|s| s.id == default_status(&db))
            .unwrap();
        let expected = app.statuses[position + 1].id;
        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(selected_id(&app), Some(target.id));
        let stored = db
            .list_all()
            .unwrap()
            .into_iter()
            .find(|t| t.id == target.id)
            .unwrap();
        assert_eq!(stored.status_id, expected);
        // The untouched sibling keeps its status.
        assert_eq!(db.list_all().unwrap()[0].status_id, default_status(&db));
    }

    // Tests forward wrap-around of the status cycle.
    // Given: a database task already on the last status in display order
    // When: Shift+j ("J") is pressed
    // Then: the task wraps to the first status in display order
    #[test]
    fn shift_j_from_last_status_wraps_to_first() {
        let db = Db::open_in_memory().unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let statuses = db.list_statuses().unwrap();
        db.set_status(target.id, statuses.last().unwrap().id)
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        app.handle_key(&db, key::Key::Char('J')).unwrap();

        assert_eq!(db.list_all().unwrap()[0].status_id, statuses[0].id);
    }

    // Tests cycling the selected task's status backward.
    // Given: a database task on the second status in display order
    // When: Shift+k ("K") is pressed
    // Then: the task moves back to the first status in display order
    #[test]
    fn shift_k_moves_status_to_previous_in_display_order() {
        let db = Db::open_in_memory().unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let statuses = db.list_statuses().unwrap();
        db.set_status(target.id, statuses[1].id).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        app.handle_key(&db, key::Key::Char('K')).unwrap();

        assert_eq!(db.list_all().unwrap()[0].status_id, statuses[0].id);
    }

    // Tests the status cycle on an empty view.
    // Given: no tasks at all
    // When: Shift+j ("J") is pressed
    // Then: nothing happens and no error is raised
    #[test]
    fn status_cycle_on_empty_view_is_a_no_op() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        app.handle_key(&db, key::Key::Char('J')).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert!(db.list_all().unwrap().is_empty());
    }

    // Tests that new tasks are born with the app's default status.
    // Given: an app whose default status id points at a non-default seeded
    //        status (as if the user had moved the default flag), and an
    //        open create input
    // When: a title is typed and confirmed
    // Then: the created task carries that status id
    #[test]
    fn created_task_uses_default_status() {
        let db = Db::open_in_memory().unwrap();
        let statuses = db.list_statuses().unwrap();
        let ready = statuses.iter().find(|s| !s.is_default).unwrap().id;
        let mut app = App::new(vec![], statuses, ready);

        app.run_command(id::CREATE_TASK);
        app.handle_key(&db, key::Key::Char('t')).unwrap();
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert_eq!(db.list_all().unwrap()[0].status_id, ready);
    }

    use crate::status_manage::{Column, Editing};

    fn open_manage(app: &mut App, db: &Db) {
        app.handle_key(db, key::Key::Char('S')).unwrap();
    }

    fn manage_state(app: &App) -> &status_manage::ManageState {
        let Mode::StatusManage(state) = &app.mode else {
            panic!("expected the status-management modal to be open");
        };
        state
    }

    fn press(app: &mut App, db: &Db, keys: &str) {
        for c in keys.chars() {
            app.handle_key(db, key::Key::Char(c)).unwrap();
        }
    }

    // Tests that Shift+s opens the status-management modal.
    // Given: an app in the Tree mode
    // When: "S" is pressed
    // Then: the mode becomes StatusManage with the cursor on the first
    //       row's label cell, and the active context switches accordingly
    #[test]
    fn s_uppercase_opens_status_manage_modal() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        open_manage(&mut app, &db);

        let state = manage_state(&app);
        assert_eq!(state.row, 0);
        assert_eq!(state.col, Column::Label);
        assert_eq!(app.context(), command::Context::StatusManage);
    }

    // Tests that tree bindings are inert while the modal is open.
    // Given: an open status-management modal over one task
    // When: "r" (tree rename) is pressed
    // Then: the mode stays StatusManage; no rename input opens
    #[test]
    fn tree_keys_do_not_leak_into_manage_modal() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        open_manage(&mut app, &db);

        app.handle_key(&db, key::Key::Char('r')).unwrap();

        assert!(matches!(app.mode, Mode::StatusManage(_)));
    }

    // Tests cell-cursor movement inside the modal.
    // Given: an open modal over the 5 seeded statuses
    // When: moving down twice, up once, right twice past checks, left once
    // Then: j/k clamp within the rows and h/l walk the columns
    #[test]
    fn manage_navigation_moves_selected_cell() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        press(&mut app, &db, "jj");
        assert_eq!(manage_state(&app).row, 2);
        press(&mut app, &db, "k");
        assert_eq!(manage_state(&app).row, 1);
        press(&mut app, &db, "jjjjjj");
        assert_eq!(manage_state(&app).row, 4, "row clamps at the last status");

        press(&mut app, &db, "l");
        assert_eq!(manage_state(&app).col, Column::Kind);
        press(&mut app, &db, "l");
        assert_eq!(manage_state(&app).col, Column::Color);
        press(&mut app, &db, "h");
        assert_eq!(manage_state(&app).col, Column::Kind);
    }

    // Tests renaming a status through the label cell.
    // Given: an open modal with the cursor on the first row's label cell
    //        (seeded label "未着手")
    // When: Enter opens the prefilled editor, "x" is appended and confirmed
    // Then: the label becomes "未着手x" in the database and in the reloaded
    //       app statuses, and the modal stays open
    #[test]
    fn manage_enter_on_label_edits_and_saves() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        app.handle_key(&db, key::Key::Enter).unwrap();
        let state = manage_state(&app);
        let Editing::Cell(editor) = &state.editing else {
            panic!("Enter on the label cell should open a text edit");
        };
        assert_eq!(editor.text(), "未着手", "editor must be prefilled");
        press(&mut app, &db, "x");
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(manage_state(&app).editing, Editing::None));
        assert_eq!(app.statuses[0].label, "未着手x");
        assert_eq!(db.list_statuses().unwrap()[0].label, "未着手x");
    }

    // Tests that submitting a blanked-out label leaves the status alone.
    // Given: a label edit opened on "未着手"
    // When: the prefill is erased entirely and confirmed
    // Then: the label is unchanged (blank input acts as a cancel)
    #[test]
    fn manage_blank_label_submit_is_a_cancel() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        app.handle_key(&db, key::Key::Enter).unwrap();
        for _ in 0.."未着手".chars().count() {
            app.handle_key(&db, key::Key::Backspace).unwrap();
        }
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert_eq!(app.statuses[0].label, "未着手");
        assert_eq!(db.list_statuses().unwrap()[0].label, "未着手");
    }

    // Tests toggling the kind cell.
    // Given: an open modal with the cursor moved to the kind cell of the
    //        first row (seeded kind open)
    // When: Enter is pressed twice
    // Then: the kind steps open→done→cancelled, persisting each time
    #[test]
    fn manage_enter_on_kind_toggles_through_cycle() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "l");

        app.handle_key(&db, key::Key::Enter).unwrap();
        assert_eq!(app.statuses[0].kind, engine::StatusKind::Done);
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert_eq!(app.statuses[0].kind, engine::StatusKind::Cancelled);
        assert_eq!(
            db.list_statuses().unwrap()[0].kind,
            engine::StatusKind::Cancelled
        );
    }

    // Tests editing the color cell to an unknown color name.
    // Given: a color edit opened on the first row (prefill "gray")
    // When: the prefill is replaced with the typo "grean" and confirmed
    // Then: the value is saved anyway (it only degrades to the default
    //       color at render time) and a notice warns about the unknown name
    #[test]
    fn manage_unknown_color_saves_with_warning() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "ll");

        app.handle_key(&db, key::Key::Enter).unwrap();
        for _ in 0.."gray".len() {
            app.handle_key(&db, key::Key::Backspace).unwrap();
        }
        press(&mut app, &db, "grean");
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert_eq!(app.statuses[0].color, "grean");
        assert_eq!(db.list_statuses().unwrap()[0].color, "grean");
        let message = app.status_line.as_deref().unwrap();
        assert!(message.contains("grean"), "notice should name the color");
    }

    // Tests reassigning a status key through the key cell.
    // Given: a key capture opened on the first row (seeded key 't')
    // When: the unused key "z" is pressed
    // Then: the status key becomes 'z' and the capture ends
    #[test]
    fn manage_key_capture_sets_new_key() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "lll");

        app.handle_key(&db, key::Key::Enter).unwrap();
        assert!(matches!(manage_state(&app).editing, Editing::KeyCapture));
        press(&mut app, &db, "z");

        assert!(matches!(manage_state(&app).editing, Editing::None));
        assert_eq!(app.statuses[0].key, 'z');
        assert_eq!(db.list_statuses().unwrap()[0].key, 'z');
    }

    // Tests that a key already used by another status is rejected.
    // Given: a key capture opened on the first row, where "r" is the seeded
    //        key of another status (着手可能)
    // When: "r" is pressed
    // Then: the key stays 't', a notice explains the conflict, and the
    //       capture ends
    #[test]
    fn manage_key_capture_rejects_duplicate_key() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "lll");
        app.handle_key(&db, key::Key::Enter).unwrap();

        press(&mut app, &db, "r");

        assert_eq!(app.statuses[0].key, 't');
        assert_eq!(db.list_statuses().unwrap()[0].key, 't');
        assert!(app.status_line.is_some(), "conflict must be reported");
    }

    // Tests adding a status from the modal.
    // Given: an open modal over the 5 seeded statuses (keys t/r/d/x/c)
    // When: "o" is pressed, "review" is typed and confirmed
    // Then: a 6th status appears at the tail with kind open, color gray and
    //       the first free key 'a', and the cursor moves onto its row
    #[test]
    fn manage_add_creates_status_with_defaults() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        press(&mut app, &db, "o");
        assert!(matches!(manage_state(&app).editing, Editing::NewStatus(_)));
        press(&mut app, &db, "review");
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert_eq!(app.statuses.len(), 6);
        let created = app.statuses.last().unwrap();
        assert_eq!(created.label, "review");
        assert_eq!(created.kind, engine::StatusKind::Open);
        assert_eq!(created.color, "gray");
        assert_eq!(created.key, 'a');
        assert!(!created.is_default);
        assert_eq!(manage_state(&app).row, 5, "cursor follows the new row");
    }

    // Tests the delete guard for a status still in use.
    // Given: one task on the second seeded status and the modal cursor on
    //        that status's row
    // When: "d" is pressed
    // Then: nothing is deleted and the notice reports the using-task count
    #[test]
    fn manage_delete_in_use_shows_count_message() {
        let db = Db::open_in_memory().unwrap();
        let statuses = db.list_statuses().unwrap();
        db.create_task(None, "t", None, statuses[1].id).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        open_manage(&mut app, &db);
        press(&mut app, &db, "j");

        press(&mut app, &db, "d");

        assert_eq!(app.statuses.len(), 5);
        let message = app.status_line.as_deref().unwrap();
        assert!(message.contains('1'), "notice should carry the task count");
    }

    // Tests the delete guard for the default status.
    // Given: the modal cursor on the first row, which is the seeded default
    // When: "d" is pressed
    // Then: nothing is deleted and a notice explains the refusal
    #[test]
    fn manage_delete_default_is_blocked_with_message() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        press(&mut app, &db, "d");

        assert_eq!(app.statuses.len(), 5);
        assert!(app.status_line.is_some());
    }

    // Tests deleting an unprotected status.
    // Given: the modal cursor on the second seeded status, which is neither
    //        the default nor referenced by any task
    // When: "d" is pressed
    // Then: the status disappears from the app list and the database, and
    //       the cursor stays on a valid row
    #[test]
    fn manage_delete_unused_removes_row() {
        let db = Db::open_in_memory().unwrap();
        let victim = db.list_statuses().unwrap()[1].id;
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "j");

        press(&mut app, &db, "d");

        assert_eq!(app.statuses.len(), 4);
        assert!(app.statuses.iter().all(|s| s.id != victim));
        assert!(manage_state(&app).row < app.statuses.len());
    }

    // Tests reordering statuses from the modal.
    // Given: the modal cursor on the first seeded status (未着手)
    // When: Shift+j ("J") is pressed
    // Then: the status swaps with the one below it, both in the app list
    //       and persistently, and the cursor follows the moved row
    #[test]
    fn manage_shift_j_moves_status_down_and_follows_it() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        press(&mut app, &db, "J");

        assert_eq!(app.statuses[0].label, "着手可能");
        assert_eq!(app.statuses[1].label, "未着手");
        assert_eq!(db.list_statuses().unwrap()[0].label, "着手可能");
        assert_eq!(manage_state(&app).row, 1, "cursor follows the moved row");
    }

    // Tests changing the default status from the modal.
    // Given: the modal cursor moved to the second seeded status
    // When: "*" is pressed
    // Then: that status becomes the default (exactly one default row) and
    //       the app's default-for-new-tasks id follows
    #[test]
    fn manage_star_sets_default_status() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "j");
        let target = app.statuses[1].id;

        press(&mut app, &db, "*");

        assert_eq!(app.default_status_id, target);
        let defaults: Vec<i64> = app
            .statuses
            .iter()
            .filter(|s| s.is_default)
            .map(|s| s.id)
            .collect();
        assert_eq!(defaults, [target]);
        assert_eq!(db.default_status_id().unwrap(), target);
    }

    // Tests both ways of leaving the modal.
    // Given: an open status-management modal
    // When: Esc is pressed; the modal is reopened and "q" is pressed
    // Then: both keys return the app to the Tree mode
    #[test]
    fn esc_and_q_close_manage_modal() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        open_manage(&mut app, &db);
        app.handle_key(&db, key::Key::Esc).unwrap();
        assert!(matches!(app.mode, Mode::Tree));

        open_manage(&mut app, &db);
        press(&mut app, &db, "q");
        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests that a notice disappears on the next key press.
    // Given: a notice raised by trying to delete the default status
    // When: any other key ("j") is pressed
    // Then: the notice is gone
    #[test]
    fn status_line_clears_on_next_key() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "d");
        assert!(app.status_line.is_some());

        press(&mut app, &db, "j");

        assert!(app.status_line.is_none());
    }

    // Tests the defensive fallback when the zoom root disappears.
    // Given: a view zoomed on a task id that no longer exists in the task
    //        list (e.g. removed by a future delete feature)
    // When: rows are rebuilt
    // Then: the zoom is dropped so the view falls back to the real roots
    //       instead of rendering an empty screen
    #[test]
    fn rebuild_clears_zoom_when_root_is_gone() {
        let mut app = test_app(vec![task(1, None, 0)]);
        app.zoom_root = Some(999);

        app.rebuild_rows();

        assert_eq!(app.zoom_root, None);
        assert_eq!(selected_id(&app), Some(1));
    }
}
