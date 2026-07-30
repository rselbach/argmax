//! Pure scheduling and notification policy for automatic update checks.
//!
//! Network transport, durable state locking, serialization, and user-facing
//! output remain outside this module. The caller supplies wall-clock samples,
//! performs requests with [`UPDATE_REQUEST_TIMEOUT`], and persists validated
//! [`UpdateState`] snapshots atomically.

use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::config::UpdateChannel;
use crate::version::{
    AutomaticUpdateDecision, MAX_VERSION_BYTES, RemoteRelease, SemanticVersion,
    decide_automatic_update,
};

/// Exact timeout required for an automatic update request.
pub const UPDATE_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Validated updater state suitable for external serialization.
///
/// Timestamps are caller-supplied Unix epoch milliseconds. Keeping them as
/// integers makes persistence representation-independent and avoids consulting
/// the process clock inside the policy layer.
#[derive(Clone, Default)]
pub struct UpdateState {
    reserved_check_ms: Option<u64>,
    completed_check_ms: Option<u64>,
    notified_version: Option<SemanticVersion>,
}

impl UpdateState {
    /// Builds validated state loaded by an external persistence layer.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateStateError::InvalidNotifiedVersion`] when the stored
    /// version is empty, oversized, or not strict semantic-version text.
    pub fn new(
        last_reserved_check_ms: Option<u64>,
        last_completed_check_ms: Option<u64>,
        last_notified_version: Option<&str>,
    ) -> Result<Self, UpdateStateError> {
        let last_notified_version = last_notified_version
            .map(SemanticVersion::parse)
            .transpose()
            .map_err(|_| UpdateStateError::InvalidNotifiedVersion)?;
        Ok(Self {
            reserved_check_ms: last_reserved_check_ms,
            completed_check_ms: last_completed_check_ms,
            notified_version: last_notified_version,
        })
    }

    /// Unix epoch milliseconds of the latest durably reserved check.
    #[must_use]
    pub const fn last_reserved_check_ms(&self) -> Option<u64> {
        self.reserved_check_ms
    }

    /// Unix epoch milliseconds of the latest completed automatic attempt.
    #[must_use]
    pub const fn last_completed_check_ms(&self) -> Option<u64> {
        self.completed_check_ms
    }

    /// Last version durably claimed for notification.
    #[must_use]
    pub fn last_notified_version(&self) -> Option<&str> {
        self.notified_version.as_ref().map(SemanticVersion::as_str)
    }

    /// Returns an owned persistence-ready snapshot.
    #[must_use]
    pub fn persistence_clone(&self) -> Self {
        self.clone()
    }

    /// Merges two snapshots without rolling either persisted field backward.
    ///
    /// Callers should perform this merge with the latest on-disk state inside
    /// the same cross-process critical section as their atomic write.
    #[must_use]
    pub fn merged_for_persistence(&self, persisted: &Self) -> Self {
        Self {
            reserved_check_ms: latest_timestamp(
                self.reserved_check_ms,
                persisted.reserved_check_ms,
            ),
            completed_check_ms: latest_timestamp(
                self.completed_check_ms,
                persisted.completed_check_ms,
            ),
            notified_version: newest_version(
                self.notified_version.as_ref(),
                persisted.notified_version.as_ref(),
            ),
        }
    }

    fn has_same_identity(&self, other: &Self) -> bool {
        self.reserved_check_ms == other.reserved_check_ms
            && self.completed_check_ms == other.completed_check_ms
            && match (
                self.notified_version.as_ref(),
                other.notified_version.as_ref(),
            ) {
                (Some(left), Some(right)) => left.has_same_identity(right),
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            }
    }
}

impl fmt::Debug for UpdateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateState")
            .field("has_reserved_check", &self.reserved_check_ms.is_some())
            .field("has_completed_check", &self.completed_check_ms.is_some())
            .field(
                "notified_version_bytes",
                &self
                    .notified_version
                    .as_ref()
                    .map(|version| version.as_str().len()),
            )
            .finish()
    }
}

/// Validation failure for externally loaded updater state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateStateError {
    /// The stored last-notified value was not a bounded semantic version.
    InvalidNotifiedVersion,
}

impl fmt::Display for UpdateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("stored updater notification version is invalid")
    }
}

impl Error for UpdateStateError {}

/// Content-free failure category returned by the external network transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkFailure {
    /// No usable network route was available.
    Offline,
    /// Domain-name resolution failed.
    Dns,
    /// The request exceeded [`UPDATE_REQUEST_TIMEOUT`].
    Timeout,
    /// The connection or secure transport failed.
    Transport,
    /// The release service returned an unsuccessful response.
    Api,
    /// The response body could not be interpreted as release metadata.
    InvalidResponse,
}

/// Opaque identity for one in-flight network request.
pub struct CheckToken {
    authority: Arc<()>,
    sequence: u64,
}

impl PartialEq for CheckToken {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence && Arc::ptr_eq(&self.authority, &other.authority)
    }
}

impl Eq for CheckToken {}

impl CheckToken {
    fn duplicate(&self) -> Self {
        Self {
            authority: Arc::clone(&self.authority),
            sequence: self.sequence,
        }
    }
}

impl fmt::Debug for CheckToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CheckToken(<redacted>)")
    }
}

/// Parameters for a newly authorized automatic network request.
#[must_use]
pub struct CheckRequest {
    token: CheckToken,
}

impl CheckRequest {
    /// Consumes the one-shot request and returns its completion token.
    #[must_use]
    pub fn into_token(self) -> CheckToken {
        self.token
    }

    /// Required request timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        UPDATE_REQUEST_TIMEOUT
    }
}

impl fmt::Debug for CheckRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckRequest")
            .field("token", &self.token)
            .field("timeout", &UPDATE_REQUEST_TIMEOUT)
            .finish()
    }
}

/// Durable reservation proposed before an automatic network request.
///
/// Persist this reservation atomically, then pass it to
/// [`AutomaticUpdater::confirm_check_reservation`]. It cannot authorize network
/// I/O by itself.
#[must_use]
pub struct CheckReservation {
    token: CheckToken,
    state: UpdateState,
}

impl CheckReservation {
    /// Proposed state to write atomically before launching the request.
    #[must_use]
    pub fn state_for_persistence(&self) -> UpdateState {
        self.state.persistence_clone()
    }
}

impl fmt::Debug for CheckReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckReservation")
            .field("token", &self.token)
            .field("state", &self.state)
            .finish()
    }
}

/// Result of applying startup, interval, and concurrency gates.
#[derive(Debug)]
#[must_use]
pub enum CheckReservationDecision {
    /// Persist this reservation before requesting network I/O.
    Reserve(CheckReservation),
    /// Automatic startup checks are disabled.
    Disabled,
    /// The configured interval has not elapsed.
    IntervalPending,
    /// The supplied clock is behind persisted state.
    ClockAnomaly,
    /// A request is already in flight.
    AlreadyInFlight,
    /// Another check reservation awaits persistence confirmation.
    ReservationAlreadyInFlight,
    /// A notification claim awaits persistence confirmation.
    NotificationClaimInFlight,
    /// This session already durably claimed its one notification.
    SessionNoticeComplete,
    /// The opaque request-token space was exhausted.
    TokenExhausted,
}

/// Outcome of the atomic check-reservation persistence transaction.
#[derive(Clone, Debug)]
pub enum ReservationPersistence {
    /// The exact reservation state was durably persisted.
    Persisted,
    /// Persistence failed; no network request may start.
    Failed,
    /// Another writer won; merge this newly authoritative state.
    Superseded(UpdateState),
}

/// Result of confirming one durable check reservation.
#[derive(Debug)]
#[must_use]
pub enum CheckReservationConfirmation {
    /// Launch this request in the background without retaining the state lock.
    Request(CheckRequest),
    /// Persistence failed, so the reservation did not authorize I/O.
    PersistenceFailed,
    /// Another writer won, so this reservation did not authorize I/O.
    SupersededByPersistedState,
    /// The reservation did not belong to this active state machine.
    StaleReservation,
}

/// Result of accepting or rejecting one network completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum CheckCompletion {
    /// The matching attempt completed and its timestamp must be persisted.
    Recorded,
    /// The clock moved backward; the start timestamp was recorded and the
    /// response was ignored.
    ClockRollbackRecorded,
    /// The token did not identify the currently active request.
    StaleToken,
}

/// Construction failure for automatic update policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomaticUpdaterError {
    /// The compile-time running version exceeded the defensive bound.
    CurrentVersionTooLong,
    /// A zero interval would permit an unbounded retry loop.
    ZeroInterval,
    /// The interval could not be represented in epoch milliseconds.
    IntervalTooLarge,
}

impl fmt::Display for AutomaticUpdaterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentVersionTooLong => {
                formatter.write_str("running version exceeds the updater bound")
            }
            Self::ZeroInterval => formatter.write_str("update interval must be positive"),
            Self::IntervalTooLarge => {
                formatter.write_str("update interval cannot be represented safely")
            }
        }
    }
}

impl Error for AutomaticUpdaterError {}

struct InFlight {
    token: CheckToken,
    started_at_ms: u64,
}

struct ActiveCheckReservation {
    token: CheckToken,
    started_at_ms: u64,
    state: UpdateState,
}

struct ActiveClaim {
    id: u64,
    version: SemanticVersion,
    state: UpdateState,
}

/// Pure automatic-update state machine for one active shell session.
pub struct AutomaticUpdater {
    authority: Arc<()>,
    current_version: Box<str>,
    channel: UpdateChannel,
    check_on_startup: bool,
    check_interval_ms: u64,
    state: UpdateState,
    next_check_token: Option<u64>,
    active_check_reservation: Option<ActiveCheckReservation>,
    in_flight: Option<InFlight>,
    pending_version: Option<SemanticVersion>,
    next_claim_id: Option<u64>,
    active_claim: Option<ActiveClaim>,
    notified_this_session: bool,
}

impl AutomaticUpdater {
    /// Creates one session policy from resolved configuration and persisted state.
    ///
    /// # Errors
    ///
    /// Returns [`AutomaticUpdaterError`] for an oversized running version, a
    /// zero interval, or an interval too large for safe epoch arithmetic.
    pub fn new(
        current_version: &str,
        check_on_startup: bool,
        channel: UpdateChannel,
        check_interval: Duration,
        state: UpdateState,
    ) -> Result<Self, AutomaticUpdaterError> {
        if current_version.len() > MAX_VERSION_BYTES {
            return Err(AutomaticUpdaterError::CurrentVersionTooLong);
        }
        let check_interval_ms = duration_millis_ceil(check_interval)?;
        Ok(Self {
            authority: Arc::new(()),
            current_version: current_version.into(),
            channel,
            check_on_startup,
            check_interval_ms,
            state,
            next_check_token: Some(1),
            active_check_reservation: None,
            in_flight: None,
            pending_version: None,
            next_claim_id: Some(1),
            active_claim: None,
            notified_this_session: false,
        })
    }

    /// Proposes a durable reservation after applying all startup-check gates.
    ///
    /// `persisted` must be the latest state read inside a cross-process state
    /// transaction. On success, atomically write
    /// [`CheckReservation::state_for_persistence`], release the transaction,
    /// and then confirm the reservation. Only a confirmed reservation can
    /// produce network authority, so no state lock is held over network I/O.
    pub fn reserve_startup_check(
        &mut self,
        now_ms: u64,
        persisted: &UpdateState,
    ) -> CheckReservationDecision {
        self.state = self.state.merged_for_persistence(persisted);
        if !self.check_on_startup {
            return CheckReservationDecision::Disabled;
        }
        if self.notified_this_session {
            return CheckReservationDecision::SessionNoticeComplete;
        }
        if self.active_check_reservation.is_some() {
            return CheckReservationDecision::ReservationAlreadyInFlight;
        }
        if self.in_flight.is_some() {
            return CheckReservationDecision::AlreadyInFlight;
        }
        if self.active_claim.is_some() {
            return CheckReservationDecision::NotificationClaimInFlight;
        }
        if let Some(last_check_ms) =
            latest_timestamp(self.state.reserved_check_ms, self.state.completed_check_ms)
        {
            let Some(elapsed_ms) = now_ms.checked_sub(last_check_ms) else {
                return CheckReservationDecision::ClockAnomaly;
            };
            if elapsed_ms < self.check_interval_ms {
                return CheckReservationDecision::IntervalPending;
            }
        }

        let Some(raw_token) = self.next_check_token else {
            return CheckReservationDecision::TokenExhausted;
        };
        self.next_check_token = raw_token.checked_add(1);
        let token = CheckToken {
            authority: Arc::clone(&self.authority),
            sequence: raw_token,
        };
        let mut proposed_state = self.state.persistence_clone();
        proposed_state.reserved_check_ms = Some(now_ms);
        self.active_check_reservation = Some(ActiveCheckReservation {
            token: token.duplicate(),
            started_at_ms: now_ms,
            state: proposed_state.persistence_clone(),
        });
        CheckReservationDecision::Reserve(CheckReservation {
            token,
            state: proposed_state,
        })
    }

    /// Confirms whether one proposed check reservation was durably persisted.
    ///
    /// Network authority is returned only for an exact reservation confirmed as
    /// persisted. Failed or superseded writes never launch a request.
    pub fn confirm_check_reservation(
        &mut self,
        reservation: CheckReservation,
        outcome: ReservationPersistence,
    ) -> CheckReservationConfirmation {
        let CheckReservation {
            token,
            state: reservation_state,
        } = reservation;
        let Some(active) = self.active_check_reservation.as_ref() else {
            return CheckReservationConfirmation::StaleReservation;
        };
        if active.token != token || !active.state.has_same_identity(&reservation_state) {
            return CheckReservationConfirmation::StaleReservation;
        }
        let Some(active) = self.active_check_reservation.take() else {
            return CheckReservationConfirmation::StaleReservation;
        };

        match outcome {
            ReservationPersistence::Persisted => {
                self.state = self.state.merged_for_persistence(&active.state);
                self.in_flight = Some(InFlight {
                    token: active.token.duplicate(),
                    started_at_ms: active.started_at_ms,
                });
                CheckReservationConfirmation::Request(CheckRequest {
                    token: active.token,
                })
            }
            ReservationPersistence::Failed => CheckReservationConfirmation::PersistenceFailed,
            ReservationPersistence::Superseded(persisted) => {
                self.state = self.state.merged_for_persistence(&persisted);
                CheckReservationConfirmation::SupersededByPersistedState
            }
        }
    }

    /// Records one matching transport completion and evaluates its release.
    ///
    /// Every matching completion, including [`NetworkFailure`], advances the
    /// persistence snapshot. Version, channel, development-build, and network
    /// failures collapse to the same silent automatic outcome. A stale token
    /// cannot mutate scheduling or notification state.
    pub fn complete_check(
        &mut self,
        token: CheckToken,
        completed_at_ms: u64,
        response: Result<RemoteRelease<'_>, NetworkFailure>,
    ) -> CheckCompletion {
        let Some(in_flight) = self.in_flight.as_ref() else {
            return CheckCompletion::StaleToken;
        };
        if in_flight.token != token {
            return CheckCompletion::StaleToken;
        }
        drop(token);
        let Some(in_flight) = self.in_flight.take() else {
            return CheckCompletion::StaleToken;
        };

        if completed_at_ms < in_flight.started_at_ms {
            self.record_completed_check(in_flight.started_at_ms);
            return CheckCompletion::ClockRollbackRecorded;
        }
        self.record_completed_check(completed_at_ms);

        let Ok(remote_release) = response else {
            return CheckCompletion::Recorded;
        };
        if let AutomaticUpdateDecision::Available(version) =
            decide_automatic_update(&self.current_version, remote_release, self.channel)
        {
            self.retain_pending(version);
        }
        CheckCompletion::Recorded
    }

    /// Returns the latest local snapshot for external atomic persistence.
    #[must_use]
    pub fn state_for_persistence(&self) -> UpdateState {
        self.state.persistence_clone()
    }

    /// Starts a notification claim only in response to a completed command.
    ///
    /// `persisted` must be the latest state read inside the caller's
    /// cross-process state transaction. On success, atomically write
    /// [`NotificationClaim::state_for_persistence`] before confirming the claim.
    /// Never emit user-facing output from the claim itself.
    pub fn claim_after_completed_command(&mut self, persisted: &UpdateState) -> ClaimDecision {
        self.state = self.state.merged_for_persistence(persisted);
        if self.notified_this_session {
            return ClaimDecision::SessionNoticeComplete;
        }
        if self.active_claim.is_some() {
            return ClaimDecision::ClaimAlreadyInFlight;
        }
        if self.active_check_reservation.is_some() || self.in_flight.is_some() {
            return ClaimDecision::UpdateCheckInProgress;
        }
        let Some(version) = self.pending_version.take() else {
            return ClaimDecision::NoPendingVersion;
        };
        if !is_newer_than_notified(&version, &self.state) {
            return ClaimDecision::AlreadyPersisted;
        }
        let Some(id) = self.next_claim_id else {
            self.pending_version = Some(version);
            return ClaimDecision::TokenExhausted;
        };
        self.next_claim_id = id.checked_add(1);

        let mut proposed_state = self.state.persistence_clone();
        proposed_state.notified_version = Some(version.clone());
        self.active_claim = Some(ActiveClaim {
            id,
            version: version.clone(),
            state: proposed_state.persistence_clone(),
        });
        ClaimDecision::Claim(Box::new(NotificationClaim {
            authority: Arc::clone(&self.authority),
            id,
            version,
            state: proposed_state,
        }))
    }

    /// Completes the persistence phase of one notification claim.
    ///
    /// A user-facing [`NotificationIntent`] is returned only after the caller
    /// confirms that the proposed state was durably and atomically persisted.
    pub fn confirm_notification(
        &mut self,
        claim: Box<NotificationClaim>,
        outcome: ClaimPersistence,
    ) -> ClaimConfirmation {
        let NotificationClaim {
            authority: claim_authority,
            id: claim_id,
            version: claim_version,
            state: claim_state,
        } = *claim;
        let Some(active) = self.active_claim.as_ref() else {
            return ClaimConfirmation::StaleClaim;
        };
        if !Arc::ptr_eq(&self.authority, &claim_authority)
            || !claim_matches(active, claim_id, &claim_version, &claim_state)
        {
            return ClaimConfirmation::StaleClaim;
        }
        let Some(active) = self.active_claim.take() else {
            return ClaimConfirmation::StaleClaim;
        };

        match outcome {
            ClaimPersistence::Persisted => {
                self.state = self.state.merged_for_persistence(&active.state);
                self.pending_version = None;
                self.notified_this_session = true;
                ClaimConfirmation::Emit(NotificationIntent {
                    version: active.version,
                })
            }
            ClaimPersistence::Failed => {
                self.retain_pending(active.version);
                ClaimConfirmation::RetryPending
            }
            ClaimPersistence::Superseded(persisted) => {
                self.state = self.state.merged_for_persistence(&persisted);
                if is_newer_than_notified(&active.version, &self.state) {
                    self.retain_pending(active.version);
                    ClaimConfirmation::RetryPending
                } else {
                    self.pending_version = None;
                    ClaimConfirmation::SuppressedByPersistedState
                }
            }
        }
    }

    fn record_completed_check(&mut self, completed_at_ms: u64) {
        self.state.completed_check_ms = Some(
            self.state
                .completed_check_ms
                .map_or(completed_at_ms, |previous| previous.max(completed_at_ms)),
        );
    }

    fn retain_pending(&mut self, candidate: SemanticVersion) {
        if self.notified_this_session || !is_newer_than_notified(&candidate, &self.state) {
            return;
        }
        self.pending_version = match self.pending_version.take() {
            Some(pending) if !pending.precedence_cmp(&candidate).is_lt() => Some(pending),
            Some(_) | None => Some(candidate),
        };
    }
}

impl fmt::Debug for AutomaticUpdater {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AutomaticUpdater")
            .field("authority", &"<redacted>")
            .field("current_version_bytes", &self.current_version.len())
            .field("channel", &self.channel)
            .field("check_on_startup", &self.check_on_startup)
            .field("check_interval_ms", &self.check_interval_ms)
            .field("state", &self.state)
            .field("check_token_available", &self.next_check_token.is_some())
            .field(
                "active_check_reservation",
                &self.active_check_reservation.is_some(),
            )
            .field("in_flight", &self.in_flight.is_some())
            .field("pending_version", &self.pending_version.is_some())
            .field("claim_token_available", &self.next_claim_id.is_some())
            .field("active_claim", &self.active_claim.is_some())
            .field("notified_this_session", &self.notified_this_session)
            .finish()
    }
}

/// Persistence proposal created after a completed command.
///
/// This is deliberately not a notification intent. Persist its state first,
/// then pass the claim to [`AutomaticUpdater::confirm_notification`].
#[must_use]
pub struct NotificationClaim {
    authority: Arc<()>,
    id: u64,
    version: SemanticVersion,
    state: UpdateState,
}

impl NotificationClaim {
    /// Candidate version that the persistence transaction will claim.
    #[must_use]
    pub fn version(&self) -> &str {
        self.version.as_str()
    }

    /// Proposed state to write atomically before confirmation.
    #[must_use]
    pub fn state_for_persistence(&self) -> UpdateState {
        self.state.persistence_clone()
    }
}

impl fmt::Debug for NotificationClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotificationClaim")
            .field("authority", &"<redacted>")
            .field("claim_id_present", &(self.id != 0))
            .field("version_bytes", &self.version.as_str().len())
            .field("state", &self.state)
            .finish()
    }
}

/// Result of attempting to claim after a completed command.
#[derive(Debug)]
#[must_use]
pub enum ClaimDecision {
    /// Persist this claim before requesting notification output.
    Claim(Box<NotificationClaim>),
    /// No eligible remote version is pending.
    NoPendingVersion,
    /// Another claim awaits persistence confirmation.
    ClaimAlreadyInFlight,
    /// A check reservation or request must finish before claiming a notice.
    UpdateCheckInProgress,
    /// Fresh persisted state already covers the pending version's precedence.
    AlreadyPersisted,
    /// This active session already claimed its one notice.
    SessionNoticeComplete,
    /// The opaque claim-token space was exhausted.
    TokenExhausted,
}

/// Outcome of the caller's atomic persistence transaction.
#[derive(Clone, Debug)]
pub enum ClaimPersistence {
    /// The claim state was durably persisted.
    Persisted,
    /// Persistence failed; keep the candidate pending for a later command.
    Failed,
    /// Another writer won; merge this newly authoritative state.
    Superseded(UpdateState),
}

/// Result of confirming a two-phase notification claim.
#[derive(Debug)]
#[must_use]
pub enum ClaimConfirmation {
    /// Emit this notice now, after the completed command.
    Emit(NotificationIntent),
    /// Persistence did not claim the version; retry after a later command.
    RetryPending,
    /// Another session already persisted this or a newer precedence.
    SuppressedByPersistedState,
    /// The claim did not belong to this active state machine.
    StaleClaim,
}

/// Authorized user-facing update notice after durable suppression state exists.
#[must_use]
pub struct NotificationIntent {
    version: SemanticVersion,
}

impl NotificationIntent {
    /// Available version to include in the notice.
    #[must_use]
    pub fn version(&self) -> &str {
        self.version.as_str()
    }
}

impl fmt::Debug for NotificationIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotificationIntent")
            .field("version_bytes", &self.version.as_str().len())
            .finish()
    }
}

// `is_multiple_of` is newer than the crate's Rust 1.85 MSRV.
#[allow(unknown_lints, clippy::manual_is_multiple_of)]
fn duration_millis_ceil(interval: Duration) -> Result<u64, AutomaticUpdaterError> {
    if interval.is_zero() {
        return Err(AutomaticUpdaterError::ZeroInterval);
    }
    let mut milliseconds = interval.as_millis();
    if interval.subsec_nanos() % 1_000_000 != 0 {
        milliseconds = milliseconds
            .checked_add(1)
            .ok_or(AutomaticUpdaterError::IntervalTooLarge)?;
    }
    u64::try_from(milliseconds).map_err(|_| AutomaticUpdaterError::IntervalTooLarge)
}

fn latest_timestamp(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left @ Some(_), None) | (None, left @ Some(_)) => left,
        (None, None) => None,
    }
}

fn newest_version(
    left: Option<&SemanticVersion>,
    right: Option<&SemanticVersion>,
) -> Option<SemanticVersion> {
    match (left, right) {
        (Some(left), Some(right)) => match left.precedence_cmp(right) {
            Ordering::Greater => Some(left.clone()),
            Ordering::Equal if left.as_str() >= right.as_str() => Some(left.clone()),
            Ordering::Less | Ordering::Equal => Some(right.clone()),
        },
        (Some(version), None) | (None, Some(version)) => Some(version.clone()),
        (None, None) => None,
    }
}

fn is_newer_than_notified(version: &SemanticVersion, state: &UpdateState) -> bool {
    state
        .notified_version
        .as_ref()
        .is_none_or(|notified| version.precedence_cmp(notified).is_gt())
}

fn claim_matches(
    active: &ActiveClaim,
    claim_id: u64,
    claim_version: &SemanticVersion,
    claim_state: &UpdateState,
) -> bool {
    active.id == claim_id
        && active.version.has_same_identity(claim_version)
        && active.state.has_same_identity(claim_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::ReleaseKind;

    // `Duration::from_mins` is newer than the crate's Rust 1.85 MSRV.
    #[allow(unknown_lints, clippy::duration_suboptimal_units)]
    const MINUTE: Duration = Duration::from_secs(60);

    fn scheduler(
        current_version: &str,
        enabled: bool,
        channel: UpdateChannel,
        state: UpdateState,
    ) -> AutomaticUpdater {
        AutomaticUpdater::new(current_version, enabled, channel, MINUTE, state).unwrap()
    }

    fn scheduler_with_interval(
        current_version: &str,
        channel: UpdateChannel,
        check_interval: Duration,
    ) -> AutomaticUpdater {
        AutomaticUpdater::new(
            current_version,
            true,
            channel,
            check_interval,
            UpdateState::default(),
        )
        .unwrap()
    }

    fn reserve_from_local(updater: &mut AutomaticUpdater, now_ms: u64) -> CheckReservationDecision {
        let persisted = updater.state_for_persistence();
        updater.reserve_startup_check(now_ms, &persisted)
    }

    fn take_reservation(decision: CheckReservationDecision) -> CheckReservation {
        let CheckReservationDecision::Reserve(reservation) = decision else {
            panic!("expected a check reservation")
        };
        reservation
    }

    fn start(updater: &mut AutomaticUpdater, now_ms: u64) -> CheckToken {
        let reservation = take_reservation(reserve_from_local(updater, now_ms));
        let CheckReservationConfirmation::Request(request) =
            updater.confirm_check_reservation(reservation, ReservationPersistence::Persisted)
        else {
            panic!("expected an authorized request")
        };
        assert_eq!(request.timeout(), Duration::from_secs(5));
        request.into_token()
    }

    fn finish_release(
        updater: &mut AutomaticUpdater,
        token: CheckToken,
        completed_at_ms: u64,
        tag: &str,
        kind: ReleaseKind,
    ) -> CheckCompletion {
        updater.complete_check(token, completed_at_ms, Ok(RemoteRelease::new(tag, kind)))
    }

    fn take_claim(decision: ClaimDecision) -> Box<NotificationClaim> {
        let ClaimDecision::Claim(claim) = decision else {
            panic!("expected a notification claim")
        };
        claim
    }

    #[test]
    fn request_timeout_is_exactly_five_seconds() {
        assert_eq!(UPDATE_REQUEST_TIMEOUT, Duration::from_secs(5));
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let reservation = take_reservation(reserve_from_local(&mut updater, 0));
        let CheckReservationConfirmation::Request(request) =
            updater.confirm_check_reservation(reservation, ReservationPersistence::Persisted)
        else {
            panic!("expected request")
        };
        assert_eq!(request.timeout(), UPDATE_REQUEST_TIMEOUT);
    }

    #[test]
    fn persisted_state_validates_versions_and_redacts_debug() {
        let state = UpdateState::new(Some(41), Some(42), Some("1.2.3+private-token")).unwrap();
        assert_eq!(state.last_reserved_check_ms(), Some(41));
        assert_eq!(state.last_completed_check_ms(), Some(42));
        assert_eq!(state.last_notified_version(), Some("1.2.3+private-token"));
        let debug = format!("{state:?}");
        assert!(!debug.contains("42"));
        assert!(!debug.contains("private-token"));

        assert!(matches!(
            UpdateState::new(None, None, Some("latest")),
            Err(UpdateStateError::InvalidNotifiedVersion)
        ));
        let oversized = "1".repeat(MAX_VERSION_BYTES + 1);
        assert!(UpdateState::new(None, None, Some(&oversized)).is_err());
    }

    #[test]
    fn disabled_interval_and_clock_gates_fail_closed() {
        let mut disabled = scheduler(
            "1.0.0",
            false,
            UpdateChannel::Stable,
            UpdateState::default(),
        );
        assert!(matches!(
            reserve_from_local(&mut disabled, 1_000),
            CheckReservationDecision::Disabled
        ));

        let state = UpdateState::new(None, Some(1_000), None).unwrap();
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, state);
        assert!(matches!(
            reserve_from_local(&mut updater, 999),
            CheckReservationDecision::ClockAnomaly
        ));
        assert!(matches!(
            reserve_from_local(&mut updater, 60_999),
            CheckReservationDecision::IntervalPending
        ));
        assert!(matches!(
            reserve_from_local(&mut updater, 61_000),
            CheckReservationDecision::Reserve(_)
        ));

        let future = UpdateState::new(Some(u64::MAX), None, None).unwrap();
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, future);
        assert!(matches!(
            reserve_from_local(&mut updater, u64::MAX - 1),
            CheckReservationDecision::ClockAnomaly
        ));
        assert!(matches!(
            reserve_from_local(&mut updater, u64::MAX),
            CheckReservationDecision::IntervalPending
        ));
    }

    #[test]
    fn interval_conversion_rounds_up_and_rejects_overflow() {
        let updater = AutomaticUpdater::new(
            "1.0.0",
            true,
            UpdateChannel::Stable,
            Duration::from_nanos(1),
            UpdateState::default(),
        )
        .unwrap();
        assert_eq!(updater.check_interval_ms, 1);
        assert!(matches!(
            AutomaticUpdater::new(
                "1.0.0",
                true,
                UpdateChannel::Stable,
                Duration::ZERO,
                UpdateState::default(),
            ),
            Err(AutomaticUpdaterError::ZeroInterval)
        ));
        assert!(matches!(
            AutomaticUpdater::new(
                "1.0.0",
                true,
                UpdateChannel::Stable,
                Duration::from_secs(u64::MAX),
                UpdateState::default(),
            ),
            Err(AutomaticUpdaterError::IntervalTooLarge)
        ));
        let oversized = "1".repeat(MAX_VERSION_BYTES + 1);
        assert!(matches!(
            AutomaticUpdater::new(
                &oversized,
                true,
                UpdateChannel::Stable,
                MINUTE,
                UpdateState::default(),
            ),
            Err(AutomaticUpdaterError::CurrentVersionTooLong)
        ));
    }

    #[test]
    fn one_request_is_in_flight_and_stale_tokens_cannot_mutate_state() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut updater, 10);
        assert!(matches!(
            reserve_from_local(&mut updater, 20),
            CheckReservationDecision::AlreadyInFlight
        ));
        let stale = CheckToken {
            authority: Arc::clone(&token.authority),
            sequence: token.sequence + 1,
        };
        assert_eq!(
            updater.complete_check(stale, 30, Err(NetworkFailure::Offline)),
            CheckCompletion::StaleToken
        );
        assert_eq!(updater.state.last_completed_check_ms(), None);
        assert_eq!(
            updater.complete_check(token, 30, Err(NetworkFailure::Offline)),
            CheckCompletion::Recorded
        );
        assert_eq!(updater.state.last_completed_check_ms(), Some(30));
    }

    #[test]
    fn concurrent_sessions_launch_only_the_durably_winning_reservation() {
        let persisted = UpdateState::default();
        let mut winner = scheduler(
            "1.0.0",
            true,
            UpdateChannel::Stable,
            persisted.persistence_clone(),
        );
        let mut loser = scheduler(
            "1.0.0",
            true,
            UpdateChannel::Stable,
            persisted.persistence_clone(),
        );
        let winning = take_reservation(winner.reserve_startup_check(1_000, &persisted));
        let losing = take_reservation(loser.reserve_startup_check(1_000, &persisted));
        let won_state = winning.state_for_persistence();

        assert!(matches!(
            winner.confirm_check_reservation(winning, ReservationPersistence::Persisted),
            CheckReservationConfirmation::Request(_)
        ));
        assert!(matches!(
            loser.confirm_check_reservation(losing, ReservationPersistence::Superseded(won_state)),
            CheckReservationConfirmation::SupersededByPersistedState
        ));
        assert!(matches!(
            reserve_from_local(&mut loser, 60_999),
            CheckReservationDecision::IntervalPending
        ));
    }

    #[test]
    fn failed_reservation_persistence_never_authorizes_network_io() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let reservation = take_reservation(reserve_from_local(&mut updater, 1_000));
        assert_eq!(
            reservation.state_for_persistence().last_reserved_check_ms(),
            Some(1_000)
        );
        assert!(matches!(
            updater.confirm_check_reservation(reservation, ReservationPersistence::Failed),
            CheckReservationConfirmation::PersistenceFailed
        ));
        assert_eq!(updater.state.last_reserved_check_ms(), None);

        let retry = take_reservation(reserve_from_local(&mut updater, 1_000));
        assert!(matches!(
            updater.confirm_check_reservation(retry, ReservationPersistence::Persisted),
            CheckReservationConfirmation::Request(_)
        ));
    }

    #[test]
    fn persisted_reservation_survives_rebuild_without_holding_an_io_lock() {
        let mut original = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let reservation = take_reservation(reserve_from_local(&mut original, 1_000));
        let durable = reservation.state_for_persistence();
        drop(reservation);
        drop(original);

        let mut rebuilt = scheduler("1.0.0", true, UpdateChannel::Stable, durable);
        assert!(matches!(
            reserve_from_local(&mut rebuilt, 60_999),
            CheckReservationDecision::IntervalPending
        ));
        assert!(matches!(
            reserve_from_local(&mut rebuilt, 61_000),
            CheckReservationDecision::Reserve(_)
        ));
    }

    #[test]
    fn rebuilt_state_machine_rejects_foreign_request_authority() {
        let mut original = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let foreign = start(&mut original, 0);
        let mut rebuilt = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let current = start(&mut rebuilt, 0);

        assert_eq!(foreign.sequence, current.sequence);
        assert!(!Arc::ptr_eq(&foreign.authority, &current.authority));
        assert_eq!(
            rebuilt.complete_check(foreign, 1, Err(NetworkFailure::Offline)),
            CheckCompletion::StaleToken
        );
        assert_eq!(rebuilt.state.last_completed_check_ms(), None);
        assert_eq!(
            rebuilt.complete_check(current, 1, Err(NetworkFailure::Offline)),
            CheckCompletion::Recorded
        );
    }

    #[test]
    fn offline_completion_is_persisted_and_prevents_startup_retry() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut updater, 1_000);
        assert_eq!(
            updater.complete_check(token, 2_000, Err(NetworkFailure::Dns)),
            CheckCompletion::Recorded
        );
        let snapshot = updater.state_for_persistence();
        assert_eq!(snapshot.last_completed_check_ms(), Some(2_000));
        assert!(matches!(
            reserve_from_local(&mut updater, 61_999),
            CheckReservationDecision::IntervalPending
        ));
        assert!(matches!(
            reserve_from_local(&mut updater, 62_000),
            CheckReservationDecision::Reserve(_)
        ));
    }

    #[test]
    fn rollback_during_request_records_start_and_ignores_response() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut updater, 100_000);
        assert_eq!(
            finish_release(&mut updater, token, 99_999, "v2.0.0", ReleaseKind::Stable,),
            CheckCompletion::ClockRollbackRecorded
        );
        assert_eq!(updater.state.last_completed_check_ms(), Some(100_000));
        assert!(updater.pending_version.is_none());
        assert!(matches!(
            reserve_from_local(&mut updater, 99_999),
            CheckReservationDecision::ClockAnomaly
        ));
    }

    #[test]
    fn automatic_failures_are_silent_and_create_no_claim() {
        let cases = [
            ("dev", "v2.0.0", ReleaseKind::Stable, UpdateChannel::Stable),
            (
                "not-a-version",
                "v2.0.0",
                ReleaseKind::Stable,
                UpdateChannel::Stable,
            ),
            (
                "1.0.0",
                "latest",
                ReleaseKind::Stable,
                UpdateChannel::Stable,
            ),
            (
                "1.0.0",
                "v2.0.0-nightly.1",
                ReleaseKind::Nightly,
                UpdateChannel::Stable,
            ),
            (
                "1.0.0",
                "v1.0.0",
                ReleaseKind::Stable,
                UpdateChannel::Stable,
            ),
        ];
        for (current, remote, kind, channel) in cases {
            let mut updater = scheduler(current, true, channel, UpdateState::default());
            let token = start(&mut updater, 0);
            assert_eq!(
                finish_release(&mut updater, token, 1, remote, kind),
                CheckCompletion::Recorded
            );
            let state = updater.state_for_persistence();
            assert!(matches!(
                updater.claim_after_completed_command(&state),
                ClaimDecision::NoPendingVersion
            ));
        }
    }

    fn pending_after_two_releases(first: &str, second: &str) -> String {
        let mut updater =
            scheduler_with_interval("1.0.0", UpdateChannel::Stable, Duration::from_millis(1));
        let token = start(&mut updater, 0);
        assert_eq!(
            finish_release(&mut updater, token, 1, first, ReleaseKind::Stable),
            CheckCompletion::Recorded
        );

        let token = start(&mut updater, 2);
        let persisted = updater.state_for_persistence();
        assert!(matches!(
            updater.claim_after_completed_command(&persisted),
            ClaimDecision::UpdateCheckInProgress
        ));
        assert_eq!(
            finish_release(&mut updater, token, 3, second, ReleaseKind::Stable),
            CheckCompletion::Recorded
        );

        let persisted = updater.state_for_persistence();
        take_claim(updater.claim_after_completed_command(&persisted))
            .version()
            .to_owned()
    }

    #[test]
    fn later_higher_release_replaces_a_pending_lower_release() {
        assert_eq!(pending_after_two_releases("v2.0.0", "v3.0.0"), "3.0.0");
    }

    #[test]
    fn later_lower_release_cannot_regress_a_pending_higher_release() {
        assert_eq!(pending_after_two_releases("v3.0.0", "v2.0.0"), "3.0.0");
    }

    #[test]
    fn equal_precedence_build_metadata_cannot_replace_a_pending_release() {
        assert_eq!(
            pending_after_two_releases("v2.0.0+first", "v2.0.0+second"),
            "2.0.0+first"
        );
        assert_eq!(
            pending_after_two_releases("v2.0.0+second", "v2.0.0+first"),
            "2.0.0+second"
        );
    }

    #[test]
    fn claim_requires_persistence_before_emission_and_limits_the_session() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut updater, 0);
        assert_eq!(
            finish_release(&mut updater, token, 1, "v2.0.0", ReleaseKind::Stable,),
            CheckCompletion::Recorded
        );
        let persisted = updater.state_for_persistence();
        let claim = take_claim(updater.claim_after_completed_command(&persisted));
        assert_eq!(claim.version(), "2.0.0");
        assert_eq!(
            claim.state_for_persistence().last_notified_version(),
            Some("2.0.0")
        );
        assert!(matches!(
            updater.claim_after_completed_command(&persisted),
            ClaimDecision::ClaimAlreadyInFlight
        ));
        assert!(matches!(
            reserve_from_local(&mut updater, 60_001),
            CheckReservationDecision::NotificationClaimInFlight
        ));
        assert!(matches!(
            updater.confirm_notification(claim, ClaimPersistence::Failed),
            ClaimConfirmation::RetryPending
        ));

        let claim = take_claim(updater.claim_after_completed_command(&persisted));
        let confirmation = updater.confirm_notification(claim, ClaimPersistence::Persisted);
        let ClaimConfirmation::Emit(intent) = confirmation else {
            panic!("persistence should authorize one intent")
        };
        assert_eq!(intent.version(), "2.0.0");
        assert!(matches!(
            updater.claim_after_completed_command(&persisted),
            ClaimDecision::SessionNoticeComplete
        ));
        assert!(matches!(
            reserve_from_local(&mut updater, 100_000),
            CheckReservationDecision::SessionNoticeComplete
        ));
    }

    #[test]
    fn rebuilt_state_machine_rejects_an_identical_foreign_notification_claim() {
        let mut original = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut original, 0);
        assert_eq!(
            finish_release(&mut original, token, 1, "v2.0.0", ReleaseKind::Stable),
            CheckCompletion::Recorded
        );
        let persisted = original.state_for_persistence();
        let own_claim = take_claim(original.claim_after_completed_command(&persisted));

        let mut rebuilt = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut rebuilt, 0);
        assert_eq!(
            finish_release(&mut rebuilt, token, 1, "v2.0.0", ReleaseKind::Stable),
            CheckCompletion::Recorded
        );
        let rebuilt_persisted = rebuilt.state_for_persistence();
        let foreign_claim = take_claim(rebuilt.claim_after_completed_command(&rebuilt_persisted));
        assert_eq!(own_claim.id, foreign_claim.id);
        assert!(own_claim.version.has_same_identity(&foreign_claim.version));
        assert!(own_claim.state.has_same_identity(&foreign_claim.state));
        assert!(!Arc::ptr_eq(&own_claim.authority, &foreign_claim.authority));

        assert!(matches!(
            original.confirm_notification(foreign_claim, ClaimPersistence::Persisted),
            ClaimConfirmation::StaleClaim
        ));
        assert!(matches!(
            original.confirm_notification(own_claim, ClaimPersistence::Persisted),
            ClaimConfirmation::Emit(_)
        ));
    }

    #[test]
    fn persisted_same_precedence_suppresses_across_sessions() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut updater, 0);
        assert_eq!(
            finish_release(
                &mut updater,
                token,
                1,
                "v1.2.3+remote-build",
                ReleaseKind::Stable,
            ),
            CheckCompletion::Recorded
        );
        let persisted = UpdateState::new(None, Some(2), Some("1.2.3+other-build")).unwrap();
        assert!(matches!(
            updater.claim_after_completed_command(&persisted),
            ClaimDecision::AlreadyPersisted
        ));
    }

    #[test]
    fn stable_promotion_outranks_notified_nightly() {
        let state = UpdateState::new(None, Some(0), Some("1.2.0-nightly.9")).unwrap();
        let mut updater = scheduler("1.2.0-nightly.9", true, UpdateChannel::Nightly, state);
        let token = start(&mut updater, 60_000);
        assert_eq!(
            finish_release(&mut updater, token, 60_001, "v1.2.0", ReleaseKind::Stable,),
            CheckCompletion::Recorded
        );
        let persisted = updater.state_for_persistence();
        let claim = take_claim(updater.claim_after_completed_command(&persisted));
        assert_eq!(claim.version(), "1.2.0");
    }

    #[test]
    fn nightly_release_respects_the_configured_channel() {
        let mut stable = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut stable, 0);
        assert_eq!(
            finish_release(
                &mut stable,
                token,
                1,
                "v1.1.0-nightly.2",
                ReleaseKind::Nightly,
            ),
            CheckCompletion::Recorded
        );
        let state = stable.state_for_persistence();
        assert!(matches!(
            stable.claim_after_completed_command(&state),
            ClaimDecision::NoPendingVersion
        ));

        let mut nightly = scheduler(
            "1.0.0",
            true,
            UpdateChannel::Nightly,
            UpdateState::default(),
        );
        let token = start(&mut nightly, 0);
        assert_eq!(
            finish_release(
                &mut nightly,
                token,
                1,
                "v1.1.0-nightly.2",
                ReleaseKind::Nightly,
            ),
            CheckCompletion::Recorded
        );
        let state = nightly.state_for_persistence();
        assert_eq!(
            take_claim(nightly.claim_after_completed_command(&state)).version(),
            "1.1.0-nightly.2"
        );
    }

    #[test]
    fn superseded_claim_suppresses_or_retries_by_precedence() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut updater, 0);
        assert_eq!(
            finish_release(&mut updater, token, 1, "v2.0.0", ReleaseKind::Stable),
            CheckCompletion::Recorded
        );
        let local = updater.state_for_persistence();
        let claim = take_claim(updater.claim_after_completed_command(&local));
        let won_elsewhere = UpdateState::new(None, Some(2), Some("2.0.0+other")).unwrap();
        assert!(matches!(
            updater.confirm_notification(claim, ClaimPersistence::Superseded(won_elsewhere)),
            ClaimConfirmation::SuppressedByPersistedState
        ));

        let mut retry = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        let token = start(&mut retry, 0);
        assert_eq!(
            finish_release(&mut retry, token, 1, "v3.0.0", ReleaseKind::Stable),
            CheckCompletion::Recorded
        );
        let local = retry.state_for_persistence();
        let claim = take_claim(retry.claim_after_completed_command(&local));
        let older = UpdateState::new(None, Some(2), Some("2.0.0")).unwrap();
        assert!(matches!(
            retry.confirm_notification(claim, ClaimPersistence::Superseded(older)),
            ClaimConfirmation::RetryPending
        ));
        let merged = retry.state_for_persistence();
        assert_eq!(
            take_claim(retry.claim_after_completed_command(&merged)).version(),
            "3.0.0"
        );
    }

    #[test]
    fn persistence_merge_keeps_latest_attempt_and_highest_precedence() {
        let local = UpdateState::new(Some(15), Some(20), Some("2.0.0-nightly.4")).unwrap();
        let persisted = UpdateState::new(Some(25), Some(30), Some("2.0.0")).unwrap();
        let merged = local.merged_for_persistence(&persisted);
        assert_eq!(merged.last_reserved_check_ms(), Some(25));
        assert_eq!(merged.last_completed_check_ms(), Some(30));
        assert_eq!(merged.last_notified_version(), Some("2.0.0"));
    }

    #[test]
    fn exhausted_tokens_fail_closed() {
        let mut updater = scheduler("1.0.0", true, UpdateChannel::Stable, UpdateState::default());
        updater.next_check_token = None;
        assert!(matches!(
            reserve_from_local(&mut updater, 0),
            CheckReservationDecision::TokenExhausted
        ));

        updater.next_check_token = Some(1);
        let token = start(&mut updater, 0);
        assert_eq!(
            finish_release(&mut updater, token, 1, "v2.0.0", ReleaseKind::Stable),
            CheckCompletion::Recorded
        );
        updater.next_claim_id = None;
        let state = updater.state_for_persistence();
        assert!(matches!(
            updater.claim_after_completed_command(&state),
            ClaimDecision::TokenExhausted
        ));
    }

    #[test]
    fn public_debug_output_redacts_versions_and_tokens() {
        let mut updater = scheduler(
            "1.0.0+private-current",
            true,
            UpdateChannel::Stable,
            UpdateState::default(),
        );
        let reservation = take_reservation(reserve_from_local(&mut updater, 0));
        let CheckReservationConfirmation::Request(request) =
            updater.confirm_check_reservation(reservation, ReservationPersistence::Persisted)
        else {
            panic!("expected request")
        };
        let request_debug = format!("{request:?}");
        let token = request.into_token();
        let request_debug = format!("{request_debug} {token:?}");
        assert!(request_debug.contains("<redacted>"));
        assert!(!request_debug.contains("CheckToken(1)"));
        assert_eq!(
            finish_release(
                &mut updater,
                token,
                1,
                "v2.0.0+private-remote",
                ReleaseKind::Stable,
            ),
            CheckCompletion::Recorded
        );
        let state = updater.state_for_persistence();
        let claim = take_claim(updater.claim_after_completed_command(&state));
        let debug = format!("{updater:?} {claim:?}");
        assert!(!debug.contains("private-current"));
        assert!(!debug.contains("private-remote"));
        let confirmation = updater.confirm_notification(claim, ClaimPersistence::Persisted);
        assert!(!format!("{confirmation:?}").contains("private-remote"));
    }
}
