//! Bounded runtime orchestration for optional AI completion.
//!
//! The dispatcher owns a small scheduler thread and never performs filesystem,
//! process, prompt, or network work on the terminal-input path. Provider text is
//! accepted only through the existing exact-prefix validator and leaves this
//! module as an inert [`ProviderBatch`].

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::ai::ai_suggestion;
use crate::ai_lifecycle::{
    CancellationReason, Lifecycle, LifecyclePoll, LifecycleSettings, ProviderOutcome,
    RequestPermit, RequestToken, ResponseDisposition, SessionId,
};
use crate::ai_prompt::{
    GatheredPromptContext, GitPromptData, LocalResourceGroup, MAX_BRANCH_NAMES,
    MAX_COMMIT_SUBJECTS, MAX_DIRECTORY_NAMES, MAX_GIT_STATUS_BYTES, MAX_HELP_BYTES,
    MAX_HELP_ENTRIES, MAX_LOCAL_RESOURCE_GROUPS, MAX_LOCAL_RESOURCES_PER_GROUP,
    MAX_PACKAGE_SCRIPTS, MAX_RECENT_COMMAND_BYTES, MAX_RECENT_COMMANDS, MAX_RESOURCE_NAME_BYTES,
    MAX_SIGNATURE_FILENAMES, MAX_STAGED_DIFF_BYTES, MAX_TARGETS, NamedPromptValue,
    WorkspacePromptData, build_prompt,
};
use crate::ai_provider::{
    PlainHttpPolicy, PreparedAiRequest, SanitizedProviderError, prepare_openai_request,
};
use crate::ai_transport::execute_openai_request;
use crate::completion::{CancellationToken, CommandSpec, CompletionQuery, ProviderBatch};
use crate::config::{Ai, AiContextLevel, Settings};
use crate::coordinator::QueryWork;
use crate::process_runner::{LocalProcessRequest, run_local_process};
use crate::providers::{WorkspaceKind, detect_workspace};
use crate::reload::SharedSettings;

/// Exact provider identity registered with the session coordinator.
pub const AI_COMPLETION_PROVIDER: &str = "ai";

/// Provider identities emitted by [`AiCompletionDispatcher`].
pub const AI_COMPLETION_PROVIDERS: [&str; 1] = [AI_COMPLETION_PROVIDER];

/// Maximum AI provider batches returned by one nonblocking drain.
pub const MAX_AI_DRAIN_BATCHES: usize = 8;

/// Maximum recent completed commands retained for workspace prompts.
pub const MAX_AI_RECENT_COMMANDS: usize = MAX_RECENT_COMMANDS;

const EVENT_QUEUE_CAPACITY: usize = 128;
const OUTPUT_QUEUE_CAPACITY: usize = 16;
const MAX_REQUEST_WORKERS: usize = 4;
const SCHEDULER_TICK: Duration = Duration::from_millis(10);
const CONTEXT_CACHE_TTL: Duration = Duration::from_secs(4);
const MAX_STRUCTURED_METADATA_BYTES: usize = 256 * 1024;
const LOCAL_CONTEXT_PROCESS_TIMEOUT: Duration = Duration::from_millis(350);
const MAX_DIRECTORY_ENTRIES_INSPECTED: usize = 4_096;
const MAX_HELP_SUBCOMMANDS: usize = 8;
const MAX_HELP_OPTIONS: usize = 16;

type Transport = dyn Fn(PreparedAiRequest) -> Result<String, SanitizedProviderError> + Send + Sync;

/// Immutable startup inputs for one session-local AI scheduler.
pub struct AiCompletionOptions {
    shell: String,
    operating_system: String,
    settings: Settings,
    shared_settings: Option<SharedSettings>,
    session: SessionId,
}

impl AiCompletionOptions {
    /// Creates options with fixed settings and privacy-safe platform metadata.
    #[must_use]
    pub fn new(shell: impl Into<String>, settings: Settings) -> Self {
        Self {
            shell: shell.into(),
            operating_system: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            settings,
            shared_settings: None,
            session: SessionId::new(1),
        }
    }

    /// Reads atomically published configuration generations for live AI changes.
    #[must_use]
    pub fn with_shared_settings(mut self, settings: SharedSettings) -> Self {
        self.shared_settings = Some(settings);
        self
    }

    /// Supplies the caller-owned shell session identity.
    #[must_use]
    pub fn with_session_id(mut self, session: SessionId) -> Self {
        self.session = session;
        self
    }

    fn settings_snapshot(&self) -> (u64, Settings) {
        self.shared_settings.as_ref().map_or_else(
            || (0, self.settings.clone()),
            |shared| {
                let snapshot = shared.snapshot();
                (snapshot.generation(), snapshot.settings().clone())
            },
        )
    }
}

impl fmt::Debug for AiCompletionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiCompletionOptions")
            .field("shell_bytes", &self.shell.len())
            .field("operating_system_bytes", &self.operating_system.len())
            .field("has_shared_settings", &self.shared_settings.is_some())
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// Disposition of one nonblocking AI query submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum AiQueryAdmission {
    /// The immutable query was queued for asynchronous scheduling.
    Queued,
    /// The query had already lost coordinator authority.
    Cancelled,
    /// The bounded scheduler queue was full; local completion remains usable.
    Full,
    /// The scheduler is shutting down or has exited.
    Closed,
}

/// Disposition of one completed-command notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum AiCommandAdmission {
    /// The command was queued for bounded workspace context.
    Queued,
    /// Blank or oversized text was intentionally not retained.
    Ignored,
    /// The bounded scheduler queue was full.
    Full,
    /// The scheduler is shutting down or has exited.
    Closed,
}

/// Non-sensitive AI scheduler health and activity counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiDispatcherStatus {
    /// Whether the scheduler thread has not exited.
    pub alive: bool,
    /// Number of events currently retained by the bounded input queue.
    pub queued_events: usize,
    /// Most recently submitted coordinator generation.
    pub latest_generation: u64,
    /// Number of provider calls started since this dispatcher was created.
    pub requests_started: u64,
    /// Number of results discarded after losing authority.
    pub stale_results: u64,
    /// Number of request workers still inside their bounded transport call.
    pub active_requests: usize,
}

/// Session-local asynchronous AI completion scheduler.
pub struct AiCompletionDispatcher {
    sender: SyncSender<RuntimeEvent>,
    pending_reconfiguration: Arc<Mutex<Option<PendingReconfiguration>>>,
    output: Arc<Mutex<VecDeque<AuthorizedBatch>>>,
    authority: Arc<AtomicU64>,
    latest_generation: Arc<AtomicU64>,
    queued_events: Arc<AtomicUsize>,
    requests_started: Arc<AtomicU64>,
    stale_results: Arc<AtomicU64>,
    active_requests: Arc<AtomicUsize>,
    alive: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AiCompletionDispatcher {
    /// Starts one bounded scheduler using the production `OpenAI` transport.
    ///
    /// No request is started by this operation, including when AI is enabled.
    ///
    /// # Errors
    ///
    /// Returns the operating-system thread creation error.
    pub fn spawn(options: AiCompletionOptions) -> io::Result<Self> {
        Self::spawn_with_transport(
            options,
            Arc::new(|request| {
                execute_openai_request(&request)
                    .map(crate::ai_transport::ProviderCompletion::into_content)
            }),
        )
    }

    fn spawn_with_transport(
        options: AiCompletionOptions,
        transport: Arc<Transport>,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let (result_sender, result_receiver) = mpsc::sync_channel(MAX_REQUEST_WORKERS * 2);
        let pending_reconfiguration = Arc::new(Mutex::new(None));
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let authority = Arc::new(AtomicU64::new(0));
        let latest_generation = Arc::new(AtomicU64::new(0));
        let queued_events = Arc::new(AtomicUsize::new(0));
        let requests_started = Arc::new(AtomicU64::new(0));
        let stale_results = Arc::new(AtomicU64::new(0));
        let active_requests = Arc::new(AtomicUsize::new(0));
        let alive = Arc::new(AtomicBool::new(true));

        let worker_output = Arc::clone(&output);
        let worker_reconfiguration = Arc::clone(&pending_reconfiguration);
        let worker_authority = Arc::clone(&authority);
        let worker_latest_generation = Arc::clone(&latest_generation);
        let worker_queued_events = Arc::clone(&queued_events);
        let worker_requests_started = Arc::clone(&requests_started);
        let worker_stale_results = Arc::clone(&stale_results);
        let worker_active_requests = Arc::clone(&active_requests);
        let worker_alive = Arc::clone(&alive);
        let worker = thread::Builder::new()
            .name("argmax-ai-scheduler".to_owned())
            .spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Scheduler::new(
                        options,
                        receiver,
                        result_sender,
                        result_receiver,
                        worker_reconfiguration,
                        worker_output,
                        worker_authority,
                        worker_latest_generation,
                        worker_queued_events,
                        worker_requests_started,
                        worker_stale_results,
                        worker_active_requests,
                        transport,
                    )
                    .run();
                }));
                worker_alive.store(false, Ordering::Release);
                drop(run);
            })?;

        Ok(Self {
            sender,
            pending_reconfiguration,
            output,
            authority,
            latest_generation,
            queued_events,
            requests_started,
            stale_results,
            active_requests,
            alive,
            worker: Some(worker),
        })
    }

    /// Queues one immutable coordinator query without waiting for AI work.
    pub fn submit_query(&self, work: QueryWork) -> AiQueryAdmission {
        if work.cancellation().is_cancelled() {
            return AiQueryAdmission::Cancelled;
        }
        if !self.alive.load(Ordering::Acquire) {
            return AiQueryAdmission::Closed;
        }
        let Some(authority) = advance_authority(&self.authority) else {
            return AiQueryAdmission::Closed;
        };
        self.latest_generation
            .store(work.query().generation, Ordering::Release);
        match self.send_event(RuntimeEvent::Query { work, authority }) {
            SendDisposition::Queued => AiQueryAdmission::Queued,
            SendDisposition::Full => AiQueryAdmission::Full,
            SendDisposition::Closed => AiQueryAdmission::Closed,
        }
    }

    /// Revokes pending and in-flight AI authority without waiting.
    pub fn cancel(&self, reason: CancellationReason) {
        let Some(authority) = advance_authority(&self.authority) else {
            return;
        };
        let _ = self.send_event(RuntimeEvent::Cancel { reason, authority });
    }

    /// Revokes current authority and wakes the shared-settings observer.
    ///
    /// Call this after a live settings publication so disabling AI or changing
    /// its provider suppresses an older result immediately.
    pub fn settings_changed(&self) {
        let Some(authority) = advance_authority(&self.authority) else {
            return;
        };
        let _ = self.send_event(RuntimeEvent::SettingsChanged { authority });
    }

    /// Applies one complete validated settings replacement before later query
    /// events, revoking every request authorized by the prior configuration.
    #[must_use]
    pub fn reconfigure(&self, settings: Settings) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        let Some(authority) = advance_authority(&self.authority) else {
            return false;
        };
        let mut pending = lock(&self.pending_reconfiguration);
        *pending = Some(PendingReconfiguration {
            settings,
            authority,
        });
        true
    }

    /// Queues a completed command for workspace-level prompts and cancels any
    /// request authorized before command execution.
    pub fn record_completed_command(&self, command: impl Into<String>) -> AiCommandAdmission {
        let command = command.into();
        if command.trim().is_empty() || command.len() > MAX_RECENT_COMMAND_BYTES {
            self.cancel(CancellationReason::CommandExecution);
            return AiCommandAdmission::Ignored;
        }
        if !self.alive.load(Ordering::Acquire) {
            return AiCommandAdmission::Closed;
        }
        let Some(authority) = advance_authority(&self.authority) else {
            return AiCommandAdmission::Closed;
        };
        match self.send_event(RuntimeEvent::CommandExecuted { command, authority }) {
            SendDisposition::Queued => AiCommandAdmission::Queued,
            SendDisposition::Full => AiCommandAdmission::Full,
            SendDisposition::Closed => AiCommandAdmission::Closed,
        }
    }

    /// Drains at most `limit` current AI provider replacements without waiting.
    #[must_use]
    pub fn drain_batches(&self, limit: usize) -> Vec<ProviderBatch> {
        if limit == 0 {
            return Vec::new();
        }
        let mut output = match self.output.try_lock() {
            Ok(output) => output,
            Err(TryLockError::WouldBlock) => return Vec::new(),
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        let authority = self.authority.load(Ordering::Acquire);
        let generation = self.latest_generation.load(Ordering::Acquire);
        let limit = limit.min(MAX_AI_DRAIN_BATCHES);
        let mut batches = Vec::with_capacity(limit);
        while batches.len() < limit {
            let Some(authorized) = output.pop_front() else {
                break;
            };
            if authorized.authority == authority && authorized.batch.generation == generation {
                batches.push(authorized.batch);
            }
        }
        batches
    }

    /// Returns content-free scheduler health and queue counters.
    #[must_use]
    pub fn status(&self) -> AiDispatcherStatus {
        AiDispatcherStatus {
            alive: self.alive.load(Ordering::Acquire),
            queued_events: self.queued_events.load(Ordering::Acquire),
            latest_generation: self.latest_generation.load(Ordering::Acquire),
            requests_started: self.requests_started.load(Ordering::Acquire),
            stale_results: self.stale_results.load(Ordering::Acquire),
            active_requests: self.active_requests.load(Ordering::Acquire),
        }
    }

    fn send_event(&self, event: RuntimeEvent) -> SendDisposition {
        match self.sender.try_send(event) {
            Ok(()) => {
                self.queued_events.fetch_add(1, Ordering::AcqRel);
                SendDisposition::Queued
            }
            Err(TrySendError::Full(_)) => SendDisposition::Full,
            Err(TrySendError::Disconnected(_)) => SendDisposition::Closed,
        }
    }
}

impl fmt::Debug for AiCompletionDispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiCompletionDispatcher")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Drop for AiCompletionDispatcher {
    fn drop(&mut self) {
        let _ = advance_authority(&self.authority);
        if self.sender.send(RuntimeEvent::Shutdown).is_ok() {
            self.queued_events.fetch_add(1, Ordering::AcqRel);
        }
        if let Some(worker) = self.worker.take() {
            let _join_result = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SendDisposition {
    Queued,
    Full,
    Closed,
}

enum RuntimeEvent {
    Query {
        work: QueryWork,
        authority: u64,
    },
    Cancel {
        reason: CancellationReason,
        authority: u64,
    },
    SettingsChanged {
        authority: u64,
    },
    CommandExecuted {
        command: String,
        authority: u64,
    },
    Shutdown,
}

struct PendingReconfiguration {
    settings: Settings,
    authority: u64,
}

struct QueryAuthority {
    work: QueryWork,
    authority: u64,
}

struct RequestResult {
    token: RequestToken,
    query: QueryWork,
    authority: u64,
    settings_generation: u64,
    result: Result<String, SanitizedProviderError>,
}

struct AuthorizedBatch {
    authority: u64,
    batch: ProviderBatch,
}

struct CachedContext {
    key: u64,
    expires_at: Instant,
    workspace: Option<WorkspacePromptData>,
    git: Option<GitPromptData>,
}

struct Scheduler {
    options: AiCompletionOptions,
    receiver: Receiver<RuntimeEvent>,
    result_sender: SyncSender<RequestResult>,
    result_receiver: Receiver<RequestResult>,
    pending_reconfiguration: Arc<Mutex<Option<PendingReconfiguration>>>,
    output: Arc<Mutex<VecDeque<AuthorizedBatch>>>,
    authority: Arc<AtomicU64>,
    latest_generation: Arc<AtomicU64>,
    queued_events: Arc<AtomicUsize>,
    requests_started: Arc<AtomicU64>,
    stale_results: Arc<AtomicU64>,
    active_requests: Arc<AtomicUsize>,
    transport: Arc<Transport>,
    lifecycle: Lifecycle,
    settings_generation: u64,
    settings: Settings,
    current_query: Option<QueryAuthority>,
    recent_commands: VecDeque<String>,
    context_cache: Option<CachedContext>,
    started: Instant,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    fn new(
        options: AiCompletionOptions,
        receiver: Receiver<RuntimeEvent>,
        result_sender: SyncSender<RequestResult>,
        result_receiver: Receiver<RequestResult>,
        pending_reconfiguration: Arc<Mutex<Option<PendingReconfiguration>>>,
        output: Arc<Mutex<VecDeque<AuthorizedBatch>>>,
        authority: Arc<AtomicU64>,
        latest_generation: Arc<AtomicU64>,
        queued_events: Arc<AtomicUsize>,
        requests_started: Arc<AtomicU64>,
        stale_results: Arc<AtomicU64>,
        active_requests: Arc<AtomicUsize>,
        transport: Arc<Transport>,
    ) -> Self {
        let (settings_generation, settings) = options.settings_snapshot();
        let lifecycle = configured_lifecycle(&settings.ai, options.session, 0);
        Self {
            options,
            receiver,
            result_sender,
            result_receiver,
            pending_reconfiguration,
            output,
            authority,
            latest_generation,
            queued_events,
            requests_started,
            stale_results,
            active_requests,
            transport,
            lifecycle,
            settings_generation,
            settings,
            current_query: None,
            recent_commands: VecDeque::new(),
            context_cache: None,
            started: Instant::now(),
        }
    }

    fn run(&mut self) {
        loop {
            self.apply_pending_reconfiguration();
            match self.receiver.recv_timeout(SCHEDULER_TICK) {
                Ok(RuntimeEvent::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(event) => {
                    decrement_saturating(&self.queued_events);
                    self.apply_pending_reconfiguration();
                    self.handle_event(event);
                    if self.drain_events() {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            self.synchronize_settings();
            self.drain_results();
            self.poll_lifecycle();
        }
        self.lifecycle.end_session(self.now_ms());
    }

    fn drain_events(&mut self) -> bool {
        loop {
            self.apply_pending_reconfiguration();
            match self.receiver.try_recv() {
                Ok(RuntimeEvent::Shutdown) => return true,
                Ok(event) => {
                    decrement_saturating(&self.queued_events);
                    self.apply_pending_reconfiguration();
                    self.handle_event(event);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn handle_event(&mut self, event: RuntimeEvent) {
        let now_ms = self.now_ms();
        match event {
            RuntimeEvent::Query { work, authority } => {
                if authority != self.authority.load(Ordering::Acquire)
                    || work.cancellation().is_cancelled()
                {
                    return;
                }
                self.lifecycle.observe_query(
                    work.query().line.clone(),
                    work.query().cursor,
                    now_ms,
                );
                self.current_query = Some(QueryAuthority { work, authority });
            }
            RuntimeEvent::Cancel { reason, authority } => {
                if authority != self.authority.load(Ordering::Acquire) {
                    return;
                }
                apply_cancellation(&mut self.lifecycle, reason, now_ms);
                self.current_query = None;
            }
            RuntimeEvent::SettingsChanged { authority } => {
                if authority == self.authority.load(Ordering::Acquire) {
                    self.synchronize_settings();
                }
            }
            RuntimeEvent::CommandExecuted { command, authority } => {
                if authority != self.authority.load(Ordering::Acquire) {
                    return;
                }
                self.lifecycle.command_executed(now_ms);
                self.current_query = None;
                self.context_cache = None;
                self.recent_commands.push_front(command);
                self.recent_commands.truncate(MAX_AI_RECENT_COMMANDS);
            }
            RuntimeEvent::Shutdown => {}
        }
    }

    fn apply_pending_reconfiguration(&mut self) {
        let Some(pending) = lock(&self.pending_reconfiguration).take() else {
            return;
        };
        self.options.settings = pending.settings;
        self.options.shared_settings = None;
        self.settings_generation = 0;
        self.settings = self.options.settings.clone();
        self.apply_settings(pending.authority);
    }

    fn synchronize_settings(&mut self) {
        let (generation, settings) = self.options.settings_snapshot();
        if generation == self.settings_generation || settings.ai == self.settings.ai {
            self.settings_generation = generation;
            self.settings = settings;
            return;
        }

        let authority = advance_authority(&self.authority)
            .unwrap_or_else(|| self.authority.load(Ordering::Acquire));
        self.settings_generation = generation;
        self.settings = settings;
        self.apply_settings(authority);
    }

    fn apply_settings(&mut self, authority: u64) {
        self.lifecycle =
            configured_lifecycle(&self.settings.ai, self.options.session, self.now_ms());
        self.context_cache = None;
        if let Some(current) = self.current_query.as_mut() {
            current.authority = authority;
            if !current.work.cancellation().is_cancelled() {
                self.lifecycle.observe_query(
                    current.work.query().line.clone(),
                    current.work.query().cursor,
                    self.now_ms(),
                );
            }
        }
    }

    fn drain_results(&mut self) {
        while let Ok(result) = self.result_receiver.try_recv() {
            self.handle_result(result);
        }
    }

    fn handle_result(&mut self, result: RequestResult) {
        let now_ms = self.now_ms();
        let outcome = match &result.result {
            Ok(completion) => ProviderOutcome::Suggestion(completion.clone()),
            Err(SanitizedProviderError::RateLimited) => ProviderOutcome::RateLimited,
            Err(_) => ProviderOutcome::Failed,
        };
        let disposition = self
            .lifecycle
            .complete_request(result.token, now_ms, outcome);
        if !self.result_has_authority(&result) {
            self.stale_results.fetch_add(1, Ordering::AcqRel);
            return;
        }

        match (disposition, result.result) {
            (ResponseDisposition::Accepted { .. }, Ok(completion)) => {
                match ai_suggestion(result.query.query(), &completion) {
                    Ok(suggestion) => self.emit(
                        result.authority,
                        ProviderBatch::success(
                            AI_COMPLETION_PROVIDER,
                            result.query.query().generation,
                            vec![suggestion],
                        ),
                    ),
                    Err(error) => self.emit(
                        result.authority,
                        ProviderBatch::failure(
                            AI_COMPLETION_PROVIDER,
                            result.query.query().generation,
                            error.to_string(),
                        ),
                    ),
                }
            }
            (ResponseDisposition::RateLimited { .. } | ResponseDisposition::Failed, Err(error)) => {
                self.emit(
                    result.authority,
                    ProviderBatch::failure(
                        AI_COMPLETION_PROVIDER,
                        result.query.query().generation,
                        error.to_string(),
                    ),
                );
            }
            (ResponseDisposition::TimedOut, _) => self.emit(
                result.authority,
                ProviderBatch::failure(
                    AI_COMPLETION_PROVIDER,
                    result.query.query().generation,
                    "AI provider request timed out",
                ),
            ),
            (ResponseDisposition::IncompatibleSuggestion, _) => self.emit(
                result.authority,
                ProviderBatch::failure(
                    AI_COMPLETION_PROVIDER,
                    result.query.query().generation,
                    "AI response did not satisfy the completion contract",
                ),
            ),
            (ResponseDisposition::NoSuggestion | ResponseDisposition::Stale, _)
            | (ResponseDisposition::Failed | ResponseDisposition::RateLimited { .. }, Ok(_))
            | (ResponseDisposition::Accepted { .. }, Err(_)) => {}
        }
    }

    fn result_has_authority(&self, result: &RequestResult) -> bool {
        result.authority == self.authority.load(Ordering::Acquire)
            && result.settings_generation == self.settings_generation
            && result.query.query().generation == self.latest_generation.load(Ordering::Acquire)
            && !result.query.cancellation().is_cancelled()
    }

    fn poll_lifecycle(&mut self) {
        let now_ms = self.now_ms();
        match self.lifecycle.poll(now_ms) {
            LifecyclePoll::Start(permit) => self.start_request(&permit),
            LifecyclePoll::Cached(cached) => {
                let Some(current) = self.current_query.as_ref() else {
                    return;
                };
                if current.authority != self.authority.load(Ordering::Acquire)
                    || current.work.cancellation().is_cancelled()
                {
                    return;
                }
                if let Ok(suggestion) = ai_suggestion(current.work.query(), cached.completion()) {
                    self.emit(
                        current.authority,
                        ProviderBatch::success(
                            AI_COMPLETION_PROVIDER,
                            current.work.query().generation,
                            vec![suggestion],
                        ),
                    );
                }
            }
            LifecyclePoll::TimedOut(_) => {
                if let Some(current) = self.current_query.as_ref() {
                    self.emit(
                        current.authority,
                        ProviderBatch::failure(
                            AI_COMPLETION_PROVIDER,
                            current.work.query().generation,
                            "AI provider request timed out",
                        ),
                    );
                }
            }
            LifecyclePoll::Idle
            | LifecyclePoll::Debouncing { .. }
            | LifecyclePoll::MinimumInterval { .. }
            | LifecyclePoll::Cooldown { .. }
            | LifecyclePoll::InFlight { .. }
            | LifecyclePoll::CounterExhausted => {}
        }
    }

    fn start_request(&mut self, permit: &RequestPermit) {
        let Some(current) = self.current_query.as_ref() else {
            self.fail_permit(permit.token(), "AI query authority was unavailable");
            return;
        };
        if current.authority != self.authority.load(Ordering::Acquire)
            || current.work.cancellation().is_cancelled()
            || permit.buffer() != current.work.query().line
        {
            self.fail_permit(permit.token(), "AI query authority was cancelled");
            return;
        }

        let query = current.work.clone();
        let authority = current.authority;
        let settings_generation = self.settings_generation;
        let request = match self.prepare_request(permit, &query) {
            Ok(request) => request,
            Err(error) => {
                self.fail_permit(permit.token(), &error);
                return;
            }
        };
        if authority != self.authority.load(Ordering::Acquire)
            || query.cancellation().is_cancelled()
        {
            self.fail_permit(permit.token(), "AI query authority was cancelled");
            return;
        }

        let permit_slot =
            self.active_requests
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                    (active < MAX_REQUEST_WORKERS).then_some(active + 1)
                });
        if permit_slot.is_err() {
            self.fail_permit(permit.token(), "AI provider worker capacity is exhausted");
            return;
        }

        let sender = self.result_sender.clone();
        let transport = Arc::clone(&self.transport);
        let active_requests = Arc::clone(&self.active_requests);
        let request_authority = Arc::clone(&self.authority);
        let token = permit.token();
        let spawn = thread::Builder::new()
            .name("argmax-ai-request".to_owned())
            .spawn(move || {
                let _active = ActiveRequest::new(active_requests);
                if authority != request_authority.load(Ordering::Acquire)
                    || query.cancellation().is_cancelled()
                {
                    return;
                }
                let result = transport(request);
                let _send_result = sender.send(RequestResult {
                    token,
                    query,
                    authority,
                    settings_generation,
                    result,
                });
            });
        if spawn.is_err() {
            self.active_requests.fetch_sub(1, Ordering::AcqRel);
            self.fail_permit(token, "AI provider worker could not start");
            return;
        }
        self.requests_started.fetch_add(1, Ordering::AcqRel);
    }

    fn prepare_request(
        &mut self,
        permit: &RequestPermit,
        query: &QueryWork,
    ) -> Result<PreparedAiRequest, String> {
        let context_level = self.settings.ai.context_level;
        let (workspace, git) = self.context_for(context_level, permit.provider(), query);
        let gathered = GatheredPromptContext {
            input: permit.buffer().to_owned(),
            shell: self.options.shell.clone(),
            operating_system: self.options.operating_system.clone(),
            workspace,
            git,
        };
        let prompt = build_prompt(context_level, &gathered).map_err(|error| error.to_string())?;
        let provider = self
            .settings
            .ai
            .providers
            .get(permit.provider())
            .ok_or_else(|| "selected AI provider is not configured".to_owned())?;
        prepare_openai_request(provider, &prompt, PlainHttpPolicy::LoopbackOnly, |name| {
            std::env::var_os(name).and_then(|value| value.into_string().ok())
        })
        .map_err(|error| error.to_string())
    }

    fn context_for(
        &mut self,
        level: AiContextLevel,
        provider: &str,
        query: &QueryWork,
    ) -> (Option<WorkspacePromptData>, Option<GitPromptData>) {
        if level == AiContextLevel::Minimal {
            return (None, None);
        }
        if query.cancellation().is_cancelled() {
            return (None, None);
        }
        let key = context_key(level, provider, query.query(), &self.recent_commands);
        let now = Instant::now();
        let now_ms = self.now_ms();
        let lifecycle_fresh = self.lifecycle.fresh_provider_context(key, now_ms).is_some();
        if lifecycle_fresh {
            if let Some(cached) = self
                .context_cache
                .as_ref()
                .filter(|cached| cached.key == key && now < cached.expires_at)
            {
                return (cached.workspace.clone(), cached.git.clone());
            }
        }

        let (workspace, git) = gather_broader_context(level, query, &self.recent_commands);
        if query.cancellation().is_cancelled() {
            return (None, None);
        }
        let _metadata = self.lifecycle.record_provider_context(key, now_ms);
        self.context_cache = Some(CachedContext {
            key,
            expires_at: now.checked_add(CONTEXT_CACHE_TTL).unwrap_or(now),
            workspace: workspace.clone(),
            git: git.clone(),
        });
        (workspace, git)
    }

    fn fail_permit(&mut self, token: RequestToken, message: &str) {
        let disposition =
            self.lifecycle
                .complete_request(token, self.now_ms(), ProviderOutcome::Failed);
        if disposition == ResponseDisposition::Failed {
            if let Some(current) = self.current_query.as_ref() {
                self.emit(
                    current.authority,
                    ProviderBatch::failure(
                        AI_COMPLETION_PROVIDER,
                        current.work.query().generation,
                        message,
                    ),
                );
            }
        }
    }

    fn emit(&self, authority: u64, batch: ProviderBatch) {
        if authority != self.authority.load(Ordering::Acquire)
            || batch.generation != self.latest_generation.load(Ordering::Acquire)
        {
            self.stale_results.fetch_add(1, Ordering::AcqRel);
            return;
        }
        let mut output = lock(&self.output);
        if output.len() == OUTPUT_QUEUE_CAPACITY {
            output.pop_front();
        }
        output.push_back(AuthorizedBatch { authority, batch });
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct ActiveRequest {
    active: Arc<AtomicUsize>,
}

impl ActiveRequest {
    const fn new(active: Arc<AtomicUsize>) -> Self {
        Self { active }
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn configured_lifecycle(ai: &Ai, session: SessionId, now_ms: u64) -> Lifecycle {
    let timeout = ai
        .provider
        .as_deref()
        .and_then(|name| ai.providers.get(name))
        .map_or(
            crate::ai_lifecycle::DEFAULT_PROVIDER_TIMEOUT_MS,
            |provider| provider.timeout_ms,
        );
    let Ok(settings) = LifecycleSettings::new(ai.debounce_ms, ai.min_interval_ms, timeout) else {
        let mut lifecycle = Lifecycle::new(LifecycleSettings::default());
        lifecycle.start_session(session, now_ms);
        return lifecycle;
    };
    let mut lifecycle = Lifecycle::new(settings);
    lifecycle.set_provider(ai.provider.clone(), now_ms);
    lifecycle.start_session(session, now_ms);
    lifecycle.set_enabled(ai.enabled && ai.readiness().is_ok(), now_ms);
    lifecycle
}

fn apply_cancellation(lifecycle: &mut Lifecycle, reason: CancellationReason, now_ms: u64) {
    match reason {
        CancellationReason::ModeChanged => {
            lifecycle.mode_changed(now_ms);
        }
        CancellationReason::MenuNavigation => {
            lifecycle.menu_navigated(now_ms);
        }
        CancellationReason::CommandExecution => {
            lifecycle.command_executed(now_ms);
        }
        CancellationReason::SessionExit => {
            lifecycle.end_session(now_ms);
        }
        CancellationReason::Disabled => {
            lifecycle.set_enabled(false, now_ms);
        }
        CancellationReason::ProviderChanged => {
            lifecycle.provider_configuration_changed(now_ms);
        }
        CancellationReason::SessionChanged => {
            lifecycle.start_session(SessionId::new(u64::MAX), now_ms);
        }
        CancellationReason::BufferChanged | CancellationReason::CursorMovedAwayFromEnd => {
            lifecycle.observe_query(String::new(), 0, now_ms);
        }
    }
}

fn advance_authority(authority: &AtomicU64) -> Option<u64> {
    authority
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .ok()
        .and_then(|previous| previous.checked_add(1))
}

fn decrement_saturating(value: &AtomicUsize) {
    let _ = value.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_sub(1))
    });
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn context_key(
    level: AiContextLevel,
    provider: &str,
    query: &CompletionQuery,
    recent_commands: &VecDeque<String>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    match level {
        AiContextLevel::Minimal => 0_u8,
        AiContextLevel::Workspace => 1,
        AiContextLevel::Full => 2,
    }
    .hash(&mut hasher);
    provider.hash(&mut hasher);
    query.cwd.hash(&mut hasher);
    query.line.hash(&mut hasher);
    query.cursor.hash(&mut hasher);
    recent_commands.hash(&mut hasher);
    hasher.finish()
}

fn gather_broader_context(
    level: AiContextLevel,
    query: &QueryWork,
    recent_commands: &VecDeque<String>,
) -> (Option<WorkspacePromptData>, Option<GitPromptData>) {
    let cancellation = query.cancellation();
    if cancellation.is_cancelled() {
        return (None, None);
    }
    let cwd = &query.query().cwd;
    let detected = detect_workspace(cwd);
    let mut signature_filenames = detected
        .signatures
        .iter()
        .filter_map(|signature| signature.marker.file_name())
        .filter_map(|name| name.to_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    signature_filenames.sort();
    signature_filenames.dedup();
    signature_filenames.truncate(MAX_SIGNATURE_FILENAMES);

    let mut workspace = WorkspacePromptData {
        cwd: cwd.to_str().map(str::to_owned),
        recent_commands: recent_commands.iter().cloned().collect(),
        signature_filenames,
        directory_names: immediate_directory_names(cwd, cancellation),
        allowlisted_command_help: catalog_command_help(query.query()),
        ..WorkspacePromptData::default()
    };
    let mut node_packages = Vec::new();
    if let Some(root) = detected
        .signatures
        .iter()
        .find(|signature| signature.kind == WorkspaceKind::Node)
        .map(|signature| signature.root.as_path())
    {
        let metadata = node_metadata(&root.join("package.json"));
        workspace.package_scripts = metadata.scripts;
        node_packages = metadata.packages;
    }
    if let Some(root) = detected
        .signatures
        .iter()
        .find(|signature| signature.kind == WorkspaceKind::Make)
        .map(|signature| signature.root.as_path())
    {
        let path = ["Makefile", "makefile", "GNUmakefile"]
            .iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file());
        workspace.make_targets = structured_targets(path.as_deref(), MAX_TARGETS);
    }
    if let Some(root) = detected
        .signatures
        .iter()
        .find(|signature| signature.kind == WorkspaceKind::Just)
        .map(|signature| signature.root.as_path())
    {
        let path = ["justfile", "Justfile"]
            .iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file());
        workspace.just_targets = structured_targets(path.as_deref(), MAX_TARGETS);
    }
    workspace.local_resources = relevant_local_resources(query.query(), &workspace, node_packages);

    let git = if level == AiContextLevel::Full {
        detected
            .signatures
            .iter()
            .find(|signature| signature.kind == WorkspaceKind::Git)
            .map(|signature| gather_git_context(&signature.root, cancellation))
    } else {
        None
    };
    if cancellation.is_cancelled() {
        return (None, None);
    }
    (Some(workspace), git)
}

fn catalog_command_help(query: &CompletionQuery) -> Vec<NamedPromptValue> {
    let Some((name, command)) = resolved_catalog_command(query) else {
        return Vec::new();
    };
    let value = render_catalog_help(command);
    if value.is_empty() {
        return Vec::new();
    }
    vec![NamedPromptValue {
        name: truncate_utf8(&name, MAX_RESOURCE_NAME_BYTES),
        value,
    }]
    .into_iter()
    .take(MAX_HELP_ENTRIES)
    .collect()
}

fn resolved_catalog_command(query: &CompletionQuery) -> Option<(String, &'static CommandSpec)> {
    let index = crate::catalog::spec_index().ok()?;
    let prefix = query.line.get(..query.cursor)?;
    let skeleton = index.command_skeleton(prefix)?;
    let mut path = skeleton.split_whitespace();
    let root_name = path.next()?;
    let mut command = index
        .roots()
        .iter()
        .find(|command| command.name == root_name)?;
    for name in path {
        command = command
            .subcommands
            .iter()
            .find(|subcommand| subcommand.name == name)?;
    }
    Some((skeleton, command))
}

fn render_catalog_help(command: &CommandSpec) -> String {
    let mut lines = Vec::new();
    if !command.description.trim().is_empty() {
        lines.push(command.description.clone());
    }
    lines.extend(
        command
            .subcommands
            .iter()
            .take(MAX_HELP_SUBCOMMANDS)
            .map(|subcommand| {
                format!("subcommand {}: {}", subcommand.name, subcommand.description)
            }),
    );
    lines.extend(command.options.iter().take(MAX_HELP_OPTIONS).map(|option| {
        let names = option.names().collect::<Vec<_>>().join(", ");
        format!("option {names}: {}", option.description)
    }));
    truncate_utf8(&lines.join("\n"), MAX_HELP_BYTES)
}

fn relevant_local_resources(
    query: &CompletionQuery,
    workspace: &WorkspacePromptData,
    node_packages: Vec<String>,
) -> Vec<LocalResourceGroup> {
    let Some((command, _)) = resolved_catalog_command(query) else {
        return Vec::new();
    };
    let Some(root) = command.split_whitespace().next() else {
        return Vec::new();
    };
    let mut groups = Vec::new();
    match root {
        "npm" | "npx" | "pnpm" | "yarn" | "bun" => {
            push_resource_group(&mut groups, "package", node_packages);
            push_resource_group(
                &mut groups,
                "package-script",
                workspace
                    .package_scripts
                    .iter()
                    .map(|script| script.name.clone()),
            );
        }
        "make" => push_resource_group(
            &mut groups,
            "make-target",
            workspace.make_targets.iter().cloned(),
        ),
        "just" => push_resource_group(
            &mut groups,
            "just-target",
            workspace.just_targets.iter().cloned(),
        ),
        "cd" | "du" | "find" | "ls" | "tree" => push_resource_group(
            &mut groups,
            "directory",
            workspace.directory_names.iter().cloned(),
        ),
        _ => {}
    }
    groups
}

fn push_resource_group(
    groups: &mut Vec<LocalResourceGroup>,
    kind: &str,
    names: impl IntoIterator<Item = String>,
) {
    if groups.len() == MAX_LOCAL_RESOURCE_GROUPS {
        return;
    }
    let mut names = names
        .into_iter()
        .filter(|name| !name.is_empty())
        .map(|name| truncate_utf8(&name, MAX_RESOURCE_NAME_BYTES))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names.truncate(MAX_LOCAL_RESOURCES_PER_GROUP);
    if names.is_empty() {
        return;
    }
    groups.push(LocalResourceGroup {
        kind: truncate_utf8(kind, MAX_RESOURCE_NAME_BYTES),
        names,
    });
}

fn truncate_utf8(value: &str, maximum: usize) -> String {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn immediate_directory_names(cwd: &Path, cancellation: &CancellationToken) -> Vec<String> {
    let Ok(entries) = fs::read_dir(cwd) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.take(MAX_DIRECTORY_ENTRIES_INSPECTED) {
        if cancellation.is_cancelled() {
            return Vec::new();
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }
    names.sort();
    names.dedup();
    names.truncate(MAX_DIRECTORY_NAMES);
    names
}

#[derive(Default)]
struct NodePromptMetadata {
    scripts: Vec<NamedPromptValue>,
    packages: Vec<String>,
}

fn node_metadata(path: &Path) -> NodePromptMetadata {
    let Some(contents) = read_bounded_utf8(path) else {
        return NodePromptMetadata::default();
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return NodePromptMetadata::default();
    };
    let mut scripts = document
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .map_or_else(Vec::new, |scripts| {
            scripts
                .iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|value| NamedPromptValue {
                        name: name.clone(),
                        value: value.to_owned(),
                    })
                })
                .collect::<Vec<_>>()
        });
    scripts.sort_by(|left, right| left.name.cmp(&right.name));
    scripts.truncate(MAX_PACKAGE_SCRIPTS);

    let mut packages = [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ]
    .into_iter()
    .filter_map(|field| document.get(field).and_then(serde_json::Value::as_object))
    .flat_map(|dependencies| dependencies.keys().cloned())
    .map(|name| truncate_utf8(&name, MAX_RESOURCE_NAME_BYTES))
    .collect::<Vec<_>>();
    packages.sort();
    packages.dedup();
    packages.truncate(MAX_LOCAL_RESOURCES_PER_GROUP);
    NodePromptMetadata { scripts, packages }
}

fn structured_targets(path: Option<&Path>, limit: usize) -> Vec<String> {
    let Some(contents) = path.and_then(read_bounded_utf8) else {
        return Vec::new();
    };
    let mut targets = contents
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| line.split_once(':').map(|(target, _)| target.trim()))
        .filter(|target| valid_target(target))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    targets.truncate(limit);
    targets
}

fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= crate::ai_prompt::MAX_RESOURCE_NAME_BYTES
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn read_bounded_utf8(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let limit = u64::try_from(MAX_STRUCTURED_METADATA_BYTES)
        .ok()?
        .checked_add(1)?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_STRUCTURED_METADATA_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn gather_git_context(root: &Path, cancellation: &CancellationToken) -> GitPromptData {
    GitPromptData {
        status: git_output(
            root,
            &["status", "--short", "--branch", "--untracked-files=normal"],
            MAX_GIT_STATUS_BYTES,
            cancellation,
        ),
        staged_diff: git_output(
            root,
            &["diff", "--cached", "--no-ext-diff"],
            MAX_STAGED_DIFF_BYTES,
            cancellation,
        ),
        branch_names: git_lines(
            root,
            &["branch", "--format=%(refname:short)"],
            MAX_BRANCH_NAMES,
            cancellation,
        ),
        recent_commit_subjects: git_lines(
            root,
            &["log", "-6", "--pretty=format:%s"],
            MAX_COMMIT_SUBJECTS,
            cancellation,
        ),
    }
}

fn git_lines(
    root: &Path,
    arguments: &[&str],
    limit: usize,
    cancellation: &CancellationToken,
) -> Vec<String> {
    git_output(root, arguments, MAX_GIT_STATUS_BYTES, cancellation)
        .map_or_else(Vec::new, |output| {
            output.lines().take(limit).map(str::to_owned).collect()
        })
}

fn git_output(
    root: &Path,
    arguments: &[&str],
    limit: usize,
    cancellation: &CancellationToken,
) -> Option<String> {
    if cancellation.is_cancelled() {
        return None;
    }
    let request = LocalProcessRequest::new(
        "git",
        arguments.iter().map(OsString::from),
        root,
        LOCAL_CONTEXT_PROCESS_TIMEOUT,
        limit,
    )
    .ok()?;
    let output = run_local_process(&request).ok()?;
    if cancellation.is_cancelled() || !output.exit().success() {
        return None;
    }
    String::from_utf8(output.into_stdout()).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use crate::completion::SuggestionSource;
    use crate::config::AiProvider;
    use crate::coordinator::CompletionCoordinator;

    static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            let identifier = TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-runtime-ai-test-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _remove_result = fs::remove_dir_all(&self.0);
        }
    }

    fn settings(enabled: bool) -> Settings {
        let mut settings = Settings::default();
        settings.ai.enabled = enabled;
        settings.ai.provider = Some("community".to_owned());
        settings.ai.debounce_ms = 0;
        settings.ai.min_interval_ms = 1;
        settings.ai.providers.insert(
            "community".to_owned(),
            AiProvider {
                endpoint: Some("http://127.0.0.1:11434/v1".to_owned()),
                model: Some("greendale".to_owned()),
                timeout_ms: 500,
                ..AiProvider::default()
            },
        );
        settings
    }

    fn query(line: &str, cwd: &Path) -> QueryWork {
        let coordinator = Box::leak(Box::new(
            CompletionCoordinator::new([AI_COMPLETION_PROVIDER], 8).unwrap(),
        ));
        coordinator.start_query(line, line.len(), cwd).unwrap()
    }

    fn wait_for_batch(dispatcher: &AiCompletionDispatcher) -> ProviderBatch {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(batch) = dispatcher.drain_batches(1).pop() {
                return batch;
            }
            assert!(Instant::now() < deadline, "AI batch did not arrive");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn disabled_configuration_never_invokes_transport() {
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let dispatcher = AiCompletionDispatcher::spawn_with_transport(
            AiCompletionOptions::new("bash", settings(false)),
            Arc::new(move |_| {
                worker_calls.fetch_add(1, Ordering::AcqRel);
                Ok("git status".to_owned())
            }),
        )
        .unwrap();
        let cwd = TemporaryDirectory::new();
        assert_eq!(
            dispatcher.submit_query(query("git", &cwd.0)),
            AiQueryAdmission::Queued
        );
        thread::sleep(Duration::from_millis(60));
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert_eq!(dispatcher.status().requests_started, 0);
        assert!(dispatcher.drain_batches(8).is_empty());
    }

    #[test]
    fn incomplete_enabled_configuration_never_invokes_transport() {
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let mut incomplete = Settings::default();
        incomplete.ai.enabled = true;
        incomplete.ai.debounce_ms = 0;
        incomplete.ai.min_interval_ms = 1;
        let dispatcher = AiCompletionDispatcher::spawn_with_transport(
            AiCompletionOptions::new("bash", incomplete),
            Arc::new(move |_| {
                worker_calls.fetch_add(1, Ordering::AcqRel);
                Ok("git status".to_owned())
            }),
        )
        .unwrap();
        let cwd = TemporaryDirectory::new();
        assert_eq!(
            dispatcher.submit_query(query("git", &cwd.0)),
            AiQueryAdmission::Queued
        );
        thread::sleep(Duration::from_millis(60));
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert_eq!(dispatcher.status().requests_started, 0);
        assert!(dispatcher.drain_batches(8).is_empty());
    }

    #[test]
    fn explicit_reconfiguration_precedes_the_following_query() {
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let dispatcher = AiCompletionDispatcher::spawn_with_transport(
            AiCompletionOptions::new("bash", settings(false)),
            Arc::new(move |_| {
                worker_calls.fetch_add(1, Ordering::AcqRel);
                Ok("git status".to_owned())
            }),
        )
        .unwrap();
        assert!(dispatcher.reconfigure(settings(true)));
        let cwd = TemporaryDirectory::new();
        assert_eq!(
            dispatcher.submit_query(query("git", &cwd.0)),
            AiQueryAdmission::Queued
        );

        let batch = wait_for_batch(&dispatcher);
        assert!(batch.error.is_none());
        assert_eq!(batch.suggestions.len(), 1);
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn explicit_reconfiguration_does_not_depend_on_event_queue_capacity() {
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let queued_events = Arc::new(AtomicUsize::new(EVENT_QUEUE_CAPACITY));
        for authority in 1..=EVENT_QUEUE_CAPACITY {
            assert!(
                sender
                    .try_send(RuntimeEvent::SettingsChanged {
                        authority: authority as u64,
                    })
                    .is_ok()
            );
        }
        let pending_reconfiguration = Arc::new(Mutex::new(None));
        let dispatcher = AiCompletionDispatcher {
            sender,
            pending_reconfiguration: Arc::clone(&pending_reconfiguration),
            output: Arc::new(Mutex::new(VecDeque::new())),
            authority: Arc::new(AtomicU64::new(0)),
            latest_generation: Arc::new(AtomicU64::new(0)),
            queued_events,
            requests_started: Arc::new(AtomicU64::new(0)),
            stale_results: Arc::new(AtomicU64::new(0)),
            active_requests: Arc::new(AtomicUsize::new(0)),
            alive: Arc::new(AtomicBool::new(true)),
            worker: None,
        };

        assert!(dispatcher.reconfigure(settings(true)));
        let pending = lock(&pending_reconfiguration);
        assert!(pending.as_ref().is_some_and(|replacement| {
            replacement.authority == 1 && replacement.settings.ai.enabled
        }));
        drop(pending);
        assert_eq!(dispatcher.status().queued_events, EVENT_QUEUE_CAPACITY);

        drop(receiver);
        drop(dispatcher);
    }

    #[test]
    fn accepted_output_is_an_inert_ai_provider_batch() {
        let dispatcher = AiCompletionDispatcher::spawn_with_transport(
            AiCompletionOptions::new("zsh", settings(true)),
            Arc::new(|_| Ok("git status".to_owned())),
        )
        .unwrap();
        let cwd = TemporaryDirectory::new();
        assert_eq!(
            dispatcher.submit_query(query("git", &cwd.0)),
            AiQueryAdmission::Queued
        );

        let batch = wait_for_batch(&dispatcher);
        assert_eq!(batch.provider, AI_COMPLETION_PROVIDER);
        assert_eq!(batch.generation, 0);
        assert!(batch.error.is_none());
        assert_eq!(batch.suggestions.len(), 1);
        let suggestion = &batch.suggestions[0];
        assert_eq!(suggestion.source(), SuggestionSource::Ai);
        assert_eq!(suggestion.edit().apply("git").unwrap(), "git status");
    }

    #[test]
    fn changed_query_suppresses_the_late_generation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let dispatcher = AiCompletionDispatcher::spawn_with_transport(
            AiCompletionOptions::new("fish", settings(true)),
            Arc::new(move |_| {
                let call = worker_calls.fetch_add(1, Ordering::AcqRel);
                if call == 0 {
                    thread::sleep(Duration::from_millis(80));
                    Ok("git status".to_owned())
                } else {
                    Ok("git stash".to_owned())
                }
            }),
        )
        .unwrap();
        let cwd = TemporaryDirectory::new();

        assert_eq!(
            dispatcher.submit_query(query("git", &cwd.0)),
            AiQueryAdmission::Queued
        );
        let started_deadline = Instant::now() + Duration::from_secs(1);
        while calls.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < started_deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            dispatcher.submit_query(query("git s", &cwd.0)),
            AiQueryAdmission::Queued
        );

        let batch = wait_for_batch(&dispatcher);
        assert_eq!(batch.generation, 0);
        assert_eq!(batch.suggestions.len(), 1);
        assert_eq!(
            batch.suggestions[0].edit().apply("git s").unwrap(),
            "git stash"
        );
        let stale_deadline = Instant::now() + Duration::from_secs(1);
        while dispatcher.status().stale_results == 0 {
            assert!(Instant::now() < stale_deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(dispatcher.status().stale_results >= 1);
    }

    #[test]
    fn cancellation_discards_a_provider_result() {
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let dispatcher = AiCompletionDispatcher::spawn_with_transport(
            AiCompletionOptions::new("bash", settings(true)),
            Arc::new(move |_| {
                worker_calls.fetch_add(1, Ordering::AcqRel);
                thread::sleep(Duration::from_millis(50));
                Ok("git status".to_owned())
            }),
        )
        .unwrap();
        let cwd = TemporaryDirectory::new();
        assert_eq!(
            dispatcher.submit_query(query("git", &cwd.0)),
            AiQueryAdmission::Queued
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while calls.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        dispatcher.cancel(CancellationReason::MenuNavigation);
        let stale_deadline = Instant::now() + Duration::from_secs(1);
        while dispatcher.status().stale_results == 0 {
            assert!(Instant::now() < stale_deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(dispatcher.drain_batches(8).is_empty());
        assert!(dispatcher.status().stale_results >= 1);
    }

    #[test]
    fn workspace_gathering_is_bounded_and_structured() {
        let temporary = TemporaryDirectory::new();
        fs::write(
            temporary.0.join("package.json"),
            r#"{"scripts":{"study":"cargo test","blank":4},"dependencies":{"@greendale/study-group":"1"}}"#,
        )
        .unwrap();
        fs::write(temporary.0.join("Makefile"), "study:\n\t@true\n").unwrap();
        fs::write(temporary.0.join("notes.txt"), "secret contents").unwrap();
        fs::create_dir(temporary.0.join("study-group")).unwrap();
        let recent = VecDeque::from(["git status".to_owned()]);
        let work = query("npm install ", &temporary.0);

        let (workspace, git) = gather_broader_context(AiContextLevel::Workspace, &work, &recent);
        let workspace = workspace.unwrap();
        assert!(git.is_none());
        assert_eq!(workspace.recent_commands, ["git status"]);
        assert_eq!(workspace.directory_names, ["study-group"]);
        assert_eq!(workspace.make_targets, ["study"]);
        assert_eq!(workspace.package_scripts.len(), 1);
        assert_eq!(workspace.package_scripts[0].name, "study");
        assert_eq!(workspace.allowlisted_command_help.len(), 1);
        assert_eq!(workspace.allowlisted_command_help[0].name, "npm install");
        assert!(
            workspace.allowlisted_command_help[0]
                .value
                .contains("install packages")
        );
        assert_eq!(workspace.local_resources.len(), 2);
        assert_eq!(workspace.local_resources[0].kind, "package");
        assert_eq!(
            workspace.local_resources[0].names,
            ["@greendale/study-group"]
        );
        assert_eq!(workspace.local_resources[1].kind, "package-script");
        assert_eq!(workspace.local_resources[1].names, ["study"]);
        assert!(!workspace.directory_names.contains(&"notes.txt".to_owned()));
        assert!(!format!("{workspace:?}").contains("secret contents"));
    }

    #[test]
    fn provider_context_cache_key_is_command_sensitive() {
        let temporary = TemporaryDirectory::new();
        let recent = VecDeque::from(["git status".to_owned()]);
        let npm = query("npm install ", &temporary.0);
        let git = query("git status ", &temporary.0);

        assert_ne!(
            context_key(AiContextLevel::Workspace, "community", npm.query(), &recent),
            context_key(AiContextLevel::Workspace, "community", git.query(), &recent)
        );
    }

    #[test]
    fn cancelled_query_skips_workspace_gathering() {
        let temporary = TemporaryDirectory::new();
        let mut coordinator = CompletionCoordinator::new([AI_COMPLETION_PROVIDER], 8).unwrap();
        let cancelled = coordinator
            .start_query("npm install ", 12, &temporary.0)
            .unwrap();
        let _replacement = coordinator
            .start_query("git status ", 11, &temporary.0)
            .unwrap();

        assert!(cancelled.cancellation().is_cancelled());
        assert_eq!(
            gather_broader_context(AiContextLevel::Workspace, &cancelled, &VecDeque::new()),
            (None, None)
        );
    }

    #[test]
    fn debug_output_contains_no_prompt_or_provider_configuration() {
        let options = AiCompletionOptions::new("bash-with-secret", settings(true));
        let debug = format!("{options:?}");
        assert!(!debug.contains("bash-with-secret"));
        assert!(!debug.contains("127.0.0.1"));
        assert!(!debug.contains("greendale"));
    }
}
