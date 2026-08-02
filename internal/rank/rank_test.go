package rank

import (
	"context"
	"os"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/complete"
)

func TestMatchQuality(t *testing.T) {
	tests := map[string]struct {
		query     string
		candidate string
		want      int
	}{
		"exact prefix":     {query: "git ch", candidate: "git checkout", want: 100},
		"case-insensitive": {query: "GIT ch", candidate: "git checkout", want: 80},
		"substring":        {query: "check", candidate: "git checkout", want: 50},
		"subsequence":      {query: "gco", candidate: "git checkout", want: 30},
		"unrelated":        {query: "docker", candidate: "git checkout", want: 0},
		"empty query":      {query: "", candidate: "git", want: 0},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := MatchQuality(tc.query, tc.candidate); got != tc.want {
				t.Errorf("MatchQuality(%q, %q) = %d, want %d", tc.query, tc.candidate, got, tc.want)
			}
		})
	}
}

func TestBasePriorityDefaults(t *testing.T) {
	tests := map[string]struct {
		c    complete.Candidate
		want float64
	}{
		"explicit priority":       {c: complete.Candidate{Priority: 90, Source: complete.SourceSpec}, want: 90},
		"spec default":            {c: complete.Candidate{Source: complete.SourceSpec}, want: 60},
		"ai with confidence":      {c: complete.Candidate{Source: complete.SourceAI, Confidence: 85}, want: 85},
		"ai without confidence":   {c: complete.Candidate{Source: complete.SourceAI}, want: 50},
		"history with confidence": {c: complete.Candidate{Source: complete.SourceHistory, Confidence: 70}, want: 70},
		"history default":         {c: complete.Candidate{Source: complete.SourceHistory}, want: 40},
		"file generator":          {c: complete.Candidate{Source: complete.SourceFile}, want: 50},
		"other source":            {c: complete.Candidate{Source: complete.SourceSystem}, want: 50},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := basePriority(tc.c); got != tc.want {
				t.Errorf("basePriority(%+v) = %v, want %v", tc.c, got, tc.want)
			}
		})
	}
}

func TestRankDeterministicTieBreak(t *testing.T) {
	cands := []complete.Candidate{
		{Text: "beta", Source: complete.SourceSystem},
		{Text: "alpha", Source: complete.SourceSystem},
	}
	got := Rank(cands, "", Signals{})
	if got[0].Text != "alpha" || got[1].Text != "beta" {
		t.Errorf("equal scores must tie-break by command text: %v, %v", got[0].Text, got[1].Text)
	}
}

func TestRankPrefersFrecency(t *testing.T) {
	cands := []complete.Candidate{
		{Text: "git stash", Source: complete.SourceSpec},
		{Text: "git status", Source: complete.SourceSpec},
	}
	sig := Signals{Frecency: map[string]float64{"git status": 500}}
	got := Rank(cands, "git st", sig)
	if got[0].Text != "git status" {
		t.Errorf("frecency should promote git status, got %q first", got[0].Text)
	}
}

func TestContextBonusGitInit(t *testing.T) {
	ws := Workspace{Signatures: map[string]bool{"git": true}}
	if b := contextBonus("git init", ws); b >= 0 {
		t.Errorf("git init inside a repo should be negative, got %v", b)
	}
	if b := contextBonus("git status", ws); b <= 0 {
		t.Errorf("git status inside a repo should be positive, got %v", b)
	}
}

func TestContextBonusActiveBranch(t *testing.T) {
	ws := Workspace{Signatures: map[string]bool{"git": true}, GitBranch: "main"}
	with := contextBonus("git checkout main", ws)
	without := contextBonus("git checkout other", ws)
	if with <= without {
		t.Errorf("active-branch reference should score higher: %v <= %v", with, without)
	}
}

func TestRecencyBuckets(t *testing.T) {
	now := time.Now()
	tests := map[string]struct {
		age  time.Duration
		want float64
	}{
		"within hour":  {age: 30 * time.Minute, want: 100},
		"within day":   {age: 5 * time.Hour, want: 50},
		"within week":  {age: 3 * 24 * time.Hour, want: 20},
		"within month": {age: 20 * 24 * time.Hour, want: 5},
		"ancient":      {age: 90 * 24 * time.Hour, want: 1},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got := recencyBucket(now.Add(-tc.age).Unix(), now)
			if got != tc.want {
				t.Errorf("recencyBucket(%v ago) = %v, want %v", tc.age, got, tc.want)
			}
		})
	}
}

func openTestStore(t *testing.T) *Store {
	t.Helper()
	t.Setenv("XDG_DATA_HOME", t.TempDir())
	store, err := Open()
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return store
}

func TestStoreFrecencySuccessAndFailure(t *testing.T) {
	store := openTestStore(t)
	ctx := context.Background()
	for range 3 {
		if err := store.Record(ctx, "git status", "git status", "", "/proj", 0); err != nil {
			t.Fatal(err)
		}
	}
	if err := store.Record(ctx, "make broken", "make", "", "/proj", 2); err != nil {
		t.Fatal(err)
	}
	fr, err := store.Frecency(ctx, "/proj")
	if err != nil {
		t.Fatal(err)
	}
	if fr["git status"] <= 0 {
		t.Errorf("successful command must gain frecency, got %v", fr["git status"])
	}
	if fr["make broken"] != 0 {
		t.Errorf("failed command must not gain success frecency, got %v", fr["make broken"])
	}
}

func TestStoreCWDPreferredOverGlobal(t *testing.T) {
	store := openTestStore(t)
	ctx := context.Background()
	if err := store.Record(ctx, "go test ./...", "go test", "", "/other", 0); err != nil {
		t.Fatal(err)
	}
	fr, err := store.Frecency(ctx, "/proj")
	if err != nil {
		t.Fatal(err)
	}
	local := openTestStore(t)
	if err := local.Record(ctx, "go test ./...", "go test", "", "/proj", 0); err != nil {
		t.Fatal(err)
	}
	frLocal, err := local.Frecency(ctx, "/proj")
	if err != nil {
		t.Fatal(err)
	}
	if frLocal["go test ./..."] <= fr["go test ./..."] {
		t.Errorf("exact-CWD frecency (%v) should exceed the 70%% global fallback (%v)",
			frLocal["go test ./..."], fr["go test ./..."])
	}
}

func TestStoreTransitions(t *testing.T) {
	store := openTestStore(t)
	ctx := context.Background()
	if err := store.Record(ctx, "git add .", "git add", "", "/proj", 0); err != nil {
		t.Fatal(err)
	}
	if err := store.Record(ctx, "git commit -m x", "git commit", "git add", "/proj", 0); err != nil {
		t.Fatal(err)
	}
	tr, err := store.Transitions(ctx, "git add", "/proj", func(string) string { return "" })
	if err != nil {
		t.Fatal(err)
	}
	if tr["git commit"] <= 0 {
		t.Errorf("learned transition missing: %v", tr)
	}
	// Parent fallback: unknown deep skeleton falls back to its parent.
	parentOf := func(s string) string {
		if s == "git add special" {
			return "git add"
		}
		return ""
	}
	tr, err = store.Transitions(ctx, "git add special", "/proj", parentOf)
	if err != nil {
		t.Fatal(err)
	}
	if tr["git commit"] <= 0 {
		t.Errorf("parent-skeleton fallback missing: %v", tr)
	}
}

func TestWorkspaceDetect(t *testing.T) {
	dir := t.TempDir()
	mustWrite(t, dir+"/go.mod", "module example.com/x\n")
	mustWrite(t, dir+"/Makefile", "all:\n")
	d := &Detector{}
	ws := d.Detect(dir)
	if !ws.Has("go") || !ws.Has("make") {
		t.Errorf("signatures = %v, want go and make", ws.Signatures)
	}
	if ws.Has("rust") {
		t.Error("rust signature should be absent")
	}
}

func mustWrite(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}
