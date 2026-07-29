//! Private, bounded, atomic configuration-file storage.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};
use tempfile::NamedTempFile;

use super::file::render_config_for_storage;
use super::{
    ConfigDocument, ConfigFileError, DEFAULT_CONFIG_TEMPLATE, MAX_CONFIG_BYTES, parse_config,
};

const CONFIG_FILE_NAME: &str = "config.toml";
const MAX_BACKUP_COLLISIONS: u8 = 100;

/// Platform paths considered for current and legacy configuration discovery.
#[derive(Clone, Eq, PartialEq)]
pub struct ConfigPaths {
    current: PathBuf,
    legacy: Box<[PathBuf]>,
}

impl ConfigPaths {
    /// Resolves the standard platform path and documented legacy locations.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigStoreError::NoPlatformDirectory`] when the host has no
    /// discoverable user home/config directory.
    pub fn discover() -> Result<Self, ConfigStoreError> {
        let project =
            ProjectDirs::from("", "", "argmax").ok_or(ConfigStoreError::NoPlatformDirectory)?;
        let base = BaseDirs::new().ok_or(ConfigStoreError::NoPlatformDirectory)?;
        Ok(Self::from_discovered(
            project.config_dir(),
            base.config_dir(),
            base.home_dir(),
            std::env::var_os("XDG_CONFIG_HOME"),
        ))
    }

    fn from_discovered(
        platform_config_dir: &Path,
        base_config_dir: &Path,
        home_dir: &Path,
        xdg_config_home: Option<OsString>,
    ) -> Self {
        let platform_current = platform_config_dir.join(CONFIG_FILE_NAME);
        let current = xdg_config_home
            .filter(|path| !path.is_empty() && Path::new(path).is_absolute())
            .map_or_else(
                || platform_current.clone(),
                |path| PathBuf::from(path).join("argmax").join(CONFIG_FILE_NAME),
            );
        let candidates = [
            platform_current,
            base_config_dir.join("argmax").join(CONFIG_FILE_NAME),
            home_dir.join(".argmax").join(CONFIG_FILE_NAME),
        ];
        let legacy = candidates
            .into_iter()
            .filter(|candidate| candidate != &current)
            .fold(Vec::new(), |mut unique, candidate| {
                if !unique.contains(&candidate) {
                    unique.push(candidate);
                }
                unique
            })
            .into_boxed_slice();
        Self { current, legacy }
    }

    /// Builds explicit paths for deterministic tests and embedded callers.
    #[must_use]
    pub fn new(current: impl Into<PathBuf>, legacy: Vec<PathBuf>) -> Self {
        let current = current.into();
        let legacy = legacy
            .into_iter()
            .filter(|candidate| candidate != &current)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { current, legacy }
    }

    /// Current schema location used for initialization and migration output.
    #[must_use]
    pub fn current(&self) -> &Path {
        &self.current
    }

    /// Existing path to read, preferring current over every legacy location.
    #[must_use]
    pub fn selected_existing(&self) -> Option<&Path> {
        std::iter::once(self.current.as_path())
            .chain(self.legacy.iter().map(PathBuf::as_path))
            .find(|path| path_may_exist(path))
    }
}

impl fmt::Debug for ConfigPaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigPaths")
            .field("current_path_bytes", &path_bytes(&self.current))
            .field("legacy_path_count", &self.legacy.len())
            .finish()
    }
}

/// One explicit configuration path with safe load/init/migration operations.
#[derive(Clone, Eq, PartialEq)]
pub struct ConfigStore {
    source: PathBuf,
    current: PathBuf,
}

impl ConfigStore {
    /// Uses an explicit path. This does not touch the filesystem.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            source: path.clone(),
            current: path,
        }
    }

    /// Discovers an existing current/legacy file, or selects the current path.
    ///
    /// # Errors
    ///
    /// Returns an error when platform directories cannot be resolved.
    pub fn discover() -> Result<Self, ConfigStoreError> {
        let paths = ConfigPaths::discover()?;
        let source = paths
            .selected_existing()
            .unwrap_or_else(|| paths.current())
            .to_path_buf();
        Ok(Self {
            source,
            current: paths.current().to_path_buf(),
        })
    }

    /// Selects an existing current/legacy path while retaining the current
    /// migration destination.
    #[must_use]
    pub fn from_paths(paths: &ConfigPaths) -> Self {
        let source = paths
            .selected_existing()
            .unwrap_or_else(|| paths.current())
            .to_path_buf();
        Self {
            source,
            current: paths.current().to_path_buf(),
        }
    }

    /// Exact configuration path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.effective_source()
    }

    /// Standard current-schema destination.
    #[must_use]
    pub fn current_path(&self) -> &Path {
        &self.current
    }

    /// Loads and validates the file, returning `None` when it does not exist.
    ///
    /// The final path is opened without following a symlink. A successfully
    /// opened regular file is tightened to user-only permissions before its
    /// bounded contents are read.
    ///
    /// # Errors
    ///
    /// Returns safe I/O, type, UTF-8, size, or schema errors.
    pub fn load(&self) -> Result<Option<ConfigDocument>, ConfigStoreError> {
        for _ in 0..3 {
            let source = self.effective_source().to_path_buf();
            if !path_may_exist(&source) {
                return Ok(None);
            }
            secure_existing_parent(&source)?;
            let lock = acquire_config_lock(&source)?;
            if self.effective_source() != source {
                drop(lock);
                continue;
            }
            if source == self.current {
                recover_migration_claim(&source)?;
            }
            let Some(file) = read_optional_private_file(&source)? else {
                return Ok(None);
            };
            let input =
                std::str::from_utf8(&file.bytes).map_err(|_| ConfigStoreError::InvalidUtf8)?;
            return parse_config(input)
                .map(Some)
                .map_err(ConfigStoreError::Config);
        }
        Err(ConfigStoreError::MigrationRecoveryRequired)
    }

    /// Creates the commented default without replacing an existing path.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent cannot be secured, the temporary file
    /// cannot be durably written, or the final atomic installation fails.
    pub fn init(&self) -> Result<InitOutcome, ConfigStoreError> {
        let source = self.effective_source();
        let parent = required_parent(source)?;
        secure_directory(parent)?;
        let _lock = acquire_config_lock(source)?;
        match write_new_file(source, DEFAULT_CONFIG_TEMPLATE.as_bytes()) {
            Ok(()) => Ok(InitOutcome::Created),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Ok(InitOutcome::AlreadyExists)
            }
            Err(error) => Err(ConfigStoreError::io("initialize configuration", error)),
        }
    }

    /// Backs up a legacy-schema file and atomically rewrites current TOML.
    ///
    /// A current-schema file is a no-op. The backup is written and synced before
    /// the source path is replaced; the source remains untouched on every earlier
    /// failure. The caller supplies wall-clock seconds solely for the backup name.
    ///
    /// # Errors
    ///
    /// Returns an error when load/validation, backup, render, or atomic replacement
    /// fails. No only usable source is deleted.
    pub fn migrate_if_needed(
        &self,
        timestamp_seconds: u64,
    ) -> Result<MigrationOutcome, ConfigStoreError> {
        let current_parent = required_parent(&self.current)?;
        secure_directory(current_parent)?;
        let _lock = acquire_config_lock(&self.current)?;
        let source = self.effective_source().to_path_buf();
        secure_existing_parent(&source)?;
        let _source_lock = if source == self.current {
            None
        } else {
            Some(acquire_config_lock(&source)?)
        };
        if source == self.current {
            recover_migration_claim(&source)?;
        }
        let Some(original) = read_optional_private_file(&source)? else {
            return Ok(MigrationOutcome::Unneeded);
        };
        let input =
            std::str::from_utf8(&original.bytes).map_err(|_| ConfigStoreError::InvalidUtf8)?;
        let document = parse_config(input).map_err(ConfigStoreError::Config)?;
        if !document.needs_migration() && source == self.current {
            return Ok(MigrationOutcome::Unneeded);
        }

        let source_parent = required_parent(&source)?;
        secure_directory(source_parent)?;
        let backup = write_timestamped_backup(&source, timestamp_seconds, &original.bytes)?;

        let rendered =
            render_config_for_storage(&document.settings).map_err(ConfigStoreError::Config)?;
        parse_config(&rendered).map_err(ConfigStoreError::Config)?;
        if source == self.current {
            replace_file_if_unchanged(
                &self.current,
                &original,
                rendered.as_bytes(),
                || {},
                || {},
                || {},
            )?;
        } else {
            let current_parent = required_parent(&self.current)?;
            secure_directory(current_parent)?;
            match install_if_source_unchanged(
                &self.current,
                &source,
                &original,
                rendered.as_bytes(),
                || {},
                || {},
            )? {
                ConditionalInstall::Installed => {}
                ConditionalInstall::DestinationExists => {
                    let Some(existing) = read_optional_private_file(&self.current)? else {
                        return Err(ConfigStoreError::MigrationDestinationExists);
                    };
                    let existing = std::str::from_utf8(&existing.bytes)
                        .map_err(|_| ConfigStoreError::MigrationDestinationExists)?;
                    let existing = parse_config(existing)
                        .map_err(|_| ConfigStoreError::MigrationDestinationExists)?;
                    if existing.source_schema != super::CURRENT_SCHEMA_VERSION
                        || existing.settings != document.settings
                    {
                        return Err(ConfigStoreError::MigrationDestinationExists);
                    }
                }
            }
        }
        Ok(MigrationOutcome::Migrated {
            backup,
            destination: self.current.clone(),
        })
    }

    fn effective_source(&self) -> &Path {
        if self.source != self.current && path_may_exist(&self.current) {
            &self.current
        } else {
            &self.source
        }
    }
}

impl fmt::Debug for ConfigStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigStore")
            .field("source_path_bytes", &path_bytes(&self.source))
            .field("current_path_bytes", &path_bytes(&self.current))
            .finish()
    }
}

/// Result of creating the default configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitOutcome {
    /// A new private file was installed.
    Created,
    /// A path already existed and was preserved byte-for-byte.
    AlreadyExists,
}

/// Result of applying schema migration.
#[derive(Clone, Eq, PartialEq)]
pub enum MigrationOutcome {
    /// No file existed or it already used the current schema.
    Unneeded,
    /// A backup was created before the current file was replaced.
    Migrated {
        /// Unique timestamped backup path.
        backup: PathBuf,
        /// Current-schema path installed or replaced.
        destination: PathBuf,
    },
}

impl fmt::Debug for MigrationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unneeded => formatter.write_str("Unneeded"),
            Self::Migrated {
                backup,
                destination,
            } => formatter
                .debug_struct("Migrated")
                .field("backup_path_bytes", &path_bytes(backup))
                .field("destination_path_bytes", &path_bytes(destination))
                .finish(),
        }
    }
}

/// Safe storage failure without configuration contents.
#[derive(Debug)]
pub enum ConfigStoreError {
    /// No standard home/config directory is available.
    NoPlatformDirectory,
    /// The configured path has no parent directory.
    MissingParent,
    /// A final path is a symlink or non-regular file.
    UnsafeFileType,
    /// A product directory is a symlink or non-directory.
    UnsafeDirectoryType,
    /// The file is not UTF-8 TOML.
    InvalidUtf8,
    /// TOML/schema failure.
    Config(ConfigFileError),
    /// Filesystem failure, classified without retaining a sensitive path.
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    /// No unique timestamped backup name was available.
    BackupNameExhausted,
    /// The current destination appeared after legacy discovery and was preserved.
    MigrationDestinationExists,
    /// The source changed after its backup was created and was preserved.
    MigrationSourceChanged,
    /// An interrupted or conflicting migration claim requires recovery.
    MigrationRecoveryRequired,
}

impl ConfigStoreError {
    fn io(operation: &'static str, error: io::Error) -> Self {
        let kind = error.kind();
        drop(error);
        Self::Io { operation, kind }
    }
}

impl fmt::Display for ConfigStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlatformDirectory => {
                formatter.write_str("no standard user configuration directory is available")
            }
            Self::MissingParent => formatter.write_str("configuration path has no parent"),
            Self::UnsafeFileType => {
                formatter.write_str("configuration path is not a safe regular file")
            }
            Self::UnsafeDirectoryType => {
                formatter.write_str("configuration directory is not a safe directory")
            }
            Self::InvalidUtf8 => formatter.write_str("configuration is not valid UTF-8"),
            Self::Config(error) => error.fmt(formatter),
            Self::Io { operation, kind } => write!(formatter, "{operation}: {kind:?}"),
            Self::BackupNameExhausted => {
                formatter.write_str("no unused timestamped configuration backup name is available")
            }
            Self::MigrationDestinationExists => formatter
                .write_str("current configuration appeared during migration and was preserved"),
            Self::MigrationSourceChanged => {
                formatter.write_str("configuration changed during migration and was preserved")
            }
            Self::MigrationRecoveryRequired => {
                formatter.write_str("an interrupted configuration migration requires recovery")
            }
        }
    }
}

impl Error for ConfigStoreError {}

fn required_parent(path: &Path) -> Result<&Path, ConfigStoreError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(ConfigStoreError::MissingParent)
}

struct PrivateFile {
    bytes: Vec<u8>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl PrivateFile {
    fn same_source(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

fn read_optional_private_file(path: &Path) -> Result<Option<PrivateFile>, ConfigStoreError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigStoreError::io("open configuration", error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| ConfigStoreError::io("inspect configuration", error))?;
    if !metadata.is_file() {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    set_file_private(&file)?;

    let limit = u64::try_from(MAX_CONFIG_BYTES + 1).expect("configuration limit fits u64");
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| ConfigStoreError::io("read configuration", error))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigStoreError::Config(ConfigFileError::TooLarge {
            bytes: bytes.len(),
        }));
    }
    Ok(Some(PrivateFile {
        bytes,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    }))
}

fn secure_existing_parent(path: &Path) -> Result<(), ConfigStoreError> {
    let parent = required_parent(path)?;
    let metadata = match fs::symlink_metadata(parent) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ConfigStoreError::io(
                "inspect configuration directory",
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ConfigStoreError::UnsafeDirectoryType);
    }
    #[cfg(unix)]
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| ConfigStoreError::io("secure configuration directory", error))?;
    Ok(())
}

fn secure_directory(path: &Path) -> Result<(), ConfigStoreError> {
    fs::create_dir_all(path)
        .map_err(|error| ConfigStoreError::io("create configuration directory", error))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| ConfigStoreError::io("inspect configuration directory", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ConfigStoreError::UnsafeDirectoryType);
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| ConfigStoreError::io("secure configuration directory", error))?;
    Ok(())
}

fn set_file_private(file: &File) -> Result<(), ConfigStoreError> {
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| ConfigStoreError::io("secure configuration", error))?;
    Ok(())
}

fn acquire_config_lock(path: &Path) -> Result<File, ConfigStoreError> {
    let lock_path = sidecar_path(path, ".lock")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    let file = options
        .open(&lock_path)
        .map_err(|error| ConfigStoreError::io("open configuration lock", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| ConfigStoreError::io("inspect configuration lock", error))?;
    if !metadata.is_file() {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    set_file_private(&file)?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(rustix_error)
        .map_err(|error| ConfigStoreError::io("lock configuration", error))?;

    let path_metadata = fs::symlink_metadata(&lock_path)
        .map_err(|error| ConfigStoreError::io("reinspect configuration lock", error))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if path_metadata.nlink() != 1
        || path_metadata.dev() != metadata.dev()
        || path_metadata.ino() != metadata.ino()
    {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    Ok(file)
}

fn sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, ConfigStoreError> {
    let name = path.file_name().ok_or(ConfigStoreError::MissingParent)?;
    let mut sidecar_name = OsString::from(".");
    sidecar_name.push(name);
    sidecar_name.push(suffix);
    Ok(path.with_file_name(sidecar_name))
}

fn private_temp_file(parent: &Path) -> Result<NamedTempFile, io::Error> {
    let file = NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    let mut temporary = private_temp_file(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    sync_directory(parent)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConditionalInstall {
    Installed,
    DestinationExists,
}

fn install_if_source_unchanged(
    destination: &Path,
    source: &Path,
    original: &PrivateFile,
    bytes: &[u8],
    before_validation: impl FnOnce(),
    after_install: impl FnOnce(),
) -> Result<ConditionalInstall, ConfigStoreError> {
    let parent = required_parent(destination)?;
    let mut temporary = private_temp_file(parent)
        .map_err(|error| ConfigStoreError::io("prepare migrated configuration", error))?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| ConfigStoreError::io("prepare migrated configuration", error))?;

    before_validation();
    if !source_matches(source, original)? {
        return Err(ConfigStoreError::MigrationSourceChanged);
    }
    let installed = match temporary.persist_noclobber(destination) {
        Ok(file) => file,
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            return Ok(ConditionalInstall::DestinationExists);
        }
        Err(error) => {
            return Err(ConfigStoreError::io(
                "install migrated configuration",
                error.error,
            ));
        }
    };
    sync_directory(parent)
        .map_err(|error| ConfigStoreError::io("sync migrated configuration", error))?;

    after_install();
    if source_matches(source, original)? {
        return Ok(ConditionalInstall::Installed);
    }
    quarantine_installed_file(destination, &installed, parent)?;
    Err(ConfigStoreError::MigrationSourceChanged)
}

fn source_matches(path: &Path, original: &PrivateFile) -> Result<bool, ConfigStoreError> {
    let Some(current) = read_optional_private_file(path)? else {
        return Ok(false);
    };
    Ok(current.same_source(original) && current.bytes == original.bytes)
}

fn quarantine_installed_file(
    path: &Path,
    installed: &File,
    parent: &Path,
) -> Result<(), ConfigStoreError> {
    let conflict = quarantine_path(path, path, parent)?;

    if path_matches_file(&conflict, installed)? {
        return Ok(());
    }
    match atomic_move_noclobber(&conflict, path) {
        Ok(()) => sync_directory(parent)
            .map_err(|error| ConfigStoreError::io("sync restored concurrent edit", error)),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(ConfigStoreError::MigrationRecoveryRequired)
        }
        Err(error) => Err(ConfigStoreError::io("restore concurrent edit", error)),
    }
}

fn replace_file_if_unchanged(
    path: &Path,
    original: &PrivateFile,
    bytes: &[u8],
    before_claim: impl FnOnce(),
    after_mismatch: impl FnOnce(),
    during_resolution: impl FnOnce(),
) -> Result<(), ConfigStoreError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(ConfigStoreError::MissingParent)?;
    let mut temporary = private_temp_file(parent)
        .map_err(|error| ConfigStoreError::io("prepare migrated configuration", error))?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| ConfigStoreError::io("prepare migrated configuration", error))?;

    let transaction = migration_transaction_path(path)?;
    let prepared = match temporary.persist_noclobber(&transaction) {
        Ok(file) => file,
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ConfigStoreError::MigrationRecoveryRequired);
        }
        Err(error) => {
            return Err(ConfigStoreError::io(
                "prepare configuration transaction",
                error.error,
            ));
        }
    };
    sync_directory(parent)
        .map_err(|error| ConfigStoreError::io("sync configuration transaction", error))?;

    before_claim();
    if let Err(error) = atomic_exchange(&transaction, path) {
        remove_transaction(&transaction, parent)?;
        return Err(ConfigStoreError::io(
            "exchange migrated configuration",
            error,
        ));
    }
    sync_directory(parent)
        .map_err(|error| ConfigStoreError::io("sync migrated configuration", error))?;

    let Ok(Some(claimed)) = read_optional_private_file(&transaction) else {
        resolve_changed_exchange(
            &transaction,
            path,
            parent,
            &prepared,
            bytes,
            during_resolution,
        )?;
        return Err(ConfigStoreError::MigrationRecoveryRequired);
    };
    if !claimed.same_source(original) || claimed.bytes != original.bytes {
        after_mismatch();
        resolve_changed_exchange(
            &transaction,
            path,
            parent,
            &prepared,
            bytes,
            during_resolution,
        )?;
        return Err(ConfigStoreError::MigrationSourceChanged);
    }
    remove_transaction(&transaction, parent)
}

fn migration_transaction_path(path: &Path) -> Result<PathBuf, ConfigStoreError> {
    sidecar_path(path, ".migration")
}

fn migration_conflict_path(path: &Path) -> Result<PathBuf, ConfigStoreError> {
    sidecar_path(path, ".migration-conflict")
}

fn migration_conflict_candidate(path: &Path, collision: u8) -> Result<PathBuf, ConfigStoreError> {
    if collision == 0 {
        return migration_conflict_path(path);
    }
    sidecar_path(path, &format!(".migration-conflict.{collision}"))
}

fn atomic_exchange(left: &Path, right: &Path) -> Result<(), io::Error> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        left,
        rustix::fs::CWD,
        right,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(rustix_error)
}

fn atomic_move_noclobber(source: &Path, destination: &Path) -> Result<(), io::Error> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(rustix_error)
}

fn rustix_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

fn resolve_changed_exchange(
    transaction: &Path,
    path: &Path,
    parent: &Path,
    prepared: &File,
    expected: &[u8],
    during_resolution: impl FnOnce(),
) -> Result<(), ConfigStoreError> {
    if !path_matches_file(path, prepared)? {
        return quarantine_transaction(transaction, path, parent);
    }
    during_resolution();
    atomic_exchange(transaction, path)
        .map_err(|error| ConfigStoreError::io("roll back configuration exchange", error))?;
    sync_directory(parent)
        .map_err(|error| ConfigStoreError::io("sync configuration rollback", error))?;
    if path_matches_file(transaction, prepared)? {
        let unchanged =
            read_optional_private_file(transaction)?.is_some_and(|file| file.bytes == expected);
        if unchanged {
            return quarantine_transaction(transaction, path, parent);
        }
    }

    atomic_exchange(transaction, path)
        .map_err(|error| ConfigStoreError::io("restore concurrent configuration edit", error))?;
    sync_directory(parent)
        .map_err(|error| ConfigStoreError::io("sync concurrent configuration edit", error))?;
    quarantine_transaction(transaction, path, parent)
}

fn quarantine_transaction(
    transaction: &Path,
    path: &Path,
    parent: &Path,
) -> Result<(), ConfigStoreError> {
    quarantine_path(transaction, path, parent).map(|_| ())
}

fn quarantine_path(
    source: &Path,
    config_path: &Path,
    parent: &Path,
) -> Result<PathBuf, ConfigStoreError> {
    for collision in 0..MAX_BACKUP_COLLISIONS {
        let candidate = migration_conflict_candidate(config_path, collision)?;
        match atomic_move_noclobber(source, &candidate) {
            Ok(()) => {
                sync_directory(parent).map_err(|error| {
                    ConfigStoreError::io("sync preserved migration conflict", error)
                })?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(ConfigStoreError::io("preserve migration conflict", error));
            }
        }
    }
    Err(ConfigStoreError::MigrationRecoveryRequired)
}

fn path_matches_file(path: &Path, file: &File) -> Result<bool, ConfigStoreError> {
    let file_metadata = file
        .metadata()
        .map_err(|error| ConfigStoreError::io("inspect migration file", error))?;
    let path_metadata = fs::symlink_metadata(path)
        .map_err(|error| ConfigStoreError::io("inspect migration path", error))?;
    #[cfg(unix)]
    {
        Ok(
            file_metadata.dev() == path_metadata.dev()
                && file_metadata.ino() == path_metadata.ino(),
        )
    }
    #[cfg(not(unix))]
    {
        Ok(file_metadata.len() == path_metadata.len()
            && file_metadata.modified().ok() == path_metadata.modified().ok())
    }
}

fn remove_transaction(transaction: &Path, parent: &Path) -> Result<(), ConfigStoreError> {
    fs::remove_file(transaction)
        .map_err(|error| ConfigStoreError::io("remove configuration transaction", error))?;
    sync_directory(parent)
        .map_err(|error| ConfigStoreError::io("sync configuration transaction removal", error))
}

fn recover_migration_claim(path: &Path) -> Result<(), ConfigStoreError> {
    let transaction = migration_transaction_path(path)?;
    let transaction_metadata = match fs::symlink_metadata(&transaction) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ConfigStoreError::io("inspect configuration claim", error));
        }
    };
    if transaction_metadata.file_type().is_symlink() || !transaction_metadata.is_file() {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if transaction_metadata.nlink() != 1 {
        return Err(ConfigStoreError::UnsafeFileType);
    }
    let Some(claimed) = read_optional_private_file(&transaction)? else {
        return Ok(());
    };
    let parent = required_parent(path)?;
    let Some(current) = read_optional_private_file(path)? else {
        atomic_move_noclobber(&transaction, path).map_err(|error| {
            ConfigStoreError::io("restore interrupted configuration migration", error)
        })?;
        return sync_directory(parent)
            .map_err(|error| ConfigStoreError::io("sync restored configuration", error));
    };
    if current.bytes == claimed.bytes {
        return remove_transaction(&transaction, parent);
    }
    let current_document = std::str::from_utf8(&current.bytes)
        .ok()
        .and_then(|input| parse_config(input).ok());
    if current_document.is_some() {
        return quarantine_transaction(&transaction, path, parent);
    }
    Err(ConfigStoreError::MigrationRecoveryRequired)
}

fn sync_directory(path: &Path) -> Result<(), io::Error> {
    File::open(path)?.sync_all()
}

fn write_timestamped_backup(
    path: &Path,
    timestamp: u64,
    bytes: &[u8],
) -> Result<PathBuf, ConfigStoreError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(CONFIG_FILE_NAME);
    for collision in 0..MAX_BACKUP_COLLISIONS {
        let suffix = if collision == 0 {
            String::new()
        } else {
            format!(".{collision}")
        };
        let candidate = path.with_file_name(format!("{name}.backup.{timestamp}{suffix}"));
        match write_new_file(&candidate, bytes) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(ConfigStoreError::io("write configuration backup", error));
            }
        }
    }
    Err(ConfigStoreError::BackupNameExhausted)
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

fn path_may_exist(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CURRENT_SCHEMA_VERSION, REDACTED_CREDENTIAL};
    use std::fmt::Write as _;
    use std::io::{Seek, SeekFrom};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn replace_and_sync(path: &Path, bytes: &[u8]) {
        let parent = path.parent().unwrap();
        let mut temporary = private_temp_file(parent).unwrap();
        temporary.write_all(bytes).unwrap();
        temporary.as_file_mut().sync_all().unwrap();
        drop(temporary.persist(path).unwrap());
        sync_directory(parent).unwrap();
    }

    #[test]
    fn init_is_private_atomic_and_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config").join(CONFIG_FILE_NAME);
        let store = ConfigStore::new(&path);

        assert_eq!(store.init().unwrap(), InitOutcome::Created);
        let original = fs::read(&path).unwrap();
        assert_eq!(original, DEFAULT_CONFIG_TEMPLATE.as_bytes());
        #[cfg(unix)]
        {
            assert_eq!(mode(path.parent().unwrap()), 0o700);
            assert_eq!(mode(&path), 0o600);
        }

        fs::write(&path, b"[core]\nversion = 2\n").unwrap();
        assert_eq!(store.init().unwrap(), InitOutcome::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"[core]\nversion = 2\n");
    }

    #[test]
    fn load_is_bounded_and_tightens_permissions() {
        let home = tempfile::tempdir().unwrap();
        let parent = home.path().join("argmax");
        fs::create_dir(&parent).unwrap();
        let path = parent.join(CONFIG_FILE_NAME);
        fs::write(&path, b"[core]\nversion = 2\n").unwrap();
        #[cfg(unix)]
        {
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        }

        let loaded = ConfigStore::new(&path).load().unwrap().unwrap();
        assert_eq!(loaded.source_schema, CURRENT_SCHEMA_VERSION);
        #[cfg(unix)]
        {
            assert_eq!(mode(&parent), 0o700);
            assert_eq!(mode(&path), 0o600);
        }

        fs::write(&path, vec![b'x'; MAX_CONFIG_BYTES + 1]).unwrap();
        assert!(matches!(
            ConfigStore::new(&path).load(),
            Err(ConfigStoreError::Config(ConfigFileError::TooLarge { .. }))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_is_never_followed_or_modified() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("target.toml");
        let link = home.path().join(CONFIG_FILE_NAME);
        fs::write(&target, b"Troy's irreplaceable config").unwrap();
        symlink(&target, &link).unwrap();

        assert!(ConfigStore::new(&link).load().is_err());
        assert_eq!(fs::read(&target).unwrap(), b"Troy's irreplaceable config");
    }

    #[test]
    fn migration_backs_up_before_replacing_and_is_repeatable() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        let source = b"[core]\nversion = 1\nmode = 'history'\n";
        fs::write(&path, source).unwrap();
        let store = ConfigStore::new(&path);

        let MigrationOutcome::Migrated {
            backup,
            destination,
        } = store.migrate_if_needed(1_234).unwrap()
        else {
            panic!("wanted migration")
        };
        assert_eq!(destination, path);
        assert_eq!(fs::read(&backup).unwrap(), source);
        let current = store.load().unwrap().unwrap();
        assert_eq!(current.source_schema, CURRENT_SCHEMA_VERSION);
        assert_eq!(current.settings.core.mode, crate::config::Mode::History);
        assert_eq!(
            store.migrate_if_needed(1_235).unwrap(),
            MigrationOutcome::Unneeded
        );
    }

    #[test]
    fn colliding_backup_names_never_overwrite_existing_data() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        fs::write(&path, b"[core]\nversion = 1\n").unwrap();
        fs::write(
            path.with_file_name("config.toml.backup.7"),
            b"Annie's existing backup",
        )
        .unwrap();

        let MigrationOutcome::Migrated { backup, .. } =
            ConfigStore::new(&path).migrate_if_needed(7).unwrap()
        else {
            panic!("wanted migration")
        };
        assert!(backup.ends_with("config.toml.backup.7.1"));
        assert_eq!(
            fs::read(path.with_file_name("config.toml.backup.7")).unwrap(),
            b"Annie's existing backup"
        );
    }

    #[test]
    fn path_discovery_prefers_current_then_legacy() {
        let home = tempfile::tempdir().unwrap();
        let current = home.path().join("new").join(CONFIG_FILE_NAME);
        let legacy = home.path().join("old").join(CONFIG_FILE_NAME);
        let paths = ConfigPaths::new(&current, vec![legacy.clone()]);
        assert_eq!(paths.selected_existing(), None);

        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"").unwrap();
        assert_eq!(paths.selected_existing(), Some(legacy.as_path()));
        let legacy_store = ConfigStore::from_paths(&paths);
        assert_eq!(legacy_store.path(), legacy);
        assert_eq!(legacy_store.current_path(), current);

        fs::create_dir_all(current.parent().unwrap()).unwrap();
        fs::write(&current, b"").unwrap();
        assert_eq!(paths.selected_existing(), Some(current.as_path()));
    }

    #[test]
    fn xdg_override_is_honored_and_platform_path_remains_legacy() {
        let paths = ConfigPaths::from_discovered(
            Path::new("/platform/argmax"),
            Path::new("/base/config"),
            Path::new("/home/troy"),
            Some(OsString::from("/xdg/config")),
        );
        assert_eq!(paths.current(), Path::new("/xdg/config/argmax/config.toml"));
        assert!(
            paths
                .legacy
                .iter()
                .any(|path| path == Path::new("/platform/argmax/config.toml"))
        );

        let relative = ConfigPaths::from_discovered(
            Path::new("/platform/argmax"),
            Path::new("/base/config"),
            Path::new("/home/troy"),
            Some(OsString::from("relative")),
        );
        assert_eq!(
            relative.current(),
            Path::new("/platform/argmax/config.toml")
        );
    }

    #[test]
    fn legacy_location_migrates_to_current_without_deleting_source() {
        let home = tempfile::tempdir().unwrap();
        let current = home.path().join("new").join(CONFIG_FILE_NAME);
        let legacy = home.path().join("old").join(CONFIG_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let source = b"[core]\nversion = 2\nmode = 'history'\n";
        fs::write(&legacy, source).unwrap();
        let paths = ConfigPaths::new(&current, vec![legacy.clone()]);
        let store = ConfigStore::from_paths(&paths);

        let MigrationOutcome::Migrated {
            backup,
            destination,
        } = store.migrate_if_needed(42).unwrap()
        else {
            panic!("wanted location migration")
        };
        assert_eq!(destination, current);
        assert_eq!(fs::read(&legacy).unwrap(), source);
        assert_eq!(fs::read(&backup).unwrap(), source);
        assert_eq!(
            store.load().unwrap().unwrap().settings.core.mode,
            crate::config::Mode::History
        );
        assert_eq!(store.path(), current);
        assert_eq!(
            store.migrate_if_needed(43).unwrap(),
            MigrationOutcome::Unneeded
        );
    }

    #[test]
    fn migration_preserves_credentials_endpoint_query_and_extra_fields() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        fs::write(
            &path,
            r#"
                [core]
                version = 1

                [ai.providers.greendale]
                endpoint = "https://example.invalid/v1?tenant=study-room"
                api_key = "Troy-and-Abed-secret"

                [ai.providers.greendale.extra_request_body]
                access_token = "Dean-token"
            "#,
        )
        .unwrap();

        ConfigStore::new(&path).migrate_if_needed(99).unwrap();
        let migrated = fs::read_to_string(&path).unwrap();
        assert!(migrated.contains("Troy-and-Abed-secret"));
        assert!(migrated.contains("tenant=study-room"));
        assert!(migrated.contains("Dean-token"));
        assert!(!migrated.contains(REDACTED_CREDENTIAL));
    }

    #[test]
    fn concurrent_equivalent_location_migration_is_successful() {
        let home = tempfile::tempdir().unwrap();
        let current = home.path().join("new").join(CONFIG_FILE_NAME);
        let legacy = home.path().join("old").join(CONFIG_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"[core]\nversion = 1\nmode = 'history'\n").unwrap();
        let paths = ConfigPaths::new(&current, vec![legacy]);
        let store = ConfigStore::from_paths(&paths);

        fs::create_dir_all(current.parent().unwrap()).unwrap();
        let mut equivalent = crate::config::Settings::default();
        equivalent.core.mode = crate::config::Mode::History;
        fs::write(&current, render_config_for_storage(&equivalent).unwrap()).unwrap();

        assert_eq!(
            store.migrate_if_needed(100).unwrap(),
            MigrationOutcome::Unneeded
        );
        assert_eq!(
            ConfigStore::new(&current).load().unwrap().unwrap().settings,
            equivalent
        );
    }

    #[test]
    fn conflicting_current_path_is_preserved_during_location_migration() {
        let home = tempfile::tempdir().unwrap();
        let current = home.path().join("new").join(CONFIG_FILE_NAME);
        let legacy = home.path().join("old").join(CONFIG_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"[core]\nversion = 1\nmode = 'history'\n").unwrap();
        let paths = ConfigPaths::new(&current, vec![legacy]);
        let store = ConfigStore::from_paths(&paths);

        fs::create_dir_all(current.parent().unwrap()).unwrap();
        let current_contents = b"[core]\nversion = 2\nmode = 'spec'\n";
        fs::write(&current, current_contents).unwrap();

        assert_eq!(
            store.migrate_if_needed(101).unwrap(),
            MigrationOutcome::Unneeded
        );
        assert_eq!(fs::read(&current).unwrap(), current_contents);
    }

    #[test]
    fn same_path_migration_preserves_an_edit_after_preparation() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        let original_bytes = b"[core]\nversion = 1\nmode = 'history'\n";
        fs::write(&path, original_bytes).unwrap();
        let original = read_optional_private_file(&path).unwrap().unwrap();
        let concurrent = b"[core]\nversion = 2\nmode = 'spec'\n";

        assert!(matches!(
            replace_file_if_unchanged(
                &path,
                &original,
                b"[core]\nversion = 2\nmode = 'history'\n",
                || {
                    assert!(path.exists());
                    assert!(migration_transaction_path(&path).unwrap().exists());
                    fs::write(&path, concurrent).unwrap();
                    File::open(&path).unwrap().sync_all().unwrap();
                },
                || {},
                || {},
            ),
            Err(ConfigStoreError::MigrationSourceChanged)
        ));
        assert_eq!(fs::read(&path).unwrap(), concurrent);
        assert!(!migration_transaction_path(&path).unwrap().exists());
    }

    #[test]
    fn second_edit_during_exchange_resolution_wins() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        let original_bytes = b"[core]\nversion = 1\nmode = 'history'\n";
        let first_edit = b"[core]\nversion = 2\nmode = 'spec'\ndebug = false\n";
        let latest_edit = b"[core]\nversion = 2\nmode = 'spec'\ndebug = true\n";
        fs::write(&path, original_bytes).unwrap();
        let original = read_optional_private_file(&path).unwrap().unwrap();

        assert!(matches!(
            replace_file_if_unchanged(
                &path,
                &original,
                b"[core]\nversion = 2\nmode = 'history'\n",
                || replace_and_sync(&path, first_edit),
                || {},
                || replace_and_sync(&path, latest_edit),
            ),
            Err(ConfigStoreError::MigrationSourceChanged)
        ));
        let loaded = ConfigStore::new(&path).load().unwrap().unwrap();
        assert_eq!(loaded.settings.core.mode, crate::config::Mode::Spec);
        assert!(loaded.settings.core.debug);
        assert!(!migration_transaction_path(&path).unwrap().exists());
    }

    #[test]
    fn in_place_edit_during_exchange_resolution_wins() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        let original_bytes = b"[core]\nversion = 1\nmode = 'history'\n";
        let first_edit = b"[core]\nversion = 2\nmode = 'spec'\ndebug = false\n";
        let latest_edit = b"[core]\nversion = 2\nmode = 'spec'\ndebug = true\n";
        fs::write(&path, original_bytes).unwrap();
        let original = read_optional_private_file(&path).unwrap().unwrap();

        assert!(matches!(
            replace_file_if_unchanged(
                &path,
                &original,
                b"[core]\nversion = 2\nmode = 'history'\n",
                || replace_and_sync(&path, first_edit),
                || {},
                || {
                    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
                    file.set_len(0).unwrap();
                    file.write_all(latest_edit).unwrap();
                    file.sync_all().unwrap();
                },
            ),
            Err(ConfigStoreError::MigrationSourceChanged)
        ));
        let loaded = ConfigStore::new(&path).load().unwrap().unwrap();
        assert_eq!(loaded.settings.core.mode, crate::config::Mode::Spec);
        assert!(loaded.settings.core.debug);
        assert_eq!(
            fs::read(migration_conflict_path(&path).unwrap()).unwrap(),
            first_edit
        );
    }

    #[test]
    fn location_migration_never_hides_a_newer_legacy_edit() {
        let home = tempfile::tempdir().unwrap();
        let legacy_parent = home.path().join("legacy");
        let current_parent = home.path().join("current");
        fs::create_dir(&legacy_parent).unwrap();
        fs::create_dir(&current_parent).unwrap();
        let legacy = legacy_parent.join(CONFIG_FILE_NAME);
        let current = current_parent.join(CONFIG_FILE_NAME);
        let original_bytes = b"[core]\nversion = 1\nmode = 'history'\n";
        let concurrent = b"[core]\nversion = 2\nmode = 'spec'\n";

        for edit_after_install in [false, true] {
            fs::write(&legacy, original_bytes).unwrap();
            let original = read_optional_private_file(&legacy).unwrap().unwrap();
            let before = || {
                if !edit_after_install {
                    fs::write(&legacy, concurrent).unwrap();
                }
            };
            let after = || {
                if edit_after_install {
                    fs::write(&legacy, concurrent).unwrap();
                }
            };
            assert!(matches!(
                install_if_source_unchanged(
                    &current,
                    &legacy,
                    &original,
                    b"[core]\nversion = 2\nmode = 'history'\n",
                    before,
                    after,
                ),
                Err(ConfigStoreError::MigrationSourceChanged)
            ));
            assert_eq!(fs::read(&legacy).unwrap(), concurrent);
            assert!(!current.exists());
        }
    }

    #[test]
    fn location_cleanup_quarantines_an_open_current_edit() {
        let home = tempfile::tempdir().unwrap();
        let legacy_parent = home.path().join("legacy");
        let current_parent = home.path().join("current");
        fs::create_dir(&legacy_parent).unwrap();
        fs::create_dir(&current_parent).unwrap();
        let legacy = legacy_parent.join(CONFIG_FILE_NAME);
        let current = current_parent.join(CONFIG_FILE_NAME);
        let original_bytes = b"[core]\nversion = 1\nmode = 'history'\n";
        let legacy_edit = b"[core]\nversion = 2\nmode = 'spec'\n";
        let current_edit = b"[core]\nversion = 2\nmode = 'spec'\ndebug = true\n";
        fs::write(&legacy, original_bytes).unwrap();
        let original = read_optional_private_file(&legacy).unwrap().unwrap();

        assert!(matches!(
            install_if_source_unchanged(
                &current,
                &legacy,
                &original,
                b"[core]\nversion = 2\nmode = 'history'\n",
                || {},
                || {
                    fs::write(&legacy, legacy_edit).unwrap();
                    let mut current_file = OpenOptions::new().write(true).open(&current).unwrap();
                    current_file.seek(SeekFrom::Start(0)).unwrap();
                    current_file.set_len(0).unwrap();
                    current_file.write_all(current_edit).unwrap();
                    current_file.sync_all().unwrap();
                },
            ),
            Err(ConfigStoreError::MigrationSourceChanged)
        ));
        assert!(!current.exists());
        assert_eq!(
            fs::read(migration_conflict_path(&current).unwrap()).unwrap(),
            current_edit
        );
        assert_eq!(fs::read(&legacy).unwrap(), legacy_edit);
    }

    #[test]
    fn simultaneous_same_path_migrations_are_serialized() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        fs::write(&path, b"[core]\nversion = 1\nmode = 'history'\n").unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let workers = [201_u64, 202].map(|timestamp| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                ConfigStore::new(path).migrate_if_needed(timestamp)
            })
        });
        barrier.wait();
        let outcomes = workers.map(|worker| worker.join().unwrap().unwrap());

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, MigrationOutcome::Migrated { .. }))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, MigrationOutcome::Unneeded))
                .count(),
            1
        );
        assert_eq!(
            ConfigStore::new(&path)
                .load()
                .unwrap()
                .unwrap()
                .source_schema,
            CURRENT_SCHEMA_VERSION
        );
        assert!(!migration_transaction_path(&path).unwrap().exists());
    }

    #[test]
    fn blocked_legacy_load_reselects_current_after_migration() {
        let home = tempfile::tempdir().unwrap();
        let current = home.path().join("current").join(CONFIG_FILE_NAME);
        let legacy = home.path().join("legacy").join(CONFIG_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let mut source = String::from(
            "[core]\nversion = 1\nmode = 'history'\n\
             [ai.providers.greendale.extra_request_body]\n",
        );
        for index in 0..45_000 {
            writeln!(source, "field_{index:05} = 'x'").unwrap();
        }
        fs::write(&legacy, source).unwrap();
        let paths = ConfigPaths::new(&current, vec![legacy]);
        let store = ConfigStore::from_paths(&paths);
        let worker_store = store.clone();
        let worker = thread::spawn(move || worker_store.migrate_if_needed(401));
        let backup = paths
            .legacy
            .first()
            .unwrap()
            .with_file_name("config.toml.backup.401");
        while !backup.exists() && !worker.is_finished() {
            thread::yield_now();
        }

        let loaded = store.load().unwrap().unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(loaded.source_schema, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.settings.core.mode, crate::config::Mode::History);
        assert_eq!(store.path(), current);
    }

    #[test]
    fn interrupted_exchange_states_recover_without_discarding_active_config() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        let transaction = migration_transaction_path(&path).unwrap();
        let legacy = b"[core]\nversion = 1\nmode = 'history'\n";
        let current = b"[core]\nversion = 2\nmode = 'history'\n";

        fs::write(&path, legacy).unwrap();
        fs::write(&transaction, current).unwrap();
        assert!(matches!(
            ConfigStore::new(&path).migrate_if_needed(301).unwrap(),
            MigrationOutcome::Migrated { .. }
        ));
        assert!(!transaction.exists());

        fs::write(&path, current).unwrap();
        fs::write(&transaction, legacy).unwrap();
        assert_eq!(
            ConfigStore::new(&path).migrate_if_needed(302).unwrap(),
            MigrationOutcome::Unneeded
        );
        assert_eq!(fs::read(&path).unwrap(), current);
        assert!(!transaction.exists());
    }

    #[test]
    fn ambiguous_interrupted_exchange_preserves_displaced_config() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        let transaction = migration_transaction_path(&path).unwrap();
        let active = b"[core]\nversion = 2\nmode = 'history'\n";
        let displaced = b"[core]\nversion = 2\nmode = 'spec'\ndebug = true\n";
        fs::write(&path, active).unwrap();
        fs::write(&transaction, displaced).unwrap();

        let loaded = ConfigStore::new(&path).load().unwrap().unwrap();

        assert_eq!(loaded.settings.core.mode, crate::config::Mode::History);
        assert_eq!(fs::read(&path).unwrap(), active);
        assert_eq!(
            fs::read(migration_conflict_path(&path).unwrap()).unwrap(),
            displaced
        );
        assert!(!transaction.exists());
    }

    #[test]
    fn migration_outcome_debug_omits_paths() {
        let outcome = MigrationOutcome::Migrated {
            backup: PathBuf::from("/tmp/Troy-secret-backup"),
            destination: PathBuf::from("/tmp/Abed-secret-destination"),
        };
        let debug = format!("{outcome:?}");
        assert!(!debug.contains("Troy"));
        assert!(!debug.contains("Abed"));
        assert!(debug.contains("backup_path_bytes"));
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_configuration_is_rejected_without_chmod() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("source.toml");
        let path = home.path().join(CONFIG_FILE_NAME);
        fs::write(&source, b"[core]\nversion = 2\n").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        fs::hard_link(&source, &path).unwrap();

        assert!(matches!(
            ConfigStore::new(&path).load(),
            Err(ConfigStoreError::UnsafeFileType)
        ));
        assert_eq!(mode(&source), 0o644);
    }

    #[test]
    fn debug_and_errors_never_retain_file_paths_or_contents() {
        let store = ConfigStore::new("/tmp/hunter2/secret-config.toml");
        let debug = format!("{store:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("secret-config"));

        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(CONFIG_FILE_NAME);
        fs::write(&path, b"[core\npassword = 'study-room-secret'").unwrap();
        let error = ConfigStore::new(&path).load().unwrap_err();
        let message = format!("{error:?} {error}");
        assert!(!message.contains("study-room-secret"));
        assert!(!message.contains(path.to_string_lossy().as_ref()));
    }
}
