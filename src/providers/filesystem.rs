//! Bounded filesystem completion.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::completion::{
    CompletionQuery, InsertionBehavior, Suggestion, SuggestionSource, TextEdit, tokenize,
};

use super::ShellKind;

const MAX_DIRECTORY_ENTRIES: usize = 8_192;
const MAX_FILESYSTEM_CANDIDATES: usize = 2_048;

/// Filters and insertion metadata for one filesystem lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemOptions {
    /// Include entries whose final component starts with a dot.
    pub include_hidden: bool,
    /// Return directories only.
    pub directory_only: bool,
    /// Allowed file extensions, with or without a leading dot.
    ///
    /// Directories remain eligible so traversal can continue.
    pub extensions: Vec<String>,
    /// Home directory used to resolve `~/` fragments.
    pub home_directory: Option<PathBuf>,
    /// Insertion behavior for files accepted by this generator.
    pub file_insertion: InsertionBehavior,
}

impl Default for FilesystemOptions {
    fn default() -> Self {
        Self {
            include_hidden: false,
            directory_only: false,
            extensions: Vec::new(),
            home_directory: None,
            file_insertion: InsertionBehavior::AppendSpace,
        }
    }
}

/// Completes the active shell token from one bounded directory read.
///
/// The query working directory is authoritative. Missing, unreadable, and
/// concurrently disappearing entries are ignored.
#[must_use]
pub fn filesystem_suggestions(
    query: &CompletionQuery,
    shell: ShellKind,
    options: &FilesystemOptions,
) -> Vec<Suggestion> {
    filesystem_suggestions_bounded(query, shell, options, MAX_DIRECTORY_ENTRIES, || false)
}

/// Completes one token with an explicit scan bound and cooperative stop check.
///
/// A stopped scan returns no partial results. No worker is spawned, so a
/// cancelled or expired lookup cannot leave filesystem work running.
pub(super) fn filesystem_suggestions_bounded(
    query: &CompletionQuery,
    shell: ShellKind,
    options: &FilesystemOptions,
    maximum_entries: usize,
    mut should_stop: impl FnMut() -> bool,
) -> Vec<Suggestion> {
    if should_stop() {
        return Vec::new();
    }
    let Ok(tokenized) = tokenize(&query.line, query.cursor) else {
        return Vec::new();
    };
    let token = tokenized.active_token();
    let fragment = &token.cooked;
    let Some((directory, displayed_parent, needle)) =
        resolve_fragment(fragment, &query.cwd, options.home_directory.as_deref())
    else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        return Vec::new();
    };
    if should_stop() {
        return Vec::new();
    }

    let allowed_extensions: Vec<_> = options
        .extensions
        .iter()
        .map(|extension| extension.strip_prefix('.').unwrap_or(extension))
        .collect();
    let mut candidates = Vec::new();

    for entry in entries.take(maximum_entries.min(MAX_DIRECTORY_ENTRIES)) {
        if should_stop() {
            return Vec::new();
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.chars().any(char::is_control)
            || (!options.include_hidden && name.starts_with('.'))
            || !name.starts_with(needle)
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if should_stop() {
            return Vec::new();
        }
        let is_directory = metadata.is_dir();
        if options.directory_only && !is_directory {
            continue;
        }
        if !is_directory
            && !allowed_extensions.is_empty()
            && !extension_allowed(&entry.path(), &allowed_extensions)
        {
            continue;
        }

        let displayed_path = format!("{displayed_parent}{name}");
        candidates.push((displayed_path, entry.path(), is_directory));
        if candidates.len() >= MAX_FILESYSTEM_CANDIDATES {
            break;
        }
    }

    if should_stop() {
        return Vec::new();
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    let mut suggestions = Vec::with_capacity(candidates.len());
    for (displayed_path, resolved_path, is_directory) in candidates {
        if should_stop() {
            return Vec::new();
        }
        suggestions.push(filesystem_suggestion(
            shell,
            options,
            token.raw.clone(),
            &displayed_path,
            &resolved_path,
            is_directory,
        ));
    }
    if should_stop() {
        Vec::new()
    } else {
        suggestions
    }
}

fn filesystem_suggestion(
    shell: ShellKind,
    options: &FilesystemOptions,
    replacement: std::ops::Range<usize>,
    displayed_path: &str,
    resolved_path: &Path,
    is_directory: bool,
) -> Suggestion {
    let insertion = if is_directory {
        InsertionBehavior::Directory
    } else {
        options.file_insertion
    };
    let description = if is_directory {
        "directory".to_owned()
    } else {
        file_description(resolved_path)
    };
    let icon = if is_directory { "directory" } else { "file" };
    let mut suggestion = Suggestion::new(
        TextEdit {
            range: replacement,
            replacement: quote_path(shell, displayed_path),
        },
        displayed_path,
        description,
        icon,
        SuggestionSource::File,
        insertion,
        format!("file:{}", resolved_path.to_string_lossy()),
    );
    suggestion.static_priority = 0.45;
    suggestion.confidence = 0.85;
    suggestion
}

/// Quotes or escapes a logical path as one token for the selected shell.
#[must_use]
pub fn quote_path(shell: ShellKind, value: &str) -> String {
    if path_is_shell_safe(value) {
        return value.to_owned();
    }

    match shell {
        ShellKind::Bash | ShellKind::Zsh => quote_bourne_path(value),
        ShellKind::Fish => quote_fish_path(value),
    }
}

fn resolve_fragment<'a>(
    fragment: &'a str,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<(PathBuf, &'a str, &'a str)> {
    let split = fragment.rfind('/').map_or(0, |index| index + 1);
    let (displayed_parent, needle) = fragment.split_at(split);

    let directory = if let Some(home_relative) = displayed_parent.strip_prefix("~/") {
        home?.join(home_relative)
    } else if displayed_parent.starts_with('/') {
        PathBuf::from(displayed_parent)
    } else {
        cwd.join(displayed_parent)
    };

    Some((directory, displayed_parent, needle))
}

fn extension_allowed(path: &Path, allowed_extensions: &[&str]) -> bool {
    // Filters are collected case-insensitively, so matching them exactly would
    // drop a file whose extension differs only in case from an accepted filter.
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            allowed_extensions
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(extension))
        })
}

fn file_description(path: &Path) -> String {
    path.extension()
        .and_then(OsStr::to_str)
        .filter(|extension| !extension.is_empty())
        .map_or_else(
            || "file".to_owned(),
            |extension| format!("{extension} file"),
        )
}

fn shell_safe_path_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '/' | '_' | '-' | '.' | '~')
}

fn path_is_shell_safe(value: &str) -> bool {
    !value.is_empty()
        && (!value.starts_with('~') || value.starts_with("~/"))
        && value.chars().all(shell_safe_path_character)
}

fn quote_bourne_path(value: &str) -> String {
    let (prefix, remainder) = value
        .strip_prefix("~/")
        .map_or(("", value), |remainder| ("~/", remainder));
    let escaped = remainder.replace('\'', "'\\''");
    format!("{prefix}'{escaped}'")
}

fn quote_fish_path(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len());
    for (index, character) in value.char_indices() {
        let unsafe_leading_tilde = index == 0 && character == '~' && !value.starts_with("~/");
        if shell_safe_path_character(character) && !unsafe_leading_tilde {
            quoted.push(character);
        } else {
            quoted.push('\\');
            quoted.push(character);
        }
    }
    if quoted.is_empty() {
        "''".to_owned()
    } else {
        quoted
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let identifier = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-filesystem-test-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn query(&self, line: impl Into<String>) -> CompletionQuery {
            let line = line.into();
            CompletionQuery::new(line.clone(), line.len(), &self.0, 1).unwrap()
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bounded_scan_discards_partial_candidates_after_cooperative_stop() {
        let temp = TempDirectory::new();
        fs::write(temp.0.join("abed"), "study").unwrap();
        fs::write(temp.0.join("troy"), "study").unwrap();
        let checks = Cell::new(0_usize);

        let suggestions = filesystem_suggestions_bounded(
            &temp.query("open "),
            ShellKind::Bash,
            &FilesystemOptions::default(),
            MAX_DIRECTORY_ENTRIES,
            || {
                let next = checks.get() + 1;
                checks.set(next);
                next >= 4
            },
        );

        assert!(suggestions.is_empty());
        assert_eq!(checks.get(), 4);
    }

    #[test]
    fn completes_relative_nested_paths_deterministically() {
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("src/components")).unwrap();
        fs::write(temp.0.join("src/community.rs"), "").unwrap();
        fs::write(temp.0.join("src/config.rs"), "").unwrap();

        let suggestions = filesystem_suggestions(
            &temp.query("cat src/co"),
            ShellKind::Bash,
            &FilesystemOptions::default(),
        );

        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.display.as_str())
                .collect::<Vec<_>>(),
            ["src/community.rs", "src/components", "src/config.rs"]
        );
        assert_eq!(suggestions[1].insertion, InsertionBehavior::Directory);
        assert_eq!(suggestions[0].edit.range, 4..10);
        assert_eq!(
            suggestions[1]
                .resulting_line(&temp.query("cat src/co"))
                .unwrap(),
            "cat src/components/"
        );
    }

    #[test]
    fn filters_hidden_files_and_keeps_directories_for_extension_traversal() {
        let temp = TempDirectory::new();
        fs::create_dir(temp.0.join("nested")).unwrap();
        fs::write(temp.0.join("main.rs"), "").unwrap();
        fs::write(temp.0.join("notes.txt"), "").unwrap();
        fs::write(temp.0.join(".secret.rs"), "").unwrap();
        let options = FilesystemOptions {
            extensions: vec![".rs".into()],
            ..FilesystemOptions::default()
        };

        let suggestions = filesystem_suggestions(&temp.query("open "), ShellKind::Zsh, &options);
        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.display.as_str())
                .collect::<Vec<_>>(),
            ["main.rs", "nested"]
        );
    }

    #[test]
    fn extension_filters_ignore_case() {
        let temp = TempDirectory::new();
        fs::write(temp.0.join("main.RS"), "").unwrap();
        fs::write(temp.0.join("notes.txt"), "").unwrap();
        let options = FilesystemOptions {
            extensions: vec![".rs".into()],
            ..FilesystemOptions::default()
        };

        let suggestions = filesystem_suggestions(&temp.query("open "), ShellKind::Zsh, &options);

        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.display.as_str())
                .collect::<Vec<_>>(),
            ["main.RS"]
        );
    }

    #[test]
    fn supports_absolute_and_home_relative_paths() {
        let temp = TempDirectory::new();
        let home = temp.0.join("home");
        fs::create_dir_all(home.join("Documents")).unwrap();
        fs::create_dir_all(temp.0.join("absolute-directory")).unwrap();

        let options = FilesystemOptions {
            home_directory: Some(home),
            ..FilesystemOptions::default()
        };
        let home_suggestions =
            filesystem_suggestions(&temp.query("cd ~/Doc"), ShellKind::Fish, &options);
        assert_eq!(home_suggestions[0].display, "~/Documents");

        let absolute_fragment = format!("cd {}/abs", temp.0.display());
        let absolute_suggestions =
            filesystem_suggestions(&temp.query(absolute_fragment), ShellKind::Bash, &options);
        assert_eq!(
            absolute_suggestions[0].display,
            temp.0.join("absolute-directory").to_string_lossy()
        );
    }

    #[test]
    fn supports_directory_only_lookup_and_missing_paths() {
        let temp = TempDirectory::new();
        fs::create_dir(temp.0.join("study-room")).unwrap();
        fs::write(temp.0.join("study-guide"), "").unwrap();
        let options = FilesystemOptions {
            directory_only: true,
            ..FilesystemOptions::default()
        };

        let suggestions = filesystem_suggestions(&temp.query("cd stu"), ShellKind::Bash, &options);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].display, "study-room");
        assert!(
            filesystem_suggestions(&temp.query("cd missing/path"), ShellKind::Bash, &options,)
                .is_empty()
        );
    }

    #[test]
    fn quotes_paths_for_each_supported_shell() {
        assert_eq!(
            quote_path(ShellKind::Bash, "Dean Pelton/file"),
            "'Dean Pelton/file'"
        );
        assert_eq!(
            quote_path(ShellKind::Zsh, "Troy's file"),
            "'Troy'\\''s file'"
        );
        assert_eq!(
            quote_path(ShellKind::Fish, "Dean Pelton/file"),
            "Dean\\ Pelton/file"
        );
        assert_eq!(
            quote_path(ShellKind::Bash, "~/Dean Pelton"),
            "~/'Dean Pelton'"
        );
        assert_eq!(quote_path(ShellKind::Bash, "~troy"), "'~troy'");
        assert_eq!(quote_path(ShellKind::Fish, "~troy"), "\\~troy");
    }

    #[test]
    fn replacement_preserves_the_line_suffix() {
        let temp = TempDirectory::new();
        fs::write(temp.0.join("Community Notes.txt"), "").unwrap();
        let line = "cat Com --verbose";
        let query = CompletionQuery::new(line, 7, &temp.0, 1).unwrap();

        let suggestion =
            filesystem_suggestions(&query, ShellKind::Bash, &FilesystemOptions::default())
                .remove(0);
        assert_eq!(
            suggestion.resulting_line(&query).unwrap(),
            "cat 'Community Notes.txt' --verbose"
        );
    }
}
