//! Menu selection and ghost-text invariants independent of terminal rendering.

use crate::completion::Suggestion;

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
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionState {
    generation: u64,
    candidates: Vec<Suggestion>,
    selected: Option<usize>,
    navigated: bool,
    layer_enabled: bool,
    hidden_until_change: bool,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self {
            generation: 0,
            candidates: Vec::new(),
            selected: None,
            navigated: false,
            layer_enabled: true,
            hidden_until_change: false,
        }
    }
}

impl SelectionState {
    /// Starts a new query, selecting its first ranked result.
    pub fn begin_query(&mut self, generation: u64, candidates: Vec<Suggestion>) {
        self.generation = generation;
        self.selected = (!candidates.is_empty()).then_some(0);
        self.candidates = candidates;
        self.navigated = false;
        self.hidden_until_change = false;
    }

    /// Applies a cumulative current-generation provider update.
    ///
    /// Before navigation, the newly ranked top result becomes selected. After
    /// navigation, the selected identity remains authoritative. An update that
    /// omits it is rejected rather than moving the user's selection.
    pub fn apply_update(&mut self, generation: u64, candidates: Vec<Suggestion>) -> UpdateOutcome {
        if generation != self.generation {
            return UpdateOutcome::Stale;
        }

        if self.navigated {
            let Some(identity) = self.selected().map(|value| value.identity.clone()) else {
                return UpdateOutcome::SelectionConflict;
            };
            let Some(selected) = candidates
                .iter()
                .position(|candidate| candidate.identity == identity)
            else {
                return UpdateOutcome::SelectionConflict;
            };
            self.selected = Some(selected);
        } else {
            self.selected = (!candidates.is_empty()).then_some(0);
        }

        self.candidates = candidates;
        UpdateOutcome::Applied
    }

    /// Moves selection up without wrapping.
    pub fn up(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = Some(selected.saturating_sub(1));
        self.navigated = true;
    }

    /// Moves selection down without wrapping.
    pub fn down(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = Some((selected + 1).min(self.candidates.len() - 1));
        self.navigated = true;
    }

    /// Hides menu and ghost text until the buffer changes.
    pub fn escape(&mut self) {
        self.hidden_until_change = true;
    }

    /// Makes current candidates eligible after a buffer-changing key.
    pub fn buffer_changed(&mut self) {
        self.hidden_until_change = false;
    }

    /// Toggles the session suggestion layer. The setting persists across queries.
    pub fn toggle_visible(&mut self) {
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
    fn navigation_freezes_identity_across_late_update() {
        let mut state = SelectionState::default();
        state.begin_query(1, vec![candidate("alpha"), candidate("beta")]);
        state.down();
        let outcome = state.apply_update(
            1,
            vec![candidate("aardvark"), candidate("alpha"), candidate("beta")],
        );
        assert_eq!(outcome, UpdateOutcome::Applied);
        assert_eq!(state.selected().unwrap().identity, "beta");
        assert_eq!(state.selected_index(), Some(2));
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
