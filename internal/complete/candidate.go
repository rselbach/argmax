// Package complete contains the completion data model: candidates,
// tokenization, command specifications, and the traversal engine that turns
// a typed buffer into ranked completion candidates.
package complete

import "strings"

// Source identifies where a candidate came from.
type Source string

// Candidate sources.
const (
	SourceSpec     Source = "spec"
	SourceAlias    Source = "alias"
	SourceSystem   Source = "system"
	SourceHistory  Source = "history"
	SourceInferred Source = "inferred"
	SourceAI       Source = "ai"
	SourceFile     Source = "file"
)

// Candidate is one completion suggestion. Generating or highlighting a
// candidate never changes shell state; only explicit user acceptance does.
type Candidate struct {
	// Text is the full command line after acceptance.
	Text string
	// Title is the short display form, such as the subcommand or file name.
	Title string
	// Insert overrides the token text a generator candidate inserts when
	// it differs from Title, such as a path keeping its typed directory
	// prefix. Empty means insert Title.
	Insert string
	// Description is a short human explanation.
	Description string
	// Icon is a category or ecosystem key resolved by the UI.
	Icon string
	// Source identifies the candidate origin.
	Source Source
	// Confidence is the source confidence from 0-100.
	Confidence int
	// Priority is the author-defined priority from 0-100; 0 means unset.
	Priority int
	// IsDirectory marks directory completions, which suppress the trailing
	// space so path traversal can continue.
	IsDirectory bool
}

// Dedupe removes candidates whose final command text repeats an earlier
// entry and drops exact copies of the current query, keeping candidates
// the user may be discovering: aliases, and documented flags whose
// description explains what was just typed.
func Dedupe(cands []Candidate, query string) []Candidate {
	seen := make(map[string]int, len(cands))
	out := cands[:0]
	for _, c := range cands {
		if c.Text == query && c.Source != SourceAlias && !documentsFlag(c) {
			continue
		}
		if i, ok := seen[c.Text]; ok {
			// Permit a higher-confidence duplicate (such as AI) to replace
			// the earlier row without producing a second one.
			if c.Confidence > out[i].Confidence {
				out[i] = c
			}
			continue
		}
		seen[c.Text] = len(out)
		out = append(out, c)
	}
	return out
}

// documentsFlag reports whether an exact-match candidate still teaches
// the user something: a flag row whose description explains the typed
// option.
func documentsFlag(c Candidate) bool {
	return c.Description != "" && strings.HasPrefix(c.Title, "-")
}
