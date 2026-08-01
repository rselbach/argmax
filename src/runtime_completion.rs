//! Application-level orchestration for local completion providers.
//!
//! The dispatcher owns exactly one worker. Query submissions replace pending
//! work during a short debounce window, and completed-command events use a
//! separate bounded queue. The interactive runtime can therefore submit work
//! and drain inert provider batches without performing filesystem, process, or
//! database I/O on its input-forwarding path.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::catalog;
use crate::completion::{
    CompletionQuery, InsertionBehavior, ProviderBatch, SpecIndex, Suggestion, SuggestionSource,
    TextEdit, tokenize,
};
use crate::config::Settings;
use crate::coordinator::{
    MAX_BATCH_BYTES, MAX_BATCH_CANDIDATES, MAX_CANDIDATE_BYTES, MAX_QUERY_CWD_BYTES, QueryWork,
};
use crate::history::{
    HistoryCache, HistoryEntry, HistoryFormat, HistoryTier, parse_history, search_history,
    search_history_with_specs,
};
use crate::learning::{CommandOutcome, LearningEvent, LearningState};
use crate::learning_store::{LearningStore, MAX_LEARNING_COMMAND_BYTES, MAX_LEARNING_CWD_BYTES};
use crate::providers::execution::{
    DynamicExecutor, GeneratorExecutionContext, GitGeneratorSettings,
};
use crate::providers::{
    Alias, FilesystemOptions, PathExecutableCache, ShellKind, WorkspaceContext,
    alias_expansion_edit, alias_suggestions, detect_workspace, filesystem_suggestions,
    load_aliases, path_suggestions, resolve_alias_for_lookup,
};
use crate::ranking::{
    LocalRankingCandidate, LocalRankingContext, rank_all_with_local_intelligence,
};
use crate::reload::SharedSettings;
use crate::session::{SessionEffect, SessionMode};

/// Exact provider identity registered with [`crate::session::SessionReducer`].
pub const LOCAL_COMPLETION_PROVIDER: &str = "local";

/// Provider identities emitted by [`LocalCompletionDispatcher`].
pub const LOCAL_COMPLETION_PROVIDERS: [&str; 1] = [LOCAL_COMPLETION_PROVIDER];

/// Maximum batches returned by one nonblocking drain.
pub const MAX_COMPLETION_DRAIN_BATCHES: usize = 8;

/// Maximum alias edits returned by one nonblocking drain.
pub const MAX_ALIAS_EXPANSION_DRAIN: usize = 8;

/// Maximum completed commands awaiting asynchronous learning.
pub const MAX_PENDING_COMMAND_EVENTS: usize = 64;

const QUERY_DEBOUNCE: Duration = Duration::from_millis(20);
const MAX_ENVIRONMENT_NAMES: usize = 4_096;
const MAX_ENVIRONMENT_NAME_BYTES: usize = 1_024;
const MAX_SESSION_HISTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_SESSION_HISTORY_ENTRIES: usize = 16 * 1024;
const MAX_RANKING_SKELETON_BYTES: usize = 8 * 1024;
const MAX_RANKING_SKELETON_TOKENS: usize = 64;
const UNKNOWN_SKELETON: &str = "command";

/// One persistent shell-history source.
#[derive(Clone, Eq, PartialEq)]
pub struct HistorySource {
    path: PathBuf,
    format: HistoryFormat,
}

impl HistorySource {
    /// Creates an explicit history source.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, format: HistoryFormat) -> Self {
        Self {
            path: path.into(),
            format,
        }
    }

    /// History-file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// History-file syntax.
    #[must_use]
    pub const fn format(&self) -> HistoryFormat {
        self.format
    }
}

impl fmt::Debug for HistorySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HistorySource")
            .field("path_bytes", &path_bytes(&self.path))
            .field("format", &self.format)
            .finish()
    }
}

/// Immutable startup inputs for one session-local completion worker.
pub struct LocalCompletionOptions {
    shell: ShellKind,
    settings: Settings,
    shared_settings: Option<SharedSettings>,
    path: OsString,
    home_directory: Option<PathBuf>,
    environment_names: Vec<String>,
    alias_paths: Vec<PathBuf>,
    history: Option<HistorySource>,
    learning_store: Option<LearningStore>,
}

impl LocalCompletionOptions {
    /// Creates local-provider options with fixed settings and a captured `PATH`.
    #[must_use]
    pub fn new(shell: ShellKind, settings: Settings, path: impl Into<OsString>) -> Self {
        Self {
            shell,
            settings,
            shared_settings: None,
            path: path.into(),
            home_directory: None,
            environment_names: Vec::new(),
            alias_paths: Vec::new(),
            history: None,
            learning_store: None,
        }
    }

    /// Reads complete settings generations published by the config reloader.
    #[must_use]
    pub fn with_shared_settings(mut self, settings: SharedSettings) -> Self {
        self.shared_settings = Some(settings);
        self
    }

    /// Supplies the home directory used for home-relative completion.
    #[must_use]
    pub fn with_home_directory(mut self, home: impl Into<PathBuf>) -> Self {
        self.home_directory = Some(home.into());
        self
    }

    /// Supplies conventional alias-bearing files in shell load order.
    #[must_use]
    pub fn with_alias_paths(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.alias_paths = paths.into_iter().collect();
        self
    }

    /// Supplies structured environment names without retaining their values.
    #[must_use]
    pub fn with_environment_names<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.environment_names = bounded_environment_names(names);
        self
    }

    /// Supplies one lazy persistent-history source.
    #[must_use]
    pub fn with_history(mut self, history: HistorySource) -> Self {
        self.history = Some(history);
        self
    }

    /// Enables asynchronous persistent command learning.
    #[must_use]
    pub fn with_learning_store(mut self, store: LearningStore) -> Self {
        self.learning_store = Some(store);
        self
    }

    fn settings_snapshot(&self) -> Settings {
        self.shared_settings.as_ref().map_or_else(
            || self.settings.clone(),
            |shared| shared.snapshot().settings().clone(),
        )
    }
}

impl fmt::Debug for LocalCompletionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalCompletionOptions")
            .field("shell", &self.shell)
            .field("has_shared_settings", &self.shared_settings.is_some())
            .field("path_bytes", &self.path.as_encoded_bytes().len())
            .field(
                "home_bytes",
                &self.home_directory.as_deref().map_or(0, path_bytes),
            )
            .field("environment_name_count", &self.environment_names.len())
            .field("alias_path_count", &self.alias_paths.len())
            .field("has_history", &self.history.is_some())
            .field("has_learning_store", &self.learning_store.is_some())
            .finish_non_exhaustive()
    }
}

/// Disposition of one coalescing query submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum QueryAdmission {
    /// Work became the pending query.
    Queued,
    /// Older pending work was replaced before execution.
    Coalesced,
    /// Work was already cancelled and was ignored.
    Cancelled,
    /// The worker is shutting down or has exited.
    Closed,
}

/// Disposition of a completed-command event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum CommandEventAdmission {
    /// The event will be applied asynchronously.
    Queued,
    /// A blank command was intentionally ignored.
    Ignored,
    /// The bounded event queue was full.
    Full,
    /// The worker is shutting down or has exited.
    Closed,
}

/// Invalid completed-command or working-directory input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionEventError {
    /// Command text exceeded the learning-store input bound.
    CommandTooLarge {
        /// Observed UTF-8 bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// Working directory was empty or relative.
    CwdNotAbsolute,
    /// Working directory exceeded the strictest consumer bound.
    CwdTooLarge {
        /// Observed encoded bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
}

impl fmt::Display for CompletionEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "completed command is {bytes} bytes; limit is {limit}"
                )
            }
            Self::CwdNotAbsolute => formatter.write_str("working directory must be absolute"),
            Self::CwdTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "working directory is {bytes} bytes; limit is {limit}"
                )
            }
        }
    }
}

impl std::error::Error for CompletionEventError {}

/// Non-sensitive worker health and queue counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionDispatcherStatus {
    /// Whether the worker has not exited.
    pub alive: bool,
    /// Pending completed-command count.
    pub pending_commands: usize,
    /// Number of accepted state events processed by the worker.
    pub processed_events: u64,
    /// Number of learning load or persistence failures isolated from the session.
    pub learning_failures: u64,
    /// Most recently submitted query generation.
    pub latest_generation: u64,
}

/// One background-validated inert alias edit for an exact query generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AliasExpansion {
    generation: u64,
    edit: TextEdit,
}

impl AliasExpansion {
    /// Exact query generation against which the edit was derived.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Takes the generation and inert text edit for reducer validation.
    #[must_use]
    pub fn into_parts(self) -> (u64, TextEdit) {
        (self.generation, self.edit)
    }
}

/// One session-local, coalescing completion and learning worker.
pub struct LocalCompletionDispatcher {
    inbox: Arc<WorkerInbox>,
    output: Arc<Mutex<VecDeque<ProviderBatch>>>,
    alias_output: Arc<Mutex<VecDeque<AliasExpansion>>>,
    latest_generation: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
    processed_events: Arc<AtomicU64>,
    learning_failures: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl LocalCompletionDispatcher {
    /// Starts exactly one local-provider worker.
    ///
    /// # Errors
    ///
    /// Returns the operating-system thread creation error.
    pub fn spawn(options: LocalCompletionOptions) -> io::Result<Self> {
        let inbox = Arc::new(WorkerInbox::default());
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let alias_output = Arc::new(Mutex::new(VecDeque::new()));
        let latest_generation = Arc::new(AtomicU64::new(0));
        let alive = Arc::new(AtomicBool::new(true));
        let processed_events = Arc::new(AtomicU64::new(0));
        let learning_failures = Arc::new(AtomicU64::new(0));

        let worker_inbox = Arc::clone(&inbox);
        let worker_output = Arc::clone(&output);
        let worker_alias_output = Arc::clone(&alias_output);
        let worker_latest = Arc::clone(&latest_generation);
        let worker_alive = Arc::clone(&alive);
        let worker_processed = Arc::clone(&processed_events);
        let worker_learning_failures = Arc::clone(&learning_failures);
        let worker = thread::Builder::new()
            .name("argmax-local-completion".to_owned())
            .spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Worker::new(
                        options,
                        worker_inbox,
                        worker_output,
                        worker_alias_output,
                        worker_latest,
                        worker_processed,
                        worker_learning_failures,
                    )
                    .run();
                }));
                worker_alive.store(false, Ordering::Release);
                drop(run);
            })?;

        Ok(Self {
            inbox,
            output,
            alias_output,
            latest_generation,
            alive,
            processed_events,
            learning_failures,
            worker: Some(worker),
        })
    }

    /// Accepts a reducer effect only when it is provider query work.
    ///
    /// Non-query effects are returned unchanged so the caller cannot
    /// accidentally consume terminal input, rendering, or control work.
    ///
    /// # Errors
    ///
    /// Returns the original effect when it is not [`SessionEffect::StartQuery`].
    pub fn submit_effect(&self, effect: SessionEffect) -> Result<QueryAdmission, SessionEffect> {
        match effect {
            SessionEffect::StartQuery {
                mode,
                alias_expansion,
                work,
            } => Ok(self.submit_query_with_alias_expansion(mode, work, alias_expansion)),
            other => Err(other),
        }
    }

    /// Coalesces one immutable reducer query into the worker inbox.
    pub fn submit_query(&self, mode: SessionMode, work: QueryWork) -> QueryAdmission {
        self.submit_query_with_alias_expansion(mode, work, false)
    }

    /// Coalesces a query and its exact typed-space provenance into the inbox.
    ///
    /// `alias_expansion` must only be true for a non-pasted typed ASCII space.
    /// The worker still parses the authoritative query, aliases, and live
    /// setting before it can emit an inert edit.
    pub fn submit_query_with_alias_expansion(
        &self,
        mode: SessionMode,
        work: QueryWork,
        alias_expansion: bool,
    ) -> QueryAdmission {
        if work.cancellation().is_cancelled() {
            return QueryAdmission::Cancelled;
        }
        if !self.alive.load(Ordering::Acquire) {
            return QueryAdmission::Closed;
        }

        let generation = work.query().generation;
        self.latest_generation.store(generation, Ordering::Release);
        let mut state = lock(&self.inbox.state);
        if state.shutdown {
            return QueryAdmission::Closed;
        }
        let coalesced = state.pending_query.replace(PendingQuery {
            mode,
            alias_expansion,
            work,
            submitted_at: Instant::now(),
        });
        drop(state);
        self.inbox.ready.notify_one();
        if coalesced.is_some() {
            QueryAdmission::Coalesced
        } else {
            QueryAdmission::Queued
        }
    }

    /// Queues one completed command for session history and local learning.
    ///
    /// # Errors
    ///
    /// Returns a bounded-input error before retaining any command text or path.
    pub fn record_completed_command(
        &self,
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timestamp: u64,
        outcome: CommandOutcome,
    ) -> Result<CommandEventAdmission, CompletionEventError> {
        let command = command.into();
        if command.trim().is_empty() {
            return Ok(CommandEventAdmission::Ignored);
        }
        if command.len() > MAX_LEARNING_COMMAND_BYTES {
            return Err(CompletionEventError::CommandTooLarge {
                bytes: command.len(),
                limit: MAX_LEARNING_COMMAND_BYTES,
            });
        }
        let cwd = validate_cwd(cwd.into())?;
        if !self.alive.load(Ordering::Acquire) {
            return Ok(CommandEventAdmission::Closed);
        }

        let mut state = lock(&self.inbox.state);
        if state.shutdown {
            return Ok(CommandEventAdmission::Closed);
        }
        if state.commands.len() == MAX_PENDING_COMMAND_EVENTS {
            return Ok(CommandEventAdmission::Full);
        }
        state.commands.push_back(CompletedCommand {
            command,
            cwd,
            timestamp,
            outcome,
        });
        drop(state);
        self.inbox.ready.notify_one();
        Ok(CommandEventAdmission::Queued)
    }

    /// Coalesces a working-directory change and invalidates relevant caches.
    ///
    /// # Errors
    ///
    /// Returns a bounded-input error before retaining the path.
    pub fn update_cwd(&self, cwd: impl Into<PathBuf>) -> Result<bool, CompletionEventError> {
        let cwd = validate_cwd(cwd.into())?;
        if !self.alive.load(Ordering::Acquire) {
            return Ok(false);
        }
        let mut state = lock(&self.inbox.state);
        if state.shutdown {
            return Ok(false);
        }
        state.pending_cwd = Some(cwd);
        drop(state);
        self.inbox.ready.notify_one();
        Ok(true)
    }

    /// Coalesces a complete validated settings replacement on the worker.
    ///
    /// Callers enqueue this before the replacement query so Git, filesystem,
    /// alias, and UI behavior change at the same runtime boundary.
    #[must_use]
    pub fn reconfigure(&self, settings: Settings) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        let mut state = lock(&self.inbox.state);
        if state.shutdown {
            return false;
        }
        state.pending_settings = Some(settings);
        drop(state);
        self.inbox.ready.notify_one();
        true
    }

    /// Drains at most `limit` current provider replacements without waiting.
    ///
    /// Lock contention is treated like an empty queue, keeping this suitable for
    /// the interactive runtime's polling loop. Every batch is a cumulative,
    /// fully ranked replacement. Clone its suggestions before acceptance, then
    /// pass that exact permutation to
    /// [`crate::session::SessionReducer::apply_ranked_candidates`].
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
        let count = output.len().min(limit).min(MAX_COMPLETION_DRAIN_BATCHES);
        output.drain(..count).collect()
    }

    /// Drains bounded alias edits without waiting on worker or filesystem I/O.
    ///
    /// Each edit remains inert until
    /// [`crate::session::SessionReducer::apply_alias_expansion`] validates its
    /// exact generation and emits the synchronized replacement effects.
    #[must_use]
    pub fn drain_alias_expansions(&self, limit: usize) -> Vec<AliasExpansion> {
        if limit == 0 {
            return Vec::new();
        }
        let mut output = match self.alias_output.try_lock() {
            Ok(output) => output,
            Err(TryLockError::WouldBlock) => return Vec::new(),
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        let count = output.len().min(limit).min(MAX_ALIAS_EXPANSION_DRAIN);
        output.drain(..count).collect()
    }

    /// Returns non-sensitive worker health and queue counters.
    #[must_use]
    pub fn status(&self) -> CompletionDispatcherStatus {
        let pending_commands = lock(&self.inbox.state).commands.len();
        CompletionDispatcherStatus {
            alive: self.alive.load(Ordering::Acquire),
            pending_commands,
            processed_events: self.processed_events.load(Ordering::Acquire),
            learning_failures: self.learning_failures.load(Ordering::Acquire),
            latest_generation: self.latest_generation.load(Ordering::Acquire),
        }
    }
}

impl fmt::Debug for LocalCompletionDispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalCompletionDispatcher")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Drop for LocalCompletionDispatcher {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.inbox.state);
            state.shutdown = true;
            state.pending_query = None;
            state.pending_cwd = None;
            state.pending_settings = None;
            state.commands.clear();
        }
        self.inbox.ready.notify_all();
        if let Some(worker) = self.worker.take() {
            drop(worker.join());
        }
    }
}

#[derive(Default)]
struct WorkerInbox {
    state: Mutex<InboxState>,
    ready: Condvar,
}

#[derive(Default)]
struct InboxState {
    pending_query: Option<PendingQuery>,
    pending_cwd: Option<PathBuf>,
    pending_settings: Option<Settings>,
    commands: VecDeque<CompletedCommand>,
    shutdown: bool,
}

struct PendingQuery {
    mode: SessionMode,
    alias_expansion: bool,
    work: QueryWork,
    submitted_at: Instant,
}

struct CompletedCommand {
    command: String,
    cwd: PathBuf,
    timestamp: u64,
    outcome: CommandOutcome,
}

enum WorkerItem {
    Query(PendingQuery),
    Cwd(PathBuf),
    Command(CompletedCommand),
    Settings(Settings),
    Shutdown,
}

#[derive(Clone, Copy)]
struct QueryRankingContext<'a> {
    mode: SessionMode,
    settings: &'a Settings,
    history_order: &'a BTreeMap<String, usize>,
    workspace: &'a WorkspaceContext,
}

struct Worker {
    options: LocalCompletionOptions,
    inbox: Arc<WorkerInbox>,
    output: Arc<Mutex<VecDeque<ProviderBatch>>>,
    alias_output: Arc<Mutex<VecDeque<AliasExpansion>>>,
    latest_generation: Arc<AtomicU64>,
    processed_events: Arc<AtomicU64>,
    learning_failures: Arc<AtomicU64>,
    index: Option<&'static SpecIndex>,
    aliases: AliasCache,
    paths: PathExecutableCache,
    dynamic: DynamicExecutor,
    history: HistoryCache,
    session_history: VecDeque<HistoryEntry>,
    session_history_bytes: usize,
    learning: LearningState,
    prior_skeleton: Option<String>,
}

impl Worker {
    fn new(
        options: LocalCompletionOptions,
        inbox: Arc<WorkerInbox>,
        output: Arc<Mutex<VecDeque<ProviderBatch>>>,
        alias_output: Arc<Mutex<VecDeque<AliasExpansion>>>,
        latest_generation: Arc<AtomicU64>,
        processed_events: Arc<AtomicU64>,
        learning_failures: Arc<AtomicU64>,
    ) -> Self {
        let learning =
            options
                .learning_store
                .as_ref()
                .map_or_else(LearningState::default, |store| {
                    store.load().unwrap_or_else(|_| {
                        learning_failures.fetch_add(1, Ordering::Relaxed);
                        LearningState::default()
                    })
                });
        Self {
            index: catalog::spec_index().ok(),
            aliases: AliasCache::new(options.alias_paths.clone()),
            options,
            inbox,
            output,
            alias_output,
            latest_generation,
            processed_events,
            learning_failures,
            paths: PathExecutableCache::default(),
            dynamic: DynamicExecutor::new(),
            history: HistoryCache::default(),
            session_history: VecDeque::new(),
            session_history_bytes: 0,
            learning,
            prior_skeleton: None,
        }
    }

    fn run(&mut self) {
        loop {
            match self.next_item() {
                WorkerItem::Query(query) => self.complete(query),
                WorkerItem::Cwd(cwd) => {
                    self.dynamic.invalidate_cwd(&cwd);
                    self.paths.invalidate();
                    self.processed_events.fetch_add(1, Ordering::Release);
                }
                WorkerItem::Command(command) => {
                    self.record_command(command);
                    self.processed_events.fetch_add(1, Ordering::Release);
                }
                WorkerItem::Settings(settings) => {
                    self.options.settings = settings;
                    self.options.shared_settings = None;
                    self.processed_events.fetch_add(1, Ordering::Release);
                }
                WorkerItem::Shutdown => return,
            }
        }
    }

    fn next_item(&self) -> WorkerItem {
        let mut state = lock(&self.inbox.state);
        loop {
            if state.shutdown {
                return WorkerItem::Shutdown;
            }
            if let Some(settings) = state.pending_settings.take() {
                return WorkerItem::Settings(settings);
            }
            if let Some(cwd) = state.pending_cwd.take() {
                return WorkerItem::Cwd(cwd);
            }
            if let Some(command) = state.commands.pop_front() {
                return WorkerItem::Command(command);
            }
            if let Some(query) = state.pending_query.as_ref() {
                let elapsed = query.submitted_at.elapsed();
                if elapsed >= QUERY_DEBOUNCE {
                    if let Some(query) = state.pending_query.take() {
                        return WorkerItem::Query(query);
                    }
                    continue;
                }
                let Some(remaining) = QUERY_DEBOUNCE.checked_sub(elapsed) else {
                    continue;
                };
                let waited = self
                    .inbox
                    .ready
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner);
                state = waited.0;
                continue;
            }
            state = self
                .inbox
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn complete(&mut self, pending: PendingQuery) {
        let PendingQuery {
            mode,
            alias_expansion,
            work,
            submitted_at: _,
        } = pending;
        if self.aborted(&work) {
            return;
        }
        let query = work.query();
        if mode == SessionMode::Spec && query.prefix().trim().is_empty() {
            self.publish(query.generation, Vec::new(), &work);
            return;
        }

        let settings = self.options.settings_snapshot();
        let aliases = self.aliases.current(self.options.shell).to_vec();
        if alias_expansion {
            if let Some(edit) =
                alias_expansion_edit(&aliases, query, settings.core.expand_alias, false)
            {
                self.publish_alias_expansion(query.generation, edit, &work);
            }
            if self.aborted(&work) {
                return;
            }
        }
        let workspace = detect_workspace(&query.cwd);
        let mut history_order = BTreeMap::new();
        let mut suggestions =
            self.fast_suggestions(query, mode, &settings, &aliases, &mut history_order);
        let ranking = QueryRankingContext {
            mode,
            settings: &settings,
            history_order: &history_order,
            workspace: &workspace,
        };
        self.rank_and_publish(query, &suggestions, ranking, &work);

        if self.aborted(&work) {
            return;
        }
        let dynamic = self.curated_dynamic(query, &settings, &aliases, &work);
        if !dynamic.is_empty() {
            suggestions.extend(dynamic);
            self.rank_and_publish(query, &suggestions, ranking, &work);
        }

        if self.aborted(&work) {
            return;
        }
        let cobra = self.cobra_suggestions(query, &settings, &work);
        if !cobra.is_empty() {
            suggestions.extend(cobra);
            self.rank_and_publish(query, &suggestions, ranking, &work);
        }
    }

    fn fast_suggestions(
        &mut self,
        query: &CompletionQuery,
        mode: SessionMode,
        settings: &Settings,
        aliases: &[Alias],
        history_order: &mut BTreeMap<String, usize>,
    ) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();
        let parsed = tokenize(&query.line, query.cursor).ok();

        if let Some(line) = parsed.as_ref() {
            if line.committed_tokens().is_empty() {
                let active = line.active_token();
                let range = active.raw.start..line.full_active_token().raw.end;
                suggestions.extend(alias_suggestions(aliases, &active.cooked, range.clone()));
                let executables = self
                    .paths
                    .executables(&self.options.path, &query.cwd)
                    .to_vec();
                suggestions.extend(path_suggestions(
                    &executables,
                    &active.cooked,
                    range,
                    self.options.shell,
                ));
            }
        }

        if let Some(index) = self.index {
            suggestions.extend(static_spec_suggestions(index, aliases, query));
        }

        if should_complete_filesystem(parsed.as_ref()) {
            let options = FilesystemOptions {
                include_hidden: settings.ui.hidden_files,
                home_directory: self.options.home_directory.clone(),
                ..FilesystemOptions::default()
            };
            suggestions.extend(filesystem_suggestions(query, self.options.shell, &options));
        }

        if mode == SessionMode::History {
            let history = self.history_suggestions(query, aliases);
            for (position, suggestion) in history.iter().enumerate() {
                if let Ok(line) = suggestion.resulting_line(query) {
                    history_order.entry(line).or_insert(position);
                }
            }
            suggestions.extend(history);
        }
        suggestions
    }

    fn history_suggestions(
        &mut self,
        query: &CompletionQuery,
        aliases: &[Alias],
    ) -> Vec<Suggestion> {
        let entries = self.options.history.as_ref().map_or_else(
            || self.session_history.iter().rev().cloned().collect(),
            |source| {
                self.history.merged(source.path(), |contents| {
                    parse_history(source.format(), contents)
                })
            },
        );
        let alias_pairs = aliases
            .iter()
            .map(|alias| (alias.name.as_str(), alias.value.as_str()))
            .collect::<Vec<_>>();
        let matches = self.index.map_or_else(
            || search_history(&entries, query.prefix(), &alias_pairs),
            |index| search_history_with_specs(&entries, query.prefix(), &alias_pairs, index),
        );
        matches
            .into_iter()
            .enumerate()
            .map(|(position, matched)| {
                let mut suggestion = Suggestion::new(
                    TextEdit {
                        range: 0..query.line.len(),
                        replacement: matched.entry.command.clone(),
                    },
                    &matched.entry.command,
                    "shell history",
                    "history",
                    SuggestionSource::History,
                    InsertionBehavior::Exact,
                    format!("history:{position}"),
                );
                suggestion.static_priority = history_priority(matched.tier);
                suggestion.confidence = 1.0;
                suggestion
            })
            .collect()
    }

    fn curated_dynamic(
        &mut self,
        query: &CompletionQuery,
        settings: &Settings,
        aliases: &[Alias],
        work: &QueryWork,
    ) -> Vec<Suggestion> {
        let Some(index) = self.index else {
            return Vec::new();
        };
        let credential_environment = settings.ai.credential_environment_names();
        let context = execution_context(&self.options, settings, &credential_environment);
        if let Some(lookup) = resolve_alias_for_lookup(aliases, query) {
            let suggestions = self.dynamic.complete_curated(
                index,
                lookup.lookup_query(),
                context,
                work.cancellation(),
            );
            return remap_alias_suggestions(suggestions, &lookup);
        }
        self.dynamic
            .complete_curated(index, query, context, work.cancellation())
    }

    fn cobra_suggestions(
        &mut self,
        query: &CompletionQuery,
        settings: &Settings,
        work: &QueryWork,
    ) -> Vec<Suggestion> {
        let Some(index) = self.index else {
            return Vec::new();
        };
        let credential_environment = settings.ai.credential_environment_names();
        let context = execution_context(&self.options, settings, &credential_environment);
        self.dynamic
            .complete_cobra(index, query, context, work.cancellation())
            .map_or_else(Vec::new, |result| result.suggestions)
    }

    /// Ranks the complete merged local set, then applies provider/UI bounds.
    ///
    /// This worker is the production owner of local composite ranking. Bounding
    /// happens only after every local candidate has participated in scoring.
    fn rank_and_publish(
        &self,
        query: &CompletionQuery,
        suggestions: &[Suggestion],
        context: QueryRankingContext<'_>,
        work: &QueryWork,
    ) {
        if self.aborted(work) {
            return;
        }
        let merged = crate::completion::merge_suggestions(query, [suggestions.to_vec()]);
        let local_context = LocalRankingContext {
            workspace: context.workspace,
            learning: &self.learning,
            cwd: &query.cwd,
            now: unix_seconds_now(),
            prior_skeleton: self.prior_skeleton.as_deref(),
        };
        // The query's active token is the same for every candidate, so it is
        // tokenized once per batch rather than once per suggestion.
        let active_token = active_query_token(query);
        let candidates = merged
            .into_iter()
            .map(|suggestion| {
                let skeleton = suggestion_skeleton(self.index, query, &suggestion);
                let quality = match_quality(&active_token, &suggestion);
                LocalRankingCandidate::new(suggestion, skeleton, quality)
            })
            .collect::<Vec<_>>();
        let mut ranked = rank_all_with_local_intelligence(candidates, local_context)
            .candidates
            .into_iter()
            .map(|candidate| candidate.suggestion)
            .collect::<Vec<_>>();
        if context.mode == SessionMode::History {
            let composite_order = ranked
                .iter()
                .enumerate()
                .map(|(position, suggestion)| (suggestion.identity().to_owned(), position))
                .collect::<BTreeMap<_, _>>();
            ranked.sort_by_key(|suggestion| {
                suggestion
                    .resulting_line(query)
                    .ok()
                    .and_then(|line| context.history_order.get(&line).copied())
                    .map_or_else(
                        || {
                            (
                                1,
                                composite_order
                                    .get(suggestion.identity())
                                    .copied()
                                    .unwrap_or(usize::MAX),
                            )
                        },
                        |position| (0, position),
                    )
            });
        }
        let maximum = usize::from(context.settings.ui.max_suggestions).min(MAX_BATCH_CANDIDATES);
        let ranked = bound_provider_batch(ranked, maximum);
        self.publish(query.generation, ranked, work);
    }

    fn publish(&self, generation: u64, suggestions: Vec<Suggestion>, work: &QueryWork) {
        if self.aborted(work) {
            return;
        }
        let batch = ProviderBatch::success(LOCAL_COMPLETION_PROVIDER, generation, suggestions);
        let mut output = lock(&self.output);
        output.retain(|queued| queued.generation > generation);
        if self.latest_generation.load(Ordering::Acquire) == generation {
            output.push_back(batch);
            while output.len() > MAX_COMPLETION_DRAIN_BATCHES {
                output.pop_front();
            }
        }
    }

    fn publish_alias_expansion(&self, generation: u64, edit: TextEdit, work: &QueryWork) {
        if self.aborted(work) {
            return;
        }
        let mut output = lock(&self.alias_output);
        output.retain(|queued| queued.generation > generation);
        if self.latest_generation.load(Ordering::Acquire) == generation {
            output.push_back(AliasExpansion { generation, edit });
            while output.len() > MAX_ALIAS_EXPANSION_DRAIN {
                output.pop_front();
            }
        }
    }

    fn record_command(&mut self, completed: CompletedCommand) {
        // A leading blank is the shells' established "keep this out of
        // history" convention. This store outlives shell history and has no
        // expiry, so honoring it matters more here than there.
        if completed.command.starts_with([' ', '\t']) {
            self.prior_skeleton = None;
            return;
        }
        let history =
            HistoryEntry::new(completed.command.clone()).with_timestamp(completed.timestamp);
        if self.options.history.is_some() {
            self.history.record_session(history);
        } else {
            self.record_session_only(history);
        }
        let skeleton = command_skeleton(self.index, &completed.command);
        let mut event = LearningEvent::new(
            completed.command,
            &skeleton,
            completed.cwd,
            completed.timestamp,
            completed.outcome,
        );
        if let Some(prior) = &self.prior_skeleton {
            event = event.with_prior_skeleton(prior);
        }
        if self.learning.record(&event).is_err() {
            self.learning_failures.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.prior_skeleton = Some(skeleton);
        if self
            .options
            .learning_store
            .as_ref()
            .is_some_and(|store| store.record(&event).is_err())
        {
            self.learning_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_session_only(&mut self, entry: HistoryEntry) {
        let bytes = entry.command.len();
        if bytes > MAX_SESSION_HISTORY_BYTES {
            return;
        }
        while self.session_history.len() >= MAX_SESSION_HISTORY_ENTRIES
            || self.session_history_bytes.saturating_add(bytes) > MAX_SESSION_HISTORY_BYTES
        {
            let Some(removed) = self.session_history.pop_front() else {
                break;
            };
            self.session_history_bytes = self
                .session_history_bytes
                .saturating_sub(removed.command.len());
        }
        self.session_history.push_back(entry);
        self.session_history_bytes += bytes;
    }

    fn aborted(&self, work: &QueryWork) -> bool {
        work.cancellation().is_cancelled()
            || self.latest_generation.load(Ordering::Acquire) != work.query().generation
            || lock(&self.inbox.state).shutdown
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AliasFileSignature {
    path: PathBuf,
    length: Option<u64>,
    modified: Option<SystemTime>,
}

struct AliasCache {
    paths: Vec<PathBuf>,
    signatures: Vec<AliasFileSignature>,
    aliases: Vec<Alias>,
}

impl AliasCache {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            signatures: Vec::new(),
            aliases: Vec::new(),
        }
    }

    fn current(&mut self, shell: ShellKind) -> &[Alias] {
        let signatures = self
            .paths
            .iter()
            .map(|path| {
                let metadata = std::fs::metadata(path).ok();
                AliasFileSignature {
                    path: path.clone(),
                    length: metadata.as_ref().map(std::fs::Metadata::len),
                    modified: metadata.and_then(|value| value.modified().ok()),
                }
            })
            .collect::<Vec<_>>();
        if signatures != self.signatures {
            self.aliases = load_aliases(shell, &self.paths);
            self.signatures = signatures;
        }
        &self.aliases
    }
}

fn execution_context<'a>(
    options: &'a LocalCompletionOptions,
    settings: &Settings,
    credential_environment: &'a [OsString],
) -> GeneratorExecutionContext<'a> {
    GeneratorExecutionContext {
        shell: options.shell,
        path: &options.path,
        home_directory: options.home_directory.as_deref(),
        environment_names: &options.environment_names,
        credential_environment,
        include_hidden_files: settings.ui.hidden_files,
        git: GitGeneratorSettings {
            filter_active_branch: settings.git.filter_active_branch,
            deduplicate_branches: settings.git.deduplicate_branches,
        },
        infer_completions: settings.core.infer_completions,
    }
}

fn static_spec_suggestions(
    index: &SpecIndex,
    aliases: &[Alias],
    query: &CompletionQuery,
) -> Vec<Suggestion> {
    resolve_alias_for_lookup(aliases, query).map_or_else(
        || index.suggestions(query),
        |lookup| remap_alias_suggestions(index.suggestions(lookup.lookup_query()), &lookup),
    )
}

fn remap_alias_suggestions(
    suggestions: Vec<Suggestion>,
    lookup: &crate::providers::AliasLookup,
) -> Vec<Suggestion> {
    suggestions
        .into_iter()
        .filter_map(|mut suggestion| {
            suggestion.edit = lookup.map_lookup_edit(suggestion.edit())?;
            Some(suggestion)
        })
        .collect()
}

fn should_complete_filesystem(line: Option<&crate::completion::TokenizedLine>) -> bool {
    let Some(line) = line else {
        return false;
    };
    let active = &line.active_token().cooked;
    !line.committed_tokens().is_empty()
        || active.starts_with(['.', '/', '~'])
        || active.contains('/')
}

const fn history_priority(tier: HistoryTier) -> f64 {
    match tier {
        HistoryTier::Exact => 1.0,
        HistoryTier::Prefix => 0.9,
        HistoryTier::Contains => 0.75,
        HistoryTier::Fuzzy => 0.6,
    }
}

fn suggestion_skeleton(
    index: Option<&SpecIndex>,
    query: &CompletionQuery,
    suggestion: &Suggestion,
) -> String {
    suggestion.resulting_line(query).ok().map_or_else(
        || UNKNOWN_SKELETON.to_owned(),
        |line| command_skeleton(index, &line),
    )
}

fn command_skeleton(index: Option<&SpecIndex>, command: &str) -> String {
    let skeleton = index
        .and_then(|index| index.command_skeleton(command))
        .or_else(|| fallback_skeleton(command))
        .unwrap_or_else(|| UNKNOWN_SKELETON.to_owned());
    if valid_skeleton(&skeleton) {
        skeleton
    } else {
        UNKNOWN_SKELETON.to_owned()
    }
}

fn fallback_skeleton(command: &str) -> Option<String> {
    let mut line = command.to_owned();
    if !line.chars().last().is_some_and(char::is_whitespace) {
        line.push(' ');
    }
    let parsed = tokenize(&line, line.len()).ok()?;
    let first = parsed.committed_tokens().first()?;
    // Any equals sign in a leading token marks a probable assignment whose
    // value must not become a ranking key; assignment shapes vary too much
    // for name validation to enumerate safely.
    if first.cooked.contains('=') {
        return None;
    }
    Some(first.cooked.clone())
}

fn valid_skeleton(skeleton: &str) -> bool {
    skeleton.len() <= MAX_RANKING_SKELETON_BYTES
        && skeleton
            .split(' ')
            .take(MAX_RANKING_SKELETON_TOKENS + 1)
            .enumerate()
            .all(|(index, token)| {
                index < MAX_RANKING_SKELETON_TOKENS
                    && !token.is_empty()
                    && !token.chars().any(char::is_whitespace)
                    && !token.chars().any(char::is_control)
            })
}

fn active_query_token(query: &CompletionQuery) -> String {
    tokenize(&query.line, query.cursor)
        .ok()
        .map_or_else(String::new, |line| {
            line.active_token().cooked.to_lowercase()
        })
}

fn match_quality(active: &str, suggestion: &Suggestion) -> f64 {
    let display = suggestion.display().to_lowercase();
    if active.is_empty() {
        0.5
    } else if display == active {
        1.0
    } else if display.starts_with(active) {
        0.9
    } else if display.contains(active) {
        0.65
    } else if is_subsequence(active, &display) {
        0.4
    } else {
        0.2
    }
}

fn is_subsequence(needle: &str, value: &str) -> bool {
    let mut needle = needle.chars();
    let mut next = needle.next();
    for character in value.chars() {
        if next == Some(character) {
            next = needle.next();
            if next.is_none() {
                return true;
            }
        }
    }
    next.is_none()
}

fn bound_provider_batch(ranked: Vec<Suggestion>, maximum: usize) -> Vec<Suggestion> {
    let mut retained = Vec::with_capacity(ranked.len().min(maximum));
    let mut bytes = 0_usize;
    for suggestion in ranked {
        let candidate_bytes = candidate_bytes(&suggestion);
        if candidate_bytes > MAX_CANDIDATE_BYTES
            || bytes.saturating_add(candidate_bytes) > MAX_BATCH_BYTES
        {
            continue;
        }
        bytes += candidate_bytes;
        retained.push(suggestion);
        if retained.len() == maximum {
            break;
        }
    }
    retained.into_boxed_slice().into_vec()
}

fn candidate_bytes(suggestion: &Suggestion) -> usize {
    suggestion
        .edit
        .replacement
        .len()
        .saturating_add(suggestion.display.len())
        .saturating_add(suggestion.description.len())
        .saturating_add(suggestion.icon.len())
        .saturating_add(suggestion.identity.len())
}

fn bounded_environment_names<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut names = names
        .into_iter()
        .map(Into::into)
        .filter(|name| {
            !name.is_empty()
                && name.len() <= MAX_ENVIRONMENT_NAME_BYTES
                && !name.chars().any(|character| {
                    character.is_control() || character == '=' || character.is_whitespace()
                })
        })
        .take(MAX_ENVIRONMENT_NAMES)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn validate_cwd(cwd: PathBuf) -> Result<PathBuf, CompletionEventError> {
    if !cwd.is_absolute() {
        return Err(CompletionEventError::CwdNotAbsolute);
    }
    let bytes = path_bytes(&cwd);
    let limit = MAX_QUERY_CWD_BYTES.min(MAX_LEARNING_CWD_BYTES);
    if bytes > limit {
        return Err(CompletionEventError::CwdTooLarge { bytes, limit });
    }
    Ok(cwd)
}

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use crate::coordinator::CompletionCoordinator;

    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let identifier = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-runtime-completion-test-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.0));
        }
    }

    fn dispatcher(options: LocalCompletionOptions) -> LocalCompletionDispatcher {
        LocalCompletionDispatcher::spawn(options).unwrap()
    }

    fn work(coordinator: &mut CompletionCoordinator, line: &str, cwd: &Path) -> QueryWork {
        coordinator.start_query(line, line.len(), cwd).unwrap()
    }

    fn worker(options: LocalCompletionOptions) -> (Worker, Arc<Mutex<VecDeque<ProviderBatch>>>) {
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker = Worker::new(
            options,
            Arc::new(WorkerInbox::default()),
            Arc::clone(&output),
            Arc::new(Mutex::new(VecDeque::new())),
            latest_generation,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        (worker, output)
    }

    fn local_suggestion(index: usize, best: bool) -> Suggestion {
        let mut suggestion = Suggestion::new(
            TextEdit {
                range: 0..1,
                replacement: format!("git command-{index:03}"),
            },
            if best { "g" } else { "unmatched" },
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            format!("local-{index:03}"),
        );
        suggestion.static_priority = 0.0;
        suggestion.confidence = 0.0;
        suggestion
    }

    fn wait_for_batches(dispatcher: &LocalCompletionDispatcher) -> Vec<ProviderBatch> {
        for _ in 0..100 {
            let batches = dispatcher.drain_batches(MAX_COMPLETION_DRAIN_BATCHES);
            if !batches.is_empty() {
                return batches;
            }
            thread::sleep(Duration::from_millis(5));
        }
        Vec::new()
    }

    fn wait_for_alias_expansions(dispatcher: &LocalCompletionDispatcher) -> Vec<AliasExpansion> {
        for _ in 0..100 {
            let expansions = dispatcher.drain_alias_expansions(MAX_ALIAS_EXPANSION_DRAIN);
            if !expansions.is_empty() {
                return expansions;
            }
            thread::sleep(Duration::from_millis(5));
        }
        Vec::new()
    }

    fn wait_for_events(dispatcher: &LocalCompletionDispatcher, count: u64) {
        for _ in 0..100 {
            if dispatcher.status().processed_events >= count {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("worker did not process {count} events");
    }

    #[test]
    fn execution_context_carries_the_inference_opt_in() {
        let options =
            LocalCompletionOptions::new(ShellKind::Bash, Settings::default(), OsString::new());
        let mut settings = Settings::default();
        assert!(!execution_context(&options, &settings, &[]).infer_completions);
        settings.core.infer_completions = true;
        assert!(execution_context(&options, &settings, &[]).infer_completions);
    }

    #[test]
    fn worker_ranks_the_full_local_set_before_applying_the_ui_bound() {
        let temporary = TempDirectory::new();
        let mut settings = Settings::default();
        settings.ui.max_suggestions = 500;
        let (worker, output) = worker(LocalCompletionOptions::new(
            ShellKind::Bash,
            settings.clone(),
            OsString::new(),
        ));
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 500).unwrap();
        let work = work(&mut coordinator, "g", &temporary.0);
        worker
            .latest_generation
            .store(work.query().generation, Ordering::Release);
        let suggestions = (0..=500)
            .map(|index| local_suggestion(index, index == 500))
            .collect::<Vec<_>>();
        let workspace = WorkspaceContext {
            cwd: temporary.0.clone(),
            signatures: Vec::new(),
        };
        let history_order = BTreeMap::new();

        worker.rank_and_publish(
            work.query(),
            &suggestions,
            QueryRankingContext {
                mode: SessionMode::Spec,
                settings: &settings,
                history_order: &history_order,
                workspace: &workspace,
            },
            &work,
        );

        let batch = lock(&output).pop_front().expect("ranked local batch");
        assert_eq!(batch.suggestions.len(), 500);
        assert_eq!(batch.suggestions[0].identity(), "local-500");
    }

    #[test]
    fn worker_skeleton_fallback_is_bounded_and_canonical() {
        let oversized = "x".repeat(MAX_RANKING_SKELETON_BYTES + 1);
        let too_many_tokens = (0..=MAX_RANKING_SKELETON_TOKENS)
            .map(|_| "token")
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!valid_skeleton(&oversized));
        assert!(!valid_skeleton(&too_many_tokens));
        assert!(!valid_skeleton("git  status"));

        for command in [
            "git status",
            "AWS_SECRET_ACCESS_KEY=greendale",
            oversized.as_str(),
            too_many_tokens.as_str(),
        ] {
            let skeleton = command_skeleton(None, command);
            assert!(valid_skeleton(&skeleton));
            assert!(skeleton.len() <= MAX_RANKING_SKELETON_BYTES);
            assert!(skeleton.split(' ').count() <= MAX_RANKING_SKELETON_TOKENS);
        }
        assert_eq!(
            command_skeleton(None, "AWS_SECRET_ACCESS_KEY=greendale"),
            UNKNOWN_SKELETON
        );
    }

    #[test]
    fn query_effects_coalesce_and_non_query_effects_are_returned() {
        let temporary = TempDirectory::new();
        let settings = Settings::default();
        let dispatcher = dispatcher(LocalCompletionOptions::new(
            ShellKind::Bash,
            settings,
            OsString::new(),
        ));
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();

        let first = work(&mut coordinator, "git c", &temporary.0);
        assert_eq!(
            dispatcher.submit_query(SessionMode::Spec, first),
            QueryAdmission::Queued
        );
        let second = work(&mut coordinator, "git che", &temporary.0);
        let generation = second.query().generation;
        assert_eq!(
            dispatcher.submit_query(SessionMode::Spec, second),
            QueryAdmission::Coalesced
        );
        assert!(matches!(
            dispatcher.submit_effect(SessionEffect::ClearOverlay),
            Err(SessionEffect::ClearOverlay)
        ));

        let batches = wait_for_batches(&dispatcher);
        assert!(!batches.is_empty());
        assert!(batches.iter().all(|batch| batch.generation == generation));
        assert!(
            batches
                .iter()
                .flat_map(|batch| &batch.suggestions)
                .any(|suggestion| suggestion
                    .resulting_line(coordinator.active_query().unwrap())
                    .is_ok_and(|line| line.starts_with("git checkout")))
        );
    }

    #[test]
    fn hyphenated_root_prefix_keeps_matching_commands() {
        let temporary = TempDirectory::new();
        let dispatcher = dispatcher(LocalCompletionOptions::new(
            ShellKind::Zsh,
            Settings::default(),
            OsString::new(),
        ));
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();
        let query = work(&mut coordinator, "ssh-", &temporary.0);
        assert_eq!(
            dispatcher.submit_query(SessionMode::Spec, query),
            QueryAdmission::Queued
        );

        let active = coordinator.active_query().unwrap();
        let results = wait_for_batches(&dispatcher)
            .into_iter()
            .flat_map(|batch| batch.suggestions)
            .filter_map(|suggestion| suggestion.resulting_line(active).ok())
            .collect::<BTreeSet<_>>();

        assert!(results.contains("ssh-add "));
        assert!(results.contains("ssh-agent "));
        assert!(results.contains("ssh-keygen "));
        assert!(results.contains("ssh-keyscan "));
    }

    #[test]
    fn cancelled_work_emits_no_batch() {
        let temporary = TempDirectory::new();
        let dispatcher = dispatcher(LocalCompletionOptions::new(
            ShellKind::Zsh,
            Settings::default(),
            OsString::new(),
        ));
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();
        let pending = work(&mut coordinator, "git che", &temporary.0);
        assert_eq!(
            dispatcher.submit_query(SessionMode::Spec, pending),
            QueryAdmission::Queued
        );
        let _ = coordinator.cancel_active_query();

        thread::sleep(Duration::from_millis(40));
        assert!(dispatcher.drain_batches(8).is_empty());
    }

    #[test]
    fn typed_space_expansion_is_parsed_on_worker_and_honors_setting() {
        let temporary = TempDirectory::new();
        let aliases = temporary.path("bashrc");
        fs::write(&aliases, "alias gs='git status'\n").unwrap();
        let options =
            LocalCompletionOptions::new(ShellKind::Bash, Settings::default(), OsString::new())
                .with_alias_paths([aliases.clone()]);
        let enabled_dispatcher = dispatcher(options);
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();
        let enabled_work = work(&mut coordinator, "gs ", &temporary.0);
        let generation = enabled_work.query().generation;

        assert_eq!(
            enabled_dispatcher.submit_query_with_alias_expansion(
                SessionMode::Spec,
                enabled_work,
                true
            ),
            QueryAdmission::Queued
        );
        let expansions = wait_for_alias_expansions(&enabled_dispatcher);
        assert_eq!(expansions.len(), 1);
        let (received, edit) = expansions.into_iter().next().unwrap().into_parts();
        assert_eq!(received, generation);
        assert_eq!(edit.range, 0..2);
        assert_eq!(edit.replacement, "git status");

        let mut disabled = Settings::default();
        disabled.core.expand_alias = false;
        let dispatcher = dispatcher(
            LocalCompletionOptions::new(ShellKind::Bash, disabled, OsString::new())
                .with_alias_paths([aliases]),
        );
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();
        let disabled_work = work(&mut coordinator, "gs ", &temporary.0);
        assert_eq!(
            dispatcher.submit_query_with_alias_expansion(SessionMode::Spec, disabled_work, true),
            QueryAdmission::Queued
        );
        assert!(!wait_for_batches(&dispatcher).is_empty());
        assert!(dispatcher.drain_alias_expansions(8).is_empty());

        assert!(dispatcher.reconfigure(Settings::default()));
        let work = work(&mut coordinator, "gs ", &temporary.0);
        assert_eq!(
            dispatcher.submit_query_with_alias_expansion(SessionMode::Spec, work, true),
            QueryAdmission::Queued
        );
        assert_eq!(wait_for_alias_expansions(&dispatcher).len(), 1);
    }

    #[test]
    fn alias_worker_rejects_non_first_or_quoted_spaces_and_cancelled_work() {
        let temporary = TempDirectory::new();
        let aliases = temporary.path("bashrc");
        fs::write(&aliases, "alias gs='git status'\n").unwrap();
        let dispatcher = dispatcher(
            LocalCompletionOptions::new(ShellKind::Bash, Settings::default(), OsString::new())
                .with_alias_paths([aliases]),
        );
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();

        for line in ["'gs' ", "gs  ", "gs argument "] {
            let work = work(&mut coordinator, line, &temporary.0);
            assert!(matches!(
                dispatcher.submit_query_with_alias_expansion(SessionMode::Spec, work, true),
                QueryAdmission::Queued | QueryAdmission::Coalesced
            ));
            assert!(!wait_for_batches(&dispatcher).is_empty());
            assert!(dispatcher.drain_alias_expansions(8).is_empty());
        }

        let pending = work(&mut coordinator, "gs ", &temporary.0);
        assert_eq!(
            dispatcher.submit_query_with_alias_expansion(SessionMode::Spec, pending, true),
            QueryAdmission::Queued
        );
        let _ = coordinator.cancel_active_query();
        let barrier = work(&mut coordinator, "git", &temporary.0);
        assert!(matches!(
            dispatcher.submit_query(SessionMode::Spec, barrier),
            QueryAdmission::Queued | QueryAdmission::Coalesced
        ));
        assert!(!wait_for_batches(&dispatcher).is_empty());
        assert!(dispatcher.drain_alias_expansions(8).is_empty());
    }

    #[test]
    fn completed_commands_feed_history_and_persistent_learning() {
        let temporary = TempDirectory::new();
        let learning_path = temporary.path("data/learning.sqlite3");
        let store = LearningStore::new(&learning_path);
        let options =
            LocalCompletionOptions::new(ShellKind::Bash, Settings::default(), OsString::new())
                .with_learning_store(store.clone());
        let dispatcher = dispatcher(options);

        assert_eq!(
            dispatcher
                .record_completed_command(
                    "git status --short",
                    &temporary.0,
                    4_000_000,
                    CommandOutcome::Success,
                )
                .unwrap(),
            CommandEventAdmission::Queued
        );
        wait_for_events(&dispatcher, 1);

        let loaded = store.load().unwrap();
        let scores = loaded.frecency_scores(["git status"], &temporary.0, 4_000_001);
        assert_eq!(scores.len(), 1);
        assert!(scores[0].normalized_score > 0.0);

        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();
        let query = work(&mut coordinator, "git sta", &temporary.0);
        assert_eq!(
            dispatcher.submit_query(SessionMode::History, query),
            QueryAdmission::Queued
        );
        let batches = wait_for_batches(&dispatcher);
        assert!(
            batches
                .iter()
                .flat_map(|batch| &batch.suggestions)
                .any(|suggestion| suggestion
                    .resulting_line(coordinator.active_query().unwrap())
                    .is_ok_and(|line| line == "git status --short"))
        );
    }

    #[test]
    fn completed_command_is_applied_before_an_already_due_query() {
        let temporary = TempDirectory::new();
        let dispatcher = dispatcher(LocalCompletionOptions::new(
            ShellKind::Bash,
            Settings::default(),
            OsString::new(),
        ));
        let mut coordinator = CompletionCoordinator::new(LOCAL_COMPLETION_PROVIDERS, 100).unwrap();
        let query = work(&mut coordinator, "git sta", &temporary.0);
        let generation = query.query().generation;

        {
            let mut state = lock(&dispatcher.inbox.state);
            state.commands.push_back(CompletedCommand {
                command: "git status --short".to_owned(),
                cwd: temporary.0.clone(),
                timestamp: 4_000_000,
                outcome: CommandOutcome::Success,
            });
            state.pending_query = Some(PendingQuery {
                mode: SessionMode::History,
                alias_expansion: false,
                work: query,
                submitted_at: Instant::now()
                    .checked_sub(QUERY_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            });
            dispatcher
                .latest_generation
                .store(generation, Ordering::Release);
        }
        dispatcher.inbox.ready.notify_one();

        let batches = wait_for_batches(&dispatcher);
        assert!(
            batches
                .iter()
                .flat_map(|batch| &batch.suggestions)
                .any(|suggestion| suggestion
                    .resulting_line(coordinator.active_query().unwrap())
                    .is_ok_and(|line| line == "git status --short"))
        );
        assert_eq!(dispatcher.status().processed_events, 1);
    }

    #[test]
    fn a_leading_blank_keeps_a_command_out_of_the_learning_store() {
        let temporary = TempDirectory::new();
        let store = LearningStore::new(temporary.0.join("argmax/learning.sqlite3"));
        let dispatcher = dispatcher(
            LocalCompletionOptions::new(ShellKind::Fish, Settings::default(), OsString::new())
                .with_learning_store(store.clone()),
        );

        assert_eq!(
            dispatcher
                .record_completed_command(
                    " export TOKEN=greendale",
                    &temporary.0,
                    1,
                    CommandOutcome::Success
                )
                .unwrap(),
            CommandEventAdmission::Queued
        );
        assert_eq!(
            dispatcher
                .record_completed_command("git status", &temporary.0, 2, CommandOutcome::Success)
                .unwrap(),
            CommandEventAdmission::Queued
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if store.load().is_ok_and(|state| !state.commands.is_empty()) {
                break;
            }
            assert!(Instant::now() < deadline, "learning store never recorded");
            thread::sleep(Duration::from_millis(10));
        }

        let recorded = store.load().unwrap();
        assert!(
            recorded
                .commands
                .keys()
                .all(|key| !key.skeleton.contains("TOKEN")),
            "a blank-prefixed command reached the store: {:?}",
            recorded.commands.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_bare_assignment_never_becomes_a_ranking_skeleton() {
        assert_eq!(fallback_skeleton("AWS_SECRET_ACCESS_KEY=xyz"), None);
        assert_eq!(fallback_skeleton("_TOKEN=abc"), None);
        assert_eq!(
            fallback_skeleton("git status"),
            Some("git status".split(' ').next().unwrap().to_owned())
        );
        // Even an invalid variable name stays out of the keyspace: the
        // token could still carry a value.
        assert_eq!(fallback_skeleton("7zip=archive"), None);
        assert_eq!(fallback_skeleton("LD_PRELOAD+=/secret"), None);
        assert_eq!(fallback_skeleton("TOKEN[0]=secret"), None);
    }

    #[test]
    fn state_inputs_are_bounded_before_queueing() {
        let temporary = TempDirectory::new();
        let dispatcher = dispatcher(LocalCompletionOptions::new(
            ShellKind::Fish,
            Settings::default(),
            OsString::new(),
        ));
        assert_eq!(
            dispatcher
                .record_completed_command(" ", &temporary.0, 1, CommandOutcome::Failure)
                .unwrap(),
            CommandEventAdmission::Ignored
        );
        assert!(matches!(
            dispatcher.record_completed_command(
                "x".repeat(MAX_LEARNING_COMMAND_BYTES + 1),
                &temporary.0,
                1,
                CommandOutcome::Failure
            ),
            Err(CompletionEventError::CommandTooLarge { .. })
        ));
        assert_eq!(
            dispatcher.update_cwd("relative"),
            Err(CompletionEventError::CwdNotAbsolute)
        );
        assert!(dispatcher.drain_batches(0).is_empty());
    }
}
