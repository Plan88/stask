# stask

A keyboard-driven TUI task manager, built in Rust.

Tasks form a tree of unlimited depth. Each task carries a title, a due date, a
user-defined status, and a free-form Markdown note. Long-form editing is
delegated to your `$EDITOR`, so you keep your own editor, config and muscle
memory instead of learning a built-in one.

![The task tree: nested tasks with statuses and due dates, the selected task's note below, and the key hints in the footer](https://raw.githubusercontent.com/Plan88/stask/main/img/tree.png)

Incremental search shows matches as a flat list with each task's parent path:

![The search view: results as a flat list, each with its parent path](https://raw.githubusercontent.com/Plan88/stask/main/img/search.png)

Statuses are yours to define — label, kind, color and selection key, edited in-app:

![The status management view: a table of label, kind, color, key and default](https://raw.githubusercontent.com/Plan88/stask/main/img/status.png)

## Features

- **Task tree with unlimited depth** — subtasks all the way down. Zoom into
  any task to make it the view root (its ancestors collapse into a breadcrumb
  header), so deep hierarchies never squeeze the screen.
- **Custom statuses** — statuses live in the database, not in code. Edit them
  in an in-app management modal: label, kind (`open` / `done` / `cancelled`),
  color, selection key, order, and which one new tasks get.
- **Markdown notes via `$EDITOR`** — each task has one free-form note. Press
  `e` and the note opens in your editor (`vi` if `$EDITOR` is unset); saving
  and quitting lands the whole edit as a single undo step.
- **Search, filter, sort** — incremental full-text search over titles and
  notes. Results are a flat list with each task's parent path. Filter by
  status or overdue, sort by due date, update time, creation time, or title.
  The filter is shared with the tree view.
- **Undo / redo** — every data change is undoable (`u` / `U`), implemented as
  a trigger-based SQLite undo log, so nothing is missed. Undo always reveals
  the affected task — expanding ancestors and dropping the zoom if needed —
  and says what it undid.
- **Discoverable keybindings** — a one-line footer shows the keys for the
  current context, and `?` opens a searchable list of every binding. Both are
  generated from the single keymap definition, so they can never drift from
  the actual bindings — including your overrides.
- **Configurable keymap** — every binding can be replaced per context in a
  TOML config file, referenced by the command ids shown in the `?` help.
- **Single binary, local data** — a SQLite file under your XDG data
  directory. No server, no sync, no accounts.

## Installation

Requires a recent stable Rust toolchain (Rust 1.85+, edition 2024).

```sh
cargo install stask
```

Or from source:

```sh
git clone https://github.com/Plan88/stask.git
cd stask
cargo install --path tui
```

Either way this installs the `stask` binary.

## Usage

```sh
stask
```

Options:

| Flag | Meaning |
|---|---|
| `--config <path>` | Use this config file instead of the default location |
| `--db <path>` | Use this database file (overrides the config) |

On first run, stask writes a commented default config and creates the
database, so a fresh machine just works.

### Default keys (tree view)

Press `?` inside the app for the complete, always-accurate list. The
essentials:

| Key | Action |
|---|---|
| `j` / `k` (or `↓` / `↑`) | Move down / up |
| `gg` / `ge` | First / last row |
| `Ctrl-d` / `Ctrl-u` | Half page down / up |
| `Tab` | Expand / collapse |
| `l` / `h` | Zoom in / zoom out |
| `n` / `N` | New sibling task / new subtask |
| `r` | Rename |
| `s` | Set status (one key per status) |
| `J` / `K` | Cycle status forward / backward |
| `t` | Set due date (`YYYY-MM-DD`) |
| `e` | Edit note in `$EDITOR` |
| `Ctrl-j` / `Ctrl-k` | Move task down / up among siblings |
| `>` / `<` | Indent / outdent |
| `D` | Delete (childless tasks delete instantly — undo covers mistakes; subtrees ask for confirmation with a count) |
| `u` / `U` | Undo / redo |
| `/` | Search |
| `f` | Filter (all / open / per-status / overdue) |
| `S` | Manage statuses |
| `\` | Toggle the footer hints |
| `?` | Help |
| `q` | Quit |

In the search view: type to search incrementally, `Enter` on a result jumps
to it in the tree, `,` picks the sort order, `f` the filter.

Text inputs (titles, due dates, search text) support the readline-style
keys: `Ctrl-a` / `Ctrl-e` jump to the start / end, `Ctrl-k` / `Ctrl-u`
delete to the end / start, and `Ctrl-w` deletes the previous word.

## Configuration

Config file: `$XDG_CONFIG_HOME/stask/config.toml`
(`~/.config/stask/config.toml` when `$XDG_CONFIG_HOME` is unset).

Database: `$XDG_DATA_HOME/stask/tasks.db`
(`~/.local/share/stask/tasks.db` when `$XDG_DATA_HOME` is unset), overridable
with `db_path` in the config or the `--db` flag.

```toml
# Show the one-line key hints at the bottom (toggled at runtime with \).
footer = true

# Where the task database file lives. A leading ~ is expanded.
# db_path = "~/Dropbox/stask/tasks.db"

# Key overrides. One table per context (tree, query, help, input,
# status_select, status_manage, filter_select, sort_select); keys are
# command ids as shown in the ? help, values are a key sequence or a
# list of key sequences, replacing that command's default keys.
# Plain characters concatenate ("gg"); special keys are written <tab>
# <enter> <esc> <backspace> <left> <right> <up> <down> <alt-x> <ctrl-x>;
# a literal < is <lt>.
[keymap.tree]
"task.delete" = "x"
"tree.select_first" = ["gg", "<alt-g>"]
```

The note editor is taken from `$EDITOR` (arguments are supported, e.g.
`EDITOR="code --wait"`) and falls back to `vi`.

## Design notes

- **Workspace layout** — `stask-engine` (in `engine/`) holds the domain
  model, persistence and search (plain Rust, no UI dependency); `stask` (in
  `tui/`) is the ratatui front end and owns the binary. Tests concentrate in
  the engine.
- **Undo** — implemented with SQLite triggers on `tasks` / `statuses` that
  record inverse statements into a TEMP undo log (the pattern from sqlite.org).
  New operations are covered automatically, and TEMP objects make the history
  session-scoped by construction. The status-management modal forms an undo
  sub-session: fine-grained undo inside, folded into one atomic step when it
  closes.
- **Search** — a `LIKE` scan behind a `Query` abstraction. At personal scale
  this is milliseconds; if it ever gets slow, the implementation can swap to
  FTS5 trigram without touching the UI.
- **View state stays in memory** — expansion, zoom, cursor and filter are
  never persisted and never enter the undo history; undo touches data only.

## Development

```sh
cargo test                     # run all tests
cargo clippy -- -D warnings    # lint
cargo fmt                      # format
```

## License

[MIT](LICENSE)
