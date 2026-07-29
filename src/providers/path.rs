//! Executable discovery on `PATH`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::completion::{InsertionBehavior, Suggestion, SuggestionSource, TextEdit};

use super::{ShellKind, quote_path};

const MAX_PATH_DIRECTORIES: usize = 256;
const MAX_ENTRIES_PER_DIRECTORY: usize = 8_192;
const MAX_EXECUTABLES: usize = 8_192;

/// An executable basename discovered on `PATH`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathExecutable {
    /// Name offered for insertion.
    pub name: String,
    /// First executable with this name in `PATH` order.
    pub path: PathBuf,
}

/// Discovers executable files from a `PATH` value.
///
/// Results are deduplicated by basename, preserve the first matching executable
/// in `PATH` order, and are returned in deterministic lexical order. Missing and
/// unreadable directories are ignored.
#[must_use]
pub fn discover_path_executables(path_value: &OsStr, cwd: &Path) -> Vec<PathExecutable> {
    let mut executables = BTreeMap::new();

    'directories: for configured_directory in
        std::env::split_paths(path_value).take(MAX_PATH_DIRECTORIES)
    {
        let directory = if configured_directory.as_os_str().is_empty() {
            cwd.to_path_buf()
        } else if configured_directory.is_absolute() {
            configured_directory
        } else {
            cwd.join(configured_directory)
        };
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries
            .take(MAX_ENTRIES_PER_DIRECTORY)
            .filter_map(Result::ok)
        {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.chars().any(char::is_control) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() || !is_executable(&metadata) {
                continue;
            }

            executables.entry(name.clone()).or_insert(PathExecutable {
                name,
                path: entry.path(),
            });
            if executables.len() >= MAX_EXECUTABLES {
                break 'directories;
            }
        }
    }

    executables.into_values().collect()
}

/// Creates inert root-command suggestions for matching `PATH` executables.
#[must_use]
pub fn path_suggestions(
    executables: &[PathExecutable],
    typed_name: &str,
    replacement_range: Range<usize>,
    shell: ShellKind,
) -> Vec<Suggestion> {
    let folded_prefix = typed_name.to_lowercase();
    let mut matches: Vec<_> = executables
        .iter()
        .take(MAX_EXECUTABLES)
        .filter(|executable| executable.name.to_lowercase().starts_with(&folded_prefix))
        .collect();
    matches.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });

    matches
        .into_iter()
        .map(|executable| {
            let mut suggestion = Suggestion::new(
                TextEdit {
                    range: replacement_range.clone(),
                    replacement: quote_path(shell, &executable.name),
                },
                &executable.name,
                "executable on PATH",
                "command",
                SuggestionSource::System,
                InsertionBehavior::AppendSpace,
                format!("system:{}", executable.name),
            );
            suggestion.static_priority = 0.55;
            suggestion.confidence = 0.8;
            suggestion
        })
        .collect()
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let identifier = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-path-test-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        #[cfg(unix)]
        fn executable(&self, directory: &str, name: &str) -> PathBuf {
            let directory = self.0.join(directory);
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join(name);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn discovers_only_executable_files_and_uses_first_path_entry() {
        let temp = TempDirectory::new();
        let first_troy = temp.executable("first", "troy");
        let _second_troy = temp.executable("second", "troy");
        let greendale = temp.executable("second", "greendale");
        fs::write(temp.0.join("first/not-executable"), "nope").unwrap();
        fs::create_dir(temp.0.join("first/executable-directory")).unwrap();

        let path_value = std::env::join_paths([
            temp.0.join("missing"),
            temp.0.join("first"),
            temp.0.join("second"),
        ])
        .unwrap();
        let discovered = discover_path_executables(&path_value, &temp.0);

        assert_eq!(
            discovered,
            vec![
                PathExecutable {
                    name: "greendale".into(),
                    path: greendale,
                },
                PathExecutable {
                    name: "troy".into(),
                    path: first_troy,
                },
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn empty_path_component_uses_current_directory() {
        let temp = TempDirectory::new();
        let executable = temp.executable("", "dean");

        assert_eq!(
            discover_path_executables(OsStr::new(""), &temp.0),
            vec![PathExecutable {
                name: "dean".into(),
                path: executable,
            }]
        );
    }

    #[test]
    fn suggestions_match_case_insensitively_and_are_inert() {
        let executables = vec![
            PathExecutable {
                name: "Git".into(),
                path: "/bin/Git".into(),
            },
            PathExecutable {
                name: "gist".into(),
                path: "/bin/gist".into(),
            },
        ];

        let suggestions = path_suggestions(&executables, "GI", 2..4, ShellKind::Bash);
        assert_eq!(suggestions[0].display, "gist");
        assert_eq!(suggestions[1].display, "Git");
        assert_eq!(suggestions[0].source, SuggestionSource::System);
        assert_eq!(suggestions[0].edit.range, 2..4);
        assert_eq!(suggestions[0].insertion, InsertionBehavior::AppendSpace);
    }

    #[cfg(unix)]
    #[test]
    fn resolves_relative_path_entries_against_active_cwd() {
        let temp = TempDirectory::new();
        let executable = temp.executable("bin", "greendale");
        let discovered = discover_path_executables(OsStr::new("bin"), &temp.0);
        assert_eq!(discovered[0].path, executable);
    }

    #[cfg(unix)]
    #[test]
    fn quotes_executable_names_with_shell_metacharacters() {
        let temp = TempDirectory::new();
        let path = temp.executable("bin", "Dean;Pelton");
        let executables = vec![PathExecutable {
            name: "Dean;Pelton".into(),
            path,
        }];
        let suggestions = path_suggestions(&executables, "Dean", 0..4, ShellKind::Bash);
        assert_eq!(suggestions[0].edit.replacement, "'Dean;Pelton'");
    }
}
