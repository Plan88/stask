mod color;
mod command;
mod config;
mod external_editor;
mod footer;
mod help;
mod input;
mod key;
mod keymap;
mod keyspec;
mod note;
mod overlay;
mod query_view;
mod render;
mod status_cycle;
mod status_manage;
mod tree;

use std::collections::HashSet;
use std::error::Error;
use std::path::PathBuf;

use engine::{Db, Status, Task};
use ratatui::crossterm::event;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::command::id;
use crate::key::key_from_event;

/// Bounds for the note pane: it may grow to a third of the screen but never
/// squeezes the task list out, and small terminals still get a useful pane.
const NOTE_PANE_MIN_LINES: usize = 8;
const NOTE_PANE_MAX_LINES: usize = 20;

fn main() -> Result<(), Box<dyn Error>> {
    // The config may name the database, so it is read first. A broken config
    // or an unresolvable path is a user mistake, not a crash: explain the
    // whole cause chain and stop before touching the terminal state.
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            eprint_error_chain(&err);
            std::process::exit(1);
        }
    };
    let db_path = match resolve_db_path(&config) {
        Ok(path) => path,
        Err(err) => {
            eprint_error_chain(&err);
            std::process::exit(1);
        }
    };
    let db = Db::open(&db_path)?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &db, config);
    ratatui::restore();
    result
}

fn load_config() -> Result<config::Config, config::Error> {
    let path = match parse_path_flag("--config") {
        Some(path) => path,
        None => config::default_path()?,
    };
    config::load_or_init(&path)
}

/// Resolves the database path and makes sure its directory exists, so a
/// first run on a fresh machine just works.
fn resolve_db_path(config: &config::Config) -> Result<PathBuf, config::Error> {
    let path = config::db_path(parse_path_flag("--db"), config.db_path.as_deref())?;
    config::ensure_db_dir(&path)?;
    Ok(path)
}

/// Prints an error and every cause below it, so e.g. a key-notation
/// problem inside a config value is fully explained.
fn eprint_error_chain(err: &dyn Error) {
    eprintln!("error: {err}");
    let mut source = err.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
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
    SetDue(i64),
}

impl InputAction {
    fn prompt(&self) -> &'static str {
        match self {
            Self::Create(_) => "New task: ",
            Self::Rename(_) => "Rename: ",
            Self::SetDue(_) => "Due (YYYY-MM-DD): ",
        }
    }
}

/// Which direction through the edit history a key asked for.
#[derive(Clone, Copy)]
enum History {
    Undo,
    Redo,
}

impl History {
    fn empty_message(self) -> &'static str {
        match self {
            Self::Undo => "nothing to undo",
            Self::Redo => "nothing to redo",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Undo => "undid",
            Self::Redo => "redid",
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
    /// Waiting for the user to confirm deleting `task_id`'s subtree of
    /// `count` tasks. `y` deletes, any other key cancels.
    ConfirmDelete {
        task_id: i64,
        count: i64,
    },
    /// Picking a filter for the tree view. The same menu opened from the
    /// query view lives inside the query state instead, so it can fall back
    /// to browsing.
    FilterSelect,
    Query(query_view::QueryState),
    Help(help::HelpState),
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
    /// Which tasks the tree and the search show. Defaults to hiding
    /// finished work; view state only, never persisted.
    filter: engine::Filter,
    mode: Mode,
    /// One-line notice shown above the footer (e.g. why a delete was
    /// refused). Cleared by the next key press.
    status_line: Option<String>,
    /// Task whose note should be opened in the external editor. Set by the
    /// edit-note command and consumed by the event loop, which owns the
    /// terminal needed for the handover.
    pending_note_edit: Option<i64>,
    keymap: keymap::Keymap,
    dispatcher: keymap::Dispatcher,
    /// Whether the key-hint footer line is shown. Seeded from the config,
    /// toggled at runtime; view state only, never persisted.
    show_footer: bool,
    should_quit: bool,
    /// Rows the task tree showed on the last draw; sizes half-page jumps.
    /// 0 until the first draw, which the jump step treats as one row.
    list_height: usize,
    /// Rows the modal popup's list showed on the last draw; sizes the
    /// half-page jumps of the query and help views.
    popup_height: usize,
}

impl App {
    fn new(tasks: Vec<Task>, statuses: Vec<Status>, default_status_id: i64) -> Self {
        let mut app = Self {
            statuses,
            default_status_id,
            tasks,
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            zoom_root: None,
            filter: engine::Filter::default(),
            mode: Mode::Tree,
            status_line: None,
            pending_note_edit: None,
            keymap: keymap::Keymap::default(),
            dispatcher: keymap::Dispatcher::default(),
            show_footer: true,
            should_quit: false,
            list_height: 0,
            popup_height: 0,
        };
        app.rebuild_rows();
        app
    }

    /// The local date every overdue check compares against.
    fn today() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
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
            Mode::ConfirmDelete { .. } => command::Context::ConfirmDelete,
            Mode::FilterSelect => command::Context::FilterSelect,
            // While the search text is being typed, the active keys are the
            // text-input ones, so show those hints.
            Mode::Query(ref state) => match state.focus {
                query_view::Focus::Edit => command::Context::Input,
                query_view::Focus::Browse => command::Context::Query,
                query_view::Focus::SortMenu => command::Context::SortSelect,
                query_view::Focus::FilterMenu => command::Context::FilterSelect,
            },
            // Same for the help filter text.
            Mode::Help(ref state) => match state.focus {
                help::Focus::Edit => command::Context::Input,
                help::Focus::Browse => command::Context::Help,
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
        let visible =
            tree::filter_visible_ids(&self.tasks, &self.statuses, self.filter, &Self::today());
        self.rows = tree::build_visible_rows(
            &self.tasks,
            &self.expanded,
            self.zoom_root,
            visible.as_ref(),
        );
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
                        id::TASK_MOVE_UP => self.move_selected(db, engine::TaskMove::Up)?,
                        id::TASK_MOVE_DOWN => self.move_selected(db, engine::TaskMove::Down)?,
                        id::TASK_INDENT => self.indent_selected(db)?,
                        id::TASK_OUTDENT => self.outdent_selected(db)?,
                        id::TASK_DELETE => self.request_delete(db)?,
                        id::UNDO => self.apply_history(db, History::Undo)?,
                        id::REDO => self.apply_history(db, History::Redo)?,
                        id::STATUS_MANAGE => self.open_status_manage(db)?,
                        id::VIEW_SEARCH => self.open_query(db)?,
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
                    input::EditResult::Submitted(text) => self.submit(db, action, &text)?,
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
            Mode::ConfirmDelete { task_id, count } => {
                // The prompt is one-shot: whatever the key, the mode ends.
                self.mode = Mode::Tree;
                if key == key::Key::Char('y') {
                    self.delete_and_reselect(db, task_id)?;
                    self.status_line = Some(format!("deleted {count} task(s) (u to undo)"));
                }
            }
            Mode::FilterSelect => match key {
                key::Key::Esc => self.mode = Mode::Tree,
                key::Key::Char(c) => {
                    // Unbound keys are ignored so a typo cannot close the
                    // menu or change anything (like the status-select menu).
                    if let Some(filter) = query_view::filter_from_key(&self.statuses, c) {
                        self.filter = filter;
                        self.mode = Mode::Tree;
                        self.rebuild_rows();
                    }
                }
                _ => {}
            },
            Mode::Query(_) => {
                // The handler needs the state and &mut self at once, so take
                // the state out; it is restored unless the view closes.
                let Mode::Query(mut state) = std::mem::replace(&mut self.mode, Mode::Tree) else {
                    unreachable!("mode was just matched as Query");
                };
                if self.handle_query_key(db, &mut state, key)? {
                    self.mode = Mode::Query(state);
                }
            }
            Mode::Help(_) => {
                // The handler needs the state and &mut self at once, so take
                // the state out; it is restored unless the view closes.
                let Mode::Help(mut state) = std::mem::replace(&mut self.mode, Mode::Tree) else {
                    unreachable!("mode was just matched as Help");
                };
                if self.handle_help_key(&mut state, key) {
                    self.mode = Mode::Help(state);
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

    fn selected_task_id(&self) -> Option<i64> {
        self.rows
            .get(self.selected)
            .map(|row| self.tasks[row.task_index].id)
    }

    /// Swaps the selected task with its sibling neighbour, keeping the
    /// cursor on the task rather than on its old row.
    fn move_selected(&mut self, db: &Db, direction: engine::TaskMove) -> Result<(), engine::Error> {
        let Some(task_id) = self.selected_task_id() else {
            return Ok(());
        };
        db.move_task(task_id, direction)?;
        self.reload(db)?;
        self.select_task(task_id);
        Ok(())
    }

    /// Makes the selected task a child of its preceding sibling. A no-op for
    /// first siblings, which have nothing above to indent under.
    fn indent_selected(&mut self, db: &Db) -> Result<(), engine::Error> {
        let Some(task_id) = self.selected_task_id() else {
            return Ok(());
        };
        let Some(new_parent) = tree::indent_new_parent(&self.tasks, task_id) else {
            return Ok(());
        };
        db.reparent(task_id, Some(new_parent), None)?;
        // Without expanding the new parent the task would vanish from view.
        self.expanded.insert(new_parent);
        self.reload(db)?;
        self.select_task(task_id);
        Ok(())
    }

    /// Moves the selected task up one level, right after its old parent.
    /// A no-op at the root level and directly under the zoom root.
    fn outdent_selected(&mut self, db: &Db) -> Result<(), engine::Error> {
        let Some(task_id) = self.selected_task_id() else {
            return Ok(());
        };
        let Some(target) = tree::outdent_target(&self.tasks, task_id, self.zoom_root) else {
            return Ok(());
        };
        db.reparent(task_id, target.new_parent, target.after)?;
        self.reload(db)?;
        self.select_task(task_id);
        Ok(())
    }

    /// Brings `task_id` into view and puts the cursor on it: the task may
    /// sit under collapsed ancestors, outside the zoom, or off-cursor. So
    /// the ancestors are expanded and the zoom dropped unless it contains
    /// the task. Shared by undo/redo and the search-result jump.
    fn reveal_task(&mut self, task_id: i64) {
        let ancestors = tree::ancestors_of(&self.tasks, task_id);
        for &ancestor in &ancestors {
            self.expanded.insert(ancestor);
        }
        if let Some(zoom) = self.zoom_root
            && !ancestors.contains(&zoom)
        {
            self.zoom_root = None;
        }
        self.rebuild_rows();
        self.select_task(task_id);
    }

    /// Runs undo or redo and then makes the result visible (reveal_task),
    /// where a successful undo would otherwise look like nothing happened,
    /// and names the change in the status line.
    fn apply_history(&mut self, db: &Db, kind: History) -> Result<(), engine::Error> {
        let outcome = match kind {
            History::Undo => db.undo()?,
            History::Redo => db.redo()?,
        };
        let Some(outcome) = outcome else {
            self.status_line = Some(kind.empty_message().to_string());
            return Ok(());
        };
        // Status edits are undoable too, so both loaded data sets may be
        // stale now.
        self.reload_statuses(db)?;
        self.tasks = db.list_all()?;
        // Tasks the replay removed (e.g. undoing a create) cannot be shown;
        // only the surviving ones steer the view.
        let surviving: Vec<i64> = outcome
            .affected_task_ids
            .iter()
            .copied()
            .filter(|id| self.tasks.iter().any(|task| task.id == *id))
            .collect();
        for &id in &surviving {
            for ancestor in tree::ancestors_of(&self.tasks, id) {
                self.expanded.insert(ancestor);
            }
        }
        match surviving.first() {
            Some(&first) => self.reveal_task(first),
            None => self.rebuild_rows(),
        }
        self.status_line = Some(format!("{}: {}", kind.verb(), outcome.description));
        Ok(())
    }

    /// Opens the query view with an empty search, i.e. everything under the
    /// current filter, ready for incremental typing.
    fn open_query(&mut self, db: &Db) -> Result<(), engine::Error> {
        let mut state = query_view::QueryState::new();
        self.run_search(db, &mut state)?;
        self.mode = Mode::Query(state);
        Ok(())
    }

    /// Re-runs the search for the view's current text/sort and the shared
    /// filter, keeping the selection on an existing result.
    fn run_search(&self, db: &Db, state: &mut query_view::QueryState) -> Result<(), engine::Error> {
        state.results = db.search(&state.query(self.filter), &Self::today())?;
        state.clamp_selection();
        Ok(())
    }

    /// Handles one key inside the query view. Returns whether the view
    /// stays open. Tasks are never edited from here: the view is read-only,
    /// and unbound keys fall through to nothing.
    fn handle_query_key(
        &mut self,
        db: &Db,
        state: &mut query_view::QueryState,
        key: key::Key,
    ) -> Result<bool, engine::Error> {
        match state.focus {
            query_view::Focus::Edit => {
                // The editor consumes itself on every key, so take it out;
                // every branch below decides the next state explicitly.
                let editor = std::mem::replace(&mut state.editor, input::Editor::new());
                match editor.handle_key(key) {
                    input::EditResult::Continue(editor) => {
                        state.editor = editor;
                        // Searching on every keystroke is what makes the
                        // search incremental.
                        self.run_search(db, state)?;
                    }
                    input::EditResult::Submitted(text) => {
                        // The text stays in the editor so `/` can reopen the
                        // edit with it prefilled.
                        state.editor = input::Editor::with_text(&text);
                        state.focus = query_view::Focus::Browse;
                    }
                    input::EditResult::Cancelled => return Ok(false),
                }
            }
            query_view::Focus::Browse => {
                if let Some(command) =
                    self.dispatcher
                        .key(&self.keymap, command::Context::Query, key)
                {
                    match command {
                        id::QUERY_NEXT => {
                            if state.selected + 1 < state.results.len() {
                                state.selected += 1;
                            }
                        }
                        id::QUERY_PREV => state.selected = state.selected.saturating_sub(1),
                        id::QUERY_FIRST => state.selected = 0,
                        id::QUERY_LAST => state.selected = state.results.len().saturating_sub(1),
                        id::QUERY_HALF_PAGE_DOWN => {
                            state.selected = (state.selected + half_page_step(self.popup_height))
                                .min(state.results.len().saturating_sub(1));
                        }
                        id::QUERY_HALF_PAGE_UP => {
                            state.selected = state
                                .selected
                                .saturating_sub(half_page_step(self.popup_height));
                        }
                        id::QUERY_EDIT => {
                            // Rebuilt so the cursor lands at the end of the
                            // preserved text.
                            state.editor = input::Editor::with_text(state.editor.text());
                            state.focus = query_view::Focus::Edit;
                        }
                        id::QUERY_SORT => state.focus = query_view::Focus::SortMenu,
                        id::VIEW_FILTER => state.focus = query_view::Focus::FilterMenu,
                        id::QUERY_CLOSE => return Ok(false),
                        id::QUERY_JUMP => {
                            if let Some(task) = state.results.get(state.selected) {
                                self.reveal_task(task.id);
                                return Ok(false);
                            }
                        }
                        _ => {}
                    }
                }
            }
            query_view::Focus::SortMenu => match key {
                key::Key::Esc => state.focus = query_view::Focus::Browse,
                key::Key::Char(c) => {
                    if let Some(sort) = query_view::sort_from_key(c) {
                        state.sort = sort;
                        state.focus = query_view::Focus::Browse;
                        self.run_search(db, state)?;
                    }
                }
                _ => {}
            },
            query_view::Focus::FilterMenu => match key {
                key::Key::Esc => state.focus = query_view::Focus::Browse,
                key::Key::Char(c) => {
                    if let Some(filter) = query_view::filter_from_key(&self.statuses, c) {
                        self.filter = filter;
                        state.focus = query_view::Focus::Browse;
                        self.run_search(db, state)?;
                        // The filter is shared with the tree view, which
                        // must reflect it once the query view closes.
                        self.rebuild_rows();
                    }
                }
                _ => {}
            },
        }
        Ok(true)
    }

    /// Handles one key inside the help view. Returns whether the view stays
    /// open. Pure view logic: nothing here touches the database.
    fn handle_help_key(&mut self, state: &mut help::HelpState, key: key::Key) -> bool {
        match state.focus {
            help::Focus::Edit => {
                // The editor consumes itself on every key, so take it out;
                // every branch below decides the next state explicitly.
                let editor = std::mem::replace(&mut state.editor, input::Editor::new());
                match editor.handle_key(key) {
                    input::EditResult::Continue(editor) => {
                        state.editor = editor;
                        // Every keystroke re-filters the list, so a kept
                        // offset could point past the shrunken list's end.
                        state.scroll = 0;
                    }
                    input::EditResult::Submitted(text) => {
                        // The text stays in the editor so `/` can reopen the
                        // filter with it prefilled, and browsing stays on
                        // the filtered list.
                        state.editor = input::Editor::with_text(&text);
                        state.focus = help::Focus::Browse;
                    }
                    input::EditResult::Cancelled => return false,
                }
            }
            help::Focus::Browse => {
                if let Some(command) =
                    self.dispatcher
                        .key(&self.keymap, command::Context::Help, key)
                {
                    let last = self.help_line_count(state).saturating_sub(1);
                    match command {
                        id::HELP_NEXT => state.scroll = (state.scroll + 1).min(last),
                        id::HELP_PREV => state.scroll = state.scroll.saturating_sub(1),
                        id::HELP_FIRST => state.scroll = 0,
                        id::HELP_LAST => state.scroll = last,
                        id::HELP_HALF_PAGE_DOWN => {
                            state.scroll =
                                (state.scroll + half_page_step(self.popup_height)).min(last);
                        }
                        id::HELP_HALF_PAGE_UP => {
                            state.scroll = state
                                .scroll
                                .saturating_sub(half_page_step(self.popup_height));
                        }
                        id::HELP_FILTER => {
                            // Rebuilt so the cursor lands at the end of the
                            // preserved text.
                            state.editor = input::Editor::with_text(state.editor.text());
                            state.focus = help::Focus::Edit;
                        }
                        id::HELP_CLOSE => return false,
                        _ => {}
                    }
                }
            }
        }
        true
    }

    /// How many lines the help view currently shows under its filter;
    /// bounds the scroll offset.
    fn help_line_count(&self, state: &help::HelpState) -> usize {
        help::filter_lines(
            help::lines(command::COMMANDS, &self.keymap),
            state.editor.text(),
        )
        .len()
    }

    /// Deletes on `d`: a childless task goes instantly — undo covers
    /// mistakes, so a prompt would only cost tempo. Only a subtree, whose
    /// size is not visible at a glance, warrants a confirmation.
    fn request_delete(&mut self, db: &Db) -> Result<(), engine::Error> {
        let Some(row) = self.rows.get(self.selected) else {
            return Ok(());
        };
        let task = &self.tasks[row.task_index];
        let task_id = task.id;
        let count = db.count_subtree(task_id)?;
        if count == 1 {
            let title = task.title.clone();
            self.delete_and_reselect(db, task_id)?;
            self.status_line = Some(format!("deleted \"{title}\" (u to undo)"));
        } else {
            self.status_line = Some(format!("delete {count} task(s)? (y/n)"));
            self.mode = Mode::ConfirmDelete { task_id, count };
        }
        Ok(())
    }

    /// Deletes the subtree and moves the cursor to the nearest surviving
    /// neighbour: next sibling, else previous sibling, else parent.
    fn delete_and_reselect(&mut self, db: &Db, task_id: i64) -> Result<(), engine::Error> {
        // Computed from the pre-delete snapshot; after the reload the
        // deleted task's neighbours are gone from `rows`.
        let fallback = tree::selection_after_delete(&self.tasks, task_id);
        db.delete_subtree(task_id)?;
        self.reload(db)?;
        if let Some(target) = fallback {
            self.select_task(target);
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

    /// Opens the status-management modal. Its edits form an undo
    /// sub-session: fine-grained undo/redo while it is open, folded into a
    /// single outer step when it closes.
    fn open_status_manage(&mut self, db: &Db) -> Result<(), engine::Error> {
        // Statuses can never be empty (the last row is undeletable), but an
        // empty table would leave the cursor nowhere to sit.
        if self.statuses.is_empty() {
            return Ok(());
        }
        db.begin_undo_scope()?;
        self.mode = Mode::StatusManage(status_manage::ManageState::new());
        Ok(())
    }

    /// Undo or redo inside the status modal. Edits here only touch
    /// statuses, so making the change visible is just reloading the table —
    /// and keeping the cursor on a row that still exists.
    fn apply_manage_history(
        &mut self,
        db: &Db,
        state: &mut status_manage::ManageState,
        kind: History,
    ) -> Result<(), engine::Error> {
        let outcome = match kind {
            History::Undo => db.undo()?,
            History::Redo => db.redo()?,
        };
        let Some(outcome) = outcome else {
            self.status_line = Some(kind.empty_message().to_string());
            return Ok(());
        };
        self.reload_statuses(db)?;
        state.clamp_row(self.statuses.len());
        self.status_line = Some(format!("{}: {}", kind.verb(), outcome.description));
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
            id::MANAGE_CLOSE => {
                // Whatever survived the session's own undo becomes one
                // atomic step in the tree-level history.
                db.end_undo_scope("edit statuses")?;
                return Ok(false);
            }
            id::MANAGE_UNDO => self.apply_manage_history(db, state, History::Undo)?,
            id::MANAGE_REDO => self.apply_manage_history(db, state, History::Redo)?,
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
                // Unknown specs are stored anyway (they only degrade to the
                // default color at render time), but warn about the typo.
                db.update_status_color(id, text)?;
                if color::parse_color(text) == ratatui::style::Color::Reset {
                    self.status_line = Some(format!(
                        "unknown color `{text}` (expected a name or #rrggbb); \
                         it will render as the terminal default"
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

    fn submit(&mut self, db: &Db, action: InputAction, text: &str) -> Result<(), engine::Error> {
        let trimmed = text.trim();
        match action {
            // An empty input is treated as a cancel; a blank title would
            // only produce noise to clean up.
            InputAction::Create(_) | InputAction::Rename(_) if trimmed.is_empty() => {}
            InputAction::Create(target) => {
                let task = db.create_task(
                    target.parent_id,
                    trimmed,
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
                db.rename_task(id, trimmed)?;
                self.reload(db)?;
                // Reloading rebuilds the rows; put the cursor back on the
                // task that was just renamed.
                self.select_task(id);
            }
            InputAction::SetDue(task_id) => {
                // For a due date, confirming an empty input means "no due
                // date" rather than cancel; Esc is the cancel gesture.
                let due = (!trimmed.is_empty()).then_some(trimmed);
                match db.set_due(task_id, due) {
                    Ok(()) => {
                        self.reload(db)?;
                        self.select_task(task_id);
                    }
                    // A typo should not throw the whole input away: report
                    // it and reopen the editor with the text preserved.
                    Err(err @ engine::Error::InvalidDate(_)) => {
                        self.status_line = Some(err.to_string());
                        self.mode = Mode::Input {
                            editor: input::Editor::with_text(text),
                            action: InputAction::SetDue(task_id),
                        };
                    }
                    Err(other) => return Err(other),
                }
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
            id::HALF_PAGE_DOWN => {
                self.selected = (self.selected + half_page_step(self.list_height))
                    .min(self.rows.len().saturating_sub(1));
            }
            id::HALF_PAGE_UP => {
                self.selected = self
                    .selected
                    .saturating_sub(half_page_step(self.list_height));
            }
            // Depth 0 means the parent is the zoom root or nothing at all;
            // either way there is no parent row to land on.
            id::SELECT_PARENT => {
                if let Some(row) = self.rows.get(self.selected)
                    && row.depth > 0
                    && let Some(parent_id) = self.tasks[row.task_index].parent_id
                {
                    self.select_task(parent_id);
                }
            }
            id::SELECT_FIRST_CHILD => {
                if let Some(row) = self.rows.get(self.selected)
                    && row.has_children
                {
                    let depth = row.depth;
                    self.expanded.insert(self.tasks[row.task_index].id);
                    self.rebuild_rows();
                    // An active filter can hide every child even though
                    // has_children is set, so only move onto a real child.
                    if let Some(next) = self.rows.get(self.selected + 1)
                        && next.depth == depth + 1
                    {
                        self.selected += 1;
                    }
                }
            }
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
            id::SET_STATUS => {
                if let Some(row) = self.rows.get(self.selected) {
                    self.mode = Mode::StatusSelect {
                        task_id: self.tasks[row.task_index].id,
                    };
                }
            }
            id::SET_DUE => {
                if let Some(row) = self.rows.get(self.selected) {
                    let task = &self.tasks[row.task_index];
                    self.mode = Mode::Input {
                        editor: input::Editor::with_text(task.due.as_deref().unwrap_or("")),
                        action: InputAction::SetDue(task.id),
                    };
                }
            }
            id::EDIT_NOTE => {
                if let Some(row) = self.rows.get(self.selected) {
                    self.pending_note_edit = Some(self.tasks[row.task_index].id);
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
            id::VIEW_FILTER => self.mode = Mode::FilterSelect,
            id::HELP => self.mode = Mode::Help(help::HelpState::new()),
            // The binding stays active while the footer is hidden, so the
            // same key brings it back.
            id::TOGGLE_FOOTER => self.show_footer = !self.show_footer,
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

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    db: &Db,
    config: config::Config,
) -> Result<(), Box<dyn Error>> {
    let mut app = App::new(db.list_all()?, db.list_statuses()?, db.default_status_id()?);
    app.keymap = config.keymap;
    app.show_footer = config.footer;
    // Shadowed bindings are legal but surprising (the longer sequence can
    // never fire), so say so once at startup.
    let warnings = app.keymap.shadow_warnings();
    if !warnings.is_empty() {
        app.status_line = Some(warnings.join("; "));
    }
    while !app.should_quit {
        terminal.draw(|frame| draw(frame, &mut app))?;
        if let event::Event::Key(key_event) = event::read()?
            && key_event.kind == event::KeyEventKind::Press
            && let Some(key) = key_from_event(&key_event)
        {
            app.handle_key(db, key)?;
        }
        // Handled here rather than in handle_key because the editor session
        // needs the terminal, which only this loop owns.
        if let Some(task_id) = app.pending_note_edit.take() {
            edit_note(terminal, db, &mut app, task_id)?;
        }
    }
    Ok(())
}

/// Runs the selected task's note through the external editor and stores the
/// result. An unchanged note is not written back, so no empty undo step is
/// recorded.
fn edit_note(
    terminal: &mut ratatui::DefaultTerminal,
    db: &Db,
    app: &mut App,
    task_id: i64,
) -> Result<(), Box<dyn Error>> {
    let Some(original) = app
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| task.note.clone())
    else {
        return Ok(());
    };
    match external_editor::edit_in_editor(terminal, &original)? {
        external_editor::EditOutcome::Changed(text) => {
            db.set_note(task_id, &text)?;
            app.reload(db)?;
            app.select_task(task_id);
            app.status_line = Some("note updated (u to undo)".to_string());
        }
        external_editor::EditOutcome::Unchanged => {
            app.status_line = Some("note unchanged".to_string());
        }
        external_editor::EditOutcome::Aborted => {
            app.status_line = Some("note edit discarded (editor exited with an error)".to_string());
        }
    }
    Ok(())
}

/// Share of the screen a modal popup covers, matching the proportions of
/// typical editor pickers.
const POPUP_WIDTH_PCT: u16 = 80;
const POPUP_HEIGHT_PCT: u16 = 80;

/// Rows one Ctrl-d/Ctrl-u jump covers: half the visible list, vim-style.
/// At least one row, so the keys work even before the first draw sizes
/// the list.
fn half_page_step(visible_height: usize) -> usize {
    (visible_height / 2).max(1)
}

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    // The one clock read for rendering: every overdue check compares
    // against this local date.
    let today = App::today();
    // The line above the footer doubles as the text input and the
    // menu candidate lists. The modal views carry their own prompt row
    // inside the popup instead.
    let input_height = match app.mode {
        // The delete prompt lives in the status line, not the input line.
        Mode::Tree | Mode::ConfirmDelete { .. } | Mode::StatusManage(_) | Mode::Help(_) => 0,
        Mode::Input { .. } | Mode::StatusSelect { .. } | Mode::FilterSelect => 1,
        Mode::Query(ref state) => match state.focus {
            // One-key menus stay on the screen-bottom line, where they are
            // in every other mode.
            query_view::Focus::SortMenu | query_view::Focus::FilterMenu => 1,
            query_view::Focus::Edit | query_view::Focus::Browse => 0,
        },
    };
    // The tree is the base layer even under a popup, so its context line
    // stays meaningful.
    let header_text = render::tree_header(
        app.zoom_root.map(|id| tree::breadcrumb(&app.tasks, id)),
        app.filter,
        &app.statuses,
    );
    let header_height = if header_text.is_some() { 1 } else { 0 };
    let message_height = if app.status_line.is_some() { 1 } else { 0 };
    // A popup covers the middle of the screen, so a task detail pane under
    // it would only compete with the modal for attention.
    let pane_budget =
        (frame.area().height as usize / 3).clamp(NOTE_PANE_MIN_LINES, NOTE_PANE_MAX_LINES);
    let note_lines = if is_modal(&app.mode) {
        Vec::new()
    } else {
        app.rows
            .get(app.selected)
            .map(|row| note::pane_lines(&app.tasks[row.task_index].note, pane_budget))
            .unwrap_or_default()
    };
    // +1 for the pane's own "note" header line.
    let note_height = if note_lines.is_empty() {
        0
    } else {
        note_lines.len() as u16 + 1
    };
    let [
        header_area,
        list_area,
        input_area,
        message_area,
        note_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(message_height),
        Constraint::Length(note_height),
        // Hiding the footer frees its line for the list.
        Constraint::Length(if app.show_footer { 1 } else { 0 }),
    ])
    .areas(frame.area());
    // Remembered for the half-page jump commands, which need to know how
    // many rows the list showed.
    app.list_height = list_area.height as usize;

    if let Some(text) = &header_text {
        frame.render_widget(
            Paragraph::new(text.as_str()).style(Style::default().add_modifier(Modifier::DIM)),
            header_area,
        );
    }

    let list = List::new(app.rows.iter().map(|row| {
        let task = &app.tasks[row.task_index];
        let is_expanded = app.expanded.contains(&task.id);
        render::task_line(
            tree::row_prefix(row.depth, row.has_children, is_expanded),
            task,
            &app.statuses,
            &today,
        )
    }))
    .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default();
    if !app.rows.is_empty() {
        list_state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    if is_modal(&app.mode) {
        // Remembered for the half-page jump commands of the popup views.
        app.popup_height = draw_popup(frame, app, &today);
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
    if let Mode::FilterSelect = &app.mode {
        frame.render_widget(
            Paragraph::new(render::filter_menu_line(&app.statuses)),
            input_area,
        );
    }
    if let Mode::Query(state) = &app.mode {
        match state.focus {
            query_view::Focus::SortMenu => {
                frame.render_widget(Paragraph::new(render::sort_menu_line()), input_area);
            }
            query_view::Focus::FilterMenu => {
                frame.render_widget(
                    Paragraph::new(render::filter_menu_line(&app.statuses)),
                    input_area,
                );
            }
            query_view::Focus::Edit | query_view::Focus::Browse => {}
        }
    }

    if let Some(message) = &app.status_line {
        frame.render_widget(Paragraph::new(message.as_str()), message_area);
    }

    if !note_lines.is_empty() {
        let mut lines = vec![Line::styled(
            "note",
            Style::default().add_modifier(Modifier::DIM),
        )];
        lines.extend(note_lines.into_iter().map(Line::from));
        frame.render_widget(Paragraph::new(lines), note_area);
    }

    if app.show_footer {
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
}

/// Whether the mode is shown as a popup over the task tree.
fn is_modal(mode: &Mode) -> bool {
    matches!(mode, Mode::StatusManage(_) | Mode::Help(_) | Mode::Query(_))
}

/// Blanks the base-layer characters whose second half the popup's left edge
/// covers. A terminal draws such a character in full, so its right half
/// would bleed over the popup border — CJK titles make this the common case.
fn blank_chars_split_by(frame: &mut ratatui::Frame, popup_area: Rect) {
    if popup_area.x == 0 || popup_area.width == 0 {
        return;
    }
    let column = popup_area.x - 1;
    let buffer = frame.buffer_mut();
    for y in popup_area.y..popup_area.bottom() {
        if buffer[(column, y)].symbol().width() > 1 {
            buffer[(column, y)].set_symbol(" ");
        }
    }
}

/// Draws the current modal view as a centered, bordered popup over the task
/// tree: while text is being captured, a prompt row and a divider lead the
/// list.
/// Returns the rows of the popup's list area, which sizes the half-page
/// jumps of the popup views.
fn draw_popup(frame: &mut ratatui::Frame, app: &App, today: &str) -> usize {
    let popup_area = overlay::centered_rect(frame.area(), POPUP_WIDTH_PCT, POPUP_HEIGHT_PCT);
    let block = Block::bordered().title(popup_title(app, popup_area.width));
    let inner = block.inner(popup_area);
    // The tree underneath would otherwise show through the popup.
    frame.render_widget(Clear, popup_area);
    blank_chars_split_by(frame, popup_area);
    frame.render_widget(block, popup_area);

    let prompt = popup_prompt(app);
    let prompt_height = if prompt.is_some() { 1 } else { 0 };
    let row = Constraint::Length(prompt_height);
    let [prompt_area, divider_area, content_area] =
        Layout::vertical([row, row, Constraint::Min(0)]).areas(inner);

    draw_popup_content(frame, app, today, content_area);
    if let Some(prompt) = prompt {
        draw_divider(frame, popup_area, divider_area);
        frame.render_widget(Paragraph::new(prompt), prompt_area);
    }
    content_area.height as usize
}

/// The popup's border title, cut to what the border can show.
fn popup_title(app: &App, popup_width: u16) -> String {
    let text = match &app.mode {
        // The search text, sort and filter belong together and have no
        // other place in the popup, so the whole header goes in the title.
        Mode::Query(state) => {
            query_view::header(state.editor.text(), state.sort, app.filter, &app.statuses)
        }
        // A confirmed help filter has nowhere else to show once the prompt
        // row is gone, and a silently filtered list looks incomplete.
        Mode::Help(state) => match state.editor.text() {
            "" => "help".to_string(),
            filter => format!("help: {filter}"),
        },
        Mode::StatusManage(_) => "manage statuses".to_string(),
        _ => String::new(),
    };
    // Two border corners plus the space padding the title on each side.
    let budget = popup_width.saturating_sub(4) as usize;
    format!(" {} ", overlay::truncate_to_width(&text, budget))
}

/// The popup's prompt line, if the view is currently capturing input. It is
/// drawn above the list in every view: reading order goes from what is
/// typed to what it acts on, like `fzf --reverse`.
fn popup_prompt(app: &App) -> Option<Line<'_>> {
    match &app.mode {
        Mode::Query(state) => {
            (state.focus == query_view::Focus::Edit).then(|| input_line("Search: ", &state.editor))
        }
        Mode::Help(state) => {
            (state.focus == help::Focus::Edit).then(|| input_line("Filter: ", &state.editor))
        }
        Mode::StatusManage(state) => match &state.editing {
            status_manage::Editing::Cell(editor) => {
                let prompt = match state.col {
                    status_manage::Column::Label => "Label: ",
                    status_manage::Column::Color => "Color: ",
                    _ => "Edit: ",
                };
                Some(input_line(prompt, editor))
            }
            status_manage::Editing::NewStatus(editor) => Some(input_line("New status: ", editor)),
            status_manage::Editing::KeyCapture => {
                Some(Line::from("Press a key for this status (Esc cancels)"))
            }
            status_manage::Editing::None => None,
        },
        _ => None,
    }
}

/// Draws the list of the currently open modal view into the popup's inner
/// area, scrolled so the cursor stays visible.
fn draw_popup_content(frame: &mut ratatui::Frame, app: &App, today: &str, area: Rect) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    match &app.mode {
        Mode::StatusManage(state) => {
            let lines = render::manage_table_lines(&app.statuses, state.row, state.col);
            // The cursor row is one below its status because of the table's
            // own header line, which scrolls away with the rows.
            let offset = overlay::scroll_to_show(state.row + 1, area.height as usize);
            frame.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), area);
        }
        Mode::Help(state) => {
            let lines = help::filter_lines(
                help::lines(command::COMMANDS, &app.keymap),
                state.editor.text(),
            );
            let rendered: Vec<Line> = help::display_lines(&lines)
                .into_iter()
                .skip(state.scroll)
                .collect();
            if rendered.is_empty() {
                frame.render_widget(Paragraph::new("no matching bindings").style(dim), area);
            } else {
                frame.render_widget(Paragraph::new(rendered), area);
            }
        }
        Mode::Query(state) => {
            if state.results.is_empty() {
                frame.render_widget(Paragraph::new("no matches").style(dim), area);
            } else {
                let items = state.results.iter().map(|task| {
                    ratatui::text::Text::from(query_view::result_item(
                        task,
                        &app.statuses,
                        &app.tasks,
                        state.editor.text(),
                        today,
                    ))
                });
                let list = List::new(items)
                    .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
                let mut list_state = ListState::default();
                list_state.select(Some(state.selected));
                frame.render_stateful_widget(list, area, &mut list_state);
            }
        }
        _ => {}
    }
}

/// Separates the popup's list from its prompt row, joined to the border so
/// the box does not look broken open.
fn draw_divider(frame: &mut ratatui::Frame, popup_area: Rect, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let line = symbols::line::HORIZONTAL.repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
    let buffer = frame.buffer_mut();
    buffer[(popup_area.x, area.y)].set_symbol(symbols::line::VERTICAL_RIGHT);
    buffer[(popup_area.right() - 1, area.y)].set_symbol(symbols::line::VERTICAL_LEFT);
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
            note: String::new(),
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

    // Tests the vim-style half-page jumps in the task tree.
    // Given: ten root tasks and a last-drawn list height of 6 (half: 3)
    // When: half-page down runs twice, then half-page up once
    // Then: the cursor moves 0 → 3 → 6 → 3, one half page per command
    #[test]
    fn half_page_commands_move_cursor_by_half_the_list_height() {
        let mut app = test_app((1..=10).map(|i| task(i, None, i)).collect());
        app.list_height = 6;

        app.run_command(id::HALF_PAGE_DOWN);
        assert_eq!(app.selected, 3);
        app.run_command(id::HALF_PAGE_DOWN);
        assert_eq!(app.selected, 6);
        app.run_command(id::HALF_PAGE_UP);
        assert_eq!(app.selected, 3);
    }

    // Tests clamping of the half-page jumps at both list ends.
    // Given: three root tasks and a list height of 40, so one half page
    //        (20) overshoots the whole list
    // When: half-page down runs, then half-page up
    // Then: the cursor clamps to the last row, then back to the first
    #[test]
    fn half_page_jumps_clamp_at_the_list_ends() {
        let mut app = test_app(vec![task(1, None, 0), task(2, None, 1), task(3, None, 2)]);
        app.list_height = 40;

        app.run_command(id::HALF_PAGE_DOWN);
        assert_eq!(app.selected, 2);
        app.run_command(id::HALF_PAGE_UP);
        assert_eq!(app.selected, 0);
    }

    // Tests the half-page jump before anything has been drawn.
    // Given: ten root tasks and the initial list height of 0
    // When: half-page down runs
    // Then: the cursor still moves (by the minimum step of one row)
    //       instead of panicking or standing still
    #[test]
    fn half_page_jump_before_first_draw_moves_one_row() {
        let mut app = test_app((1..=10).map(|i| task(i, None, i)).collect());

        app.run_command(id::HALF_PAGE_DOWN);

        assert_eq!(app.selected, 1);
    }

    // Tests the half-page jumps of the query view's result list.
    // Given: ten tasks, the query view opened on the empty search (all
    //        results) and a last-drawn popup height of 6 (half: 3)
    // When: Ctrl-d is pressed, then Ctrl-u
    // Then: the result selection moves to 3 and back to 0
    #[test]
    fn query_half_page_moves_selection_by_half_the_popup_height() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..10 {
            db.create_task(None, &format!("t{i}"), None, default_status(&db))
                .unwrap();
        }
        let mut app = app_for(&db, db.list_all().unwrap());
        app.popup_height = 6;

        app.handle_key(&db, key::Key::Char('/')).unwrap();
        app.handle_key(&db, key::Key::Enter).unwrap();
        app.handle_key(&db, key::Key::Ctrl('d')).unwrap();

        let Mode::Query(state) = &app.mode else {
            panic!("query view must stay open");
        };
        assert_eq!(state.selected, 3);

        app.handle_key(&db, key::Key::Ctrl('u')).unwrap();
        let Mode::Query(state) = &app.mode else {
            panic!("query view must stay open");
        };
        assert_eq!(state.selected, 0);
    }

    // Tests the half-page jumps of the help list.
    // Given: the help view open and a last-drawn popup height of 6 (half: 3)
    // When: Ctrl-d is pressed, then Ctrl-u
    // Then: the scroll offset moves to 3 and back to 0
    #[test]
    fn help_half_page_scrolls_by_half_the_popup_height() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        app.popup_height = 6;

        app.handle_key(&db, key::Key::Char('?')).unwrap();
        app.handle_key(&db, key::Key::Ctrl('d')).unwrap();

        let Mode::Help(state) = &app.mode else {
            panic!("help must stay open");
        };
        assert_eq!(state.scroll, 3);

        app.handle_key(&db, key::Key::Ctrl('u')).unwrap();
        let Mode::Help(state) = &app.mode else {
            panic!("help must stay open");
        };
        assert_eq!(state.scroll, 0);
    }

    // Tests that drawing records the heights that size half-page jumps.
    // Given: a 60x20 terminal, first showing the bare tree, then the help
    //        popup over it
    // When: a frame is rendered in each state
    // Then: the tree records its list rows (19: the screen minus the
    //       footer line) and the popup its inner rows (14: the 80%-high
    //       popup of 16 rows minus its two border rows)
    #[test]
    fn draw_records_list_and_popup_heights() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        rendered_rows(&mut app, 60, 20);
        assert_eq!(app.list_height, 19);

        app.handle_key(&db, key::Key::Char('?')).unwrap();
        rendered_rows(&mut app, 60, 20);
        assert_eq!(app.popup_height, 14);
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

    // Tests moving the selection to the parent task.
    // Given: root 1 expanded with children 11 and 12, cursor on 12
    // When: the select-parent command runs
    // Then: the cursor moves to the parent 1 and the expansion is untouched
    #[test]
    fn select_parent_moves_to_parent() {
        let mut app = test_app(vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(12, Some(1), 1),
        ]);
        app.expanded.insert(1);
        app.rebuild_rows();
        app.select_task(12);

        app.run_command(id::SELECT_PARENT);

        assert_eq!(selected_id(&app), Some(1));
        assert!(app.expanded.contains(&1));
    }

    // Tests select-parent on a top-level task.
    // Given: two roots with the cursor on the second
    // When: the select-parent command runs
    // Then: nothing changes, there is no parent to move to
    #[test]
    fn select_parent_at_top_level_is_a_no_op() {
        let mut app = test_app(vec![task(1, None, 0), task(2, None, 1)]);
        app.selected = 1;

        app.run_command(id::SELECT_PARENT);

        assert_eq!(selected_id(&app), Some(2));
    }

    // Tests select-parent directly under the zoom root.
    // Given: a chain 1 > 11 > 111 zoomed on 1, cursor on 11 (depth 0)
    // When: the select-parent command runs
    // Then: nothing changes, the parent sits outside the zoomed view
    #[test]
    fn select_parent_under_zoom_root_is_a_no_op() {
        let mut app = test_app(vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ]);
        app.zoom_root = Some(1);
        app.rebuild_rows();
        app.select_task(11);

        app.run_command(id::SELECT_PARENT);

        assert_eq!(selected_id(&app), Some(11));
        assert_eq!(app.zoom_root, Some(1));
    }

    // Tests moving the selection to the first child of a collapsed task.
    // Given: root 1 collapsed with children 11 and 12
    // When: the select-first-child command runs
    // Then: task 1 is expanded and the cursor moves to its first child 11
    #[test]
    fn select_first_child_expands_and_moves() {
        let mut app = test_app(vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(12, Some(1), 1),
        ]);

        app.run_command(id::SELECT_FIRST_CHILD);

        assert!(app.expanded.contains(&1));
        assert_eq!(selected_id(&app), Some(11));
    }

    // Tests select-first-child on an already expanded task.
    // Given: root 1 expanded with child 11, cursor on 1
    // When: the select-first-child command runs
    // Then: the cursor moves to 11 without touching the expansion
    #[test]
    fn select_first_child_moves_when_already_expanded() {
        let mut app = test_app(vec![task(1, None, 0), task(11, Some(1), 0)]);
        app.expanded.insert(1);
        app.rebuild_rows();

        app.run_command(id::SELECT_FIRST_CHILD);

        assert_eq!(selected_id(&app), Some(11));
    }

    // Tests select-first-child on a task without children.
    // Given: two childless roots with the cursor on the first
    // When: the select-first-child command runs
    // Then: nothing changes, there is no child to move to
    #[test]
    fn select_first_child_on_leaf_is_a_no_op() {
        let mut app = test_app(vec![task(1, None, 0), task(2, None, 1)]);

        app.run_command(id::SELECT_FIRST_CHILD);

        assert_eq!(selected_id(&app), Some(1));
        assert!(!app.expanded.contains(&1));
    }

    // Tests the whole help-view key flow.
    // Given: an app in the tree view
    // When: `?` opens the help, `j` scrolls, `/` enters the filter, "zoom"
    //       is typed, Enter returns to browsing, and `q` closes the view
    // Then: each step lands in the expected mode/focus, typing resets the
    //       scroll, and the filter text survives into browsing
    #[test]
    fn help_view_opens_filters_and_closes() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![task(1, None, 0)]);

        app.handle_key(&db, key::Key::Char('?')).unwrap();
        assert!(matches!(app.mode, Mode::Help(_)));

        app.handle_key(&db, key::Key::Char('j')).unwrap();
        let Mode::Help(state) = &app.mode else {
            panic!("help must stay open while scrolling");
        };
        assert_eq!(state.scroll, 1);

        app.handle_key(&db, key::Key::Char('/')).unwrap();
        for c in "zoom".chars() {
            app.handle_key(&db, key::Key::Char(c)).unwrap();
        }
        let Mode::Help(state) = &app.mode else {
            panic!("help must stay open while filtering");
        };
        assert_eq!(state.focus, help::Focus::Edit);
        assert_eq!(state.scroll, 0, "typing must reset the scroll");

        app.handle_key(&db, key::Key::Enter).unwrap();
        let Mode::Help(state) = &app.mode else {
            panic!("help must stay open after confirming the filter");
        };
        assert_eq!(state.focus, help::Focus::Browse);
        assert_eq!(state.editor.text(), "zoom");

        app.handle_key(&db, key::Key::Char('q')).unwrap();
        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests the footer toggle.
    // Given: an app with the footer shown (the default)
    // When: the toggle-footer key `\` is pressed twice in the tree view
    // Then: the footer turns off and back on (the binding stays active
    //       while the footer is hidden)
    #[test]
    fn backslash_toggles_the_footer() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        assert!(app.show_footer);

        app.handle_key(&db, key::Key::Char('\\')).unwrap();
        assert!(!app.show_footer);

        app.handle_key(&db, key::Key::Char('\\')).unwrap();
        assert!(app.show_footer);
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
    //        where "d" is the seeded key for the status labelled "Doing"
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
    //        (cancelled kind, so the view filter is widened to All to keep
    //        the task's row selectable)
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
        app.filter = engine::Filter::All;
        app.rebuild_rows();

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

    // Tests that "t" opens the due input prefilled with the current date.
    // Given: a database task with due = 2026-09-15, cursor on it
    // When: "t" is pressed in the Tree context
    // Then: the mode becomes Input targeting that task's due date, with the
    //       editor prefilled with the stored date and a prompt naming the
    //       expected format
    #[test]
    fn t_opens_due_input_prefilled_with_current_due() {
        let db = Db::open_in_memory().unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        db.set_due(target.id, Some("2026-09-15")).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        app.handle_key(&db, key::Key::Char('t')).unwrap();

        let Mode::Input { editor, action } = &app.mode else {
            panic!("t should enter input mode");
        };
        assert!(matches!(action, InputAction::SetDue(id) if *id == target.id));
        assert_eq!(editor.text(), "2026-09-15");
        assert_eq!(action.prompt(), "Due (YYYY-MM-DD): ");
    }

    // Tests the due input prefill for a task without a due date.
    // Given: a database task with no due date
    // When: "t" is pressed
    // Then: the editor opens empty
    #[test]
    fn t_opens_empty_due_input_when_no_due_is_set() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        app.handle_key(&db, key::Key::Char('t')).unwrap();

        let Mode::Input { editor, .. } = &app.mode else {
            panic!("t should enter input mode");
        };
        assert_eq!(editor.text(), "");
    }

    // Tests submitting a valid due date end to end.
    // Given: two database tasks with the due input open for the second
    // When: "2026-09-15" is typed and confirmed
    // Then: the date is persisted, the mode returns to Tree, and the cursor
    //       stays on the same task
    #[test]
    fn submitting_valid_due_persists_and_returns_to_tree() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "other", None, default_status(&db))
            .unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(target.id);

        app.handle_key(&db, key::Key::Char('t')).unwrap();
        press(&mut app, &db, "2026-09-15");
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(selected_id(&app), Some(target.id));
        let dues: Vec<Option<String>> = db.list_all().unwrap().into_iter().map(|t| t.due).collect();
        assert_eq!(dues, [None, Some("2026-09-15".to_string())]);
    }

    // Tests that confirming an emptied due input clears the date.
    // Given: a database task with due = 2026-09-15 and the due input open
    //        (prefilled with that date)
    // When: the prefill is erased and the empty input is confirmed
    // Then: the stored due is NULL again and the mode returns to Tree
    #[test]
    fn submitting_empty_due_clears_the_date() {
        let db = Db::open_in_memory().unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        db.set_due(target.id, Some("2026-09-15")).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        app.handle_key(&db, key::Key::Char('t')).unwrap();
        for _ in 0.."2026-09-15".len() {
            app.handle_key(&db, key::Key::Backspace).unwrap();
        }
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(db.list_all().unwrap()[0].due, None);
    }

    // Tests that an invalid date keeps the input open for correction.
    // Given: a database task with the due input open
    // When: the malformed date "2026-13-99" is typed and confirmed
    // Then: an error notice appears in the status line, the mode stays
    //       Input with the typed text preserved (nothing to retype), and
    //       the stored due is unchanged
    #[test]
    fn submitting_invalid_due_keeps_input_open_with_text() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        app.handle_key(&db, key::Key::Char('t')).unwrap();
        press(&mut app, &db, "2026-13-99");
        app.handle_key(&db, key::Key::Enter).unwrap();

        let Mode::Input { editor, action } = &app.mode else {
            panic!("invalid date must keep the input open");
        };
        assert!(matches!(action, InputAction::SetDue(_)));
        assert_eq!(editor.text(), "2026-13-99");
        assert!(
            app.status_line
                .as_deref()
                .is_some_and(|line| line.contains("2026-13-99")),
            "status line should name the rejected input, was {:?}",
            app.status_line
        );
        assert_eq!(db.list_all().unwrap()[0].due, None);
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
    //        (seeded label "Todo")
    // When: Enter opens the prefilled editor, "x" is appended and confirmed
    // Then: the label becomes "Todox" in the database and in the reloaded
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
        assert_eq!(editor.text(), "Todo", "editor must be prefilled");
        press(&mut app, &db, "x");
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(manage_state(&app).editing, Editing::None));
        assert_eq!(app.statuses[0].label, "Todox");
        assert_eq!(db.list_statuses().unwrap()[0].label, "Todox");
    }

    // Tests that submitting a blanked-out label leaves the status alone.
    // Given: a label edit opened on "Todo"
    // When: the prefill is erased entirely and confirmed
    // Then: the label is unchanged (blank input acts as a cancel)
    #[test]
    fn manage_blank_label_submit_is_a_cancel() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        app.handle_key(&db, key::Key::Enter).unwrap();
        for _ in 0.."Todo".chars().count() {
            app.handle_key(&db, key::Key::Backspace).unwrap();
        }
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert_eq!(app.statuses[0].label, "Todo");
        assert_eq!(db.list_statuses().unwrap()[0].label, "Todo");
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

    // Tests editing the color cell to an RGB hex value.
    // Given: a color edit opened on the first row (prefill "gray")
    // When: the prefill is replaced with "#ff8800" and confirmed
    // Then: the value is saved and no unknown-color notice appears,
    //       because the hex spec parses to a real color
    #[test]
    fn manage_hex_color_saves_without_warning() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "ll");

        app.handle_key(&db, key::Key::Enter).unwrap();
        for _ in 0.."gray".len() {
            app.handle_key(&db, key::Key::Backspace).unwrap();
        }
        press(&mut app, &db, "#ff8800");
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert_eq!(app.statuses[0].color, "#ff8800");
        assert_eq!(db.list_statuses().unwrap()[0].color, "#ff8800");
        assert_eq!(app.status_line, None, "a valid hex color needs no warning");
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
    //        key of another status (Ready)
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
    // When: "n" is pressed, "review" is typed and confirmed
    // Then: a 6th status appears at the tail with kind open, color gray and
    //       the first free key 'a', and the cursor moves onto its row
    #[test]
    fn manage_add_creates_status_with_defaults() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        press(&mut app, &db, "n");
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

        press(&mut app, &db, "D");

        assert_eq!(app.statuses.len(), 5);
        let message = app.status_line.as_deref().unwrap();
        assert!(message.contains('1'), "notice should carry the task count");
    }

    // Tests the delete guard for the default status.
    // Given: the modal cursor on the first row, which is the seeded default
    // When: "D" is pressed
    // Then: nothing is deleted and a notice explains the refusal
    #[test]
    fn manage_delete_default_is_blocked_with_message() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        press(&mut app, &db, "D");

        assert_eq!(app.statuses.len(), 5);
        assert!(app.status_line.is_some());
    }

    // Tests deleting an unprotected status.
    // Given: the modal cursor on the second seeded status, which is neither
    //        the default nor referenced by any task
    // When: "D" is pressed
    // Then: the status disappears from the app list and the database, and
    //       the cursor stays on a valid row
    #[test]
    fn manage_delete_unused_removes_row() {
        let db = Db::open_in_memory().unwrap();
        let victim = db.list_statuses().unwrap()[1].id;
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "j");

        press(&mut app, &db, "D");

        assert_eq!(app.statuses.len(), 4);
        assert!(app.statuses.iter().all(|s| s.id != victim));
        assert!(manage_state(&app).row < app.statuses.len());
    }

    // Tests reordering statuses from the modal.
    // Given: the modal cursor on the first seeded status (Todo)
    // When: Ctrl-j is pressed (matching the tree's task-move key)
    // Then: the status swaps with the one below it, both in the app list
    //       and persistently, and the cursor follows the moved row
    #[test]
    fn manage_ctrl_j_moves_status_down_and_follows_it() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        app.handle_key(&db, key::Key::Ctrl('j')).unwrap();

        assert_eq!(app.statuses[0].label, "Ready");
        assert_eq!(app.statuses[1].label, "Todo");
        assert_eq!(db.list_statuses().unwrap()[0].label, "Ready");
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
        press(&mut app, &db, "D");
        assert!(app.status_line.is_some());

        press(&mut app, &db, "j");

        assert!(app.status_line.is_none());
    }

    /// Creates root tasks titled a, b, c and returns their ids.
    fn three_roots(db: &Db) -> (i64, i64, i64) {
        let status = default_status(db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let b = db.create_task(None, "b", None, status).unwrap().id;
        let c = db.create_task(None, "c", None, status).unwrap().id;
        (a, b, c)
    }

    fn root_titles(db: &Db) -> Vec<String> {
        db.list_children(None)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect()
    }

    // Tests moving the selected task down among its siblings.
    // Given: roots a, b, c with the cursor on a
    // When: Ctrl-j is pressed
    // Then: the persisted order becomes b, a, c and the cursor follows a to
    //       its new position
    #[test]
    fn ctrl_j_moves_task_down_and_cursor_follows() {
        let db = Db::open_in_memory().unwrap();
        let (a, ..) = three_roots(&db);
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(a);

        app.handle_key(&db, key::Key::Ctrl('j')).unwrap();

        assert_eq!(root_titles(&db), ["b", "a", "c"]);
        assert_eq!(selected_id(&app), Some(a));
        assert_eq!(app.selected, 1, "cursor row moved down with the task");
    }

    // Tests moving the selected task up at the top edge.
    // Given: roots a, b, c with the cursor on the first root a
    // When: Ctrl-k is pressed
    // Then: nothing changes (no wrap-around, no error)
    #[test]
    fn ctrl_k_at_top_edge_is_a_no_op() {
        let db = Db::open_in_memory().unwrap();
        let (a, ..) = three_roots(&db);
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(a);

        app.handle_key(&db, key::Key::Ctrl('k')).unwrap();

        assert_eq!(root_titles(&db), ["a", "b", "c"]);
        assert_eq!(selected_id(&app), Some(a));
    }

    // Tests indenting a task under its preceding sibling.
    // Given: roots a, b, c with the cursor on b
    // When: > is pressed
    // Then: b becomes a's child, a is auto-expanded so b stays visible, and
    //       the cursor stays on b
    #[test]
    fn alt_l_indents_under_preceding_sibling_and_expands_it() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, _) = three_roots(&db);
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(b);

        app.handle_key(&db, key::Key::Char('>')).unwrap();

        let moved = db
            .list_all()
            .unwrap()
            .into_iter()
            .find(|t| t.id == b)
            .unwrap();
        assert_eq!(moved.parent_id, Some(a));
        assert!(app.expanded.contains(&a), "new parent must be expanded");
        assert_eq!(selected_id(&app), Some(b));
    }

    // Tests indenting the first sibling.
    // Given: roots a, b, c with the cursor on a (no preceding sibling)
    // When: > is pressed
    // Then: nothing changes
    #[test]
    fn alt_l_on_first_sibling_is_a_no_op() {
        let db = Db::open_in_memory().unwrap();
        let (a, ..) = three_roots(&db);
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(a);

        app.handle_key(&db, key::Key::Char('>')).unwrap();

        assert_eq!(root_titles(&db), ["a", "b", "c"]);
        assert!(db.list_all().unwrap().iter().all(|t| t.parent_id.is_none()));
    }

    // Tests outdenting a nested task.
    // Given: root a with child x (a expanded, cursor on x), plus root b
    // When: < is pressed
    // Then: x becomes a root task placed right after its old parent a, and
    //       the cursor stays on x
    #[test]
    fn alt_h_outdents_to_after_old_parent() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        db.create_task(None, "b", None, status).unwrap();
        let x = db.create_task(Some(a), "x", None, status).unwrap().id;
        let mut app = app_for(&db, db.list_all().unwrap());
        app.expanded.insert(a);
        app.rebuild_rows();
        app.select_task(x);

        app.handle_key(&db, key::Key::Char('<')).unwrap();

        assert_eq!(root_titles(&db), ["a", "x", "b"]);
        assert_eq!(selected_id(&app), Some(x));
    }

    // Tests outdenting a root-level task.
    // Given: roots a, b, c with the cursor on b
    // When: < is pressed
    // Then: nothing changes (there is no level above the roots)
    #[test]
    fn alt_h_on_root_task_is_a_no_op() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, _) = three_roots(&db);
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(b);

        app.handle_key(&db, key::Key::Char('<')).unwrap();

        assert_eq!(root_titles(&db), ["a", "b", "c"]);
    }

    // Tests that outdenting cannot move a task out of the zoomed subtree.
    // Given: root a > child x, zoomed on a with the cursor on x
    // When: < is pressed
    // Then: x stays a's child and the zoom is untouched
    #[test]
    fn alt_h_inside_zoom_does_not_escape_zoom_root() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let x = db.create_task(Some(a), "x", None, status).unwrap().id;
        let mut app = app_for(&db, db.list_all().unwrap());
        app.zoom_root = Some(a);
        app.rebuild_rows();
        app.select_task(x);

        app.handle_key(&db, key::Key::Char('<')).unwrap();

        let child = db
            .list_all()
            .unwrap()
            .into_iter()
            .find(|t| t.id == x)
            .unwrap();
        assert_eq!(child.parent_id, Some(a));
        assert_eq!(app.zoom_root, Some(a));
    }

    // Tests that the delete key asks for confirmation with the subtree size.
    // Given: root a with children x and y (3 tasks in the subtree), cursor
    //        on a
    // When: "D" is pressed
    // Then: the mode becomes ConfirmDelete for a with count 3, the prompt
    //       appears in the status line, and nothing is deleted yet
    #[test]
    fn shift_d_asks_for_confirmation_with_subtree_count() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        db.create_task(Some(a), "x", None, status).unwrap();
        db.create_task(Some(a), "y", None, status).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        press(&mut app, &db, "D");

        assert!(matches!(
            app.mode,
            Mode::ConfirmDelete { task_id, count } if task_id == a && count == 3
        ));
        assert_eq!(app.status_line.as_deref(), Some("delete 3 task(s)? (y/n)"));
        assert_eq!(db.list_all().unwrap().len(), 3);
    }

    // Tests that a childless task is deleted instantly, without the prompt.
    // Given: roots a, b, c (all leaves) with the cursor on b
    // When: "D" is pressed
    // Then: b is gone immediately (undo covers mistakes), the cursor lands
    //       on the next sibling c, and the status line names the deleted
    //       task and points at undo
    #[test]
    fn shift_d_on_leaf_deletes_immediately_and_selects_next_sibling() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, c) = three_roots(&db);
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(b);

        press(&mut app, &db, "D");

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(root_titles(&db), ["a", "c"]);
        assert_eq!(selected_id(&app), Some(c));
        assert!(db.list_all().unwrap().iter().all(|t| t.id != b));
        assert_eq!(
            app.status_line.as_deref(),
            Some("deleted \"b\" (u to undo)")
        );
    }

    // Tests confirming a subtree delete.
    // Given: root b with child x (2 tasks) among roots a, b, c, with the
    //        delete prompt open for b
    // When: "y" is pressed
    // Then: the subtree is deleted, the cursor lands on the next sibling c,
    //       the mode returns to Tree, and the status line reports the count
    #[test]
    fn y_confirms_subtree_delete_and_selects_next_sibling() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, c) = three_roots(&db);
        db.create_task(Some(b), "x", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(b);

        press(&mut app, &db, "Dy");

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(root_titles(&db), ["a", "c"]);
        assert_eq!(selected_id(&app), Some(c));
        assert_eq!(
            app.status_line.as_deref(),
            Some("deleted 2 task(s) (u to undo)")
        );
    }

    // Tests the fallback selection after deleting an only child.
    // Given: root a with the single child x (a expanded, cursor on x)
    // When: the leaf x is deleted via "D" (no confirmation)
    // Then: the cursor falls back to the parent a
    #[test]
    fn delete_only_child_selects_parent() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let x = db.create_task(Some(a), "x", None, status).unwrap().id;
        let mut app = app_for(&db, db.list_all().unwrap());
        app.expanded.insert(a);
        app.rebuild_rows();
        app.select_task(x);

        press(&mut app, &db, "D");

        assert!(db.list_all().unwrap().iter().all(|t| t.id != x));
        assert_eq!(selected_id(&app), Some(a));
    }

    // Tests cancelling a subtree delete.
    // Given: root b with child x among roots a, b, c, prompt open for b
    // When: any key other than "y" ("n") is pressed
    // Then: nothing is deleted and the mode returns to Tree
    #[test]
    fn any_other_key_cancels_subtree_delete() {
        let db = Db::open_in_memory().unwrap();
        let (_, b, _) = three_roots(&db);
        db.create_task(Some(b), "x", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(b);

        press(&mut app, &db, "Dn");

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(root_titles(&db), ["a", "b", "c"]);
        assert_eq!(db.list_all().unwrap().len(), 4);
    }

    // Tests the delete key on an empty view.
    // Given: no tasks at all
    // When: "D" is pressed
    // Then: the mode stays Tree (there is nothing to delete)
    #[test]
    fn shift_d_on_empty_view_is_a_no_op() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        press(&mut app, &db, "D");

        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests deleting the last remaining task.
    // Given: a single root task with the cursor on it
    // When: the leaf is deleted via "D" (no confirmation)
    // Then: the view is empty and no cursor row remains (no panic)
    #[test]
    fn delete_last_task_leaves_empty_view() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "only", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        press(&mut app, &db, "D");

        assert!(app.rows.is_empty());
        assert!(db.list_all().unwrap().is_empty());
    }

    // Tests that deleting inside a zoom keeps the zoom.
    // Given: root a > children x, y, zoomed on a with the cursor on x
    // When: the leaf x is deleted via "D" (no confirmation)
    // Then: the zoom root stays a and the cursor lands on the sibling y
    #[test]
    fn delete_inside_zoom_keeps_zoom_root() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let x = db.create_task(Some(a), "x", None, status).unwrap().id;
        let y = db.create_task(Some(a), "y", None, status).unwrap().id;
        let mut app = app_for(&db, db.list_all().unwrap());
        app.zoom_root = Some(a);
        app.rebuild_rows();
        app.select_task(x);

        press(&mut app, &db, "D");

        assert_eq!(app.zoom_root, Some(a));
        assert_eq!(selected_id(&app), Some(y));
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

    // Tests that undo brings a restored subtree into view.
    // Given: a collapsed subtree a > x > leaf deleted through the engine,
    //        with the app reloaded afterwards
    // When: "u" is pressed
    // Then: the subtree is back, its inner ancestors are expanded so every
    //       restored task is visible, the cursor lands on the subtree root,
    //       and the status line names the undone action
    #[test]
    fn u_undoes_delete_expands_ancestors_and_selects_restored_task() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let x = db.create_task(Some(a), "x", None, status).unwrap().id;
        db.create_task(Some(x), "leaf", None, status).unwrap();
        db.delete_subtree(a).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        press(&mut app, &db, "u");

        assert_eq!(db.list_all().unwrap().len(), 3);
        assert!(
            app.expanded.contains(&a),
            "restored parent must be expanded"
        );
        assert!(
            app.expanded.contains(&x),
            "restored parent must be expanded"
        );
        assert_eq!(selected_id(&app), Some(a));
        assert_eq!(app.status_line.as_deref(), Some("undid: delete \"a\""));
    }

    // Tests undo with an empty history.
    // Given: an app over a database where nothing has been changed
    // When: "u" is pressed
    // Then: nothing happens except a "nothing to undo" notice
    #[test]
    fn u_with_empty_history_reports_nothing_to_undo() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        press(&mut app, &db, "u");

        assert_eq!(app.status_line.as_deref(), Some("nothing to undo"));
        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests undoing a creation, where no affected task survives.
    // Given: a task created through the app's input flow
    // When: "u" is pressed
    // Then: the task is gone again, the view is empty without a crash, and
    //       the status line names the undone creation
    #[test]
    fn u_after_create_removes_task_and_reports() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        app.run_command(id::CREATE_TASK);
        press(&mut app, &db, "t");
        app.handle_key(&db, key::Key::Enter).unwrap();
        assert_eq!(db.list_all().unwrap().len(), 1);

        press(&mut app, &db, "u");

        assert!(db.list_all().unwrap().is_empty());
        assert!(app.rows.is_empty());
        assert_eq!(app.status_line.as_deref(), Some("undid: create \"t\""));
    }

    // Tests that undo drops the zoom when the restored task is outside it.
    // Given: two roots a and b, with a deleted through the engine and the
    //        app zoomed on b
    // When: "u" is pressed
    // Then: the zoom is cleared so the restored root a is visible, and the
    //       cursor lands on it
    #[test]
    fn u_unzooms_when_restored_task_is_outside_zoom() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let b = db.create_task(None, "b", None, status).unwrap().id;
        db.create_task(Some(b), "inside", None, status).unwrap();
        db.delete_subtree(a).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.zoom_root = Some(b);
        app.rebuild_rows();

        press(&mut app, &db, "u");

        assert_eq!(app.zoom_root, None);
        assert_eq!(selected_id(&app), Some(a));
    }

    // Tests that undo keeps the zoom when the restored task is inside it.
    // Given: root b > child x, with x deleted through the engine and the
    //        app zoomed on b
    // When: "u" is pressed
    // Then: the zoom stays on b and the cursor lands on the restored x
    #[test]
    fn u_keeps_zoom_when_restored_task_is_inside_zoom() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let b = db.create_task(None, "b", None, status).unwrap().id;
        let x = db.create_task(Some(b), "x", None, status).unwrap().id;
        db.delete_subtree(x).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.zoom_root = Some(b);
        app.rebuild_rows();

        press(&mut app, &db, "u");

        assert_eq!(app.zoom_root, Some(b));
        assert_eq!(selected_id(&app), Some(x));
    }

    // Tests redo after an undo.
    // Given: a deleted root whose deletion was undone via "u"
    // When: "U" is pressed
    // Then: the task is deleted again and the status line names the redone
    //       action; a further "U" reports nothing to redo
    #[test]
    fn shift_u_redoes_the_undone_delete() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .create_task(None, "a", None, default_status(&db))
            .unwrap()
            .id;
        db.delete_subtree(a).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "u");
        assert_eq!(db.list_all().unwrap().len(), 1);

        press(&mut app, &db, "U");

        assert!(db.list_all().unwrap().is_empty());
        assert_eq!(app.status_line.as_deref(), Some("redid: delete \"a\""));

        press(&mut app, &db, "U");
        assert_eq!(app.status_line.as_deref(), Some("nothing to redo"));
    }

    // Tests that undoing a status edit refreshes the loaded statuses.
    // Given: a status renamed through the engine, with the app started
    //        after the rename
    // When: "u" is pressed
    // Then: the app's status list shows the original label again and the
    //       status line names the undone action
    #[test]
    fn u_after_status_edit_reloads_statuses() {
        let db = Db::open_in_memory().unwrap();
        let victim = db.list_statuses().unwrap()[1].id;
        let old_label = db.list_statuses().unwrap()[1].label.clone();
        db.update_status_label(victim, "renamed").unwrap();
        let mut app = app_for(&db, vec![]);
        assert_eq!(app.statuses[1].label, "renamed");

        press(&mut app, &db, "u");

        assert_eq!(app.statuses[1].label, old_label);
        let message = app.status_line.as_deref().unwrap();
        assert!(message.starts_with("undid: rename status"));
    }

    /// Renames the label of the modal's current row by appending `suffix`.
    fn append_to_label(app: &mut App, db: &Db, suffix: &str) {
        app.handle_key(db, key::Key::Enter).unwrap();
        press(app, db, suffix);
        app.handle_key(db, key::Key::Enter).unwrap();
    }

    // Tests that a modal session lands as one outer undo step.
    // Given: a status-management session with two edits (a label rename and
    //        a reorder), closed with Esc
    // When: "u" is pressed once back in the tree
    // Then: both edits are reverted together (full status snapshot match),
    //       the status line names the session, and "U" reapplies it whole
    #[test]
    fn u_after_modal_close_reverts_whole_session() {
        let db = Db::open_in_memory().unwrap();
        let statuses_before = db.list_statuses().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        append_to_label(&mut app, &db, "x");
        press(&mut app, &db, "J");
        let statuses_edited = db.list_statuses().unwrap();
        app.handle_key(&db, key::Key::Esc).unwrap();

        press(&mut app, &db, "u");
        assert_eq!(db.list_statuses().unwrap(), statuses_before);
        assert_eq!(app.statuses, statuses_before);
        assert_eq!(app.status_line.as_deref(), Some("undid: edit statuses"));

        press(&mut app, &db, "U");
        assert_eq!(db.list_statuses().unwrap(), statuses_edited);
        assert_eq!(app.status_line.as_deref(), Some("redid: edit statuses"));
    }

    // Tests that undo inside the modal cannot reach earlier task edits.
    // Given: a task created before the modal was opened, and a modal with
    //        no edits of its own
    // When: "u" is pressed inside the modal
    // Then: the task survives, the modal stays open, and the status line
    //       reports there is nothing to undo
    #[test]
    fn modal_undo_does_not_descend_into_pre_modal_history() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        app.run_command(id::CREATE_TASK);
        press(&mut app, &db, "t");
        app.handle_key(&db, key::Key::Enter).unwrap();
        open_manage(&mut app, &db);

        press(&mut app, &db, "u");

        assert!(matches!(app.mode, Mode::StatusManage(_)));
        assert_eq!(app.status_line.as_deref(), Some("nothing to undo"));
        assert_eq!(db.list_all().unwrap().len(), 1);
    }

    // Tests fine-grained undo and redo inside the modal.
    // Given: an open modal in which the first status's label was renamed
    // When: "u" and then "U" are pressed inside the modal
    // Then: the undo restores the old label in the reloaded table and names
    //       the edit; the redo brings the new label back
    #[test]
    fn modal_undo_and_redo_step_through_session_edits() {
        let db = Db::open_in_memory().unwrap();
        let old_label = db.list_statuses().unwrap()[0].label.clone();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        append_to_label(&mut app, &db, "x");

        press(&mut app, &db, "u");
        assert!(matches!(app.mode, Mode::StatusManage(_)));
        assert_eq!(app.statuses[0].label, old_label);
        let message = app.status_line.as_deref().unwrap();
        assert!(message.starts_with("undid: rename status"));

        press(&mut app, &db, "U");
        assert_eq!(app.statuses[0].label, format!("{old_label}x"));
        let message = app.status_line.as_deref().unwrap();
        assert!(message.starts_with("redid: rename status"));
    }

    // Tests that an edit-free modal session leaves the history untouched.
    // Given: a task creation undone before the modal (redo holds it), and a
    //        modal opened and closed without edits
    // When: "U" is pressed back in the tree
    // Then: the stashed redo still works and recreates the task — no empty
    //       "edit statuses" step was recorded in either direction
    #[test]
    fn modal_without_edits_leaves_history_untouched() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        app.run_command(id::CREATE_TASK);
        press(&mut app, &db, "t");
        app.handle_key(&db, key::Key::Enter).unwrap();
        press(&mut app, &db, "u");
        assert!(db.list_all().unwrap().is_empty());
        open_manage(&mut app, &db);
        app.handle_key(&db, key::Key::Esc).unwrap();

        press(&mut app, &db, "U");

        assert_eq!(app.status_line.as_deref(), Some("redid: create \"t\""));
        assert_eq!(db.list_all().unwrap().len(), 1);
    }

    // Tests that a modal undo which empties the table cursor row clamps it.
    // Given: a modal session whose only edit is adding a status, with the
    //        cursor left on the new last row
    // When: "u" is pressed inside the modal
    // Then: the added status is gone and the cursor sits on a valid row
    #[test]
    fn modal_undo_of_add_clamps_cursor_row() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        press(&mut app, &db, "n");
        press(&mut app, &db, "review");
        app.handle_key(&db, key::Key::Enter).unwrap();
        assert_eq!(manage_state(&app).row, 5);

        press(&mut app, &db, "u");

        assert_eq!(app.statuses.len(), 5);
        assert!(manage_state(&app).row < app.statuses.len());
    }

    // Tests that "e" requests an external-editor session for the selection.
    // Given: two database tasks with the cursor on the second one
    // When: "e" is pressed in the Tree context
    // Then: the app records a pending note edit for that task (the event
    //       loop, which owns the terminal, performs the actual handover)
    //       and stays in Tree mode
    #[test]
    fn e_requests_note_edit_for_selected_task() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "other", None, default_status(&db))
            .unwrap();
        let target = db
            .create_task(None, "t", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        app.select_task(target.id);

        app.handle_key(&db, key::Key::Char('e')).unwrap();

        assert_eq!(app.pending_note_edit, Some(target.id));
        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests "e" on an empty view.
    // Given: no tasks at all
    // When: "e" is pressed
    // Then: no note edit is requested
    #[test]
    fn e_on_empty_view_requests_nothing() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        app.handle_key(&db, key::Key::Char('e')).unwrap();

        assert_eq!(app.pending_note_edit, None);
    }

    use crate::query_view::Focus;

    fn query_state(app: &App) -> &query_view::QueryState {
        let Mode::Query(state) = &app.mode else {
            panic!("expected the query view to be open");
        };
        state
    }

    fn result_titles(app: &App) -> Vec<String> {
        query_state(app)
            .results
            .iter()
            .map(|task| task.title.clone())
            .collect()
    }

    /// The seeded status whose kind is done ("Done").
    fn done_status(db: &Db) -> i64 {
        db.list_statuses()
            .unwrap()
            .into_iter()
            .find(|s| s.kind == engine::StatusKind::Done)
            .unwrap()
            .id
    }

    // Tests that "f" opens the filter menu and "o" narrows the view.
    // Given: one done root task, visible under the default all filter
    // When: "f" is pressed, then "o"
    // Then: the menu opens (FilterSelect context), the filter becomes
    //       Open, the mode returns to Tree, and the done task's row is gone
    #[test]
    fn f_then_o_hides_finished_tasks_in_tree() {
        let db = Db::open_in_memory().unwrap();
        let done = db
            .create_task(None, "done work", None, done_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        assert_eq!(
            selected_id(&app),
            Some(done.id),
            "the default filter shows done tasks"
        );

        press(&mut app, &db, "f");
        assert!(matches!(app.mode, Mode::FilterSelect));
        assert_eq!(app.context(), command::Context::FilterSelect);
        press(&mut app, &db, "o");

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(app.filter, engine::Filter::Open);
        assert!(app.rows.is_empty(), "the open filter hides done tasks");
    }

    // Tests filtering the tree to one status from the menu.
    // Given: a task on the default status and one on the "Doing" status
    //        (seeded menu key "d")
    // When: "f" then "d" are pressed
    // Then: the filter becomes Status(Doing) and only that task's row is
    //       left in the tree
    #[test]
    fn f_then_status_key_filters_tree_to_that_status() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "other", None, default_status(&db))
            .unwrap();
        let statuses = db.list_statuses().unwrap();
        let doing = statuses.iter().find(|s| s.key == 'd').unwrap().id;
        let target = db.create_task(None, "doing work", None, doing).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        press(&mut app, &db, "fd");

        assert_eq!(app.filter, engine::Filter::Status(doing));
        assert_eq!(app.rows.len(), 1);
        assert_eq!(selected_id(&app), Some(target.id));
    }

    // Tests cancelling the filter menu.
    // Given: the filter menu opened from the tree
    // When: Esc is pressed
    // Then: the mode returns to Tree and the filter is unchanged
    #[test]
    fn esc_cancels_filter_menu_without_change() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        press(&mut app, &db, "f");

        app.handle_key(&db, key::Key::Esc).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(app.filter, engine::Filter::All);
    }

    // Tests that keys not bound to any filter are ignored.
    // Given: the filter menu opened from the tree, where "z" names neither
    //        a fixed choice nor a seeded status key
    // When: "z" is pressed
    // Then: the menu stays open and the filter is unchanged
    #[test]
    fn unbound_key_in_filter_menu_is_ignored() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        press(&mut app, &db, "fz");

        assert!(matches!(app.mode, Mode::FilterSelect));
        assert_eq!(app.filter, engine::Filter::All);
    }

    // Tests that "/" opens the query view listing everything under the
    // current filter.
    // Given: an open task and a done task under the default all filter
    // When: "/" is pressed
    // Then: the mode becomes Query with the text edit focused (Input
    //       context for the footer) and the results hold both tasks — the
    //       empty query means "filter and sort only"
    #[test]
    fn slash_opens_query_with_all_tasks_under_filter() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "open work", None, default_status(&db))
            .unwrap();
        db.create_task(None, "done work", None, done_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        press(&mut app, &db, "/");

        assert_eq!(query_state(&app).focus, Focus::Edit);
        assert_eq!(app.context(), command::Context::Input);
        assert_eq!(result_titles(&app), ["open work", "done work"]);
    }

    // Tests that every keystroke of the search text re-runs the search.
    // Given: tasks "design" and "deploy", with the query edit open
    // When: "de" then "s" are typed
    // Then: after "de" both tasks match; after "des" only "design" is left
    #[test]
    fn typing_in_query_edit_reruns_search_incrementally() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        db.create_task(None, "design", None, status).unwrap();
        db.create_task(None, "deploy", None, status).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        press(&mut app, &db, "/de");
        assert_eq!(result_titles(&app), ["design", "deploy"]);

        press(&mut app, &db, "s");
        assert_eq!(result_titles(&app), ["design"]);
    }

    // Tests that a Japanese two-character word in a note is found.
    // Given: a task whose note contains "設計" and one that does not
    // When: "設計" is typed into the query edit
    // Then: only the task with the note hit remains in the results
    #[test]
    fn query_finds_japanese_word_in_note() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let hit = db.create_task(None, "auth", None, status).unwrap();
        db.set_note(hit.id, "JWT の設計を検討").unwrap();
        db.create_task(None, "other", None, status).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());

        press(&mut app, &db, "/設計");

        assert_eq!(result_titles(&app), ["auth"]);
    }

    // Tests leaving the query edit with Esc.
    // Given: an open query edit
    // When: Esc is pressed
    // Then: the mode returns to Tree
    #[test]
    fn esc_in_query_edit_returns_to_tree() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        press(&mut app, &db, "/");

        app.handle_key(&db, key::Key::Esc).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests browsing the results after confirming the search text.
    // Given: three matching tasks and the query edit open
    // When: Enter confirms the text, then j/j/k and ge/gg move the cursor
    // Then: the focus is Browse (Query context) and the selection follows
    //       each movement, clamped to the result range
    #[test]
    fn enter_switches_to_browse_where_jk_and_gg_ge_move() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        for title in ["a", "b", "c"] {
            db.create_task(None, title, None, status).unwrap();
        }
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "/");

        app.handle_key(&db, key::Key::Enter).unwrap();
        assert_eq!(query_state(&app).focus, Focus::Browse);
        assert_eq!(app.context(), command::Context::Query);

        press(&mut app, &db, "jj");
        assert_eq!(query_state(&app).selected, 2);
        press(&mut app, &db, "j");
        assert_eq!(query_state(&app).selected, 2, "clamped at the last item");
        press(&mut app, &db, "k");
        assert_eq!(query_state(&app).selected, 1);
        press(&mut app, &db, "gg");
        assert_eq!(query_state(&app).selected, 0);
        press(&mut app, &db, "ge");
        assert_eq!(query_state(&app).selected, 2);
    }

    // Tests reopening the text edit from the browse focus.
    // Given: a browsed query whose text is "de"
    // When: "/" is pressed
    // Then: the focus returns to Edit with the text preserved and the
    //       cursor at its end, ready to continue typing
    #[test]
    fn slash_in_browse_reopens_edit_with_text_preserved() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "design", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "/de");
        app.handle_key(&db, key::Key::Enter).unwrap();

        press(&mut app, &db, "/");

        let state = query_state(&app);
        assert_eq!(state.focus, Focus::Edit);
        assert_eq!(state.editor.text(), "de");
        assert_eq!(state.editor.cursor(), "de".len());
    }

    // Tests picking a sort order from the sort menu.
    // Given: tasks created in the order b (due 2026-02-01), a (due
    //        2026-01-01), c (no due), browsed with the default tree order
    // When: "," opens the sort menu and "d" picks the due sort
    // Then: the results reorder to earliest due first with the dateless
    //       task last, and the focus returns to Browse
    #[test]
    fn comma_then_d_sorts_results_by_due() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let b = db.create_task(None, "b", None, status).unwrap();
        db.set_due(b.id, Some("2026-02-01")).unwrap();
        let a = db.create_task(None, "a", None, status).unwrap();
        db.set_due(a.id, Some("2026-01-01")).unwrap();
        db.create_task(None, "c", None, status).unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "/");
        app.handle_key(&db, key::Key::Enter).unwrap();
        assert_eq!(result_titles(&app), ["b", "a", "c"], "tree order at first");

        press(&mut app, &db, ",");
        assert_eq!(query_state(&app).focus, Focus::SortMenu);
        assert_eq!(app.context(), command::Context::SortSelect);
        press(&mut app, &db, "d");

        let state = query_state(&app);
        assert_eq!(state.focus, Focus::Browse);
        assert_eq!(state.sort, engine::Sort::Due);
        assert_eq!(result_titles(&app), ["a", "b", "c"]);
    }

    // Tests cancelling the sort menu.
    // Given: an open sort menu over a browsed query
    // When: Esc is pressed
    // Then: the focus returns to Browse with the sort unchanged
    #[test]
    fn esc_cancels_sort_menu_without_change() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        press(&mut app, &db, "/");
        app.handle_key(&db, key::Key::Enter).unwrap();
        press(&mut app, &db, ",");

        app.handle_key(&db, key::Key::Esc).unwrap();

        let state = query_state(&app);
        assert_eq!(state.focus, Focus::Browse);
        assert_eq!(state.sort, engine::Sort::TreeOrder);
    }

    // Tests changing the filter from inside the query view.
    // Given: an open task and a done task, browsed under the default all
    //        filter (results show both)
    // When: "f" opens the filter menu and "o" picks the open filter
    // Then: the results drop the done task, the shared app filter becomes
    //       Open (so the tree follows), and the focus is Browse
    #[test]
    fn f_in_browse_refilters_results_and_tree() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "open work", None, default_status(&db))
            .unwrap();
        db.create_task(None, "done work", None, done_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "/");
        app.handle_key(&db, key::Key::Enter).unwrap();
        assert_eq!(result_titles(&app), ["open work", "done work"]);

        press(&mut app, &db, "f");
        assert_eq!(query_state(&app).focus, Focus::FilterMenu);
        assert_eq!(app.context(), command::Context::FilterSelect);
        press(&mut app, &db, "o");

        assert_eq!(query_state(&app).focus, Focus::Browse);
        assert_eq!(app.filter, engine::Filter::Open);
        assert_eq!(result_titles(&app), ["open work"]);
        assert_eq!(app.rows.len(), 1, "the tree follows the shared filter");
    }

    // Tests both ways of leaving the browse focus.
    // Given: a browsed query
    // When: "q" is pressed; the query is reopened and Esc is pressed
    // Then: both keys return the app to the Tree mode
    #[test]
    fn q_and_esc_close_query_browse() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);

        press(&mut app, &db, "/");
        app.handle_key(&db, key::Key::Enter).unwrap();
        press(&mut app, &db, "q");
        assert!(matches!(app.mode, Mode::Tree));

        press(&mut app, &db, "/");
        app.handle_key(&db, key::Key::Enter).unwrap();
        app.handle_key(&db, key::Key::Esc).unwrap();
        assert!(matches!(app.mode, Mode::Tree));
    }

    // Tests that task-editing keys are inert while browsing results.
    // Given: a browsed query over one task
    // When: the tree keys "r" (rename), "D" (delete) and "t" (due) are
    //       pressed
    // Then: the query view stays open in Browse focus and the task is
    //       untouched — the query view is read-only
    #[test]
    fn edit_keys_are_inert_while_browsing() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "keep", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "/");
        app.handle_key(&db, key::Key::Enter).unwrap();

        press(&mut app, &db, "rDt");

        assert_eq!(query_state(&app).focus, Focus::Browse);
        let tasks = db.list_all().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "keep");
        assert_eq!(tasks[0].due, None);
    }

    // Tests jumping from a result to its place in the tree.
    // Given: a chain a > x > leaf (all collapsed) plus a root b, with the
    //        app zoomed on b and the query browsing a hit on "leaf"
    // When: Enter is pressed on the result
    // Then: the app returns to the tree, drops the zoom (the target is
    //       outside it), expands the target's ancestors and selects it
    #[test]
    fn enter_jumps_to_result_expanding_ancestors_and_unzooming() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let x = db.create_task(Some(a), "x", None, status).unwrap().id;
        let leaf = db.create_task(Some(x), "leaf", None, status).unwrap().id;
        let b = db.create_task(None, "b", None, status).unwrap().id;
        let mut app = app_for(&db, db.list_all().unwrap());
        app.zoom_root = Some(b);
        app.rebuild_rows();
        press(&mut app, &db, "/leaf");
        app.handle_key(&db, key::Key::Enter).unwrap();
        assert_eq!(result_titles(&app), ["leaf"]);

        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(app.zoom_root, None);
        assert!(app.expanded.contains(&a), "ancestors must be expanded");
        assert!(app.expanded.contains(&x), "ancestors must be expanded");
        assert_eq!(selected_id(&app), Some(leaf));
    }

    // Tests that a jump inside the zoomed subtree keeps the zoom.
    // Given: root a > child x with the app zoomed on a, browsing a hit on x
    // When: Enter is pressed on the result
    // Then: the zoom stays on a and the cursor lands on x
    #[test]
    fn enter_jump_inside_zoom_keeps_zoom_root() {
        let db = Db::open_in_memory().unwrap();
        let status = default_status(&db);
        let a = db.create_task(None, "a", None, status).unwrap().id;
        let x = db.create_task(Some(a), "x", None, status).unwrap().id;
        let mut app = app_for(&db, db.list_all().unwrap());
        app.zoom_root = Some(a);
        app.rebuild_rows();
        press(&mut app, &db, "/x");
        app.handle_key(&db, key::Key::Enter).unwrap();

        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(app.mode, Mode::Tree));
        assert_eq!(app.zoom_root, Some(a));
        assert_eq!(selected_id(&app), Some(x));
    }

    // Tests browsing an empty result list.
    // Given: a query whose text matches nothing, confirmed into browse
    // When: movement keys and Enter are pressed
    // Then: nothing crashes and the jump is a no-op (the view stays open,
    //       there is nothing to jump to)
    #[test]
    fn browse_with_no_matches_ignores_movement_and_jump() {
        let db = Db::open_in_memory().unwrap();
        db.create_task(None, "design", None, default_status(&db))
            .unwrap();
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "/zzz");
        app.handle_key(&db, key::Key::Enter).unwrap();
        assert!(query_state(&app).results.is_empty());

        press(&mut app, &db, "jk");
        app.handle_key(&db, key::Key::Enter).unwrap();

        assert!(matches!(app.mode, Mode::Query(_)));
    }

    /// Renders one frame into an off-screen terminal and returns its rows as
    /// plain strings, so layout assertions can read the screen as text.
    fn rendered_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    // Tests that a modal view is drawn as a popup over the task tree.
    // Given: two root tasks and the help view open, on a 60x20 terminal
    // When: a frame is rendered
    // Then: the popup is a bordered box centered at 80% of the screen, the
    //       task tree is still visible above it, and the footer keeps the
    //       bottom line
    #[test]
    fn modal_is_drawn_as_a_bordered_popup_over_the_tree() {
        let mut app = test_app(vec![task(1, None, 0), task(2, None, 1)]);
        app.mode = Mode::Help(help::HelpState::new());

        let rows = rendered_rows(&mut app, 60, 20);

        assert!(rows[0].contains("task 1"), "row 0 was: {}", rows[0]);
        assert!(rows[1].contains("task 2"), "row 1 was: {}", rows[1]);
        // 80% of 60x20, centered: 48x16 at (6, 2).
        assert_eq!(rows[2].chars().nth(6), Some('\u{250c}'));
        assert_eq!(rows[2].chars().nth(53), Some('\u{2510}'));
        assert_eq!(rows[17].chars().nth(6), Some('\u{2514}'));
        assert_eq!(rows[17].chars().nth(53), Some('\u{2518}'));
        assert!(rows[2].contains("help"), "title was: {}", rows[2]);
        assert!(!rows[19].trim().is_empty(), "the footer must stay visible");
    }

    // Tests that the search prompt moves inside the popup, above its results.
    // Given: the query view open with search text being typed, on a 60x20
    //        terminal whose popup spans rows 2..17
    // When: a frame is rendered
    // Then: the popup title carries the search header, and the prompt sits
    //       on the popup's first inner line over a divider joined to the
    //       border
    #[test]
    fn query_popup_carries_the_header_and_a_prompt_above_the_results() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        press(&mut app, &db, "/design");

        let rows = rendered_rows(&mut app, 60, 20);

        assert!(
            rows[2].contains("search: design | sort:"),
            "title was: {}",
            rows[2]
        );
        assert!(
            rows[3].contains("Search: design"),
            "prompt row was: {}",
            rows[3]
        );
        assert_eq!(rows[4].chars().nth(6), Some('\u{251c}'));
        assert_eq!(rows[4].chars().nth(53), Some('\u{2524}'));
        assert!(
            rows[4][9..30].chars().all(|c| c == '\u{2500}'),
            "divider was: {}",
            rows[4]
        );
    }

    // Tests that the help filter prompt sits above the bindings it narrows.
    // Given: the help view with "zoom" being typed into the filter, on a
    //        60x20 terminal whose popup spans rows 2..17
    // When: a frame is rendered
    // Then: the prompt is the popup's first inner row, a divider joined to
    //       the border follows it, and the matching bindings come below
    #[test]
    fn help_filter_prompt_sits_above_the_bindings() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        press(&mut app, &db, "?/zoom");

        let rows = rendered_rows(&mut app, 60, 20);

        assert!(
            rows[3].contains("Filter: zoom"),
            "prompt row was: {}",
            rows[3]
        );
        assert_eq!(rows[4].chars().nth(6), Some('\u{251c}'));
        assert_eq!(rows[4].chars().nth(53), Some('\u{2524}'));
        // The title also echoes the filter, so only the list rows are read.
        let list_rows = &rows[5..17];
        assert!(
            list_rows.iter().any(|row| row.contains("zoom")),
            "matching bindings must follow the divider, list was: {list_rows:#?}"
        );
    }

    // Tests that a cell editor puts its input at the popup's top, like the
    // query and help prompts, so every modal reads from input to list.
    // Given: the status management modal with a label edit open, on a 60x20
    //        terminal whose popup spans rows 2..17
    // When: a frame is rendered
    // Then: the prompt is the popup's first inner row, a divider joined to
    //       the border follows, and the table header sits below it
    #[test]
    fn status_cell_editor_keeps_its_prompt_above_the_table() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);
        app.handle_key(&db, key::Key::Enter).unwrap();

        let rows = rendered_rows(&mut app, 60, 20);

        assert!(rows[3].contains("Label: "), "prompt row was: {}", rows[3]);
        assert_eq!(rows[4].chars().nth(6), Some('\u{251c}'));
        assert_eq!(rows[4].chars().nth(53), Some('\u{2524}'));
        assert!(
            rows[5].contains("Label ") && rows[5].contains("Color"),
            "table header row was: {}",
            rows[5]
        );
    }

    // Tests that the status popup names itself as a management screen.
    // Given: the status management modal open
    // When: the popup title is built with room to spare
    // Then: it reads as an action, not just a list of statuses
    #[test]
    fn status_manage_popup_title_names_the_screen() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        open_manage(&mut app, &db);

        assert_eq!(popup_title(&app, 40), " manage statuses ");
    }

    // Tests that a popup title too long for the border is cut.
    // Given: the query view with a long search text and a narrow popup
    // When: the title is built
    // Then: it fits inside the border and ends with an ellipsis
    #[test]
    fn popup_title_is_cut_to_the_border_width() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        press(&mut app, &db, "/a very long search text indeed");

        let title = popup_title(&app, 20);

        assert_eq!(unicode_width::UnicodeWidthStr::width(title.as_str()), 18);
        assert!(title.ends_with("\u{2026} "), "title was: {title}");
    }

    // Tests that the status table scrolls with its cursor.
    // Given: more statuses than fit in a short popup, cursor on the last row
    // When: the popup content is laid out
    // Then: the offset scrolls the table just far enough to show the cursor
    //       row, counting the table's own header line
    #[test]
    fn status_table_scrolls_to_keep_the_cursor_visible() {
        let selected_row = 9;
        let popup_height = 5;

        let offset = overlay::scroll_to_show(selected_row + 1, popup_height);

        assert_eq!(offset, 6);
    }

    // Tests that the popup edge cannot cut a double-width character in half.
    // Given: tasks whose titles place a CJK character across the popup's
    //        left border, with the help view open
    // When: a frame is rendered
    // Then: the character left of the border is blanked, so the terminal
    //       cannot draw its right half over the border
    #[test]
    fn popup_edge_blanks_a_half_covered_wide_char() {
        let db = Db::open_in_memory().unwrap();
        for _ in 0..5 {
            // "  a" puts the third character across columns 5 and 6, and the
            // popup border sits at column 6 on a 60 column screen.
            db.create_task(None, "a\u{3042}\u{3044}\u{3046}", None, default_status(&db))
                .unwrap();
        }
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "?");

        let rows = rendered_rows(&mut app, 60, 20);

        // Cells map one to one onto characters here: the right half of a
        // wide character is stored as a space and only its left half is
        // drawn.
        assert_eq!(rows[3].chars().nth(5), Some(' '), "row 3 was: {}", rows[3]);
        assert_eq!(rows[3].chars().nth(6), Some('\u{2502}'));
    }

    // Tests that a confirmed help filter stays visible while browsing.
    // Given: the help view filtered by "zoom" and confirmed back to browse,
    //        where the prompt row is hidden
    // When: a frame is rendered
    // Then: the popup title shows the active filter
    #[test]
    fn help_popup_title_shows_the_active_filter() {
        let db = Db::open_in_memory().unwrap();
        let mut app = app_for(&db, vec![]);
        press(&mut app, &db, "?/zoom");
        app.handle_key(&db, key::Key::Enter).unwrap();

        let rows = rendered_rows(&mut app, 60, 20);

        assert!(rows[2].contains("help: zoom"), "title was: {}", rows[2]);
    }

    // Tests that a long result list scrolls inside the popup.
    // Given: 20 matching tasks with the cursor on the last one, on a 70x12
    //        terminal whose popup fits only seven result rows
    // When: a frame is rendered
    // Then: the selected result is the popup's last content row and the
    //       earlier results have scrolled out of the popup
    #[test]
    fn query_results_scroll_inside_the_popup() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..20 {
            db.create_task(None, &format!("task {i}"), None, default_status(&db))
                .unwrap();
        }
        let mut app = app_for(&db, db.list_all().unwrap());
        press(&mut app, &db, "/task");
        app.handle_key(&db, key::Key::Enter).unwrap();
        press(&mut app, &db, &"j".repeat(19));

        let rows = rendered_rows(&mut app, 70, 12);

        // 80% of 70x12, centered: the popup spans rows 1..9, so its content
        // rows are 2..8.
        assert!(rows[8].contains("task 19"), "row 8 was: {}", rows[8]);
        assert!(!rows[2].contains("task 0 "), "row 2 was: {}", rows[2]);
    }
}
