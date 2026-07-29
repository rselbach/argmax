//! Storage-neutral, in-memory command learning and ranking.
//!
//! Events and aggregate keys contain only owned standard-library values so a
//! persistence layer can store and restore them without coupling learning to a
//! command catalog or database implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

/// Seconds since the Unix epoch.
pub type Timestamp = u64;

/// Weight applied when only a global aggregate is available for a directory.
pub const GLOBAL_FALLBACK_WEIGHT: f64 = 0.7;

const HOUR_SECONDS: Timestamp = 60 * 60;
const DAY_SECONDS: Timestamp = 24 * HOUR_SECONDS;
const WEEK_SECONDS: Timestamp = 7 * DAY_SECONDS;
const MONTH_SECONDS: Timestamp = 30 * DAY_SECONDS;

/// Outcome of a submitted command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    /// The command completed successfully and contributes to frequency.
    Success,
    /// The command failed and updates recency without contributing frequency.
    Failure,
}

/// One storage-neutral observation submitted to the learning core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearningEvent {
    /// Exact submitted shell command.
    pub command: String,
    /// Recognized command/subcommand path, or the executable for unknown commands.
    pub skeleton: String,
    /// Active shell working directory at submission time.
    pub cwd: PathBuf,
    /// Submission time in seconds since the Unix epoch.
    pub timestamp: Timestamp,
    /// Observed command outcome.
    pub outcome: CommandOutcome,
    /// Previous recognized command skeleton, when one exists.
    pub prior_skeleton: Option<String>,
}

impl LearningEvent {
    /// Creates an event with an explicitly resolved command skeleton.
    #[must_use]
    pub fn new(
        command: impl Into<String>,
        skeleton: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timestamp: Timestamp,
        outcome: CommandOutcome,
    ) -> Self {
        Self {
            command: command.into(),
            skeleton: skeleton.into(),
            cwd: cwd.into(),
            timestamp,
            outcome,
            prior_skeleton: None,
        }
    }

    /// Creates an event using a caller-supplied skeleton resolver.
    ///
    /// The callback is the integration boundary for catalog-aware recognition;
    /// this module does not inspect flags or positional arguments itself.
    #[must_use]
    pub fn resolved(
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timestamp: Timestamp,
        outcome: CommandOutcome,
        resolve: impl FnOnce(&str) -> String,
    ) -> Self {
        let command = command.into();
        let skeleton = resolve(&command);
        Self::new(command, skeleton, cwd, timestamp, outcome)
    }

    /// Records the prior command skeleton used for transition learning.
    #[must_use]
    pub fn with_prior_skeleton(mut self, prior: impl Into<String>) -> Self {
        self.prior_skeleton = Some(prior.into());
        self
    }

    /// Validates the required event fields and canonical skeleton shape.
    ///
    /// # Errors
    ///
    /// Returns an error for a blank command or directory, or for a skeleton that
    /// is empty, contains control characters, or is not single-space separated.
    pub fn validate(&self) -> Result<(), LearningError> {
        if self.command.trim().is_empty() {
            return Err(LearningError::new("command", "must not be blank"));
        }
        if self.cwd.as_os_str().is_empty() {
            return Err(LearningError::new("cwd", "must not be empty"));
        }
        validate_skeleton("skeleton", &self.skeleton)?;
        if let Some(prior) = &self.prior_skeleton {
            validate_skeleton("prior_skeleton", prior)?;
        }
        Ok(())
    }
}

/// Directory scope represented by one aggregate row.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LearningScope {
    /// Aggregate across all recorded directories.
    Global,
    /// Aggregate for one exact working directory.
    Directory(PathBuf),
}

/// Stable key for a command-usage aggregate.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CommandAggregateKey {
    /// Global or exact-directory scope.
    pub scope: LearningScope,
    /// Canonical command skeleton.
    pub skeleton: String,
}

/// Stable key for a learned command transition.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TransitionAggregateKey {
    /// Global or exact-directory scope.
    pub scope: LearningScope,
    /// Canonical prior command skeleton.
    pub prior_skeleton: String,
    /// Canonical current command skeleton.
    pub current_skeleton: String,
}

/// Frequency and recency retained for one aggregate key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageAggregate {
    /// Number of successful observations.
    pub successful_count: u64,
    /// Newest successful or failed observation.
    pub last_used_at: Timestamp,
}

impl UsageAggregate {
    /// Creates an aggregate from its first observation.
    #[must_use]
    pub const fn new(timestamp: Timestamp, outcome: CommandOutcome) -> Self {
        Self {
            successful_count: match outcome {
                CommandOutcome::Success => 1,
                CommandOutcome::Failure => 0,
            },
            last_used_at: timestamp,
        }
    }

    /// Applies another observation, preserving newest recency for out-of-order data.
    pub fn observe(&mut self, timestamp: Timestamp, outcome: CommandOutcome) {
        if outcome == CommandOutcome::Success {
            self.successful_count = self.successful_count.saturating_add(1);
        }
        self.last_used_at = self.last_used_at.max(timestamp);
    }
}

/// Persistable aggregate state plus pure in-memory ranking operations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LearningState {
    /// Command usage keyed by global and directory scopes.
    pub commands: BTreeMap<CommandAggregateKey, UsageAggregate>,
    /// Prior-to-current transitions keyed by global and directory scopes.
    pub transitions: BTreeMap<TransitionAggregateKey, UsageAggregate>,
}

impl LearningState {
    /// Reconstructs state from storage-neutral aggregate maps.
    #[must_use]
    pub const fn from_aggregates(
        commands: BTreeMap<CommandAggregateKey, UsageAggregate>,
        transitions: BTreeMap<TransitionAggregateKey, UsageAggregate>,
    ) -> Self {
        Self {
            commands,
            transitions,
        }
    }

    /// Applies an event to global and exact-directory command and transition rows.
    ///
    /// # Errors
    ///
    /// Returns an error without changing state when the event is malformed.
    pub fn record(&mut self, event: &LearningEvent) -> Result<(), LearningError> {
        event.validate()?;

        for scope in event_scopes(&event.cwd) {
            let command_key = CommandAggregateKey {
                scope: scope.clone(),
                skeleton: event.skeleton.clone(),
            };
            observe(
                &mut self.commands,
                command_key,
                event.timestamp,
                event.outcome,
            );

            if let Some(prior_skeleton) = &event.prior_skeleton {
                let transition_key = TransitionAggregateKey {
                    scope,
                    prior_skeleton: prior_skeleton.clone(),
                    current_skeleton: event.skeleton.clone(),
                };
                observe(
                    &mut self.transitions,
                    transition_key,
                    event.timestamp,
                    event.outcome,
                );
            }
        }

        Ok(())
    }

    /// Scores and normalizes command frecency within the supplied candidate set.
    ///
    /// Exact-directory aggregates win when they contain a success. Otherwise the
    /// global aggregate is used with [`GLOBAL_FALLBACK_WEIGHT`]. Duplicate input
    /// candidates are collapsed, and equal scores sort by skeleton.
    #[must_use]
    pub fn frecency_scores<I, S>(
        &self,
        candidates: I,
        cwd: &Path,
        now: Timestamp,
    ) -> Vec<CandidateScore>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let candidates = unique_candidates(candidates);
        let mut scores: Vec<CandidateScore> = candidates
            .into_iter()
            .map(|skeleton| {
                let (aggregate, scope, weight) = self
                    .preferred_command_aggregate(&skeleton, cwd)
                    .map_or((None, None, 0.0), |(aggregate, scope, weight)| {
                        (Some(aggregate), Some(scope), weight)
                    });
                CandidateScore {
                    skeleton,
                    raw_score: aggregate
                        .map_or(0.0, |value| aggregate_frecency(value, now) * weight),
                    normalized_score: 0.0,
                    scope,
                }
            })
            .collect();
        normalize_and_sort(&mut scores);
        scores
    }

    /// Scores transitions for the deepest prior skeleton containing candidate data.
    ///
    /// If the exact prior has no successful transition to the supplied candidates,
    /// each parent is tried in turn. At the selected depth, local rows win per
    /// candidate and global rows receive the 0.7 fallback weight.
    #[must_use]
    pub fn transition_scores<I, S>(
        &self,
        prior_skeleton: &str,
        candidates: I,
        cwd: &Path,
        now: Timestamp,
    ) -> TransitionScores
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let candidates = unique_candidates(candidates);
        let matched_prior = skeleton_lineage(prior_skeleton)
            .into_iter()
            .find(|prior| self.has_transition_data(prior, &candidates, cwd));

        let mut scores: Vec<CandidateScore> = candidates
            .into_iter()
            .map(|skeleton| {
                let (aggregate, scope, weight) = matched_prior
                    .as_deref()
                    .and_then(|prior| self.preferred_transition_aggregate(prior, &skeleton, cwd))
                    .map_or((None, None, 0.0), |(aggregate, scope, weight)| {
                        (Some(aggregate), Some(scope), weight)
                    });
                CandidateScore {
                    skeleton,
                    raw_score: aggregate
                        .map_or(0.0, |value| aggregate_frecency(value, now) * weight),
                    normalized_score: 0.0,
                    scope,
                }
            })
            .collect();
        normalize_and_sort(&mut scores);

        TransitionScores {
            matched_prior,
            candidates: scores,
        }
    }

    fn preferred_command_aggregate<'a>(
        &'a self,
        skeleton: &str,
        cwd: &Path,
    ) -> Option<(&'a UsageAggregate, LearningScope, f64)> {
        let local_scope = LearningScope::Directory(cwd.to_path_buf());
        let local_key = CommandAggregateKey {
            scope: local_scope.clone(),
            skeleton: skeleton.to_owned(),
        };
        if let Some(aggregate) = successful(self.commands.get(&local_key)) {
            return Some((aggregate, local_scope, 1.0));
        }

        let global_key = CommandAggregateKey {
            scope: LearningScope::Global,
            skeleton: skeleton.to_owned(),
        };
        successful(self.commands.get(&global_key))
            .map(|aggregate| (aggregate, LearningScope::Global, GLOBAL_FALLBACK_WEIGHT))
    }

    fn preferred_transition_aggregate<'a>(
        &'a self,
        prior: &str,
        current: &str,
        cwd: &Path,
    ) -> Option<(&'a UsageAggregate, LearningScope, f64)> {
        let local_scope = LearningScope::Directory(cwd.to_path_buf());
        let local_key = TransitionAggregateKey {
            scope: local_scope.clone(),
            prior_skeleton: prior.to_owned(),
            current_skeleton: current.to_owned(),
        };
        if let Some(aggregate) = successful(self.transitions.get(&local_key)) {
            return Some((aggregate, local_scope, 1.0));
        }

        let global_key = TransitionAggregateKey {
            scope: LearningScope::Global,
            prior_skeleton: prior.to_owned(),
            current_skeleton: current.to_owned(),
        };
        successful(self.transitions.get(&global_key))
            .map(|aggregate| (aggregate, LearningScope::Global, GLOBAL_FALLBACK_WEIGHT))
    }

    fn has_transition_data(&self, prior: &str, candidates: &BTreeSet<String>, cwd: &Path) -> bool {
        candidates.iter().any(|current| {
            self.preferred_transition_aggregate(prior, current, cwd)
                .is_some()
        })
    }
}

/// One candidate's raw and candidate-set-normalized learning score.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateScore {
    /// Canonical candidate skeleton.
    pub skeleton: String,
    /// Frequency multiplied by the age band and scope weight.
    pub raw_score: f64,
    /// Raw score divided by the maximum raw score in this candidate set.
    pub normalized_score: f64,
    /// Aggregate scope used for this candidate, or none when it has no data.
    pub scope: Option<LearningScope>,
}

/// Candidate transition scores and the exact or parent prior that supplied them.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionScores {
    /// Deepest prior skeleton with successful candidate transition data.
    pub matched_prior: Option<String>,
    /// Deterministically ranked, normalized candidate scores.
    pub candidates: Vec<CandidateScore>,
}

/// Validation failure for a learning event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearningError {
    /// Invalid event field.
    pub field: &'static str,
    /// Human-readable reason suitable for diagnostics.
    pub message: String,
}

impl LearningError {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl fmt::Display for LearningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for LearningError {}

/// Returns the documented frecency multiplier for an observation age.
#[must_use]
pub const fn age_multiplier(age_seconds: Timestamp) -> u32 {
    if age_seconds <= HOUR_SECONDS {
        100
    } else if age_seconds <= DAY_SECONDS {
        50
    } else if age_seconds <= WEEK_SECONDS {
        20
    } else if age_seconds <= MONTH_SECONDS {
        5
    } else {
        1
    }
}

fn validate_skeleton(field: &'static str, skeleton: &str) -> Result<(), LearningError> {
    if skeleton.is_empty()
        || skeleton.split(' ').any(|token| {
            token.is_empty()
                || token
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
        })
    {
        return Err(LearningError::new(
            field,
            "must be non-empty tokens separated by single spaces",
        ));
    }
    Ok(())
}

fn event_scopes(cwd: &Path) -> [LearningScope; 2] {
    [
        LearningScope::Global,
        LearningScope::Directory(cwd.to_path_buf()),
    ]
}

fn observe<K: Ord>(
    aggregates: &mut BTreeMap<K, UsageAggregate>,
    key: K,
    timestamp: Timestamp,
    outcome: CommandOutcome,
) {
    aggregates
        .entry(key)
        .and_modify(|aggregate| aggregate.observe(timestamp, outcome))
        .or_insert_with(|| UsageAggregate::new(timestamp, outcome));
}

fn successful(aggregate: Option<&UsageAggregate>) -> Option<&UsageAggregate> {
    aggregate.filter(|value| value.successful_count > 0)
}

fn aggregate_frecency(aggregate: &UsageAggregate, now: Timestamp) -> f64 {
    let age = now.saturating_sub(aggregate.last_used_at);
    let bounded_count = u32::try_from(aggregate.successful_count).unwrap_or(u32::MAX);
    f64::from(bounded_count) * f64::from(age_multiplier(age))
}

fn unique_candidates<I, S>(candidates: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    candidates.into_iter().map(Into::into).collect()
}

fn normalize_and_sort(scores: &mut [CandidateScore]) {
    let maximum = scores
        .iter()
        .map(|candidate| candidate.raw_score)
        .fold(0.0, f64::max);
    if maximum > 0.0 {
        for candidate in &mut *scores {
            candidate.normalized_score = candidate.raw_score / maximum;
        }
    }
    scores.sort_by(|left, right| {
        right
            .normalized_score
            .total_cmp(&left.normalized_score)
            .then_with(|| left.skeleton.cmp(&right.skeleton))
    });
}

fn skeleton_lineage(skeleton: &str) -> Vec<String> {
    let mut lineage = Vec::new();
    let mut current = skeleton;
    while !current.is_empty() {
        lineage.push(current.to_owned());
        let Some((parent, _)) = current.rsplit_once(' ') else {
            break;
        };
        current = parent;
    }
    lineage
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = 4_000_000;
    const GREENDALE_LIBRARY: &str = "/home/troy/Greendale/library";
    const GREENDALE_STUDY_ROOM: &str = "/home/annie/Greendale/study-room";

    fn event(
        command: &str,
        skeleton: &str,
        cwd: &str,
        timestamp: Timestamp,
        outcome: CommandOutcome,
    ) -> LearningEvent {
        LearningEvent::new(command, skeleton, cwd, timestamp, outcome)
    }

    fn score<'a>(scores: &'a [CandidateScore], skeleton: &str) -> &'a CandidateScore {
        scores
            .iter()
            .find(|candidate| candidate.skeleton == skeleton)
            .unwrap()
    }

    fn assert_score(actual: f64, want: f64) {
        assert!(
            (actual - want).abs() <= f64::EPSILON,
            "got {actual}, want {want}"
        );
    }

    #[test]
    fn explicit_and_callback_skeleton_events_validate() {
        let explicit = event(
            "git commit -m 'Save Greendale'",
            "git commit",
            GREENDALE_LIBRARY,
            NOW,
            CommandOutcome::Success,
        );
        let resolved = LearningEvent::resolved(
            "git status --short",
            GREENDALE_LIBRARY,
            NOW,
            CommandOutcome::Success,
            |_| "git status".to_owned(),
        );

        assert_eq!(explicit.validate(), Ok(()));
        assert_eq!(resolved.skeleton, "git status");
        assert_eq!(resolved.validate(), Ok(()));

        let invalid = BTreeMap::from([
            (
                "blank command",
                event(
                    " ",
                    "git status",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Failure,
                ),
            ),
            (
                "empty cwd",
                event("git status", "git status", "", NOW, CommandOutcome::Failure),
            ),
            (
                "empty skeleton",
                event(
                    "git status",
                    "",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Failure,
                ),
            ),
            (
                "noncanonical skeleton",
                event(
                    "git status",
                    "git  status",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Failure,
                ),
            ),
        ]);
        for (name, invalid_event) in invalid {
            assert!(invalid_event.validate().is_err(), "{name}");
        }
    }

    #[test]
    fn successes_increment_frequency_and_failures_only_update_recency() {
        let events = BTreeMap::from([
            (
                "first success",
                event(
                    "git status",
                    "git status",
                    GREENDALE_LIBRARY,
                    NOW - 300,
                    CommandOutcome::Success,
                )
                .with_prior_skeleton("cd"),
            ),
            (
                "newer failure",
                event(
                    "git status --short",
                    "git status",
                    GREENDALE_LIBRARY,
                    NOW - 10,
                    CommandOutcome::Failure,
                )
                .with_prior_skeleton("cd"),
            ),
            (
                "older success applied last",
                event(
                    "git status --branch",
                    "git status",
                    GREENDALE_LIBRARY,
                    NOW - 200,
                    CommandOutcome::Success,
                )
                .with_prior_skeleton("cd"),
            ),
        ]);
        let mut state = LearningState::default();
        for event in events.values() {
            state.record(event).unwrap();
        }

        let local_scope = LearningScope::Directory(PathBuf::from(GREENDALE_LIBRARY));
        let command = state.commands[&CommandAggregateKey {
            scope: local_scope.clone(),
            skeleton: "git status".to_owned(),
        }];
        let transition = state.transitions[&TransitionAggregateKey {
            scope: local_scope,
            prior_skeleton: "cd".to_owned(),
            current_skeleton: "git status".to_owned(),
        }];

        assert_eq!(command.successful_count, 2);
        assert_eq!(command.last_used_at, NOW - 10);
        assert_eq!(transition, command);
        assert_eq!(
            state.commands[&CommandAggregateKey {
                scope: LearningScope::Global,
                skeleton: "git status".to_owned(),
            }],
            command
        );
    }

    #[test]
    fn age_band_boundaries_match_the_product_definition() {
        let cases = BTreeMap::from([
            ("01 current", (0, 100)),
            ("02 one hour", (HOUR_SECONDS, 100)),
            ("03 over one hour", (HOUR_SECONDS + 1, 50)),
            ("04 one day", (DAY_SECONDS, 50)),
            ("05 over one day", (DAY_SECONDS + 1, 20)),
            ("06 one week", (WEEK_SECONDS, 20)),
            ("07 over one week", (WEEK_SECONDS + 1, 5)),
            ("08 thirty days", (MONTH_SECONDS, 5)),
            ("09 older", (MONTH_SECONDS + 1, 1)),
        ]);

        for (name, (age, want)) in cases {
            assert_eq!(age_multiplier(age), want, "{name}");
        }
    }

    #[test]
    fn frecency_prefers_local_then_weighted_global_and_normalizes() {
        let mut state = LearningState::default();
        let events = BTreeMap::from([
            (
                "global cargo",
                event(
                    "cargo test",
                    "cargo test",
                    GREENDALE_STUDY_ROOM,
                    NOW,
                    CommandOutcome::Success,
                ),
            ),
            (
                "local git",
                event(
                    "git status",
                    "git status",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Success,
                ),
            ),
            (
                "local git failure",
                event(
                    "git status --short",
                    "git status",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Failure,
                ),
            ),
        ]);
        for event in events.values() {
            state.record(event).unwrap();
        }

        let scores = state.frecency_scores(
            ["unknown", "cargo test", "git status", "cargo test"],
            Path::new(GREENDALE_LIBRARY),
            NOW,
        );

        assert_eq!(
            scores
                .iter()
                .map(|candidate| candidate.skeleton.as_str())
                .collect::<Vec<_>>(),
            ["git status", "cargo test", "unknown"]
        );
        assert_score(score(&scores, "git status").normalized_score, 1.0);
        assert_score(score(&scores, "cargo test").normalized_score, 0.7);
        assert_score(score(&scores, "unknown").normalized_score, 0.0);
        assert!(matches!(
            score(&scores, "git status").scope,
            Some(LearningScope::Directory(_))
        ));
        assert_eq!(
            score(&scores, "cargo test").scope,
            Some(LearningScope::Global)
        );
    }

    #[test]
    fn failure_recency_affects_existing_frequency_but_failure_only_stays_zero() {
        let mut state = LearningState::default();
        let events = BTreeMap::from([
            (
                "old success",
                event(
                    "just study",
                    "just study",
                    GREENDALE_LIBRARY,
                    NOW - MONTH_SECONDS - 1,
                    CommandOutcome::Success,
                ),
            ),
            (
                "recent failure",
                event(
                    "just study",
                    "just study",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Failure,
                ),
            ),
            (
                "troy typo",
                event(
                    "just paintballl",
                    "just paintballl",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Failure,
                ),
            ),
        ]);
        for event in events.values() {
            state.record(event).unwrap();
        }

        let scores = state.frecency_scores(
            ["just study", "just paintballl"],
            Path::new(GREENDALE_LIBRARY),
            NOW,
        );
        assert_score(score(&scores, "just study").raw_score, 100.0);
        assert_score(score(&scores, "just paintballl").raw_score, 0.0);
    }

    #[test]
    fn transitions_fall_back_to_the_deepest_parent_with_data() {
        let mut state = LearningState::default();
        let events = BTreeMap::from([
            (
                "local fetch",
                event(
                    "git fetch greendale",
                    "git fetch",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Success,
                )
                .with_prior_skeleton("git remote"),
            ),
            (
                "remote branch elsewhere",
                event(
                    "git branch",
                    "git branch",
                    GREENDALE_STUDY_ROOM,
                    NOW,
                    CommandOutcome::Success,
                )
                .with_prior_skeleton("git remote"),
            ),
        ]);
        for event in events.values() {
            state.record(event).unwrap();
        }

        let ranking = state.transition_scores(
            "git remote add",
            ["git branch", "git fetch", "git status"],
            Path::new(GREENDALE_LIBRARY),
            NOW,
        );

        assert_eq!(ranking.matched_prior.as_deref(), Some("git remote"));
        assert_eq!(
            ranking
                .candidates
                .iter()
                .map(|candidate| candidate.skeleton.as_str())
                .collect::<Vec<_>>(),
            ["git fetch", "git branch", "git status"]
        );
        assert_score(
            score(&ranking.candidates, "git fetch").normalized_score,
            1.0,
        );
        assert_score(
            score(&ranking.candidates, "git branch").normalized_score,
            0.7,
        );
    }

    #[test]
    fn exact_transition_data_wins_and_equal_scores_sort_by_skeleton() {
        let mut state = LearningState::default();
        let events = BTreeMap::from([
            (
                "parent data",
                event(
                    "git fetch",
                    "git fetch",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Success,
                )
                .with_prior_skeleton("git remote"),
            ),
            (
                "troy exact first",
                event(
                    "git branch",
                    "git branch",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Success,
                )
                .with_prior_skeleton("git remote add"),
            ),
            (
                "troy exact second",
                event(
                    "git checkout",
                    "git checkout",
                    GREENDALE_LIBRARY,
                    NOW,
                    CommandOutcome::Success,
                )
                .with_prior_skeleton("git remote add"),
            ),
        ]);
        for event in events.values() {
            state.record(event).unwrap();
        }

        let ranking = state.transition_scores(
            "git remote add",
            ["git fetch", "git checkout", "git branch"],
            Path::new(GREENDALE_LIBRARY),
            NOW,
        );

        assert_eq!(ranking.matched_prior.as_deref(), Some("git remote add"));
        assert_eq!(
            ranking
                .candidates
                .iter()
                .map(|candidate| candidate.skeleton.as_str())
                .collect::<Vec<_>>(),
            ["git branch", "git checkout", "git fetch"]
        );
        assert_score(score(&ranking.candidates, "git fetch").raw_score, 0.0);
    }

    #[test]
    fn malformed_events_do_not_partially_mutate_aggregate_maps() {
        let mut state = LearningState::default();
        let bad = event(
            "git status",
            "git  status",
            GREENDALE_LIBRARY,
            NOW,
            CommandOutcome::Success,
        )
        .with_prior_skeleton("cd");

        assert!(state.record(&bad).is_err());
        assert!(state.commands.is_empty());
        assert!(state.transitions.is_empty());
    }
}
