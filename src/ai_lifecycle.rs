//! Deterministic lifecycle policy for optional AI completion requests.
//!
//! This module owns no threads, timers, network clients, or terminal state. A
//! caller reports state changes and polls with a monotonic millisecond clock;
//! the returned request permit can then be handed to an asynchronous provider.

use std::error::Error;
use std::fmt;

use crate::ai::validate_candidate;
use crate::config::{
    DEFAULT_AI_CALL_INTERVAL_MS, DEFAULT_AI_DEBOUNCE_MS, DEFAULT_AI_PROVIDER_TIMEOUT_MS,
    MAX_AI_CALL_INTERVAL_MS, MAX_AI_DEBOUNCE_MS, MAX_AI_PROVIDER_TIMEOUT_MS,
    MIN_AI_CALL_INTERVAL_MS, MIN_AI_PROVIDER_TIMEOUT_MS,
};

/// Minimum number of non-surrounding-whitespace characters required for AI.
pub const MIN_INPUT_CHARS: usize = 3;
/// Default quiet period after a query change before a provider request.
pub const DEFAULT_DEBOUNCE_MS: u64 = DEFAULT_AI_DEBOUNCE_MS;
/// Default minimum spacing between provider requests.
pub const DEFAULT_MIN_INTERVAL_MS: u64 = DEFAULT_AI_CALL_INTERVAL_MS;
/// Default provider request timeout.
pub const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = DEFAULT_AI_PROVIDER_TIMEOUT_MS;
/// Lifetime of a compatible validated suggestion.
pub const SUGGESTION_CACHE_TTL_MS: u64 = 30_000;
/// Lifetime of gathered provider-context metadata.
pub const PROVIDER_CONTEXT_CACHE_TTL_MS: u64 = 4_000;
/// Initial fixed cooldown after a provider rate limit.
pub const RATE_LIMIT_COOLDOWN_MS: u64 = 20_000;

/// Validated timing policy for one AI lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleSettings {
    debounce: u64,
    min_interval: u64,
    provider_timeout: u64,
}

impl LifecycleSettings {
    /// Builds settings from bounded millisecond values.
    ///
    /// Bounds are shared with the resolved configuration model. Debounce may be
    /// zero; request spacing and provider timeout must be positive.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError`] when a timing value is outside its documented
    /// inclusive configuration range.
    pub const fn new(
        debounce_ms: u64,
        min_interval_ms: u64,
        provider_timeout_ms: u64,
    ) -> Result<Self, SettingsError> {
        if debounce_ms > MAX_AI_DEBOUNCE_MS {
            return Err(SettingsError::DebounceOutOfRange);
        }
        if min_interval_ms < MIN_AI_CALL_INTERVAL_MS || min_interval_ms > MAX_AI_CALL_INTERVAL_MS {
            return Err(SettingsError::MinimumIntervalOutOfRange);
        }
        if provider_timeout_ms < MIN_AI_PROVIDER_TIMEOUT_MS
            || provider_timeout_ms > MAX_AI_PROVIDER_TIMEOUT_MS
        {
            return Err(SettingsError::ProviderTimeoutOutOfRange);
        }
        Ok(Self {
            debounce: debounce_ms,
            min_interval: min_interval_ms,
            provider_timeout: provider_timeout_ms,
        })
    }

    /// Configured debounce in milliseconds.
    #[must_use]
    pub const fn debounce_ms(self) -> u64 {
        self.debounce
    }

    /// Configured minimum provider-call interval in milliseconds.
    #[must_use]
    pub const fn min_interval_ms(self) -> u64 {
        self.min_interval
    }

    /// Configured provider timeout in milliseconds.
    #[must_use]
    pub const fn provider_timeout_ms(self) -> u64 {
        self.provider_timeout
    }
}

impl Default for LifecycleSettings {
    fn default() -> Self {
        Self {
            debounce: DEFAULT_DEBOUNCE_MS,
            min_interval: DEFAULT_MIN_INTERVAL_MS,
            provider_timeout: DEFAULT_PROVIDER_TIMEOUT_MS,
        }
    }
}

/// Invalid lifecycle timing configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsError {
    /// Debounce lies outside the configuration bounds.
    DebounceOutOfRange,
    /// Provider call spacing lies outside the configuration bounds.
    MinimumIntervalOutOfRange,
    /// Provider timeout lies outside the configuration bounds.
    ProviderTimeoutOutOfRange,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DebounceOutOfRange => "AI debounce is outside the supported range",
            Self::MinimumIntervalOutOfRange => "AI minimum interval is outside the supported range",
            Self::ProviderTimeoutOutOfRange => "AI provider timeout is outside the supported range",
        })
    }
}

impl Error for SettingsError {}

/// Monotonic authority generation for one observed query.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Generation(u64);

impl Generation {
    /// Numeric generation for diagnostics and cross-component correlation.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Caller-defined identity for a shell session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(u64);

impl SessionId {
    /// Creates a session identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Numeric session identifier.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Unique authority token for one started provider request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestToken {
    sequence: u64,
    generation: Generation,
}

impl RequestToken {
    /// Monotonic request sequence within this lifecycle.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Query generation that authorized this request.
    #[must_use]
    pub const fn generation(self) -> Generation {
        self.generation
    }
}

/// Reason pending or in-flight AI work lost authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationReason {
    /// The editable input buffer changed.
    BufferChanged,
    /// The cursor moved away from the end of the input buffer.
    CursorMovedAwayFromEnd,
    /// The interaction mode changed.
    ModeChanged,
    /// Explicit menu navigation froze the user's selection.
    MenuNavigation,
    /// The shell command was executed.
    CommandExecution,
    /// AI completion was disabled.
    Disabled,
    /// The selected provider or its configuration changed.
    ProviderChanged,
    /// The active shell session changed without first exiting.
    SessionChanged,
    /// The active shell session exited.
    SessionExit,
}

/// Audit metadata for the most recent cancellation of active work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cancellation {
    reason: CancellationReason,
    generation: Generation,
    token: Option<RequestToken>,
    at_ms: u64,
}

impl Cancellation {
    /// Cancellation cause.
    #[must_use]
    pub const fn reason(self) -> CancellationReason {
        self.reason
    }

    /// Invalidated query generation.
    #[must_use]
    pub const fn generation(self) -> Generation {
        self.generation
    }

    /// Invalidated request token, if work had already started.
    #[must_use]
    pub const fn token(self) -> Option<RequestToken> {
        self.token
    }

    /// Caller-supplied cancellation time.
    #[must_use]
    pub const fn at_ms(self) -> u64 {
        self.at_ms
    }
}

/// Why an observed buffer cannot issue an AI request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryIneligibility {
    /// AI is disabled.
    Disabled,
    /// No non-empty provider name is selected.
    ProviderMissing,
    /// No active shell session exists.
    SessionInactive,
    /// Fewer than three trimmed characters were entered.
    InputTooShort,
    /// The cursor is not exactly at the end of the buffer.
    CursorNotAtEnd,
    /// The buffer contains control characters, such as a multi-line command.
    ControlCharacters,
}

/// Immutable snapshot of the currently observed shell input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuerySnapshot {
    buffer: String,
    cursor: usize,
    generation: Generation,
    observed_at_ms: u64,
    eligible: bool,
}

impl QuerySnapshot {
    /// Exact editable shell buffer.
    #[must_use]
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Cursor byte offset supplied by the session layer.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Authority generation for this snapshot.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Time at which this exact buffer and cursor were observed.
    #[must_use]
    pub const fn observed_at_ms(&self) -> u64 {
        self.observed_at_ms
    }

    /// Whether all eligibility conditions held when the snapshot was observed.
    #[must_use]
    pub const fn is_eligible(&self) -> bool {
        self.eligible
    }
}

/// Result of observing a buffer and cursor pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryObservation {
    /// An eligible query awaits its debounce and other timing gates.
    Scheduled {
        /// New query authority generation.
        generation: Generation,
        /// Earliest time at which the debounce gate opens.
        debounce_until_ms: u64,
    },
    /// The query was retained for inspection but cannot issue a request.
    Ineligible {
        /// New query authority generation.
        generation: Generation,
        /// Failed eligibility condition.
        reason: QueryIneligibility,
    },
    /// The exact buffer and cursor pair was already current.
    Unchanged {
        /// Existing query authority generation.
        generation: Generation,
        /// Whether that query is eligible.
        eligible: bool,
    },
    /// No new authority can be issued after exhausting the generation counter.
    CounterExhausted,
}

/// Permission to start exactly one provider request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestPermit {
    token: RequestToken,
    buffer: String,
    provider: String,
    session: SessionId,
    started_at_ms: u64,
    deadline_at_ms: u64,
}

impl RequestPermit {
    /// Token that must accompany the eventual response.
    #[must_use]
    pub const fn token(&self) -> RequestToken {
        self.token
    }

    /// Exact input buffer authorized for disclosure to the provider.
    #[must_use]
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Provider selected when the request started.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Shell session that authorized the request.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Provider-call start time.
    #[must_use]
    pub const fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    /// Time at or after which the response loses authority.
    #[must_use]
    pub const fn deadline_at_ms(&self) -> u64 {
        self.deadline_at_ms
    }
}

/// One validated suggestion retained for compatible prefix queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedSuggestion {
    requested_input: String,
    completion: String,
    provider: String,
    session: SessionId,
    stored_at_ms: u64,
    expires_at_ms: u64,
}

impl CachedSuggestion {
    /// Input that originally produced this candidate.
    #[must_use]
    pub fn requested_input(&self) -> &str {
        &self.requested_input
    }

    /// Full validated shell-line completion.
    #[must_use]
    pub fn completion(&self) -> &str {
        &self.completion
    }

    /// Provider that produced the completion.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Session in which the completion was produced.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Time at which the candidate was stored.
    #[must_use]
    pub const fn stored_at_ms(&self) -> u64 {
        self.stored_at_ms
    }

    /// Exclusive expiry boundary.
    #[must_use]
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// Tests exact-prefix compatibility and cache scope.
    #[must_use]
    pub fn is_compatible(
        &self,
        buffer: &str,
        cursor: usize,
        provider: &str,
        session: SessionId,
        now_ms: u64,
    ) -> bool {
        now_ms < self.expires_at_ms
            && cursor == buffer.len()
            && buffer.trim().chars().count() >= MIN_INPUT_CHARS
            && self.provider == provider
            && self.session == session
            && self.completion != buffer
            && self.completion.starts_with(buffer)
    }
}

/// Metadata for a gathered provider-context cache entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextCacheMetadata {
    key: u64,
    provider: String,
    session: SessionId,
    gathered_at_ms: u64,
    expires_at_ms: u64,
}

impl ContextCacheMetadata {
    /// Caller-defined fingerprint of the gathered context inputs.
    #[must_use]
    pub const fn key(&self) -> u64 {
        self.key
    }

    /// Provider for which context was gathered.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Session for which context was gathered.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Context gathering time.
    #[must_use]
    pub const fn gathered_at_ms(&self) -> u64 {
        self.gathered_at_ms
    }

    /// Exclusive expiry boundary.
    #[must_use]
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }
}

/// Active provider cooldown after rate limiting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cooldown {
    provider: String,
    started_at_ms: u64,
    retry_at_ms: u64,
}

impl Cooldown {
    /// Rate-limited provider.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Time at which rate limiting was observed.
    #[must_use]
    pub const fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    /// Earliest permitted retry time.
    #[must_use]
    pub const fn retry_at_ms(&self) -> u64 {
        self.retry_at_ms
    }
}

/// Non-blocking result of polling the lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecyclePoll {
    /// No request or cache delivery is pending.
    Idle,
    /// The current query is still inside its quiet period.
    Debouncing {
        /// Earliest time at which this gate opens.
        ready_at_ms: u64,
    },
    /// A previous provider call still enforces request spacing.
    MinimumInterval {
        /// Earliest time at which this gate opens.
        ready_at_ms: u64,
    },
    /// Rate limiting temporarily prevents provider calls.
    Cooldown {
        /// Earliest time at which the provider may be retried.
        retry_at_ms: u64,
    },
    /// A request has started and has not yet reached its deadline.
    InFlight {
        /// Active request authority.
        token: RequestToken,
        /// Exclusive response deadline.
        deadline_at_ms: u64,
    },
    /// The caller should start this provider request asynchronously.
    Start(RequestPermit),
    /// A compatible unexpired suggestion avoids a provider request.
    Cached(CachedSuggestion),
    /// The active provider request reached its deadline.
    TimedOut(RequestToken),
    /// No new authority can be issued after a counter is exhausted.
    CounterExhausted,
}

/// Provider outcome reported without performing any provider work here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderOutcome {
    /// Decoded provider text to pass through the AI output-validation boundary.
    Suggestion(String),
    /// The provider returned no usable candidate.
    NoSuggestion,
    /// The provider failed without rate limiting.
    Failed,
    /// The provider reported a rate limit.
    RateLimited,
}

/// Effect of reporting a provider outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseDisposition {
    /// A compatible suggestion was cached through this exclusive boundary.
    Accepted {
        /// Exclusive cache expiry time.
        expires_at_ms: u64,
    },
    /// The candidate did not preserve the authorized input prefix.
    IncompatibleSuggestion,
    /// The request completed without a candidate.
    NoSuggestion,
    /// The request failed without entering cooldown.
    Failed,
    /// The provider entered its fixed initial cooldown.
    RateLimited {
        /// Earliest provider retry time.
        retry_at_ms: u64,
    },
    /// The response arrived at or after its timeout deadline.
    TimedOut,
    /// The token no longer owns the current request authority.
    Stale,
}

/// Pure state machine for optional AI request scheduling and authority.
#[derive(Debug)]
pub struct Lifecycle {
    settings: LifecycleSettings,
    enabled: bool,
    provider: Option<String>,
    session: Option<SessionId>,
    generation: Generation,
    next_request_sequence: u64,
    counter_exhausted: bool,
    current_query: Option<QuerySnapshot>,
    attempted_generation: Option<Generation>,
    in_flight: Option<RequestPermit>,
    last_started_at_ms: Option<u64>,
    last_cancellation: Option<Cancellation>,
    suggestion_cache: Option<CachedSuggestion>,
    context_cache: Option<ContextCacheMetadata>,
    cooldown: Option<Cooldown>,
}

impl Lifecycle {
    /// Creates a disabled lifecycle with no provider or active session.
    #[must_use]
    pub const fn new(settings: LifecycleSettings) -> Self {
        Self {
            settings,
            enabled: false,
            provider: None,
            session: None,
            generation: Generation(0),
            next_request_sequence: 0,
            counter_exhausted: false,
            current_query: None,
            attempted_generation: None,
            in_flight: None,
            last_started_at_ms: None,
            last_cancellation: None,
            suggestion_cache: None,
            context_cache: None,
            cooldown: None,
        }
    }

    /// Validated timing settings.
    #[must_use]
    pub const fn settings(&self) -> LifecycleSettings {
        self.settings
    }

    /// Whether AI requests are enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Selected provider name, if configured.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Active shell session, if any.
    #[must_use]
    pub const fn session(&self) -> Option<SessionId> {
        self.session
    }

    /// Latest authority generation, including invalidations.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Currently observed buffer and cursor.
    #[must_use]
    pub const fn current_query(&self) -> Option<&QuerySnapshot> {
        self.current_query.as_ref()
    }

    /// Active provider request, if any.
    #[must_use]
    pub const fn in_flight(&self) -> Option<&RequestPermit> {
        self.in_flight.as_ref()
    }

    /// Most recent provider-call start time.
    #[must_use]
    pub const fn last_started_at_ms(&self) -> Option<u64> {
        self.last_started_at_ms
    }

    /// Most recent cancellation of pending or in-flight work.
    #[must_use]
    pub const fn last_cancellation(&self) -> Option<Cancellation> {
        self.last_cancellation
    }

    /// Stored suggestion cache metadata, which may require lazy expiry.
    #[must_use]
    pub const fn suggestion_cache(&self) -> Option<&CachedSuggestion> {
        self.suggestion_cache.as_ref()
    }

    /// Stored provider-context metadata, which may require lazy expiry.
    #[must_use]
    pub const fn context_cache_metadata(&self) -> Option<&ContextCacheMetadata> {
        self.context_cache.as_ref()
    }

    /// Active rate-limit cooldown, which may require lazy expiry.
    #[must_use]
    pub const fn cooldown(&self) -> Option<&Cooldown> {
        self.cooldown.as_ref()
    }

    /// Enables or disables provider requests.
    ///
    /// Disabling cancels active work and clears both in-memory AI caches.
    pub fn set_enabled(&mut self, enabled: bool, now_ms: u64) -> Option<Cancellation> {
        if self.enabled == enabled {
            return None;
        }
        self.enabled = enabled;
        if enabled {
            self.invalidate(None, now_ms, false, false)
        } else {
            self.invalidate(Some(CancellationReason::Disabled), now_ms, true, true)
        }
    }

    /// Selects a provider by name, or removes the provider with `None`.
    ///
    /// Empty and whitespace-only names are treated as missing. A changed provider
    /// cancels active work and invalidates provider-scoped caches and cooldown.
    pub fn set_provider(&mut self, provider: Option<String>, now_ms: u64) -> Option<Cancellation> {
        let provider = provider.filter(|name| !name.trim().is_empty());
        if self.provider == provider {
            return None;
        }
        let cancellation = self.invalidate(
            Some(CancellationReason::ProviderChanged),
            now_ms,
            true,
            true,
        );
        self.provider = provider;
        self.cooldown = None;
        cancellation
    }

    /// Invalidates a provider whose configuration changed without changing name.
    pub fn provider_configuration_changed(&mut self, now_ms: u64) -> Option<Cancellation> {
        let cancellation = self.invalidate(
            Some(CancellationReason::ProviderChanged),
            now_ms,
            true,
            true,
        );
        self.cooldown = None;
        cancellation
    }

    /// Starts or switches to a shell session.
    pub fn start_session(&mut self, session: SessionId, now_ms: u64) -> Option<Cancellation> {
        if self.session == Some(session) {
            return None;
        }
        let reason = self.session.map(|_| CancellationReason::SessionChanged);
        let cancellation = self.invalidate(reason, now_ms, true, true);
        self.session = Some(session);
        cancellation
    }

    /// Ends the active shell session and cancels all active work.
    pub fn end_session(&mut self, now_ms: u64) -> Option<Cancellation> {
        self.session?;
        let cancellation =
            self.invalidate(Some(CancellationReason::SessionExit), now_ms, true, true);
        self.session = None;
        cancellation
    }

    /// Cancels active work because the interaction mode changed.
    pub fn mode_changed(&mut self, now_ms: u64) -> Option<Cancellation> {
        self.invalidate(Some(CancellationReason::ModeChanged), now_ms, true, false)
    }

    /// Cancels active work because the user explicitly navigated the menu.
    pub fn menu_navigated(&mut self, now_ms: u64) -> Option<Cancellation> {
        self.invalidate(
            Some(CancellationReason::MenuNavigation),
            now_ms,
            true,
            false,
        )
    }

    /// Cancels active work because the shell command executed.
    pub fn command_executed(&mut self, now_ms: u64) -> Option<Cancellation> {
        self.invalidate(
            Some(CancellationReason::CommandExecution),
            now_ms,
            true,
            true,
        )
    }

    /// Observes the complete editable buffer and cursor byte offset.
    ///
    /// A changed buffer or cursor-away-from-end cancels prior work. Eligible
    /// observations become request candidates but do not start work until
    /// [`Self::poll`] opens all timing gates.
    pub fn observe_query(
        &mut self,
        buffer: impl Into<String>,
        cursor: usize,
        now_ms: u64,
    ) -> QueryObservation {
        let buffer = buffer.into();
        if let Some(current) = &self.current_query {
            if current.buffer == buffer && current.cursor == cursor {
                return QueryObservation::Unchanged {
                    generation: current.generation,
                    eligible: current.eligible,
                };
            }

            let reason = if current.buffer != buffer {
                Some(CancellationReason::BufferChanged)
            } else if cursor != buffer.len() {
                Some(CancellationReason::CursorMovedAwayFromEnd)
            } else {
                None
            };
            self.invalidate(reason, now_ms, false, false);
        } else {
            self.advance_generation();
        }

        if self.counter_exhausted {
            return QueryObservation::CounterExhausted;
        }

        let reason = self.query_ineligibility(&buffer, cursor);
        let generation = self.generation;
        self.current_query = Some(QuerySnapshot {
            buffer,
            cursor,
            generation,
            observed_at_ms: now_ms,
            eligible: reason.is_none(),
        });
        self.attempted_generation = None;

        if let Some(reason) = reason {
            QueryObservation::Ineligible { generation, reason }
        } else {
            QueryObservation::Scheduled {
                generation,
                debounce_until_ms: add_ms(now_ms, self.settings.debounce),
            }
        }
    }

    /// Polls timing and authority without waiting or performing provider work.
    pub fn poll(&mut self, now_ms: u64) -> LifecyclePoll {
        self.expire_transient_state(now_ms);

        if let Some(request) = &self.in_flight {
            let token = request.token;
            let deadline_at_ms = request.deadline_at_ms;
            if now_ms >= deadline_at_ms {
                self.in_flight = None;
                return LifecyclePoll::TimedOut(token);
            }
            return LifecyclePoll::InFlight {
                token,
                deadline_at_ms,
            };
        }

        let Some(query) = self.current_query.clone() else {
            return LifecyclePoll::Idle;
        };
        if !query.eligible || self.attempted_generation == Some(query.generation) {
            return LifecyclePoll::Idle;
        }
        if self.counter_exhausted {
            return LifecyclePoll::CounterExhausted;
        }

        if let Some(cached) = self.compatible_cache_for(&query, now_ms) {
            self.attempted_generation = Some(query.generation);
            return LifecyclePoll::Cached(cached);
        }

        let debounce_until_ms = add_ms(query.observed_at_ms, self.settings.debounce);
        if now_ms < debounce_until_ms {
            return LifecyclePoll::Debouncing {
                ready_at_ms: debounce_until_ms,
            };
        }

        if let Some(cooldown) = &self.cooldown {
            if now_ms < cooldown.retry_at_ms {
                return LifecyclePoll::Cooldown {
                    retry_at_ms: cooldown.retry_at_ms,
                };
            }
        }

        if let Some(last_started_at_ms) = self.last_started_at_ms {
            let ready_at_ms = add_ms(last_started_at_ms, self.settings.min_interval);
            if now_ms < ready_at_ms {
                return LifecyclePoll::MinimumInterval { ready_at_ms };
            }
        }

        let Some(sequence) = self.next_request_sequence.checked_add(1) else {
            self.counter_exhausted = true;
            self.current_query = None;
            return LifecyclePoll::CounterExhausted;
        };
        let Some(provider) = self.provider.clone() else {
            return LifecyclePoll::Idle;
        };
        let Some(session) = self.session else {
            return LifecyclePoll::Idle;
        };

        self.next_request_sequence = sequence;
        let token = RequestToken {
            sequence,
            generation: query.generation,
        };
        let request = RequestPermit {
            token,
            buffer: query.buffer,
            provider,
            session,
            started_at_ms: now_ms,
            deadline_at_ms: add_ms(now_ms, self.settings.provider_timeout),
        };
        self.attempted_generation = Some(query.generation);
        self.last_started_at_ms = Some(now_ms);
        self.in_flight = Some(request.clone());
        LifecyclePoll::Start(request)
    }

    /// Applies a provider outcome only when the token still owns authority.
    pub fn complete_request(
        &mut self,
        token: RequestToken,
        now_ms: u64,
        outcome: ProviderOutcome,
    ) -> ResponseDisposition {
        let Some(request) = self.in_flight.as_ref() else {
            return ResponseDisposition::Stale;
        };
        if request.token != token || token.generation != self.generation {
            return ResponseDisposition::Stale;
        }
        if now_ms >= request.deadline_at_ms {
            let provider = request.provider.clone();
            self.in_flight = None;
            // A rate limit is a fact about the provider, not about this
            // request, so it still applies when the answer arrives past the
            // deadline. Discarding it as merely late would keep requests going
            // out at the minimum interval against a provider actively refusing
            // them, which is the case the cooldown exists for.
            if matches!(outcome, ProviderOutcome::RateLimited) {
                self.cooldown = Some(Cooldown {
                    provider,
                    started_at_ms: now_ms,
                    retry_at_ms: add_ms(now_ms, RATE_LIMIT_COOLDOWN_MS),
                });
            }
            return ResponseDisposition::TimedOut;
        }

        let Some(request) = self.in_flight.take() else {
            return ResponseDisposition::Stale;
        };
        match outcome {
            ProviderOutcome::Suggestion(completion) => {
                let Ok(completion) = validate_candidate(&request.buffer, &completion) else {
                    return ResponseDisposition::IncompatibleSuggestion;
                };
                let expires_at_ms = add_ms(now_ms, SUGGESTION_CACHE_TTL_MS);
                self.suggestion_cache = Some(CachedSuggestion {
                    requested_input: request.buffer,
                    completion,
                    provider: request.provider,
                    session: request.session,
                    stored_at_ms: now_ms,
                    expires_at_ms,
                });
                ResponseDisposition::Accepted { expires_at_ms }
            }
            ProviderOutcome::NoSuggestion => ResponseDisposition::NoSuggestion,
            ProviderOutcome::Failed => ResponseDisposition::Failed,
            ProviderOutcome::RateLimited => {
                let retry_at_ms = add_ms(now_ms, RATE_LIMIT_COOLDOWN_MS);
                self.cooldown = Some(Cooldown {
                    provider: request.provider,
                    started_at_ms: now_ms,
                    retry_at_ms,
                });
                ResponseDisposition::RateLimited { retry_at_ms }
            }
        }
    }

    /// Returns a scoped compatible cache entry without marking a query handled.
    pub fn compatible_cached_suggestion(
        &mut self,
        buffer: &str,
        cursor: usize,
        now_ms: u64,
    ) -> Option<CachedSuggestion> {
        self.expire_transient_state(now_ms);
        if !self.enabled {
            return None;
        }
        let provider = self.provider.as_deref()?;
        let session = self.session?;
        self.suggestion_cache
            .as_ref()
            .filter(|cached| cached.is_compatible(buffer, cursor, provider, session, now_ms))
            .cloned()
    }

    /// Records provider-context metadata for the current provider and session.
    ///
    /// Returns `None` while AI is disabled or its provider/session scope is
    /// incomplete, preventing context gathering from being treated as usable.
    pub fn record_provider_context(
        &mut self,
        key: u64,
        now_ms: u64,
    ) -> Option<&ContextCacheMetadata> {
        if !self.enabled {
            return None;
        }
        let provider = self.provider.clone()?;
        let session = self.session?;
        self.context_cache = Some(ContextCacheMetadata {
            key,
            provider,
            session,
            gathered_at_ms: now_ms,
            expires_at_ms: add_ms(now_ms, PROVIDER_CONTEXT_CACHE_TTL_MS),
        });
        self.context_cache.as_ref()
    }

    /// Returns fresh provider-context metadata for an exact scope and key.
    pub fn fresh_provider_context(
        &mut self,
        key: u64,
        now_ms: u64,
    ) -> Option<&ContextCacheMetadata> {
        self.expire_transient_state(now_ms);
        if !self.enabled {
            return None;
        }
        let provider = self.provider.as_deref()?;
        let session = self.session?;
        self.context_cache.as_ref().filter(|metadata| {
            metadata.key == key
                && metadata.provider == provider
                && metadata.session == session
                && now_ms < metadata.expires_at_ms
        })
    }

    fn query_ineligibility(&self, buffer: &str, cursor: usize) -> Option<QueryIneligibility> {
        if !self.enabled {
            return Some(QueryIneligibility::Disabled);
        }
        if self.provider.is_none() {
            return Some(QueryIneligibility::ProviderMissing);
        }
        if self.session.is_none() {
            return Some(QueryIneligibility::SessionInactive);
        }
        if buffer.trim().chars().count() < MIN_INPUT_CHARS {
            return Some(QueryIneligibility::InputTooShort);
        }
        if cursor != buffer.len() {
            return Some(QueryIneligibility::CursorNotAtEnd);
        }
        // A restored multi-line command is a legitimate buffer, but the
        // prompt input contract rejects control characters; skipping the
        // request beats surfacing a provider failure for every keystroke.
        if buffer.chars().any(char::is_control) {
            return Some(QueryIneligibility::ControlCharacters);
        }
        None
    }

    fn compatible_cache_for(&self, query: &QuerySnapshot, now_ms: u64) -> Option<CachedSuggestion> {
        let provider = self.provider.as_deref()?;
        let session = self.session?;
        self.suggestion_cache
            .as_ref()
            .filter(|cached| {
                cached.is_compatible(&query.buffer, query.cursor, provider, session, now_ms)
            })
            .cloned()
    }

    fn invalidate(
        &mut self,
        reason: Option<CancellationReason>,
        now_ms: u64,
        clear_suggestion: bool,
        clear_context: bool,
    ) -> Option<Cancellation> {
        let active_generation = self.in_flight.as_ref().map_or_else(
            || self.current_query.as_ref().map(QuerySnapshot::generation),
            |request| Some(request.token.generation),
        );
        let has_pending = self.current_query.as_ref().is_some_and(|query| {
            query.eligible && self.attempted_generation != Some(query.generation)
        });
        let has_active_work = self.in_flight.is_some() || has_pending;
        let cancellation = reason.and_then(|reason| {
            has_active_work.then_some(Cancellation {
                reason,
                generation: active_generation.unwrap_or(self.generation),
                token: self.in_flight.as_ref().map(|request| request.token),
                at_ms: now_ms,
            })
        });
        if let Some(cancellation) = cancellation {
            self.last_cancellation = Some(cancellation);
        }

        self.current_query = None;
        self.attempted_generation = None;
        self.in_flight = None;
        if clear_suggestion {
            self.suggestion_cache = None;
        }
        if clear_context {
            self.context_cache = None;
        }
        self.advance_generation();
        cancellation
    }

    fn advance_generation(&mut self) {
        let Some(value) = self.generation.0.checked_add(1) else {
            self.counter_exhausted = true;
            self.current_query = None;
            self.in_flight = None;
            return;
        };
        self.generation = Generation(value);
    }

    fn expire_transient_state(&mut self, now_ms: u64) {
        if self
            .suggestion_cache
            .as_ref()
            .is_some_and(|cached| now_ms >= cached.expires_at_ms)
        {
            self.suggestion_cache = None;
        }
        if self
            .context_cache
            .as_ref()
            .is_some_and(|metadata| now_ms >= metadata.expires_at_ms)
        {
            self.context_cache = None;
        }
        if self
            .cooldown
            .as_ref()
            .is_some_and(|cooldown| now_ms >= cooldown.retry_at_ms)
        {
            self.cooldown = None;
        }
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new(LifecycleSettings::default())
    }
}

const fn add_ms(timestamp_ms: u64, duration_ms: u64) -> u64 {
    timestamp_ms.saturating_add(duration_ms)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn ready_lifecycle(settings: LifecycleSettings) -> Lifecycle {
        let mut lifecycle = Lifecycle::new(settings);
        lifecycle.set_provider(Some("openai".to_owned()), 0);
        lifecycle.start_session(SessionId::new(1), 0);
        lifecycle.set_enabled(true, 0);
        lifecycle
    }

    fn start_request(lifecycle: &mut Lifecycle, buffer: &str, now_ms: u64) -> RequestPermit {
        let observation = lifecycle.observe_query(buffer, buffer.len(), now_ms);
        assert!(matches!(observation, QueryObservation::Scheduled { .. }));
        let start_at_ms = add_ms(now_ms, lifecycle.settings().debounce_ms());
        let LifecyclePoll::Start(request) = lifecycle.poll(start_at_ms) else {
            panic!("request did not start")
        };
        request
    }

    #[test]
    fn settings_defaults_and_validation_are_explicit() {
        let defaults = LifecycleSettings::default();
        assert_eq!(defaults.debounce_ms(), 500);
        assert_eq!(defaults.min_interval_ms(), 1_000);
        assert_eq!(defaults.provider_timeout_ms(), 2_000);

        let cases = BTreeMap::from([
            (
                "positive values",
                (10, 20, 30, Ok(LifecycleSettings::new(10, 20, 30).unwrap())),
            ),
            (
                "zero debounce",
                (0, 20, 30, Ok(LifecycleSettings::new(0, 20, 30).unwrap())),
            ),
            (
                "zero interval",
                (10, 0, 30, Err(SettingsError::MinimumIntervalOutOfRange)),
            ),
            (
                "zero timeout",
                (10, 20, 0, Err(SettingsError::ProviderTimeoutOutOfRange)),
            ),
            (
                "excessive debounce",
                (
                    MAX_AI_DEBOUNCE_MS + 1,
                    20,
                    30,
                    Err(SettingsError::DebounceOutOfRange),
                ),
            ),
        ]);
        for (name, (debounce, interval, timeout, want)) in cases {
            assert_eq!(
                LifecycleSettings::new(debounce, interval, timeout),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn eligibility_is_deterministic_at_every_boundary() {
        #[derive(Clone, Copy)]
        enum Setup {
            Disabled,
            MissingProvider,
            MissingSession,
            Ready,
        }

        let cases = BTreeMap::from([
            (
                "cursor not at end",
                (Setup::Ready, "git", 2, QueryIneligibility::CursorNotAtEnd),
            ),
            (
                "disabled",
                (Setup::Disabled, "git", 3, QueryIneligibility::Disabled),
            ),
            (
                "missing provider",
                (
                    Setup::MissingProvider,
                    "git",
                    3,
                    QueryIneligibility::ProviderMissing,
                ),
            ),
            (
                "missing session",
                (
                    Setup::MissingSession,
                    "git",
                    3,
                    QueryIneligibility::SessionInactive,
                ),
            ),
            (
                "trimmed input too short",
                (Setup::Ready, " g ", 3, QueryIneligibility::InputTooShort),
            ),
            (
                "multi-line buffer",
                (
                    Setup::Ready,
                    "printf 'one\ntwo'",
                    16,
                    QueryIneligibility::ControlCharacters,
                ),
            ),
        ]);

        for (name, (setup, buffer, cursor, want)) in cases {
            let mut lifecycle = Lifecycle::default();
            match setup {
                Setup::Disabled => {
                    lifecycle.set_provider(Some("openai".to_owned()), 0);
                    lifecycle.start_session(SessionId::new(1), 0);
                }
                Setup::MissingProvider => {
                    lifecycle.start_session(SessionId::new(1), 0);
                    lifecycle.set_enabled(true, 0);
                }
                Setup::MissingSession => {
                    lifecycle.set_provider(Some("openai".to_owned()), 0);
                    lifecycle.set_enabled(true, 0);
                }
                Setup::Ready => lifecycle = ready_lifecycle(LifecycleSettings::default()),
            }
            assert!(
                matches!(
                    lifecycle.observe_query(buffer, cursor, 10),
                    QueryObservation::Ineligible { reason, .. } if reason == want
                ),
                "{name}"
            );
            assert_eq!(lifecycle.poll(10_000), LifecyclePoll::Idle, "{name}");
        }

        let mut lifecycle = ready_lifecycle(LifecycleSettings::default());
        assert!(matches!(
            lifecycle.observe_query("  git  ", 7, 10),
            QueryObservation::Scheduled { .. }
        ));
    }

    #[test]
    fn debounce_spacing_and_timeout_use_exact_boundaries() {
        let mut lifecycle = ready_lifecycle(LifecycleSettings::default());
        lifecycle.observe_query("git", 3, 0);
        assert_eq!(
            lifecycle.poll(499),
            LifecyclePoll::Debouncing { ready_at_ms: 500 }
        );
        let LifecyclePoll::Start(first) = lifecycle.poll(500) else {
            panic!("first request did not start")
        };
        assert_eq!(first.started_at_ms(), 500);
        assert_eq!(first.deadline_at_ms(), 2_500);
        assert_eq!(first.token().sequence(), 1);
        assert_eq!(
            lifecycle.complete_request(first.token(), 600, ProviderOutcome::NoSuggestion),
            ResponseDisposition::NoSuggestion
        );

        lifecycle.observe_query("git s", 5, 600);
        assert_eq!(
            lifecycle.poll(1_100),
            LifecyclePoll::MinimumInterval { ready_at_ms: 1_500 }
        );
        let LifecyclePoll::Start(second) = lifecycle.poll(1_500) else {
            panic!("second request did not start")
        };
        assert!(second.token().sequence() > first.token().sequence());
        assert!(second.token().generation() > first.token().generation());
        assert!(matches!(
            lifecycle.poll(3_499),
            LifecyclePoll::InFlight { .. }
        ));
        assert_eq!(
            lifecycle.poll(3_500),
            LifecyclePoll::TimedOut(second.token())
        );
        assert_eq!(
            lifecycle.complete_request(second.token(), 3_500, ProviderOutcome::Failed),
            ResponseDisposition::Stale
        );
    }

    #[test]
    fn compatible_suggestions_expire_at_thirty_seconds() {
        let settings = LifecycleSettings::new(0, 1, 2_000).unwrap();
        let mut lifecycle = ready_lifecycle(settings);
        let request = start_request(&mut lifecycle, "git", 100);
        assert_eq!(
            lifecycle.complete_request(
                request.token(),
                200,
                ProviderOutcome::Suggestion("git status --short".to_owned())
            ),
            ResponseDisposition::Accepted {
                expires_at_ms: 30_200
            }
        );

        let cases = BTreeMap::from([
            ("01 cursor away", ("git s", 4, 1_000, false)),
            (
                "02 exact completion",
                ("git status --short", 18, 1_000, false),
            ),
            ("03 non-prefix", ("git log", 7, 1_000, false)),
            ("04 short prefix", ("gi", 2, 1_000, false)),
            ("05 fresh prefix", ("git s", 5, 30_199, true)),
        ]);
        for (name, (buffer, cursor, now_ms, want_some)) in cases {
            assert_eq!(
                lifecycle
                    .compatible_cached_suggestion(buffer, cursor, now_ms)
                    .is_some(),
                want_some,
                "{name}"
            );
        }
        assert!(
            lifecycle
                .compatible_cached_suggestion("git st", 6, 30_200)
                .is_none()
        );
        assert!(lifecycle.suggestion_cache().is_none());
    }

    #[test]
    fn polling_delivers_a_compatible_cache_without_a_provider_call() {
        let settings = LifecycleSettings::new(500, 1, 2_000).unwrap();
        let mut lifecycle = ready_lifecycle(settings);
        let request = start_request(&mut lifecycle, "git", 0);
        lifecycle.complete_request(
            request.token(),
            600,
            ProviderOutcome::Suggestion("git status".to_owned()),
        );

        lifecycle.observe_query("git s", 5, 700);
        let LifecyclePoll::Cached(cached) = lifecycle.poll(700) else {
            panic!("compatible cache was not delivered")
        };
        assert_eq!(cached.completion(), "git status");
        assert_eq!(lifecycle.poll(20_000), LifecyclePoll::Idle);
        assert_eq!(lifecycle.last_started_at_ms(), Some(500));
    }

    #[test]
    fn provider_context_metadata_expires_at_four_seconds() {
        let mut lifecycle = ready_lifecycle(LifecycleSettings::default());
        let metadata = lifecycle.record_provider_context(42, 1_000).unwrap();
        assert_eq!(metadata.gathered_at_ms(), 1_000);
        assert_eq!(metadata.expires_at_ms(), 5_000);
        assert!(lifecycle.fresh_provider_context(7, 4_999).is_none());
        assert!(lifecycle.fresh_provider_context(42, 4_999).is_some());
        assert!(lifecycle.fresh_provider_context(42, 5_000).is_none());
        assert!(lifecycle.context_cache_metadata().is_none());
    }

    #[test]
    fn a_rate_limit_arriving_after_the_deadline_still_applies_its_cooldown() {
        let settings = LifecycleSettings::new(0, 1, 2_000).unwrap();
        let mut lifecycle = ready_lifecycle(settings);
        let request = start_request(&mut lifecycle, "git", 100);
        let late = 100 + 2_000;

        assert_eq!(
            lifecycle.complete_request(request.token(), late, ProviderOutcome::RateLimited),
            ResponseDisposition::TimedOut
        );

        lifecycle.observe_query("git s", 5, late + 1);
        assert_eq!(
            lifecycle.poll(late + 1),
            LifecyclePoll::Cooldown {
                retry_at_ms: late + 20_000
            }
        );
    }

    #[test]
    fn rate_limits_apply_a_fixed_twenty_second_cooldown() {
        let settings = LifecycleSettings::new(0, 1, 2_000).unwrap();
        let mut lifecycle = ready_lifecycle(settings);
        let request = start_request(&mut lifecycle, "git", 100);
        assert_eq!(
            lifecycle.complete_request(request.token(), 200, ProviderOutcome::RateLimited),
            ResponseDisposition::RateLimited {
                retry_at_ms: 20_200
            }
        );
        lifecycle.observe_query("git s", 5, 300);
        assert_eq!(
            lifecycle.poll(20_199),
            LifecyclePoll::Cooldown {
                retry_at_ms: 20_200
            }
        );
        assert!(matches!(lifecycle.poll(20_200), LifecyclePoll::Start(_)));
        assert!(lifecycle.cooldown().is_none());
    }

    #[test]
    fn provider_outcomes_have_exhaustive_authority_effects() {
        let cases = BTreeMap::from([
            (
                "compatible suggestion",
                (
                    ProviderOutcome::Suggestion("git status".to_owned()),
                    ResponseDisposition::Accepted {
                        expires_at_ms: 30_001,
                    },
                ),
            ),
            (
                "failed",
                (ProviderOutcome::Failed, ResponseDisposition::Failed),
            ),
            (
                "fenced suggestion",
                (
                    ProviderOutcome::Suggestion("```sh\ngit status\n```".to_owned()),
                    ResponseDisposition::Accepted {
                        expires_at_ms: 30_001,
                    },
                ),
            ),
            (
                "identical suggestion",
                (
                    ProviderOutcome::Suggestion("git".to_owned()),
                    ResponseDisposition::IncompatibleSuggestion,
                ),
            ),
            (
                "multi-line suggestion",
                (
                    ProviderOutcome::Suggestion("git status\ngit push".to_owned()),
                    ResponseDisposition::IncompatibleSuggestion,
                ),
            ),
            (
                "no suggestion",
                (
                    ProviderOutcome::NoSuggestion,
                    ResponseDisposition::NoSuggestion,
                ),
            ),
            (
                "rate limited",
                (
                    ProviderOutcome::RateLimited,
                    ResponseDisposition::RateLimited {
                        retry_at_ms: 20_001,
                    },
                ),
            ),
            (
                "wrong prefix",
                (
                    ProviderOutcome::Suggestion("just study".to_owned()),
                    ResponseDisposition::IncompatibleSuggestion,
                ),
            ),
        ]);

        for (name, (outcome, want)) in cases {
            let settings = LifecycleSettings::new(0, 1, 2_000).unwrap();
            let mut lifecycle = ready_lifecycle(settings);
            let request = start_request(&mut lifecycle, "git", 0);
            assert_eq!(
                lifecycle.complete_request(request.token(), 1, outcome),
                want,
                "{name}"
            );
            assert!(lifecycle.in_flight().is_none(), "{name}");
        }
    }

    #[test]
    fn every_required_event_records_its_cancellation_reason() {
        #[derive(Clone, Copy)]
        enum Event {
            Buffer,
            Cursor,
            Mode,
            Navigation,
            Execution,
            Disable,
            Provider,
            Session,
            Exit,
        }

        let cases = BTreeMap::from([
            ("buffer", (Event::Buffer, CancellationReason::BufferChanged)),
            (
                "cursor",
                (Event::Cursor, CancellationReason::CursorMovedAwayFromEnd),
            ),
            ("disable", (Event::Disable, CancellationReason::Disabled)),
            (
                "execution",
                (Event::Execution, CancellationReason::CommandExecution),
            ),
            ("mode", (Event::Mode, CancellationReason::ModeChanged)),
            (
                "navigation",
                (Event::Navigation, CancellationReason::MenuNavigation),
            ),
            (
                "provider",
                (Event::Provider, CancellationReason::ProviderChanged),
            ),
            (
                "session",
                (Event::Session, CancellationReason::SessionChanged),
            ),
            (
                "session exit",
                (Event::Exit, CancellationReason::SessionExit),
            ),
        ]);

        for (name, (event, want)) in cases {
            let settings = LifecycleSettings::new(0, 1, 2_000).unwrap();
            let mut lifecycle = ready_lifecycle(settings);
            let request = start_request(&mut lifecycle, "git", 0);
            let old_generation = request.token().generation();
            match event {
                Event::Buffer => {
                    lifecycle.observe_query("git s", 5, 10);
                }
                Event::Cursor => {
                    lifecycle.observe_query("git", 2, 10);
                }
                Event::Mode => {
                    lifecycle.mode_changed(10);
                }
                Event::Navigation => {
                    lifecycle.menu_navigated(10);
                }
                Event::Execution => {
                    lifecycle.command_executed(10);
                }
                Event::Disable => {
                    lifecycle.set_enabled(false, 10);
                }
                Event::Provider => {
                    lifecycle.set_provider(Some("groq".to_owned()), 10);
                }
                Event::Session => {
                    lifecycle.start_session(SessionId::new(2), 10);
                }
                Event::Exit => {
                    lifecycle.end_session(10);
                }
            }

            let cancellation = lifecycle.last_cancellation().unwrap();
            assert_eq!(cancellation.reason(), want, "{name}");
            assert_eq!(cancellation.generation(), old_generation, "{name}");
            assert_eq!(cancellation.token(), Some(request.token()), "{name}");
            assert!(lifecycle.generation() > old_generation, "{name}");
            assert!(lifecycle.in_flight().is_none(), "{name}");
            assert_eq!(
                lifecycle.complete_request(request.token(), 11, ProviderOutcome::NoSuggestion),
                ResponseDisposition::Stale,
                "{name}"
            );
        }
    }

    #[test]
    fn stale_response_cannot_disturb_a_newer_in_flight_request() {
        let settings = LifecycleSettings::new(0, 1, 2_000).unwrap();
        let mut lifecycle = ready_lifecycle(settings);
        let first = start_request(&mut lifecycle, "git", 0);
        lifecycle.observe_query("git s", 5, 1);
        let LifecyclePoll::Start(second) = lifecycle.poll(1) else {
            panic!("second request did not start")
        };

        assert_eq!(
            lifecycle.complete_request(first.token(), 2, ProviderOutcome::NoSuggestion),
            ResponseDisposition::Stale
        );
        assert_eq!(
            lifecycle.in_flight().map(RequestPermit::token),
            Some(second.token())
        );
        assert_eq!(
            lifecycle.complete_request(second.token(), 2, ProviderOutcome::NoSuggestion),
            ResponseDisposition::NoSuggestion
        );
    }

    #[test]
    fn late_response_times_out_even_without_an_intervening_poll() {
        let settings = LifecycleSettings::new(0, 1, 2_000).unwrap();
        let mut lifecycle = ready_lifecycle(settings);
        let request = start_request(&mut lifecycle, "git", 0);
        assert_eq!(request.deadline_at_ms(), 2_000);
        assert_eq!(
            lifecycle.complete_request(
                request.token(),
                2_000,
                ProviderOutcome::Suggestion("git status".to_owned())
            ),
            ResponseDisposition::TimedOut
        );
        assert!(lifecycle.suggestion_cache().is_none());
        assert!(lifecycle.in_flight().is_none());
    }
}
