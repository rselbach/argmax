// Package core defines the shared types used across argmax packages.
package core

// Source identifies where a suggestion originated.
type Source string

const (
	SourceSpec      Source = "spec"      // bundled command specification
	SourceAlias     Source = "alias"     // shell alias
	SourceToolAlias Source = "toolalias" // git/cargo alias
	SourceSystem    Source = "system"    // executable found on PATH
	SourceHistory   Source = "history"   // shell history
	SourceInferred  Source = "inferred"  // cobra __complete inference
	SourceAI        Source = "ai"        // AI completion
	SourceFile      Source = "file"      // file/directory completion
	SourceDynamic   Source = "dynamic"   // live generator value
)

// Suggestion is a single completion candidate.
type Suggestion struct {
	// Text is the completed command text as it would be inserted.
	Text string
	// Description is a short human-readable explanation.
	Description string
	// Icon is a category/icon key (e.g. "git", "docker"); empty uses a fallback glyph.
	Icon string
	// Source identifies the producing source.
	Source Source
	// Confidence is the source confidence, 0-100.
	Confidence int
	// Priority is an optional author-defined priority, 0-100. -1 means unset.
	Priority int
}

// Mode is the suggestion mode.
type Mode int

const (
	// ModeSpec ranks specs, aliases, PATH executables, dynamic values, and AI.
	ModeSpec Mode = iota
	// ModeHistory prioritizes history matches.
	ModeHistory
)

func (m Mode) String() string {
	if m == ModeHistory {
		return "history"
	}
	return "spec"
}

// ParseMode resolves a configured mode name. "last" is resolved by the caller
// from persisted state and never returned here.
func ParseMode(s string) (Mode, bool) {
	switch s {
	case "spec":
		return ModeSpec, true
	case "history":
		return ModeHistory, true
	}
	return ModeSpec, false
}
