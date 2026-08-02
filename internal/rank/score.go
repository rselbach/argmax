package rank

import (
	"sort"
	"strings"

	"github.com/rselbach/argmax/internal/complete"
)

// Signal weights per the ranking contract.
const (
	weightBase       = 0.30
	weightContext    = 0.25
	weightFrecency   = 0.15
	weightTransition = 0.10
	weightMatch      = 0.20
)

// Signals carries the collected raw ranking evidence for one query.
type Signals struct {
	// Frecency maps candidate command text to a raw frecency weight.
	Frecency map[string]float64
	// Transitions maps candidate skeletons to a raw transition weight.
	Transitions map[string]float64
	// Workspace holds the detected workspace signatures.
	Workspace Workspace
	// Skeleton resolves a command line to its normalized skeleton.
	Skeleton func(string) string
}

// Rank orders candidates by the weighted score with stable deterministic
// tie-breaking: total score, transition, frecency, context bonus, then
// command text.
func Rank(cands []complete.Candidate, query string, sig Signals) []complete.Candidate {
	type scored struct {
		c          complete.Candidate
		total      float64
		transition float64
		frecency   float64
		context    float64
	}
	frecency := normalize(sig.Frecency)
	transitions := normalize(sig.Transitions)
	items := make([]scored, 0, len(cands))
	for _, c := range cands {
		var skeleton string
		if sig.Skeleton != nil {
			skeleton = sig.Skeleton(c.Text)
		}
		s := scored{
			c:          c,
			frecency:   frecency[c.Text],
			transition: transitions[skeleton],
			context:    contextBonus(c.Text, sig.Workspace),
		}
		s.total = weightBase*basePriority(c) +
			weightContext*clamp(s.context, -100, 100) +
			weightFrecency*s.frecency +
			weightTransition*s.transition +
			weightMatch*float64(MatchQuality(query, c.Text))
		items = append(items, s)
	}
	sort.SliceStable(items, func(i, j int) bool {
		a, b := items[i], items[j]
		switch {
		case a.total != b.total:
			return a.total > b.total
		case a.transition != b.transition:
			return a.transition > b.transition
		case a.frecency != b.frecency:
			return a.frecency > b.frecency
		case a.context != b.context:
			return a.context > b.context
		}
		return a.c.Text < b.c.Text
	})
	out := make([]complete.Candidate, len(items))
	for i, s := range items {
		out[i] = s.c
	}
	return out
}

// basePriority implements the documented base-priority defaults.
func basePriority(c complete.Candidate) float64 {
	if c.Priority > 0 {
		return clamp(float64(c.Priority), 0, 100)
	}
	switch c.Source {
	case complete.SourceSpec:
		return 60
	case complete.SourceAI:
		if c.Confidence > 0 {
			return clamp(float64(c.Confidence), 0, 100)
		}
		return 50
	case complete.SourceHistory:
		if c.Confidence > 0 {
			return clamp(float64(c.Confidence), 0, 100)
		}
		return 40
	default:
		return 50
	}
}

// MatchQuality implements the documented match-quality tiers between the
// typed query and a candidate command.
func MatchQuality(query, candidate string) int {
	if query == "" {
		return 0
	}
	switch {
	case strings.HasPrefix(candidate, query):
		return 100
	case len(candidate) >= len(query) && strings.EqualFold(candidate[:len(query)], query):
		return 80
	case strings.Contains(strings.ToLower(candidate), strings.ToLower(query)):
		return 50
	case isSubsequence(strings.ToLower(query), strings.ToLower(candidate)):
		return 30
	default:
		return 0
	}
}

func isSubsequence(needle, haystack string) bool {
	if needle == "" {
		return false
	}
	pos := 0
	for i := 0; i < len(haystack) && pos < len(needle); i++ {
		if haystack[i] == needle[pos] {
			pos++
		}
	}
	return pos == len(needle)
}

// normalize scales raw weights to 0-100 within the result set.
func normalize(raw map[string]float64) map[string]float64 {
	if len(raw) == 0 {
		return nil
	}
	max := 0.0
	for _, v := range raw {
		if v > max {
			max = v
		}
	}
	if max <= 0 {
		return nil
	}
	out := make(map[string]float64, len(raw))
	for k, v := range raw {
		out[k] = 100 * v / max
	}
	return out
}

// contextRule boosts candidates matching a command prefix when the
// workspace signature is present.
type contextRule struct {
	signature string
	prefix    string
	bonus     float64
}

var contextRules = []contextRule{
	{"git", "git status", 40}, {"git", "git diff", 35}, {"git", "git add", 35},
	{"git", "git push", 30}, {"git", "git pull", 30}, {"git", "git commit", 35},
	{"git", "git switch", 30}, {"git", "git checkout", 30}, {"git", "git branch", 25},
	{"git", "git init", -40}, {"git", "git clone", -40},
	{"node", "npm ", 30}, {"node", "pnpm ", 30}, {"node", "yarn ", 30}, {"node", "bun ", 30},
	{"go", "go test", 35}, {"go", "go run", 30}, {"go", "go build", 30},
	{"go", "go mod tidy", 25}, {"go", "go vet", 20}, {"go", "gofmt", 15},
	{"rust", "cargo test", 35}, {"rust", "cargo run", 30}, {"rust", "cargo build", 30},
	{"rust", "cargo check", 25}, {"rust", "cargo clippy", 25},
	{"python", "pytest", 35}, {"python", "python", 25}, {"python", "pip ", 20},
	{"python", "poetry ", 25}, {"python", "uv ", 25},
	{"just", "just", 40},
	{"make", "make", 40}, {"make", "cmake", 15}, {"make", "gcc", 10}, {"make", "g++", 10},
	{"docker", "docker build", 30}, {"docker", "docker compose", 35}, {"docker", "docker ", 20},
	{"kubernetes", "kubectl ", 35}, {"kubernetes", "helm ", 30},
}

// contextBonus accumulates workspace rules, clamped to -100..100, with a
// stronger bonus for Git commands explicitly referencing the active
// branch.
func contextBonus(command string, ws Workspace) float64 {
	if len(ws.Signatures) == 0 {
		return 0
	}
	total := 0.0
	for _, r := range contextRules {
		if ws.Has(r.signature) && strings.HasPrefix(command, r.prefix) {
			total += r.bonus
		}
	}
	if ws.GitBranch != "" && strings.HasPrefix(command, "git ") &&
		containsWord(command, ws.GitBranch) {
		total += 25
	}
	return clamp(total, -100, 100)
}

func containsWord(command, word string) bool {
	for _, f := range strings.Fields(command) {
		if f == word {
			return true
		}
	}
	return false
}

func clamp(v, lo, hi float64) float64 {
	switch {
	case v < lo:
		return lo
	case v > hi:
		return hi
	}
	return v
}
