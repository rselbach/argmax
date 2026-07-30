//! Executable command dispatch and non-interactive application services.

use std::env::VarError;
use std::error::Error;
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use argmax::cli::Invocation;
use argmax::config::{
    Ai, CliOverrides, ConfigFileError, ConfigStore, ConfigStoreError, ENV_AI_ENABLED,
    ENV_AI_PROVIDER, ENV_CORE_DEBUG, ENV_CORE_MODE, ENV_CORE_SHELL, ENV_UI_GHOST_TEXT,
    ENV_UI_MAX_HEIGHT, ENV_UI_MAX_SUGGESTIONS, ENV_UPDATER_CHANNEL, ENV_UPDATER_CHECK_ON_STARTUP,
    ENV_UPDATER_INTERVAL, EnvironmentOverrides, InitOutcome, OverrideErrors, Shell,
    ValidationErrors, render_resolved_config, resolve_settings,
};
use argmax::crash_boundary::{
    CrashBoundaryOutcome, CrashRecovery, NotificationOutcome, RecoveryDecision, ReportOutcome,
    RescueCommand, RescueShellSpec, RestorationOutcome, TerminalRestoration, WrapperFailure,
    run_with_crash_boundary,
};
use argmax::diagnostics::{
    CrashReportStore, DEBUG_LOGGING_WARNING, DebugLog, DiagnosticError, DiagnosticLevel,
    DiagnosticSession,
};
use argmax::integration::init_script;
use argmax::learning_store::LearningStore;
use argmax::pty::{ChildExit, ENV_ACTIVE_SHELL, ShellKind, ShellSelectionRequest, select_shell};
use argmax::release::{
    ManualUpdateCheck, ManualUpdateError, ManualUpdateOutcome, ReleaseSource, check_manual_update,
};
use argmax::reload::{
    ConfigReloader, ReloadFailure, ReloadRequestError, request_active_session_reload,
};
use argmax::runtime::{
    RuntimeDiagnosticSink, RuntimeError, run_interactive_with_diagnostics,
    run_interactive_with_reloader,
};
use argmax::setup::{SetupError, SetupTarget, detect_setup_shell};
use argmax::state::{RuntimeStateStore, StateLoadStatus};
use argmax::uninstall::{RemovalKind, UninstallError, UninstallPlan, UninstallReport};
use argmax::version::{VersionError, running_version};
use directories::BaseDirs;
use nix::unistd::isatty;

const EXIT_SUCCESS: u8 = 0;
const EXIT_FAILURE: u8 = 1;
const ENV_LOG_LEVEL: &str = "argmax_LOG_LEVEL";

const CONFIG_OVERRIDE_NAMES: &[&str] = &[
    ENV_CORE_DEBUG,
    ENV_CORE_SHELL,
    ENV_CORE_MODE,
    ENV_UI_GHOST_TEXT,
    ENV_UI_MAX_SUGGESTIONS,
    ENV_UI_MAX_HEIGHT,
    ENV_UPDATER_CHANNEL,
    ENV_UPDATER_INTERVAL,
    ENV_UPDATER_CHECK_ON_STARTUP,
    ENV_AI_ENABLED,
    ENV_AI_PROVIDER,
];

/// Complete command output with an explicit process status.
pub struct CommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: u8,
}

impl CommandOutput {
    /// Creates a successful result with standard-output bytes.
    #[must_use]
    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: Vec::new(),
            status: EXIT_SUCCESS,
        }
    }

    /// Creates an explicit result with separate output streams.
    #[must_use]
    pub fn new(status: u8, stdout: impl Into<Vec<u8>>, stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            status,
        }
    }

    fn success_with_stderr(stdout: impl Into<Vec<u8>>, stderr: impl Into<Vec<u8>>) -> Self {
        Self::new(EXIT_SUCCESS, stdout, stderr)
    }

    fn failure(stderr: impl Into<Vec<u8>>) -> Self {
        Self::new(EXIT_FAILURE, Vec::new(), stderr)
    }
}

/// Parses and executes one process invocation.
#[must_use]
pub fn run(arguments: impl IntoIterator<Item = OsString>) -> ExitCode {
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    run_with_io(arguments, &mut stdout, &mut stderr)
}

fn run_with_io(
    arguments: impl IntoIterator<Item = OsString>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> ExitCode {
    let output = match Invocation::try_parse_from(arguments) {
        Ok(invocation) => {
            execute(invocation, stdout, stderr).unwrap_or_else(|error| application_failure(&error))
        }
        Err(error) => clap_output(&error),
    };
    emit(&output, stdout, stderr)
}

fn execute(
    invocation: Invocation,
    progress_stdout: &mut impl Write,
    progress_stderr: &mut impl Write,
) -> Result<CommandOutput, ApplicationError> {
    match invocation {
        Invocation::Run { shell, debug } => execute_run(shell, debug, progress_stderr),
        Invocation::Init { shell } => Ok(CommandOutput::success(init_script(shell).as_bytes())),
        Invocation::Setup { shell } => execute_setup(shell),
        Invocation::ConfigInit => execute_config_init(&ConfigStore::discover()?),
        Invocation::ConfigShow => execute_config_show(&ConfigStore::discover()?),
        Invocation::Reload => execute_reload(),
        Invocation::Version => execute_version(),
        Invocation::Update => execute_update(progress_stdout),
        Invocation::CrashLog { clear } => execute_crash_log(clear),
        Invocation::Uninstall => execute_uninstall(),
    }
}

fn execute_run(
    shell: Option<Shell>,
    debug: bool,
    progress_stderr: &mut impl Write,
) -> Result<CommandOutput, ApplicationError> {
    let migration_time = current_unix_seconds()?;
    let store = ConfigStore::discover()?;
    let _migration = store.migrate_if_needed(migration_time)?;
    migrate_runtime_data(migration_time, progress_stderr);
    let environment = process_environment_overrides()?;
    let reloader = ConfigReloader::start(
        store,
        environment,
        CliOverrides {
            shell,
            debug: debug.then_some(true),
        },
    )?;
    let initial = reloader.shared_settings().snapshot();
    let settings = initial.settings();
    let version = running_version()?;
    let base = BaseDirs::new().ok_or(ApplicationError::NoPlatformDirectory)?;
    let fallback_rescue = RescueShellSpec::posix_fallback();
    let selected = select_shell(&ShellSelectionRequest::from_process(
        shell.map(shell_kind),
        None,
        settings.core.shell.map(shell_kind),
    ));
    let selected = match selected {
        Ok(selected) => selected,
        Err(error) => {
            if standard_io_is_terminal() {
                let _ignored = write_progress_warning(
                    progress_stderr,
                    "shell selection failed; starting the /bin/sh rescue shell",
                );
                start_rescue_spec(&fallback_rescue)?;
            }
            return Err(ApplicationError::Runtime(RuntimeError::Setup(
                error.to_string(),
            )));
        }
    };
    let rescue = RescueShellSpec::from_selected(&selected);
    let log_level = requested_log_level(settings.core.debug)?;
    let mut logger = match log_level {
        Some(threshold) => match DebugSessionLog::open(
            threshold,
            selected.kind(),
            version.as_str(),
            base.cache_dir(),
        ) {
            Ok(logger) => Some(logger),
            Err(error) => {
                let _ignored = write_progress_warning(
                    progress_stderr,
                    &format!("debug logging is unavailable: {error}"),
                );
                None
            }
        },
        None => None,
    };

    if logger.is_some() {
        let _ignored = write_progress_warning(progress_stderr, DEBUG_LOGGING_WARNING);
    }
    record_ai_readiness(logger.as_ref(), &settings.ai);
    let initial_log_error = logger.as_ref().and_then(|logger| {
        logger
            .write(
                DiagnosticLevel::Info,
                "application",
                "interactive session starting",
            )
            .err()
    });
    if let Some(error) = initial_log_error {
        let _ignored = write_progress_warning(
            progress_stderr,
            &format!("debug logging is unavailable: {error}"),
        );
        logger = None;
    }
    let exit = run_contained_interactive(
        reloader,
        shell,
        logger.as_ref(),
        version.as_str(),
        base.cache_dir(),
        rescue,
        progress_stderr,
    )?;
    let status = exit
        .wrapper_code()
        .and_then(|code| u8::try_from(code).ok())
        .ok_or(ApplicationError::UnrepresentableChildExit)?;
    if let Some(logger) = &logger {
        let _ignored = logger.write(
            DiagnosticLevel::Info,
            "application",
            &format!("interactive session exited with wrapper status {status}"),
        );
    }
    Ok(CommandOutput::new(status, Vec::new(), Vec::new()))
}

fn migrate_runtime_data(timestamp: u64, stderr: &mut impl Write) {
    match RuntimeStateStore::discover() {
        Ok(store) => match store.load() {
            Ok(loaded) if matches!(loaded.status, StateLoadStatus::Corrupt(_)) => {
                let _ignored = write_progress_warning(
                    stderr,
                    "runtime state is corrupt; using defaults without overwriting it",
                );
            }
            Ok(_) => {
                if let Err(error) = store.migrate_if_needed(timestamp) {
                    let _ignored = write_progress_warning(
                        stderr,
                        &format!("runtime-state migration is unavailable: {error}"),
                    );
                }
            }
            Err(error) => {
                let _ignored = write_progress_warning(
                    stderr,
                    &format!("runtime state is unavailable: {error}"),
                );
            }
        },
        Err(error) => {
            let _ignored =
                write_progress_warning(stderr, &format!("runtime state is unavailable: {error}"));
        }
    }

    match LearningStore::discover() {
        Ok(store) => {
            if let Err(error) = store.migrate_if_needed(timestamp) {
                let _ignored = write_progress_warning(
                    stderr,
                    &format!("learning migration is unavailable: {error}"),
                );
            }
        }
        Err(error) => {
            let _ignored =
                write_progress_warning(stderr, &format!("learning store is unavailable: {error}"));
        }
    }
}

fn run_contained_interactive(
    reloader: ConfigReloader,
    shell: Option<Shell>,
    logger: Option<&DebugSessionLog>,
    version: &str,
    cache_directory: &Path,
    rescue: RescueShellSpec,
    progress_stderr: &mut impl Write,
) -> Result<ChildExit, ApplicationError> {
    let crash_store = match CrashReportStore::open(cache_directory) {
        Ok(store) => store,
        Err(error) => {
            let _ignored = write_progress_warning(
                progress_stderr,
                &format!("crash reporting is unavailable: {error}"),
            );
            if standard_io_is_terminal() {
                start_rescue_spec(&rescue)?;
            }
            return Err(ApplicationError::Diagnostics(error));
        }
    };
    let boundary = run_with_crash_boundary(
        &crash_store,
        version,
        rescue,
        progress_stderr,
        || {
            // `run_interactive` owns the terminal guard. Its unwind-time drop
            // completes before control reaches this outer recovery callback,
            // but the application cannot observe a drop-time restore failure.
            TerminalRestoration::Incomplete
        },
        || match match logger {
            Some(logger) => run_interactive_with_diagnostics(reloader, shell, logger),
            None => run_interactive_with_reloader(reloader, shell),
        } {
            Err(error) if runtime_failure_requires_recovery(&error) => {
                if let Some(logger) = logger {
                    let _ignored =
                        logger.write(DiagnosticLevel::Error, "runtime", &error.to_string());
                }
                Err(WrapperFailure::new(&error.to_string()))
            }
            result => Ok(result),
        },
    );
    let exit = match boundary {
        CrashBoundaryOutcome::Completed(result) => match result {
            Ok(exit) => exit,
            Err(error) => {
                if let Some(logger) = logger {
                    let _ignored =
                        logger.write(DiagnosticLevel::Error, "runtime", &error.to_string());
                }
                return Err(ApplicationError::Runtime(error));
            }
        },
        CrashBoundaryOutcome::Recovered(recovery) => {
            if let Some(logger) = logger {
                let _ignored = logger.write(
                    DiagnosticLevel::Error,
                    "runtime",
                    "interactive runtime panic was contained",
                );
            }
            let _ignored = report_recovery_problems(&recovery, progress_stderr);
            start_rescue_shell(recovery.decision())?;
            return Err(ApplicationError::RecoveredCrash);
        }
        CrashBoundaryOutcome::Rejected(_) => {
            return Err(ApplicationError::CrashBoundaryUnavailable);
        }
    };
    Ok(exit)
}

const fn runtime_failure_requires_recovery(error: &RuntimeError) -> bool {
    !matches!(
        error,
        RuntimeError::Configuration(_)
            | RuntimeError::InvalidEnvironmentShell
            | RuntimeError::TerminalRequired
    )
}

const fn shell_kind(shell: Shell) -> ShellKind {
    match shell {
        Shell::Bash => ShellKind::Bash,
        Shell::Zsh => ShellKind::Zsh,
        Shell::Fish => ShellKind::Fish,
    }
}

fn standard_io_is_terminal() -> bool {
    isatty(io::stdin().as_raw_fd()).is_ok_and(|terminal| terminal)
        && isatty(io::stdout().as_raw_fd()).is_ok_and(|terminal| terminal)
}

fn write_progress_warning(stderr: &mut impl Write, message: &str) -> Result<(), ApplicationError> {
    writeln!(stderr, "warning: {message}")
        .and_then(|()| stderr.flush())
        .map_err(|error| ApplicationError::ProgressWrite {
            stream: "standard error",
            kind: error.kind(),
        })
}

struct DebugSessionLog {
    log: DebugLog,
    threshold: DiagnosticLevel,
}

impl DebugSessionLog {
    fn open(
        threshold: DiagnosticLevel,
        shell: ShellKind,
        version: &str,
        cache_directory: &Path,
    ) -> Result<Self, ApplicationError> {
        let session = DiagnosticSession::new(
            format!("process-{}-{}", std::process::id(), current_unix_seconds()?),
            version,
            shell.as_str(),
        )?;
        Ok(Self {
            log: DebugLog::open(cache_directory, session)?,
            threshold,
        })
    }

    fn write(
        &self,
        level: DiagnosticLevel,
        component: &str,
        message: &str,
    ) -> Result<(), DiagnosticError> {
        if level < self.threshold {
            return Ok(());
        }
        self.log.write(level, component, message)
    }
}

impl RuntimeDiagnosticSink for DebugSessionLog {
    fn record(&self, component: &'static str, message: &str) {
        let _ignored = self.write(DiagnosticLevel::Debug, component, message);
    }
}

fn record_ai_readiness(logger: Option<&DebugSessionLog>, ai: &Ai) {
    if let (Some(logger), Err(error)) = (logger, ai.readiness()) {
        let _ignored = logger.write(
            DiagnosticLevel::Warn,
            "ai",
            &format!("AI completion is unavailable: {error}"),
        );
    }
}

fn requested_log_level(debug: bool) -> Result<Option<DiagnosticLevel>, ApplicationError> {
    match std::env::var(ENV_LOG_LEVEL) {
        Ok(value) => parse_log_level(&value).map(Some),
        Err(VarError::NotPresent) if debug => Ok(Some(DiagnosticLevel::Debug)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(ApplicationError::NonUtf8Environment(ENV_LOG_LEVEL)),
    }
}

fn parse_log_level(value: &str) -> Result<DiagnosticLevel, ApplicationError> {
    match value {
        "trace" => Ok(DiagnosticLevel::Trace),
        "debug" => Ok(DiagnosticLevel::Debug),
        "info" => Ok(DiagnosticLevel::Info),
        "warn" => Ok(DiagnosticLevel::Warn),
        "error" => Ok(DiagnosticLevel::Error),
        _ => Err(ApplicationError::InvalidLogLevel),
    }
}

fn report_recovery_problems(
    recovery: &CrashRecovery,
    stderr: &mut impl Write,
) -> Result<(), ApplicationError> {
    let mut warnings = Vec::new();
    match recovery.restoration() {
        RestorationOutcome::Restored => {}
        RestorationOutcome::Incomplete => {
            warnings.extend_from_slice(b"warning: terminal restoration may be incomplete\n");
        }
        RestorationOutcome::Panicked => {
            warnings.extend_from_slice(b"warning: terminal restoration panicked\n");
        }
    }
    match recovery.report() {
        ReportOutcome::Written(_) => {}
        ReportOutcome::Failed(error) => {
            writeln!(
                warnings,
                "warning: writing the crash report failed: {error}"
            )
            .expect("writing to a byte buffer cannot fail");
        }
        ReportOutcome::Panicked => {
            warnings.extend_from_slice(b"warning: crash report creation panicked\n");
        }
        ReportOutcome::RelativePathRejected => {
            warnings.extend_from_slice(b"warning: crash report path was invalid\n");
        }
    }
    match recovery.notification() {
        NotificationOutcome::Emitted | NotificationOutcome::NotAttempted => {}
        NotificationOutcome::Failed(kind) => {
            writeln!(
                warnings,
                "warning: writing the crash report path failed: {kind:?}"
            )
            .expect("writing to a byte buffer cannot fail");
        }
        NotificationOutcome::Panicked => {
            warnings.extend_from_slice(b"warning: writing the crash report path panicked\n");
        }
    }
    warnings.extend_from_slice(b"starting a rescue shell after the contained wrapper failure\n");
    stderr
        .write_all(&warnings)
        .and_then(|()| stderr.flush())
        .map_err(|error| ApplicationError::ProgressWrite {
            stream: "standard error",
            kind: error.kind(),
        })
}

fn start_rescue_shell(decision: &RecoveryDecision) -> Result<(), ApplicationError> {
    let RecoveryDecision::StartRescueShell(spec) = decision;
    start_rescue_spec(spec)
}

fn start_rescue_spec(spec: &RescueShellSpec) -> Result<(), ApplicationError> {
    match launch_rescue_command(spec.primary()) {
        Ok(()) => Ok(()),
        Err(RescueLaunchFailure::Spawn(_)) if spec.fallback().is_some() => {
            launch_rescue_command(spec.fallback().expect("fallback was checked"))
                .map_err(ApplicationError::RescueLaunch)
        }
        Err(error) => Err(ApplicationError::RescueLaunch(error)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RescueLaunchFailure {
    Spawn(io::ErrorKind),
    Wait(io::ErrorKind),
}

fn launch_rescue_command(command: &RescueCommand) -> Result<(), RescueLaunchFailure> {
    let mut process = Command::new(command.executable());
    process
        .args(command.arguments())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for name in command.environment_to_remove() {
        process.env_remove(name);
    }
    let mut child = process
        .spawn()
        .map_err(|error| RescueLaunchFailure::Spawn(error.kind()))?;
    child
        .wait()
        .map_err(|error| RescueLaunchFailure::Wait(error.kind()))?;
    Ok(())
}

fn execute_reload() -> Result<CommandOutput, ApplicationError> {
    request_active_session_reload()?;
    Ok(CommandOutput::success(
        b"active session reloaded\n".to_vec(),
    ))
}

fn execute_version() -> Result<CommandOutput, ApplicationError> {
    let version = running_version()?;
    Ok(CommandOutput::success(
        format!("{}\n", version.as_str()).into_bytes(),
    ))
}

fn execute_update(progress_stdout: &mut impl Write) -> Result<CommandOutput, ApplicationError> {
    let store = ConfigStore::discover()?;
    let environment = process_environment_overrides()?;
    let settings = load_resolved_settings(&store, &environment, CliOverrides::default())?;
    let version = running_version()?;
    let executable = std::env::current_exe()
        .map_err(|error| ApplicationError::CurrentExecutable { kind: error.kind() })?;

    progress_stdout
        .write_all(b"checking for updates...\n")
        .map_err(|error| ApplicationError::ProgressWrite {
            stream: "standard output",
            kind: error.kind(),
        })?;
    progress_stdout
        .flush()
        .map_err(|error| ApplicationError::ProgressWrite {
            stream: "standard output",
            kind: error.kind(),
        })?;

    let check = check_manual_update(
        &ReleaseSource::official(),
        settings.updater.channel,
        version.as_str(),
    )
    .map_err(ManualUpdateError::from)?;
    let outcome = match check {
        ManualUpdateCheck::AlreadyCurrent { version } => {
            ManualUpdateOutcome::AlreadyCurrent { version }
        }
        ManualUpdateCheck::Available(plan) => {
            writeln!(progress_stdout, "update available: {}", plan.version()).map_err(|error| {
                ApplicationError::ProgressWrite {
                    stream: "standard output",
                    kind: error.kind(),
                }
            })?;
            progress_stdout
                .flush()
                .map_err(|error| ApplicationError::ProgressWrite {
                    stream: "standard output",
                    kind: error.kind(),
                })?;
            plan.apply(&executable)?
        }
    };
    Ok(manual_update_success(outcome))
}

fn manual_update_success(outcome: ManualUpdateOutcome) -> CommandOutput {
    match outcome {
        ManualUpdateOutcome::AlreadyCurrent { version } => {
            CommandOutput::success(format!("already current: {version}\n").into_bytes())
        }
        ManualUpdateOutcome::Updated { version, .. } => CommandOutput::success(
            format!(
                "updated argmax to {version}\n\
                 exit the active argmax session or reopen the terminal to use it\n"
            )
            .into_bytes(),
        ),
    }
}

fn execute_setup(shell: Option<Shell>) -> Result<CommandOutput, ApplicationError> {
    let detected = shell.map_or_else(
        || {
            detect_setup_shell(
                std::env::var_os(ENV_ACTIVE_SHELL).as_deref(),
                std::env::var_os("SHELL").as_deref(),
            )
        },
        Ok,
    );
    let shell = match detected {
        Ok(shell) => shell,
        Err(SetupError::UnsupportedShell) => return Err(ApplicationError::ManualSetupRequired),
        Err(error) => return Err(ApplicationError::Setup(error)),
    };
    let base = BaseDirs::new().ok_or(ApplicationError::NoPlatformDirectory)?;
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let target = SetupTarget::from_environment(
        shell,
        base.home_dir(),
        zdotdir.as_deref(),
        xdg_config_home.as_deref(),
    )?;
    let outcome = target.install(current_unix_seconds()?)?;
    let config_store = ConfigStore::discover()?;
    let config_outcome = config_store.init()?;
    let action = if outcome.changed() {
        "installed"
    } else {
        "already present"
    };
    let mut stdout = format!(
        "argmax integration {action}: {}\n",
        printable_path(outcome.path())
    );
    if let Some(backup) = outcome.backup() {
        writeln!(stdout, "backup: {}", printable_path(backup))
            .expect("writing to a string cannot fail");
    }
    let config_action = match config_outcome {
        InitOutcome::Created => "created",
        InitOutcome::AlreadyExists => "already exists",
    };
    writeln!(
        stdout,
        "configuration {config_action}: {}",
        printable_path(config_store.path())
    )
    .expect("writing to a string cannot fail");
    Ok(CommandOutput::success(stdout.into_bytes()))
}

fn execute_config_init(store: &ConfigStore) -> Result<CommandOutput, ApplicationError> {
    let action = match store.init()? {
        InitOutcome::Created => "created",
        InitOutcome::AlreadyExists => "already exists",
    };
    Ok(CommandOutput::success(
        format!("configuration {action}: {}\n", printable_path(store.path())).into_bytes(),
    ))
}

fn execute_config_show(store: &ConfigStore) -> Result<CommandOutput, ApplicationError> {
    let overrides = process_environment_overrides()?;
    execute_config_show_with_overrides(store, &overrides)
}

fn execute_config_show_with_overrides(
    store: &ConfigStore,
    overrides: &EnvironmentOverrides,
) -> Result<CommandOutput, ApplicationError> {
    let document = store.load()?;
    let settings = resolve_settings(
        document.as_ref().map(|document| &document.settings),
        overrides,
        CliOverrides::default(),
    )?;
    let stdout = render_resolved_config(&settings)?.into_bytes();
    let mut stderr = Vec::new();
    if let Some(document) = document {
        for warning in document.warnings {
            writeln!(
                stderr,
                "warning: configuration {}: {}",
                warning.path, warning.reason
            )
            .expect("writing to a byte buffer cannot fail");
        }
    }
    if let Err(error) = settings.ai.readiness() {
        writeln!(stderr, "warning: AI completion is unavailable: {error}")
            .expect("writing to a byte buffer cannot fail");
    }
    Ok(CommandOutput::success_with_stderr(stdout, stderr))
}

fn load_resolved_settings(
    store: &ConfigStore,
    environment: &EnvironmentOverrides,
    cli: CliOverrides,
) -> Result<argmax::config::Settings, ApplicationError> {
    let document = store.load()?;
    resolve_settings(
        document.as_ref().map(|document| &document.settings),
        environment,
        cli,
    )
    .map_err(ApplicationError::Validation)
}

fn process_environment_overrides() -> Result<EnvironmentOverrides, ApplicationError> {
    for name in CONFIG_OVERRIDE_NAMES {
        if matches!(std::env::var(name), Err(VarError::NotUnicode(_))) {
            return Err(ApplicationError::NonUtf8Environment(name));
        }
    }
    EnvironmentOverrides::from_lookup(|name| std::env::var(name).ok())
        .map_err(ApplicationError::Overrides)
}

fn execute_crash_log(clear: bool) -> Result<CommandOutput, ApplicationError> {
    let base = BaseDirs::new().ok_or(ApplicationError::NoPlatformDirectory)?;
    let store = CrashReportStore::open(base.cache_dir())?;
    execute_crash_store(&store, clear)
}

fn execute_crash_store(
    store: &CrashReportStore,
    clear: bool,
) -> Result<CommandOutput, ApplicationError> {
    if !clear {
        let stdout = store.newest()?.map_or_else(
            || b"no crash reports\n".to_vec(),
            |path| format!("{}\n", printable_path(&path)).into_bytes(),
        );
        return Ok(CommandOutput::success(stdout));
    }

    let outcome = store.clear()?;
    let mut stdout = Vec::new();
    for path in &outcome.removed {
        writeln!(stdout, "removed crash report: {}", printable_path(path))
            .expect("writing to a byte buffer cannot fail");
    }
    if outcome.removed.is_empty() && outcome.failures.is_empty() {
        stdout.extend_from_slice(b"no crash reports to clear\n");
    }
    let mut stderr = Vec::new();
    for failure in &outcome.failures {
        writeln!(
            stderr,
            "failed to remove crash report {}: {:?}",
            printable_path(&failure.path),
            failure.kind
        )
        .expect("writing to a byte buffer cannot fail");
    }
    let status = if outcome.failures.is_empty() {
        EXIT_SUCCESS
    } else {
        EXIT_FAILURE
    };
    Ok(CommandOutput::new(status, stdout, stderr))
}

fn execute_uninstall() -> Result<CommandOutput, ApplicationError> {
    let plan = UninstallPlan::discover()?;
    let report = plan.execute(current_unix_seconds()?);
    Ok(uninstall_report_output(&report))
}

fn uninstall_report_output(report: &UninstallReport) -> CommandOutput {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    if report.active_session {
        stderr.extend_from_slice(
            b"warning: an argmax parent session is active; close or exit it naturally after uninstalling rather than killing the wrapper\n",
        );
    }
    for removed in &report.removed {
        writeln!(
            stdout,
            "removed {}: {}",
            removal_kind(removed.kind),
            printable_path(&removed.path)
        )
        .expect("writing to a byte buffer cannot fail");
        if let Some(backup) = &removed.backup {
            writeln!(
                stdout,
                "shell configuration backup: {}",
                printable_path(backup)
            )
            .expect("writing to a byte buffer cannot fail");
        }
    }
    for path in &report.retained_legacy_integrations {
        writeln!(
            stderr,
            "warning: retained unmarked legacy shell integration: {}",
            printable_path(path)
        )
        .expect("writing to a byte buffer cannot fail");
    }
    for failure in &report.failures {
        writeln!(
            stderr,
            "failed to remove {} {}: {}",
            removal_kind(failure.kind),
            printable_path(&failure.path),
            failure.error
        )
        .expect("writing to a byte buffer cannot fail");
    }
    if report.removed.is_empty() && report.failures.is_empty() {
        stdout.extend_from_slice(b"no argmax-owned locations found\n");
    }
    let status = if report.succeeded() {
        EXIT_SUCCESS
    } else {
        EXIT_FAILURE
    };
    CommandOutput::new(status, stdout, stderr)
}

const fn removal_kind(kind: RemovalKind) -> &'static str {
    match kind {
        RemovalKind::ShellIntegration => "shell integration",
        RemovalKind::LocalData => "local data (including learned commands and crash logs)",
        RemovalKind::Executable => "executable",
    }
}

fn printable_path(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let bytes = path.as_os_str().as_bytes();
    let mut output = String::with_capacity(bytes.len().saturating_mul(3));
    for &byte in bytes {
        if (b' '..=b'~').contains(&byte) && byte != b'%' {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    output
}

fn current_unix_seconds() -> Result<u64, ApplicationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| ApplicationError::ClockBeforeEpoch)
        .map(|duration| duration.as_secs())
}

fn clap_output(error: &clap::Error) -> CommandOutput {
    let bytes = error.to_string().into_bytes();
    let status = u8::try_from(error.exit_code()).unwrap_or(EXIT_FAILURE);
    if error.use_stderr() {
        CommandOutput::new(status, Vec::new(), bytes)
    } else {
        CommandOutput::new(status, bytes, Vec::new())
    }
}

fn application_failure(error: &ApplicationError) -> CommandOutput {
    CommandOutput::failure(format!("argmax: {error}\n").into_bytes())
}

fn emit(output: &CommandOutput, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    if let Err(error) = stdout.write_all(&output.stdout) {
        let _ignored = writeln!(
            stderr,
            "argmax: writing standard output failed: {:?}",
            error.kind()
        );
        return ExitCode::from(EXIT_FAILURE);
    }
    if let Err(error) = stderr.write_all(&output.stderr) {
        let _ignored = writeln!(
            io::stderr().lock(),
            "argmax: writing standard error failed: {:?}",
            error.kind()
        );
        return ExitCode::from(EXIT_FAILURE);
    }
    ExitCode::from(output.status)
}

#[derive(Debug)]
enum ApplicationError {
    NoPlatformDirectory,
    ClockBeforeEpoch,
    ManualSetupRequired,
    InvalidLogLevel,
    NonUtf8Environment(&'static str),
    ProgressWrite {
        stream: &'static str,
        kind: io::ErrorKind,
    },
    CurrentExecutable {
        kind: io::ErrorKind,
    },
    UnrepresentableChildExit,
    CrashBoundaryUnavailable,
    RecoveredCrash,
    RescueLaunch(RescueLaunchFailure),
    Setup(SetupError),
    ConfigStore(ConfigStoreError),
    ConfigFile(ConfigFileError),
    Overrides(OverrideErrors),
    Validation(ValidationErrors),
    Diagnostics(DiagnosticError),
    Version(VersionError),
    ManualUpdate(ManualUpdateError),
    Runtime(RuntimeError),
    ReloadStartup(ReloadFailure),
    ReloadRequest(ReloadRequestError),
    Uninstall(UninstallError),
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlatformDirectory => {
                formatter.write_str("user configuration directory is unavailable")
            }
            Self::ClockBeforeEpoch => formatter.write_str("system clock is before the Unix epoch"),
            Self::ManualSetupRequired => formatter.write_str(
                "could not detect Bash, Zsh, or Fish; run `argmax init <bash|zsh|fish>` and source its output manually",
            ),
            Self::InvalidLogLevel => formatter.write_str(
                "argmax_LOG_LEVEL must be one of trace, debug, info, warn, or error",
            ),
            Self::NonUtf8Environment(name) => {
                write!(formatter, "environment override {name} is not valid UTF-8")
            }
            Self::ProgressWrite { stream, kind } => {
                write!(formatter, "writing {stream} failed: {kind:?}")
            }
            Self::CurrentExecutable { kind } => {
                write!(formatter, "locating the current executable failed: {kind:?}")
            }
            Self::UnrepresentableChildExit => {
                formatter.write_str("wrapped shell exit status is unavailable")
            }
            Self::CrashBoundaryUnavailable => {
                formatter.write_str("interactive crash containment is already active")
            }
            Self::RecoveredCrash => {
                formatter.write_str("interactive wrapper failure was contained")
            }
            Self::RescueLaunch(RescueLaunchFailure::Spawn(kind)) => {
                write!(formatter, "starting the rescue shell failed: {kind:?}")
            }
            Self::RescueLaunch(RescueLaunchFailure::Wait(kind)) => {
                write!(formatter, "waiting for the rescue shell failed: {kind:?}")
            }
            Self::Setup(error) => write!(formatter, "setup failed: {error}"),
            Self::ConfigStore(error) => write!(formatter, "configuration failed: {error}"),
            Self::ConfigFile(error) => write!(formatter, "configuration failed: {error}"),
            Self::Overrides(error) => write!(formatter, "environment override failed: {error}"),
            Self::Validation(error) => write!(formatter, "configuration failed: {error}"),
            Self::Diagnostics(error) => write!(formatter, "diagnostic operation failed: {error}"),
            Self::Version(error) => write!(formatter, "version metadata is invalid: {error}"),
            Self::ManualUpdate(error) => write!(formatter, "update failed: {error}"),
            Self::Runtime(error) => write!(formatter, "{error}"),
            Self::ReloadStartup(error) => {
                write!(formatter, "interactive configuration failed: {error}")
            }
            Self::ReloadRequest(error) => write!(formatter, "reload failed: {error}"),
            Self::Uninstall(error) => write!(formatter, "uninstall failed: {error}"),
        }
    }
}

impl Error for ApplicationError {}

impl From<SetupError> for ApplicationError {
    fn from(error: SetupError) -> Self {
        Self::Setup(error)
    }
}

impl From<ConfigStoreError> for ApplicationError {
    fn from(error: ConfigStoreError) -> Self {
        Self::ConfigStore(error)
    }
}

impl From<ConfigFileError> for ApplicationError {
    fn from(error: ConfigFileError) -> Self {
        Self::ConfigFile(error)
    }
}

impl From<ValidationErrors> for ApplicationError {
    fn from(error: ValidationErrors) -> Self {
        Self::Validation(error)
    }
}

impl From<DiagnosticError> for ApplicationError {
    fn from(error: DiagnosticError) -> Self {
        Self::Diagnostics(error)
    }
}

impl From<VersionError> for ApplicationError {
    fn from(error: VersionError) -> Self {
        Self::Version(error)
    }
}

impl From<ManualUpdateError> for ApplicationError {
    fn from(error: ManualUpdateError) -> Self {
        Self::ManualUpdate(error)
    }
}

impl From<RuntimeError> for ApplicationError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<ReloadFailure> for ApplicationError {
    fn from(error: ReloadFailure) -> Self {
        Self::ReloadStartup(error)
    }
}

impl From<ReloadRequestError> for ApplicationError {
    fn from(error: ReloadRequestError) -> Self {
        Self::ReloadRequest(error)
    }
}

impl From<UninstallError> for ApplicationError {
    fn from(error: UninstallError) -> Self {
        Self::Uninstall(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_arguments(arguments: &[&str]) -> (ExitCode, Vec<u8>, Vec<u8>) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run_with_io(
            arguments.iter().map(OsString::from),
            &mut stdout,
            &mut stderr,
        );
        (status, stdout, stderr)
    }

    #[test]
    fn version_and_init_are_stdout_only() {
        let (status, stdout, stderr) = run_arguments(&["argmax", "version"]);
        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(
            stdout,
            format!("{}\n", running_version().unwrap().as_str()).as_bytes()
        );
        assert!(stderr.is_empty());

        let (status, stdout, stderr) = run_arguments(&["argmax", "init", "bash"]);
        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(stdout, init_script(Shell::Bash).as_bytes());
        assert!(stderr.is_empty());
    }

    #[test]
    fn clap_help_and_errors_use_the_correct_stream_and_status() {
        let (status, stdout, stderr) = run_arguments(&["argmax", "--help"]);
        assert_eq!(status, ExitCode::SUCCESS);
        assert!(!stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(!stdout.contains(&0x1b));

        let (status, stdout, stderr) = run_arguments(&["argmax", "unknown"]);
        assert_ne!(status, ExitCode::SUCCESS);
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
        assert!(!stderr.contains(&0x1b));
    }

    #[test]
    fn reload_failures_are_safe_and_stderr_only() {
        let output = application_failure(&ApplicationError::ReloadRequest(
            ReloadRequestError::NoActiveSession,
        ));

        assert_eq!(output.status, EXIT_FAILURE);
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"argmax: reload failed: reload is available only inside an active argmax session\n"
        );
    }

    #[test]
    fn config_helpers_create_resolve_redact_and_preserve_existing_files() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        let store = ConfigStore::new(&path);

        let created = execute_config_init(&store).unwrap();
        assert_eq!(created.status, EXIT_SUCCESS);
        let original = std::fs::read(&path).unwrap();
        let existing = execute_config_init(&store).unwrap();
        assert_eq!(existing.status, EXIT_SUCCESS);
        assert_eq!(std::fs::read(&path).unwrap(), original);

        std::fs::write(
            &path,
            "[core]\nversion = 2\n\n[ai]\nenabled = false\n\n\
             [ai.providers.greendale]\ninherited_from = \"openai\"\n\
             api_key = \"secret-study-group-key\"\n",
        )
        .unwrap();
        let shown =
            execute_config_show_with_overrides(&store, &EnvironmentOverrides::default()).unwrap();
        assert_eq!(shown.status, EXIT_SUCCESS);
        let rendered = String::from_utf8(shown.stdout).unwrap();
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("secret-study-group-key"));

        std::fs::write(
            &path,
            "[core]\nversion = 2\n\n[ai]\nenabled = true\nprovider = \"greendale\"\n\n\
             [ai.providers.greendale]\n",
        )
        .unwrap();
        let incomplete =
            execute_config_show_with_overrides(&store, &EnvironmentOverrides::default()).unwrap();
        assert_eq!(incomplete.status, EXIT_SUCCESS);
        assert!(
            String::from_utf8(incomplete.stdout)
                .unwrap()
                .contains("enabled = true")
        );
        let warning = String::from_utf8(incomplete.stderr).unwrap();
        assert!(warning.contains("AI completion is unavailable"));
        assert!(warning.contains("endpoint is missing or blank"));
    }

    #[test]
    fn run_settings_apply_cli_precedence_without_disabling_file_debug() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "[core]\nversion = 2\nshell = \"bash\"\ndebug = true\n",
        )
        .unwrap();
        let reloader = ConfigReloader::start(
            ConfigStore::new(path),
            EnvironmentOverrides::default(),
            CliOverrides {
                shell: Some(Shell::Zsh),
                debug: None,
            },
        )
        .unwrap();
        let shared = reloader.shared_settings();
        let snapshot = shared.snapshot();
        let settings = snapshot.settings();

        assert_eq!(settings.core.shell, Some(Shell::Zsh));
        assert!(settings.core.debug);
    }

    #[test]
    fn documented_log_levels_are_closed_and_ordered() {
        for (value, want) in [
            ("trace", DiagnosticLevel::Trace),
            ("debug", DiagnosticLevel::Debug),
            ("info", DiagnosticLevel::Info),
            ("warn", DiagnosticLevel::Warn),
            ("error", DiagnosticLevel::Error),
        ] {
            assert_eq!(parse_log_level(value).unwrap(), want);
        }
        assert!(matches!(
            parse_log_level("verbose"),
            Err(ApplicationError::InvalidLogLevel)
        ));
    }

    #[test]
    fn abnormal_runtime_failures_request_crash_recovery() {
        assert!(!runtime_failure_requires_recovery(
            &RuntimeError::TerminalRequired
        ));
        assert!(!runtime_failure_requires_recovery(
            &RuntimeError::InvalidEnvironmentShell
        ));
        assert!(!runtime_failure_requires_recovery(
            &RuntimeError::Configuration("invalid keybinding".to_owned())
        ));
        assert!(runtime_failure_requires_recovery(&RuntimeError::Input(
            io::ErrorKind::BrokenPipe
        )));
        assert!(runtime_failure_requires_recovery(
            &RuntimeError::ReaderPanicked
        ));
    }

    #[test]
    fn manual_update_success_reports_terminal_states() {
        let current = manual_update_success(ManualUpdateOutcome::AlreadyCurrent {
            version: "1.2.3".into(),
        });
        assert_eq!(current.status, EXIT_SUCCESS);
        assert_eq!(current.stdout, b"already current: 1.2.3\n");
        assert!(current.stderr.is_empty());

        let updated = manual_update_success(ManualUpdateOutcome::Updated {
            version: "1.2.4".into(),
            apply: argmax::update_apply::UpdateApplyOutcome::AlreadyCurrent,
        });
        assert_eq!(updated.status, EXIT_SUCCESS);
        assert_eq!(
            updated.stdout,
            b"updated argmax to 1.2.4\n\
              exit the active argmax session or reopen the terminal to use it\n"
        );
        assert!(updated.stderr.is_empty());
    }

    #[test]
    fn uninstall_output_reports_every_result_and_active_session_warning() {
        let report = UninstallReport {
            removed: vec![argmax::uninstall::RemovedLocation {
                kind: RemovalKind::ShellIntegration,
                path: "/tmp/Greendale/.bashrc".into(),
                backup: Some("/tmp/Greendale/.bashrc.argmax-backup".into()),
            }],
            failures: vec![argmax::uninstall::RemovalFailure {
                kind: RemovalKind::LocalData,
                path: "/tmp/Greendale/argmax".into(),
                error: UninstallError::NotOwned,
            }],
            retained_legacy_integrations: vec!["/tmp/Greendale/.zshrc".into()],
            active_session: true,
        };

        let output = uninstall_report_output(&report);

        assert_eq!(output.status, EXIT_FAILURE);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("removed shell integration: /tmp/Greendale/.bashrc\n"));
        assert!(
            stdout.contains("shell configuration backup: /tmp/Greendale/.bashrc.argmax-backup\n")
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("close or exit it naturally"));
        assert!(stderr.contains(
            "warning: retained unmarked legacy shell integration: /tmp/Greendale/.zshrc\n"
        ));
        assert!(stderr.contains(
            "failed to remove local data (including learned commands and crash logs) /tmp/Greendale/argmax: removal target is not owned by this user\n"
        ));
    }

    #[test]
    fn displayed_paths_reversibly_escape_terminal_controls_and_percent() {
        let path = Path::new(std::ffi::OsStr::from_bytes(
            b"/tmp/Greendale\nStudy%Group\x1b",
        ));

        assert_eq!(printable_path(path), "/tmp/Greendale%0AStudy%25Group%1B");
    }

    #[test]
    fn crash_log_helpers_report_newest_and_clear_only_owned_reports() {
        let temporary = tempfile::tempdir().unwrap();
        let store = CrashReportStore::open(temporary.path()).unwrap();
        let empty = execute_crash_store(&store, false).unwrap();
        assert_eq!(empty.stdout, b"no crash reports\n");

        let report = argmax::diagnostics::CrashReport::new("1.0.0", "failure", "backtrace");
        let path = store.write(&report).unwrap();
        let newest = execute_crash_store(&store, false).unwrap();
        assert_eq!(newest.stdout, format!("{}\n", path.display()).as_bytes());

        let unrelated = path.parent().unwrap().join("Community.txt");
        std::fs::write(&unrelated, b"Troy Barnes").unwrap();
        let cleared = execute_crash_store(&store, true).unwrap();
        assert_eq!(cleared.status, EXIT_SUCCESS);
        assert!(!path.exists());
        assert!(unrelated.exists());
    }
}
