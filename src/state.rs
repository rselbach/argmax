//! Private, versioned persistence for small runtime state.
//!
//! Runtime state is deliberately separate from user configuration.  Every
//! mutation takes the same cross-process lock, reloads the latest state, and
//! atomically replaces the state file before releasing the lock.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};
use serde::{Deserialize, Deserializer, Serialize};
use tempfile::NamedTempFile;

use crate::updater::{UpdateState, UpdateStateError};

/// Current on-disk runtime-state schema.
pub const CURRENT_STATE_VERSION: u32 = 2;
/// Maximum state or legacy-state input size: 256 KiB.
pub const MAX_STATE_BYTES: usize = 256 * 1024;

const STATE_FILE_NAME: &str = "state.toml";
const MAX_BACKUP_COLLISIONS: u8 = 100;

/// Last interactive completion mode selected by the user.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LastMode {
    /// Specification completion mode.
    Spec,
    /// Shell-history search mode.
    History,
}

/// Small runtime state shared by terminal sessions.
#[derive(Clone, Default)]
pub struct RuntimeState {
    last_mode: Option<LastMode>,
    updater: UpdateState,
}

impl RuntimeState {
    /// Builds runtime state from validated components.
    #[must_use]
    pub const fn new(last_mode: Option<LastMode>, updater: UpdateState) -> Self {
        Self { last_mode, updater }
    }

    /// Last selected mode, or none when no selection has been persisted.
    #[must_use]
    pub const fn last_mode(&self) -> Option<LastMode> {
        self.last_mode
    }

    /// Validated automatic-updater state.
    #[must_use]
    pub const fn updater(&self) -> &UpdateState {
        &self.updater
    }
}

impl fmt::Debug for RuntimeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeState")
            .field("last_mode", &self.last_mode)
            .field("updater", &self.updater)
            .finish()
    }
}

/// Platform and legacy state locations.
#[derive(Clone, Eq, PartialEq)]
pub struct StatePaths {
    current: PathBuf,
    legacy_toml: Box<[PathBuf]>,
    legacy_state_json: Box<[PathBuf]>,
    legacy_update_json: Box<[PathBuf]>,
}

impl StatePaths {
    /// Discovers the platform data file and documented legacy locations.
    ///
    /// # Errors
    ///
    /// Returns [`StateStoreError::NoPlatformDirectory`] when a user data or
    /// home directory cannot be resolved.
    pub fn discover() -> Result<Self, StateStoreError> {
        let project =
            ProjectDirs::from("", "", "argmax").ok_or(StateStoreError::NoPlatformDirectory)?;
        let iris = ProjectDirs::from("", "", "iris").ok_or(StateStoreError::NoPlatformDirectory)?;
        let base = BaseDirs::new().ok_or(StateStoreError::NoPlatformDirectory)?;
        let current = project.data_dir().join(STATE_FILE_NAME);
        let mut candidates = Vec::new();
        if let Some(xdg_data) = std::env::var_os("XDG_DATA_HOME") {
            let xdg_data = PathBuf::from(xdg_data);
            if xdg_data.is_absolute() {
                candidates.push(xdg_data.join("argmax").join(STATE_FILE_NAME));
                candidates.push(xdg_data.join("iris").join(STATE_FILE_NAME));
            }
        }
        candidates.extend([
            project.config_dir().join(STATE_FILE_NAME),
            base.config_dir().join("argmax").join(STATE_FILE_NAME),
            base.home_dir()
                .join(".local/share/argmax")
                .join(STATE_FILE_NAME),
            base.home_dir().join(".argmax").join(STATE_FILE_NAME),
            iris.data_dir().join(STATE_FILE_NAME),
            iris.config_dir().join(STATE_FILE_NAME),
            base.home_dir()
                .join(".local/share/iris")
                .join(STATE_FILE_NAME),
        ]);
        let legacy_toml = candidates
            .into_iter()
            .filter(|candidate| candidate != &current)
            .fold(Vec::new(), |mut paths, candidate| {
                if !paths.contains(&candidate) {
                    paths.push(candidate);
                }
                paths
            });
        let argmax_legacy = base.home_dir().join(".argmax");
        let iris_legacy = base.home_dir().join(".iris");
        Ok(Self::with_legacy_json_candidates(
            current,
            legacy_toml,
            vec![
                argmax_legacy.join("state.json"),
                iris_legacy.join("state.json"),
            ],
            vec![
                argmax_legacy.join("update_state.json"),
                iris_legacy.join("update_state.json"),
            ],
        ))
    }

    /// Builds explicit paths for deterministic tests and embedded callers.
    #[must_use]
    pub fn new(
        current: impl Into<PathBuf>,
        legacy_toml: Vec<PathBuf>,
        legacy_state_json: Option<PathBuf>,
        legacy_update_json: Option<PathBuf>,
    ) -> Self {
        Self::with_legacy_json_candidates(
            current,
            legacy_toml,
            legacy_state_json.into_iter().collect(),
            legacy_update_json.into_iter().collect(),
        )
    }

    /// Builds explicit paths with ordered candidate lists for each legacy JSON
    /// source. At most the first existing state and updater source is imported.
    #[must_use]
    pub fn with_legacy_json_candidates(
        current: impl Into<PathBuf>,
        legacy_toml: Vec<PathBuf>,
        legacy_state_json: Vec<PathBuf>,
        legacy_update_json: Vec<PathBuf>,
    ) -> Self {
        let current = current.into();
        let legacy_toml = legacy_toml
            .into_iter()
            .filter(|candidate| candidate != &current)
            .fold(Vec::new(), |mut paths, candidate| {
                if !paths.contains(&candidate) {
                    paths.push(candidate);
                }
                paths
            })
            .into_boxed_slice();
        let legacy_state_json = deduplicate_paths(legacy_state_json, &current);
        let legacy_update_json = deduplicate_paths(legacy_update_json, &current);
        Self {
            current,
            legacy_toml,
            legacy_state_json: legacy_state_json.into_boxed_slice(),
            legacy_update_json: legacy_update_json.into_boxed_slice(),
        }
    }

    /// Current state path.
    #[must_use]
    pub fn current(&self) -> &Path {
        &self.current
    }
}

impl fmt::Debug for StatePaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StatePaths")
            .field("current_path_bytes", &path_bytes(&self.current))
            .field("legacy_toml_count", &self.legacy_toml.len())
            .field("legacy_state_json_count", &self.legacy_state_json.len())
            .field("legacy_update_json_count", &self.legacy_update_json.len())
            .finish()
    }
}

/// One runtime-state store.
#[derive(Clone, Eq, PartialEq)]
pub struct RuntimeStateStore {
    paths: StatePaths,
}

impl RuntimeStateStore {
    /// Uses one explicit current path without legacy discovery.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            paths: StatePaths::new(path, Vec::new(), None, None),
        }
    }

    /// Discovers platform and legacy paths without touching their contents.
    ///
    /// # Errors
    ///
    /// Returns an error when standard user directories are unavailable.
    pub fn discover() -> Result<Self, StateStoreError> {
        Ok(Self {
            paths: StatePaths::discover()?,
        })
    }

    /// Uses an explicit path set.
    #[must_use]
    pub const fn from_paths(paths: StatePaths) -> Self {
        Self { paths }
    }

    /// Current state path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.paths.current()
    }

    /// Loads bounded state. Missing or corrupt contents return defaults with an
    /// inspectable status and never affect configuration.
    ///
    /// # Errors
    ///
    /// Returns only filesystem or path-security failures. Parse and schema
    /// failures are represented by [`StateLoadStatus::Corrupt`].
    pub fn load(&self) -> Result<LoadedState, StateStoreError> {
        let parent = required_parent(self.path())?;
        if !path_may_exist(parent) && !path_may_exist(self.path()) {
            return Ok(LoadedState::missing());
        }
        secure_existing_directory(parent)?;
        let _lock = acquire_lock(self.path())?;
        load_unlocked(self.path())
    }

    /// Persists only a selected mode, preserving every updater field.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, I/O failures, or an existing state
    /// source that requires explicit migration or repair.
    pub fn store_mode(&self, mode: LastMode) -> Result<RuntimeState, StateStoreError> {
        self.update_current(|state| state.last_mode = Some(mode))
    }

    /// Merges updater state without rolling timestamps or the notified version
    /// backward and without modifying the last selected mode.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, I/O failures, or an existing state
    /// source that requires explicit migration or repair.
    pub fn store_updater(&self, proposed: &UpdateState) -> Result<RuntimeState, StateStoreError> {
        self.update_current(|state| {
            state.updater = proposed.merged_for_persistence(&state.updater);
        })
    }

    /// Atomically stores a two-phase updater proposal only if the on-disk
    /// updater snapshot still equals `expected`.
    ///
    /// This is the persistence boundary for check reservations and notification
    /// claims. A superseded proposal does not write anything.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, I/O failures, or state requiring
    /// explicit migration or repair.
    pub fn compare_and_store_updater(
        &self,
        expected: &UpdateState,
        proposed: &UpdateState,
    ) -> Result<UpdateStoreOutcome, StateStoreError> {
        let parent = required_parent(self.path())?;
        secure_directory(parent)?;
        let lock = acquire_lock(self.path())?;
        let loaded = load_unlocked(self.path())?;
        ensure_writable_status(loaded.status)?;
        if !same_update_state(&loaded.state.updater, expected) {
            return Ok(UpdateStoreOutcome::Superseded(loaded.state.updater));
        }
        let mut state = loaded.state;
        state.updater = proposed.merged_for_persistence(&state.updater);
        write_state(self.path(), &state, WriteDisposition::Replace, &lock)?;
        Ok(UpdateStoreOutcome::Persisted(state))
    }

    /// Migrates an in-place legacy TOML file or imports documented legacy state
    /// only when the current path is absent.
    ///
    /// Every usable source is durably copied to a unique timestamped backup
    /// before a validated current file is atomically installed. Sources are
    /// never renamed or deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when a source is unsafe, corrupt, oversized, cannot be
    /// backed up, or cannot be installed durably.
    pub fn migrate_if_needed(
        &self,
        timestamp_seconds: u64,
    ) -> Result<StateMigrationOutcome, StateStoreError> {
        let parent = required_parent(self.path())?;
        secure_directory(parent)?;
        let lock = acquire_lock(self.path())?;

        if let Some(current) = read_optional_private(self.path())? {
            let parsed = parse_toml(&current.bytes).map_err(StateStoreError::InvalidSource)?;
            if parsed.version == CURRENT_STATE_VERSION {
                return Ok(StateMigrationOutcome::Unneeded);
            }
            let backup = write_timestamped_backup(self.path(), timestamp_seconds, &current.bytes)?;
            if !source_matches(self.path(), &current)? {
                return Err(StateStoreError::MigrationSourceChanged);
            }
            write_state(self.path(), &parsed.state, WriteDisposition::Replace, &lock)?;
            return Ok(StateMigrationOutcome::Migrated {
                source_count: 1,
                backups: vec![backup].into_boxed_slice(),
            });
        }

        let sources = self.read_legacy_sources()?;
        if sources.is_empty() {
            return Ok(StateMigrationOutcome::Unneeded);
        }
        let state = merge_legacy_sources(&sources)?;
        let mut backups = Vec::with_capacity(sources.len());
        for source in &sources {
            backups.push(write_timestamped_backup(
                &source.path,
                timestamp_seconds,
                &source.file.bytes,
            )?);
        }
        for source in &sources {
            if !source_matches(&source.path, &source.file)? {
                return Err(StateStoreError::MigrationSourceChanged);
            }
        }

        match write_state(self.path(), &state, WriteDisposition::Create, &lock) {
            Ok(()) => Ok(StateMigrationOutcome::Migrated {
                source_count: sources.len(),
                backups: backups.into_boxed_slice(),
            }),
            Err(StateStoreError::Io {
                kind: io::ErrorKind::AlreadyExists,
                ..
            }) => Ok(StateMigrationOutcome::Unneeded),
            Err(error) => Err(error),
        }
    }

    fn update_current(
        &self,
        mutate: impl FnOnce(&mut RuntimeState),
    ) -> Result<RuntimeState, StateStoreError> {
        let parent = required_parent(self.path())?;
        secure_directory(parent)?;
        let lock = acquire_lock(self.path())?;
        let loaded = load_unlocked(self.path())?;
        ensure_writable_status(loaded.status)?;
        let mut state = loaded.state;
        mutate(&mut state);
        write_state(self.path(), &state, WriteDisposition::Replace, &lock)?;
        Ok(state)
    }

    fn read_legacy_sources(&self) -> Result<Vec<LegacySource>, StateStoreError> {
        for path in &self.paths.legacy_toml {
            if let Some(file) = read_optional_private(path)? {
                return Ok(vec![LegacySource {
                    path: path.clone(),
                    file,
                    format: LegacyFormat::Toml,
                }]);
            }
        }

        let mut sources = Vec::new();
        for (paths, format) in [
            (&self.paths.legacy_state_json, LegacyFormat::StateJson),
            (&self.paths.legacy_update_json, LegacyFormat::UpdateJson),
        ] {
            for path in paths {
                let Some(file) = read_optional_private(path)? else {
                    continue;
                };
                sources.push(LegacySource {
                    path: path.clone(),
                    file,
                    format,
                });
                break;
            }
        }
        Ok(sources)
    }
}

impl fmt::Debug for RuntimeStateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeStateStore")
            .field("paths", &self.paths)
            .finish()
    }
}

/// Result of a bounded state load.
#[derive(Clone)]
pub struct LoadedState {
    /// Valid state, or defaults when the source was missing/corrupt.
    pub state: RuntimeState,
    /// Inspectable source disposition.
    pub status: StateLoadStatus,
}

impl LoadedState {
    fn missing() -> Self {
        Self {
            state: RuntimeState::new(None, UpdateState::default()),
            status: StateLoadStatus::Missing,
        }
    }
}

impl fmt::Debug for LoadedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedState")
            .field("state", &self.state)
            .field("status", &self.status)
            .finish()
    }
}

/// State-file disposition from a load.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateLoadStatus {
    /// No current state file exists.
    Missing,
    /// Current schema loaded successfully.
    Current,
    /// A readable older schema requires explicit migration.
    Legacy {
        /// Schema number found, with a missing version treated as version 1.
        version: u32,
    },
    /// Contents were ignored and defaults returned.
    Corrupt(StateCorruption),
}

/// Content-free state corruption category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateCorruption {
    /// Input exceeded [`MAX_STATE_BYTES`].
    TooLarge,
    /// Input was not valid UTF-8.
    InvalidUtf8,
    /// TOML or JSON syntax/schema was invalid.
    InvalidDocument,
    /// A newer binary wrote an unsupported schema.
    UnsupportedVersion,
    /// Stored updater fields failed validation.
    InvalidUpdater,
}

/// Outcome of an updater compare-and-store transaction.
#[must_use]
pub enum UpdateStoreOutcome {
    /// The proposal was durably installed.
    Persisted(RuntimeState),
    /// Another writer changed updater state first. The winner is returned.
    Superseded(UpdateState),
}

impl fmt::Debug for UpdateStoreOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Persisted(state) => formatter.debug_tuple("Persisted").field(state).finish(),
            Self::Superseded(state) => formatter.debug_tuple("Superseded").field(state).finish(),
        }
    }
}

/// Outcome of state migration/import.
#[must_use]
pub enum StateMigrationOutcome {
    /// No source existed or current state already used the current schema.
    Unneeded,
    /// Current state was installed after all sources were backed up.
    Migrated {
        /// Number of source files represented by the migration.
        source_count: usize,
        /// Timestamped backups retained beside their sources.
        backups: Box<[PathBuf]>,
    },
}

impl fmt::Debug for StateMigrationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unneeded => formatter.write_str("Unneeded"),
            Self::Migrated {
                source_count,
                backups,
            } => formatter
                .debug_struct("Migrated")
                .field("source_count", source_count)
                .field(
                    "backup_path_bytes",
                    &backups
                        .iter()
                        .map(|path| path_bytes(path))
                        .collect::<Vec<_>>(),
                )
                .finish(),
        }
    }
}

/// Redacted runtime-state storage failure.
pub enum StateStoreError {
    /// No standard user data/home directory is available.
    NoPlatformDirectory,
    /// A configured state path has no usable parent.
    MissingParent,
    /// A state, backup, or lock path is unsafe.
    UnsafeFileType,
    /// A state directory is a symlink or non-directory.
    UnsafeDirectoryType,
    /// Existing state must be migrated before a mutation.
    NeedsMigration,
    /// Existing state is corrupt and was preserved.
    InvalidSource(StateCorruption),
    /// No unique backup name remained.
    BackupNameExhausted,
    /// A migration source changed after it was backed up and was preserved.
    MigrationSourceChanged,
    /// The process-wide lock pathname no longer names the locked inode.
    LockReplaced,
    /// Filesystem failure classified without retaining a path.
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
}

impl StateStoreError {
    fn io(operation: &'static str, error: io::Error) -> Self {
        let kind = error.kind();
        drop(error);
        Self::Io { operation, kind }
    }
}

impl fmt::Debug for StateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for StateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlatformDirectory => {
                formatter.write_str("no standard user data directory is available")
            }
            Self::MissingParent => formatter.write_str("runtime state path has no parent"),
            Self::UnsafeFileType => {
                formatter.write_str("runtime state path is not a safe regular file")
            }
            Self::UnsafeDirectoryType => {
                formatter.write_str("runtime state directory is not a safe directory")
            }
            Self::NeedsMigration => {
                formatter.write_str("runtime state requires migration before it can be written")
            }
            Self::InvalidSource(corruption) => {
                write!(formatter, "runtime state is invalid: {corruption:?}")
            }
            Self::BackupNameExhausted => {
                formatter.write_str("no unused runtime-state backup name is available")
            }
            Self::MigrationSourceChanged => {
                formatter.write_str("runtime state changed during migration and was preserved")
            }
            Self::LockReplaced => {
                formatter.write_str("runtime state lock was replaced; write was preserved")
            }
            Self::Io { operation, kind } => write!(formatter, "{operation}: {kind:?}"),
        }
    }
}

impl Error for StateStoreError {}

#[derive(Deserialize, Default)]
struct RawState {
    #[serde(default, alias = "schema-version", alias = "schema_version")]
    version: Option<u32>,
    #[serde(default, alias = "last-mode", alias = "lastMode", alias = "mode")]
    last_mode: Option<LastMode>,
    #[serde(default)]
    core: RawCore,
    #[serde(
        default,
        alias = "update",
        alias = "update-state",
        alias = "update_state"
    )]
    updater: RawUpdater,
    #[serde(flatten)]
    root_update: RawUpdater,
}

#[derive(Deserialize, Default)]
struct RawCore {
    #[serde(default, alias = "last-mode", alias = "lastMode", alias = "mode")]
    last_mode: Option<LastMode>,
}

#[derive(Deserialize, Default)]
struct RawUpdater {
    #[serde(
        rename = "last_reserved_check_ms",
        default,
        alias = "last-reserved-check-ms",
        alias = "lastReservedCheckMs",
        alias = "reserved_check_ms",
        alias = "reservedCheckMs"
    )]
    reserved_check_ms: Option<FlexibleTimestamp>,
    #[serde(
        rename = "last_completed_check_ms",
        default,
        alias = "last-completed-check-ms",
        alias = "lastCompletedCheckMs",
        alias = "completed_check_ms",
        alias = "completedCheckMs"
    )]
    completed_check_ms: Option<FlexibleTimestamp>,
    #[serde(
        rename = "last_check_ms",
        default,
        alias = "last-check-ms",
        alias = "lastCheckMs"
    )]
    check_ms: Option<FlexibleTimestamp>,
    #[serde(
        rename = "last_check",
        default,
        alias = "last-check",
        alias = "lastCheck"
    )]
    check_seconds: Option<FlexibleTimestamp>,
    #[serde(
        rename = "last_notified_version",
        default,
        alias = "last-notified-version",
        alias = "lastNotifiedVersion",
        alias = "notified_version",
        alias = "notifiedVersion",
        alias = "seen-version",
        alias = "seen_version",
        alias = "seenVersion"
    )]
    notified_version: Option<String>,
}

#[derive(Clone, Copy)]
struct FlexibleTimestamp(u64);

impl<'de> Deserialize<'de> for FlexibleTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TimestampVisitor;

        impl serde::de::Visitor<'_> for TimestampVisitor {
            type Value = FlexibleTimestamp;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a non-negative timestamp integer or digit string")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(FlexibleTimestamp(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                u64::try_from(value)
                    .map(FlexibleTimestamp)
                    .map_err(|_| E::custom("timestamp must not be negative"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                value
                    .parse::<u64>()
                    .map(FlexibleTimestamp)
                    .map_err(|_| E::custom("timestamp string must contain decimal digits"))
            }
        }

        deserializer.deserialize_any(TimestampVisitor)
    }
}

#[derive(Serialize)]
struct StoredState<'a> {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_mode: Option<LastMode>,
    updater: StoredUpdater<'a>,
}

#[derive(Serialize)]
struct StoredUpdater<'a> {
    #[serde(
        rename = "last_reserved_check_ms",
        skip_serializing_if = "Option::is_none"
    )]
    reserved_check_ms: Option<u64>,
    #[serde(
        rename = "last_completed_check_ms",
        skip_serializing_if = "Option::is_none"
    )]
    completed_check_ms: Option<u64>,
    #[serde(
        rename = "last_notified_version",
        skip_serializing_if = "Option::is_none"
    )]
    notified_version: Option<&'a str>,
}

struct ParsedState {
    version: u32,
    state: RuntimeState,
}

struct LegacySource {
    path: PathBuf,
    file: PrivateFile,
    format: LegacyFormat,
}

#[derive(Clone, Copy)]
enum LegacyFormat {
    Toml,
    StateJson,
    UpdateJson,
}

#[derive(Clone, Copy)]
enum WriteDisposition {
    Create,
    Replace,
}

fn load_unlocked(path: &Path) -> Result<LoadedState, StateStoreError> {
    let file = match read_optional_private(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(LoadedState::missing()),
        Err(StateStoreError::InvalidSource(corruption)) => {
            return Ok(LoadedState {
                state: RuntimeState::default(),
                status: StateLoadStatus::Corrupt(corruption),
            });
        }
        Err(error) => return Err(error),
    };
    match parse_toml(&file.bytes) {
        Ok(parsed) if parsed.version == CURRENT_STATE_VERSION => Ok(LoadedState {
            state: parsed.state,
            status: StateLoadStatus::Current,
        }),
        Ok(parsed) => Ok(LoadedState {
            state: parsed.state,
            status: StateLoadStatus::Legacy {
                version: parsed.version,
            },
        }),
        Err(corruption) => Ok(LoadedState {
            state: RuntimeState::default(),
            status: StateLoadStatus::Corrupt(corruption),
        }),
    }
}

fn ensure_writable_status(status: StateLoadStatus) -> Result<(), StateStoreError> {
    match status {
        StateLoadStatus::Missing | StateLoadStatus::Current => Ok(()),
        StateLoadStatus::Legacy { .. } => Err(StateStoreError::NeedsMigration),
        StateLoadStatus::Corrupt(corruption) => Err(StateStoreError::InvalidSource(corruption)),
    }
}

fn parse_toml(bytes: &[u8]) -> Result<ParsedState, StateCorruption> {
    let input = std::str::from_utf8(bytes).map_err(|_| StateCorruption::InvalidUtf8)?;
    let value: toml::Value = toml::from_str(input).map_err(|_| StateCorruption::InvalidDocument)?;
    let legacy_check_ms = legacy_toml_check_ms(&value)?;
    let raw: RawState = value
        .try_into()
        .map_err(|_| StateCorruption::InvalidDocument)?;
    let mut parsed = raw.into_parsed()?;
    if let Some(check_ms) = legacy_check_ms {
        let legacy =
            UpdateState::new(Some(check_ms), Some(check_ms), None).map_err(map_update_error)?;
        parsed.state.updater = parsed.state.updater.merged_for_persistence(&legacy);
    }
    Ok(parsed)
}

fn parse_json(bytes: &[u8]) -> Result<ParsedState, StateCorruption> {
    let input = std::str::from_utf8(bytes).map_err(|_| StateCorruption::InvalidUtf8)?;
    let raw: RawState =
        serde_json::from_str(input).map_err(|_| StateCorruption::InvalidDocument)?;
    raw.into_parsed()
}

impl RawState {
    fn into_parsed(self) -> Result<ParsedState, StateCorruption> {
        let version = self.version.unwrap_or(1);
        if version > CURRENT_STATE_VERSION {
            return Err(StateCorruption::UnsupportedVersion);
        }
        let last_mode = self.last_mode.or(self.core.last_mode);
        let nested = self.updater.into_update()?;
        let root = self.root_update.into_update()?;
        Ok(ParsedState {
            version,
            state: RuntimeState::new(last_mode, nested.merged_for_persistence(&root)),
        })
    }
}

impl RawUpdater {
    fn into_update(self) -> Result<UpdateState, StateCorruption> {
        let legacy_seconds = self
            .check_seconds
            .filter(|value| value.0 != 0)
            .map(|value| {
                value
                    .0
                    .checked_mul(1_000)
                    .ok_or(StateCorruption::InvalidUpdater)
            })
            .transpose()?;
        let legacy_check = self.check_ms.map(|value| value.0).or(legacy_seconds);
        let reserved = self.reserved_check_ms.map(|value| value.0).or(legacy_check);
        let completed = self
            .completed_check_ms
            .map(|value| value.0)
            .or(legacy_check);
        let notified = self
            .notified_version
            .filter(|version| !version.is_empty())
            .map(|version| version.strip_prefix('v').unwrap_or(&version).to_owned());
        UpdateState::new(reserved, completed, notified.as_deref()).map_err(map_update_error)
    }
}

fn legacy_toml_check_ms(value: &toml::Value) -> Result<Option<u64>, StateCorruption> {
    let Some(updater) = value.get("updater").and_then(toml::Value::as_table) else {
        return Ok(None);
    };
    let Some(timestamp) = updater.get("last-check-time") else {
        return Ok(None);
    };
    match timestamp {
        toml::Value::Datetime(datetime) => parse_rfc3339_millis(&datetime.to_string()),
        toml::Value::String(text) => parse_rfc3339_millis(text),
        _ => Err(StateCorruption::InvalidUpdater),
    }
}

fn parse_rfc3339_millis(value: &str) -> Result<Option<u64>, StateCorruption> {
    let (date, time_and_offset) = value
        .split_once('T')
        .or_else(|| value.split_once('t'))
        .ok_or(StateCorruption::InvalidUpdater)?;
    let mut date_parts = date.split('-');
    let year = parse_date_part(date_parts.next(), 4)?;
    let month = parse_date_part(date_parts.next(), 2)?;
    let day = parse_date_part(date_parts.next(), 2)?;
    if date_parts.next().is_some()
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
    {
        return Err(StateCorruption::InvalidUpdater);
    }

    let (time, offset_seconds) = split_rfc3339_offset(time_and_offset)?;
    let mut time_parts = time.split(':');
    let hour = parse_date_part(time_parts.next(), 2)?;
    let minute = parse_date_part(time_parts.next(), 2)?;
    let second_and_fraction = time_parts.next().ok_or(StateCorruption::InvalidUpdater)?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 {
        return Err(StateCorruption::InvalidUpdater);
    }
    let (second_text, fraction) = second_and_fraction
        .split_once('.')
        .map_or((second_and_fraction, None), |(second, fraction)| {
            (second, Some(fraction))
        });
    let second = parse_date_part(Some(second_text), 2)?;
    if second > 59 {
        return Err(StateCorruption::InvalidUpdater);
    }
    let fraction_ms = parse_fraction_millis(fraction)?;

    let days = days_from_civil(year, month, day);
    let local_seconds = i128::from(days) * 86_400
        + i128::from(hour) * 3_600
        + i128::from(minute) * 60
        + i128::from(second);
    let utc_seconds = local_seconds - i128::from(offset_seconds);
    if utc_seconds < 0 {
        return Ok(None);
    }
    let millis = utc_seconds
        .checked_mul(1_000)
        .and_then(|seconds| seconds.checked_add(i128::from(fraction_ms)))
        .and_then(|millis| u64::try_from(millis).ok())
        .ok_or(StateCorruption::InvalidUpdater)?;
    Ok(Some(millis))
}

fn split_rfc3339_offset(value: &str) -> Result<(&str, i32), StateCorruption> {
    if let Some(time) = value.strip_suffix('Z').or_else(|| value.strip_suffix('z')) {
        return Ok((time, 0));
    }
    let Some(index) = value
        .char_indices()
        .rev()
        .find_map(|(index, character)| matches!(character, '+' | '-').then_some(index))
    else {
        return Err(StateCorruption::InvalidUpdater);
    };
    let (time, offset) = value.split_at(index);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let mut parts = offset[1..].split(':');
    let hours = parse_date_part(parts.next(), 2)?;
    let minutes = parse_date_part(parts.next(), 2)?;
    if parts.next().is_some() || hours > 23 || minutes > 59 {
        return Err(StateCorruption::InvalidUpdater);
    }
    let seconds =
        i32::try_from(hours * 3_600 + minutes * 60).map_err(|_| StateCorruption::InvalidUpdater)?;
    Ok((time, sign * seconds))
}

fn parse_date_part(value: Option<&str>, length: usize) -> Result<u32, StateCorruption> {
    let value = value.ok_or(StateCorruption::InvalidUpdater)?;
    if value.len() != length || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(StateCorruption::InvalidUpdater);
    }
    value.parse().map_err(|_| StateCorruption::InvalidUpdater)
}

fn parse_fraction_millis(fraction: Option<&str>) -> Result<u16, StateCorruption> {
    let Some(fraction) = fraction else {
        return Ok(0);
    };
    if fraction.is_empty()
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(StateCorruption::InvalidUpdater);
    }
    let mut millis = 0_u16;
    for (index, digit) in fraction.bytes().take(3).enumerate() {
        let place = [100_u16, 10, 1][index];
        millis += u16::from(digit - b'0') * place;
    }
    Ok(millis)
}

// `is_multiple_of` is newer than the crate's Rust 1.85 MSRV.
#[allow(unknown_lints, clippy::manual_is_multiple_of)]
const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: u32, month: u32, day: u32) -> i64 {
    let mut year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

const fn map_update_error(error: UpdateStateError) -> StateCorruption {
    match error {
        UpdateStateError::InvalidNotifiedVersion => StateCorruption::InvalidUpdater,
    }
}

fn merge_legacy_sources(sources: &[LegacySource]) -> Result<RuntimeState, StateStoreError> {
    let mut merged = RuntimeState::default();
    for source in sources {
        let parsed = match source.format {
            LegacyFormat::Toml => parse_toml(&source.file.bytes),
            LegacyFormat::StateJson | LegacyFormat::UpdateJson => parse_json(&source.file.bytes),
        }
        .map_err(StateStoreError::InvalidSource)?;
        if source.format.is_state_source() {
            merged.last_mode = parsed.state.last_mode.or(merged.last_mode);
        }
        merged.updater = parsed.state.updater.merged_for_persistence(&merged.updater);
    }
    Ok(merged)
}

impl LegacyFormat {
    const fn is_state_source(self) -> bool {
        matches!(self, Self::Toml | Self::StateJson)
    }
}

fn same_update_state(left: &UpdateState, right: &UpdateState) -> bool {
    left.last_reserved_check_ms() == right.last_reserved_check_ms()
        && left.last_completed_check_ms() == right.last_completed_check_ms()
        && left.last_notified_version() == right.last_notified_version()
}

fn render_state(state: &RuntimeState) -> Result<String, StateStoreError> {
    let stored = StoredState {
        version: CURRENT_STATE_VERSION,
        last_mode: state.last_mode,
        updater: StoredUpdater {
            reserved_check_ms: state.updater.last_reserved_check_ms(),
            completed_check_ms: state.updater.last_completed_check_ms(),
            notified_version: state.updater.last_notified_version(),
        },
    };
    let rendered = toml::to_string_pretty(&stored)
        .map_err(|_| StateStoreError::InvalidSource(StateCorruption::InvalidDocument))?;
    parse_toml(rendered.as_bytes()).map_err(StateStoreError::InvalidSource)?;
    Ok(rendered)
}

fn write_state(
    path: &Path,
    state: &RuntimeState,
    disposition: WriteDisposition,
    lock: &StateLock,
) -> Result<(), StateStoreError> {
    let rendered = render_state(state)?;
    let parent = required_parent(path)?;
    let mut temporary = private_temp_file(parent)
        .map_err(|error| StateStoreError::io("prepare runtime state", error))?;
    temporary
        .write_all(rendered.as_bytes())
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| StateStoreError::io("write runtime state", error))?;

    match disposition {
        WriteDisposition::Create => {
            lock.validate_anchor()?;
            temporary
                .persist_noclobber(path)
                .map_err(|error| StateStoreError::io("install runtime state", error.error))?
        }
        WriteDisposition::Replace => {
            if path_may_exist(path) {
                validate_path_file(path)?;
            }
            lock.validate_anchor()?;
            temporary
                .persist(path)
                .map_err(|error| StateStoreError::io("replace runtime state", error.error))?
        }
    };
    sync_directory(parent).map_err(|error| StateStoreError::io("sync runtime state", error))
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
            self.bytes == other.bytes
        }
    }
}

fn read_optional_private(path: &Path) -> Result<Option<PrivateFile>, StateStoreError> {
    let parent = required_parent(path)?;
    if !path_may_exist(parent) {
        return Ok(None);
    }
    secure_existing_directory(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_file_metadata(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StateStoreError::io("inspect runtime state path", error)),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StateStoreError::io("open runtime state", error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| StateStoreError::io("inspect runtime state", error))?;
    validate_file_metadata(&metadata)?;
    set_file_private(&file)?;
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_STATE_BYTES + 1).expect("state size limit fits u64");
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| StateStoreError::io("read runtime state", error))?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(StateStoreError::InvalidSource(StateCorruption::TooLarge));
    }
    Ok(Some(PrivateFile {
        bytes,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    }))
}

fn source_matches(path: &Path, original: &PrivateFile) -> Result<bool, StateStoreError> {
    let Some(current) = read_optional_private(path)? else {
        return Ok(false);
    };
    Ok(original.same_source(&current) && original.bytes == current.bytes)
}

fn validate_open_file(file: &File) -> Result<(), StateStoreError> {
    let metadata = file
        .metadata()
        .map_err(|error| StateStoreError::io("inspect runtime state", error))?;
    if !metadata.is_file() {
        return Err(StateStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(StateStoreError::UnsafeFileType);
    }
    Ok(())
}

fn validate_path_file(path: &Path) -> Result<(), StateStoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StateStoreError::io("inspect runtime state path", error))?;
    validate_file_metadata(&metadata)
}

fn validate_file_metadata(metadata: &fs::Metadata) -> Result<(), StateStoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StateStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(StateStoreError::UnsafeFileType);
    }
    Ok(())
}

fn required_parent(path: &Path) -> Result<&Path, StateStoreError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(StateStoreError::MissingParent)
}

fn secure_existing_directory(path: &Path) -> Result<(), StateStoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StateStoreError::io("inspect runtime state directory", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StateStoreError::UnsafeDirectoryType);
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| StateStoreError::io("secure runtime state directory", error))?;
    Ok(())
}

fn secure_directory(path: &Path) -> Result<(), StateStoreError> {
    fs::create_dir_all(path)
        .map_err(|error| StateStoreError::io("create runtime state directory", error))?;
    secure_existing_directory(path)
}

fn set_file_private(file: &File) -> Result<(), StateStoreError> {
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| StateStoreError::io("secure runtime state file", error))?;
    Ok(())
}

struct StateLock {
    file: File,
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl StateLock {
    fn validate_anchor(&self) -> Result<(), StateStoreError> {
        let opened = self
            .file
            .metadata()
            .map_err(|error| StateStoreError::io("inspect runtime state lock", error))?;
        let current = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(StateStoreError::LockReplaced);
            }
            Err(error) => {
                return Err(StateStoreError::io("reinspect runtime state lock", error));
            }
        };
        if current.file_type().is_symlink() || !current.is_file() || !opened.is_file() {
            return Err(StateStoreError::LockReplaced);
        }
        #[cfg(unix)]
        if opened.nlink() != 1
            || current.nlink() != 1
            || opened.dev() != self.device
            || opened.ino() != self.inode
            || current.dev() != self.device
            || current.ino() != self.inode
        {
            return Err(StateStoreError::LockReplaced);
        }
        Ok(())
    }
}

fn acquire_lock(path: &Path) -> Result<StateLock, StateStoreError> {
    let lock_path = sidecar_path(path, ".lock")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let file = options
        .open(&lock_path)
        .map_err(|error| StateStoreError::io("open runtime state lock", error))?;
    validate_open_file(&file)?;
    set_file_private(&file)?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(rustix_error)
        .map_err(|error| StateStoreError::io("lock runtime state", error))?;

    let opened = file
        .metadata()
        .map_err(|error| StateStoreError::io("inspect runtime state lock", error))?;
    let current = fs::symlink_metadata(&lock_path)
        .map_err(|error| StateStoreError::io("reinspect runtime state lock", error))?;
    if current.file_type().is_symlink() || !current.is_file() {
        return Err(StateStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if current.nlink() != 1 || current.dev() != opened.dev() || current.ino() != opened.ino() {
        return Err(StateStoreError::UnsafeFileType);
    }
    Ok(StateLock {
        file,
        path: lock_path,
        #[cfg(unix)]
        device: opened.dev(),
        #[cfg(unix)]
        inode: opened.ino(),
    })
}

fn private_temp_file(parent: &Path) -> Result<NamedTempFile, io::Error> {
    let file = NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn write_timestamped_backup(
    source: &Path,
    timestamp: u64,
    bytes: &[u8],
) -> Result<PathBuf, StateStoreError> {
    let name = source
        .file_name()
        .unwrap_or_else(|| OsStr::new(STATE_FILE_NAME));
    for collision in 0..MAX_BACKUP_COLLISIONS {
        let mut candidate_name = OsString::from(name);
        candidate_name.push(format!(".backup.{timestamp}"));
        if collision != 0 {
            candidate_name.push(format!(".{collision}"));
        }
        let candidate = source.with_file_name(candidate_name);
        match write_private_new(&candidate, bytes) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StateStoreError::io("write runtime state backup", error)),
        }
    }
    Err(StateStoreError::BackupNameExhausted)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
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

fn sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, StateStoreError> {
    let name = path.file_name().ok_or(StateStoreError::MissingParent)?;
    let mut sidecar_name = OsString::from(".");
    sidecar_name.push(name);
    sidecar_name.push(suffix);
    Ok(path.with_file_name(sidecar_name))
}

fn sync_directory(path: &Path) -> Result<(), io::Error> {
    File::open(path)?.sync_all()
}

fn rustix_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

fn deduplicate_paths(paths: Vec<PathBuf>, current: &Path) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|candidate| candidate != current)
        .fold(Vec::new(), |mut retained, candidate| {
            if !retained.contains(&candidate) {
                retained.push(candidate);
            }
            retained
        })
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
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn store(root: &Path) -> RuntimeStateStore {
        RuntimeStateStore::new(root.join("argmax").join(STATE_FILE_NAME))
    }

    #[test]
    fn missing_and_corrupt_state_return_inspectable_defaults() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(temporary.path());
        assert_eq!(store.load().unwrap().status, StateLoadStatus::Missing);

        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), b"not = [toml").unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.status,
            StateLoadStatus::Corrupt(StateCorruption::InvalidDocument)
        );
        assert_eq!(loaded.state.last_mode(), None);
        assert_eq!(loaded.state.updater().last_completed_check_ms(), None);

        fs::write(store.path(), vec![b'x'; MAX_STATE_BYTES + 1]).unwrap();
        assert_eq!(
            store.load().unwrap().status,
            StateLoadStatus::Corrupt(StateCorruption::TooLarge)
        );
    }

    #[test]
    fn typed_writes_preserve_unrelated_and_monotonic_fields() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(temporary.path());
        store.store_mode(LastMode::History).unwrap();
        store
            .store_updater(&UpdateState::new(Some(20), Some(30), Some("2.0.0")).unwrap())
            .unwrap();
        store
            .store_updater(&UpdateState::new(Some(10), Some(25), Some("1.0.0")).unwrap())
            .unwrap();

        let loaded = store.load().unwrap().state;
        assert_eq!(loaded.last_mode(), Some(LastMode::History));
        assert_eq!(loaded.updater().last_reserved_check_ms(), Some(20));
        assert_eq!(loaded.updater().last_completed_check_ms(), Some(30));
        assert_eq!(loaded.updater().last_notified_version(), Some("2.0.0"));
    }

    #[test]
    fn compare_and_store_has_one_cross_process_style_winner() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(store(temporary.path()));
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for timestamp in [10, 20] {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                let expected = UpdateState::default();
                let proposed = UpdateState::new(Some(timestamp), None, None).unwrap();
                barrier.wait();
                store
                    .compare_and_store_updater(&expected, &proposed)
                    .unwrap()
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, UpdateStoreOutcome::Persisted(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, UpdateStoreOutcome::Superseded(_)))
                .count(),
            1
        );
    }

    #[test]
    fn imports_both_legacy_json_files_once_and_keeps_sources() {
        let temporary = tempfile::tempdir().unwrap();
        let current = temporary.path().join("data/argmax/state.toml");
        let legacy = temporary.path().join("home/.argmax");
        fs::create_dir_all(&legacy).unwrap();
        let state_json = legacy.join("state.json");
        let update_json = legacy.join("update_state.json");
        fs::write(&state_json, br#"{"mode":"history"}"#).unwrap();
        fs::write(
            &update_json,
            br#"{"last_check":20,"seen_version":"v1.2.3"}"#,
        )
        .unwrap();
        let paths = StatePaths::new(
            current,
            Vec::new(),
            Some(state_json.clone()),
            Some(update_json.clone()),
        );
        let store = RuntimeStateStore::from_paths(paths);

        let outcome = store.migrate_if_needed(42).unwrap();
        let StateMigrationOutcome::Migrated {
            source_count,
            backups,
        } = outcome
        else {
            panic!("expected migration");
        };
        assert_eq!(source_count, 2);
        assert_eq!(backups.len(), 2);
        assert!(state_json.exists());
        assert!(update_json.exists());
        assert!(backups.iter().all(|backup| backup.exists()));
        assert!(matches!(
            store.migrate_if_needed(43).unwrap(),
            StateMigrationOutcome::Unneeded
        ));

        let state = store.load().unwrap().state;
        assert_eq!(state.last_mode(), Some(LastMode::History));
        assert_eq!(state.updater().last_reserved_check_ms(), Some(20_000));
        assert_eq!(state.updater().last_completed_check_ms(), Some(20_000));
        assert_eq!(state.updater().last_notified_version(), Some("1.2.3"));
    }

    #[test]
    fn discovers_real_iris_sources_in_isolated_processes_and_current_wins() {
        const CHILD_CASE: &str = "ARGMAX_STATE_DISCOVERY_CHILD_CASE";
        if let Ok(case) = std::env::var(CHILD_CASE) {
            let store = RuntimeStateStore::discover().unwrap();
            match case.as_str() {
                "xdg-toml" | "xdg-conflict" | "home-json" => {
                    assert!(matches!(
                        store.migrate_if_needed(71).unwrap(),
                        StateMigrationOutcome::Migrated { .. }
                    ));
                    assert_eq!(
                        store.load().unwrap().state.last_mode(),
                        Some(LastMode::History)
                    );
                    assert!(matches!(
                        store.migrate_if_needed(72).unwrap(),
                        StateMigrationOutcome::Unneeded
                    ));
                }
                "current" => {
                    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
                    fs::write(
                        store.path(),
                        b"version = 2\nlast_mode = \"spec\"\n[updater]\n",
                    )
                    .unwrap();
                    assert!(matches!(
                        store.migrate_if_needed(73).unwrap(),
                        StateMigrationOutcome::Unneeded
                    ));
                    assert_eq!(
                        store.load().unwrap().state.last_mode(),
                        Some(LastMode::Spec)
                    );
                }
                _ => panic!("unknown child case"),
            }
            return;
        }

        let test_name =
            "state::tests::discovers_real_iris_sources_in_isolated_processes_and_current_wins";
        for case in ["xdg-toml", "xdg-conflict", "home-json", "current"] {
            let temporary = tempfile::tempdir().unwrap();
            let home = temporary.path().join("home");
            let xdg = temporary.path().join("xdg");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&xdg).unwrap();
            match case {
                "xdg-toml" => {
                    let source = xdg.join("iris/state.toml");
                    fs::create_dir_all(source.parent().unwrap()).unwrap();
                    fs::write(&source, b"last-mode = \"history\"\n").unwrap();
                }
                "xdg-conflict" => {
                    let active = xdg.join("iris/state.toml");
                    fs::create_dir_all(active.parent().unwrap()).unwrap();
                    fs::write(&active, b"last-mode = \"history\"\n").unwrap();
                    let fallback = home.join(".local/share/iris/state.toml");
                    fs::create_dir_all(fallback.parent().unwrap()).unwrap();
                    fs::write(&fallback, b"last-mode = \"spec\"\n").unwrap();
                }
                "home-json" => {
                    let source = home.join(".iris/state.json");
                    fs::create_dir_all(source.parent().unwrap()).unwrap();
                    fs::write(&source, br#"{"mode":"history"}"#).unwrap();
                }
                "current" => {
                    let source = xdg.join("iris/state.toml");
                    fs::create_dir_all(source.parent().unwrap()).unwrap();
                    fs::write(&source, b"not valid toml = [").unwrap();
                }
                _ => unreachable!(),
            }
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(test_name)
                .arg("--nocapture")
                .env(CHILD_CASE, case)
                .env("HOME", &home)
                .env("XDG_DATA_HOME", &xdg)
                .status()
                .unwrap();
            assert!(status.success(), "discovery child {case} failed");
        }
    }

    #[test]
    fn migrates_the_existing_toml_shape_with_a_durable_backup() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(temporary.path());
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let legacy = br#"last-mode = "history"

[updater]
last-check-time = 2009-02-13T23:31:30Z
seen-version = "v1.2.3"
"#;
        fs::write(store.path(), legacy).unwrap();

        let StateMigrationOutcome::Migrated {
            source_count,
            backups,
        } = store.migrate_if_needed(55).unwrap()
        else {
            panic!("expected migration");
        };
        assert_eq!(source_count, 1);
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), legacy);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.status, StateLoadStatus::Current);
        assert_eq!(loaded.state.last_mode(), Some(LastMode::History));
        assert_eq!(
            loaded.state.updater().last_reserved_check_ms(),
            Some(1_234_567_890_000)
        );
        assert_eq!(
            loaded.state.updater().last_completed_check_ms(),
            Some(1_234_567_890_000)
        );
        assert_eq!(
            loaded.state.updater().last_notified_version(),
            Some("1.2.3")
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions_and_unsafe_paths_are_enforced() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let temporary = tempfile::tempdir().unwrap();
        let store = store(temporary.path());
        store.store_mode(LastMode::Spec).unwrap();
        assert_eq!(
            fs::metadata(store.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let target = temporary.path().join("target");
        fs::write(&target, b"version = 2\n[updater]\n").unwrap();
        let linked = temporary.path().join("linked.toml");
        symlink(&target, &linked).unwrap();
        let linked_store = RuntimeStateStore::new(linked);
        assert!(matches!(
            linked_store.load(),
            Err(StateStoreError::Io { .. } | StateStoreError::UnsafeFileType)
        ));

        let hardlinked = temporary.path().join("hardlinked.toml");
        fs::hard_link(&target, &hardlinked).unwrap();
        let hardlinked_store = RuntimeStateStore::new(hardlinked);
        assert!(matches!(
            hardlinked_store.load(),
            Err(StateStoreError::UnsafeFileType)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn replaced_lock_inode_fails_closed_without_losing_concurrent_state() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(temporary.path());
        store.store_mode(LastMode::Spec).unwrap();

        let stale_lock = acquire_lock(store.path()).unwrap();
        let mut stale_state = load_unlocked(store.path()).unwrap().state;
        fs::remove_file(&stale_lock.path).unwrap();
        File::create(&stale_lock.path).unwrap();

        store
            .store_updater(&UpdateState::new(Some(90), Some(80), None).unwrap())
            .unwrap();
        stale_state.last_mode = Some(LastMode::History);
        assert!(matches!(
            write_state(
                store.path(),
                &stale_state,
                WriteDisposition::Replace,
                &stale_lock
            ),
            Err(StateStoreError::LockReplaced)
        ));

        let current = store.load().unwrap().state;
        assert_eq!(current.last_mode(), Some(LastMode::Spec));
        assert_eq!(current.updater().last_reserved_check_ms(), Some(90));
        assert_eq!(current.updater().last_completed_check_ms(), Some(80));
    }

    #[test]
    fn debug_output_redacts_paths_and_notified_version() {
        let paths = StatePaths::new(
            "/private/Troy-secret/state.toml",
            vec![PathBuf::from("/private/Annie-secret/state.toml")],
            None,
            None,
        );
        let state = RuntimeState::new(
            Some(LastMode::History),
            UpdateState::new(None, None, Some("9.8.7+private-token")).unwrap(),
        );
        let debug = format!("{paths:?} {state:?}");
        assert!(!debug.contains("Troy-secret"));
        assert!(!debug.contains("Annie-secret"));
        assert!(!debug.contains("private-token"));
    }
}
