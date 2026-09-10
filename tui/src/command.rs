/// Stable identifier for a command; referenced by the keymap and, later, by
/// user configuration files.
pub type CommandId = &'static str;

/// Which part of the UI currently receives keys. Bindings and footer hints
/// are resolved per context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    Tree,
    Input,
}

/// A user-invocable operation. Every key-hint display (footer, help) is
/// generated from this table plus the keymap, so what is shown can never
/// drift from what the keys actually do.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    pub id: CommandId,
    pub label: &'static str,
    pub context: Context,
    /// Footer display priority: higher shows first, 0 hides the command
    /// from the footer.
    pub hint_priority: u8,
}

pub mod id {
    use super::CommandId;

    pub const QUIT: CommandId = "app.quit";
    pub const SELECT_NEXT: CommandId = "tree.select_next";
    pub const SELECT_PREV: CommandId = "tree.select_prev";
    pub const SELECT_FIRST: CommandId = "tree.select_first";
    pub const SELECT_LAST: CommandId = "tree.select_last";
    pub const CREATE_TASK: CommandId = "task.create";
    pub const INPUT_CONFIRM: CommandId = "input.confirm";
    pub const INPUT_CANCEL: CommandId = "input.cancel";
}

pub const COMMANDS: &[Command] = &[
    Command {
        id: id::SELECT_NEXT,
        label: "down",
        context: Context::Tree,
        hint_priority: 100,
    },
    Command {
        id: id::SELECT_PREV,
        label: "up",
        context: Context::Tree,
        hint_priority: 90,
    },
    Command {
        id: id::CREATE_TASK,
        label: "new task",
        context: Context::Tree,
        hint_priority: 80,
    },
    Command {
        id: id::SELECT_FIRST,
        label: "first",
        context: Context::Tree,
        hint_priority: 40,
    },
    Command {
        id: id::SELECT_LAST,
        label: "last",
        context: Context::Tree,
        hint_priority: 30,
    },
    Command {
        id: id::QUIT,
        label: "quit",
        context: Context::Tree,
        hint_priority: 20,
    },
    Command {
        id: id::INPUT_CONFIRM,
        label: "confirm",
        context: Context::Input,
        hint_priority: 100,
    },
    Command {
        id: id::INPUT_CANCEL,
        label: "cancel",
        context: Context::Input,
        hint_priority: 90,
    },
];
