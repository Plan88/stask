mod command;
mod footer;
mod input;
mod key;
mod keymap;

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

enum Mode {
    Tree,
    Input(input::Editor),
}

struct App {
    tasks: Vec<Task>,
    selected: usize,
    mode: Mode,
    keymap: keymap::Keymap,
    dispatcher: keymap::Dispatcher,
    should_quit: bool,
}

impl App {
    fn new(tasks: Vec<Task>) -> Self {
        Self {
            tasks,
            selected: 0,
            mode: Mode::Tree,
            keymap: keymap::Keymap::default(),
            dispatcher: keymap::Dispatcher::default(),
            should_quit: false,
        }
    }

    fn context(&self) -> command::Context {
        match self.mode {
            Mode::Tree => command::Context::Tree,
            Mode::Input(_) => command::Context::Input,
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
            Mode::Input(_) => {
                // The editor consumes the state, so take it out of the mode
                // first; every branch below decides the next mode explicitly.
                let Mode::Input(editor) = std::mem::replace(&mut self.mode, Mode::Tree) else {
                    unreachable!("mode was just matched as Input");
                };
                match editor.handle_key(key) {
                    input::EditResult::Continue(editor) => self.mode = Mode::Input(editor),
                    input::EditResult::Submitted(title) => {
                        let title = title.trim();
                        // An empty title is treated as a cancel; creating a
                        // blank task would only produce noise to clean up.
                        if !title.is_empty() {
                            // Insert right below the cursor;
                            // an empty list has no cursor, so the new task
                            // simply becomes the first row.
                            let after = self.tasks.get(self.selected).map(|t| t.display_order);
                            db.create_task(None, title, after)?;
                            self.tasks = db.list_children(None)?;
                            self.selected = if after.is_some() {
                                self.selected + 1
                            } else {
                                0
                            };
                        }
                    }
                    input::EditResult::Cancelled => {}
                }
            }
        }
        Ok(())
    }

    fn run_command(&mut self, command: command::CommandId) {
        match command {
            id::QUIT => self.should_quit = true,
            id::SELECT_NEXT => {
                if self.selected + 1 < self.tasks.len() {
                    self.selected += 1;
                }
            }
            id::SELECT_PREV => self.selected = self.selected.saturating_sub(1),
            id::SELECT_FIRST => self.selected = 0,
            id::SELECT_LAST => self.selected = self.tasks.len().saturating_sub(1),
            id::CREATE_TASK => self.mode = Mode::Input(input::Editor::new()),
            _ => {}
        }
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, db: &Db) -> Result<(), Box<dyn Error>> {
    let mut app = App::new(db.list_children(None)?);
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
    let input_height = if matches!(app.mode, Mode::Input(_)) {
        1
    } else {
        0
    };
    let [list_area, input_area, footer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let list = List::new(app.tasks.iter().map(|task| task.title.clone()))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default();
    if !app.tasks.is_empty() {
        list_state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    if let Mode::Input(editor) = &app.mode {
        frame.render_widget(Paragraph::new(input_line(editor)), input_area);
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
fn input_line(editor: &input::Editor) -> Line<'_> {
    let (before, after) = editor.text().split_at(editor.cursor());
    let cursor_len = after.chars().next().map_or(0, char::len_utf8);
    let (at_cursor, rest) = after.split_at(cursor_len);
    let cursor_display = if at_cursor.is_empty() { " " } else { at_cursor };
    Line::from(vec![
        Span::raw("New task: "),
        Span::raw(before),
        Span::styled(
            cursor_display,
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        Span::raw(rest),
    ])
}
