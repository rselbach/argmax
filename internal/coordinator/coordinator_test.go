package coordinator

import (
	"errors"
	"fmt"
	"math"
	"reflect"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/completion"
)

func suggestion(t *testing.T, value, identity string) completion.Suggestion {
	t.Helper()
	edit, err := completion.NewTextEdit(0, 1, value)
	if err != nil {
		t.Fatal(err)
	}
	item, err := completion.NewSuggestion(edit, value, "from Greendale", "command", completion.SourceSpec, completion.InsertionExact, identity)
	if err != nil {
		t.Fatal(err)
	}
	return item
}
func start(t *testing.T, c *CompletionCoordinator, line string) QueryWork {
	t.Helper()
	work, err := c.StartQuery(line, len(line), []byte("/tmp/Greendale"))
	if err != nil {
		t.Fatal(err)
	}
	return work
}
func accept(t *testing.T, c *CompletionCoordinator, b completion.ProviderBatch) BatchAcceptance {
	t.Helper()
	out := c.AcceptBatch(b)
	got, ok := out.Acceptance()
	if !ok {
		rejection, _ := out.Rejection()
		t.Fatalf("rejected: %v", rejection)
	}
	return got
}

func TestZeroOutcomesAndWorkAreSafe(t *testing.T) {
	t.Parallel()
	var outcome BatchOutcome
	if rejection, ok := outcome.Rejection(); ok || rejection != nil {
		t.Fatalf("zero rejection = %#v, %t", rejection, ok)
	}
	if _, ok := outcome.Acceptance(); ok {
		t.Fatal("zero outcome was accepted")
	}

	var work QueryWork
	if query := work.Query(); query.Line() != "" || query.Cursor() != 0 {
		t.Fatalf("zero query = %#v", query)
	}
	if work.Cancellation().IsCancelled() {
		t.Fatal("zero work cancellation is cancelled")
	}
	if got, want := work.String(), "QueryWork { valid: false }"; got != want {
		t.Fatalf("String() = %q, want %q", got, want)
	}
}

func TestRegistrationExactBounds(t *testing.T) {
	t.Parallel()
	tests := map[string]struct {
		providers []string
		limit     int
		kind      RegistrationErrorKind
	}{"duplicate": {[]string{"spec", "spec"}, 10, RegistrationDuplicateProvider}, "padded": {[]string{" spec"}, 10, RegistrationInvalidProviderName}, "long": {[]string{strings.Repeat("x", MaxProviderNameBytes+1)}, 10, RegistrationProviderNameTooLong}, "zero UI": {[]string{"spec"}, 0, RegistrationInvalidUILimit}, "too many": {func() []string {
		v := make([]string, MaxRegisteredProviders+1)
		for i := range v {
			v[i] = fmt.Sprintf("p%d", i)
		}
		return v
	}(), 10, RegistrationTooManyProviders}}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := New(tc.providers, tc.limit)
			var registration *RegistrationError
			if !errors.As(err, &registration) || registration.Kind() != tc.kind {
				t.Fatalf("error = %#v", err)
			}
		})
	}
	if MaxQueryLineBytes != 262144 || MaxQueryCWDBytes != 16384 || MaxBatchCandidates != 500 || MaxCandidateBytes != 65536 || MaxBatchBytes != 1048576 || MaxCumulativeCandidates != 4096 || MaxCumulativeBytes != 4194304 || MaxUISuggestions != 500 || MaxProviderErrorBytes != 8192 {
		t.Fatal("completion bounds changed")
	}
}

func TestCancellationAuthorityAndGenerationExhaustion(t *testing.T) {
	t.Parallel()
	c, err := newWithNextGeneration([]string{"spec"}, 10, math.MaxUint64, true)
	if err != nil {
		t.Fatal(err)
	}
	work := start(t, c, "g")
	if work.Query().Generation() != math.MaxUint64 {
		t.Fatal("wrong generation")
	}
	if _, err := c.StartQuery("c", 1, []byte("/tmp")); err == nil {
		t.Fatal("generation wrapped")
	}
	if !work.Cancellation().IsCancelled() {
		t.Fatal("observer retained authority")
	}
	out := c.AcceptBatch(completion.NewSuccessBatch("spec", math.MaxUint64, []completion.Suggestion{suggestion(t, "git", "late")}))
	rejection, ok := out.Rejection()
	authority, hasAuthority := rejection.Authority()
	if !ok || !hasAuthority || authority.Kind() != AuthorityCancelled {
		t.Fatalf("rejection = %#v", rejection)
	}
}

func TestBatchReplacementCumulativeRollback(t *testing.T) {
	t.Parallel()
	providers := make([]string, 9)
	for i := range providers {
		providers[i] = fmt.Sprintf("p%d", i)
	}
	c, err := New(providers, 10)
	if err != nil {
		t.Fatal(err)
	}
	generation := start(t, c, "g").Query().Generation()
	makeBatch := func(provider string, count int) []completion.Suggestion {
		values := make([]completion.Suggestion, count)
		for i := range values {
			values[i] = suggestion(t, fmt.Sprintf("v%d", i), provider+fmt.Sprint(i))
		}
		return values
	}
	for _, provider := range providers[:8] {
		accept(t, c, completion.NewSuccessBatch(provider, generation, makeBatch(provider, MaxBatchCandidates)))
	}
	accept(t, c, completion.NewSuccessBatch("p8", generation, makeBatch("p8", 1)))
	out := c.AcceptBatch(completion.NewSuccessBatch("p8", generation, makeBatch("p8", 97)))
	rejection, ok := out.Rejection()
	if !ok || rejection.Kind() != BatchTooManyCumulativeCandidates || rejection.Observed() != 4097 {
		t.Fatalf("rejection = %#v", rejection)
	}
	diagnostics := c.ProviderDiagnostics()
	var p8 ProviderDiagnostic
	for _, item := range diagnostics {
		if item.Provider() == "p8" {
			p8 = item
		}
	}
	if p8.CandidateCount() != 1 {
		t.Fatalf("rollback retained %d candidates", p8.CandidateCount())
	}
}

func TestProviderIsolationRankingValidationAndRedaction(t *testing.T) {
	t.Parallel()
	c, err := New([]string{"classified-provider", "system"}, 2)
	if err != nil {
		t.Fatal(err)
	}
	work := start(t, c, "g")
	generation := work.Query().Generation()
	accept(t, c, completion.NewSuccessBatch("classified-provider", generation, []completion.Suggestion{suggestion(t, "git", "git")}))
	accept(t, c, completion.NewSuccessBatch("system", generation, []completion.Suggestion{suggestion(t, "go", "go")}))
	ranked, err := c.MergedCandidates(generation)
	if err != nil {
		t.Fatal(err)
	}
	ranked[0], ranked[1] = ranked[1], ranked[0]
	out := c.ApplyRanked(generation, ranked)
	if out.Kind() != PresentationApplied || out.Displayed() != 2 {
		t.Fatalf("presentation = %#v", out)
	}
	invalid := append([]completion.Suggestion(nil), ranked...)
	invalid[1] = invalid[0]
	selectionBefore := c.Selection()
	before := selectionBefore.Candidates()
	out = c.ApplyRanked(generation, invalid)
	rejection, ok := out.Rejection()
	selectionAfter := c.Selection()
	if !ok || rejection.Kind() != PresentationCandidateSetMismatch || !reflect.DeepEqual(before, selectionAfter.Candidates()) {
		t.Fatalf("non-atomic ranking rejection: %#v", out)
	}
	accept(t, c, completion.NewFailureBatch("classified-provider", generation, "secret provider failure"))
	merged, _ := c.MergedCandidates(generation)
	if len(merged) != 1 {
		t.Fatalf("failure removed sibling: %d", len(merged))
	}
	debug := fmt.Sprintf("%s %#v %#v", c, work, c.ProviderDiagnostics())
	for _, secret := range []string{"classified-provider", "secret provider failure", "Greendale"} {
		if strings.Contains(debug, secret) {
			t.Fatalf("debug leaked %q: %s", secret, debug)
		}
	}
}

func TestMalformedBatchRejectionsAreAtomic(t *testing.T) {
	t.Parallel()
	c, _ := New([]string{"spec"}, 10)
	generation := start(t, c, "g").Query().Generation()
	accept(t, c, completion.NewSuccessBatch("spec", generation, []completion.Suggestion{suggestion(t, "git", "git")}))
	tests := map[string]struct {
		batch completion.ProviderBatch
		kind  BatchRejectionKind
	}{"conflict": {func() completion.ProviderBatch {
		e := "failure"
		return completion.NewProviderBatch("spec", generation, []completion.Suggestion{suggestion(t, "bad", "bad")}, &e)
	}(), BatchConflictingSuccessAndFailure}, "unsafe error": {func() completion.ProviderBatch {
		e := "bad\nerror"
		return completion.NewProviderBatch("spec", generation, nil, &e)
	}(), BatchUnsafeErrorText}, "oversized error": {func() completion.ProviderBatch {
		e := strings.Repeat("x", MaxProviderErrorBytes+1)
		return completion.NewProviderBatch("spec", generation, nil, &e)
	}(), BatchErrorTooLarge}}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			out := c.AcceptBatch(tc.batch)
			rejection, ok := out.Rejection()
			if !ok || rejection.Kind() != tc.kind {
				t.Fatalf("rejection = %#v", rejection)
			}
			merged, err := c.MergedCandidates(generation)
			if err != nil || len(merged) != 1 || merged[0].Identity() != "git" {
				t.Fatal("rejection changed cumulative state")
			}
		})
	}
}
