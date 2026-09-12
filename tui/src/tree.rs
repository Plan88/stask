use std::collections::HashSet;

use engine::Task;

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
/// Traversal uses an explicit stack because tree depth is unbounded and a
/// recursive walk would tie stack usage to user data.
pub fn build_visible_rows(
    tasks: &[Task],
    expanded: &HashSet<i64>,
    zoom_root: Option<i64>,
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
            log: String::new(),
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

        let rows = build_visible_rows(&tasks, &HashSet::new(), None);

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

        let rows = build_visible_rows(&tasks, &expanded, None);

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

        let rows = build_visible_rows(&tasks, &expanded, None);

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

        let rows = build_visible_rows(&tasks, &expanded, None);

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
        let rows = build_visible_rows(&tasks, &HashSet::from([1]), None);
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
        let rows = build_visible_rows(&tasks, &HashSet::new(), None);
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
        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1));

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
        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1));

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

        let rows = build_visible_rows(&tasks, &expanded, Some(1));

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

        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1));

        assert_eq!(ids_and_depths(&tasks, &rows), [(11, 0)]);
    }

    // Tests zooming on a leaf task.
    // Given: a single root leaf task
    // When: visible rows are built zoomed on that leaf
    // Then: no rows are produced (the zoom root itself is never a row)
    #[test]
    fn zoom_on_leaf_shows_no_rows() {
        let tasks = vec![task(1, None, 0)];

        let rows = build_visible_rows(&tasks, &HashSet::new(), Some(1));

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

    // Tests that sibling order follows display_order, not slice order.
    // Given: two roots whose slice order is the reverse of their
    //        display_order
    // When: visible rows are built
    // Then: rows come out in display_order
    #[test]
    fn siblings_are_ordered_by_display_order_not_input_order() {
        let tasks = vec![task(2, None, 1), task(1, None, 0)];

        let rows = build_visible_rows(&tasks, &HashSet::new(), None);

        assert_eq!(ids_and_depths(&tasks, &rows), [(1, 0), (2, 0)]);
    }
}
