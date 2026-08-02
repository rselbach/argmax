package history

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// writeFile writes a history fixture into t.TempDir() and returns its path.
func writeFile(t *testing.T, name, contents string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(path, []byte(contents), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func commands(ms []Match) []string {
	out := make([]string, len(ms))
	for i, m := range ms {
		out[i] = m.Command
	}
	return out
}

func entryCommands(es []Entry) []string {
	out := make([]string, len(es))
	for i, e := range es {
		out[i] = e.Command
	}
	return out
}

func TestMissingFileIsEmptyHistory(t *testing.T) {
	p := New("bash", filepath.Join(t.TempDir(), "does-not-exist"))
	if got := p.Entries(); len(got) != 0 {
		t.Fatalf("Entries() = %v, want empty", got)
	}
	if got := p.Search("git", nil, 0); len(got) != 0 {
		t.Fatalf("Search() = %v, want empty", got)
	}

	// The file appearing later is picked up (mtime advances from zero).
	path := filepath.Join(t.TempDir(), "late")
	p = New("zsh", path)
	_ = p.Entries()
	if err := os.WriteFile(path, []byte("late command\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got := entryCommands(p.Entries()); len(got) != 1 || got[0] != "late command" {
		t.Fatalf("Entries() after file appeared = %v", got)
	}
}

func TestAddSessionBeforeFileFlush(t *testing.T) {
	// Session command visible even though the history file was never written.
	p := New("bash", filepath.Join(t.TempDir(), "missing"))
	p.AddSession("session only")
	got := p.Entries()
	if len(got) != 1 || got[0].Command != "session only" {
		t.Fatalf("Entries() = %v", got)
	}
	if got[0].Time.IsZero() {
		t.Fatal("session entry has zero Time")
	}

	// Session commands sort ahead of file commands, newest first.
	path := writeFile(t, "hist", "file one\nfile two\n")
	p = New("bash", path)
	p.AddSession("session one")
	p.AddSession("session two")
	want := []string{"session two", "session one", "file two", "file one"}
	if got := entryCommands(p.Entries()); strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("Entries() = %v, want %v", got, want)
	}
}

func TestAddSessionConsecutiveDuplicates(t *testing.T) {
	p := New("bash", filepath.Join(t.TempDir(), "missing"))
	p.AddSession("same")
	p.AddSession("same")
	p.AddSession("same")
	if got := entryCommands(p.Entries()); len(got) != 1 || got[0] != "same" {
		t.Fatalf("Entries() = %v, want one [same] row", got)
	}

	// Non-consecutive repeat is a new submission but de-dupes to one row.
	p.AddSession("other")
	p.AddSession("same")
	want := []string{"same", "other"}
	if got := entryCommands(p.Entries()); strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("Entries() = %v, want %v", got, want)
	}
}

func TestFileDuplicatesKeepNewest(t *testing.T) {
	path := writeFile(t, "hist", "#1000\ndup\n#2000\nother\n#3000\ndup\n")
	got := New("bash", path).Entries()
	want := []string{"dup", "other"}
	if strings.Join(entryCommands(got), "|") != strings.Join(want, "|") {
		t.Fatalf("Entries() = %v, want %v", entryCommands(got), want)
	}
	if got[0].Time != time.Unix(3000, 0) {
		t.Fatalf("dup Time = %v, want %v (newest occurrence)", got[0].Time, time.Unix(3000, 0))
	}
}

func TestMtimeAdvanceInvalidatesCache(t *testing.T) {
	path := writeFile(t, "hist", "first\n")
	p := New("bash", path)
	if got := entryCommands(p.Entries()); len(got) != 1 || got[0] != "first" {
		t.Fatalf("Entries() = %v", got)
	}

	if err := os.WriteFile(path, []byte("second\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	// Force the mtime forward so the change is detected regardless of the
	// filesystem's timestamp granularity.
	future := time.Now().Add(2 * time.Hour)
	if err := os.Chtimes(path, future, future); err != nil {
		t.Fatal(err)
	}
	if got := entryCommands(p.Entries()); len(got) != 1 || got[0] != "second" {
		t.Fatalf("Entries() after mtime advance = %v, want [second]", got)
	}
}

func TestShellParserSelection(t *testing.T) {
	zf := writeFile(t, "zsh_hist", ": 1691234567:0;git status\n")
	if got := New("zsh", zf).Entries(); len(got) != 1 || got[0].Time != time.Unix(1691234567, 0) {
		t.Fatalf("zsh Entries() = %v", got)
	}
	ff := writeFile(t, "fish_hist", "- cmd: fish cmd\n  when: 1691234567\n")
	if got := New("fish", ff).Entries(); len(got) != 1 || got[0].Command != "fish cmd" {
		t.Fatalf("fish Entries() = %v", got)
	}
}

func TestSearchEmptyQuery(t *testing.T) {
	var sb strings.Builder
	for i := 1; i <= 150; i++ {
		fmt.Fprintf(&sb, "cmd%03d\n", i)
	}
	p := New("bash", writeFile(t, "hist", sb.String()))

	got := p.Search("", nil, 0)
	if len(got) != 100 {
		t.Fatalf("empty query returned %d matches, want 100", len(got))
	}
	if got[0].Command != "cmd150" || got[99].Command != "cmd051" {
		t.Fatalf("empty query order = [%s ... %s], want newest-first cmd150...cmd051", got[0].Command, got[99].Command)
	}
	for _, m := range got {
		if m.Tier != TierPrefix {
			t.Fatalf("empty query match %q has Tier %d, want %d (TierPrefix)", m.Command, m.Tier, TierPrefix)
		}
	}

	if got := p.Search("  ", nil, 5); len(got) != 5 {
		t.Fatalf("whitespace query with limit 5 returned %d matches", len(got))
	}
}

func TestSearchTierOrdering(t *testing.T) {
	path := writeFile(t, "hist", "git status\ngit stash\ngit commit\ngrep git\n")
	p := New("bash", path)

	got := p.Search("git", nil, 0)
	wantCmd := []string{"git commit", "git stash", "git status", "grep git"}
	wantTier := []int{TierPrefix, TierPrefix, TierPrefix, TierSubstring}
	if len(got) != len(wantCmd) {
		t.Fatalf("Search(git) = %v", commands(got))
	}
	for i := range wantCmd {
		if got[i].Command != wantCmd[i] || got[i].Tier != wantTier[i] {
			t.Errorf("match %d = (%q, tier %d), want (%q, tier %d)",
				i, got[i].Command, got[i].Tier, wantCmd[i], wantTier[i])
		}
	}

	// An exact match beats every prefix match.
	p.AddSession("git")
	got = p.Search("git", nil, 0)
	if got[0].Command != "git" || got[0].Tier != TierExact {
		t.Fatalf("first match = (%q, tier %d), want exact (git, tier 0)", got[0].Command, got[0].Tier)
	}
}

func TestSearchAllWordSubstring(t *testing.T) {
	path := writeFile(t, "hist", "git commit --amend\namended the notes\n")
	p := New("bash", path)
	got := p.Search("commit amend", nil, 0)
	if len(got) != 1 || got[0].Command != "git commit --amend" || got[0].Tier != TierSubstring {
		t.Fatalf("Search(commit amend) = %v, want one tier-2 match", commands(got))
	}
}

func TestSearchFuzzy(t *testing.T) {
	// Ordered by score, not recency: "git commit -m x" scores 62 (boundary
	// and streak bonuses), "gclm" scores 49 (weaker anchoring, one gap).
	path := writeFile(t, "hist", "git commit -m x\ngclm\n")
	p := New("bash", path)

	got := p.Search("gcm", nil, 0)
	want := []string{"git commit -m x", "gclm"}
	if strings.Join(commands(got), "|") != strings.Join(want, "|") {
		t.Fatalf("Search(gcm) = %v, want %v", commands(got), want)
	}
	for _, m := range got {
		if m.Tier != TierFuzzy {
			t.Errorf("%q: Tier = %d, want TierFuzzy", m.Command, m.Tier)
		}
	}
	if got[0].Score <= got[1].Score {
		t.Errorf("fuzzy not ordered by score: %d then %d", got[0].Score, got[1].Score)
	}

	// "gtsc" is a subsequence of both "git status commit" (tight, kept) and
	// "xgxxxxtxxxsxxxc" (scattered, score 30 < 4*8, rejected).
	path = writeFile(t, "hist2", "xgxxxxtxxxsxxxc\ngit status commit\n")
	p = New("bash", path)
	got = p.Search("gtsc", nil, 0)
	if len(got) != 1 || got[0].Command != "git status commit" || got[0].Tier != TierFuzzy {
		t.Fatalf("Search(gtsc) = %v, want only [git status commit] tier 3", commands(got))
	}

	// Not a subsequence at all.
	if got := p.Search("gzz", nil, 0); len(got) != 0 {
		t.Fatalf("Search(gzz) = %v, want empty", commands(got))
	}
}

func TestSearchStrictCap(t *testing.T) {
	var sb strings.Builder
	for i := 1; i <= 250; i++ {
		fmt.Fprintf(&sb, "run foo item%03d\n", i)
	}
	p := New("bash", writeFile(t, "hist", sb.String()))

	got := p.Search("foo", nil, 0)
	if len(got) != 200 {
		t.Fatalf("Search(foo) returned %d matches, want exactly 200", len(got))
	}
	for _, m := range got {
		if m.Tier > TierSubstring {
			t.Fatalf("fuzzy tier appended despite strict cap: %q tier %d", m.Command, m.Tier)
		}
	}
	// The newest 200 survive the cap.
	if got[0].Command != "run foo item250" || got[199].Command != "run foo item051" {
		t.Fatalf("cap kept [%s ... %s], want [run foo item250 ... run foo item051]",
			got[0].Command, got[199].Command)
	}
}

func TestSearchAliasAware(t *testing.T) {
	path := writeFile(t, "hist", "git checkout main\ngco main\n")
	aliases := map[string]string{"gco": "git checkout"}

	p := New("bash", path)
	got := p.Search("gco", aliases, 0)
	want := []string{"gco main", "git checkout main"} // both, newest first
	if strings.Join(commands(got), "|") != strings.Join(want, "|") {
		t.Fatalf("Search(gco) = %v, want %v", commands(got), want)
	}

	// The reverse direction: the expanded query finds the alias spelling.
	got = p.Search("git checkout", aliases, 0)
	want = []string{"gco main", "git checkout main"}
	if strings.Join(commands(got), "|") != strings.Join(want, "|") {
		t.Fatalf("Search(git checkout) = %v, want %v", commands(got), want)
	}

	// Both spellings exact-match their own entry; no duplicates.
	got = p.Search("gco main", aliases, 0)
	if len(got) != 2 {
		t.Fatalf("Search(gco main) = %v, want 2 matches without duplicates", commands(got))
	}
	for _, m := range got {
		if m.Tier != TierExact {
			t.Errorf("%q: Tier = %d, want TierExact", m.Command, m.Tier)
		}
	}
}

func TestSearchLimit(t *testing.T) {
	path := writeFile(t, "hist", "git one\ngit two\ngit three\ngit four\n")
	p := New("bash", path)
	if got := p.Search("git", nil, 2); len(got) != 2 {
		t.Fatalf("limit 2 returned %d matches", len(got))
	}
	if got := p.Search("git", nil, -1); len(got) != 4 {
		t.Fatalf("limit -1 returned %d matches, want all 4", len(got))
	}
	if got := p.Search("", nil, 3); len(got) != 3 {
		t.Fatalf("empty query with limit 3 returned %d matches", len(got))
	}
}

func TestSearchRecencyWithinTier(t *testing.T) {
	path := writeFile(t, "hist", "git older\ngit middle\ngit newer\n")
	p := New("bash", path)
	got := p.Search("git", nil, 0)
	want := []string{"git newer", "git middle", "git older"}
	if strings.Join(commands(got), "|") != strings.Join(want, "|") {
		t.Fatalf("Search(git) = %v, want newest-first %v", commands(got), want)
	}
}
