mod command;
mod footer;
mod input;
mod key;
mod keymap;
mod tree;

use std::collections::HashSet;
use std::error::Error;
use std::path::PathBuf;

use engine::{Db, Task};
use ratatui::crossterm::event;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListState, Paragraph};

use crate::command::id;
use crate::key::key_from_event;

fn main() -> Result<(), Box<dyn Error>> {
    // XDG path resolution is not implemented yet; until then --db is mandatory.
    let db_path = parse_db_arg().ok_or("usage: dandori --db <path>")?;
    let db = Db::open(&db_path)?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &db);
    ratatui::restore();
    result
}

fn parse_db_arg() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--db" {
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
}

struct App {
    tasks: Vec<Task>,
    expanded: HashSet<i64>,
    rows: Vec<tree::Row>,
    /// Index into `rows`, i.e. cursor position among visible lines.
    selected: usize,
    /// Task shown as the view root; its children render at depth 0. View
    /// state only, never persisted.
    zoom_root: Option<i64>,
    mode: Mode,
    keymap: keymap::Keymap,
    dispatcher: keymap::Dispatcher,
    should_quit: bool,
}

impl App {
    fn new(tasks: Vec<Task>) -> Self {
        let expanded = HashSet::new();
        let rows = tree::build_visible_rows(&tasks, &expanded, None);
        Self {
            tasks,
            expanded,
            rows,
            selected: 0,
            zoom_root: None,
            mode: Mode::Tree,
            keymap: keymap::Keymap::default(),
            dispatcher: keymap::Dispatcher::default(),
            should_quit: false,
        }
    }

    fn context(&self) -> command::Context {
        match self.mode {
            Mode::Tree => command::Context::Tree,
            Mode::Input { .. } => command::Context::Input,
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
        match self.mode {
            Mode::Tree => {
                if let Some(command) =
                    self.dispatcher
                        .key(&self.keymap, command::Context::Tree, key)
                {
                    self.run_command(command);
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
        }
        Ok(())
    }

    fn submit(&mut self, db: &Db, action: InputAction, title: &str) -> Result<(), engine::Error> {
        match action {
            InputAction::Create(target) => {
                let task = db.create_task(target.parent_id, title, target.after)?;
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
    let mut app = App::new(db.list_all()?);
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
    let input_height = if matches!(app.mode, Mode::Input { .. }) {
        1
    } else {
        0
    };
    let header_height = if app.zoom_root.is_some() { 1 } else { 0 };
    let [header_area, list_area, input_area, footer_area] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(0),
        Constraint::Length(input_height),
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

    let list = List::new(app.rows.iter().map(|row| {
        let task = &app.tasks[row.task_index];
        let is_expanded = app.expanded.contains(&task.id);
        format!(
            "{}{}",
            tree::row_prefix(row.depth, row.has_children, is_expanded),
            task.title
        )
    }))
    .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default();
    if !app.rows.is_empty() {
        list_state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    if let Mode::Input { editor, action } = &app.mode {
        frame.render_widget(
            Paragraph::new(input_line(action.prompt(), editor)),
            input_area,
        );
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
            status: "todo".to_string(),
            due: None,
            log: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
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
        let mut app = App::new(vec![
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
        let mut app = App::new(vec![
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
        let mut app = App::new(vec![
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
        let mut app = App::new(vec![task(1, None, 0), task(2, None, 1)]);
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
        let mut app = App::new(vec![]);

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
        let mut app = App::new(vec![task(1, None, 0), task(2, None, 1)]);
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
        let mut app = App::new(vec![]);

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
        db.create_task(None, "other", None).unwrap();
        let target = db.create_task(None, "設計", None).unwrap();
        let mut app = App::new(db.list_all().unwrap());
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
        let target = db.create_task(None, "keep", None).unwrap();
        let mut app = App::new(db.list_all().unwrap());

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

    // Tests the defensive fallback when the zoom root disappears.
    // Given: a view zoomed on a task id that no longer exists in the task
    //        list (e.g. removed by a future delete feature)
    // When: rows are rebuilt
    // Then: the zoom is dropped so the view falls back to the real roots
    //       instead of rendering an empty screen
    #[test]
    fn rebuild_clears_zoom_when_root_is_gone() {
        let mut app = App::new(vec![task(1, None, 0)]);
        app.zoom_root = Some(999);

        app.rebuild_rows();

        assert_eq!(app.zoom_root, None);
        assert_eq!(selected_id(&app), Some(1));
    }
}
