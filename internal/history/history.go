// Package history lazily loads shell history files (Bash, Zsh, Fish),
// merges commands submitted during the current session, and provides
// alias-aware ranked search over the merged history.
//
// It implements the history retrieval behavior from PRD sections 8.3 and
// 9.10 (HIST-001..012): lazy loading with mtime-based cache invalidation,
// session merge before the shell flushes its file, newest-first
// de-duplication, and four-tier match ranking (exact, prefix,
// substring/all-word, fuzzy subsequence).
package history

import (
	"os"
	"sort"
	"strings"
	"sync"
	"time"
)

// Entry is one history command.
type Entry struct {
	Command string
	Time    time.Time // zero when unknown (bash without timestamps)
}

// Match is a ranked search result.
type Match struct {
	Command string
	Time    time.Time
	Tier    int // 0 exact, 1 prefix, 2 substring/all-word, 3 fuzzy subsequence
	Score   int // fuzzy score (only meaningful in tier 3)
}

// Match tiers returned by Search, best first.
const (
	TierExact     = 0 // command equals the query
	TierPrefix    = 1 // command starts with the query (case-insensitive)
	TierSubstring = 2 // every query word appears in the command (case-insensitive)
	TierFuzzy     = 3 // query is a subsequence of the command (case-insensitive)
)

const (
	// emptyQueryCap bounds the result of an empty query (HIST-007).
	emptyQueryCap = 100
	// strictCap bounds how many strict (tier 0-2) matches are retained
	// before fuzzy lookup (HIST-009). When the cap is reached, no fuzzy
	// tier is appended.
	strictCap = 200
	// fuzzyPerChar is the minimum acceptable fuzzy score per query
	// character; see fuzzyMatch for the rule.
	fuzzyPerChar = 8
)

// Provider lazily loads and caches one history file. The zero value is not
// usable; create one with New. A Provider is safe for concurrent use.
type Provider struct {
	shell string // "bash" | "zsh" | "fish", selects the parser
	path  string

	once sync.Once
	mu   sync.Mutex
	mt   time.Time // mod time of the loaded file; zero when missing/unreadable
	file []Entry   // parsed file entries, newest-first
	sess []Entry   // session entries, newest-first
}

// New creates a provider for the given history file path (already resolved by
// the caller, e.g. via the shell package). shell is "bash"|"zsh"|"fish" and
// selects the parser; unknown values fall back to the Bash parser, which
// treats the file as plain lines with optional timestamp markers.
func New(shell, path string) *Provider {
	return &Provider{shell: shell, path: path}
}

// AddSession merges a command submitted during the current session (HIST-004).
// Consecutive duplicate submissions create one row (HIST-005).
func (p *Provider) AddSession(cmd string) {
	if cmd == "" {
		return
	}
	p.once.Do(p.load)
	p.mu.Lock()
	defer p.mu.Unlock()
	if len(p.sess) > 0 && p.sess[0].Command == cmd {
		return
	}
	p.sess = append([]Entry{{Command: cmd, Time: time.Now()}}, p.sess...)
}

// Entries returns merged history (file + session), newest-first, exact
// duplicates removed keeping the newest occurrence (HIST-005). It lazily
// loads on first call (HIST-001); the cache is invalidated when the file
// mtime advances (HIST-006); a missing file is an empty history, never an
// error (HIST-012).
func (p *Provider) Entries() []Entry {
	p.once.Do(p.load)
	p.mu.Lock()
	defer p.mu.Unlock()
	p.refreshLocked()

	// Session entries are newer than any file entry and both slices are
	// newest-first, so the first occurrence of a command is the newest.
	merged := make([]Entry, 0, len(p.sess)+len(p.file))
	seen := make(map[string]struct{}, len(p.sess)+len(p.file))
	for _, e := range p.sess {
		if _, ok := seen[e.Command]; ok {
			continue
		}
		seen[e.Command] = struct{}{}
		merged = append(merged, e)
	}
	for _, e := range p.file {
		if _, ok := seen[e.Command]; ok {
			continue
		}
		seen[e.Command] = struct{}{}
		merged = append(merged, e)
	}
	return merged
}

// load performs the initial parse; it runs exactly once via sync.Once.
func (p *Provider) load() {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.reloadLocked()
}

// refreshLocked reloads the file when its mtime has advanced (HIST-006).
func (p *Provider) refreshLocked() {
	fi, err := os.Stat(p.path)
	if err != nil {
		return // missing file stays an empty history (HIST-012)
	}
	if !fi.ModTime().After(p.mt) {
		return
	}
	p.reloadLocked()
}

// reloadLocked re-reads and re-parses the history file. Callers hold p.mu.
func (p *Provider) reloadLocked() {
	p.file = nil
	p.mt = time.Time{}
	fi, err := os.Stat(p.path)
	if err != nil {
		return
	}
	data, err := os.ReadFile(p.path)
	if err != nil {
		return
	}
	var ents []Entry
	switch p.shell {
	case "zsh":
		ents = ParseZsh(data)
	case "fish":
		ents = ParseFish(data)
	default: // "bash" and unknown shells
		ents = ParseBash(data)
	}
	// Parsers return oldest->newest; the provider keeps newest-first.
	for i, j := 0, len(ents)-1; i < j; i, j = i+1, j-1 {
		ents[i], ents[j] = ents[j], ents[i]
	}
	p.file = ents
	p.mt = fi.ModTime()
}

// Search implements HIST-007..010:
//   - empty query: up to 100 newest unique commands (tier 1)
//   - non-empty: tier 0 exact, tier 1 prefix, tier 2 substring where ALL
//     whitespace-separated query words appear (case-insensitive), tier 3
//     fuzzy subsequence of the whole query; ordered by tier, then recency
//     within tiers 0-2, then fuzzy score within tier 3
//   - at most 200 strict (tier 0-2) matches are retained before fuzzy
//     lookup; when the cap is reached no fuzzy tier is appended (HIST-009)
//   - extremely weak fuzzy matches are rejected (see fuzzyMatch)
//   - aliases maps alias name → expansion; when the query starts with an
//     alias name it is also searched with the alias replaced by its
//     expansion, and vice versa (HIST-010); results merge without duplicates
//   - limit caps the final result count; limit<=0 means no cap
func (p *Provider) Search(query string, aliases map[string]string, limit int) []Match {
	entries := p.Entries()
	q := strings.TrimSpace(query)
	if q == "" {
		n := min(emptyQueryCap, len(entries))
		out := make([]Match, 0, n)
		for _, e := range entries[:n] {
			out = append(out, Match{Command: e.Command, Time: e.Time, Tier: TierPrefix})
		}
		return applyLimit(out, limit)
	}

	variants := queryVariants(q, aliases)

	// Pass 1: strict tiers. entries is newest-first and unique, so each
	// tier bucket stays newest-first with no duplicates.
	var tiers [3][]Match
	matched := make(map[string]struct{})
	for _, e := range entries {
		tier, ok := bestStrictTier(e.Command, variants)
		if !ok {
			continue
		}
		tiers[tier] = append(tiers[tier], Match{Command: e.Command, Time: e.Time, Tier: tier})
		matched[e.Command] = struct{}{}
	}
	strict := make([]Match, 0, len(entries))
	for t := 0; t < 3; t++ {
		strict = append(strict, tiers[t]...)
	}
	if len(strict) >= strictCap {
		return applyLimit(strict[:strictCap], limit)
	}

	// Pass 2: fuzzy over the commands not already matched strictly.
	var fuzzy []Match
	for _, e := range entries {
		if _, ok := matched[e.Command]; ok {
			continue
		}
		best := -1
		for _, v := range variants {
			if score, ok := fuzzyMatch(e.Command, v); ok && score > best {
				best = score
			}
		}
		if best < 0 {
			continue
		}
		fuzzy = append(fuzzy, Match{Command: e.Command, Time: e.Time, Tier: TierFuzzy, Score: best})
	}
	// Highest score first; stable so equal scores stay newest-first.
	sort.SliceStable(fuzzy, func(i, j int) bool { return fuzzy[i].Score > fuzzy[j].Score })
	return applyLimit(append(strict, fuzzy...), limit)
}

// queryVariants returns the query plus alias-expanded forms (HIST-010): for
// every alias name → expansion, when the query starts with the name (as its
// first word) the name is replaced by the expansion, and vice versa. The
// original query is always first; variants are de-duplicated.
func queryVariants(query string, aliases map[string]string) []string {
	variants := []string{query}
	seen := map[string]bool{query: true}
	add := func(v string) {
		if !seen[v] {
			seen[v] = true
			variants = append(variants, v)
		}
	}
	for name, exp := range aliases {
		if name == "" || exp == "" || name == exp {
			continue
		}
		for _, pair := range [][2]string{{name, exp}, {exp, name}} {
			from, to := pair[0], pair[1]
			switch {
			case query == from:
				add(to)
			case strings.HasPrefix(query, from+" "):
				add(to + query[len(from):])
			}
		}
	}
	return variants
}

// bestStrictTier returns the best (lowest) strict tier at which cmd matches
// any query variant, or ok=false when no variant matches strictly.
func bestStrictTier(cmd string, variants []string) (tier int, ok bool) {
	best := -1
	for _, v := range variants {
		if t, hit := strictTier(cmd, v); hit && (best < 0 || t < best) {
			best = t
		}
	}
	if best < 0 {
		return 0, false
	}
	return best, true
}

// strictTier classifies cmd against a single query for tiers 0-2. Exact is
// case-sensitive; prefix and all-word substring are case-insensitive.
func strictTier(cmd, q string) (int, bool) {
	if cmd == q {
		return TierExact, true
	}
	lcmd, lq := strings.ToLower(cmd), strings.ToLower(q)
	if strings.HasPrefix(lcmd, lq) {
		return TierPrefix, true
	}
	for _, w := range strings.Fields(lq) {
		if !strings.Contains(lcmd, w) {
			return 0, false
		}
	}
	return TierSubstring, true
}

// fuzzyMatch scores the whole query as a subsequence of cmd,
// case-insensitive, and reports whether the match is strong enough to keep.
//
// Scoring: +10 per matched character, +15 when the match sits on a word
// boundary (start of the command or after a space, '/', '-', '_' or '.'),
// +5 when it directly follows the previous match (streak bonus), and -1 per
// skipped command character between matches (gap penalty).
//
// A match is rejected when score < len(query)*8. Contiguous runs always
// score at least +10 per character, so a score below 8 per character means
// the characters are scattered across the command with wide gaps and weak
// anchoring (HIST-009). For example "gco" matches "git commit -am \"x\""
// (score 62 >= 24) while "gzz" never matches "git status" — it is not even
// a subsequence.
func fuzzyMatch(cmd, q string) (score int, ok bool) {
	lcmd, lq := strings.ToLower(cmd), strings.ToLower(q)
	score, prev, qi := 0, -1, 0
	for ci := 0; ci < len(lcmd) && qi < len(lq); ci++ {
		if lcmd[ci] != lq[qi] {
			continue
		}
		score += 10
		if ci == 0 || isBoundary(lcmd[ci-1]) {
			score += 15
		}
		if prev >= 0 {
			if ci == prev+1 {
				score += 5
			} else {
				score -= ci - prev - 1
			}
		}
		prev = ci
		qi++
	}
	if qi < len(lq) {
		return 0, false // query is not a subsequence of cmd
	}
	if score < len(q)*fuzzyPerChar {
		return 0, false // too scattered
	}
	return score, true
}

// isBoundary reports whether c separates words inside a command line.
func isBoundary(c byte) bool {
	switch c {
	case ' ', '/', '-', '_', '.':
		return true
	}
	return false
}

// applyLimit truncates ms to limit entries; limit<=0 means no cap.
func applyLimit(ms []Match, limit int) []Match {
	if limit > 0 && len(ms) > limit {
		return ms[:limit]
	}
	return ms
}
