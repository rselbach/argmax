// Package coordinator bounds and coordinates asynchronous completion-provider
// results for one session.
package coordinator

import (
	"bytes"
	"cmp"
	"context"
	"fmt"
	"math"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/rselbach/argmax/internal/completion"
	"github.com/rselbach/argmax/internal/selection"
)

const (
	// MaxRegisteredProviders bounds providers retained by one coordinator.
	MaxRegisteredProviders = 64
	// MaxProviderNameBytes bounds one provider identity.
	MaxProviderNameBytes = 128
	// MaxQueryLineBytes bounds one shell buffer query.
	MaxQueryLineBytes = 256 * 1024
	// MaxQueryCWDBytes bounds one opaque working-directory path.
	MaxQueryCWDBytes = 16 * 1024
	// MaxBatchCandidates bounds candidates in one provider response.
	MaxBatchCandidates = 500
	// MaxCandidateBytes bounds text owned by one candidate.
	MaxCandidateBytes = 64 * 1024
	// MaxBatchBytes bounds text retained from one provider response.
	MaxBatchBytes = 1024 * 1024
	// MaxProviderErrorBytes bounds one sanitized provider failure.
	MaxProviderErrorBytes = completion.MaxProviderFailureBytes
	// MaxCumulativeCandidates bounds candidates retained for one query.
	MaxCumulativeCandidates = 4_096
	// MaxCumulativeBytes bounds candidate text retained for one query.
	MaxCumulativeBytes = 4 * 1024 * 1024
	// MaxUISuggestions bounds candidates exposed to terminal rendering.
	MaxUISuggestions = 500
)

// RegistrationErrorKind classifies invalid coordinator configuration.
type RegistrationErrorKind uint8

const (
	// RegistrationTooManyProviders rejects an excessive provider count.
	RegistrationTooManyProviders RegistrationErrorKind = iota + 1
	// RegistrationInvalidProviderName rejects malformed provider identity text.
	RegistrationInvalidProviderName
	// RegistrationProviderNameTooLong rejects an oversized provider identity.
	RegistrationProviderNameTooLong
	// RegistrationDuplicateProvider rejects a repeated provider identity.
	RegistrationDuplicateProvider
	// RegistrationInvalidUILimit rejects an out-of-range rendering limit.
	RegistrationInvalidUILimit
)

// RegistrationError is a content-redacted configuration failure.
type RegistrationError struct {
	kind                                         RegistrationErrorKind
	index, observed, limit, providerBytes, value int
}

// Kind returns the configuration failure class.
func (e *RegistrationError) Kind() RegistrationErrorKind { return e.kind }

// Index returns the provider index associated with the failure.
func (e *RegistrationError) Index() int { return e.index }

// Observed returns the rejected count or byte length.
func (e *RegistrationError) Observed() int { return e.observed }

// Limit returns the applicable maximum.
func (e *RegistrationError) Limit() int { return e.limit }

// ProviderBytes returns the redacted provider identity length.
func (e *RegistrationError) ProviderBytes() int { return e.providerBytes }

// Value returns the rejected scalar configuration value.
func (e *RegistrationError) Value() int { return e.value }

// Error describes the configuration failure without exposing provider text.
func (e *RegistrationError) Error() string {
	switch e.kind {
	case RegistrationTooManyProviders:
		return fmt.Sprintf("registered %d providers; limit is %d", e.observed, e.limit)
	case RegistrationInvalidProviderName:
		return fmt.Sprintf("provider name at index %d is invalid", e.index)
	case RegistrationProviderNameTooLong:
		return fmt.Sprintf("provider name at index %d is %d bytes; limit is %d", e.index, e.observed, e.limit)
	case RegistrationDuplicateProvider:
		return fmt.Sprintf("provider identity at index %d is a duplicate (%d bytes)", e.index, e.providerBytes)
	case RegistrationInvalidUILimit:
		return fmt.Sprintf("UI result limit is %d; expected 1 through %d", e.value, e.limit)
	default:
		return "invalid completion coordinator registration"
	}
}

// GoString describes the configuration failure without exposing provider text.
func (e *RegistrationError) GoString() string { return e.Error() }

// QueryStartErrorKind classifies a rejected authoritative query.
type QueryStartErrorKind uint8

const (
	// QueryGenerationExhausted rejects a query after generation space is consumed.
	QueryGenerationExhausted QueryStartErrorKind = iota + 1
	// QueryLineTooLarge rejects an oversized shell buffer.
	QueryLineTooLarge
	// QueryCWDTooLarge rejects an oversized working directory.
	QueryCWDTooLarge
	// QueryCWDNotAbsolute rejects a non-absolute working directory.
	QueryCWDNotAbsolute
	// QueryInvalidCursor rejects a cursor outside a UTF-8 boundary.
	QueryInvalidCursor
	// QueryInvalidUTF8 rejects a malformed shell buffer.
	QueryInvalidUTF8
)

// QueryStartError is a bounded, content-redacted query failure.
type QueryStartError struct {
	kind                            QueryStartErrorKind
	bytes, limit, cursor, lineBytes int
}

// Kind returns the query rejection class.
func (e *QueryStartError) Kind() QueryStartErrorKind { return e.kind }

// Bytes returns the rejected byte count.
func (e *QueryStartError) Bytes() int { return e.bytes }

// Limit returns the applicable byte bound.
func (e *QueryStartError) Limit() int { return e.limit }

// Cursor returns the rejected cursor offset.
func (e *QueryStartError) Cursor() int { return e.cursor }

// LineBytes returns the redacted shell-buffer length.
func (e *QueryStartError) LineBytes() int { return e.lineBytes }

// Error describes the rejection without exposing query content.
func (e *QueryStartError) Error() string {
	switch e.kind {
	case QueryGenerationExhausted:
		return "completion query generation space is exhausted"
	case QueryLineTooLarge:
		return fmt.Sprintf("completion line is %d bytes; limit is %d", e.bytes, e.limit)
	case QueryCWDTooLarge:
		return fmt.Sprintf("completion working directory is %d bytes; limit is %d", e.bytes, e.limit)
	case QueryCWDNotAbsolute:
		return "completion working directory must be absolute"
	case QueryInvalidCursor:
		return fmt.Sprintf("cursor %d is not a UTF-8 boundary in a %d-byte line", e.cursor, e.lineBytes)
	case QueryInvalidUTF8:
		return "completion line is not UTF-8"
	default:
		return "completion query is invalid"
	}
}

// QueryWork is immutable provider work for one generation.
type QueryWork struct {
	query        *completion.CompletionQuery
	cancellation completion.CancellationToken
}

// Query returns the immutable query, or its zero value when this work is invalid.
func (w QueryWork) Query() completion.CompletionQuery {
	if w.query == nil {
		return completion.CompletionQuery{}
	}
	return *w.query
}

// Cancellation returns the observer-only cancellation token.
func (w QueryWork) Cancellation() completion.CancellationToken { return w.cancellation }

// String returns content-redacted work state.
func (w QueryWork) String() string {
	if w.query == nil {
		return "QueryWork { valid: false }"
	}
	return fmt.Sprintf("QueryWork { generation: %d, cursor: %d, line_bytes: %d, cwd_bytes: %d, cancelled: %t }",
		w.query.Generation(), w.query.Cursor(), len(w.query.Line()), len(w.query.CWD()), w.cancellation.IsCancelled())
}

// GoString returns content-redacted work state.
func (w QueryWork) GoString() string { return w.String() }

// CancellationOutcomeKind identifies explicit cancellation disposition.
type CancellationOutcomeKind uint8

const (
	// CancellationNoActiveQuery indicates there was no work to cancel.
	CancellationNoActiveQuery CancellationOutcomeKind = iota + 1
	// CancellationCancelled indicates active work lost authority.
	CancellationCancelled
)

// CancellationOutcome records explicit cancellation disposition.
type CancellationOutcome struct {
	kind       CancellationOutcomeKind
	generation uint64
}

// Kind returns the cancellation disposition.
func (o CancellationOutcome) Kind() CancellationOutcomeKind { return o.kind }

// Generation returns the cancelled generation when cancellation occurred.
func (o CancellationOutcome) Generation() (uint64, bool) {
	return o.generation, o.kind == CancellationCancelled
}

// AuthorityRejectionKind says why work lacks current query authority.
type AuthorityRejectionKind uint8

const (
	// AuthorityNoActiveQuery rejects work when no query is active.
	AuthorityNoActiveQuery AuthorityRejectionKind = iota + 1
	// AuthorityCancelled rejects work from the last cancelled generation.
	AuthorityCancelled
	// AuthorityGenerationMismatch rejects work for another generation.
	AuthorityGenerationMismatch
)

// AuthorityRejection describes why provider work lacks current authority.
type AuthorityRejection struct {
	kind                         AuthorityRejectionKind
	generation, active, received uint64
}

// Kind returns the authority rejection class.
func (e *AuthorityRejection) Kind() AuthorityRejectionKind { return e.kind }

// Generation returns the cancelled generation for AuthorityCancelled.
func (e *AuthorityRejection) Generation() uint64 { return e.generation }

// Active returns the active generation for AuthorityGenerationMismatch.
func (e *AuthorityRejection) Active() uint64 { return e.active }

// Received returns the rejected generation for AuthorityGenerationMismatch.
func (e *AuthorityRejection) Received() uint64 { return e.received }

// Error describes the authority rejection.
func (e *AuthorityRejection) Error() string {
	switch e.kind {
	case AuthorityNoActiveQuery:
		return "no completion query is active"
	case AuthorityCancelled:
		return fmt.Sprintf("completion generation %d was cancelled", e.generation)
	case AuthorityGenerationMismatch:
		return fmt.Sprintf("completion generation %d is stale or premature; active generation is %d", e.received, e.active)
	default:
		return "completion query authority rejected"
	}
}

// ProviderPhase is accepted provider state for the active query.
type ProviderPhase uint8

const (
	// ProviderPending indicates that no provider response is retained.
	ProviderPending ProviderPhase = iota + 1
	// ProviderReady indicates that successful candidates are retained.
	ProviderReady
	// ProviderFailed indicates that an isolated provider failure is retained.
	ProviderFailed
)

// ProviderDiagnostic is bounded state for one registered provider.
type ProviderDiagnostic struct {
	provider       string
	phase          ProviderPhase
	candidateCount int
	err            string
	hasError       bool
}

// Provider returns the exact registered provider identity.
func (d ProviderDiagnostic) Provider() string { return d.provider }

// Phase returns the provider state for the active query.
func (d ProviderDiagnostic) Phase() ProviderPhase { return d.phase }

// CandidateCount returns candidates retained from this provider.
func (d ProviderDiagnostic) CandidateCount() int { return d.candidateCount }

// Error returns the bounded sanitized provider failure, if any.
func (d ProviderDiagnostic) Error() (string, bool) { return d.err, d.hasError }

// String returns content-redacted provider state.
func (d ProviderDiagnostic) String() string {
	errorBytes := 0
	if d.hasError {
		errorBytes = len(d.err)
	}
	return fmt.Sprintf("ProviderDiagnostic { provider_bytes: %d, phase: %d, candidate_count: %d, error_bytes: %d }", len(d.provider), d.phase, d.candidateCount, errorBytes)
}

// GoString returns content-redacted provider state.
func (d ProviderDiagnostic) GoString() string { return d.String() }

// AcceptedBatchKind distinguishes success from isolated provider failure.
type AcceptedBatchKind uint8

const (
	// AcceptedSuccess identifies a retained successful provider response.
	AcceptedSuccess AcceptedBatchKind = iota + 1
	// AcceptedFailure identifies a retained isolated provider failure.
	AcceptedFailure
)

// BatchAcceptance summarizes one accepted provider response.
type BatchAcceptance struct {
	provider                                 string
	generation                               uint64
	kind                                     AcceptedBatchKind
	replaced                                 bool
	providerCandidates, cumulativeCandidates int
}

// Provider returns the exact accepted provider identity.
func (a BatchAcceptance) Provider() string { return a.provider }

// Generation returns the accepted query generation.
func (a BatchAcceptance) Generation() uint64 { return a.generation }

// Kind returns whether success or isolated failure was accepted.
func (a BatchAcceptance) Kind() AcceptedBatchKind { return a.kind }

// ReplacedPrevious reports whether this provider had a retained response.
func (a BatchAcceptance) ReplacedPrevious() bool { return a.replaced }

// ProviderCandidates returns candidates retained for this provider.
func (a BatchAcceptance) ProviderCandidates() int { return a.providerCandidates }

// CumulativeCandidates returns candidates retained across providers.
func (a BatchAcceptance) CumulativeCandidates() int { return a.cumulativeCandidates }

// String returns content-redacted acceptance state.
func (a BatchAcceptance) String() string {
	return fmt.Sprintf("BatchAcceptance { provider_bytes: %d, generation: %d, kind: %d, replaced_previous: %t, provider_candidates: %d, cumulative_candidates: %d }", len(a.provider), a.generation, a.kind, a.replaced, a.providerCandidates, a.cumulativeCandidates)
}

// GoString returns content-redacted acceptance state.
func (a BatchAcceptance) GoString() string { return a.String() }

// BatchRejectionKind classifies an atomic provider-batch rejection.
type BatchRejectionKind uint8

const (
	// BatchAuthority rejects work without current query authority.
	BatchAuthority BatchRejectionKind = iota + 1
	// BatchUnknownProvider rejects an unregistered provider identity.
	BatchUnknownProvider
	// BatchConflictingSuccessAndFailure rejects mixed candidates and failure.
	BatchConflictingSuccessAndFailure
	// BatchErrorTooLarge rejects an oversized provider failure.
	BatchErrorTooLarge
	// BatchUnsafeErrorText rejects unsafe control data in a provider failure.
	BatchUnsafeErrorText
	// BatchTooManyCandidates rejects an excessive provider candidate count.
	BatchTooManyCandidates
	// BatchCandidateTooLarge rejects an oversized candidate.
	BatchCandidateTooLarge
	// BatchTooLarge rejects excessive text in one provider response.
	BatchTooLarge
	// BatchTooManyCumulativeCandidates rejects excessive retained candidates.
	BatchTooManyCumulativeCandidates
	// BatchCumulativeBytesTooLarge rejects excessive retained candidate text.
	BatchCumulativeBytesTooLarge
)

// BatchRejection describes an atomic provider-response rejection.
type BatchRejection struct {
	kind                                         BatchRejectionKind
	authority                                    *AuthorityRejection
	providerBytes, index, observed, bytes, limit int
}

// Kind returns the provider-response rejection class.
func (e *BatchRejection) Kind() BatchRejectionKind { return e.kind }

// Authority returns the nested authority failure when applicable.
func (e *BatchRejection) Authority() (*AuthorityRejection, bool) {
	return e.authority, e.authority != nil
}

// ProviderBytes returns the redacted provider identity length.
func (e *BatchRejection) ProviderBytes() int { return e.providerBytes }

// Index returns the rejected candidate index.
func (e *BatchRejection) Index() int { return e.index }

// Observed returns the rejected count.
func (e *BatchRejection) Observed() int { return e.observed }

// Bytes returns the rejected byte count.
func (e *BatchRejection) Bytes() int { return e.bytes }

// Limit returns the applicable bound.
func (e *BatchRejection) Limit() int { return e.limit }

// Error describes the rejection without exposing provider content.
func (e *BatchRejection) Error() string {
	if e.authority != nil {
		return e.authority.Error()
	}
	switch e.kind {
	case BatchUnknownProvider:
		return fmt.Sprintf("unregistered completion provider identity is %d bytes", e.providerBytes)
	case BatchConflictingSuccessAndFailure:
		return "provider batch contains candidates and an error"
	case BatchErrorTooLarge:
		return fmt.Sprintf("provider error is %d bytes; limit is %d", e.bytes, e.limit)
	case BatchUnsafeErrorText:
		return "provider error contains an unsafe control character"
	case BatchTooManyCandidates:
		return fmt.Sprintf("provider batch contains %d candidates; limit is %d", e.observed, e.limit)
	case BatchCandidateTooLarge:
		return fmt.Sprintf("candidate %d contains %d owned-text bytes; limit is %d", e.index, e.bytes, e.limit)
	case BatchTooLarge:
		return fmt.Sprintf("provider batch contains %d bytes; limit is %d", e.bytes, e.limit)
	case BatchTooManyCumulativeCandidates:
		return fmt.Sprintf("query would retain %d provider candidates; limit is %d", e.observed, e.limit)
	case BatchCumulativeBytesTooLarge:
		return fmt.Sprintf("query would retain %d provider bytes; limit is %d", e.bytes, e.limit)
	default:
		return "provider batch rejected"
	}
}

// Unwrap returns the nested authority rejection, if any.
func (e *BatchRejection) Unwrap() error {
	if e.authority != nil {
		return e.authority
	}
	return nil
}

// BatchOutcome contains exactly one acceptance or rejection for a valid result.
type BatchOutcome struct {
	acceptance BatchAcceptance
	rejection  *BatchRejection
	accepted   bool
}

// Acceptance returns accepted response state when present.
func (o BatchOutcome) Acceptance() (BatchAcceptance, bool) { return o.acceptance, o.accepted }

// Rejection returns rejected response state when present.
func (o BatchOutcome) Rejection() (*BatchRejection, bool) {
	return o.rejection, o.rejection != nil
}

// PresentationRejectionKind classifies a ranked-list rejection.
type PresentationRejectionKind uint8

const (
	// PresentationAuthority rejects ranking without current query authority.
	PresentationAuthority PresentationRejectionKind = iota + 1
	// PresentationTooManyCandidates rejects an excessive ranked candidate count.
	PresentationTooManyCandidates
	// PresentationCandidateTooLarge rejects an oversized ranked candidate.
	PresentationCandidateTooLarge
	// PresentationCumulativeBytesTooLarge rejects excessive ranked candidate text.
	PresentationCumulativeBytesTooLarge
	// PresentationCandidateSetMismatch rejects a non-permutation of merged candidates.
	PresentationCandidateSetMismatch
	// PresentationSelectionGenerationMismatch rejects selection from another query.
	PresentationSelectionGenerationMismatch
)

// PresentationRejection describes an atomic ranked-list rejection.
type PresentationRejection struct {
	kind                          PresentationRejectionKind
	authority                     *AuthorityRejection
	index, observed, bytes, limit int
}

// Kind returns the ranked-list rejection class.
func (e *PresentationRejection) Kind() PresentationRejectionKind { return e.kind }

// Authority returns the nested authority failure when applicable.
func (e *PresentationRejection) Authority() (*AuthorityRejection, bool) {
	return e.authority, e.authority != nil
}

// Index returns the rejected candidate index.
func (e *PresentationRejection) Index() int { return e.index }

// Observed returns the rejected candidate count.
func (e *PresentationRejection) Observed() int { return e.observed }

// Bytes returns the rejected byte count.
func (e *PresentationRejection) Bytes() int { return e.bytes }

// Limit returns the applicable bound.
func (e *PresentationRejection) Limit() int { return e.limit }

// Error describes the rejection without exposing candidate content.
func (e *PresentationRejection) Error() string {
	if e.authority != nil {
		return e.authority.Error()
	}
	switch e.kind {
	case PresentationTooManyCandidates:
		return fmt.Sprintf("ranked result contains %d candidates; limit is %d", e.observed, e.limit)
	case PresentationCandidateTooLarge:
		return fmt.Sprintf("ranked candidate %d contains %d owned-text bytes; limit is %d", e.index, e.bytes, e.limit)
	case PresentationCumulativeBytesTooLarge:
		return fmt.Sprintf("ranked candidates contain %d owned-text bytes; limit is %d", e.bytes, e.limit)
	case PresentationCandidateSetMismatch:
		return "ranked candidates are not an exact permutation of the merged candidate set"
	case PresentationSelectionGenerationMismatch:
		return "selection generation does not match the active query"
	default:
		return "ranked candidates rejected"
	}
}

// Unwrap returns the nested authority rejection, if any.
func (e *PresentationRejection) Unwrap() error {
	if e.authority != nil {
		return e.authority
	}
	return nil
}

// PresentationOutcomeKind identifies ranked presentation disposition.
type PresentationOutcomeKind uint8

const (
	// PresentationApplied indicates that ranked candidates were applied.
	PresentationApplied PresentationOutcomeKind = iota + 1
	// PresentationSelectionConflict preserves a navigated selection.
	PresentationSelectionConflict
	// PresentationRejected indicates invalid or unauthorized ranked candidates.
	PresentationRejected
)

// PresentationOutcome records ranked-list disposition and bounded counts.
type PresentationOutcome struct {
	kind                 PresentationOutcomeKind
	available, displayed int
	rejection            *PresentationRejection
}

// Kind returns the ranked-list disposition.
func (o PresentationOutcome) Kind() PresentationOutcomeKind { return o.kind }

// Available returns the validated ranked candidate count.
func (o PresentationOutcome) Available() int { return o.available }

// Displayed returns the retained rendering-window count.
func (o PresentationOutcome) Displayed() int { return o.displayed }

// Rejection returns ranked-list rejection details when present.
func (o PresentationOutcome) Rejection() (*PresentationRejection, bool) {
	return o.rejection, o.kind == PresentationRejected
}

type storedBatch struct {
	suggestions   []completion.Suggestion
	err           string
	hasError      bool
	retainedBytes int
}

func (b storedBatch) phase() ProviderPhase {
	if b.hasError {
		return ProviderFailed
	}
	return ProviderReady
}

type activeQuery struct {
	query                             *completion.CompletionQuery
	cancel                            context.CancelFunc
	batches                           map[string]storedBatch
	retainedCandidates, retainedBytes int
}

type noCopy struct{}

// Lock marks enclosing values as unsafe to copy for go vet's copylocks check.
func (*noCopy) Lock() {}

// Unlock completes the marker interface used by go vet.
func (*noCopy) Unlock() {}

// CompletionCoordinator owns query generation, cancellation, cumulative batches,
// exact ranking validation, and bounded selection state. It must not be copied.
// Its methods must be called by one owning goroutine; asynchronous provider
// results must be marshalled back to that goroutine before acceptance.
type CompletionCoordinator struct {
	noCopy                     noCopy
	providers                  []string
	providerSet                map[string]struct{}
	uiMaxSuggestions           int
	nextGeneration             uint64
	hasNextGeneration          bool
	active                     *activeQuery
	lastCancelledGeneration    uint64
	hasLastCancelledGeneration bool
	selection                  *selection.SelectionState
}

// New validates exact provider identities in caller order and the UI bound.
func New(providers []string, uiMaxSuggestions int) (*CompletionCoordinator, error) {
	return newWithNextGeneration(providers, uiMaxSuggestions, 0, true)
}

func newWithNextGeneration(providers []string, uiMaxSuggestions int, next uint64, hasNext bool) (*CompletionCoordinator, error) {
	if uiMaxSuggestions < 1 || uiMaxSuggestions > MaxUISuggestions {
		return nil, &RegistrationError{kind: RegistrationInvalidUILimit, value: uiMaxSuggestions, limit: MaxUISuggestions}
	}
	set := make(map[string]struct{}, len(providers))
	for index, provider := range providers {
		observed := index + 1
		if observed > MaxRegisteredProviders {
			return nil, &RegistrationError{kind: RegistrationTooManyProviders, observed: observed, limit: MaxRegisteredProviders}
		}
		if provider == "" || strings.TrimSpace(provider) != provider || strings.IndexFunc(provider, unicode.IsControl) >= 0 || !utf8.ValidString(provider) {
			return nil, &RegistrationError{kind: RegistrationInvalidProviderName, index: index}
		}
		if len(provider) > MaxProviderNameBytes {
			return nil, &RegistrationError{kind: RegistrationProviderNameTooLong, index: index, observed: len(provider), limit: MaxProviderNameBytes}
		}
		if _, exists := set[provider]; exists {
			return nil, &RegistrationError{kind: RegistrationDuplicateProvider, index: index, providerBytes: len(provider)}
		}
		set[provider] = struct{}{}
	}
	ordered := append([]string(nil), providers...)
	slices.Sort(ordered)
	return &CompletionCoordinator{providers: ordered, providerSet: set, uiMaxSuggestions: uiMaxSuggestions, nextGeneration: next, hasNextGeneration: hasNext, selection: selection.New()}, nil
}

// ReconfigureUILimit atomically validates a new bound, then cancels current work.
func (c *CompletionCoordinator) ReconfigureUILimit(limit int) error {
	if limit < 1 || limit > MaxUISuggestions {
		return &RegistrationError{kind: RegistrationInvalidUILimit, value: limit, limit: MaxUISuggestions}
	}
	c.uiMaxSuggestions = limit
	c.abandonActiveQuery()
	c.clearSelection()
	return nil
}

// StartQuery cancels prior authority, consumes a non-wrapping generation, and
// validates a UTF-8 shell buffer plus opaque absolute Unix cwd bytes.
func (c *CompletionCoordinator) StartQuery(line string, cursor int, cwd []byte) (QueryWork, error) {
	c.abandonActiveQuery()
	generation, ok := c.takeGeneration()
	if !ok {
		c.clearSelection()
		return QueryWork{}, &QueryStartError{kind: QueryGenerationExhausted}
	}
	c.selection.BeginQuery(generation, nil)
	fail := func(err *QueryStartError) (QueryWork, error) {
		c.lastCancelledGeneration = generation
		c.hasLastCancelledGeneration = true
		return QueryWork{}, err
	}
	if len(line) > MaxQueryLineBytes {
		return fail(&QueryStartError{kind: QueryLineTooLarge, bytes: len(line), limit: MaxQueryLineBytes})
	}
	if len(cwd) > MaxQueryCWDBytes {
		return fail(&QueryStartError{kind: QueryCWDTooLarge, bytes: len(cwd), limit: MaxQueryCWDBytes})
	}
	if len(cwd) == 0 || cwd[0] != '/' {
		return fail(&QueryStartError{kind: QueryCWDNotAbsolute})
	}
	if !utf8.ValidString(line) {
		return fail(&QueryStartError{kind: QueryInvalidUTF8, lineBytes: len(line)})
	}
	if cursor < 0 || cursor > len(line) || (cursor < len(line) && !utf8.RuneStart(line[cursor])) {
		return fail(&QueryStartError{kind: QueryInvalidCursor, cursor: cursor, lineBytes: len(line)})
	}
	query, err := completion.NewQuery(strings.Clone(line), cursor, bytes.Clone(cwd), generation)
	if err != nil {
		return fail(&QueryStartError{kind: QueryInvalidCursor, cursor: cursor, lineBytes: len(line)})
	}
	ctx, cancel := context.WithCancel(context.Background())
	queryPointer := &query
	c.active = &activeQuery{query: queryPointer, cancel: cancel, batches: make(map[string]storedBatch)}
	c.hasLastCancelledGeneration = false
	return QueryWork{query: queryPointer, cancellation: completion.ObserveCancellation(ctx)}, nil
}

// Close cancels active provider work. It is safe to call more than once.
func (c *CompletionCoordinator) Close() error {
	c.CancelActiveQuery()
	return nil
}

// CancelActiveQuery cancels active work and clears cumulative and selection state.
func (c *CompletionCoordinator) CancelActiveQuery() CancellationOutcome {
	generation, ok := c.abandonActiveQuery()
	if !ok {
		return CancellationOutcome{kind: CancellationNoActiveQuery}
	}
	return CancellationOutcome{kind: CancellationCancelled, generation: generation}
}

// ActiveQuery returns the active immutable query snapshot.
func (c *CompletionCoordinator) ActiveQuery() (completion.CompletionQuery, bool) {
	if c.active == nil {
		return completion.CompletionQuery{}, false
	}
	return *c.active.query, true
}

// Selection returns an immutable snapshot of bounded selection state.
func (c *CompletionCoordinator) Selection() selection.SelectionState { return *c.selection }

// SelectPrevious moves selection up without wrapping.
func (c *CompletionCoordinator) SelectPrevious() { c.selection.Up() }

// SelectNext moves selection down without wrapping.
func (c *CompletionCoordinator) SelectNext() { c.selection.Down() }

// DismissSuggestions hides suggestions until the buffer changes.
func (c *CompletionCoordinator) DismissSuggestions() { c.selection.Escape() }

// NoteBufferChanged makes current suggestions eligible for display.
func (c *CompletionCoordinator) NoteBufferChanged() { c.selection.BufferChanged() }

// ToggleSuggestionLayer changes the persistent suggestion-layer setting.
func (c *CompletionCoordinator) ToggleSuggestionLayer() { c.selection.ToggleVisible() }

// ProviderDiagnostics returns bounded provider state in lexical identity order.
func (c *CompletionCoordinator) ProviderDiagnostics() []ProviderDiagnostic {
	result := make([]ProviderDiagnostic, 0, len(c.providers))
	for _, provider := range c.providers {
		diagnostic := ProviderDiagnostic{provider: provider, phase: ProviderPending}
		if c.active != nil {
			if batch, ok := c.active.batches[provider]; ok {
				diagnostic.phase = batch.phase()
				diagnostic.candidateCount = len(batch.suggestions)
				diagnostic.err = batch.err
				diagnostic.hasError = batch.hasError
			}
		}
		result = append(result, diagnostic)
	}
	return slices.Clip(result)
}

// AcceptBatch atomically replaces one provider's cumulative contribution.
// The owning goroutine must call it after receiving an asynchronous result.
func (c *CompletionCoordinator) AcceptBatch(batch completion.ProviderBatch) BatchOutcome {
	provider, generation := batch.Provider(), batch.Generation()
	if authority := c.authority(generation); authority != nil {
		return rejectedBatch(&BatchRejection{kind: BatchAuthority, authority: authority})
	}
	if _, ok := c.providerSet[provider]; !ok {
		return rejectedBatch(&BatchRejection{kind: BatchUnknownProvider, providerBytes: len(provider)})
	}
	stored, rejection := validateBatch(batch)
	if rejection != nil {
		return rejectedBatch(rejection)
	}
	if authority := c.authority(generation); authority != nil {
		return rejectedBatch(&BatchRejection{kind: BatchAuthority, authority: authority})
	}
	previous, replaced := c.active.batches[provider]
	prospectiveCandidates := saturatedAdd(c.active.retainedCandidates-len(previous.suggestions), len(stored.suggestions))
	if prospectiveCandidates > MaxCumulativeCandidates {
		return rejectedBatch(&BatchRejection{kind: BatchTooManyCumulativeCandidates, observed: prospectiveCandidates, limit: MaxCumulativeCandidates})
	}
	prospectiveBytes := saturatedAdd(c.active.retainedBytes-previous.retainedBytes, stored.retainedBytes)
	if prospectiveBytes > MaxCumulativeBytes {
		return rejectedBatch(&BatchRejection{kind: BatchCumulativeBytesTooLarge, bytes: prospectiveBytes, limit: MaxCumulativeBytes})
	}
	kind := AcceptedSuccess
	if stored.hasError {
		kind = AcceptedFailure
	}
	c.active.batches[provider] = stored
	c.active.retainedCandidates, c.active.retainedBytes = prospectiveCandidates, prospectiveBytes
	return BatchOutcome{accepted: true, acceptance: BatchAcceptance{provider: provider, generation: generation, kind: kind, replaced: replaced, providerCandidates: len(stored.suggestions), cumulativeCandidates: prospectiveCandidates}}
}

// MergedCandidates returns every deduplicated contribution without ranking/UI limits.
func (c *CompletionCoordinator) MergedCandidates(generation uint64) ([]completion.Suggestion, error) {
	if authority := c.authority(generation); authority != nil {
		return nil, authority
	}
	return mergedFor(c.active), nil
}

// ApplyRanked validates an exact caller-ordered permutation and applies the UI bound.
func (c *CompletionCoordinator) ApplyRanked(generation uint64, ranked []completion.Suggestion) PresentationOutcome {
	if authority := c.authority(generation); authority != nil {
		return rejectedPresentation(&PresentationRejection{kind: PresentationAuthority, authority: authority})
	}
	merged := mergedFor(c.active)
	normalized, rejection := validateRanked(merged, ranked)
	if rejection != nil {
		return rejectedPresentation(rejection)
	}
	available := len(normalized)
	displayed := min(available, c.uiMaxSuggestions)
	switch c.selection.ApplyRankedUpdate(generation, normalized, c.uiMaxSuggestions) {
	case selection.UpdateApplied:
		return PresentationOutcome{kind: PresentationApplied, available: available, displayed: c.selection.CandidateCount()}
	case selection.UpdateSelectionConflict:
		return PresentationOutcome{kind: PresentationSelectionConflict, available: available, displayed: displayed}
	default:
		return rejectedPresentation(&PresentationRejection{kind: PresentationSelectionGenerationMismatch})
	}
}

func (c *CompletionCoordinator) authority(generation uint64) *AuthorityRejection {
	if c.active == nil {
		if c.hasLastCancelledGeneration && c.lastCancelledGeneration == generation {
			return &AuthorityRejection{kind: AuthorityCancelled, generation: generation}
		}
		return &AuthorityRejection{kind: AuthorityNoActiveQuery}
	}
	active := c.active.query.Generation()
	if generation != active {
		return &AuthorityRejection{kind: AuthorityGenerationMismatch, active: active, received: generation}
	}
	return nil
}
func (c *CompletionCoordinator) takeGeneration() (uint64, bool) {
	if !c.hasNextGeneration {
		return 0, false
	}
	value := c.nextGeneration
	if value == math.MaxUint64 {
		c.hasNextGeneration = false
	} else {
		c.nextGeneration++
	}
	return value, true
}
func (c *CompletionCoordinator) abandonActiveQuery() (uint64, bool) {
	if c.active == nil {
		return 0, false
	}
	generation := c.active.query.Generation()
	c.active.cancel()
	c.active = nil
	c.lastCancelledGeneration, c.hasLastCancelledGeneration = generation, true
	c.clearSelection()
	return generation, true
}
func (c *CompletionCoordinator) clearSelection() {
	c.selection.BeginQuery(c.selection.Generation(), nil)
}

func validateBatch(batch completion.ProviderBatch) (storedBatch, *BatchRejection) {
	suggestions := batch.Suggestions()
	providerError, hasError := batch.Error()
	if hasError && len(suggestions) != 0 {
		return storedBatch{}, &BatchRejection{kind: BatchConflictingSuccessAndFailure}
	}
	if len(suggestions) > MaxBatchCandidates {
		return storedBatch{}, &BatchRejection{kind: BatchTooManyCandidates, observed: len(suggestions), limit: MaxBatchCandidates}
	}
	if hasError {
		if len(providerError) > MaxProviderErrorBytes {
			return storedBatch{}, &BatchRejection{kind: BatchErrorTooLarge, bytes: len(providerError), limit: MaxProviderErrorBytes}
		}
		if strings.IndexFunc(providerError, unicode.IsControl) >= 0 || !utf8.ValidString(providerError) {
			return storedBatch{}, &BatchRejection{kind: BatchUnsafeErrorText}
		}
	}
	retainedBytes := 0
	if hasError {
		retainedBytes = len(providerError)
	}
	normalized := make([]completion.Suggestion, 0, len(suggestions))
	for index, candidate := range suggestions {
		owned := candidateBytes(candidate)
		if owned > MaxCandidateBytes {
			return storedBatch{}, &BatchRejection{kind: BatchCandidateTooLarge, index: index, bytes: owned, limit: MaxCandidateBytes}
		}
		retainedBytes = saturatedAdd(retainedBytes, owned)
		if retainedBytes > MaxBatchBytes {
			return storedBatch{}, &BatchRejection{kind: BatchTooLarge, bytes: retainedBytes, limit: MaxBatchBytes}
		}
		normalized = append(normalized, candidate)
	}
	return storedBatch{suggestions: slices.Clip(normalized), err: strings.Clone(providerError), hasError: hasError, retainedBytes: retainedBytes}, nil
}

func validateRanked(expected, ranked []completion.Suggestion) ([]completion.Suggestion, *PresentationRejection) {
	if len(ranked) > MaxCumulativeCandidates {
		return nil, &PresentationRejection{kind: PresentationTooManyCandidates, observed: len(ranked), limit: MaxCumulativeCandidates}
	}
	cumulative := 0
	for index, candidate := range ranked {
		owned := candidateBytes(candidate)
		if owned > MaxCandidateBytes {
			return nil, &PresentationRejection{kind: PresentationCandidateTooLarge, index: index, bytes: owned, limit: MaxCandidateBytes}
		}
		cumulative = saturatedAdd(cumulative, owned)
		if cumulative > MaxCumulativeBytes {
			return nil, &PresentationRejection{kind: PresentationCumulativeBytesTooLarge, bytes: cumulative, limit: MaxCumulativeBytes}
		}
	}
	if !exactPermutation(expected, ranked) {
		return nil, &PresentationRejection{kind: PresentationCandidateSetMismatch}
	}
	return slices.Clip(append([]completion.Suggestion(nil), ranked...)), nil
}

type candidateKey struct {
	start, end                              int
	replacement, display, description, icon string
	source                                  completion.SuggestionSource
	sources                                 []completion.SuggestionSource
	priority, confidence                    uint64
	insertion                               completion.InsertionBehavior
	identity                                string
}

func newCandidateKey(candidate completion.Suggestion) candidateKey {
	edit := candidate.Edit()
	return candidateKey{start: edit.Start(), end: edit.End(), replacement: edit.Replacement(), display: candidate.Display(), description: candidate.Description(), icon: candidate.Icon(), source: candidate.Source(), sources: candidate.Sources(), priority: math.Float64bits(candidate.StaticPriority()), confidence: math.Float64bits(candidate.Confidence()), insertion: candidate.Insertion(), identity: candidate.Identity()}
}
func compareCandidateKeys(left, right candidateKey) int {
	for _, pair := range [][2]int{{left.start, right.start}, {left.end, right.end}} {
		if order := cmp.Compare(pair[0], pair[1]); order != 0 {
			return order
		}
	}
	for _, pair := range [][2]string{{left.replacement, right.replacement}, {left.display, right.display}, {left.description, right.description}, {left.icon, right.icon}} {
		if order := strings.Compare(pair[0], pair[1]); order != 0 {
			return order
		}
	}
	if order := cmp.Compare(left.source, right.source); order != 0 {
		return order
	}
	for index := 0; index < min(len(left.sources), len(right.sources)); index++ {
		if order := cmp.Compare(left.sources[index], right.sources[index]); order != 0 {
			return order
		}
	}
	if order := cmp.Compare(len(left.sources), len(right.sources)); order != 0 {
		return order
	}
	if order := cmp.Compare(left.priority, right.priority); order != 0 {
		return order
	}
	if order := cmp.Compare(left.confidence, right.confidence); order != 0 {
		return order
	}
	if order := cmp.Compare(left.insertion, right.insertion); order != 0 {
		return order
	}
	return strings.Compare(left.identity, right.identity)
}
func exactPermutation(expected, actual []completion.Suggestion) bool {
	if len(expected) != len(actual) {
		return false
	}
	left := make([]candidateKey, len(expected))
	right := make([]candidateKey, len(actual))
	for i := range expected {
		left[i] = newCandidateKey(expected[i])
		right[i] = newCandidateKey(actual[i])
	}
	slices.SortFunc(left, compareCandidateKeys)
	slices.SortFunc(right, compareCandidateKeys)
	for i := range left {
		if compareCandidateKeys(left[i], right[i]) != 0 {
			return false
		}
	}
	return true
}
func candidateBytes(candidate completion.Suggestion) int {
	edit := candidate.Edit()
	total := len(edit.Replacement())
	for _, value := range []string{candidate.Display(), candidate.Description(), candidate.Icon(), candidate.Identity()} {
		total = saturatedAdd(total, len(value))
	}
	return total
}
func mergedFor(active *activeQuery) []completion.Suggestion {
	names := make([]string, 0, len(active.batches))
	for name := range active.batches {
		names = append(names, name)
	}
	slices.Sort(names)
	batches := make([][]completion.Suggestion, 0, len(names))
	for _, name := range names {
		batches = append(batches, active.batches[name].suggestions)
	}
	return completion.MergeSuggestions(*active.query, batches...)
}
func saturatedAdd(left, right int) int {
	if right > math.MaxInt-left {
		return math.MaxInt
	}
	return left + right
}
func rejectedBatch(rejection *BatchRejection) BatchOutcome { return BatchOutcome{rejection: rejection} }
func rejectedPresentation(rejection *PresentationRejection) PresentationOutcome {
	return PresentationOutcome{kind: PresentationRejected, rejection: rejection}
}

// String returns structural coordinator state without query, path, provider, candidate, or error text.
func (c *CompletionCoordinator) String() string {
	activeGeneration, lineBytes, cwdBytes, retainedCandidates, retainedBytes := "None", 0, 0, 0, 0
	if c.active != nil {
		activeGeneration = fmt.Sprintf("Some(%d)", c.active.query.Generation())
		lineBytes = len(c.active.query.Line())
		cwdBytes = len(c.active.query.CWD())
		retainedCandidates = c.active.retainedCandidates
		retainedBytes = c.active.retainedBytes
	}
	return fmt.Sprintf("CompletionCoordinator { provider_count: %d, ui_max_suggestions: %d, active_generation: %s, active_line_bytes: %d, active_cwd_bytes: %d, retained_candidates: %d, retained_bytes: %d, displayed_candidates: %d }", len(c.providers), c.uiMaxSuggestions, activeGeneration, lineBytes, cwdBytes, retainedCandidates, retainedBytes, c.selection.CandidateCount())
}

// GoString returns structural coordinator state with sensitive content redacted.
func (c *CompletionCoordinator) GoString() string { return c.String() }
