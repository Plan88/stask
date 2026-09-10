use ratatui::crossterm::event;

/// Terminal-backend-agnostic key representation used by the keymap and the
/// inline editor, so both stay testable without a real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Left,
    Right,
}

/// A key sequence such as `gg`; bindings may span multiple keystrokes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeySeq(Vec<Key>);

impl KeySeq {
    /// Builds a sequence of character keys, one per char of `s`.
    pub fn chars(s: &str) -> Self {
        Self(s.chars().map(Key::Char).collect())
    }

    pub fn as_slice(&self) -> &[Key] {
        &self.0
    }

    pub fn starts_with(&self, prefix: &[Key]) -> bool {
        self.0.starts_with(prefix)
    }
}

impl From<Key> for KeySeq {
    fn from(key: Key) -> Self {
        Self(vec![key])
    }
}

/// Sole conversion boundary from the terminal backend's key events.
pub fn key_from_event(event: &event::KeyEvent) -> Option<Key> {
    // Ctrl/Alt chords are not plain text; letting them through would insert
    // the bare character into text inputs.
    if event
        .modifiers
        .intersects(event::KeyModifiers::CONTROL | event::KeyModifiers::ALT)
    {
        return None;
    }
    match event.code {
        event::KeyCode::Char(c) => Some(Key::Char(c)),
        event::KeyCode::Enter => Some(Key::Enter),
        event::KeyCode::Esc => Some(Key::Esc),
        event::KeyCode::Backspace => Some(Key::Backspace),
        event::KeyCode::Left => Some(Key::Left),
        event::KeyCode::Right => Some(Key::Right),
        _ => None,
    }
}
