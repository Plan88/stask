//! Pure builders and view state for the flat search-result view.

use engine::{Filter, Query, Sort, Status, Task};
use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::input;

/// Display columns a note snippet may occupy in a result line.
pub const SNIPPET_WIDTH: usize = 60;

/// One-key choices of the sort menu, in the order the menu lists them.
/// Single source for both the menu display and the key resolution, so they
/// can never drift apart.
pub const SORT_KEYS: [(char, Sort); 5] = [
    ('d', Sort::Due),
    ('u', Sort::Updated),
    ('c', Sort::Created),
    ('t', Sort::Title),
    ('-', Sort::TreeOrder),
];

/// Resolves a sort-menu key press. None for keys the menu does not offer.
pub fn sort_from_key(key: char) -> Option<Sort> {
    SORT_KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, sort)| *sort)
}

/// Resolves a filter-menu key press: the fixed choices first, then one key
/// per status (from the status definitions, like the status-select menu).
/// The fixed keys win over a status that happens to use the same letter,
/// matching the order the menu displays them in. None for unbound keys.
pub fn filter_from_key(statuses: &[Status], key: char) -> Option<Filter> {
    match key {
        'a' => Some(Filter::All),
        'o' => Some(Filter::Open),
        '!' => Some(Filter::Overdue),
        _ => statuses
            .iter()
            .find(|status| status.key == key)
            .map(|status| Filter::Status(status.id)),
    }
}

/// Which part of the query view currently receives keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The search text is being typed; every keystroke re-runs the search.
    Edit,
    /// Moving through the results.
    Browse,
    /// The one-key sort menu is open.
    SortMenu,
    /// The one-key filter menu is open (over the query view).
    FilterMenu,
}

/// View state of the query view. The filter is deliberately not here: it is
/// shared with the tree view and lives in the app.
pub struct QueryState {
    /// Holds the search text at all times; only focused while editing.
    pub editor: input::Editor,
    pub focus: Focus,
    pub sort: Sort,
    pub results: Vec<Task>,
    /// Index into `results` (per item, not per display line).
    pub selected: usize,
}

impl QueryState {
    pub fn new() -> Self {
        Self {
            editor: input::Editor::new(),
            focus: Focus::Edit,
            sort: Sort::default(),
            results: Vec::new(),
            selected: 0,
        }
    }

    /// The search request this view currently describes. An empty text means
    /// "match everything": the view then shows all tasks flat, which is how
    /// a filter or sort is applied without searching.
    pub fn query(&self, filter: Filter) -> Query {
        let text = self.editor.text();
        Query {
            text: (!text.is_empty()).then(|| text.to_string()),
            filter,
            sort: self.sort,
        }
    }

    /// Keeps the selection on an existing result after the list changed
    /// (typing, sort or filter changes can shrink it).
    pub fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }
}

impl Default for QueryState {
    fn default() -> Self {
        Self::new()
    }
}

/// Display name of a filter, e.g. for the query header. A status filter
/// shows the status label; an undefined status id (possible only through
/// outside edits of the database file) shows the raw id.
pub fn filter_name(filter: Filter, statuses: &[Status]) -> String {
    match filter {
        Filter::Open => "open".to_string(),
        Filter::All => "all".to_string(),
        Filter::Overdue => "overdue".to_string(),
        Filter::Status(status_id) => statuses
            .iter()
            .find(|s| s.id == status_id)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| status_id.to_string()),
    }
}

/// Display name of a sort order, e.g. for the query header and sort menu.
pub fn sort_name(sort: Sort) -> &'static str {
    match sort {
        Sort::TreeOrder => "tree order",
        Sort::Due => "due",
        Sort::Updated => "updated",
        Sort::Created => "created",
        Sort::Title => "title",
    }
}

/// Builds the one-line query header, e.g.
/// `search: 設計 | sort: due | filter: open`.
pub fn header(text: &str, sort: Sort, filter: Filter, statuses: &[Status]) -> String {
    format!(
        "search: {text} | sort: {} | filter: {}",
        sort_name(sort),
        filter_name(filter, statuses)
    )
}

/// Builds the one or two display lines of a search result: the task line
/// (title, status, due) followed by its ancestor path dimmed, and — only
/// when the note contains the search text — a dimmed snippet line with the
/// hit left undimmed so it stands out.
pub fn result_item(
    task: &Task,
    statuses: &[Status],
    all_tasks: &[Task],
    search_text: &str,
    today: &str,
) -> Vec<Line<'static>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;

    // The tree row already renders title/status/due correctly; reusing it
    // keeps the two views from drifting apart. Its spans borrow from the
    // task, so they are copied into owned spans here.
    let tree_line = crate::render::task_line(String::new(), task, statuses, today);
    let mut spans: Vec<Span<'static>> = tree_line
        .spans
        .into_iter()
        .map(|span| Span::styled(span.content.into_owned(), span.style))
        .collect();
    if let Some(parent_id) = task.parent_id {
        spans.push(Span::styled(
            format!("  {}", crate::tree::breadcrumb(all_tasks, parent_id)),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    let mut lines = vec![Line::from(spans)];
    if let Some(snippet) = note_snippet(&task.note, search_text, SNIPPET_WIDTH) {
        let dim = Style::default().add_modifier(Modifier::DIM);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(snippet.before, dim),
            // Left undimmed so the hit stands out against its context.
            Span::raw(snippet.matched),
            Span::styled(snippet.after, dim),
        ]));
    }
    lines
}

/// The part of a note line surrounding a search hit, split so the hit can be
/// highlighted. Sides that were cut to fit carry a leading/trailing `…`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub before: String,
    pub matched: String,
    pub after: String,
}

/// Extracts a snippet for `needle` from the first note line containing it,
/// fitting the whole snippet into `max_width` display columns. Matching is
/// ASCII case-insensitive, mirroring how SQLite's LIKE found the hit. None
/// when the note has no hit (e.g. the title matched instead).
pub fn note_snippet(note: &str, needle: &str, max_width: usize) -> Option<Snippet> {
    if needle.is_empty() {
        return None;
    }
    // ASCII lowercasing keeps every byte offset valid in the original line,
    // which full Unicode lowercasing would not (it can change lengths).
    let needle_lower = needle.to_ascii_lowercase();
    let (line, start) = note.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .find(&needle_lower)
            .map(|start| (line, start))
    })?;
    let end = start + needle.len();
    let matched = &line[start..end];
    let before_full = &line[..start];
    let after_full = &line[end..];

    // Split the columns left over by the match evenly, then let a side that
    // does not need its half donate the rest to the other side.
    let remaining = max_width.saturating_sub(matched.width());
    let mut before_budget = remaining / 2;
    let mut after_budget = remaining - before_budget;
    if before_full.width() <= before_budget {
        before_budget = before_full.width();
        after_budget = remaining - before_budget;
    } else if after_full.width() <= after_budget {
        after_budget = after_full.width();
        before_budget = remaining - after_budget;
    }
    Some(Snippet {
        before: take_tail(before_full, before_budget),
        matched: matched.to_string(),
        after: take_head(after_full, after_budget),
    })
}

/// Keeps the end of `text` within `budget` display columns, marking a cut
/// start with `…` (which spends one of the columns).
fn take_tail(text: &str, budget: usize) -> String {
    if text.width() <= budget {
        return text.to_string();
    }
    let mut used = 0;
    let mut kept: Vec<char> = Vec::new();
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > budget.saturating_sub(1) {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept.push('…');
    kept.into_iter().rev().collect()
}

/// Keeps the start of `text` within `budget` display columns, marking a cut
/// end with `…` (which spends one of the columns).
fn take_head(text: &str, budget: usize) -> String {
    if text.width() <= budget {
        return text.to_string();
    }
    let mut used = 0;
    let mut kept = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > budget.saturating_sub(1) {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests snippets when the note does not contain the search text.
    // Given: a note without the needle, and an empty needle
    // When: note_snippet is called
    // Then: both yield None — there is nothing to show for the note
    #[test]
    fn note_snippet_without_hit_is_none() {
        assert_eq!(note_snippet("some note text", "missing", 60), None);
        assert_eq!(note_snippet("some note text", "", 60), None);
    }

    // Tests a hit on a line that fits the width entirely.
    // Given: the note line "abc def" and the needle "def", with ample width
    // When: note_snippet is called
    // Then: the line splits around the hit with no ellipsis on either side
    #[test]
    fn note_snippet_shows_short_line_in_full() {
        let snippet = note_snippet("abc def", "def", 60).unwrap();

        assert_eq!(
            snippet,
            Snippet {
                before: "abc ".to_string(),
                matched: "def".to_string(),
                after: String::new(),
            }
        );
    }

    // Tests that the first matching line wins.
    // Given: a note whose second and third lines both contain the needle
    // When: note_snippet is called
    // Then: the snippet comes from the second line (the first hit)
    #[test]
    fn note_snippet_uses_first_matching_line() {
        let snippet = note_snippet("l1\nhit here\nhit again", "hit", 60).unwrap();

        assert_eq!(snippet.before, "");
        assert_eq!(snippet.matched, "hit");
        assert_eq!(snippet.after, " here");
    }

    // Tests truncation of a hit in the middle of a long line.
    // Given: a line of 10 a's, "XX", 10 b's; needle "XX"; width 12
    // When: note_snippet is called
    // Then: the remaining 10 columns split evenly, and both cut sides carry
    //       an ellipsis inside their 5-column halves
    #[test]
    fn note_snippet_truncates_both_sides_with_ellipsis() {
        let snippet = note_snippet("aaaaaaaaaaXXbbbbbbbbbb", "XX", 12).unwrap();

        assert_eq!(
            snippet,
            Snippet {
                before: "…aaaa".to_string(),
                matched: "XX".to_string(),
                after: "bbbb…".to_string(),
            }
        );
    }

    // Tests that a short side donates its unused budget to the other side.
    // Given: the line "abXXcccccccccc" (before the hit only 2 columns wide),
    //        needle "XX", width 12
    // When: note_snippet is called
    // Then: the before side keeps its 2 columns and the after side gets the
    //       remaining 8, ending in an ellipsis
    #[test]
    fn note_snippet_gives_unused_budget_to_the_longer_side() {
        let snippet = note_snippet("abXXcccccccccc", "XX", 12).unwrap();

        assert_eq!(
            snippet,
            Snippet {
                before: "ab".to_string(),
                matched: "XX".to_string(),
                after: "ccccccc…".to_string(),
            }
        );
    }

    // Tests ASCII case-insensitive matching.
    // Given: the note "Design Doc" and the lowercase needle "design"
    // When: note_snippet is called
    // Then: the hit is found and the matched part keeps its original casing
    #[test]
    fn note_snippet_matches_ascii_case_insensitively() {
        let snippet = note_snippet("Design Doc", "design", 60).unwrap();

        assert_eq!(snippet.matched, "Design");
        assert_eq!(snippet.after, " Doc");
    }

    // Tests width accounting for double-width characters.
    // Given: the line "この設計を検討する" and the needle "設計" (4 columns)
    //        with width 12
    // When: note_snippet is called
    // Then: the 8 remaining columns fit the whole 4-column before side, and
    //       the after side is cut to "を…" within its 4 columns
    #[test]
    fn note_snippet_counts_display_columns_of_wide_characters() {
        let snippet = note_snippet("この設計を検討する", "設計", 12).unwrap();

        assert_eq!(
            snippet,
            Snippet {
                before: "この".to_string(),
                matched: "設計".to_string(),
                after: "を…".to_string(),
            }
        );
    }

    use engine::Db;
    use ratatui::style::Modifier;

    /// A fixed "today" for the result-line tests, so they never depend on
    /// the clock.
    const TODAY: &str = "2026-09-13";

    fn seeded_statuses() -> Vec<Status> {
        Db::open_in_memory().unwrap().list_statuses().unwrap()
    }

    fn task_titled(id: i64, parent_id: Option<i64>, title: &str, status_id: i64) -> Task {
        Task {
            id,
            parent_id,
            display_order: id,
            title: title.to_string(),
            status_id,
            due: None,
            note: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    // Tests the display names of every filter.
    // Given: the seeded statuses
    // When: each filter variant is named
    // Then: fixed filters use fixed names, a status filter uses the status
    //       label, and an undefined status id degrades to the raw id
    #[test]
    fn filter_name_covers_all_variants() {
        let statuses = seeded_statuses();
        let doing = statuses.iter().find(|s| s.label == "Doing").unwrap().id;

        assert_eq!(filter_name(Filter::Open, &statuses), "open");
        assert_eq!(filter_name(Filter::All, &statuses), "all");
        assert_eq!(filter_name(Filter::Overdue, &statuses), "overdue");
        assert_eq!(filter_name(Filter::Status(doing), &statuses), "Doing");
        assert_eq!(filter_name(Filter::Status(999), &statuses), "999");
    }

    // Tests the sort-menu key resolution.
    // Given: the menu keys d/u/c/t/- and an unbound key
    // When: each is resolved
    // Then: the menu keys map to their sort orders and the unbound key to
    //       None
    #[test]
    fn sort_from_key_resolves_menu_keys_only() {
        assert_eq!(sort_from_key('d'), Some(Sort::Due));
        assert_eq!(sort_from_key('u'), Some(Sort::Updated));
        assert_eq!(sort_from_key('c'), Some(Sort::Created));
        assert_eq!(sort_from_key('t'), Some(Sort::Title));
        assert_eq!(sort_from_key('-'), Some(Sort::TreeOrder));
        assert_eq!(sort_from_key('z'), None);
    }

    // Tests the filter-menu key resolution.
    // Given: the seeded statuses (keys t/r/d/x/c)
    // When: the fixed keys, a status key and an unbound key are resolved
    // Then: a/o/! map to their fixed filters, a status key maps to a status
    //       filter, and an unbound key maps to None
    #[test]
    fn filter_from_key_resolves_fixed_and_status_keys() {
        let statuses = seeded_statuses();
        let doing = statuses.iter().find(|s| s.label == "Doing").unwrap().id;

        assert_eq!(filter_from_key(&statuses, 'a'), Some(Filter::All));
        assert_eq!(filter_from_key(&statuses, 'o'), Some(Filter::Open));
        assert_eq!(filter_from_key(&statuses, '!'), Some(Filter::Overdue));
        assert_eq!(filter_from_key(&statuses, 'd'), Some(Filter::Status(doing)));
        assert_eq!(filter_from_key(&statuses, 'z'), None);
    }

    // Tests that the fixed filter keys shadow same-letter status keys.
    // Given: a status whose key is 'a', which the menu also uses for "all"
    // When: 'a' is resolved
    // Then: the fixed "all" choice wins, matching the menu display order
    #[test]
    fn filter_from_key_prefers_fixed_keys_over_status_keys() {
        let status = Status {
            id: 7,
            label: "archived".to_string(),
            kind: engine::StatusKind::Done,
            color: "gray".to_string(),
            key: 'a',
            display_order: 0,
            is_default: false,
        };

        assert_eq!(filter_from_key(&[status], 'a'), Some(Filter::All));
    }

    // Tests the query a fresh and an edited view state describe.
    // Given: a new query state, then one whose editor holds "設計" with the
    //        due sort
    // When: the query is built with the open filter
    // Then: the empty text becomes None (match everything) and the typed
    //       text is carried verbatim with sort and filter
    #[test]
    fn query_maps_empty_text_to_none() {
        let mut state = QueryState::new();
        assert_eq!(
            state.query(Filter::Open),
            engine::Query {
                text: None,
                filter: Filter::Open,
                sort: Sort::TreeOrder,
            }
        );

        state.editor = crate::input::Editor::with_text("設計");
        state.sort = Sort::Due;
        assert_eq!(
            state.query(Filter::Open),
            engine::Query {
                text: Some("設計".to_string()),
                filter: Filter::Open,
                sort: Sort::Due,
            }
        );
    }

    // Tests selection clamping after the result list shrinks.
    // Given: a selection index past the end of a one-item result list, and
    //        then an empty list
    // When: the selection is clamped
    // Then: it lands on the last item, and on 0 for the empty list
    #[test]
    fn clamp_selection_keeps_index_in_bounds() {
        let mut state = QueryState::new();
        state.results = vec![Task {
            id: 1,
            parent_id: None,
            display_order: 0,
            title: "t".to_string(),
            status_id: 1,
            due: None,
            note: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }];
        state.selected = 5;

        state.clamp_selection();
        assert_eq!(state.selected, 0);

        state.results.clear();
        state.selected = 3;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }

    // Tests the display names of every sort order.
    // Given: each sort variant
    // When: it is named
    // Then: the names match the sort-menu wording
    #[test]
    fn sort_name_covers_all_variants() {
        assert_eq!(sort_name(Sort::TreeOrder), "tree order");
        assert_eq!(sort_name(Sort::Due), "due");
        assert_eq!(sort_name(Sort::Updated), "updated");
        assert_eq!(sort_name(Sort::Created), "created");
        assert_eq!(sort_name(Sort::Title), "title");
    }

    // Tests the query header line.
    // Given: the search text "設計", the due sort and the open filter
    // When: the header is built
    // Then: it lists search text, sort name and filter name in one line
    #[test]
    fn header_shows_text_sort_and_filter() {
        let header = header("設計", Sort::Due, Filter::Open, &seeded_statuses());

        assert_eq!(header, "search: 設計 | sort: due | filter: open");
    }

    // Tests the first line of a result for a nested task.
    // Given: a chain work > design with the result task "design" on the
    //        seeded "Doing" status, searched with text matching nothing in
    //        the note
    // When: the result item is built
    // Then: it is a single line: title and status (as in the tree view)
    //       followed by a dimmed span naming the ancestor path "work"
    #[test]
    fn result_item_appends_dimmed_ancestor_path() {
        let statuses = seeded_statuses();
        let doing = statuses.iter().find(|s| s.label == "Doing").unwrap().id;
        let parent = task_titled(1, None, "work", doing);
        let task = task_titled(2, Some(1), "design", doing);
        let all = vec![parent, task.clone()];

        let lines = result_item(&task, &statuses, &all, "design", TODAY);

        assert_eq!(lines.len(), 1, "no note hit, so no snippet line");
        let contents: Vec<String> = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(contents, ["", "design", " [Doing]", "  work"]);
        let path = lines[0].spans.last().unwrap();
        assert!(path.style.add_modifier.contains(Modifier::DIM));
    }

    // Tests the first line of a result for a root task.
    // Given: a root task with no ancestors
    // When: the result item is built
    // Then: no path span is appended after the status
    #[test]
    fn result_item_for_root_task_has_no_path() {
        let statuses = seeded_statuses();
        let task = task_titled(1, None, "design", statuses[0].id);
        let all = vec![task.clone()];

        let lines = result_item(&task, &statuses, &all, "", TODAY);

        let contents: Vec<String> = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(contents, ["", "design", " [Todo]"]);
    }

    // Tests the snippet line of a result whose note contains the hit.
    // Given: a task whose note's second line contains the searched word
    //        "設計"
    // When: the result item is built
    // Then: a second line appears: an indent, the dimmed text before the
    //       hit, the hit itself undimmed, and the dimmed text after it
    #[test]
    fn result_item_adds_snippet_line_for_note_hit() {
        let statuses = seeded_statuses();
        let mut task = task_titled(1, None, "auth", statuses[0].id);
        task.note = "memo\nJWT の設計を検討".to_string();
        let all = vec![task.clone()];

        let lines = result_item(&task, &statuses, &all, "設計", TODAY);

        assert_eq!(lines.len(), 2);
        let snippet = &lines[1];
        let contents: Vec<String> = snippet
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(contents, ["  ", "JWT の", "設計", "を検討"]);
        assert!(snippet.spans[1].style.add_modifier.contains(Modifier::DIM));
        assert!(
            !snippet.spans[2].style.add_modifier.contains(Modifier::DIM),
            "the hit must stand out against the dimmed context"
        );
        assert!(snippet.spans[3].style.add_modifier.contains(Modifier::DIM));
    }

    // Tests that a result with a title-only hit gets no snippet line.
    // Given: a task whose title matches the search but whose note does not
    // When: the result item is built
    // Then: only the task line is produced
    #[test]
    fn result_item_without_note_hit_has_single_line() {
        let statuses = seeded_statuses();
        let mut task = task_titled(1, None, "設計", statuses[0].id);
        task.note = "unrelated memo".to_string();
        let all = vec![task.clone()];

        let lines = result_item(&task, &statuses, &all, "設計", TODAY);

        assert_eq!(lines.len(), 1);
    }
}
