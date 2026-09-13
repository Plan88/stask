//! The user configuration file: footer visibility and keymap overrides.
//! An override replaces the command's default bindings, so the footer and
//! the help list follow it automatically (they render from the keymap).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::command;
use crate::key::KeySeq;
use crate::keymap::Keymap;
use crate::keyspec;

/// Written on first start so the file documents itself.
pub const DEFAULT_CONFIG: &str = r#"# stask configuration.

# Show the one-line key hints at the bottom (toggled at runtime with \).
footer = true

# Key overrides. One table per context (tree, query, help, input,
# status_select, status_manage, filter_select, sort_select); keys are
# command ids as shown in the ? help, values are a key sequence or a
# list of key sequences, replacing that command's default keys.
# Plain characters concatenate ("gg"); special keys are written <tab>
# <enter> <esc> <backspace> <left> <right> <alt-x>; a literal < is <lt>.
#
# [keymap.tree]
# "task.delete" = "D"
# "tree.select_first" = ["gg", "<alt-g>"]
"#;

/// Commands whose keys the active mode consumes directly (text input,
/// one-key menus). Rebinding them would change the displays but not the
/// behaviour, so the config refuses them instead of lying.
const NOT_OVERRIDABLE: &[command::CommandId] = &[
    command::id::INPUT_CONFIRM,
    command::id::INPUT_CANCEL,
    command::id::STATUS_CANCEL,
    command::id::FILTER_CANCEL,
    command::id::SORT_CANCEL,
];

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to read config at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write default config at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config syntax")]
    Parse(#[from] toml::de::Error),
    #[error(
        "unknown keymap context `{0}` (valid: tree, query, help, input, \
         status_select, status_manage, filter_select, sort_select)"
    )]
    UnknownContext(String),
    #[error("no command `{id}` in context `{context}` (see the ? help for ids)")]
    UnknownCommand { context: String, id: String },
    #[error("`{0}` cannot be rebound: its keys are handled by the mode itself, not the keymap")]
    NotOverridable(String),
    #[error("invalid key sequence `{spec}` for `{id}`")]
    InvalidKeySpec {
        id: String,
        spec: String,
        #[source]
        source: keyspec::ParseError,
    },
    #[error(
        "duplicate binding in context `{context}`: `{keys}` is bound to both `{first}` and `{second}`"
    )]
    DuplicateBinding {
        context: String,
        keys: String,
        first: command::CommandId,
        second: command::CommandId,
    },
    #[error("cannot locate the config directory: neither $XDG_CONFIG_HOME nor $HOME is set")]
    HomeNotSet,
}

/// The validated configuration: the flag and the fully merged keymap.
#[derive(Debug)]
pub struct Config {
    pub footer: bool,
    pub keymap: Keymap,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default = "default_footer")]
    footer: bool,
    #[serde(default)]
    keymap: BTreeMap<String, BTreeMap<String, KeySpecs>>,
}

fn default_footer() -> bool {
    true
}

/// A single key sequence or several alternatives for one command.
#[derive(Deserialize)]
#[serde(untagged)]
enum KeySpecs {
    One(String),
    Many(Vec<String>),
}

impl KeySpecs {
    fn specs(&self) -> &[String] {
        match self {
            Self::One(spec) => std::slice::from_ref(spec),
            Self::Many(specs) => specs,
        }
    }
}

/// Parses and validates config text into a merged keymap.
pub fn parse(text: &str) -> Result<Config, Error> {
    let file: ConfigFile = toml::from_str(text)?;
    let mut keymap = Keymap::default();
    for (context_name, overrides) in &file.keymap {
        let context = command::context_from_name(context_name)
            .ok_or_else(|| Error::UnknownContext(context_name.clone()))?;
        for (id, specs) in overrides {
            let cmd = command::COMMANDS
                .iter()
                .find(|cmd| cmd.context == context && cmd.id == id)
                .ok_or_else(|| Error::UnknownCommand {
                    context: context_name.clone(),
                    id: id.clone(),
                })?;
            if NOT_OVERRIDABLE.contains(&cmd.id) {
                return Err(Error::NotOverridable(id.clone()));
            }
            let seqs = specs
                .specs()
                .iter()
                .map(|spec| {
                    keyspec::parse_seq(spec).map_err(|source| Error::InvalidKeySpec {
                        id: id.clone(),
                        spec: spec.clone(),
                        source,
                    })
                })
                .collect::<Result<Vec<KeySeq>, Error>>()?;
            keymap.rebind(context, cmd.id, seqs);
        }
    }
    // Checked on the merged map so an override clashing with an untouched
    // default is caught too.
    if let Some((context, seq, first, second)) = keymap.duplicate() {
        return Err(Error::DuplicateBinding {
            context: command::CONTEXT_NAMES
                .iter()
                .find(|(c, _)| *c == context)
                .map(|(_, name)| (*name).to_string())
                .unwrap_or_default(),
            keys: keyspec::format_seq(seq),
            first,
            second,
        });
    }
    Ok(Config {
        footer: file.footer,
        keymap,
    })
}

/// Loads the config at `path`, writing (and then reading) the commented
/// default first when the file does not exist yet.
pub fn load_or_init(path: &Path) -> Result<Config, Error> {
    if !path.exists() {
        let write_err = |source| Error::Write {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(write_err)?;
        }
        std::fs::write(path, DEFAULT_CONFIG).map_err(write_err)?;
    }
    let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}

/// XDG-style path resolution, injectable for tests: `$XDG_CONFIG_HOME`
/// wins, then `~/.config`; macOS follows the same rule deliberately.
pub fn config_path_from(
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf, Error> {
    let base = match (xdg_config_home, home) {
        (Some(xdg), _) => xdg.to_path_buf(),
        (None, Some(home)) => home.join(".config"),
        (None, None) => return Err(Error::HomeNotSet),
    };
    Ok(base.join("stask").join("config.toml"))
}

/// Resolves the real config path from the environment.
pub fn default_path() -> Result<PathBuf, Error> {
    config_path_from(
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Context, id};
    use crate::key::Key;
    use crate::keymap::Lookup;

    // Tests that the shipped default config is valid and changes nothing.
    // Given: the commented default config text
    // When: it is parsed
    // Then: the footer is on and the keymap matches the built-in default
    //       (spot-checked on a binding)
    #[test]
    fn default_config_parses_to_defaults() {
        let config = parse(DEFAULT_CONFIG).expect("default config must parse");

        assert!(config.footer);
        assert_eq!(
            config
                .keymap
                .lookup(Context::Tree, KeySeq::chars("d").as_slice()),
            Lookup::Match(id::TASK_DELETE)
        );
    }

    // Tests the footer flag.
    // Given: a config with footer = false
    // When: it is parsed
    // Then: the flag comes through
    #[test]
    fn footer_flag_is_read() {
        let config = parse("footer = false\n").unwrap();

        assert!(!config.footer);
    }

    // Tests a single-string keymap override.
    // Given: a config rebinding task.delete to "D" in the tree context
    // When: it is parsed
    // Then: "D" fires delete and the default "d" no longer resolves
    //       (an override replaces the default bindings)
    #[test]
    fn override_replaces_default_binding() {
        let config = parse("[keymap.tree]\n\"task.delete\" = \"D\"\n").unwrap();

        assert_eq!(
            config
                .keymap
                .lookup(Context::Tree, KeySeq::chars("D").as_slice()),
            Lookup::Match(id::TASK_DELETE)
        );
        assert_eq!(
            config
                .keymap
                .lookup(Context::Tree, KeySeq::chars("d").as_slice()),
            Lookup::Miss
        );
    }

    // Tests an array override with special-key notation.
    // Given: a config giving task.toggle_expand both <tab> and "z"
    // When: it is parsed
    // Then: both sequences fire the command
    #[test]
    fn override_accepts_multiple_sequences_and_notation() {
        let config = parse("[keymap.tree]\n\"task.toggle_expand\" = [\"<tab>\", \"z\"]\n").unwrap();

        assert_eq!(
            config.keymap.lookup(Context::Tree, &[Key::Tab]),
            Lookup::Match(id::TOGGLE_EXPAND)
        );
        assert_eq!(
            config.keymap.lookup(Context::Tree, &[Key::Char('z')]),
            Lookup::Match(id::TOGGLE_EXPAND)
        );
    }

    // Tests rejection of an unknown context table.
    // Given: a config with [keymap.treee] (typo)
    // When: it is parsed
    // Then: it fails with UnknownContext naming the typo
    #[test]
    fn unknown_context_is_rejected() {
        let err = parse("[keymap.treee]\n\"task.delete\" = \"D\"\n").unwrap_err();

        assert!(matches!(err, Error::UnknownContext(name) if name == "treee"));
    }

    // Tests rejection of an unknown command id, including an id that
    // exists but in a different context.
    // Given: configs binding a nonexistent id and a query-context id
    //        under [keymap.tree]
    // When: they are parsed
    // Then: both fail with UnknownCommand naming context and id
    #[test]
    fn unknown_command_is_rejected() {
        let err = parse("[keymap.tree]\n\"task.explode\" = \"D\"\n").unwrap_err();
        assert!(
            matches!(&err, Error::UnknownCommand { context, id } if context == "tree" && id == "task.explode")
        );

        let err = parse("[keymap.tree]\n\"query.jump\" = \"D\"\n").unwrap_err();
        assert!(matches!(&err, Error::UnknownCommand { id, .. } if id == "query.jump"));
    }

    // Tests rejection of a malformed key sequence.
    // Given: a config binding task.delete to the unclosed spec "<del"
    // When: it is parsed
    // Then: it fails with InvalidKeySpec carrying the parse error as its
    //       source, so the cause chain explains the notation problem
    #[test]
    fn invalid_key_sequence_is_rejected() {
        let err = parse("[keymap.tree]\n\"task.delete\" = \"<del\"\n").unwrap_err();

        let Error::InvalidKeySpec { id, spec, source } = err else {
            panic!("expected InvalidKeySpec, got {err:?}");
        };
        assert_eq!(id, "task.delete");
        assert_eq!(spec, "<del");
        assert_eq!(
            source,
            keyspec::ParseError::UnclosedAngle("<del".to_string())
        );
    }

    // Tests rejection of two commands sharing one sequence in a context.
    // Given: a config rebinding task.delete to "u", which app.undo
    //        already uses in the tree context
    // When: it is parsed
    // Then: it fails with DuplicateBinding naming the sequence and both
    //       command ids
    #[test]
    fn duplicate_sequence_in_context_is_rejected() {
        let err = parse("[keymap.tree]\n\"task.delete\" = \"u\"\n").unwrap_err();

        let Error::DuplicateBinding {
            context,
            keys,
            first,
            second,
        } = err
        else {
            panic!("expected DuplicateBinding, got {err:?}");
        };
        assert_eq!(context, "tree");
        assert_eq!(keys, "u");
        let mut pair = [first, second];
        pair.sort_unstable();
        assert_eq!(pair, [id::UNDO, id::TASK_DELETE]);
    }

    // Tests rejection of mode-consumed command ids.
    // Given: configs rebinding input.confirm and status.cancel, whose
    //        keys the modes consume directly (the binding only feeds the
    //        hint displays)
    // When: they are parsed
    // Then: both fail with NotOverridable instead of silently showing
    //       keys that would not work
    #[test]
    fn mode_consumed_commands_cannot_be_rebound() {
        let err = parse("[keymap.input]\n\"input.confirm\" = \"<tab>\"\n").unwrap_err();
        assert!(matches!(&err, Error::NotOverridable(id) if id == "input.confirm"));

        let err = parse("[keymap.status_select]\n\"status.cancel\" = \"q\"\n").unwrap_err();
        assert!(matches!(&err, Error::NotOverridable(id) if id == "status.cancel"));
    }

    // Tests that config syntax errors surface as Parse errors.
    // Given: a config with broken TOML and one with an unknown top-level
    //        field (a likely typo)
    // When: they are parsed
    // Then: both fail with Parse
    #[test]
    fn broken_toml_is_rejected() {
        assert!(matches!(parse("footer = \n").unwrap_err(), Error::Parse(_)));
        assert!(matches!(
            parse("foter = true\n").unwrap_err(),
            Error::Parse(_)
        ));
    }

    // Tests first-run initialisation and the following load.
    // Given: a config path in a directory that does not exist yet
    // When: load_or_init runs twice, with an edit in between
    // Then: the first run writes the commented default and yields the
    //       defaults; the second run reads the edited file
    #[test]
    fn load_or_init_writes_default_then_reads_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");

        let config = load_or_init(&path).unwrap();
        assert!(config.footer);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);

        std::fs::write(&path, "footer = false\n").unwrap();
        let config = load_or_init(&path).unwrap();
        assert!(!config.footer);
    }

    // Tests the XDG config path resolution.
    // Given: both, only $HOME, and neither of the path variables
    // When: the config path is resolved
    // Then: $XDG_CONFIG_HOME wins, $HOME falls back to ~/.config, and
    //       nothing set is an error
    #[test]
    fn config_path_prefers_xdg_then_home() {
        assert_eq!(
            config_path_from(Some(Path::new("/xdg")), Some(Path::new("/home/u"))).unwrap(),
            PathBuf::from("/xdg/stask/config.toml")
        );
        assert_eq!(
            config_path_from(None, Some(Path::new("/home/u"))).unwrap(),
            PathBuf::from("/home/u/.config/stask/config.toml")
        );
        assert!(matches!(
            config_path_from(None, None).unwrap_err(),
            Error::HomeNotSet
        ));
    }
}
