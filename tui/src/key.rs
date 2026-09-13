use ratatui::crossterm::event;

/// Terminal-backend-agnostic key representation used by the keymap and the
/// inline editor, so both stay testable without a real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    /// Alt (Meta) + character chord, e.g. Alt-j. On macOS this requires the
    /// terminal's Option-as-Meta setting.
    Alt(char),
    /// Ctrl + character chord, e.g. Ctrl-d. The character is stored
    /// lowercase because terminals disagree on the case they deliver.
    Ctrl(char),
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

    pub fn from_keys(keys: Vec<Key>) -> Self {
        Self(keys)
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
    if event.modifiers.contains(event::KeyModifiers::CONTROL) {
        // Only Ctrl+character is a distinct chord; anything else is noise.
        // Lowercased because terminals disagree on the delivered case.
        return match event.code {
            event::KeyCode::Char(c) if !event.modifiers.contains(event::KeyModifiers::ALT) => {
                Some(Key::Ctrl(c.to_ascii_lowercase()))
            }
            _ => None,
        };
    }
    if event.modifiers.contains(event::KeyModifiers::ALT) {
        // Only Alt+character is a distinct chord; anything else is noise.
        return match event.code {
            event::KeyCode::Char(c) => Some(Key::Alt(c)),
            _ => None,
        };
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

    // Tests that Ctrl + character chords become their own key kind.
    // Given: a key event for Ctrl+d
    // When: converting it through key_from_event
    // Then: it becomes Key::Ctrl('d'), distinct from the plain character,
    //       so Ctrl bindings can fire without leaking 'd' into text inputs
    #[test]
    fn ctrl_char_chord_becomes_ctrl_key() {
        let ctrl = event::KeyEvent::new(event::KeyCode::Char('d'), event::KeyModifiers::CONTROL);

        assert_eq!(key_from_event(&ctrl), Some(Key::Ctrl('d')));
    }

    // Tests that Ctrl chords normalise their character to lowercase.
    // Given: a key event for Ctrl+Shift+u, which some terminals deliver as
    //        the uppercase char 'U' with CONTROL set
    // When: converting it through key_from_event
    // Then: it becomes Key::Ctrl('u') so a <ctrl-u> binding fires either way
    #[test]
    fn ctrl_chord_char_is_lowercased() {
        let ctrl = event::KeyEvent::new(
            event::KeyCode::Char('U'),
            event::KeyModifiers::CONTROL | event::KeyModifiers::SHIFT,
        );

        assert_eq!(key_from_event(&ctrl), Some(Key::Ctrl('u')));
    }

    // Tests that Ctrl combined with a non-character key stays rejected.
    // Given: a key event for Ctrl+Enter
    // When: converting it through key_from_event
    // Then: it yields None (only Ctrl+character chords are meaningful here)
    #[test]
    fn ctrl_non_char_chord_is_dropped() {
        let ctrl = event::KeyEvent::new(event::KeyCode::Enter, event::KeyModifiers::CONTROL);

        assert_eq!(key_from_event(&ctrl), None);
    }

    // Tests that Alt + character chords become their own key kind.
    // Given: a key event for Alt+j
    // When: converting it through key_from_event
    // Then: it becomes Key::Alt('j'), distinct from the plain character, so
    //       Alt bindings can fire without leaking 'j' into text inputs
    #[test]
    fn alt_char_chord_becomes_alt_key() {
        let alt = event::KeyEvent::new(event::KeyCode::Char('j'), event::KeyModifiers::ALT);

        assert_eq!(key_from_event(&alt), Some(Key::Alt('j')));
    }

    // Tests that Alt combined with a non-character key stays rejected.
    // Given: a key event for Alt+Enter
    // When: converting it through key_from_event
    // Then: it yields None (only Alt+character chords are meaningful here)
    #[test]
    fn alt_non_char_chord_is_dropped() {
        let alt = event::KeyEvent::new(event::KeyCode::Enter, event::KeyModifiers::ALT);

        assert_eq!(key_from_event(&alt), None);
    }

    // Tests that Ctrl+Alt chords are rejected even though Alt alone passes.
    // Given: a key event for Ctrl+Alt+j
    // When: converting it through key_from_event
    // Then: it yields None (Ctrl still disqualifies the chord)
    #[test]
    fn ctrl_alt_chord_is_dropped() {
        let both = event::KeyEvent::new(
            event::KeyCode::Char('j'),
            event::KeyModifiers::CONTROL | event::KeyModifiers::ALT,
        );

        assert_eq!(key_from_event(&both), None);
    }
}
