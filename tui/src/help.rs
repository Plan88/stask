//! The searchable key-binding list opened with `?`. Everything shown is
//! generated from the command table and the live keymap, so config
//! overrides are always reflected and the list can never go stale.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use crate::command;
use crate::input;
use crate::keymap;
use crate::keyspec;

/// One line of the help list: a context heading or a binding entry. The
/// command id is shown so it can be copied straight into the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    Heading(&'static str),
    Entry {
        /// All bound key sequences in canonical notation, space-separated.
        keys: String,
        label: &'static str,
        id: command::CommandId,
    },
}

/// Which part of the help view currently receives keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Scrolling through the (possibly filtered) list.
    Browse,
    /// The filter text is being typed; every keystroke re-filters.
    Edit,
}

/// View state of the help view.
pub struct HelpState {
    /// Holds the filter text at all times; only focused while editing.
    pub editor: input::Editor,
    pub focus: Focus,
    /// Index of the first visible line; no selection, just an offset.
    pub scroll: usize,
}

impl HelpState {
    pub fn new() -> Self {
        Self {
            editor: input::Editor::new(),
            focus: Focus::Browse,
            scroll: 0,
        }
    }
}

/// Builds the full help list: per context (in display order) a heading
/// followed by one entry per bound command, in command-table order.
pub fn lines(commands: &[command::Command], keymap: &keymap::Keymap) -> Vec<HelpLine> {
    let mut lines = Vec::new();
    for &(context, name) in command::CONTEXT_NAMES {
        let entries: Vec<HelpLine> = commands
            .iter()
            .filter(|cmd| cmd.context == context)
            .filter_map(|cmd| {
                let seqs = keymap.bindings_for(context, cmd.id);
                // A command stripped of all bindings cannot be invoked, so
                // listing it would only mislead.
                if seqs.is_empty() {
                    return None;
                }
                let keys = seqs
                    .iter()
                    .map(|seq| keyspec::format_seq(seq))
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(HelpLine::Entry {
                    keys,
                    label: cmd.label,
                    id: cmd.id,
                })
            })
            .collect();
        if !entries.is_empty() {
            lines.push(HelpLine::Heading(name));
            lines.extend(entries);
        }
    }
    lines
}

/// Keeps the entries whose keys, label or id contain `filter`
/// (case-insensitively), and the headings that still have entries under
/// them. An empty filter keeps everything.
pub fn filter_lines(lines: Vec<HelpLine>, filter: &str) -> Vec<HelpLine> {
    if filter.is_empty() {
        return lines;
    }
    let needle = filter.to_lowercase();
    let mut kept = Vec::new();
    // Emitting each heading lazily, only when one of its entries matches,
    // drops the headings of fully filtered-out contexts.
    let mut pending_heading: Option<HelpLine> = None;
    for line in lines {
        match line {
            HelpLine::Heading(_) => pending_heading = Some(line),
            HelpLine::Entry {
                ref keys,
                label,
                id,
            } => {
                let matches = keys.to_lowercase().contains(&needle)
                    || label.to_lowercase().contains(&needle)
                    || id.to_lowercase().contains(&needle);
                if matches {
                    if let Some(heading) = pending_heading.take() {
                        kept.push(heading);
                    }
                    kept.push(line);
                }
            }
        }
    }
    kept
}

/// Renders help lines for display: dimmed headings, and entries aligned
/// into keys / label / id columns sized to the widest value of each.
pub fn display_lines(lines: &[HelpLine]) -> Vec<Line<'static>> {
    let (keys_width, label_width) =
        lines
            .iter()
            .fold((0, 0), |(keys_max, label_max), line| match line {
                HelpLine::Entry { keys, label, .. } => {
                    (keys_max.max(keys.width()), label_max.max(label.width()))
                }
                HelpLine::Heading(_) => (keys_max, label_max),
            });
    lines
        .iter()
        .map(|line| match line {
            HelpLine::Heading(name) => Line::from(ratatui::text::Span::styled(
                (*name).to_string(),
                Style::default().add_modifier(Modifier::DIM),
            )),
            HelpLine::Entry { keys, label, id } => {
                // Manual padding because format!'s width counts chars, not
                // display columns, and a user key could be double-width.
                let keys_pad = " ".repeat(keys_width - keys.width());
                let label_pad = " ".repeat(label_width - label.width());
                Line::from(format!("  {keys}{keys_pad}  {label}{label_pad}  {id}"))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{COMMANDS, id};

    fn real_lines() -> Vec<HelpLine> {
        lines(COMMANDS, &keymap::Keymap::default())
    }

    fn entry(lines: &[HelpLine], wanted: command::CommandId) -> &HelpLine {
        lines
            .iter()
            .find(|line| matches!(line, HelpLine::Entry { id, .. } if *id == wanted))
            .expect("entry must exist")
    }

    // Tests the overall shape of the generated help list.
    // Given: the real command table and the default keymap
    // When: the help lines are built
    // Then: the list starts with the "tree" heading, headings follow the
    //       fixed context display order, and every command of the table
    //       appears exactly once as an entry
    #[test]
    fn lines_cover_every_command_grouped_by_context() {
        let lines = real_lines();

        assert_eq!(lines.first(), Some(&HelpLine::Heading("tree")));
        let headings: Vec<&str> = lines
            .iter()
            .filter_map(|line| match line {
                HelpLine::Heading(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(
            headings,
            vec![
                "tree",
                "query",
                "help",
                "input",
                "status_select",
                "status_manage",
                "filter_select",
                "sort_select",
            ]
        );
        let entry_count = lines
            .iter()
            .filter(|line| matches!(line, HelpLine::Entry { .. }))
            .count();
        assert_eq!(entry_count, COMMANDS.len());
    }

    // Tests the content of a single-binding entry.
    // Given: the default keymap, where delete is bound to "D" in Tree
    // When: the help lines are built
    // Then: the delete entry carries the key notation, the label and the
    //       command id
    #[test]
    fn entry_shows_keys_label_and_id() {
        let lines = real_lines();

        assert_eq!(
            entry(&lines, id::TASK_DELETE),
            &HelpLine::Entry {
                keys: "D".to_string(),
                label: "delete",
                id: id::TASK_DELETE,
            }
        );
    }

    // Tests that an entry lists all bindings of its command.
    // Given: the default keymap, where query-close is bound to "q" and Esc
    // When: the help lines are built
    // Then: the entry joins both notations with a space
    #[test]
    fn entry_joins_multiple_bindings() {
        let lines = real_lines();

        let HelpLine::Entry { keys, .. } = entry(&lines, id::QUERY_CLOSE) else {
            unreachable!("entry() only returns entries");
        };
        assert_eq!(keys, "q <esc>");
    }

    // Tests that special keys render in the canonical config notation.
    // Given: the default keymap, where toggle-expand is bound to Tab
    // When: the help lines are built
    // Then: the entry shows "<tab>", ready to copy into the config file
    #[test]
    fn entry_uses_canonical_key_notation() {
        let lines = real_lines();

        let HelpLine::Entry { keys, .. } = entry(&lines, id::TOGGLE_EXPAND) else {
            unreachable!("entry() only returns entries");
        };
        assert_eq!(keys, "<tab>");
    }

    // Tests filtering by each searchable field.
    // Given: the full help list
    // When: filtering by a label fragment ("expand"), an id fragment
    //       ("zoom") and a key notation fragment ("<tab>")
    // Then: only entries matching in that field survive, with their
    //       context headings; unrelated headings disappear
    #[test]
    fn filter_matches_keys_label_and_id() {
        let by_label = filter_lines(real_lines(), "expand");
        assert_eq!(
            by_label
                .iter()
                .filter(|l| matches!(l, HelpLine::Entry { .. }))
                .count(),
            1
        );
        assert_eq!(by_label.first(), Some(&HelpLine::Heading("tree")));

        let by_id = filter_lines(real_lines(), "zoom");
        assert!(by_id.iter().all(|line| match line {
            HelpLine::Entry { id, .. } => id.contains("zoom"),
            HelpLine::Heading(name) => *name == "tree",
        }));

        let by_keys = filter_lines(real_lines(), "<tab>");
        assert!(
            by_keys
                .iter()
                .any(|line| matches!(line, HelpLine::Entry { id, .. } if *id == id::TOGGLE_EXPAND))
        );
    }

    // Tests that filtering is case-insensitive and that an empty filter
    // keeps the whole list.
    // Given: the full help list
    // When: filtering by "EXPAND" and by ""
    // Then: the uppercase filter still finds the entry, and the empty
    //       filter changes nothing
    #[test]
    fn filter_is_case_insensitive_and_empty_keeps_all() {
        let upper = filter_lines(real_lines(), "EXPAND");
        assert!(
            upper
                .iter()
                .any(|line| matches!(line, HelpLine::Entry { id, .. } if *id == id::TOGGLE_EXPAND))
        );

        let all = real_lines();
        assert_eq!(filter_lines(real_lines(), ""), all);
    }

    // Tests that headings without surviving entries are dropped.
    // Given: the full help list
    // When: filtering by "jump", which only the query context matches
    // Then: exactly one heading remains, and it is "query"
    #[test]
    fn filter_drops_empty_headings() {
        let filtered = filter_lines(real_lines(), "jump");

        let headings: Vec<&str> = filtered
            .iter()
            .filter_map(|line| match line {
                HelpLine::Heading(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(headings, vec!["query"]);
    }

    // Tests the rendered form of the list.
    // Given: a heading and two entries whose keys and labels differ in
    //        width
    // When: the display lines are built
    // Then: the heading is dimmed, and the entries pad keys and labels so
    //       the three columns line up
    #[test]
    fn display_lines_align_columns_and_dim_headings() {
        let lines = vec![
            HelpLine::Heading("tree"),
            HelpLine::Entry {
                keys: "d".to_string(),
                label: "delete",
                id: id::TASK_DELETE,
            },
            HelpLine::Entry {
                keys: "<tab>".to_string(),
                label: "expand",
                id: id::TOGGLE_EXPAND,
            },
        ];

        let rendered = display_lines(&lines);

        assert_eq!(rendered.len(), 3);
        assert!(
            rendered[0]
                .spans
                .iter()
                .all(|span| span.style.add_modifier.contains(Modifier::DIM)),
            "headings must be dimmed"
        );
        let texts: Vec<String> = rendered.iter().map(|line| line.to_string()).collect();
        assert_eq!(texts[1], "  d      delete  task.delete");
        assert_eq!(texts[2], "  <tab>  expand  task.toggle_expand");
    }
}
