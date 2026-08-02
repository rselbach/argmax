// Package completion defines inert completion queries, edits, suggestions, and
// provider responses. Shell buffers are UTF-8; working directories are opaque
// Unix path bytes and may be non-UTF-8.
package completion

import (
	"bytes"
	"fmt"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"
)

// MaxProviderFailureBytes is the maximum sanitized provider failure retained.
const MaxProviderFailureBytes = 8 * 1024

// CompletionQuery is an immutable completion input snapshot.
type CompletionQuery struct {
	line       string
	cursor     int
	cwd        []byte
	generation uint64
}

// NewQuery validates a UTF-8 shell buffer and byte cursor. cwd is preserved as
// opaque Unix path bytes.
func NewQuery(line string, cursor int, cwd []byte, generation uint64) (CompletionQuery, error) {
	if !utf8.ValidString(line) {
		return CompletionQuery{}, fmt.Errorf("completion line is not UTF-8")
	}
	if !utf8Boundary(line, cursor) {
		return CompletionQuery{}, fmt.Errorf("cursor %d is not a valid UTF-8 boundary", cursor)
	}
	return CompletionQuery{
		line: line, cursor: cursor, cwd: bytes.Clone(cwd), generation: generation,
	}, nil
}

// Line returns the complete editable shell buffer.
func (q CompletionQuery) Line() string { return q.line }

// Cursor returns the UTF-8 byte cursor.
func (q CompletionQuery) Cursor() int { return q.cursor }

// CWD returns a copy of the opaque Unix working-directory bytes.
func (q CompletionQuery) CWD() []byte { return bytes.Clone(q.cwd) }

// Generation returns the query generation.
func (q CompletionQuery) Generation() uint64 { return q.generation }

// Prefix returns the authoritative buffer prefix before the cursor.
func (q CompletionQuery) Prefix() string { return q.line[:q.cursor] }

// String returns a content-redacted representation.
func (q CompletionQuery) String() string {
	return fmt.Sprintf(
		"CompletionQuery { generation: %d, cursor: %d, line_bytes: %d, cwd_bytes: %d }",
		q.generation, q.cursor, len(q.line), len(q.cwd),
	)
}

// GoString returns a content-redacted representation.
func (q CompletionQuery) GoString() string { return q.String() }

// TextEdit is an inert replacement over byte offsets in an original query.
type TextEdit struct {
	start       int
	end         int
	replacement string
}

// NewTextEdit creates edit metadata. The range is validated against the query
// only when applied or merged; this permits providers' invalid edits to be
// safely represented and discarded.
func NewTextEdit(start, end int, replacement string) (TextEdit, error) {
	if !utf8.ValidString(replacement) {
		return TextEdit{}, fmt.Errorf("edit replacement is not UTF-8")
	}
	return TextEdit{start: start, end: end, replacement: replacement}, nil
}

// Start returns the inclusive byte offset.
func (e TextEdit) Start() int { return e.start }

// End returns the exclusive byte offset.
func (e TextEdit) End() int { return e.end }

// Replacement returns the replacement shell-buffer text.
func (e TextEdit) Replacement() string { return e.replacement }

// Apply applies the edit without executing it.
func (e TextEdit) Apply(line string) (string, error) {
	if !utf8.ValidString(line) || e.start < 0 || e.start > e.end ||
		!utf8Boundary(line, e.start) || !utf8Boundary(line, e.end) {
		return "", fmt.Errorf("invalid edit range %d..%d", e.start, e.end)
	}
	var result strings.Builder
	result.Grow(len(line) - (e.end - e.start) + len(e.replacement))
	result.WriteString(line[:e.start])
	result.WriteString(e.replacement)
	result.WriteString(line[e.end:])
	return result.String(), nil
}

// String returns a content-redacted representation.
func (e TextEdit) String() string {
	return fmt.Sprintf("TextEdit { range: %d..%d, replacement_bytes: %d }", e.start, e.end, len(e.replacement))
}

// GoString returns a content-redacted representation.
func (e TextEdit) GoString() string { return e.String() }

// SuggestionSource identifies suggestion provenance.
type SuggestionSource uint8

const (
	// SourceAlias identifies a shell-alias suggestion.
	SourceAlias SuggestionSource = iota + 1
	// SourceSpec identifies a built-in command-specification suggestion.
	SourceSpec
	// SourceSpecInferred identifies an inferred command-specification suggestion.
	SourceSpecInferred
	// SourceSystem identifies a suggestion discovered from the local system.
	SourceSystem
	// SourceFile identifies a filesystem suggestion.
	SourceFile
	// SourceHistory identifies a shell-history suggestion.
	SourceHistory
	// SourceAI identifies an optional AI suggestion.
	SourceAI
)

// Badge returns the stable user-facing source badge.
func (s SuggestionSource) Badge() string {
	switch s {
	case SourceAlias:
		return "alias"
	case SourceSpec:
		return "spec"
	case SourceSpecInferred:
		return "inferred"
	case SourceSystem:
		return "system"
	case SourceFile:
		return "file"
	case SourceHistory:
		return "history"
	case SourceAI:
		return "ai"
	default:
		return ""
	}
}

func (s SuggestionSource) strength() uint8 {
	switch s {
	case SourceSpec:
		return 7
	case SourceAlias:
		return 6
	case SourceHistory:
		return 5
	case SourceSpecInferred:
		return 4
	case SourceFile:
		return 3
	case SourceSystem:
		return 2
	case SourceAI:
		return 1
	default:
		return 0
	}
}

// InsertionBehavior explicitly controls insertion resolution.
type InsertionBehavior uint8

const (
	// InsertionExact inserts the replacement unchanged.
	InsertionExact InsertionBehavior = iota + 1
	// InsertionAppendSpace appends a space when insertion ends the line.
	InsertionAppendSpace
	// InsertionDirectory appends a path separator when one is absent.
	InsertionDirectory
)

// Suggestion is an immutable inert completion candidate.
type Suggestion struct {
	edit           TextEdit
	display        string
	description    string
	icon           string
	source         SuggestionSource
	sources        []SuggestionSource
	staticPriority float64
	confidence     float64
	insertion      InsertionBehavior
	identity       string
}

// NewSuggestion creates a sanitized suggestion with one initial provenance.
func NewSuggestion(
	edit TextEdit,
	display, description, icon string,
	source SuggestionSource,
	insertion InsertionBehavior,
	identity string,
) (Suggestion, error) {
	for name, value := range map[string]string{
		"display": display, "description": description, "icon": icon, "identity": identity,
	} {
		if !utf8.ValidString(value) {
			return Suggestion{}, fmt.Errorf("suggestion %s is not UTF-8", name)
		}
	}
	if source < SourceAlias || source > SourceAI {
		return Suggestion{}, fmt.Errorf("invalid suggestion source %d", source)
	}
	if insertion < InsertionExact || insertion > InsertionDirectory {
		return Suggestion{}, fmt.Errorf("invalid insertion behavior %d", insertion)
	}
	edit.replacement = strings.Map(func(character rune) rune {
		if unicode.IsControl(character) && character != '\n' && character != '\t' {
			return -1
		}
		return character
	}, edit.replacement)
	return Suggestion{
		edit:           edit,
		display:        SanitizeTerminalText(display),
		description:    SanitizeTerminalText(description),
		icon:           icon,
		source:         source,
		sources:        []SuggestionSource{source},
		staticPriority: 0.5,
		confidence:     0.5,
		insertion:      insertion,
		identity:       identity,
	}, nil
}

// WithRanking returns a copy with provider ranking metadata.
func (s Suggestion) WithRanking(staticPriority, confidence float64) Suggestion {
	s.staticPriority = staticPriority
	s.confidence = confidence
	return s
}

// Edit returns validated replacement metadata before insertion resolution.
func (s Suggestion) Edit() TextEdit { return s.edit }

// Display returns sanitized candidate display text.
func (s Suggestion) Display() string { return s.display }

// Description returns sanitized explanatory text.
func (s Suggestion) Description() string { return s.description }

// Icon returns the candidate icon identifier.
func (s Suggestion) Icon() string { return s.icon }

// Source returns the strongest candidate provenance.
func (s Suggestion) Source() SuggestionSource { return s.source }

// Sources returns a copy of all merged candidate provenance.
func (s Suggestion) Sources() []SuggestionSource { return slices.Clone(s.sources) }

// StaticPriority returns provider-supplied static ranking metadata.
func (s Suggestion) StaticPriority() float64 { return s.staticPriority }

// Confidence returns provider-supplied confidence metadata.
func (s Suggestion) Confidence() float64 { return s.confidence }

// Insertion returns the insertion-resolution behavior.
func (s Suggestion) Insertion() InsertionBehavior { return s.insertion }

// Identity returns the stable provider identity used for deterministic merging.
func (s Suggestion) Identity() string { return s.identity }

// ResultingLine returns the complete inert line produced by this suggestion.
func (s Suggestion) ResultingLine(query CompletionQuery) (string, error) {
	edit, err := s.ResolvedEdit(query.line)
	if err != nil {
		return "", err
	}
	return edit.Apply(query.line)
}

// ResolvedEdit turns insertion metadata into the exact shell edit.
func (s Suggestion) ResolvedEdit(line string) (TextEdit, error) {
	if _, err := s.edit.Apply(line); err != nil {
		return TextEdit{}, err
	}
	replacement := s.edit.replacement
	suffix := line[s.edit.end:]
	switch s.insertion {
	case InsertionExact:
	case InsertionAppendSpace:
		lastWhitespace := false
		if replacement != "" {
			last, _ := utf8.DecodeLastRuneInString(replacement)
			lastWhitespace = unicode.IsSpace(last)
		}
		if !lastWhitespace && suffix == "" {
			replacement += " "
		}
	case InsertionDirectory:
		if !strings.HasSuffix(replacement, "/") && !strings.HasPrefix(suffix, "/") {
			replacement += "/"
		}
	}
	return TextEdit{start: s.edit.start, end: s.edit.end, replacement: replacement}, nil
}

func (s *Suggestion) mergeMetadata(other Suggestion) {
	for _, source := range other.sources {
		if !slices.Contains(s.sources, source) {
			s.sources = append(s.sources, source)
		}
	}
	slices.Sort(s.sources)
	if other.source.strength() > s.source.strength() {
		s.source = other.source
	}
	if len(other.description) > len(s.description) {
		s.description = other.description
	}
	if s.icon == "" && other.icon != "" {
		s.icon = other.icon
	}
	s.staticPriority = maxFloat(s.staticPriority, other.staticPriority)
	s.confidence = maxFloat(s.confidence, other.confidence)
	if other.identity < s.identity {
		s.identity = other.identity
	}
}

// String returns a content-redacted representation.
func (s Suggestion) String() string {
	return fmt.Sprintf(
		"Suggestion { edit: %s, display_bytes: %d, description_bytes: %d, icon_bytes: %d, source: %d, source_count: %d, static_priority: %v, confidence: %v, insertion: %d, identity_bytes: %d }",
		s.edit, len(s.display), len(s.description), len(s.icon), s.source,
		len(s.sources), s.staticPriority, s.confidence, s.insertion, len(s.identity),
	)
}

// GoString returns a content-redacted representation.
func (s Suggestion) GoString() string { return s.String() }

// SanitizeTerminalText removes escape/control data and makes line controls visible.
func SanitizeTerminalText(value string) string {
	return strings.Map(func(character rune) rune {
		switch {
		case character == 0x1b:
			return -1
		case character == '\n' || character == '\r' || character == '\t':
			return ' '
		case unicode.IsControl(character):
			return -1
		default:
			return character
		}
	}, value)
}

func utf8Boundary(value string, index int) bool {
	return index >= 0 && index <= len(value) && (index == len(value) || utf8.RuneStart(value[index]))
}

func maxFloat(left, right float64) float64 {
	// Rust f64::max returns the non-NaN argument when exactly one is NaN and
	// positive zero when comparing signed zeroes.
	if left != left {
		return right
	}
	if right != right {
		return left
	}
	if left == 0 && right == 0 {
		return 0
	}
	if left > right {
		return left
	}
	return right
}
