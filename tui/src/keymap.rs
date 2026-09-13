use crate::command;
use crate::key::{self, Key};

/// Maps `(Context, KeySeq)` to a command. This single table is the source of
/// truth for both dispatch and generated key-hint displays.
pub struct Keymap {
    bindings: Vec<Binding>,
}

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
                    key::KeySeq::chars("o"),
                    command::id::CREATE_TASK,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("a"),
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
                // Symbol keys instead of Alt chords: terminal multiplexers
                // (zellij in particular) swallow Alt+hjkl for pane focus, so
                // Alt bindings never reach the app there.
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("["),
                    command::id::TASK_MOVE_UP,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("]"),
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
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("d"),
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
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("l"),
                    command::id::ZOOM_IN,
                ),
                bind(
                    command::Context::Tree,
                    key::KeySeq::chars("h"),
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
                    key::KeySeq::chars("o"),
                    command::id::MANAGE_ADD,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("d"),
                    command::id::MANAGE_DELETE,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("J"),
                    command::id::MANAGE_MOVE_DOWN,
                ),
                bind(
                    command::Context::StatusManage,
                    key::KeySeq::chars("K"),
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

    // Tests the Helix-style zoom keys.
    // Given: the default keymap
    // When: looking up "l" and "h" in the Tree context
    // Then: they resolve to zoom-in and zoom-out respectively
    #[test]
    fn l_and_h_zoom_in_and_out() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("l").as_slice()),
            Lookup::Match(id::ZOOM_IN)
        );
        assert_eq!(
            keymap.lookup(command::Context::Tree, key::KeySeq::chars("h").as_slice()),
            Lookup::Match(id::ZOOM_OUT)
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

    // Tests the symbol structure-editing keys.
    // Given: the default keymap
    // When: looking up [ ] > < in the Tree context
    // Then: they resolve to move-up, move-down, indent and outdent
    #[test]
    fn symbol_keys_bind_structure_editing() {
        let keymap = Keymap::default();
        let cases = [
            ('[', id::TASK_MOVE_UP),
            (']', id::TASK_MOVE_DOWN),
            ('>', id::TASK_INDENT),
            ('<', id::TASK_OUTDENT),
        ];

        for (c, command) in cases {
            assert_eq!(
                keymap.lookup(command::Context::Tree, &[Key::Char(c)]),
                Lookup::Match(command)
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
