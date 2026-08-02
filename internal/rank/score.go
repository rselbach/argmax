package rank

import (
	"sort"
	"strings"

	"github.com/rselbach/argmax/internal/core"
)

// Signal weights for the weighted score (PRD 9.11).
const (
	weightBase       = 0.30
	weightContext    = 0.25
	weightFrecency   = 0.15
	weightTransition = 0.10
	weightMatch      = 0.20
)

// Candidate pairs a suggestion with engine-computed context.
type Candidate struct {
	Suggestion   core.Suggestion
	Skeleton     string  // normalized command skeleton (engine-provided)
	MatchQuality float64 // 0-100 per PRD 10.2, engine-computed against the query
}

// Env carries the ranking environment for one result set.
type Env struct {
	CWD          string
	WS           *Workspace // may be nil → no context bonuses
	Store        *Store     // may be nil → static deterministic ranking
	PrevSkeleton string
}

// scoredCandidate is a Candidate with its computed ranking signals.
type scoredCandidate struct {
	Candidate
	base       float64
	context    float64
	frecency   float64
	transition float64
	total      float64
}

// Score computes the weighted score of every candidate (PRD 9.11): base
// priority 30%, workspace context 25%, frecency 15%, transition 10%, match
// quality 20%. It returns the candidates sorted descending with deterministic
// tie-breaking (RANK-007): total score, transition, frecency, context bonus,
// then command text. The input slice is not modified.
//
// Frecency and transition are normalized to 0-100 per result set (RANK-006);
// base priorities and context bonuses are clamped to safe ranges. Score is
// pure and fast: it performs no I/O beyond Store queries, and every Store
// query carries its own 500 ms timeout so a slow database degrades to static
// ranking instead of stalling completion (RANK-012).
func Score(cands []Candidate, env Env) []Candidate {
	scored := scoreCandidates(cands, env)
	out := make([]Candidate, len(scored))
	for i := range scored {
		out[i] = scored[i].Candidate
	}
	return out
}

func scoreCandidates(cands []Candidate, env Env) []scoredCandidate {
	if len(cands) == 0 {
		return nil
	}
	scored := make([]scoredCandidate, len(cands))
	var maxFrecency, maxTransition float64
	for i, c := range cands {
		sc := scoredCandidate{Candidate: c}
		sc.base = basePriority(c.Suggestion)
		sc.context = contextBonus(env.WS, c.Suggestion.Text)
		if env.Store != nil && c.Skeleton != "" {
			sc.frecency = env.Store.Frecency(env.CWD, c.Skeleton)
			if env.PrevSkeleton != "" {
				sc.transition = env.Store.Transition(env.CWD, env.PrevSkeleton, c.Skeleton)
			}
		}
		if sc.frecency > maxFrecency {
			maxFrecency = sc.frecency
		}
		if sc.transition > maxTransition {
			maxTransition = sc.transition
		}
		scored[i] = sc
	}
	for i := range scored {
		s := &scored[i]
		s.frecency = normalize(s.frecency, maxFrecency)
		s.transition = normalize(s.transition, maxTransition)
		s.total = weightBase*s.base + weightContext*s.context +
			weightFrecency*s.frecency + weightTransition*s.transition +
			weightMatch*clamp(s.MatchQuality, 0, 100)
	}
	sort.SliceStable(scored, func(i, j int) bool {
		return ranksBefore(scored[i], scored[j])
	})
	return scored
}

// ranksBefore implements the deterministic tie-breaking chain (RANK-007).
func ranksBefore(a, b scoredCandidate) bool {
	if a.total != b.total {
		return a.total > b.total
	}
	if a.transition != b.transition {
		return a.transition > b.transition
	}
	if a.frecency != b.frecency {
		return a.frecency > b.frecency
	}
	if a.context != b.context {
		return a.context > b.context
	}
	return a.Suggestion.Text < b.Suggestion.Text
}

// normalize scales a raw learned signal to 0-100 relative to the strongest
// candidate in the result set (RANK-006).
func normalize(v, max float64) float64 {
	if max <= 0 {
		return 0
	}
	return v / max * 100
}

// basePriority resolves the candidate's base priority (PRD 10.1): an explicit
// author Priority (>= 0) clamped to 0-100, otherwise a per-source default.
func basePriority(s core.Suggestion) float64 {
	if s.Priority >= 0 {
		return clamp(float64(s.Priority), 0, 100)
	}
	switch s.Source {
	case core.SourceSpec:
		return 60
	case core.SourceAI:
		if s.Confidence > 0 {
			return clamp(float64(s.Confidence), 0, 100)
		}
		return 50
	case core.SourceHistory:
		if s.Confidence > 0 {
			return clamp(float64(s.Confidence), 0, 100)
		}
		return 40
	default:
		// File/directory generators and every other source.
		return 50
	}
}

// contextBonus applies the workspace context rules of PRD 10.3. These are
// heuristic string rules, not semantic analysis: the candidate text is split
// into whitespace tokens and the first tokens are compared literally against
// well-known command and subcommand names (e.g. "git checkout …" matches the
// git rule pair ("git", "checkout")). Bonuses accumulate across rules and are
// clamped to -100..100 (RANK-006).
func contextBonus(ws *Workspace, text string) float64 {
	if ws == nil {
		return 0
	}
	tok := strings.Fields(text)
	if len(tok) == 0 {
		return 0
	}
	t0 := tok[0]
	var t1, t2 string
	if len(tok) > 1 {
		t1 = tok[1]
	}
	if len(tok) > 2 {
		t2 = tok[2]
	}
	var bonus float64
	if ws.Has(SigGit) && t0 == "git" {
		switch t1 {
		case "status", "diff", "add", "push", "pull", "commit", "switch", "checkout", "branch":
			bonus += 25
		case "init", "clone":
			bonus -= 30 // de-prioritize repo creation inside a repo (RANK-010)
		}
		if ws.GitBranch != "" && referencesBranch(tok, ws.GitBranch) {
			bonus += 35 // explicit current-branch reference (RANK-011)
		}
	}
	if ws.Has(SigNode) {
		switch t0 {
		case "npm", "pnpm", "yarn", "bun":
			switch t1 {
			case "run", "test", "start", "install", "add":
				bonus += 20
			}
		}
	}
	if ws.Has(SigGo) && t0 == "go" {
		switch t1 {
		case "test", "run", "build", "vet", "fmt":
			bonus += 20
		case "mod":
			if t2 == "tidy" {
				bonus += 20
			}
		}
	}
	if ws.Has(SigRust) && t0 == "cargo" {
		switch t1 {
		case "test", "run", "build", "check", "clippy":
			bonus += 20
		}
	}
	if ws.Has(SigPython) {
		switch t0 {
		case "pytest", "python", "python3", "pip", "pip3", "poetry", "uv":
			bonus += 20
		}
	}
	if ws.Has(SigJust) && t0 == "just" {
		bonus += 20
	}
	if ws.Has(SigMake) {
		switch t0 {
		case "make", "gcc", "g++", "clang", "cc", "c++", "cmake":
			bonus += 15
		}
	}
	if ws.Has(SigDocker) {
		if (t0 == "docker" && (t1 == "build" || t1 == "compose")) || t0 == "docker-compose" {
			bonus += 20
		}
	}
	if ws.Has(SigKubernetes) && (t0 == "kubectl" || t0 == "helm") {
		bonus += 20
	}
	return clamp(bonus, -100, 100)
}

// referencesBranch reports whether any token of the candidate is exactly the
// current branch name (RANK-011).
func referencesBranch(tok []string, branch string) bool {
	for _, t := range tok {
		if t == branch {
			return true
		}
	}
	return false
}

func clamp(v, lo, hi float64) float64 {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}
