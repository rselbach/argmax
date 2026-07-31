//! Executable discovery on `PATH`.

use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::ffi::OsStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::completion::{InsertionBehavior, Suggestion, SuggestionSource, TextEdit};

use super::{ShellKind, quote_path};

const MAX_PATH_DIRECTORIES: usize = 256;
const MAX_ENTRIES_PER_DIRECTORY: usize = 8_192;
const MAX_EXECUTABLES: usize = 8_192;
const MAX_EXECUTABLE_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const MAX_PATH_CACHE_KEY_BYTES: usize = 64 * 1024;
const MAX_PATH_CACHE_ENTRY_BYTES: usize = 4 * 1024;
const PATH_CACHE_REVALIDATE_INTERVAL: Duration = Duration::from_secs(1);

/// An executable basename discovered on `PATH`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathExecutable {
    /// Name offered for insertion.
    pub name: String,
    /// First executable with this name in `PATH` order.
    pub path: PathBuf,
    /// Whether a relative or empty `PATH` entry placed this in the working
    /// directory.
    ///
    /// Such an executable is whatever the current directory happens to contain.
    /// It stays offered as a suggestion, because the shell would run it, but
    /// generators refuse to execute it: they run while the user types, before
    /// the user has decided to run anything.
    pub from_working_directory: bool,
}

/// Lazy executable snapshot invalidated by `PATH`, cwd, or directory metadata.
///
/// Retained executable names and paths are bounded to four MiB in addition to the
/// item-count limit.
#[derive(Clone, Debug, Default)]
pub struct PathExecutableCache {
    key: Option<PathCacheIdentity>,
    executables: Vec<PathExecutable>,
    validated_at: Option<Instant>,
    #[cfg(test)]
    scan_count: usize,
}

impl PathExecutableCache {
    /// Returns the current bounded executable snapshot, refreshing only when its
    /// observable lookup inputs changed.
    ///
    /// Relative and empty `PATH` entries make `cwd` part of resolution. Directory
    /// creation, removal, replacement, modification, or permission changes alter
    /// the metadata key and trigger a refresh. A one-second revalidation bound
    /// catches entry eligibility changes such as `chmod` that do not update the
    /// parent directory and retries snapshots affected by filesystem errors.
    pub fn executables(&mut self, path_value: &OsStr, cwd: &Path) -> &[PathExecutable] {
        self.executables_at(path_value, cwd, Instant::now())
    }

    fn executables_at(
        &mut self,
        path_value: &OsStr,
        cwd: &Path,
        now: Instant,
    ) -> &[PathExecutable] {
        let before = PathCacheIdentity::capture(path_value, cwd);
        let refresh_due =
            self.key.as_ref() != Some(&before) || revalidation_due(self.validated_at, now);
        if !refresh_due {
            return &self.executables;
        }

        let discovery = discover_path_snapshot(path_value, cwd);
        let after = PathCacheIdentity::capture(path_value, cwd);
        #[cfg(test)]
        {
            self.scan_count = self.scan_count.saturating_add(1);
        }
        self.executables = discovery.executables;
        self.key = stable_scan_key(&before, after);
        self.validated_at = self.key.as_ref().map(|_| now);
        &self.executables
    }

    /// Forces the next lookup to rescan even when metadata appears unchanged.
    pub fn invalidate(&mut self) {
        self.key = None;
        self.validated_at = None;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathCacheIdentity {
    Bounded(PathCacheKey),
    OversizedInput([u64; 2]),
}

impl PathCacheIdentity {
    fn capture(path_value: &OsStr, cwd: &Path) -> Self {
        PathCacheKey::capture(path_value, cwd).map_or_else(
            || Self::OversizedInput(path_cache_fingerprint(path_value, cwd)),
            Self::Bounded,
        )
    }
}

fn path_cache_fingerprint(path_value: &OsStr, cwd: &Path) -> [u64; 2] {
    [0_u8, 1_u8].map(|domain| {
        let mut hasher = DefaultHasher::new();
        domain.hash(&mut hasher);
        path_value.hash(&mut hasher);
        cwd.hash(&mut hasher);
        hasher.finish()
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PathCacheKey {
    directories: Vec<PathDirectorySignature>,
}

impl PathCacheKey {
    fn capture(path_value: &OsStr, cwd: &Path) -> Option<Self> {
        let mut key_bytes = 0_usize;
        let mut directories = Vec::new();
        for configured_directory in std::env::split_paths(path_value).take(MAX_PATH_DIRECTORIES) {
            if os_str_bytes(configured_directory.as_os_str()) > MAX_PATH_CACHE_ENTRY_BYTES {
                return None;
            }
            let directory = resolve_path_directory(configured_directory, cwd);
            let path_bytes = os_str_bytes(directory.as_os_str());
            if path_bytes > MAX_PATH_CACHE_ENTRY_BYTES {
                return None;
            }
            key_bytes = key_bytes.saturating_add(path_bytes);
            if key_bytes > MAX_PATH_CACHE_KEY_BYTES {
                return None;
            }
            directories.push(PathDirectorySignature::capture(directory));
        }
        Some(Self { directories })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PathDirectorySignature {
    path: PathBuf,
    link: Option<PathMetadata>,
    target: Option<PathMetadata>,
}

impl PathDirectorySignature {
    fn capture(path: PathBuf) -> Self {
        let link_metadata = fs::symlink_metadata(&path).ok();
        let target = link_metadata
            .as_ref()
            .filter(|metadata| metadata.file_type().is_symlink())
            .and_then(|_| fs::metadata(&path).ok())
            .as_ref()
            .map(PathMetadata::from);
        let link = link_metadata.as_ref().map(PathMetadata::from);
        Self { path, link, target }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathMetadata {
    modified: Option<SystemTime>,
    length: u64,
    permissions: u32,
    is_directory: bool,
    is_symlink: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl From<&fs::Metadata> for PathMetadata {
    fn from(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        let (device, inode, changed_seconds, changed_nanoseconds) = unix_identity(metadata);
        Self {
            modified: metadata.modified().ok(),
            length: metadata.len(),
            permissions: permission_signature(metadata),
            is_directory: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            #[cfg(unix)]
            device,
            #[cfg(unix)]
            inode,
            #[cfg(unix)]
            changed_seconds,
            #[cfg(unix)]
            changed_nanoseconds,
        }
    }
}

fn stable_scan_key(
    before: &PathCacheIdentity,
    after: PathCacheIdentity,
) -> Option<PathCacheIdentity> {
    if before == &after { Some(after) } else { None }
}

fn revalidation_due(validated_at: Option<Instant>, now: Instant) -> bool {
    validated_at
        .and_then(|validated_at| now.checked_duration_since(validated_at))
        .is_none_or(|elapsed| elapsed >= PATH_CACHE_REVALIDATE_INTERVAL)
}

/// Discovers executable files from a `PATH` value.
///
/// Results are deduplicated by basename, preserve the first matching executable
/// in `PATH` order, and are returned in deterministic lexical order. Missing and
/// unreadable directories are ignored.
#[must_use]
pub fn discover_path_executables(path_value: &OsStr, cwd: &Path) -> Vec<PathExecutable> {
    discover_path_snapshot(path_value, cwd).executables
}

struct PathDiscovery {
    executables: Vec<PathExecutable>,
}

fn discover_path_snapshot(path_value: &OsStr, cwd: &Path) -> PathDiscovery {
    let mut executables = BTreeMap::new();
    let mut executable_bytes = 0_usize;

    'directories: for configured_directory in
        std::env::split_paths(path_value).take(MAX_PATH_DIRECTORIES)
    {
        let from_working_directory = !configured_directory.is_absolute();
        let directory = resolve_path_directory(configured_directory, cwd);
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries.take(MAX_ENTRIES_PER_DIRECTORY) {
            let Ok(entry) = entry else {
                continue;
            };
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
            if executables.contains_key(&name) {
                continue;
            }

            let path = entry.path();
            let item_bytes = name.len().saturating_add(os_str_bytes(path.as_os_str()));
            if !reserve_executable_bytes(&mut executable_bytes, item_bytes) {
                break 'directories;
            }
            executables.insert(
                name.clone(),
                PathExecutable {
                    name,
                    path,
                    from_working_directory,
                },
            );
            if executables.len() >= MAX_EXECUTABLES {
                break 'directories;
            }
        }
    }

    PathDiscovery {
        executables: executables.into_values().collect(),
    }
}

fn resolve_path_directory(configured_directory: PathBuf, cwd: &Path) -> PathBuf {
    if configured_directory.as_os_str().is_empty() {
        cwd.to_path_buf()
    } else if configured_directory.is_absolute() {
        configured_directory
    } else {
        cwd.join(configured_directory)
    }
}

fn reserve_executable_bytes(used: &mut usize, item_bytes: usize) -> bool {
    let Some(next) = used.checked_add(item_bytes) else {
        return false;
    };
    if next > MAX_EXECUTABLE_PAYLOAD_BYTES {
        return false;
    }
    *used = next;
    true
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().len()
}

#[cfg(not(unix))]
fn os_str_bytes(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[cfg(unix)]
fn permission_signature(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    metadata.permissions().mode()
}

#[cfg(unix)]
fn unix_identity(metadata: &fs::Metadata) -> (u64, u64, i64, i64) {
    use std::os::unix::fs::MetadataExt as _;

    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(not(unix))]
fn permission_signature(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
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
    use std::os::unix::fs::{PermissionsExt, symlink};

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
                    from_working_directory: false,
                },
                PathExecutable {
                    name: "troy".into(),
                    path: first_troy,
                    from_working_directory: false,
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
                from_working_directory: true,
            }]
        );
    }

    #[test]
    fn suggestions_match_case_insensitively_and_are_inert() {
        let executables = vec![
            PathExecutable {
                name: "Git".into(),
                path: "/bin/Git".into(),
                from_working_directory: false,
            },
            PathExecutable {
                name: "gist".into(),
                path: "/bin/gist".into(),
                from_working_directory: false,
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
            from_working_directory: false,
        }];
        let suggestions = path_suggestions(&executables, "Dean", 0..4, ShellKind::Bash);
        assert_eq!(suggestions[0].edit.replacement, "'Dean;Pelton'");
    }

    #[cfg(unix)]
    #[test]
    fn cache_reuses_stable_inputs_and_refreshes_when_path_changes() {
        let temp = TempDirectory::new();
        let troy = temp.executable("first", "troy");
        let abed = temp.executable("second", "abed");
        let first_path = std::env::join_paths([temp.0.join("first")]).unwrap();
        let second_path = std::env::join_paths([temp.0.join("second")]).unwrap();
        let mut cache = PathExecutableCache::default();
        let now = Instant::now();

        assert_eq!(
            cache.executables_at(&first_path, &temp.0, now),
            [PathExecutable {
                name: "troy".into(),
                path: troy,
                from_working_directory: false,
            }]
        );
        assert_eq!(cache.scan_count, 1);
        assert_eq!(
            cache.executables_at(
                &first_path,
                &temp.0,
                now + PATH_CACHE_REVALIDATE_INTERVAL / 2,
            )[0]
            .name,
            "troy"
        );
        assert_eq!(cache.scan_count, 1);

        assert_eq!(
            cache.executables_at(
                &second_path,
                &temp.0,
                now + PATH_CACHE_REVALIDATE_INTERVAL / 2,
            ),
            [PathExecutable {
                name: "abed".into(),
                path: abed,
                from_working_directory: false,
            }]
        );
        assert_eq!(cache.scan_count, 2);
    }

    #[cfg(unix)]
    #[test]
    fn cache_refreshes_for_relative_cwd_and_directory_metadata() {
        let temp = TempDirectory::new();
        let first_cwd = temp.0.join("first-cwd");
        let second_cwd = temp.0.join("second-cwd");
        let _troy = temp.executable("first-cwd/bin", "troy");
        let abed = temp.executable("second-cwd/bin", "abed");
        let mut cache = PathExecutableCache::default();

        assert_eq!(
            cache.executables(OsStr::new("bin"), &first_cwd)[0].name,
            "troy"
        );
        assert_eq!(
            cache.executables(OsStr::new("bin"), &second_cwd),
            [PathExecutable {
                name: "abed".into(),
                path: abed,
                from_working_directory: true,
            }]
        );

        let empty = temp.0.join("empty");
        fs::create_dir(&empty).unwrap();
        fs::set_permissions(&empty, fs::Permissions::from_mode(0o700)).unwrap();
        let path_value = std::env::join_paths([&empty]).unwrap();
        assert!(cache.executables(&path_value, &temp.0).is_empty());

        let dean = temp.executable("empty", "dean");
        fs::set_permissions(&empty, fs::Permissions::from_mode(0o711)).unwrap();
        assert_eq!(
            cache.executables(&path_value, &temp.0),
            [PathExecutable {
                name: "dean".into(),
                path: dean,
                from_working_directory: false,
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_invalidation_forces_an_authoritative_rescan() {
        let temp = TempDirectory::new();
        let _troy = temp.executable("bin", "troy");
        let path_value = std::env::join_paths([temp.0.join("bin")]).unwrap();
        let mut cache = PathExecutableCache::default();

        assert_eq!(cache.executables(&path_value, &temp.0).len(), 1);
        let _abed = temp.executable("bin", "abed");
        cache.invalidate();

        assert_eq!(
            cache
                .executables(&path_value, &temp.0)
                .iter()
                .map(|executable| executable.name.as_str())
                .collect::<Vec<_>>(),
            ["abed", "troy"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_revalidates_entry_permissions_within_one_second() {
        let temp = TempDirectory::new();
        let troy = temp.executable("bin", "troy");
        let path_value = std::env::join_paths([temp.0.join("bin")]).unwrap();
        let mut cache = PathExecutableCache::default();
        let now = Instant::now();

        assert_eq!(
            cache.executables_at(&path_value, &temp.0, now)[0].name,
            "troy"
        );
        fs::set_permissions(&troy, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(
            cache
                .executables_at(&path_value, &temp.0, now + PATH_CACHE_REVALIDATE_INTERVAL,)
                .is_empty()
        );
        assert_eq!(cache.scan_count, 2);
    }

    #[cfg(unix)]
    #[test]
    fn cache_detects_directory_symlink_retargeting_immediately() {
        let temp = TempDirectory::new();
        let _troy = temp.executable("first", "troy");
        let _abed = temp.executable("second", "abed");
        let link = temp.0.join("current");
        symlink(temp.0.join("first"), &link).unwrap();
        let path_value = std::env::join_paths([&link]).unwrap();
        let mut cache = PathExecutableCache::default();
        let now = Instant::now();

        assert_eq!(
            cache.executables_at(&path_value, &temp.0, now)[0].name,
            "troy"
        );
        fs::remove_file(&link).unwrap();
        symlink(temp.0.join("second"), &link).unwrap();

        assert_eq!(
            cache.executables_at(&path_value, &temp.0, now)[0].name,
            "abed"
        );
        assert_eq!(cache.scan_count, 2);
    }

    #[cfg(unix)]
    #[test]
    fn stable_structural_errors_do_not_disable_warm_cache_reuse() {
        let temp = TempDirectory::new();
        let troy = temp.executable("bin", "troy");
        let regular_file = temp.0.join("not-a-directory");
        fs::write(&regular_file, "Greendale").unwrap();
        symlink(temp.0.join("missing"), temp.0.join("bin/dangling")).unwrap();
        let too_long = temp.0.join("x".repeat(300));
        let symlink_loop = temp.0.join("loop");
        symlink(&symlink_loop, &symlink_loop).unwrap();
        let path_value =
            std::env::join_paths([too_long, symlink_loop, regular_file, temp.0.join("bin")])
                .unwrap();
        let mut cache = PathExecutableCache::default();
        let now = Instant::now();

        assert_eq!(
            cache.executables_at(&path_value, &temp.0, now),
            [PathExecutable {
                name: "troy".into(),
                path: troy,
                from_working_directory: false,
            }]
        );
        assert_eq!(cache.scan_count, 1);
        assert_eq!(
            cache
                .executables_at(
                    &path_value,
                    &temp.0,
                    now + PATH_CACHE_REVALIDATE_INTERVAL / 2,
                )
                .len(),
            1
        );
        assert_eq!(cache.scan_count, 1);
    }

    #[test]
    fn executable_payload_budget_is_hard_and_overflow_safe() {
        let mut used = 0;
        assert!(reserve_executable_bytes(
            &mut used,
            MAX_EXECUTABLE_PAYLOAD_BYTES
        ));
        assert_eq!(used, MAX_EXECUTABLE_PAYLOAD_BYTES);
        assert!(!reserve_executable_bytes(&mut used, 1));
        assert_eq!(used, MAX_EXECUTABLE_PAYLOAD_BYTES);

        let mut near_overflow = usize::MAX - 1;
        assert!(!reserve_executable_bytes(&mut near_overflow, 2));
        assert_eq!(near_overflow, usize::MAX - 1);
    }

    #[cfg(unix)]
    #[test]
    fn oversized_cache_keys_use_a_bounded_identity_and_refresh_on_change() {
        let temp = TempDirectory::new();
        let troy = temp.executable("first", "troy");
        let abed = temp.executable("second", "abed");
        let oversized = PathBuf::from("x".repeat(MAX_PATH_CACHE_KEY_BYTES + 1));
        let first_path = std::env::join_paths([&oversized, &temp.0.join("first")]).unwrap();
        let second_path = std::env::join_paths([&oversized, &temp.0.join("second")]).unwrap();
        let mut cache = PathExecutableCache::default();
        let now = Instant::now();

        assert_eq!(
            cache.executables_at(&first_path, &temp.0, now),
            [PathExecutable {
                name: "troy".into(),
                path: troy,
                from_working_directory: false,
            }]
        );
        assert!(matches!(
            cache.key,
            Some(PathCacheIdentity::OversizedInput(_))
        ));
        assert_eq!(cache.scan_count, 1);
        assert_eq!(
            cache
                .executables_at(
                    &first_path,
                    &temp.0,
                    now + PATH_CACHE_REVALIDATE_INTERVAL / 2,
                )
                .len(),
            1
        );
        assert_eq!(cache.scan_count, 1);
        assert_eq!(
            cache.executables_at(
                &second_path,
                &temp.0,
                now + PATH_CACHE_REVALIDATE_INTERVAL / 2,
            ),
            [PathExecutable {
                name: "abed".into(),
                path: abed,
                from_working_directory: false,
            }]
        );
        assert_eq!(cache.scan_count, 2);
        assert_ne!(
            path_cache_fingerprint(&first_path, &temp.0),
            path_cache_fingerprint(&second_path, &temp.0)
        );
    }

    #[test]
    fn only_stable_pre_and_post_scan_identities_are_cacheable() {
        let temp = TempDirectory::new();
        let key = PathCacheKey::capture(OsStr::new(""), &temp.0).unwrap();
        let bounded = PathCacheIdentity::Bounded(key);
        let oversized = PathCacheIdentity::OversizedInput([1, 2]);

        assert_eq!(
            stable_scan_key(&bounded, bounded.clone()),
            Some(bounded.clone())
        );
        assert_eq!(stable_scan_key(&bounded, oversized.clone()), None);
        assert_eq!(
            stable_scan_key(&oversized, oversized.clone()),
            Some(oversized)
        );
    }
}
