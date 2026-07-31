//! Atomic live configuration reload with last-known-good retention.
//!
//! The caller invokes [`ConfigReloader::poll`] from a background task. The
//! reloader enforces its own one-second floor, so an edit observed immediately
//! after a poll is still eligible well inside the two-second product deadline.

use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::num::NonZeroI32;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::config::{
    CliOverrides, ConfigFileError, ConfigStore, ConfigStoreError, EnvironmentOverrides, Settings,
    ValidationErrors, ValidationProblem, resolve_settings,
};

/// Minimum interval between filesystem polls.
pub const RELOAD_POLL_INTERVAL: Duration = Duration::from_secs(1);
const RELOAD_POLL_INTERVAL_MS: u64 = 1_000;
const MAX_RESOLVED_FAILURE_FIELDS: usize = 64;
const MAX_RELOAD_REASON_BYTES: usize = 256;

/// Private event-stream prefix for a correlated active-session reload request.
pub const RELOAD_REQUEST_PREFIX: &[u8] = b"reload-request:";
/// Private control-stream prefix for the corresponding wrapper acknowledgment.
pub const RELOAD_ACK_PREFIX: &[u8] = b"reload-ack:";
/// Maximum time an explicit child command waits for its owning wrapper.
pub const RELOAD_REQUEST_TIMEOUT: Duration = Duration::from_millis(2_500);
const MAX_RELOAD_WIRE_BYTES: usize = 128;

/// Failure to request a reload from the active wrapper capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadRequestError {
    /// The command is not running inside an owned argmax session.
    NoActiveSession,
    /// The inherited integration descriptor was malformed.
    InvalidDescriptor,
    /// The inherited descriptor was unavailable or did not acknowledge in time.
    Transport(io::ErrorKind),
    /// The response was not the exact correlated private acknowledgment.
    InvalidAcknowledgment,
    /// The wrapper rejected the candidate and retained its prior settings.
    Rejected,
}

impl fmt::Display for ReloadRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveSession => {
                formatter.write_str("reload is available only inside an active argmax session")
            }
            Self::InvalidDescriptor => {
                formatter.write_str("active-session reload capability is invalid")
            }
            Self::Transport(kind) => {
                write!(formatter, "active-session reload transport failed: {kind}")
            }
            Self::InvalidAcknowledgment => {
                formatter.write_str("active-session reload acknowledgment was invalid")
            }
            Self::Rejected => formatter
                .write_str("configuration reload was rejected; previous settings remain active"),
        }
    }
}

impl Error for ReloadRequestError {}

/// Requests and confirms one reload through the inherited session capability.
///
/// Only a fixed, correlated control frame is exchanged. No configuration,
/// command buffer, environment value, or path is transmitted.
///
/// # Errors
///
/// Returns [`ReloadRequestError`] when no active wrapper exists, its descriptor
/// is invalid, the bounded exchange fails, or the wrapper rejects the reload.
#[cfg(unix)]
pub fn request_active_session_reload() -> Result<(), ReloadRequestError> {
    if std::env::var_os(crate::pty::ENV_PRIVATE_SESSION).is_none() {
        return Err(ReloadRequestError::NoActiveSession);
    }
    let descriptor = std::env::var_os(crate::pty::ENV_EVENT_FD)
        .as_deref()
        .and_then(parse_descriptor)
        .ok_or(ReloadRequestError::InvalidDescriptor)?;
    let nonce = std::process::id();
    let request = format!("reload-request:{nonce}");
    let mut response = [0_u8; MAX_RELOAD_WIRE_BYTES];
    let response_length = argmax_platform::unix::exchange_inherited_unix_frame(
        descriptor,
        request.as_bytes(),
        &mut response,
        RELOAD_REQUEST_TIMEOUT,
    )
    .map_err(|error| ReloadRequestError::Transport(error.kind()))?;
    let response = response
        .get(..response_length)
        .ok_or(ReloadRequestError::InvalidAcknowledgment)?;
    let ok = format!("reload-ack:{nonce}:ok");
    if response == ok.as_bytes() {
        return Ok(());
    }
    let rejected = format!("reload-ack:{nonce}:rejected");
    if response == rejected.as_bytes() {
        Err(ReloadRequestError::Rejected)
    } else {
        Err(ReloadRequestError::InvalidAcknowledgment)
    }
}

#[cfg(unix)]
fn parse_descriptor(value: &OsStr) -> Option<NonZeroI32> {
    let value = value.to_str()?;
    if value.is_empty() || value.len() > 10 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<i32>()
        .ok()
        .filter(|descriptor| *descriptor >= 3)
        .and_then(NonZeroI32::new)
}

#[derive(Clone)]
struct PublishedSettings {
    generation: u64,
    settings: Arc<Settings>,
}

/// Thread-safe handle to one complete, validated settings generation.
#[derive(Clone)]
pub struct SharedSettings {
    inner: Arc<RwLock<PublishedSettings>>,
}

impl SharedSettings {
    fn new(settings: Settings) -> Self {
        Self {
            inner: Arc::new(RwLock::new(PublishedSettings {
                generation: 0,
                settings: Arc::new(settings),
            })),
        }
    }

    /// Returns one coherent generation without retaining the read lock.
    #[must_use]
    pub fn snapshot(&self) -> SettingsSnapshot {
        let published = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        SettingsSnapshot {
            generation: published.generation,
            settings: Arc::clone(&published.settings),
        }
    }

    fn publish(&self, generation: u64, settings: Settings) {
        let mut published = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *published = PublishedSettings {
            generation,
            settings: Arc::new(settings),
        };
    }
}

impl fmt::Debug for SharedSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = self.snapshot();
        formatter
            .debug_struct("SharedSettings")
            .field("generation", &snapshot.generation)
            .finish_non_exhaustive()
    }
}

/// Owned view of one atomically published settings generation.
#[derive(Clone)]
pub struct SettingsSnapshot {
    generation: u64,
    settings: Arc<Settings>,
}

impl SettingsSnapshot {
    /// Monotonic generation, beginning at zero for the startup settings.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Complete validated settings for this generation.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}

impl fmt::Debug for SettingsSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SettingsSnapshot")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// One runtime action caused by a successful whole-settings replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadChange {
    /// Session mode policy or core behavior changed.
    Core,
    /// Rendering settings changed.
    Ui,
    /// Resolved keybindings changed.
    Keybindings,
    /// Git candidate settings changed.
    Git,
    /// Update scheduling settings changed.
    Updater,
    /// One or more AI settings changed.
    Ai,
    /// Active AI work must lose authority immediately.
    CancelAi,
    /// Shell selection changed and applies only to the next wrapper session.
    ShellForNextSession,
}

/// Ordered, duplicate-free runtime actions for one successful replacement.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReloadDelta {
    changes: Box<[ReloadChange]>,
}

impl ReloadDelta {
    fn between(previous: &Settings, next: &Settings) -> Self {
        let selected_provider_changed = selected_provider_changed(previous, next);
        let mut changes = Vec::with_capacity(8);
        for (changed, change) in [
            (previous.core != next.core, ReloadChange::Core),
            (previous.ui != next.ui, ReloadChange::Ui),
            (
                previous.keybindings != next.keybindings,
                ReloadChange::Keybindings,
            ),
            (previous.git != next.git, ReloadChange::Git),
            (previous.updater != next.updater, ReloadChange::Updater),
            (previous.ai != next.ai, ReloadChange::Ai),
            (
                previous.ai.enabled && (!next.ai.enabled || selected_provider_changed),
                ReloadChange::CancelAi,
            ),
            (
                previous.core.shell != next.core.shell,
                ReloadChange::ShellForNextSession,
            ),
        ] {
            if changed {
                changes.push(change);
            }
        }
        Self {
            changes: changes.into_boxed_slice(),
        }
    }

    /// Whether this replacement requires the supplied runtime action.
    #[must_use]
    pub fn contains(&self, change: ReloadChange) -> bool {
        self.changes.contains(&change)
    }

    /// Required runtime actions in stable component order.
    #[must_use]
    pub fn changes(&self) -> &[ReloadChange] {
        &self.changes
    }
}

/// Result of one caller-requested poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadPoll {
    /// The one-second poll floor has not elapsed.
    NotDue {
        /// Earliest caller time that may touch the filesystem.
        ready_at_ms: u64,
    },
    /// The file was valid but resolved to the already active settings.
    Unchanged,
    /// A complete validated settings generation was published.
    Applied {
        /// Newly published generation.
        generation: u64,
        /// Components that consumers should refresh.
        delta: ReloadDelta,
    },
    /// The candidate was rejected and the prior generation remains active.
    Rejected,
}

/// Safe failure retained for diagnostics and the next explicit config display.
#[derive(Debug)]
pub enum ReloadFailure {
    /// Loading or parsing the file failed.
    Store(ReloadStoreFailure),
    /// Environment/CLI precedence produced invalid resolved settings.
    Resolved(ResolvedSettingsFailure),
    /// A previously present configuration disappeared.
    MissingReplacement,
    /// No distinct generation can be assigned without wrapping.
    GenerationExhausted,
}

/// Bounded, value-free classification of a configuration-store failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadStoreFailure {
    /// No standard platform directory is available.
    NoPlatformDirectory,
    /// The configured path has no parent.
    MissingParent,
    /// A final path is not a safe regular private file.
    UnsafeFileType,
    /// A parent path is not a safe private directory.
    UnsafeDirectoryType,
    /// The file is not UTF-8.
    InvalidUtf8,
    /// The configured byte limit was exceeded.
    TooLarge {
        /// Observed bounded-read size.
        bytes: usize,
    },
    /// TOML syntax or types are malformed.
    InvalidToml,
    /// Known fields failed validation.
    InvalidFields(ResolvedSettingsFailure),
    /// Validated settings could not be rendered.
    Render,
    /// Filesystem operation failed without retaining a path.
    Io {
        /// Stable operation label.
        operation: &'static str,
        /// Standard path-free I/O category.
        kind: io::ErrorKind,
    },
    /// No backup name was available.
    BackupNameExhausted,
    /// A migration destination appeared concurrently.
    MigrationDestinationExists,
    /// The migration source changed concurrently.
    MigrationSourceChanged,
    /// Interrupted migration state needs explicit recovery.
    MigrationRecoveryRequired,
    /// The effective source kept changing under concurrent processes.
    SourceChangedConcurrently,
    /// Another process held the configuration lock past the bounded wait.
    LockUnavailable,
}

impl ReloadStoreFailure {
    fn from_store(error: ConfigStoreError) -> Self {
        match error {
            ConfigStoreError::NoPlatformDirectory => Self::NoPlatformDirectory,
            ConfigStoreError::MissingParent => Self::MissingParent,
            ConfigStoreError::UnsafeFileType => Self::UnsafeFileType,
            ConfigStoreError::UnsafeDirectoryType => Self::UnsafeDirectoryType,
            ConfigStoreError::InvalidUtf8 => Self::InvalidUtf8,
            ConfigStoreError::Config(error) => Self::from_config(error),
            ConfigStoreError::Io { operation, kind } => Self::Io { operation, kind },
            ConfigStoreError::BackupNameExhausted => Self::BackupNameExhausted,
            ConfigStoreError::MigrationDestinationExists => Self::MigrationDestinationExists,
            ConfigStoreError::MigrationSourceChanged => Self::MigrationSourceChanged,
            ConfigStoreError::MigrationRecoveryRequired => Self::MigrationRecoveryRequired,
            ConfigStoreError::SourceChangedConcurrently => Self::SourceChangedConcurrently,
            ConfigStoreError::LockUnavailable => Self::LockUnavailable,
        }
    }

    fn from_config(error: ConfigFileError) -> Self {
        match error {
            ConfigFileError::TooLarge { bytes } => Self::TooLarge { bytes },
            ConfigFileError::InvalidToml => Self::InvalidToml,
            ConfigFileError::Invalid(problems) => {
                Self::InvalidFields(ResolvedSettingsFailure::from_problems(
                    problems
                        .problems()
                        .iter()
                        .map(|problem| ReloadProblem::new(&problem.field, &problem.message)),
                ))
            }
            ConfigFileError::Render => Self::Render,
        }
    }
}

impl fmt::Display for ReloadStoreFailure {
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
            Self::TooLarge { bytes } => {
                write!(
                    formatter,
                    "configuration exceeds its byte limit ({bytes} bytes observed)"
                )
            }
            Self::InvalidToml => formatter.write_str("configuration is not valid TOML"),
            Self::InvalidFields(error) => error.fmt(formatter),
            Self::Render => formatter.write_str("configuration could not be rendered"),
            Self::Io { operation, kind } => write!(formatter, "{operation}: {kind:?}"),
            Self::BackupNameExhausted => {
                formatter.write_str("no unused configuration backup name is available")
            }
            Self::MigrationDestinationExists => {
                formatter.write_str("a migration destination appeared and was preserved")
            }
            Self::MigrationSourceChanged => {
                formatter.write_str("the migration source changed and was preserved")
            }
            Self::MigrationRecoveryRequired => {
                formatter.write_str("an interrupted configuration migration needs recovery")
            }
            Self::SourceChangedConcurrently => {
                formatter.write_str("configuration changed concurrently; retrying")
            }
            Self::LockUnavailable => {
                formatter.write_str("another process is holding the configuration lock; retrying")
            }
        }
    }
}

impl Error for ReloadStoreFailure {}

impl fmt::Display for ReloadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Resolved(error) => error.fmt(formatter),
            Self::MissingReplacement => formatter
                .write_str("configuration disappeared; retaining the last known good settings"),
            Self::GenerationExhausted => {
                formatter.write_str("configuration generation counter is exhausted")
            }
        }
    }
}

impl Error for ReloadFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Resolved(_) | Self::MissingReplacement | Self::GenerationExhausted => None,
        }
    }
}

/// One bounded, value-free field validation summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReloadProblem {
    field: String,
    reason: String,
}

impl ReloadProblem {
    fn new(field: &str, reason: &str) -> Self {
        Self {
            field: safe_validation_field(field),
            reason: bounded_safe_reason(reason),
        }
    }

    /// Responsible field, with dynamic provider names redacted.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Safe bounded reason without the rejected value.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Bounded, value-free resolved-validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSettingsFailure {
    problems: Box<[ReloadProblem]>,
    problem_count: usize,
}

impl ResolvedSettingsFailure {
    fn from_validation(errors: ValidationErrors) -> Self {
        Self::from_problems(
            errors.into_errors().into_iter().map(|error| {
                ReloadProblem::new(&error.field, &safe_validation_reason(&error.problem))
            }),
        )
    }

    fn from_problems(problems: impl IntoIterator<Item = ReloadProblem>) -> Self {
        let mut retained = Vec::new();
        let mut problem_count = 0_usize;
        for problem in problems {
            problem_count = problem_count.saturating_add(1);
            if retained.len() < MAX_RESOLVED_FAILURE_FIELDS && !retained.contains(&problem) {
                retained.push(problem);
            }
        }
        Self {
            problems: retained.into_boxed_slice(),
            problem_count,
        }
    }

    /// Distinct retained field failures in deterministic order.
    #[must_use]
    pub fn problems(&self) -> &[ReloadProblem] {
        &self.problems
    }

    /// Total number of failures before bounding and safe deduplication.
    #[must_use]
    pub const fn problem_count(&self) -> usize {
        self.problem_count
    }

    /// Whether repeated or additional failures were omitted.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.problem_count > self.problems.len()
    }
}

impl fmt::Display for ResolvedSettingsFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid resolved configuration")?;
        if !self.problems.is_empty() {
            formatter.write_str(": ")?;
            for (index, problem) in self.problems.iter().enumerate() {
                if index != 0 {
                    formatter.write_str("; ")?;
                }
                write!(formatter, "{}: {}", problem.field, problem.reason)?;
            }
        }
        let omitted = self.problem_count.saturating_sub(self.problems.len());
        if omitted != 0 {
            write!(formatter, "; {omitted} additional failures omitted")?;
        }
        Ok(())
    }
}

impl Error for ResolvedSettingsFailure {}

/// Background-poll state for one resolved configuration.
pub struct ConfigReloader {
    store: ConfigStore,
    environment: EnvironmentOverrides,
    cli: CliOverrides,
    shared: SharedSettings,
    observed_file: bool,
    last_poll_ms: Option<u64>,
    last_failure: Option<ReloadFailure>,
    warning_count: usize,
}

impl ConfigReloader {
    /// Loads startup settings and creates a live-reload coordinator.
    ///
    /// # Errors
    ///
    /// Returns the exact safe load or resolved-validation error when the startup
    /// file is invalid. A missing file resolves defaults plus process overrides.
    pub fn start(
        store: ConfigStore,
        environment: EnvironmentOverrides,
        cli: CliOverrides,
    ) -> Result<Self, ReloadFailure> {
        let document = store
            .load()
            .map_err(ReloadStoreFailure::from_store)
            .map_err(ReloadFailure::Store)?;
        let observed_file = document.is_some();
        let warning_count = document
            .as_ref()
            .map_or(0, |document| document.warnings.len());
        let settings = resolve_settings(
            document.as_ref().map(|document| &document.settings),
            &environment,
            cli,
        )
        .map_err(ResolvedSettingsFailure::from_validation)
        .map_err(ReloadFailure::Resolved)?;
        Ok(Self {
            store,
            environment,
            cli,
            shared: SharedSettings::new(settings),
            observed_file,
            last_poll_ms: None,
            last_failure: None,
            warning_count,
        })
    }

    /// Cloneable atomic settings handle for runtime consumers.
    #[must_use]
    pub fn shared_settings(&self) -> SharedSettings {
        self.shared.clone()
    }

    /// Latest rejected candidate error, cleared by the next valid poll.
    #[must_use]
    pub const fn last_failure(&self) -> Option<&ReloadFailure> {
        self.last_failure.as_ref()
    }

    /// Unknown/compatibility warning count from the active file.
    #[must_use]
    pub const fn warning_count(&self) -> usize {
        self.warning_count
    }

    /// Loads, resolves, and atomically publishes a changed configuration.
    ///
    /// Calls before [`RELOAD_POLL_INTERVAL`] elapses do not touch the filesystem.
    /// A malformed, missing-after-startup, or invalid replacement is retained as
    /// [`Self::last_failure`] while all readers continue using the prior snapshot.
    pub fn poll(&mut self, now_ms: u64) -> ReloadPoll {
        if let Some(last_poll_ms) = self.last_poll_ms {
            match last_poll_ms.checked_add(RELOAD_POLL_INTERVAL_MS) {
                Some(ready_at_ms) if now_ms >= ready_at_ms => {}
                Some(ready_at_ms) => return ReloadPoll::NotDue { ready_at_ms },
                None => {
                    return ReloadPoll::NotDue {
                        ready_at_ms: u64::MAX,
                    };
                }
            }
        }
        self.last_poll_ms = Some(now_ms);

        self.load_candidate()
    }

    /// Immediately attempts a complete replacement for an explicit reload.
    ///
    /// Unlike [`Self::poll`], this bypasses the automatic one-second floor. It
    /// still updates that floor so the event loop does not immediately repeat
    /// the same filesystem work.
    #[must_use]
    pub fn reload_now(&mut self, now_ms: u64) -> ReloadPoll {
        self.last_poll_ms = Some(now_ms);
        self.load_candidate()
    }

    fn load_candidate(&mut self) -> ReloadPoll {
        let document = match self.store.load() {
            Ok(document) => document,
            Err(error) => {
                return self.reject(ReloadFailure::Store(ReloadStoreFailure::from_store(error)));
            }
        };
        let Some(document) = document else {
            if self.observed_file {
                return self.reject(ReloadFailure::MissingReplacement);
            }
            self.last_failure = None;
            self.warning_count = 0;
            return ReloadPoll::Unchanged;
        };
        let candidate =
            match resolve_settings(Some(&document.settings), &self.environment, self.cli) {
                Ok(settings) => settings,
                Err(error) => {
                    return self.reject(ReloadFailure::Resolved(
                        ResolvedSettingsFailure::from_validation(error),
                    ));
                }
            };
        let previous = self.shared.snapshot();
        if previous.settings() == &candidate {
            self.observed_file = true;
            self.last_failure = None;
            self.warning_count = document.warnings.len();
            return ReloadPoll::Unchanged;
        }
        let Some(generation) = previous.generation().checked_add(1) else {
            return self.reject(ReloadFailure::GenerationExhausted);
        };
        let delta = ReloadDelta::between(previous.settings(), &candidate);
        self.shared.publish(generation, candidate);
        self.observed_file = true;
        self.last_failure = None;
        self.warning_count = document.warnings.len();
        ReloadPoll::Applied { generation, delta }
    }

    fn reject(&mut self, failure: ReloadFailure) -> ReloadPoll {
        self.last_failure = Some(failure);
        ReloadPoll::Rejected
    }
}

impl fmt::Debug for ConfigReloader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigReloader")
            .field("settings", &self.shared)
            .field("observed_file", &self.observed_file)
            .field("has_failure", &self.last_failure.is_some())
            .field("warning_count", &self.warning_count)
            .finish_non_exhaustive()
    }
}

fn selected_provider_changed(previous: &Settings, next: &Settings) -> bool {
    if previous.ai.provider != next.ai.provider {
        return true;
    }
    let Some(name) = previous.ai.provider.as_ref() else {
        return false;
    };
    previous.ai.providers.get(name) != next.ai.providers.get(name)
}

fn safe_validation_field(field: &str) -> String {
    if field.starts_with("ai.providers.") {
        for suffix in ["timeout_ms", "endpoint", "model", "inherited_from"] {
            if field.ends_with(&format!(".{suffix}")) {
                return format!("ai.providers.<provider>.{suffix}");
            }
        }
        if field.contains(".extra_request_body.") {
            return "ai.providers.<provider>.extra_request_body.<field>".to_owned();
        }
        return "ai.providers.<provider>".to_owned();
    }
    if field.len() <= 128
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        field.to_owned()
    } else {
        "<field>".to_owned()
    }
}

fn safe_validation_reason(problem: &ValidationProblem) -> String {
    match problem {
        ValidationProblem::UnsupportedSchema { found, supported } => {
            format!("schema version {found} is unsupported; expected {supported}")
        }
        ValidationProblem::Blank => "must not be blank".to_owned(),
        ValidationProblem::Duplicate { other_field } => format!("duplicates {other_field}"),
        ValidationProblem::OutOfRange {
            value,
            minimum,
            maximum,
        } => format!("{value} is outside the inclusive range {minimum}..={maximum}"),
        ValidationProblem::DurationOutOfRange {
            value,
            minimum,
            maximum,
        } => format!("{value:?} is outside the inclusive range {minimum:?}..={maximum:?}"),
        ValidationProblem::Missing => "is required".to_owned(),
        ValidationProblem::UnknownProvider { .. } => "references an unknown provider".to_owned(),
        ValidationProblem::Keybinding { problem } => problem.to_string(),
    }
}

fn bounded_safe_reason(reason: &str) -> String {
    let mut bounded = String::new();
    let mut truncated = false;
    for character in reason.chars() {
        let character = if character.is_control() {
            '?'
        } else {
            character
        };
        if bounded.len() + character.len_utf8() > MAX_RELOAD_REASON_BYTES.saturating_sub(3) {
            truncated = true;
            break;
        }
        bounded.push(character);
    }
    if truncated {
        bounded.push_str("...");
    }
    bounded
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use super::*;
    use crate::config::{Mode, Shell};

    fn current_config(mode: &str, extra: &str) -> String {
        format!("[core]\nversion = 2\nmode = '{mode}'\n{extra}\n")
    }

    fn start_with_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf, ConfigReloader) {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        fs::write(&path, contents).unwrap();
        let reloader = ConfigReloader::start(
            ConfigStore::new(&path),
            EnvironmentOverrides::default(),
            CliOverrides::default(),
        )
        .unwrap();
        (home, path, reloader)
    }

    #[test]
    fn polls_no_more_than_once_per_second_and_applies_within_the_budget() {
        let (_home, path, mut reloader) = start_with_file(&current_config("spec", ""));
        assert_eq!(reloader.poll(50), ReloadPoll::Unchanged);
        fs::write(&path, current_config("history", "")).unwrap();

        assert_eq!(
            reloader.poll(1_049),
            ReloadPoll::NotDue { ready_at_ms: 1_050 }
        );
        assert!(matches!(
            reloader.poll(1_050),
            ReloadPoll::Applied { generation: 1, .. }
        ));
        assert_eq!(
            reloader.shared_settings().snapshot().settings().core.mode,
            Mode::History
        );
    }

    #[test]
    fn a_reversed_clock_cannot_bypass_the_poll_floor() {
        let (_home, path, mut reloader) = start_with_file(&current_config("spec", ""));
        assert_eq!(reloader.poll(10_000), ReloadPoll::Unchanged);
        fs::write(&path, current_config("history", "")).unwrap();

        assert_eq!(
            reloader.poll(9_999),
            ReloadPoll::NotDue {
                ready_at_ms: 11_000
            }
        );
        assert_eq!(
            reloader.poll(10_999),
            ReloadPoll::NotDue {
                ready_at_ms: 11_000
            }
        );
        assert!(matches!(
            reloader.poll(11_000),
            ReloadPoll::Applied { generation: 1, .. }
        ));
    }

    #[test]
    fn an_unrepresentable_poll_deadline_never_fires_early() {
        let (_home, path, mut reloader) = start_with_file(&current_config("spec", ""));
        assert_eq!(reloader.poll(u64::MAX - 500), ReloadPoll::Unchanged);
        fs::write(&path, current_config("history", "")).unwrap();

        assert_eq!(
            reloader.poll(u64::MAX),
            ReloadPoll::NotDue {
                ready_at_ms: u64::MAX
            }
        );
        let snapshot = reloader.shared_settings().snapshot();
        assert_eq!(snapshot.generation(), 0);
        assert_eq!(snapshot.settings().core.mode, Mode::Spec);
    }

    #[test]
    fn invalid_edit_keeps_last_good_and_retains_the_exact_field_error() {
        let (_home, path, mut reloader) = start_with_file(&current_config("spec", ""));
        let shared = reloader.shared_settings();
        let before = shared.snapshot();
        fs::write(&path, current_config("history", "[ui]\nmax-height = 2")).unwrap();

        assert_eq!(reloader.poll(0), ReloadPoll::Rejected);
        let after = shared.snapshot();
        assert_eq!(after.generation(), before.generation());
        assert_eq!(after.settings(), before.settings());
        assert!(
            reloader
                .last_failure()
                .unwrap()
                .to_string()
                .contains("ui.max-height")
        );
        assert!(
            reloader
                .last_failure()
                .unwrap()
                .to_string()
                .contains("inclusive range 3..=50")
        );
    }

    #[test]
    fn selected_provider_changes_and_disablement_cancel_ai_authority() {
        let enabled = current_config(
            "spec",
            "[ai]\nenabled = true\nprovider = 'greendale'\n\
             [ai.providers.greendale]\nendpoint = 'https://example.invalid/v1'\nmodel = 'study-room'",
        );
        let (_home, path, mut reloader) = start_with_file(&enabled);
        fs::write(&path, enabled.replace("study-room'", "study-room-2'")).unwrap();

        let ReloadPoll::Applied { delta, .. } = reloader.poll(0) else {
            panic!("provider edit should apply")
        };
        assert!(delta.contains(ReloadChange::Ai));
        assert!(delta.contains(ReloadChange::CancelAi));

        fs::write(&path, current_config("spec", "")).unwrap();
        let ReloadPoll::Applied { delta, .. } = reloader.poll(1_000) else {
            panic!("AI disablement should apply")
        };
        assert!(delta.contains(ReloadChange::Ai));
        assert!(delta.contains(ReloadChange::CancelAi));
    }

    #[test]
    fn shell_change_is_deferred_to_the_next_session() {
        let (_home, path, mut reloader) =
            start_with_file(&current_config("spec", "shell = 'bash'"));
        fs::write(&path, current_config("spec", "shell = 'fish'")).unwrap();

        let ReloadPoll::Applied { delta, .. } = reloader.poll(0) else {
            panic!("shell edit should apply")
        };
        assert!(delta.contains(ReloadChange::Core));
        assert!(delta.contains(ReloadChange::ShellForNextSession));
        assert_eq!(
            reloader.shared_settings().snapshot().settings().core.shell,
            Some(Shell::Fish)
        );
    }

    #[test]
    fn creation_is_detected_but_disappearance_keeps_last_good() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let mut reloader = ConfigReloader::start(
            ConfigStore::new(&path),
            EnvironmentOverrides::default(),
            CliOverrides::default(),
        )
        .unwrap();
        fs::write(&path, current_config("history", "")).unwrap();
        assert!(matches!(
            reloader.poll(0),
            ReloadPoll::Applied { generation: 1, .. }
        ));

        fs::remove_file(&path).unwrap();
        assert_eq!(reloader.poll(1_000), ReloadPoll::Rejected);
        let snapshot = reloader.shared_settings().snapshot();
        assert_eq!(snapshot.generation(), 1);
        assert_eq!(snapshot.settings().core.mode, Mode::History);
        assert!(matches!(
            reloader.last_failure(),
            Some(ReloadFailure::MissingReplacement)
        ));
    }

    #[test]
    fn environment_and_cli_precedence_survive_every_reload() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        fs::write(&path, current_config("history", "shell = 'fish'")).unwrap();
        let environment = EnvironmentOverrides {
            core_mode: Some(Mode::Spec),
            ..EnvironmentOverrides::default()
        };
        let mut reloader = ConfigReloader::start(
            ConfigStore::new(&path),
            environment,
            CliOverrides {
                shell: Some(Shell::Bash),
                debug: None,
            },
        )
        .unwrap();
        fs::write(
            &path,
            current_config("history", "shell = 'zsh'\ndebug = true"),
        )
        .unwrap();

        assert!(matches!(
            reloader.poll(0),
            ReloadPoll::Applied { generation: 1, .. }
        ));
        let snapshot = reloader.shared_settings().snapshot();
        assert_eq!(snapshot.settings().core.mode, Mode::Spec);
        assert_eq!(snapshot.settings().core.shell, Some(Shell::Bash));
        assert!(snapshot.settings().core.debug);
    }

    #[test]
    fn readers_observe_only_complete_settings_generations() {
        let (_home, path, mut reloader) = start_with_file(&current_config(
            "spec",
            "[ui]\nmax-suggestions = 10\nmax-height = 10",
        ));
        let shared = reloader.shared_settings();
        let stop = Arc::new(AtomicBool::new(false));
        let readers = (0..4)
            .map(|_| {
                let shared = shared.clone();
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let snapshot = shared.snapshot();
                        let ui = snapshot.settings().ui;
                        assert!(
                            (ui.max_suggestions == 10 && ui.max_height == 10)
                                || (ui.max_suggestions == 40 && ui.max_height == 40)
                        );
                    }
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            &path,
            current_config("spec", "[ui]\nmax-suggestions = 40\nmax-height = 40"),
        )
        .unwrap();
        assert!(matches!(
            reloader.poll(0),
            ReloadPoll::Applied { generation: 1, .. }
        ));
        stop.store(true, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }
    }

    #[test]
    fn debug_output_never_includes_provider_or_file_contents() {
        let secret = current_config(
            "spec",
            "[ai.providers.greendale]\nendpoint = 'https://example.invalid/?token=Dean'\n\
             api-key = 'Troy-secret'\nmodel = 'Abed-model'",
        );
        let (_home, _path, reloader) = start_with_file(&secret);
        let debug = format!("{reloader:?} {:?}", reloader.shared_settings().snapshot());
        assert!(!debug.contains("Troy"));
        assert!(!debug.contains("Abed"));
        assert!(!debug.contains("Dean"));
        assert!(!debug.contains("greendale"));
    }

    #[test]
    fn incomplete_provider_override_keeps_local_settings_usable_and_redacted() {
        let enabled = current_config(
            "spec",
            "[ai]\nenabled = true\nprovider = 'greendale'\n\
             [ai.providers.greendale]\nendpoint = 'https://example.invalid/v1'\nmodel = 'study-room'",
        );
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        fs::write(&path, enabled).unwrap();
        let reloader = ConfigReloader::start(
            ConfigStore::new(path),
            EnvironmentOverrides {
                ai_provider: Some("Troy-secret-provider".to_owned()),
                ..EnvironmentOverrides::default()
            },
            CliOverrides::default(),
        )
        .unwrap();

        let snapshot = reloader.shared_settings().snapshot();
        let error = snapshot.settings().ai.readiness().unwrap_err();
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("Troy"));
        assert!(rendered.contains("no matching provider configuration"));
    }

    #[test]
    fn retained_store_validation_errors_are_bounded() {
        let (_home, path, mut reloader) = start_with_file(&current_config("spec", ""));
        let mut invalid = String::from("[core]\nversion = 2\n");
        for index in 0..1_000 {
            use std::fmt::Write as _;
            writeln!(invalid, "[ai.providers.provider_{index}]\ntimeout-ms = 0").unwrap();
        }
        fs::write(&path, invalid).unwrap();

        assert_eq!(reloader.poll(0), ReloadPoll::Rejected);
        let failure = reloader.last_failure().unwrap();
        let ReloadFailure::Store(ReloadStoreFailure::InvalidFields(problems)) = failure else {
            panic!("wanted bounded store validation failure")
        };
        assert_eq!(problems.problems().len(), 1);
        assert_eq!(problems.problem_count(), 1_000);
        assert!(problems.truncated());
        assert!(failure.to_string().len() < 4_096);
        assert_eq!(
            problems.problems()[0].field(),
            "ai.providers.<provider>.timeout_ms"
        );
        assert!(problems.problems()[0].reason().contains("1..=60000"));
    }

    #[cfg(unix)]
    #[test]
    fn inherited_reload_descriptor_parser_is_strict() {
        assert_eq!(
            parse_descriptor(OsStr::new("3")).map(NonZeroI32::get),
            Some(3)
        );
        for invalid in ["", "0", "2", "-1", "+3", "3x", "2147483648"] {
            assert_eq!(parse_descriptor(OsStr::new(invalid)), None, "{invalid}");
        }
    }
}
