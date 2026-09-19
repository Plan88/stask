use crate::command;
use crate::key::{self, Key};
use crate::keyspec;

/// Maps `(Context, KeySeq)` to a command. This single table is the source of
/// truth for both dispatch and generated key-hint displays.
#[derive(Debug)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

#[derive(Debug)]
struct Binding {
    context: command::Context,
    seq: key::KeySeq,
    command: command::CommandId,
}

/// Outcome of matching an in-progress key sequence against the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// The sequence exactly matches a binding.
    Match(command::CommandId),
    /// The sequence is a strict prefix of at least one binding.
    Prefix,
    /// No binding matches or extends the sequence.
    Miss,
}

impl Keymap {
    pub fn lookup(&self, context: command::Context, keys: &[Key]) -> Lookup {
        let mut is_prefix = false;
        for binding in self.bindings.iter().filter(|b| b.context == context) {
            if binding.seq.as_slice() == keys {
                return Lookup::Match(binding.command);
            }
            if binding.seq.starts_with(keys) {
                is_prefix = true;
            }
        }
        if is_prefix {
            Lookup::Prefix
        } else {
            Lookup::Miss
        }
    }

    /// Returns the key sequence bound to `command` in `context`, if any.
    pub fn binding_for(
        &self,
        context: command::Context,
        command: command::CommandId,
    ) -> Option<&key::KeySeq> {
        self.bindings
            .iter()
            .find(|b| b.context == context && b.command == command)
            .map(|b| &b.seq)
    }

    /// Returns every key sequence bound to `command` in `context`, in
    /// binding order (a command may have several, e.g. `q` and `<esc>`).
    pub fn bindings_for(
        &self,
        context: command::Context,
        command: command::CommandId,
    ) -> Vec<&key::KeySeq> {
        self.bindings
            .iter()
            .filter(|b| b.context == context && b.command == command)
            .map(|b| &b.seq)
            .collect()
    }

    /// Replaces every binding of `command` in `context` with `seqs`, used
    /// by config overrides. The new bindings keep the replaced bindings'
    /// position so footer ordering stays stable.
    pub fn rebind(
        &mut self,
        context: command::Context,
        command: command::CommandId,
        seqs: Vec<key::KeySeq>,
    ) {
        let position = self
            .bindings
            .iter()
            .position(|b| b.context == context && b.command == command)
            .unwrap_or(self.bindings.len());
        self.bindings
            .retain(|b| !(b.context == context && b.command == command));
        for (offset, seq) in seqs.into_iter().enumerate() {
            self.bindings.insert(
                position + offset,
                Binding {
                    context,
                    seq,
                    command,
                },
            );
        }
    }

    /// Finds two bindings sharing the exact same sequence in one context;
    /// such a pair would make the loser silently unreachable, so config
    /// loading treats it as an error.
    pub fn duplicate(
        &self,
    ) -> Option<(
        command::Context,
        &key::KeySeq,
        command::CommandId,
        command::CommandId,
    )> {
        for (index, first) in self.bindings.iter().enumerate() {
            for second in &self.bindings[index + 1..] {
                if first.context == second.context && first.seq == second.seq {
                    return Some((first.context, &first.seq, first.command, second.command));
                }
            }
        }
        None
    }

    /// Describes bindings that can never fire because another binding in
    /// the same context is a strict prefix of theirs (exact matches win
    /// over prefix waiting, so the shorter sequence always fires first).
    pub fn shadow_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for shadowed in &self.bindings {
            for shorter in &self.bindings {
                let is_strict_prefix = shorter.context == shadowed.context
                    && shorter.seq.as_slice().len() < shadowed.seq.as_slice().len()
                    && shadowed.seq.starts_with(shorter.seq.as_slice());
                if is_strict_prefix {
                    warnings.push(format!(
                        "warning: \"{}\" shadows \"{}\" ({})",
                        keyspec::format_seq(&shorter.seq),
                        keyspec::format_seq(&shadowed.seq),
                        shadowed.command,
                    ));
                }
            }
        }
        warnings
    }
}

impl Default for Keymap {
    fn default() -> Self {
        let bind = |context, seq, command| Binding {
            context,
            seq,
            command,
        };
        Self {
            bindings: vec![
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("q"),
                    command::id::QUIT,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("j"),
                    command::id::SELECT_NEXT,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("k"),
                    command::id::SELECT_PREV,
                ),
                // Arrow aliases in every list context; bound after j/k so
                // the footer (which shows the first binding) keeps j/k.
                bind(
                    command::Context::Tree,
                    key::KeySeq::from(Key::Down),
                    command::id::SELECT_NEXT,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::from(Key::Up),
                    command::id::SELECT_PREV,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("gg"),
                    command::id::SELECT_FIRST,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("ge"),
                    command::id::SELECT_LAST,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::from(Key::Ctrl('d')),
                    command::id::HALF_PAGE_DOWN,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::from(Key::Ctrl('u')),
                    command::id::HALF_PAGE_UP,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("n"),
                    command::id::CREATE_TASK,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("N"),
                    command::id::CREATE_CHILD,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("r"),
                    command::id::RENAME_TASK,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("s"),
                    command::id::SET_STATUS,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("t"),
                    command::id::SET_DUE,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("e"),
                    command::id::EDIT_NOTE,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("J"),
                    command::id::STATUS_NEXT,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("K"),
                    command::id::STATUS_PREV,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::from(Key::Tab),
                    command::id::TOGGLE_EXPAND,
                ),
                // Ctrl instead of Alt chords: terminal multiplexers (zellij
                // in particular) swallow Alt+hjkl for pane focus, so Alt
                // bindings never reach the app there.
                bind(
                    command::Context::Tree,
                    key::KeySeq::from(Key::Ctrl('k')),
                    command::id::TASK_MOVE_UP,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::from(Key::Ctrl('j')),
                    command::id::TASK_MOVE_DOWN,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars(">"),
                    command::id::TASK_INDENT,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("<"),
                    command::id::TASK_OUTDENT,
                ),
                // Shift-d: deleting wants a deliberate keystroke, and the
                // plain `d` stays free.
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("D"),
                    command::id::TASK_DELETE,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("u"),
                    command::id::UNDO,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("U"),
                    command::id::REDO,
                ),
                bind(
                    command::Context::StatusSelect,
                    key::KeySeq::from(Key::Esc),
                    command::id::STATUS_CANCEL,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("/"),
                    command::id::VIEW_SEARCH,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("f"),
                    command::id::VIEW_FILTER,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::chars("j"),
                    command::id::QUERY_NEXT,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::chars("k"),
                    command::id::QUERY_PREV,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::from(Key::Down),
                    command::id::QUERY_NEXT,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::from(Key::Up),
                    command::id::QUERY_PREV,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::chars("gg"),
                    command::id::QUERY_FIRST,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::chars("ge"),
                    command::id::QUERY_LAST,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::from(Key::Ctrl('d')),
                    command::id::QUERY_HALF_PAGE_DOWN,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::from(Key::Ctrl('u')),
                    command::id::QUERY_HALF_PAGE_UP,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::chars("/"),
                    command::id::QUERY_EDIT,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::chars(","),
                    command::id::QUERY_SORT,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::chars("f"),
                    command::id::VIEW_FILTER,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::from(Key::Enter),
                    command::id::QUERY_JUMP,
                ),
                // `q` is listed before Esc so the footer (which shows the
                // first binding) advertises the single-letter key.
                bind(
                    command::Context::Query,
                    key::KeySeq::chars("q"),
                    command::id::QUERY_CLOSE,
                ),
                bind(
                    command::Context::Query,
                    key::KeySeq::from(Key::Esc),
                    command::id::QUERY_CLOSE,
                ),
                bind(
                    command::Context::FilterSelect,
                    key::KeySeq::from(Key::Esc),
                    command::id::FILTER_CANCEL,
                ),
                bind(
                    command::Context::SortSelect,
                    key::KeySeq::from(Key::Esc),
                    command::id::SORT_CANCEL,
                ),
                // h/l move the selection across hierarchy levels; their
                // shifted twins H/L do the same to the view (zoom), matching
                // the lowercase/uppercase sibling pairs elsewhere (n/N, u/U).
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("h"),
                    command::id::SELECT_PARENT,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("l"),
                    command::id::SELECT_FIRST_CHILD,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("L"),
                    command::id::ZOOM_IN,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("H"),
                    command::id::ZOOM_OUT,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("S"),
                    command::id::STATUS_MANAGE,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("j"),
                    command::id::MANAGE_ROW_NEXT,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("k"),
                    command::id::MANAGE_ROW_PREV,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::from(Key::Down),
                    command::id::MANAGE_ROW_NEXT,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::from(Key::Up),
                    command::id::MANAGE_ROW_PREV,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("h"),
                    command::id::MANAGE_COL_PREV,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("l"),
                    command::id::MANAGE_COL_NEXT,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::from(Key::Enter),
                    command::id::MANAGE_EDIT,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("n"),
                    command::id::MANAGE_ADD,
                ),
                // Shift-d, matching the tree's delete key.
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("D"),
                    command::id::MANAGE_DELETE,
                ),
                // Ctrl-j/Ctrl-k, matching the tree's task-move keys.
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::from(Key::Ctrl('j')),
                    command::id::MANAGE_MOVE_DOWN,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::from(Key::Ctrl('k')),
                    command::id::MANAGE_MOVE_UP,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("*"),
                    command::id::MANAGE_SET_DEFAULT,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("u"),
                    command::id::MANAGE_UNDO,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("U"),
                    command::id::MANAGE_REDO,
                ),
                // `q` is listed before Esc so the footer (which shows the
                // first binding) advertises the single-letter key.
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("q"),
                    command::id::MANAGE_CLOSE,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::from(Key::Esc),
                    command::id::MANAGE_CLOSE,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("?"),
                    command::id::HELP,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("\\"),
                    command::id::TOGGLE_FOOTER,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::chars("j"),
                    command::id::HELP_NEXT,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::chars("k"),
                    command::id::HELP_PREV,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::from(Key::Down),
                    command::id::HELP_NEXT,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::from(Key::Up),
                    command::id::HELP_PREV,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::chars("gg"),
                    command::id::HELP_FIRST,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::chars("ge"),
                    command::id::HELP_LAST,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::from(Key::Ctrl('d')),
                    command::id::HELP_HALF_PAGE_DOWN,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::from(Key::Ctrl('u')),
                    command::id::HELP_HALF_PAGE_UP,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::chars("/"),
                    command::id::HELP_FILTER,
                ),
                // `q` is listed before Esc so the footer (which shows the
                // first binding) advertises the single-letter key.
                bind(
                    command::Context::Help,
                    key::KeySeq::chars("q"),
                    command::id::HELP_CLOSE,
                ),
                bind(
                    command::Context::Help,
                    key::KeySeq::from(Key::Esc),
                    command::id::HELP_CLOSE,
                ),
                bind(
                    command::Context::Input,
                    key::KeySeq::from(Key::Enter),
                    command::id::INPUT_CONFIRM,
                ),
                bind(
                    command::Context::Input,
                    key::KeySeq::from(Key::Esc),
                    command::id::INPUT_CANCEL,
                ),
            ],
        }
    }
}

/// Accumulates keys until they resolve to a command. A full match fires and
/// resets; a prefix waits for more keys (no timeout); a mismatch drops the
/// whole pending sequence including the offending key.
#[derive(Default)]
pub struct Dispatcher {
    pending: Vec<Key>,
}

impl Dispatcher {
    pub fn key(
        &mut self,
        keymap: &Keymap,
        context: command::Context,
        key: Key,
    ) -> Option<command::CommandId> {
        self.pending.push(key);
        match keymap.lookup(context, &self.pending) {
            Lookup::Match(command) => {
                self.pending.clear();
                Some(command)
            }
            Lookup::Prefix => None,
            Lookup::Miss => {
                self.pending.clear();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::id;

    // Tests that the keymap resolves a full key sequence to a command id.
    // Given: the default keymap
    // When: looking up single-key "q" and multi-key "gg" in the Tree context
    // Then: they resolve to the quit and select-first commands respectively
    #[test]
    fn lookup_resolves_context_and_sequence_to_command() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("q").as_slice()),
            Lookup::Match(id::QUIT)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("gg").as_slice()),
            Lookup::Match(id::SELECT_FIRST)
        );
    }

    // Tests the zoom keys.
    // Given: the default keymap
    // When: looking up "L" and "H" in the Tree context
    // Then: they resolve to zoom-in and zoom-out respectively
    #[test]
    fn shift_l_and_h_zoom_in_and_out() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("L").as_slice()),
            Lookup::Match(id::ZOOM_IN)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("H").as_slice()),
            Lookup::Match(id::ZOOM_OUT)
        );
    }

    // Tests the hierarchy navigation keys.
    // Given: the default keymap
    // When: looking up "h" and "l" in the Tree context
    // Then: they resolve to select-parent and select-first-child, the
    //       selection-level siblings of the H/L zoom pair
    #[test]
    fn h_and_l_move_between_hierarchy_levels() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("h").as_slice()),
            Lookup::Match(id::SELECT_PARENT)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("l").as_slice()),
            Lookup::Match(id::SELECT_FIRST_CHILD)
        );
    }

    // Tests the arrow-key aliases for list navigation.
    // Given: the default keymap
    // When: looking up Down and Up in every list context
    // Then: each resolves to that context's next/prev command, alongside
    //       the j/k bindings (which stay first, so the footer shows j/k)
    #[test]
    fn arrows_navigate_every_list_context() {
        let keymap = Keymap::default();
        let cases = [
            (command::Context::Tree, id::SELECT_NEXT, id::SELECT_PREV),
            (command::Context::Query, id::QUERY_NEXT, id::QUERY_PREV),
            (command::Context::Help, id::HELP_NEXT, id::HELP_PREV),
            (
                command::Context::StatusManage,
                id::MANAGE_ROW_NEXT,
                id::MANAGE_ROW_PREV,
            ),
        ];

        for (context, next, prev) in cases {
            assert_eq!(
                keymap.lookup(context, key::KeySeq::from(Key::Down).as_slice()),
                Lookup::Match(next),
                "{context:?} down"
            );
            assert_eq!(
                keymap.lookup(context, key::KeySeq::from(Key::Up).as_slice()),
                Lookup::Match(prev),
                "{context:?} up"
            );
            assert_eq!(
                keymap.binding_for(context, next),
                Some(&key::KeySeq::chars("j")),
                "{context:?} footer still advertises j"
            );
        }
    }

    // Tests the status-manage row-move keys.
    // Given: the default keymap
    // When: looking up Ctrl-j and Ctrl-k in the StatusManage context
    // Then: they resolve to move-down and move-up (matching the tree's
    //       task-move keys), and the former "J"/"K" keys are unbound
    #[test]
    fn ctrl_j_and_ctrl_k_move_status_rows() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(
                command::Context::StatusManage,
                key::KeySeq::from(Key::Ctrl('j')).as_slice()
            ),
            Lookup::Match(id::MANAGE_MOVE_DOWN)
        );
        assert_eq!(
            keymap.lookup(
                command::Context::StatusManage,
                key::KeySeq::from(Key::Ctrl('k')).as_slice()
            ),
            Lookup::Match(id::MANAGE_MOVE_UP)
        );
        assert_eq!(
            keymap.lookup(
                command::Context::StatusManage,
                key::KeySeq::chars("J").as_slice()
            ),
            Lookup::Miss
        );
        assert_eq!(
            keymap.lookup(
                command::Context::StatusManage,
                key::KeySeq::chars("K").as_slice()
            ),
            Lookup::Miss
        );
    }

    // Tests the creation keys.
    // Given: the default keymap
    // When: looking up "n"/"N" in Tree and "n" in StatusManage
    // Then: they resolve to create-task, create-child and manage-add, and
    //       the former "o"/"a" keys are unbound (kept free for later use)
    #[test]
    fn n_keys_bind_creation_commands() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("n").as_slice()),
            Lookup::Match(id::CREATE_TASK)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("N").as_slice()),
            Lookup::Match(id::CREATE_CHILD)
        );
        assert_eq!(
            keymap.lookup(
                command::Context::StatusManage,
                key::KeySeq::chars("n").as_slice()
            ),
            Lookup::Match(id::MANAGE_ADD)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("o").as_slice()),
            Lookup::Miss
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("a").as_slice()),
            Lookup::Miss
        );
        assert_eq!(
            keymap.lookup(
                command::Context::StatusManage,
                key::KeySeq::chars("o").as_slice()
            ),
            Lookup::Miss
        );
    }

    // Tests the shifted status-cycling keys.
    // Given: the default keymap
    // When: looking up "J" and "K" (Shift+j/k) in the Tree context
    // Then: they resolve to status-next and status-prev respectively
    #[test]
    fn shift_j_and_k_cycle_status() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("J").as_slice()),
            Lookup::Match(id::STATUS_NEXT)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("K").as_slice()),
            Lookup::Match(id::STATUS_PREV)
        );
    }

    // Tests the structure-editing keys.
    // Given: the default keymap
    // When: looking up Ctrl-k, Ctrl-j, > and < in the Tree context
    // Then: they resolve to move-up, move-down, indent and outdent, and
    //       the former [ ] move keys are unbound
    #[test]
    fn structure_editing_keys_move_and_indent() {
        let keymap = Keymap::default();
        let cases = [
            (Key::Ctrl('k'), id::TASK_MOVE_UP),
            (Key::Ctrl('j'), id::TASK_MOVE_DOWN),
            (Key::Char('>'), id::TASK_INDENT),
            (Key::Char('<'), id::TASK_OUTDENT),
        ];

        for (key, command) in cases {
            assert_eq!(
                keymap.lookup(command::Context::Tree, &[key]),
                Lookup::Match(command),
                "{key:?}"
            );
        }
        for c in ['[', ']'] {
            assert_eq!(
                keymap.lookup(command::Context::Tree, &[Key::Char(c)]),
                Lookup::Miss,
                "'{c}' must be unbound"
            );
        }
    }

    // Tests the search and filter entry points of the tree view.
    // Given: the default keymap
    // When: looking up "/" and "f" in the Tree context
    // Then: they resolve to the search and filter commands respectively
    #[test]
    fn slash_and_f_open_search_and_filter() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("/").as_slice()),
            Lookup::Match(id::VIEW_SEARCH)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("f").as_slice()),
            Lookup::Match(id::VIEW_FILTER)
        );
    }

    // Tests the result-browsing keys of the query view.
    // Given: the default keymap
    // When: looking up the query-context bindings
    // Then: movement, re-edit, sort, filter, jump and close all resolve
    #[test]
    fn query_context_binds_browse_keys() {
        let keymap = Keymap::default();
        let cases: [(&[Key], command::CommandId); 8] = [
            (&[Key::Char('j')], id::QUERY_NEXT),
            (&[Key::Char('k')], id::QUERY_PREV),
            (&[Key::Char('/')], id::QUERY_EDIT),
            (&[Key::Char(',')], id::QUERY_SORT),
            (&[Key::Char('f')], id::VIEW_FILTER),
            (&[Key::Enter], id::QUERY_JUMP),
            (&[Key::Char('q')], id::QUERY_CLOSE),
            (&[Key::Esc], id::QUERY_CLOSE),
        ];

        for (keys, command) in cases {
            assert_eq!(
                keymap.lookup(command::Context::Query, keys),
                Lookup::Match(command)
            );
        }
    }

    // Tests the vim-style half-page movement chords.
    // Given: the default keymap
    // When: looking up Ctrl-d and Ctrl-u in the Tree, Query and Help
    //       contexts
    // Then: each resolves to that context's half-page down/up command
    #[test]
    fn ctrl_d_and_u_jump_half_pages() {
        let keymap = Keymap::default();
        let cases = [
            (command::Context::Tree, 'd', id::HALF_PAGE_DOWN),
            (command::Context::Tree, 'u', id::HALF_PAGE_UP),
            (command::Context::Query, 'd', id::QUERY_HALF_PAGE_DOWN),
            (command::Context::Query, 'u', id::QUERY_HALF_PAGE_UP),
            (command::Context::Help, 'd', id::HELP_HALF_PAGE_DOWN),
            (command::Context::Help, 'u', id::HELP_HALF_PAGE_UP),
        ];

        for (context, c, command) in cases {
            assert_eq!(
                keymap.lookup(context, &[Key::Ctrl(c)]),
                Lookup::Match(command),
                "<ctrl-{c}> in {context:?}"
            );
        }
    }

    // Tests that bindings are scoped to their context.
    // Given: the default keymap, where "q" is bound only in the Tree context
    // When: looking up "q" in the Input context
    // Then: the lookup misses
    #[test]
    fn lookup_does_not_cross_contexts() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Input, key::KeySeq::chars("q").as_slice()),
            Lookup::Miss
        );
    }

    // Tests that a prefix of a longer binding waits instead of firing.
    // Given: the default keymap, which binds "gg" but not "g" alone
    // When: feeding a lone "g" to the dispatcher in the Tree context
    // Then: no command fires (the dispatcher waits for the next key)
    #[test]
    fn prefix_key_waits_without_firing() {
        let keymap = Keymap::default();
        let mut dispatcher = Dispatcher::default();

        let fired = dispatcher.key(&keymap, command::Context::Tree, Key::Char('g'));

        assert_eq!(fired, None);
    }

    // Tests that completing a multi-key sequence fires its command.
    // Given: a dispatcher holding a pending "g"
    // When: a second "g" arrives
    // Then: the select-first command fires
    #[test]
    fn completed_sequence_fires_command() {
        let keymap = Keymap::default();
        let mut dispatcher = Dispatcher::default();

        dispatcher.key(&keymap, command::Context::Tree, Key::Char('g'));
        let fired = dispatcher.key(&keymap, command::Context::Tree, Key::Char('g'));

        assert_eq!(fired, Some(id::SELECT_FIRST));
    }

    // Tests that a mismatch clears the pending buffer entirely.
    // Given: a dispatcher holding a pending "g"
    // When: an unbound continuation "x" arrives, then "g" then "g"
    // Then: "gx" fires nothing, and the fresh "gg" fires select-first,
    //       proving the stale prefix was dropped
    #[test]
    fn mismatch_clears_pending_buffer() {
        let keymap = Keymap::default();
        let mut dispatcher = Dispatcher::default();

        dispatcher.key(&keymap, command::Context::Tree, Key::Char('g'));
        let miss = dispatcher.key(&keymap, command::Context::Tree, Key::Char('x'));
        dispatcher.key(&keymap, command::Context::Tree, Key::Char('g'));
        let fired = dispatcher.key(&keymap, command::Context::Tree, Key::Char('g'));

        assert_eq!(miss, None);
        assert_eq!(fired, Some(id::SELECT_FIRST));
    }

    // Tests that a single-key command fires immediately after a mismatch.
    // Given: a dispatcher whose pending "g" was cleared by an unbound "x"
    // When: "j" arrives
    // Then: the select-next command fires on that single key
    #[test]
    fn single_key_fires_after_mismatch() {
        let keymap = Keymap::default();
        let mut dispatcher = Dispatcher::default();

        dispatcher.key(&keymap, command::Context::Tree, Key::Char('g'));
        dispatcher.key(&keymap, command::Context::Tree, Key::Char('x'));
        let fired = dispatcher.key(&keymap, command::Context::Tree, Key::Char('j'));

        assert_eq!(fired, Some(id::SELECT_NEXT));
    }

    // Tests that all bindings of a command are returned in binding order.
    // Given: the default keymap, where query-close is bound to both "q"
    //        and Esc
    // When: asking for all bindings of query-close
    // Then: both sequences come back, "q" first
    #[test]
    fn bindings_for_returns_every_binding_of_a_command() {
        let keymap = Keymap::default();

        let seqs = keymap.bindings_for(command::Context::Query, id::QUERY_CLOSE);

        assert_eq!(
            seqs,
            vec![&key::KeySeq::chars("q"), &key::KeySeq::from(Key::Esc)]
        );
    }

    // Tests that rebinding replaces a command's default bindings.
    // Given: the default keymap, where delete is bound to "D" in Tree
    // When: rebinding task.delete to "x"
    // Then: "x" fires delete, "D" no longer resolves, and the command's
    //       binding list contains only the new sequence
    #[test]
    fn rebind_replaces_default_bindings() {
        let mut keymap = Keymap::default();

        keymap.rebind(
            command::Context::Tree,
            id::TASK_DELETE,
            vec![key::KeySeq::chars("x")],
        );

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("x").as_slice()),
            Lookup::Match(id::TASK_DELETE)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("D").as_slice()),
            Lookup::Miss
        );
        assert_eq!(
            keymap.bindings_for(command::Context::Tree, id::TASK_DELETE),
            vec![&key::KeySeq::chars("x")]
        );
    }

    // Tests that rebinding can give a command several sequences at once.
    // Given: the default keymap
    // When: rebinding select-first to both "gg" and "<" (freed is not
    //       required for this test; duplicates are checked separately)
    // Then: both sequences resolve to select-first
    #[test]
    fn rebind_accepts_multiple_sequences() {
        let mut keymap = Keymap::default();

        keymap.rebind(
            command::Context::Tree,
            id::SELECT_FIRST,
            vec![key::KeySeq::chars("gg"), key::KeySeq::chars("G")],
        );

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("gg").as_slice()),
            Lookup::Match(id::SELECT_FIRST)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("G").as_slice()),
            Lookup::Match(id::SELECT_FIRST)
        );
    }

    // Tests duplicate detection across a context.
    // Given: the default keymap (clean), then a rebind that gives delete
    //        the same "u" sequence undo already uses in Tree
    // When: scanning for duplicates before and after
    // Then: the clean map reports none; the clashing map names the
    //       sequence and both commands
    #[test]
    fn duplicate_detects_same_sequence_in_one_context() {
        let mut keymap = Keymap::default();
        assert!(keymap.duplicate().is_none());

        keymap.rebind(
            command::Context::Tree,
            id::TASK_DELETE,
            vec![key::KeySeq::chars("u")],
        );

        let (context, seq, first, second) = keymap.duplicate().expect("duplicate must be found");
        assert_eq!(context, command::Context::Tree);
        assert_eq!(seq, &key::KeySeq::chars("u"));
        // The pair's order follows binding order, which is incidental here.
        let mut pair = [first, second];
        pair.sort_unstable();
        assert_eq!(pair, [id::UNDO, id::TASK_DELETE]);
    }

    // Tests shadowed-binding detection.
    // Given: the default keymap (clean), then "g" bound to select-last so
    //        it becomes a strict prefix of "gg" (select-first) in Tree
    // When: collecting shadow warnings before and after
    // Then: the clean map yields none; the shadowing map yields a warning
    //       naming both sequences and the unreachable command
    #[test]
    fn shadow_warnings_report_prefix_shadowing() {
        let mut keymap = Keymap::default();
        assert!(keymap.shadow_warnings().is_empty());

        keymap.rebind(
            command::Context::Tree,
            id::SELECT_LAST,
            vec![key::KeySeq::chars("g")],
        );

        let warnings = keymap.shadow_warnings();
        assert_eq!(
            warnings,
            vec![r#"warning: "g" shadows "gg" (tree.select_first)"#.to_string()]
        );
    }

    // Tests that the keymap and the command table stay consistent.
    // Given: the default keymap and the command table
    // When: cross-checking ids in both directions
    // Then: every binding refers to a defined command, and every command
    //       has a binding in its own context (so hints never dangle)
    #[test]
    fn default_keymap_and_command_table_are_consistent() {
        let keymap = Keymap::default();

        for binding in &keymap.bindings {
            assert!(
                command::COMMANDS.iter().any(|c| c.id == binding.command),
                "binding for `{}` refers to an undefined command",
                binding.command
            );
        }
        for cmd in command::COMMANDS {
            assert!(
                keymap.binding_for(cmd.context, cmd.id).is_some(),
                "command `{}` has no binding in its context",
                cmd.id
            );
        }
    }
}
