//! Deterministic composite ranking for completion candidates.

use std::cmp::Ordering;

use crate::completion::Suggestion;

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
    use std::path::Path;

    use super::*;
    use crate::completion::{CompletionQuery, InsertionBehavior, SuggestionSource, TextEdit};

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
}
