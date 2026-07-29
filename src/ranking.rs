//! Deterministic composite ranking for completion candidates.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use crate::completion::Suggestion;
use crate::context::score_workspace_context;
use crate::learning::{LearningState, Timestamp};
use crate::providers::WorkspaceContext;

/// Normalized per-candidate inputs to the parity scoring formula.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RankSignals {
    /// Static source/spec priority.
    pub source_priority: f64,
    /// Match with detected workspace ecosystems.
    pub workspace_context: f64,
    /// Prefix/fuzzy query quality.
    pub match_quality: f64,
    /// Normalized local/global frecency.
    pub frecency: f64,
    /// Normalized prior-command transition strength.
    pub transition: f64,
}

/// Inspectable breakdown used by developer diagnostics and ranking tests.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScoreBreakdown {
    /// Clamped static component before weighting.
    pub source_priority: f64,
    /// Clamped workspace component before weighting.
    pub workspace_context: f64,
    /// Clamped match component before weighting.
    pub match_quality: f64,
    /// Clamped frecency component before weighting.
    pub frecency: f64,
    /// Clamped transition component before weighting.
    pub transition: f64,
    /// Weighted total in the inclusive range 0–1.
    pub total: f64,
}

impl RankSignals {
    /// Applies the PRD's 30/25/20/15/10 weights.
    #[must_use]
    pub fn breakdown(self) -> ScoreBreakdown {
        let source_priority = clamp(self.source_priority);
        let workspace_context = clamp(self.workspace_context);
        let match_quality = clamp(self.match_quality);
        let frecency = clamp(self.frecency);
        let transition = clamp(self.transition);
        let total = source_priority.mul_add(
            0.30,
            workspace_context.mul_add(
                0.25,
                match_quality.mul_add(0.20, frecency.mul_add(0.15, transition * 0.10)),
            ),
        );
        ScoreBreakdown {
            source_priority,
            workspace_context,
            match_quality,
            frecency,
            transition,
            total,
        }
    }
}

/// Candidate paired with its stable diagnostic score.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedSuggestion {
    /// Inert suggestion.
    pub suggestion: Suggestion,
    /// Component score breakdown.
    pub score: ScoreBreakdown,
}

/// One merged candidate plus the canonical data needed for local ranking.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalRankingCandidate {
    /// Inert suggestion to rank.
    pub suggestion: Suggestion,
    /// Canonical command/subcommand path used for learning and context.
    pub skeleton: String,
    /// Provider-computed query match quality in the inclusive range zero to one.
    pub match_quality: f64,
}

impl LocalRankingCandidate {
    /// Creates a candidate for composite local ranking.
    #[must_use]
    pub fn new(suggestion: Suggestion, skeleton: impl Into<String>, match_quality: f64) -> Self {
        Self {
            suggestion,
            skeleton: skeleton.into(),
            match_quality,
        }
    }
}

/// Session-local inputs shared by every candidate in one ranking pass.
#[derive(Clone, Copy)]
pub struct LocalRankingContext<'a> {
    /// Cached workspace signatures for the active directory.
    pub workspace: &'a WorkspaceContext,
    /// Learned command and transition aggregates.
    pub learning: &'a LearningState,
    /// Exact active working directory used for local aggregate preference.
    pub cwd: &'a Path,
    /// Current Unix timestamp in seconds.
    pub now: Timestamp,
    /// Prior canonical command skeleton, when the session has one.
    pub prior_skeleton: Option<&'a str>,
}

/// Composite ranking output plus the transition lineage used for diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalRankingResult {
    /// Candidates after deterministic composite ranking and result limiting.
    pub candidates: Vec<RankedSuggestion>,
    /// Exact or parent prior skeleton that supplied transition data.
    pub matched_prior_skeleton: Option<String>,
}

/// Combines static, workspace, match, frecency, and transition signals.
///
/// Learning scores are normalized over the complete merged candidate set before
/// the caller's result limit is applied. The suggestion's static priority is the
/// source/static component; provider confidence remains merge metadata.
#[must_use]
pub fn rank_with_local_intelligence(
    candidates: impl IntoIterator<Item = LocalRankingCandidate>,
    context: LocalRankingContext<'_>,
    max_suggestions: usize,
) -> LocalRankingResult {
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    let skeletons = candidates
        .iter()
        .map(|candidate| candidate.skeleton.clone())
        .collect::<Vec<_>>();
    let frecency = context
        .learning
        .frecency_scores(skeletons.iter().cloned(), context.cwd, context.now)
        .into_iter()
        .map(|score| (score.skeleton, score.normalized_score))
        .collect::<BTreeMap<_, _>>();
    let transitions = context.prior_skeleton.map_or_else(
        || (None, BTreeMap::new()),
        |prior| {
            let scored = context.learning.transition_scores(
                prior,
                skeletons.iter().cloned(),
                context.cwd,
                context.now,
            );
            let by_skeleton = scored
                .candidates
                .into_iter()
                .map(|score| (score.skeleton, score.normalized_score))
                .collect();
            (scored.matched_prior, by_skeleton)
        },
    );

    let mut ranked = candidates
        .into_iter()
        .map(|candidate| {
            let signals = RankSignals {
                source_priority: candidate.suggestion.static_priority(),
                workspace_context: score_workspace_context(context.workspace, &candidate.skeleton)
                    .normalized_score,
                match_quality: candidate.match_quality,
                frecency: frecency.get(&candidate.skeleton).copied().unwrap_or(0.0),
                transition: transitions
                    .1
                    .get(&candidate.skeleton)
                    .copied()
                    .unwrap_or(0.0),
            };
            RankedSuggestion {
                suggestion: candidate.suggestion,
                score: signals.breakdown(),
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_by(compare_ranked);
    ranked.truncate(max_suggestions.clamp(1, 500));

    LocalRankingResult {
        candidates: ranked,
        matched_prior_skeleton: transitions.0,
    }
}

/// Ranks suggestions deterministically using caller-provided local signals.
#[must_use]
pub fn rank_suggestions(
    suggestions: impl IntoIterator<Item = Suggestion>,
    signals: impl Fn(&Suggestion) -> RankSignals,
) -> Vec<RankedSuggestion> {
    let mut ranked: Vec<_> = suggestions
        .into_iter()
        .map(|suggestion| {
            let score = signals(&suggestion).breakdown();
            RankedSuggestion { suggestion, score }
        })
        .collect();

    ranked.sort_by(compare_ranked);
    ranked
}

/// Ranks first, then applies the validated UI result limit.
#[must_use]
pub fn rank_suggestions_limited(
    suggestions: impl IntoIterator<Item = Suggestion>,
    signals: impl Fn(&Suggestion) -> RankSignals,
    max_suggestions: usize,
) -> Vec<RankedSuggestion> {
    let mut ranked = rank_suggestions(suggestions, signals);
    ranked.truncate(max_suggestions.clamp(1, 500));
    ranked
}

fn compare_ranked(left: &RankedSuggestion, right: &RankedSuggestion) -> Ordering {
    descending(left.score.total, right.score.total)
        .then_with(|| descending(left.score.transition, right.score.transition))
        .then_with(|| descending(left.score.frecency, right.score.frecency))
        .then_with(|| descending(left.score.workspace_context, right.score.workspace_context))
        .then_with(|| left.suggestion.identity.cmp(&right.suggestion.identity))
}

fn descending(left: f64, right: f64) -> Ordering {
    right.total_cmp(&left)
}

fn clamp(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Success-frequency decay multiplier for the age since last use.
#[must_use]
pub const fn frecency_multiplier(age_seconds: u64) -> u64 {
    match age_seconds {
        0..=3_600 => 100,
        3_601..=86_400 => 50,
        86_401..=604_800 => 20,
        604_801..=2_592_000 => 5,
        _ => 1,
    }
}

/// Applies the 0.7 global fallback only when a local signal is absent.
#[must_use]
pub fn local_or_global(local: Option<f64>, global: Option<f64>) -> f64 {
    local
        .or_else(|| global.map(|value| value * 0.7))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::completion::{CompletionQuery, InsertionBehavior, SuggestionSource, TextEdit};
    use crate::learning::{CommandOutcome, LearningEvent};
    use crate::providers::{WorkspaceKind, WorkspaceSignature};

    fn suggestion(identity: &str) -> Suggestion {
        let query = CompletionQuery::new("g", 1, Path::new("/tmp"), 1).unwrap();
        let replacement = format!("g{identity}");
        let candidate = Suggestion::new(
            TextEdit {
                range: 0..query.cursor,
                replacement: replacement.clone(),
            },
            replacement,
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            identity,
        );
        assert!(candidate.resulting_line(&query).is_ok());
        candidate
    }

    fn local_candidate(skeleton: &str) -> LocalRankingCandidate {
        LocalRankingCandidate::new(suggestion(skeleton), skeleton, 1.0)
    }

    fn workspace(kinds: &[WorkspaceKind]) -> WorkspaceContext {
        let cwd = PathBuf::from("/home/troy/Greendale");
        WorkspaceContext {
            cwd: cwd.clone(),
            signatures: kinds
                .iter()
                .map(|kind| WorkspaceSignature {
                    kind: *kind,
                    root: cwd.clone(),
                    marker: cwd.join(format!("{kind:?}")),
                })
                .collect(),
        }
    }

    #[test]
    fn uses_documented_weights_and_clamps() {
        let score = RankSignals {
            source_priority: 2.0,
            workspace_context: 1.0,
            match_quality: 1.0,
            frecency: 1.0,
            transition: f64::NAN,
        }
        .breakdown();
        assert!((score.total - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn ties_resolve_by_transition_then_identity() {
        let ranked = rank_suggestions([suggestion("beta"), suggestion("alpha")], |candidate| {
            RankSignals {
                source_priority: 0.5,
                transition: f64::from(candidate.identity == "beta"),
                ..RankSignals::default()
            }
        });
        assert_eq!(ranked[0].suggestion.identity, "beta");
    }

    #[test]
    fn decay_boundaries_match_product_contract() {
        assert_eq!(frecency_multiplier(3_600), 100);
        assert_eq!(frecency_multiplier(3_601), 50);
        assert_eq!(frecency_multiplier(86_401), 20);
        assert_eq!(frecency_multiplier(604_801), 5);
        assert_eq!(frecency_multiplier(2_592_001), 1);
    }

    #[test]
    fn global_signal_is_only_a_fallback() {
        assert!((local_or_global(Some(0.2), Some(1.0)) - 0.2).abs() < f64::EPSILON);
        assert!((local_or_global(None, Some(1.0)) - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn ui_limit_is_applied_after_ranking() {
        let suggestions: Vec<_> = (0..101)
            .map(|index| {
                let identity = if index == 100 {
                    "zzz-best".to_owned()
                } else {
                    format!("item-{index:03}")
                };
                suggestion(&identity)
            })
            .collect();
        let ranked = rank_suggestions_limited(
            suggestions,
            |candidate| RankSignals {
                source_priority: f64::from(candidate.identity == "zzz-best"),
                ..RankSignals::default()
            },
            100,
        );

        assert_eq!(ranked.len(), 100);
        assert_eq!(ranked[0].suggestion.identity, "zzz-best");
    }

    #[test]
    fn local_ranking_boosts_matches_and_penalizes_cold_starts() {
        let workspace = workspace(&[WorkspaceKind::Git, WorkspaceKind::Rust]);
        let learning = LearningState::default();
        let context = LocalRankingContext {
            workspace: &workspace,
            learning: &learning,
            cwd: &workspace.cwd,
            now: 4_000_000,
            prior_skeleton: None,
        };

        let ranked = rank_with_local_intelligence(
            [
                local_candidate("git init"),
                local_candidate("echo"),
                local_candidate("cargo test"),
            ],
            context,
            100,
        );

        assert_eq!(
            ranked
                .candidates
                .iter()
                .map(|candidate| candidate.suggestion.identity())
                .collect::<Vec<_>>(),
            ["cargo test", "echo", "git init"]
        );
        assert_eq!(
            ranked
                .candidates
                .iter()
                .map(|candidate| candidate.score.workspace_context)
                .collect::<Vec<_>>(),
            [1.0, 0.5, 0.0]
        );
    }

    #[test]
    fn local_ranking_normalizes_learning_before_limiting() {
        const NOW: Timestamp = 4_000_000;
        let workspace = workspace(&[]);
        let mut learning = LearningState::default();
        for event in [
            LearningEvent::new(
                "echo one",
                "echo",
                &workspace.cwd,
                NOW,
                CommandOutcome::Success,
            ),
            LearningEvent::new(
                "echo two",
                "echo",
                &workspace.cwd,
                NOW,
                CommandOutcome::Success,
            ),
            LearningEvent::new(
                "cargo test",
                "cargo test",
                &workspace.cwd,
                NOW,
                CommandOutcome::Success,
            )
            .with_prior_skeleton("git status"),
        ] {
            learning.record(&event).unwrap();
        }
        let context = LocalRankingContext {
            workspace: &workspace,
            learning: &learning,
            cwd: &workspace.cwd,
            now: NOW,
            prior_skeleton: Some("git status porcelain"),
        };

        let ranked = rank_with_local_intelligence(
            [local_candidate("echo"), local_candidate("cargo test")],
            context,
            1,
        );

        assert_eq!(ranked.matched_prior_skeleton.as_deref(), Some("git status"));
        assert_eq!(ranked.candidates.len(), 1);
        assert_eq!(ranked.candidates[0].suggestion.identity(), "cargo test");
        assert!((ranked.candidates[0].score.frecency - 0.5).abs() <= f64::EPSILON);
        assert!((ranked.candidates[0].score.transition - 1.0).abs() <= f64::EPSILON);
    }
}
