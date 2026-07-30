//! Transparent interactive runtime for one wrapped shell session.
//!
//! The runtime owns transport and terminal lifecycle only. Completion provider
//! work is cancelled immediately until an application-level dispatcher is
//! wired; ordinary input and child output remain byte-for-byte transparent.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::unistd::isatty;

use crate::config::{Settings, Shell};
use crate::keybindings::KeybindingAction;
use crate::pty::{
    ChildExit, ForegroundState, ForwardSignal, IntegrationRead, PrivateSessionId, PtyClose,
    PtyIntegration, PtyRead, PtyReadError, PtyReader, PtySession, PtySpawnRequest, PtyStartup,
    ShellKind, ShellSelectionRequest, SignalEvent, SignalEvents, select_shell,
};
use crate::session::{EffectBatch, SessionEffect, SessionReducer};
use crate::shell_control::{ControlRequestId, ReplacementControl};
use crate::shell_events::{
    DecodedFrame, ForegroundCommandState, SYNC_PROBE_SEQUENCE, ShellEventDecoder, StreamEpoch,
};
use crate::terminal::{SerializedOutput, TerminalDimensions, TerminalGuard};

const INPUT_CHUNK_BYTES: usize = 4 * 1024;
const EVENT_CHUNK_BYTES: usize = 64 * 1024;
const OUTPUT_CHANNEL_CAPACITY: usize = 8;
const MAX_PENDING_WRITE_BYTES: usize = 512 * 1024;
const IDLE_POLL: Duration = Duration::from_millis(1);
const STANDALONE_ESCAPE_TIMEOUT: Duration = Duration::from_millis(25);
const MAX_DRAIN_PER_TICK: usize = 64;

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
    /// Parent standard-input flags could not be changed or restored.
    InputFlags(io::ErrorKind),
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
            Self::InputFlags(kind) => write!(formatter, "standard-input flags failed: {kind}"),
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
    let private_session = private_session_id()?;

    let mut startup =
        PtyStartup::acquire().map_err(|error| RuntimeError::Setup(error.to_string()))?;
    let mut terminal = TerminalGuard::enter_stdio(dimensions)
        .map_err(|error| RuntimeError::Setup(error.to_string()))?;
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
    let mut reducer = SessionReducer::new(
        StreamEpoch::INITIAL,
        keybindings.binding(KeybindingAction::ToggleMode).sequence(),
        keybindings.binding(KeybindingAction::ToggleMenu).sequence(),
        std::iter::empty(),
        usize::from(settings.ui.max_suggestions),
        working_directory,
    )
    .map_err(|error| RuntimeError::Setup(error.to_string()))?;

    let mut input_flags = NonblockingInput::enable(io::stdin().as_raw_fd())?;
    let reader_stop = Arc::new(AtomicBool::new(false));
    let (output_sender, output_receiver) = mpsc::sync_channel(OUTPUT_CHANNEL_CAPACITY);
    let reader_thread = spawn_reader(reader, output_sender, Arc::clone(&reader_stop))?;
    let output = terminal.output();
    let mut driver = SessionDriver::new(integration, signals, output_receiver);

    let result = drive_session(
        &mut session,
        &mut driver,
        &mut reducer,
        &output,
        &mut terminal,
        &mut input_flags,
    );

    reader_stop.store(true, Ordering::Release);
    drop(session);
    let reader_result = reader_thread
        .join()
        .map_err(|_| RuntimeError::ReaderPanicked);
    let flags_result = input_flags.restore();
    let terminal_result = terminal
        .restore()
        .map_err(|error| RuntimeError::Restore(error.to_string()));

    let exit = result?;
    reader_result?;
    flags_result?;
    terminal_result?;
    Ok(exit)
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

fn private_session_id() -> Result<PrivateSessionId, RuntimeError> {
    let sequence = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    PrivateSessionId::new(format!("argmax-{}-{sequence}", std::process::id()))
        .map_err(|error| RuntimeError::Setup(error.to_string()))
}

struct NonblockingInput {
    descriptor: RawFd,
    original: OFlag,
    active: bool,
}

impl NonblockingInput {
    fn enable(descriptor: RawFd) -> Result<Self, RuntimeError> {
        let original = fcntl(descriptor, FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .map_err(|error| RuntimeError::InputFlags(io::Error::from(error).kind()))?;
        fcntl(descriptor, FcntlArg::F_SETFL(original | OFlag::O_NONBLOCK))
            .map_err(|error| RuntimeError::InputFlags(io::Error::from(error).kind()))?;
        Ok(Self {
            descriptor,
            original,
            active: true,
        })
    }

    fn restore(&mut self) -> Result<(), RuntimeError> {
        if !self.active {
            return Ok(());
        }
        fcntl(self.descriptor, FcntlArg::F_SETFL(self.original))
            .map_err(|error| RuntimeError::InputFlags(io::Error::from(error).kind()))?;
        self.active = false;
        Ok(())
    }
}

impl Drop for NonblockingInput {
    fn drop(&mut self) {
        if self.active {
            let _ = fcntl(self.descriptor, FcntlArg::F_SETFL(self.original));
        }
    }
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
    output_receiver: mpsc::Receiver<ReaderMessage>,
    reader_state: ReadState,
    integration_state: ReadState,
    input_state: InputState,
    child_exit: Option<ChildExit>,
    escape_deadline: Option<Instant>,
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

fn drive_session(
    session: &mut PtySession,
    driver: &mut SessionDriver,
    reducer: &mut SessionReducer,
    output: &SerializedOutput<io::Stdout>,
    terminal: &mut TerminalGuard<io::Stdin, io::Stdout>,
    input_flags: &mut NonblockingInput,
) -> Result<ChildExit, RuntimeError> {
    let mut input = io::stdin();
    let mut input_buffer = [0_u8; INPUT_CHUNK_BYTES];
    let mut integration_buffer = vec![0_u8; EVENT_CHUNK_BYTES];

    loop {
        let mut progress = false;
        progress |= driver.handle_signals(session, terminal, input_flags)?;
        progress |= driver.flush_pending(session)?;
        progress |= driver.drain_output(output, reducer)?;
        progress |= driver.drain_integration(reducer, &mut integration_buffer)?;
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
        integration: PtyIntegration,
        signals: SignalEvents,
        output_receiver: mpsc::Receiver<ReaderMessage>,
    ) -> Self {
        Self {
            integration,
            signals,
            decoder: ShellEventDecoder::new(StreamEpoch::INITIAL),
            pending: PendingWrites::default(),
            output_receiver,
            reader_state: ReadState::Open,
            integration_state: ReadState::Open,
            input_state: InputState::Open,
            child_exit: None,
            escape_deadline: None,
        }
    }

    fn handle_signals(
        &mut self,
        session: &PtySession,
        terminal: &mut TerminalGuard<io::Stdin, io::Stdout>,
        input_flags: &mut NonblockingInput,
    ) -> Result<bool, RuntimeError> {
        let mut events = [None; 7];
        let count = self.signals.drain_pending(&mut events);
        for event in events.into_iter().take(count).flatten() {
            let signal = match event {
                SignalEvent::Resize => {
                    Self::resize(session, terminal)?;
                    continue;
                }
                SignalEvent::Interrupt => ForwardSignal::Interrupt,
                SignalEvent::Quit => ForwardSignal::Quit,
                SignalEvent::Terminate => ForwardSignal::Terminate,
                SignalEvent::Hangup => ForwardSignal::Hangup,
                SignalEvent::Suspend => {
                    Self::suspend(session, terminal, input_flags)?;
                    continue;
                }
                SignalEvent::Continue => ForwardSignal::Continue,
            };
            session
                .forward_signal(signal)
                .map_err(|error| RuntimeError::Write(error.to_string()))?;
        }
        Ok(count != 0)
    }

    fn resize(
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
            .map_err(|error| RuntimeError::Setup(error.to_string()))
    }

    fn suspend(
        session: &PtySession,
        terminal: &mut TerminalGuard<io::Stdin, io::Stdout>,
        input_flags: &mut NonblockingInput,
    ) -> Result<(), RuntimeError> {
        input_flags.restore()?;
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
        let resumed_flags = NonblockingInput::enable(io::stdin().as_raw_fd())?;
        session
            .resize(dimensions.size())
            .map_err(|error| RuntimeError::Setup(error.to_string()))?;
        *terminal = resumed_terminal;
        *input_flags = resumed_flags;
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

    fn drain_output(
        &mut self,
        output: &SerializedOutput<io::Stdout>,
        reducer: &mut SessionReducer,
    ) -> Result<bool, RuntimeError> {
        let mut progress = false;
        for _ in 0..MAX_DRAIN_PER_TICK {
            match self.output_receiver.try_recv() {
                Ok(ReaderMessage::Bytes(bytes)) => {
                    let effects = reducer.observe_shell_output();
                    self.apply_effects(reducer, effects)?;
                    output
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
        let (_, effects) = reducer.apply_shell_frame(frame);
        self.apply_effects(reducer, effects)
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
        match input.read(buffer) {
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
            && !reducer.selection().candidates().is_empty()
    }

    fn route_input(
        &mut self,
        reducer: &mut SessionReducer,
        input: &[u8],
    ) -> Result<(), RuntimeError> {
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
        let mut cancel_query = false;

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
                        .push(WriteDestination::Pty, SYNC_PROBE_SEQUENCE)?;
                }
                SessionEffect::StartQuery { work, .. } => {
                    drop(work);
                    cancel_query = true;
                }
                SessionEffect::ClearOverlay
                | SessionEffect::RefreshOverlay
                | SessionEffect::ModeChanged(_) => {}
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
        if cancel_query {
            drop(reducer.observe_shell_output());
        }
        Ok(())
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
        self.child_exit
            .filter(|_| self.reader_state == ReadState::Eof && self.pending.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn shell_mapping_is_total() {
        assert_eq!(shell_kind(Shell::Bash), ShellKind::Bash);
        assert_eq!(shell_kind(Shell::Zsh), ShellKind::Zsh);
        assert_eq!(shell_kind(Shell::Fish), ShellKind::Fish);
    }

    #[test]
    fn nonblocking_input_restores_exact_descriptor_flags() {
        let (reader, _writer) = nix::unistd::pipe().unwrap();
        let descriptor = reader.as_raw_fd();
        let before = fcntl(descriptor, FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .unwrap();

        let mut guard = NonblockingInput::enable(descriptor).unwrap();
        let enabled = fcntl(descriptor, FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .unwrap();
        assert!(enabled.contains(OFlag::O_NONBLOCK));

        guard.restore().unwrap();
        let restored = fcntl(descriptor, FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .unwrap();
        assert_eq!(restored, before);
    }
}
