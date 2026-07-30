//! Private, bounded diagnostic logs and crash-report storage.
//!
//! This module owns diagnostic files only. Terminal restoration, panic
//! containment, and rescue-shell startup remain responsibilities of the
//! interactive session boundary.

use std::collections::hash_map::RandomState;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::fcntl::{AtFlags, FlockArg, OFlag, open, openat, renameat};
#[cfg(unix)]
use nix::sys::stat::{FileStat, Mode, SFlag, fchmod, fstat, fstatat};
#[cfg(unix)]
use nix::unistd::{UnlinkatFlags, close, fsync, linkat, read, unlinkat};
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

/// Maximum size of the active debug log before rotation: five MiB.
pub const MAX_DEBUG_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// Maximum bytes retained from one diagnostic message.
pub const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 16 * 1024;
/// Maximum bytes retained from one crash failure description.
pub const MAX_CRASH_FAILURE_BYTES: usize = 64 * 1024;
/// Maximum bytes retained from one crash backtrace.
pub const MAX_CRASH_BACKTRACE_BYTES: usize = 512 * 1024;
/// Maximum directory entries inspected by crash-report operations.
pub const MAX_CRASH_DIRECTORY_ENTRIES: usize = 4_096;

const DEBUG_LOG_NAME: &str = "debug.log";
const PREVIOUS_DEBUG_LOG_NAME: &str = "debug.previous.log";
const DEBUG_LOG_LOCK_NAME: &str = ".debug.lock";
const DEBUG_TEMP_PREFIX: &str = ".debug-transaction-";
const APPLICATION_DIRECTORY_NAME: &str = "argmax";
const LOG_DIRECTORY_NAME: &str = "logs";
const CRASH_DIRECTORY_NAME: &str = "crashes";
const CRASH_PREFIX: &str = "crash-";
const CRASH_SUFFIX: &str = ".log";
const MAX_CONTEXT_FIELD_BYTES: usize = 256;
const MAX_COMPONENT_BYTES: usize = 64;
const REDACTED: &str = "<redacted>";
static CRASH_COUNTER: AtomicU64 = AtomicU64::new(0);
static DEBUG_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static DEBUG_LOG_WRITE_GATE: Mutex<()> = Mutex::new(());

#[cfg(unix)]
struct OwnedDescriptor(RawFd);

#[cfg(unix)]
impl OwnedDescriptor {
    const fn raw(&self) -> RawFd {
        self.0
    }
}

#[cfg(unix)]
impl Drop for OwnedDescriptor {
    fn drop(&mut self) {
        let _ = close(self.0);
    }
}

/// Warning shown once when debug logging starts.
pub const DEBUG_LOGGING_WARNING: &str =
    "argmax debug logging is enabled; typed commands may contain secrets";

/// Severity attached to one debug-log record.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticLevel {
    /// Detailed internal tracing.
    Trace,
    /// Developer-oriented state transitions.
    Debug,
    /// Informational lifecycle events.
    Info,
    /// Recoverable failures or degraded behavior.
    Warn,
    /// Actionable internal failures.
    Error,
}

/// Closed diagnostic-session field identifiers used in validation errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextField {
    /// Opaque per-session identifier.
    SessionId,
    /// Running argmax version.
    Version,
    /// Selected shell name.
    Shell,
}

impl fmt::Display for ContextField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SessionId => "session id",
            Self::Version => "version",
            Self::Shell => "shell",
        })
    }
}

impl DiagnosticLevel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Content-free metadata included with every debug-log record.
#[derive(Clone, Eq, PartialEq)]
pub struct DiagnosticSession {
    session_id: Box<str>,
    version: Box<str>,
    shell: Box<str>,
}

impl DiagnosticSession {
    /// Validates stable session metadata.
    ///
    /// # Errors
    ///
    /// Returns [`DiagnosticError::InvalidContext`] when a field is empty,
    /// oversized, or contains bytes unsuitable for the line-oriented format.
    pub fn new(
        session_id: impl Into<Box<str>>,
        version: impl Into<Box<str>>,
        shell: impl Into<Box<str>>,
    ) -> Result<Self, DiagnosticError> {
        let session_id = session_id.into();
        let version = version.into();
        let shell = shell.into();
        validate_context_field(ContextField::SessionId, &session_id)?;
        validate_context_field(ContextField::Version, &version)?;
        validate_context_field(ContextField::Shell, &shell)?;
        Ok(Self {
            session_id,
            version,
            shell,
        })
    }
}

impl fmt::Debug for DiagnosticSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiagnosticSession")
            .field("session_id_bytes", &self.session_id.len())
            .field("version_bytes", &self.version.len())
            .field("shell_bytes", &self.shell.len())
            .finish()
    }
}

/// A private debug log with one retained rotated file.
pub struct DebugLog {
    directory: PathBuf,
    #[cfg(unix)]
    directory_fd: OwnedDescriptor,
    session: DiagnosticSession,
    max_bytes: u64,
    #[cfg(test)]
    append_failure_after: Option<usize>,
    #[cfg(test)]
    fail_previous_publish: bool,
    #[cfg(test)]
    interrupt_after_active_publish: bool,
}

impl DebugLog {
    /// Opens the `argmax/logs` child of an absolute user cache directory.
    ///
    /// Existing cache-root permissions are never changed. Argmax creates and
    /// secures only its fixed owned descendants, canonicalizing the cache root
    /// before retaining any path.
    ///
    /// # Errors
    ///
    /// Returns an error for a relative path, a symlink/non-directory target, or
    /// an I/O failure while creating or securing the directory.
    pub fn open(
        cache_directory: impl Into<PathBuf>,
        session: DiagnosticSession,
    ) -> Result<Self, DiagnosticError> {
        let directory = prepare_owned_directory(&cache_directory.into(), LOG_DIRECTORY_NAME)?;
        #[cfg(unix)]
        let directory_fd = open_owned_directory(&directory)?;
        Ok(Self {
            directory,
            #[cfg(unix)]
            directory_fd,
            session,
            max_bytes: MAX_DEBUG_LOG_BYTES,
            #[cfg(test)]
            append_failure_after: None,
            #[cfg(test)]
            fail_previous_publish: false,
            #[cfg(test)]
            interrupt_after_active_publish: false,
        })
    }

    /// Absolute active-log path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.directory.join(DEBUG_LOG_NAME)
    }

    /// Writes one bounded, single-line, credential-redacted record.
    ///
    /// Rotation occurs before a record would exceed five MiB. The previous log
    /// is the only retained rotated file.
    ///
    /// # Errors
    ///
    /// Returns an error when the component name is invalid or a private file
    /// cannot be validated, rotated, or written.
    pub fn write(
        &self,
        level: DiagnosticLevel,
        component: &str,
        message: &str,
    ) -> Result<(), DiagnosticError> {
        let _write_guard = DEBUG_LOG_WRITE_GATE
            .lock()
            .map_err(|_| DiagnosticError::LockPoisoned)?;
        validate_component(component)?;
        let message = sanitize_log_message(message);
        let timestamp = unix_milliseconds(SystemTime::now());
        let record = format!(
            "timestamp_ms={timestamp} level={} component={component} \
             session={} version={} shell={} os={} arch={} message={message}\n",
            level.as_str(),
            self.session.session_id,
            self.session.version,
            self.session.shell,
            std::env::consts::OS,
            std::env::consts::ARCH,
        );

        #[cfg(unix)]
        {
            secure_owned_directory_descriptor(&self.directory, &self.directory_fd)?;
            let _process_guard = lock_debug_log(self.directory_fd.raw())?;
            cleanup_stale_debug_temporaries(&self.directory, &self.directory_fd)?;
            self.write_record_unix(record.as_bytes())
        }
        #[cfg(not(unix))]
        {
            secure_owned_directory(&self.directory)?;
            self.rotate_if_needed(record.len())?;
            let path = self.path();
            let mut file = open_private_append(&path)?;
            file.write_all(record.as_bytes())?;
            file.flush()?;
            Ok(())
        }
    }

    #[cfg(unix)]
    fn write_record_unix(&self, record: &[u8]) -> Result<(), DiagnosticError> {
        let directory_fd = self.directory_fd.raw();
        let previous = open_optional_private_entry(
            directory_fd,
            OsStr::new(PREVIOUS_DEBUG_LOG_NAME),
            OFlag::O_RDONLY,
        )?;
        let active =
            open_optional_private_entry(directory_fd, OsStr::new(DEBUG_LOG_NAME), OFlag::O_RDONLY)?;
        let Some((active_fd, active_stat)) = active else {
            let mut replacement =
                create_synced_temporary(&self.directory, directory_fd, "active", |file| {
                    file.write_all(record).map_err(Into::into)
                })?;
            if let Err(error) =
                ensure_optional_entry_unchanged(directory_fd, OsStr::new(DEBUG_LOG_NAME), None)
            {
                return Err(cleanup_temporary_after_error(&mut replacement, error));
            }
            if let Err(error) =
                rename_temporary(directory_fd, &mut replacement, OsStr::new(DEBUG_LOG_NAME))
            {
                return Err(cleanup_temporary_after_error(&mut replacement, error));
            }
            sync_directory(directory_fd)?;
            return Ok(());
        };

        let active_bytes = private_file_length(&active_stat)?;
        if active_bytes > MAX_DEBUG_LOG_BYTES {
            return Err(DiagnosticError::UnsafePath);
        }
        let record_bytes = u64::try_from(record.len()).unwrap_or(u64::MAX);
        if active_bytes.saturating_add(record_bytes) <= self.max_bytes {
            let mut file = open_private_append_at(
                &self.directory,
                directory_fd,
                OsStr::new(DEBUG_LOG_NAME),
                &active_stat,
            )?;
            #[cfg(test)]
            let append_failure_after = self.append_failure_after;
            #[cfg(not(test))]
            let append_failure_after = None;
            append_with_rollback(&mut file, active_bytes, record, append_failure_after)?;
            return Ok(());
        }

        self.rotate_record_unix(
            directory_fd,
            previous.as_ref().map(|(_, stat)| stat),
            active_fd.raw(),
            &active_stat,
            active_bytes,
            record,
        )
    }

    #[cfg(unix)]
    fn rotate_record_unix(
        &self,
        directory_fd: RawFd,
        previous_stat: Option<&FileStat>,
        active_fd: RawFd,
        active_stat: &FileStat,
        active_bytes: u64,
        record: &[u8],
    ) -> Result<(), DiagnosticError> {
        let mut previous_replacement =
            create_synced_temporary(&self.directory, directory_fd, "previous-ready", |file| {
                copy_descriptor_to_file(active_fd, active_bytes, file)
            })?;
        let mut active_replacement =
            match create_synced_temporary(&self.directory, directory_fd, "active", |file| {
                file.write_all(record).map_err(Into::into)
            }) {
                Ok(temporary) => temporary,
                Err(error) => {
                    return Err(cleanup_temporary_after_error(
                        &mut previous_replacement,
                        error,
                    ));
                }
            };

        if let Err(error) = ensure_optional_entry_unchanged(
            directory_fd,
            OsStr::new(PREVIOUS_DEBUG_LOG_NAME),
            previous_stat,
        ) {
            return Err(cleanup_two_temporaries_after_error(
                &mut previous_replacement,
                &mut active_replacement,
                error,
            ));
        }
        if let Err(error) = ensure_optional_entry_unchanged(
            directory_fd,
            OsStr::new(DEBUG_LOG_NAME),
            Some(active_stat),
        ) {
            return Err(cleanup_two_temporaries_after_error(
                &mut previous_replacement,
                &mut active_replacement,
                error,
            ));
        }
        if let Err(error) =
            mark_previous_temporary_committing(directory_fd, &mut previous_replacement)
        {
            return Err(cleanup_two_temporaries_after_error(
                &mut previous_replacement,
                &mut active_replacement,
                error,
            ));
        }
        if let Err(error) = rename_temporary(
            directory_fd,
            &mut active_replacement,
            OsStr::new(DEBUG_LOG_NAME),
        ) {
            return Err(cleanup_two_temporaries_after_error(
                &mut previous_replacement,
                &mut active_replacement,
                error,
            ));
        }
        #[cfg(test)]
        if self.interrupt_after_active_publish {
            previous_replacement.preserve();
            return Err(DiagnosticError::Io(io::ErrorKind::Interrupted));
        }
        #[cfg(test)]
        let previous_publish = if self.fail_previous_publish {
            Err(DiagnosticError::Io(io::ErrorKind::Other))
        } else {
            rename_temporary(
                directory_fd,
                &mut previous_replacement,
                OsStr::new(PREVIOUS_DEBUG_LOG_NAME),
            )
        };
        #[cfg(not(test))]
        let previous_publish = rename_temporary(
            directory_fd,
            &mut previous_replacement,
            OsStr::new(PREVIOUS_DEBUG_LOG_NAME),
        );
        if let Err(operation) = previous_publish {
            if let Err(rollback) = rename_temporary(
                directory_fd,
                &mut previous_replacement,
                OsStr::new(DEBUG_LOG_NAME),
            ) {
                previous_replacement.preserve();
                return Err(DiagnosticError::LogRollbackFailed {
                    operation: diagnostic_error_kind(&operation),
                    rollback: diagnostic_error_kind(&rollback),
                });
            }
            sync_directory(directory_fd)?;
            return Err(operation);
        }
        sync_directory(directory_fd)
    }

    #[cfg(not(unix))]
    fn rotate_if_needed(&self, next_record_bytes: usize) -> Result<(), DiagnosticError> {
        let active = self.path();
        let Some(active_bytes) = regular_file_length(&active)? else {
            return Ok(());
        };
        let next_record_bytes = u64::try_from(next_record_bytes).unwrap_or(u64::MAX);
        if active_bytes.saturating_add(next_record_bytes) <= self.max_bytes {
            return Ok(());
        }

        let previous = self.directory.join(PREVIOUS_DEBUG_LOG_NAME);
        remove_owned_regular_file_if_present(&previous)?;
        fs::rename(active, previous)?;
        Ok(())
    }

    #[cfg(test)]
    fn with_test_limit(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    #[cfg(test)]
    fn with_append_failure_after(mut self, bytes: usize) -> Self {
        self.append_failure_after = Some(bytes);
        self
    }

    #[cfg(test)]
    fn with_previous_publish_failure(mut self) -> Self {
        self.fail_previous_publish = true;
        self
    }

    #[cfg(test)]
    fn with_active_publish_interruption(mut self) -> Self {
        self.interrupt_after_active_publish = true;
        self
    }
}

impl fmt::Debug for DebugLog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DebugLog")
            .field("directory_bytes", &path_bytes(&self.directory))
            .field("session", &self.session)
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

/// Bounded content used to create one local crash report.
pub struct CrashReport {
    version: Box<str>,
    failure: Box<str>,
    backtrace: Box<str>,
}

impl CrashReport {
    /// Sanitizes and bounds crash information before it reaches the filesystem.
    #[must_use]
    pub fn new(version: &str, failure: &str, backtrace: &str) -> Self {
        Self {
            version: sanitize_single_line(version, MAX_CONTEXT_FIELD_BYTES).into(),
            failure: sanitize_report_field(failure, MAX_CRASH_FAILURE_BYTES).into(),
            backtrace: sanitize_report_field(backtrace, MAX_CRASH_BACKTRACE_BYTES).into(),
        }
    }

    fn render(&self, timestamp_ms: u128) -> String {
        format!(
            "argmax crash report\n\
             timestamp_ms: {timestamp_ms}\n\
             version: {}\n\
             os: {}\n\
             architecture: {}\n\
             failure:\n{}\n\
             backtrace:\n{}\n",
            self.version,
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.failure,
            self.backtrace,
        )
    }
}

impl fmt::Debug for CrashReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrashReport")
            .field("version_bytes", &self.version.len())
            .field("failure_bytes", &self.failure.len())
            .field("backtrace_bytes", &self.backtrace.len())
            .finish()
    }
}

/// Argmax-owned private crash-report directory.
pub struct CrashReportStore {
    directory: PathBuf,
    #[cfg(unix)]
    directory_fd: OwnedDescriptor,
}

impl CrashReportStore {
    /// Opens the `argmax/crashes` child of an absolute user cache directory.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe path or an I/O failure.
    pub fn open(cache_directory: impl Into<PathBuf>) -> Result<Self, DiagnosticError> {
        let directory = prepare_owned_directory(&cache_directory.into(), CRASH_DIRECTORY_NAME)?;
        #[cfg(unix)]
        let directory_fd = open_owned_directory(&directory)?;
        Ok(Self {
            directory,
            #[cfg(unix)]
            directory_fd,
        })
    }

    /// Writes one uniquely named mode-`0600` report and returns its absolute
    /// path.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory is unsafe or no report can be
    /// created after bounded collision retries.
    pub fn write(&self, report: &CrashReport) -> Result<PathBuf, DiagnosticError> {
        #[cfg(unix)]
        {
            self.write_unix(report)
        }
        #[cfg(not(unix))]
        {
            self.write_portable(report)
        }
    }

    #[cfg(unix)]
    fn write_unix(&self, report: &CrashReport) -> Result<PathBuf, DiagnosticError> {
        secure_owned_directory_descriptor(&self.directory, &self.directory_fd)?;
        let directory_fd = self.directory_fd.raw();
        let timestamp_ms = unix_milliseconds(SystemTime::now());
        let pid = std::process::id();
        let rendered = report.render(timestamp_ms);
        for _ in 0..64 {
            let counter = CRASH_COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = OsString::from(format!(
                "{CRASH_PREFIX}{timestamp_ms}-{pid}-{counter}{CRASH_SUFFIX}"
            ));
            let temporary_name = OsString::from(format!(
                ".{CRASH_PREFIX}{timestamp_ms}-{pid}-{counter}-{:016x}.tmp",
                crash_nonce(timestamp_ms, pid, counter)
            ));
            if let Some(path) =
                self.try_write_unix(directory_fd, name, temporary_name, rendered.as_bytes())?
            {
                return Ok(path);
            }
        }
        Err(DiagnosticError::ReportNameExhausted)
    }

    #[cfg(unix)]
    fn try_write_unix(
        &self,
        directory_fd: RawFd,
        name: OsString,
        temporary_name: OsString,
        rendered: &[u8],
    ) -> Result<Option<PathBuf>, DiagnosticError> {
        let (mut file, temporary_stat, mut temporary) =
            match create_private_temporary_named(&self.directory, directory_fd, temporary_name) {
                Ok(created) => created,
                Err(DiagnosticError::Io(io::ErrorKind::AlreadyExists)) => return Ok(None),
                Err(error) => return Err(error),
            };
        if let Err(error) = file.write_all(rendered).and_then(|()| file.sync_all()) {
            drop(file);
            return Err(temporary_operation_failed(&mut temporary, error.kind()));
        }
        drop(file);

        let temporary_is_valid =
            match entry_matches_stat(directory_fd, temporary.name(), &temporary_stat, Some(1)) {
                Ok(valid) => valid,
                Err(error) => {
                    return Err(cleanup_temporary_after_error(&mut temporary, error));
                }
            };
        if !temporary_is_valid {
            return Err(cleanup_temporary_after_error(
                &mut temporary,
                DiagnosticError::UnsafePath,
            ));
        }
        match linkat(
            Some(directory_fd),
            temporary.name(),
            Some(directory_fd),
            &name,
            AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(Errno::EEXIST) => {
                temporary
                    .remove()
                    .map_err(|cleanup| DiagnosticError::TemporaryCleanupFailed {
                        operation: io::ErrorKind::AlreadyExists,
                        cleanup: diagnostic_error_kind(&cleanup),
                    })?;
                return Ok(None);
            }
            Err(error) => {
                return Err(temporary_operation_failed(
                    &mut temporary,
                    errno_kind(error),
                ));
            }
        }

        let published_is_valid =
            match entry_matches_stat(directory_fd, &name, &temporary_stat, Some(2)) {
                Ok(valid) => valid,
                Err(error) => {
                    return Err(rollback_crash_publication(
                        directory_fd,
                        &name,
                        &temporary_stat,
                        &mut temporary,
                        error,
                    ));
                }
            };
        if !published_is_valid {
            return Err(rollback_crash_publication(
                directory_fd,
                &name,
                &temporary_stat,
                &mut temporary,
                DiagnosticError::UnsafePath,
            ));
        }
        if let Err(error) = sync_directory(directory_fd) {
            return Err(rollback_crash_publication(
                directory_fd,
                &name,
                &temporary_stat,
                &mut temporary,
                error,
            ));
        }
        temporary
            .remove()
            .map_err(|cleanup| DiagnosticError::PublishedTemporaryCleanupFailed {
                cleanup: diagnostic_error_kind(&cleanup),
            })?;
        if !entry_matches_stat(directory_fd, &name, &temporary_stat, Some(1))? {
            return Err(DiagnosticError::UnsafePath);
        }
        sync_directory(directory_fd)?;
        Ok(Some(self.directory.join(name)))
    }

    #[cfg(not(unix))]
    fn write_portable(&self, report: &CrashReport) -> Result<PathBuf, DiagnosticError> {
        secure_owned_directory(&self.directory)?;
        let timestamp_ms = unix_milliseconds(SystemTime::now());
        let pid = std::process::id();
        for _ in 0..64 {
            let counter = CRASH_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = self.directory.join(format!(
                "{CRASH_PREFIX}{timestamp_ms}-{pid}-{counter}{CRASH_SUFFIX}"
            ));
            let temporary = self.directory.join(format!(
                ".{CRASH_PREFIX}{timestamp_ms}-{pid}-{counter}-{:016x}.tmp",
                crash_nonce(timestamp_ms, pid, counter)
            ));
            match create_private_new(&temporary) {
                Ok(mut file) => {
                    let write_result = file
                        .write_all(report.render(timestamp_ms).as_bytes())
                        .and_then(|()| file.sync_all());
                    if let Err(error) = write_result {
                        drop(file);
                        return Err(cleanup_temporary(&temporary, error.kind()));
                    }
                    drop(file);
                    match fs::hard_link(&temporary, &path) {
                        Ok(()) => {
                            if let Err(error) = fs::remove_file(&temporary) {
                                return Err(DiagnosticError::PublishedTemporaryCleanupFailed {
                                    cleanup: error.kind(),
                                });
                            }
                            File::open(&self.directory)?.sync_all()?;
                            return Ok(path);
                        }
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            let cleanup = fs::remove_file(&temporary);
                            if let Err(cleanup) = cleanup {
                                return Err(DiagnosticError::TemporaryCleanupFailed {
                                    operation: error.kind(),
                                    cleanup: cleanup.kind(),
                                });
                            }
                        }
                        Err(error) => {
                            return Err(cleanup_temporary(&temporary, error.kind()));
                        }
                    }
                }
                Err(DiagnosticError::Io(io::ErrorKind::AlreadyExists)) => {}
                Err(error) => return Err(error),
            }
        }
        Err(DiagnosticError::ReportNameExhausted)
    }

    /// Finds the newest regular argmax crash report without following symlinks.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable metadata or a directory traversal that
    /// exceeds [`MAX_CRASH_DIRECTORY_ENTRIES`].
    pub fn newest(&self) -> Result<Option<PathBuf>, DiagnosticError> {
        #[cfg(unix)]
        {
            self.newest_unix()
        }
        #[cfg(not(unix))]
        {
            self.newest_portable()
        }
    }

    #[cfg(unix)]
    fn newest_unix(&self) -> Result<Option<PathBuf>, DiagnosticError> {
        type NewestReport = ((i64, i64), (u128, u32, u64), PathBuf);

        let directory_fd = self.directory_fd.raw();
        let mut newest: Option<NewestReport> = None;
        for name in self.owned_report_names_unix()? {
            let EntryInspection::Regular(_file, stat) =
                inspect_report_entry(directory_fd, &name).map_err(errno_error)?
            else {
                continue;
            };
            let path = self.directory.join(&name);
            let order = parse_owned_crash_name(&name).ok_or(DiagnosticError::UnsafePath)?;
            let modified = (stat.st_mtime, stat.st_mtime_nsec);
            let replace =
                newest
                    .as_ref()
                    .is_none_or(|(current_time, current_order, current_path)| {
                        modified > *current_time
                            || (modified == *current_time
                                && (order > *current_order
                                    || (order == *current_order && path > *current_path)))
                    });
            if replace {
                newest = Some((modified, order, path));
            }
        }
        Ok(newest.map(|(_, _, path)| path))
    }

    #[cfg(not(unix))]
    fn newest_portable(&self) -> Result<Option<PathBuf>, DiagnosticError> {
        let mut newest: Option<(SystemTime, (u128, u32, u64), PathBuf)> = None;
        for path in self.owned_reports()? {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                continue;
            }
            let modified = metadata.modified()?;
            let order = path
                .file_name()
                .and_then(parse_owned_crash_name)
                .ok_or(DiagnosticError::UnsafePath)?;
            let replace =
                newest
                    .as_ref()
                    .is_none_or(|(current_time, current_order, current_path)| {
                        modified > *current_time
                            || (modified == *current_time
                                && (order > *current_order
                                    || (order == *current_order && path > *current_path)))
                    });
            if replace {
                newest = Some((modified, order, path));
            }
        }
        Ok(newest.map(|(_, _, path)| path))
    }

    /// Removes regular argmax crash reports only.
    ///
    /// Every candidate is represented in the returned outcome: regular files
    /// are listed as removed, while symlinks, non-files, and removal failures
    /// are listed as failures for explicit CLI reporting.
    ///
    /// # Errors
    ///
    /// Returns an error when the bounded directory scan itself cannot finish.
    pub fn clear(&self) -> Result<CrashClearOutcome, DiagnosticError> {
        #[cfg(unix)]
        {
            self.clear_unix()
        }
        #[cfg(not(unix))]
        {
            self.clear_portable()
        }
    }

    #[cfg(unix)]
    fn clear_unix(&self) -> Result<CrashClearOutcome, DiagnosticError> {
        let directory_fd = self.directory_fd.raw();
        let mut outcome = CrashClearOutcome::default();
        for name in self.owned_report_names_unix()? {
            let path = self.directory.join(&name);
            match inspect_report_entry(directory_fd, &name) {
                Ok(EntryInspection::Regular(_file, stat)) => {
                    if ensure_entry_unchanged(directory_fd, &name, &stat).is_err() {
                        outcome.failures.push(CrashRemovalFailure {
                            path,
                            kind: io::ErrorKind::InvalidInput,
                        });
                        continue;
                    }
                    match unlinkat(
                        Some(directory_fd),
                        name.as_os_str(),
                        UnlinkatFlags::NoRemoveDir,
                    ) {
                        Ok(()) => outcome.removed.push(path),
                        Err(error) => outcome.failures.push(CrashRemovalFailure {
                            path,
                            kind: errno_kind(error),
                        }),
                    }
                }
                Ok(EntryInspection::Missing) => outcome.failures.push(CrashRemovalFailure {
                    path,
                    kind: io::ErrorKind::NotFound,
                }),
                Ok(EntryInspection::Unsafe) => outcome.failures.push(CrashRemovalFailure {
                    path,
                    kind: io::ErrorKind::InvalidInput,
                }),
                Err(error) => outcome.failures.push(CrashRemovalFailure {
                    path,
                    kind: errno_kind(error),
                }),
            }
        }
        sync_directory(directory_fd)?;
        Ok(outcome)
    }

    #[cfg(not(unix))]
    fn clear_portable(&self) -> Result<CrashClearOutcome, DiagnosticError> {
        let mut outcome = CrashClearOutcome::default();
        for path in self.owned_reports()? {
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => match fs::remove_file(&path) {
                    Ok(()) => outcome.removed.push(path),
                    Err(error) => outcome.failures.push(CrashRemovalFailure {
                        path,
                        kind: error.kind(),
                    }),
                },
                Ok(_) => outcome.failures.push(CrashRemovalFailure {
                    path,
                    kind: io::ErrorKind::InvalidInput,
                }),
                Err(error) => outcome.failures.push(CrashRemovalFailure {
                    path,
                    kind: error.kind(),
                }),
            }
        }
        Ok(outcome)
    }

    #[cfg(unix)]
    fn owned_report_names_unix(&self) -> Result<Vec<OsString>, DiagnosticError> {
        secure_owned_directory_descriptor(&self.directory, &self.directory_fd)?;
        let mut names = Vec::new();
        for (index, entry) in fs::read_dir(&self.directory)?.enumerate() {
            if index >= MAX_CRASH_DIRECTORY_ENTRIES {
                return Err(DiagnosticError::TooManyDirectoryEntries {
                    limit: MAX_CRASH_DIRECTORY_ENTRIES,
                });
            }
            let name = entry?.file_name();
            if parse_owned_crash_name(&name).is_some() {
                names.push(name);
            }
        }
        secure_owned_directory_descriptor(&self.directory, &self.directory_fd)?;
        Ok(names)
    }

    #[cfg(not(unix))]
    fn owned_reports(&self) -> Result<Vec<PathBuf>, DiagnosticError> {
        secure_owned_directory(&self.directory)?;
        let mut paths = Vec::new();
        for (index, entry) in fs::read_dir(&self.directory)?.enumerate() {
            if index >= MAX_CRASH_DIRECTORY_ENTRIES {
                return Err(DiagnosticError::TooManyDirectoryEntries {
                    limit: MAX_CRASH_DIRECTORY_ENTRIES,
                });
            }
            let entry = entry?;
            if parse_owned_crash_name(&entry.file_name()).is_some() {
                paths.push(entry.path());
            }
        }
        Ok(paths)
    }
}

impl fmt::Debug for CrashReportStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrashReportStore")
            .field("directory_bytes", &path_bytes(&self.directory))
            .finish_non_exhaustive()
    }
}

/// Result of clearing known argmax crash reports.
#[derive(Default)]
pub struct CrashClearOutcome {
    /// Successfully removed report paths.
    pub removed: Vec<PathBuf>,
    /// Owned-looking paths that could not be removed safely.
    pub failures: Vec<CrashRemovalFailure>,
}

/// One crash-report removal failure without potentially sensitive OS text.
pub struct CrashRemovalFailure {
    /// Absolute candidate report path.
    pub path: PathBuf,
    /// Stable I/O failure category.
    pub kind: io::ErrorKind,
}

impl fmt::Debug for CrashClearOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrashClearOutcome")
            .field("removed_count", &self.removed.len())
            .field("failure_count", &self.failures.len())
            .finish()
    }
}

impl fmt::Debug for CrashRemovalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrashRemovalFailure")
            .field("path_bytes", &path_bytes(&self.path))
            .field("kind", &self.kind)
            .finish()
    }
}

/// Diagnostics boundary failure.
pub enum DiagnosticError {
    /// The caller supplied a relative storage directory.
    RelativeDirectory,
    /// An existing storage target is a symlink or not the required file type.
    UnsafePath,
    /// Stable context metadata was unsafe for the line-oriented format.
    InvalidContext(ContextField),
    /// A component label was malformed.
    InvalidComponent,
    /// A crash-report directory exceeded its traversal bound.
    TooManyDirectoryEntries {
        /// Maximum inspected directory entries.
        limit: usize,
    },
    /// Bounded unique crash-report name attempts were exhausted.
    ReportNameExhausted,
    /// Another logging operation panicked while holding the process-local lock.
    LockPoisoned,
    /// A private diagnostic temporary could not be removed after failure.
    TemporaryCleanupFailed {
        /// Failure category from the write or publication operation.
        operation: io::ErrorKind,
        /// Failure category from removing the temporary file.
        cleanup: io::ErrorKind,
    },
    /// Two diagnostic entries both resisted cleanup after failure.
    MultipleCleanupFailed {
        /// Failure category from the operation being rolled back.
        operation: io::ErrorKind,
        /// Failure category from removing the first temporary.
        first_cleanup: io::ErrorKind,
        /// Failure category from removing the second temporary.
        second_cleanup: io::ErrorKind,
    },
    /// A failed log operation could not restore the prior visible state.
    LogRollbackFailed {
        /// Failure category from append, rotation, or synchronization.
        operation: io::ErrorKind,
        /// Failure category from restoring or synchronizing the rollback.
        rollback: io::ErrorKind,
    },
    /// A complete published report remained valid, but its private temporary
    /// hard link could not be removed.
    PublishedTemporaryCleanupFailed {
        /// Stable cleanup failure category.
        cleanup: io::ErrorKind,
    },
    /// Stable category of an underlying filesystem failure.
    Io(io::ErrorKind),
}

impl fmt::Debug for DiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeDirectory => formatter.write_str("RelativeDirectory"),
            Self::UnsafePath => formatter.write_str("UnsafePath"),
            Self::InvalidContext(field) => formatter
                .debug_tuple("InvalidContext")
                .field(field)
                .finish(),
            Self::InvalidComponent => formatter.write_str("InvalidComponent"),
            Self::TooManyDirectoryEntries { limit } => formatter
                .debug_struct("TooManyDirectoryEntries")
                .field("limit", limit)
                .finish(),
            Self::ReportNameExhausted => formatter.write_str("ReportNameExhausted"),
            Self::LockPoisoned => formatter.write_str("LockPoisoned"),
            Self::TemporaryCleanupFailed { operation, cleanup } => formatter
                .debug_struct("TemporaryCleanupFailed")
                .field("operation", operation)
                .field("cleanup", cleanup)
                .finish(),
            Self::MultipleCleanupFailed {
                operation,
                first_cleanup,
                second_cleanup,
            } => formatter
                .debug_struct("MultipleCleanupFailed")
                .field("operation", operation)
                .field("first_cleanup", first_cleanup)
                .field("second_cleanup", second_cleanup)
                .finish(),
            Self::LogRollbackFailed {
                operation,
                rollback,
            } => formatter
                .debug_struct("LogRollbackFailed")
                .field("operation", operation)
                .field("rollback", rollback)
                .finish(),
            Self::PublishedTemporaryCleanupFailed { cleanup } => formatter
                .debug_struct("PublishedTemporaryCleanupFailed")
                .field("cleanup", cleanup)
                .finish(),
            Self::Io(kind) => formatter.debug_tuple("Io").field(kind).finish(),
        }
    }
}

impl fmt::Display for DiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeDirectory => {
                formatter.write_str("diagnostic storage directory must be absolute")
            }
            Self::UnsafePath => formatter.write_str("diagnostic storage path is unsafe"),
            Self::InvalidContext(field) => write!(formatter, "diagnostic {field} is invalid"),
            Self::InvalidComponent => formatter.write_str("diagnostic component is invalid"),
            Self::TooManyDirectoryEntries { limit } => {
                write!(
                    formatter,
                    "diagnostic directory contains more than {limit} entries"
                )
            }
            Self::ReportNameExhausted => {
                formatter.write_str("could not allocate a unique crash-report name")
            }
            Self::LockPoisoned => formatter.write_str("diagnostic writer lock is unavailable"),
            Self::TemporaryCleanupFailed { operation, cleanup } => write!(
                formatter,
                "diagnostic file operation failed ({operation:?}); temporary cleanup also failed ({cleanup:?})"
            ),
            Self::MultipleCleanupFailed {
                operation,
                first_cleanup,
                second_cleanup,
            } => write!(
                formatter,
                "diagnostic file operation failed ({operation:?}); two temporary cleanups also failed ({first_cleanup:?}, {second_cleanup:?})"
            ),
            Self::LogRollbackFailed {
                operation,
                rollback,
            } => write!(
                formatter,
                "diagnostic log operation failed ({operation:?}); rollback also failed ({rollback:?})"
            ),
            Self::PublishedTemporaryCleanupFailed { cleanup } => write!(
                formatter,
                "crash report was published, but temporary cleanup failed ({cleanup:?})"
            ),
            Self::Io(kind) => write!(
                formatter,
                "diagnostic filesystem operation failed ({kind:?})"
            ),
        }
    }
}

impl Error for DiagnosticError {}

impl From<io::Error> for DiagnosticError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

fn validate_context_field(field: ContextField, value: &str) -> Result<(), DiagnosticError> {
    if value.is_empty()
        || value.len() > MAX_CONTEXT_FIELD_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(DiagnosticError::InvalidContext(field));
    }
    Ok(())
}

fn validate_component(component: &str) -> Result<(), DiagnosticError> {
    if component.is_empty()
        || component.len() > MAX_COMPONENT_BYTES
        || !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(DiagnosticError::InvalidComponent);
    }
    Ok(())
}

fn prepare_owned_directory(cache_directory: &Path, leaf: &str) -> Result<PathBuf, DiagnosticError> {
    if !cache_directory.is_absolute() {
        return Err(DiagnosticError::RelativeDirectory);
    }
    match fs::metadata(cache_directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(DiagnosticError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(cache_directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    let cache_directory = fs::canonicalize(cache_directory)?;
    if !fs::metadata(&cache_directory)?.is_dir() {
        return Err(DiagnosticError::UnsafePath);
    }

    let application_directory = cache_directory.join(APPLICATION_DIRECTORY_NAME);
    create_owned_directory(&application_directory)?;
    let directory = application_directory.join(leaf);
    create_owned_directory(&directory)?;
    let directory = fs::canonicalize(directory)?;
    secure_owned_directory(&directory)?;
    Ok(directory)
}

fn create_owned_directory(path: &Path) -> Result<(), DiagnosticError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(DiagnosticError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            if let Err(error) = builder.create(path) {
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
            }
        }
        Err(error) => return Err(error.into()),
    }
    secure_owned_directory(path)
}

fn secure_owned_directory(path: &Path) -> Result<(), DiagnosticError> {
    let expected = fs::symlink_metadata(path)?;
    if !expected.file_type().is_dir() {
        return Err(DiagnosticError::UnsafePath);
    }
    let directory = File::open(path)?;
    let actual = directory.metadata()?;
    if !actual.is_dir() || !same_file(&expected, &actual) {
        return Err(DiagnosticError::UnsafePath);
    }
    #[cfg(unix)]
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
fn open_owned_directory(path: &Path) -> Result<OwnedDescriptor, DiagnosticError> {
    let expected = fs::symlink_metadata(path)?;
    if !expected.file_type().is_dir() {
        return Err(DiagnosticError::UnsafePath);
    }
    let raw = open(
        path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(open_error)?;
    let descriptor = OwnedDescriptor(raw);
    let actual = fstat(descriptor.raw()).map_err(errno_error)?;
    if !stat_is_directory(&actual) || !metadata_matches_stat(&expected, &actual) {
        return Err(DiagnosticError::UnsafePath);
    }
    fchmod(descriptor.raw(), Mode::from_bits_truncate(0o700)).map_err(errno_error)?;
    Ok(descriptor)
}

#[cfg(unix)]
fn secure_owned_directory_descriptor(
    path: &Path,
    descriptor: &OwnedDescriptor,
) -> Result<(), DiagnosticError> {
    let expected = fs::symlink_metadata(path)?;
    let actual = fstat(descriptor.raw()).map_err(errno_error)?;
    if !expected.file_type().is_dir()
        || !stat_is_directory(&actual)
        || !metadata_matches_stat(&expected, &actual)
    {
        return Err(DiagnosticError::UnsafePath);
    }
    fchmod(descriptor.raw(), Mode::from_bits_truncate(0o700)).map_err(errno_error)
}

#[cfg(unix)]
fn lock_debug_log(directory_fd: RawFd) -> Result<OwnedDescriptor, DiagnosticError> {
    // Concurrent first-time creators can observe a transient missing entry on
    // some filesystems. Retry only that race against the already pinned
    // directory; persistent failures remain bounded and visible.
    let mut attempt = 0_u8;
    let raw = loop {
        match openat(
            Some(directory_fd),
            OsStr::new(DEBUG_LOG_LOCK_NAME),
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_CLOEXEC
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK,
            Mode::from_bits_truncate(0o600),
        ) {
            Ok(raw) => break raw,
            Err(Errno::EINTR | Errno::ENOENT) if attempt < 63 => {
                attempt += 1;
                std::thread::yield_now();
            }
            Err(error) => return Err(open_error(error)),
        }
    };
    let descriptor = OwnedDescriptor(raw);
    let stat = fstat(descriptor.raw()).map_err(errno_error)?;
    if !stat_is_private_regular(&stat) {
        return Err(DiagnosticError::UnsafePath);
    }
    fchmod(descriptor.raw(), Mode::from_bits_truncate(0o600)).map_err(errno_error)?;
    ensure_entry_unchanged(directory_fd, OsStr::new(DEBUG_LOG_LOCK_NAME), &stat)?;
    loop {
        #[allow(deprecated)]
        match nix::fcntl::flock(descriptor.raw(), FlockArg::LockExclusive) {
            Ok(()) => break,
            Err(Errno::EINTR) => {}
            Err(error) => return Err(errno_error(error)),
        }
    }
    ensure_entry_unchanged(directory_fd, OsStr::new(DEBUG_LOG_LOCK_NAME), &stat)?;
    Ok(descriptor)
}

#[cfg(unix)]
fn cleanup_stale_debug_temporaries(
    directory: &Path,
    directory_fd: &OwnedDescriptor,
) -> Result<(), DiagnosticError> {
    secure_owned_directory_descriptor(directory, directory_fd)?;
    let mut discard_temporaries = Vec::new();
    let mut previous_temporaries = Vec::new();
    let mut active_replacement_exists = false;
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        if index >= MAX_CRASH_DIRECTORY_ENTRIES {
            return Err(DiagnosticError::TooManyDirectoryEntries {
                limit: MAX_CRASH_DIRECTORY_ENTRIES,
            });
        }
        let name = entry?.file_name();
        match parse_debug_temporary_name(&name) {
            Some(DebugTemporaryPurpose::Active) => {
                active_replacement_exists = true;
                discard_temporaries.push(name);
            }
            Some(DebugTemporaryPurpose::PreviousReady) => discard_temporaries.push(name),
            Some(DebugTemporaryPurpose::Previous) => previous_temporaries.push(name),
            None => {}
        }
    }
    if previous_temporaries.len() > 1 {
        return Err(DiagnosticError::UnsafePath);
    }

    let mut changed = false;
    for name in discard_temporaries {
        changed |= remove_stale_debug_temporary(directory_fd.raw(), name)?;
    }
    if let Some(name) = previous_temporaries.pop() {
        changed |= if active_replacement_exists {
            remove_stale_debug_temporary(directory_fd.raw(), name)?
        } else {
            recover_previous_debug_temporary(directory_fd.raw(), name)?
        };
    }

    secure_owned_directory_descriptor(directory, directory_fd)?;
    if changed {
        sync_directory(directory_fd.raw())?;
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum DebugTemporaryPurpose {
    Active,
    PreviousReady,
    Previous,
}

#[cfg(unix)]
fn parse_debug_temporary_name(name: &OsStr) -> Option<DebugTemporaryPurpose> {
    let body = name
        .to_str()
        .and_then(|name| name.strip_prefix(DEBUG_TEMP_PREFIX))
        .and_then(|name| name.strip_suffix(".tmp"))?;
    let (purpose, body) = if let Some(body) = body.strip_prefix("active-") {
        (DebugTemporaryPurpose::Active, body)
    } else if let Some(body) = body.strip_prefix("previous-ready-") {
        (DebugTemporaryPurpose::PreviousReady, body)
    } else if let Some(body) = body.strip_prefix("previous-") {
        (DebugTemporaryPurpose::Previous, body)
    } else {
        return None;
    };
    let mut parts = body.split('-');
    let pid = parts.next()?;
    let counter = parts.next()?;
    let nonce = parts.next()?;
    (pid.parse::<u32>().is_ok()
        && counter.parse::<u64>().is_ok()
        && nonce.len() == 16
        && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        && parts.next().is_none())
    .then_some(purpose)
}

#[cfg(unix)]
fn remove_stale_debug_temporary(
    directory_fd: RawFd,
    name: OsString,
) -> Result<bool, DiagnosticError> {
    let Some((_file, stat)) = open_optional_private_entry(directory_fd, &name, OFlag::O_RDONLY)?
    else {
        return Ok(false);
    };
    let mut temporary = TemporaryEntry::new(directory_fd, name, stat);
    temporary.remove()?;
    Ok(true)
}

#[cfg(unix)]
fn recover_previous_debug_temporary(
    directory_fd: RawFd,
    name: OsString,
) -> Result<bool, DiagnosticError> {
    let Some((_temporary_file, temporary_stat)) =
        open_optional_private_entry(directory_fd, &name, OFlag::O_RDONLY)?
    else {
        return Ok(false);
    };
    let mut temporary = TemporaryEntry::new(directory_fd, name, temporary_stat);
    let active = match open_optional_private_entry(
        directory_fd,
        OsStr::new(DEBUG_LOG_NAME),
        OFlag::O_RDONLY,
    ) {
        Ok(active) => active,
        Err(error) => {
            temporary.preserve();
            return Err(error);
        }
    };
    if active.is_none() {
        temporary.preserve();
        return Err(DiagnosticError::UnsafePath);
    }

    if let Err(error) = open_optional_private_entry(
        directory_fd,
        OsStr::new(PREVIOUS_DEBUG_LOG_NAME),
        OFlag::O_RDONLY,
    ) {
        temporary.preserve();
        return Err(error);
    }
    if let Err(error) = rename_temporary(
        directory_fd,
        &mut temporary,
        OsStr::new(PREVIOUS_DEBUG_LOG_NAME),
    ) {
        temporary.preserve();
        return Err(error);
    }
    Ok(true)
}

#[cfg(unix)]
fn mark_previous_temporary_committing(
    directory_fd: RawFd,
    temporary: &mut TemporaryEntry,
) -> Result<(), DiagnosticError> {
    let name = temporary
        .name()
        .to_str()
        .ok_or(DiagnosticError::UnsafePath)?;
    let suffix = name
        .strip_prefix(".debug-transaction-previous-ready-")
        .ok_or(DiagnosticError::UnsafePath)?;
    let destination = OsString::from(format!("{DEBUG_TEMP_PREFIX}previous-{suffix}"));
    rename_retained_temporary(directory_fd, temporary, destination)?;
    sync_directory(directory_fd)
}

#[cfg(unix)]
fn open_optional_private_entry(
    directory_fd: RawFd,
    name: &OsStr,
    access: OFlag,
) -> Result<Option<(OwnedDescriptor, FileStat)>, DiagnosticError> {
    let expected = match fstatat(Some(directory_fd), name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) if stat_is_private_regular(&stat) => stat,
        Ok(_) => return Err(DiagnosticError::UnsafePath),
        Err(Errno::ENOENT) => return Ok(None),
        Err(error) => return Err(errno_error(error)),
    };
    let raw = openat(
        Some(directory_fd),
        name,
        access | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(open_error)?;
    let descriptor = OwnedDescriptor(raw);
    let actual = fstat(descriptor.raw()).map_err(errno_error)?;
    if !stat_is_private_regular(&actual) || !same_stat(&expected, &actual) {
        return Err(DiagnosticError::UnsafePath);
    }
    fchmod(descriptor.raw(), Mode::from_bits_truncate(0o600)).map_err(errno_error)?;
    ensure_entry_unchanged(directory_fd, name, &actual)?;
    Ok(Some((descriptor, actual)))
}

#[cfg(unix)]
fn open_private_append_at(
    directory: &Path,
    directory_fd: RawFd,
    name: &OsStr,
    expected: &FileStat,
) -> Result<File, DiagnosticError> {
    let anchor = open_optional_private_entry(directory_fd, name, OFlag::O_WRONLY)?
        .ok_or(DiagnosticError::UnsafePath)?;
    if !same_stat(expected, &anchor.1) {
        return Err(DiagnosticError::UnsafePath);
    }
    let mut options = OpenOptions::new();
    options.append(true).custom_flags(
        OFlag::O_CLOEXEC.bits() | OFlag::O_NOFOLLOW.bits() | OFlag::O_NONBLOCK.bits(),
    );
    let file = options.open(directory.join(name))?;
    secure_private_file_stat(&file, &anchor.1)?;
    ensure_entry_unchanged(directory_fd, name, &anchor.1)?;
    Ok(file)
}

#[cfg(unix)]
fn append_with_rollback(
    file: &mut File,
    original_bytes: u64,
    record: &[u8],
    failure_after: Option<usize>,
) -> Result<(), DiagnosticError> {
    if let Err(operation) = append_record(file, record, failure_after) {
        if let Err(rollback) = file.set_len(original_bytes).and_then(|()| file.sync_all()) {
            return Err(DiagnosticError::LogRollbackFailed {
                operation: operation.kind(),
                rollback: rollback.kind(),
            });
        }
        return Err(operation.into());
    }
    Ok(())
}

#[cfg(unix)]
fn append_record(file: &mut File, record: &[u8], failure_after: Option<usize>) -> io::Result<()> {
    if let Some(failure_after) = failure_after {
        file.write_all(&record[..failure_after.min(record.len())])?;
        return Err(io::Error::other("injected diagnostic append failure"));
    }
    file.write_all(record)?;
    file.sync_all()
}

#[cfg(unix)]
fn create_synced_temporary(
    directory: &Path,
    directory_fd: RawFd,
    purpose: &str,
    populate: impl FnOnce(&mut File) -> Result<(), DiagnosticError>,
) -> Result<TemporaryEntry, DiagnosticError> {
    let timestamp_ms = unix_milliseconds(SystemTime::now());
    let pid = std::process::id();
    for _ in 0..64 {
        let counter = DEBUG_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = OsString::from(format!(
            "{DEBUG_TEMP_PREFIX}{purpose}-{pid}-{counter}-{:016x}.tmp",
            crash_nonce(timestamp_ms, pid, counter)
        ));
        let (mut file, _stat, mut temporary) =
            match create_private_temporary_named(directory, directory_fd, name) {
                Ok(created) => created,
                Err(DiagnosticError::Io(io::ErrorKind::AlreadyExists)) => continue,
                Err(error) => return Err(error),
            };
        if let Err(error) = populate(&mut file) {
            drop(file);
            let operation = diagnostic_error_kind(&error);
            if let Err(cleanup) = temporary.remove() {
                return Err(DiagnosticError::TemporaryCleanupFailed {
                    operation,
                    cleanup: diagnostic_error_kind(&cleanup),
                });
            }
            return Err(error);
        }
        if let Err(error) = file.sync_all() {
            drop(file);
            return Err(temporary_operation_failed(&mut temporary, error.kind()));
        }
        drop(file);
        return Ok(temporary);
    }
    Err(DiagnosticError::ReportNameExhausted)
}

#[cfg(unix)]
fn create_private_temporary_named(
    directory: &Path,
    directory_fd: RawFd,
    name: OsString,
) -> Result<(File, FileStat, TemporaryEntry), DiagnosticError> {
    let raw_result = openat(
        Some(directory_fd),
        name.as_os_str(),
        OFlag::O_WRONLY
            | OFlag::O_CREAT
            | OFlag::O_EXCL
            | OFlag::O_CLOEXEC
            | OFlag::O_NOFOLLOW
            | OFlag::O_NONBLOCK,
        Mode::from_bits_truncate(0o600),
    );
    let raw = raw_result.map_err(open_error)?;
    let anchor = OwnedDescriptor(raw);
    let stat = match fstat(anchor.raw()) {
        Ok(stat) => stat,
        Err(error) => {
            return Err(cleanup_unverified_temporary_after_fstat_error(
                directory_fd,
                name.as_os_str(),
                &anchor,
                error,
            ));
        }
    };
    let mut temporary = TemporaryEntry::new(directory_fd, name, stat);
    if !stat_is_private_regular(&stat) {
        return Err(cleanup_temporary_after_error(
            &mut temporary,
            DiagnosticError::UnsafePath,
        ));
    }
    if let Err(error) = fchmod(anchor.raw(), Mode::from_bits_truncate(0o600)) {
        return Err(cleanup_temporary_after_error(
            &mut temporary,
            errno_error(error),
        ));
    }
    if let Err(error) = ensure_entry_unchanged(directory_fd, temporary.name(), &stat) {
        return Err(cleanup_temporary_after_error(&mut temporary, error));
    }

    let mut options = OpenOptions::new();
    options.write(true).custom_flags(
        OFlag::O_CLOEXEC.bits() | OFlag::O_NOFOLLOW.bits() | OFlag::O_NONBLOCK.bits(),
    );
    let file = match options.open(directory.join(temporary.name())) {
        Ok(file) => file,
        Err(error) => {
            return Err(temporary_operation_failed(&mut temporary, error.kind()));
        }
    };
    if let Err(error) = secure_private_file_stat(&file, &stat) {
        drop(file);
        let operation = diagnostic_error_kind(&error);
        if let Err(cleanup) = temporary.remove() {
            return Err(DiagnosticError::TemporaryCleanupFailed {
                operation,
                cleanup: diagnostic_error_kind(&cleanup),
            });
        }
        return Err(error);
    }
    if let Err(error) = ensure_entry_unchanged(directory_fd, temporary.name(), &stat) {
        drop(file);
        return Err(cleanup_temporary_after_error(&mut temporary, error));
    }
    Ok((file, stat, temporary))
}

#[cfg(unix)]
fn copy_descriptor_to_file(
    source_fd: RawFd,
    source_bytes: u64,
    destination: &mut File,
) -> Result<(), DiagnosticError> {
    let mut remaining = source_bytes;
    let mut buffer = [0_u8; 16 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read_bytes = match read(source_fd, &mut buffer[..requested]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Ok(read_bytes) => read_bytes,
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(errno_error(error)),
        };
        destination.write_all(&buffer[..read_bytes])?;
        remaining = remaining.saturating_sub(read_bytes as u64);
    }
    Ok(())
}

#[cfg(unix)]
fn rename_temporary(
    directory_fd: RawFd,
    temporary: &mut TemporaryEntry,
    destination: &OsStr,
) -> Result<(), DiagnosticError> {
    if !entry_matches_stat(
        directory_fd,
        temporary.name(),
        temporary.expected(),
        Some(1),
    )? {
        return Err(DiagnosticError::UnsafePath);
    }
    renameat(
        Some(directory_fd),
        temporary.name(),
        Some(directory_fd),
        destination,
    )
    .map_err(errno_error)?;
    temporary.disarm();
    if entry_matches_stat(directory_fd, destination, temporary.expected(), Some(1))? {
        Ok(())
    } else {
        Err(DiagnosticError::UnsafePath)
    }
}

#[cfg(unix)]
fn rename_retained_temporary(
    directory_fd: RawFd,
    temporary: &mut TemporaryEntry,
    destination: OsString,
) -> Result<(), DiagnosticError> {
    if !entry_matches_stat(
        directory_fd,
        temporary.name(),
        temporary.expected(),
        Some(1),
    )? {
        return Err(DiagnosticError::UnsafePath);
    }
    renameat(
        Some(directory_fd),
        temporary.name(),
        Some(directory_fd),
        destination.as_os_str(),
    )
    .map_err(errno_error)?;
    temporary.name = destination;
    if entry_matches_stat(
        directory_fd,
        temporary.name(),
        temporary.expected(),
        Some(1),
    )? {
        Ok(())
    } else {
        Err(DiagnosticError::UnsafePath)
    }
}

#[cfg(unix)]
fn sync_directory(directory_fd: RawFd) -> Result<(), DiagnosticError> {
    fsync(directory_fd).map_err(errno_error)
}

#[cfg(unix)]
struct TemporaryEntry {
    directory_fd: RawFd,
    name: OsString,
    expected: FileStat,
    armed: bool,
}

#[cfg(unix)]
impl TemporaryEntry {
    const fn new(directory_fd: RawFd, name: OsString, expected: FileStat) -> Self {
        Self {
            directory_fd,
            name,
            expected,
            armed: true,
        }
    }

    fn name(&self) -> &OsStr {
        &self.name
    }

    const fn expected(&self) -> &FileStat {
        &self.expected
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }

    const fn preserve(&mut self) {
        self.armed = false;
    }

    fn remove(&mut self) -> Result<(), DiagnosticError> {
        if !self.armed {
            return Ok(());
        }
        match fstatat(
            Some(self.directory_fd),
            self.name.as_os_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(actual) if stat_is_regular(&actual) && same_stat(&self.expected, &actual) => {}
            Ok(_) => return Err(DiagnosticError::UnsafePath),
            Err(Errno::ENOENT) => {
                self.armed = false;
                return Ok(());
            }
            Err(error) => return Err(errno_error(error)),
        }
        match unlinkat(
            Some(self.directory_fd),
            self.name.as_os_str(),
            UnlinkatFlags::NoRemoveDir,
        ) {
            Ok(()) | Err(Errno::ENOENT) => {
                self.armed = false;
                Ok(())
            }
            Err(error) => Err(errno_error(error)),
        }
    }
}

#[cfg(unix)]
impl Drop for TemporaryEntry {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

#[cfg(unix)]
enum EntryInspection {
    Regular(OwnedDescriptor, FileStat),
    Missing,
    Unsafe,
}

#[cfg(unix)]
fn inspect_report_entry(directory_fd: RawFd, name: &OsStr) -> Result<EntryInspection, Errno> {
    let expected = match fstatat(Some(directory_fd), name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) if stat_is_private_regular(&stat) => stat,
        Ok(_) => return Ok(EntryInspection::Unsafe),
        Err(Errno::ENOENT) => return Ok(EntryInspection::Missing),
        Err(error) => return Err(error),
    };
    let raw = match openat(
        Some(directory_fd),
        name,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    ) {
        Ok(raw) => raw,
        Err(Errno::ENOENT) => return Ok(EntryInspection::Missing),
        Err(Errno::ELOOP) => return Ok(EntryInspection::Unsafe),
        Err(error) => return Err(error),
    };
    let descriptor = OwnedDescriptor(raw);
    let actual = fstat(descriptor.raw())?;
    if !stat_is_private_regular(&actual) || !same_stat(&expected, &actual) {
        return Ok(EntryInspection::Unsafe);
    }
    fchmod(descriptor.raw(), Mode::from_bits_truncate(0o600))?;
    match fstatat(Some(directory_fd), name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(current) if same_stat(&actual, &current) => {
            Ok(EntryInspection::Regular(descriptor, actual))
        }
        Ok(_) | Err(Errno::ENOENT | Errno::ELOOP) => Ok(EntryInspection::Unsafe),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn ensure_entry_unchanged(
    directory_fd: RawFd,
    name: &OsStr,
    expected: &FileStat,
) -> Result<(), DiagnosticError> {
    if entry_matches_stat(directory_fd, name, expected, Some(1))? {
        Ok(())
    } else {
        Err(DiagnosticError::UnsafePath)
    }
}

#[cfg(unix)]
fn ensure_optional_entry_unchanged(
    directory_fd: RawFd,
    name: &OsStr,
    expected: Option<&FileStat>,
) -> Result<(), DiagnosticError> {
    match (
        expected,
        fstatat(Some(directory_fd), name, AtFlags::AT_SYMLINK_NOFOLLOW),
    ) {
        (None, Err(Errno::ENOENT)) => Ok(()),
        (Some(expected), Ok(actual))
            if stat_is_private_regular(&actual) && same_stat(expected, &actual) =>
        {
            Ok(())
        }
        (_, Err(error)) if error != Errno::ENOENT => Err(errno_error(error)),
        _ => Err(DiagnosticError::UnsafePath),
    }
}

#[cfg(unix)]
fn entry_matches_stat(
    directory_fd: RawFd,
    name: &OsStr,
    expected: &FileStat,
    links: Option<u64>,
) -> Result<bool, DiagnosticError> {
    match fstatat(Some(directory_fd), name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(actual) => Ok(stat_is_regular(&actual)
            && same_stat(expected, &actual)
            && links.is_none_or(|links| stat_link_count(&actual) == links)),
        Err(Errno::ENOENT | Errno::ELOOP) => Ok(false),
        Err(error) => Err(errno_error(error)),
    }
}

#[cfg(unix)]
fn remove_linked_entry_if_same(
    directory_fd: RawFd,
    name: &OsStr,
    expected: &FileStat,
) -> Result<(), DiagnosticError> {
    if !entry_matches_stat(directory_fd, name, expected, None)? {
        return Err(DiagnosticError::UnsafePath);
    }
    unlinkat(Some(directory_fd), name, UnlinkatFlags::NoRemoveDir).map_err(errno_error)
}

#[cfg(unix)]
fn rollback_crash_publication(
    directory_fd: RawFd,
    name: &OsStr,
    expected: &FileStat,
    temporary: &mut TemporaryEntry,
    error: DiagnosticError,
) -> DiagnosticError {
    let operation = diagnostic_error_kind(&error);
    let published_cleanup = remove_linked_entry_if_same(directory_fd, name, expected).err();
    let temporary_cleanup = temporary.remove().err();
    match (published_cleanup, temporary_cleanup) {
        (None, None) => error,
        (Some(cleanup), None) | (None, Some(cleanup)) => DiagnosticError::TemporaryCleanupFailed {
            operation,
            cleanup: diagnostic_error_kind(&cleanup),
        },
        (Some(first_cleanup), Some(second_cleanup)) => DiagnosticError::MultipleCleanupFailed {
            operation,
            first_cleanup: diagnostic_error_kind(&first_cleanup),
            second_cleanup: diagnostic_error_kind(&second_cleanup),
        },
    }
}

#[cfg(unix)]
fn private_file_length(stat: &FileStat) -> Result<u64, DiagnosticError> {
    u64::try_from(stat.st_size).map_err(|_| DiagnosticError::UnsafePath)
}

#[cfg(unix)]
fn stat_is_regular(stat: &FileStat) -> bool {
    SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFREG
}

#[cfg(unix)]
fn stat_is_directory(stat: &FileStat) -> bool {
    SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR
}

#[cfg(unix)]
fn stat_is_private_regular(stat: &FileStat) -> bool {
    stat_is_regular(stat) && stat_link_count(stat) == 1
}

#[cfg(unix)]
#[allow(clippy::useless_conversion)]
fn stat_link_count(stat: &FileStat) -> u64 {
    u64::from(stat.st_nlink)
}

#[cfg(unix)]
fn same_stat(left: &FileStat, right: &FileStat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

#[cfg(unix)]
fn metadata_matches_stat(metadata: &fs::Metadata, stat: &FileStat) -> bool {
    stat_device(stat) == Some(metadata.dev()) && stat.st_ino == metadata.ino()
}

#[cfg(target_os = "macos")]
fn stat_device(stat: &FileStat) -> Option<u64> {
    u64::try_from(stat.st_dev).ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
#[allow(clippy::unnecessary_wraps)]
const fn stat_device(stat: &FileStat) -> Option<u64> {
    Some(stat.st_dev)
}

#[cfg(unix)]
fn secure_private_file_stat(file: &File, expected: &FileStat) -> Result<(), DiagnosticError> {
    let actual = file.metadata()?;
    if !actual.file_type().is_file()
        || actual.nlink() != 1
        || !metadata_matches_stat(&actual, expected)
    {
        return Err(DiagnosticError::UnsafePath);
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
fn open_error(error: Errno) -> DiagnosticError {
    if matches!(error, Errno::ELOOP | Errno::EISDIR | Errno::ENOTDIR) {
        DiagnosticError::UnsafePath
    } else {
        errno_error(error)
    }
}

#[cfg(unix)]
fn errno_error(error: Errno) -> DiagnosticError {
    DiagnosticError::Io(errno_kind(error))
}

#[cfg(unix)]
fn errno_kind(error: Errno) -> io::ErrorKind {
    io::Error::from_raw_os_error(error as i32).kind()
}

#[cfg(unix)]
fn diagnostic_error_kind(error: &DiagnosticError) -> io::ErrorKind {
    match error {
        DiagnosticError::Io(kind) => *kind,
        _ => io::ErrorKind::InvalidData,
    }
}

#[cfg(unix)]
fn temporary_operation_failed(
    temporary: &mut TemporaryEntry,
    operation: io::ErrorKind,
) -> DiagnosticError {
    match temporary.remove() {
        Ok(()) => DiagnosticError::Io(operation),
        Err(cleanup) => DiagnosticError::TemporaryCleanupFailed {
            operation,
            cleanup: diagnostic_error_kind(&cleanup),
        },
    }
}

#[cfg(unix)]
fn cleanup_temporary_after_error(
    temporary: &mut TemporaryEntry,
    error: DiagnosticError,
) -> DiagnosticError {
    let operation = diagnostic_error_kind(&error);
    match temporary.remove() {
        Ok(()) => error,
        Err(cleanup) => DiagnosticError::TemporaryCleanupFailed {
            operation,
            cleanup: diagnostic_error_kind(&cleanup),
        },
    }
}

#[cfg(unix)]
fn cleanup_two_temporaries_after_error(
    first: &mut TemporaryEntry,
    second: &mut TemporaryEntry,
    error: DiagnosticError,
) -> DiagnosticError {
    let operation = diagnostic_error_kind(&error);
    let first_cleanup = first.remove().err();
    let second_cleanup = second.remove().err();
    match (first_cleanup, second_cleanup) {
        (None, None) => error,
        (Some(cleanup), None) | (None, Some(cleanup)) => DiagnosticError::TemporaryCleanupFailed {
            operation,
            cleanup: diagnostic_error_kind(&cleanup),
        },
        (Some(first_cleanup), Some(second_cleanup)) => DiagnosticError::MultipleCleanupFailed {
            operation,
            first_cleanup: diagnostic_error_kind(&first_cleanup),
            second_cleanup: diagnostic_error_kind(&second_cleanup),
        },
    }
}

#[cfg(unix)]
fn cleanup_unverified_temporary_after_fstat_error(
    directory_fd: RawFd,
    name: &OsStr,
    anchor: &OwnedDescriptor,
    operation: Errno,
) -> DiagnosticError {
    let error = errno_error(operation);
    match fstat(anchor.raw()) {
        Ok(stat) => {
            let mut temporary = TemporaryEntry::new(directory_fd, name.to_os_string(), stat);
            cleanup_temporary_after_error(&mut temporary, error)
        }
        Err(cleanup) => DiagnosticError::TemporaryCleanupFailed {
            operation: errno_kind(operation),
            cleanup: errno_kind(cleanup),
        },
    }
}

#[cfg(not(unix))]
fn regular_file_length(path: &Path) -> Result<Option<u64>, DiagnosticError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(metadata.len())),
        Ok(_) => Err(DiagnosticError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn remove_owned_regular_file_if_present(path: &Path) -> Result<(), DiagnosticError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => fs::remove_file(path).map_err(Into::into),
        Ok(_) => Err(DiagnosticError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn open_private_append(path: &Path) -> Result<File, DiagnosticError> {
    let expected = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => return Err(DiagnosticError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return create_private_new(path);
        }
        Err(error) => return Err(error.into()),
    };
    let mut options = OpenOptions::new();
    options.append(true);
    let file = options.open(path)?;
    secure_private_file(&file, &expected)?;
    Ok(file)
}

#[cfg(not(unix))]
fn create_private_new(path: &Path) -> Result<File, DiagnosticError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(DiagnosticError::UnsafePath);
    }
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn secure_private_file(file: &File, expected: &fs::Metadata) -> Result<(), DiagnosticError> {
    let actual = file.metadata()?;
    if !actual.is_file() || !same_file(expected, &actual) {
        return Err(DiagnosticError::UnsafePath);
    }
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    true
}

fn sanitize_log_message(value: &str) -> String {
    sanitize_single_line(value, MAX_DIAGNOSTIC_MESSAGE_BYTES)
}

fn sanitize_single_line(value: &str, limit: usize) -> String {
    let value = truncate_utf8(value, limit);
    let one_line = value
        .chars()
        .filter_map(|character| match character {
            '\n' | '\r' | '\t' => Some(' '),
            '\u{1b}' => None,
            character if character.is_control() => None,
            character => Some(character),
        })
        .collect::<String>();
    let redacted = redact_credentials(&one_line);
    truncate_utf8(&redacted, limit).to_owned()
}

fn sanitize_report_field(value: &str, limit: usize) -> String {
    let value = truncate_utf8(value, limit);
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_was_carriage_return = false;
    for character in value.chars() {
        match character {
            '\r' => {
                sanitized.push('\n');
                previous_was_carriage_return = true;
            }
            '\n' if previous_was_carriage_return => previous_was_carriage_return = false,
            '\n' | '\t' => {
                sanitized.push(character);
                previous_was_carriage_return = false;
            }
            '\u{1b}' => previous_was_carriage_return = false,
            character if character.is_control() => previous_was_carriage_return = false,
            character => {
                sanitized.push(character);
                previous_was_carriage_return = false;
            }
        }
    }
    let redacted = redact_credentials(&sanitized);
    truncate_utf8(&redacted, limit).to_owned()
}

fn redact_credentials(value: &str) -> String {
    let value = redact_matching_spans(value, url_userinfo_at);
    let value = redact_matching_spans(&value, credential_value_at);
    let value = redact_matching_spans(&value, cli_credential_value_at);
    redact_matching_spans(&value, bearer_value_at)
}

fn redact_matching_spans(
    value: &str,
    finder: impl Fn(&[u8], &[u8], usize) -> Option<(usize, usize)>,
) -> String {
    let lowercase = value.to_ascii_lowercase();
    let original = value.as_bytes();
    let lowercase = lowercase.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut copied = 0_usize;
    let mut offset = 0_usize;
    while offset < lowercase.len() {
        let Some((value_start, value_end)) = finder(original, lowercase, offset) else {
            offset += 1;
            continue;
        };
        output.push_str(&value[copied..value_start]);
        output.push_str(REDACTED);
        copied = value_end;
        offset = value_end.max(offset + 1);
    }
    output.push_str(&value[copied..]);
    output
}

fn url_userinfo_at(_original: &[u8], bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    if bytes.get(offset..offset.checked_add(3)?)? != b"://" {
        return None;
    }
    let start = offset + 3;
    let authority_end = bytes[start..]
        .iter()
        .position(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_whitespace())
        .map_or(bytes.len(), |relative| start + relative);
    let at = bytes
        .get(start..authority_end)?
        .iter()
        .rposition(|byte| *byte == b'@')?
        + start;
    Some((start, at))
}

fn cli_credential_value_at(original: &[u8], bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    if bytes.get(offset..offset.checked_add(2)?)? != b"--"
        || (offset > 0 && is_credential_key_byte(bytes[offset - 1]))
    {
        return None;
    }
    let key_start = offset + 2;
    let mut key_end = key_start;
    while bytes
        .get(key_end)
        .is_some_and(|byte| is_credential_key_byte(*byte))
    {
        key_end += 1;
    }
    if key_end == key_start
        || key_end.saturating_sub(key_start) > 128
        || !is_sensitive_credential_key(
            std::str::from_utf8(original.get(key_start..key_end)?).ok()?,
        )
        || !bytes.get(key_end).is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    let mut start = key_end;
    while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    quoted_or_token_value(bytes, start)
}

fn bearer_value_at(_original: &[u8], bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    const BEARER: &[u8] = b"bearer";
    let end = offset.checked_add(BEARER.len())?;
    if bytes.get(offset..end)? != BEARER
        || (offset > 0 && bytes[offset - 1].is_ascii_alphanumeric())
        || !bytes.get(end).is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    let mut start = end;
    while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    quoted_or_token_value(bytes, start)
}

fn quoted_or_token_value(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let first = *bytes.get(start)?;
    if matches!(first, b'\'' | b'"') {
        let content_start = start + 1;
        let end = bytes[content_start..]
            .iter()
            .position(|byte| *byte == first)
            .map_or(bytes.len(), |relative| content_start + relative);
        return Some((content_start, end));
    }
    let end = bytes[start..]
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || matches!(byte, b',' | b';' | b'&'))
        .map_or(bytes.len(), |relative| start + relative);
    (end > start).then_some((start, end))
}

fn credential_value_at(original: &[u8], bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    if offset > 0 && is_credential_key_byte(bytes[offset - 1]) {
        return None;
    }

    let quote = bytes
        .get(offset)
        .copied()
        .filter(|byte| matches!(byte, b'\'' | b'"'));
    let key_start = offset + usize::from(quote.is_some());
    let mut key_end = key_start;
    while bytes
        .get(key_end)
        .is_some_and(|byte| is_credential_key_byte(*byte))
    {
        key_end += 1;
    }
    if key_end == key_start || key_end.saturating_sub(key_start) > 128 {
        return None;
    }
    let mut separator = key_end;
    if let Some(quote) = quote {
        if bytes.get(separator) != Some(&quote) {
            return None;
        }
        separator += 1;
    }
    let key = std::str::from_utf8(original.get(key_start..key_end)?).ok()?;
    if !is_sensitive_credential_key(key) {
        return None;
    }
    while bytes.get(separator).is_some_and(u8::is_ascii_whitespace) {
        separator += 1;
    }
    if !bytes
        .get(separator)
        .is_some_and(|byte| matches!(byte, b':' | b'='))
    {
        return None;
    }
    let mut start = separator + 1;
    while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    let end = bytes[start..]
        .iter()
        .position(|byte| matches!(byte, b',' | b';' | b'&' | b'\n' | b'\r'))
        .map_or(bytes.len(), |relative| start + relative);
    Some((start, end))
}

fn is_sensitive_credential_key(key: &str) -> bool {
    let words = credential_key_words(key);
    let compact = words.concat();
    if matches!(
        compact.as_str(),
        "authorization"
            | "proxyauthorization"
            | "setcookie"
            | "xapikey"
            | "apikey"
            | "apitoken"
            | "accesskey"
            | "accesstoken"
            | "refreshtoken"
            | "clientsecret"
            | "privatekey"
            | "password"
            | "passwd"
            | "cookie"
            | "secret"
            | "token"
            | "credential"
    ) {
        return true;
    }

    let mut previous = None;
    for part in &words {
        let part = part.as_str();
        if matches!(
            part,
            "authorization" | "password" | "passwd" | "secret" | "token" | "cookie" | "credential"
        ) || matches!(
            (previous, part),
            (Some("api" | "access" | "private"), "key")
                | (Some("api" | "access" | "refresh"), "token")
                | (Some("client"), "secret")
        ) {
            return true;
        }
        previous = Some(part);
    }
    false
}

fn credential_key_words(key: &str) -> Vec<String> {
    let bytes = key.as_bytes();
    let mut words = Vec::new();
    let mut word = String::new();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if matches!(byte, b'_' | b'-' | b'.') {
            push_credential_word(&mut words, &mut word);
            continue;
        }

        let previous = index.checked_sub(1).and_then(|index| bytes.get(index));
        let next = bytes.get(index + 1);
        let camel_boundary = byte.is_ascii_uppercase()
            && !word.is_empty()
            && (previous.is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                || (previous.is_some_and(u8::is_ascii_uppercase)
                    && next.is_some_and(u8::is_ascii_lowercase)));
        if camel_boundary {
            push_credential_word(&mut words, &mut word);
        }
        word.push(char::from(byte.to_ascii_lowercase()));
    }
    push_credential_word(&mut words, &mut word);
    words
}

fn push_credential_word(words: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
    }
}

const fn is_credential_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn unix_milliseconds(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn crash_nonce(timestamp_ms: u128, pid: u32, counter: u64) -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(timestamp_ms);
    hasher.write_u32(pid);
    hasher.write_u64(counter);
    hasher.finish()
}

#[cfg(not(unix))]
fn cleanup_temporary(path: &Path, operation: io::ErrorKind) -> DiagnosticError {
    match fs::remove_file(path) {
        Ok(()) => DiagnosticError::Io(operation),
        Err(error) if error.kind() == io::ErrorKind::NotFound => DiagnosticError::Io(operation),
        Err(error) => DiagnosticError::TemporaryCleanupFailed {
            operation,
            cleanup: error.kind(),
        },
    }
}

fn parse_owned_crash_name(name: &OsStr) -> Option<(u128, u32, u64)> {
    let name = name.to_str()?;
    let body = name
        .strip_prefix(CRASH_PREFIX)
        .and_then(|name| name.strip_suffix(CRASH_SUFFIX))?;
    let mut parts = body.split('-');
    let timestamp = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    let counter = parts.next()?.parse().ok()?;
    (parts.next().is_none()).then_some((timestamp, pid, counter))
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let counter = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-diagnostics-test-{}-{counter}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session() -> DiagnosticSession {
        DiagnosticSession::new("greendale-7", "1.2.3", "zsh").unwrap()
    }

    #[test]
    fn rejects_relative_and_unsafe_directories() {
        assert!(matches!(
            DebugLog::open("relative", session()),
            Err(DiagnosticError::RelativeDirectory)
        ));

        let directory = TestDirectory::new();
        let file = directory.0.join("file");
        fs::write(&file, b"not a directory").unwrap();
        assert!(matches!(
            CrashReportStore::open(file),
            Err(DiagnosticError::UnsafePath)
        ));
    }

    #[test]
    fn validates_context_and_component_names() {
        assert!(DiagnosticSession::new("", "1.0.0", "bash").is_err());
        assert!(DiagnosticSession::new("session\nsecret", "1.0.0", "bash").is_err());

        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        assert!(matches!(
            log.write(DiagnosticLevel::Info, "bad component", "message"),
            Err(DiagnosticError::InvalidComponent)
        ));
    }

    #[test]
    fn log_is_single_line_bounded_and_redacts_known_credentials() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        log.write(
            DiagnosticLevel::Error,
            "ai:http",
            "Authorization: Bearer hunter2\napi_key=second-secret; request failed\u{1b}[31m",
        )
        .unwrap();

        let contents = fs::read_to_string(log.path()).unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.contains("level=ERROR component=ai:http"));
        assert!(contents.contains("message=Authorization: <redacted>"));
        assert!(!contents.contains("hunter2"));
        assert!(!contents.contains("second-secret"));
        assert!(!contents.contains('\u{1b}'));

        let bounded = sanitize_log_message(&format!(
            "{} api_key=x",
            "é".repeat(MAX_DIAGNOSTIC_MESSAGE_BYTES)
        ));
        assert!(bounded.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn redacts_header_json_query_and_provider_environment_forms() {
        let cases = [
            ("authorization header", "Authorization: Bearer hunter2"),
            ("proxy header", "Proxy-Authorization = Basic hunter2"),
            ("JSON key", r#"{"OPENAI_API_KEY": "hunter2"}"#),
            ("provider token", "GREENDALE_PROVIDER_TOKEN=hunter2"),
            ("query key", "url?api-key=hunter2&safe=true"),
            ("camel key", "clientSecret=hunter2"),
            ("camel API token", "apiToken=hunter2"),
            ("camel refresh token", "refreshToken=hunter2"),
            ("acronym token", "APIToken=hunter2"),
            ("JSON camel token", r#"{"refreshToken": "hunter2"}"#),
            ("space flag", "argmax --client-secret hunter2 --debug"),
            ("camel flag", "argmax --apiToken hunter2 --debug"),
            ("quoted flag", "argmax --api-key 'hunter2 value'"),
            ("bearer", "request failed for Bearer hunter2"),
            ("URL userinfo", "https://dean:hunter2@example.test/path"),
        ];
        for (label, input) in cases {
            let redacted = redact_credentials(input);
            assert!(!redacted.contains("hunter2"), "{label}: {redacted}");
            assert!(redacted.contains(REDACTED), "{label}: {redacted}");
        }

        for input in [
            "https://hunter2@example.test/path",
            "https://@example.test/path",
            "https://ghp%5Fhunter2@example.test/path",
            "https://hunter2@@example.test/path",
        ] {
            assert_eq!(
                redact_credentials(input),
                "https://<redacted>@example.test/path"
            );
        }

        for harmless in [
            "monkey=banana hockey=puck tokenizer=enabled",
            "contact dean@example.test",
            "https://example.test/path/dean@example.test",
            "https://example.test?contact=dean@example.test",
            "https://example.test#contact=dean@example.test",
        ] {
            assert_eq!(redact_credentials(harmless), harmless);
        }
    }

    #[test]
    fn debug_log_redacts_token_only_url_userinfo() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        log.write(
            DiagnosticLevel::Error,
            "ai:http",
            "request failed at https://ghp_DEANSECRET@example.test/v1",
        )
        .unwrap();

        let contents = fs::read_to_string(log.path()).unwrap();
        assert!(
            contents.contains("https://<redacted>@example.test/v1"),
            "{contents}"
        );
        assert!(!contents.contains("ghp_DEANSECRET"), "{contents}");
    }

    #[test]
    fn rotates_before_crossing_limit_and_keeps_one_previous_file() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session())
            .unwrap()
            .with_test_limit(400);
        log.write(DiagnosticLevel::Info, "session", &"a".repeat(220))
            .unwrap();
        log.write(DiagnosticLevel::Info, "session", &"b".repeat(220))
            .unwrap();
        assert!(log.directory.join(PREVIOUS_DEBUG_LOG_NAME).is_file());
        assert!(log.path().is_file());

        log.write(DiagnosticLevel::Info, "session", &"c".repeat(220))
            .unwrap();
        let mut entries = fs::read_dir(&log.directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            [
                OsString::from(DEBUG_LOG_LOCK_NAME),
                OsString::from(DEBUG_LOG_NAME),
                OsString::from(PREVIOUS_DEBUG_LOG_NAME),
            ]
        );
        assert!(
            fs::read_to_string(log.directory.join(PREVIOUS_DEBUG_LOG_NAME))
                .unwrap()
                .contains(&"b".repeat(32))
        );
    }

    #[cfg(unix)]
    #[test]
    fn rolls_back_partial_appends() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session())
            .unwrap()
            .with_append_failure_after(8);
        log.write(DiagnosticLevel::Info, "session", "first record")
            .unwrap();
        let original = fs::read(log.path()).unwrap();

        assert!(matches!(
            log.write(DiagnosticLevel::Info, "session", "second record"),
            Err(DiagnosticError::Io(io::ErrorKind::Other))
        ));
        assert_eq!(fs::read(log.path()).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn rolls_back_rotation_when_previous_cannot_be_published() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session())
            .unwrap()
            .with_test_limit(1)
            .with_previous_publish_failure();
        let active = b"old active\n";
        let previous = b"older previous\n";
        fs::write(log.path(), active).unwrap();
        fs::write(log.directory.join(PREVIOUS_DEBUG_LOG_NAME), previous).unwrap();

        assert!(matches!(
            log.write(DiagnosticLevel::Info, "session", "new record"),
            Err(DiagnosticError::Io(io::ErrorKind::Other))
        ));
        assert_eq!(fs::read(log.path()).unwrap(), active);
        assert_eq!(
            fs::read(log.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap(),
            previous
        );
        assert!(fs::read_dir(&log.directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(DEBUG_TEMP_PREFIX)
        }));
    }

    #[test]
    fn serializes_concurrent_writers_within_one_process() {
        let directory = TestDirectory::new();
        let log = std::sync::Arc::new(DebugLog::open(directory.0.clone(), session()).unwrap());
        let threads = (0..8)
            .map(|worker| {
                let log = std::sync::Arc::clone(&log);
                std::thread::spawn(move || {
                    for record in 0..50 {
                        log.write(
                            DiagnosticLevel::Debug,
                            "concurrency",
                            &format!("worker={worker} record={record}"),
                        )
                        .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }

        let contents = fs::read_to_string(log.path()).unwrap();
        assert_eq!(contents.lines().count(), 400);
        assert!(contents.lines().all(|line| {
            line.starts_with("timestamp_ms=")
                && line.contains(" level=DEBUG component=concurrency ")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn serializes_rotation_across_processes() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session())
            .unwrap()
            .with_test_limit(1_400);
        let mut children = (0..6)
            .map(|worker| spawn_log_writer(&directory.0, worker, 60, 1_400))
            .collect::<Vec<_>>();
        let statuses = children
            .iter_mut()
            .map(|child| child.wait().unwrap())
            .collect::<Vec<_>>();
        assert!(
            statuses.iter().all(std::process::ExitStatus::success),
            "subprocess statuses: {statuses:?}"
        );

        let active = fs::read_to_string(log.path()).unwrap();
        let previous = fs::read_to_string(log.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap();
        for contents in [&active, &previous] {
            assert!(contents.ends_with('\n'));
            assert!(!contents.is_empty());
            assert!(contents.lines().all(|line| {
                line.starts_with("timestamp_ms=")
                    && line.contains(" level=DEBUG component=multiprocess ")
                    && line.contains(" message=worker=")
                    && line.contains(" payload=")
            }));
        }
        assert!(fs::read_dir(&log.directory).unwrap().all(|entry| {
            matches!(
                entry.unwrap().file_name().to_str(),
                Some(DEBUG_LOG_NAME | PREVIOUS_DEBUG_LOG_NAME | DEBUG_LOG_LOCK_NAME)
            )
        }));
    }

    #[cfg(unix)]
    #[test]
    fn initializes_diagnostic_directories_across_processes() {
        let directory = TestDirectory::new();
        let mut children = (0..6)
            .map(|worker| spawn_log_writer(&directory.0, worker, 10, MAX_DEBUG_LOG_BYTES))
            .collect::<Vec<_>>();
        let statuses = children
            .iter_mut()
            .map(|child| child.wait().unwrap())
            .collect::<Vec<_>>();
        assert!(
            statuses.iter().all(std::process::ExitStatus::success),
            "subprocess statuses: {statuses:?}"
        );

        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let contents = fs::read_to_string(log.path()).unwrap();
        assert_eq!(contents.lines().count(), 60);
    }

    #[cfg(unix)]
    fn spawn_log_writer(
        cache_directory: &Path,
        worker: usize,
        records: usize,
        limit: u64,
    ) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "multiprocess_log_writer_helper", "--nocapture"])
            .env("ARGMAX_DIAGNOSTICS_CHILD_CACHE", cache_directory)
            .env("ARGMAX_DIAGNOSTICS_CHILD_WORKER", worker.to_string())
            .env("ARGMAX_DIAGNOSTICS_CHILD_RECORDS", records.to_string())
            .env("ARGMAX_DIAGNOSTICS_CHILD_LIMIT", limit.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess helper"]
    fn multiprocess_log_writer_helper() {
        let Some(cache_directory) = std::env::var_os("ARGMAX_DIAGNOSTICS_CHILD_CACHE") else {
            return;
        };
        let worker = std::env::var("ARGMAX_DIAGNOSTICS_CHILD_WORKER")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let records = std::env::var("ARGMAX_DIAGNOSTICS_CHILD_RECORDS")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let limit = std::env::var("ARGMAX_DIAGNOSTICS_CHILD_LIMIT")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let log = DebugLog::open(PathBuf::from(cache_directory), session())
            .unwrap()
            .with_test_limit(limit);
        for record in 0..records {
            if let Err(error) = log.write(
                DiagnosticLevel::Debug,
                "multiprocess",
                &format!(
                    "worker={worker} record={record} payload={}",
                    "x".repeat(180)
                ),
            ) {
                let entries = fs::read_dir(&log.directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>();
                panic!("worker={worker} record={record} error={error:?} entries={entries:?}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_storage_tightens_permissions() {
        let directory = TestDirectory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o777)).unwrap();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        log.write(DiagnosticLevel::Info, "session", "started")
            .unwrap();
        assert_eq!(
            fs::metadata(&directory.0).unwrap().permissions().mode() & 0o777,
            0o777
        );
        assert_eq!(
            fs::metadata(&log.directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(log.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(log.directory.join(DEBUG_LOG_LOCK_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn crash_reports_are_private_redacted_and_discoverable() {
        let directory = TestDirectory::new();
        let store = CrashReportStore::open(directory.0.clone()).unwrap();
        let report = CrashReport::new(
            "1.2.3",
            "panic: api_key=dean-secret; provider failed at https://ghp_DEANSECRET@example.test/v1",
            "Authorization: Bearer troy-secret\nframe two",
        );
        let path = store.write(&report).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("argmax crash report"));
        assert!(contents.contains("version: 1.2.3"));
        assert!(contents.contains("<redacted>"));
        assert!(
            contents.contains("https://<redacted>@example.test/v1"),
            "{contents}"
        );
        assert!(!contents.contains("dean-secret"));
        assert!(!contents.contains("ghp_DEANSECRET"));
        assert!(!contents.contains("troy-secret"));
        assert_eq!(store.newest().unwrap(), Some(path));
        assert!(fs::read_dir(&store.directory).unwrap().all(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".log")
        }));
    }

    #[test]
    fn crash_version_cannot_inject_report_fields() {
        let report = CrashReport::new(
            "1.2.3\nfailure: forged api_key=secret",
            "real failure",
            "frame",
        );
        let rendered = report.render(7);
        assert!(
            rendered.contains("version: 1.2.3 failure: forged api_key=<redacted>"),
            "{rendered:?}"
        );
        assert_eq!(rendered.matches("failure:\n").count(), 1);
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn clear_removes_only_strict_owned_report_names() {
        let directory = TestDirectory::new();
        let store = CrashReportStore::open(directory.0.clone()).unwrap();
        let report = store
            .write(&CrashReport::new("1.0.0", "failure", "frame"))
            .unwrap();
        let unrelated = store.directory.join("crash-not-owned.log");
        fs::write(&unrelated, b"keep").unwrap();
        let other = store.directory.join("notes.txt");
        fs::write(&other, b"keep").unwrap();

        let outcome = store.clear().unwrap();
        assert_eq!(outcome.removed, vec![report]);
        assert!(outcome.failures.is_empty());
        assert!(unrelated.exists());
        assert!(other.exists());
    }

    #[test]
    fn newest_uses_numeric_name_order_when_modified_times_tie() {
        let directory = TestDirectory::new();
        let store = CrashReportStore::open(directory.0.clone()).unwrap();
        let ninth = store.directory.join("crash-100-2-9.log");
        let tenth = store.directory.join("crash-100-2-10.log");
        fs::write(&ninth, b"ninth").unwrap();
        fs::write(&tenth, b"tenth").unwrap();
        let times =
            fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(100));
        File::open(&ninth).unwrap().set_times(times).unwrap();
        File::open(&tenth).unwrap().set_times(times).unwrap();
        assert_eq!(store.newest().unwrap(), Some(tenth));
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_cache_ancestor_symlinks_before_retaining_paths() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let actual = directory.0.join("actual-cache");
        let replacement = directory.0.join("replacement-cache");
        fs::create_dir(&actual).unwrap();
        fs::create_dir(&replacement).unwrap();
        let alias = directory.0.join("cache-alias");
        symlink(&actual, &alias).unwrap();

        let log = DebugLog::open(&alias, session()).unwrap();
        assert!(
            log.directory
                .starts_with(fs::canonicalize(&actual).unwrap())
        );
        fs::remove_file(&alias).unwrap();
        symlink(&replacement, &alias).unwrap();
        log.write(DiagnosticLevel::Info, "session", "still canonical")
            .unwrap();
        assert!(log.path().is_file());
        assert!(!replacement.join(APPLICATION_DIRECTORY_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn newest_and_clear_never_follow_owned_looking_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let store = CrashReportStore::open(directory.0.clone()).unwrap();
        let target = directory.0.join("outside.txt");
        fs::write(&target, b"preserve").unwrap();
        let link = store.directory.join("crash-1-2-3.log");
        symlink(&target, &link).unwrap();

        assert_eq!(store.newest().unwrap(), None);
        let outcome = store.clear().unwrap();
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert!(target.exists());
        assert!(link.exists());
    }

    #[cfg(unix)]
    #[test]
    fn debug_log_never_follows_active_or_rotated_symlinks() {
        use std::os::unix::fs::symlink;

        let active_directory = TestDirectory::new();
        let active_target = active_directory.0.join("target.txt");
        fs::write(&active_target, b"preserve").unwrap();
        let active = DebugLog::open(active_directory.0.clone(), session()).unwrap();
        symlink(&active_target, active.path()).unwrap();
        assert!(matches!(
            active.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
        assert_eq!(fs::read(&active_target).unwrap(), b"preserve");

        let rotated_directory = TestDirectory::new();
        let rotated_target = rotated_directory.0.join("target.txt");
        fs::write(&rotated_target, b"preserve").unwrap();
        let rotated = DebugLog::open(rotated_directory.0.clone(), session())
            .unwrap()
            .with_test_limit(1);
        fs::write(rotated.path(), b"active").unwrap();
        symlink(
            &rotated_target,
            rotated.directory.join(PREVIOUS_DEBUG_LOG_NAME),
        )
        .unwrap();
        assert!(matches!(
            rotated.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
        assert_eq!(fs::read(&rotated_target).unwrap(), b"preserve");
    }

    #[cfg(unix)]
    #[test]
    fn debug_log_rejects_lock_symlinks_hard_links_and_directory_swaps() {
        use std::os::unix::fs::symlink;

        let lock_directory = TestDirectory::new();
        let lock_log = DebugLog::open(lock_directory.0.clone(), session()).unwrap();
        let lock_target = lock_directory.0.join("lock-target");
        fs::write(&lock_target, b"preserve").unwrap();
        symlink(&lock_target, lock_log.directory.join(DEBUG_LOG_LOCK_NAME)).unwrap();
        assert!(matches!(
            lock_log.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
        assert_eq!(fs::read(&lock_target).unwrap(), b"preserve");

        let hard_link_directory = TestDirectory::new();
        let hard_link_log = DebugLog::open(hard_link_directory.0.clone(), session()).unwrap();
        let hard_link_target = hard_link_directory.0.join("outside.log");
        fs::write(&hard_link_target, b"preserve").unwrap();
        fs::hard_link(&hard_link_target, hard_link_log.path()).unwrap();
        assert!(matches!(
            hard_link_log.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
        assert_eq!(fs::read(&hard_link_target).unwrap(), b"preserve");

        let swap_directory = TestDirectory::new();
        let swap_log = DebugLog::open(swap_directory.0.clone(), session()).unwrap();
        let retained = swap_log.directory.clone();
        let parked = swap_directory.0.join("parked-logs");
        let replacement = swap_directory.0.join("replacement-logs");
        fs::create_dir(&replacement).unwrap();
        fs::rename(&retained, &parked).unwrap();
        symlink(&replacement, &retained).unwrap();
        assert!(matches!(
            swap_log.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
        assert!(fs::read_dir(replacement).unwrap().next().is_none());

        let special_directory = TestDirectory::new();
        let special_log = DebugLog::open(special_directory.0.clone(), session()).unwrap();
        nix::unistd::mkfifo(&special_log.path(), Mode::from_bits_truncate(0o600)).unwrap();
        assert!(matches!(
            special_log.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
        fs::remove_file(special_log.path()).unwrap();
        fs::create_dir(special_log.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap();
        assert!(matches!(
            special_log.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn removes_only_valid_stale_debug_transaction_files() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let stale = log
            .directory
            .join(".debug-transaction-active-10-20-0123456789abcdef.tmp");
        fs::write(&stale, b"incomplete").unwrap();
        log.write(DiagnosticLevel::Info, "session", "message")
            .unwrap();
        assert!(!stale.exists());

        let target = directory.0.join("outside-temporary");
        fs::write(&target, b"preserve").unwrap();
        let hostile = log
            .directory
            .join(".debug-transaction-previous-10-21-fedcba9876543210.tmp");
        symlink(&target, &hostile).unwrap();
        assert!(matches!(
            log.write(DiagnosticLevel::Info, "session", "message"),
            Err(DiagnosticError::UnsafePath)
        ));
        assert_eq!(fs::read(target).unwrap(), b"preserve");
        assert!(hostile.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn recovers_previous_log_after_active_rotation_publish() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let old_active = b"old active\n";
        let temporary = log
            .directory
            .join(".debug-transaction-previous-10-20-0123456789abcdef.tmp");
        fs::write(log.path(), b"new active\n").unwrap();
        fs::write(
            log.directory.join(PREVIOUS_DEBUG_LOG_NAME),
            b"older previous\n",
        )
        .unwrap();
        fs::write(&temporary, old_active).unwrap();

        log.write(DiagnosticLevel::Info, "session", "continued")
            .unwrap();

        assert_eq!(
            fs::read(log.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap(),
            old_active
        );
        let active = fs::read_to_string(log.path()).unwrap();
        assert!(active.starts_with("new active\n"));
        assert!(active.contains("message=continued"));
        assert!(!temporary.exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovers_an_interrupted_rotation_on_the_next_write() {
        let directory = TestDirectory::new();
        let interrupted = DebugLog::open(directory.0.clone(), session())
            .unwrap()
            .with_test_limit(1)
            .with_active_publish_interruption();
        let old_active = b"old active\n";
        let older_previous = b"older previous\n";
        fs::write(interrupted.path(), old_active).unwrap();
        fs::write(
            interrupted.directory.join(PREVIOUS_DEBUG_LOG_NAME),
            older_previous,
        )
        .unwrap();

        assert!(matches!(
            interrupted.write(DiagnosticLevel::Info, "session", "new active"),
            Err(DiagnosticError::Io(io::ErrorKind::Interrupted))
        ));
        assert_eq!(
            fs::read(interrupted.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap(),
            older_previous
        );
        let temporary = fs::read_dir(&interrupted.directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name().is_some_and(|name| {
                    parse_debug_temporary_name(name) == Some(DebugTemporaryPurpose::Previous)
                })
            })
            .unwrap();
        assert_eq!(fs::read(&temporary).unwrap(), old_active);

        let recovered = DebugLog::open(directory.0.clone(), session()).unwrap();
        recovered
            .write(DiagnosticLevel::Info, "session", "continued")
            .unwrap();

        assert_eq!(
            fs::read(recovered.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap(),
            old_active
        );
        let active = fs::read_to_string(recovered.path()).unwrap();
        assert!(active.contains("message=new active"));
        assert!(active.contains("message=continued"));
        assert!(!temporary.exists());
    }

    #[cfg(unix)]
    #[test]
    fn discards_precommit_previous_copy_without_replacing_previous_log() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let active = b"old active\n";
        let previous = b"older previous\n";
        let temporary = log
            .directory
            .join(".debug-transaction-previous-ready-10-20-0123456789abcdef.tmp");
        fs::write(log.path(), active).unwrap();
        fs::write(log.directory.join(PREVIOUS_DEBUG_LOG_NAME), previous).unwrap();
        fs::write(&temporary, active).unwrap();

        log.write(DiagnosticLevel::Info, "session", "continued")
            .unwrap();

        assert_eq!(
            fs::read(log.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap(),
            previous
        );
        assert!(!temporary.exists());
    }

    #[cfg(unix)]
    #[test]
    fn discards_committing_copy_when_active_replacement_still_exists() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let active = b"old active\n";
        let previous = b"older previous\n";
        let previous_temporary = log
            .directory
            .join(".debug-transaction-previous-10-20-0123456789abcdef.tmp");
        let active_temporary = log
            .directory
            .join(".debug-transaction-active-10-21-fedcba9876543210.tmp");
        fs::write(log.path(), active).unwrap();
        fs::write(log.directory.join(PREVIOUS_DEBUG_LOG_NAME), previous).unwrap();
        fs::write(&previous_temporary, active).unwrap();
        fs::write(&active_temporary, b"new active\n").unwrap();

        log.write(DiagnosticLevel::Info, "session", "continued")
            .unwrap();

        assert_eq!(
            fs::read(log.directory.join(PREVIOUS_DEBUG_LOG_NAME)).unwrap(),
            previous
        );
        assert!(!previous_temporary.exists());
        assert!(!active_temporary.exists());
    }

    #[cfg(unix)]
    #[test]
    fn reports_temporary_cleanup_identity_failures() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let name = OsString::from(".debug-transaction-active-10-20-0123456789abcdef.tmp");
        let (file, _stat, mut temporary) =
            create_private_temporary_named(&log.directory, log.directory_fd.raw(), name.clone())
                .unwrap();
        drop(file);
        fs::remove_file(log.directory.join(&name)).unwrap();
        let target = directory.0.join("outside-cleanup-target");
        fs::write(&target, b"preserve").unwrap();
        symlink(&target, log.directory.join(&name)).unwrap();

        let error = cleanup_temporary_after_error(
            &mut temporary,
            DiagnosticError::Io(io::ErrorKind::Other),
        );

        assert!(matches!(
            error,
            DiagnosticError::TemporaryCleanupFailed {
                operation: io::ErrorKind::Other,
                cleanup: io::ErrorKind::InvalidData,
            }
        ));
        assert_eq!(fs::read(target).unwrap(), b"preserve");
        assert!(
            log.directory
                .join(name)
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn reports_both_temporary_cleanup_failures() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let first_name = OsString::from(".debug-transaction-active-10-20-0123456789abcdef.tmp");
        let second_name = OsString::from(".debug-transaction-active-10-21-fedcba9876543210.tmp");
        let (first_file, _first_stat, mut first) = create_private_temporary_named(
            &log.directory,
            log.directory_fd.raw(),
            first_name.clone(),
        )
        .unwrap();
        let (second_file, _second_stat, mut second) = create_private_temporary_named(
            &log.directory,
            log.directory_fd.raw(),
            second_name.clone(),
        )
        .unwrap();
        drop((first_file, second_file));
        fs::remove_file(log.directory.join(&first_name)).unwrap();
        fs::remove_file(log.directory.join(&second_name)).unwrap();
        let target = directory.0.join("outside-cleanup-target");
        fs::write(&target, b"preserve").unwrap();
        symlink(&target, log.directory.join(&first_name)).unwrap();
        symlink(&target, log.directory.join(&second_name)).unwrap();

        let error = cleanup_two_temporaries_after_error(
            &mut first,
            &mut second,
            DiagnosticError::Io(io::ErrorKind::Other),
        );

        assert!(matches!(
            error,
            DiagnosticError::MultipleCleanupFailed {
                operation: io::ErrorKind::Other,
                first_cleanup: io::ErrorKind::InvalidData,
                second_cleanup: io::ErrorKind::InvalidData,
            }
        ));
        assert_eq!(fs::read(target).unwrap(), b"preserve");
        for name in [first_name, second_name] {
            assert!(
                log.directory
                    .join(name)
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn cleans_a_created_entry_when_initial_metadata_retry_succeeds() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let name = OsStr::new(".debug-transaction-active-10-20-0123456789abcdef.tmp");
        let raw = openat(
            Some(log.directory_fd.raw()),
            name,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .unwrap();
        let anchor = OwnedDescriptor(raw);

        let error = cleanup_unverified_temporary_after_fstat_error(
            log.directory_fd.raw(),
            name,
            &anchor,
            Errno::EIO,
        );

        assert!(matches!(error, DiagnosticError::Io(_)));
        assert!(!log.directory.join(name).exists());
    }

    #[cfg(unix)]
    #[test]
    fn crash_clear_rejects_hard_linked_reports() {
        let directory = TestDirectory::new();
        let store = CrashReportStore::open(directory.0.clone()).unwrap();
        let report = store
            .write(&CrashReport::new("1.0.0", "failure", "frame"))
            .unwrap();
        let outside = directory.0.join("outside-report.log");
        fs::hard_link(&report, &outside).unwrap();

        assert_eq!(store.newest().unwrap(), None);
        let outcome = store.clear().unwrap();
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert!(report.exists());
        assert!(outside.exists());
    }

    #[test]
    fn debug_output_does_not_disclose_paths_or_crash_contents() {
        let directory = TestDirectory::new();
        let log = DebugLog::open(directory.0.clone(), session()).unwrap();
        let store = CrashReportStore::open(directory.0.clone()).unwrap();
        let report = CrashReport::new("1.0.0", "Troy's secret", "Greendale path");
        for debug in [
            format!("{log:?}"),
            format!("{store:?}"),
            format!("{report:?}"),
        ] {
            assert!(!debug.contains(directory.0.to_string_lossy().as_ref()));
            assert!(!debug.contains("Troy"));
            assert!(!debug.contains("Greendale"));
        }

        let outcome = CrashClearOutcome {
            removed: vec![directory.0.join("crash-1-2-3.log")],
            failures: vec![CrashRemovalFailure {
                path: directory.0.join("crash-4-5-6.log"),
                kind: io::ErrorKind::PermissionDenied,
            }],
        };
        let debug = format!("{outcome:?}");
        assert_eq!(
            debug,
            "CrashClearOutcome { removed_count: 1, failure_count: 1 }"
        );
        assert!(!debug.contains(directory.0.to_string_lossy().as_ref()));

        let error = DiagnosticError::from(io::Error::other("api_key=secret"));
        assert!(!format!("{error:?} {error}").contains("secret"));
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let message = format!("{}é", "a".repeat(MAX_DIAGNOSTIC_MESSAGE_BYTES - 1));
        let sanitized = sanitize_log_message(&message);
        assert_eq!(sanitized.len(), MAX_DIAGNOSTIC_MESSAGE_BYTES - 1);
        assert!(sanitized.is_char_boundary(sanitized.len()));
    }
}
