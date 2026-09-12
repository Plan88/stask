use engine::{Status, StatusKind};

use crate::input;

/// Editable columns of the status-management table, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Column {
    #[default]
    Label,
    Kind,
    Color,
    Key,
}

impl Column {
    pub fn left(self) -> Self {
        match self {
            Self::Label | Self::Kind => Self::Label,
            Self::Color => Self::Kind,
            Self::Key => Self::Color,
        }
    }

    pub fn right(self) -> Self {
        match self {
            Self::Label => Self::Kind,
            Self::Kind => Self::Color,
            Self::Color | Self::Key => Self::Key,
        }
    }
}

/// What the modal is currently capturing, if anything. While this is not
/// `None`, keys bypass the keymap and feed the capture instead.
#[derive(Debug, Default)]
pub enum Editing {
    #[default]
    None,
    /// Inline text edit of the selected label/color cell.
    Cell(input::Editor),
    /// The next plain key press becomes the status key.
    KeyCapture,
    /// Entering the label for a status about to be created.
    NewStatus(input::Editor),
}

/// Cursor state of the status-management modal.
#[derive(Debug, Default)]
pub struct ManageState {
    pub row: usize,
    pub col: Column,
    pub editing: Editing,
}

impl ManageState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_down(&mut self, len: usize) {
        self.row = (self.row + 1).min(len.saturating_sub(1));
    }

    pub fn move_up(&mut self) {
        self.row = self.row.saturating_sub(1);
    }

    /// Keeps the cursor on an existing row after the list shrinks.
    pub fn clamp_row(&mut self, len: usize) {
        self.row = self.row.min(len.saturating_sub(1));
    }
}

/// Advances the kind one step through its fixed cycle, so a single key can
/// reach every variant.
pub fn toggle_kind(kind: StatusKind) -> StatusKind {
    match kind {
        StatusKind::Open => StatusKind::Done,
        StatusKind::Done => StatusKind::Cancelled,
        StatusKind::Cancelled => StatusKind::Open,
    }
}

/// First key in a-z not used by any status; None when all are taken. New
/// statuses get this as their auto-assigned menu key.
pub fn free_key(statuses: &[Status]) -> Option<char> {
    ('a'..='z').find(|c| statuses.iter().all(|s| s.key != *c))
}

/// Whether `key` is already the menu key of a status other than
/// `exclude_id` (a status may keep its own key).
pub fn key_taken(statuses: &[Status], key: char, exclude_id: i64) -> bool {
    statuses.iter().any(|s| s.key == key && s.id != exclude_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_with_key(id: i64, key: char) -> Status {
        Status {
            id,
            label: format!("status {id}"),
            kind: StatusKind::Open,
            color: "gray".to_string(),
            key,
            display_order: id,
            is_default: false,
        }
    }

    // Tests horizontal cell movement across the four editable columns.
    // Given: the leftmost column (Label)
    // When: moving right three times and once more past the end, then back
    // Then: the order is Label→Kind→Color→Key, clamped at Key, and left
    //       moves retrace the same path clamped at Label
    #[test]
    fn column_moves_clamp_at_both_ends() {
        let mut col = Column::Label;

        col = col.right();
        assert_eq!(col, Column::Kind);
        col = col.right();
        assert_eq!(col, Column::Color);
        col = col.right();
        assert_eq!(col, Column::Key);
        col = col.right();
        assert_eq!(col, Column::Key, "right clamps at the last column");

        col = col.left();
        assert_eq!(col, Column::Color);
        col = Column::Label.left();
        assert_eq!(col, Column::Label, "left clamps at the first column");
    }

    // Tests vertical cursor movement over a 3-row table.
    // Given: a fresh state (row 0)
    // When: moving down four times and then up three times
    // Then: the row clamps at 2 on the way down and at 0 on the way up
    #[test]
    fn row_moves_clamp_at_both_ends() {
        let mut state = ManageState::new();

        for _ in 0..4 {
            state.move_down(3);
        }
        assert_eq!(state.row, 2);

        for _ in 0..3 {
            state.move_up();
        }
        assert_eq!(state.row, 0);
    }

    // Tests that the row cursor survives the list shrinking.
    // Given: a cursor on row 4
    // When: the list shrinks to 3 rows and the cursor is clamped
    // Then: the cursor lands on the new last row (2)
    #[test]
    fn clamp_row_moves_cursor_onto_shrunk_list() {
        let mut state = ManageState::new();
        state.row = 4;

        state.clamp_row(3);

        assert_eq!(state.row, 2);
    }

    // Tests the kind toggle cycle.
    // Given: each StatusKind variant
    // When: toggling three times starting from Open
    // Then: the cycle is Open→Done→Cancelled→Open
    #[test]
    fn toggle_kind_cycles_through_all_variants() {
        assert_eq!(toggle_kind(StatusKind::Open), StatusKind::Done);
        assert_eq!(toggle_kind(StatusKind::Done), StatusKind::Cancelled);
        assert_eq!(toggle_kind(StatusKind::Cancelled), StatusKind::Open);
    }

    // Tests the auto-assignment key search.
    // Given: statuses using keys 'a' and 'c'
    // When: asking for a free key
    // Then: 'b' (the first unused letter) is returned
    #[test]
    fn free_key_returns_first_unused_letter() {
        let statuses = vec![status_with_key(1, 'a'), status_with_key(2, 'c')];

        assert_eq!(free_key(&statuses), Some('b'));
    }

    // Tests the exhausted-keys case.
    // Given: 26 statuses using every letter a-z
    // When: asking for a free key
    // Then: None is returned so the caller can refuse to add a status
    #[test]
    fn free_key_is_none_when_all_letters_are_taken() {
        let statuses: Vec<Status> = ('a'..='z')
            .enumerate()
            .map(|(i, c)| status_with_key(i as i64, c))
            .collect();

        assert_eq!(free_key(&statuses), None);
    }

    // Tests duplicate detection for key edits.
    // Given: statuses 1 ('a') and 2 ('b'), editing status 1's key
    // When: checking 'b', 'a' (its own key) and 'z' (unused)
    // Then: only 'b' counts as taken; keeping one's own key is allowed
    #[test]
    fn key_taken_ignores_own_key_and_unused_letters() {
        let statuses = vec![status_with_key(1, 'a'), status_with_key(2, 'b')];

        assert!(key_taken(&statuses, 'b', 1));
        assert!(!key_taken(&statuses, 'a', 1));
        assert!(!key_taken(&statuses, 'z', 1));
    }
}
