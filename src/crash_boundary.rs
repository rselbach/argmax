//! Panic containment and explicit rescue-shell recovery for interactive sessions.
//!
//! The boundary suppresses process panic output while the protected wrapper owns
//! raw terminal state, restores terminal state before performing diagnostic I/O,
//! and leaves process creation to the caller. Rescue commands deliberately bypass
//! shell startup files so a broken argmax integration cannot recurse during recovery.
//!
//! Panic hooks are process-global. Boundary calls are serialized, and the
//! executable runtime must not replace the process panic hook concurrently.

use std::any::Any;
use std::backtrace::Backtrace;
use std::error::Error;
use std::fmt;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use crate::diagnostics::{
    CrashReport, CrashReportStore, DiagnosticError, MAX_CRASH_BACKTRACE_BYTES,
    MAX_CRASH_FAILURE_BYTES,
};
use crate::pty::{
    ENV_ACTIVE_SHELL, ENV_CONTROL_FD, ENV_EVENT_FD, ENV_PRIVATE_SESSION, ENV_SESSION_OWNER_PID,
    SelectedShell, ShellKind,
};

const POSIX_RESCUE_SHELL: &str = "/bin/sh";
const FALLBACK_ARGUMENTS: &[&str] = &["-i"];
const BASH_ARGUMENTS: &[&str] = &["--noprofile", "--norc", "--noediting", "-i"];
const ZSH_ARGUMENTS: &[&str] = &["-f", "-i"];
const FISH_ARGUMENTS: &[&str] = &["--no-config", "-i"];
const RESCUE_ENVIRONMENT_REMOVALS: &[&str] = &[
    ENV_PRIVATE_SESSION,
    ENV_EVENT_FD,
    ENV_CONTROL_FD,
    ENV_ACTIVE_SHELL,
    ENV_SESSION_OWNER_PID,
    "ENV",
    "BASH_ENV",
    "INPUTRC",
    "PROMPT_COMMAND",
    "PS0",
    "PS1",
    "PS2",
    "PS3",
    "PS4",
    "BASHOPTS",
    "SHELLOPTS",
    "TMOUT",
    "MAIL",
    "MAILPATH",
    "MAILCHECK",
    "PROMPT",
    "PROMPT2",
    "PROMPT3",
    "PROMPT4",
    "RPROMPT",
    "RPROMPT2",
    "RPS1",
    "RPS2",
    "SPROMPT",
    "PSVAR",
    "ZDOTDIR",
    "fish_greeting",
    "fish_function_path",
    "fish_complete_path",
];
const BACKTRACE_UNAVAILABLE: &str = "backtrace unavailable";
const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

static PANIC_HOOK_GATE: Mutex<()> = Mutex::new(());

type PanicHook = dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static;

/// A bounded abnormal wrapper failure destined only for a private crash report.
pub struct WrapperFailure {
    description: Box<str>,
}

impl WrapperFailure {
    /// Copies at most the crash-report failure bound from `description`.
    #[must_use]
    pub fn new(description: &str) -> Self {
        Self {
            description: bounded_copy(description, MAX_CRASH_FAILURE_BYTES).into(),
        }
    }
}

impl fmt::Debug for WrapperFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WrapperFailure")
            .field("description_bytes", &self.description.len())
            .finish()
    }
}

impl fmt::Display for WrapperFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the interactive wrapper failed")
    }
}

impl Error for WrapperFailure {}

/// Best-effort result returned by the terminal-restoration callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalRestoration {
    /// Termios and all argmax-owned visual state were restored.
    Restored,
    /// Restoration returned normally but could not restore every owned state.
    Incomplete,
}

/// Observed terminal-restoration result after panic containment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestorationOutcome {
    /// The callback reported complete restoration.
    Restored,
    /// The callback reported incomplete best-effort restoration.
    Incomplete,
    /// The callback panicked; its payload was suppressed and never formatted.
    Panicked,
}

/// One direct command the caller may start as a rescue shell.
#[derive(Clone, Eq, PartialEq)]
pub struct RescueCommand {
    shell: Option<ShellKind>,
    executable: PathBuf,
}

impl RescueCommand {
    /// Absolute executable path selected before the interactive callback starts.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Supported shell kind, or `None` for the `/bin/sh` fallback.
    #[must_use]
    pub const fn shell(&self) -> Option<ShellKind> {
        self.shell
    }

    /// Arguments that request an interactive, configuration-free rescue shell.
    #[must_use]
    pub const fn arguments(&self) -> &'static [&'static str] {
        match self.shell {
            Some(ShellKind::Bash) => BASH_ARGUMENTS,
            Some(ShellKind::Zsh) => ZSH_ARGUMENTS,
            Some(ShellKind::Fish) => FISH_ARGUMENTS,
            None => FALLBACK_ARGUMENTS,
        }
    }

    /// Inherited variables the caller must remove before starting this command.
    ///
    /// Removing argmax ownership descriptors, startup selectors, prompt code,
    /// and option imports prevents a rescue shell from attaching to a dead
    /// wrapper or executing inherited code before the first user input. Normal
    /// terminal, home-directory, locale, and executable-search variables remain.
    #[must_use]
    pub const fn environment_to_remove(&self) -> &'static [&'static str] {
        RESCUE_ENVIRONMENT_REMOVALS
    }
}

impl fmt::Debug for RescueCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RescueCommand")
            .field("shell", &self.shell)
            .field("executable", &"<validated absolute path>")
            .field("argument_count", &self.arguments().len())
            .field(
                "environment_removal_count",
                &RESCUE_ENVIRONMENT_REMOVALS.len(),
            )
            .finish()
    }
}

/// Preallocated primary and fallback commands for crash recovery.
#[derive(Clone, Eq, PartialEq)]
pub struct RescueShellSpec {
    primary: RescueCommand,
    fallback: Option<RescueCommand>,
}

impl RescueShellSpec {
    /// Uses a previously validated supported shell, then `/bin/sh` if it fails.
    ///
    /// Construct this before entering raw terminal mode so recovery itself does
    /// not need to discover a shell or clone an environment-derived path.
    #[must_use]
    pub fn from_selected(selected: &SelectedShell) -> Self {
        Self {
            primary: RescueCommand {
                shell: Some(selected.kind()),
                executable: selected.executable().to_path_buf(),
            },
            fallback: Some(posix_rescue_command()),
        }
    }

    /// Uses `/bin/sh` when no supported selected shell is available.
    #[must_use]
    pub fn posix_fallback() -> Self {
        Self {
            primary: posix_rescue_command(),
            fallback: None,
        }
    }

    /// Command to try first.
    #[must_use]
    pub const fn primary(&self) -> &RescueCommand {
        &self.primary
    }

    /// `/bin/sh` fallback after a selected shell fails, if one is needed.
    #[must_use]
    pub const fn fallback(&self) -> Option<&RescueCommand> {
        self.fallback.as_ref()
    }
}

impl fmt::Debug for RescueShellSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RescueShellSpec")
            .field("primary", &self.primary)
            .field("has_fallback", &self.fallback.is_some())
            .finish()
    }
}

fn posix_rescue_command() -> RescueCommand {
    RescueCommand {
        shell: None,
        executable: PathBuf::from(POSIX_RESCUE_SHELL),
    }
}

/// Recovery action returned to the executable runtime without spawning it.
#[must_use = "the caller must start the returned rescue shell when recovery is required"]
#[derive(Debug, Eq, PartialEq)]
pub enum RecoveryDecision {
    /// Start the prepared rescue command, falling back as specified.
    StartRescueShell(RescueShellSpec),
}

/// Reason a crash boundary could not start without corrupting outer ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrashBoundaryRejection {
    /// A boundary already owns the process-global panic hook.
    AlreadyActive,
}

/// Result of persisting one private crash report.
pub enum ReportOutcome {
    /// The report was written at the retained absolute path.
    Written(PathBuf),
    /// The crash-report store rejected or could not persist the report.
    Failed(DiagnosticError),
    /// Report construction or store handling panicked and was contained.
    Panicked,
    /// A report writer violated the absolute-path contract; nothing was emitted.
    RelativePathRejected,
}

impl ReportOutcome {
    /// Absolute report path when persistence succeeded.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Written(path) => Some(path),
            Self::Failed(_) | Self::Panicked | Self::RelativePathRejected => None,
        }
    }

    /// Structured diagnostic-store failure when one was returned.
    #[must_use]
    pub const fn error(&self) -> Option<&DiagnosticError> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Written(_) | Self::Panicked | Self::RelativePathRejected => None,
        }
    }
}

impl fmt::Debug for ReportOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Written(path) => formatter
                .debug_struct("Written")
                .field("absolute", &path.is_absolute())
                .field("path_bytes", &path.as_os_str().len())
                .finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
            Self::Panicked => formatter.write_str("Panicked"),
            Self::RelativePathRejected => formatter.write_str("RelativePathRejected"),
        }
    }
}

/// Result of emitting the successful report path to the supplied STDERR writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationOutcome {
    /// A reversible printable-ASCII path representation and newline were
    /// written and flushed.
    Emitted,
    /// No absolute report path existed, so the writer was not called.
    NotAttempted,
    /// The writer returned a stable I/O failure category.
    Failed(io::ErrorKind),
    /// The writer panicked; its payload was suppressed and never formatted.
    Panicked,
}

/// Complete contained-crash result for the executable runtime.
#[must_use = "crash recovery status and the rescue decision must be handled"]
pub struct CrashRecovery {
    restoration: RestorationOutcome,
    report: ReportOutcome,
    notification: NotificationOutcome,
    decision: RecoveryDecision,
}

impl CrashRecovery {
    /// Terminal restoration result, always produced before report I/O.
    #[must_use]
    pub const fn restoration(&self) -> RestorationOutcome {
        self.restoration
    }

    /// Private crash-report persistence result.
    #[must_use]
    pub const fn report(&self) -> &ReportOutcome {
        &self.report
    }

    /// STDERR path notification result.
    #[must_use]
    pub const fn notification(&self) -> NotificationOutcome {
        self.notification
    }

    /// Explicit rescue action for the caller.
    pub const fn decision(&self) -> &RecoveryDecision {
        &self.decision
    }
}

impl fmt::Debug for CrashRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrashRecovery")
            .field("restoration", &self.restoration)
            .field("report", &self.report)
            .field("notification", &self.notification)
            .field("decision", &self.decision)
            .finish()
    }
}

/// Completion or contained recovery from an interactive wrapper callback.
#[must_use = "a contained failure requires the returned rescue decision"]
pub enum CrashBoundaryOutcome<T> {
    /// The wrapper returned normally.
    Completed(T),
    /// A panic or explicit abnormal failure was contained.
    Recovered(CrashRecovery),
    /// The callback was not run because another boundary owns this thread.
    Rejected(CrashBoundaryRejection),
}

impl<T> fmt::Debug for CrashBoundaryOutcome<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed(_) => formatter.write_str("Completed(<opaque>)"),
            Self::Recovered(recovery) => {
                formatter.debug_tuple("Recovered").field(recovery).finish()
            }
            Self::Rejected(rejection) => {
                formatter.debug_tuple("Rejected").field(rejection).finish()
            }
        }
    }
}

/// Runs one interactive wrapper callback behind the crash boundary.
///
/// A wrapper panic is captured by a thread-specific temporary panic hook, so
/// the default hook cannot print while the terminal is raw. On either a panic
/// or [`WrapperFailure`], `restore_terminal` runs before report construction,
/// filesystem access, or STDERR output. Only a successfully persisted absolute
/// report path, percent-escaped into printable ASCII, plus `\n` is sent to
/// `stderr`. A nested or concurrent call is rejected before any callback or
/// recovery resource is used.
///
/// The returned rescue decision is inert. The caller must apply its argument
/// and environment-removal contract and perform any process creation after this
/// function returns.
pub fn run_with_crash_boundary<T, F, R, W>(
    store: &CrashReportStore,
    version: &str,
    rescue: RescueShellSpec,
    stderr: &mut W,
    restore_terminal: R,
    wrapper: F,
) -> CrashBoundaryOutcome<T>
where
    F: FnOnce() -> Result<T, WrapperFailure>,
    R: FnOnce() -> TerminalRestoration,
    W: Write + ?Sized,
{
    run_with_reporter(
        version,
        rescue,
        stderr,
        restore_terminal,
        wrapper,
        |report| store.write(report),
    )
}

fn run_with_reporter<T, F, R, W, S>(
    version: &str,
    rescue: RescueShellSpec,
    stderr: &mut W,
    restore_terminal: R,
    wrapper: F,
    report_writer: S,
) -> CrashBoundaryOutcome<T>
where
    F: FnOnce() -> Result<T, WrapperFailure>,
    R: FnOnce() -> TerminalRestoration,
    W: Write + ?Sized,
    S: FnOnce(&CrashReport) -> Result<PathBuf, DiagnosticError>,
{
    let Some(_gate) = BoundaryGate::enter() else {
        return CrashBoundaryOutcome::Rejected(CrashBoundaryRejection::AlreadyActive);
    };
    let capture = Arc::new(Mutex::new(None));
    let mut hook = ScopedPanicHook::install(Arc::clone(&capture));
    let wrapper_result = panic::catch_unwind(AssertUnwindSafe(wrapper));

    let incident = match wrapper_result {
        Ok(Ok(value)) => {
            hook.restore();
            return CrashBoundaryOutcome::Completed(value);
        }
        Ok(Err(failure)) => {
            // The wrapper returned and said why, so its description is
            // authoritative. A captured panic may be one it already recovered
            // from, or one from an unrelated worker, and letting that replace
            // the description reported a failure the wrapper did not return.
            // Its backtrace is still the more informative one, so it is kept.
            let captured = take_capture(&capture);
            PanicSnapshot {
                failure: failure.description.into(),
                backtrace: captured
                    .map_or_else(capture_backtrace_safely, |snapshot| snapshot.backtrace),
            }
        }
        Err(payload) => {
            let incident =
                take_capture(&capture).unwrap_or_else(|| snapshot_from_payload(payload.as_ref()));
            forget_panic_payload(payload);
            incident
        }
    };

    let restoration = match panic::catch_unwind(AssertUnwindSafe(restore_terminal)) {
        Ok(TerminalRestoration::Restored) => RestorationOutcome::Restored,
        Ok(TerminalRestoration::Incomplete) => RestorationOutcome::Incomplete,
        Err(payload) => {
            forget_panic_payload(payload);
            RestorationOutcome::Panicked
        }
    };

    let report = match panic::catch_unwind(AssertUnwindSafe(|| {
        let report = CrashReport::new(version, &incident.failure, &incident.backtrace);
        report_writer(&report)
    })) {
        Ok(Ok(path)) if path.is_absolute() => ReportOutcome::Written(path),
        Ok(Ok(_)) => ReportOutcome::RelativePathRejected,
        Ok(Err(error)) => ReportOutcome::Failed(error),
        Err(payload) => {
            forget_panic_payload(payload);
            ReportOutcome::Panicked
        }
    };

    let notification = match report.path() {
        Some(path) => {
            match panic::catch_unwind(AssertUnwindSafe(|| write_report_path(stderr, path))) {
                Ok(Ok(())) => NotificationOutcome::Emitted,
                Ok(Err(error)) => NotificationOutcome::Failed(error.kind()),
                Err(payload) => {
                    forget_panic_payload(payload);
                    NotificationOutcome::Panicked
                }
            }
        }
        None => NotificationOutcome::NotAttempted,
    };

    hook.restore();
    CrashBoundaryOutcome::Recovered(CrashRecovery {
        restoration,
        report,
        notification,
        decision: RecoveryDecision::StartRescueShell(rescue),
    })
}

struct PanicSnapshot {
    failure: String,
    backtrace: String,
}

struct BoundaryGate {
    _gate: MutexGuard<'static, ()>,
}

impl BoundaryGate {
    fn enter() -> Option<Self> {
        match PANIC_HOOK_GATE.try_lock() {
            Ok(gate) => Some(Self { _gate: gate }),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(error)) => Some(Self {
                _gate: error.into_inner(),
            }),
        }
    }
}

struct ScopedPanicHook {
    previous: Arc<Mutex<Option<Box<PanicHook>>>>,
    installed: bool,
}

impl ScopedPanicHook {
    fn install(capture: Arc<Mutex<Option<PanicSnapshot>>>) -> Self {
        let previous = Arc::new(Mutex::new(Some(panic::take_hook())));
        panic::set_hook(Box::new(move |information| {
            route_panic(information, &capture);
        }));
        Self {
            previous,
            installed: true,
        }
    }

    fn restore(&mut self) {
        if !self.installed {
            return;
        }
        let installed = panic::take_hook();
        drop(installed);
        if let Some(previous) = recover_lock(&self.previous).take() {
            panic::set_hook(previous);
        }
        self.installed = false;
    }
}

impl Drop for ScopedPanicHook {
    fn drop(&mut self) {
        if self.installed && !thread::panicking() {
            self.restore();
        }
    }
}

fn route_panic(information: &PanicHookInfo<'_>, capture: &Mutex<Option<PanicSnapshot>>) {
    // Any argmax worker may panic while the owner thread has the terminal in
    // raw mode. Capture the first incident without invoking a prior hook; the
    // owner observes worker failure through its bounded channel/join path and
    // completes terminal restoration before diagnostics are emitted.
    let mut capture = recover_lock(capture);
    if capture.is_none() {
        *capture = Some(snapshot_from_hook(information));
    }
}

fn take_capture(capture: &Mutex<Option<PanicSnapshot>>) -> Option<PanicSnapshot> {
    recover_lock(capture).take()
}

fn snapshot_from_hook(information: &PanicHookInfo<'_>) -> PanicSnapshot {
    let mut failure = BoundedText::new(MAX_CRASH_FAILURE_BYTES);
    let _ = failure.write_str("panic");
    if let Some(location) = information.location() {
        let _ = write!(
            failure,
            " at {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }
    let _ = failure.write_str(": ");
    write_payload(&mut failure, information.payload());
    PanicSnapshot {
        failure: failure.finish(),
        // This runs inside the process panic hook, where a second panic is not
        // an unwind but an immediate abort, before the terminal is restored.
        // Capturing a backtrace allocates and resolves symbols, so it is
        // guarded here for the same reason it is guarded off the hook.
        backtrace: capture_backtrace_safely(),
    }
}

fn snapshot_from_payload(payload: &(dyn Any + Send)) -> PanicSnapshot {
    let mut failure = BoundedText::new(MAX_CRASH_FAILURE_BYTES);
    let _ = failure.write_str("panic: ");
    write_payload(&mut failure, payload);
    PanicSnapshot {
        failure: failure.finish(),
        backtrace: capture_backtrace_safely(),
    }
}

fn write_payload(output: &mut BoundedText, payload: &(dyn Any + Send)) {
    if let Some(message) = payload.downcast_ref::<&str>() {
        let _ = output.write_str(message);
    } else if let Some(message) = payload.downcast_ref::<String>() {
        let _ = output.write_str(message);
    } else {
        let _ = output.write_str("non-string panic payload");
    }
}

fn capture_backtrace_safely() -> String {
    match panic::catch_unwind(AssertUnwindSafe(capture_backtrace)) {
        Ok(backtrace) => backtrace,
        Err(payload) => {
            forget_panic_payload(payload);
            BACKTRACE_UNAVAILABLE.to_owned()
        }
    }
}

fn capture_backtrace() -> String {
    let mut output = BoundedText::new(MAX_CRASH_BACKTRACE_BYTES);
    let _ = write!(output, "{}", Backtrace::force_capture());
    output.finish()
}

fn forget_panic_payload(payload: Box<dyn Any + Send>) {
    // A custom panic payload may have a destructor that panics. Crash recovery
    // is process-terminal and deliberately leaks this one bounded ownership
    // object rather than allowing untrusted destructor code to unwind again.
    std::mem::forget(payload);
}

fn write_report_path(writer: &mut (impl Write + ?Sized), path: &Path) -> io::Result<()> {
    debug_assert!(path.is_absolute());
    #[cfg(unix)]
    let bytes = path.as_os_str().as_bytes();
    #[cfg(not(unix))]
    let bytes = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "crash report path is not Unicode",
        )
    })?;
    #[cfg(not(unix))]
    let bytes = bytes.as_bytes();

    for &byte in bytes {
        if (b' '..=b'~').contains(&byte) && byte != b'%' {
            writer.write_all(std::slice::from_ref(&byte))?;
        } else {
            let escaped = [
                b'%',
                HEX_DIGITS[usize::from(byte >> 4)],
                HEX_DIGITS[usize::from(byte & 0x0f)],
            ];
            writer.write_all(&escaped)?;
        }
    }
    writer.write_all(b"\n")?;
    writer.flush()
}

fn bounded_copy(value: &str, limit: usize) -> String {
    let mut output = BoundedText::new(limit);
    let _ = output.write_str(value);
    output.finish()
}

struct BoundedText {
    value: String,
    limit: usize,
}

impl BoundedText {
    fn new(limit: usize) -> Self {
        Self {
            value: String::with_capacity(limit.min(4_096)),
            limit,
        }
    }

    fn finish(self) -> String {
        self.value
    }
}

impl fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let remaining = self.limit.saturating_sub(self.value.len());
        if remaining == 0 {
            return Err(fmt::Error);
        }
        if value.len() <= remaining {
            self.value.push_str(value);
            return Ok(());
        }
        let mut end = remaining;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        self.value.push_str(&value[..end]);
        Err(fmt::Error)
    }
}

fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    use tempfile::TempDir;

    use crate::pty::{ShellSelectionRequest, select_shell};

    use super::*;

    static TEST_BOUNDARY_GATE: Mutex<()> = Mutex::new(());

    fn serialize_boundary_test() -> MutexGuard<'static, ()> {
        recover_lock(&TEST_BOUNDARY_GATE)
    }

    fn store(directory: &TempDir) -> CrashReportStore {
        CrashReportStore::open(directory.path()).unwrap()
    }

    fn recover(outcome: CrashBoundaryOutcome<usize>) -> CrashRecovery {
        match outcome {
            CrashBoundaryOutcome::Completed(_) => panic!("expected crash recovery"),
            CrashBoundaryOutcome::Recovered(recovery) => recovery,
            CrashBoundaryOutcome::Rejected(rejection) => {
                panic!("crash boundary was unexpectedly rejected: {rejection:?}")
            }
        }
    }

    fn report_contents(recovery: &CrashRecovery) -> String {
        fs::read_to_string(recovery.report().path().unwrap()).unwrap()
    }

    fn decode_notification(bytes: &[u8]) -> Vec<u8> {
        let encoded = bytes.strip_suffix(b"\n").unwrap();
        let mut decoded = Vec::new();
        let mut index = 0;
        while index < encoded.len() {
            if encoded[index] == b'%' {
                let high = hex_value(encoded[index + 1]);
                let low = hex_value(encoded[index + 2]);
                decoded.push((high << 4) | low);
                index += 3;
            } else {
                decoded.push(encoded[index]);
                index += 1;
            }
        }
        decoded
    }

    const fn hex_value(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'A'..=b'F' => byte - b'A' + 10,
            _ => panic!("invalid hexadecimal notification"),
        }
    }

    fn stop_shell(child: &mut Child) -> std::process::ExitStatus {
        child.stdin.take().unwrap().write_all(b"exit\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("rescue shell did not exit after input");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_poisoned_environment_does_not_run(command: &RescueCommand) {
        let directory = TempDir::new().unwrap();
        let marker = directory.path().join("poison-ran");
        let startup = directory.path().join("poison-startup");
        let poison = "printf '%s' poison > \"$ARGMAX_CRASH_TEST_MARKER\"";
        fs::write(&startup, format!("{poison}\n")).unwrap();

        let mut unprotected = Command::new(command.executable());
        unprotected
            .args(command.arguments())
            .env("ARGMAX_CRASH_TEST_MARKER", &marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match command.shell() {
            Some(ShellKind::Bash) => {
                unprotected.env("PROMPT_COMMAND", poison);
            }
            None => {
                unprotected.env("ENV", &startup);
            }
            Some(ShellKind::Zsh | ShellKind::Fish) => {
                panic!("poison control supports Bash and /bin/sh only")
            }
        }
        let mut unprotected = unprotected.spawn().unwrap();
        thread::sleep(Duration::from_millis(150));
        let poison_is_live = marker.exists();
        let _ = stop_shell(&mut unprotected);
        assert!(poison_is_live, "test poison did not run before input");
        fs::remove_file(&marker).unwrap();

        let mut child = Command::new(command.executable());
        child
            .args(command.arguments())
            .env("ARGMAX_CRASH_TEST_MARKER", &marker)
            .env("ENV", &startup)
            .env("BASH_ENV", &startup)
            .env("INPUTRC", &startup)
            .env("PROMPT_COMMAND", poison)
            .env("PS0", format!("$({poison})"))
            .env("PS1", format!("$({poison})"))
            .env("PS2", format!("$({poison})"))
            .env("PS3", format!("$({poison})"))
            .env("PS4", format!("$({poison})"))
            .env("BASHOPTS", "promptvars")
            .env("SHELLOPTS", "verbose:xtrace")
            .env("PROMPT", format!("$({poison})"))
            .env("PROMPT2", format!("$({poison})"))
            .env("PROMPT3", format!("$({poison})"))
            .env("PROMPT4", format!("$({poison})"))
            .env("RPROMPT", format!("$({poison})"))
            .env("RPROMPT2", format!("$({poison})"))
            .env("RPS1", format!("$({poison})"))
            .env("RPS2", format!("$({poison})"))
            .env("SPROMPT", format!("$({poison})"))
            .env("PSVAR", format!("$({poison})"))
            .env("ZDOTDIR", directory.path())
            .env("fish_function_path", directory.path())
            .env("fish_complete_path", directory.path())
            .env("fish_greeting", poison)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for variable in command.environment_to_remove() {
            child.env_remove(variable);
        }

        let mut child = child.spawn().unwrap();
        thread::sleep(Duration::from_millis(150));
        let ran_before_input = marker.exists();
        let status = stop_shell(&mut child);

        assert!(!ran_before_input, "inherited prompt code ran before input");
        assert!(
            !marker.exists(),
            "inherited startup code ran in rescue shell"
        );
        assert!(status.success(), "rescue shell status: {status:?}");
    }

    #[test]
    fn normal_completion_is_opaque_and_does_not_touch_recovery_resources() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let restored = AtomicBool::new(false);
        let mut output = Vec::new();

        let outcome = run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || {
                restored.store(true, Ordering::Relaxed);
                TerminalRestoration::Restored
            },
            || Ok(42),
        );

        assert!(matches!(outcome, CrashBoundaryOutcome::Completed(42)));
        assert_eq!(format!("{outcome:?}"), "Completed(<opaque>)");
        assert!(!restored.load(Ordering::Relaxed));
        assert!(output.is_empty());
        assert_eq!(store.newest().unwrap(), None);
    }

    #[test]
    fn explicit_failure_restores_then_writes_and_emits_only_absolute_path() {
        struct OrderedWriter<'a> {
            restored: &'a AtomicBool,
            bytes: Vec<u8>,
        }

        impl Write for OrderedWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                assert!(self.restored.load(Ordering::Acquire));
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                assert!(self.restored.load(Ordering::Acquire));
                Ok(())
            }
        }

        let _serial = serialize_boundary_test();

        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let restored = AtomicBool::new(false);
        let mut output = OrderedWriter {
            restored: &restored,
            bytes: Vec::new(),
        };
        let recovery = recover(run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || {
                restored.store(true, Ordering::Release);
                TerminalRestoration::Restored
            },
            || Err(WrapperFailure::new("Greendale wrapper stopped")),
        ));

        assert_eq!(recovery.restoration(), RestorationOutcome::Restored);
        assert_eq!(recovery.notification(), NotificationOutcome::Emitted);
        let path = recovery.report().path().unwrap();
        assert!(path.is_absolute());
        assert_eq!(
            decode_notification(&output.bytes),
            path.as_os_str().as_bytes()
        );
        assert!(report_contents(&recovery).contains("Greendale wrapper stopped"));
    }

    #[test]
    fn panic_captures_location_message_and_forced_backtrace_privately() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let mut output = Vec::new();
        let recovery = recover(run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || TerminalRestoration::Restored,
            || -> Result<usize, WrapperFailure> { panic!("Dean caused the wrapper panic") },
        ));
        let contents = report_contents(&recovery);

        assert!(contents.contains("panic at"), "{contents}");
        assert!(
            contents.contains("Dean caused the wrapper panic"),
            "{contents}"
        );
        let backtrace = contents.split("backtrace:\n").nth(1).unwrap();
        assert!(!backtrace.trim().is_empty());
        assert_eq!(recovery.notification(), NotificationOutcome::Emitted);
    }

    #[test]
    fn panic_fields_are_bounded_and_secrets_are_redacted() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let mut output = Vec::new();
        let oversized = format!(
            "api_key=dean-secret {}",
            "é".repeat(MAX_CRASH_FAILURE_BYTES)
        );
        let recovery = recover(run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || TerminalRestoration::Restored,
            move || -> Result<usize, WrapperFailure> { panic_any(oversized) },
        ));
        let contents = report_contents(&recovery);

        assert!(!contents.contains("dean-secret"));
        assert!(contents.contains("api_key=<redacted>"));
        assert!(contents.len() <= MAX_CRASH_FAILURE_BYTES + MAX_CRASH_BACKTRACE_BYTES + 1_024);
    }

    #[test]
    fn custom_panic_payload_is_neither_formatted_nor_dropped() {
        struct DropBomb(Arc<AtomicBool>);

        impl Drop for DropBomb {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
                panic!("panic payload destructor ran");
            }
        }

        let _serial = serialize_boundary_test();

        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let dropped = Arc::new(AtomicBool::new(false));
        let payload_flag = Arc::clone(&dropped);
        let mut output = Vec::new();
        let outer = catch_unwind(AssertUnwindSafe(|| {
            recover(run_with_crash_boundary(
                &store,
                "1.2.3",
                RescueShellSpec::posix_fallback(),
                &mut output,
                || TerminalRestoration::Restored,
                move || -> Result<usize, WrapperFailure> {
                    panic_any(DropBomb(payload_flag));
                },
            ))
        }));

        assert!(outer.is_ok());
        let recovery = outer.unwrap();
        assert!(!dropped.load(Ordering::Relaxed));
        assert!(report_contents(&recovery).contains("non-string panic payload"));
    }

    #[test]
    fn restoration_panic_is_contained_without_replacing_primary_failure() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let mut output = Vec::new();
        let outer = catch_unwind(AssertUnwindSafe(|| {
            recover(run_with_crash_boundary(
                &store,
                "1.2.3",
                RescueShellSpec::posix_fallback(),
                &mut output,
                || -> TerminalRestoration { panic!("restoration payload") },
                || -> Result<usize, WrapperFailure> { panic!("primary wrapper payload") },
            ))
        }));

        assert!(outer.is_ok());
        let recovery = outer.unwrap();
        assert_eq!(recovery.restoration(), RestorationOutcome::Panicked);
        assert_eq!(recovery.notification(), NotificationOutcome::Emitted);
        let contents = report_contents(&recovery);
        assert!(contents.contains("primary wrapper payload"));
        assert!(!contents.contains("restoration payload"));
    }

    #[test]
    fn incomplete_restoration_is_structured_and_diagnostics_continue() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let mut output = Vec::new();
        let recovery = recover(run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || TerminalRestoration::Incomplete,
            || Err(WrapperFailure::new("incomplete cleanup")),
        ));

        assert_eq!(recovery.restoration(), RestorationOutcome::Incomplete);
        assert!(matches!(recovery.report(), ReportOutcome::Written(_)));
        assert_eq!(recovery.notification(), NotificationOutcome::Emitted);
    }

    struct PanickingWriter;

    impl Write for PanickingWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            panic!("stderr writer payload")
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stderr_writer_panic_is_contained_and_report_remains_primary() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let outer = catch_unwind(AssertUnwindSafe(|| {
            let mut output = PanickingWriter;
            recover(run_with_crash_boundary(
                &store,
                "1.2.3",
                RescueShellSpec::posix_fallback(),
                &mut output,
                || TerminalRestoration::Restored,
                || -> Result<usize, WrapperFailure> { panic!("primary wrapper payload") },
            ))
        }));

        assert!(outer.is_ok());
        let recovery = outer.unwrap();
        assert_eq!(recovery.notification(), NotificationOutcome::Panicked);
        let contents = report_contents(&recovery);
        assert!(contents.contains("primary wrapper payload"));
        assert!(!contents.contains("stderr writer payload"));
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stderr_failure_is_reduced_to_an_io_kind() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let mut output = FailingWriter;
        let recovery = recover(run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || TerminalRestoration::Restored,
            || Err(WrapperFailure::new("failure")),
        ));

        assert_eq!(
            recovery.notification(),
            NotificationOutcome::Failed(io::ErrorKind::BrokenPipe)
        );
        assert!(matches!(recovery.report(), ReportOutcome::Written(_)));
    }

    #[test]
    fn store_error_prevents_stderr_output_and_remains_structured() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        fs::remove_dir_all(directory.path().join("argmax")).unwrap();
        let mut output = Vec::new();
        let recovery = recover(run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || TerminalRestoration::Restored,
            || Err(WrapperFailure::new("failure")),
        ));

        assert!(matches!(recovery.report(), ReportOutcome::Failed(_)));
        assert!(recovery.report().error().is_some());
        assert_eq!(recovery.notification(), NotificationOutcome::NotAttempted);
        assert!(output.is_empty());
    }

    #[test]
    fn report_writer_panic_is_contained_before_stderr() {
        let _serial = serialize_boundary_test();
        let mut output = Vec::new();
        let outer = catch_unwind(AssertUnwindSafe(|| {
            recover(run_with_reporter(
                "1.2.3",
                RescueShellSpec::posix_fallback(),
                &mut output,
                || TerminalRestoration::Restored,
                || Err(WrapperFailure::new("primary failure")),
                |_report| -> Result<PathBuf, DiagnosticError> { panic!("store payload") },
            ))
        }));

        assert!(outer.is_ok());
        let recovery = outer.unwrap();
        assert!(matches!(recovery.report(), ReportOutcome::Panicked));
        assert_eq!(recovery.notification(), NotificationOutcome::NotAttempted);
        assert!(output.is_empty());
    }

    #[test]
    fn relative_report_path_is_rejected_without_output() {
        let _serial = serialize_boundary_test();
        let mut output = Vec::new();
        let recovery = recover(run_with_reporter(
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || TerminalRestoration::Restored,
            || Err(WrapperFailure::new("failure")),
            |_report| Ok(PathBuf::from("relative.log")),
        ));

        assert!(matches!(
            recovery.report(),
            ReportOutcome::RelativePathRejected
        ));
        assert_eq!(recovery.notification(), NotificationOutcome::NotAttempted);
        assert!(output.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn report_notification_reversibly_escapes_terminal_control_bytes() {
        let _serial = serialize_boundary_test();
        let raw_path = b"/tmp/Greendale%\n\x1b\x7f\x80-crash.log".to_vec();
        let path = PathBuf::from(OsString::from_vec(raw_path.clone()));
        let reported_path = path.clone();
        let mut output = Vec::new();
        let recovery = recover(run_with_reporter(
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut output,
            || TerminalRestoration::Restored,
            || Err(WrapperFailure::new("failure")),
            move |_report| Ok(reported_path),
        ));

        assert_eq!(recovery.report().path(), Some(path.as_path()));
        assert_eq!(recovery.notification(), NotificationOutcome::Emitted);
        assert_eq!(decode_notification(&output), raw_path);
        assert!(output.ends_with(b"\n"));
        assert!(!output[..output.len() - 1].contains(&b'\n'));
        assert!(
            output[..output.len() - 1]
                .iter()
                .all(|byte| *byte >= b' ' && *byte != 0x7f)
        );
        for escaped in [b"%25".as_slice(), b"%0A", b"%1B", b"%7F", b"%80"] {
            assert!(
                output.windows(escaped.len()).any(|bytes| bytes == escaped),
                "missing escape {escaped:?} in {output:?}"
            );
        }
    }

    #[test]
    fn rescue_spec_bypasses_startup_files_and_scrubs_wrapper_ownership() {
        let request = ShellSelectionRequest {
            command_line: Some(ShellKind::Bash),
            search_path: Some("/bin:/usr/bin".into()),
            ..ShellSelectionRequest::default()
        };
        let selected = select_shell(&request).unwrap();
        let spec = RescueShellSpec::from_selected(&selected);

        assert_eq!(spec.primary().shell(), Some(ShellKind::Bash));
        assert!(spec.primary().executable().is_absolute());
        assert_eq!(spec.primary().arguments(), BASH_ARGUMENTS);
        for variable in [
            ENV_PRIVATE_SESSION,
            ENV_EVENT_FD,
            ENV_CONTROL_FD,
            ENV_ACTIVE_SHELL,
            ENV_SESSION_OWNER_PID,
            "ENV",
            "BASH_ENV",
            "INPUTRC",
            "PROMPT_COMMAND",
            "PS0",
            "PS1",
            "PS2",
            "PS3",
            "PS4",
            "BASHOPTS",
            "SHELLOPTS",
            "TMOUT",
            "MAIL",
            "MAILPATH",
            "MAILCHECK",
            "PROMPT",
            "PROMPT2",
            "PROMPT3",
            "PROMPT4",
            "RPROMPT",
            "RPROMPT2",
            "RPS1",
            "RPS2",
            "SPROMPT",
            "PSVAR",
            "ZDOTDIR",
            "fish_greeting",
            "fish_function_path",
            "fish_complete_path",
        ] {
            assert!(spec.primary().environment_to_remove().contains(&variable));
        }
        for preserved in ["TERM", "HOME", "PATH"] {
            assert!(!spec.primary().environment_to_remove().contains(&preserved));
        }
        let fallback = spec.fallback().unwrap();
        assert_eq!(fallback.executable(), Path::new("/bin/sh"));
        assert_eq!(fallback.arguments(), &["-i"]);
    }

    #[test]
    fn real_bash_rescue_does_not_execute_poisoned_prompt_environment() {
        let request = ShellSelectionRequest {
            command_line: Some(ShellKind::Bash),
            search_path: Some("/bin:/usr/bin".into()),
            ..ShellSelectionRequest::default()
        };
        let selected = select_shell(&request).unwrap();
        let spec = RescueShellSpec::from_selected(&selected);

        assert_poisoned_environment_does_not_run(spec.primary());
    }

    #[test]
    fn real_posix_rescue_does_not_execute_poisoned_prompt_environment() {
        let spec = RescueShellSpec::posix_fallback();
        assert_poisoned_environment_does_not_run(spec.primary());
    }

    #[test]
    fn fallback_rescue_spec_has_no_redundant_second_fallback() {
        let spec = RescueShellSpec::posix_fallback();
        assert_eq!(spec.primary().executable(), Path::new("/bin/sh"));
        assert_eq!(spec.primary().shell(), None);
        assert!(spec.fallback().is_none());
        assert!(matches!(
            RecoveryDecision::StartRescueShell(spec),
            RecoveryDecision::StartRescueShell(_)
        ));
    }

    #[test]
    fn debug_output_never_contains_failure_or_paths() {
        let failure = WrapperFailure::new("api_key=dean-secret");
        assert_eq!(
            format!("{failure:?}"),
            "WrapperFailure { description_bytes: 19 }"
        );

        let report = ReportOutcome::Written(PathBuf::from("/Users/troy/private/crash.log"));
        let debug = format!("{report:?}");
        assert!(!debug.contains("troy"));
        assert!(!debug.contains("crash.log"));

        let completed = CrashBoundaryOutcome::Completed("secret payload");
        assert_eq!(format!("{completed:?}"), "Completed(<opaque>)");
    }

    #[test]
    fn bounded_text_stops_on_utf8_boundary() {
        let copied = bounded_copy("ééé", 5);
        assert_eq!(copied, "éé");
        assert_eq!(copied.len(), 4);
    }

    #[test]
    fn same_thread_nested_boundary_is_rejected_before_callbacks_or_io() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let inner_wrapper_ran = AtomicBool::new(false);
        let inner_restore_ran = AtomicBool::new(false);
        let inner_report_ran = AtomicBool::new(false);
        let mut outer_output = Vec::new();
        let started = Instant::now();

        let outcome = run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut outer_output,
            || TerminalRestoration::Restored,
            || {
                let mut inner_output = PanickingWriter;
                let inner = run_with_reporter(
                    "1.2.3",
                    RescueShellSpec::posix_fallback(),
                    &mut inner_output,
                    || {
                        inner_restore_ran.store(true, Ordering::Relaxed);
                        TerminalRestoration::Restored
                    },
                    || {
                        inner_wrapper_ran.store(true, Ordering::Relaxed);
                        Ok(11)
                    },
                    |_report| {
                        inner_report_ran.store(true, Ordering::Relaxed);
                        Ok(PathBuf::from("/tmp/unused-crash.log"))
                    },
                );
                assert!(matches!(
                    inner,
                    CrashBoundaryOutcome::Rejected(CrashBoundaryRejection::AlreadyActive)
                ));
                Ok(7)
            },
        );

        assert!(matches!(outcome, CrashBoundaryOutcome::Completed(7)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!inner_wrapper_ran.load(Ordering::Relaxed));
        assert!(!inner_restore_ran.load(Ordering::Relaxed));
        assert!(!inner_report_ran.load(Ordering::Relaxed));
        assert!(outer_output.is_empty());
        assert_eq!(store.newest().unwrap(), None);
    }

    #[test]
    fn cross_thread_nested_boundary_rejects_before_outer_join_can_deadlock() {
        let _serial = serialize_boundary_test();
        let directory = TempDir::new().unwrap();
        let store = store(&directory);
        let inner_wrapper_ran = Arc::new(AtomicBool::new(false));
        let inner_restore_ran = Arc::new(AtomicBool::new(false));
        let inner_report_ran = Arc::new(AtomicBool::new(false));
        let mut outer_output = Vec::new();

        let outcome = run_with_crash_boundary(
            &store,
            "1.2.3",
            RescueShellSpec::posix_fallback(),
            &mut outer_output,
            || TerminalRestoration::Restored,
            || {
                let wrapper_flag = Arc::clone(&inner_wrapper_ran);
                let restore_flag = Arc::clone(&inner_restore_ran);
                let report_flag = Arc::clone(&inner_report_ran);
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                let worker = thread::spawn(move || {
                    let mut inner_output = PanickingWriter;
                    let inner = run_with_reporter(
                        "1.2.3",
                        RescueShellSpec::posix_fallback(),
                        &mut inner_output,
                        || {
                            restore_flag.store(true, Ordering::Relaxed);
                            TerminalRestoration::Restored
                        },
                        || {
                            wrapper_flag.store(true, Ordering::Relaxed);
                            Ok(11)
                        },
                        |_report| {
                            report_flag.store(true, Ordering::Relaxed);
                            Ok(PathBuf::from("/tmp/unused-crash.log"))
                        },
                    );
                    let rejected = matches!(
                        inner,
                        CrashBoundaryOutcome::Rejected(CrashBoundaryRejection::AlreadyActive)
                    );
                    let _ = sender.send(rejected);
                });

                let rejected = receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("nested boundary blocked on the panic-hook gate");
                worker.join().unwrap();
                assert!(rejected);
                Ok(7)
            },
        );

        assert!(matches!(outcome, CrashBoundaryOutcome::Completed(7)));
        assert!(!inner_wrapper_ran.load(Ordering::Relaxed));
        assert!(!inner_restore_ran.load(Ordering::Relaxed));
        assert!(!inner_report_ran.load(Ordering::Relaxed));
        assert!(outer_output.is_empty());
        assert_eq!(store.newest().unwrap(), None);
    }

    #[test]
    fn concurrent_boundaries_complete_or_reject_without_losing_results() {
        let _serial = serialize_boundary_test();
        let completed = Arc::new(AtomicUsize::new(0));
        let rejected = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let completed = Arc::clone(&completed);
            let rejected = Arc::clone(&rejected);
            workers.push(thread::spawn(move || {
                let directory = TempDir::new().unwrap();
                let store = store(&directory);
                let mut output = Vec::new();
                let outcome = run_with_crash_boundary(
                    &store,
                    "1.2.3",
                    RescueShellSpec::posix_fallback(),
                    &mut output,
                    || TerminalRestoration::Restored,
                    || Ok(7),
                );
                match outcome {
                    CrashBoundaryOutcome::Completed(7) => {
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                    CrashBoundaryOutcome::Rejected(CrashBoundaryRejection::AlreadyActive) => {
                        rejected.fetch_add(1, Ordering::Relaxed);
                    }
                    CrashBoundaryOutcome::Completed(_) | CrashBoundaryOutcome::Recovered(_) => {
                        panic!("unexpected concurrent boundary outcome")
                    }
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let completed = completed.load(Ordering::Relaxed);
        let rejected = rejected.load(Ordering::Relaxed);
        assert!(completed >= 1);
        assert_eq!(completed + rejected, 4);
    }
}
