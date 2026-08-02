package history

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestParseBash(t *testing.T) {
	content := "#1700000000\ngit status\nls -la\n#1700000100\nmake build\n"
	got := Parse(content, FormatBash)
	if len(got) != 3 {
		t.Fatalf("parsed %d entries, want 3", len(got))
	}
	if got[0].Command != "git status" || got[0].Time.Unix() != 1700000000 {
		t.Errorf("first entry = %+v, want git status @1700000000", got[0])
	}
	if got[1].Command != "ls -la" || !got[1].Time.IsZero() {
		t.Errorf("untimestamped entry = %+v", got[1])
	}
}

func TestParseZsh(t *testing.T) {
	content := ": 1700000000:0;git status\n: 1700000005:2;echo one \\\ntwo\nplain command\n"
	got := Parse(content, FormatZsh)
	if len(got) != 3 {
		t.Fatalf("parsed %d entries, want 3: %+v", len(got), got)
	}
	if got[0].Command != "git status" || got[0].Time.Unix() != 1700000000 {
		t.Errorf("first entry = %+v", got[0])
	}
	if got[1].Command != "echo one \ntwo" {
		t.Errorf("continuation entry = %q", got[1].Command)
	}
	if got[2].Command != "plain command" {
		t.Errorf("plain entry = %q", got[2].Command)
	}
}

func TestParseFish(t *testing.T) {
	content := "- cmd: git status\n  when: 1700000000\n- cmd: make test\n  when: 1700000500\n  paths:\n    - Makefile\n"
	got := Parse(content, FormatFish)
	if len(got) != 2 {
		t.Fatalf("parsed %d entries, want 2", len(got))
	}
	if got[0].Command != "git status" || got[0].Time.Unix() != 1700000000 {
		t.Errorf("first entry = %+v", got[0])
	}
	if got[1].Command != "make test" {
		t.Errorf("second entry = %q", got[1].Command)
	}
}

func TestProviderMissingFileIsEmpty(t *testing.T) {
	p := NewProvider(filepath.Join(t.TempDir(), "nope"), FormatBash)
	if got := p.Entries(); len(got) != 0 {
		t.Errorf("missing history file should be empty, got %d entries", len(got))
	}
}

func TestProviderSessionMergeAndDedupe(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history")
	if err := os.WriteFile(path, []byte("old command\ngit status\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	p := NewProvider(path, FormatBash)
	p.AddSession("git status") // duplicates a persistent entry
	p.AddSession("new command")
	p.AddSession("new command") // consecutive duplicate collapses

	got := p.Entries()
	if len(got) != 3 {
		t.Fatalf("merged %d entries, want 3: %+v", len(got), got)
	}
	if got[0].Command != "new command" {
		t.Errorf("newest session command first, got %q", got[0].Command)
	}
	if got[1].Command != "git status" {
		t.Errorf("session copy wins over persistent duplicate, got %q", got[1].Command)
	}
}

func TestProviderInvalidatesOnModTime(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history")
	if err := os.WriteFile(path, []byte("first\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	p := NewProvider(path, FormatBash)
	if got := p.Entries(); len(got) != 1 {
		t.Fatalf("initial load got %d entries", len(got))
	}
	if err := os.WriteFile(path, []byte("first\nsecond\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	future := time.Now().Add(2 * time.Second)
	if err := os.Chtimes(path, future, future); err != nil {
		t.Fatal(err)
	}
	if got := p.Entries(); len(got) != 2 {
		t.Errorf("after modification got %d entries, want 2", len(got))
	}
}

func TestSearchTiers(t *testing.T) {
	entries := []Entry{
		{Command: "docker compose up"},
		{Command: "git status"},
		{Command: "git stash pop"},
		{Command: "grep -r sts ."},
		{Command: "git status --short"},
	}
	got := Search(entries, "git status", nil)
	if len(got) == 0 {
		t.Fatal("no matches")
	}
	if got[0].Entry.Command != "git status" {
		t.Errorf("exact match first, got %q", got[0].Entry.Command)
	}
	if got[1].Entry.Command != "git status --short" {
		t.Errorf("prefix match second, got %q", got[1].Entry.Command)
	}
	for _, m := range got {
		if m.Entry.Command == "docker compose up" {
			t.Error("unrelated command must not match")
		}
	}
}

func TestSearchEmptyQueryReturnsNewest(t *testing.T) {
	entries := make([]Entry, 150)
	for i := range entries {
		entries[i] = Entry{Command: string(rune('a'+i%26)) + "-cmd-" + string(rune('0'+i%10))}
	}
	got := Search(entries, "", nil)
	if len(got) > 100 {
		t.Errorf("empty query returned %d, want at most 100", len(got))
	}
}

func TestSearchAliasForms(t *testing.T) {
	entries := []Entry{{Command: "gco feature/login"}}
	aliasForms := func(cmd string) []string {
		if cmd == "gco feature/login" {
			return []string{"git checkout feature/login"}
		}
		return nil
	}
	got := Search(entries, "git checkout", aliasForms)
	if len(got) != 1 {
		t.Fatalf("alias-form search found %d matches, want 1", len(got))
	}
}

func TestSearchFuzzy(t *testing.T) {
	entries := []Entry{{Command: "docker compose up --build"}}
	got := Search(entries, "dkrup", nil)
	if len(got) != 1 {
		t.Fatalf("fuzzy subsequence should match, got %d", len(got))
	}
	if got[0].Tier != tierFuzzy {
		t.Errorf("tier = %d, want fuzzy", got[0].Tier)
	}
	// Extremely weak subsequences are rejected.
	weak := []Entry{{Command: "a123456789b123456789c123456789d123456789e123456789f123456789g123456789h1"}}
	if got := Search(weak, "abcdefgh", nil); len(got) != 0 {
		t.Errorf("weak fuzzy match should be rejected, got %v", got)
	}
}
