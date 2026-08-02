package completion

import (
	"reflect"
	"strings"
	"testing"
)

func mustEdit(t *testing.T, start, end int, replacement string) TextEdit {
	t.Helper()
	edit, err := NewTextEdit(start, end, replacement)
	if err != nil {
		t.Fatal(err)
	}
	return edit
}
func mustSuggestion(t *testing.T, value, identity string, source SuggestionSource, insertion InsertionBehavior) Suggestion {
	t.Helper()
	candidate, err := NewSuggestion(mustEdit(t, 0, 2, value), value, "", "command", source, insertion, identity)
	if err != nil {
		t.Fatal(err)
	}
	return candidate
}
func mustQuery(t *testing.T, line string) CompletionQuery {
	t.Helper()
	query, err := NewQuery(line, len(line), []byte("/tmp/Greendale"), 1)
	if err != nil {
		t.Fatal(err)
	}
	return query
}

func TestCancellationTokenZeroValue(t *testing.T) {
	t.Parallel()
	var token CancellationToken
	if token.IsCancelled() {
		t.Fatal("zero token is cancelled")
	}
	if got, want := token.String(), "CancellationToken { cancelled: false }"; got != want {
		t.Fatalf("String() = %q, want %q", got, want)
	}
}

func TestEditAndInsertionValidation(t *testing.T) {
	t.Parallel()
	tests := map[string]struct {
		line      string
		edit      TextEdit
		insertion InsertionBehavior
		want      string
		wantErr   bool
	}{
		"suffix preserved": {"git ch --help", mustEdit(t, 4, 6, "checkout"), InsertionExact, "git checkout --help", false},
		"unicode split":    {"é", mustEdit(t, 1, 2, ""), InsertionExact, "", true},
		"append at end":    {"gi", mustEdit(t, 0, 2, "git"), InsertionAppendSpace, "git ", false},
		"directory slash":  {"gi", mustEdit(t, 0, 2, "src"), InsertionDirectory, "src/", false},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			candidate, err := NewSuggestion(tc.edit, "candidate", "", "", SourceSpec, tc.insertion, name)
			if err != nil {
				t.Fatal(err)
			}
			got, err := candidate.ResultingLine(mustQuery(t, tc.line))
			if (err != nil) != tc.wantErr {
				t.Fatalf("error = %v", err)
			}
			if got != tc.want {
				t.Errorf("result = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestSanitizationAndFailureBounds(t *testing.T) {
	t.Parallel()
	candidate, err := NewSuggestion(mustEdit(t, 0, 0, "printf 'one\ntwo'\t\x1b[31m\a"), "safe\x1b[31m\ntext", "", "", SourceHistory, InsertionExact, "history")
	if err != nil {
		t.Fatal(err)
	}
	if got := candidate.Edit().Replacement(); got != "printf 'one\ntwo'\t[31m" {
		t.Errorf("replacement = %q", got)
	}
	if candidate.Display() != "safe[31m text" {
		t.Errorf("display = %q", candidate.Display())
	}
	batch := NewFailureBatch("classified", 3, "hunter2\n\x1b[31m"+strings.Repeat("é", MaxProviderFailureBytes))
	failure, ok := batch.Error()
	if !ok || len(failure) > MaxProviderFailureBytes || strings.ContainsAny(failure, "\n\x1b") {
		t.Fatalf("unsafe failure len=%d: %q", len(failure), failure[:min(len(failure), 30)])
	}
	if strings.Contains(batch.String(), "classified") || strings.Contains(candidate.String(), "printf") {
		t.Fatal("debug output leaked content")
	}
}

func TestMergeIsInputOrderIndependentAndRich(t *testing.T) {
	t.Parallel()
	query := mustQuery(t, "gi")
	system := mustSuggestion(t, "git", "system:git", SourceSystem, InsertionAppendSpace)
	spec := mustSuggestion(t, "git", "spec:git", SourceSpec, InsertionAppendSpace)
	spec.description = "distributed version control"
	spec = spec.WithRanking(.8, .9)
	first := MergeSuggestions(query, []Suggestion{system}, []Suggestion{spec})
	second := MergeSuggestions(query, []Suggestion{spec}, []Suggestion{system})
	if !reflect.DeepEqual(first, second) {
		t.Fatalf("merge depends on input order\n%#v\n%#v", first, second)
	}
	if len(first) != 1 || first[0].Source() != SourceSpec || len(first[0].Sources()) != 2 || first[0].Description() != "distributed version control" {
		t.Fatalf("merged metadata = %#v", first)
	}
}

func TestMergeDropsInvalidBlankAndNormalizedIdentity(t *testing.T) {
	t.Parallel()
	query := mustQuery(t, "gi\u2003")
	identical := mustSuggestion(t, "gi", "same", SourceSpec, InsertionExact)
	invalid := mustSuggestion(t, "bad", "invalid", SourceSpec, InsertionExact)
	invalid.edit.end = len(query.Line()) + 1
	blank := mustSuggestion(t, "other", "blank", SourceSpec, InsertionExact)
	blank.display = " "
	if got := MergeSuggestions(query, []Suggestion{identical, invalid, blank}); len(got) != 0 {
		t.Fatalf("merged = %#v", got)
	}
}
