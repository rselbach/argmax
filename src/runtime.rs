//! Transparent interactive runtime for one wrapped shell session.
//!
//! The runtime owns transport and terminal lifecycle only. Completion provider
//! work is cancelled immediately until an application-level dispatcher is
//! wired; ordinary input and child output remain byte-for-byte transparent.

use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use directories::BaseDirs;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::isatty;

use crate::ai_lifecycle::{CancellationReason, SessionId as AiSessionId};
use crate::completion::{CompletionQuery, ProviderBatch, Suggestion};
use crate::config::{Mode, Settings, Shell};
use crate::coordinator::BatchOutcome;
use crate::history::HistoryFormat;
use crate::integration::FISH_SYNC_PROBE_SEQUENCE;
use crate::keybindings::KeybindingAction;
use crate::learning::CommandOutcome;
use crate::learning_store::LearningStore;
use crate::overlay::{OverlayOptions, OverlayRenderer, OverlayRequest, RenderTransaction};
use crate::providers::{ShellKind as ProviderShellKind, alias_config_paths};
use crate::pty::{
    ChildExit, ForegroundState, ForwardSignal, IntegrationRead, PrivateSessionId, PtyClose,
    PtyIntegration, PtyRead, PtyReadError, PtyReader, PtySession, PtySpawnRequest, PtyStartup,
    SelectedShell, ShellKind, ShellSelectionRequest, SignalEvent, SignalEvents, select_shell,
};
use crate::reload::{ConfigReloader, RELOAD_ACK_PREFIX, ReloadChange, ReloadPoll};
use crate::runtime_ai::{
    AI_COMPLETION_PROVIDER, AiCompletionDispatcher, AiCompletionOptions, MAX_AI_DRAIN_BATCHES,
};
use crate::runtime_completion::{
    HistorySource, LOCAL_COMPLETION_PROVIDER, LocalCompletionDispatcher, LocalCompletionOptions,
    MAX_ALIAS_EXPANSION_DRAIN, MAX_COMPLETION_DRAIN_BATCHES,
};
use crate::runtime_update::{RuntimeUpdateOptions, RuntimeUpdateWorker};
use crate::screen::{CursorPosition, ScreenObserver, TerminalSize};
use crate::session::{EffectBatch, SessionEffect, SessionReducer};
use crate::shell_control::{ControlRequestId, ReplacementControl};
use crate::shell_events::{
    DecodedFrame, ForegroundCommandState, ReloadRequest, SYNC_PROBE_SEQUENCE, ShellEventDecoder,
    StateUpdate, StreamEpoch,
};
use crate::state::{LastMode, RuntimeStateStore};
use crate::terminal::{SerializedOutput, TerminalDimensions, TerminalGuard};

const INPUT_CHUNK_BYTES: usize = 4 * 1024;
const EVENT_CHUNK_BYTES: usize = 64 * 1024;
const OUTPUT_CHANNEL_CAPACITY: usize = 8;
const MAX_PENDING_WRITE_BYTES: usize = 512 * 1024;
const IDLE_POLL: Duration = Duration::from_millis(1);
const STANDALONE_ESCAPE_TIMEOUT: Duration = Duration::from_millis(25);
const CURSOR_POSITION_TIMEOUT: Duration = Duration::from_millis(100);
/// How long trailing output may keep the session alive after the child is
/// reaped. A background job inheriting the slave keeps the master from ever
/// reaching end of file; without a bound the wrapper would hold the terminal
/// captive until that job exits.
const EXIT_OUTPUT_DRAIN: Duration = Duration::from_millis(500);
const CURSOR_POSITION_QUERY: &[u8] = b"\x1b[6n";
const MAX_WAITING_RELOAD_ACKNOWLEDGMENTS: usize = 8;
const MAX_DRAIN_PER_TICK: usize = 64;
const COMPLETION_PROVIDERS: [&str; 2] = [LOCAL_COMPLETION_PROVIDER, AI_COMPLETION_PROVIDER];

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Failure while running one interactive wrapped shell.
#[derive(Debug)]
pub enum RuntimeError {
    /// Resolved settings or keybindings were invalid.
    Configuration(String),
    /// The documented environment shell override was malformed.
    InvalidEnvironmentShell,
    /// Standard input or output was not a usable terminal.
    TerminalRequired,
    /// A terminal, PTY, reducer, or shell-selection operation failed.
    Setup(String),
    /// Reading parent standard input failed.
    Input(io::ErrorKind),
    /// PTY output or integration input failed.
    Read(String),
    /// PTY input or integration control output failed.
    Write(String),
    /// Parent terminal output failed.
    Output(String),
    /// A reducer effect could not be represented without reordering bytes.
    ProtocolInvariant(&'static str),
    /// Bounded pending transport storage was exhausted.
    PendingInputFull,
    /// The PTY reader worker panicked.
    ReaderPanicked,
    /// Restoring the parent terminal failed.
    Restore(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => {
                write!(formatter, "invalid runtime configuration: {error}")
            }
            Self::InvalidEnvironmentShell => {
                formatter.write_str("argmax_CORE_SHELL must be one of bash, zsh, or fish")
            }
            Self::TerminalRequired => formatter.write_str(
                "argmax requires usable terminals on standard input and standard output",
            ),
            Self::Setup(error) => write!(formatter, "interactive runtime setup failed: {error}"),
            Self::Input(kind) => write!(formatter, "standard-input read failed: {kind}"),
            Self::Read(error) => write!(formatter, "interactive transport read failed: {error}"),
            Self::Write(error) => write!(formatter, "interactive transport write failed: {error}"),
            Self::Output(error) => write!(formatter, "terminal output failed: {error}"),
            Self::ProtocolInvariant(message) => {
                write!(
                    formatter,
                    "interactive protocol invariant failed: {message}"
                )
            }
            Self::PendingInputFull => {
                formatter.write_str("bounded pending shell-input queue is full")
            }
            Self::ReaderPanicked => formatter.write_str("PTY reader worker panicked"),
            Self::Restore(error) => write!(formatter, "terminal restoration failed: {error}"),
        }
    }
}

impl Error for RuntimeError {}

/// Optional content-safe diagnostic destination for isolated runtime workers.
pub trait RuntimeDiagnosticSink {
    /// Records one sanitized component event without affecting the session.
    fn record(&self, component: &'static str, message: &str);
}

/// Runs one supported shell under the transparent interactive wrapper.
///
/// Shell selection applies CLI, documented environment, resolved
/// configuration, active-shell, `SHELL`, and fallback precedence. The returned
/// status preserves native exit and signal information.
///
/// # Errors
///
/// Returns a bounded, redacted [`RuntimeError`] when setup, transport, protocol,
/// or restoration fails.
pub fn run_interactive(
    settings: &Settings,
    cli_shell: Option<Shell>,
) -> Result<ChildExit, RuntimeError> {
    run_interactive_inner(settings, cli_shell, None, None)
}

/// Runs one supported shell with automatic and explicit live configuration.
///
/// The reloader's initial validated snapshot selects runtime behavior. Later
/// generations update session settings without replacing the shell process or
/// its working directory.
///
/// # Errors
///
/// Returns the same bounded failures as [`run_interactive`].
pub fn run_interactive_with_reloader(
    reloader: ConfigReloader,
    cli_shell: Option<Shell>,
) -> Result<ChildExit, RuntimeError> {
    let initial = reloader.shared_settings().snapshot();
    run_interactive_inner(initial.settings(), cli_shell, Some(reloader), None)
}

/// Runs an interactive session while reporting sanitized worker failures.
///
/// # Errors
///
/// Returns the same bounded failures as [`run_interactive_with_reloader`].
pub fn run_interactive_with_diagnostics(
    reloader: ConfigReloader,
    cli_shell: Option<Shell>,
    diagnostics: &dyn RuntimeDiagnosticSink,
) -> Result<ChildExit, RuntimeError> {
    let initial = reloader.shared_settings().snapshot();
    run_interactive_inner(
        initial.settings(),
        cli_shell,
        Some(reloader),
        Some(diagnostics),
    )
}

fn run_interactive_inner(
    settings: &Settings,
    cli_shell: Option<Shell>,
    reloader: Option<ConfigReloader>,
    diagnostics: Option<&dyn RuntimeDiagnosticSink>,
) -> Result<ChildExit, RuntimeError> {
    run_prepared_runtime(prepare_runtime(settings, cli_shell, reloader)?, diagnostics)
}

struct PreparedRuntime {
    settings: Settings,
    keybindings: crate::keybindings::ResolvedKeybindings,
    dimensions: TerminalDimensions,
    working_directory: PathBuf,
    selected_shell: SelectedShell,
    reloader: Option<ConfigReloader>,
    private_session: PrivateSessionId,
    ai_session: AiSessionId,
    initial_mode: crate::session::SessionMode,
}

fn prepare_runtime(
    settings: &Settings,
    cli_shell: Option<Shell>,
    reloader: Option<ConfigReloader>,
) -> Result<PreparedRuntime, RuntimeError> {
    settings
        .validate()
        .map_err(|error| RuntimeError::Configuration(error.to_string()))?;
    let keybindings = settings
        .keybindings
        .resolve()
        .map_err(|error| RuntimeError::Configuration(error.to_string()))?;
    validate_stdio_ttys()?;

    let input = io::stdin();
    let dimensions = TerminalDimensions::from_terminal(&input)
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let working_directory = std::env::current_dir()
        .map_err(|error| RuntimeError::Setup(format!("current directory: {}", error.kind())))?;
    let selected_shell = select_shell(&ShellSelectionRequest::from_process(
        cli_shell.map(shell_kind),
        environment_shell_override()?,
        settings.core.shell.map(shell_kind),
    ))
    .map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let (private_session, ai_session) = session_identity()?;
    let initial_mode = initial_session_mode(settings);
    Ok(PreparedRuntime {
        settings: settings.clone(),
        keybindings,
        dimensions,
        working_directory,
        selected_shell,
        reloader,
        private_session,
        ai_session,
        initial_mode,
    })
}

fn run_prepared_runtime(
    prepared: PreparedRuntime,
    diagnostics: Option<&dyn RuntimeDiagnosticSink>,
) -> Result<ChildExit, RuntimeError> {
    let PreparedRuntime {
        settings,
        keybindings,
        dimensions,
        working_directory,
        selected_shell,
        reloader,
        private_session,
        ai_session,
        initial_mode,
    } = prepared;
    let selected_shell_kind = selected_shell.kind();

    let mut startup =
        PtyStartup::acquire().map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let mut terminal = TerminalGuard::enter_stdio(dimensions)
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let screen_size = TerminalSize::new(dimensions.size().cols, dimensions.size().rows)
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let initial_terminal = probe_initial_terminal(&terminal, screen_size, diagnostics)?;
    let request = PtySpawnRequest {
        shell: selected_shell,
        working_directory: working_directory.clone(),
        size: dimensions.size(),
        private_session,
    };
    let mut session = startup
        .spawn(&request)
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;

    // These sources and all threads are deliberately created only after the
    // startup-only descriptor inheritance window has been permanently sealed.
    let signals = SignalEvents::new().map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let reader = session
        .take_reader()
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let integration = session
        .take_integration()
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let services = spawn_runtime_services(selected_shell_kind, &settings, ai_session);
    let mut reducer = SessionReducer::new_with_mode(
        StreamEpoch::INITIAL,
        keybindings.binding(KeybindingAction::ToggleMode).sequence(),
        keybindings.binding(KeybindingAction::ToggleMenu).sequence(),
        COMPLETION_PROVIDERS,
        usize::from(settings.ui.max_suggestions),
        working_directory,
        initial_mode,
    )
    .map_err(|error| RuntimeError::Setup(error.to_string()))?;

    let reader_stop = Arc::new(AtomicBool::new(false));
    let (output_sender, output_receiver) = mpsc::sync_channel(OUTPUT_CHANNEL_CAPACITY);
    let reader_thread = spawn_reader(reader, output_sender, Arc::clone(&reader_stop))?;
    let mut driver = SessionDriver::new(
        DriverTransport {
            integration,
            signals,
            output_receiver,
            output: terminal.output(),
        },
        InitialOverlay {
            screen_size,
            initial_cursor: initial_terminal.cursor,
            options: OverlayOptions::from_ui(settings.ui),
            footer_hint: footer_hint(&keybindings),
        },
        services,
        sync_probe_sequence(selected_shell_kind),
    );
    driver
        .pending
        .push(WriteDestination::Pty, initial_terminal.pending_input)?;
    let mut live_configuration = LiveConfiguration::new(settings, reloader);

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        drive_session(
            &mut session,
            &mut driver,
            &mut reducer,
            &mut terminal,
            &mut live_configuration,
            diagnostics,
        )
    }));

    let overlay_result = if result.is_ok() {
        driver.clear_overlay()
    } else {
        driver.clear_overlay_after_failure()
    };
    if let Some(ai) = driver.ai.as_ref() {
        ai.cancel(CancellationReason::SessionExit);
    }
    reader_stop.store(true, Ordering::Release);
    drop(session);
    let reader_result = reader_thread
        .join()
        .map_err(|_| RuntimeError::ReaderPanicked);
    let terminal_result = terminal
        .restore()
        .map_err(|error| RuntimeError::Restore(error.to_string()));

    finish_runtime(result, overlay_result, reader_result, terminal_result)
}

fn finish_runtime(
    result: thread::Result<Result<ChildExit, RuntimeError>>,
    overlay_result: Result<(), RuntimeError>,
    reader_result: Result<(), RuntimeError>,
    terminal_result: Result<(), RuntimeError>,
) -> Result<ChildExit, RuntimeError> {
    match result {
        Ok(result) => {
            let exit = result?;
            overlay_result?;
            reader_result?;
            terminal_result?;
            Ok(exit)
        }
        Err(payload) => {
            drop(overlay_result);
            drop(reader_result);
            drop(terminal_result);
            panic::resume_unwind(payload);
        }
    }
}

fn footer_hint(keybindings: &crate::keybindings::ResolvedKeybindings) -> String {
    format!(
        "{} history  {} menu  Tab insert  Esc hide",
        keybindings.footer_hint(KeybindingAction::ToggleMode),
        keybindings.footer_hint(KeybindingAction::ToggleMenu)
    )
}

fn validate_stdio_ttys() -> Result<(), RuntimeError> {
    let input = isatty(io::stdin().as_raw_fd()).map_err(|_| RuntimeError::TerminalRequired)?;
    let output = isatty(io::stdout().as_raw_fd()).map_err(|_| RuntimeError::TerminalRequired)?;
    if input && output {
        Ok(())
    } else {
        Err(RuntimeError::TerminalRequired)
    }
}

const fn shell_kind(shell: Shell) -> ShellKind {
    match shell {
        Shell::Bash => ShellKind::Bash,
        Shell::Zsh => ShellKind::Zsh,
        Shell::Fish => ShellKind::Fish,
    }
}

const fn sync_probe_sequence(shell: ShellKind) -> &'static [u8] {
    match shell {
        ShellKind::Fish => FISH_SYNC_PROBE_SEQUENCE,
        ShellKind::Bash | ShellKind::Zsh => SYNC_PROBE_SEQUENCE,
    }
}

const fn provider_shell_kind(shell: ShellKind) -> ProviderShellKind {
    match shell {
        ShellKind::Bash => ProviderShellKind::Bash,
        ShellKind::Zsh => ProviderShellKind::Zsh,
        ShellKind::Fish => ProviderShellKind::Fish,
    }
}

fn local_completion_options(
    shell: ProviderShellKind,
    settings: Settings,
    shared_settings: Option<crate::reload::SharedSettings>,
) -> LocalCompletionOptions {
    let mut options = LocalCompletionOptions::new(
        shell,
        settings,
        std::env::var_os("PATH").unwrap_or_default(),
    )
    .with_environment_names(std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()));
    if let Some(shared_settings) = shared_settings {
        options = options.with_shared_settings(shared_settings);
    }
    if let Some(base) = BaseDirs::new() {
        let home = base.home_dir();
        let alias_paths = alias_config_paths(
            shell,
            home,
            std::env::var_os("ZDOTDIR").as_deref(),
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        );
        options = options
            .with_home_directory(home)
            .with_alias_paths(alias_paths)
            .with_history(history_source(shell, home));
    }
    if let Ok(store) = LearningStore::discover() {
        options = options.with_learning_store(store);
    }
    options
}

fn spawn_local_completion(
    shell: ProviderShellKind,
    settings: Settings,
    shared_settings: Option<crate::reload::SharedSettings>,
) -> Option<LocalCompletionDispatcher> {
    LocalCompletionDispatcher::spawn(local_completion_options(shell, settings, shared_settings))
        .ok()
}

fn spawn_ai_completion(
    shell: ShellKind,
    settings: Settings,
    shared_settings: Option<crate::reload::SharedSettings>,
    session: AiSessionId,
) -> Option<AiCompletionDispatcher> {
    let mut options = AiCompletionOptions::new(shell.as_str(), settings).with_session_id(session);
    if let Some(shared_settings) = shared_settings {
        options = options.with_shared_settings(shared_settings);
    }
    AiCompletionDispatcher::spawn(options).ok()
}

fn spawn_runtime_updater(
    settings: &Settings,
    shared_settings: Option<crate::reload::SharedSettings>,
) -> Option<RuntimeUpdateWorker> {
    let mut options = RuntimeUpdateOptions::discover(settings.updater).ok()?;
    if let Some(shared_settings) = shared_settings {
        options = options.with_shared_settings(shared_settings);
    }
    RuntimeUpdateWorker::spawn(options).ok()
}

fn spawn_runtime_services(
    shell: ShellKind,
    settings: &Settings,
    session: AiSessionId,
) -> RuntimeServices {
    RuntimeServices {
        completion: spawn_local_completion(provider_shell_kind(shell), settings.clone(), None),
        ai: spawn_ai_completion(shell, settings.clone(), None, session),
        updater: spawn_runtime_updater(settings, None),
        persist_mode: settings.core.mode == Mode::Last,
    }
}

fn history_source(shell: ProviderShellKind, home: &Path) -> HistorySource {
    match shell {
        ProviderShellKind::Bash => HistorySource::new(
            configured_history_path(home, ".bash_history"),
            HistoryFormat::Bash,
        ),
        ProviderShellKind::Zsh => HistorySource::new(
            configured_history_path(home, ".zsh_history"),
            HistoryFormat::Zsh,
        ),
        ProviderShellKind::Fish => {
            let data_home = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(".local/share"));
            HistorySource::new(data_home.join("fish/fish_history"), HistoryFormat::Fish)
        }
    }
}

fn configured_history_path(home: &Path, fallback: &str) -> PathBuf {
    std::env::var_os("HISTFILE")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(fallback))
}

fn unix_seconds_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn environment_shell_override() -> Result<Option<ShellKind>, RuntimeError> {
    let Some(value) = std::env::var_os(crate::pty::ENV_SHELL_OVERRIDE) else {
        return Ok(None);
    };
    match value.to_str() {
        Some("bash") => Ok(Some(ShellKind::Bash)),
        Some("zsh") => Ok(Some(ShellKind::Zsh)),
        Some("fish") => Ok(Some(ShellKind::Fish)),
        _ => Err(RuntimeError::InvalidEnvironmentShell),
    }
}

fn session_identity() -> Result<(PrivateSessionId, AiSessionId), RuntimeError> {
    let sequence = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let private = PrivateSessionId::new(format!("argmax-{}-{sequence}", std::process::id()))
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;
    Ok((private, AiSessionId::new(sequence)))
}

fn initial_session_mode(settings: &Settings) -> crate::session::SessionMode {
    let last = (settings.core.mode == Mode::Last)
        .then(|| {
            RuntimeStateStore::discover()
                .and_then(|store| store.load())
                .ok()
                .and_then(|loaded| loaded.state.last_mode())
        })
        .flatten();
    configured_initial_mode(settings.core.mode, last)
}

const fn configured_initial_mode(
    configured: Mode,
    last: Option<LastMode>,
) -> crate::session::SessionMode {
    match (configured, last) {
        (Mode::History, _) | (Mode::Last, Some(LastMode::History)) => {
            crate::session::SessionMode::History
        }
        (Mode::Spec | Mode::Last, _) => crate::session::SessionMode::Spec,
    }
}

fn read_if_ready<R: Read + AsFd>(
    input: &mut R,
    buffer: &mut [u8],
) -> Result<Option<io::Result<usize>>, RuntimeError> {
    // Poll without changing file status flags: terminal stdin and stdout can
    // share one open-file description, so O_NONBLOCK on input can corrupt a
    // partially written overlay frame when output backpressure occurs.
    let ready = {
        let mut descriptors = [PollFd::new(input.as_fd(), PollFlags::POLLIN)];
        poll(&mut descriptors, PollTimeout::ZERO)
    };
    match ready {
        Ok(0) | Err(Errno::EINTR) => Ok(None),
        Ok(_) => Ok(Some(input.read(buffer))),
        Err(error) => Err(RuntimeError::Input(io::Error::from(error).kind())),
    }
}

struct InitialTerminalState {
    cursor: Option<CursorPosition>,
    pending_input: Box<[u8]>,
}

fn probe_initial_terminal(
    terminal: &TerminalGuard<io::Stdin, io::Stdout>,
    size: TerminalSize,
    diagnostics: Option<&dyn RuntimeDiagnosticSink>,
) -> Result<InitialTerminalState, RuntimeError> {
    let mut parent_input = io::stdin();
    let state = probe_terminal_cursor(&mut parent_input, &terminal.output(), size)?;
    if state.cursor.is_none() {
        if let Some(diagnostics) = diagnostics {
            diagnostics.record(
                "terminal",
                "cursor position report unavailable; overlay rendering is suppressed",
            );
        }
    }
    Ok(state)
}

fn probe_terminal_cursor<R: Read + AsFd, W: io::Write>(
    input: &mut R,
    output: &SerializedOutput<W>,
    size: TerminalSize,
) -> Result<InitialTerminalState, RuntimeError> {
    output
        .write_overlay(CURSOR_POSITION_QUERY)
        .map_err(|error| RuntimeError::Output(error.to_string()))?;

    let deadline = Instant::now() + CURSOR_POSITION_TIMEOUT;
    let mut pending_input = Vec::with_capacity(INPUT_CHUNK_BYTES);
    let mut buffer = [0_u8; 256];
    loop {
        if let Some((start, end, cursor)) = cursor_position_report(&pending_input, size) {
            pending_input.drain(start..end);
            return Ok(InitialTerminalState {
                cursor: Some(cursor),
                pending_input: pending_input.into_boxed_slice(),
            });
        }
        if Instant::now() >= deadline || pending_input.len() == INPUT_CHUNK_BYTES {
            return Ok(InitialTerminalState {
                cursor: None,
                pending_input: pending_input.into_boxed_slice(),
            });
        }

        let available = INPUT_CHUNK_BYTES - pending_input.len();
        let read_limit = available.min(buffer.len());
        let read_buffer = &mut buffer[..read_limit];
        match read_if_ready(input, read_buffer)? {
            Some(Ok(0)) => {
                return Ok(InitialTerminalState {
                    cursor: None,
                    pending_input: pending_input.into_boxed_slice(),
                });
            }
            Some(Ok(read)) => pending_input.extend_from_slice(&read_buffer[..read]),
            Some(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
            Some(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
            Some(Err(error)) => return Err(RuntimeError::Input(error.kind())),
            None => thread::sleep(IDLE_POLL),
        }
    }
}

fn cursor_position_report(
    bytes: &[u8],
    size: TerminalSize,
) -> Option<(usize, usize, CursorPosition)> {
    for start in 0..bytes.len().saturating_sub(1) {
        if bytes.get(start..start + 2) != Some(b"\x1b[") {
            continue;
        }
        let mut index = start + 2;
        let Some(row) = parse_cursor_coordinate(bytes, &mut index, b';') else {
            continue;
        };
        let Some(column) = parse_cursor_coordinate(bytes, &mut index, b'R') else {
            continue;
        };
        // A terminal never reports a position outside its own dimensions, so a
        // well-formed report that does not fit means the buffer holds input
        // that merely resembles one. Continuing to search would consume a later
        // lookalike out of the user's typed-ahead bytes and anchor the overlay
        // to a position the terminal never reported, so detection stops here
        // and the caller falls back to no anchor.
        if row > size.rows() || column > size.columns() {
            return None;
        }
        return Some((start, index, CursorPosition::new(row - 1, column - 1)));
    }
    None
}

fn parse_cursor_coordinate(bytes: &[u8], index: &mut usize, terminator: u8) -> Option<u16> {
    let start = *index;
    let mut value = 0_u16;
    while let Some(byte) = bytes.get(*index).copied().filter(u8::is_ascii_digit) {
        value = value.checked_mul(10)?.checked_add(u16::from(byte - b'0'))?;
        *index += 1;
    }
    if *index == start || value == 0 || bytes.get(*index) != Some(&terminator) {
        return None;
    }
    *index += 1;
    Some(value)
}

enum ReaderMessage {
    Bytes(Box<[u8]>),
    Eof,
    Error(PtyReadError),
}

fn spawn_reader(
    reader: PtyReader,
    sender: mpsc::SyncSender<ReaderMessage>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, RuntimeError> {
    thread::Builder::new()
        .name("argmax-pty-reader".to_owned())
        .spawn(move || reader_pump(reader, &sender, &stop))
        .map_err(|error| RuntimeError::Setup(format!("PTY reader thread: {}", error.kind())))
}

fn reader_pump(mut reader: PtyReader, sender: &mpsc::SyncSender<ReaderMessage>, stop: &AtomicBool) {
    let mut buffer = vec![0_u8; EVENT_CHUNK_BYTES];
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let message = match reader.read_chunk(&mut buffer) {
            Ok(PtyRead::Bytes(read)) => {
                ReaderMessage::Bytes(buffer[..read].to_vec().into_boxed_slice())
            }
            Ok(PtyRead::Eof) => ReaderMessage::Eof,
            Err(error) => ReaderMessage::Error(error),
        };
        let terminal = matches!(message, ReaderMessage::Eof | ReaderMessage::Error(_));
        if !send_reader_message(sender, message, stop) || terminal {
            return;
        }
    }
}

fn send_reader_message(
    sender: &mpsc::SyncSender<ReaderMessage>,
    mut message: ReaderMessage,
    stop: &AtomicBool,
) -> bool {
    loop {
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(returned)) => {
                if stop.load(Ordering::Acquire) {
                    return false;
                }
                message = returned;
                thread::sleep(IDLE_POLL);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteDestination {
    Pty,
    Control,
}

struct PendingWrite {
    destination: WriteDestination,
    bytes: Box<[u8]>,
    written: usize,
}

#[derive(Default)]
struct PendingWrites {
    writes: VecDeque<PendingWrite>,
    bytes: usize,
}

impl PendingWrites {
    fn available(&self) -> usize {
        MAX_PENDING_WRITE_BYTES.saturating_sub(self.bytes)
    }

    fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    fn push(
        &mut self,
        destination: WriteDestination,
        bytes: impl Into<Box<[u8]>>,
    ) -> Result<(), RuntimeError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Ok(());
        }
        let Some(total) = self.bytes.checked_add(bytes.len()) else {
            return Err(RuntimeError::PendingInputFull);
        };
        if total > MAX_PENDING_WRITE_BYTES {
            return Err(RuntimeError::PendingInputFull);
        }
        self.bytes = total;
        self.writes.push_back(PendingWrite {
            destination,
            bytes,
            written: 0,
        });
        Ok(())
    }

    fn advance_front(&mut self, written: usize) -> Result<(), RuntimeError> {
        let Some(front) = self.writes.front_mut() else {
            return Err(RuntimeError::ProtocolInvariant(
                "write progress without pending bytes",
            ));
        };
        let remaining = front.bytes.len().saturating_sub(front.written);
        if written > remaining {
            return Err(RuntimeError::ProtocolInvariant(
                "transport over-reported write progress",
            ));
        }
        front.written += written;
        self.bytes -= written;
        if front.written == front.bytes.len() {
            self.writes.pop_front();
        }
        Ok(())
    }
}

struct SessionDriver {
    integration: PtyIntegration,
    signals: SignalEvents,
    decoder: ShellEventDecoder,
    pending: PendingWrites,
    sync_probe_sequence: &'static [u8],
    output_receiver: mpsc::Receiver<ReaderMessage>,
    reader_state: ReadState,
    integration_state: ReadState,
    input_state: InputState,
    child_exit: Option<ChildExit>,
    exit_drain_deadline: Option<Instant>,
    escape_deadline: Option<Instant>,
    reload_request: Option<ReloadRequest>,
    output: SerializedOutput<io::Stdout>,
    screen: ScreenObserver,
    renderer: OverlayRenderer,
    overlay_options: OverlayOptions,
    footer_hint: String,
    completion: Option<LocalCompletionDispatcher>,
    ai: Option<AiCompletionDispatcher>,
    ai_configured: bool,
    updater: Option<RuntimeUpdateWorker>,
    persist_mode: bool,
    local_ranking: Option<LocalRankingSnapshot>,
    worker_diagnostics: WorkerDiagnosticState,
}

struct DriverTransport {
    integration: PtyIntegration,
    signals: SignalEvents,
    output_receiver: mpsc::Receiver<ReaderMessage>,
    output: SerializedOutput<io::Stdout>,
}

struct RuntimeServices {
    completion: Option<LocalCompletionDispatcher>,
    ai: Option<AiCompletionDispatcher>,
    updater: Option<RuntimeUpdateWorker>,
    persist_mode: bool,
}

#[derive(Default)]
struct WorkerDiagnosticState {
    completion_unavailable_reported: bool,
    ai_unavailable_reported: bool,
    updater_unavailable_reported: bool,
    learning_failures: u64,
    update_network_failures: u64,
    update_persistence_failures: u64,
}

struct LocalRankingSnapshot {
    generation: u64,
    candidates: Vec<Suggestion>,
}

fn order_merged_candidates(
    candidates: &mut [Suggestion],
    query: &CompletionQuery,
    local: Option<&LocalRankingSnapshot>,
) {
    let local_positions = local.map_or_else(BTreeMap::new, |snapshot| {
        snapshot
            .candidates
            .iter()
            .filter_map(|candidate| candidate.resulting_line(query).ok())
            .enumerate()
            .map(|(position, line)| (line, position))
            .collect::<BTreeMap<_, _>>()
    });
    candidates.sort_by(|left, right| {
        let left_position = left
            .resulting_line(query)
            .ok()
            .and_then(|line| local_positions.get(&line).copied())
            .unwrap_or(usize::MAX);
        let right_position = right
            .resulting_line(query)
            .ok()
            .and_then(|line| local_positions.get(&line).copied())
            .unwrap_or(usize::MAX);
        left_position
            .cmp(&right_position)
            .then_with(|| right.static_priority().total_cmp(&left.static_priority()))
            .then_with(|| right.confidence().total_cmp(&left.confidence()))
            .then_with(|| left.identity().cmp(right.identity()))
    });
}

struct InitialOverlay {
    screen_size: TerminalSize,
    initial_cursor: Option<CursorPosition>,
    options: OverlayOptions,
    footer_hint: String,
}

fn screen_at_cursor(size: TerminalSize, cursor: Option<CursorPosition>) -> ScreenObserver {
    let mut screen = ScreenObserver::new(size);
    if cursor.is_none_or(|cursor| screen.synchronize(cursor).is_err()) {
        screen.desynchronize();
    }
    screen
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadState {
    Open,
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputState {
    Open,
    EofPending { written: usize },
    Closed,
}

struct PendingConfiguration {
    settings: Settings,
    /// Requests answered only once this configuration takes effect.
    ///
    /// A deferred configuration can be superseded by a newer one before the
    /// reducer accepts it. The requests that were waiting on the superseded
    /// configuration carry over, because each shell-side reload command blocks
    /// on its acknowledgment; dropping one leaves that command hanging until
    /// its timeout.
    acknowledgments: Vec<ReloadRequest>,
}

struct LiveConfiguration {
    settings: Settings,
    reloader: Option<ConfigReloader>,
    pending: Option<PendingConfiguration>,
    reported_failure: Option<String>,
    started: Instant,
}

impl LiveConfiguration {
    fn new(settings: Settings, reloader: Option<ConfigReloader>) -> Self {
        Self {
            settings,
            reloader,
            pending: None,
            reported_failure: None,
            started: Instant::now(),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn poll(
        &mut self,
        driver: &mut SessionDriver,
        reducer: &mut SessionReducer,
        diagnostics: Option<&dyn RuntimeDiagnosticSink>,
    ) -> Result<bool, RuntimeError> {
        if self.apply_pending(driver, reducer)? {
            return Ok(true);
        }

        let request = driver.reload_request.take();
        let now_ms = self.now_ms();
        let Some(reloader) = self.reloader.as_mut() else {
            if let Some(request) = request {
                driver.enqueue_reload_ack(request, false)?;
                return Ok(true);
            }
            return Ok(false);
        };
        let outcome = if request.is_some() {
            reloader.reload_now(now_ms)
        } else {
            reloader.poll(now_ms)
        };
        match outcome {
            ReloadPoll::Applied { delta, .. } => {
                self.reported_failure = None;
                if let Some(diagnostics) = diagnostics {
                    diagnostics.record("reload", "configuration replacement applied");
                    if delta.contains(ReloadChange::ShellForNextSession) {
                        diagnostics.record(
                            "reload",
                            "configured shell changed; the change applies to the next wrapper session",
                        );
                    }
                }
                let settings = reloader.shared_settings().snapshot().settings().clone();
                if let (Some(diagnostics), Err(error)) = (diagnostics, settings.ai.readiness()) {
                    diagnostics.record(
                        "ai",
                        &format!("AI completion is unavailable after reload: {error}"),
                    );
                }
                let mut acknowledgments =
                    Self::carried_acknowledgments(self.pending.take(), request);
                Self::bound_acknowledgments(driver, &mut acknowledgments)?;
                self.pending = Some(PendingConfiguration {
                    settings,
                    acknowledgments,
                });
                self.apply_pending(driver, reducer)
            }
            ReloadPoll::Unchanged => {
                self.reported_failure = None;
                let Some(request) = request else {
                    return Ok(false);
                };
                // An unchanged file with a replacement still deferred means the
                // requested configuration is not yet in effect; the answer
                // waits with the deferred replacement instead of claiming
                // completion early.
                if let Some(pending) = self.pending.as_mut() {
                    pending.acknowledgments.push(request);
                    Self::bound_acknowledgments(driver, &mut pending.acknowledgments)?;
                    return Ok(false);
                }
                driver.enqueue_reload_ack(request, true)?;
                Ok(true)
            }
            ReloadPoll::Rejected => {
                if let Some(failure) = reloader.last_failure() {
                    let message = format!("configuration replacement rejected: {failure}");
                    if self.reported_failure.as_deref() != Some(&message) {
                        if let Some(diagnostics) = diagnostics {
                            diagnostics.record("reload", &message);
                        }
                        self.reported_failure = Some(message);
                    }
                }
                if let Some(request) = request {
                    driver.enqueue_reload_ack(request, false)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            ReloadPoll::NotDue { .. } => Ok(false),
        }
    }

    fn apply_pending(
        &mut self,
        driver: &mut SessionDriver,
        reducer: &mut SessionReducer,
    ) -> Result<bool, RuntimeError> {
        let Some(pending) = self.pending.take() else {
            return Ok(false);
        };
        let keybindings = pending
            .settings
            .keybindings
            .resolve()
            .map_err(|error| RuntimeError::Configuration(error.to_string()))?;
        let effects = reducer
            .reconfigure(
                keybindings.binding(KeybindingAction::ToggleMode).sequence(),
                keybindings.binding(KeybindingAction::ToggleMenu).sequence(),
                usize::from(pending.settings.ui.max_suggestions),
            )
            .map_err(|error| RuntimeError::Configuration(error.to_string()))?;
        let Some(effects) = effects else {
            self.pending = Some(pending);
            return Ok(false);
        };
        driver.configure_runtime(&pending.settings, &keybindings);
        driver.apply_effects(reducer, effects)?;
        self.settings = pending.settings;
        for request in pending.acknowledgments {
            driver.enqueue_reload_ack(request, true)?;
        }
        Ok(true)
    }

    /// Carries requests waiting on a superseded configuration to its successor.
    fn carried_acknowledgments(
        superseded: Option<PendingConfiguration>,
        request: Option<ReloadRequest>,
    ) -> Vec<ReloadRequest> {
        let mut acknowledgments =
            superseded.map_or_else(Vec::new, |pending| pending.acknowledgments);
        acknowledgments.extend(request);
        acknowledgments
    }

    /// Answers the oldest waiting requests beyond the retained bound.
    ///
    /// A client issuing reloads faster than the reducer can accept them would
    /// otherwise grow the waiting list without limit. The oldest requests are
    /// acknowledged immediately: their replacement was accepted and only its
    /// session activation is still pending behind newer configurations.
    fn bound_acknowledgments(
        driver: &mut SessionDriver,
        acknowledgments: &mut Vec<ReloadRequest>,
    ) -> Result<(), RuntimeError> {
        let Some(excess) = acknowledgments
            .len()
            .checked_sub(MAX_WAITING_RELOAD_ACKNOWLEDGMENTS)
        else {
            return Ok(());
        };
        for request in acknowledgments.drain(..excess) {
            driver.enqueue_reload_ack(request, true)?;
        }
        Ok(())
    }
}

fn drive_session(
    session: &mut PtySession,
    driver: &mut SessionDriver,
    reducer: &mut SessionReducer,
    terminal: &mut TerminalGuard<io::Stdin, io::Stdout>,
    live_configuration: &mut LiveConfiguration,
    diagnostics: Option<&dyn RuntimeDiagnosticSink>,
) -> Result<ChildExit, RuntimeError> {
    let mut input = io::stdin();
    let mut input_buffer = [0_u8; INPUT_CHUNK_BYTES];
    let mut integration_buffer = vec![0_u8; EVENT_CHUNK_BYTES];

    loop {
        let mut progress = false;
        progress |= driver.handle_signals(session, terminal)?;
        progress |= live_configuration.poll(driver, reducer, diagnostics)?;
        progress |= driver.flush_pending(session)?;
        progress |= driver.drain_output(reducer)?;
        progress |= driver.drain_integration(reducer, &mut integration_buffer)?;
        progress |= driver.drain_completion(reducer, diagnostics)?;
        driver.report_worker_diagnostics(diagnostics);
        progress |= driver.poll_child(session)?;
        progress |= driver.read_input(session, reducer, &mut input, &mut input_buffer)?;
        progress |= driver.flush_timed_input(reducer)?;
        progress |= driver.close_input_if_ready(session)?;
        progress |= driver.flush_pending(session)?;

        if let Some(exit) = driver.completed_exit() {
            if driver.integration_state == ReadState::Open {
                driver.finish_decoder(reducer)?;
            }
            return Ok(exit);
        }
        if !progress {
            thread::sleep(IDLE_POLL);
        }
    }
}

impl SessionDriver {
    fn new(
        transport: DriverTransport,
        overlay: InitialOverlay,
        services: RuntimeServices,
        sync_probe_sequence: &'static [u8],
    ) -> Self {
        let ai_configured = services.ai.is_some();
        let screen = screen_at_cursor(overlay.screen_size, overlay.initial_cursor);
        Self {
            integration: transport.integration,
            signals: transport.signals,
            decoder: ShellEventDecoder::new(StreamEpoch::INITIAL),
            pending: PendingWrites::default(),
            sync_probe_sequence,
            output_receiver: transport.output_receiver,
            reader_state: ReadState::Open,
            integration_state: ReadState::Open,
            input_state: InputState::Open,
            child_exit: None,
            exit_drain_deadline: None,
            escape_deadline: None,
            reload_request: None,
            output: transport.output,
            screen,
            renderer: OverlayRenderer::new(),
            overlay_options: overlay.options,
            footer_hint: overlay.footer_hint,
            completion: services.completion,
            ai: services.ai,
            ai_configured,
            updater: services.updater,
            persist_mode: services.persist_mode,
            local_ranking: None,
            worker_diagnostics: WorkerDiagnosticState::default(),
        }
    }

    fn handle_signals(
        &mut self,
        session: &PtySession,
        terminal: &mut TerminalGuard<io::Stdin, io::Stdout>,
    ) -> Result<bool, RuntimeError> {
        let mut events = [None; 7];
        let count = self.signals.drain_pending(&mut events);
        for event in events.into_iter().take(count).flatten() {
            let signal = match event {
                SignalEvent::Resize => {
                    self.resize(session, terminal)?;
                    continue;
                }
                SignalEvent::Interrupt => ForwardSignal::Interrupt,
                SignalEvent::Quit => ForwardSignal::Quit,
                SignalEvent::Terminate => ForwardSignal::Terminate,
                SignalEvent::Hangup => ForwardSignal::Hangup,
                SignalEvent::Suspend => {
                    self.suspend(session, terminal)?;
                    continue;
                }
                SignalEvent::Continue => ForwardSignal::Continue,
            };
            // Once the child is reaped there is no foreground group left to
            // receive the signal; a termination request then applies to the
            // wrapper itself and ends the trailing-output drain immediately.
            if self.child_exit.is_some() {
                if matches!(
                    signal,
                    ForwardSignal::Interrupt
                        | ForwardSignal::Quit
                        | ForwardSignal::Terminate
                        | ForwardSignal::Hangup
                ) {
                    self.exit_drain_deadline = Some(Instant::now());
                }
                continue;
            }
            session
                .forward_signal(signal)
                .map_err(|error| RuntimeError::Write(error.to_string()))?;
        }
        Ok(count != 0)
    }

    fn resize(
        &mut self,
        session: &PtySession,
        terminal: &mut TerminalGuard<io::Stdin, io::Stdout>,
    ) -> Result<(), RuntimeError> {
        let dimensions = TerminalDimensions::from_terminal(&io::stdin())
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        session
            .resize(dimensions.size())
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        terminal
            .update_dimensions(dimensions)
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        let size = TerminalSize::new(dimensions.size().cols, dimensions.size().rows)
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        let _ = self.screen.resize(size);
        let frame = self
            .renderer
            .on_resize(self.screen.snapshot())
            .map_err(|error| RuntimeError::Output(error.to_string()))?;
        let (transaction, _) = frame.into_parts();
        self.write_overlay_transaction(&transaction)
    }

    fn suspend(
        &mut self,
        session: &PtySession,
        terminal: &mut TerminalGuard<io::Stdin, io::Stdout>,
    ) -> Result<(), RuntimeError> {
        self.clear_overlay()?;
        terminal
            .restore()
            .map_err(|error| RuntimeError::Restore(error.to_string()))?;
        session
            .forward_signal(ForwardSignal::Suspend)
            .map_err(|error| RuntimeError::Write(error.to_string()))?;
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGSTOP)
            .map_err(|error| RuntimeError::Setup(io::Error::from(error).kind().to_string()))?;

        let dimensions = TerminalDimensions::from_terminal(&io::stdin())
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        let resumed_terminal = TerminalGuard::enter_stdio(dimensions)
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        let size = TerminalSize::new(dimensions.size().cols, dimensions.size().rows)
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        let mut parent_input = io::stdin();
        let resumed = probe_terminal_cursor(&mut parent_input, &resumed_terminal.output(), size)?;
        session
            .resize(dimensions.size())
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        self.screen = screen_at_cursor(size, resumed.cursor);
        self.pending
            .push(WriteDestination::Pty, resumed.pending_input)?;
        *terminal = resumed_terminal;
        session
            .forward_signal(ForwardSignal::Continue)
            .map_err(|error| RuntimeError::Write(error.to_string()))?;
        Ok(())
    }

    fn flush_pending(&mut self, session: &mut PtySession) -> Result<bool, RuntimeError> {
        let mut progress = false;
        loop {
            let Some(front) = self.pending.writes.front() else {
                return Ok(progress);
            };
            let remaining = &front.bytes[front.written..];
            let write = match front.destination {
                WriteDestination::Pty => session.write_input(remaining),
                WriteDestination::Control => self.integration.write_control(remaining),
            };
            let written = write
                .map_err(|error| RuntimeError::Write(error.to_string()))?
                .written(remaining.len());
            self.pending.advance_front(written)?;
            progress |= written != 0;
            if written == 0 {
                return Ok(progress);
            }
        }
    }

    fn drain_output(&mut self, reducer: &mut SessionReducer) -> Result<bool, RuntimeError> {
        let mut progress = false;
        for _ in 0..MAX_DRAIN_PER_TICK {
            match self.output_receiver.try_recv() {
                Ok(ReaderMessage::Bytes(bytes)) => {
                    let effects = reducer.observe_shell_output();
                    self.apply_effects(reducer, effects)?;
                    self.clear_overlay()?;
                    let _ = self.screen.observe(&bytes);
                    self.output
                        .write_shell(&bytes)
                        .map_err(|error| RuntimeError::Output(error.to_string()))?;
                    progress = true;
                }
                Ok(ReaderMessage::Eof) => {
                    self.reader_state = ReadState::Eof;
                    return Ok(true);
                }
                Ok(ReaderMessage::Error(error)) => {
                    return Err(RuntimeError::Read(error.to_string()));
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(progress),
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.reader_state == ReadState::Open {
                        return Err(RuntimeError::Read(
                            "PTY reader disconnected before EOF".to_owned(),
                        ));
                    }
                    return Ok(progress);
                }
            }
        }
        Ok(progress)
    }

    fn drain_integration(
        &mut self,
        reducer: &mut SessionReducer,
        buffer: &mut [u8],
    ) -> Result<bool, RuntimeError> {
        if self.integration_state == ReadState::Eof {
            return Ok(false);
        }
        let mut progress = false;
        for _ in 0..MAX_DRAIN_PER_TICK {
            match self
                .integration
                .read_events(buffer)
                .map_err(|error| RuntimeError::Read(error.to_string()))?
            {
                IntegrationRead::Bytes(read) => {
                    let mut frames = Vec::new();
                    self.decoder
                        .push(&buffer[..read], |frame| frames.push(frame));
                    for frame in frames {
                        self.apply_frame(reducer, frame)?;
                    }
                    progress = true;
                }
                IntegrationRead::Pending => return Ok(progress),
                IntegrationRead::Eof => {
                    self.integration_state = ReadState::Eof;
                    self.finish_decoder(reducer)?;
                    return Ok(true);
                }
            }
        }
        Ok(progress)
    }

    fn drain_completion(
        &mut self,
        reducer: &mut SessionReducer,
        diagnostics: Option<&dyn RuntimeDiagnosticSink>,
    ) -> Result<bool, RuntimeError> {
        let alias_expansions = self
            .completion
            .as_ref()
            .map_or_else(Vec::new, |completion| {
                completion.drain_alias_expansions(MAX_ALIAS_EXPANSION_DRAIN)
            });
        let mut progress = !alias_expansions.is_empty();
        for expansion in alias_expansions {
            let (generation, edit) = expansion.into_parts();
            let effects = reducer.apply_alias_expansion(generation, edit);
            self.apply_effects(reducer, effects)?;
        }

        let mut batches = Vec::new();
        if let Some(completion) = self.completion.as_ref() {
            batches.extend(completion.drain_batches(MAX_COMPLETION_DRAIN_BATCHES));
        }
        if let Some(ai) = self.ai.as_ref() {
            batches.extend(ai.drain_batches(MAX_AI_DRAIN_BATCHES));
        }
        progress |= !batches.is_empty();
        for batch in batches {
            if let (Some(diagnostics), Some(error)) = (diagnostics, batch.error.as_deref()) {
                diagnostics.record(batch.provider, error);
            }
            self.accept_provider_batch(reducer, batch)?;
        }
        Ok(progress)
    }

    fn report_worker_diagnostics(&mut self, diagnostics: Option<&dyn RuntimeDiagnosticSink>) {
        let Some(diagnostics) = diagnostics else {
            return;
        };
        let completion_alive = self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.status().alive);
        if !completion_alive && !self.worker_diagnostics.completion_unavailable_reported {
            diagnostics.record(
                "completion",
                "local completion worker is unavailable; shell forwarding continues",
            );
            self.worker_diagnostics.completion_unavailable_reported = true;
        }
        if let Some(completion) = self.completion.as_ref() {
            let failures = completion.status().learning_failures;
            if failures > self.worker_diagnostics.learning_failures {
                diagnostics.record(
                    "learning",
                    "local learning operation failed; in-memory completion continues",
                );
                self.worker_diagnostics.learning_failures = failures;
            }
        }

        let ai_alive = self.ai.as_ref().is_some_and(|ai| ai.status().alive);
        if !ai_alive && !self.worker_diagnostics.ai_unavailable_reported {
            diagnostics.record(
                "ai",
                "AI completion worker is unavailable; local completion continues",
            );
            self.worker_diagnostics.ai_unavailable_reported = true;
        }

        let update_alive = self
            .updater
            .as_ref()
            .is_some_and(|updater| updater.status().alive);
        if !update_alive && !self.worker_diagnostics.updater_unavailable_reported {
            diagnostics.record(
                "updater",
                "automatic update worker is unavailable; the manual update command remains usable",
            );
            self.worker_diagnostics.updater_unavailable_reported = true;
        }
        if let Some(updater) = self.updater.as_ref() {
            let status = updater.status();
            if status.network_failures > self.worker_diagnostics.update_network_failures {
                diagnostics.record("updater", "automatic release check failed");
                self.worker_diagnostics.update_network_failures = status.network_failures;
            }
            if status.persistence_failures > self.worker_diagnostics.update_persistence_failures {
                diagnostics.record("updater", "automatic update state persistence failed");
                self.worker_diagnostics.update_persistence_failures = status.persistence_failures;
            }
        }
    }

    fn accept_provider_batch(
        &mut self,
        reducer: &mut SessionReducer,
        batch: ProviderBatch,
    ) -> Result<(), RuntimeError> {
        let generation = batch.generation;
        if batch.provider == LOCAL_COMPLETION_PROVIDER {
            self.local_ranking = Some(LocalRankingSnapshot {
                generation,
                candidates: batch.suggestions.clone(),
            });
        }
        if !matches!(
            reducer.accept_provider_batch(batch),
            BatchOutcome::Accepted(_)
        ) {
            return Ok(());
        }
        let Ok(mut ranked) = reducer.merged_candidates(generation) else {
            return Ok(());
        };
        if let Some(query) = reducer.active_query() {
            order_merged_candidates(
                &mut ranked,
                query,
                self.local_ranking
                    .as_ref()
                    .filter(|snapshot| snapshot.generation == generation),
            );
        }
        let (_, effects) = reducer
            .apply_ranked_candidates(generation, ranked)
            .into_parts();
        self.apply_effects(reducer, effects)
    }

    fn finish_decoder(&mut self, reducer: &mut SessionReducer) -> Result<(), RuntimeError> {
        if let Some(frame) = self.decoder.finish() {
            self.apply_frame(reducer, frame)?;
        }
        self.integration_state = ReadState::Eof;
        Ok(())
    }

    fn apply_frame(
        &mut self,
        reducer: &mut SessionReducer,
        frame: DecodedFrame,
    ) -> Result<(), RuntimeError> {
        let (update, effects) = reducer.apply_shell_frame(frame);
        self.apply_effects(reducer, effects)
            .and_then(|()| self.observe_state_update(&update, reducer))
    }

    fn observe_state_update(
        &mut self,
        update: &StateUpdate,
        reducer: &SessionReducer,
    ) -> Result<(), RuntimeError> {
        match update {
            StateUpdate::WorkingDirectoryChanged(directory) => {
                if let Some(completion) = self.completion.as_ref() {
                    let _ignored = completion.update_cwd(directory.as_path());
                }
            }
            StateUpdate::CommandStopped(completed) => {
                if let (Some(command), Some(timestamp)) =
                    (completed.command().as_str(), unix_seconds_now())
                {
                    let outcome = if completed.status().success() {
                        CommandOutcome::Success
                    } else {
                        CommandOutcome::Failure
                    };
                    if let Some(completion) = self.completion.as_ref() {
                        let _ignored = completion.record_completed_command(
                            command,
                            reducer.cwd(),
                            timestamp,
                            outcome,
                        );
                    }
                    if let Some(ai) = self.ai.as_ref() {
                        let _ignored = ai.record_completed_command(command);
                    }
                }
                self.completed_command_boundary()?;
            }
            StateUpdate::CommandStoppedWithoutAttribution(_) => {
                self.completed_command_boundary()?;
            }
            StateUpdate::CommandStarted { .. } => {
                if let Some(ai) = self.ai.as_ref() {
                    ai.cancel(CancellationReason::CommandExecution);
                }
            }
            StateUpdate::ReloadRequested(request) => {
                if self.reload_request.is_some() {
                    self.enqueue_reload_ack(*request, false)?;
                } else {
                    self.reload_request = Some(*request);
                }
            }
            StateUpdate::BufferSynchronized { .. }
            | StateUpdate::PromptReady { .. }
            | StateUpdate::CapabilityChanged(_)
            | StateUpdate::FrameRejected(_)
            | StateUpdate::LifecycleRejected(_)
            | StateUpdate::LifecycleSuppressed
            | StateUpdate::SnapshotRejected(_)
            | StateUpdate::StreamOrderRejected { .. } => {}
        }
        Ok(())
    }

    fn completed_command_boundary(&mut self) -> Result<(), RuntimeError> {
        let notice = self.updater.as_ref().and_then(|updater| {
            let _admission = updater.completed_command();
            updater.take_notification()
        });
        let Some(notice) = notice else {
            return Ok(());
        };
        self.clear_overlay()?;
        let line = format!("\r\n{notice}\r\n");
        let _ = self.screen.observe(line.as_bytes());
        self.output
            .write_shell(line.as_bytes())
            .map_err(|error| RuntimeError::Output(error.to_string()))
    }

    fn enqueue_reload_ack(
        &mut self,
        request: ReloadRequest,
        accepted: bool,
    ) -> Result<(), RuntimeError> {
        let disposition = if accepted { "ok" } else { "rejected" };
        let prefix = std::str::from_utf8(RELOAD_ACK_PREFIX)
            .map_err(|_| RuntimeError::ProtocolInvariant("reload prefix is not UTF-8"))?;
        let frame = format!("{prefix}{}:{disposition}\0", request.nonce());
        self.pending
            .push(WriteDestination::Control, frame.into_bytes())
    }

    fn read_input(
        &mut self,
        session: &PtySession,
        reducer: &mut SessionReducer,
        input: &mut io::Stdin,
        buffer: &mut [u8],
    ) -> Result<bool, RuntimeError> {
        if !matches!(self.input_state, InputState::Open)
            || self.child_exit.is_some()
            || self.pending.available()
                < buffer.len() + crate::shell_control::MAX_CONTROL_WIRE_BYTES
        {
            return Ok(false);
        }
        let Some(read) = read_if_ready(input, buffer)? else {
            return Ok(false);
        };
        match read {
            Ok(0) => {
                self.input_state = InputState::EofPending { written: 0 };
                self.escape_deadline = None;
                let (_, effects) = reducer.finish_input().into_parts();
                self.apply_effects(reducer, effects)?;
                Ok(true)
            }
            Ok(read) => {
                if Self::interception_allowed(session, reducer) {
                    self.route_input(reducer, &buffer[..read])?;
                    self.escape_deadline = Some(Instant::now() + STANDALONE_ESCAPE_TIMEOUT);
                } else {
                    let effects = reducer.observe_shell_output();
                    self.apply_effects(reducer, effects)?;
                    self.pending.push(WriteDestination::Pty, &buffer[..read])?;
                }
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
            Err(error) => Err(RuntimeError::Input(error.kind())),
        }
    }

    fn flush_timed_input(&mut self, reducer: &mut SessionReducer) -> Result<bool, RuntimeError> {
        let Some(deadline) = self.escape_deadline else {
            return Ok(false);
        };
        if Instant::now() < deadline {
            return Ok(false);
        }
        self.escape_deadline = None;
        let (_, effects) = reducer.flush_input().into_parts();
        self.apply_effects(reducer, effects)?;
        Ok(true)
    }

    fn interception_allowed(session: &PtySession, reducer: &SessionReducer) -> bool {
        matches!(
            session.foreground_state(),
            ForegroundState::Available {
                shell_owns_terminal: true,
                ..
            }
        ) && reducer.shell().foreground() == ForegroundCommandState::Idle
    }

    fn route_input(
        &mut self,
        reducer: &mut SessionReducer,
        input: &[u8],
    ) -> Result<(), RuntimeError> {
        if let Some(ai) = self.ai.as_ref() {
            ai.cancel(CancellationReason::BufferChanged);
        }
        let mut consumed = 0;
        while consumed < input.len() {
            let reduction = reducer.route_input(&input[consumed..]);
            let (advanced, effects) = reduction.into_parts();
            if advanced == 0 || advanced > input.len() - consumed {
                return Err(RuntimeError::ProtocolInvariant(
                    "input reducer reported invalid progress",
                ));
            }
            consumed += advanced;
            self.apply_effects(reducer, effects)?;
        }
        Ok(())
    }

    fn apply_effects(
        &mut self,
        reducer: &mut SessionReducer,
        effects: EffectBatch,
    ) -> Result<(), RuntimeError> {
        let mut replacement = None;
        let mut staged = Vec::new();

        for effect in effects.into_effects() {
            match effect {
                SessionEffect::ForwardInput(bytes) => {
                    if replacement.is_some() {
                        staged.push(bytes);
                    } else {
                        self.pending.push(WriteDestination::Pty, bytes)?;
                    }
                }
                SessionEffect::ReplaceBuffer(value) => {
                    if let Some(ai) = self.ai.as_ref() {
                        ai.cancel(CancellationReason::BufferChanged);
                    }
                    if replacement.replace(value).is_some() {
                        return Err(RuntimeError::ProtocolInvariant(
                            "multiple replacements preceded one synchronization request",
                        ));
                    }
                }
                SessionEffect::RequestBufferSync(nonce) => {
                    if let Some(value) = replacement.take() {
                        self.enqueue_replacement(value.as_str(), value.cursor(), nonce.get())?;
                        for bytes in staged.drain(..) {
                            self.pending.push(WriteDestination::Pty, bytes)?;
                        }
                    }
                    self.pending
                        .push(WriteDestination::Pty, self.sync_probe_sequence)?;
                }
                SessionEffect::StartQuery {
                    mode,
                    alias_expansion,
                    work,
                } => {
                    if let Some(completion) = self.completion.as_ref() {
                        let _admission = completion.submit_query_with_alias_expansion(
                            mode,
                            work.clone(),
                            alias_expansion,
                        );
                    }
                    if let Some(ai) = self.ai.as_ref() {
                        if self.ai_configured && mode == crate::session::SessionMode::Spec {
                            let _admission = ai.submit_query(work);
                        } else {
                            ai.cancel(CancellationReason::ModeChanged);
                        }
                    }
                }
                SessionEffect::ClearOverlay => self.clear_overlay()?,
                SessionEffect::RefreshOverlay => {
                    self.refresh_overlay(reducer)?;
                }
                SessionEffect::ModeChanged(mode) => {
                    if let Some(ai) = self.ai.as_ref() {
                        ai.cancel(CancellationReason::ModeChanged);
                    }
                    if self.persist_mode {
                        if let Some(updater) = self.updater.as_ref() {
                            let _admission = updater.record_mode(mode);
                        }
                    }
                    self.refresh_overlay(reducer)?;
                }
                SessionEffect::Fault(_) => {
                    return Err(RuntimeError::ProtocolInvariant(
                        "session reducer entered a closed fault state",
                    ));
                }
            }
        }
        if replacement.is_some() || !staged.is_empty() {
            return Err(RuntimeError::ProtocolInvariant(
                "buffer replacement lacked a following synchronization request",
            ));
        }
        Ok(())
    }

    fn refresh_overlay(&mut self, reducer: &SessionReducer) -> Result<(), RuntimeError> {
        let Some(query) = reducer.active_query() else {
            return self.clear_overlay();
        };
        let frame = self
            .renderer
            .render(
                self.screen.snapshot(),
                OverlayRequest::new(query, reducer.selection()).with_footer_hint(&self.footer_hint),
                self.overlay_options,
            )
            .map_err(|error| RuntimeError::Output(error.to_string()))?;
        let (transaction, _) = frame.into_parts();
        self.write_overlay_transaction(&transaction)
    }

    fn clear_overlay(&mut self) -> Result<(), RuntimeError> {
        let frame = self
            .renderer
            .clear(self.screen.snapshot())
            .map_err(|error| RuntimeError::Output(error.to_string()))?;
        let (transaction, _) = frame.into_parts();
        self.write_overlay_transaction(&transaction)
    }

    fn clear_overlay_after_failure(&mut self) -> Result<(), RuntimeError> {
        let frame = self
            .renderer
            .on_failure(self.screen.snapshot())
            .map_err(|error| RuntimeError::Output(error.to_string()))?;
        let (transaction, _) = frame.into_parts();
        self.write_overlay_transaction(&transaction)
    }

    fn write_overlay_transaction(
        &mut self,
        transaction: &RenderTransaction,
    ) -> Result<(), RuntimeError> {
        if !transaction.is_empty() {
            self.output
                .write_overlay(transaction.bytes())
                .map_err(|error| RuntimeError::Output(error.to_string()))?;
        }
        self.renderer
            .acknowledge_transaction(transaction)
            .map_err(|error| RuntimeError::Output(error.to_string()))
    }

    fn configure_runtime(
        &mut self,
        settings: &Settings,
        keybindings: &crate::keybindings::ResolvedKeybindings,
    ) {
        self.overlay_options = OverlayOptions::from_ui(settings.ui);
        self.footer_hint = footer_hint(keybindings);
        self.persist_mode = settings.core.mode == Mode::Last;
        if let Some(completion) = self.completion.as_ref() {
            let _accepted = completion.reconfigure(settings.clone());
        }
        if let Some(ai) = self.ai.as_ref() {
            self.ai_configured = ai.reconfigure(settings.clone());
        } else {
            self.ai_configured = false;
        }
        if let Some(updater) = self.updater.as_ref() {
            let _accepted = updater.reconfigure(settings.updater);
        }
    }

    fn enqueue_replacement(
        &mut self,
        text: &str,
        cursor: usize,
        nonce: u64,
    ) -> Result<(), RuntimeError> {
        let request =
            ControlRequestId::new(nonce).map_err(|error| RuntimeError::Write(error.to_string()))?;
        let control = ReplacementControl::new(request, text.to_owned(), cursor)
            .map_err(|error| RuntimeError::Write(error.to_string()))?
            .encode();
        self.pending
            .push(WriteDestination::Control, control.as_bytes())
    }

    fn poll_child(&mut self, session: &mut PtySession) -> Result<bool, RuntimeError> {
        if self.child_exit.is_some() {
            return Ok(false);
        }
        let exit = session
            .try_wait()
            .map_err(|error| RuntimeError::Read(error.to_string()))?;
        self.child_exit = exit;
        if exit.is_some() {
            self.exit_drain_deadline = Some(Instant::now() + EXIT_OUTPUT_DRAIN);
        }
        Ok(exit.is_some())
    }

    fn close_input_if_ready(&mut self, session: &mut PtySession) -> Result<bool, RuntimeError> {
        let InputState::EofPending {
            written: previous_written,
        } = self.input_state
        else {
            return Ok(false);
        };
        if !self.pending.is_empty() {
            return Ok(false);
        }
        match session
            .close_input()
            .map_err(|error| RuntimeError::Write(error.to_string()))?
        {
            PtyClose::Closed => {
                self.input_state = InputState::Closed;
                Ok(true)
            }
            PtyClose::Pending { written, .. } => {
                self.input_state = InputState::EofPending { written };
                Ok(written > previous_written)
            }
        }
    }

    fn completed_exit(&self) -> Option<ChildExit> {
        completed_exit_after_drain(
            self.child_exit,
            self.reader_state,
            self.pending.is_empty(),
            self.exit_drain_deadline,
            Instant::now(),
        )
    }
}

/// Decides whether the session is over once the child has been reaped.
///
/// The clean path still requires the reader to observe end of file and the
/// pending queues to drain. Past the drain deadline the exit completes
/// anyway: the remaining reader traffic belongs to orphans holding the
/// slave, and queued writes have no shell left to read them.
fn completed_exit_after_drain(
    child_exit: Option<ChildExit>,
    reader_state: ReadState,
    pending_empty: bool,
    drain_deadline: Option<Instant>,
    now: Instant,
) -> Option<ChildExit> {
    let exit = child_exit?;
    if reader_state == ReadState::Eof && pending_empty {
        return Some(exit);
    }
    let deadline = drain_deadline?;
    (now >= deadline).then_some(exit)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::pty::openpty;

    use super::*;

    fn suggestion(query: &CompletionQuery, suffix: &str, identity: &str) -> Suggestion {
        Suggestion::new(
            crate::completion::TextEdit {
                range: query.cursor..query.cursor,
                replacement: suffix.to_owned(),
            },
            format!("{}{suffix}", query.line),
            "candidate",
            "test",
            crate::completion::SuggestionSource::Spec,
            crate::completion::InsertionBehavior::Exact,
            identity,
        )
    }

    #[test]
    fn exit_completes_on_reader_eof_or_past_the_drain_deadline() {
        let exit = Some(ChildExit::Exited(0));
        let now = Instant::now();
        let deadline = Some(now + EXIT_OUTPUT_DRAIN);
        let elapsed = now + EXIT_OUTPUT_DRAIN;

        assert_eq!(
            completed_exit_after_drain(None, ReadState::Open, true, None, now),
            None
        );
        assert_eq!(
            completed_exit_after_drain(exit, ReadState::Eof, true, deadline, now),
            exit
        );
        assert_eq!(
            completed_exit_after_drain(exit, ReadState::Open, true, deadline, now),
            None
        );
        assert_eq!(
            completed_exit_after_drain(exit, ReadState::Open, true, deadline, elapsed),
            exit
        );
        assert_eq!(
            completed_exit_after_drain(exit, ReadState::Eof, false, deadline, now),
            None
        );
        assert_eq!(
            completed_exit_after_drain(exit, ReadState::Eof, false, deadline, elapsed),
            exit
        );
    }

    #[test]
    fn a_superseded_configuration_keeps_the_requests_waiting_on_it() {
        let first = ReloadRequest::from_nonce(41);
        let second = ReloadRequest::from_nonce(42);
        let deferred = PendingConfiguration {
            settings: Settings::default(),
            acknowledgments: vec![first],
        };

        let carried = LiveConfiguration::carried_acknowledgments(Some(deferred), Some(second));

        assert_eq!(carried, vec![first, second]);
        assert!(LiveConfiguration::carried_acknowledgments(None, None).is_empty());
        assert_eq!(
            LiveConfiguration::carried_acknowledgments(None, Some(second)),
            vec![second]
        );
    }

    #[test]
    fn pending_writes_preserve_destination_order_and_exact_progress() {
        let mut pending = PendingWrites::default();
        pending
            .push(WriteDestination::Control, &b"control"[..])
            .unwrap();
        pending.push(WriteDestination::Pty, &b"probe"[..]).unwrap();

        pending.advance_front(3).unwrap();
        let front = pending.writes.front().unwrap();
        assert_eq!(front.destination, WriteDestination::Control);
        assert_eq!(&front.bytes[front.written..], b"trol");
        pending.advance_front(4).unwrap();
        assert_eq!(
            pending.writes.front().unwrap().destination,
            WriteDestination::Pty
        );
        assert_eq!(pending.bytes, 5);
    }

    #[test]
    fn pending_write_limit_rejects_without_mutating_queue() {
        let mut pending = PendingWrites::default();
        let oversized = vec![0_u8; MAX_PENDING_WRITE_BYTES + 1];
        assert!(matches!(
            pending.push(WriteDestination::Pty, oversized),
            Err(RuntimeError::PendingInputFull)
        ));
        assert!(pending.is_empty());
        assert_eq!(pending.bytes, 0);
    }

    #[test]
    fn cursor_report_uses_terminal_coordinates_and_stops_at_an_impossible_one() {
        let size = TerminalSize::new(80, 24).unwrap();
        let bytes = b"user\x1b[7;33Rtail";
        let (start, end, cursor) = cursor_position_report(bytes, size).unwrap();

        assert_eq!(cursor, CursorPosition::new(6, 32));
        assert_eq!(&bytes[start..end], b"\x1b[7;33R");
        assert!(cursor_position_report(b"\x1b[0;1R\x1b[1;81R", size).is_none());
        // An impossible report means the stream carries lookalike input, so a
        // later well-formed one is not treated as the terminal's answer.
        assert!(cursor_position_report(b"\x1b[999;1Rdata\x1b[7;33R", size).is_none());

        let mut screen = ScreenObserver::new(size);
        screen.synchronize(cursor).unwrap();
        let _ = screen.observe(b"\r\ngpg warning\r\nprompt> git");
        assert_eq!(screen.snapshot().cursor(), CursorPosition::new(8, 11));
    }

    #[test]
    fn cursor_probe_removes_only_the_report_and_preserves_early_input() {
        let (reader, writer) = nix::unistd::pipe().unwrap();
        let mut input = File::from(reader);
        let mut response = File::from(writer);
        response.write_all(b"g\x1b[7;33Ri").unwrap();
        let output = SerializedOutput::new(io::sink());

        let state =
            probe_terminal_cursor(&mut input, &output, TerminalSize::new(80, 24).unwrap()).unwrap();

        assert_eq!(state.cursor, Some(CursorPosition::new(6, 32)));
        assert_eq!(state.pending_input.as_ref(), b"gi");
    }

    #[test]
    fn shell_mapping_is_total() {
        assert_eq!(shell_kind(Shell::Bash), ShellKind::Bash);
        assert_eq!(shell_kind(Shell::Zsh), ShellKind::Zsh);
        assert_eq!(shell_kind(Shell::Fish), ShellKind::Fish);
        assert_eq!(sync_probe_sequence(ShellKind::Bash), SYNC_PROBE_SEQUENCE);
        assert_eq!(sync_probe_sequence(ShellKind::Zsh), SYNC_PROBE_SEQUENCE);
        assert_eq!(
            sync_probe_sequence(ShellKind::Fish),
            FISH_SYNC_PROBE_SEQUENCE
        );
    }

    #[test]
    fn fixed_mode_overrides_persisted_mode_and_last_defaults_to_spec() {
        assert_eq!(
            configured_initial_mode(Mode::Spec, Some(LastMode::History)),
            crate::session::SessionMode::Spec
        );
        assert_eq!(
            configured_initial_mode(Mode::History, Some(LastMode::Spec)),
            crate::session::SessionMode::History
        );
        assert_eq!(
            configured_initial_mode(Mode::Last, Some(LastMode::History)),
            crate::session::SessionMode::History
        );
        assert_eq!(
            configured_initial_mode(Mode::Last, None),
            crate::session::SessionMode::Spec
        );
    }

    #[test]
    fn late_ai_candidate_preserves_the_complete_local_ranking() {
        let query = CompletionQuery::new("git ch", 6, "/tmp", 7).unwrap();
        let cherry = suggestion(&query, "erry-pick", "cherry");
        let checkout = suggestion(&query, "eckout", "checkout");
        let ai = suggestion(&query, "at --amend", "ai");
        let local = LocalRankingSnapshot {
            generation: query.generation,
            candidates: vec![cherry.clone(), checkout.clone()],
        };
        let mut merged = vec![ai, checkout, cherry];

        order_merged_candidates(&mut merged, &query, Some(&local));

        let lines = merged
            .iter()
            .map(|candidate| candidate.resulting_line(&query).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            ["git cherry-pick", "git checkout", "git chat --amend"]
        );
    }

    #[test]
    fn input_readiness_does_not_change_shared_terminal_flags() {
        let pair = openpty(None, None).unwrap();
        let _master = File::from(pair.master);
        let mut input = File::from(pair.slave);
        let output = input.try_clone().unwrap();
        let before = fcntl(input.as_raw_fd(), FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .unwrap();

        let mut buffer = [0_u8; 1];
        assert!(read_if_ready(&mut input, &mut buffer).unwrap().is_none());
        let after_read = fcntl(output.as_raw_fd(), FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .unwrap();
        assert_eq!(after_read, before);
    }

    #[test]
    fn input_readiness_reads_available_bytes_and_reports_eof() {
        let (reader, writer) = nix::unistd::pipe().unwrap();
        let mut input = File::from(reader);
        let mut source = File::from(writer);
        let mut buffer = [0_u8; 8];

        assert!(read_if_ready(&mut input, &mut buffer).unwrap().is_none());
        source.write_all(b"Troy").unwrap();
        assert_eq!(
            read_if_ready(&mut input, &mut buffer)
                .unwrap()
                .unwrap()
                .unwrap(),
            4
        );
        assert_eq!(&buffer[..4], b"Troy");

        drop(source);
        assert_eq!(
            read_if_ready(&mut input, &mut buffer)
                .unwrap()
                .unwrap()
                .unwrap(),
            0
        );
    }
}
