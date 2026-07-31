//! Menu selection and ghost-text invariants independent of terminal rendering.

use std::fmt;

use crate::completion::{InsertionBehavior, Suggestion};

/// Result of applying an asynchronous provider update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateOutcome {
    /// Current-generation candidates were accepted.
    Applied,
    /// A stale generation was ignored.
    Stale,
    /// Navigation had frozen a selection that the update did not retain.
    SelectionConflict,
}

/// Selected candidate state for one immutable query generation.
#[derive(Clone, PartialEq)]
pub struct SelectionState {
    generation: u64,
    candidates: Box<[Suggestion]>,
    selected: Option<usize>,
    navigated: bool,
    frozen_order: Option<Box<[SelectionKey]>>,
    layer_enabled: bool,
    hidden_until_change: bool,
}

#[derive(Clone, Eq, PartialEq)]
struct SelectionKey {
    range_start: usize,
    range_end: usize,
    replacement: Box<str>,
    insertion: InsertionBehavior,
}

impl fmt::Debug for SelectionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectionKey")
            .field("range_start", &self.range_start)
            .field("range_end", &self.range_end)
            .field("replacement_bytes", &self.replacement.len())
            .field("insertion", &self.insertion)
            .finish()
    }
}

impl SelectionKey {
    fn from_candidate(candidate: &Suggestion) -> Self {
        Self {
            range_start: candidate.edit().range.start,
            range_end: candidate.edit().range.end,
            replacement: candidate.edit().replacement.clone().into_boxed_str(),
            insertion: candidate.insertion(),
        }
    }

    fn matches(&self, candidate: &Suggestion) -> bool {
        self.range_start == candidate.edit().range.start
            && self.range_end == candidate.edit().range.end
            && self.replacement.as_ref() == candidate.edit().replacement
            && self.insertion == candidate.insertion()
    }
}

impl Default for SelectionState {
    fn default() -> Self {
        Self {
            generation: 0,
            candidates: Box::default(),
            selected: None,
            navigated: false,
            frozen_order: None,
            layer_enabled: true,
            hidden_until_change: false,
        }
    }
}

impl fmt::Debug for SelectionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectionState")
            .field("generation", &self.generation)
            .field("candidate_count", &self.candidates.len())
            .field("selected", &self.selected)
            .field("navigated", &self.navigated)
            .field(
                "frozen_candidate_count",
                &self.frozen_order.as_ref().map(|order| order.len()),
            )
            .field("layer_enabled", &self.layer_enabled)
            .field("hidden_until_change", &self.hidden_until_change)
            .finish()
    }
}

impl SelectionState {
    /// Starts a new query, selecting its first ranked result.
    pub(crate) fn begin_query(&mut self, generation: u64, candidates: Vec<Suggestion>) {
        self.generation = generation;
        self.selected = (!candidates.is_empty()).then_some(0);
        self.candidates = compact_candidates(candidates);
        self.navigated = false;
        self.frozen_order = None;
        self.hidden_until_change = false;
    }

    /// Applies a cumulative current-generation provider update.
    ///
    /// Before navigation, the newly ranked top result becomes selected. After
    /// navigation, the selected edit and insertion behavior remain authoritative.
    /// An update that omits those immutable semantics is rejected rather than
    /// moving the user's selection.
    #[cfg(test)]
    fn apply_update(&mut self, generation: u64, candidates: Vec<Suggestion>) -> UpdateOutcome {
        self.apply_ranked_update(generation, candidates, usize::MAX)
    }

    /// Applies a full ranked update and retains a bounded rendering window.
    ///
    /// Before navigation, the result limit follows the newest rank. After
    /// navigation, surviving visible edits keep their prior order and newly
    /// ranked edits append only into free rows. The explicitly selected edit
    /// must remain present.
    pub(crate) fn apply_ranked_update(
        &mut self,
        generation: u64,
        candidates: Vec<Suggestion>,
        max_candidates: usize,
    ) -> UpdateOutcome {
        if generation != self.generation {
            return UpdateOutcome::Stale;
        }

        let selected = if self.navigated {
            let Some(frozen_order) = self.frozen_order.as_deref() else {
                return UpdateOutcome::SelectionConflict;
            };
            let Some(selected_key) = self
                .selected
                .and_then(|selected| frozen_order.get(selected))
            else {
                return UpdateOutcome::SelectionConflict;
            };

            let mut remaining = candidates.into_iter().map(Some).collect::<Vec<_>>();
            let mut retained = Vec::with_capacity(max_candidates.min(remaining.len()));
            for key in frozen_order {
                let Some(index) = remaining.iter().position(|candidate| {
                    candidate
                        .as_ref()
                        .is_some_and(|candidate| key.matches(candidate))
                }) else {
                    continue;
                };
                let Some(candidate) = remaining[index].take() else {
                    continue;
                };
                retained.push(candidate);
            }

            let Some(selected) = retained
                .iter()
                .position(|candidate| selected_key.matches(candidate))
            else {
                return UpdateOutcome::SelectionConflict;
            };
            if selected >= max_candidates {
                return UpdateOutcome::SelectionConflict;
            }

            retained.truncate(max_candidates);
            for candidate in remaining.into_iter().flatten() {
                if retained.len() == max_candidates {
                    break;
                }
                retained.push(candidate);
            }

            self.candidates = compact_candidates(retained);
            Some(selected)
        } else {
            let mut candidates = candidates;
            candidates.truncate(max_candidates);
            let selected = (!candidates.is_empty()).then_some(0);
            self.candidates = compact_candidates(candidates);
            selected
        };

        self.selected = selected;
        if self.navigated {
            self.freeze_visible_order();
        }
        UpdateOutcome::Applied
    }

    /// Moves selection up without wrapping.
    pub(crate) fn up(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = Some(selected.saturating_sub(1));
        self.navigated = true;
        self.freeze_visible_order();
    }

    /// Moves selection down without wrapping.
    pub(crate) fn down(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        // A selection currently implies a nonempty candidate list, but relying
        // on that here would turn any future change to that invariant into a
        // panic on an arrow key.
        let Some(last) = self.candidates.len().checked_sub(1) else {
            return;
        };
        self.selected = Some((selected + 1).min(last));
        self.navigated = true;
        self.freeze_visible_order();
    }

    /// Hides menu and ghost text until the buffer changes.
    pub(crate) fn escape(&mut self) {
        self.hidden_until_change = true;
    }

    /// Makes current candidates eligible after a buffer-changing key.
    pub(crate) fn buffer_changed(&mut self) {
        self.hidden_until_change = false;
    }

    /// Toggles the session suggestion layer. The setting persists across queries.
    pub(crate) fn toggle_visible(&mut self) {
        self.layer_enabled = !self.layer_enabled;
        self.hidden_until_change = false;
    }

    /// Whether Shift+Tab has enabled suggestions for this session.
    #[must_use]
    pub const fn layer_enabled(&self) -> bool {
        self.layer_enabled
    }

    /// Current selected candidate, shared by menu and ghost rendering.
    #[must_use]
    pub fn selected(&self) -> Option<&Suggestion> {
        self.selected.and_then(|index| self.candidates.get(index))
    }

    /// Authoritative bounded candidate window used by menu rendering.
    #[must_use]
    pub const fn candidates(&self) -> &[Suggestion] {
        &self.candidates
    }

    /// Current selected row.
    #[must_use]
    pub const fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Whether the menu/suggestion layer should render.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.layer_enabled && !self.hidden_until_change && self.selected.is_some()
    }

    /// Current query generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    fn freeze_visible_order(&mut self) {
        self.frozen_order = Some(
            self.candidates
                .iter()
                .map(SelectionKey::from_candidate)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
    }
}

fn compact_candidates(candidates: Vec<Suggestion>) -> Box<[Suggestion]> {
    candidates
        .into_iter()
        .map(compact_candidate)
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn compact_candidate(mut candidate: Suggestion) -> Suggestion {
    candidate.edit.replacement = compact_string(candidate.edit.replacement);
    candidate.display = compact_string(candidate.display);
    candidate.description = compact_string(candidate.description);
    candidate.icon = compact_string(candidate.icon);
    candidate.identity = compact_string(candidate.identity);
    candidate
}

fn compact_string(value: String) -> String {
    value.into_boxed_str().into_string()
}

/// Returns the candidate suffix when it begins with the user's logical prefix,
/// comparing case-insensitively while preserving candidate bytes.
///
/// Ghost acceptance appends only this suffix, so the user's exact prefix casing
/// and spacing remain unchanged.
#[must_use]
pub fn ghost_suffix<'a>(prefix: &str, candidate: &'a str) -> Option<&'a str> {
    let mut candidate_chars = candidate.char_indices();
    let mut suffix_start = 0;

    for prefix_char in prefix.chars() {
        let (candidate_index, candidate_char) = candidate_chars.next()?;
        if prefix_char.to_lowercase().to_string() != candidate_char.to_lowercase().to_string() {
            return None;
        }
        suffix_start = candidate_index + candidate_char.len_utf8();
    }

    (suffix_start < candidate.len()).then(|| &candidate[suffix_start..])
}

#[cfg(test)]
mod tests {
    use crate::completion::{InsertionBehavior, SuggestionSource, TextEdit};

    use super::*;

    fn candidate(identity: &str) -> Suggestion {
        Suggestion::new(
            TextEdit {
                range: 0..1,
                replacement: identity.into(),
            },
            identity,
            String::new(),
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            identity,
        )
    }

    #[test]
    fn selection_does_not_wrap() {
        let mut state = SelectionState::default();
        state.begin_query(1, vec![candidate("alpha"), candidate("beta")]);
        state.up();
        assert_eq!(state.selected_index(), Some(0));
        state.down();
        state.down();
        assert_eq!(state.selected_index(), Some(1));
    }

    #[test]
    fn navigation_freezes_edit_semantics_across_late_update() {
        let mut state = SelectionState::default();
        state.begin_query(1, vec![candidate("alpha"), candidate("beta")]);
        state.down();
        let mut renamed = candidate("beta");
        renamed.identity = "provider-controlled-new-identity".into();
        let outcome =
            state.apply_update(1, vec![candidate("aardvark"), candidate("alpha"), renamed]);
        assert_eq!(outcome, UpdateOutcome::Applied);
        assert_eq!(
            state
                .candidates()
                .iter()
                .map(Suggestion::identity)
                .collect::<Vec<_>>(),
            ["alpha", "provider-controlled-new-identity", "aardvark"]
        );
        assert_eq!(
            state.selected().unwrap().identity,
            "provider-controlled-new-identity"
        );
        assert_eq!(state.selected_index(), Some(1));
    }

    #[test]
    fn duplicate_identities_cannot_redirect_a_frozen_selection() {
        let mut state = SelectionState::default();
        let mut alpha = candidate("alpha");
        let mut beta = candidate("beta");
        alpha.identity = "duplicate".into();
        beta.identity = "duplicate".into();
        state.begin_query(1, vec![alpha.clone(), beta.clone()]);
        state.down();

        assert_eq!(
            state.apply_update(1, vec![beta.clone(), alpha]),
            UpdateOutcome::Applied
        );
        assert_eq!(state.selected().unwrap().edit().replacement, "beta");
        assert_eq!(state.selected_index(), Some(1));
    }

    #[test]
    fn late_results_append_without_displacing_the_frozen_visible_order() {
        let mut state = SelectionState::default();
        state.begin_query(1, vec![candidate("alpha"), candidate("beta")]);
        state.down();

        let outcome = state.apply_ranked_update(
            1,
            vec![candidate("aardvark"), candidate("alpha"), candidate("beta")],
            2,
        );
        assert_eq!(outcome, UpdateOutcome::Applied);
        assert_eq!(state.candidates().len(), 2);
        assert_eq!(state.candidates()[0].identity(), "alpha");
        assert_eq!(state.selected().unwrap().identity(), "beta");
        assert_eq!(state.selected_index(), Some(1));
    }

    #[test]
    fn vanished_unselected_rows_are_replaced_only_at_the_end() {
        let mut state = SelectionState::default();
        state.begin_query(
            1,
            vec![candidate("alpha"), candidate("beta"), candidate("gamma")],
        );
        state.down();

        assert_eq!(
            state.apply_ranked_update(
                1,
                vec![candidate("aardvark"), candidate("beta"), candidate("gamma")],
                3,
            ),
            UpdateOutcome::Applied
        );
        assert_eq!(
            state
                .candidates()
                .iter()
                .map(Suggestion::identity)
                .collect::<Vec<_>>(),
            ["beta", "gamma", "aardvark"]
        );
        assert_eq!(state.selected_index(), Some(0));
    }

    #[test]
    fn debug_output_redacts_candidate_and_frozen_edit_text() {
        let mut state = SelectionState::default();
        state.begin_query(1, vec![candidate("hunter2")]);
        state.down();

        let debug = format!("{state:?}");
        assert!(!debug.contains("hunter2"));
        assert!(debug.contains("candidate_count: 1"));
    }

    #[test]
    fn stale_update_cannot_replace_selection() {
        let mut state = SelectionState::default();
        state.begin_query(2, vec![candidate("alpha")]);
        assert_eq!(
            state.apply_update(1, vec![candidate("stale")]),
            UpdateOutcome::Stale
        );
        assert_eq!(state.selected().unwrap().identity, "alpha");
    }

    #[test]
    fn escape_hides_until_buffer_change() {
        let mut state = SelectionState::default();
        state.begin_query(1, vec![candidate("alpha")]);
        state.escape();
        assert!(!state.is_visible());
        state.buffer_changed();
        assert!(state.is_visible());
    }

    #[test]
    fn menu_toggle_persists_across_queries() {
        let mut state = SelectionState::default();
        state.begin_query(1, vec![candidate("alpha")]);
        state.toggle_visible();
        assert!(!state.layer_enabled());
        assert!(!state.is_visible());

        state.begin_query(2, vec![candidate("beta")]);
        assert!(!state.is_visible());
        state.toggle_visible();
        assert!(state.layer_enabled());
        assert!(state.is_visible());
    }

    #[test]
    fn ghost_suffix_preserves_original_candidate_bytes() {
        assert_eq!(ghost_suffix("Git Ch", "git checkout"), Some("eckout"));
        assert_eq!(ghost_suffix("git checkout", "git checkout"), None);
        assert_eq!(ghost_suffix("git x", "git checkout"), None);
        assert_eq!(ghost_suffix("é", "Éclair"), Some("clair"));
    }
}
