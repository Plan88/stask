use std::collections::HashSet;

use engine::{Filter, Status, Task};

/// One visible line of the tree view, referring back into the task slice
/// the view was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub task_index: usize,
    pub depth: usize,
    pub has_children: bool,
}

/// Flattens the task tree into visible rows: roots in display order, with
/// the children of each expanded task interleaved beneath it. Subtrees under
/// a collapsed task stay hidden even if their own expansion flags are set.
///
/// With `zoom_root` set, only that task's subtree is shown: its children
/// become depth-0 rows (the zoom root itself gets no row) so deep nesting
/// never squeezes the view to the right.
///
/// With `visible` set, tasks outside it are skipped along with their whole
/// subtrees (the set already contains the ancestors of everything it wants
/// shown, so nothing below a skipped task can be in it).
///
/// Traversal uses an explicit stack because tree depth is unbounded and a
/// recursive walk would tie stack usage to user data.
pub fn build_visible_rows(
    tasks: &[Task],
    expanded: &HashSet<i64>,
    zoom_root: Option<i64>,
    visible: Option<&HashSet<i64>>,
) -> Vec<Row> {
    let mut children: std::collections::HashMap<Option<i64>, Vec<usize>> =
        std::collections::HashMap::new();
    for (index, task) in tasks.iter().enumerate() {
        children.entry(task.parent_id).or_default().push(index);
    }
    for group in children.values_mut() {
        group.sort_by_key(|&index| tasks[index].display_order);
    }

    let mut rows = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    // The top-level group is the zoom root's children, or the real roots
    // (parent_id None) when not zoomed — exactly the `children` key shape.
    // Reversed so that popping yields siblings in display order.
    if let Some(top) = children.get(&zoom_root) {
        stack.extend(top.iter().rev().map(|&index| (index, 0)));
    }
    while let Some((task_index, depth)) = stack.pop() {
        let id = tasks[task_index].id;
        if let Some(visible) = visible
            && !visible.contains(&id)
        {
            continue;
        }
        let child_group = children.get(&Some(id));
        rows.push(Row {
            task_index,
            depth,
            has_children: child_group.is_some(),
        });
        if let Some(group) = child_group
            && expanded.contains(&id)
        {
            stack.extend(group.iter().rev().map(|&index| (index, depth + 1)));
        }
    }
    rows
}

/// Computes which tasks the tree may show under `filter`: every task that
/// matches it, plus all their ancestors — hiding a done parent must not
/// hide its still-open children. None means "show everything" (Filter::All
/// needs no set at all). `today` (as `YYYY-MM-DD`) is passed in so the
/// overdue check stays a pure function of its inputs.
pub fn filter_visible_ids(
    tasks: &[Task],
    statuses: &[Status],
    filter: Filter,
    today: &str,
) -> Option<HashSet<i64>> {
    if filter == Filter::All {
        return None;
    }
    let mut visible: HashSet<i64> = tasks
        .iter()
        .filter(|task| task_matches_filter(task, statuses, filter, today))
        .map(|task| task.id)
        .collect();
    let parent_of: std::collections::HashMap<i64, Option<i64>> =
        tasks.iter().map(|task| (task.id, task.parent_id)).collect();
    for task in tasks {
        if !visible.contains(&task.id) {
            continue;
        }
        // Walk up until an already-visible ancestor; everything above it
        // has been added before.
        let mut current = task.parent_id;
        while let Some(parent) = current {
            if !visible.insert(parent) {
                break;
            }
            current = parent_of.get(&parent).copied().flatten();
        }
    }
    Some(visible)
}

/// Whether a task itself satisfies `filter`. A status id matching no
/// definition (possible only through outside edits of the database file) is
/// treated as open so broken data stays visible instead of vanishing.
fn task_matches_filter(task: &Task, statuses: &[Status], filter: Filter, today: &str) -> bool {
    let kind = statuses
        .iter()
        .find(|s| s.id == task.status_id)
        .map(|s| s.kind)
        .unwrap_or(engine::StatusKind::Open);
    match filter {
        Filter::All => true,
        Filter::Open => kind == engine::StatusKind::Open,
        Filter::Status(status_id) => task.status_id == status_id,
        // Stored dates are canonical YYYY-MM-DD (enforced on write), so
        // plain string comparison is a correct date comparison.
        Filter::Overdue => {
            kind == engine::StatusKind::Open && task.due.as_deref().is_some_and(|due| due < today)
        }
    }
}

/// Where a task created from the current selection should go. Decided when
/// input starts, so the submit path no longer cares which key opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateTarget {
    pub parent_id: Option<i64>,
    /// Insert right after this sibling display_order; None appends at the
    /// tail of the sibling group.
    pub after: Option<i64>,
}

/// Target for creating a sibling right below the selected row. With no
/// visible row (empty view) the task goes to the tail under the zoom root,
/// or to the root tail when not zoomed.
pub fn sibling_target(
    tasks: &[Task],
    rows: &[Row],
    selected: usize,
    zoom_root: Option<i64>,
) -> CreateTarget {
    match rows.get(selected) {
        Some(row) => {
            let task = &tasks[row.task_index];
            CreateTarget {
                parent_id: task.parent_id,
                after: Some(task.display_order),
            }
        }
        None => CreateTarget {
            parent_id: zoom_root,
            after: None,
        },
    }
}

/// Target for creating a child at the tail of the selected row's children.
/// With no visible row (empty view) this degrades to creation under the
/// zoom root, or at the root level when not zoomed.
pub fn child_target(
    tasks: &[Task],
    rows: &[Row],
    selected: usize,
    zoom_root: Option<i64>,
) -> CreateTarget {
    CreateTarget {
        parent_id: rows
            .get(selected)
            .map(|row| tasks[row.task_index].id)
            .or(zoom_root),
        after: None,
    }
}

/// Where a re-parented task should be attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReparentTarget {
    pub new_parent: Option<i64>,
    /// Land right after this sibling display_order; None appends at the
    /// tail of the new sibling group.
    pub after: Option<i64>,
}

/// New parent for indenting `task_id`: its preceding sibling by
/// display_order (not by visible row, which may belong to another subtree).
/// None for the first sibling — there is nothing to indent under.
pub fn indent_new_parent(tasks: &[Task], task_id: i64) -> Option<i64> {
    let task = tasks.iter().find(|t| t.id == task_id)?;
    siblings_of(tasks, task)
        .filter(|s| s.display_order < task.display_order)
        .max_by_key(|s| s.display_order)
        .map(|s| s.id)
}

/// Target for outdenting `task_id`: it becomes its parent's next sibling.
/// None when the task is already at the root level, or when its parent is
/// the zoom root (outdenting would move it outside the zoomed view).
pub fn outdent_target(
    tasks: &[Task],
    task_id: i64,
    zoom_root: Option<i64>,
) -> Option<ReparentTarget> {
    let task = tasks.iter().find(|t| t.id == task_id)?;
    let parent_id = task.parent_id?;
    if zoom_root == Some(parent_id) {
        return None;
    }
    let parent = tasks.iter().find(|t| t.id == parent_id)?;
    Some(ReparentTarget {
        new_parent: parent.parent_id,
        after: Some(parent.display_order),
    })
}

/// Which task the cursor should land on after `deleted_id`'s subtree is
/// removed: next sibling, else previous sibling, else parent. None when the
/// last root task is deleted.
pub fn selection_after_delete(tasks: &[Task], deleted_id: i64) -> Option<i64> {
    let task = tasks.iter().find(|t| t.id == deleted_id)?;
    let next = siblings_of(tasks, task)
        .filter(|s| s.display_order > task.display_order)
        .min_by_key(|s| s.display_order);
    let previous = siblings_of(tasks, task)
        .filter(|s| s.display_order < task.display_order)
        .max_by_key(|s| s.display_order);
    next.or(previous).map(|s| s.id).or(task.parent_id)
}

/// The other members of `task`'s sibling group.
fn siblings_of<'a>(tasks: &'a [Task], task: &'a Task) -> impl Iterator<Item = &'a Task> {
    tasks
        .iter()
        .filter(move |t| t.parent_id == task.parent_id && t.id != task.id)
}

/// Renders the header path for a zoomed view: ancestor titles down to the
/// zoom root itself, e.g. `work › project X › design`. Walks parent links
/// iteratively because tree depth is unbounded.
pub fn breadcrumb(tasks: &[Task], zoom_root: i64) -> String {
    let by_id: std::collections::HashMap<i64, &Task> =
        tasks.iter().map(|task| (task.id, task)).collect();
    let mut titles = Vec::new();
    let mut current = Some(zoom_root);
    while let Some(id) = current {
        let Some(task) = by_id.get(&id) else { break };
        titles.push(task.title.as_str());
        current = task.parent_id;
    }
    titles.reverse();
    titles.join(" › ")
}

/// Ids of `id`'s ancestors, nearest parent first. Expanding all of them
/// makes the task visible in the tree. Walks parent links iteratively
/// because tree depth is unbounded. Empty for roots and unknown ids.
pub fn ancestors_of(tasks: &[Task], id: i64) -> Vec<i64> {
    let by_id: std::collections::HashMap<i64, &Task> =
        tasks.iter().map(|task| (task.id, task)).collect();
    let mut ancestors = Vec::new();
    let mut current = by_id.get(&id).and_then(|task| task.parent_id);
    while let Some(parent) = current {
        ancestors.push(parent);
        current = by_id.get(&parent).and_then(|task| task.parent_id);
    }
    ancestors
}

/// Renders the indentation and expansion marker preceding a row title.
/// Leaves get a marker-width blank so titles align across sibling rows.
pub fn row_prefix(depth: usize, has_children: bool, is_expanded: bool) -> String {
    let marker = match (has_children, is_expanded) {
        (true, true) => "▾ ",
        (true, false) => "▸ ",
        (false, _) => "  ",
    };
    format!("{}{}", "  ".repeat(depth), marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: i64, parent_id: Option<i64>, display_order: i64) -> Task {
        Task {
            id,
            parent_id,
            display_order,
            title: format!("task {id}"),
            status_id: 1,
            due: None,
            note: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn ids_and_depths(tasks: &[Task], rows: &[Row]) -> Vec<(i64, usize)> {
        rows.iter()
            .map(|r| (tasks[r.task_index].id, r.depth))
            .collect()
    }

    // Tests the fully collapsed view.
    // Given: roots 1 and 2, with children 11, 12 under root 1, and nothing
    //        expanded
    // When: visible rows are built
    // Then: only the roots appear at depth 0, and the parent root is flagged
    //       as having children while the childless one is not
    #[test]
    fn all_collapsed_shows_only_roots() {
        let tasks = vec![
            task(1, None, 0),
            task(2, None, 1),
            task(11, Some(1), 0),
            task(12, Some(1), 1),
        ];

        let rows = build_visible_rows(&tasks, &HashSet::new(), None, None);

        assert_eq!(ids_and_depths(&tasks, &rows), [(1, 0), (2, 0)]);
        assert!(rows[0].has_children);
        assert!(!rows[1].has_children);
    }

    // Tests that expanding a task interleaves its children beneath it.
    // Given: roots 1 and 2, children 11, 12 under root 1, and root 1 expanded
    // When: visible rows are built
    // Then: the children appear right after root 1 at depth 1, in display
    //       order, before root 2
    #[test]
    fn expanded_parent_interleaves_children_with_depth() {
        let tasks = vec![
            task(1, None, 0),
            task(2, None, 1),
            task(11, Some(1), 0),
            task(12, Some(1), 1),
        ];
        let expanded = HashSet::from([1]);

        let rows = build_visible_rows(&tasks, &expanded, None, None);

        assert_eq!(
            ids_and_depths(&tasks, &rows),
            [(1, 0), (11, 1), (12, 1), (2, 0)]
        );
    }

    // Tests that a collapsed ancestor hides its whole subtree.
    // Given: root 1 > child 11 > grandchild 111, where child 11 is expanded
    //        but root 1 is not (a stale flag left from before collapsing)
    // When: visible rows are built
    // Then: only root 1 is visible; neither child nor grandchild leaks out
    #[test]
    fn collapsed_ancestor_hides_descendants_despite_stale_flags() {
        let tasks = vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ];
        let expanded = HashSet::from([11]);

        let rows = build_visible_rows(&tasks, &expanded, None, None);

        assert_eq!(ids_and_depths(&tasks, &rows), [(1, 0)]);
    }

    // Tests a deep chain of expanded tasks.
    // Given: a 3-level chain 1 > 11 > 111 with every level expanded
    // When: visible rows are built
    // Then: each level appears once, one depth step deeper than its parent,
    //       and only the leaf has no children
    #[test]
    fn expanded_chain_increases_depth_per_level() {
        let tasks = vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ];
        let expanded = HashSet::from([1, 11]);

        let rows = build_visible_rows(&tasks, &expanded, None, None);

        assert_eq!(ids_and_depths(&tasks, &rows), [(1, 0), (11, 1), (111, 2)]);
        assert_eq!(
            rows.iter().map(|r| r.has_children).collect::<Vec<_>>(),
            [true, true, false]
        );
    }

    // Tests sibling creation targeting for a nested selection.
    // Given: root 1 expanded with children 11 (order 0) and 12 (order 1),
    //        and the row for child 11 selected
    // When: the sibling target is computed
    // Then: the new task goes under parent 1, right after display_order 0
    #[test]
    fn sibling_target_uses_selected_rows_parent_and_order() {
        let tasks = vec![task(1, None, 0), task(11, Some(1), 0), task(12, Some(1), 1)];
        let rows = build_visible_rows(&tasks, &HashSet::from([1]), None, None);
        let selected = rows
            .iter()
            .position(|r| tasks[r.task_index].id == 11)
            .unwrap();

        let target = sibling_target(&tasks, &rows, selected, None);

        assert_eq!(
            target,
            CreateTarget {
                parent_id: Some(1),
                after: Some(0),
            }
        );
    }

    // Tests sibling creation targeting with no rows to select.
    // Given: an empty task list
    // When: the sibling target is computed
    // Then: the new task is appended at the root level
    #[test]
    fn sibling_target_on_empty_list_appends_at_root() {
        let target = sibling_target(&[], &[], 0, None);

        assert_eq!(
            target,
            CreateTarget {
                parent_id: None,
                after: None,
            }
        );
    }

    // Tests child creation targeting.
    // Given: roots 1 and 2 with root 2 selected, where root 2 already has a
    //        child
    // When: the child target is computed
    // Then: the new task goes under task 2, appended at the tail of its
    //       children (after = None)
    #[test]
    fn child_target_appends_under_selected_task() {
        let tasks = vec![task(1, None, 0), task(2, None, 1), task(21, Some(2), 0)];
        let rows = build_visible_rows(&tasks, &HashSet::new(), None, None);
        let selected = rows
            .iter()
            .position(|r| tasks[r.task_index].id == 2)
            .unwrap();

        let target = child_target(&tasks, &rows, selected, None);

        assert_eq!(
            target,
            CreateTarget {
                parent_id: Some(2),
                after: None,
            }
        );
    }

    // Tests child creation targeting with no rows to select.
    // Given: an empty task list
    // When: the child target is computed
    // Then: it degrades to root creation
    #[test]
    fn child_target_on_empty_list_appends_at_root() {
        let target = child_target(&[], &[], 0, None);

        assert_eq!(
            target,
            CreateTarget {
                parent_id: None,
                after: None,
            }
        );
    }

    // Tests the marker and indentation of row prefixes.
    // Given: rows at various depths, with and without children/expansion
    // When: the prefix is rendered
    // Then: expanded parents get a down triangle, collapsed parents a right
    //       triangle, leaves a marker-width blank, and each depth level adds
    //       two spaces of indentation
    #[test]
    fn row_prefix_combines_indent_and_marker() {
        assert_eq!(row_prefix(0, true, true), "▾ ");
        assert_eq!(row_prefix(0, true, false), "▸ ");
        assert_eq!(row_prefix(0, false, false), "  ");
        assert_eq!(row_prefix(2, true, false), "    ▸ ");
        assert_eq!(row_prefix(1, false, false), "    ");
    }

    // Tests sibling creation targeting inside a zoomed, empty subtree.
    // Given: a leaf task 1 zoomed in on, so the view has no rows
    // When: the sibling target is computed
    // Then: the new task goes under the zoom root, not to the real root level
    #[test]
    fn sibling_target_in_empty_zoom_creates_under_zoom_root() {
        let tasks = vec![task(1, None, 0)];
        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1), None);

        let target = sibling_target(&tasks, &rows, 0, Some(1));

        assert_eq!(
            target,
            CreateTarget {
                parent_id: Some(1),
                after: None,
            }
        );
    }

    // Tests child creation targeting inside a zoomed, empty subtree.
    // Given: a leaf task 1 zoomed in on, so the view has no rows
    // When: the child target is computed
    // Then: the new task goes under the zoom root, not to the real root level
    #[test]
    fn child_target_in_empty_zoom_creates_under_zoom_root() {
        let tasks = vec![task(1, None, 0)];
        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1), None);

        let target = child_target(&tasks, &rows, 0, Some(1));

        assert_eq!(
            target,
            CreateTarget {
                parent_id: Some(1),
                after: None,
            }
        );
    }

    // Tests that zooming shows only the zoomed subtree with depth reset.
    // Given: roots 1 and 2, where 1 > 11 > 111 and child 11 is expanded
    // When: visible rows are built zoomed on task 1
    // Then: task 1 itself gets no row, its child 11 starts at depth 0 with
    //       grandchild 111 at depth 1, and the unrelated root 2 is absent
    #[test]
    fn zoom_shows_subtree_with_depth_reset() {
        let tasks = vec![
            task(1, None, 0),
            task(2, None, 1),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ];
        let expanded = HashSet::from([11]);

        let rows = build_visible_rows(&tasks, &expanded, Some(1), None);

        assert_eq!(ids_and_depths(&tasks, &rows), [(11, 0), (111, 1)]);
    }

    // Tests that collapsing still works inside a zoom.
    // Given: root 1 > child 11 > grandchild 111 with nothing expanded
    // When: visible rows are built zoomed on task 1
    // Then: only child 11 shows; its collapsed subtree stays hidden
    #[test]
    fn zoom_respects_collapsed_children() {
        let tasks = vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ];

        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1), None);

        assert_eq!(ids_and_depths(&tasks, &rows), [(11, 0)]);
    }

    // Tests zooming on a leaf task.
    // Given: a single root leaf task
    // When: visible rows are built zoomed on that leaf
    // Then: no rows are produced (the zoom root itself is never a row)
    #[test]
    fn zoom_on_leaf_shows_no_rows() {
        let tasks = vec![task(1, None, 0)];

        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1), None);

        assert_eq!(rows, []);
    }

    fn titled(id: i64, parent_id: Option<i64>, title: &str) -> Task {
        Task {
            title: title.to_string(),
            ..task(id, parent_id, 0)
        }
    }

    // Tests the breadcrumb for a deeply nested zoom root.
    // Given: a 3-level chain "work" > "project X" > "design", zoomed on
    //        "design"
    // When: the breadcrumb is rendered
    // Then: it lists every ancestor and the zoom root itself, top-down,
    //       joined by the separator
    #[test]
    fn breadcrumb_lists_ancestors_and_self_top_down() {
        let tasks = vec![
            titled(1, None, "work"),
            titled(2, Some(1), "project X"),
            titled(3, Some(2), "design"),
        ];

        assert_eq!(breadcrumb(&tasks, 3), "work › project X › design");
    }

    // Tests the breadcrumb when zoomed directly on a root task.
    // Given: a root task "work"
    // When: the breadcrumb is rendered for it
    // Then: it shows just that title, with no separator
    #[test]
    fn breadcrumb_on_root_shows_single_title() {
        let tasks = vec![titled(1, None, "work")];

        assert_eq!(breadcrumb(&tasks, 1), "work");
    }

    // Tests indent targeting for a task with a preceding sibling.
    // Given: siblings 11(0), 12(1), 13(2) under parent 1, where the slice
    //        order differs from the display order
    // When: the indent parent for 13 is computed
    // Then: it is 12, the sibling directly above by display_order
    #[test]
    fn indent_new_parent_is_preceding_sibling_by_display_order() {
        let tasks = vec![
            task(1, None, 0),
            task(13, Some(1), 2),
            task(11, Some(1), 0),
            task(12, Some(1), 1),
        ];

        assert_eq!(indent_new_parent(&tasks, 13), Some(12));
        assert_eq!(indent_new_parent(&tasks, 12), Some(11));
    }

    // Tests indent targeting for a first sibling.
    // Given: siblings 11(0), 12(1) under parent 1, and a lone root 1
    // When: the indent parent for the first sibling 11 (and for root 1) is
    //       computed
    // Then: both are None — there is no sibling above to indent under
    #[test]
    fn indent_new_parent_for_first_sibling_is_none() {
        let tasks = vec![task(1, None, 0), task(11, Some(1), 0), task(12, Some(1), 1)];

        assert_eq!(indent_new_parent(&tasks, 11), None);
        assert_eq!(indent_new_parent(&tasks, 1), None);
    }

    // Tests that indent targeting ignores same-order tasks of other groups.
    // Given: roots 1(0) and 2(1), each with one child of display_order 0
    // When: the indent parent for root 2's child is computed
    // Then: it is None; root 1's child (same display_order, other group)
    //       must not be picked up
    #[test]
    fn indent_new_parent_stays_within_sibling_group() {
        let tasks = vec![
            task(1, None, 0),
            task(2, None, 1),
            task(11, Some(1), 0),
            task(21, Some(2), 0),
        ];

        assert_eq!(indent_new_parent(&tasks, 21), None);
    }

    // Tests outdent targeting for a nested task.
    // Given: root 1(0) > child 11(0) > grandchild 111(0), unzoomed
    // When: the outdent target for grandchild 111 is computed
    // Then: it moves under root 1, right after its old parent 11 (after =
    //       11's display_order)
    #[test]
    fn outdent_target_moves_after_old_parent() {
        let tasks = vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ];

        let target = outdent_target(&tasks, 111, None);

        assert_eq!(
            target,
            Some(ReparentTarget {
                new_parent: Some(1),
                after: Some(0),
            })
        );
    }

    // Tests outdent targeting to the root level.
    // Given: roots 1(0), 2(1) where root 2 has child 21, unzoomed
    // When: the outdent target for 21 is computed
    // Then: it moves to the root level right after its old parent 2
    #[test]
    fn outdent_target_to_root_level_lands_after_parent() {
        let tasks = vec![task(1, None, 0), task(2, None, 1), task(21, Some(2), 0)];

        let target = outdent_target(&tasks, 21, None);

        assert_eq!(
            target,
            Some(ReparentTarget {
                new_parent: None,
                after: Some(1),
            })
        );
    }

    // Tests that a root-level task cannot be outdented.
    // Given: a root task 1
    // When: its outdent target is computed
    // Then: it is None
    #[test]
    fn outdent_target_for_root_task_is_none() {
        let tasks = vec![task(1, None, 0)];

        assert_eq!(outdent_target(&tasks, 1, None), None);
    }

    // Tests that outdenting never escapes the zoomed subtree.
    // Given: root 1 > child 11 > grandchild 111, zoomed on task 1
    // When: outdent targets are computed for 11 (child of the zoom root)
    //       and 111 (one level deeper)
    // Then: 11 yields None (it would leave the zoomed view) while 111 still
    //       outdents normally within the zoom
    #[test]
    fn outdent_target_stops_at_zoom_root() {
        let tasks = vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ];

        assert_eq!(outdent_target(&tasks, 11, Some(1)), None);
        assert_eq!(
            outdent_target(&tasks, 111, Some(1)),
            Some(ReparentTarget {
                new_parent: Some(1),
                after: Some(0),
            })
        );
    }

    // Tests the cursor target after deleting a middle sibling.
    // Given: siblings 11(0), 12(1), 13(2) under parent 1
    // When: the post-delete selection for 12 is computed
    // Then: it is the next sibling 13
    #[test]
    fn selection_after_delete_prefers_next_sibling() {
        let tasks = vec![
            task(1, None, 0),
            task(11, Some(1), 0),
            task(12, Some(1), 1),
            task(13, Some(1), 2),
        ];

        assert_eq!(selection_after_delete(&tasks, 12), Some(13));
    }

    // Tests the cursor target after deleting the last sibling.
    // Given: siblings 11(0), 12(1) under parent 1
    // When: the post-delete selection for 12 is computed
    // Then: it falls back to the previous sibling 11
    #[test]
    fn selection_after_delete_falls_back_to_previous_sibling() {
        let tasks = vec![task(1, None, 0), task(11, Some(1), 0), task(12, Some(1), 1)];

        assert_eq!(selection_after_delete(&tasks, 12), Some(11));
    }

    // Tests the cursor target after deleting an only child.
    // Given: parent 1 with the single child 11
    // When: the post-delete selection for 11 is computed
    // Then: it falls back to the parent 1
    #[test]
    fn selection_after_delete_falls_back_to_parent() {
        let tasks = vec![task(1, None, 0), task(11, Some(1), 0)];

        assert_eq!(selection_after_delete(&tasks, 11), Some(1));
    }

    // Tests the cursor target after deleting the only root.
    // Given: a single root task 1
    // When: the post-delete selection for 1 is computed
    // Then: it is None — nothing is left to select
    #[test]
    fn selection_after_delete_of_last_root_is_none() {
        let tasks = vec![task(1, None, 0)];

        assert_eq!(selection_after_delete(&tasks, 1), None);
    }

    // Tests that sibling order follows display_order, not slice order.
    // Given: two roots whose slice order is the reverse of their
    //        display_order
    // When: visible rows are built
    // Then: rows come out in display_order
    #[test]
    fn siblings_are_ordered_by_display_order_not_input_order() {
        let tasks = vec![task(2, None, 1), task(1, None, 0)];

        let rows = build_visible_rows(&tasks, &HashSet::new(), None, None);

        assert_eq!(ids_and_depths(&tasks, &rows), [(1, 0), (2, 0)]);
    }

    // Tests the ancestor chain of a deeply nested task.
    // Given: a chain 1 > 11 > 111 plus an unrelated root 2
    // When: the ancestors of the grandchild 111 are computed
    // Then: they come back nearest-first — parent 11, then root 1 — without
    //       the unrelated root
    #[test]
    fn ancestors_of_nested_task_lists_chain_nearest_first() {
        let tasks = vec![
            task(1, None, 0),
            task(2, None, 1),
            task(11, Some(1), 0),
            task(111, Some(11), 0),
        ];

        assert_eq!(ancestors_of(&tasks, 111), [11, 1]);
    }

    // Tests the ancestor chain of a root task and of an unknown id.
    // Given: a single root task 1
    // When: the ancestors of the root and of a missing id are computed
    // Then: both are empty — there is nothing to expand for either
    #[test]
    fn ancestors_of_root_or_unknown_task_is_empty() {
        let tasks = vec![task(1, None, 0)];

        assert_eq!(ancestors_of(&tasks, 1), Vec::<i64>::new());
        assert_eq!(ancestors_of(&tasks, 999), Vec::<i64>::new());
    }

    use engine::StatusKind;

    /// A fixed "today" for the filter tests, so they never depend on the
    /// clock.
    const TODAY: &str = "2026-09-13";

    fn status(id: i64, kind: StatusKind) -> Status {
        Status {
            id,
            label: format!("status {id}"),
            kind,
            color: "gray".to_string(),
            key: char::from_digit(id as u32, 10).unwrap_or('z'),
            display_order: id,
            is_default: false,
        }
    }

    /// Statuses 1 (open) and 2 (done) for the filter tests.
    fn open_and_done() -> Vec<Status> {
        vec![status(1, StatusKind::Open), status(2, StatusKind::Done)]
    }

    fn task_with_status(id: i64, parent_id: Option<i64>, status_id: i64) -> Task {
        Task {
            status_id,
            ..task(id, parent_id, id)
        }
    }

    // Tests that rows outside the visible set are skipped with their
    // subtrees.
    // Given: roots 1 and 2 (both expanded) where 1 has child 11; a visible
    //        set containing only 1 and 11
    // When: visible rows are built with that set
    // Then: root 2 is gone while 1 and its child 11 render normally
    #[test]
    fn build_visible_rows_skips_tasks_outside_the_visible_set() {
        let tasks = vec![task(1, None, 0), task(2, None, 1), task(11, Some(1), 0)];
        let expanded = HashSet::from([1, 2]);
        let visible = HashSet::from([1, 11]);

        let rows = build_visible_rows(&tasks, &expanded, None, Some(&visible));

        assert_eq!(ids_and_depths(&tasks, &rows), [(1, 0), (11, 1)]);
    }

    // Tests that Filter::All imposes no visible set at all.
    // Given: any tasks and statuses
    // When: the visible ids for Filter::All are computed
    // Then: the result is None, meaning nothing gets filtered
    #[test]
    fn filter_visible_ids_for_all_is_none() {
        let tasks = vec![task_with_status(1, None, 2)];

        let ids = filter_visible_ids(&tasks, &open_and_done(), Filter::All, TODAY);

        assert_eq!(ids, None);
    }

    // Tests the default open filter with a finished parent of open work.
    // Given: a done root 1 with an open child 11, and a done leaf root 2
    // When: the visible ids for Filter::Open are computed
    // Then: the open child and its done parent stay visible (hiding the
    //       parent would hide the child), while the done leaf disappears
    #[test]
    fn filter_visible_ids_open_keeps_done_ancestors_of_open_tasks() {
        let tasks = vec![
            task_with_status(1, None, 2),
            task_with_status(11, Some(1), 1),
            task_with_status(2, None, 2),
        ];

        let ids = filter_visible_ids(&tasks, &open_and_done(), Filter::Open, TODAY).unwrap();

        assert_eq!(ids, HashSet::from([1, 11]));
    }

    // Tests that a task with an undefined status id stays visible.
    // Given: a task whose status id 999 matches no status (possible only
    //        through outside edits of the database file)
    // When: the visible ids for Filter::Open are computed
    // Then: the task is kept — broken data must stay visible, like the
    //       tree rendering of unknown statuses
    #[test]
    fn filter_visible_ids_keeps_tasks_with_unknown_status() {
        let tasks = vec![task_with_status(1, None, 999)];

        let ids = filter_visible_ids(&tasks, &open_and_done(), Filter::Open, TODAY).unwrap();

        assert_eq!(ids, HashSet::from([1]));
    }

    // Tests filtering by one specific status.
    // Given: root 1 on status 1 with child 11 on status 2, plus root 2 on
    //        status 1
    // When: the visible ids for Filter::Status(2) are computed
    // Then: the matching child and its ancestor root survive; the unrelated
    //       root 2 does not
    #[test]
    fn filter_visible_ids_status_matches_exact_status_plus_ancestors() {
        let tasks = vec![
            task_with_status(1, None, 1),
            task_with_status(11, Some(1), 2),
            task_with_status(2, None, 1),
        ];

        let ids = filter_visible_ids(&tasks, &open_and_done(), Filter::Status(2), TODAY).unwrap();

        assert_eq!(ids, HashSet::from([1, 11]));
    }

    // Tests the overdue filter.
    // Given: under root 1 (open, no due): child 11 open and past due, child
    //        12 open and due today, child 13 done and past due
    // When: the visible ids for Filter::Overdue are computed
    // Then: only the open past-due child and its ancestor remain — due
    //       today is not overdue, and finished tasks never are
    #[test]
    fn filter_visible_ids_overdue_keeps_open_past_due_tasks_only() {
        let mut overdue = task_with_status(11, Some(1), 1);
        overdue.due = Some("2026-09-12".to_string());
        let mut due_today = task_with_status(12, Some(1), 1);
        due_today.due = Some(TODAY.to_string());
        let mut done_late = task_with_status(13, Some(1), 2);
        done_late.due = Some("2026-09-01".to_string());
        let tasks = vec![task_with_status(1, None, 1), overdue, due_today, done_late];

        let ids = filter_visible_ids(&tasks, &open_and_done(), Filter::Overdue, TODAY).unwrap();

        assert_eq!(ids, HashSet::from([1, 11]));
    }
}
