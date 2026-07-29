//! Private `SQLite` persistence for command-learning events and aggregates.
//!
//! Each event and both global/directory aggregate updates commit in one
//! immediate transaction. `SQLite` supplies cross-process locking; a bounded
//! busy timeout lets simultaneous terminal sessions serialize without holding
//! a lock across prompt work.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use directories::{BaseDirs, ProjectDirs};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};

use crate::catalog;
use crate::learning::{
    CommandAggregateKey, CommandOutcome, LearningEvent, LearningScope, LearningState,
    TransitionAggregateKey, UsageAggregate,
};

/// Current `SQLite` `user_version`.
pub const CURRENT_LEARNING_SCHEMA: u32 = 1;
/// Maximum exact submitted command size.
pub const MAX_LEARNING_COMMAND_BYTES: usize = 64 * 1024;
/// Maximum canonical skeleton size.
pub const MAX_LEARNING_SKELETON_BYTES: usize = 4 * 1024;
/// Maximum encoded working-directory size.
pub const MAX_LEARNING_CWD_BYTES: usize = 16 * 1024;
/// Maximum aggregate rows loaded into memory per table.
pub const MAX_LEARNING_AGGREGATES: usize = 250_000;
/// Maximum aggregate key bytes loaded into memory per table.
pub const MAX_LEARNING_AGGREGATE_BYTES: usize = 64 * 1024 * 1024;

const DATABASE_FILE_NAME: &str = "history.db";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_PAGE_COUNT: u32 = 262_144;
const MAX_BACKUP_COLLISIONS: u16 = 1_000;

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS learning_events (
    id INTEGER PRIMARY KEY,
    command TEXT NOT NULL,
    skeleton TEXT NOT NULL,
    cwd BLOB NOT NULL,
    observed_at INTEGER NOT NULL,
    outcome INTEGER NOT NULL CHECK (outcome IN (0, 1)),
    prior_skeleton TEXT
);

CREATE INDEX IF NOT EXISTS learning_events_observed_at
    ON learning_events(observed_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS command_aggregates (
    scope INTEGER NOT NULL CHECK (scope IN (0, 1)),
    cwd BLOB NOT NULL,
    skeleton TEXT NOT NULL,
    successful_count INTEGER NOT NULL CHECK (successful_count >= 0),
    last_used_at INTEGER NOT NULL CHECK (last_used_at >= 0),
    PRIMARY KEY (scope, cwd, skeleton)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS transition_aggregates (
    scope INTEGER NOT NULL CHECK (scope IN (0, 1)),
    cwd BLOB NOT NULL,
    prior_skeleton TEXT NOT NULL,
    current_skeleton TEXT NOT NULL,
    successful_count INTEGER NOT NULL CHECK (successful_count >= 0),
    last_used_at INTEGER NOT NULL CHECK (last_used_at >= 0),
    PRIMARY KEY (scope, cwd, prior_skeleton, current_skeleton)
) WITHOUT ROWID;
";

const UPSERT_COMMAND: &str = r"
INSERT INTO command_aggregates (
    scope, cwd, skeleton, successful_count, last_used_at
) VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT (scope, cwd, skeleton) DO UPDATE SET
    successful_count = CASE
        WHEN excluded.successful_count = 0 THEN command_aggregates.successful_count
        WHEN command_aggregates.successful_count = 9223372036854775807
            THEN command_aggregates.successful_count
        ELSE command_aggregates.successful_count + 1
    END,
    last_used_at = MAX(command_aggregates.last_used_at, excluded.last_used_at)
";

const UPSERT_TRANSITION: &str = r"
INSERT INTO transition_aggregates (
    scope, cwd, prior_skeleton, current_skeleton,
    successful_count, last_used_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT (scope, cwd, prior_skeleton, current_skeleton) DO UPDATE SET
    successful_count = CASE
        WHEN excluded.successful_count = 0 THEN transition_aggregates.successful_count
        WHEN transition_aggregates.successful_count = 9223372036854775807
            THEN transition_aggregates.successful_count
        ELSE transition_aggregates.successful_count + 1
    END,
    last_used_at = MAX(transition_aggregates.last_used_at, excluded.last_used_at)
";

const INSERT_IMPORTED_COMMAND: &str = r"
INSERT INTO command_aggregates (
    scope, cwd, skeleton, successful_count, last_used_at
) VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT (scope, cwd, skeleton) DO NOTHING
";

const INSERT_IMPORTED_TRANSITION: &str = r"
INSERT INTO transition_aggregates (
    scope, cwd, prior_skeleton, current_skeleton,
    successful_count, last_used_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT (scope, cwd, prior_skeleton, current_skeleton) DO NOTHING
";

/// Platform or explicit learning-database location.
#[derive(Clone, Eq, PartialEq)]
pub struct LearningStorePaths {
    database: PathBuf,
}

impl LearningStorePaths {
    /// Discovers the standard platform data location.
    ///
    /// # Errors
    ///
    /// Returns [`LearningStoreError::NoPlatformDirectory`] when no user data
    /// directory is available.
    pub fn discover() -> Result<Self, LearningStoreError> {
        let project =
            ProjectDirs::from("", "", "argmax").ok_or(LearningStoreError::NoPlatformDirectory)?;
        let iris =
            ProjectDirs::from("", "", "iris").ok_or(LearningStoreError::NoPlatformDirectory)?;
        let base = BaseDirs::new().ok_or(LearningStoreError::NoPlatformDirectory)?;
        let current = project.data_dir().join(DATABASE_FILE_NAME);
        let mut legacy = Vec::new();
        if let Some(xdg_data) = std::env::var_os("XDG_DATA_HOME") {
            let xdg_data = PathBuf::from(xdg_data);
            if xdg_data.is_absolute() {
                legacy.push(xdg_data.join("argmax").join(DATABASE_FILE_NAME));
                legacy.push(xdg_data.join("iris").join(DATABASE_FILE_NAME));
            }
        }
        legacy.extend([
            base.data_dir().join("argmax").join(DATABASE_FILE_NAME),
            base.home_dir()
                .join(".local/share/argmax")
                .join(DATABASE_FILE_NAME),
            iris.data_dir().join(DATABASE_FILE_NAME),
            base.data_dir().join("iris").join(DATABASE_FILE_NAME),
            base.home_dir()
                .join(".local/share/iris")
                .join(DATABASE_FILE_NAME),
        ]);
        let database = if path_may_exist(&current) {
            current
        } else {
            legacy
                .into_iter()
                .filter(|candidate| candidate != &current)
                .find(|candidate| path_may_exist(candidate))
                .unwrap_or(current)
        };
        Ok(Self::new(database))
    }

    /// Builds an explicit location for tests and embedded callers.
    #[must_use]
    pub fn new(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
        }
    }

    /// Exact database path.
    #[must_use]
    pub fn database(&self) -> &Path {
        &self.database
    }
}

impl fmt::Debug for LearningStorePaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LearningStorePaths")
            .field("database_path_bytes", &path_bytes(&self.database))
            .finish()
    }
}

/// Persistent command-learning store.
#[derive(Clone, Eq, PartialEq)]
pub struct LearningStore {
    paths: LearningStorePaths,
}

impl LearningStore {
    /// Uses an explicit `SQLite` path.
    #[must_use]
    pub fn new(database: impl Into<PathBuf>) -> Self {
        Self {
            paths: LearningStorePaths::new(database),
        }
    }

    /// Discovers the standard platform data path.
    ///
    /// # Errors
    ///
    /// Returns an error when no user data directory is available.
    pub fn discover() -> Result<Self, LearningStoreError> {
        Ok(Self {
            paths: LearningStorePaths::discover()?,
        })
    }

    /// Uses explicit store paths.
    #[must_use]
    pub const fn from_paths(paths: LearningStorePaths) -> Self {
        Self { paths }
    }

    /// Exact `SQLite` path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.paths.database()
    }

    /// Creates or migrates the database transactionally without recording data.
    ///
    /// Version-zero databases already containing current-shaped command and
    /// transition tables are adopted in place without rewriting their rows.
    ///
    /// # Errors
    ///
    /// Returns redacted path-security, I/O, corruption, busy, or schema errors.
    pub fn initialize(&self) -> Result<(), LearningStoreError> {
        drop(self.open()?);
        Ok(())
    }

    /// Runs a schema migration using a caller-supplied backup timestamp and
    /// returns its inspectable, path-redacted outcome.
    ///
    /// # Errors
    ///
    /// Returns before schema mutation unless an existing database has first
    /// been copied to a validated durable snapshot.
    pub fn migrate_if_needed(
        &self,
        timestamp_seconds: u64,
    ) -> Result<LearningMigrationOutcome, LearningStoreError> {
        let opened = self.open_at(timestamp_seconds)?;
        Ok(opened.migration)
    }

    /// Records one exact event and updates its global/directory aggregates in a
    /// single transaction.
    ///
    /// Successful outcomes increment frequency. All outcomes advance recency.
    ///
    /// # Errors
    ///
    /// Returns a bounded-input error before opening the database, or a redacted
    /// storage error. Callers may isolate this error from prompt/session flow.
    pub fn record(&self, event: &LearningEvent) -> Result<(), LearningStoreError> {
        validate_event(event)?;
        let timestamp = sqlite_integer(event.timestamp, "timestamp")?;
        let successful = i64::from(event.outcome == CommandOutcome::Success);
        let cwd = encode_path(&event.cwd);
        let OpenedDatabase {
            mut connection,
            anchor,
            ..
        } = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| database_error("begin learning transaction", error))?;

        transaction
            .execute(
                "INSERT INTO learning_events (command, skeleton, cwd, observed_at, outcome, prior_skeleton) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    event.command,
                    event.skeleton,
                    cwd,
                    timestamp,
                    successful,
                    event.prior_skeleton,
                ],
            )
            .map_err(|error| database_error("record learning event", error))?;

        update_aggregates(&transaction, event, &cwd, successful, timestamp)?;
        verify_database_identity(self.path(), &anchor)?;
        transaction
            .commit()
            .map_err(|error| database_error("commit learning transaction", error))
    }

    /// Loads bounded global and exact-directory aggregates for ranking.
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt/incompatible rows, excessive aggregate
    /// counts, unsafe paths, I/O, or `SQLite` failures.
    pub fn load(&self) -> Result<LearningState, LearningStoreError> {
        let OpenedDatabase {
            connection, anchor, ..
        } = self.open()?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Deferred)
            .map_err(|error| database_error("begin learning snapshot", error))?;
        let commands = load_command_aggregates(&transaction)?;
        let transitions = load_transition_aggregates(&transaction)?;
        verify_database_identity(self.path(), &anchor)?;
        transaction
            .commit()
            .map_err(|error| database_error("finish learning snapshot", error))?;
        Ok(LearningState::from_aggregates(commands, transitions))
    }

    fn open(&self) -> Result<OpenedDatabase, LearningStoreError> {
        self.open_at(unix_seconds_now()?)
    }

    fn open_at(&self, timestamp_seconds: u64) -> Result<OpenedDatabase, LearningStoreError> {
        let parent = required_parent(self.path())?;
        secure_directory(parent)?;
        let migration_lock = acquire_migration_lock(self.path())?;
        validate_existing_sidecars(self.path())?;
        let anchor = open_database_anchor(self.path())?;
        let connection = Connection::open_with_flags(
            self.path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )
        .map_err(|error| database_error("open learning database", error))?;
        verify_database_identity(self.path(), &anchor)?;
        configure_connection(&connection)?;
        let migration = migrate_schema(
            &connection,
            self.path(),
            &anchor,
            &migration_lock,
            timestamp_seconds,
        )?;
        configure_persistent_database(&connection)?;
        Ok(OpenedDatabase {
            connection,
            migration,
            anchor,
        })
    }
}

impl fmt::Debug for LearningStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LearningStore")
            .field("paths", &self.paths)
            .finish()
    }
}

struct OpenedDatabase {
    connection: Connection,
    migration: LearningMigrationOutcome,
    anchor: DatabaseAnchor,
}

/// Outcome of opening and, when necessary, migrating a learning database.
#[must_use]
pub enum LearningMigrationOutcome {
    /// The database already used the current schema.
    Unneeded,
    /// An older or empty database was migrated transactionally.
    Migrated {
        /// Schema version observed before migration.
        source_schema: u32,
        /// Validated durable pre-migration snapshot; absent only for a newly
        /// created empty database.
        backup: Option<PathBuf>,
    },
}

impl fmt::Debug for LearningMigrationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unneeded => formatter.write_str("Unneeded"),
            Self::Migrated {
                source_schema,
                backup,
            } => formatter
                .debug_struct("Migrated")
                .field("source_schema", source_schema)
                .field("backup_path_bytes", &backup.as_deref().map(path_bytes))
                .finish(),
        }
    }
}

/// Redacted persistent-learning failure.
pub enum LearningStoreError {
    /// No standard user data directory is available.
    NoPlatformDirectory,
    /// Database path has no usable parent.
    MissingParent,
    /// Database path is a symlink, hard link, or non-regular file.
    UnsafeFileType,
    /// Data directory is a symlink or non-directory.
    UnsafeDirectoryType,
    /// One event/row field was invalid or exceeded its bound.
    InvalidInput(&'static str),
    /// Aggregate input exceeded the bounded in-memory row count.
    TooManyAggregates,
    /// Aggregate keys exceeded the bounded in-memory byte budget.
    TooMuchAggregateData,
    /// Database `user_version` is newer than this binary supports.
    UnsupportedSchema,
    /// `SQLite` reported a corrupt or non-database file.
    CorruptDatabase,
    /// Another process retained a lock beyond the bounded busy timeout.
    Busy,
    /// The built-in command catalog could not be validated for migration.
    CatalogUnavailable,
    /// The system clock was earlier than the Unix epoch.
    ClockBeforeEpoch,
    /// No unused timestamped backup name remained.
    BackupNameExhausted,
    /// A database backup was created but failed integrity or identity checks.
    InvalidBackup,
    /// The migration lock pathname no longer names the locked inode.
    LockReplaced,
    /// Filesystem error classified without retaining a sensitive path.
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    /// Other `SQLite` failure classified without retaining SQL or values.
    Database { operation: &'static str },
}

impl LearningStoreError {
    fn io(operation: &'static str, error: io::Error) -> Self {
        let kind = error.kind();
        drop(error);
        Self::Io { operation, kind }
    }
}

impl fmt::Debug for LearningStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for LearningStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlatformDirectory => {
                formatter.write_str("no standard user data directory is available")
            }
            Self::MissingParent => formatter.write_str("learning database path has no parent"),
            Self::UnsafeFileType => {
                formatter.write_str("learning database is not a safe regular file")
            }
            Self::UnsafeDirectoryType => {
                formatter.write_str("learning data directory is not a safe directory")
            }
            Self::InvalidInput(field) => write!(formatter, "invalid learning {field}"),
            Self::TooManyAggregates => {
                formatter.write_str("learning database contains too many aggregate rows")
            }
            Self::TooMuchAggregateData => {
                formatter.write_str("learning aggregate keys exceed the in-memory byte budget")
            }
            Self::UnsupportedSchema => {
                formatter.write_str("learning database schema is newer than this binary")
            }
            Self::CorruptDatabase => formatter.write_str("learning database is corrupt"),
            Self::Busy => formatter.write_str("learning database remained busy"),
            Self::CatalogUnavailable => {
                formatter.write_str("learning command catalog is unavailable")
            }
            Self::ClockBeforeEpoch => formatter.write_str("system clock is before the Unix epoch"),
            Self::BackupNameExhausted => {
                formatter.write_str("no unused learning backup name is available")
            }
            Self::InvalidBackup => formatter.write_str("learning database backup is invalid"),
            Self::LockReplaced => {
                formatter.write_str("learning migration lock was replaced; write was preserved")
            }
            Self::Io { operation, kind } => write!(formatter, "{operation}: {kind:?}"),
            Self::Database { operation } => write!(formatter, "{operation} failed"),
        }
    }
}

impl Error for LearningStoreError {}

fn validate_event(event: &LearningEvent) -> Result<(), LearningStoreError> {
    event
        .validate()
        .map_err(|error| LearningStoreError::InvalidInput(error.field))?;
    if event.command.len() > MAX_LEARNING_COMMAND_BYTES {
        return Err(LearningStoreError::InvalidInput("command"));
    }
    validate_skeleton(&event.skeleton, "skeleton")?;
    if let Some(prior) = &event.prior_skeleton {
        validate_skeleton(prior, "prior_skeleton")?;
    }
    let cwd = encode_path(&event.cwd);
    if cwd.is_empty() || cwd.len() > MAX_LEARNING_CWD_BYTES {
        return Err(LearningStoreError::InvalidInput("cwd"));
    }
    sqlite_integer(event.timestamp, "timestamp").map(|_| ())
}

fn validate_skeleton(value: &str, field: &'static str) -> Result<(), LearningStoreError> {
    if value.len() > MAX_LEARNING_SKELETON_BYTES
        || value.is_empty()
        || value.split(' ').any(|token| {
            token.is_empty()
                || token
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
        })
    {
        return Err(LearningStoreError::InvalidInput(field));
    }
    Ok(())
}

fn sqlite_integer(value: u64, field: &'static str) -> Result<i64, LearningStoreError> {
    i64::try_from(value).map_err(|_| LearningStoreError::InvalidInput(field))
}

fn update_aggregates(
    transaction: &Transaction<'_>,
    event: &LearningEvent,
    cwd: &[u8],
    successful: i64,
    timestamp: i64,
) -> Result<(), LearningStoreError> {
    for (scope, directory) in [(0_i64, &[][..]), (1_i64, cwd)] {
        transaction
            .execute(
                UPSERT_COMMAND,
                params![scope, directory, event.skeleton, successful, timestamp],
            )
            .map_err(|error| database_error("update command aggregate", error))?;
        if let Some(prior) = &event.prior_skeleton {
            transaction
                .execute(
                    UPSERT_TRANSITION,
                    params![
                        scope,
                        directory,
                        prior,
                        event.skeleton,
                        successful,
                        timestamp,
                    ],
                )
                .map_err(|error| database_error("update transition aggregate", error))?;
        }
    }
    Ok(())
}

fn load_command_aggregates(
    connection: &Connection,
) -> Result<BTreeMap<CommandAggregateKey, UsageAggregate>, LearningStoreError> {
    reject_invalid_aggregate_rows(
        connection,
        "SELECT EXISTS(
            SELECT 1 FROM command_aggregates
            WHERE typeof(scope) != 'integer'
                OR scope NOT IN (0, 1)
                OR typeof(cwd) != 'blob'
                OR length(cwd) > 16384
                OR (scope = 0 AND length(cwd) != 0)
                OR (scope = 1 AND length(cwd) = 0)
                OR typeof(skeleton) != 'text'
                OR length(CAST(skeleton AS BLOB)) NOT BETWEEN 1 AND 4096
                OR typeof(successful_count) != 'integer'
                OR successful_count < 0
                OR typeof(last_used_at) != 'integer'
                OR last_used_at < 0
        )",
        "stored command aggregate",
    )?;
    let limit = i64::try_from(MAX_LEARNING_AGGREGATES + 1).expect("aggregate limit fits i64");
    let mut statement = connection
        .prepare(
            "SELECT scope, cwd, skeleton, successful_count, last_used_at FROM command_aggregates ORDER BY scope, cwd, skeleton LIMIT ?1",
        )
        .map_err(|error| database_error("prepare command aggregates", error))?;
    let rows = statement
        .query_map([limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|error| database_error("read command aggregates", error))?;
    let mut aggregates = BTreeMap::new();
    let mut retained_bytes = 0_usize;
    for row in rows {
        let (scope, cwd, skeleton, successful_count, last_used_at) =
            row.map_err(|error| database_error("decode command aggregate", error))?;
        if aggregates.len() == MAX_LEARNING_AGGREGATES {
            return Err(LearningStoreError::TooManyAggregates);
        }
        retained_bytes = add_aggregate_bytes(retained_bytes, cwd.len() + skeleton.len())?;
        validate_skeleton(&skeleton, "stored skeleton")?;
        let key = CommandAggregateKey {
            scope: decode_scope(scope, cwd)?,
            skeleton,
        };
        aggregates.insert(
            key,
            UsageAggregate {
                successful_count: decode_nonnegative(successful_count, "stored count")?,
                last_used_at: decode_nonnegative(last_used_at, "stored timestamp")?,
            },
        );
    }
    Ok(aggregates)
}

fn load_transition_aggregates(
    connection: &Connection,
) -> Result<BTreeMap<TransitionAggregateKey, UsageAggregate>, LearningStoreError> {
    reject_invalid_aggregate_rows(
        connection,
        "SELECT EXISTS(
            SELECT 1 FROM transition_aggregates
            WHERE typeof(scope) != 'integer'
                OR scope NOT IN (0, 1)
                OR typeof(cwd) != 'blob'
                OR length(cwd) > 16384
                OR (scope = 0 AND length(cwd) != 0)
                OR (scope = 1 AND length(cwd) = 0)
                OR typeof(prior_skeleton) != 'text'
                OR length(CAST(prior_skeleton AS BLOB)) NOT BETWEEN 1 AND 4096
                OR typeof(current_skeleton) != 'text'
                OR length(CAST(current_skeleton AS BLOB)) NOT BETWEEN 1 AND 4096
                OR typeof(successful_count) != 'integer'
                OR successful_count < 0
                OR typeof(last_used_at) != 'integer'
                OR last_used_at < 0
        )",
        "stored transition aggregate",
    )?;
    let limit = i64::try_from(MAX_LEARNING_AGGREGATES + 1).expect("aggregate limit fits i64");
    let mut statement = connection
        .prepare(
            "SELECT scope, cwd, prior_skeleton, current_skeleton, successful_count, last_used_at FROM transition_aggregates ORDER BY scope, cwd, prior_skeleton, current_skeleton LIMIT ?1",
        )
        .map_err(|error| database_error("prepare transition aggregates", error))?;
    let rows = statement
        .query_map([limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|error| database_error("read transition aggregates", error))?;
    let mut aggregates = BTreeMap::new();
    let mut retained_bytes = 0_usize;
    for row in rows {
        let (scope, cwd, prior_skeleton, current_skeleton, successful_count, last_used_at) =
            row.map_err(|error| database_error("decode transition aggregate", error))?;
        if aggregates.len() == MAX_LEARNING_AGGREGATES {
            return Err(LearningStoreError::TooManyAggregates);
        }
        retained_bytes = add_aggregate_bytes(
            retained_bytes,
            cwd.len() + prior_skeleton.len() + current_skeleton.len(),
        )?;
        validate_skeleton(&prior_skeleton, "stored prior skeleton")?;
        validate_skeleton(&current_skeleton, "stored current skeleton")?;
        let key = TransitionAggregateKey {
            scope: decode_scope(scope, cwd)?,
            prior_skeleton,
            current_skeleton,
        };
        aggregates.insert(
            key,
            UsageAggregate {
                successful_count: decode_nonnegative(successful_count, "stored count")?,
                last_used_at: decode_nonnegative(last_used_at, "stored timestamp")?,
            },
        );
    }
    Ok(aggregates)
}

fn decode_nonnegative(value: i64, field: &'static str) -> Result<u64, LearningStoreError> {
    u64::try_from(value).map_err(|_| LearningStoreError::InvalidInput(field))
}

fn add_aggregate_bytes(current: usize, additional: usize) -> Result<usize, LearningStoreError> {
    let total = current
        .checked_add(additional)
        .ok_or(LearningStoreError::TooMuchAggregateData)?;
    if total > MAX_LEARNING_AGGREGATE_BYTES {
        return Err(LearningStoreError::TooMuchAggregateData);
    }
    Ok(total)
}

fn reject_invalid_aggregate_rows(
    connection: &Connection,
    query: &str,
    field: &'static str,
) -> Result<(), LearningStoreError> {
    let invalid: bool = connection
        .query_row(query, [], |row| row.get(0))
        .map_err(|error| database_error("validate learning aggregates", error))?;
    if invalid {
        return Err(LearningStoreError::InvalidInput(field));
    }
    Ok(())
}

fn decode_scope(scope: i64, cwd: Vec<u8>) -> Result<LearningScope, LearningStoreError> {
    match scope {
        0 if cwd.is_empty() => Ok(LearningScope::Global),
        1 if !cwd.is_empty() && cwd.len() <= MAX_LEARNING_CWD_BYTES => {
            Ok(LearningScope::Directory(decode_path(cwd)))
        }
        _ => Err(LearningStoreError::InvalidInput("stored scope")),
    }
}

fn configure_connection(connection: &Connection) -> Result<(), LearningStoreError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| database_error("configure learning busy timeout", error))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| database_error("configure learning foreign keys", error))?;
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(|error| database_error("configure trusted schema", error))?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| database_error("configure learning durability", error))?;
    Ok(())
}

fn configure_persistent_database(connection: &Connection) -> Result<(), LearningStoreError> {
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| database_error("configure learning journal", error))?;
    connection
        .pragma_update(None, "max_page_count", MAX_PAGE_COUNT)
        .map_err(|error| database_error("bound learning database", error))
}

fn migrate_schema(
    connection: &Connection,
    database: &Path,
    anchor: &DatabaseAnchor,
    migration_lock: &MigrationLock,
    timestamp_seconds: u64,
) -> Result<LearningMigrationOutcome, LearningStoreError> {
    let observed_version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| database_error("read learning schema version", error))?;
    if observed_version > CURRENT_LEARNING_SCHEMA {
        return Err(LearningStoreError::UnsupportedSchema);
    }
    if observed_version == CURRENT_LEARNING_SCHEMA {
        return Ok(LearningMigrationOutcome::Unneeded);
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(|error| database_error("freeze learning database for migration", error))?;
    let version: u32 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| database_error("recheck learning schema version", error))?;
    if version > CURRENT_LEARNING_SCHEMA {
        return Err(LearningStoreError::UnsupportedSchema);
    }
    if version == CURRENT_LEARNING_SCHEMA {
        transaction
            .rollback()
            .map_err(|error| database_error("release learning migration snapshot", error))?;
        return Ok(LearningMigrationOutcome::Unneeded);
    }
    verify_database_identity(database, anchor)?;
    migration_lock.validate_anchor()?;
    let backup = if anchor.created {
        None
    } else {
        let snapshot_source = open_backup_source(database, anchor)?;
        Some(write_database_backup(
            &snapshot_source,
            database,
            timestamp_seconds,
            version,
        )?)
    };
    verify_database_identity(database, anchor)?;
    migration_lock.validate_anchor()?;
    transaction
        .execute_batch(SCHEMA)
        .map_err(|error| database_error("migrate learning schema", error))?;
    migrate_legacy_tables(&transaction)?;
    transaction
        .pragma_update(None, "user_version", CURRENT_LEARNING_SCHEMA)
        .map_err(|error| database_error("record learning schema version", error))?;
    verify_database_identity(database, anchor)?;
    migration_lock.validate_anchor()?;
    transaction
        .commit()
        .map_err(|error| database_error("commit learning schema migration", error))?;
    verify_database_identity(database, anchor)?;
    Ok(LearningMigrationOutcome::Migrated {
        source_schema: version,
        backup,
    })
}

fn open_backup_source(
    database: &Path,
    anchor: &DatabaseAnchor,
) -> Result<Connection, LearningStoreError> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
    .map_err(|error| database_error("open learning backup source", error))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| database_error("configure learning backup timeout", error))?;
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(|error| database_error("configure learning backup schema", error))?;
    verify_database_identity(database, anchor)?;
    Ok(connection)
}

fn migrate_legacy_tables(transaction: &Transaction<'_>) -> Result<(), LearningStoreError> {
    if table_exists(transaction, "history_entries")? {
        import_legacy_commands(transaction)?;
    }
    if table_exists(transaction, "command_transitions")? {
        import_legacy_transitions(transaction)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ImportedAggregate {
    successful_count: i64,
    last_used_at: i64,
}

fn import_legacy_commands(transaction: &Transaction<'_>) -> Result<(), LearningStoreError> {
    let index = catalog::spec_index().map_err(|_| LearningStoreError::CatalogUnavailable)?;
    let limit = i64::try_from(MAX_LEARNING_AGGREGATES + 1).expect("aggregate limit fits i64");
    let mut statement = transaction
        .prepare(
            "SELECT
                CASE WHEN typeof(cmd) = 'text'
                    AND length(CAST(cmd AS BLOB)) BETWEEN 1 AND 65536
                    THEN cmd END,
                CASE WHEN typeof(cwd) = 'text'
                    AND length(CAST(cwd AS BLOB)) BETWEEN 1 AND 16384
                    THEN cwd END,
                CASE WHEN typeof(count) = 'integer' AND count >= 0 THEN count END,
                CASE
                    WHEN typeof(last_used) IN ('integer', 'real')
                        AND CAST(last_used AS INTEGER) >= 0
                        THEN CAST(last_used AS INTEGER)
                    WHEN typeof(last_used) = 'text'
                        AND CAST(strftime('%s', last_used) AS INTEGER) >= 0
                        THEN CAST(strftime('%s', last_used) AS INTEGER)
                END
             FROM history_entries LIMIT ?1",
        )
        .map_err(|error| database_error("prepare legacy command import", error))?;
    let rows = statement
        .query_map([limit], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .map_err(|error| database_error("read legacy commands", error))?;
    let mut aggregates = BTreeMap::<(i64, Vec<u8>, String), ImportedAggregate>::new();
    let mut retained_bytes = 0_usize;
    for (row_index, row) in rows.enumerate() {
        if row_index == MAX_LEARNING_AGGREGATES {
            return Err(LearningStoreError::TooManyAggregates);
        }
        let (Some(command), Some(cwd), Some(count), Some(last_used_at)) =
            row.map_err(|error| database_error("decode legacy command", error))?
        else {
            continue;
        };
        let Some(skeleton) = index.command_skeleton(&command) else {
            continue;
        };
        if validate_skeleton(&skeleton, "legacy skeleton").is_err() {
            continue;
        }
        let cwd = cwd.into_bytes();
        if cwd.is_empty() || cwd.len() > MAX_LEARNING_CWD_BYTES {
            continue;
        }
        retained_bytes = merge_imported_command(
            &mut aggregates,
            retained_bytes,
            (1, cwd, skeleton.clone()),
            count,
            last_used_at,
        )?;
        retained_bytes = merge_imported_command(
            &mut aggregates,
            retained_bytes,
            (0, Vec::new(), skeleton),
            count,
            last_used_at,
        )?;
    }
    drop(statement);
    for ((scope, cwd, skeleton), aggregate) in aggregates {
        transaction
            .execute(
                INSERT_IMPORTED_COMMAND,
                params![
                    scope,
                    cwd,
                    skeleton,
                    aggregate.successful_count,
                    aggregate.last_used_at
                ],
            )
            .map_err(|error| database_error("import legacy command aggregate", error))?;
    }
    Ok(())
}

fn merge_imported_command(
    aggregates: &mut BTreeMap<(i64, Vec<u8>, String), ImportedAggregate>,
    retained_bytes: usize,
    key: (i64, Vec<u8>, String),
    count: i64,
    last_used_at: i64,
) -> Result<usize, LearningStoreError> {
    if let Some(existing) = aggregates.get_mut(&key) {
        existing.successful_count = existing.successful_count.saturating_add(count);
        existing.last_used_at = existing.last_used_at.max(last_used_at);
        return Ok(retained_bytes);
    }
    if aggregates.len() == MAX_LEARNING_AGGREGATES {
        return Err(LearningStoreError::TooManyAggregates);
    }
    let retained_bytes = add_aggregate_bytes(retained_bytes, key.1.len() + key.2.len())?;
    aggregates.insert(
        key,
        ImportedAggregate {
            successful_count: count,
            last_used_at,
        },
    );
    Ok(retained_bytes)
}

fn import_legacy_transitions(transaction: &Transaction<'_>) -> Result<(), LearningStoreError> {
    let limit = i64::try_from(MAX_LEARNING_AGGREGATES + 1).expect("aggregate limit fits i64");
    let mut statement = transaction
        .prepare(
            "SELECT
                CASE WHEN typeof(prev_skeleton) = 'text'
                    AND length(CAST(prev_skeleton AS BLOB)) BETWEEN 1 AND 4096
                    THEN prev_skeleton END,
                CASE WHEN typeof(next_skeleton) = 'text'
                    AND length(CAST(next_skeleton AS BLOB)) BETWEEN 1 AND 4096
                    THEN next_skeleton END,
                CASE WHEN typeof(cwd) = 'text'
                    AND length(CAST(cwd AS BLOB)) BETWEEN 1 AND 16384
                    THEN cwd END,
                CASE WHEN typeof(count) = 'integer' AND count >= 0 THEN count END,
                CASE
                    WHEN typeof(last_used) IN ('integer', 'real')
                        AND CAST(last_used AS INTEGER) >= 0
                        THEN CAST(last_used AS INTEGER)
                    WHEN typeof(last_used) = 'text'
                        AND CAST(strftime('%s', last_used) AS INTEGER) >= 0
                        THEN CAST(strftime('%s', last_used) AS INTEGER)
                END
             FROM command_transitions LIMIT ?1",
        )
        .map_err(|error| database_error("prepare legacy transition import", error))?;
    let rows = statement
        .query_map([limit], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(|error| database_error("read legacy transitions", error))?;
    let mut aggregates = BTreeMap::<(i64, Vec<u8>, String, String), ImportedAggregate>::new();
    let mut retained_bytes = 0_usize;
    for (row_index, row) in rows.enumerate() {
        if row_index == MAX_LEARNING_AGGREGATES {
            return Err(LearningStoreError::TooManyAggregates);
        }
        let (Some(prior), Some(current), Some(cwd), Some(count), Some(last_used_at)) =
            row.map_err(|error| database_error("decode legacy transition", error))?
        else {
            continue;
        };
        if validate_skeleton(&prior, "legacy prior skeleton").is_err()
            || validate_skeleton(&current, "legacy current skeleton").is_err()
        {
            continue;
        }
        let cwd = cwd.into_bytes();
        if cwd.is_empty() || cwd.len() > MAX_LEARNING_CWD_BYTES {
            continue;
        }
        retained_bytes = merge_imported_transition(
            &mut aggregates,
            retained_bytes,
            (1, cwd, prior.clone(), current.clone()),
            count,
            last_used_at,
        )?;
        retained_bytes = merge_imported_transition(
            &mut aggregates,
            retained_bytes,
            (0, Vec::new(), prior, current),
            count,
            last_used_at,
        )?;
    }
    drop(statement);
    for ((scope, cwd, prior, current), aggregate) in aggregates {
        transaction
            .execute(
                INSERT_IMPORTED_TRANSITION,
                params![
                    scope,
                    cwd,
                    prior,
                    current,
                    aggregate.successful_count,
                    aggregate.last_used_at
                ],
            )
            .map_err(|error| database_error("import legacy transition aggregate", error))?;
    }
    Ok(())
}

fn merge_imported_transition(
    aggregates: &mut BTreeMap<(i64, Vec<u8>, String, String), ImportedAggregate>,
    retained_bytes: usize,
    key: (i64, Vec<u8>, String, String),
    count: i64,
    last_used_at: i64,
) -> Result<usize, LearningStoreError> {
    if let Some(existing) = aggregates.get_mut(&key) {
        existing.successful_count = existing.successful_count.saturating_add(count);
        existing.last_used_at = existing.last_used_at.max(last_used_at);
        return Ok(retained_bytes);
    }
    if aggregates.len() == MAX_LEARNING_AGGREGATES {
        return Err(LearningStoreError::TooManyAggregates);
    }
    let retained_bytes =
        add_aggregate_bytes(retained_bytes, key.1.len() + key.2.len() + key.3.len())?;
    aggregates.insert(
        key,
        ImportedAggregate {
            successful_count: count,
            last_used_at,
        },
    );
    Ok(retained_bytes)
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, LearningStoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(|error| database_error("inspect legacy learning schema", error))
}

fn write_database_backup(
    connection: &Connection,
    source: &Path,
    timestamp_seconds: u64,
    source_schema: u32,
) -> Result<PathBuf, LearningStoreError> {
    let name = source
        .file_name()
        .unwrap_or_else(|| OsStr::new(DATABASE_FILE_NAME));
    for collision in 0..MAX_BACKUP_COLLISIONS {
        let mut candidate_name = OsString::from(name);
        candidate_name.push(format!(".backup.{timestamp_seconds}"));
        if collision != 0 {
            candidate_name.push(format!(".{collision}"));
        }
        let candidate = source.with_file_name(candidate_name);
        let candidate_text = candidate
            .to_str()
            .ok_or(LearningStoreError::InvalidInput("backup path"))?;
        let backup_anchor = match create_private_backup_anchor(&candidate) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(LearningStoreError::io("create learning backup", error)),
        };
        connection
            .execute("VACUUM main INTO ?1", [candidate_text])
            .map_err(|error| database_error("snapshot learning database", error))?;
        validate_database_backup(&candidate, source_schema, &backup_anchor)?;
        return Ok(candidate);
    }
    Err(LearningStoreError::BackupNameExhausted)
}

fn create_private_backup_anchor(path: &Path) -> Result<File, io::Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    options.open(path)
}

fn validate_database_backup(
    path: &Path,
    expected_schema: u32,
    backup_anchor: &File,
) -> Result<(), LearningStoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| LearningStoreError::io("inspect learning backup", error))?;
    validate_database_metadata(&metadata).map_err(|_| LearningStoreError::InvalidBackup)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|error| LearningStoreError::io("open learning backup", error))?;
    let opened = file
        .metadata()
        .map_err(|error| LearningStoreError::io("inspect learning backup", error))?;
    validate_database_metadata(&opened).map_err(|_| LearningStoreError::InvalidBackup)?;
    let anchored = backup_anchor
        .metadata()
        .map_err(|error| LearningStoreError::io("inspect learning backup anchor", error))?;
    validate_database_metadata(&anchored).map_err(|_| LearningStoreError::InvalidBackup)?;
    #[cfg(unix)]
    if metadata.dev() != opened.dev()
        || metadata.ino() != opened.ino()
        || metadata.dev() != anchored.dev()
        || metadata.ino() != anchored.ino()
    {
        return Err(LearningStoreError::InvalidBackup);
    }
    set_file_private(&file)?;

    let backup = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
    .map_err(|error| database_error("open learning backup snapshot", error))?;
    let integrity: String = backup
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| database_error("validate learning backup snapshot", error))?;
    let schema: u32 = backup
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| database_error("validate learning backup schema", error))?;
    if integrity != "ok" || schema != expected_schema {
        return Err(LearningStoreError::InvalidBackup);
    }
    drop(backup);
    let final_metadata = fs::symlink_metadata(path)
        .map_err(|error| LearningStoreError::io("reinspect learning backup", error))?;
    validate_database_metadata(&final_metadata).map_err(|_| LearningStoreError::InvalidBackup)?;
    #[cfg(unix)]
    if final_metadata.dev() != anchored.dev() || final_metadata.ino() != anchored.ino() {
        return Err(LearningStoreError::InvalidBackup);
    }
    file.sync_all()
        .map_err(|error| LearningStoreError::io("sync learning backup", error))?;
    sync_directory(required_parent(path)?)
        .map_err(|error| LearningStoreError::io("sync learning backup directory", error))
}

fn unix_seconds_now() -> Result<u64, LearningStoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LearningStoreError::ClockBeforeEpoch)
        .map(|duration| duration.as_secs())
}

struct MigrationLock {
    file: File,
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl MigrationLock {
    fn validate_anchor(&self) -> Result<(), LearningStoreError> {
        let opened = self
            .file
            .metadata()
            .map_err(|error| LearningStoreError::io("inspect learning migration lock", error))?;
        let current = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(LearningStoreError::LockReplaced);
            }
            Err(error) => {
                return Err(LearningStoreError::io(
                    "reinspect learning migration lock",
                    error,
                ));
            }
        };
        if current.file_type().is_symlink() || !current.is_file() || !opened.is_file() {
            return Err(LearningStoreError::LockReplaced);
        }
        #[cfg(unix)]
        if opened.nlink() != 1
            || current.nlink() != 1
            || opened.dev() != self.device
            || opened.ino() != self.inode
            || current.dev() != self.device
            || current.ino() != self.inode
        {
            return Err(LearningStoreError::LockReplaced);
        }
        Ok(())
    }
}

fn acquire_migration_lock(database: &Path) -> Result<MigrationLock, LearningStoreError> {
    acquire_migration_lock_with_timeout(database, BUSY_TIMEOUT)
}

fn acquire_migration_lock_with_timeout(
    database: &Path,
    timeout: Duration,
) -> Result<MigrationLock, LearningStoreError> {
    let lock_path = database_sidecar_path(database, ".migration.lock")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let file = options
        .open(&lock_path)
        .map_err(|error| LearningStoreError::io("open learning migration lock", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| LearningStoreError::io("inspect learning migration lock", error))?;
    validate_database_metadata(&metadata)?;
    set_file_private(&file)?;
    let started = Instant::now();
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(error)
                if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::WOULDBLOCK =>
            {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return Err(LearningStoreError::Busy);
                }
                std::thread::sleep(LOCK_RETRY_INTERVAL.min(timeout.saturating_sub(elapsed)));
            }
            Err(error) => {
                return Err(LearningStoreError::io(
                    "lock learning migration",
                    rustix_error(error),
                ));
            }
        }
    }
    let lock = MigrationLock {
        file,
        path: lock_path,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    };
    lock.validate_anchor()?;
    Ok(lock)
}

struct DatabaseAnchor {
    file: File,
    created: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn open_database_anchor(path: &Path) -> Result<DatabaseAnchor, LearningStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_database_metadata(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LearningStoreError::io(
                "inspect learning database path",
                error,
            ));
        }
    }
    let (file, created) = open_or_create_database(path)?;
    if created {
        file.sync_all()
            .map_err(|error| LearningStoreError::io("sync learning database", error))?;
        sync_directory(required_parent(path)?)
            .map_err(|error| LearningStoreError::io("sync learning data directory", error))?;
    }
    let metadata = file
        .metadata()
        .map_err(|error| LearningStoreError::io("inspect learning database", error))?;
    validate_database_metadata(&metadata)?;
    set_file_private(&file)?;
    Ok(DatabaseAnchor {
        file,
        created,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

fn open_or_create_database(path: &Path) -> Result<(File, bool), LearningStoreError> {
    for _ in 0..3 {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        options.custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
        match options.open(path) {
            Ok(file) => return Ok((file, false)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LearningStoreError::io("open learning database", error));
            }
        }

        let mut create = OpenOptions::new();
        create.read(true).write(true).create_new(true);
        #[cfg(unix)]
        create
            .mode(0o600)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
        match create.open(path) {
            Ok(file) => return Ok((file, true)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(LearningStoreError::io("create learning database", error));
            }
        }
    }
    Err(LearningStoreError::Io {
        operation: "open learning database after concurrent creation",
        kind: io::ErrorKind::WouldBlock,
    })
}

fn validate_database_metadata(metadata: &fs::Metadata) -> Result<(), LearningStoreError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_DATABASE_BYTES
    {
        return Err(LearningStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(LearningStoreError::UnsafeFileType);
    }
    Ok(())
}

fn validate_existing_sidecars(path: &Path) -> Result<(), LearningStoreError> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let sidecar = database_sidecar_path(path, suffix)?;
        validate_existing_sidecar(&sidecar)?;
    }
    Ok(())
}

fn validate_existing_sidecar(sidecar: &Path) -> Result<(), LearningStoreError> {
    for _ in 0..8 {
        let metadata = match fs::symlink_metadata(sidecar) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(LearningStoreError::io(
                    "inspect learning database sidecar",
                    error,
                ));
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_DATABASE_BYTES
        {
            return Err(LearningStoreError::UnsafeFileType);
        }
        #[cfg(unix)]
        if metadata.nlink() == 0 {
            continue;
        }
        #[cfg(unix)]
        if metadata.nlink() != 1 {
            return Err(LearningStoreError::UnsafeFileType);
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        options.custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
        let file = match options.open(sidecar) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LearningStoreError::io(
                    "open learning database sidecar",
                    error,
                ));
            }
        };
        let opened = file
            .metadata()
            .map_err(|error| LearningStoreError::io("inspect learning database sidecar", error))?;
        if !opened.is_file() || opened.len() > MAX_DATABASE_BYTES {
            return Err(LearningStoreError::UnsafeFileType);
        }
        #[cfg(unix)]
        if opened.nlink() == 0 {
            continue;
        }
        #[cfg(unix)]
        if opened.nlink() != 1 {
            return Err(LearningStoreError::UnsafeFileType);
        }
        let current = match fs::symlink_metadata(sidecar) {
            Ok(current) => current,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LearningStoreError::io(
                    "reinspect learning database sidecar",
                    error,
                ));
            }
        };
        #[cfg(unix)]
        if current.nlink() == 0 {
            continue;
        }
        validate_database_metadata(&current)?;
        #[cfg(unix)]
        if opened.dev() != current.dev() || opened.ino() != current.ino() {
            continue;
        }
        set_file_private(&file)?;
        return Ok(());
    }
    Err(LearningStoreError::Busy)
}

fn database_sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, LearningStoreError> {
    let name = path.file_name().ok_or(LearningStoreError::MissingParent)?;
    let mut sidecar = OsString::from(name);
    sidecar.push(suffix);
    Ok(path.with_file_name(sidecar))
}

fn verify_database_identity(
    path: &Path,
    anchor: &DatabaseAnchor,
) -> Result<(), LearningStoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| LearningStoreError::io("reinspect learning database", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LearningStoreError::UnsafeFileType);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 || metadata.dev() != anchor.device || metadata.ino() != anchor.inode {
        return Err(LearningStoreError::UnsafeFileType);
    }
    let opened = anchor
        .file
        .metadata()
        .map_err(|error| LearningStoreError::io("verify learning database", error))?;
    if !opened.is_file() {
        return Err(LearningStoreError::UnsafeFileType);
    }
    Ok(())
}

fn required_parent(path: &Path) -> Result<&Path, LearningStoreError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(LearningStoreError::MissingParent)
}

fn secure_directory(path: &Path) -> Result<(), LearningStoreError> {
    fs::create_dir_all(path)
        .map_err(|error| LearningStoreError::io("create learning data directory", error))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| LearningStoreError::io("inspect learning data directory", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LearningStoreError::UnsafeDirectoryType);
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| LearningStoreError::io("secure learning data directory", error))?;
    Ok(())
}

fn set_file_private(file: &File) -> Result<(), LearningStoreError> {
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| LearningStoreError::io("secure learning database", error))?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), io::Error> {
    File::open(path)?.sync_all()
}

fn rustix_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn encode_path(path: &Path) -> Vec<u8> {
    path.as_os_str().to_string_lossy().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_path(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn decode_path(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
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

fn database_error(operation: &'static str, error: rusqlite::Error) -> LearningStoreError {
    use rusqlite::ErrorCode;

    let classified = match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) =>
        {
            LearningStoreError::CorruptDatabase
        }
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            LearningStoreError::Busy
        }
        _ => LearningStoreError::Database { operation },
    };
    drop(error);
    classified
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::mpsc::{self, TryRecvError};
    use std::sync::{Arc, Barrier};
    use std::thread;

    const LIBRARY: &str = "/home/troy/Greendale/library";

    fn event(command: &str, timestamp: u64, outcome: CommandOutcome) -> LearningEvent {
        LearningEvent::new(command, "git status", LIBRARY, timestamp, outcome)
            .with_prior_skeleton("git add")
    }

    fn local_command(state: &LearningState) -> UsageAggregate {
        state.commands[&CommandAggregateKey {
            scope: LearningScope::Directory(PathBuf::from(LIBRARY)),
            skeleton: "git status".to_owned(),
        }]
    }

    #[test]
    fn exact_events_and_aggregate_success_rules_commit_together() {
        let temporary = tempfile::tempdir().unwrap();
        let store = LearningStore::new(temporary.path().join("argmax/learning.sqlite3"));
        store
            .record(&event("git status --short", 10, CommandOutcome::Success))
            .unwrap();
        store
            .record(&event(
                "git status --porcelain",
                20,
                CommandOutcome::Failure,
            ))
            .unwrap();

        let state = store.load().unwrap();
        assert_eq!(local_command(&state).successful_count, 1);
        assert_eq!(local_command(&state).last_used_at, 20);
        assert_eq!(state.commands.len(), 2);
        assert_eq!(state.transitions.len(), 2);

        let connection = Connection::open(store.path()).unwrap();
        let commands: Vec<String> = connection
            .prepare("SELECT command FROM learning_events ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(commands, ["git status --short", "git status --porcelain"]);
    }

    #[test]
    fn concurrent_writers_preserve_every_success() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(LearningStore::new(
            temporary.path().join("argmax/learning.sqlite3"),
        ));
        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for index in 0..8 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                store
                    .record(&event(
                        "git status --short",
                        100 + index,
                        CommandOutcome::Success,
                    ))
                    .unwrap();
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        let aggregate = local_command(&store.load().unwrap());
        assert_eq!(aggregate.successful_count, 8);
        assert_eq!(aggregate.last_used_at, 107);
    }

    #[test]
    fn command_and_transition_loads_share_one_snapshot_under_writes() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(LearningStore::new(
            temporary.path().join("argmax/learning.sqlite3"),
        ));
        store
            .record(&event("git status --short", 1, CommandOutcome::Success))
            .unwrap();
        let writer_store = Arc::clone(&store);
        let writer = thread::spawn(move || {
            for timestamp in 2..=200 {
                writer_store
                    .record(&event(
                        "git status --short",
                        timestamp,
                        CommandOutcome::Success,
                    ))
                    .unwrap();
            }
        });

        for _ in 0..200 {
            let state = store.load().unwrap();
            let command = state.commands[&CommandAggregateKey {
                scope: LearningScope::Global,
                skeleton: "git status".to_owned(),
            }];
            let transition = state.transitions[&TransitionAggregateKey {
                scope: LearningScope::Global,
                prior_skeleton: "git add".to_owned(),
                current_skeleton: "git status".to_owned(),
            }];
            assert_eq!(command, transition);
        }
        writer.join().unwrap();
    }

    #[test]
    fn adopts_version_zero_compatible_aggregate_data() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("argmax/learning.sqlite3");
        let store = LearningStore::new(&path);
        store.initialize().unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 0).unwrap();
        connection
            .execute(
                "INSERT INTO command_aggregates VALUES (0, X'', 'cargo test', 4, 50)",
                [],
            )
            .unwrap();
        drop(connection);

        store.initialize().unwrap();
        let state = store.load().unwrap();
        assert_eq!(
            state.commands[&CommandAggregateKey {
                scope: LearningScope::Global,
                skeleton: "cargo test".to_owned(),
            }],
            UsageAggregate {
                successful_count: 4,
                last_used_at: 50,
            }
        );
    }

    #[test]
    fn imports_the_existing_history_database_without_removing_legacy_rows() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("argmax/history.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE history_entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    cmd TEXT NOT NULL,
                    cwd TEXT NOT NULL,
                    count INTEGER DEFAULT 1,
                    last_used TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(cmd, cwd)
                );
                CREATE TABLE command_transitions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    prev_skeleton TEXT NOT NULL,
                    next_skeleton TEXT NOT NULL,
                    cwd TEXT NOT NULL,
                    count INTEGER DEFAULT 1,
                    last_used TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(prev_skeleton, next_skeleton, cwd)
                );
                INSERT INTO history_entries (cmd, cwd, count, last_used)
                    VALUES ('git status', '/home/troy/Greendale/library', 3,
                            '2009-02-13 23:31:30');
                INSERT INTO command_transitions (
                    prev_skeleton, next_skeleton, cwd, count, last_used
                ) VALUES ('git add', 'git status',
                          '/home/troy/Greendale/library', 2,
                          '2009-02-13 23:31:30');
                ",
            )
            .unwrap();
        drop(connection);

        let store = LearningStore::new(&path);
        store.initialize().unwrap();
        let state = store.load().unwrap();
        assert_eq!(
            state.commands[&CommandAggregateKey {
                scope: LearningScope::Directory(PathBuf::from(LIBRARY)),
                skeleton: "git status".to_owned(),
            }],
            UsageAggregate {
                successful_count: 3,
                last_used_at: 1_234_567_890,
            }
        );
        assert_eq!(
            state.transitions[&TransitionAggregateKey {
                scope: LearningScope::Global,
                prior_skeleton: "git add".to_owned(),
                current_skeleton: "git status".to_owned(),
            }],
            UsageAggregate {
                successful_count: 2,
                last_used_at: 1_234_567_890,
            }
        );
        let connection = Connection::open(path).unwrap();
        let legacy_commands: u64 = connection
            .query_row("SELECT COUNT(*) FROM history_entries", [], |row| row.get(0))
            .unwrap();
        let legacy_transitions: u64 = connection
            .query_row("SELECT COUNT(*) FROM command_transitions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!((legacy_commands, legacy_transitions), (1, 1));
    }

    #[test]
    fn exact_iris_commands_are_canonicalized_and_backed_up_with_wal_data() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("iris/history.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE history_entries (
                    id INTEGER PRIMARY KEY,
                    cmd,
                    cwd,
                    count,
                    last_used
                );
                INSERT INTO history_entries VALUES
                    (1, 'git commit -m first', '/home/troy/Greendale/library', 2, 100),
                    (2, 'git commit --amend', '/home/troy/Greendale/library', 3, 110),
                    (3, 'git commit -m second', '/home/troy/Greendale/cafeteria', 4, 120),
                    (4, 'greendale-tool --verbose Troy', '/home/troy/Greendale/library', 1, 130),
                    (5, X'00FF', '/home/troy/Greendale/library', 99, 140),
                    (6, 'git status', '/home/troy/Greendale/library', 'bad', 150);",
            )
            .unwrap();
        let occupied = path.with_file_name("history.db.backup.42");
        fs::write(&occupied, b"occupied").unwrap();

        let store = LearningStore::new(&path);
        let LearningMigrationOutcome::Migrated {
            source_schema,
            backup: Some(backup),
        } = store.migrate_if_needed(42).unwrap()
        else {
            panic!("expected a backed-up migration");
        };
        assert_eq!(source_schema, 0);
        assert_eq!(backup.file_name().unwrap(), "history.db.backup.42.1");
        assert_eq!(fs::read(&occupied).unwrap(), b"occupied");

        let backup_connection = Connection::open_with_flags(
            &backup,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )
        .unwrap();
        assert_eq!(
            backup_connection
                .query_row("SELECT COUNT(*) FROM history_entries", [], |row| {
                    row.get::<_, u64>(0)
                })
                .unwrap(),
            6
        );
        drop(backup_connection);
        drop(connection);

        let state = store.load().unwrap();
        assert_eq!(
            state.commands[&CommandAggregateKey {
                scope: LearningScope::Directory(PathBuf::from(LIBRARY)),
                skeleton: "git commit".to_owned(),
            }],
            UsageAggregate {
                successful_count: 5,
                last_used_at: 110,
            }
        );
        assert_eq!(
            state.commands[&CommandAggregateKey {
                scope: LearningScope::Global,
                skeleton: "git commit".to_owned(),
            }],
            UsageAggregate {
                successful_count: 9,
                last_used_at: 120,
            }
        );
        assert_eq!(
            state.commands[&CommandAggregateKey {
                scope: LearningScope::Global,
                skeleton: "greendale-tool".to_owned(),
            }]
                .successful_count,
            1
        );
        assert!(!state.commands.keys().any(|key| key.skeleton.contains("-m")));
        assert!(matches!(
            store.migrate_if_needed(43).unwrap(),
            LearningMigrationOutcome::Unneeded
        ));
        assert!(!path.with_file_name("history.db.backup.43").exists());
        assert_eq!(
            Connection::open(&path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM history_entries", [], |row| {
                    row.get::<_, u64>(0)
                })
                .unwrap(),
            6
        );
    }

    #[test]
    fn failed_migration_keeps_source_and_a_valid_pre_mutation_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("iris/history.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE history_entries (id INTEGER PRIMARY KEY, wrong TEXT);
                 INSERT INTO history_entries VALUES (1, 'untouched');",
            )
            .unwrap();
        drop(connection);

        let store = LearningStore::new(&path);
        assert!(store.migrate_if_needed(77).is_err());
        let source = Connection::open(&path).unwrap();
        assert_eq!(
            source
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            0
        );
        assert_eq!(
            source
                .query_row("SELECT wrong FROM history_entries", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "untouched"
        );
        assert!(!table_exists(&source, "command_aggregates").unwrap());
        let backup = Connection::open_with_flags(
            path.with_file_name("history.db.backup.77"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }

    #[test]
    fn migration_waits_for_wal_writer_and_backup_matches_import_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("iris/history.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE history_entries (
                    id INTEGER PRIMARY KEY, cmd TEXT, cwd TEXT,
                    count INTEGER, last_used INTEGER
                );
                INSERT INTO history_entries VALUES
                    (1, 'git commit -m first', '/tmp', 1, 10);",
            )
            .unwrap();
        let writer_transaction = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        writer_transaction
            .execute(
                "INSERT INTO history_entries VALUES
                    (2, 'git commit --amend', '/tmp', 2, 20)",
                [],
            )
            .unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let worker_path = path.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            worker_barrier.wait();
            sender
                .send(LearningStore::new(worker_path).migrate_if_needed(91))
                .unwrap();
        });
        barrier.wait();
        thread::sleep(Duration::from_millis(150));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));

        writer_transaction.commit().unwrap();
        let outcome = receiver.recv_timeout(BUSY_TIMEOUT).unwrap().unwrap();
        worker.join().unwrap();
        let LearningMigrationOutcome::Migrated {
            backup: Some(backup),
            ..
        } = outcome
        else {
            panic!("expected backed-up migration");
        };
        let snapshot =
            Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            snapshot
                .query_row("SELECT COUNT(*) FROM history_entries", [], |row| {
                    row.get::<_, u64>(0)
                })
                .unwrap(),
            2
        );
        drop(snapshot);

        let state = LearningStore::new(&path).load().unwrap();
        assert_eq!(
            state.commands[&CommandAggregateKey {
                scope: LearningScope::Global,
                skeleton: "git commit".to_owned(),
            }],
            UsageAggregate {
                successful_count: 3,
                last_used_at: 20,
            }
        );
    }

    #[test]
    fn discovers_iris_history_in_isolated_process_and_prefers_current() {
        const CHILD_CASE: &str = "ARGMAX_LEARNING_DISCOVERY_CHILD_CASE";
        if let Ok(case) = std::env::var(CHILD_CASE) {
            let expected = PathBuf::from(std::env::var_os("ARGMAX_EXPECTED_DATABASE").unwrap());
            if case == "current" {
                let project = ProjectDirs::from("", "", "argmax").unwrap();
                let current = project.data_dir().join(DATABASE_FILE_NAME);
                fs::create_dir_all(current.parent().unwrap()).unwrap();
                let connection = Connection::open(&current).unwrap();
                connection
                    .pragma_update(None, "user_version", CURRENT_LEARNING_SCHEMA)
                    .unwrap();
                drop(connection);
                assert_eq!(LearningStore::discover().unwrap().path(), current);
            } else {
                let store = LearningStore::discover().unwrap();
                assert_eq!(store.path(), expected);
                assert!(matches!(
                    store.migrate_if_needed(88).unwrap(),
                    LearningMigrationOutcome::Migrated { .. }
                ));
                assert!(store.path().exists());
                assert!(matches!(
                    store.migrate_if_needed(89).unwrap(),
                    LearningMigrationOutcome::Unneeded
                ));
            }
            return;
        }

        let test_name =
            "learning_store::tests::discovers_iris_history_in_isolated_process_and_prefers_current";
        for case in ["iris", "current"] {
            let temporary = tempfile::tempdir().unwrap();
            let home = temporary.path().join("home");
            let xdg = temporary.path().join("xdg");
            let iris = xdg.join("iris/history.db");
            fs::create_dir_all(iris.parent().unwrap()).unwrap();
            let connection = Connection::open(&iris).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE history_entries (
                        id INTEGER PRIMARY KEY, cmd TEXT, cwd TEXT,
                        count INTEGER, last_used INTEGER
                    );
                    INSERT INTO history_entries VALUES
                        (1, 'git status --short', '/tmp', 1, 1);",
                )
                .unwrap();
            drop(connection);
            let fallback = home.join(".local/share/iris/history.db");
            fs::create_dir_all(fallback.parent().unwrap()).unwrap();
            let fallback_connection = Connection::open(&fallback).unwrap();
            fallback_connection
                .pragma_update(None, "user_version", CURRENT_LEARNING_SCHEMA + 10)
                .unwrap();
            drop(fallback_connection);
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(test_name)
                .arg("--nocapture")
                .env(CHILD_CASE, case)
                .env("ARGMAX_EXPECTED_DATABASE", &iris)
                .env("HOME", &home)
                .env("XDG_DATA_HOME", &xdg)
                .status()
                .unwrap();
            assert!(status.success(), "learning discovery child {case} failed");
        }
    }

    #[test]
    fn corrupt_database_is_isolated_and_not_replaced() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("argmax/learning.sqlite3");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = b"Greendale is not a database";
        fs::write(&path, original).unwrap();
        let store = LearningStore::new(&path);
        assert!(matches!(
            store.load(),
            Err(LearningStoreError::CorruptDatabase)
        ));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn enforces_private_permissions_and_rejects_links() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("argmax/learning.sqlite3");
        let store = LearningStore::new(&path);
        store.initialize().unwrap();
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let target = temporary.path().join("target.sqlite3");
        fs::write(&target, []).unwrap();
        let linked = temporary.path().join("linked.sqlite3");
        symlink(&target, &linked).unwrap();
        assert!(LearningStore::new(linked).initialize().is_err());

        let hardlinked = temporary.path().join("hardlinked.sqlite3");
        fs::hard_link(&target, &hardlinked).unwrap();
        assert!(matches!(
            LearningStore::new(hardlinked).initialize(),
            Err(LearningStoreError::UnsafeFileType)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn replaced_migration_lock_fails_before_schema_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("argmax/history.db");
        secure_directory(path.parent().unwrap()).unwrap();
        let lock = acquire_migration_lock(&path).unwrap();
        let anchor = open_database_anchor(&path).unwrap();
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )
        .unwrap();
        configure_connection(&connection).unwrap();

        fs::remove_file(&lock.path).unwrap();
        File::create(&lock.path).unwrap();
        assert!(matches!(
            migrate_schema(&connection, &path, &anchor, &lock, 99),
            Err(LearningStoreError::LockReplaced)
        ));
        assert_eq!(
            connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            0
        );
        assert!(!table_exists(&connection, "command_aggregates").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn migration_lock_contention_returns_busy_within_its_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("argmax/history.db");
        secure_directory(path.parent().unwrap()).unwrap();
        let held = acquire_migration_lock_with_timeout(&path, Duration::from_secs(1)).unwrap();

        let started = Instant::now();
        assert!(matches!(
            acquire_migration_lock(&path),
            Err(LearningStoreError::Busy)
        ));
        let elapsed = started.elapsed();
        assert!(elapsed >= BUSY_TIMEOUT);
        assert!(elapsed < BUSY_TIMEOUT.saturating_add(Duration::from_secs(2)));

        drop(held);
        acquire_migration_lock(&path).unwrap();
    }

    #[test]
    fn input_bounds_fail_before_creating_a_database() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("argmax/learning.sqlite3");
        let store = LearningStore::new(&path);
        let oversized = LearningEvent::new(
            "x".repeat(MAX_LEARNING_COMMAND_BYTES + 1),
            "x",
            LIBRARY,
            1,
            CommandOutcome::Success,
        );
        assert!(matches!(
            store.record(&oversized),
            Err(LearningStoreError::InvalidInput("command"))
        ));
        assert!(!path.exists());
    }

    #[test]
    fn debug_and_errors_do_not_expose_paths_or_sqlite_payloads() {
        let store = LearningStore::new("/private/Troy-secret/learning.sqlite3");
        let debug = format!("{store:?}");
        assert!(!debug.contains("Troy-secret"));
        let error = database_error(
            "read learning database",
            rusqlite::Error::InvalidParameterName("Annie-secret".to_owned()),
        );
        assert!(!format!("{error:?}").contains("Annie-secret"));
    }
}
