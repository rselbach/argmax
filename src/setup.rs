//! Safe filesystem boundary for installing one shell-integration block.

use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use crate::config::Shell;
use crate::integration::{ConfigEditError, EditOutcome, edit_config, suggest_config_target};

/// Maximum shell-configuration bytes excluding one bounded managed block.
pub const MAX_SHELL_CONFIG_BYTES: usize = 2 * 1024 * 1024;

// Leaves room for one version-independent managed block on a configuration
// whose unrelated bytes already reach the public bound.
const MAX_MANAGED_BLOCK_BYTES: usize = 16 * 1024;
const MAX_SHELL_FILE_BYTES: usize = MAX_SHELL_CONFIG_BYTES + MAX_MANAGED_BLOCK_BYTES;
const MAX_EXTENDED_ATTRIBUTE_LIST_BYTES: usize = 64 * 1024;
const MAX_EXTENDED_ATTRIBUTE_VALUE_BYTES: usize = 128 * 1024;
const MAX_EXTENDED_ATTRIBUTE_TOTAL_BYTES: usize = 512 * 1024;
const MAX_BACKUP_COLLISIONS: u8 = 100;
const MAX_TRANSACTION_COLLISIONS: u8 = 100;
static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Detects one supported shell without starting or sourcing it.
///
/// The active-shell marker takes precedence over `SHELL`. Each value may be a
/// basename or an absolute executable path.
///
/// # Errors
///
/// Returns [`SetupError::UnsupportedShell`] when a supplied active-shell marker
/// is unsupported, or when no active marker exists and `SHELL` is unsupported.
pub fn detect_setup_shell(
    active_shell: Option<&OsStr>,
    shell_environment: Option<&OsStr>,
) -> Result<Shell, SetupError> {
    match active_shell {
        Some(active_shell) => parse_shell_name(active_shell),
        None => shell_environment.and_then(parse_shell_name),
    }
    .ok_or(SetupError::UnsupportedShell)
}

fn parse_shell_name(value: &OsStr) -> Option<Shell> {
    let name = Path::new(value).file_name()?.to_str()?;
    match name {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        _ => None,
    }
}

/// Validated shell and configuration path for setup.
#[derive(Clone, Eq, PartialEq)]
pub struct SetupTarget {
    shell: Shell,
    path: PathBuf,
}

impl SetupTarget {
    /// Resolves the documented Bash, Zsh, or Fish configuration path.
    ///
    /// # Errors
    ///
    /// Returns an error when a selected Bash or Fish root is not absolute, or
    /// when the current directory needed to resolve a relative `ZDOTDIR` cannot
    /// be read. Overrides belonging to another shell are ignored.
    pub fn from_environment(
        shell: Shell,
        home: &Path,
        zdotdir: Option<&Path>,
        xdg_config_home: Option<&Path>,
    ) -> Result<Self, SetupError> {
        let current_directory = if shell == Shell::Zsh
            && zdotdir.is_some_and(|path| !path.is_absolute() && !path.as_os_str().is_empty())
        {
            Some(
                env::current_dir()
                    .map_err(|error| SetupError::io("resolve current directory", error))?,
            )
        } else {
            None
        };
        Self::from_environment_at(
            shell,
            home,
            zdotdir,
            xdg_config_home,
            current_directory.as_deref(),
        )
    }

    fn from_environment_at(
        shell: Shell,
        home: &Path,
        zdotdir: Option<&Path>,
        xdg_config_home: Option<&Path>,
        current_directory: Option<&Path>,
    ) -> Result<Self, SetupError> {
        match shell {
            Shell::Bash => validate_absolute(home)?,
            Shell::Zsh if zdotdir.is_none() => validate_absolute(home)?,
            Shell::Zsh => {}
            Shell::Fish => match xdg_config_home {
                Some(path) if path.is_absolute() => {}
                Some(_) | None => validate_absolute(home)?,
            },
        }
        let suggested = suggest_config_target(shell, home, zdotdir, xdg_config_home);
        let path = if suggested.path().is_absolute() {
            suggested.path().to_path_buf()
        } else {
            let current_directory = current_directory.ok_or(SetupError::RelativePath)?;
            validate_absolute(current_directory)?;
            current_directory.join(suggested.path())
        };
        Ok(Self { shell, path })
    }

    /// Builds an explicit target for deterministic callers and tests.
    ///
    /// # Errors
    ///
    /// Returns [`SetupError::RelativePath`] for a relative path.
    pub fn new(shell: Shell, path: impl Into<PathBuf>) -> Result<Self, SetupError> {
        let path = path.into();
        validate_absolute(&path)?;
        Ok(Self { shell, path })
    }

    /// Selected shell.
    #[must_use]
    pub const fn shell(&self) -> Shell {
        self.shell
    }

    /// Exact configuration path to inspect or modify.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Installs one marked integration block atomically.
    ///
    /// Existing bytes are backed up and synced before the first changed file is
    /// published. An unchanged block or recognized same-shell legacy line is a
    /// byte-for-byte no-op. Symlinks, non-regular files, hard links, oversized
    /// files, and concurrent source replacement fail closed.
    ///
    /// # Errors
    ///
    /// Returns a bounded structural or filesystem error. Existing content is
    /// preserved whenever publication cannot be proven safe.
    pub fn install(&self, timestamp_seconds: u64) -> Result<SetupOutcome, SetupError> {
        self.install_with_post_publication_hook(timestamp_seconds, || {})
    }

    fn install_with_post_publication_hook(
        &self,
        timestamp_seconds: u64,
        post_publication_hook: impl FnOnce(),
    ) -> Result<SetupOutcome, SetupError> {
        let anchored = AnchoredTarget::open(&self.path)?;
        let lock = acquire_setup_lock(&anchored)?;
        let source = read_optional_source(&anchored)?;
        let input = source
            .as_ref()
            .map_or(&[][..], |source| source.bytes.as_slice());
        let edit = edit_config(input, self.shell).map_err(SetupError::MalformedConfig)?;
        if edit.content().len() > MAX_SHELL_FILE_BYTES
            || edit.source_managed_bytes() > MAX_MANAGED_BLOCK_BYTES
            || edit.source_unmanaged_bytes() > MAX_SHELL_CONFIG_BYTES
        {
            return Err(SetupError::ConfigTooLarge);
        }
        if edit.outcome() == EditOutcome::Unchanged {
            validate_unchanged(&anchored, &lock, source.as_ref())?;
            return Ok(SetupOutcome::unchanged(self.path.clone()));
        }

        let backup = source
            .as_ref()
            .map(|source| write_backup(&anchored, source, timestamp_seconds))
            .transpose()?;
        let mut replacement = prepare_replacement(&anchored, edit.content(), source.as_ref())?;
        if let (Some(backup), Some(source)) = (backup.as_ref(), source.as_ref()) {
            validate_backup(&anchored, backup, source)?;
        }
        anchored.validate_parent()?;
        validate_lock_anchor(&anchored, &lock)?;

        let publication = match source.as_ref() {
            Some(source) => publish_replacement(&anchored, source, &mut replacement),
            None => publish_new(&anchored, &replacement),
        };
        let directory_sync = sync_directory(&anchored.parent);
        publication?;
        directory_sync?;
        post_publication_hook();
        if let Err(error) = validate_published_authority(&anchored, &lock) {
            restore_published_state(&anchored, source.as_ref(), &mut replacement)?;
            sync_directory(&anchored.parent)?;
            return Err(error);
        }
        if let (Some(backup), Some(source)) = (backup.as_ref(), source.as_ref())
            && let Err(error) = validate_backup(&anchored, backup, source)
        {
            restore_published_state(&anchored, Some(source), &mut replacement)?;
            sync_directory(&anchored.parent)?;
            return Err(error);
        }
        Ok(SetupOutcome::installed(
            self.path.clone(),
            backup.map(|backup| backup.path),
        ))
    }
}

fn validate_published_authority(target: &AnchoredTarget, lock: &File) -> Result<(), SetupError> {
    target.validate_parent()?;
    validate_lock_anchor(target, lock)?;
    target.validate_parent()
}

fn restore_published_state(
    target: &AnchoredTarget,
    source: Option<&SourceFile>,
    replacement: &mut TransactionFile,
) -> Result<(), SetupError> {
    match source {
        Some(source) => restore_published_source(target, source, replacement),
        None => quarantine_published_new(target, replacement),
    }
}

fn restore_published_source(
    target: &AnchoredTarget,
    source: &SourceFile,
    replacement: &TransactionFile,
) -> Result<(), SetupError> {
    if !source_is_current(target, source, &replacement.name)?
        || !transaction_is_current_at(target, replacement, &target.name)?
    {
        return Err(SetupError::SourceChanged);
    }
    atomic_exchange(target, &replacement.name, &target.name)
        .map_err(|error| SetupError::io("restore shell configuration after validation", error))
}

fn quarantine_published_new(
    target: &AnchoredTarget,
    replacement: &mut TransactionFile,
) -> Result<(), SetupError> {
    if !transaction_is_current_at(target, replacement, &target.name)? {
        return Err(SetupError::SourceChanged);
    }
    for _ in 0..MAX_TRANSACTION_COLLISIONS {
        let quarantine_name = next_transaction_name();
        match atomic_move_noclobber(target, &target.name, &quarantine_name) {
            Ok(()) => {
                replacement.name = quarantine_name;
                return if transaction_is_current_at(target, replacement, &replacement.name)? {
                    Ok(())
                } else {
                    Err(SetupError::SourceChanged)
                };
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SetupError::SourceChanged);
            }
            Err(error) => {
                return Err(SetupError::io(
                    "quarantine published shell configuration",
                    error,
                ));
            }
        }
    }
    Err(SetupError::TransactionNameExhausted)
}

fn validate_unchanged(
    target: &AnchoredTarget,
    lock: &File,
    source: Option<&SourceFile>,
) -> Result<(), SetupError> {
    target.validate_parent()?;
    match source {
        Some(source) if source_is_current(target, source, &target.name)? => Ok(()),
        None if !named_path_exists(target, &target.name)? => Ok(()),
        Some(_) | None => Err(SetupError::SourceChanged),
    }?;
    validate_lock_anchor(target, lock)?;
    target.validate_parent()
}

impl fmt::Debug for SetupTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupTarget")
            .field("shell", &self.shell)
            .field("path_bytes", &path_bytes(&self.path))
            .finish()
    }
}

fn validate_absolute(path: &Path) -> Result<(), SetupError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(SetupError::RelativePath)
    }
}

/// Result of one idempotent setup attempt.
#[derive(Clone, Eq, PartialEq)]
pub struct SetupOutcome {
    path: PathBuf,
    backup: Option<PathBuf>,
    changed: bool,
}

impl SetupOutcome {
    fn unchanged(path: PathBuf) -> Self {
        Self {
            path,
            backup: None,
            changed: false,
        }
    }

    fn installed(path: PathBuf, backup: Option<PathBuf>) -> Self {
        Self {
            path,
            backup,
            changed: true,
        }
    }

    /// Configuration path inspected or modified.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Durable pre-modification backup, when an existing file changed.
    #[must_use]
    pub fn backup(&self) -> Option<&Path> {
        self.backup.as_deref()
    }

    /// Whether setup published new bytes.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }
}

impl fmt::Debug for SetupOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupOutcome")
            .field("path_bytes", &path_bytes(&self.path))
            .field("backup_path_bytes", &self.backup.as_deref().map(path_bytes))
            .field("changed", &self.changed)
            .finish()
    }
}

/// Closed setup failure without sensitive path contents.
#[derive(Debug)]
pub enum SetupError {
    /// No supplied environment value identified a supported shell.
    UnsupportedShell,
    /// A required setup path was relative.
    RelativePath,
    /// The target had no usable parent or filename.
    MissingPathComponent,
    /// A target, lock, or parent path had an unsafe type or link count.
    UnsafeFileType,
    /// Existing unrelated configuration exceeded [`MAX_SHELL_CONFIG_BYTES`].
    ConfigTooLarge,
    /// Stable integration markers were malformed or duplicated.
    MalformedConfig(ConfigEditError),
    /// Extended filesystem metadata could not be preserved safely.
    UnsupportedMetadata,
    /// No collision-free backup name remained.
    BackupNameExhausted,
    /// No collision-free transaction quarantine name remained.
    TransactionNameExhausted,
    /// The source changed after inspection and was preserved.
    SourceChanged,
    /// A filesystem operation failed.
    Io {
        /// Sanitized operation label.
        operation: &'static str,
        /// Content-free error kind.
        kind: io::ErrorKind,
    },
}

impl SetupError {
    fn io(operation: &'static str, error: io::Error) -> Self {
        let kind = error.kind();
        drop(error);
        Self::Io { operation, kind }
    }
}

impl fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedShell => formatter
                .write_str("could not detect Bash, Zsh, or Fish; use `argmax setup <shell>`"),
            Self::RelativePath => formatter.write_str("shell configuration path is not absolute"),
            Self::MissingPathComponent => {
                formatter.write_str("shell configuration path is incomplete")
            }
            Self::UnsafeFileType => {
                formatter.write_str("shell configuration path is not a safe regular file")
            }
            Self::ConfigTooLarge => formatter.write_str("shell configuration exceeds setup limit"),
            Self::MalformedConfig(error) => write!(formatter, "{error}"),
            Self::UnsupportedMetadata => {
                formatter.write_str("shell configuration metadata cannot be preserved safely")
            }
            Self::BackupNameExhausted => {
                formatter.write_str("no unused shell configuration backup name is available")
            }
            Self::TransactionNameExhausted => {
                formatter.write_str("no unused shell transaction name is available")
            }
            Self::SourceChanged => formatter.write_str(
                "shell configuration changed during setup and was left untouched; retry setup",
            ),
            Self::Io { operation, kind } => write!(formatter, "{operation}: {kind:?}"),
        }
    }
}

impl Error for SetupError {}

struct SourceFile {
    file: File,
    bytes: Vec<u8>,
    metadata: PreservedMetadata,
}

#[derive(Clone, Eq, PartialEq)]
struct PreservedMetadata {
    mode: u32,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    extended_attributes: Vec<ExtendedAttribute>,
}

#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExtendedAttribute {
    name: OsString,
    value: Vec<u8>,
}

struct AnchoredTarget {
    parent: File,
    parent_path: PathBuf,
    name: OsString,
}

impl AnchoredTarget {
    fn open(path: &Path) -> Result<Self, SetupError> {
        let parent_path = platform_anchor_path(required_parent(path)?);
        let name = path
            .file_name()
            .ok_or(SetupError::MissingPathComponent)?
            .to_os_string();
        let parent = open_directory_chain(&parent_path, true)?;
        Ok(Self {
            parent,
            parent_path,
            name,
        })
    }

    fn absolute_path_for(&self, name: &OsStr) -> PathBuf {
        self.parent_path.join(name)
    }

    fn validate_parent(&self) -> Result<(), SetupError> {
        let reopened = match open_directory_chain(&self.parent_path, false) {
            Ok(parent) => parent,
            Err(SetupError::UnsafeFileType | SetupError::Io { .. }) => {
                return Err(SetupError::SourceChanged);
            }
            Err(error) => return Err(error),
        };
        if open_files_match(&self.parent, &reopened)? {
            Ok(())
        } else {
            Err(SetupError::SourceChanged)
        }
    }
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

fn open_directory_chain(path: &Path, create: bool) -> Result<File, SetupError> {
    if !path.is_absolute() {
        return Err(SetupError::RelativePath);
    }
    let mut directory = File::from(
        rustix::fs::open(
            "/",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(rustix_error)
        .map_err(|error| SetupError::io("open filesystem root", error))?,
    );
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(SetupError::UnsafeFileType);
        };
        let opened = match open_directory_at(&directory, name) {
            Ok(opened) => opened,
            Err(error) if error == rustix::io::Errno::NOENT && create => {
                let created = match rustix::fs::mkdirat(
                    &directory,
                    name,
                    rustix::fs::Mode::from_raw_mode(0o700),
                ) {
                    Ok(()) => true,
                    Err(rustix::io::Errno::EXIST) => false,
                    Err(error) => return Err(directory_component_error(error)),
                };
                let opened =
                    open_directory_at(&directory, name).map_err(directory_component_error)?;
                if created {
                    rustix::fs::fchmod(&opened, rustix::fs::Mode::from_raw_mode(0o700))
                        .map_err(rustix_error)
                        .map_err(|error| {
                            SetupError::io("secure shell configuration directory", error)
                        })?;
                }
                opened
            }
            Err(error) => return Err(directory_component_error(error)),
        };
        directory = File::from(opened);
    }
    Ok(directory)
}

fn open_directory_at(
    parent: &File,
    name: &OsStr,
) -> Result<std::os::fd::OwnedFd, rustix::io::Errno> {
    rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
}

fn directory_component_error(error: rustix::io::Errno) -> SetupError {
    if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) {
        SetupError::UnsafeFileType
    } else {
        SetupError::io("open shell configuration directory", rustix_error(error))
    }
}

fn open_files_match(left: &File, right: &File) -> Result<bool, SetupError> {
    let left = left
        .metadata()
        .map_err(|error| SetupError::io("inspect shell configuration directory", error))?;
    let right = right
        .metadata()
        .map_err(|error| SetupError::io("reinspect shell configuration directory", error))?;
    #[cfg(unix)]
    {
        Ok(left.is_dir()
            && right.is_dir()
            && left.dev() == right.dev()
            && left.ino() == right.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        Ok(false)
    }
}

fn required_parent(path: &Path) -> Result<&Path, SetupError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(SetupError::MissingPathComponent)
}

fn read_optional_source(target: &AnchoredTarget) -> Result<Option<SourceFile>, SetupError> {
    let mut file = match rustix::fs::openat(
        &target.parent,
        &target.name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => {
            return Err(SetupError::io(
                "open shell configuration",
                rustix_error(error),
            ));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| SetupError::io("inspect shell configuration", error))?;
    if !metadata.is_file() {
        return Err(SetupError::UnsafeFileType);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(SetupError::UnsafeFileType);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(MAX_SHELL_FILE_BYTES)
            .min(MAX_SHELL_FILE_BYTES),
    );
    Read::take(
        &mut file,
        u64::try_from(MAX_SHELL_FILE_BYTES + 1).expect("setup bound fits u64"),
    )
    .read_to_end(&mut bytes)
    .map_err(|error| SetupError::io("read shell configuration", error))?;
    if bytes.len() > MAX_SHELL_FILE_BYTES {
        return Err(SetupError::ConfigTooLarge);
    }
    ensure_no_unsupported_acl(&file)?;
    let metadata = capture_preserved_metadata(&file)?;
    Ok(Some(SourceFile {
        file,
        bytes,
        metadata,
    }))
}

fn capture_preserved_metadata(file: &File) -> Result<PreservedMetadata, SetupError> {
    let metadata = file
        .metadata()
        .map_err(|error| SetupError::io("inspect shell configuration metadata", error))?;
    #[cfg(unix)]
    {
        Ok(PreservedMetadata {
            mode: metadata.mode() & 0o7777,
            uid: metadata.uid(),
            gid: metadata.gid(),
            extended_attributes: read_extended_attributes(file)?,
        })
    }
    #[cfg(not(unix))]
    {
        Ok(PreservedMetadata {
            mode: if metadata.permissions().readonly() {
                0o400
            } else {
                0o600
            },
        })
    }
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "hurd"
))]
fn read_extended_attributes(file: &File) -> Result<Vec<ExtendedAttribute>, SetupError> {
    let mut names = vec![0; MAX_EXTENDED_ATTRIBUTE_LIST_BYTES + 1];
    let names_len = rustix::fs::flistxattr(file, &mut names).map_err(extended_metadata_error)?;
    if names_len > MAX_EXTENDED_ATTRIBUTE_LIST_BYTES {
        return Err(SetupError::UnsupportedMetadata);
    }
    names.truncate(names_len);
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if !names.is_empty() && names.last() != Some(&0) {
        return Err(SetupError::UnsupportedMetadata);
    }

    let mut attributes = Vec::new();
    let mut total_bytes = names.len();
    let names_without_terminator = names.strip_suffix(&[0]).unwrap_or(&names);
    for name in names_without_terminator.split(|byte| *byte == 0) {
        if name.is_empty() {
            return Err(SetupError::UnsupportedMetadata);
        }
        let name = OsStr::from_bytes(name).to_os_string();
        let mut value = vec![0; MAX_EXTENDED_ATTRIBUTE_VALUE_BYTES + 1];
        let value_len =
            rustix::fs::fgetxattr(file, &name, &mut value).map_err(extended_metadata_error)?;
        if value_len > MAX_EXTENDED_ATTRIBUTE_VALUE_BYTES {
            return Err(SetupError::UnsupportedMetadata);
        }
        value.truncate(value_len);
        total_bytes = total_bytes
            .checked_add(value.len())
            .ok_or(SetupError::UnsupportedMetadata)?;
        if total_bytes > MAX_EXTENDED_ATTRIBUTE_TOTAL_BYTES {
            return Err(SetupError::UnsupportedMetadata);
        }
        attributes.push(ExtendedAttribute { name, value });
    }
    attributes.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    Ok(attributes)
}

#[cfg(all(
    unix,
    not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "hurd"
    ))
))]
fn read_extended_attributes(_file: &File) -> Result<Vec<ExtendedAttribute>, SetupError> {
    Err(SetupError::UnsupportedMetadata)
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "hurd"
))]
fn replace_extended_attributes(
    file: &File,
    attributes: &[ExtendedAttribute],
    operation: &'static str,
) -> Result<(), SetupError> {
    for existing in read_extended_attributes(file)? {
        if attributes
            .iter()
            .all(|attribute| attribute.name != existing.name)
        {
            rustix::fs::fremovexattr(file, &existing.name)
                .map_err(rustix_error)
                .map_err(|error| SetupError::io(operation, error))?;
        }
    }
    for attribute in attributes {
        rustix::fs::fsetxattr(
            file,
            &attribute.name,
            &attribute.value,
            rustix::fs::XattrFlags::empty(),
        )
        .map_err(rustix_error)
        .map_err(|error| SetupError::io(operation, error))?;
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "hurd"
    ))
))]
fn replace_extended_attributes(
    _file: &File,
    _attributes: &[ExtendedAttribute],
    _operation: &'static str,
) -> Result<(), SetupError> {
    Err(SetupError::UnsupportedMetadata)
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "hurd"
))]
fn extended_metadata_error(error: rustix::io::Errno) -> SetupError {
    if error == rustix::io::Errno::RANGE {
        SetupError::UnsupportedMetadata
    } else {
        SetupError::io("inspect extended shell metadata", rustix_error(error))
    }
}

#[cfg(target_os = "macos")]
fn ensure_no_unsupported_acl(file: &File) -> Result<(), SetupError> {
    if argmax_platform::macos::has_extended_acl(file)
        .map_err(|error| SetupError::io("inspect shell access controls", error))?
    {
        return Err(SetupError::UnsupportedMetadata);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_no_unsupported_acl(_file: &File) -> Result<(), SetupError> {
    Ok(())
}

fn acquire_setup_lock(target: &AnchoredTarget) -> Result<File, SetupError> {
    let lock_name = sidecar_name(&target.name, ".argmax-setup.lock");
    let file = File::from(
        rustix::fs::openat(
            &target.parent,
            &lock_name,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(rustix_error)
        .map_err(|error| SetupError::io("open shell setup lock", error))?,
    );
    let metadata = file
        .metadata()
        .map_err(|error| SetupError::io("inspect shell setup lock", error))?;
    if !metadata.is_file() {
        return Err(SetupError::UnsafeFileType);
    }
    #[cfg(unix)]
    {
        if metadata.nlink() != 1 {
            return Err(SetupError::UnsafeFileType);
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| SetupError::io("secure shell setup lock", error))?;
    }
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(rustix_error)
        .map_err(|error| SetupError::io("lock shell setup", error))?;
    validate_lock_anchor(target, &file)?;
    Ok(file)
}

fn validate_lock_anchor(target: &AnchoredTarget, file: &File) -> Result<(), SetupError> {
    let lock_name = sidecar_name(&target.name, ".argmax-setup.lock");
    if !file_is_exclusively_named_at(target, file, &lock_name, "reinspect shell setup lock")? {
        return Err(SetupError::UnsafeFileType);
    }
    Ok(())
}

fn sidecar_name(name: &OsStr, suffix: &str) -> OsString {
    let mut sidecar = OsString::from(".");
    sidecar.push(name);
    sidecar.push(suffix);
    sidecar
}

fn write_backup(
    target: &AnchoredTarget,
    source: &SourceFile,
    timestamp_seconds: u64,
) -> Result<BackupFile, SetupError> {
    for collision in 0..MAX_BACKUP_COLLISIONS {
        let mut backup_name = OsString::from(&target.name);
        backup_name.push(format!(".argmax-backup.{timestamp_seconds}"));
        if collision > 0 {
            backup_name.push(format!(".{collision}"));
        }
        let candidate = target.absolute_path_for(&backup_name);
        let mut backup = match rustix::fs::openat(
            &target.parent,
            &backup_name,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        ) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => {
                return Err(SetupError::io(
                    "create shell configuration backup",
                    rustix_error(error),
                ));
            }
        };
        backup
            .write_all(&source.bytes)
            .map_err(|error| SetupError::io("write shell configuration backup", error))?;
        preserve_source_metadata(&backup, source, "preserve shell backup")?;
        backup
            .sync_all()
            .map_err(|error| SetupError::io("sync shell configuration backup", error))?;
        sync_directory(&target.parent)?;
        let backup = BackupFile {
            file: backup,
            name: backup_name,
            path: candidate,
        };
        validate_backup(target, &backup, source)?;
        return Ok(backup);
    }
    Err(SetupError::BackupNameExhausted)
}

struct BackupFile {
    file: File,
    name: OsString,
    path: PathBuf,
}

fn validate_backup(
    target: &AnchoredTarget,
    backup: &BackupFile,
    source: &SourceFile,
) -> Result<(), SetupError> {
    if named_file_is_current_at(
        target,
        &backup.file,
        &backup.name,
        &source.bytes,
        &source.metadata,
        "reinspect shell configuration backup",
    )? {
        ensure_no_unsupported_acl(&backup.file)?;
        return Ok(());
    }
    Err(SetupError::SourceChanged)
}

fn prepare_replacement(
    target: &AnchoredTarget,
    bytes: &[u8],
    source: Option<&SourceFile>,
) -> Result<TransactionFile, SetupError> {
    let mut created = None;
    for _ in 0..MAX_TRANSACTION_COLLISIONS {
        let name = next_transaction_name();
        match rustix::fs::openat(
            &target.parent,
            &name,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        ) {
            Ok(file) => {
                created = Some((File::from(file), name));
                break;
            }
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => {
                return Err(SetupError::io(
                    "create shell configuration transaction",
                    rustix_error(error),
                ));
            }
        }
    }
    let (file, name) = created.ok_or(SetupError::TransactionNameExhausted)?;
    let mut replacement = TransactionFile {
        file,
        name,
        bytes: bytes.to_vec(),
        metadata: None,
    };
    replacement
        .file
        .write_all(bytes)
        .map_err(|error| SetupError::io("write shell configuration transaction", error))?;
    match source {
        Some(source) => {
            preserve_source_metadata(&replacement.file, source, "preserve shell configuration")?;
        }
        None => {
            #[cfg(unix)]
            replacement
                .file
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| SetupError::io("secure shell configuration", error))?;
        }
    }
    replacement
        .file
        .sync_all()
        .map_err(|error| SetupError::io("sync shell configuration transaction", error))?;
    ensure_no_unsupported_acl(&replacement.file)?;
    replacement.metadata = Some(capture_preserved_metadata(&replacement.file)?);
    Ok(replacement)
}

fn next_transaction_name() -> OsString {
    let sequence = TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    OsString::from(format!(
        ".argmax-setup-quarantine.{}.{sequence}",
        std::process::id()
    ))
}

struct TransactionFile {
    file: File,
    name: OsString,
    bytes: Vec<u8>,
    metadata: Option<PreservedMetadata>,
}

fn preserve_source_metadata(
    file: &File,
    source: &SourceFile,
    operation: &'static str,
) -> Result<(), SetupError> {
    #[cfg(unix)]
    {
        rustix::fs::fchown(
            file,
            Some(rustix::fs::Uid::from_raw(source.metadata.uid)),
            Some(rustix::fs::Gid::from_raw(source.metadata.gid)),
        )
        .map_err(rustix_error)
        .map_err(|error| SetupError::io(operation, error))?;
        rustix::fs::fchmod(
            file,
            rustix::fs::Mode::from_raw_mode(
                source
                    .metadata
                    .mode
                    .try_into()
                    .expect("permission bits fit raw mode"),
            ),
        )
        .map_err(rustix_error)
        .map_err(|error| SetupError::io(operation, error))?;
        replace_extended_attributes(file, &source.metadata.extended_attributes, operation)?;
    }
    #[cfg(not(unix))]
    file.set_permissions(fs::Permissions::from_readonly(
        source.metadata.mode == 0o400,
    ))
    .map_err(|error| SetupError::io(operation, error))?;
    ensure_no_unsupported_acl(file)?;
    if capture_preserved_metadata(file)? != source.metadata {
        return Err(SetupError::UnsupportedMetadata);
    }
    Ok(())
}

fn publish_new(target: &AnchoredTarget, replacement: &TransactionFile) -> Result<(), SetupError> {
    if !transaction_is_current_at(target, replacement, &replacement.name)? {
        return Err(SetupError::SourceChanged);
    }
    atomic_move_noclobber(target, &replacement.name, &target.name).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            SetupError::SourceChanged
        } else {
            SetupError::io("install new shell configuration", error)
        }
    })?;
    if transaction_is_current_at(target, replacement, &target.name)? {
        return Ok(());
    }
    if file_identity_matches_at(
        target,
        &replacement.file,
        &target.name,
        "reinspect new shell configuration",
    )? {
        atomic_move_noclobber(target, &target.name, &replacement.name)
            .map_err(|error| SetupError::io("quarantine ambiguous shell configuration", error))?;
    }
    Err(SetupError::SourceChanged)
}

fn publish_replacement(
    target: &AnchoredTarget,
    source: &SourceFile,
    replacement: &mut TransactionFile,
) -> Result<(), SetupError> {
    if !transaction_is_current_at(target, replacement, &replacement.name)?
        || !source_is_current(target, source, &target.name)?
    {
        return Err(SetupError::SourceChanged);
    }
    atomic_exchange(target, &replacement.name, &target.name)
        .map_err(|error| SetupError::io("replace shell configuration", error))?;
    settle_replacement_exchange(target, source, replacement)
}

fn settle_replacement_exchange(
    target: &AnchoredTarget,
    source: &SourceFile,
    replacement: &TransactionFile,
) -> Result<(), SetupError> {
    let source_check = source_is_current(target, source, &replacement.name);
    let source_at_transaction = matches!(source_check, Ok(true));
    let replacement_identity_at_target = file_identity_matches_at(
        target,
        &replacement.file,
        &target.name,
        "inspect installed shell configuration",
    )?;
    let replacement_check = if replacement_identity_at_target {
        transaction_is_current_at(target, replacement, &target.name)
    } else {
        Ok(false)
    };
    let replacement_at_target = matches!(replacement_check, Ok(true));

    if source_at_transaction && replacement_at_target {
        // The verified old source remains at the random transaction pathname as
        // a quarantine. Deleting it by pathname would reintroduce an unlink race.
        return Ok(());
    }

    if source_at_transaction {
        // The transaction pathname or installed target became foreign. Restore
        // the verified source and retain the foreign non-directory entry at the
        // quarantine pathname.
        if path_is_exchangeable_non_directory_at(
            target,
            &target.name,
            "inspect ambiguous shell configuration",
        )? {
            atomic_exchange(target, &replacement.name, &target.name)
                .map_err(|error| SetupError::io("restore ambiguous shell configuration", error))?;
        }
        source_check?;
        replacement_check?;
        return Err(SetupError::SourceChanged);
    }

    if !replacement_identity_at_target {
        // An editor published after setup. Its target remains authoritative;
        // every displaced or ambiguous pathname is retained.
        return source_check.and(Err(SetupError::SourceChanged));
    }

    if path_is_exclusive_regular_at(
        target,
        &replacement.name,
        "inspect displaced shell configuration",
    )? {
        // Either replacement metadata became ambiguous or the source changed in
        // the final pre-exchange window. Exchange the displaced regular file
        // back and retain both names; no pathname is unlinked.
        atomic_exchange(target, &replacement.name, &target.name)
            .map_err(|error| SetupError::io("restore changed shell configuration", error))?;
    }
    source_check?;
    replacement_check?;
    Err(SetupError::SourceChanged)
}

fn source_is_current(
    target: &AnchoredTarget,
    source: &SourceFile,
    name: &OsStr,
) -> Result<bool, SetupError> {
    if !named_file_is_current_at(
        target,
        &source.file,
        name,
        &source.bytes,
        &source.metadata,
        "reinspect shell configuration",
    )? {
        return Ok(false);
    }
    ensure_no_unsupported_acl(&source.file)?;
    Ok(true)
}

fn transaction_is_current_at(
    target: &AnchoredTarget,
    replacement: &TransactionFile,
    name: &OsStr,
) -> Result<bool, SetupError> {
    let metadata = replacement
        .metadata
        .as_ref()
        .expect("prepared transaction metadata is present");
    if !named_file_is_current_at(
        target,
        &replacement.file,
        name,
        &replacement.bytes,
        metadata,
        "reinspect shell configuration transaction",
    )? {
        return Ok(false);
    }
    ensure_no_unsupported_acl(&replacement.file)?;
    Ok(true)
}

fn named_file_is_current_at(
    target: &AnchoredTarget,
    file: &File,
    name: &OsStr,
    bytes: &[u8],
    metadata: &PreservedMetadata,
    operation: &'static str,
) -> Result<bool, SetupError> {
    Ok(file_is_exclusively_named_at(target, file, name, operation)?
        && capture_preserved_metadata(file)? == *metadata
        && anchored_bytes_match(file, bytes, operation)?)
}

fn anchored_bytes_match(
    anchored: &File,
    expected: &[u8],
    operation: &'static str,
) -> Result<bool, SetupError> {
    let mut file = anchored
        .try_clone()
        .map_err(|error| SetupError::io(operation, error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| SetupError::io(operation, error))?;
    let mut bytes = Vec::with_capacity(expected.len());
    file.take(u64::try_from(MAX_SHELL_FILE_BYTES + 1).expect("setup bound fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|error| SetupError::io(operation, error))?;
    Ok(bytes == expected)
}

fn file_is_exclusively_named_at(
    target: &AnchoredTarget,
    file: &File,
    name: &OsStr,
    operation: &'static str,
) -> Result<bool, SetupError> {
    let anchored = file
        .metadata()
        .map_err(|error| SetupError::io(operation, error))?;
    #[cfg(unix)]
    {
        Ok(file_identity_matches_at(target, file, name, operation)?
            && anchored.is_file()
            && anchored.nlink() == 1)
    }
    #[cfg(not(unix))]
    {
        let _ = (anchored, name);
        Ok(false)
    }
}

fn file_identity_matches_at(
    target: &AnchoredTarget,
    file: &File,
    name: &OsStr,
    operation: &'static str,
) -> Result<bool, SetupError> {
    let anchored = file
        .metadata()
        .map_err(|error| SetupError::io(operation, error))?;
    let named = match open_named_at(target, name) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP) => return Ok(false),
        Err(error) => return Err(SetupError::io(operation, rustix_error(error))),
    };
    let named = named
        .metadata()
        .map_err(|error| SetupError::io(operation, error))?;
    #[cfg(unix)]
    {
        Ok(named.is_file()
            && !named.file_type().is_symlink()
            && named.dev() == anchored.dev()
            && named.ino() == anchored.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (anchored, named);
        Ok(false)
    }
}

fn path_is_exclusive_regular_at(
    target: &AnchoredTarget,
    name: &OsStr,
    operation: &'static str,
) -> Result<bool, SetupError> {
    let file = match open_named_at(target, name) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP) => return Ok(false),
        Err(error) => return Err(SetupError::io(operation, rustix_error(error))),
    };
    let metadata = file
        .metadata()
        .map_err(|error| SetupError::io(operation, error))?;
    #[cfg(unix)]
    {
        Ok(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.nlink() == 1)
    }
    #[cfg(not(unix))]
    {
        Ok(metadata.is_file() && !metadata.file_type().is_symlink())
    }
}

fn path_is_exchangeable_non_directory_at(
    target: &AnchoredTarget,
    name: &OsStr,
    operation: &'static str,
) -> Result<bool, SetupError> {
    match rustix::fs::statat(&target.parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => Ok(rustix::fs::FileType::from_raw_mode(metadata.st_mode)
            != rustix::fs::FileType::Directory),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(SetupError::io(operation, rustix_error(error))),
    }
}

fn open_named_at(target: &AnchoredTarget, name: &OsStr) -> Result<File, rustix::io::Errno> {
    rustix::fs::openat(
        &target.parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
}

fn named_path_exists(target: &AnchoredTarget, name: &OsStr) -> Result<bool, SetupError> {
    match rustix::fs::statat(&target.parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(SetupError::io(
            "reinspect shell configuration",
            rustix_error(error),
        )),
    }
}

fn atomic_exchange(target: &AnchoredTarget, left: &OsStr, right: &OsStr) -> Result<(), io::Error> {
    rustix::fs::renameat_with(
        &target.parent,
        left,
        &target.parent,
        right,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(rustix_error)
}

fn atomic_move_noclobber(
    target: &AnchoredTarget,
    source: &OsStr,
    destination: &OsStr,
) -> Result<(), io::Error> {
    rustix::fs::renameat_with(
        &target.parent,
        source,
        &target.parent,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(rustix_error)
}

fn rustix_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

fn sync_directory(directory: &File) -> Result<(), SetupError> {
    directory
        .sync_all()
        .map_err(|error| SetupError::io("sync shell configuration directory", error))
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    use super::*;
    use crate::integration::{BEGIN_MARKER, END_MARKER, activation_line};

    fn marked_source(unmanaged_bytes: usize, shell: Shell) -> Vec<u8> {
        let mut source = vec![b'x'; unmanaged_bytes];
        source.push(b'\n');
        source.extend_from_slice(BEGIN_MARKER.as_bytes());
        source.push(b'\n');
        source.extend_from_slice(activation_line(shell).as_bytes());
        source.push(b'\n');
        source.extend_from_slice(END_MARKER.as_bytes());
        source.push(b'\n');
        source
    }

    fn legacy_source(unmanaged_bytes: usize) -> Vec<u8> {
        const LEGACY: &[u8] = b"eval \"$(argmax init bash)\"\n";
        let comment_bytes = unmanaged_bytes - LEGACY.len() - 1;
        let mut source = vec![b'#'; comment_bytes];
        source.push(b'\n');
        source.extend_from_slice(LEGACY);
        source
    }

    fn assert_rejected_without_backup(path: &Path, source: &[u8], timestamp: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
        let target = SetupTarget::new(Shell::Bash, path).unwrap();
        assert!(matches!(
            target.install(timestamp),
            Err(SetupError::ConfigTooLarge)
        ));
        assert_eq!(fs::read(path).unwrap(), source);
        assert!(
            !path
                .with_file_name(format!(".bashrc.argmax-backup.{timestamp}"))
                .exists()
        );
    }

    fn retained_quarantine_bytes(parent: &Path) -> Vec<Vec<u8>> {
        fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| {
                entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b".argmax-setup-quarantine.")
            })
            .map(|entry| fs::read(entry.path()).unwrap())
            .collect()
    }

    #[test]
    fn detects_only_supported_shell_basenames_with_active_precedence() {
        assert_eq!(
            detect_setup_shell(Some(OsStr::new("fish")), Some(OsStr::new("/bin/zsh"))).unwrap(),
            Shell::Fish
        );
        assert_eq!(
            detect_setup_shell(None, Some(OsStr::new("/opt/homebrew/bin/bash"))).unwrap(),
            Shell::Bash
        );
        assert!(matches!(
            detect_setup_shell(Some(OsStr::new("sh")), Some(OsStr::new("/bin/zsh"))),
            Err(SetupError::UnsupportedShell)
        ));
    }

    #[test]
    fn resolves_zdotdir_and_xdg_fish_paths_without_sourcing_them() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path();
        let zdir = home.join("z-config");
        let xdg = home.join("xdg");
        assert_eq!(
            SetupTarget::from_environment(Shell::Zsh, home, Some(&zdir), None)
                .unwrap()
                .path(),
            zdir.join(".zshrc")
        );
        assert_eq!(
            SetupTarget::from_environment(Shell::Fish, home, None, Some(&xdg))
                .unwrap()
                .path(),
            xdg.join("fish/config.fish")
        );

        let current = home.join("workspace");
        let relative = Path::new("relative-zdotdir");
        assert_eq!(
            SetupTarget::from_environment_at(
                Shell::Zsh,
                Path::new("irrelevant-home"),
                Some(relative),
                Some(Path::new("irrelevant-xdg")),
                Some(&current),
            )
            .unwrap()
            .path(),
            current.join(relative).join(".zshrc")
        );
        assert_eq!(
            SetupTarget::from_environment_at(
                Shell::Zsh,
                Path::new("irrelevant-home"),
                Some(Path::new("")),
                Some(Path::new("irrelevant-xdg")),
                None,
            )
            .unwrap()
            .path(),
            Path::new("/.zshrc")
        );

        for target in [
            SetupTarget::from_environment(Shell::Bash, home, Some(relative), Some(relative))
                .unwrap(),
            SetupTarget::from_environment(Shell::Fish, home, Some(relative), Some(&xdg)).unwrap(),
        ] {
            assert!(target.path().is_absolute());
        }
        assert_eq!(
            SetupTarget::from_environment(Shell::Fish, home, None, Some(relative))
                .unwrap()
                .path(),
            home.join(".config/fish/config.fish")
        );
    }

    #[test]
    fn installs_once_with_exact_backup_and_preserved_permissions() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        fs::write(&path, original).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let target = SetupTarget::new(Shell::Bash, &path).unwrap();

        let first = target.install(42).unwrap();
        assert!(first.changed());
        let backup = first.backup().unwrap();
        assert_eq!(fs::read(backup).unwrap(), original);
        let installed = fs::read_to_string(&path).unwrap();
        assert!(installed.starts_with("export DEAN=Pelton\n\n"));
        assert!(installed.contains(BEGIN_MARKER));
        assert!(installed.contains(activation_line(Shell::Bash)));
        assert!(installed.contains(END_MARKER));
        #[cfg(unix)]
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);

        let second = target.install(43).unwrap();
        assert!(!second.changed());
        assert!(second.backup().is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), installed);
        assert!(!path.with_file_name(".bashrc.argmax-backup.43").exists());
    }

    #[test]
    fn creates_private_fish_parent_and_config() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("xdg/fish/config.fish");
        let target = SetupTarget::new(Shell::Fish, &path).unwrap();
        let outcome = target.install(1).unwrap();
        assert!(outcome.changed());
        assert!(outcome.backup().is_none());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!(
                "{BEGIN_MARKER}\n{}\n{END_MARKER}\n",
                activation_line(Shell::Fish)
            )
        );
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
                0o700
            );
            assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_without_modifying_their_targets() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let target_path = temporary.path().join("real");
        let link = temporary.path().join(".zshrc");
        fs::write(&target_path, b"preserved\n").unwrap();
        symlink(&target_path, &link).unwrap();
        let target = SetupTarget::new(Shell::Zsh, link).unwrap();
        assert!(matches!(target.install(1), Err(SetupError::Io { .. })));
        assert_eq!(fs::read(target_path).unwrap(), b"preserved\n");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_ancestor_without_touching_victim_tree() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let victim = temporary.path().join("victim");
        let victim_subdirectory = victim.join("sub");
        fs::create_dir_all(&victim_subdirectory).unwrap();
        let victim_config = victim_subdirectory.join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        fs::write(&victim_config, original).unwrap();
        let link = temporary.path().join("link");
        symlink(&victim, &link).unwrap();
        let escaped_target = link.join("sub/.bashrc");

        assert!(matches!(
            SetupTarget::new(Shell::Bash, escaped_target)
                .unwrap()
                .install(20),
            Err(SetupError::UnsafeFileType)
        ));
        assert_eq!(fs::read(&victim_config).unwrap(), original);
        assert!(
            !victim_subdirectory
                .join("..bashrc.argmax-setup.lock")
                .exists()
        );
        assert!(
            !victim_subdirectory
                .join(".bashrc.argmax-backup.20")
                .exists()
        );
    }

    #[test]
    fn preserves_existing_backup_on_name_collision() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let occupied = temporary.path().join(".bashrc.argmax-backup.7");
        fs::write(&path, b"alias ll='ls -l'\n").unwrap();
        fs::write(&occupied, b"Troy's backup\n").unwrap();
        let outcome = SetupTarget::new(Shell::Bash, &path)
            .unwrap()
            .install(7)
            .unwrap();
        assert_eq!(fs::read(occupied).unwrap(), b"Troy's backup\n");
        assert_eq!(
            outcome.backup().unwrap().file_name().unwrap(),
            ".bashrc.argmax-backup.7.1"
        );
    }

    #[test]
    fn rejects_oversized_and_malformed_files_before_backup() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        fs::write(&path, vec![b'x'; MAX_SHELL_CONFIG_BYTES + 1]).unwrap();
        let target = SetupTarget::new(Shell::Bash, &path).unwrap();
        assert!(matches!(target.install(8), Err(SetupError::ConfigTooLarge)));
        assert!(!path.with_file_name(".bashrc.argmax-backup.8").exists());

        fs::write(&path, format!("{BEGIN_MARKER}\nmissing end\n")).unwrap();
        assert!(matches!(
            target.install(9),
            Err(SetupError::MalformedConfig(_))
        ));
        assert!(!path.with_file_name(".bashrc.argmax-backup.9").exists());
    }

    #[test]
    fn maximum_unmanaged_config_remains_idempotent_after_setup() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let original = vec![b'x'; MAX_SHELL_CONFIG_BYTES];
        fs::write(&path, &original).unwrap();
        let target = SetupTarget::new(Shell::Bash, &path).unwrap();

        let first = target.install(10).unwrap();
        assert!(first.changed());
        let installed = fs::read(&path).unwrap();
        assert!(installed.len() > MAX_SHELL_CONFIG_BYTES);
        assert!(installed.len() <= MAX_SHELL_FILE_BYTES);

        let second = target.install(11).unwrap();
        assert!(!second.changed());
        assert_eq!(fs::read(path).unwrap(), installed);
    }

    #[test]
    fn appended_unmanaged_limit_is_exact_and_checked_before_backup() {
        let temporary = tempfile::tempdir().unwrap();
        let appended_path = temporary.path().join("appended/.bashrc");
        fs::create_dir(appended_path.parent().unwrap()).unwrap();
        fs::write(&appended_path, vec![b'x'; MAX_SHELL_CONFIG_BYTES]).unwrap();
        let appended = SetupTarget::new(Shell::Bash, &appended_path)
            .unwrap()
            .install(30)
            .unwrap();
        assert!(appended.changed());
        assert!(
            !SetupTarget::new(Shell::Bash, &appended_path)
                .unwrap()
                .install(31)
                .unwrap()
                .changed()
        );
        assert_rejected_without_backup(
            &temporary.path().join("appended-over/.bashrc"),
            &vec![b'x'; MAX_SHELL_CONFIG_BYTES + 1],
            32,
        );
    }

    #[test]
    fn replaced_unmanaged_limit_is_exact_and_checked_before_backup() {
        let temporary = tempfile::tempdir().unwrap();
        let replaced_path = temporary.path().join("replaced/.bashrc");
        fs::create_dir(replaced_path.parent().unwrap()).unwrap();
        fs::write(
            &replaced_path,
            marked_source(MAX_SHELL_CONFIG_BYTES, Shell::Zsh),
        )
        .unwrap();
        let replaced = SetupTarget::new(Shell::Bash, &replaced_path)
            .unwrap()
            .install(33)
            .unwrap();
        assert!(replaced.changed());
        assert!(replaced.backup().is_some());
        let replaced_over_path = temporary.path().join("replaced-over/.bashrc");
        fs::create_dir(replaced_over_path.parent().unwrap()).unwrap();
        assert_rejected_without_backup(
            &replaced_over_path,
            &marked_source(MAX_SHELL_CONFIG_BYTES + 1, Shell::Zsh),
            34,
        );
    }

    #[test]
    fn marked_unchanged_unmanaged_limit_is_exact_and_checked_before_backup() {
        let temporary = tempfile::tempdir().unwrap();
        let unchanged_path = temporary.path().join("unchanged/.bashrc");
        fs::create_dir(unchanged_path.parent().unwrap()).unwrap();
        fs::write(
            &unchanged_path,
            marked_source(MAX_SHELL_CONFIG_BYTES, Shell::Bash),
        )
        .unwrap();
        let unchanged = SetupTarget::new(Shell::Bash, &unchanged_path)
            .unwrap()
            .install(35)
            .unwrap();
        assert!(!unchanged.changed());
        let unchanged_over_path = temporary.path().join("unchanged-over/.bashrc");
        fs::create_dir(unchanged_over_path.parent().unwrap()).unwrap();
        assert_rejected_without_backup(
            &unchanged_over_path,
            &marked_source(MAX_SHELL_CONFIG_BYTES + 1, Shell::Bash),
            36,
        );
    }

    #[test]
    fn legacy_unchanged_and_managed_block_limits_are_checked_before_backup() {
        let temporary = tempfile::tempdir().unwrap();
        let legacy_path = temporary.path().join("legacy/.bashrc");
        fs::create_dir(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, legacy_source(MAX_SHELL_CONFIG_BYTES)).unwrap();
        let legacy = SetupTarget::new(Shell::Bash, &legacy_path)
            .unwrap()
            .install(37)
            .unwrap();
        assert!(!legacy.changed());
        let legacy_over_path = temporary.path().join("legacy-over/.bashrc");
        fs::create_dir(legacy_over_path.parent().unwrap()).unwrap();
        assert_rejected_without_backup(
            &legacy_over_path,
            &legacy_source(MAX_SHELL_CONFIG_BYTES + 1),
            38,
        );

        let oversized_block_path = temporary.path().join("block-over/.bashrc");
        fs::create_dir(oversized_block_path.parent().unwrap()).unwrap();
        let mut oversized_block = Vec::from(BEGIN_MARKER.as_bytes());
        oversized_block.push(b'\n');
        oversized_block.extend(std::iter::repeat_n(b'x', MAX_MANAGED_BLOCK_BYTES));
        oversized_block.push(b'\n');
        oversized_block.extend_from_slice(END_MARKER.as_bytes());
        oversized_block.push(b'\n');
        assert_rejected_without_backup(&oversized_block_path, &oversized_block, 39);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fifo_without_waiting_for_a_writer() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
        let target = SetupTarget::new(Shell::Bash, path).unwrap();

        let started = Instant::now();
        assert!(matches!(
            target.install(12),
            Err(SetupError::UnsafeFileType)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_owner_group_access_and_special_bits() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        fs::write(&path, b"export DEAN=Pelton\n").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let parent_gid = fs::metadata(temporary.path()).unwrap().gid();
        let groups = Command::new("id").arg("-G").output().unwrap();
        assert!(groups.status.success());
        if let Some(group) = String::from_utf8(groups.stdout)
            .unwrap()
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .find(|group| *group != parent_gid)
        {
            rustix::fs::fchown(&file, None, Some(rustix::fs::Gid::from_raw(group))).unwrap();
        }
        file.set_permissions(fs::Permissions::from_mode(0o6750))
            .unwrap();
        let before = file.metadata().unwrap();

        let outcome = SetupTarget::new(Shell::Bash, &path)
            .unwrap()
            .install(13)
            .unwrap();
        let backup = outcome.backup().unwrap();
        for metadata in [fs::metadata(&path).unwrap(), fs::metadata(backup).unwrap()] {
            assert_eq!(metadata.uid(), before.uid());
            assert_eq!(metadata.gid(), before.gid());
            assert_eq!(metadata.mode() & 0o7777, before.mode() & 0o7777);
        }
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "hurd"
    ))]
    #[test]
    fn preserves_extended_attributes_on_active_config_and_backup() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        fs::write(&path, b"export DEAN=Pelton\n").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        #[cfg(target_vendor = "apple")]
        let attribute_name = OsStr::new("com.argmax.greendale");
        #[cfg(not(target_vendor = "apple"))]
        let attribute_name = OsStr::new("user.argmax.greendale");
        rustix::fs::fsetxattr(
            &file,
            attribute_name,
            b"Troy and Abed",
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
        let before = read_extended_attributes(&file).unwrap();

        let outcome = SetupTarget::new(Shell::Bash, &path)
            .unwrap()
            .install(14)
            .unwrap();
        for candidate in [&path, outcome.backup().unwrap()] {
            let candidate = File::open(candidate).unwrap();
            assert_eq!(read_extended_attributes(&candidate).unwrap(), before);
        }
    }

    #[cfg(target_os = "macos")]
    fn add_macos_acl(path: &Path) {
        let identity = Command::new("/usr/bin/id").arg("-un").output().unwrap();
        assert!(identity.status.success());
        let user = String::from_utf8(identity.stdout).unwrap();
        let status = Command::new("/bin/chmod")
            .arg("+a")
            .arg(format!("user:{} allow read", user.trim()))
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_macos_acl_before_backup_or_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        fs::write(&path, original).unwrap();
        add_macos_acl(&path);

        assert!(matches!(
            SetupTarget::new(Shell::Bash, &path).unwrap().install(15),
            Err(SetupError::UnsupportedMetadata)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!path.with_file_name(".bashrc.argmax-backup.15").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn acl_inspection_stays_on_fd_after_parent_and_name_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("config");
        let detached = temporary.path().join("detached-config");
        fs::create_dir(&parent).unwrap();
        let path = parent.join(".bashrc");
        fs::write(&path, b"protected\n").unwrap();
        add_macos_acl(&path);

        let target = AnchoredTarget::open(&path).unwrap();
        let protected = open_named_at(&target, &target.name).unwrap();
        fs::rename(&parent, &detached).unwrap();
        fs::create_dir(&parent).unwrap();
        let rebound_victim = parent.join(".bashrc");
        fs::write(&rebound_victim, b"victim\n").unwrap();

        assert!(matches!(
            ensure_no_unsupported_acl(&protected),
            Err(SetupError::UnsupportedMetadata)
        ));
        ensure_no_unsupported_acl(&File::open(&rebound_victim).unwrap()).unwrap();
        assert_eq!(fs::read(rebound_victim).unwrap(), b"victim\n");
        assert_eq!(fs::read(detached.join(".bashrc")).unwrap(), b"protected\n");
    }

    #[test]
    fn replaced_or_hardlinked_transaction_is_never_published() {
        let temporary = tempfile::tempdir().unwrap();
        let original = b"export DEAN=Pelton\n";

        let replaced_path = temporary.path().join("replaced-bashrc");
        fs::write(&replaced_path, original).unwrap();
        let target = AnchoredTarget::open(&replaced_path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();
        let edit = edit_config(&source.bytes, Shell::Bash).unwrap();
        let mut replacement = prepare_replacement(&target, edit.content(), Some(&source)).unwrap();
        let transaction_path = target.absolute_path_for(&replacement.name);
        let foreign_path = temporary.path().join("foreign-transaction");
        let foreign = b"foreign transaction bytes\n";
        fs::write(&foreign_path, foreign).unwrap();
        fs::rename(&foreign_path, &transaction_path).unwrap();
        assert!(matches!(
            publish_replacement(&target, &source, &mut replacement),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&replaced_path).unwrap(), original);
        assert_eq!(fs::read(&transaction_path).unwrap(), foreign);

        let hardlinked_path = temporary.path().join("hardlinked-bashrc");
        fs::write(&hardlinked_path, original).unwrap();
        let target = AnchoredTarget::open(&hardlinked_path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();
        let edit = edit_config(&source.bytes, Shell::Bash).unwrap();
        let mut replacement = prepare_replacement(&target, edit.content(), Some(&source)).unwrap();
        let transaction_path = target.absolute_path_for(&replacement.name);
        let alias = temporary.path().join("transaction-hardlink");
        fs::hard_link(&transaction_path, &alias).unwrap();
        assert!(matches!(
            publish_replacement(&target, &source, &mut replacement),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&hardlinked_path).unwrap(), original);
        assert_eq!(fs::read(&alias).unwrap(), edit.content());

        let raced_path = temporary.path().join("raced-bashrc");
        fs::write(&raced_path, original).unwrap();
        let target = AnchoredTarget::open(&raced_path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();
        let edit = edit_config(&source.bytes, Shell::Bash).unwrap();
        let replacement = prepare_replacement(&target, edit.content(), Some(&source)).unwrap();
        let transaction_path = target.absolute_path_for(&replacement.name);
        let foreign_path = temporary.path().join("raced-foreign-transaction");
        fs::write(&foreign_path, foreign).unwrap();
        fs::rename(&foreign_path, &transaction_path).unwrap();
        atomic_exchange(&target, &replacement.name, &target.name).unwrap();
        assert!(matches!(
            settle_replacement_exchange(&target, &source, &replacement),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&raced_path).unwrap(), original);
        assert_eq!(fs::read(&transaction_path).unwrap(), foreign);
    }

    #[test]
    fn replaced_or_hardlinked_backup_never_validates() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        fs::write(&path, original).unwrap();
        let target = AnchoredTarget::open(&path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();

        let backup = write_backup(&target, &source, 16).unwrap();
        let foreign_path = temporary.path().join("foreign-backup");
        let foreign = b"foreign backup bytes\n";
        fs::write(&foreign_path, foreign).unwrap();
        fs::rename(&foreign_path, &backup.path).unwrap();
        assert!(matches!(
            validate_backup(&target, &backup, &source),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&backup.path).unwrap(), foreign);

        let backup = write_backup(&target, &source, 17).unwrap();
        let alias = temporary.path().join("backup-hardlink");
        fs::hard_link(&backup.path, &alias).unwrap();
        assert!(matches!(
            validate_backup(&target, &backup, &source),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&backup.path).unwrap(), original);
        assert_eq!(fs::read(alias).unwrap(), original);
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn successful_exchange_retains_verified_source_quarantine() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        fs::write(&path, original).unwrap();

        SetupTarget::new(Shell::Bash, &path)
            .unwrap()
            .install(18)
            .unwrap();
        let quarantines = fs::read_dir(temporary.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|candidate| {
                candidate.file_name().is_some_and(|name| {
                    name.as_encoded_bytes()
                        .starts_with(b".argmax-setup-quarantine.")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(fs::read(&quarantines[0]).unwrap(), original);
    }

    #[test]
    fn post_exchange_editor_save_is_quarantined_without_loss() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let editor_path = temporary.path().join("editor-save");
        let original = b"export DEAN=Pelton\n";
        let editor = b"export DEAN=Vice\n";
        fs::write(&path, original).unwrap();
        fs::write(&editor_path, editor).unwrap();

        let target = AnchoredTarget::open(&path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();
        let edit = edit_config(&source.bytes, Shell::Bash).unwrap();
        let replacement = prepare_replacement(&target, edit.content(), Some(&source)).unwrap();
        let transaction_path = target.absolute_path_for(&replacement.name);
        atomic_exchange(&target, &replacement.name, &target.name).unwrap();
        fs::rename(&editor_path, &path).unwrap();

        assert!(matches!(
            settle_replacement_exchange(&target, &source, &replacement),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(&transaction_path).unwrap(), editor);
    }

    #[test]
    fn source_change_in_final_exchange_window_is_restored() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let editor_path = temporary.path().join("editor-save");
        let original = b"export DEAN=Pelton\n";
        let editor = b"export DEAN=Vice\n";
        fs::write(&path, original).unwrap();
        fs::write(&editor_path, editor).unwrap();

        let target = AnchoredTarget::open(&path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();
        let edit = edit_config(&source.bytes, Shell::Bash).unwrap();
        let replacement = prepare_replacement(&target, edit.content(), Some(&source)).unwrap();
        fs::rename(&editor_path, &path).unwrap();

        // Simulate the editor rename after publish_replacement's final source
        // check but before its exchange syscall.
        atomic_exchange(&target, &replacement.name, &target.name).unwrap();
        assert!(matches!(
            settle_replacement_exchange(&target, &source, &replacement),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&path).unwrap(), editor);
        drop(replacement);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_access_change_is_not_clobbered() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        fs::write(&path, original).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let target = AnchoredTarget::open(&path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();
        let edit = edit_config(&source.bytes, Shell::Bash).unwrap();
        let mut replacement = prepare_replacement(&target, edit.content(), Some(&source)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            publish_replacement(&target, &source, &mut replacement),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o7777, 0o600);
    }

    #[test]
    fn transaction_cleanup_never_unlinks_a_replacement_path() {
        let temporary = tempfile::tempdir().unwrap();
        let foreign = b"newer editor bytes\n";
        let path = temporary.path().join(".bashrc");
        let target = AnchoredTarget::open(&path).unwrap();
        let replacement = prepare_replacement(&target, b"owned bytes\n", None).unwrap();
        let named_path = target.absolute_path_for(&replacement.name);
        let foreign_path = temporary.path().join("foreign");
        fs::write(&foreign_path, foreign).unwrap();
        fs::rename(&foreign_path, &named_path).unwrap();

        drop(replacement);
        assert_eq!(fs::read(named_path).unwrap(), foreign);
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_validation_rejects_every_snapshot_race() {
        fn snapshot(path: &Path) -> (AnchoredTarget, File, SourceFile) {
            fs::write(path, b"eval \"$(argmax init bash)\"\n").unwrap();
            let target = AnchoredTarget::open(path).unwrap();
            let lock = acquire_setup_lock(&target).unwrap();
            let source = read_optional_source(&target).unwrap().unwrap();
            assert_eq!(
                edit_config(&source.bytes, Shell::Bash).unwrap().outcome(),
                EditOutcome::Unchanged
            );
            (target, lock, source)
        }

        let temporary = tempfile::tempdir().unwrap();

        let renamed = temporary.path().join("renamed-bashrc");
        let (target, lock, source) = snapshot(&renamed);
        let editor = temporary.path().join("editor-save");
        fs::write(&editor, b"export DEAN=Vice\n").unwrap();
        fs::rename(&editor, &renamed).unwrap();
        assert!(matches!(
            validate_unchanged(&target, &lock, Some(&source)),
            Err(SetupError::SourceChanged)
        ));

        let rewritten = temporary.path().join("rewritten-bashrc");
        let (target, lock, source) = snapshot(&rewritten);
        fs::write(&rewritten, b"export DEAN=Vice\n").unwrap();
        assert!(matches!(
            validate_unchanged(&target, &lock, Some(&source)),
            Err(SetupError::SourceChanged)
        ));

        let metadata_changed = temporary.path().join("metadata-bashrc");
        let (target, lock, source) = snapshot(&metadata_changed);
        let current_mode = fs::metadata(&metadata_changed).unwrap().mode() & 0o7777;
        let changed_mode = if current_mode == 0o600 { 0o640 } else { 0o600 };
        fs::set_permissions(&metadata_changed, fs::Permissions::from_mode(changed_mode)).unwrap();
        assert!(matches!(
            validate_unchanged(&target, &lock, Some(&source)),
            Err(SetupError::SourceChanged)
        ));

        let hardlinked = temporary.path().join("hardlinked-unchanged-bashrc");
        let (target, lock, source) = snapshot(&hardlinked);
        fs::hard_link(&hardlinked, temporary.path().join("unchanged-hardlink")).unwrap();
        assert!(matches!(
            validate_unchanged(&target, &lock, Some(&source)),
            Err(SetupError::SourceChanged)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn parent_swap_after_snapshot_cannot_redirect_unchanged_result() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("config");
        let detached = temporary.path().join("detached-config");
        let victim = temporary.path().join("victim");
        fs::create_dir(&parent).unwrap();
        fs::create_dir(&victim).unwrap();
        let path = parent.join(".bashrc");
        let victim_path = victim.join(".bashrc");
        fs::write(&path, b"eval \"$(argmax init bash)\"\n").unwrap();
        fs::write(&victim_path, b"export DEAN=Vice\n").unwrap();
        let target = AnchoredTarget::open(&path).unwrap();
        let lock = acquire_setup_lock(&target).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();

        fs::rename(&parent, &detached).unwrap();
        symlink(&victim, &parent).unwrap();

        assert!(matches!(
            validate_unchanged(&target, &lock, Some(&source)),
            Err(SetupError::SourceChanged)
        ));
        assert_eq!(fs::read(&victim_path).unwrap(), b"export DEAN=Vice\n");
        assert_eq!(
            fs::read(detached.join(".bashrc")).unwrap(),
            b"eval \"$(argmax init bash)\"\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_swap_before_exchange_keeps_publication_on_anchor() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("config");
        let detached = temporary.path().join("detached-config");
        let victim = temporary.path().join("victim");
        fs::create_dir(&parent).unwrap();
        fs::create_dir(&victim).unwrap();
        let path = parent.join(".bashrc");
        let victim_path = victim.join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        let victim_bytes = b"export DEAN=Vice\n";
        fs::write(&path, original).unwrap();
        fs::write(&victim_path, victim_bytes).unwrap();
        let target = AnchoredTarget::open(&path).unwrap();
        let source = read_optional_source(&target).unwrap().unwrap();
        let edit = edit_config(&source.bytes, Shell::Bash).unwrap();
        let mut replacement = prepare_replacement(&target, edit.content(), Some(&source)).unwrap();

        fs::rename(&parent, &detached).unwrap();
        symlink(&victim, &parent).unwrap();

        assert!(matches!(
            target.validate_parent(),
            Err(SetupError::SourceChanged)
        ));
        publish_replacement(&target, &source, &mut replacement).unwrap();
        assert_eq!(fs::read(&victim_path).unwrap(), victim_bytes);
        assert_eq!(fs::read(detached.join(".bashrc")).unwrap(), edit.content());
        assert!(matches!(
            target.validate_parent(),
            Err(SetupError::SourceChanged)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn post_publication_parent_rebind_restores_existing_source() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("config");
        let detached = temporary.path().join("detached-config");
        let victim = temporary.path().join("victim");
        fs::create_dir(&parent).unwrap();
        fs::create_dir(&victim).unwrap();
        let path = parent.join(".bashrc");
        let victim_path = victim.join(".bashrc");
        let original = b"export DEAN=Pelton\n";
        let victim_bytes = b"export DEAN=Vice\n";
        fs::write(&path, original).unwrap();
        fs::write(&victim_path, victim_bytes).unwrap();
        let installed = edit_config(original, Shell::Bash).unwrap().into_content();

        let result = SetupTarget::new(Shell::Bash, &path)
            .unwrap()
            .install_with_post_publication_hook(40, || {
                assert_eq!(fs::read(&path).unwrap(), installed);
                fs::rename(&parent, &detached).unwrap();
                symlink(&victim, &parent).unwrap();
            });

        assert!(matches!(result, Err(SetupError::SourceChanged)));
        assert_eq!(fs::read(&victim_path).unwrap(), victim_bytes);
        assert_eq!(fs::read(detached.join(".bashrc")).unwrap(), original);
        assert_eq!(retained_quarantine_bytes(&detached), vec![installed]);
    }

    #[cfg(unix)]
    #[test]
    fn post_publication_parent_rebind_restores_absence_and_retains_new_bytes() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("config");
        let detached = temporary.path().join("detached-config");
        let victim = temporary.path().join("victim");
        fs::create_dir(&parent).unwrap();
        fs::create_dir(&victim).unwrap();
        let path = parent.join(".bashrc");
        let victim_path = victim.join(".bashrc");
        let victim_bytes = b"export DEAN=Vice\n";
        fs::write(&victim_path, victim_bytes).unwrap();
        let installed = edit_config(&[], Shell::Bash).unwrap().into_content();

        let result = SetupTarget::new(Shell::Bash, &path)
            .unwrap()
            .install_with_post_publication_hook(41, || {
                assert_eq!(fs::read(&path).unwrap(), installed);
                fs::rename(&parent, &detached).unwrap();
                symlink(&victim, &parent).unwrap();
            });

        assert!(matches!(result, Err(SetupError::SourceChanged)));
        assert_eq!(fs::read(&victim_path).unwrap(), victim_bytes);
        assert!(!detached.join(".bashrc").exists());
        assert_eq!(retained_quarantine_bytes(&detached), vec![installed]);
    }
}
