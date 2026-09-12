//! Hands a note over to the user's `$EDITOR` for full editing. The whole
//! terminal handover (leave the TUI, run the editor, take the terminal
//! back) is confined to this module so no other code path can leave the
//! terminal in a broken state.

use std::io::{self, Write};

/// What the editor session produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    /// The editor exited successfully with modified text.
    Changed(String),
    /// The editor exited successfully but the text is byte-identical to the
    /// original. Callers skip the database write, so no empty undo step is
    /// recorded.
    Unchanged,
    /// The editor exited non-zero (e.g. `:cq` in Helix); the edit is
    /// discarded.
    Aborted,
}

/// Splits an `$EDITOR`-style value into command and arguments on
/// whitespace. An unset or blank value falls back to `hx`.
pub fn parse_editor(value: Option<&str>) -> (String, Vec<String>) {
    let value = match value.map(str::trim) {
        Some(v) if !v.is_empty() => v,
        _ => "hx",
    };
    let mut parts = value.split_whitespace().map(str::to_string);
    // At least one part exists: blank values were replaced above.
    let command = parts.next().unwrap_or_else(|| "hx".to_string());
    (command, parts.collect())
}

/// Decides the outcome of a successful editor exit from the text read back.
pub fn outcome(original: &str, edited: String) -> EditOutcome {
    if edited == original {
        EditOutcome::Unchanged
    } else {
        EditOutcome::Changed(edited)
    }
}

/// Suspends the TUI, runs the editor on `text`, and restores the terminal
/// whatever happens. The old terminal handle is replaced because the editor
/// owned the real terminal in between.
pub fn edit_in_editor(
    terminal: &mut ratatui::DefaultTerminal,
    text: &str,
) -> io::Result<EditOutcome> {
    ratatui::restore();
    let result = run_editor(text);
    *terminal = ratatui::init();
    result
}

/// Writes `text` to a temp file, runs the editor on it, and reads it back.
/// The `.md` suffix lets editors apply their Markdown language support.
fn run_editor(text: &str) -> io::Result<EditOutcome> {
    let mut file = tempfile::Builder::new()
        .prefix("dandori-note-")
        .suffix(".md")
        .tempfile()?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    let (command, args) = parse_editor(std::env::var("EDITOR").ok().as_deref());
    let status = std::process::Command::new(command)
        .args(args)
        .arg(file.path())
        .status()?;
    if !status.success() {
        return Ok(EditOutcome::Aborted);
    }
    let edited = std::fs::read_to_string(file.path())?;
    Ok(outcome(text, edited))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests splitting an $EDITOR value with arguments.
    // Given: EDITOR set to "code --wait"
    // When: parse_editor runs
    // Then: the command is "code" and the arguments are ["--wait"]
    #[test]
    fn parse_editor_splits_command_and_arguments() {
        let (command, args) = parse_editor(Some("code --wait"));

        assert_eq!(command, "code");
        assert_eq!(args, ["--wait"]);
    }

    // Tests the fallback when $EDITOR is unset or blank.
    // Given: no EDITOR value, and a whitespace-only one
    // When: parse_editor runs on each
    // Then: both fall back to plain "hx" with no arguments
    #[test]
    fn parse_editor_falls_back_to_hx() {
        for value in [None, Some("   ")] {
            let (command, args) = parse_editor(value);

            assert_eq!(command, "hx");
            assert!(args.is_empty());
        }
    }

    // Tests a single-word $EDITOR value.
    // Given: EDITOR set to "vim"
    // When: parse_editor runs
    // Then: the command is "vim" with no arguments
    #[test]
    fn parse_editor_handles_single_word() {
        let (command, args) = parse_editor(Some("vim"));

        assert_eq!(command, "vim");
        assert!(args.is_empty());
    }

    // Tests the unchanged-content decision.
    // Given: edited text byte-identical to the original
    // When: outcome runs
    // Then: the result is Unchanged, so the caller writes nothing
    #[test]
    fn identical_text_is_unchanged() {
        let result = outcome("# note\n", "# note\n".to_string());

        assert_eq!(result, EditOutcome::Unchanged);
    }

    // Tests the changed-content decision.
    // Given: edited text differing from the original
    // When: outcome runs
    // Then: the result carries the edited text
    #[test]
    fn modified_text_is_changed() {
        let result = outcome("# note\n", "# note\nmore\n".to_string());

        assert_eq!(result, EditOutcome::Changed("# note\nmore\n".to_string()));
    }
}
