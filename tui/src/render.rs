use engine::{Status, Task};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::color::color_from_name;
use crate::status_manage::Column;

/// Builds one tree row: tree prefix, title, then the status as ` [label]`
/// in the status color. A status id missing from the list renders as the
/// raw id in dark gray, keeping it visible instead of hiding the data.
pub fn task_line<'a>(prefix: String, task: &'a Task, statuses: &'a [Status]) -> Line<'a> {
    let (status_text, status_color) = match statuses.iter().find(|s| s.id == task.status_id) {
        Some(status) => (status.label.clone(), color_from_name(&status.color)),
        None => (task.status_id.to_string(), Color::DarkGray),
    };
    Line::from(vec![
        Span::raw(prefix),
        Span::raw(task.title.as_str()),
        Span::styled(
            format!(" [{status_text}]"),
            Style::default().fg(status_color),
        ),
    ])
}

/// Builds the status-select candidate line straight from the status list,
/// e.g. `t 未着手  r 着手可能  d 進行中`. The statuses are the single source
/// of truth for these keys, so the display can never drift from what the
/// keys actually do.
pub fn status_menu_line(statuses: &[Status]) -> String {
    statuses
        .iter()
        .map(|status| format!("{} {}", status.key, status.label))
        .collect::<Vec<_>>()
        .join("  ")
}

/// Builds the status-management table: a dim header plus one line per
/// status. The label cell is drawn in the status's own color, which doubles
/// as the color preview; the selected cell is drawn reversed.
pub fn manage_table_lines(
    statuses: &[Status],
    selected_row: usize,
    selected_col: Column,
) -> Vec<Line<'static>> {
    const GAP: &str = "  ";
    let label_width = column_width("Label", statuses.iter().map(|s| s.label.as_str()));
    let kind_width = column_width("Kind", statuses.iter().map(|s| s.kind.as_str()));
    let color_width = column_width("Color", statuses.iter().map(|s| s.color.as_str()));
    let key_width = "Key".width();

    let header = Line::from(Span::styled(
        [
            pad("Label", label_width),
            pad("Kind", kind_width),
            pad("Color", color_width),
            pad("Key", key_width),
            "Default".to_string(),
        ]
        .join(GAP),
        Style::default().add_modifier(Modifier::DIM),
    ));

    let mut lines = vec![header];
    for (row, status) in statuses.iter().enumerate() {
        let cell_style = |col: Column, base: Style| {
            if row == selected_row && col == selected_col {
                base.add_modifier(Modifier::REVERSED)
            } else {
                base
            }
        };
        lines.push(Line::from(vec![
            Span::styled(
                pad(&status.label, label_width),
                cell_style(
                    Column::Label,
                    Style::default().fg(color_from_name(&status.color)),
                ),
            ),
            Span::raw(GAP),
            Span::styled(
                pad(status.kind.as_str(), kind_width),
                cell_style(Column::Kind, Style::default()),
            ),
            Span::raw(GAP),
            Span::styled(
                pad(&status.color, color_width),
                cell_style(Column::Color, Style::default()),
            ),
            Span::raw(GAP),
            Span::styled(
                pad(&status.key.to_string(), key_width),
                cell_style(Column::Key, Style::default()),
            ),
            Span::raw(GAP),
            Span::raw(if status.is_default { "*" } else { "" }),
        ]));
    }
    lines
}

fn column_width<'a>(header: &str, cells: impl Iterator<Item = &'a str>) -> usize {
    cells
        .map(|cell| cell.width())
        .chain(std::iter::once(header.width()))
        .max()
        .unwrap_or(0)
}

/// Pads with trailing spaces to `width` display columns, so multibyte
/// labels line up with the ASCII cells around them.
fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Db;
    use ratatui::style::{Color, Style};

    fn seeded_statuses() -> Vec<Status> {
        Db::open_in_memory().unwrap().list_statuses().unwrap()
    }

    fn task_with_status(status_id: i64) -> Task {
        Task {
            id: 1,
            parent_id: None,
            display_order: 0,
            title: "design".to_string(),
            status_id,
            due: None,
            log: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    // Tests rendering a row whose status is in the status list.
    // Given: a task carrying the id of the seeded status labelled "進行中"
    //        (color yellow)
    // When: the row line is built with a tree prefix
    // Then: the line spans are prefix, title, and " [進行中]" where only the
    //       status span carries the yellow foreground
    #[test]
    fn task_line_shows_status_label_in_its_color() {
        let statuses = seeded_statuses();
        let doing = statuses.iter().find(|s| s.label == "進行中").unwrap();
        let task = task_with_status(doing.id);

        let line = task_line("▸ ".to_string(), &task, &statuses);

        let contents: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(contents, ["▸ ", "design", " [進行中]"]);
        assert_eq!(line.spans[0].style, Style::default());
        assert_eq!(line.spans[1].style, Style::default());
        assert_eq!(line.spans[2].style.fg, Some(Color::Yellow));
    }

    // Tests the status-select candidate line.
    // Given: the five seeded statuses
    // When: the menu line is built
    // Then: every status appears as "<key> <label>" in display order,
    //       separated by two spaces
    #[test]
    fn status_menu_line_lists_key_and_label_per_status() {
        let statuses = seeded_statuses();

        let line = status_menu_line(&statuses);

        assert_eq!(line, "t 未着手  r 着手可能  d 進行中  x 完了  c 破棄");
    }

    // Tests rendering a row whose status id is not in the status list.
    // Given: a task with status id 999, which no status defines (possible
    //        only through outside edits of the database file)
    // When: the row line is built
    // Then: the raw id appears as " [999]" in dark gray, so broken data
    //       stays visible instead of silently disappearing
    #[test]
    fn task_line_shows_unknown_status_id_in_dark_gray() {
        let statuses = seeded_statuses();
        let task = task_with_status(999);

        let line = task_line(String::new(), &task, &statuses);

        let contents: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(contents, ["", "design", " [999]"]);
        assert_eq!(line.spans[2].style.fg, Some(Color::DarkGray));
    }

    // Tests the layout and styling of the status-management table.
    // Given: the five seeded statuses with the cursor on the label cell of
    //        the first row
    // When: the table lines are built
    // Then: a dim header comes first; each status renders as one line whose
    //       label cell carries the status color (the color preview), only
    //       the selected cell is reversed, and only the default row shows
    //       the "*" mark
    #[test]
    fn manage_table_marks_selection_color_and_default() {
        let statuses = seeded_statuses();

        let lines = manage_table_lines(&statuses, 0, Column::Label);

        assert_eq!(lines.len(), 6, "header plus one line per status");
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.add_modifier.contains(Modifier::DIM)),
            "header must be dim"
        );

        // Row 0 (未着手, gray, default, selected on Label).
        let first = &lines[1];
        assert!(first.spans[0].content.starts_with("未着手"));
        assert_eq!(first.spans[0].style.fg, Some(Color::Gray));
        assert!(
            first.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !first.spans[2]
                .style
                .add_modifier
                .contains(Modifier::REVERSED),
            "only the selected cell is reversed"
        );
        assert_eq!(
            first.spans.last().unwrap().content,
            "*",
            "the default row carries the mark"
        );

        // Row 2 (進行中, yellow, not default, not selected).
        let third = &lines[3];
        assert_eq!(third.spans[0].style.fg, Some(Color::Yellow));
        assert!(
            !third.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert_eq!(third.spans.last().unwrap().content, "");
    }

    // Tests that the selected-cell highlight follows the column cursor.
    // Given: the seeded statuses with the cursor on the key cell of row 1
    // When: the table lines are built
    // Then: the key cell of row 1 is reversed and shows that status's key,
    //       while its label cell is not reversed
    #[test]
    fn manage_table_highlights_the_selected_key_cell() {
        let statuses = seeded_statuses();

        let lines = manage_table_lines(&statuses, 1, Column::Key);

        // Spans per row: label, gap, kind, gap, color, gap, key, gap, mark.
        let row = &lines[2];
        let key_cell = &row.spans[6];
        assert!(key_cell.content.starts_with(statuses[1].key));
        assert!(key_cell.style.add_modifier.contains(Modifier::REVERSED));
        assert!(!row.spans[0].style.add_modifier.contains(Modifier::REVERSED));
    }
}
