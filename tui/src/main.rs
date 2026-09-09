use std::error::Error;
use std::path::PathBuf;

use engine::Db;
use ratatui::crossterm::event;
use ratatui::widgets;

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

fn run(terminal: &mut ratatui::DefaultTerminal, db: &Db) -> Result<(), Box<dyn Error>> {
    let mut tasks = db.list_children(None)?;
    loop {
        terminal.draw(|frame| {
            let list = widgets::List::new(tasks.iter().map(|task| task.title.clone()));
            frame.render_widget(list, frame.area());
        })?;
        if let event::Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
        {
            // Throwaway hardcoded keys until keymap dispatch lands.
            match key.code {
                event::KeyCode::Char('q') => return Ok(()),
                event::KeyCode::Char('o') => {
                    db.create_task(None, "new task")?;
                    tasks = db.list_children(None)?;
                }
                _ => {}
            }
        }
    }
}
