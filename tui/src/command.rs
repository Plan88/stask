/// Stable identifier for a command; referenced by the keymap and, later, by
/// user configuration files.
pub type CommandId = &'static str;

/// Which part of the UI currently receives keys. Bindings and footer hints
/// are resolved per context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    Tree,
    Input,
    StatusSelect,
    StatusManage,
    /// The delete-confirmation prompt. It consumes keys directly (y or
    /// anything else), so no commands are bound here; the variant exists so
    /// the footer shows no misleading tree hints while it is open.
    ConfirmDelete,
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
    pub const CREATE_CHILD: CommandId = "task.create_child";
    pub const RENAME_TASK: CommandId = "task.rename";
    pub const SET_STATUS: CommandId = "task.set_status";
    pub const STATUS_NEXT: CommandId = "task.status_next";
    pub const STATUS_PREV: CommandId = "task.status_prev";
    pub const STATUS_CANCEL: CommandId = "status.cancel";
    pub const STATUS_MANAGE: CommandId = "status.manage";
    pub const MANAGE_ROW_NEXT: CommandId = "statuses.row_next";
    pub const MANAGE_ROW_PREV: CommandId = "statuses.row_prev";
    pub const MANAGE_COL_PREV: CommandId = "statuses.col_prev";
    pub const MANAGE_COL_NEXT: CommandId = "statuses.col_next";
    pub const MANAGE_EDIT: CommandId = "statuses.edit_cell";
    pub const MANAGE_ADD: CommandId = "statuses.add";
    pub const MANAGE_DELETE: CommandId = "statuses.delete";
    pub const MANAGE_MOVE_DOWN: CommandId = "statuses.move_down";
    pub const MANAGE_MOVE_UP: CommandId = "statuses.move_up";
    pub const MANAGE_SET_DEFAULT: CommandId = "statuses.set_default";
    pub const MANAGE_CLOSE: CommandId = "statuses.close";
    pub const TASK_MOVE_UP: CommandId = "task.move_up";
    pub const TASK_MOVE_DOWN: CommandId = "task.move_down";
    pub const TASK_INDENT: CommandId = "task.indent";
    pub const TASK_OUTDENT: CommandId = "task.outdent";
    pub const TASK_DELETE: CommandId = "task.delete";
    pub const TOGGLE_EXPAND: CommandId = "task.toggle_expand";
    pub const ZOOM_IN: CommandId = "view.zoom_in";
    pub const ZOOM_OUT: CommandId = "view.zoom_out";
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
        id: id::TOGGLE_EXPAND,
        label: "expand",
        context: Context::Tree,
        hint_priority: 85,
    },
    Command {
        id: id::CREATE_TASK,
        label: "new task",
        context: Context::Tree,
        hint_priority: 80,
    },
    Command {
        id: id::CREATE_CHILD,
        label: "sub task",
        context: Context::Tree,
        hint_priority: 75,
    },
    Command {
        id: id::RENAME_TASK,
        label: "rename",
        context: Context::Tree,
        hint_priority: 72,
    },
    Command {
        id: id::SET_STATUS,
        label: "status",
        context: Context::Tree,
        hint_priority: 71,
    },
    // Hidden from the footer (priority 0): power-user shortcuts that would
    // crowd out the discoverable commands; the help list still shows them.
    Command {
        id: id::STATUS_NEXT,
        label: "status next",
        context: Context::Tree,
        hint_priority: 0,
    },
    Command {
        id: id::STATUS_PREV,
        label: "status prev",
        context: Context::Tree,
        hint_priority: 0,
    },
    // Hidden from the footer: structure editing is a power-user gesture and
    // four Alt chords would crowd out the discoverable commands.
    Command {
        id: id::TASK_MOVE_UP,
        label: "move up",
        context: Context::Tree,
        hint_priority: 0,
    },
    Command {
        id: id::TASK_MOVE_DOWN,
        label: "move down",
        context: Context::Tree,
        hint_priority: 0,
    },
    Command {
        id: id::TASK_INDENT,
        label: "indent",
        context: Context::Tree,
        hint_priority: 0,
    },
    Command {
        id: id::TASK_OUTDENT,
        label: "outdent",
        context: Context::Tree,
        hint_priority: 0,
    },
    Command {
        id: id::TASK_DELETE,
        label: "delete",
        context: Context::Tree,
        hint_priority: 18,
    },
    Command {
        id: id::ZOOM_IN,
        label: "zoom in",
        context: Context::Tree,
        hint_priority: 70,
    },
    Command {
        id: id::ZOOM_OUT,
        label: "zoom out",
        context: Context::Tree,
        hint_priority: 65,
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
    // The status keys themselves come from the status definitions in the
    // database, not the keymap; the footer for this context shows the
    // generated candidate list, so only the cancel key lives here.
    Command {
        id: id::STATUS_CANCEL,
        label: "cancel",
        context: Context::StatusSelect,
        hint_priority: 100,
    },
    Command {
        id: id::STATUS_MANAGE,
        label: "statuses",
        context: Context::Tree,
        hint_priority: 15,
    },
    Command {
        id: id::MANAGE_EDIT,
        label: "edit",
        context: Context::StatusManage,
        hint_priority: 100,
    },
    Command {
        id: id::MANAGE_ADD,
        label: "add",
        context: Context::StatusManage,
        hint_priority: 90,
    },
    Command {
        id: id::MANAGE_DELETE,
        label: "delete",
        context: Context::StatusManage,
        hint_priority: 85,
    },
    Command {
        id: id::MANAGE_SET_DEFAULT,
        label: "default",
        context: Context::StatusManage,
        hint_priority: 80,
    },
    Command {
        id: id::MANAGE_MOVE_DOWN,
        label: "move down",
        context: Context::StatusManage,
        hint_priority: 50,
    },
    Command {
        id: id::MANAGE_MOVE_UP,
        label: "move up",
        context: Context::StatusManage,
        hint_priority: 45,
    },
    Command {
        id: id::MANAGE_CLOSE,
        label: "close",
        context: Context::StatusManage,
        hint_priority: 40,
    },
    // Hidden from the footer: plain cursor movement that every other screen
    // already teaches.
    Command {
        id: id::MANAGE_ROW_NEXT,
        label: "down",
        context: Context::StatusManage,
        hint_priority: 0,
    },
    Command {
        id: id::MANAGE_ROW_PREV,
        label: "up",
        context: Context::StatusManage,
        hint_priority: 0,
    },
    Command {
        id: id::MANAGE_COL_PREV,
        label: "left",
        context: Context::StatusManage,
        hint_priority: 0,
    },
    Command {
        id: id::MANAGE_COL_NEXT,
        label: "right",
        context: Context::StatusManage,
        hint_priority: 0,
    },
];
