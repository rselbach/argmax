// Package selection owns menu-selection and ghost-text invariants independently
// of terminal rendering.
package selection

import (
	"fmt"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/rselbach/argmax/internal/completion"
)

// UpdateOutcome is the result of applying an asynchronous provider update.
type UpdateOutcome uint8

const (
	// UpdateApplied indicates that the ranked candidate update was accepted.
	UpdateApplied UpdateOutcome = iota + 1
	// UpdateStale indicates that the update belongs to another generation.
	UpdateStale
	// UpdateSelectionConflict preserves a navigated selection instead of replacing it.
	UpdateSelectionConflict
)

type selectionKey struct {
	start       int
	end         int
	replacement string
	insertion   completion.InsertionBehavior
}

func keyFromCandidate(candidate completion.Suggestion) selectionKey {
	edit := candidate.Edit()
	return selectionKey{
		start: edit.Start(), end: edit.End(), replacement: edit.Replacement(),
		insertion: candidate.Insertion(),
	}
}

func (key selectionKey) matches(candidate completion.Suggestion) bool {
	edit := candidate.Edit()
	return key.start == edit.Start() && key.end == edit.End() &&
		key.replacement == edit.Replacement() && key.insertion == candidate.Insertion()
}

// SelectionState is selected-candidate state for one immutable query generation.
type SelectionState struct {
	generation        uint64
	candidates        []completion.Suggestion
	selected          int
	hasSelected       bool
	navigated         bool
	frozenOrder       []selectionKey
	hasFrozenOrder    bool
	layerDisabled     bool
	hiddenUntilChange bool
}

// New creates default selection state with the suggestion layer enabled.
func New() *SelectionState { return &SelectionState{} }

// BeginQuery starts a new query and selects its first candidate.
func (s *SelectionState) BeginQuery(generation uint64, candidates []completion.Suggestion) {
	s.generation = generation
	s.candidates = compactCandidates(candidates)
	s.selected = 0
	s.hasSelected = len(candidates) != 0
	s.navigated = false
	s.frozenOrder = nil
	s.hasFrozenOrder = false
	s.hiddenUntilChange = false
}

// ApplyRankedUpdate applies a full ranked update and retains a bounded rendering
// window. After navigation, surviving edits retain order and the selected edit
// must remain present.
func (s *SelectionState) ApplyRankedUpdate(
	generation uint64,
	candidates []completion.Suggestion,
	maxCandidates int,
) UpdateOutcome {
	if generation != s.generation {
		return UpdateStale
	}
	if maxCandidates < 0 {
		maxCandidates = 0
	}

	if !s.navigated {
		if len(candidates) > maxCandidates {
			candidates = candidates[:maxCandidates]
		}
		s.candidates = compactCandidates(candidates)
		s.selected = 0
		s.hasSelected = len(candidates) != 0
		return UpdateApplied
	}
	if !s.hasFrozenOrder || !s.hasSelected || s.selected >= len(s.frozenOrder) {
		return UpdateSelectionConflict
	}
	selectedKey := s.frozenOrder[s.selected]
	remaining := append([]completion.Suggestion(nil), candidates...)
	used := make([]bool, len(remaining))
	retained := make([]completion.Suggestion, 0, min(maxCandidates, len(remaining)))
	for _, key := range s.frozenOrder {
		for index, candidate := range remaining {
			if !used[index] && key.matches(candidate) {
				used[index] = true
				retained = append(retained, candidate)
				break
			}
		}
	}
	selected := -1
	for index, candidate := range retained {
		if selectedKey.matches(candidate) {
			selected = index
			break
		}
	}
	if selected < 0 || selected >= maxCandidates {
		return UpdateSelectionConflict
	}
	if len(retained) > maxCandidates {
		retained = retained[:maxCandidates]
	}
	for index, candidate := range remaining {
		if len(retained) == maxCandidates {
			break
		}
		if !used[index] {
			retained = append(retained, candidate)
		}
	}
	s.candidates = compactCandidates(retained)
	s.selected = selected
	s.hasSelected = true
	s.freezeVisibleOrder()
	return UpdateApplied
}

// Up moves selection up without wrapping and freezes edit semantics.
func (s *SelectionState) Up() {
	if !s.hasSelected {
		return
	}
	s.selected = max(0, s.selected-1)
	s.navigated = true
	s.freezeVisibleOrder()
}

// Down moves selection down without wrapping and freezes edit semantics.
func (s *SelectionState) Down() {
	if !s.hasSelected || len(s.candidates) == 0 {
		return
	}
	s.selected = min(s.selected+1, len(s.candidates)-1)
	s.navigated = true
	s.freezeVisibleOrder()
}

// Escape hides menu and ghost text until the buffer changes.
func (s *SelectionState) Escape() { s.hiddenUntilChange = true }

// BufferChanged makes current candidates eligible after a buffer-changing key.
func (s *SelectionState) BufferChanged() { s.hiddenUntilChange = false }

// ToggleVisible toggles the session suggestion layer.
func (s *SelectionState) ToggleVisible() {
	s.layerDisabled = !s.layerDisabled
	s.hiddenUntilChange = false
}

// LayerEnabled reports the persistent suggestion-layer setting.
func (s *SelectionState) LayerEnabled() bool { return !s.layerDisabled }

// Selected returns the selected candidate, if any.
func (s *SelectionState) Selected() (completion.Suggestion, bool) {
	if !s.hasSelected || s.selected < 0 || s.selected >= len(s.candidates) {
		return completion.Suggestion{}, false
	}
	return s.candidates[s.selected], true
}

// Candidates returns a copy of the authoritative bounded rendering window.
func (s *SelectionState) Candidates() []completion.Suggestion {
	return append([]completion.Suggestion(nil), s.candidates...)
}

// CandidateCount returns the rendering-window size.
func (s *SelectionState) CandidateCount() int { return len(s.candidates) }

// SelectedIndex returns the selected row and whether a selection exists.
func (s *SelectionState) SelectedIndex() (int, bool) { return s.selected, s.hasSelected }

// IsVisible reports whether the suggestion layer should render.
func (s *SelectionState) IsVisible() bool {
	return !s.layerDisabled && !s.hiddenUntilChange && s.hasSelected
}

// Generation returns the current query generation.
func (s *SelectionState) Generation() uint64 { return s.generation }

func (s *SelectionState) freezeVisibleOrder() {
	s.frozenOrder = make([]selectionKey, len(s.candidates))
	for index, candidate := range s.candidates {
		s.frozenOrder[index] = keyFromCandidate(candidate)
	}
	s.hasFrozenOrder = true
}

// String returns structural selection state without candidate or edit text.
func (s SelectionState) String() string {
	selected := "None"
	if s.hasSelected {
		selected = fmt.Sprintf("Some(%d)", s.selected)
	}
	frozen := "None"
	if s.hasFrozenOrder {
		frozen = fmt.Sprintf("Some(%d)", len(s.frozenOrder))
	}
	return fmt.Sprintf(
		"SelectionState { generation: %d, candidate_count: %d, selected: %s, navigated: %t, frozen_candidate_count: %s, layer_enabled: %t, hidden_until_change: %t }",
		s.generation, len(s.candidates), selected, s.navigated, frozen,
		!s.layerDisabled, s.hiddenUntilChange,
	)
}

// GoString returns structural selection state without candidate or edit text.
func (s SelectionState) GoString() string { return s.String() }

func compactCandidates(candidates []completion.Suggestion) []completion.Suggestion {
	if candidates == nil {
		return nil
	}
	return slicesClip(append([]completion.Suggestion(nil), candidates...))
}

func slicesClip[S ~[]E, E any](values S) S { return values[:len(values):len(values)] }

// GhostSuffix returns the candidate suffix when candidate begins with prefix
// under Rust/Unicode character-wise lowercase comparison. Candidate bytes are
// preserved and an empty suffix is reported as absent.
func GhostSuffix(prefix, candidate string) (string, bool) {
	if !utf8.ValidString(prefix) || !utf8.ValidString(candidate) {
		return "", false
	}
	candidateOffset := 0
	for _, prefixCharacter := range prefix {
		if candidateOffset >= len(candidate) {
			return "", false
		}
		candidateCharacter, size := utf8.DecodeRuneInString(candidate[candidateOffset:])
		if rustCharLower(prefixCharacter) != rustCharLower(candidateCharacter) {
			return "", false
		}
		candidateOffset += size
	}
	if candidateOffset == len(candidate) {
		return "", false
	}
	return candidate[candidateOffset:], true
}

func rustCharLower(character rune) string {
	// U+0130 is the only Unicode scalar whose unconditional lowercase mapping
	// expands under Rust's char::to_lowercase.
	if character == '\u0130' {
		return "i\u0307"
	}
	return strings.ToLower(string(unicode.ToLower(character)))
}
