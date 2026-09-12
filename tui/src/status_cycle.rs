use engine::Status;

/// Which neighbour to pick when cycling a task's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Next,
    Prev,
}

/// Returns the id of the status adjacent to `current_id` in `statuses`
/// (which is already in display order), wrapping around at both ends.
/// Returns None when `current_id` is not present: the view is then stale
/// relative to the database, and writing a status picked from an
/// inconsistent view could change data the user never asked to touch.
pub fn adjacent_status_id(
    statuses: &[Status],
    current_id: i64,
    direction: Direction,
) -> Option<i64> {
    let position = statuses.iter().position(|status| status.id == current_id)?;
    let adjacent = match direction {
        Direction::Next => (position + 1) % statuses.len(),
        Direction::Prev => (position + statuses.len() - 1) % statuses.len(),
    };
    Some(statuses[adjacent].id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::StatusKind;

    fn status(id: i64, display_order: i64) -> Status {
        Status {
            id,
            label: format!("status {id}"),
            kind: StatusKind::Open,
            color: "white".to_string(),
            key: ' ',
            display_order,
            is_default: false,
        }
    }

    fn fixture() -> Vec<Status> {
        vec![status(10, 0), status(20, 1), status(30, 2)]
    }

    // Tests stepping to the next status from a middle position.
    // Given: statuses [10, 20, 30] in display order with current 20
    // When: asking for the next status
    // Then: 30 (the following one) is returned
    #[test]
    fn next_from_middle_returns_following_status() {
        assert_eq!(
            adjacent_status_id(&fixture(), 20, Direction::Next),
            Some(30)
        );
    }

    // Tests forward wrap-around at the end of the list.
    // Given: statuses [10, 20, 30] with current 30 (the last)
    // When: asking for the next status
    // Then: 10 (the first) is returned
    #[test]
    fn next_from_last_wraps_to_first() {
        assert_eq!(
            adjacent_status_id(&fixture(), 30, Direction::Next),
            Some(10)
        );
    }

    // Tests stepping to the previous status from a middle position.
    // Given: statuses [10, 20, 30] with current 20
    // When: asking for the previous status
    // Then: 10 (the preceding one) is returned
    #[test]
    fn prev_from_middle_returns_preceding_status() {
        assert_eq!(
            adjacent_status_id(&fixture(), 20, Direction::Prev),
            Some(10)
        );
    }

    // Tests backward wrap-around at the start of the list.
    // Given: statuses [10, 20, 30] with current 10 (the first)
    // When: asking for the previous status
    // Then: 30 (the last) is returned
    #[test]
    fn prev_from_first_wraps_to_last() {
        assert_eq!(
            adjacent_status_id(&fixture(), 10, Direction::Prev),
            Some(30)
        );
    }

    // Tests the defensive path for a current id missing from the list.
    // Given: statuses [10, 20, 30] and a current id 99 that none of them has
    // When: asking for the next status
    // Then: None is returned so the caller performs no write from a view
    //       that is out of sync with the database
    #[test]
    fn unknown_current_id_yields_none() {
        assert_eq!(adjacent_status_id(&fixture(), 99, Direction::Next), None);
    }

    // Tests cycling within a single-status list.
    // Given: a single status 10
    // When: asking for the next and the previous status
    // Then: both return 10 itself (wrap-around on a one-element ring)
    #[test]
    fn single_status_cycles_to_itself() {
        let statuses = vec![status(10, 0)];

        assert_eq!(adjacent_status_id(&statuses, 10, Direction::Next), Some(10));
        assert_eq!(adjacent_status_id(&statuses, 10, Direction::Prev), Some(10));
    }
}
