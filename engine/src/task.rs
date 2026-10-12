/// Which neighbour a task swaps display order with when reordered within
/// its sibling group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMove {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub display_order: i64,
    pub title: String,
    pub status_id: i64,
    pub due: Option<String>,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A copy of a task subtree's content, detached from the database: it stays
/// pasteable after the original tasks are edited or deleted.
///
/// The nodes form a flat preorder list rather than nested children because
/// derived Drop, Clone, PartialEq and Debug recurse into nested values,
/// which would tie stack usage to the (unbounded) tree depth. Invariant:
/// `nodes[0]` is the root and every other node's `parent` is a smaller
/// index. Fields stay crate-private so no caller can break that invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub(crate) nodes: Vec<SnapshotNode>,
}

impl TaskSnapshot {
    /// Names the snapshot for messages and undo descriptions, e.g. `"a"`
    /// for one task or `"a" (3 tasks)` for a subtree.
    pub fn summary(&self) -> String {
        let title = self.nodes.first().map_or("", |root| root.title.as_str());
        match self.nodes.len() {
            0 | 1 => format!("\"{title}\""),
            count => format!("\"{title}\" ({count} tasks)"),
        }
    }
}

/// One task of a snapshot. Only the content that a duplicate keeps is
/// stored; status, due date and timestamps are chosen when pasting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotNode {
    /// Index of the parent node in the snapshot; None only for the root.
    pub(crate) parent: Option<usize>,
    pub(crate) title: String,
    pub(crate) note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(parent: Option<usize>, title: &str) -> SnapshotNode {
        SnapshotNode {
            parent,
            title: title.to_string(),
            note: String::new(),
        }
    }

    // Tests the summary of a single-task snapshot.
    // Given: a snapshot holding only the root "a"
    // When: summary is called
    // Then: it is the quoted title alone, with no task count
    #[test]
    fn summary_of_single_node_is_quoted_title() {
        let snapshot = TaskSnapshot {
            nodes: vec![node(None, "a")],
        };

        let summary = snapshot.summary();

        assert_eq!(summary, "\"a\"");
    }

    // Tests the summary of a multi-task snapshot.
    // Given: a snapshot of root "a" with two descendants
    // When: summary is called
    // Then: it is the quoted root title followed by the total task count
    #[test]
    fn summary_of_subtree_counts_all_tasks() {
        let snapshot = TaskSnapshot {
            nodes: vec![node(None, "a"), node(Some(0), "b"), node(Some(1), "c")],
        };

        let summary = snapshot.summary();

        assert_eq!(summary, "\"a\" (3 tasks)");
    }
}
