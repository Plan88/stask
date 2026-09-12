use ratatui::crossterm::event;

/// Terminal-backend-agnostic key representation used by the keymap and the
/// inline editor, so both stay testable without a real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
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
        event::KeyCode::Tab => Some(Key::Tab),
        event::KeyCode::Left => Some(Key::Left),
        event::KeyCode::Right => Some(Key::Right),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that shifted character keys survive the event conversion.
    // Given: a crossterm key event for Shift+j, which the backend delivers
    //        as the uppercase char 'J' with the SHIFT modifier set
    // When: converting it through key_from_event
    // Then: it becomes Key::Char('J') so uppercase bindings can fire
    #[test]
    fn shifted_char_passes_through_as_uppercase() {
        let event = event::KeyEvent::new(event::KeyCode::Char('J'), event::KeyModifiers::SHIFT);

        assert_eq!(key_from_event(&event), Some(Key::Char('J')));
    }

    // Tests that Ctrl/Alt chords are rejected as non-text input.
    // Given: key events for Ctrl+j and Alt+j
    // When: converting them through key_from_event
    // Then: both yield None so the bare character never leaks into inputs
    #[test]
    fn ctrl_and_alt_chords_are_dropped() {
        let ctrl = event::KeyEvent::new(event::KeyCode::Char('j'), event::KeyModifiers::CONTROL);
        let alt = event::KeyEvent::new(event::KeyCode::Char('j'), event::KeyModifiers::ALT);

        assert_eq!(key_from_event(&ctrl), None);
        assert_eq!(key_from_event(&alt), None);
    }
}
