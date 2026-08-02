package selection

import (
	"reflect"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/completion"
)

func candidate(t *testing.T, value, identity string) completion.Suggestion {
	t.Helper()
	edit, err := completion.NewTextEdit(0, 1, value)
	if err != nil {
		t.Fatal(err)
	}
	result, err := completion.NewSuggestion(edit, value, "", "command", completion.SourceSpec, completion.InsertionExact, identity)
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func TestFrozenSelectionAndBoundedLateUpdates(t *testing.T) {
	t.Parallel()
	state := New()
	state.BeginQuery(1, []completion.Suggestion{candidate(t, "alpha", "alpha"), candidate(t, "beta", "beta")})
	state.Down()
	if outcome := state.ApplyRankedUpdate(1, []completion.Suggestion{candidate(t, "aardvark", "aardvark"), candidate(t, "alpha", "alpha"), candidate(t, "beta", "renamed")}, 2); outcome != UpdateApplied {
		t.Fatalf("outcome = %d", outcome)
	}
	got := []string{}
	for _, item := range state.Candidates() {
		got = append(got, item.Identity())
	}
	if !reflect.DeepEqual(got, []string{"alpha", "renamed"}) {
		t.Errorf("order = %v", got)
	}
	selected, ok := state.Selected()
	if !ok || selected.Identity() != "renamed" {
		t.Errorf("selected = %#v, %t", selected, ok)
	}
	before := state.Candidates()
	if outcome := state.ApplyRankedUpdate(1, []completion.Suggestion{candidate(t, "alpha", "alpha")}, 2); outcome != UpdateSelectionConflict {
		t.Errorf("conflict = %d", outcome)
	}
	if !reflect.DeepEqual(before, state.Candidates()) {
		t.Fatal("conflicting update changed state")
	}
}

func TestVisibilityNavigationAndRedaction(t *testing.T) {
	t.Parallel()
	var state SelectionState
	state.BeginQuery(1, []completion.Suggestion{candidate(t, "hunter2", "secret")})
	state.Up()
	state.Down()
	index, ok := state.SelectedIndex()
	if !ok || index != 0 {
		t.Fatalf("selection = %d, %t", index, ok)
	}
	state.Escape()
	if state.IsVisible() {
		t.Fatal("escape remained visible")
	}
	state.BufferChanged()
	if !state.IsVisible() {
		t.Fatal("buffer change remained hidden")
	}
	state.ToggleVisible()
	state.BeginQuery(2, []completion.Suggestion{candidate(t, "other", "other")})
	if state.IsVisible() {
		t.Fatal("toggle did not persist")
	}
	if strings.Contains(state.String(), "hunter2") {
		t.Fatal("debug leaked candidate")
	}
}

func TestGhostSuffixUnicodeCaseFolding(t *testing.T) {
	t.Parallel()
	tests := map[string]struct {
		prefix, candidate, want string
		ok                      bool
	}{"ascii": {"Git Ch", "git checkout", "eckout", true}, "equal": {"git", "git", "", false}, "mismatch": {"git x", "git checkout", "", false}, "accent": {"é", "Éclair", "clair", true}, "expansion mismatch": {"İ", "i.example", "", false}}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got, ok := GhostSuffix(tc.prefix, tc.candidate)
			if got != tc.want || ok != tc.ok {
				t.Errorf("GhostSuffix = (%q,%t), want (%q,%t)", got, ok, tc.want, tc.ok)
			}
		})
	}
}
