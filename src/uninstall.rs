//! Explicit, best-effort removal of argmax-owned integration and local data.
//!
//! Shell configuration edits use the same descriptor-anchored transaction as
//! setup. Data trees are traversed and unlinked relative to already-open
//! directory descriptors, so symlink substitution cannot redirect recursion.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};
use rustix::fs::{AtFlags, Mode, OFlags};

use crate::config::Shell;
use crate::integration::SESSION_MARKER_ENV;
use crate::setup::{SetupError, SetupTarget};

/// Maximum filesystem entries removed by one explicit invocation.
pub const MAX_UNINSTALL_ENTRIES: usize = 1_000_000;
/// Maximum directory depth below one owned data root.
pub const MAX_UNINSTALL_DEPTH: usize = 64;

/// Category of an explicitly removed location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovalKind {
    /// Stable marked shell integration bytes.
    ShellIntegration,
    /// Argmax config, state, learning, diagnostic, or cache tree.
    LocalData,
    /// The currently running, known argmax executable.
    Executable,
}

/// One successful removal suitable for explicit command output.
#[derive(Debug)]
pub struct RemovedLocation {
    /// Removed category.
    pub kind: RemovalKind,
    /// Exact location removed or edited.
    pub path: PathBuf,
    /// Durable shell-config backup created before editing, when applicable.
    pub backup: Option<PathBuf>,
}

/// One location-specific failure; other locations are still attempted.
#[derive(Debug)]
pub struct RemovalFailure {
    /// Failed category.
    pub kind: RemovalKind,
    /// Exact attempted location.
    pub path: PathBuf,
    /// Sanitized reason.
    pub error: UninstallError,
}

/// Aggregate explicit-uninstall report.
#[derive(Debug, Default)]
pub struct UninstallReport {
    /// Successfully removed locations.
    pub removed: Vec<RemovedLocation>,
    /// Independent location failures.
    pub failures: Vec<RemovalFailure>,
    /// Unmarked legacy hook lines retained deliberately.
    pub retained_legacy_integrations: Vec<PathBuf>,
    /// Whether the command inherited an active wrapper marker.
    pub active_session: bool,
}

impl UninstallReport {
    /// Whether every authorized location completed successfully.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Discovery or per-location removal failure.
#[derive(Debug)]
pub enum UninstallError {
    /// Platform home/config/data/cache roots are unavailable.
    NoPlatformDirectory,
    /// A candidate path was relative, malformed, or outside the closed shape.
    InvalidPath,
    /// A directory component or final entry was a symlink or unsafe type.
    UnsafeFileType,
    /// The final data tree or executable is not owned by the current user.
    NotOwned,
    /// A candidate changed identity while it was being removed.
    SourceChanged,
    /// A directory exceeded the defensive recursion depth.
    DepthLimit,
    /// Aggregate entry work exceeded [`MAX_UNINSTALL_ENTRIES`].
    EntryLimit,
    /// Stable marked shell-block removal failed.
    Shell(SetupError),
    /// A descriptor-relative filesystem operation failed.
    Io {
        /// Stable operation label.
        operation: &'static str,
        /// Content-free operating-system error class.
        kind: io::ErrorKind,
    },
}

impl UninstallError {
    fn io(operation: &'static str, error: impl Into<io::Error>) -> Self {
        let error = error.into();
        Self::Io {
            operation,
            kind: error.kind(),
        }
    }
}

impl fmt::Display for UninstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlatformDirectory => {
                formatter.write_str("user data directories are unavailable")
            }
            Self::InvalidPath => formatter.write_str("removal path is invalid"),
            Self::UnsafeFileType => {
                formatter.write_str("removal path contains a symlink or unsafe file type")
            }
            Self::NotOwned => formatter.write_str("removal target is not owned by this user"),
            Self::SourceChanged => {
                formatter.write_str("removal target changed and was left in place")
            }
            Self::DepthLimit => formatter.write_str("local data exceeds the removal depth limit"),
            Self::EntryLimit => formatter.write_str("local data exceeds the removal entry limit"),
            Self::Shell(error) => write!(formatter, "shell integration: {error}"),
            Self::Io { operation, kind } => write!(formatter, "{operation}: {kind:?}"),
        }
    }
}

impl Error for UninstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Shell(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SetupError> for UninstallError {
    fn from(error: SetupError) -> Self {
        Self::Shell(error)
    }
}

/// Closed set of locations authorized by one `argmax uninstall` invocation.
pub struct UninstallPlan {
    shell_targets: Vec<SetupTarget>,
    data_directories: Vec<PathBuf>,
    update_artifacts: Vec<PathBuf>,
    executable: PathBuf,
    active_session: bool,
}

impl UninstallPlan {
    /// Discovers supported shell configs, argmax-owned platform directories,
    /// and the current executable without touching them.
    ///
    /// # Errors
    ///
    /// Returns [`UninstallError::NoPlatformDirectory`] when the required user
    /// roots are unavailable, or a path error for unsafe environment roots.
    pub fn discover() -> Result<Self, UninstallError> {
        let base = BaseDirs::new().ok_or(UninstallError::NoPlatformDirectory)?;
        let project =
            ProjectDirs::from("", "", "argmax").ok_or(UninstallError::NoPlatformDirectory)?;
        let legacy =
            ProjectDirs::from("", "", "iris").ok_or(UninstallError::NoPlatformDirectory)?;
        let home = base.home_dir();
        let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
        let xdg_config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let shell_targets = [Shell::Bash, Shell::Zsh, Shell::Fish]
            .into_iter()
            .map(|shell| {
                SetupTarget::from_environment(
                    shell,
                    home,
                    zdotdir.as_deref(),
                    xdg_config.as_deref(),
                )
                .map_err(UninstallError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let candidates = [
            project.config_dir().to_path_buf(),
            project.data_dir().to_path_buf(),
            project.cache_dir().to_path_buf(),
            legacy.config_dir().to_path_buf(),
            legacy.data_dir().to_path_buf(),
            legacy.cache_dir().to_path_buf(),
            base.config_dir().join("argmax"),
            base.data_dir().join("argmax"),
            base.cache_dir().join("argmax"),
            base.config_dir().join("iris"),
            base.data_dir().join("iris"),
            base.cache_dir().join("iris"),
            home.join(".local/share/argmax"),
            home.join(".local/share/iris"),
            home.join(".argmax"),
            home.join(".iris"),
        ];
        let mut data_directories = Vec::new();
        for candidate in candidates {
            validate_owned_root_shape(&candidate)?;
            if !data_directories.contains(&candidate) {
                data_directories.push(candidate);
            }
        }
        // Remove deeper legacy/current duplicates before an enclosing tree.
        data_directories.sort_by(|left, right| {
            right
                .components()
                .count()
                .cmp(&left.components().count())
                .then_with(|| left.cmp(right))
        });
        let executable = std::env::current_exe()
            .map_err(|error| UninstallError::io("locate current executable", error))?;
        validate_executable_shape(&executable)?;
        let update_artifacts = discover_update_artifacts(&executable)?;
        Ok(Self {
            shell_targets,
            data_directories,
            update_artifacts,
            executable,
            active_session: std::env::var_os(SESSION_MARKER_ENV)
                .is_some_and(|value| !value.is_empty()),
        })
    }

    /// Builds an explicit plan for deterministic integration tests.
    ///
    /// # Errors
    ///
    /// Returns a closed path-shape error before retaining any candidate.
    pub fn new(
        shell_targets: Vec<SetupTarget>,
        data_directories: Vec<PathBuf>,
        executable: PathBuf,
        active_session: bool,
    ) -> Result<Self, UninstallError> {
        for path in &data_directories {
            validate_owned_root_shape(path)?;
        }
        validate_executable_shape(&executable)?;
        let update_artifacts = discover_update_artifacts(&executable)?;
        Ok(Self {
            shell_targets,
            data_directories,
            update_artifacts,
            executable,
            active_session,
        })
    }

    /// Removes every authorized location independently and returns all results.
    ///
    /// The explicit command itself authorizes the closed plan; no path is
    /// discovered recursively outside these roots. Partial failure never stops
    /// later locations from being attempted.
    #[must_use]
    pub fn execute(self, timestamp_seconds: u64) -> UninstallReport {
        let mut report = UninstallReport {
            active_session: self.active_session,
            ..UninstallReport::default()
        };
        for target in self.shell_targets {
            match target.remove(timestamp_seconds) {
                Ok(outcome) => {
                    if outcome.changed() {
                        report.removed.push(RemovedLocation {
                            kind: RemovalKind::ShellIntegration,
                            path: outcome.path().to_path_buf(),
                            backup: outcome.backup().map(Path::to_path_buf),
                        });
                    }
                    if outcome.retained_legacy_integrations() > 0 {
                        report
                            .retained_legacy_integrations
                            .push(outcome.path().to_path_buf());
                    }
                }
                Err(error) => report.failures.push(RemovalFailure {
                    kind: RemovalKind::ShellIntegration,
                    path: target.path().to_path_buf(),
                    error: error.into(),
                }),
            }
        }
        for path in self.data_directories {
            match remove_owned_tree(&path) {
                Ok(RemoveOutcome::Missing) => {}
                Ok(RemoveOutcome::Removed) => report.removed.push(RemovedLocation {
                    kind: RemovalKind::LocalData,
                    path,
                    backup: None,
                }),
                Err(error) => report.failures.push(RemovalFailure {
                    kind: RemovalKind::LocalData,
                    path,
                    error,
                }),
            }
        }
        for path in self.update_artifacts {
            record_executable_removal(&mut report, path, validate_update_artifact_shape);
        }
        record_executable_removal(&mut report, self.executable, validate_executable_shape);
        report
    }
}

fn record_executable_removal(
    report: &mut UninstallReport,
    path: PathBuf,
    validate: fn(&Path) -> Result<(), UninstallError>,
) {
    match remove_owned_executable(&path, validate) {
        Ok(RemoveOutcome::Missing) => {}
        Ok(RemoveOutcome::Removed) => report.removed.push(RemovedLocation {
            kind: RemovalKind::Executable,
            path,
            backup: None,
        }),
        Err(error) => report.failures.push(RemovalFailure {
            kind: RemovalKind::Executable,
            path,
            error,
        }),
    }
}

impl fmt::Debug for UninstallPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UninstallPlan")
            .field("shell_target_count", &self.shell_targets.len())
            .field("data_directory_count", &self.data_directories.len())
            .field("update_artifact_count", &self.update_artifacts.len())
            .field(
                "executable_path_bytes",
                &self.executable.as_os_str().as_bytes().len(),
            )
            .field("active_session", &self.active_session)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoveOutcome {
    Missing,
    Removed,
}

fn validate_owned_root_shape(path: &Path) -> Result<(), UninstallError> {
    validate_absolute_normal(path)?;
    match path.file_name().and_then(OsStr::to_str) {
        Some("argmax" | ".argmax" | "iris" | ".iris") => Ok(()),
        _ => Err(UninstallError::InvalidPath),
    }
}

fn validate_executable_shape(path: &Path) -> Result<(), UninstallError> {
    validate_absolute_normal(path)?;
    if path.file_name() == Some(OsStr::new("argmax")) {
        Ok(())
    } else {
        Err(UninstallError::InvalidPath)
    }
}

fn validate_update_artifact_shape(path: &Path) -> Result<(), UninstallError> {
    validate_absolute_normal(path)?;
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Err(UninstallError::InvalidPath);
    };
    let Some(random) = name
        .strip_prefix(".argmax-update-")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return Err(UninstallError::InvalidPath);
    };
    if random.len() == 32
        && random
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(UninstallError::InvalidPath)
    }
}

fn discover_update_artifacts(executable: &Path) -> Result<Vec<PathBuf>, UninstallError> {
    let Some(anchored) = open_parent(executable)? else {
        return Ok(Vec::new());
    };
    let parent = executable.parent().ok_or(UninstallError::InvalidPath)?;
    let mut entries = rustix::fs::Dir::read_from(&anchored.parent)
        .map_err(|error| UninstallError::io("read executable directory", error))?;
    let mut paths = Vec::new();
    for entry in &mut entries {
        let entry = entry.map_err(|error| UninstallError::io("read executable entry", error))?;
        let path = parent.join(OsStr::from_bytes(entry.file_name().to_bytes()));
        if validate_update_artifact_shape(&path).is_ok() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn validate_absolute_normal(path: &Path) -> Result<(), UninstallError> {
    if !path.is_absolute() {
        return Err(UninstallError::InvalidPath);
    }
    for component in path.components() {
        if !matches!(component, Component::RootDir | Component::Normal(_)) {
            return Err(UninstallError::InvalidPath);
        }
    }
    Ok(())
}

struct AnchoredName {
    parent: File,
    name: OsString,
}

fn open_parent(path: &Path) -> Result<Option<AnchoredName>, UninstallError> {
    validate_absolute_normal(path)?;
    let parent_path = platform_anchor_path(path.parent().ok_or(UninstallError::InvalidPath)?);
    let name = path
        .file_name()
        .ok_or(UninstallError::InvalidPath)?
        .to_os_string();
    let mut parent = File::from(
        rustix::fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| UninstallError::io("open filesystem root", error))?,
    );
    for component in parent_path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        parent = match rustix::fs::openat(
            &parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => File::from(directory),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                return Err(UninstallError::UnsafeFileType);
            }
            Err(error) => {
                return Err(UninstallError::io(
                    "open removal directory component",
                    error,
                ));
            }
        };
    }
    Ok(Some(AnchoredName { parent, name }))
}

#[cfg(target_os = "macos")]
fn platform_anchor_path(path: &Path) -> PathBuf {
    for (alias, canonical) in [
        (Path::new("/tmp"), Path::new("/private/tmp")),
        (Path::new("/var"), Path::new("/private/var")),
        (Path::new("/etc"), Path::new("/private/etc")),
    ] {
        if let Ok(suffix) = path.strip_prefix(alias) {
            return canonical.join(suffix);
        }
    }
    path.to_path_buf()
}

#[cfg(not(target_os = "macos"))]
fn platform_anchor_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn remove_owned_tree(path: &Path) -> Result<RemoveOutcome, UninstallError> {
    validate_owned_root_shape(path)?;
    let Some(anchored) = open_parent(path)? else {
        return Ok(RemoveOutcome::Missing);
    };
    let directory = match rustix::fs::openat(
        &anchored.parent,
        &anchored.name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(directory) => File::from(directory),
        Err(rustix::io::Errno::NOENT) => return Ok(RemoveOutcome::Missing),
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
            return Err(UninstallError::UnsafeFileType);
        }
        Err(error) => return Err(UninstallError::io("open local data directory", error)),
    };
    let identity = owned_identity(&directory, true)?;
    let mut budget = MAX_UNINSTALL_ENTRIES;
    clear_directory(&directory, 0, &mut budget)?;
    if !named_identity_matches(&anchored.parent, &anchored.name, &identity, true)? {
        return Err(UninstallError::SourceChanged);
    }
    rustix::fs::unlinkat(&anchored.parent, &anchored.name, AtFlags::REMOVEDIR)
        .map_err(|error| UninstallError::io("remove local data directory", error))?;
    anchored
        .parent
        .sync_all()
        .map_err(|error| UninstallError::io("sync local data parent", error))?;
    Ok(RemoveOutcome::Removed)
}

fn clear_directory(
    directory: &File,
    depth: usize,
    budget: &mut usize,
) -> Result<(), UninstallError> {
    if depth >= MAX_UNINSTALL_DEPTH {
        return Err(UninstallError::DepthLimit);
    }
    let mut entries = rustix::fs::Dir::read_from(directory)
        .map_err(|error| UninstallError::io("read local data directory", error))?;
    for entry in &mut entries {
        let entry = entry.map_err(|error| UninstallError::io("read local data entry", error))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        *budget = budget.checked_sub(1).ok_or(UninstallError::EntryLimit)?;
        match rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(child) => {
                let child = File::from(child);
                let identity = file_identity(&child)?;
                clear_directory(&child, depth + 1, budget)?;
                if !named_identity_matches(
                    directory,
                    OsStr::from_bytes(name.to_bytes()),
                    &identity,
                    true,
                )? {
                    return Err(UninstallError::SourceChanged);
                }
                rustix::fs::unlinkat(directory, name, AtFlags::REMOVEDIR)
                    .map_err(|error| UninstallError::io("remove local data subdirectory", error))?;
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR | rustix::io::Errno::INVAL) => {
                match rustix::fs::unlinkat(directory, name, AtFlags::empty()) {
                    Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                    Err(rustix::io::Errno::ISDIR) => return Err(UninstallError::SourceChanged),
                    Err(error) => {
                        return Err(UninstallError::io("remove local data entry", error));
                    }
                }
            }
            Err(error) => return Err(UninstallError::io("open local data entry", error)),
        }
    }
    directory
        .sync_all()
        .map_err(|error| UninstallError::io("sync local data directory", error))
}

fn remove_owned_executable(
    path: &Path,
    validate: fn(&Path) -> Result<(), UninstallError>,
) -> Result<RemoveOutcome, UninstallError> {
    validate(path)?;
    let Some(anchored) = open_parent(path)? else {
        return Ok(RemoveOutcome::Missing);
    };
    let executable = match rustix::fs::openat(
        &anchored.parent,
        &anchored.name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(executable) => File::from(executable),
        Err(rustix::io::Errno::NOENT) => return Ok(RemoveOutcome::Missing),
        Err(rustix::io::Errno::LOOP) => return Err(UninstallError::UnsafeFileType),
        Err(error) => return Err(UninstallError::io("open argmax executable", error)),
    };
    let metadata = executable
        .metadata()
        .map_err(|error| UninstallError::io("inspect argmax executable", error))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(UninstallError::UnsafeFileType);
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(UninstallError::NotOwned);
    }
    let identity = FileIdentity::from_metadata(&metadata);
    if !named_identity_matches(&anchored.parent, &anchored.name, &identity, false)? {
        return Err(UninstallError::SourceChanged);
    }
    rustix::fs::unlinkat(&anchored.parent, &anchored.name, AtFlags::empty())
        .map_err(|error| UninstallError::io("remove argmax executable", error))?;
    anchored
        .parent
        .sync_all()
        .map_err(|error| UninstallError::io("sync executable directory", error))?;
    Ok(RemoveOutcome::Removed)
}

#[derive(Clone, Copy)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn matches(self, metadata: &Metadata) -> bool {
        metadata.dev() == self.device && metadata.ino() == self.inode
    }
}

fn file_identity(file: &File) -> Result<FileIdentity, UninstallError> {
    file.metadata()
        .map(|metadata| FileIdentity::from_metadata(&metadata))
        .map_err(|error| UninstallError::io("inspect local data identity", error))
}

fn owned_identity(file: &File, directory: bool) -> Result<FileIdentity, UninstallError> {
    let metadata = file
        .metadata()
        .map_err(|error| UninstallError::io("inspect removal target", error))?;
    if metadata.is_dir() != directory {
        return Err(UninstallError::UnsafeFileType);
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(UninstallError::NotOwned);
    }
    Ok(FileIdentity::from_metadata(&metadata))
}

fn named_identity_matches(
    parent: &File,
    name: &OsStr,
    identity: &FileIdentity,
    directory: bool,
) -> Result<bool, UninstallError> {
    let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    if directory {
        flags |= OFlags::DIRECTORY;
    } else {
        flags |= OFlags::NONBLOCK;
    }
    match rustix::fs::openat(parent, name, flags, Mode::empty()) {
        Ok(reopened) => File::from(reopened)
            .metadata()
            .map(|metadata| identity.matches(&metadata))
            .map_err(|error| UninstallError::io("reinspect removal target", error)),
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
            Ok(false)
        }
        Err(error) => Err(UninstallError::io("reopen removal target", error)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::integration::{BEGIN_MARKER, END_MARKER};

    fn executable(path: &Path) {
        fs::write(path, b"argmax test executable").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn removes_only_marked_hooks_owned_trees_and_exact_binary() {
        let temporary = tempfile::tempdir().unwrap();
        let shell_path = temporary.path().join(".bashrc");
        fs::write(
            &shell_path,
            format!("export DEAN=Pelton\n{BEGIN_MARKER}\nhook\n{END_MARKER}\n# Community\n"),
        )
        .unwrap();
        let data = temporary.path().join("argmax");
        fs::create_dir(&data).unwrap();
        fs::create_dir(data.join("nested")).unwrap();
        fs::write(data.join("nested/history.db"), b"Troy Barnes").unwrap();
        let legacy_data = temporary.path().join("iris");
        fs::create_dir(&legacy_data).unwrap();
        fs::write(legacy_data.join("state.toml"), b"legacy = true").unwrap();
        let binary = temporary.path().join("argmax-bin/argmax");
        fs::create_dir(binary.parent().unwrap()).unwrap();
        executable(&binary);
        let retained_update = binary
            .parent()
            .unwrap()
            .join(".argmax-update-00000000000000000000000000000000.tmp");
        executable(&retained_update);
        let unrelated = binary
            .parent()
            .unwrap()
            .join(".argmax-update-not-an-owned-name.tmp");
        executable(&unrelated);
        let plan = UninstallPlan::new(
            vec![SetupTarget::new(Shell::Bash, &shell_path).unwrap()],
            vec![data.clone(), legacy_data.clone()],
            binary.clone(),
            false,
        )
        .unwrap();

        let report = plan.execute(42);
        assert!(report.succeeded(), "{:#?}", report.failures);
        assert_eq!(report.removed.len(), 5);
        assert_eq!(
            fs::read_to_string(&shell_path).unwrap(),
            "export DEAN=Pelton\n# Community\n"
        );
        assert!(!data.exists());
        assert!(!legacy_data.exists());
        assert!(!binary.exists());
        assert!(!retained_update.exists());
        assert!(unrelated.exists());
        let shell = report
            .removed
            .iter()
            .find(|removed| removed.kind == RemovalKind::ShellIntegration)
            .unwrap();
        assert!(shell.backup.as_ref().unwrap().exists());
    }

    #[test]
    fn partial_failure_is_reported_and_other_locations_continue() {
        let temporary = tempfile::tempdir().unwrap();
        let victim = temporary.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("Community.txt"), b"preserve").unwrap();
        let linked = temporary.path().join("argmax");
        symlink(&victim, &linked).unwrap();
        let binary = temporary.path().join("bin/argmax");
        fs::create_dir(binary.parent().unwrap()).unwrap();
        executable(&binary);
        let report = UninstallPlan::new(Vec::new(), vec![linked], binary.clone(), true)
            .unwrap()
            .execute(1);

        assert!(report.active_session);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].kind, RemovalKind::LocalData);
        assert!(victim.join("Community.txt").exists());
        assert!(!binary.exists());
    }

    #[test]
    fn refuses_broad_or_mislabeled_paths_before_removal() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("argmax");
        executable(&binary);
        for invalid in [
            temporary.path().to_path_buf(),
            PathBuf::from("relative/argmax"),
            PathBuf::from("/"),
        ] {
            assert!(matches!(
                UninstallPlan::new(Vec::new(), vec![invalid], binary.clone(), false),
                Err(UninstallError::InvalidPath)
            ));
        }
        assert!(binary.exists());
    }
}
