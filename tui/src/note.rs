//! Formatting helpers for the note tail shown in the detail pane.

/// Builds the detail-pane body for a note: the whole note when it fits in
/// `max` lines, otherwise the head, an ellipsis line, and the tail. Both
/// ends carry the valuable parts of a note — background and purpose live at
/// the top, the latest updates at the bottom — so the middle is what gets
/// cut. A blank note yields no lines, meaning no pane.
pub fn pane_lines(note: &str, max: usize) -> Vec<String> {
    if note.trim().is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = note.lines().collect();
    if lines.len() <= max {
        return lines.iter().map(|line| line.to_string()).collect();
    }
    let head = max.div_ceil(2);
    let tail = max - head;
    let mut result = Vec::with_capacity(max + 1);
    result.extend(lines[..head].iter().map(|line| line.to_string()));
    result.push("…".to_string());
    result.extend(
        lines[lines.len() - tail..]
            .iter()
            .map(|line| line.to_string()),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that an empty note produces no pane content.
    // Given: an empty note and a whitespace-only note
    // When: pane_lines is called on each
    // Then: both yield no lines, so the pane is not shown at all
    #[test]
    fn pane_lines_of_blank_note_are_empty() {
        assert!(pane_lines("", 6).is_empty());
        assert!(pane_lines("\n \n", 6).is_empty());
    }

    // Tests that a short note is shown in full.
    // Given: a note of 3 lines and a limit of 6
    // When: pane_lines is called
    // Then: all 3 lines come back unmodified, with no ellipsis
    #[test]
    fn pane_lines_show_short_note_in_full() {
        let note = "\n## 2026-09-12 14:30\n調査した\n";

        let lines = pane_lines(note, 6);

        assert_eq!(lines, ["", "## 2026-09-12 14:30", "調査した"]);
    }

    // Tests truncation of a long note.
    // Given: a note of 9 lines "l1".."l9" and a limit of 6
    // When: pane_lines is called
    // Then: the first 3 lines (background/purpose live at the top) and the
    //       last 3 lines (latest updates) are shown, with an ellipsis line
    //       marking the hidden middle
    #[test]
    fn pane_lines_keep_head_and_tail_and_mark_hidden_middle() {
        let note = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";

        let lines = pane_lines(note, 6);

        assert_eq!(lines, ["l1", "l2", "l3", "…", "l7", "l8", "l9"]);
    }

    // Tests that an odd limit gives the extra line to the head.
    // Given: a note of 9 lines and a limit of 5
    // When: pane_lines is called
    // Then: 3 head lines and 2 tail lines are shown around the ellipsis
    #[test]
    fn pane_lines_give_the_extra_line_to_the_head_on_odd_limits() {
        let note = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";

        let lines = pane_lines(note, 5);

        assert_eq!(lines, ["l1", "l2", "l3", "…", "l8", "l9"]);
    }

    // Tests the boundary where the note exactly fills the limit.
    // Given: a note of exactly 6 lines and a limit of 6
    // When: pane_lines is called
    // Then: all 6 lines come back with no ellipsis
    #[test]
    fn pane_lines_at_exact_limit_have_no_ellipsis() {
        let note = "l1\nl2\nl3\nl4\nl5\nl6\n";

        let lines = pane_lines(note, 6);

        assert_eq!(lines, ["l1", "l2", "l3", "l4", "l5", "l6"]);
    }
}
