use unicode_width::UnicodeWidthStr;

use crate::command;
use crate::key;
use crate::keymap;

/// Builds the one-line key hint for `context`: bound commands ordered by
/// descending hint priority, taking as many as fit within `width` display
/// columns. Commands with priority 0 are never shown.
pub fn footer_line(
    commands: &[command::Command],
    keymap: &keymap::Keymap,
    context: command::Context,
    width: usize,
) -> String {
    let mut hinted: Vec<&command::Command> = commands
        .iter()
        .filter(|c| c.context == context && c.hint_priority > 0)
        .collect();
    // Stable sort keeps the command-table order for equal priorities.
    hinted.sort_by_key(|c| std::cmp::Reverse(c.hint_priority));

    let mut line = String::new();
    for cmd in hinted {
        let Some(seq) = keymap.binding_for(context, cmd.id) else {
            continue;
        };
        let entry = format!("{} {}", format_seq(seq), cmd.label);
        let separator = if line.is_empty() { "" } else { "  " };
        // Stop at the first entry that overflows so higher-priority hints
        // are never displaced by lower-priority ones that happen to fit.
        if line.width() + separator.width() + entry.width() > width {
            break;
        }
        line.push_str(separator);
        line.push_str(&entry);
    }
    line
}

fn format_seq(seq: &key::KeySeq) -> String {
    seq.as_slice().iter().map(format_key).collect()
}

fn format_key(key: &key::Key) -> String {
    match key {
        key::Key::Char(c) => c.to_string(),
        key::Key::Enter => "Enter".to_string(),
        key::Key::Esc => "Esc".to_string(),
        key::Key::Backspace => "BS".to_string(),
        key::Key::Tab => "Tab".to_string(),
        key::Key::Left => "←".to_string(),
        key::Key::Right => "→".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{COMMANDS, id};

    // A fixture command table with mixed priorities, including a hidden one.
    fn fixture_commands() -> Vec<command::Command> {
        vec![
            command::Command {
                id: id::SELECT_NEXT,
                label: "下へ",
                context: command::Context::Tree,
                hint_priority: 50,
            },
            command::Command {
                id: id::QUIT,
                label: "終了",
                context: command::Context::Tree,
                hint_priority: 90,
            },
            command::Command {
                id: id::SELECT_FIRST,
                label: "先頭へ",
                context: command::Context::Tree,
                hint_priority: 0,
            },
            command::Command {
                id: id::INPUT_CONFIRM,
                label: "確定",
                context: command::Context::Input,
                hint_priority: 100,
            },
        ]
    }

    // Tests that the footer lists bindings by descending hint priority.
    // Given: Tree commands quit (priority 90) and select-next (priority 50),
    //        defined in the opposite order in the command table
    // When: building the footer with ample width
    // Then: quit appears before select-next, each as "<keys> <label>"
    #[test]
    fn footer_orders_by_priority_descending() {
        let line = footer_line(
            &fixture_commands(),
            &keymap::Keymap::default(),
            command::Context::Tree,
            100,
        );

        assert_eq!(line, "q 終了  j 下へ");
    }

    // Tests that commands with hint priority 0 are hidden from the footer.
    // Given: select-first has priority 0 and is bound to "gg"
    // When: building the Tree footer with ample width
    // Then: the line does not mention it
    #[test]
    fn footer_hides_priority_zero() {
        let line = footer_line(
            &fixture_commands(),
            &keymap::Keymap::default(),
            command::Context::Tree,
            100,
        );

        assert!(!line.contains("先頭へ"), "line was: {line}");
    }

    // Tests that the footer only shows commands of the requested context.
    // Given: a table containing both Tree and Input commands
    // When: building the Input footer
    // Then: only the Input binding appears, with its special key spelled out
    #[test]
    fn footer_is_scoped_to_context() {
        let line = footer_line(
            &fixture_commands(),
            &keymap::Keymap::default(),
            command::Context::Input,
            100,
        );

        assert_eq!(line, "Enter 確定");
    }

    // Tests that entries stop at the first one that would overflow the width.
    // Given: "q 終了" needs 6 display columns (CJK chars are 2 wide) and the
    //        next entry "j 下へ" would need 2 (separator) + 6 more
    // When: building the footer with width 13, one column short
    // Then: only the first entry is shown
    #[test]
    fn footer_truncates_to_width_in_display_columns() {
        let line = footer_line(
            &fixture_commands(),
            &keymap::Keymap::default(),
            command::Context::Tree,
            13,
        );

        assert_eq!(line, "q 終了");
    }

    // Tests that a multi-key binding renders its keys joined together.
    // Given: the real command table where select-first ("gg") has a
    //        non-zero priority
    // When: building the Tree footer with ample width
    // Then: the entry renders as "gg first"
    #[test]
    fn footer_renders_multi_key_sequences() {
        let line = footer_line(
            COMMANDS,
            &keymap::Keymap::default(),
            command::Context::Tree,
            200,
        );

        assert!(line.contains("gg first"), "line was: {line}");
    }
}
