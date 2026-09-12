/// Progress classification of a user-defined status. Features that cannot
/// know user-chosen labels (hiding finished tasks, progress counts) key off
/// this instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Open,
    Done,
    Cancelled,
}

impl StatusKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "open" => Some(Self::Open),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Which neighbour a status swaps display order with when reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMove {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Internal key referenced by tasks; never shown to the user.
    pub id: i64,
    pub label: String,
    pub kind: StatusKind,
    pub color: String,
    /// One-key shortcut in the status-select menu.
    pub key: char,
    pub display_order: i64,
    pub is_default: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests the round trip between StatusKind and its stored text form.
    // Given: each StatusKind variant
    // When: it is rendered to text and parsed back
    // Then: the original variant comes back
    #[test]
    fn kind_round_trips_through_text() {
        for kind in [StatusKind::Open, StatusKind::Done, StatusKind::Cancelled] {
            assert_eq!(StatusKind::parse(kind.as_str()), Some(kind));
        }
    }

    // Tests parsing an unknown kind text.
    // Given: strings that are no valid kind ("in_progress", "", "Open")
    // When: they are parsed
    // Then: parsing yields None instead of guessing a variant
    #[test]
    fn parse_rejects_unknown_kind_text() {
        assert_eq!(StatusKind::parse("in_progress"), None);
        assert_eq!(StatusKind::parse(""), None);
        assert_eq!(StatusKind::parse("Open"), None);
    }
}
