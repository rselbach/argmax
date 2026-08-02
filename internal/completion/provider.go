package completion

import (
	"context"
	"fmt"
	"strings"
	"unicode"
	"unicode/utf8"
)

// CancellationToken is an observer-only cooperative cancellation handle.
// Construction from a context grants no cancellation authority.
type CancellationToken struct {
	context context.Context
}

// ObserveCancellation creates an observer for a caller-owned context.
func ObserveCancellation(ctx context.Context) CancellationToken {
	if ctx == nil {
		ctx = context.Background()
	}
	return CancellationToken{context: ctx}
}

// IsCancelled reports whether work has lost authority.
func (t CancellationToken) IsCancelled() bool {
	if t.context == nil {
		return false
	}
	select {
	case <-t.context.Done():
		return true
	default:
		return false
	}
}

// String returns only cancellation state.
func (t CancellationToken) String() string {
	return fmt.Sprintf("CancellationToken { cancelled: %t }", t.IsCancelled())
}

// GoString returns only cancellation state.
func (t CancellationToken) GoString() string { return t.String() }

// ProviderBatch is one independently failing provider response.
type ProviderBatch struct {
	provider    string
	generation  uint64
	suggestions []Suggestion
	err         string
	hasError    bool
}

// NewProviderBatch creates an untrusted provider response without sanitizing its
// optional error. Coordinators validate it atomically; ordinary providers should
// prefer NewSuccessBatch or NewFailureBatch.
func NewProviderBatch(
	provider string,
	generation uint64,
	suggestions []Suggestion,
	providerError *string,
) ProviderBatch {
	batch := ProviderBatch{
		provider: provider, generation: generation, suggestions: cloneSuggestions(suggestions),
	}
	if providerError != nil {
		batch.err = *providerError
		batch.hasError = true
	}
	return batch
}

// NewSuccessBatch creates a successful provider response.
func NewSuccessBatch(provider string, generation uint64, suggestions []Suggestion) ProviderBatch {
	return NewProviderBatch(provider, generation, suggestions, nil)
}

// NewFailureBatch creates a sanitized, bounded provider failure.
func NewFailureBatch(provider string, generation uint64, err string) ProviderBatch {
	return ProviderBatch{
		provider: provider, generation: generation, err: sanitizeFailure(err), hasError: true,
	}
}

// Provider returns the exact claimed provider identity.
func (b ProviderBatch) Provider() string { return b.provider }

// Generation returns the generation that produced this response.
func (b ProviderBatch) Generation() uint64 { return b.generation }

// Suggestions returns a copy of the inert results.
func (b ProviderBatch) Suggestions() []Suggestion { return cloneSuggestions(b.suggestions) }

// Error returns the sanitized provider error and whether this is a failure.
func (b ProviderBatch) Error() (string, bool) { return b.err, b.hasError }

// String returns a content-redacted representation.
func (b ProviderBatch) String() string {
	return fmt.Sprintf(
		"ProviderBatch { provider_bytes: %d, generation: %d, suggestion_count: %d, has_error: %t }",
		len(b.provider), b.generation, len(b.suggestions), b.hasError,
	)
}

// GoString returns a content-redacted representation.
func (b ProviderBatch) GoString() string { return b.String() }

// CompletionProvider computes inert suggestions without terminal-output access.
type CompletionProvider interface {
	Name() string
	Complete(query CompletionQuery, cancellation CancellationToken) ProviderBatch
}

func sanitizeFailure(value string) string {
	var sanitized strings.Builder
	sanitized.Grow(min(len(value), MaxProviderFailureBytes))
	for _, character := range value {
		visible := character
		switch {
		case character == 0x1b:
			continue
		case character == '\n' || character == '\r' || character == '\t':
			visible = ' '
		case unicode.IsControl(character):
			continue
		}
		if sanitized.Len()+utf8.RuneLen(visible) > MaxProviderFailureBytes {
			break
		}
		sanitized.WriteRune(visible)
	}
	return sanitized.String()
}

func cloneSuggestions(values []Suggestion) []Suggestion {
	if values == nil {
		return nil
	}
	cloned := make([]Suggestion, len(values))
	for index, value := range values {
		value.sources = append([]SuggestionSource(nil), value.sources...)
		cloned[index] = value
	}
	return cloned
}
