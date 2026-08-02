package history

import (
	"sort"
	"strings"
)

// Match tiers, higher is better.
const (
	tierExact  = 4
	tierPrefix = 3
	tierWords  = 2
	tierFuzzy  = 1
)

const (
	// emptyQueryLimit caps results for an empty query.
	emptyQueryLimit = 100
	// strictLimit caps strict all-word matches retained before fuzzy lookup.
	strictLimit = 200
	// minFuzzyScore rejects extremely weak fuzzy matches.
	minFuzzyScore = 30
)

// Match is one search result.
type Match struct {
	Entry Entry
	Tier  int
	Score int
	// index preserves recency order within a tier.
	index int
}

// Search ranks entries against query: exact, prefix, substring/all-word,
// then fuzzy-subsequence matches, ordered by tier, recency within a tier,
// and fuzzy score within the fuzzy tier. aliasForms optionally maps a
// command to alternate spellings (alias and expansion) so either form is
// found.
func Search(entries []Entry, query string, aliasForms func(string) []string) []Match {
	if strings.TrimSpace(query) == "" {
		n := min(len(entries), emptyQueryLimit)
		out := make([]Match, 0, n)
		for i, e := range entries[:n] {
			out = append(out, Match{Entry: e, Tier: tierExact, index: i})
		}
		return out
	}
	var (
		strict []Match
		fuzzy  []Match
		q      = strings.ToLower(query)
		words  = strings.Fields(q)
	)
	for i, e := range entries {
		forms := []string{e.Command}
		if aliasForms != nil {
			forms = append(forms, aliasForms(e.Command)...)
		}
		tier, score := 0, 0
		for _, form := range forms {
			t, s := matchOne(strings.ToLower(form), q, words)
			if t > tier || (t == tier && s > score) {
				tier, score = t, s
			}
		}
		switch {
		case tier >= tierWords:
			if len(strict) < strictLimit {
				strict = append(strict, Match{Entry: e, Tier: tier, Score: score, index: i})
			}
		case tier == tierFuzzy && score >= minFuzzyScore:
			fuzzy = append(fuzzy, Match{Entry: e, Tier: tier, Score: score, index: i})
		}
	}
	out := append(strict, fuzzy...)
	sort.SliceStable(out, func(i, j int) bool {
		a, b := out[i], out[j]
		if a.Tier != b.Tier {
			return a.Tier > b.Tier
		}
		if a.Tier == tierFuzzy && a.Score != b.Score {
			return a.Score > b.Score
		}
		return a.index < b.index
	})
	return out
}

func matchOne(cmd, q string, words []string) (tier, score int) {
	switch {
	case cmd == q:
		return tierExact, 100
	case strings.HasPrefix(cmd, q):
		return tierPrefix, 90
	case strings.Contains(cmd, q), allWords(cmd, words):
		return tierWords, 70
	}
	if s := subsequenceScore(cmd, q); s > 0 {
		return tierFuzzy, s
	}
	return 0, 0
}

func allWords(cmd string, words []string) bool {
	if len(words) < 2 {
		return false
	}
	for _, w := range words {
		if !strings.Contains(cmd, w) {
			return false
		}
	}
	return true
}

// subsequenceScore returns 0 when q is not a subsequence of cmd, otherwise
// a 1-100 quality score favoring compact, early matches.
func subsequenceScore(cmd, q string) int {
	if q == "" {
		return 0
	}
	start, pos := -1, 0
	for i := 0; i < len(cmd) && pos < len(q); i++ {
		if cmd[i] != q[pos] {
			continue
		}
		if pos == 0 {
			start = i
		}
		pos++
		if pos == len(q) {
			span := i - start + 1
			// Density of the matched window and an early-start bonus.
			score := 100 * len(q) / span
			if start == 0 {
				score += 10
			}
			return min(score, 100)
		}
	}
	return 0
}
