//! Debug utility: prints raw crossterm key events so terminal/modifier
//! issues (e.g. Option-as-Alt on macOS) can be diagnosed. Quit with plain q.

use ratatui::crossterm::{event, terminal};

fn main() -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    print!("press keys to inspect events; plain q quits\r\n");
    loop {
        let ev = event::read()?;
        print!("{ev:?}\r\n");
        if let event::Event::Key(key) = ev
            && key.code == event::KeyCode::Char('q')
            && key.modifiers.is_empty()
        {
            break;
        }
    }
    terminal::disable_raw_mode()
}
