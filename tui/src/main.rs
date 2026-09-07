use ratatui::crossterm::event;
use ratatui::widgets;

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| {
            frame.render_widget(widgets::Paragraph::new("dandori"), frame.area());
        })?;
        if let event::Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
            && key.code == event::KeyCode::Char('q')
        {
            return Ok(());
        }
    }
}
