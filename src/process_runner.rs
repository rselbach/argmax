//! Bounded, non-interactive local process execution for completion providers.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::num::NonZeroI32;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use argmax_platform::unix::peek_child_exit;
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, setsid};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

/// Largest output retained from one local provider process.
pub const MAX_LOCAL_PROCESS_OUTPUT: usize = 256 * 1024;
/// Largest number of arguments accepted for one local provider process.
pub const MAX_LOCAL_PROCESS_ARGUMENTS: usize = 256;
/// Largest individual program, argument, or working-directory value.
pub const MAX_LOCAL_PROCESS_VALUE_BYTES: usize = 16 * 1024;
/// Largest aggregate argument payload.
pub const MAX_LOCAL_PROCESS_ARGUMENT_BYTES: usize = 64 * 1024;
/// Largest number of explicit environment overrides accepted per request.
pub const MAX_LOCAL_PROCESS_ENVIRONMENT: usize = 64;
/// Largest aggregate explicit environment payload.
pub const MAX_LOCAL_PROCESS_ENVIRONMENT_BYTES: usize = 64 * 1024;
/// Longest local provider execution accepted by this boundary.
pub const MAX_LOCAL_PROCESS_TIMEOUT: Duration = Duration::from_secs(5);

const POLL_INTERVAL: Duration = Duration::from_millis(1);
const HIDDEN_LAUNCHER_ARGUMENT: &str = "__argmax-provider-launch-v1";
const LAUNCH_PROTOCOL_MAGIC: &[u8; 8] = b"ARGMAXP1";
const LAUNCH_READY_PREFIX: &[u8] = b"\0argmax-provider-ready-v1\0";
const LAUNCH_EXEC_FAILURE_PREFIX: &[u8] = b"\0argmax-provider-exec-failed-v1\0";
const LAUNCH_NONCE_BYTES: usize = 16;
const MAX_LAUNCH_PROTOCOL_BYTES: usize = 192 * 1024;
const MAX_LAUNCHER_PREAMBLE_BYTES: usize = 64 * 1024;
const HIDDEN_LAUNCHER_FAILURE: u8 = 125;

/// Validated non-shell command request.
pub struct LocalProcessRequest {
    program: OsString,
    arguments: Box<[OsString]>,
    environment: Box<[(OsString, Option<OsString>)]>,
    cwd: PathBuf,
    timeout: Duration,
    output_limit: usize,
}

impl LocalProcessRequest {
    /// Validates one local executable request without starting it.
    ///
    /// A program containing a slash must be absolute; otherwise it is resolved
    /// by `PATH` directly by the operating system. Arguments are passed as an
    /// array and are never interpreted by a shell.
    ///
    /// # Errors
    ///
    /// Returns a closed validation error for malformed paths, NUL bytes,
    /// excessive arguments, an invalid timeout, or an invalid output bound.
    pub fn new(
        program: impl Into<OsString>,
        arguments: impl IntoIterator<Item = OsString>,
        cwd: impl Into<PathBuf>,
        timeout: Duration,
        output_limit: usize,
    ) -> Result<Self, LocalProcessError> {
        let program = program.into();
        validate_program(&program)?;
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        validate_arguments(&arguments)?;
        let cwd = cwd.into();
        validate_working_directory(&cwd)?;
        if timeout.is_zero() || timeout > MAX_LOCAL_PROCESS_TIMEOUT {
            return Err(LocalProcessError::InvalidTimeout);
        }
        if !(1..=MAX_LOCAL_PROCESS_OUTPUT).contains(&output_limit) {
            return Err(LocalProcessError::InvalidOutputLimit);
        }
        Ok(Self {
            program,
            arguments: arguments.into_boxed_slice(),
            environment: Box::new([]),
            cwd,
            timeout,
            output_limit,
        })
    }

    /// Adds a bounded set of environment overrides and removals.
    ///
    /// A `Some(value)` entry sets a variable and `None` removes it from the
    /// inherited environment. Duplicate or malformed names are rejected so
    /// command behavior never depends on argument order.
    ///
    /// # Errors
    ///
    /// Returns [`LocalProcessError::InvalidEnvironment`] for excessive,
    /// duplicate, empty, `=`-containing, or NUL-containing entries.
    pub fn with_environment_overrides(
        mut self,
        environment: impl IntoIterator<Item = (OsString, Option<OsString>)>,
    ) -> Result<Self, LocalProcessError> {
        let environment = environment.into_iter().collect::<Vec<_>>();
        validate_environment(&environment)?;
        self.environment = environment.into_boxed_slice();
        Ok(self)
    }

    /// Program value passed directly to [`Command`].
    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// Exact structured argument array.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Absolute working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Hard wall-clock deadline for the complete process and output stream.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Maximum retained standard-output bytes.
    #[must_use]
    pub const fn output_limit(&self) -> usize {
        self.output_limit
    }
}

impl fmt::Debug for LocalProcessRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalProcessRequest")
            .field("program_bytes", &self.program.as_bytes().len())
            .field("argument_count", &self.arguments.len())
            .field("environment_count", &self.environment.len())
            .field("cwd_bytes", &self.cwd.as_os_str().as_bytes().len())
            .field("timeout", &self.timeout)
            .field("output_limit", &self.output_limit)
            .finish()
    }
}

/// Exact inert output and status from a completed local process.
#[derive(Eq, PartialEq)]
pub struct LocalProcessOutput {
    stdout: Vec<u8>,
    exit: LocalProcessExit,
}

impl LocalProcessOutput {
    /// Exact bounded standard output.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Structured native termination status.
    #[must_use]
    pub const fn exit(&self) -> LocalProcessExit {
        self.exit
    }

    /// Consumes the result and returns its exact standard output.
    #[must_use]
    pub fn into_stdout(self) -> Vec<u8> {
        self.stdout
    }
}

impl fmt::Debug for LocalProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalProcessOutput")
            .field("stdout_bytes", &self.stdout.len())
            .field("exit", &self.exit)
            .finish()
    }
}

/// Native process termination without command or output contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalProcessExit {
    /// Normal exit code.
    Code(i32),
    /// Unix signal number.
    Signal(i32),
    /// The operating system exposed neither form.
    Unknown,
}

impl LocalProcessExit {
    /// Whether the command returned exit code zero.
    #[must_use]
    pub const fn success(self) -> bool {
        matches!(self, Self::Code(0))
    }
}

/// Sanitized local-process failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalProcessError {
    /// Program was empty, oversized, relative-with-slash, or contained NUL.
    InvalidProgram,
    /// Argument count, size, aggregate size, or bytes were invalid.
    InvalidArguments,
    /// Working directory was relative, oversized, malformed, missing, or not a directory.
    InvalidWorkingDirectory,
    /// Environment override names, values, count, or aggregate size were invalid.
    InvalidEnvironment,
    /// Timeout was zero or exceeded [`MAX_LOCAL_PROCESS_TIMEOUT`].
    InvalidTimeout,
    /// Output bound was zero or exceeded [`MAX_LOCAL_PROCESS_OUTPUT`].
    InvalidOutputLimit,
    /// Private launcher discovery, request transfer, or startup failed.
    Launcher(io::ErrorKind),
    /// Process could not be created.
    Spawn(io::ErrorKind),
    /// Standard output could not be configured or read.
    Output(io::ErrorKind),
    /// Process status could not be inspected or reaped.
    Wait(io::ErrorKind),
    /// The hard wall-clock deadline elapsed.
    Timeout,
    /// Standard output exceeded the caller's validated bound.
    OutputTooLarge,
    /// A timed-out or failed process could not be terminated safely.
    Termination(io::ErrorKind),
}

impl fmt::Display for LocalProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProgram => formatter.write_str("local process program is invalid"),
            Self::InvalidArguments => formatter.write_str("local process arguments are invalid"),
            Self::InvalidWorkingDirectory => {
                formatter.write_str("local process working directory is invalid")
            }
            Self::InvalidEnvironment => formatter.write_str("local process environment is invalid"),
            Self::InvalidTimeout => formatter.write_str("local process timeout is invalid"),
            Self::InvalidOutputLimit => {
                formatter.write_str("local process output limit is invalid")
            }
            Self::Launcher(kind) => write!(formatter, "local process launcher failed: {kind}"),
            Self::Spawn(kind) => write!(formatter, "local process spawn failed: {kind}"),
            Self::Output(kind) => write!(formatter, "local process output failed: {kind}"),
            Self::Wait(kind) => write!(formatter, "local process wait failed: {kind}"),
            Self::Timeout => formatter.write_str("local process timed out"),
            Self::OutputTooLarge => formatter.write_str("local process output exceeded its limit"),
            Self::Termination(kind) => {
                write!(formatter, "local process termination failed: {kind}")
            }
        }
    }
}

impl Error for LocalProcessError {}

/// Result of checking the private provider-launcher command line.
pub enum HiddenLauncherDispatch {
    /// The command line is unrelated to the hidden launcher.
    NotHidden,
    /// The hidden launcher ran and the outer binary must return this status.
    Exit(ExitCode),
}

/// Handles the versioned private provider-launcher mode before normal startup.
///
/// `arguments` must include argument zero, as returned by
/// [`std::env::args_os`]. The real binary must call this before argument
/// parsing, logging, worker creation, or any other threaded initialization.
/// The launcher creates a new session and marks every descriptor at or above
/// three close-on-exec before decoding its bounded standard-input request.
///
/// Runtime ordering is equally important on the parent side: the one-time
/// interactive PTY spawn must finish before any call to [`run_local_process`].
#[must_use]
pub fn dispatch_hidden_launcher(
    arguments: impl IntoIterator<Item = OsString>,
) -> HiddenLauncherDispatch {
    let mut arguments = arguments.into_iter();
    let Some(_argument_zero) = arguments.next() else {
        return HiddenLauncherDispatch::NotHidden;
    };
    if arguments.next().as_deref() != Some(OsStr::new(HIDDEN_LAUNCHER_ARGUMENT)) {
        return HiddenLauncherDispatch::NotHidden;
    }
    if arguments.next().is_some() {
        return HiddenLauncherDispatch::Exit(ExitCode::from(HIDDEN_LAUNCHER_FAILURE));
    }
    HiddenLauncherDispatch::Exit(ExitCode::from(run_hidden_launcher()))
}

/// Runs one validated local provider process without a shell or terminal.
///
/// The parent self-executes the private early launcher without applying the
/// request's working directory or environment. A bounded raw protocol carries
/// exact Unix bytes to that launcher, which creates a new session before it
/// directly `exec`s the requested program. Standard input and standard error
/// are disconnected, standard output is bounded, and no ambient descriptor at
/// or above three survives the target `exec`.
///
/// Same-process-group descendants are killed even after a successful leader
/// exit. A descendant that creates another session is outside portable POSIX
/// process-group containment; it cannot extend this call beyond the request's
/// hard deadline, but may outlive it. Callers must only run trusted local
/// completion providers under that explicit limitation.
///
/// The interactive shell's one-time PTY startup must complete before calling
/// this function; provider self-exec must never overlap its inherited-FD spawn
/// window.
///
/// # Errors
///
/// Returns a sanitized lifecycle, launcher, timeout, or output-bound error.
pub fn run_local_process(
    request: &LocalProcessRequest,
) -> Result<LocalProcessOutput, LocalProcessError> {
    let executable =
        std::env::current_exe().map_err(|error| LocalProcessError::Launcher(error.kind()))?;
    let launcher = LauncherCommand {
        executable,
        arguments: vec![OsString::from(HIDDEN_LAUNCHER_ARGUMENT)].into_boxed_slice(),
    };
    run_with_launcher(request, &launcher)
}

struct LauncherCommand {
    executable: PathBuf,
    arguments: Box<[OsString]>,
}

fn run_with_launcher(
    request: &LocalProcessRequest,
    launcher: &LauncherCommand,
) -> Result<LocalProcessOutput, LocalProcessError> {
    if !fs::metadata(&request.cwd).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(LocalProcessError::InvalidWorkingDirectory);
    }
    let started = Instant::now();
    let deadline = started
        .checked_add(request.timeout)
        .ok_or(LocalProcessError::InvalidTimeout)?;
    let nonce = launch_nonce()?;
    let encoded = encode_request(request, nonce);
    let mut command = Command::new(&launcher.executable);
    command
        .args(launcher.arguments.iter())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| LocalProcessError::Spawn(error.kind()))?;
    let process_group = child_process_id(&mut child)?;
    let Some(input) = child.stdin.take() else {
        terminate_and_reap(&mut child, process_group)?;
        return Err(LocalProcessError::Launcher(io::ErrorKind::BrokenPipe));
    };
    let Some(mut stdout) = child.stdout.take() else {
        terminate_and_reap(&mut child, process_group)?;
        return Err(LocalProcessError::Output(io::ErrorKind::BrokenPipe));
    };
    if let Err(error) = make_nonblocking(&input) {
        terminate_and_reap(&mut child, process_group)?;
        return Err(LocalProcessError::Launcher(error.kind()));
    }
    if let Err(error) = make_nonblocking(&stdout) {
        terminate_and_reap(&mut child, process_group)?;
        return Err(LocalProcessError::Output(error.kind()));
    }

    let mut input = Some(input);
    let mut input_offset = 0_usize;
    let mut stdout_eof = false;
    let mut output = OutputCollector::new(request.output_limit, nonce);
    loop {
        if let Some(open_input) = &mut input {
            match write_launch_request(open_input, &encoded, &mut input_offset) {
                Ok(WriteState::Pending) => {}
                Ok(WriteState::Complete) => {
                    drop(input.take());
                }
                Err(error) => {
                    terminate_and_reap(&mut child, process_group)?;
                    return Err(LocalProcessError::Launcher(error.kind()));
                }
            }
        }
        if !stdout_eof {
            match drain_stdout(&mut stdout, &mut output) {
                Ok(DrainState::Pending) => {}
                Ok(DrainState::Eof) => stdout_eof = true,
                Err(error) => {
                    terminate_and_reap(&mut child, process_group)?;
                    return Err(error);
                }
            }
        }
        let leader_exited = match peek_child_exit(process_group) {
            Ok(exited) => exited,
            Err(error) => {
                let kind = error.kind();
                terminate_and_reap(&mut child, process_group)?;
                return Err(LocalProcessError::Wait(kind));
            }
        };
        match lifecycle_decision(leader_exited, Instant::now(), deadline) {
            LifecycleDecision::Continue => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(POLL_INTERVAL.min(remaining));
            }
            LifecycleDecision::Complete => {
                drop(input.take());
                return finish_observed_exit(
                    &mut child,
                    process_group,
                    &mut stdout,
                    stdout_eof,
                    output,
                    deadline,
                );
            }
            LifecycleDecision::Timeout => {
                drop(input.take());
                terminate_and_reap(&mut child, process_group)?;
                return Err(LocalProcessError::Timeout);
            }
        }
    }
}

fn child_process_id(child: &mut Child) -> Result<NonZeroI32, LocalProcessError> {
    let process_id = i32::try_from(child.id()).ok().and_then(NonZeroI32::new);
    process_id.ok_or_else(|| {
        let _cleanup = child.kill();
        let _reap = child.wait();
        LocalProcessError::Spawn(io::ErrorKind::InvalidData)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleDecision {
    Continue,
    Complete,
    Timeout,
}

fn lifecycle_decision(leader_exited: bool, now: Instant, deadline: Instant) -> LifecycleDecision {
    if leader_exited {
        LifecycleDecision::Complete
    } else if now >= deadline {
        LifecycleDecision::Timeout
    } else {
        LifecycleDecision::Continue
    }
}

fn finish_observed_exit(
    child: &mut Child,
    process_group: NonZeroI32,
    stdout: &mut ChildStdout,
    mut stdout_eof: bool,
    mut output: OutputCollector,
    deadline: Instant,
) -> Result<LocalProcessOutput, LocalProcessError> {
    // The exact leader is intentionally still a zombie here. It pins both the
    // PID and PGID until every same-group descendant has been signaled.
    if let Err(kind) = kill_observed_process_group(process_group) {
        terminate_child_and_reap(child)?;
        return Err(LocalProcessError::Termination(kind));
    }
    while !stdout_eof {
        match drain_stdout(stdout, &mut output) {
            Ok(DrainState::Pending) => {}
            Ok(DrainState::Eof) => stdout_eof = true,
            Err(error) => {
                reap_observed_child(child)?;
                return Err(error);
            }
        }
        let now = Instant::now();
        if !stdout_eof && now >= deadline {
            reap_observed_child(child)?;
            return Err(LocalProcessError::Timeout);
        }
        if !stdout_eof {
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
        }
    }
    let status = child
        .wait()
        .map_err(|error| LocalProcessError::Wait(error.kind()))?;
    output.finish(status)
}

fn reap_observed_child(child: &mut Child) -> Result<(), LocalProcessError> {
    child
        .wait()
        .map(|_status| ())
        .map_err(|error| LocalProcessError::Termination(error.kind()))
}

fn validate_program(program: &OsStr) -> Result<(), LocalProcessError> {
    let bytes = program.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_LOCAL_PROCESS_VALUE_BYTES
        || bytes.contains(&0)
        || (bytes.contains(&b'/') && !Path::new(program).is_absolute())
    {
        return Err(LocalProcessError::InvalidProgram);
    }
    Ok(())
}

fn validate_arguments(arguments: &[OsString]) -> Result<(), LocalProcessError> {
    if arguments.len() > MAX_LOCAL_PROCESS_ARGUMENTS {
        return Err(LocalProcessError::InvalidArguments);
    }
    let mut total = 0_usize;
    for argument in arguments {
        let bytes = argument.as_bytes();
        if bytes.len() > MAX_LOCAL_PROCESS_VALUE_BYTES || bytes.contains(&0) {
            return Err(LocalProcessError::InvalidArguments);
        }
        total = total
            .checked_add(bytes.len())
            .ok_or(LocalProcessError::InvalidArguments)?;
        if total > MAX_LOCAL_PROCESS_ARGUMENT_BYTES {
            return Err(LocalProcessError::InvalidArguments);
        }
    }
    Ok(())
}

fn validate_working_directory(cwd: &Path) -> Result<(), LocalProcessError> {
    let bytes = cwd.as_os_str().as_bytes();
    if !cwd.is_absolute() || bytes.len() > MAX_LOCAL_PROCESS_VALUE_BYTES || bytes.contains(&0) {
        return Err(LocalProcessError::InvalidWorkingDirectory);
    }
    Ok(())
}

fn validate_environment(
    environment: &[(OsString, Option<OsString>)],
) -> Result<(), LocalProcessError> {
    if environment.len() > MAX_LOCAL_PROCESS_ENVIRONMENT {
        return Err(LocalProcessError::InvalidEnvironment);
    }
    let mut names = BTreeSet::new();
    let mut total = 0_usize;
    for (name, value) in environment {
        let name = name.as_bytes();
        if name.is_empty()
            || name.len() > MAX_LOCAL_PROCESS_VALUE_BYTES
            || name.contains(&0)
            || name.contains(&b'=')
            || !names.insert(name)
        {
            return Err(LocalProcessError::InvalidEnvironment);
        }
        total = total
            .checked_add(name.len())
            .ok_or(LocalProcessError::InvalidEnvironment)?;
        if let Some(value) = value {
            let value = value.as_bytes();
            if value.len() > MAX_LOCAL_PROCESS_VALUE_BYTES || value.contains(&0) {
                return Err(LocalProcessError::InvalidEnvironment);
            }
            total = total
                .checked_add(value.len())
                .ok_or(LocalProcessError::InvalidEnvironment)?;
        }
        if total > MAX_LOCAL_PROCESS_ENVIRONMENT_BYTES {
            return Err(LocalProcessError::InvalidEnvironment);
        }
    }
    Ok(())
}

fn encode_request(request: &LocalProcessRequest, nonce: [u8; LAUNCH_NONCE_BYTES]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(
        LAUNCH_PROTOCOL_MAGIC.len()
            + request.program.as_bytes().len()
            + request.cwd.as_os_str().as_bytes().len()
            + request
                .arguments
                .iter()
                .map(|argument| argument.as_bytes().len())
                .sum::<usize>()
            + request
                .environment
                .iter()
                .map(|(name, value)| {
                    name.as_bytes().len() + value.as_ref().map_or(0, |value| value.as_bytes().len())
                })
                .sum::<usize>()
            + 2048,
    );
    encoded.extend_from_slice(LAUNCH_PROTOCOL_MAGIC);
    push_blob(&mut encoded, &nonce);
    push_blob(&mut encoded, request.program.as_bytes());
    push_blob(&mut encoded, request.cwd.as_os_str().as_bytes());
    push_count(&mut encoded, request.arguments.len());
    for argument in &request.arguments {
        push_blob(&mut encoded, argument.as_bytes());
    }
    push_count(&mut encoded, request.environment.len());
    for (name, value) in &request.environment {
        push_blob(&mut encoded, name.as_bytes());
        match value {
            Some(value) => {
                encoded.push(1);
                push_blob(&mut encoded, value.as_bytes());
            }
            None => encoded.push(0),
        }
    }
    debug_assert!(encoded.len() <= MAX_LAUNCH_PROTOCOL_BYTES);
    encoded
}

fn push_count(encoded: &mut Vec<u8>, count: usize) {
    let count = u32::try_from(count).expect("validated launch count fits in u32");
    encoded.extend_from_slice(&count.to_le_bytes());
}

fn push_blob(encoded: &mut Vec<u8>, bytes: &[u8]) {
    push_count(encoded, bytes.len());
    encoded.extend_from_slice(bytes);
}

struct DecodedLaunch {
    nonce: [u8; LAUNCH_NONCE_BYTES],
    program: OsString,
    arguments: Box<[OsString]>,
    environment: Box<[(OsString, Option<OsString>)]>,
    cwd: PathBuf,
}

fn decode_request(encoded: &[u8]) -> Result<DecodedLaunch, ()> {
    let mut cursor = Cursor::new(encoded);
    let mut magic = [0_u8; LAUNCH_PROTOCOL_MAGIC.len()];
    cursor.read_exact(&mut magic).map_err(|_error| ())?;
    if &magic != LAUNCH_PROTOCOL_MAGIC {
        return Err(());
    }
    let nonce = read_blob(&mut cursor, LAUNCH_NONCE_BYTES)?;
    let nonce = <[u8; LAUNCH_NONCE_BYTES]>::try_from(nonce).map_err(|_bytes| ())?;
    let program = OsString::from_vec(read_blob(&mut cursor, MAX_LOCAL_PROCESS_VALUE_BYTES)?);
    let cwd = PathBuf::from(OsString::from_vec(read_blob(
        &mut cursor,
        MAX_LOCAL_PROCESS_VALUE_BYTES,
    )?));
    let argument_count = read_count(&mut cursor, MAX_LOCAL_PROCESS_ARGUMENTS)?;
    let mut arguments = Vec::with_capacity(argument_count);
    for _index in 0..argument_count {
        arguments.push(OsString::from_vec(read_blob(
            &mut cursor,
            MAX_LOCAL_PROCESS_VALUE_BYTES,
        )?));
    }
    let environment_count = read_count(&mut cursor, MAX_LOCAL_PROCESS_ENVIRONMENT)?;
    let mut environment = Vec::with_capacity(environment_count);
    for _index in 0..environment_count {
        let name = OsString::from_vec(read_blob(&mut cursor, MAX_LOCAL_PROCESS_VALUE_BYTES)?);
        let mut tag = [0_u8; 1];
        cursor.read_exact(&mut tag).map_err(|_error| ())?;
        let value = match tag[0] {
            0 => None,
            1 => Some(OsString::from_vec(read_blob(
                &mut cursor,
                MAX_LOCAL_PROCESS_VALUE_BYTES,
            )?)),
            _ => return Err(()),
        };
        environment.push((name, value));
    }
    if usize::try_from(cursor.position()).ok() != Some(encoded.len()) {
        return Err(());
    }
    validate_program(&program).map_err(|_error| ())?;
    validate_arguments(&arguments).map_err(|_error| ())?;
    validate_working_directory(&cwd).map_err(|_error| ())?;
    validate_environment(&environment).map_err(|_error| ())?;
    Ok(DecodedLaunch {
        nonce,
        program,
        arguments: arguments.into_boxed_slice(),
        environment: environment.into_boxed_slice(),
        cwd,
    })
}

fn read_count(cursor: &mut Cursor<&[u8]>, maximum: usize) -> Result<usize, ()> {
    let mut bytes = [0_u8; size_of::<u32>()];
    cursor.read_exact(&mut bytes).map_err(|_error| ())?;
    let count = usize::try_from(u32::from_le_bytes(bytes)).map_err(|_error| ())?;
    if count > maximum {
        return Err(());
    }
    Ok(count)
}

fn read_blob(cursor: &mut Cursor<&[u8]>, maximum: usize) -> Result<Vec<u8>, ()> {
    let length = read_count(cursor, maximum)?;
    let mut bytes = vec![0_u8; length];
    cursor.read_exact(&mut bytes).map_err(|_error| ())?;
    Ok(bytes)
}

fn run_hidden_launcher() -> u8 {
    if setsid().is_err() {
        return HIDDEN_LAUNCHER_FAILURE;
    }
    close_fds::set_fds_cloexec(3, &[]);
    let mut encoded = Vec::new();
    let read_limit =
        u64::try_from(MAX_LAUNCH_PROTOCOL_BYTES + 1).expect("launch protocol bound fits in u64");
    if io::stdin()
        .lock()
        .take(read_limit)
        .read_to_end(&mut encoded)
        .is_err()
        || encoded.len() > MAX_LAUNCH_PROTOCOL_BYTES
    {
        return HIDDEN_LAUNCHER_FAILURE;
    }
    let Ok(request) = decode_request(&encoded) else {
        return HIDDEN_LAUNCHER_FAILURE;
    };
    let mut command = Command::new(&request.program);
    command
        .args(request.arguments.iter())
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null());
    for (name, value) in &request.environment {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    {
        let mut stdout = io::stdout().lock();
        if stdout.write_all(LAUNCH_READY_PREFIX).is_err()
            || stdout.write_all(&request.nonce).is_err()
            || stdout.flush().is_err()
        {
            return HIDDEN_LAUNCHER_FAILURE;
        }
    }
    let exec_error = command.exec();
    let mut stdout = io::stdout().lock();
    let _write_failure_prefix = stdout.write_all(LAUNCH_EXEC_FAILURE_PREFIX);
    let _write_failure_nonce = stdout.write_all(&request.nonce);
    let _write_failure_kind = stdout.write_all(&[encode_error_kind(exec_error.kind())]);
    let _flush_failure = stdout.flush();
    HIDDEN_LAUNCHER_FAILURE
}

enum WriteState {
    Pending,
    Complete,
}

fn write_launch_request(
    input: &mut ChildStdin,
    encoded: &[u8],
    offset: &mut usize,
) -> io::Result<WriteState> {
    while *offset < encoded.len() {
        match input.write(&encoded[*offset..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(written) => *offset += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(WriteState::Pending);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(WriteState::Complete)
}

fn make_nonblocking(descriptor: impl AsFd) -> io::Result<()> {
    let flags = fcntl_getfl(descriptor.as_fd()).map_err(rustix_error)?;
    fcntl_setfl(descriptor.as_fd(), flags | OFlags::NONBLOCK).map_err(rustix_error)
}

enum DrainState {
    Pending,
    Eof,
}

fn drain_stdout(
    stdout: &mut ChildStdout,
    output: &mut OutputCollector,
) -> Result<DrainState, LocalProcessError> {
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        match stdout.read(&mut buffer) {
            Ok(0) => return Ok(DrainState::Eof),
            Ok(read) => output.push(&buffer[..read])?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(DrainState::Pending);
            }
            Err(error) => return Err(LocalProcessError::Output(error.kind())),
        }
    }
}

struct OutputCollector {
    preamble: Vec<u8>,
    stdout: Vec<u8>,
    ready_marker: Vec<u8>,
    exec_failure_marker: Vec<u8>,
    exec_failure_candidate: Vec<u8>,
    output_limit: usize,
    ready: bool,
    checking_exec_failure: bool,
}

impl OutputCollector {
    fn new(output_limit: usize, nonce: [u8; LAUNCH_NONCE_BYTES]) -> Self {
        let mut ready_marker = Vec::with_capacity(LAUNCH_READY_PREFIX.len() + nonce.len());
        ready_marker.extend_from_slice(LAUNCH_READY_PREFIX);
        ready_marker.extend_from_slice(&nonce);
        let mut exec_failure_marker =
            Vec::with_capacity(LAUNCH_EXEC_FAILURE_PREFIX.len() + nonce.len());
        exec_failure_marker.extend_from_slice(LAUNCH_EXEC_FAILURE_PREFIX);
        exec_failure_marker.extend_from_slice(&nonce);
        Self {
            preamble: Vec::new(),
            stdout: Vec::with_capacity(output_limit.min(8 * 1024)),
            ready_marker,
            exec_failure_marker,
            exec_failure_candidate: Vec::new(),
            output_limit,
            ready: false,
            checking_exec_failure: true,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), LocalProcessError> {
        if self.ready {
            return self.push_after_ready(bytes);
        }
        self.preamble.extend_from_slice(bytes);
        let Some(position) = find_bytes(&self.preamble, &self.ready_marker) else {
            if self.preamble.len() > MAX_LAUNCHER_PREAMBLE_BYTES {
                return Err(LocalProcessError::Launcher(io::ErrorKind::InvalidData));
            }
            return Ok(());
        };
        if position > MAX_LAUNCHER_PREAMBLE_BYTES {
            return Err(LocalProcessError::Launcher(io::ErrorKind::InvalidData));
        }
        let stdout_start = position + self.ready_marker.len();
        let stdout = self.preamble.split_off(stdout_start);
        self.preamble.clear();
        self.ready = true;
        self.push_after_ready(&stdout)
    }

    fn push_after_ready(&mut self, bytes: &[u8]) -> Result<(), LocalProcessError> {
        if !self.checking_exec_failure {
            return self.push_target_stdout(bytes);
        }
        let frame_length = self.exec_failure_marker.len() + 1;
        let candidate_space = frame_length.saturating_sub(self.exec_failure_candidate.len());
        let candidate_bytes = candidate_space.min(bytes.len());
        self.exec_failure_candidate
            .extend_from_slice(&bytes[..candidate_bytes]);
        let remaining = &bytes[candidate_bytes..];
        if self.exec_failure_candidate_is_prefix() && remaining.is_empty() {
            return Ok(());
        }
        self.checking_exec_failure = false;
        let candidate = std::mem::take(&mut self.exec_failure_candidate);
        self.push_target_stdout(&candidate)?;
        self.push_target_stdout(remaining)
    }

    fn exec_failure_candidate_is_prefix(&self) -> bool {
        let marker_bytes = self
            .exec_failure_candidate
            .len()
            .min(self.exec_failure_marker.len());
        if self.exec_failure_candidate[..marker_bytes] != self.exec_failure_marker[..marker_bytes] {
            return false;
        }
        self.exec_failure_candidate.len() <= self.exec_failure_marker.len()
            || (self.exec_failure_candidate.len() == self.exec_failure_marker.len() + 1
                && self.exec_failure_candidate[self.exec_failure_marker.len()] <= 3)
    }

    fn push_target_stdout(&mut self, bytes: &[u8]) -> Result<(), LocalProcessError> {
        let length = self
            .stdout
            .len()
            .checked_add(bytes.len())
            .ok_or(LocalProcessError::OutputTooLarge)?;
        if length > self.output_limit {
            return Err(LocalProcessError::OutputTooLarge);
        }
        self.stdout.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(mut self, status: ExitStatus) -> Result<LocalProcessOutput, LocalProcessError> {
        if !self.ready {
            return Err(LocalProcessError::Launcher(io::ErrorKind::InvalidData));
        }
        if status.code() == Some(i32::from(HIDDEN_LAUNCHER_FAILURE))
            && self.checking_exec_failure
            && self.exec_failure_candidate.len() == self.exec_failure_marker.len() + 1
        {
            let kind =
                decode_error_kind(self.exec_failure_candidate[self.exec_failure_marker.len()]);
            return Err(LocalProcessError::Launcher(kind));
        }
        let candidate = std::mem::take(&mut self.exec_failure_candidate);
        self.push_target_stdout(&candidate)?;
        Ok(LocalProcessOutput {
            stdout: self.stdout,
            exit: classify_exit(status),
        })
    }
}

fn launch_nonce() -> Result<[u8; LAUNCH_NONCE_BYTES], LocalProcessError> {
    let mut nonce = [0_u8; LAUNCH_NONCE_BYTES];
    let mut randomness = fs::File::open("/dev/urandom")
        .map_err(|error| LocalProcessError::Launcher(error.kind()))?;
    randomness
        .read_exact(&mut nonce)
        .map_err(|error| LocalProcessError::Launcher(error.kind()))?;
    Ok(nonce)
}

const fn encode_error_kind(kind: io::ErrorKind) -> u8 {
    match kind {
        io::ErrorKind::NotFound => 0,
        io::ErrorKind::PermissionDenied => 1,
        io::ErrorKind::InvalidInput => 2,
        _ => 3,
    }
}

const fn decode_error_kind(encoded: u8) -> io::ErrorKind {
    match encoded {
        0 => io::ErrorKind::NotFound,
        1 => io::ErrorKind::PermissionDenied,
        2 => io::ErrorKind::InvalidInput,
        _ => io::ErrorKind::Other,
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn kill_process_group(process_group: NonZeroI32) -> Result<(), io::ErrorKind> {
    match killpg(Pid::from_raw(process_group.get()), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(io::Error::from(error).kind()),
    }
}

fn kill_observed_process_group(process_group: NonZeroI32) -> Result<(), io::ErrorKind> {
    match killpg(Pid::from_raw(process_group.get()), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        // Darwin reports EPERM when the observed zombie is the only remaining
        // group member. Any live same-credential descendant makes this call
        // succeed, as exercised by the descendant-cleanup tests.
        #[cfg(target_os = "macos")]
        Err(Errno::EPERM) => Ok(()),
        Err(error) => Err(io::Error::from(error).kind()),
    }
}

fn terminate_and_reap(
    child: &mut Child,
    process_group: NonZeroI32,
) -> Result<(), LocalProcessError> {
    let group_error = match kill_process_group(process_group) {
        Ok(()) => None,
        #[cfg(target_os = "macos")]
        Err(io::ErrorKind::PermissionDenied)
            if peek_child_exit(process_group).is_ok_and(|exited| exited) =>
        {
            None
        }
        Err(kind) => Some(kind),
    };
    let child_error = match child.kill() {
        Ok(()) => None,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => None,
        Err(error) => Some(error.kind()),
    };
    let wait_error = child.wait().err().map(|error| error.kind());
    if let Some(kind) = group_error.or(child_error).or(wait_error) {
        Err(LocalProcessError::Termination(kind))
    } else {
        Ok(())
    }
}

fn terminate_child_and_reap(child: &mut Child) -> Result<(), LocalProcessError> {
    let child_error = match child.kill() {
        Ok(()) => None,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => None,
        Err(error) => Some(error.kind()),
    };
    let wait_error = child.wait().err().map(|error| error.kind());
    if let Some(kind) = child_error.or(wait_error) {
        Err(LocalProcessError::Termination(kind))
    } else {
        Ok(())
    }
}

fn classify_exit(status: ExitStatus) -> LocalProcessExit {
    if let Some(code) = status.code() {
        LocalProcessExit::Code(code)
    } else if let Some(signal) = status.signal() {
        LocalProcessExit::Signal(signal)
    } else {
        LocalProcessExit::Unknown
    }
}

fn rustix_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
mod tests {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::process;

    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::{Pid, close, dup2, getpgrp, getpid, getsid};
    use pty_process::blocking::{Command as PtyCommand, open as open_pty};

    use super::*;

    fn test_name(name: &str) -> String {
        format!("process_runner::tests::{name}")
    }

    fn test_launcher() -> LauncherCommand {
        LauncherCommand {
            executable: std::env::current_exe().unwrap(),
            arguments: vec![
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from(test_name("hidden_launcher_subprocess")),
                OsString::from("--nocapture"),
            ]
            .into_boxed_slice(),
        }
    }

    fn run_test_process(
        request: &LocalProcessRequest,
    ) -> Result<LocalProcessOutput, LocalProcessError> {
        run_with_launcher(request, &test_launcher())
    }

    fn shell_request(script: &str, timeout: Duration, limit: usize) -> LocalProcessRequest {
        LocalProcessRequest::new(
            "/bin/sh",
            [OsString::from("-c"), OsString::from(script)],
            std::env::temp_dir(),
            timeout,
            limit,
        )
        .unwrap()
    }

    fn spawn_ignored_test(name: &str) -> ExitStatus {
        Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", &test_name(name), "--nocapture"])
            .status()
            .unwrap()
    }

    fn read_pid(path: &Path) -> Pid {
        let text = fs::read_to_string(path).unwrap();
        Pid::from_raw(text.trim().parse().unwrap())
    }

    fn process_exists(pid: Pid) -> bool {
        match kill(pid, None) {
            Ok(()) | Err(Errno::EPERM) => true,
            Err(Errno::ESRCH) => false,
            Err(error) => panic!("unexpected process probe error: {error}"),
        }
    }

    fn wait_until_gone(pid: Pid) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while process_exists(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        !process_exists(pid)
    }

    fn assert_gone(pid: Pid) {
        let gone = wait_until_gone(pid);
        if !gone {
            let _cleanup = kill(pid, Signal::SIGKILL);
        }
        assert!(gone, "provider process {pid} survived cleanup");
    }

    #[test]
    fn preserves_exact_binary_output_and_structured_exit_status() {
        let request = shell_request(
            "printf 'Troy\\000Barnes'; exit 7",
            Duration::from_secs(2),
            64,
        );
        let output = run_test_process(&request).unwrap();
        assert_eq!(output.stdout(), b"Troy\0Barnes");
        assert_eq!(output.exit(), LocalProcessExit::Code(7));
        assert!(!output.exit().success());
        assert!(!format!("{output:?}").contains("Troy"));
        assert!(!format!("{request:?}").contains("Barnes"));
    }

    #[test]
    fn preserves_non_utf8_arguments_and_environment_values() {
        let argument = OsString::from_vec(vec![b'T', 0xff, b'B']);
        let value = OsString::from_vec(vec![b'G', 0xfe, b'C']);
        let request = LocalProcessRequest::new(
            "/bin/sh",
            [
                OsString::from("-c"),
                OsString::from("printf '%s|' \"$1\"; printf '%s' \"$ARGMAX_RAW\""),
                OsString::from("argmax-test"),
                argument,
            ],
            std::env::temp_dir(),
            Duration::from_secs(2),
            64,
        )
        .unwrap()
        .with_environment_overrides([(OsString::from("ARGMAX_RAW"), Some(value))])
        .unwrap();
        assert_eq!(
            run_test_process(&request).unwrap().stdout(),
            b"T\xffB|G\xfeC"
        );
    }

    #[test]
    fn enforces_exact_and_over_limit_output_without_a_reader_thread() {
        let exact = shell_request("printf 12345678", Duration::from_secs(2), 8);
        assert_eq!(run_test_process(&exact).unwrap().stdout(), b"12345678");
        let oversized = shell_request("printf 123456789; sleep 5", Duration::from_secs(2), 8);
        assert_eq!(
            run_test_process(&oversized),
            Err(LocalProcessError::OutputTooLarge)
        );
    }

    #[test]
    fn timeout_and_output_overflow_kill_and_reap_the_leader() {
        let temporary = tempfile::tempdir().unwrap();
        let timeout_pid = temporary.path().join("timeout.pid");
        let timeout_script = format!(
            "printf '%s' $$ > '{}'; trap '' HUP TERM; sleep 5",
            timeout_pid.display()
        );
        let request = shell_request(&timeout_script, Duration::from_millis(100), 64);
        assert_eq!(run_test_process(&request), Err(LocalProcessError::Timeout));
        assert_gone(read_pid(&timeout_pid));

        let overflow_pid = temporary.path().join("overflow.pid");
        let overflow_script = format!(
            "printf '%s' $$ > '{}'; printf 123456789; sleep 5",
            overflow_pid.display()
        );
        let request = shell_request(&overflow_script, Duration::from_secs(2), 8);
        assert_eq!(
            run_test_process(&request),
            Err(LocalProcessError::OutputTooLarge)
        );
        assert_gone(read_pid(&overflow_pid));
    }

    #[test]
    fn successful_leader_exit_kills_same_group_descendants_with_or_without_stdout() {
        for redirect in ["", ">/dev/null"] {
            let temporary = tempfile::tempdir().unwrap();
            let pid_path = temporary.path().join("descendant.pid");
            let script = format!(
                "sleep 5 {redirect} & printf '%s' $! > '{}'; exit 0",
                pid_path.display()
            );
            let request = shell_request(&script, Duration::from_secs(2), 64);
            assert!(run_test_process(&request).unwrap().exit().success());
            assert_gone(read_pid(&pid_path));
        }
    }

    #[test]
    fn launcher_target_is_the_session_and_process_group_leader() {
        let request = LocalProcessRequest::new(
            std::env::current_exe().unwrap().into_os_string(),
            [
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from(test_name("provider_probe_subprocess")),
                OsString::from("--nocapture"),
            ],
            std::env::temp_dir(),
            Duration::from_secs(2),
            4 * 1024,
        )
        .unwrap()
        .with_environment_overrides([(
            OsString::from("ARGMAX_TEST_PROBE"),
            Some(OsString::from("identity")),
        )])
        .unwrap();
        let output = run_test_process(&request).unwrap();
        let text = String::from_utf8(output.into_stdout()).unwrap();
        let marker = text.split("ARGMAX_IDENTITY:").nth(1).unwrap().trim();
        let identifiers = marker
            .split(':')
            .map(|value| value.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(identifiers.len(), 3);
        assert_eq!(identifiers[0], identifiers[1]);
        assert_eq!(identifiers[0], identifiers[2]);
    }

    #[test]
    fn inherited_non_cloexec_descriptor_does_not_reach_target() {
        assert!(spawn_ignored_test("ambient_fd_parent_subprocess").success());
    }

    #[test]
    fn target_cannot_open_dev_tty_even_when_parent_has_a_controlling_terminal() {
        let (mut master_pty, slave_pty) = open_pty().unwrap();
        let reader = thread::spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                match master_pty.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => output.extend_from_slice(&buffer[..read]),
                    Err(error) if error.raw_os_error() == Some(Errno::EIO as i32) => break,
                    Err(error) => panic!("controlling PTY read failed: {error}"),
                }
            }
            output
        });
        let mut child = PtyCommand::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                &test_name("controlling_terminal_parent_subprocess"),
                "--nocapture",
            ])
            .spawn(slave_pty)
            .unwrap();
        let status = child.wait().unwrap();
        let output = reader.join().unwrap();
        assert!(
            status.success(),
            "controlling-terminal subprocess failed: {}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn escaped_session_is_outside_group_containment_but_cannot_extend_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        for redirect in [false, true] {
            let ready = temporary.path().join(if redirect {
                "ready-closed"
            } else {
                "ready-open"
            });
            let executable = std::env::current_exe().unwrap();
            let probe_name = test_name("escaped_provider_subprocess");
            let redirection = if redirect { ">/dev/null 2>&1" } else { "" };
            let script = format!(
                "\"$ARGMAX_TEST_EXE\" --ignored --exact \"$ARGMAX_TEST_NAME\" \
                 --nocapture {redirection} & child=$!; \
                 for ignored in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 \
                 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 \
                 43 44 45 46 47 48 49 50; do test -f \"$ARGMAX_TEST_READY\" && break; \
                 sleep 0.01; done; test -f \"$ARGMAX_TEST_READY\" || exit 91; exit 0"
            );
            let request = shell_request(&script, Duration::from_secs(2), 16 * 1024)
                .with_environment_overrides([
                    (
                        OsString::from("ARGMAX_TEST_EXE"),
                        Some(executable.into_os_string()),
                    ),
                    (
                        OsString::from("ARGMAX_TEST_NAME"),
                        Some(OsString::from(probe_name)),
                    ),
                    (
                        OsString::from("ARGMAX_TEST_READY"),
                        Some(ready.clone().into_os_string()),
                    ),
                ])
                .unwrap();
            let started = Instant::now();
            let result = run_test_process(&request);
            if redirect {
                assert!(result.unwrap().exit().success());
            } else {
                assert_eq!(result, Err(LocalProcessError::Timeout));
                assert!(started.elapsed() < Duration::from_secs(3));
            }
            let escaped = read_pid(&ready);
            assert!(process_exists(escaped));
            kill(escaped, Signal::SIGKILL).unwrap();
        }
    }

    #[test]
    fn rejects_malformed_hidden_protocol_and_reaps_launcher() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                &test_name("hidden_launcher_subprocess"),
                "--nocapture",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"malformed").unwrap();
        assert_eq!(
            child.wait().unwrap().code(),
            Some(i32::from(HIDDEN_LAUNCHER_FAILURE))
        );
    }

    #[test]
    fn reports_target_exec_failures_without_confusing_normal_status_125() {
        let missing = LocalProcessRequest::new(
            "/argmax-test-provider-does-not-exist",
            [],
            std::env::temp_dir(),
            Duration::from_secs(2),
            1,
        )
        .unwrap();
        assert_eq!(
            run_test_process(&missing),
            Err(LocalProcessError::Launcher(io::ErrorKind::NotFound))
        );

        let legitimate = shell_request("printf legitimate; exit 125", Duration::from_secs(2), 64);
        let output = run_test_process(&legitimate).unwrap();
        assert_eq!(output.stdout(), b"legitimate");
        assert_eq!(output.exit(), LocalProcessExit::Code(125));

        let forged = shell_request(
            "printf '\\000argmax-provider-exec-failed-v1\\000forged'; exit 125",
            Duration::from_secs(2),
            128,
        );
        let output = run_test_process(&forged).unwrap();
        let mut want = LAUNCH_EXEC_FAILURE_PREFIX.to_vec();
        want.extend_from_slice(b"forged");
        assert_eq!(output.stdout(), want);
        assert_eq!(output.exit(), LocalProcessExit::Code(125));
    }

    #[test]
    fn validates_every_bound_before_spawn_and_decode() {
        let cwd = std::env::temp_dir();
        for (program, want) in [
            (OsString::new(), LocalProcessError::InvalidProgram),
            (
                OsString::from("relative/tool"),
                LocalProcessError::InvalidProgram,
            ),
        ] {
            assert_eq!(
                LocalProcessRequest::new(program, [], &cwd, Duration::from_secs(1), 1).unwrap_err(),
                want
            );
        }
        assert!(matches!(
            LocalProcessRequest::new(
                "git",
                vec![OsString::from("x"); MAX_LOCAL_PROCESS_ARGUMENTS + 1],
                &cwd,
                Duration::from_secs(1),
                1,
            ),
            Err(LocalProcessError::InvalidArguments)
        ));
        assert!(matches!(
            LocalProcessRequest::new("git", [], "relative", Duration::from_secs(1), 1),
            Err(LocalProcessError::InvalidWorkingDirectory)
        ));
        assert!(matches!(
            LocalProcessRequest::new("git", [], &cwd, Duration::ZERO, 1),
            Err(LocalProcessError::InvalidTimeout)
        ));
        assert!(matches!(
            LocalProcessRequest::new("git", [], &cwd, Duration::from_secs(1), 0),
            Err(LocalProcessError::InvalidOutputLimit)
        ));
        let request = LocalProcessRequest::new("git", [], &cwd, Duration::from_secs(1), 1).unwrap();
        assert!(matches!(
            request.with_environment_overrides([
                (OsString::from("DUPLICATE"), None),
                (OsString::from("DUPLICATE"), Some(OsString::from("value"))),
            ]),
            Err(LocalProcessError::InvalidEnvironment)
        ));
        for name in [OsString::new(), OsString::from("BAD=NAME")] {
            let request =
                LocalProcessRequest::new("git", [], &cwd, Duration::from_secs(1), 1).unwrap();
            assert!(matches!(
                request.with_environment_overrides([(name, None)]),
                Err(LocalProcessError::InvalidEnvironment)
            ));
        }

        let mut encoded = encode_request(
            &shell_request("exit 0", Duration::from_secs(1), 1),
            [7; LAUNCH_NONCE_BYTES],
        );
        encoded.push(0);
        assert!(decode_request(&encoded).is_err());
        assert!(decode_request(b"bad magic").is_err());
    }

    #[test]
    fn output_framing_and_lifecycle_priority_are_deterministic() {
        let nonce = [7; LAUNCH_NONCE_BYTES];
        let mut marker = LAUNCH_READY_PREFIX.to_vec();
        marker.extend_from_slice(&nonce);
        let mut output = OutputCollector::new(4, nonce);
        output.push(b"ignored preamble").unwrap();
        output.push(&marker[..12]).unwrap();
        let mut marker_tail = marker[12..].to_vec();
        marker_tail.extend_from_slice(b"Tr");
        output.push(&marker_tail).unwrap();
        output.push(b"oy").unwrap();
        assert!(output.ready);
        assert_eq!(output.stdout, b"Troy");
        assert_eq!(output.push(b"!"), Err(LocalProcessError::OutputTooLarge));

        let now = Instant::now();
        assert_eq!(
            lifecycle_decision(false, now, now + Duration::from_secs(1)),
            LifecycleDecision::Continue
        );
        assert_eq!(
            lifecycle_decision(false, now, now),
            LifecycleDecision::Timeout
        );
        assert_eq!(
            lifecycle_decision(true, now, now),
            LifecycleDecision::Complete
        );
    }

    #[test]
    #[ignore = "private launcher subprocess entry"]
    fn hidden_launcher_subprocess() {
        process::exit(i32::from(run_hidden_launcher()));
    }

    #[test]
    #[ignore = "provider identity subprocess entry"]
    fn provider_probe_subprocess() {
        assert_eq!(std::env::var("ARGMAX_TEST_PROBE").unwrap(), "identity");
        let pid = getpid();
        let sid = getsid(None).unwrap();
        let process_group = getpgrp();
        print!(
            "ARGMAX_IDENTITY:{}:{}:{}",
            pid.as_raw(),
            sid.as_raw(),
            process_group.as_raw()
        );
        io::stdout().flush().unwrap();
        process::exit(0);
    }

    #[test]
    #[ignore = "ambient descriptor parent subprocess entry"]
    fn ambient_fd_parent_subprocess() {
        let file = File::open("/").unwrap();
        let descriptor = (200..=240)
            .find(|descriptor| fcntl(*descriptor, FcntlArg::F_GETFD).is_err())
            .unwrap();
        dup2(file.as_raw_fd(), descriptor).unwrap();
        fcntl(descriptor, FcntlArg::F_SETFD(FdFlag::empty())).unwrap();
        let script = format!(
            "if (: >&{descriptor}) 2>/dev/null; then printf leaked; else printf closed; fi"
        );
        let request = shell_request(&script, Duration::from_secs(2), 64);
        let result = run_test_process(&request);
        close(descriptor).unwrap();
        assert_eq!(result.unwrap().stdout(), b"closed");
    }

    #[test]
    #[ignore = "controlling-terminal parent subprocess entry"]
    fn controlling_terminal_parent_subprocess() {
        assert!(OpenOptions::new().write(true).open("/dev/tty").is_ok());
        let request = shell_request(
            "if /bin/sh -c 'printf leak >/dev/tty' 2>/dev/null; then \
             printf leaked; else printf detached; fi",
            Duration::from_secs(2),
            64,
        );
        assert_eq!(run_test_process(&request).unwrap().stdout(), b"detached");
    }

    #[test]
    #[ignore = "escaped provider subprocess entry"]
    fn escaped_provider_subprocess() {
        setsid().unwrap();
        let ready = std::env::var_os("ARGMAX_TEST_READY").unwrap();
        fs::write(ready, getpid().as_raw().to_string()).unwrap();
        thread::sleep(Duration::from_secs(10));
    }
}
