//! Nonblocking automatic-update orchestration for interactive sessions.
//!
//! The worker keeps release transport and state-file I/O away from terminal
//! forwarding. Every request is durably reserved through [`AutomaticUpdater`]
//! before network access, and every notice is durably claimed before it becomes
//! visible to the runtime. Release artifacts are never downloaded or installed
//! here.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{UpdateChannel, Updater};
use crate::release::{ReleaseError, ReleaseSource, fetch_automatic_release};
use crate::reload::SharedSettings;
use crate::session::SessionMode;
use crate::state::{LastMode, RuntimeStateStore, StateStoreError, UpdateStoreOutcome};
use crate::updater::{
    AutomaticUpdater, AutomaticUpdaterError, CheckCompletion, CheckReservationConfirmation,
    CheckReservationDecision, ClaimConfirmation, ClaimDecision, ClaimPersistence, NetworkFailure,
    ReservationPersistence, UpdateState,
};
use crate::version::{RUNNING_VERSION, ReleaseKind, RemoteRelease};

/// How often a worker without an explicit wake-up observes shared settings.
pub const SETTINGS_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum startup jitter used to spread simultaneous terminal sessions.
pub const MAX_STARTUP_JITTER: Duration = Duration::from_millis(250);

/// Maximum positive jitter added to periodic and retry schedules.
pub const MAX_PERIODIC_JITTER: Duration = Duration::from_secs(30);

/// Maximum exponential retry multiplier after consecutive failures.
pub const MAX_FAILURE_BACKOFF_MULTIPLIER: u32 = 8;

const MAX_PENDING_NOTICES: usize = 1;

/// Immutable inputs for one session-local automatic-update worker.
pub struct RuntimeUpdateOptions {
    current_version: Box<str>,
    settings: Updater,
    shared_settings: Option<SharedSettings>,
    state_store: RuntimeStateStore,
    source: ReleaseSource,
    fetcher: Arc<dyn ReleaseFetcher>,
    jitter_seed: u64,
    startup_jitter_limit: Duration,
}

impl RuntimeUpdateOptions {
    /// Creates explicit worker options without touching disk or the network.
    #[must_use]
    pub fn new(
        current_version: impl Into<Box<str>>,
        settings: Updater,
        state_store: RuntimeStateStore,
    ) -> Self {
        Self {
            current_version: current_version.into(),
            settings,
            shared_settings: None,
            state_store,
            source: ReleaseSource::official(),
            fetcher: Arc::new(GitHubReleaseFetcher),
            jitter_seed: process_jitter_seed(),
            startup_jitter_limit: MAX_STARTUP_JITTER,
        }
    }

    /// Discovers the runtime-state store and uses the embedded build version.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform runtime-state location is unavailable.
    pub fn discover(settings: Updater) -> Result<Self, StateStoreError> {
        Ok(Self::new(
            RUNNING_VERSION,
            settings,
            RuntimeStateStore::discover()?,
        ))
    }

    /// Observes atomically published configuration generations.
    #[must_use]
    pub fn with_shared_settings(mut self, settings: SharedSettings) -> Self {
        self.shared_settings = Some(settings);
        self
    }
}

impl fmt::Debug for RuntimeUpdateOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeUpdateOptions")
            .field("current_version_bytes", &self.current_version.len())
            .field("settings", &self.settings)
            .field("has_shared_settings", &self.shared_settings.is_some())
            .field("state_store", &self.state_store)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Failure to start an automatic-update worker.
#[derive(Debug)]
pub enum RuntimeUpdateStartError {
    /// Resolved updater settings could not build a bounded policy.
    InvalidConfiguration(AutomaticUpdaterError),
    /// The operating system rejected the background thread.
    Thread(io::Error),
}

impl fmt::Display for RuntimeUpdateStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(error) => error.fmt(formatter),
            Self::Thread(error) => write!(
                formatter,
                "automatic update worker could not start: {error}"
            ),
        }
    }
}

impl Error for RuntimeUpdateStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidConfiguration(error) => Some(error),
            Self::Thread(error) => Some(error),
        }
    }
}

/// Disposition of one completed-command signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum CompletedCommandAdmission {
    /// The next worker pass will consider a notification claim.
    Queued,
    /// A prior unprocessed command signal already permits the same work.
    Coalesced,
    /// The command-generation counter cannot advance safely.
    CounterExhausted,
    /// The worker is shutting down or has exited.
    Closed,
}

/// Disposition of one coalescing selected-mode persistence request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum ModePersistenceAdmission {
    /// The mode became the pending state write.
    Queued,
    /// An older pending mode was replaced before it reached disk.
    Coalesced,
    /// The worker is shutting down or has exited.
    Closed,
}

/// A durably claimed, command-boundary-safe update notice.
#[must_use]
pub struct UpdateNotification {
    version: Box<str>,
}

impl UpdateNotification {
    /// Available semantic version without a tag prefix.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Display for UpdateNotification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "argmax {} is available; run `argmax update`",
            self.version
        )
    }
}

impl fmt::Debug for UpdateNotification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateNotification")
            .field("version_bytes", &self.version.len())
            .finish()
    }
}

/// Non-sensitive automatic-update worker health counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeUpdateStatus {
    /// Whether the worker has not exited.
    pub alive: bool,
    /// Whether current settings permit automatic network checks.
    pub enabled: bool,
    /// Whether release metadata is currently being fetched.
    pub check_in_flight: bool,
    /// Number of release requests completed in this session.
    pub checks_completed: u64,
    /// Number of silent release transport or metadata failures.
    pub network_failures: u64,
    /// Number of silent runtime-state load or persistence failures.
    pub persistence_failures: u64,
}

/// Session-local coordinator for background automatic update checks.
pub struct RuntimeUpdateWorker {
    inbox: Arc<WorkerInbox>,
    notices: Arc<Mutex<VecDeque<UpdateNotification>>>,
    alive: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    check_in_flight: Arc<AtomicBool>,
    checks_completed: Arc<AtomicU64>,
    network_failures: Arc<AtomicU64>,
    persistence_failures: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl RuntimeUpdateWorker {
    /// Starts the coordinator without loading state or contacting the network on
    /// the calling thread.
    ///
    /// # Errors
    ///
    /// Returns a policy-validation or operating-system thread-creation error.
    pub fn spawn(options: RuntimeUpdateOptions) -> Result<Self, RuntimeUpdateStartError> {
        AutomaticUpdater::new(
            &options.current_version,
            options.settings.check_on_startup,
            options.settings.channel,
            options.settings.check_interval,
            UpdateState::default(),
        )
        .map_err(RuntimeUpdateStartError::InvalidConfiguration)?;

        let inbox = Arc::new(WorkerInbox::default());
        let notices = Arc::new(Mutex::new(VecDeque::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let enabled = Arc::new(AtomicBool::new(options.settings.check_on_startup));
        let check_in_flight = Arc::new(AtomicBool::new(false));
        let checks_completed = Arc::new(AtomicU64::new(0));
        let network_failures = Arc::new(AtomicU64::new(0));
        let persistence_failures = Arc::new(AtomicU64::new(0));

        let worker_inbox = Arc::clone(&inbox);
        let worker_notices = Arc::clone(&notices);
        let worker_alive = Arc::clone(&alive);
        let worker_enabled = Arc::clone(&enabled);
        let worker_in_flight = Arc::clone(&check_in_flight);
        let worker_checks = Arc::clone(&checks_completed);
        let worker_network_failures = Arc::clone(&network_failures);
        let worker_persistence_failures = Arc::clone(&persistence_failures);
        let worker = thread::Builder::new()
            .name("argmax-update-check".to_owned())
            .spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Worker::new(
                        options,
                        WorkerShared {
                            inbox: worker_inbox,
                            notices: worker_notices,
                            enabled: worker_enabled,
                            check_in_flight: worker_in_flight,
                            checks_completed: worker_checks,
                            network_failures: worker_network_failures,
                            persistence_failures: worker_persistence_failures,
                        },
                    )
                    .run();
                }));
                worker_alive.store(false, Ordering::Release);
                drop(run);
            })
            .map_err(RuntimeUpdateStartError::Thread)?;

        Ok(Self {
            inbox,
            notices,
            alive,
            enabled,
            check_in_flight,
            checks_completed,
            network_failures,
            persistence_failures,
            worker: Some(worker),
        })
    }

    /// Coalesces one completed-command boundary for notification delivery.
    pub fn completed_command(&self) -> CompletedCommandAdmission {
        if !self.alive.load(Ordering::Acquire) {
            return CompletedCommandAdmission::Closed;
        }
        let mut state = lock(&self.inbox.state);
        if state.shutdown {
            return CompletedCommandAdmission::Closed;
        }
        let coalesced = state.command_generation > state.consumed_command_generation;
        let Some(next) = state.command_generation.checked_add(1) else {
            return CompletedCommandAdmission::CounterExhausted;
        };
        state.command_generation = next;
        drop(state);
        self.inbox.ready.notify_one();
        if coalesced {
            CompletedCommandAdmission::Coalesced
        } else {
            CompletedCommandAdmission::Queued
        }
    }

    /// Coalesces persistence of the session's most recently selected mode.
    pub fn record_mode(&self, mode: SessionMode) -> ModePersistenceAdmission {
        if !self.alive.load(Ordering::Acquire) {
            return ModePersistenceAdmission::Closed;
        }
        let mode = match mode {
            SessionMode::Spec => LastMode::Spec,
            SessionMode::History => LastMode::History,
        };
        let mut state = lock(&self.inbox.state);
        if state.shutdown {
            return ModePersistenceAdmission::Closed;
        }
        let coalesced = state.pending_mode.replace(mode).is_some();
        drop(state);
        self.inbox.ready.notify_one();
        if coalesced {
            ModePersistenceAdmission::Coalesced
        } else {
            ModePersistenceAdmission::Queued
        }
    }

    /// Applies resolved updater settings without blocking the caller.
    ///
    /// The latest pending replacement wins. Disabling checks prevents any new
    /// request; an already-running five-second request remains bounded by the
    /// release transport deadline.
    #[must_use]
    pub fn reconfigure(&self, settings: Updater) -> bool {
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

    /// Takes one already-persisted notice without waiting or performing I/O.
    #[must_use]
    pub fn take_notification(&self) -> Option<UpdateNotification> {
        match self.notices.try_lock() {
            Ok(mut notices) => notices.pop_front(),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(error)) => error.into_inner().pop_front(),
        }
    }

    /// Returns non-sensitive worker health and failure counters.
    #[must_use]
    pub fn status(&self) -> RuntimeUpdateStatus {
        RuntimeUpdateStatus {
            alive: self.alive.load(Ordering::Acquire),
            enabled: self.enabled.load(Ordering::Acquire),
            check_in_flight: self.check_in_flight.load(Ordering::Acquire),
            checks_completed: self.checks_completed.load(Ordering::Acquire),
            network_failures: self.network_failures.load(Ordering::Acquire),
            persistence_failures: self.persistence_failures.load(Ordering::Acquire),
        }
    }
}

impl fmt::Debug for RuntimeUpdateWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeUpdateWorker")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Drop for RuntimeUpdateWorker {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.inbox.state);
            state.shutdown = true;
            state.pending_settings = None;
            state.pending_mode = None;
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
    command_generation: u64,
    consumed_command_generation: u64,
    pending_settings: Option<Updater>,
    pending_mode: Option<LastMode>,
    shutdown: bool,
}

struct WorkerShared {
    inbox: Arc<WorkerInbox>,
    notices: Arc<Mutex<VecDeque<UpdateNotification>>>,
    enabled: Arc<AtomicBool>,
    check_in_flight: Arc<AtomicBool>,
    checks_completed: Arc<AtomicU64>,
    network_failures: Arc<AtomicU64>,
    persistence_failures: Arc<AtomicU64>,
}

struct Worker {
    current_version: Box<str>,
    settings: Updater,
    shared_settings: Option<SharedSettings>,
    shared_generation: Option<u64>,
    state_store: RuntimeStateStore,
    source: ReleaseSource,
    fetcher: Arc<dyn ReleaseFetcher>,
    shared: WorkerShared,
    policy: AutomaticUpdater,
    next_attempt: Option<Instant>,
    failure_streak: u32,
    jitter: Jitter,
    startup_jitter_limit: Duration,
    notice_emitted: bool,
}

impl Worker {
    fn new(options: RuntimeUpdateOptions, shared: WorkerShared) -> Self {
        let state = load_updater_state(&options.state_store, &shared.persistence_failures);
        let policy = policy(&options.current_version, options.settings, state);
        let mut jitter = Jitter::new(options.jitter_seed);
        let next_attempt = options
            .settings
            .check_on_startup
            .then(|| Instant::now() + jitter.next(options.startup_jitter_limit));
        Self {
            current_version: options.current_version,
            settings: options.settings,
            shared_settings: options.shared_settings,
            shared_generation: None,
            state_store: options.state_store,
            source: options.source,
            fetcher: options.fetcher,
            shared,
            policy,
            next_attempt,
            failure_streak: 0,
            jitter,
            startup_jitter_limit: options.startup_jitter_limit,
            notice_emitted: false,
        }
    }

    fn run(&mut self) {
        loop {
            self.observe_shared_settings();
            if let Some(settings) = self.take_pending_settings() {
                self.apply_settings(settings);
            }
            if self.shutdown_requested() {
                return;
            }
            if let Some(mode) = self.take_pending_mode() {
                self.persist_mode(mode);
            }
            if self.take_completed_command() {
                self.claim_notification();
            }
            if self.check_due() {
                self.attempt_check();
            }
            if self.shutdown_requested() {
                return;
            }
            self.wait();
        }
    }

    fn observe_shared_settings(&mut self) {
        let Some(shared) = self.shared_settings.as_ref() else {
            return;
        };
        let snapshot = shared.snapshot();
        if self.shared_generation == Some(snapshot.generation()) {
            return;
        }
        self.shared_generation = Some(snapshot.generation());
        let settings = snapshot.settings().updater;
        if settings != self.settings {
            self.apply_settings(settings);
        }
    }

    fn take_pending_settings(&self) -> Option<Updater> {
        lock(&self.shared.inbox.state).pending_settings.take()
    }

    fn take_pending_mode(&self) -> Option<LastMode> {
        lock(&self.shared.inbox.state).pending_mode.take()
    }

    fn persist_mode(&self, mode: LastMode) {
        if self.state_store.store_mode(mode).is_err() {
            self.shared
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn apply_settings(&mut self, settings: Updater) {
        if settings == self.settings {
            return;
        }
        let state = load_updater_state(&self.state_store, &self.shared.persistence_failures);
        self.policy = policy(&self.current_version, settings, state);
        self.settings = settings;
        self.failure_streak = 0;
        self.shared
            .enabled
            .store(settings.check_on_startup, Ordering::Release);
        self.next_attempt = settings
            .check_on_startup
            .then(|| Instant::now() + self.jitter.next(self.startup_jitter_limit));
        self.discard_completed_commands();
    }

    fn take_completed_command(&self) -> bool {
        let mut state = lock(&self.shared.inbox.state);
        if state.command_generation == state.consumed_command_generation {
            return false;
        }
        state.consumed_command_generation = state.command_generation;
        true
    }

    fn discard_completed_commands(&self) {
        let mut state = lock(&self.shared.inbox.state);
        state.consumed_command_generation = state.command_generation;
    }

    fn claim_notification(&mut self) {
        if self.notice_emitted {
            return;
        }
        let Ok(loaded) = self.state_store.load() else {
            self.shared
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        let persisted = loaded.state.updater().persistence_clone();
        let ClaimDecision::Claim(claim) = self.policy.claim_after_completed_command(&persisted)
        else {
            return;
        };
        let proposed = claim.state_for_persistence();
        let persistence = match self
            .state_store
            .compare_and_store_updater(&persisted, &proposed)
        {
            Ok(UpdateStoreOutcome::Persisted(_)) => ClaimPersistence::Persisted,
            Ok(UpdateStoreOutcome::Superseded(state)) => ClaimPersistence::Superseded(state),
            Err(_) => {
                self.shared
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
                ClaimPersistence::Failed
            }
        };
        if let ClaimConfirmation::Emit(intent) =
            self.policy.confirm_notification(claim, persistence)
        {
            let mut notices = lock(&self.shared.notices);
            if notices.len() < MAX_PENDING_NOTICES {
                notices.push_back(UpdateNotification {
                    version: intent.version().into(),
                });
                self.notice_emitted = true;
            }
        }
    }

    fn check_due(&self) -> bool {
        self.next_attempt.is_some_and(|due| Instant::now() >= due)
    }

    fn attempt_check(&mut self) {
        let Ok(loaded) = self.state_store.load() else {
            self.record_persistence_failure();
            return;
        };
        let persisted = loaded.state.updater().persistence_clone();
        let now_ms = unix_epoch_millis();
        let decision = self.policy.reserve_startup_check(now_ms, &persisted);
        let CheckReservationDecision::Reserve(reservation) = decision else {
            self.schedule_after_reservation_decision(&decision, now_ms);
            return;
        };
        let proposed = reservation.state_for_persistence();
        let persistence = match self
            .state_store
            .compare_and_store_updater(&persisted, &proposed)
        {
            Ok(UpdateStoreOutcome::Persisted(_)) => ReservationPersistence::Persisted,
            Ok(UpdateStoreOutcome::Superseded(state)) => ReservationPersistence::Superseded(state),
            Err(_) => ReservationPersistence::Failed,
        };
        let confirmation = self
            .policy
            .confirm_check_reservation(reservation, persistence);
        let request = match confirmation {
            CheckReservationConfirmation::Request(request) => request,
            CheckReservationConfirmation::PersistenceFailed
            | CheckReservationConfirmation::StaleReservation => {
                self.record_persistence_failure();
                return;
            }
            CheckReservationConfirmation::SupersededByPersistedState => {
                let delay = remaining_interval(
                    &self.policy.state_for_persistence(),
                    self.settings.check_interval,
                    now_ms,
                );
                self.schedule_periodic(delay);
                return;
            }
        };

        self.shared.check_in_flight.store(true, Ordering::Release);
        let response = self.fetcher.fetch(&self.source, self.settings.channel);
        // Commands completed while metadata was in flight cannot safely anchor
        // a later prompt notice. Only a subsequent command may claim it.
        self.discard_completed_commands();
        self.shared.check_in_flight.store(false, Ordering::Release);
        let completed_at_ms = unix_epoch_millis();
        let policy_response = match &response {
            Ok(release) => Ok(RemoteRelease::new(&release.version, release.kind)),
            Err(failure) => Err(*failure),
        };
        let completion =
            self.policy
                .complete_check(request.into_token(), completed_at_ms, policy_response);
        if completion != CheckCompletion::StaleToken
            && self
                .state_store
                .store_updater(&self.policy.state_for_persistence())
                .is_err()
        {
            self.shared
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
        }

        if response.is_err() {
            self.shared.network_failures.fetch_add(1, Ordering::Relaxed);
            self.record_failure();
        } else {
            self.failure_streak = 0;
            self.schedule_periodic(self.settings.check_interval);
        }
        self.shared.checks_completed.fetch_add(1, Ordering::Release);
    }

    fn schedule_after_reservation_decision(
        &mut self,
        decision: &CheckReservationDecision,
        now_ms: u64,
    ) {
        match decision {
            CheckReservationDecision::Disabled
            | CheckReservationDecision::SessionNoticeComplete
            | CheckReservationDecision::TokenExhausted => self.next_attempt = None,
            CheckReservationDecision::IntervalPending
            | CheckReservationDecision::ClockAnomaly
            | CheckReservationDecision::AlreadyInFlight
            | CheckReservationDecision::ReservationAlreadyInFlight
            | CheckReservationDecision::NotificationClaimInFlight => {
                let delay = remaining_interval(
                    &self.policy.state_for_persistence(),
                    self.settings.check_interval,
                    now_ms,
                );
                self.schedule_periodic(delay);
            }
            CheckReservationDecision::Reserve(_) => unreachable!("reservation handled above"),
        }
    }

    fn record_persistence_failure(&mut self) {
        self.shared
            .persistence_failures
            .fetch_add(1, Ordering::Relaxed);
        self.record_failure();
    }

    fn record_failure(&mut self) {
        self.failure_streak = self.failure_streak.saturating_add(1);
        let delay = retry_delay(self.settings.check_interval, self.failure_streak);
        self.schedule_periodic(delay);
    }

    fn schedule_periodic(&mut self, delay: Duration) {
        let jitter = self.jitter.next(MAX_PERIODIC_JITTER.min(delay / 10));
        self.next_attempt = Instant::now().checked_add(delay.saturating_add(jitter));
    }

    fn shutdown_requested(&self) -> bool {
        lock(&self.shared.inbox.state).shutdown
    }

    fn wait(&self) {
        let timeout = self.wait_timeout();
        let state = lock(&self.shared.inbox.state);
        if state.shutdown
            || state.pending_settings.is_some()
            || state.pending_mode.is_some()
            || state.command_generation > state.consumed_command_generation
        {
            return;
        }
        match timeout {
            Some(timeout) => {
                drop(
                    self.shared
                        .inbox
                        .ready
                        .wait_timeout(state, timeout)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
            None => {
                drop(
                    self.shared
                        .inbox
                        .ready
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
        }
    }

    fn wait_timeout(&self) -> Option<Duration> {
        let until_check = self
            .next_attempt
            .map(|due| due.saturating_duration_since(Instant::now()));
        if self.shared_settings.is_some() {
            Some(until_check.map_or(SETTINGS_POLL_INTERVAL, |duration| {
                duration.min(SETTINGS_POLL_INTERVAL)
            }))
        } else {
            until_check
        }
    }
}

trait ReleaseFetcher: Send + Sync {
    fn fetch(
        &self,
        source: &ReleaseSource,
        channel: UpdateChannel,
    ) -> Result<FetchedRelease, NetworkFailure>;
}

struct GitHubReleaseFetcher;

impl ReleaseFetcher for GitHubReleaseFetcher {
    fn fetch(
        &self,
        source: &ReleaseSource,
        channel: UpdateChannel,
    ) -> Result<FetchedRelease, NetworkFailure> {
        let release = fetch_automatic_release(source, channel).map_err(map_release_error)?;
        Ok(FetchedRelease {
            version: release.version().into(),
            kind: release.kind(),
        })
    }
}

struct FetchedRelease {
    version: Box<str>,
    kind: ReleaseKind,
}

fn policy(current_version: &str, settings: Updater, state: UpdateState) -> AutomaticUpdater {
    AutomaticUpdater::new(
        current_version,
        settings.check_on_startup,
        settings.channel,
        settings.check_interval,
        state,
    )
    .expect("validated runtime update options")
}

fn load_updater_state(store: &RuntimeStateStore, failures: &AtomicU64) -> UpdateState {
    store.load().map_or_else(
        |_| {
            failures.fetch_add(1, Ordering::Relaxed);
            UpdateState::default()
        },
        |loaded| loaded.state.updater().persistence_clone(),
    )
}

const fn map_release_error(error: ReleaseError) -> NetworkFailure {
    match error {
        ReleaseError::Timeout => NetworkFailure::Timeout,
        ReleaseError::Network => NetworkFailure::Transport,
        ReleaseError::RateLimited | ReleaseError::HttpStatus(_) => NetworkFailure::Api,
        ReleaseError::InvalidSource
        | ReleaseError::UnsupportedHost
        | ReleaseError::ResponseTooLarge
        | ReleaseError::InvalidMetadata
        | ReleaseError::MissingAsset
        | ReleaseError::InvalidChecksum => NetworkFailure::InvalidResponse,
    }
}

fn remaining_interval(state: &UpdateState, interval: Duration, now_ms: u64) -> Duration {
    let last_check = match (
        state.last_reserved_check_ms(),
        state.last_completed_check_ms(),
    ) {
        (Some(reserved), Some(completed)) => Some(reserved.max(completed)),
        (last @ Some(_), None) | (None, last @ Some(_)) => last,
        (None, None) => None,
    };
    let Some(last_check) = last_check else {
        return interval;
    };
    let interval_ms = u64::try_from(interval.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX);
    let due_ms = last_check.saturating_add(interval_ms);
    Duration::from_millis(due_ms.saturating_sub(now_ms))
}

fn retry_delay(interval: Duration, failure_streak: u32) -> Duration {
    let exponent = failure_streak.saturating_sub(1).min(3);
    let multiplier = 1_u32 << exponent;
    debug_assert!(multiplier <= MAX_FAILURE_BACKOFF_MULTIPLIER);
    interval.checked_mul(multiplier).unwrap_or(Duration::MAX)
}

fn unix_epoch_millis() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn process_jitter_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    let folded = u64::try_from(nanos).unwrap_or_else(|_| {
        let upper = u64::try_from(nanos >> 64).unwrap_or(0);
        let lower = u64::try_from(nanos & u128::from(u64::MAX)).unwrap_or(0);
        upper ^ lower
    });
    folded ^ u64::from(std::process::id())
}

struct Jitter {
    state: u64,
}

impl Jitter {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self, limit: Duration) -> Duration {
        if limit.is_zero() {
            return Duration::ZERO;
        }
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        let limit_nanos = u64::try_from(limit.as_nanos()).unwrap_or(u64::MAX);
        let nanos = value % limit_nanos.saturating_add(1);
        Duration::from_nanos(nanos)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use tempfile::TempDir;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    struct FakeFetcher {
        calls: Arc<AtomicUsize>,
        result: Result<FetchedRelease, NetworkFailure>,
    }

    impl ReleaseFetcher for FakeFetcher {
        fn fetch(
            &self,
            _source: &ReleaseSource,
            _channel: UpdateChannel,
        ) -> Result<FetchedRelease, NetworkFailure> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match &self.result {
                Ok(release) => Ok(FetchedRelease {
                    version: release.version.clone(),
                    kind: release.kind,
                }),
                Err(error) => Err(*error),
            }
        }
    }

    fn updater(enabled: bool, channel: UpdateChannel) -> Updater {
        Updater {
            check_on_startup: enabled,
            channel,
            check_interval: Duration::from_millis(20),
        }
    }

    fn options(
        temporary: &TempDir,
        current_version: &str,
        settings: Updater,
        release: FetchedRelease,
        calls: Arc<AtomicUsize>,
    ) -> RuntimeUpdateOptions {
        let mut options = RuntimeUpdateOptions::new(
            current_version,
            settings,
            RuntimeStateStore::new(temporary.path().join("argmax/state.toml")),
        );
        options.fetcher = Arc::new(FakeFetcher {
            calls,
            result: Ok(release),
        });
        options.jitter_seed = 0;
        options.startup_jitter_limit = Duration::ZERO;
        options
    }

    fn available(version: &str, kind: ReleaseKind) -> FetchedRelease {
        FetchedRelease {
            version: version.into(),
            kind,
        }
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + TEST_TIMEOUT;
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for update worker"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn discovered_worker_uses_the_embedded_release_version() {
        let options =
            RuntimeUpdateOptions::discover(updater(false, UpdateChannel::Stable)).unwrap();
        assert_eq!(options.current_version.as_ref(), RUNNING_VERSION);
    }

    #[test]
    fn disabled_worker_performs_zero_network_requests() {
        let temporary = TempDir::new().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let worker = RuntimeUpdateWorker::spawn(options(
            &temporary,
            "1.0.0",
            updater(false, UpdateChannel::Stable),
            available("2.0.0", ReleaseKind::Stable),
            Arc::clone(&calls),
        ))
        .unwrap();

        assert_eq!(
            worker.completed_command(),
            CompletedCommandAdmission::Queued
        );
        thread::sleep(Duration::from_millis(50));

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(worker.take_notification().is_none());
        assert!(!worker.status().enabled);
    }

    #[test]
    fn notice_is_persisted_and_waits_for_a_later_completed_command() {
        let temporary = TempDir::new().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let store = RuntimeStateStore::new(temporary.path().join("argmax/state.toml"));
        let worker = RuntimeUpdateWorker::spawn(options(
            &temporary,
            "1.9.0",
            updater(true, UpdateChannel::Stable),
            available("1.10.0", ReleaseKind::Stable),
            Arc::clone(&calls),
        ))
        .unwrap();

        wait_until(|| worker.status().checks_completed == 1);
        assert!(worker.take_notification().is_none());
        assert_eq!(
            worker.completed_command(),
            CompletedCommandAdmission::Queued
        );
        wait_until(|| worker.take_notification().is_some());

        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.state.updater().last_notified_version(),
            Some("1.10.0")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn development_version_and_wrong_channel_never_notify() {
        for (current, channel, remote_kind) in [
            ("dev", UpdateChannel::Stable, ReleaseKind::Stable),
            ("", UpdateChannel::Stable, ReleaseKind::Stable),
            ("1.0.0", UpdateChannel::Stable, ReleaseKind::Nightly),
        ] {
            let temporary = TempDir::new().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let worker = RuntimeUpdateWorker::spawn(options(
                &temporary,
                current,
                updater(true, channel),
                available("2.0.0-nightly.1", remote_kind),
                calls,
            ))
            .unwrap();

            wait_until(|| worker.status().checks_completed == 1);
            let _ = worker.completed_command();
            thread::sleep(Duration::from_millis(20));
            assert!(worker.take_notification().is_none());
        }
    }

    #[test]
    fn persisted_notice_suppresses_other_sessions() {
        let temporary = TempDir::new().unwrap();
        let settings = updater(true, UpdateChannel::Stable);
        let first_calls = Arc::new(AtomicUsize::new(0));
        {
            let first = RuntimeUpdateWorker::spawn(options(
                &temporary,
                "1.0.0",
                settings,
                available("2.0.0", ReleaseKind::Stable),
                first_calls,
            ))
            .unwrap();
            wait_until(|| first.status().checks_completed == 1);
            let _ = first.completed_command();
            wait_until(|| first.take_notification().is_some());
        }

        thread::sleep(settings.check_interval);
        let second_calls = Arc::new(AtomicUsize::new(0));
        let second = RuntimeUpdateWorker::spawn(options(
            &temporary,
            "1.0.0",
            settings,
            available("2.0.0", ReleaseKind::Stable),
            second_calls,
        ))
        .unwrap();
        wait_until(|| second.status().checks_completed == 1);
        let _ = second.completed_command();
        thread::sleep(Duration::from_millis(20));
        assert!(second.take_notification().is_none());
    }

    #[test]
    fn disabled_worker_can_be_enabled_by_atomic_reconfiguration() {
        let temporary = TempDir::new().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let worker = RuntimeUpdateWorker::spawn(options(
            &temporary,
            "1.0.0",
            updater(false, UpdateChannel::Stable),
            available("2.0.0", ReleaseKind::Stable),
            Arc::clone(&calls),
        ))
        .unwrap();

        assert!(worker.reconfigure(updater(true, UpdateChannel::Stable)));
        wait_until(|| worker.status().checks_completed == 1);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn selected_modes_are_coalesced_and_persisted_without_network() {
        let temporary = TempDir::new().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let store = RuntimeStateStore::new(temporary.path().join("argmax/state.toml"));
        let worker = RuntimeUpdateWorker::spawn(options(
            &temporary,
            "1.0.0",
            updater(false, UpdateChannel::Stable),
            available("2.0.0", ReleaseKind::Stable),
            Arc::clone(&calls),
        ))
        .unwrap();

        assert_ne!(
            worker.record_mode(SessionMode::Spec),
            ModePersistenceAdmission::Closed
        );
        assert_ne!(
            worker.record_mode(SessionMode::History),
            ModePersistenceAdmission::Closed
        );
        wait_until(|| {
            store
                .load()
                .is_ok_and(|loaded| loaded.state.last_mode() == Some(LastMode::History))
        });

        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn failures_back_off_exponentially_at_the_configured_interval() {
        let interval = Duration::from_secs(60);
        assert_eq!(retry_delay(interval, 1), interval);
        assert_eq!(retry_delay(interval, 2), Duration::from_secs(120));
        assert_eq!(retry_delay(interval, 3), Duration::from_secs(240));
        assert_eq!(retry_delay(interval, 4), Duration::from_secs(480));
        assert_eq!(retry_delay(interval, 100), Duration::from_secs(480));
    }

    #[test]
    fn interval_scheduling_rounds_fractional_milliseconds_up() {
        let state = UpdateState::new(Some(100), None, None).unwrap();
        assert_eq!(
            remaining_interval(&state, Duration::from_micros(1_500), 101),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn jitter_is_positive_only_and_bounded() {
        let limit = Duration::from_millis(250);
        let mut jitter = Jitter::new(7);
        for _ in 0..100 {
            assert!(jitter.next(limit) <= limit);
        }
        assert_eq!(jitter.next(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn release_errors_collapse_to_silent_policy_categories() {
        assert_eq!(
            map_release_error(ReleaseError::Timeout),
            NetworkFailure::Timeout
        );
        assert_eq!(
            map_release_error(ReleaseError::Network),
            NetworkFailure::Transport
        );
        assert_eq!(
            map_release_error(ReleaseError::RateLimited),
            NetworkFailure::Api
        );
        assert_eq!(
            map_release_error(ReleaseError::InvalidMetadata),
            NetworkFailure::InvalidResponse
        );
    }
}
