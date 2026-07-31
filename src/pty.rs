//! Bounded PTY transport and supported-shell process lifecycle.
//!
//! The transport forwards bytes and process events only.  Completion, input
//! interpretation, foreground-program policy, and overlay rendering remain in
//! higher layers.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::socket::{
    getsockopt, setsockopt,
    sockopt::{RcvBuf, SndBuf},
};
use nix::sys::termios::{SpecialCharacterIndices, tcgetattr};
use nix::unistd::{AccessFlags, Pid, access, getpgrp, tcgetpgrp};
use pty_process::Size as BackendPtySize;
use pty_process::blocking::{Command as PtyCommand, open as open_pty};
use rustix::termios::{Winsize, tcgetwinsize, tcsetwinsize};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::integration::MAX_SYNC_EVENT_WIRE_BYTES;
use crate::shell_control::MAX_CONTROL_WIRE_BYTES;

/// Documented shell-selection environment override.
pub const ENV_SHELL_OVERRIDE: &str = "argmax_CORE_SHELL";
/// Shell-native integration hint set by generated hooks.
pub const ENV_ACTIVE_SHELL: &str = "ARGMAX_ACTIVE_SHELL";
/// Per-wrapper private session marker.
pub const ENV_PRIVATE_SESSION: &str = "ARGMAX_PRIVATE_SESSION";
/// Full-duplex integration descriptor used by hooks to emit shell events.
pub const ENV_EVENT_FD: &str = "ARGMAX_EVENT_FD";
/// Full-duplex integration control descriptor inherited by the shell.
pub const ENV_CONTROL_FD: &str = "ARGMAX_CONTROL_FD";
/// Hook ownership marker removed so the child shell can establish ownership.
pub const ENV_SESSION_OWNER_PID: &str = "ARGMAX_SESSION_OWNER_PID";

/// Largest input chunk accepted by one exact PTY write.
pub const MAX_PTY_INPUT: usize = 64 * 1024;
/// Largest output chunk returned by one PTY read.
pub const MAX_PTY_OUTPUT: usize = 64 * 1024;
/// Maximum inspected `PATH` size.
pub const MAX_SEARCH_PATH_BYTES: usize = 64 * 1024;
/// Maximum number of `PATH` entries inspected for a shell.
pub const MAX_SEARCH_PATH_ENTRIES: usize = 256;
/// Maximum private session-marker length.
pub const MAX_PRIVATE_SESSION_ID: usize = 128;
/// Largest integration event or replacement-control chunk.
pub const MAX_INTEGRATION_IO: usize = 64 * 1024;

/// Requested kernel capacity for each integration socket direction.
pub const INTEGRATION_SOCKET_BUFFER_BYTES: usize = 128 * 1024;

const MAX_INTEGRATION_WIRE_BYTES: usize = if MAX_SYNC_EVENT_WIRE_BYTES > MAX_CONTROL_WIRE_BYTES {
    MAX_SYNC_EVENT_WIRE_BYTES
} else {
    MAX_CONTROL_WIRE_BYTES
};
const MIN_INTEGRATION_SOCKET_CAPACITY: usize = MAX_INTEGRATION_WIRE_BYTES + 1;
const _: () = assert!(INTEGRATION_SOCKET_BUFFER_BYTES >= MIN_INTEGRATION_SOCKET_CAPACITY);

const CHILD_HANGUP_GRACE: Duration = Duration::from_millis(100);
const CHILD_KILL_GRACE: Duration = Duration::from_millis(100);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(5);
const READER_WAIT_POLL: Duration = Duration::from_millis(1);

// Clearing CLOEXEC is safe only during single-threaded wrapper startup. This
// lock serializes argmax PTY spawns; runtime workers must start afterwards.
static INHERITED_FD_SPAWN_LOCK: Mutex<()> = Mutex::new(());

const PTY_STARTUP_FRESH: u8 = 0;
const PTY_STARTUP_CLAIMED: u8 = 1;
const PTY_STARTUP_SEALED: u8 = 2;
static PTY_STARTUP_STATE: AtomicU8 = AtomicU8::new(PTY_STARTUP_FRESH);

/// Exact cell and pixel dimensions for a PTY.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
    /// Terminal rows.
    pub rows: u16,
    /// Terminal columns.
    pub cols: u16,
    /// Horizontal pixel extent, or zero when unavailable.
    pub pixel_width: u16,
    /// Vertical pixel extent, or zero when unavailable.
    pub pixel_height: u16,
}

/// A supported interactive shell.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ShellKind {
    /// Bourne Again Shell.
    Bash,
    /// Z shell.
    Zsh,
    /// Friendly Interactive Shell.
    Fish,
}

impl ShellKind {
    /// Stable executable and configuration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    fn from_basename(basename: &OsStr) -> Option<Self> {
        let bytes = basename.as_bytes();
        let bytes = bytes.strip_prefix(b"-").unwrap_or(bytes);
        match bytes {
            b"bash" => Some(Self::Bash),
            b"zsh" => Some(Self::Zsh),
            b"fish" => Some(Self::Fish),
            _ => None,
        }
    }
}

impl fmt::Display for ShellKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The precedence layer that selected an executable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellSource {
    /// `--shell`.
    CommandLine,
    /// Documented environment override.
    EnvironmentOverride,
    /// Resolved configuration.
    Configuration,
    /// Shell-native active-shell hint.
    ActiveShell,
    /// Conventional `SHELL` environment value.
    ShellEnvironment,
    /// Deterministic supported executable fallback.
    Fallback,
}

/// All shell-selection inputs, kept separate so precedence is testable.
#[derive(Clone, Default)]
pub struct ShellSelectionRequest {
    /// Explicit CLI shell.
    pub command_line: Option<ShellKind>,
    /// Parsed documented environment override.
    pub environment_override: Option<ShellKind>,
    /// Parsed configuration shell.
    pub configured: Option<ShellKind>,
    /// Active-shell executable or basename.
    pub active_shell: Option<OsString>,
    /// Conventional `SHELL` executable or basename.
    pub shell_environment: Option<OsString>,
    /// Search path snapshot.
    pub search_path: Option<OsString>,
}

impl ShellSelectionRequest {
    /// Captures discovery-only process environment after typed overrides have
    /// already been parsed by configuration code.
    #[must_use]
    pub fn from_process(
        command_line: Option<ShellKind>,
        environment_override: Option<ShellKind>,
        configured: Option<ShellKind>,
    ) -> Self {
        Self {
            command_line,
            environment_override,
            configured,
            active_shell: std::env::var_os(ENV_ACTIVE_SHELL),
            shell_environment: std::env::var_os("SHELL"),
            search_path: std::env::var_os("PATH"),
        }
    }
}

impl fmt::Debug for ShellSelectionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShellSelectionRequest")
            .field("command_line", &self.command_line)
            .field("environment_override", &self.environment_override)
            .field("configured", &self.configured)
            .field("has_active_shell", &self.active_shell.is_some())
            .field("has_shell_environment", &self.shell_environment.is_some())
            .field("has_search_path", &self.search_path.is_some())
            .finish()
    }
}

/// A validated supported executable.
#[derive(Clone, Eq, PartialEq)]
pub struct SelectedShell {
    kind: ShellKind,
    executable: PathBuf,
    source: ShellSource,
}

impl SelectedShell {
    /// Selected supported shell kind.
    #[must_use]
    pub const fn kind(&self) -> ShellKind {
        self.kind
    }

    /// Validated executable path.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Winning precedence layer.
    #[must_use]
    pub const fn source(&self) -> ShellSource {
        self.source
    }
}

impl fmt::Debug for SelectedShell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedShell")
            .field("kind", &self.kind)
            .field("executable", &"<validated path>")
            .field("source", &self.source)
            .finish()
    }
}

/// Shell selection failure without retained path or environment contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellSelectionError {
    /// A requested high-precedence shell is not executable.
    RequestedUnavailable {
        /// Requested shell.
        shell: ShellKind,
        /// Layer that requested it.
        source: ShellSource,
    },
    /// The search path exceeded its byte or entry bound.
    SearchPathTooLarge,
    /// No supported executable was available.
    NoSupportedShell,
}

impl fmt::Display for ShellSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestedUnavailable { shell, source } => {
                write!(
                    formatter,
                    "the {shell} shell selected by {source:?} is not executable"
                )
            }
            Self::SearchPathTooLarge => {
                formatter.write_str("the shell search path exceeds the safety limit")
            }
            Self::NoSupportedShell => {
                formatter.write_str("no executable Bash, Zsh, or Fish shell was found")
            }
        }
    }
}

impl Error for ShellSelectionError {}

/// Applies documented precedence and validates the winning executable.
///
/// Requested CLI, environment, and configuration values fail closed when the
/// requested shell is unavailable.  Discovery hints may be stale, so an
/// unsupported or unavailable active shell and `SHELL` value fall through to
/// the next discovery layer.
///
/// # Errors
///
/// Returns [`ShellSelectionError`] when a requested shell is unavailable, the
/// search path is unreasonably large, or no supported fallback exists.
pub fn select_shell(request: &ShellSelectionRequest) -> Result<SelectedShell, ShellSelectionError> {
    validate_search_path(request.search_path.as_deref())?;

    for (kind, source) in [
        (request.command_line, ShellSource::CommandLine),
        (
            request.environment_override,
            ShellSource::EnvironmentOverride,
        ),
        (request.configured, ShellSource::Configuration),
    ] {
        if let Some(kind) = kind {
            return resolve_kind(kind, source, request.search_path.as_deref()).ok_or(
                ShellSelectionError::RequestedUnavailable {
                    shell: kind,
                    source,
                },
            );
        }
    }

    for (candidate, source) in [
        (request.active_shell.as_deref(), ShellSource::ActiveShell),
        (
            request.shell_environment.as_deref(),
            ShellSource::ShellEnvironment,
        ),
    ] {
        if let Some(selected) = candidate.and_then(|candidate| {
            resolve_discovered(candidate, source, request.search_path.as_deref())
        }) {
            return Ok(selected);
        }
    }

    for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
        if let Some(selected) =
            resolve_kind(kind, ShellSource::Fallback, request.search_path.as_deref())
        {
            return Ok(selected);
        }
    }

    Err(ShellSelectionError::NoSupportedShell)
}

fn validate_search_path(search_path: Option<&OsStr>) -> Result<(), ShellSelectionError> {
    let Some(search_path) = search_path else {
        return Ok(());
    };
    if search_path.as_bytes().len() > MAX_SEARCH_PATH_BYTES
        || std::env::split_paths(search_path)
            .take(MAX_SEARCH_PATH_ENTRIES + 1)
            .count()
            > MAX_SEARCH_PATH_ENTRIES
    {
        return Err(ShellSelectionError::SearchPathTooLarge);
    }
    Ok(())
}

fn resolve_discovered(
    candidate: &OsStr,
    source: ShellSource,
    search_path: Option<&OsStr>,
) -> Option<SelectedShell> {
    let path = Path::new(candidate);
    let basename = path.file_name().unwrap_or(candidate);
    let kind = ShellKind::from_basename(basename)?;
    if path.components().count() > 1 || path.is_absolute() {
        let path = absolute_candidate(path)?;
        return executable_file(&path).then_some(SelectedShell {
            kind,
            executable: path,
            source,
        });
    }
    resolve_kind(kind, source, search_path)
}

fn resolve_kind(
    kind: ShellKind,
    source: ShellSource,
    search_path: Option<&OsStr>,
) -> Option<SelectedShell> {
    if let Some(search_path) = search_path {
        for directory in std::env::split_paths(search_path).take(MAX_SEARCH_PATH_ENTRIES) {
            let candidate = absolute_candidate(&directory.join(kind.as_str()))?;
            if executable_file(&candidate) {
                return Some(SelectedShell {
                    kind,
                    executable: candidate,
                    source,
                });
            }
        }
        return None;
    }

    for directory in ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        let candidate = Path::new(directory).join(kind.as_str());
        if executable_file(&candidate) {
            return Some(SelectedShell {
                kind,
                executable: candidate,
                source,
            });
        }
    }
    None
}

fn absolute_candidate(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::env::current_dir()
            .ok()
            .map(|directory| directory.join(path))
    }
}

fn executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
        && access(path, AccessFlags::X_OK).is_ok()
}

/// Validated private session marker that never reveals its value through Debug.
#[derive(Clone, Eq, PartialEq)]
pub struct PrivateSessionId(String);

impl PrivateSessionId {
    /// Validates a caller-generated session marker.
    ///
    /// Markers are identifiers, not authentication secrets.  Restricting their
    /// alphabet keeps inherited environment and shell hooks unambiguous.
    ///
    /// # Errors
    ///
    /// Returns [`PrivateSessionError`] for an empty, oversized, or malformed
    /// marker.
    pub fn new(value: impl Into<String>) -> Result<Self, PrivateSessionError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PrivateSessionError::Empty);
        }
        if value.len() > MAX_PRIVATE_SESSION_ID {
            return Err(PrivateSessionError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(PrivateSessionError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PrivateSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrivateSessionId(<redacted>)")
    }
}

/// Invalid private-session identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivateSessionError {
    /// Empty identifier.
    Empty,
    /// Identifier exceeded [`MAX_PRIVATE_SESSION_ID`].
    TooLong,
    /// Identifier contained a byte outside the conservative safe alphabet.
    InvalidCharacter,
}

impl fmt::Display for PrivateSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("private session identifier must not be empty"),
            Self::TooLong => formatter.write_str("private session identifier is too long"),
            Self::InvalidCharacter => {
                formatter.write_str("private session identifier contains an unsupported character")
            }
        }
    }
}

impl Error for PrivateSessionError {}

/// Why shell lifecycle events are or are not available on this transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrationEventCapability {
    /// One inherited, private, nonblocking Unix stream carries events to the
    /// wrapper and replacement controls to the shell.
    InheritedFullDuplexUnixStream,
}

/// How closing the master writer produces terminal EOF.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EofCapability {
    /// EOF bytes are queued nonblockingly and partial progress is reported.
    NonblockingTerminalEof,
}

/// Static and runtime transport capabilities that higher layers must honor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportCapabilities {
    /// Shell lifecycle event-channel capability.
    pub integration_events: IntegrationEventCapability,
    /// Master-writer EOF behavior.
    pub eof: EofCapability,
    /// Whether the backend exposes a foreground process group for signaling
    /// and full-screen gating.
    pub foreground_process_group: bool,
    /// Whether native Unix wait status is available for exact signal numbers.
    pub native_wait_status: bool,
}

/// Validated request to start one interactive shell.
pub struct PtySpawnRequest {
    /// Selected executable.
    pub shell: SelectedShell,
    /// Absolute working directory inherited by the shell.
    pub working_directory: PathBuf,
    /// Initial PTY dimensions.
    pub size: PtySize,
    /// Wrapper-private session marker.
    pub private_session: PrivateSessionId,
}

impl fmt::Debug for PtySpawnRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PtySpawnRequest")
            .field("shell", &self.shell)
            .field("working_directory", &"<validated path>")
            .field("size", &self.size)
            .field("private_session", &"<redacted>")
            .finish()
    }
}

/// Stable PTY lifecycle stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyStage {
    /// Request validation.
    Validate,
    /// Native PTY allocation.
    Open,
    /// Master output reader creation.
    Reader,
    /// Master input writer creation.
    Writer,
    /// Child shell creation.
    Spawn,
    /// Child status observation.
    Wait,
    /// PTY resize.
    Resize,
    /// Nonblocking descriptor setup.
    Nonblocking,
    /// Private shell-integration transport setup.
    Integration,
}

/// PTY transport failure without retained command, environment, or path text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyError {
    /// Invalid zero-sized terminal dimensions.
    InvalidSize,
    /// Working directory was not absolute.
    WorkingDirectoryNotAbsolute,
    /// Working directory was unavailable or not a directory.
    WorkingDirectoryUnavailable,
    /// Selected executable became unavailable before spawn.
    ShellUnavailable,
    /// A backend lifecycle stage failed.
    Backend {
        /// Failing stage.
        stage: PtyStage,
        /// Stable I/O category where the backend exposed one.
        io_kind: Option<io::ErrorKind>,
    },
    /// The master output reader was already taken.
    ReaderAlreadyTaken,
    /// The shell-integration stream was already taken.
    IntegrationAlreadyTaken,
    /// The startup token already produced its one shell session.
    StartupSpawnAlreadyUsed,
}

impl fmt::Display for PtyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize => formatter.write_str("PTY rows and columns must be nonzero"),
            Self::WorkingDirectoryNotAbsolute => {
                formatter.write_str("the shell working directory must be absolute")
            }
            Self::WorkingDirectoryUnavailable => {
                formatter.write_str("the shell working directory is unavailable")
            }
            Self::ShellUnavailable => {
                formatter.write_str("the selected shell is no longer executable")
            }
            Self::Backend { stage, io_kind } => {
                write!(formatter, "PTY {stage:?} failed")?;
                if let Some(kind) = io_kind {
                    write!(formatter, ": {kind}")?;
                }
                Ok(())
            }
            Self::ReaderAlreadyTaken => {
                formatter.write_str("the PTY output reader was already taken")
            }
            Self::IntegrationAlreadyTaken => {
                formatter.write_str("the shell-integration stream was already taken")
            }
            Self::StartupSpawnAlreadyUsed => {
                formatter.write_str("the startup token already spawned a PTY session")
            }
        }
    }
}

impl Error for PtyError {}

/// Progress from one bounded nonblocking PTY or integration write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyWrite {
    /// Every submitted byte was accepted.
    Complete,
    /// The backend would block after accepting the reported prefix. The caller
    /// may retry only the unaccepted suffix.
    Partial {
        /// Exact accepted prefix length.
        written: usize,
    },
}

impl PtyWrite {
    /// Returns the exact accepted prefix length for an input of `submitted`
    /// bytes.
    #[must_use]
    pub const fn written(self, submitted: usize) -> usize {
        match self {
            Self::Complete => submitted,
            Self::Partial { written } => written,
        }
    }
}

/// Exact bounded master-input write failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyWriteError {
    /// Submitted chunk exceeded [`MAX_PTY_INPUT`].
    TooLarge {
        /// Submitted bytes.
        actual: usize,
        /// Maximum accepted bytes.
        limit: usize,
    },
    /// Input was already closed.
    Closed,
    /// Backend write failed after accepting an exact prefix.
    Io {
        /// Stable I/O category.
        kind: io::ErrorKind,
        /// Exact accepted prefix length; retrying may begin at this offset.
        written: usize,
    },
}

impl fmt::Display for PtyWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, limit } => {
                write!(
                    formatter,
                    "PTY input has {actual} bytes; the limit is {limit}"
                )
            }
            Self::Closed => formatter.write_str("PTY input is closed"),
            Self::Io { kind, written } => {
                write!(
                    formatter,
                    "PTY input failed after {written} byte(s): {kind}"
                )
            }
        }
    }
}

impl Error for PtyWriteError {}

/// One bounded PTY output read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyRead {
    /// The exact number of bytes placed at the front of the caller's buffer.
    Bytes(usize),
    /// The slave side closed and no more output remains.
    Eof,
}

/// Progress while nonblockingly queueing terminal EOF.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyClose {
    /// Terminal EOF was fully queued, or input was already closed.
    Closed,
    /// The terminal queue would block. Calling [`PtySession::close_input`]
    /// again retries only the remaining EOF suffix.
    Pending {
        /// EOF bytes accepted so far.
        written: usize,
        /// Total EOF sequence length.
        total: usize,
    },
}

/// PTY output read failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyReadError {
    /// A nonempty caller-owned buffer is required.
    EmptyBuffer,
    /// Backend read failed.
    Io(io::ErrorKind),
}

impl fmt::Display for PtyReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBuffer => formatter.write_str("PTY output requires a nonempty buffer"),
            Self::Io(kind) => write!(formatter, "PTY output failed: {kind}"),
        }
    }
}

impl Error for PtyReadError {}

type SharedReader = Arc<Mutex<Option<File>>>;

/// Single-owner, bounded master output reader.
///
/// The handle does not independently own the master descriptor. Session
/// teardown closes the shared descriptor, after which reads return EOF.
pub struct PtyReader {
    reader: SharedReader,
}

impl PtyReader {
    /// Reads at most [`MAX_PTY_OUTPUT`] bytes without interpreting them.
    /// Linux reports a closed PTY slave as `EIO`; this backend normalizes that
    /// native terminal condition to [`PtyRead::Eof`].
    ///
    /// # Errors
    ///
    /// Returns [`PtyReadError::EmptyBuffer`] for an empty buffer or
    /// [`PtyReadError::Io`] for a backend failure.
    pub fn read_chunk(&mut self, buffer: &mut [u8]) -> Result<PtyRead, PtyReadError> {
        if buffer.is_empty() {
            return Err(PtyReadError::EmptyBuffer);
        }
        let limit = buffer.len().min(MAX_PTY_OUTPUT);
        loop {
            let result = {
                let mut reader = recover_lock(&self.reader);
                let Some(reader) = reader.as_mut() else {
                    return Ok(PtyRead::Eof);
                };
                reader.read(&mut buffer[..limit])
            };
            match result {
                Ok(0) => return Ok(PtyRead::Eof),
                Ok(read) => return Ok(PtyRead::Bytes(read)),
                Err(error) if pty_read_error_is_eof(&error) => return Ok(PtyRead::Eof),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(READER_WAIT_POLL);
                }
                Err(error) => return Err(PtyReadError::Io(error.kind())),
            }
        }
    }
}

fn pty_read_error_is_eof(error: &io::Error) -> bool {
    pty_read_errno_is_eof(error.raw_os_error(), cfg!(target_os = "linux"))
}

fn pty_read_errno_is_eof(raw_errno: Option<i32>, linux_semantics: bool) -> bool {
    linux_semantics && raw_errno == Some(Errno::EIO as i32)
}

impl fmt::Debug for PtyReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PtyReader")
            .field("reader", &"<redacted>")
            .field("maximum_chunk", &MAX_PTY_OUTPUT)
            .finish()
    }
}

type SharedIntegration = Arc<Mutex<Option<UnixStream>>>;

/// One nonblocking read from the shell-integration stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrationRead {
    /// Exact bytes were read into the caller buffer.
    Bytes(usize),
    /// No event is currently ready.
    Pending,
    /// Session teardown closed the channel.
    Eof,
}

/// Single-owner handle for shell events and replacement-control frames.
///
/// Shell hooks write bounded NUL-framed events to [`ENV_EVENT_FD`]. A future
/// replacement adapter may read bounded frames from [`ENV_CONTROL_FD`]; both
/// names identify the same full-duplex descriptor. The stream is nonblocking
/// in both processes, so a stalled peer cannot block a shell hook or wrapper.
pub struct PtyIntegration {
    stream: SharedIntegration,
}

impl PtyIntegration {
    /// Reads at most [`MAX_INTEGRATION_IO`] event bytes without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`PtyReadError::EmptyBuffer`] for an empty destination or a
    /// stable I/O category for a backend failure.
    pub fn read_events(&mut self, buffer: &mut [u8]) -> Result<IntegrationRead, PtyReadError> {
        if buffer.is_empty() {
            return Err(PtyReadError::EmptyBuffer);
        }
        let limit = buffer.len().min(MAX_INTEGRATION_IO);
        let mut stream = recover_lock(&self.stream);
        let Some(stream) = stream.as_mut() else {
            return Ok(IntegrationRead::Eof);
        };
        match stream.read(&mut buffer[..limit]) {
            Ok(0) => Ok(IntegrationRead::Eof),
            Ok(read) => Ok(IntegrationRead::Bytes(read)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(IntegrationRead::Pending),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                Ok(IntegrationRead::Pending)
            }
            Err(error) => Err(PtyReadError::Io(error.kind())),
        }
    }

    /// Writes one bounded replacement-control frame without blocking.
    ///
    /// A partial result exposes the exact accepted prefix; callers must retain
    /// and retry only the remaining suffix.
    ///
    /// # Errors
    ///
    /// Returns [`PtyWriteError`] for an oversized, closed, or failed write.
    pub fn write_control(&mut self, bytes: &[u8]) -> Result<PtyWrite, PtyWriteError> {
        if bytes.len() > MAX_INTEGRATION_IO {
            return Err(PtyWriteError::TooLarge {
                actual: bytes.len(),
                limit: MAX_INTEGRATION_IO,
            });
        }
        let mut stream = recover_lock(&self.stream);
        let Some(stream) = stream.as_mut() else {
            return Err(PtyWriteError::Closed);
        };
        write_nonblocking(stream, bytes)
    }
}

impl fmt::Debug for PtyIntegration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PtyIntegration")
            .field("stream", &"<redacted>")
            .field("maximum_chunk", &MAX_INTEGRATION_IO)
            .finish()
    }
}

/// One supported signal forwarded to a PTY foreground process group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardSignal {
    /// Interrupt (`SIGINT`).
    Interrupt,
    /// Quit (`SIGQUIT`).
    Quit,
    /// Termination request (`SIGTERM`).
    Terminate,
    /// Terminal disconnect (`SIGHUP`).
    Hangup,
    /// Job-control stop (`SIGTSTP`).
    Suspend,
    /// Job-control continuation (`SIGCONT`).
    Continue,
}

impl ForwardSignal {
    const fn native(self) -> Signal {
        match self {
            Self::Interrupt => Signal::SIGINT,
            Self::Quit => Signal::SIGQUIT,
            Self::Terminate => Signal::SIGTERM,
            Self::Hangup => Signal::SIGHUP,
            Self::Suspend => Signal::SIGTSTP,
            Self::Continue => Signal::SIGCONT,
        }
    }
}

/// Current PTY foreground ownership used for pass-through gating.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForegroundState {
    /// Backend does not expose a process group.
    Unavailable,
    /// A distinct foreground process group is active.
    Available {
        /// Foreground process-group identifier.
        process_group: i32,
        /// Whether the selected shell itself owns the terminal rather than a
        /// foreground child such as a TUI.
        shell_owns_terminal: bool,
    },
}

/// Result of one foreground signal-forward attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalDelivery {
    /// Signal reached the foreground process group.
    Delivered,
    /// Backend could not report a foreground group.
    NoForegroundProcessGroup,
    /// Safety check refused to signal the wrapper's own process group.
    RefusedWrapperProcessGroup,
}

/// Foreground signal delivery failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalError {
    /// Stable OS error category.
    pub io_kind: io::ErrorKind,
}

impl fmt::Display for SignalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "foreground signal delivery failed: {}",
            self.io_kind
        )
    }
}

impl Error for SignalError {}

/// Structured child termination with exact Unix signal numbers when available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildExit {
    /// Child called `_exit` or returned normally.
    Exited(i32),
    /// Child terminated because of a Unix signal.
    Signaled(i32),
    /// A non-native backend returned only a portable numeric code.
    PortableCode(u32),
    /// A non-native backend reported a signal without its numeric identity.
    PortableSignal,
    /// Another owner reaped the child, so no exit status or signal remains.
    ExternallyReaped,
}

impl ChildExit {
    /// Whether the child exited successfully.
    #[must_use]
    pub const fn success(self) -> bool {
        matches!(self, Self::Exited(0) | Self::PortableCode(0))
    }

    /// Conventional wrapper exit code, when representable.
    #[must_use]
    pub fn wrapper_code(self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(code),
            Self::Signaled(signal) => 128_i32.checked_add(signal),
            Self::PortableCode(code) => i32::try_from(code).ok(),
            Self::PortableSignal | Self::ExternallyReaped => None,
        }
    }
}

/// Failure to acquire the process-wide, startup-only PTY spawn capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyStartupError {
    /// Startup is already being configured by another owner.
    AlreadyClaimed,
    /// Startup was sealed before runtime workers were allowed to begin.
    Sealed,
}

impl fmt::Display for PtyStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyClaimed => formatter.write_str("PTY startup is already claimed"),
            Self::Sealed => formatter.write_str("PTY startup has already been sealed"),
        }
    }
}

impl Error for PtyStartupError {}

/// Process-wide capability for the single-threaded PTY spawn phase.
///
/// Acquire this on the main thread before creating signal, provider, or runtime
/// workers. The token is deliberately neither `Send` nor `Sync`, permits only
/// one successful shell spawn, and permanently seals the process-wide spawn
/// path after that spawn, when [`Self::seal`] runs, or on `Drop`. This makes the
/// temporary CLOEXEC inheritance window unavailable to runtime respawn paths.
pub struct PtyStartup {
    spawned: bool,
    active: bool,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl PtyStartup {
    /// Claims the one process-wide startup phase.
    ///
    /// # Errors
    ///
    /// Returns [`PtyStartupError::AlreadyClaimed`] while another token exists,
    /// or [`PtyStartupError::Sealed`] after any token has been sealed or
    /// dropped.
    pub fn acquire() -> Result<Self, PtyStartupError> {
        match PTY_STARTUP_STATE.compare_exchange(
            PTY_STARTUP_FRESH,
            PTY_STARTUP_CLAIMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(Self {
                spawned: false,
                active: true,
                not_send_or_sync: PhantomData,
            }),
            Err(PTY_STARTUP_CLAIMED) => Err(PtyStartupError::AlreadyClaimed),
            Err(_) => Err(PtyStartupError::Sealed),
        }
    }

    /// Starts the one interactive shell permitted during startup.
    ///
    /// Failed validation or spawn attempts may be retried before sealing. A
    /// successful call consumes this token's spawn allowance and seals the
    /// process-wide startup phase before it returns.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] for an invalid request, duplicate successful spawn,
    /// or backend failure.
    pub fn spawn(&mut self, request: &PtySpawnRequest) -> Result<PtySession, PtyError> {
        if self.spawned {
            return Err(PtyError::StartupSpawnAlreadyUsed);
        }
        let session = PtySession::spawn_during_startup(request)?;
        self.spawned = true;
        self.seal_inner();
        Ok(session)
    }

    /// Permanently closes the process-wide spawn phase before workers start.
    pub fn seal(mut self) {
        self.seal_inner();
    }

    fn seal_inner(&mut self) {
        if self.active {
            PTY_STARTUP_STATE.store(PTY_STARTUP_SEALED, Ordering::Release);
            self.active = false;
        }
    }
}

impl fmt::Debug for PtyStartup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PtyStartup")
            .field("spawned", &self.spawned)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl Drop for PtyStartup {
    fn drop(&mut self) {
        self.seal_inner();
    }
}

/// One interactive child shell and its byte-transparent PTY handles.
pub struct PtySession {
    master: Option<File>,
    reader: SharedReader,
    reader_available: bool,
    writer: Option<File>,
    eof_progress: Option<EofProgress>,
    child: process::Child,
    integration: SharedIntegration,
    integration_available: bool,
    shell_pid: Option<u32>,
    cached_exit: Option<ChildExit>,
    signal_authority: bool,
    capabilities: TransportCapabilities,
}

#[derive(Clone, Copy)]
struct EofProgress {
    bytes: [u8; 2],
    length: usize,
    written: usize,
}

impl PtySession {
    /// Opens a native PTY and starts the selected shell interactively during
    /// the startup-only phase.
    ///
    /// The child receives the process environment, selected `SHELL`, working
    /// directory, terminal size, controlling terminal, and only the required
    /// private integration markers. One nonblocking full-duplex Unix stream is
    /// inherited for shell events and replacement controls. Descriptor
    /// inheritance is established during serialized, single-threaded wrapper
    /// startup before provider or runtime worker threads are started.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] for invalid inputs or any allocation, handle, or
    /// child-spawn failure.
    fn spawn_during_startup(request: &PtySpawnRequest) -> Result<Self, PtyError> {
        validate_spawn_request(request)?;
        let command = configure_shell_command(
            PtyCommand::new(request.shell.executable()).arg("-i"),
            request.shell.kind(),
            request.shell.executable(),
            &request.working_directory,
            &request.private_session,
        );
        Self::spawn_command(command, request.size)
    }

    fn spawn_command(command: PtyCommand, size: PtySize) -> Result<Self, PtyError> {
        let (master_pty, slave_pty) = open_pty().map_err(|_| PtyError::Backend {
            stage: PtyStage::Open,
            io_kind: None,
        })?;
        master_pty
            .resize(backend_size(size))
            .map_err(|_| PtyError::Backend {
                stage: PtyStage::Resize,
                io_kind: None,
            })?;

        let master = File::from(std::os::fd::OwnedFd::from(master_pty));
        set_nonblocking(&master).map_err(|kind| PtyError::Backend {
            stage: PtyStage::Nonblocking,
            io_kind: Some(kind),
        })?;
        let reader = master.try_clone().map_err(|error| PtyError::Backend {
            stage: PtyStage::Reader,
            io_kind: Some(error.kind()),
        })?;
        let writer = master.try_clone().map_err(|error| PtyError::Backend {
            stage: PtyStage::Writer,
            io_kind: Some(error.kind()),
        })?;

        let (parent_integration, child_integration) =
            integration_stream_pair().map_err(|error| PtyError::Backend {
                stage: PtyStage::Integration,
                io_kind: Some(error.kind()),
            })?;
        parent_integration
            .set_nonblocking(true)
            .map_err(|error| PtyError::Backend {
                stage: PtyStage::Integration,
                io_kind: Some(error.kind()),
            })?;
        child_integration
            .set_nonblocking(true)
            .map_err(|error| PtyError::Backend {
                stage: PtyStage::Integration,
                io_kind: Some(error.kind()),
            })?;

        let child_fd = child_integration.as_raw_fd();
        let command = command
            .env(ENV_EVENT_FD, child_fd.to_string())
            .env(ENV_CONTROL_FD, child_fd.to_string())
            .env_remove(ENV_SESSION_OWNER_PID);
        let inherited_flags = fd_flags(child_fd).map_err(|kind| PtyError::Backend {
            stage: PtyStage::Integration,
            io_kind: Some(kind),
        })?;
        let spawn_guard = recover_lock(&INHERITED_FD_SPAWN_LOCK);
        set_fd_flags(child_fd, inherited_flags & !FdFlag::FD_CLOEXEC).map_err(|kind| {
            PtyError::Backend {
                stage: PtyStage::Integration,
                io_kind: Some(kind),
            }
        })?;
        let spawn_result = command.spawn(slave_pty);
        let restore_result = set_fd_flags(child_fd, inherited_flags);
        drop(spawn_guard);

        let mut child = spawn_result.map_err(|_| PtyError::Backend {
            stage: PtyStage::Spawn,
            io_kind: None,
        })?;
        if let Err(kind) = restore_result {
            let _ = child.kill();
            let _ = child.try_wait();
            return Err(PtyError::Backend {
                stage: PtyStage::Integration,
                io_kind: Some(kind),
            });
        }
        drop(child_integration);

        let shell_pid = Some(child.id());
        Ok(Self {
            master: Some(master),
            reader: Arc::new(Mutex::new(Some(reader))),
            reader_available: true,
            writer: Some(writer),
            eof_progress: None,
            child,
            integration: Arc::new(Mutex::new(Some(parent_integration))),
            integration_available: true,
            shell_pid,
            cached_exit: None,
            signal_authority: true,
            capabilities: TransportCapabilities {
                integration_events: IntegrationEventCapability::InheritedFullDuplexUnixStream,
                eof: EofCapability::NonblockingTerminalEof,
                foreground_process_group: true,
                native_wait_status: true,
            },
        })
    }

    /// Truthful backend capabilities for runtime policy decisions.
    #[must_use]
    pub const fn capabilities(&self) -> TransportCapabilities {
        self.capabilities
    }

    /// Takes the only output-reader handle.
    ///
    /// Session teardown retains authority to close its underlying descriptor,
    /// so keeping this handle cannot keep a PTY master alive.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::ReaderAlreadyTaken`] after the first call.
    pub fn take_reader(&mut self) -> Result<PtyReader, PtyError> {
        if !self.reader_available {
            return Err(PtyError::ReaderAlreadyTaken);
        }
        self.reader_available = false;
        Ok(PtyReader {
            reader: Arc::clone(&self.reader),
        })
    }

    /// Takes the only full-duplex shell-integration handle.
    ///
    /// Session teardown closes the underlying stream even if this handle
    /// outlives the session.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::IntegrationAlreadyTaken`] after the first call.
    pub fn take_integration(&mut self) -> Result<PtyIntegration, PtyError> {
        if !self.integration_available {
            return Err(PtyError::IntegrationAlreadyTaken);
        }
        self.integration_available = false;
        Ok(PtyIntegration {
            stream: Arc::clone(&self.integration),
        })
    }

    /// Writes one bounded byte chunk to the nonblocking PTY master.
    ///
    /// The transport performs no newline, key, UTF-8, paste, or escape
    /// interpretation.
    ///
    /// # Errors
    ///
    /// Returns [`PtyWriteError`] for an oversized chunk, closed input, or
    /// backend failure. [`PtyWrite::Partial`] reports the exact accepted prefix
    /// so the caller can safely retry only the remaining suffix.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<PtyWrite, PtyWriteError> {
        if bytes.len() > MAX_PTY_INPUT {
            return Err(PtyWriteError::TooLarge {
                actual: bytes.len(),
                limit: MAX_PTY_INPUT,
            });
        }
        if self.eof_progress.is_some() {
            return Err(PtyWriteError::Closed);
        }
        let Some(writer) = self.writer.as_mut() else {
            return Err(PtyWriteError::Closed);
        };
        write_nonblocking(writer, bytes)
    }

    /// Nonblockingly queues terminal EOF and closes the input writer once.
    ///
    /// A full terminal queue returns [`PtyClose::Pending`] immediately. Calling
    /// this method again resumes at the exact unaccepted EOF suffix.
    ///
    /// # Errors
    ///
    /// Returns a stable backend category and exact cumulative progress when
    /// terminal inspection or the EOF write fails.
    pub fn close_input(&mut self) -> Result<PtyClose, PtyWriteError> {
        let Some(_) = self.writer else {
            return Ok(PtyClose::Closed);
        };
        if self.eof_progress.is_none() {
            let Some(master) = self.master.as_ref() else {
                self.writer.take();
                return Ok(PtyClose::Closed);
            };
            let terminal = match tcgetattr(master) {
                Ok(terminal) => terminal,
                Err(errno) => {
                    self.writer.take();
                    return Err(PtyWriteError::Io {
                        kind: io::Error::from(errno).kind(),
                        written: 0,
                    });
                }
            };
            let eof = terminal.control_chars[SpecialCharacterIndices::VEOF as usize];
            let length = usize::from(eof != 0) * 2;
            self.eof_progress = Some(EofProgress {
                bytes: [b'\n', eof],
                length,
                written: 0,
            });
        }

        let Some(progress) = self.eof_progress.as_mut() else {
            self.writer.take();
            return Ok(PtyClose::Closed);
        };
        if progress.written == progress.length {
            self.writer.take();
            return Ok(PtyClose::Closed);
        }
        let Some(writer) = self.writer.as_mut() else {
            return Ok(PtyClose::Closed);
        };
        match write_nonblocking(writer, &progress.bytes[progress.written..progress.length]) {
            Ok(PtyWrite::Complete) => {
                progress.written = progress.length;
                self.writer.take();
                Ok(PtyClose::Closed)
            }
            Ok(PtyWrite::Partial { written }) => {
                progress.written += written;
                Ok(PtyClose::Pending {
                    written: progress.written,
                    total: progress.length,
                })
            }
            Err(PtyWriteError::Io { kind, written }) => {
                progress.written += written;
                Err(PtyWriteError::Io {
                    kind,
                    written: progress.written,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the current PTY size.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] when the backend cannot query its size.
    pub fn size(&self) -> Result<PtySize, PtyError> {
        let master = self.master.as_ref().ok_or(PtyError::Backend {
            stage: PtyStage::Resize,
            io_kind: None,
        })?;
        let size = tcgetwinsize(master).map_err(|error| PtyError::Backend {
            stage: PtyStage::Resize,
            io_kind: Some(io::Error::from(error).kind()),
        })?;
        Ok(PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }

    /// Applies new nonzero dimensions to the PTY and child terminal.
    ///
    /// The native backend also causes the terminal driver to notify the child
    /// of the resize.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::InvalidSize`] for zero rows/columns or a backend
    /// error when resize fails.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        validate_size(size)?;
        let master = self.master.as_ref().ok_or(PtyError::Backend {
            stage: PtyStage::Resize,
            io_kind: None,
        })?;
        tcsetwinsize(
            master,
            Winsize {
                ws_row: size.rows,
                ws_col: size.cols,
                ws_xpixel: size.pixel_width,
                ws_ypixel: size.pixel_height,
            },
        )
        .map_err(|error| PtyError::Backend {
            stage: PtyStage::Resize,
            io_kind: Some(io::Error::from(error).kind()),
        })
    }

    /// Reports whether the shell or one of its foreground jobs owns the PTY.
    #[must_use]
    pub fn foreground_state(&self) -> ForegroundState {
        let Some(process_group) = self.current_foreground_group() else {
            return ForegroundState::Unavailable;
        };
        let shell_owns_terminal =
            self.shell_pid.and_then(|pid| i32::try_from(pid).ok()) == Some(process_group);
        ForegroundState::Available {
            process_group,
            shell_owns_terminal,
        }
    }

    /// Sends a supported signal to the PTY's current foreground process group.
    ///
    /// Looking up the group for every delivery handles shells handing the
    /// terminal to and reclaiming it from jobs.  The wrapper's own process
    /// group is never signaled through this API.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when the OS rejects delivery.
    pub fn forward_signal(&self, signal: ForwardSignal) -> Result<SignalDelivery, SignalError> {
        if !self.signal_authority {
            return Ok(SignalDelivery::NoForegroundProcessGroup);
        }
        let Some(process_group) = self.current_foreground_group() else {
            return Ok(SignalDelivery::NoForegroundProcessGroup);
        };
        if process_group == getpgrp().as_raw() {
            return Ok(SignalDelivery::RefusedWrapperProcessGroup);
        }
        killpg(Pid::from_raw(process_group), signal.native())
            .map(|()| SignalDelivery::Delivered)
            .map_err(|errno| SignalError {
                io_kind: io::Error::from(errno).kind(),
            })
    }

    /// Polls child status without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] if the backend cannot query child status.
    pub fn try_wait(&mut self) -> Result<Option<ChildExit>, PtyError> {
        if let Some(exit) = self.cached_exit {
            return Ok(Some(exit));
        }

        let exit = match self.child.try_wait() {
            Ok(status) => status.map(child_exit_from_native),
            Err(error) if is_echild(&error) => Some(ChildExit::ExternallyReaped),
            Err(error) => return Err(wait_error(error.kind())),
        };
        if let Some(exit) = exit {
            self.cached_exit = Some(exit);
            self.shell_pid = None;
            self.signal_authority = false;
        }
        Ok(exit)
    }

    /// Waits for the child shell and preserves exact native exit/signal status.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] if the backend wait operation fails.
    pub fn wait(&mut self) -> Result<ChildExit, PtyError> {
        if let Some(exit) = self.cached_exit {
            return Ok(exit);
        }

        let exit = match self.child.wait() {
            Ok(status) => child_exit_from_native(status),
            Err(error) if is_echild(&error) => ChildExit::ExternallyReaped,
            Err(error) => return Err(wait_error(error.kind())),
        };
        self.cached_exit = Some(exit);
        self.shell_pid = None;
        self.signal_authority = false;
        Ok(exit)
    }

    fn current_foreground_group(&self) -> Option<i32> {
        let master = self.master.as_ref()?;
        tcgetpgrp(master).ok().map(Pid::as_raw)
    }

    fn shutdown_process_groups(&self) -> [Option<Pid>; 2] {
        if !self.signal_authority {
            return [None, None];
        }
        let wrapper_group = getpgrp();
        let foreground_group = self
            .current_foreground_group()
            .filter(|process_group| *process_group > 0)
            .map(Pid::from_raw);
        let shell = self
            .shell_pid
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
            .map(Pid::from_raw);

        [
            foreground_group.filter(|group| *group != wrapper_group),
            shell.filter(|group| *group != wrapper_group && Some(*group) != foreground_group),
        ]
    }

    fn signal_shutdown_targets(&self, process_groups: &[Option<Pid>; 2], signal: Signal) {
        for process_group in process_groups.iter().flatten() {
            let _ = killpg(*process_group, signal);
        }

        // The native PTY child is a session and process-group leader. Signal
        // its PID as a final fallback in case the terminal no longer reports a
        // foreground group or the group has already been dismantled.
        let running_shell = if self.signal_authority {
            self.shell_pid
                .and_then(|pid| i32::try_from(pid).ok())
                .filter(|pid| *pid > 0)
                .map(Pid::from_raw)
        } else {
            None
        };
        if let Some(shell) = running_shell {
            let _ = kill(shell, signal);
        }
    }

    fn shutdown_process_groups_alive(process_groups: &[Option<Pid>; 2]) -> bool {
        process_groups.iter().flatten().any(|process_group| {
            !matches!(killpg(*process_group, None), Err(nix::errno::Errno::ESRCH))
        })
    }

    fn wait_until(&mut self, deadline: Instant, process_groups: &[Option<Pid>; 2]) -> bool {
        loop {
            let child_reaped = self.try_wait().is_ok_and(|exit| exit.is_some());
            if child_reaped && !Self::shutdown_process_groups_alive(process_groups) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            thread::sleep(CHILD_WAIT_POLL.min(deadline.duration_since(now)));
        }
    }

    fn shutdown_on_drop(&mut self) {
        let child_reaped = self.try_wait().is_ok_and(|exit| exit.is_some());
        let process_groups = self.shutdown_process_groups();
        if child_reaped {
            // Once the direct child has been reaped, numeric PID/PGID authority
            // is gone. Deliberately terminal-detached daemons are outside the
            // wrapper's signal authority; closing every wrapper-owned transport
            // is the teardown guarantee.
            self.close_transport();
            return;
        }

        self.signal_shutdown_targets(&process_groups, Signal::SIGHUP);
        if !self.wait_until(Instant::now() + CHILD_HANGUP_GRACE, &process_groups) {
            self.signal_shutdown_targets(&process_groups, Signal::SIGKILL);
            let _ = self.wait_until(Instant::now() + CHILD_KILL_GRACE, &process_groups);
        }

        // Never call blocking wait or emit EOF from Drop. Owned descriptors
        // close directly and the final status poll is nonblocking.
        let _ = self.try_wait();
        self.close_transport();
    }

    fn close_transport(&mut self) {
        self.writer.take();
        self.eof_progress = None;
        recover_lock(&self.reader).take();
        recover_lock(&self.integration).take();
        self.master.take();
    }
}

impl fmt::Debug for PtySession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PtySession")
            .field("reader_available", &self.reader_available)
            .field("writer_open", &self.writer.is_some())
            .field("child_running", &self.cached_exit.is_none())
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.shutdown_on_drop();
    }
}

fn wait_error(kind: io::ErrorKind) -> PtyError {
    PtyError::Backend {
        stage: PtyStage::Wait,
        io_kind: Some(kind),
    }
}

fn child_exit_from_native(status: process::ExitStatus) -> ChildExit {
    if let Some(code) = status.code() {
        ChildExit::Exited(code)
    } else if let Some(signal) = status.signal() {
        ChildExit::Signaled(signal)
    } else {
        ChildExit::PortableSignal
    }
}

fn validate_spawn_request(request: &PtySpawnRequest) -> Result<(), PtyError> {
    validate_size(request.size)?;
    if !request.working_directory.is_absolute() {
        return Err(PtyError::WorkingDirectoryNotAbsolute);
    }
    if !fs::metadata(&request.working_directory).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(PtyError::WorkingDirectoryUnavailable);
    }
    if !executable_file(request.shell.executable()) {
        return Err(PtyError::ShellUnavailable);
    }
    Ok(())
}

fn validate_size(size: PtySize) -> Result<(), PtyError> {
    if size.rows == 0 || size.cols == 0 {
        Err(PtyError::InvalidSize)
    } else {
        Ok(())
    }
}

fn configure_shell_command(
    command: PtyCommand,
    shell: ShellKind,
    executable: &Path,
    working_directory: &Path,
    private_session: &PrivateSessionId,
) -> PtyCommand {
    command
        .current_dir(working_directory)
        .env("SHELL", executable)
        .env(ENV_ACTIVE_SHELL, shell.as_str())
        .env(ENV_PRIVATE_SESSION, private_session.as_str())
        .env_remove(ENV_EVENT_FD)
        .env_remove(ENV_CONTROL_FD)
        .env_remove(ENV_SESSION_OWNER_PID)
}

fn backend_size(size: PtySize) -> BackendPtySize {
    BackendPtySize::new_with_pixel(size.rows, size.cols, size.pixel_width, size.pixel_height)
}

fn fd_flags(fd: RawFd) -> Result<FdFlag, io::ErrorKind> {
    fcntl(fd, FcntlArg::F_GETFD)
        .map(FdFlag::from_bits_truncate)
        .map_err(|errno| io::Error::from(errno).kind())
}

fn set_fd_flags(fd: RawFd, flags: FdFlag) -> Result<(), io::ErrorKind> {
    fcntl(fd, FcntlArg::F_SETFD(flags))
        .map(|_| ())
        .map_err(|errno| io::Error::from(errno).kind())
}

fn set_nonblocking(file: &File) -> Result<(), io::ErrorKind> {
    let fd = file.as_raw_fd();
    let flags = fcntl(fd, FcntlArg::F_GETFL)
        .map(OFlag::from_bits_truncate)
        .map_err(|errno| io::Error::from(errno).kind())?;
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
        .map(|_| ())
        .map_err(|errno| io::Error::from(errno).kind())
}

fn integration_stream_pair() -> io::Result<(UnixStream, UnixStream)> {
    let (parent, child) = UnixStream::pair()?;
    configure_integration_socket(&parent)?;
    configure_integration_socket(&child)?;
    Ok((parent, child))
}

fn configure_integration_socket(stream: &UnixStream) -> io::Result<()> {
    setsockopt(stream, SndBuf, &INTEGRATION_SOCKET_BUFFER_BYTES).map_err(io::Error::from)?;
    setsockopt(stream, RcvBuf, &INTEGRATION_SOCKET_BUFFER_BYTES).map_err(io::Error::from)?;

    let send_capacity = getsockopt(stream, SndBuf).map_err(io::Error::from)?;
    let receive_capacity = getsockopt(stream, RcvBuf).map_err(io::Error::from)?;
    if send_capacity < MIN_INTEGRATION_SOCKET_CAPACITY
        || receive_capacity < MIN_INTEGRATION_SOCKET_CAPACITY
    {
        return Err(io::Error::other(
            "integration socket capacity is below the protocol maximum",
        ));
    }
    Ok(())
}

fn write_nonblocking(writer: &mut impl Write, bytes: &[u8]) -> Result<PtyWrite, PtyWriteError> {
    match writer.write(bytes) {
        Ok(written) if written == bytes.len() => Ok(PtyWrite::Complete),
        Ok(written) => Ok(PtyWrite::Partial { written }),
        // An interrupted write consumed nothing, so it is retryable exactly
        // like a blocked one. Reporting it as an I/O failure would let a signal
        // arriving mid-write end the session.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(PtyWrite::Partial { written: 0 })
        }
        Err(error) => Err(PtyWriteError::Io {
            kind: error.kind(),
            written: 0,
        }),
    }
}

fn is_echild(error: &io::Error) -> bool {
    error.raw_os_error() == Some(Errno::ECHILD as i32)
}

fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Signals observed without a dedicated or detached thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalEvent {
    /// Parent terminal resized.
    Resize,
    /// Wrapper received `SIGINT` externally.
    Interrupt,
    /// Wrapper received `SIGQUIT` externally.
    Quit,
    /// Wrapper received `SIGTERM`.
    Terminate,
    /// Wrapper terminal/session disconnected.
    Hangup,
    /// Wrapper received a job-control stop request.
    Suspend,
    /// Wrapper was continued after job-control suspension.
    Continue,
}

/// Signal registration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalRegistrationError {
    /// Stable OS error category.
    pub io_kind: io::ErrorKind,
}

impl fmt::Display for SignalRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not register terminal signal handling: {}",
            self.io_kind
        )
    }
}

impl Error for SignalRegistrationError {}

/// Nonblocking, coalescing signal source for a caller-owned event loop.
///
/// The source does not create a thread.  Signal-hook coalesces duplicate
/// occurrences; at most seven distinct events are returned per drain.
pub struct SignalEvents {
    signals: Signals,
}

impl SignalEvents {
    /// Registers resize, external interrupt/quit, termination, hangup, stop,
    /// and continuation.
    ///
    /// Keyboard control bytes in parent raw mode should continue through the
    /// PTY; this source handles signals delivered to the wrapper process by the
    /// OS or another process.
    ///
    /// # Errors
    ///
    /// Returns [`SignalRegistrationError`] if signal-hook cannot establish its
    /// nonblocking delivery pipe.
    pub fn new() -> Result<Self, SignalRegistrationError> {
        Signals::new([
            SIGWINCH,
            SIGINT,
            SIGQUIT,
            SIGTERM,
            SIGHUP,
            signal_hook::consts::signal::SIGTSTP,
            signal_hook::consts::signal::SIGCONT,
        ])
        .map(|signals| Self { signals })
        .map_err(|error| SignalRegistrationError {
            io_kind: error.kind(),
        })
    }

    /// Drains all currently pending distinct events into a fixed-size buffer.
    ///
    /// The returned count identifies the initialized prefix.  No allocation or
    /// blocking occurs.
    pub fn drain_pending(&mut self, output: &mut [Option<SignalEvent>; 7]) -> usize {
        output.fill(None);
        let mut count = 0;
        for signal in self.signals.pending() {
            let event = match signal {
                SIGWINCH => SignalEvent::Resize,
                SIGINT => SignalEvent::Interrupt,
                SIGQUIT => SignalEvent::Quit,
                SIGTERM => SignalEvent::Terminate,
                SIGHUP => SignalEvent::Hangup,
                signal_hook::consts::signal::SIGTSTP => SignalEvent::Suspend,
                signal_hook::consts::signal::SIGCONT => SignalEvent::Continue,
                _ => continue,
            };
            if let Some(slot) = output.get_mut(count) {
                *slot = Some(event);
                count += 1;
            }
        }
        count
    }
}

impl fmt::Debug for SignalEvents {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignalEvents")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use nix::errno::Errno;
    use nix::sys::wait::{WaitPidFlag, waitpid};

    use crate::integration::{MAX_SYNC_EVENT_CHARACTERS, Shell as IntegrationShell, init_script};
    use crate::shell_control::{
        ControlRequestId, MAX_CONTROL_BUFFER_BYTES, MAX_CONTROL_REQUEST_ID, ReplacementControl,
    };
    use crate::shell_events::{DecodedFrame, ShellEvent, ShellEventDecoder, StreamEpoch};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            for attempt in 0..32_u8 {
                let path = std::env::temp_dir().join(format!(
                    "argmax-{label}-{}-{timestamp}-{attempt}",
                    process::id()
                ));
                if fs::create_dir(&path).is_ok() {
                    return Self(path);
                }
            }
            panic!("could not create bounded test directory");
        }

        fn executable(&self, kind: ShellKind) -> PathBuf {
            let path = self.0.join(kind.as_str());
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&path, permissions).unwrap();
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn size(rows: u16, cols: u16) -> PtySize {
        PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    fn private_session() -> PrivateSessionId {
        PrivateSessionId::new("greendale-session-0001").unwrap()
    }

    fn spawn_test_command(program: &Path, arguments: &[&str]) -> PtySession {
        let command = PtyCommand::new(program)
            .args(arguments)
            .current_dir(std::env::current_dir().unwrap())
            .env_remove(ENV_EVENT_FD)
            .env_remove(ENV_CONTROL_FD)
            .env_remove(ENV_SESSION_OWNER_PID);
        PtySession::spawn_command(command, size(24, 80)).unwrap()
    }

    fn read_until_marker(reader: &mut PtyReader, marker: u8) {
        let mut buffer = [0_u8; 128];
        loop {
            match reader.read_chunk(&mut buffer).unwrap() {
                PtyRead::Bytes(read) if buffer[..read].contains(&marker) => return,
                PtyRead::Bytes(_) => {}
                PtyRead::Eof => panic!("PTY closed before readiness marker"),
            }
        }
    }

    fn read_integration_until(integration: &mut PtyIntegration, marker: u8, deadline: Instant) {
        let mut buffer = [0_u8; 128];
        loop {
            match integration.read_events(&mut buffer).unwrap() {
                IntegrationRead::Bytes(read) if buffer[..read].contains(&marker) => return,
                IntegrationRead::Bytes(_) | IntegrationRead::Pending => {}
                IntegrationRead::Eof => panic!("integration closed before readiness marker"),
            }
            assert!(Instant::now() < deadline, "integration event timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn read_integration_frame(integration: &mut PtyIntegration, deadline: Instant) -> Vec<u8> {
        let mut frame = Vec::new();
        let mut buffer = [0_u8; 128];
        loop {
            match integration.read_events(&mut buffer).unwrap() {
                IntegrationRead::Bytes(read) => {
                    if let Some(end) = buffer[..read].iter().position(|byte| *byte == 0) {
                        frame.extend_from_slice(&buffer[..end]);
                        return frame;
                    }
                    frame.extend_from_slice(&buffer[..read]);
                    assert!(frame.len() <= MAX_INTEGRATION_IO);
                }
                IntegrationRead::Pending => {}
                IntegrationRead::Eof => panic!("integration closed before complete frame"),
            }
            assert!(Instant::now() < deadline, "integration frame timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn read_socket_bytes(stream: &mut UnixStream, length: usize, deadline: Instant) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(length);
        let mut buffer = [0_u8; 4096];
        while bytes.len() < length {
            match stream.read(&mut buffer) {
                Ok(0) => panic!("integration socket closed after {} bytes", bytes.len()),
                Ok(read) => bytes.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => panic!("integration socket read failed: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "integration socket stalled after {} of {length} bytes",
                bytes.len()
            );
            thread::sleep(Duration::from_millis(1));
        }
        bytes
    }

    fn installed_control_shell(kind: ShellKind) -> Option<SelectedShell> {
        let search_path = std::env::var_os("PATH");
        let selected = resolve_kind(kind, ShellSource::Fallback, search_path.as_deref())?;
        if kind == ShellKind::Bash {
            let supported = process::Command::new(selected.executable())
                .args([
                    "-c",
                    "(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))",
                ])
                .status()
                .expect("inspect Bash version")
                .success();
            if !supported {
                return None;
            }
        }
        Some(selected)
    }

    fn wait_for_integration_ready(
        integration: &mut PtyIntegration,
        decoder: &mut ShellEventDecoder,
        deadline: Instant,
    ) {
        let mut capability = false;
        let mut prompt = false;
        let mut buffer = [0_u8; 4096];
        while !capability || !prompt {
            match integration.read_events(&mut buffer).unwrap() {
                IntegrationRead::Bytes(read) => decoder.push(&buffer[..read], |frame| {
                    if let DecodedFrame::Event(event) = frame {
                        match event.event() {
                            ShellEvent::Capability(_) => capability = true,
                            ShellEvent::PromptReady => prompt = true,
                            _ => {}
                        }
                    }
                }),
                IntegrationRead::Pending => {}
                IntegrationRead::Eof => panic!("shell integration closed before prompt readiness"),
            }
            assert!(
                Instant::now() < deadline,
                "shell integration did not become ready: capability={capability}, prompt={prompt}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_probe_snapshot(
        integration: &mut PtyIntegration,
        decoder: &mut ShellEventDecoder,
        nonce: u64,
        deadline: Instant,
    ) -> crate::shell_events::BufferSnapshot {
        let mut buffer = [0_u8; 4096];
        loop {
            let mut snapshot = None;
            match integration.read_events(&mut buffer).unwrap() {
                IntegrationRead::Bytes(read) => decoder.push(&buffer[..read], |frame| {
                    let DecodedFrame::Event(event) = frame else {
                        return;
                    };
                    let ShellEvent::Buffer(buffer) = event.event() else {
                        return;
                    };
                    if buffer
                        .probe_nonce()
                        .is_some_and(|value| value.get() == nonce)
                    {
                        snapshot = Some(buffer.clone());
                    }
                }),
                IntegrationRead::Pending => {}
                IntegrationRead::Eof => panic!("shell integration closed before probe response"),
            }
            if let Some(snapshot) = snapshot {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "shell integration probe response timed out"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn selection_precedence_and_debug_redact_paths() {
        let cli = TestDirectory::new("cli");
        let environment = TestDirectory::new("environment");
        cli.executable(ShellKind::Bash);
        environment.executable(ShellKind::Fish);
        let search_path = std::env::join_paths([&cli.0, &environment.0]).unwrap();
        let request = ShellSelectionRequest {
            command_line: Some(ShellKind::Bash),
            environment_override: Some(ShellKind::Fish),
            configured: Some(ShellKind::Zsh),
            active_shell: Some(OsString::from("zsh")),
            shell_environment: Some(OsString::from("fish")),
            search_path: Some(search_path),
        };

        let selected = select_shell(&request).unwrap();
        assert_eq!(selected.kind(), ShellKind::Bash);
        assert_eq!(selected.source(), ShellSource::CommandLine);
        assert_eq!(selected.executable(), cli.0.join("bash"));
        assert!(!format!("{request:?}").contains(cli.0.to_string_lossy().as_ref()));
        assert!(!format!("{selected:?}").contains(cli.0.to_string_lossy().as_ref()));
    }

    #[test]
    fn unavailable_explicit_selection_fails_instead_of_falling_through() {
        let fallback = TestDirectory::new("fallback");
        fallback.executable(ShellKind::Fish);
        let request = ShellSelectionRequest {
            command_line: Some(ShellKind::Zsh),
            search_path: Some(fallback.0.clone().into_os_string()),
            ..ShellSelectionRequest::default()
        };
        assert_eq!(
            select_shell(&request),
            Err(ShellSelectionError::RequestedUnavailable {
                shell: ShellKind::Zsh,
                source: ShellSource::CommandLine,
            })
        );
    }

    #[test]
    fn stale_discovery_falls_through_to_supported_shell() {
        let directory = TestDirectory::new("discovery");
        directory.executable(ShellKind::Fish);
        let request = ShellSelectionRequest {
            active_shell: Some(OsString::from("tcsh")),
            shell_environment: Some(OsString::from("/missing/zsh")),
            search_path: Some(directory.0.clone().into_os_string()),
            ..ShellSelectionRequest::default()
        };
        let selected = select_shell(&request).unwrap();
        assert_eq!(selected.kind(), ShellKind::Fish);
        assert_eq!(selected.source(), ShellSource::Fallback);
    }

    #[test]
    fn non_executable_requested_shell_is_rejected() {
        let directory = TestDirectory::new("permissions");
        let path = directory.executable(ShellKind::Bash);
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(directory.0.join("bash"), permissions).unwrap();
        let request = ShellSelectionRequest {
            configured: Some(ShellKind::Bash),
            search_path: Some(directory.0.clone().into_os_string()),
            ..ShellSelectionRequest::default()
        };
        assert!(matches!(
            select_shell(&request),
            Err(ShellSelectionError::RequestedUnavailable {
                shell: ShellKind::Bash,
                source: ShellSource::Configuration,
            })
        ));
    }

    #[test]
    fn search_path_is_bounded_before_filesystem_work() {
        let request = ShellSelectionRequest {
            search_path: Some(OsString::from("x:".repeat(MAX_SEARCH_PATH_BYTES))),
            ..ShellSelectionRequest::default()
        };
        assert_eq!(
            select_shell(&request),
            Err(ShellSelectionError::SearchPathTooLarge)
        );
    }

    #[test]
    fn private_identifier_and_request_debug_are_redacted() {
        assert_eq!(PrivateSessionId::new(""), Err(PrivateSessionError::Empty));
        assert_eq!(
            PrivateSessionId::new("dean/pelton"),
            Err(PrivateSessionError::InvalidCharacter)
        );
        let id = private_session();
        assert!(!format!("{id:?}").contains("greendale"));

        let selected = select_shell(&ShellSelectionRequest::from_process(
            Some(ShellKind::Bash),
            None,
            None,
        ));
        if let Ok(shell) = selected {
            let request = PtySpawnRequest {
                shell,
                working_directory: std::env::current_dir().unwrap(),
                size: size(24, 80),
                private_session: id,
            };
            let debug = format!("{request:?}");
            assert!(!debug.contains("greendale"));
            assert!(!debug.contains(std::env::current_dir().unwrap().to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn master_transport_preserves_exact_binary_bytes() {
        let mut session = spawn_test_command(
            Path::new("/bin/sh"),
            &[
                "-c",
                "stty raw -echo; printf R; dd bs=8 count=1 2>/dev/null",
            ],
        );
        let mut reader = session.take_reader().unwrap();
        read_until_marker(&mut reader, b'R');
        let bytes = [0, b'\n', 0x1b, b'[', b'A', 0xff, b'\r', b'Z'];
        assert_eq!(session.write_input(&bytes).unwrap(), PtyWrite::Complete);

        let mut received = Vec::new();
        let mut buffer = [0_u8; 32];
        while received.len() < bytes.len() {
            match reader.read_chunk(&mut buffer).unwrap() {
                PtyRead::Bytes(read) => received.extend_from_slice(&buffer[..read]),
                PtyRead::Eof => break,
            }
        }
        assert_eq!(received, bytes);
        assert_eq!(session.wait().unwrap(), ChildExit::Exited(0));
    }

    #[test]
    fn inherited_integration_stream_is_full_duplex_and_single_owner() {
        let request = ShellSelectionRequest::from_process(Some(ShellKind::Bash), None, None);
        let Ok(selected) = select_shell(&request) else {
            return;
        };
        let script = concat!(
            "eval \"printf E >&$ARGMAX_EVENT_FD\"; ",
            "sleep 1; ",
            "value=; ",
            "eval \"IFS= read -r -n 1 value <&$ARGMAX_CONTROL_FD\"; ",
            "test \"$value\" = C && exit 23; ",
            "exit 24"
        );
        let mut session = spawn_test_command(
            selected.executable(),
            &["--noprofile", "--norc", "-c", script],
        );
        let mut integration = session.take_integration().unwrap();
        assert_eq!(
            session.take_integration().unwrap_err(),
            PtyError::IntegrationAlreadyTaken
        );
        read_integration_until(
            &mut integration,
            b'E',
            Instant::now() + Duration::from_secs(2),
        );
        assert_eq!(integration.write_control(b"C"), Ok(PtyWrite::Complete));
        assert_eq!(session.wait().unwrap(), ChildExit::Exited(23));
    }

    #[test]
    fn configured_integration_socket_queues_both_maximum_directions() {
        let (parent, mut child) = integration_stream_pair().unwrap();
        parent.set_nonblocking(true).unwrap();
        child.set_nonblocking(true).unwrap();

        for stream in [&parent, &child] {
            assert!(getsockopt(stream, SndBuf).unwrap() >= MIN_INTEGRATION_SOCKET_CAPACITY);
            assert!(getsockopt(stream, RcvBuf).unwrap() >= MIN_INTEGRATION_SOCKET_CAPACITY);
        }

        let replacement = "x".repeat(MAX_CONTROL_BUFFER_BYTES);
        let control = ReplacementControl::new(
            ControlRequestId::new(MAX_CONTROL_REQUEST_ID).unwrap(),
            replacement,
            MAX_CONTROL_BUFFER_BYTES,
        )
        .unwrap()
        .encode();
        assert_eq!(control.len(), MAX_CONTROL_WIRE_BYTES);

        let mut integration = PtyIntegration {
            stream: Arc::new(Mutex::new(Some(parent))),
        };
        assert_eq!(
            integration.write_control(control.as_bytes()),
            Ok(PtyWrite::Complete)
        );

        let snapshot = "\u{10ffff}".repeat(MAX_SYNC_EVENT_CHARACTERS);
        let event = format!(
            "probe-buffer:f:{MAX_CONTROL_REQUEST_ID}:{MAX_SYNC_EVENT_CHARACTERS}:{snapshot}\n\0"
        )
        .into_bytes();
        assert!(event.len() <= MAX_SYNC_EVENT_WIRE_BYTES);
        assert_eq!(
            write_nonblocking(&mut child, &event),
            Ok(PtyWrite::Complete)
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        let received_control = read_socket_bytes(&mut child, control.len(), deadline);
        assert_eq!(received_control, control.as_bytes());

        let mut received_event = Vec::with_capacity(event.len());
        let mut buffer = [0_u8; 4096];
        while received_event.len() < event.len() {
            match integration.read_events(&mut buffer).unwrap() {
                IntegrationRead::Bytes(read) => {
                    received_event.extend_from_slice(&buffer[..read]);
                }
                IntegrationRead::Pending => {}
                IntegrationRead::Eof => panic!("integration event socket closed early"),
            }
            assert!(Instant::now() < deadline, "integration event read stalled");
        }
        assert_eq!(received_event, event);

        let mut decoder = ShellEventDecoder::new(StreamEpoch::INITIAL);
        let mut frames = Vec::new();
        decoder.push(&received_event, |frame| frames.push(frame));
        let [DecodedFrame::Event(event)] = frames.as_slice() else {
            panic!("maximum probe event was not decoded: {frames:?}");
        };
        let ShellEvent::Buffer(snapshot) = event.event() else {
            panic!("maximum probe event was not a buffer snapshot");
        };
        assert_eq!(snapshot.len(), MAX_SYNC_EVENT_CHARACTERS * 4);
        assert_eq!(snapshot.cursor(), snapshot.len());
        assert_eq!(
            snapshot
                .probe_nonce()
                .map(crate::shell_events::SnapshotNonce::get),
            Some(MAX_CONTROL_REQUEST_ID)
        );
    }

    #[test]
    fn real_shells_acknowledge_maximum_control_over_production_socket() {
        for kind in [ShellKind::Bash, ShellKind::Zsh] {
            let Some(selected) = installed_control_shell(kind) else {
                continue;
            };
            let directory = TestDirectory::new(kind.as_str());
            let init_path = directory.0.join("init");
            let integration_shell = match kind {
                ShellKind::Bash => IntegrationShell::Bash,
                ShellKind::Zsh => IntegrationShell::Zsh,
                ShellKind::Fish => unreachable!(),
            };
            fs::write(&init_path, init_script(integration_shell)).unwrap();

            let arguments = match kind {
                ShellKind::Bash => ["--noprofile", "--norc", "-i"].as_slice(),
                ShellKind::Zsh => ["-f", "-i"].as_slice(),
                ShellKind::Fish => unreachable!(),
            };
            let command = PtyCommand::new(selected.executable())
                .args(arguments)
                .current_dir(std::env::current_dir().unwrap())
                .env(ENV_PRIVATE_SESSION, "greendale-production-socket")
                .env("ARGMAX_TEST_INIT", &init_path)
                .env_remove(ENV_SESSION_OWNER_PID);
            let mut session = PtySession::spawn_command(command, size(24, 80)).unwrap();
            let mut reader = session.take_reader().unwrap();
            let drain = thread::spawn(move || {
                let mut output = vec![0_u8; MAX_PTY_OUTPUT].into_boxed_slice();
                while matches!(reader.read_chunk(&mut output), Ok(PtyRead::Bytes(_))) {}
            });
            let mut integration = session.take_integration().unwrap();
            let mut decoder = ShellEventDecoder::new(StreamEpoch::INITIAL);

            let probe_counter = match kind {
                ShellKind::Bash => "__ARGMAX_BASH_PROBE_NONCE=2147483646",
                ShellKind::Zsh => "__ARGMAX_ZSH_PROBE_NONCE=2147483646",
                ShellKind::Fish => unreachable!(),
            };
            let source = format!("PS1='ARGMAX> '; source \"$ARGMAX_TEST_INIT\"; {probe_counter}\r");
            assert_eq!(
                session.write_input(source.as_bytes()),
                Ok(PtyWrite::Complete)
            );
            wait_for_integration_ready(
                &mut integration,
                &mut decoder,
                Instant::now() + Duration::from_secs(5),
            );

            let replacement = "x".repeat(MAX_CONTROL_BUFFER_BYTES);
            let control = ReplacementControl::new(
                ControlRequestId::new(MAX_CONTROL_REQUEST_ID).unwrap(),
                replacement.clone(),
                replacement.len(),
            )
            .unwrap()
            .encode();
            assert_eq!(control.len(), MAX_CONTROL_WIRE_BYTES);
            assert_eq!(
                integration.write_control(control.as_bytes()),
                Ok(PtyWrite::Complete),
                "{} production control socket did not accept a full frame",
                kind.as_str()
            );
            assert_eq!(
                session.write_input(crate::integration::SYNC_PROBE_SEQUENCE),
                Ok(PtyWrite::Complete)
            );

            let snapshot = wait_for_probe_snapshot(
                &mut integration,
                &mut decoder,
                MAX_CONTROL_REQUEST_ID,
                Instant::now() + Duration::from_secs(5),
            );
            assert_eq!(snapshot.as_bytes(), replacement.as_bytes());
            assert_eq!(snapshot.cursor(), replacement.len());
            drop(integration);
            drop(session);
            drain.join().unwrap();
        }
    }

    #[test]
    fn session_teardown_closes_a_taken_integration_stream() {
        let mut session = spawn_test_command(
            Path::new("/bin/sh"),
            &["-c", "trap '' HUP; while :; do sleep 1; done"],
        );
        let mut integration = session.take_integration().unwrap();
        drop(session);
        let mut buffer = [0_u8; 1];
        assert_eq!(
            integration.read_events(&mut buffer),
            Ok(IntegrationRead::Eof)
        );
        assert_eq!(integration.write_control(b"x"), Err(PtyWriteError::Closed));
    }

    #[test]
    fn detached_daemon_is_outside_signal_policy_but_wrapper_resources_close() {
        if !Path::new("/usr/bin/perl").is_file() {
            return;
        }
        let script = concat!(
            "/usr/bin/perl -MPOSIX -e '",
            "POSIX::setsid() >= 0 or exit 2; ",
            "$SIG{HUP}=\"IGNORE\"; ",
            "open(my $event, \">&=$ENV{ARGMAX_EVENT_FD}\") or exit 3; ",
            "select($event); $|=1; print \"$$\\0\"; sleep 5",
            "' & sleep 1; exit 0"
        );
        let mut session = spawn_test_command(Path::new("/bin/sh"), &["-c", script]);
        let mut integration = session.take_integration().unwrap();
        let frame =
            read_integration_frame(&mut integration, Instant::now() + Duration::from_secs(2));
        let daemon_pid = std::str::from_utf8(&frame)
            .unwrap()
            .parse::<i32>()
            .map(Pid::from_raw)
            .unwrap();
        assert_eq!(session.wait(), Ok(ChildExit::Exited(0)));
        assert!(kill(daemon_pid, None).is_ok());

        drop(session);
        let mut buffer = [0_u8; 1];
        assert_eq!(
            integration.read_events(&mut buffer),
            Ok(IntegrationRead::Eof)
        );
        assert_eq!(integration.write_control(b"x"), Err(PtyWriteError::Closed));
        assert!(kill(daemon_pid, None).is_ok());

        let _ = kill(daemon_pid, Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(1);
        while kill(daemon_pid, None).is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if kill(daemon_pid, None).is_ok() {
            let _ = kill(daemon_pid, Signal::SIGKILL);
            panic!("detached test daemon ignored SIGTERM");
        }
    }

    #[test]
    fn input_and_output_chunks_are_bounded() {
        let mut session = spawn_test_command(Path::new("/bin/sh"), &["-c", "exit 0"]);
        assert_eq!(
            session.write_input(&vec![0; MAX_PTY_INPUT + 1]),
            Err(PtyWriteError::TooLarge {
                actual: MAX_PTY_INPUT + 1,
                limit: MAX_PTY_INPUT,
            })
        );
        let mut reader = session.take_reader().unwrap();
        assert_eq!(reader.read_chunk(&mut []), Err(PtyReadError::EmptyBuffer));
        assert_eq!(
            session.take_reader().unwrap_err(),
            PtyError::ReaderAlreadyTaken
        );
        session.wait().unwrap();
    }

    #[test]
    fn linux_pty_master_eio_is_portable_eof() {
        let eio = io::Error::from_raw_os_error(Errno::EIO as i32);
        assert!(pty_read_errno_is_eof(eio.raw_os_error(), true));
        assert!(!pty_read_errno_is_eof(eio.raw_os_error(), false));
        assert!(!pty_read_errno_is_eof(
            io::Error::from(io::ErrorKind::BrokenPipe).raw_os_error(),
            true
        ));
        assert_eq!(pty_read_error_is_eof(&eio), cfg!(target_os = "linux"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn real_linux_master_reports_closed_slave_as_reader_eof() {
        let pair = nix::pty::openpty(None, None).unwrap();
        let master = File::from(pair.master);
        set_nonblocking(&master).unwrap();
        let mut reader = PtyReader {
            reader: Arc::new(Mutex::new(Some(master))),
        };
        drop(pair.slave);

        let mut buffer = [0_u8; 1];
        assert_eq!(reader.read_chunk(&mut buffer), Ok(PtyRead::Eof));
    }

    #[test]
    fn resize_updates_kernel_pty_dimensions() {
        let session = spawn_test_command(Path::new("/bin/sh"), &["-c", "exit 0"]);
        let resized = PtySize {
            rows: 41,
            cols: 119,
            pixel_width: 952,
            pixel_height: 656,
        };
        session.resize(resized).unwrap();
        assert_eq!(session.size().unwrap(), resized);
        assert_eq!(session.resize(size(0, 80)), Err(PtyError::InvalidSize));
    }

    #[test]
    fn drop_kills_and_reaps_a_hup_ignoring_group_with_an_outliving_reader() {
        let mut session = spawn_test_command(
            Path::new("/bin/sh"),
            &[
                "-c",
                "trap '' HUP; stty raw -echo; printf R; while :; do :; done",
            ],
        );
        let shell_pid = Pid::from_raw(i32::try_from(session.shell_pid.unwrap()).unwrap());
        let mut reader = session.take_reader().unwrap();
        read_until_marker(&mut reader, b'R');

        let started = Instant::now();
        drop(session);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            waitpid(shell_pid, Some(WaitPidFlag::WNOHANG)),
            Err(Errno::ECHILD)
        );
        assert_eq!(killpg(shell_pid, None), Err(Errno::ESRCH));

        // A taken reader cannot retain the wrapper's master descriptor.
        let mut buffer = [0_u8; 1];
        assert_eq!(reader.read_chunk(&mut buffer), Ok(PtyRead::Eof));
    }

    #[test]
    fn drop_kills_group_members_that_outlive_the_direct_child() {
        let mut session = spawn_test_command(
            Path::new("/bin/sh"),
            &[
                "-c",
                "sh -c 'trap \"\" HUP; while :; do :; done' & stty raw -echo; printf R; wait",
            ],
        );
        let shell_pid = Pid::from_raw(i32::try_from(session.shell_pid.unwrap()).unwrap());
        let mut reader = session.take_reader().unwrap();
        read_until_marker(&mut reader, b'R');

        let started = Instant::now();
        drop(session);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            waitpid(shell_pid, Some(WaitPidFlag::WNOHANG)),
            Err(Errno::ECHILD)
        );
        assert_eq!(killpg(shell_pid, None), Err(Errno::ESRCH));
        drop(reader);
    }

    #[test]
    fn drop_does_not_close_a_full_blocking_writer_before_killing_the_child() {
        let mut session = spawn_test_command(
            Path::new("/bin/sh"),
            &[
                "-c",
                "trap '' HUP; stty raw -echo; printf R; while :; do :; done",
            ],
        );
        let shell_pid = Pid::from_raw(i32::try_from(session.shell_pid.unwrap()).unwrap());
        let mut reader = session.take_reader().unwrap();
        read_until_marker(&mut reader, b'R');

        let input = vec![b'x'; MAX_PTY_INPUT];
        let mut queue_full = false;
        for _ in 0..64 {
            match session.write_input(&input) {
                Ok(PtyWrite::Complete) => {}
                Ok(PtyWrite::Partial { .. }) => {
                    queue_full = true;
                    break;
                }
                Err(error) => panic!("unexpected PTY fill failure: {error}"),
            }
        }
        assert!(queue_full);

        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let drop_thread = thread::spawn(move || {
            drop(session);
            let _ = done_tx.send(());
        });
        if done_rx.recv_timeout(Duration::from_secs(2)).is_err() {
            let _ = killpg(shell_pid, Signal::SIGKILL);
            drop_thread.join().unwrap();
            panic!("PTY session drop blocked on a full writer");
        }
        drop_thread.join().unwrap();
        assert_eq!(
            waitpid(shell_pid, Some(WaitPidFlag::WNOHANG)),
            Err(Errno::ECHILD)
        );
        drop(reader);
    }

    #[test]
    fn saturated_input_reports_partial_progress_and_close_never_blocks() {
        let mut session = spawn_test_command(
            Path::new("/bin/sh"),
            &[
                "-c",
                "trap '' HUP; stty raw -echo; printf R; while :; do sleep 1; done",
            ],
        );
        let mut reader = session.take_reader().unwrap();
        read_until_marker(&mut reader, b'R');
        let input = vec![b'x'; MAX_PTY_INPUT];
        let mut saw_partial = false;
        for _ in 0..64 {
            let progress = session.write_input(&input).unwrap();
            if let PtyWrite::Partial { written } = progress {
                assert!(written < input.len());
                saw_partial = true;
                if written == 0 {
                    break;
                }
            }
        }
        assert!(saw_partial, "PTY input queue never saturated");

        let started = Instant::now();
        let close = session.close_input().unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(matches!(close, PtyClose::Closed | PtyClose::Pending { .. }));

        let started = Instant::now();
        drop(session);
        assert!(started.elapsed() < Duration::from_secs(1));
        let mut buffer = [0_u8; 1];
        assert_eq!(reader.read_chunk(&mut buffer), Ok(PtyRead::Eof));
    }

    #[test]
    fn foreground_signal_reaches_child_process_group() {
        let mut session = spawn_test_command(
            Path::new("/bin/sh"),
            &[
                "-c",
                "trap 'exit 42' TERM; stty raw -echo; printf R; while :; do read value; done",
            ],
        );
        let mut reader = session.take_reader().unwrap();
        read_until_marker(&mut reader, b'R');
        assert!(matches!(
            session.foreground_state(),
            ForegroundState::Available { .. }
        ));
        assert_eq!(
            session.forward_signal(ForwardSignal::Terminate).unwrap(),
            SignalDelivery::Delivered
        );
        assert_eq!(session.wait().unwrap(), ChildExit::Exited(42));
    }

    #[test]
    fn native_signal_exit_is_structured() {
        let mut session = spawn_test_command(Path::new("/bin/sh"), &["-c", "kill -TERM $$"]);
        let exit = session.wait().unwrap();
        assert_eq!(exit, ChildExit::Signaled(SIGTERM));
        assert_eq!(exit.wrapper_code(), Some(128 + SIGTERM));
    }

    #[test]
    fn externally_reaped_child_clears_numeric_signal_authority() {
        let mut session = spawn_test_command(Path::new("/bin/sh"), &["-c", "exit 7"]);
        let shell_pid = Pid::from_raw(i32::try_from(session.shell_pid.unwrap()).unwrap());
        let status = waitpid(shell_pid, None).unwrap();
        assert!(matches!(status, nix::sys::wait::WaitStatus::Exited(_, 7)));

        assert_eq!(session.try_wait(), Ok(Some(ChildExit::ExternallyReaped)));
        assert_eq!(session.shell_pid, None);
        assert!(!session.signal_authority);
        assert_eq!(
            session.forward_signal(ForwardSignal::Terminate),
            Ok(SignalDelivery::NoForegroundProcessGroup)
        );

        let started = Instant::now();
        drop(session);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn invalid_spawn_inputs_fail_before_opening_a_pty() {
        let selected = SelectedShell {
            kind: ShellKind::Bash,
            executable: PathBuf::from("/bin/sh"),
            source: ShellSource::CommandLine,
        };
        let relative = PtySpawnRequest {
            shell: selected.clone(),
            working_directory: PathBuf::from("relative"),
            size: size(24, 80),
            private_session: private_session(),
        };
        assert!(matches!(
            PtySession::spawn_during_startup(&relative),
            Err(PtyError::WorkingDirectoryNotAbsolute)
        ));

        let zero = PtySpawnRequest {
            shell: selected,
            working_directory: std::env::current_dir().unwrap(),
            size: size(0, 80),
            private_session: private_session(),
        };
        assert!(matches!(
            PtySession::spawn_during_startup(&zero),
            Err(PtyError::InvalidSize)
        ));
    }

    #[test]
    fn startup_spawn_capability_cannot_be_reopened_by_runtime_workers() {
        let directory = TestDirectory::new("startup-token");
        let executable = directory.executable(ShellKind::Bash);
        let request = PtySpawnRequest {
            shell: SelectedShell {
                kind: ShellKind::Bash,
                executable,
                source: ShellSource::CommandLine,
            },
            working_directory: std::env::current_dir().unwrap(),
            size: size(24, 80),
            private_session: private_session(),
        };
        let mut startup = PtyStartup::acquire().unwrap();
        assert!(matches!(
            PtyStartup::acquire(),
            Err(PtyStartupError::AlreadyClaimed)
        ));
        let mut session = startup.spawn(&request).unwrap();
        assert_eq!(session.wait(), Ok(ChildExit::Exited(0)));
        assert!(matches!(
            startup.spawn(&request),
            Err(PtyError::StartupSpawnAlreadyUsed)
        ));
        assert!(matches!(
            PtyStartup::acquire(),
            Err(PtyStartupError::Sealed)
        ));
    }

    #[test]
    fn supported_shell_executables_open_in_real_ptys_when_available() {
        for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
            let request = ShellSelectionRequest::from_process(Some(kind), None, None);
            let Ok(selected) = select_shell(&request) else {
                continue;
            };
            let arguments: &[&str] = match kind {
                ShellKind::Bash => &["--noprofile", "--norc", "-c", "exit 17"],
                ShellKind::Zsh => &["-f", "-c", "exit 17"],
                ShellKind::Fish => &["--no-config", "-c", "exit 17"],
            };
            let mut session = spawn_test_command(selected.executable(), arguments);
            let deadline = Instant::now() + Duration::from_secs(5);
            let exit = loop {
                if let Some(exit) = session.try_wait().unwrap() {
                    break exit;
                }
                assert!(Instant::now() < deadline, "{kind} did not exit");
                thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(exit, ChildExit::Exited(17), "{kind}");
            assert_eq!(
                session.capabilities().integration_events,
                IntegrationEventCapability::InheritedFullDuplexUnixStream
            );
        }
    }

    #[test]
    fn closing_input_is_idempotent_and_future_writes_fail() {
        let mut session =
            spawn_test_command(Path::new("/bin/sh"), &["-c", "line=; IFS= read -r line"]);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(session.close_input().unwrap(), PtyClose::Closed) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal EOF did not become writable"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(session.close_input(), Ok(PtyClose::Closed));
        assert_eq!(session.write_input(b"later"), Err(PtyWriteError::Closed));
    }

    #[test]
    fn errors_and_debug_do_not_retain_io_bytes() {
        let session = spawn_test_command(Path::new("/bin/sh"), &["-c", "exit 0"]);
        let debug = format!("{session:?}");
        assert!(!debug.contains("/bin/sh"));
        assert!(!debug.contains("greendale"));

        let directory = TestDirectory::new("ordinary-file");
        let file = directory.0.join("bash");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .unwrap();
        let request = ShellSelectionRequest {
            command_line: Some(ShellKind::Bash),
            search_path: Some(directory.0.clone().into_os_string()),
            ..ShellSelectionRequest::default()
        };
        let error = select_shell(&request).unwrap_err();
        assert!(!format!("{error:?}").contains(file.to_string_lossy().as_ref()));
    }

    #[test]
    fn an_interrupted_write_is_retryable_rather_than_fatal() {
        struct FailingWriter(io::ErrorKind);

        impl Write for FailingWriter {
            fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(self.0))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        for kind in [io::ErrorKind::Interrupted, io::ErrorKind::WouldBlock] {
            assert_eq!(
                write_nonblocking(&mut FailingWriter(kind), b"greendale"),
                Ok(PtyWrite::Partial { written: 0 }),
                "{kind:?} was not retryable"
            );
        }

        assert_eq!(
            write_nonblocking(&mut FailingWriter(io::ErrorKind::BrokenPipe), b"greendale"),
            Err(PtyWriteError::Io {
                kind: io::ErrorKind::BrokenPipe,
                written: 0,
            })
        );
    }
}
