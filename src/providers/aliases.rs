//! Shell alias discovery and parsing.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::completion::{InsertionBehavior, Suggestion, SuggestionSource, TextEdit};

use super::ShellKind;

const MAX_ALIAS_FILE_BYTES: u64 = 1024 * 1024;
const MAX_ALIAS_CONFIG_FILES: usize = 1_024;
const MAX_ALIAS_LINES: usize = 32_768;
const MAX_ALIAS_LINE_BYTES: usize = 64 * 1024;
const MAX_ALIASES: usize = 4_096;

/// A simple shell alias with its canonical expansion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Alias {
    /// Token typed at the start of a command.
    pub name: String,
    /// Static alias expansion used for deeper completion lookup.
    pub value: String,
}

/// Parses simple aliases from one shell configuration source.
///
/// Unsupported functions and malformed declarations are ignored. When the same
/// alias is declared repeatedly, the last valid declaration wins.
#[must_use]
pub fn parse_aliases(source: &str, shell: ShellKind) -> Vec<Alias> {
    let mut aliases = BTreeMap::new();
    let mut block_depth = 0_usize;

    for line in source.lines().take(MAX_ALIAS_LINES) {
        if line.len() > MAX_ALIAS_LINE_BYTES {
            continue;
        }
        let Some(tokens) = lex_line(line) else {
            continue;
        };
        let (closes, opens, contains_block_syntax) = block_changes(&tokens, shell);
        block_depth = block_depth.saturating_sub(closes);
        if block_depth == 0 && !contains_block_syntax {
            parse_statements(&tokens, shell, &mut aliases);
        }
        block_depth = block_depth.saturating_add(opens);
    }

    aliases
        .into_iter()
        .map(|(name, value)| Alias { name, value })
        .collect()
}

/// Returns conventional alias-bearing configuration paths for a shell.
///
/// Fish `conf.d` files are returned in lexical order. Missing paths are retained
/// in the result so callers can watch them for later creation.
#[must_use]
pub fn alias_config_paths(
    shell: ShellKind,
    home: &Path,
    zdotdir: Option<&OsStr>,
    xdg_config_home: Option<&OsStr>,
) -> Vec<PathBuf> {
    match shell {
        ShellKind::Bash => vec![home.join(".bashrc"), home.join(".bash_aliases")],
        ShellKind::Zsh => {
            let directory = zdotdir.map_or_else(|| home.to_path_buf(), PathBuf::from);
            vec![directory.join(".zshrc"), directory.join(".zsh_aliases")]
        }
        ShellKind::Fish => {
            let config_home = xdg_config_home.map_or_else(|| home.join(".config"), PathBuf::from);
            let fish_directory = config_home.join("fish");
            let mut paths = fish_conf_d_paths(&fish_directory.join("conf.d"));
            paths.push(fish_directory.join("config.fish"));
            paths
        }
    }
}

/// Loads and merges aliases from configuration files in the supplied order.
///
/// Unreadable, missing, and malformed files are ignored. Later files override
/// aliases declared by earlier files.
#[must_use]
pub fn load_aliases(shell: ShellKind, paths: &[PathBuf]) -> Vec<Alias> {
    let mut aliases = BTreeMap::new();

    for path in paths.iter().take(MAX_ALIAS_CONFIG_FILES) {
        let Ok(file) = fs::File::open(path) else {
            continue;
        };
        let mut source = String::new();
        if file
            .take(MAX_ALIAS_FILE_BYTES + 1)
            .read_to_string(&mut source)
            .is_err()
            || source.len() as u64 > MAX_ALIAS_FILE_BYTES
        {
            continue;
        }
        for alias in parse_aliases(&source, shell) {
            aliases.insert(alias.name, alias.value);
        }
    }

    aliases
        .into_iter()
        .map(|(name, value)| Alias { name, value })
        .collect()
}

/// Creates inert root-command suggestions for matching aliases.
#[must_use]
pub fn alias_suggestions(
    aliases: &[Alias],
    typed_name: &str,
    replacement_range: Range<usize>,
) -> Vec<Suggestion> {
    let folded_prefix = typed_name.to_lowercase();
    let mut matches: Vec<_> = aliases
        .iter()
        .take(MAX_ALIASES)
        .filter(|alias| alias.name.to_lowercase().starts_with(&folded_prefix))
        .collect();
    matches.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });

    matches
        .into_iter()
        .map(|alias| {
            let mut suggestion = Suggestion::new(
                TextEdit {
                    range: replacement_range.clone(),
                    replacement: alias.name.clone(),
                },
                &alias.name,
                format!("alias for {}", alias.value),
                "alias",
                SuggestionSource::Alias,
                InsertionBehavior::AppendSpace,
                format!("alias:{}", alias.name),
            );
            suggestion.static_priority = 0.9;
            suggestion.confidence = 0.95;
            suggestion
        })
        .collect()
}

fn fish_conf_d_paths(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .take(MAX_ALIAS_CONFIG_FILES)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension() == Some(OsStr::new("fish")))
        .collect();
    paths.sort();
    paths
}

fn parse_statements(tokens: &[String], shell: ShellKind, aliases: &mut BTreeMap<String, String>) {
    for statement in tokens.split(|token| token == ";") {
        match statement.first().map(String::as_str) {
            Some("alias") if shell == ShellKind::Fish => {
                parse_fish_alias(statement, aliases);
            }
            Some("alias") => parse_bourne_aliases(statement, aliases),
            Some("abbr") if shell == ShellKind::Fish => {
                parse_fish_abbreviation(statement, aliases);
            }
            _ => {}
        }
    }
}

fn parse_bourne_aliases(statement: &[String], aliases: &mut BTreeMap<String, String>) {
    if statement.iter().skip(1).any(|token| token.starts_with('-')) {
        return;
    }
    for declaration in statement.iter().skip(1) {
        let Some((name, value)) = declaration.split_once('=') else {
            continue;
        };
        insert_alias(aliases, name, value);
    }
}

fn parse_fish_alias(statement: &[String], aliases: &mut BTreeMap<String, String>) {
    if statement.len() < 3 || statement[1].starts_with('-') {
        return;
    }
    insert_alias(aliases, &statement[1], &statement[2..].join(" "));
}

fn parse_fish_abbreviation(statement: &[String], aliases: &mut BTreeMap<String, String>) {
    let Some(add_index) = statement
        .iter()
        .position(|token| token == "-a" || token == "--add")
    else {
        return;
    };
    let mut definition = &statement[add_index + 1..];
    if definition.first().is_some_and(|token| token == "--") {
        definition = &definition[1..];
    }
    if definition.len() < 2 {
        return;
    }
    insert_alias(aliases, &definition[0], &definition[1..].join(" "));
}

fn insert_alias(aliases: &mut BTreeMap<String, String>, name: &str, value: &str) {
    if valid_alias_name(name)
        && !value.is_empty()
        && (aliases.len() < MAX_ALIASES || aliases.contains_key(name))
    {
        aliases.insert(name.to_owned(), value.to_owned());
    }
}

fn block_changes(tokens: &[String], shell: ShellKind) -> (usize, usize, bool) {
    let mut closes = 0;
    let mut opens = 0;

    for statement in tokens.split(|token| token == ";") {
        let Some(first) = statement.first().map(String::as_str) else {
            continue;
        };
        match shell {
            ShellKind::Fish => {
                closes += usize::from(first == "end");
                opens += usize::from(matches!(
                    first,
                    "if" | "for" | "while" | "switch" | "function" | "begin"
                ));
            }
            ShellKind::Bash | ShellKind::Zsh => {
                closes += usize::from(matches!(first, "fi" | "done" | "esac" | "}"));
                let function = first == "function"
                    || first.ends_with("()")
                    || statement.iter().any(|token| token == "{");
                opens += usize::from(
                    function
                        || matches!(first, "if" | "for" | "while" | "until" | "case" | "select"),
                );
            }
        }
    }

    (closes, opens, closes != 0 || opens != 0)
}

fn valid_alias_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || "_+-.".contains(character))
}

fn lex_line(line: &str) -> Option<Vec<String>> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = Quote::None;
    let mut escaped = false;

    for character in line.chars() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }

        match (quote, character) {
            (Quote::None, '#') if token.is_empty() => break,
            (Quote::None, ';') => {
                push_token(&mut tokens, &mut token);
                tokens.push(";".to_owned());
            }
            (Quote::None, character) if character.is_whitespace() => {
                push_token(&mut tokens, &mut token);
            }
            (Quote::None | Quote::Double, '\\') => escaped = true,
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::Single, '\'') | (Quote::Double, '"') => quote = Quote::None,
            (_, character) => token.push(character),
        }
    }

    if escaped || quote != Quote::None {
        return None;
    }
    push_token(&mut tokens, &mut token);
    Some(tokens)
}

fn push_token(tokens: &mut Vec<String>, token: &mut String) {
    if !token.is_empty() {
        tokens.push(std::mem::take(token));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let identifier = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-alias-test-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_multiple_bourne_aliases_and_comments() {
        let source = concat!(
            "alias ll='ls -la' gs=\"git status\" # ignored\n",
            "# alias nope='false'\n",
            "alias hash='printf #value' plain=echo\n",
        );

        assert_eq!(
            parse_aliases(source, ShellKind::Bash),
            vec![
                Alias {
                    name: "gs".into(),
                    value: "git status".into(),
                },
                Alias {
                    name: "hash".into(),
                    value: "printf #value".into(),
                },
                Alias {
                    name: "ll".into(),
                    value: "ls -la".into(),
                },
                Alias {
                    name: "plain".into(),
                    value: "echo".into(),
                },
            ]
        );
    }

    #[test]
    fn parses_fish_aliases_and_abbreviations() {
        let source = concat!(
            "alias ll 'ls -la'\n",
            "abbr -a gs 'git status'\n",
            "abbr --add -- co 'git checkout' # note\n",
            "function dynamic; echo nope; end\n",
        );

        assert_eq!(
            parse_aliases(source, ShellKind::Fish),
            vec![
                Alias {
                    name: "co".into(),
                    value: "git checkout".into(),
                },
                Alias {
                    name: "gs".into(),
                    value: "git status".into(),
                },
                Alias {
                    name: "ll".into(),
                    value: "ls -la".into(),
                },
            ]
        );
    }

    #[test]
    fn ignores_aliases_inside_functions_and_conditionals() {
        let bash = parse_aliases(
            "if false; then\nalias hidden='echo no'\nfi\nalias visible='echo yes'\n\
             helper() {\nalias also_hidden='echo no'\n}\n",
            ShellKind::Bash,
        );
        assert_eq!(
            bash,
            vec![Alias {
                name: "visible".into(),
                value: "echo yes".into(),
            }]
        );

        let fish = parse_aliases(
            "function helper\nalias hidden 'echo no'\nend\nalias visible 'echo yes'\n",
            ShellKind::Fish,
        );
        assert_eq!(fish[0].name, "visible");
    }

    #[test]
    fn rejects_option_bearing_bourne_alias_declarations() {
        assert!(parse_aliases("alias -g G='| grep'", ShellKind::Zsh).is_empty());
    }

    #[test]
    fn honors_shell_specific_configuration_roots() {
        let temp = TempDirectory::new();
        let zdotdir = temp.0.join("zsh");
        let xdg = temp.0.join("xdg");
        fs::create_dir_all(xdg.join("fish/conf.d")).unwrap();
        fs::write(xdg.join("fish/conf.d/20-second.fish"), "").unwrap();
        fs::write(xdg.join("fish/conf.d/10-first.fish"), "").unwrap();

        assert_eq!(
            alias_config_paths(ShellKind::Zsh, &temp.0, Some(zdotdir.as_os_str()), None,),
            vec![zdotdir.join(".zshrc"), zdotdir.join(".zsh_aliases")]
        );
        assert_eq!(
            alias_config_paths(ShellKind::Fish, &temp.0, None, Some(xdg.as_os_str()),),
            vec![
                xdg.join("fish/conf.d/10-first.fish"),
                xdg.join("fish/conf.d/20-second.fish"),
                xdg.join("fish/config.fish"),
            ]
        );
    }

    #[test]
    fn later_files_override_and_unreadable_files_are_ignored() {
        let temp = TempDirectory::new();
        let first = temp.0.join("first");
        let second = temp.0.join("second");
        fs::write(&first, "alias ll='ls -l'\n").unwrap();
        fs::write(&second, "alias ll='ls -la'\n").unwrap();

        assert_eq!(
            load_aliases(ShellKind::Zsh, &[temp.0.join("missing"), first, second],),
            vec![Alias {
                name: "ll".into(),
                value: "ls -la".into(),
            }]
        );
    }

    #[test]
    fn creates_deterministic_inert_alias_suggestions() {
        let aliases = vec![
            Alias {
                name: "GS".into(),
                value: "git status --short".into(),
            },
            Alias {
                name: "gc".into(),
                value: "git commit".into(),
            },
        ];

        let suggestions = alias_suggestions(&aliases, "g", 0..1);
        assert_eq!(suggestions[0].display, "gc");
        assert_eq!(suggestions[1].display, "GS");
        assert_eq!(suggestions[0].source, SuggestionSource::Alias);
        assert_eq!(suggestions[0].edit.replacement, "gc");
        assert_eq!(suggestions[0].insertion, InsertionBehavior::AppendSpace);
    }
}
