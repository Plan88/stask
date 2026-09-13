/// Which tasks a search or the tree view lets through. The default shows
/// everything; hiding finished work is one keystroke away (`f`, then `o`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Only tasks whose status kind is open.
    Open,
    #[default]
    All,
    /// Only tasks carrying exactly this status.
    Status(i64),
    /// Only open tasks whose due date has passed.
    Overdue,
}

/// Result order of a search. Everything except TreeOrder produces a flat,
/// attribute-ordered list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// The tree's own order: parents before children, siblings by their
    /// manual display order.
    #[default]
    TreeOrder,
    /// Earliest due date first; tasks without one come last.
    Due,
    /// Most recently updated first.
    Updated,
    /// Most recently created first.
    Created,
    Title,
}

/// One search/filter/sort request, kept independent of any UI state so
/// conditions can later be named and saved, and so the LIKE scan behind it
/// can be swapped out without touching callers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// Substring to look for in titles and notes; None matches everything.
    pub text: Option<String>,
    pub filter: Filter,
    pub sort: Sort,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests the default filter.
    // Given: no explicit filter choice
    // When: the default is taken
    // Then: it is All — the views start out showing every task
    #[test]
    fn default_filter_shows_everything() {
        assert_eq!(Filter::default(), Filter::All);
    }
}
