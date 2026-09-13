use crate::key::{self, Key};

/// Single-line text editor state. `cursor` is a byte offset into `text`
/// that always lies on a `char` boundary, so multibyte input is safe.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Editor {
    text: String,
    cursor: usize,
}

/// Outcome of feeding one key to the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditResult {
    /// Editing continues with the updated state.
    Continue(Editor),
    /// The user confirmed the input; carries the final text.
    Submitted(String),
    /// The user abandoned the input.
    Cancelled,
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the editor prefilled, e.g. for renaming. The cursor starts at
    /// the end because appending is the most common first edit.
    pub fn with_text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            cursor: text.len(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte offset of the cursor within `text`, always on a char boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Applies one key, consuming the state. Pure aside from the move:
    /// same state + same key always yields the same result.
    pub fn handle_key(mut self, key: Key) -> EditResult {
        match key {
            key::Key::Char(c) => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            key::Key::Backspace => {
                if let Some(prev) = self.prev_boundary() {
                    self.text.remove(prev);
                    self.cursor = prev;
                }
            }
            key::Key::Left => {
                if let Some(prev) = self.prev_boundary() {
                    self.cursor = prev;
                }
            }
            key::Key::Right => {
                if let Some(next) = self.next_boundary() {
                    self.cursor = next;
                }
            }
            key::Key::Enter => return EditResult::Submitted(self.text),
            key::Key::Esc => return EditResult::Cancelled,
            // Tab is a tree-navigation key; a literal tab in a one-line
            // title would only break alignment, so it is ignored here.
            // Alt and Ctrl chords are commands, never text.
            key::Key::Tab | key::Key::Alt(_) | key::Key::Ctrl(_) => {}
        }
        EditResult::Continue(self)
    }

    /// Start of the char preceding the cursor, or None at the beginning.
    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .chars()
            .next_back()
            .map(|c| self.cursor - c.len_utf8())
    }

    /// End of the char at the cursor, or None at the end of the text.
    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(editor: Editor, s: &str) -> Editor {
        s.chars()
            .fold(editor, |ed, c| match ed.handle_key(Key::Char(c)) {
                EditResult::Continue(next) => next,
                other => panic!("typing should continue editing, got {other:?}"),
            })
    }

    // Tests opening the editor prefilled with existing text.
    // Given: a multibyte initial string "設計する"
    // When: an editor is created with that text
    // Then: the text is prefilled and the cursor sits at the end (in bytes),
    //       ready to append
    #[test]
    fn with_text_prefills_and_puts_cursor_at_end() {
        let editor = Editor::with_text("設計する");

        assert_eq!(editor.text(), "設計する");
        assert_eq!(editor.cursor(), "設計する".len());
    }

    // Tests that a prefilled editor is immediately editable.
    // Given: an editor prefilled with "設計"
    // When: Backspace is pressed and then "図" is typed
    // Then: the last char is replaced, yielding "設図"
    #[test]
    fn with_text_allows_editing_from_the_end() {
        let editor = Editor::with_text("設計");

        let EditResult::Continue(editor) = editor.handle_key(Key::Backspace) else {
            panic!("backspace should continue editing");
        };
        let editor = type_str(editor, "図");

        assert_eq!(editor.text(), "設図");
        assert_eq!(editor.cursor(), "設図".len());
    }

    // Tests that typed characters are inserted at the cursor in order.
    // Given: an empty editor
    // When: the characters "abc" are typed
    // Then: the text is "abc" and the cursor sits at the end
    #[test]
    fn typing_inserts_at_cursor() {
        let editor = type_str(Editor::new(), "abc");

        assert_eq!(editor.text(), "abc");
        assert_eq!(editor.cursor(), 3);
    }

    // Tests that multibyte characters are inserted without panicking.
    // Given: an empty editor
    // When: the Japanese string "設計する" is typed
    // Then: the text matches and the cursor is at the end (in bytes)
    #[test]
    fn typing_multibyte_characters_is_safe() {
        let editor = type_str(Editor::new(), "設計する");

        assert_eq!(editor.text(), "設計する");
        assert_eq!(editor.cursor(), "設計する".len());
    }

    // Tests that Backspace removes the character before the cursor.
    // Given: an editor containing "ab" with the cursor at the end
    // When: Backspace is pressed
    // Then: the text is "a" and the cursor moved back by one char
    #[test]
    fn backspace_removes_previous_char() {
        let editor = type_str(Editor::new(), "ab");

        let EditResult::Continue(editor) = editor.handle_key(Key::Backspace) else {
            panic!("backspace should continue editing");
        };

        assert_eq!(editor.text(), "a");
        assert_eq!(editor.cursor(), 1);
    }

    // Tests that Backspace deletes a whole multibyte character.
    // Given: an editor containing "設計" with the cursor at the end
    // When: Backspace is pressed
    // Then: the text is "設" (no partial-byte corruption, no panic)
    #[test]
    fn backspace_removes_whole_multibyte_char() {
        let editor = type_str(Editor::new(), "設計");

        let EditResult::Continue(editor) = editor.handle_key(Key::Backspace) else {
            panic!("backspace should continue editing");
        };

        assert_eq!(editor.text(), "設");
        assert_eq!(editor.cursor(), "設".len());
    }

    // Tests that Backspace on an empty editor is a no-op.
    // Given: an empty editor
    // When: Backspace is pressed
    // Then: editing continues with unchanged (empty) text
    #[test]
    fn backspace_at_start_is_noop() {
        let EditResult::Continue(editor) = Editor::new().handle_key(Key::Backspace) else {
            panic!("backspace should continue editing");
        };

        assert_eq!(editor.text(), "");
        assert_eq!(editor.cursor(), 0);
    }

    // Tests cursor movement over multibyte characters and mid-text insertion.
    // Given: an editor containing "設計" with the cursor at the end
    // When: the cursor moves left once and "再" is typed
    // Then: the text is "設再計" with the cursor right after "再"
    #[test]
    fn left_then_insert_lands_between_multibyte_chars() {
        let editor = type_str(Editor::new(), "設計");

        let EditResult::Continue(editor) = editor.handle_key(Key::Left) else {
            panic!("left should continue editing");
        };
        let editor = type_str(editor, "再");

        assert_eq!(editor.text(), "設再計");
        assert_eq!(editor.cursor(), "設再".len());
    }

    // Tests that Left at the start and Right at the end are no-ops.
    // Given: an editor containing "あ"
    // When: moving left past the start, then right twice past the end
    // Then: the cursor clamps to the text boundaries without panicking
    #[test]
    fn cursor_clamps_at_boundaries() {
        let editor = type_str(Editor::new(), "あ");

        let EditResult::Continue(editor) = editor.handle_key(Key::Left) else {
            panic!("left should continue editing");
        };
        let EditResult::Continue(editor) = editor.handle_key(Key::Left) else {
            panic!("left at start should continue editing");
        };
        assert_eq!(editor.cursor(), 0);

        let EditResult::Continue(editor) = editor.handle_key(Key::Right) else {
            panic!("right should continue editing");
        };
        let EditResult::Continue(editor) = editor.handle_key(Key::Right) else {
            panic!("right at end should continue editing");
        };
        assert_eq!(editor.cursor(), "あ".len());
    }

    // Tests that Enter submits the accumulated text.
    // Given: an editor containing "設計する"
    // When: Enter is pressed
    // Then: the result is Submitted with the full text
    #[test]
    fn enter_submits_text() {
        let editor = type_str(Editor::new(), "設計する");

        let result = editor.handle_key(Key::Enter);

        assert_eq!(result, EditResult::Submitted("設計する".to_string()));
    }

    // Tests that Tab does not modify the input text.
    // Given: an editor containing "ab"
    // When: Tab is pressed
    // Then: editing continues with the text and cursor unchanged
    #[test]
    fn tab_is_noop_in_editor() {
        let editor = type_str(Editor::new(), "ab");

        let EditResult::Continue(editor) = editor.handle_key(Key::Tab) else {
            panic!("tab should continue editing");
        };

        assert_eq!(editor.text(), "ab");
        assert_eq!(editor.cursor(), 2);
    }

    // Tests that Esc cancels the input regardless of content.
    // Given: an editor containing "abc"
    // When: Esc is pressed
    // Then: the result is Cancelled
    #[test]
    fn esc_cancels_input() {
        let editor = type_str(Editor::new(), "abc");

        let result = editor.handle_key(Key::Esc);

        assert_eq!(result, EditResult::Cancelled);
    }
}
