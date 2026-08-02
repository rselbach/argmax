package engine

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/core"
	"github.com/rselbach/argmax/internal/shell"
)

func newTestEngine(t *testing.T, cfg *config.Config) *Engine {
	t.Helper()
	dir := t.TempDir()
	t.Setenv("HOME", dir)
	t.Setenv("XDG_CONFIG_HOME", filepath.Join(dir, ".config"))
	t.Setenv("XDG_DATA_HOME", filepath.Join(dir, ".local", "share"))
	paths := config.Paths{
		ConfigFile: filepath.Join(dir, "config.toml"),
		DataDir:    filepath.Join(dir, "data"),
		StateFile:  filepath.Join(dir, "data", "state.toml"),
		DBFile:     filepath.Join(dir, "data", "history.db"),
		CacheDir:   filepath.Join(dir, "cache"),
	}
	e := New(cfg, shell.Bash, dir, paths)
	t.Cleanup(e.Close)
	return e
}

// home returns the engine's test HOME directory.
func (e *Engine) home() string { return e.CWD() }

func TestGitCheYieldsCheckout(t *testing.T) {
	e := newTestEngine(t, config.Default())
	ctx := context.Background()
	out := e.Suggest(ctx, "git che")
	found := false
	for _, s := range out {
		if s.Text == "git checkout" {
			found = true
			break
		}
	}
	if !found {
		names := []string{}
		for i, s := range out {
			if i >= 5 {
				break
			}
			names = append(names, s.Text)
		}
		t.Fatalf("git che must yield git checkout; top results: %v", names)
	}
}

func TestTopLevelMergesSpecsAndSystem(t *testing.T) {
	e := newTestEngine(t, config.Default())
	out := e.Suggest(context.Background(), "gi")
	foundGit := false
	for _, s := range out {
		if s.Text == "git" && s.Source == core.SourceSpec {
			foundGit = true
		}
	}
	if !foundGit {
		t.Fatal("top-level must include the git spec")
	}
}

func TestOptionPromotionOnDash(t *testing.T) {
	e := newTestEngine(t, config.Default())
	out := e.Suggest(context.Background(), "git commit -")
	if len(out) == 0 {
		t.Fatal("expected option candidates for git commit -")
	}
	for _, s := range out {
		if s.Text == "git commit --amend" {
			return
		}
	}
	texts := []string{}
	for i, s := range out {
		if i >= 8 {
			break
		}
		texts = append(texts, s.Text)
	}
	t.Fatalf("expected git commit --amend among options; got %v", texts)
}

func TestDedupeAndQueryExclusion(t *testing.T) {
	in := []core.Suggestion{
		{Text: "git", Source: core.SourceSpec, Confidence: 90},
		{Text: "git", Source: core.SourceSystem, Confidence: 50},
		{Text: "gitk", Source: core.SourceSystem, Confidence: 50},
	}
	out := dedupe(in, "git")
	if len(out) != 1 || out[0].Text != "gitk" {
		t.Fatalf("dedupe must drop the exact query copy and duplicates, got %+v", out)
	}
	// Aliases survive exact-query exclusion (SRC-009).
	out = dedupe([]core.Suggestion{{Text: "g", Source: core.SourceAlias, Confidence: 90}}, "g")
	if len(out) != 1 {
		t.Fatal("alias candidates must survive exact-query exclusion")
	}
}

func TestHistoryModeConfidenceDecay(t *testing.T) {
	if c := historyConfidence(0, 10); c != 75 {
		t.Fatalf("first = %d, want 75", c)
	}
	if c := historyConfidence(9, 10); c != 60 {
		t.Fatalf("last = %d, want 60", c)
	}
	if c := historyConfidence(0, 1); c != 75 {
		t.Fatalf("single = %d, want 75", c)
	}
}

func TestSessionHistoryBeforeFlush(t *testing.T) {
	e := newTestEngine(t, config.Default())
	e.CommandStarted("kubectl get pods")
	e.SetMode(core.ModeHistory)
	out := e.Suggest(context.Background(), "kubectl")
	found := false
	for _, s := range out {
		if s.Text == "kubectl get pods" && s.Source == core.SourceHistory {
			found = true
		}
	}
	if !found {
		t.Fatal("session command must appear in history mode before shell flush")
	}
}

func TestCommandFinishedRecordsLearning(t *testing.T) {
	e := newTestEngine(t, config.Default())
	e.CommandStarted("git add .")
	e.CommandFinished(0)
	e.CommandStarted("git commit -m x")
	e.CommandFinished(0)
	// Now a transition git add -> git commit should exist; verify ranking
	// prefers git commit after git add indirectly via the store.
	if e.store == nil {
		t.Skip("store unavailable")
	}
	if got := e.store.Transition(e.CWD(), "git add", "git commit"); got <= 0 {
		t.Fatalf("expected learned transition, got %v", got)
	}
	if got := e.store.Frecency(e.CWD(), "git commit"); got <= 0 {
		t.Fatalf("expected frecency for successful command, got %v", got)
	}
}

func TestExpandAlias(t *testing.T) {
	e := newTestEngine(t, config.Default())
	if err := os.WriteFile(filepath.Join(e.home(), ".bashrc"), []byte("alias gco='git checkout'\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if exp, ok := e.ExpandAlias("gco"); !ok || exp != "git checkout" {
		t.Fatalf("ExpandAlias = %q,%v", exp, ok)
	}
	if _, ok := e.ExpandAlias("gco main"); ok {
		t.Fatal("no expansion for multi-token buffer")
	}
	cfg := config.Default()
	cfg.Core.ExpandAlias = false
	e2 := newTestEngine(t, cfg)
	_ = os.WriteFile(filepath.Join(e2.home(), ".bashrc"), []byte("alias gco='git checkout'\n"), 0o600)
	if _, ok := e2.ExpandAlias("gco"); ok {
		t.Fatal("expansion must honor core.expand-alias=false")
	}
}

func TestNestedAliasCompletionMappedBack(t *testing.T) {
	e := newTestEngine(t, config.Default())
	if err := os.WriteFile(filepath.Join(e.home(), ".bashrc"), []byte("alias gco='git checkout'\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	// A file the git-checkout generator's file merge can find.
	if err := os.WriteFile(filepath.Join(e.home(), "main.go"), []byte("package main\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	out := e.Suggest(context.Background(), "gco ma")
	for _, s := range out {
		if s.Text == "gco main.go" {
			return
		}
	}
	texts := []string{}
	for i, s := range out {
		if i >= 8 {
			break
		}
		texts = append(texts, s.Text)
	}
	t.Fatalf("expected nested alias completion mapped to alias form; got %v", texts)
}

func TestInjectAI(t *testing.T) {
	dup := []core.Suggestion{
		{Text: "git checkout main", Source: core.SourceSpec, Confidence: 40, Priority: -1},
	}
	out := InjectAI(dup, "git checkout main")
	if len(out) != 1 || out[0].Source != core.SourceAI {
		t.Fatalf("AI must replace lower-confidence duplicate: %+v", out)
	}
	weak := []core.Suggestion{
		{Text: "git checkout main", Source: core.SourceSpec, Confidence: 40, Priority: -1},
	}
	out = InjectAI(weak, "git checkout feature")
	if len(out) != 2 || out[0].Text != "git checkout feature" {
		t.Fatalf("stronger AI must lead: %+v", out)
	}
	// A top result stronger than the AI keeps its place.
	strong := []core.Suggestion{
		{Text: "git checkout x", Source: core.SourceSpec, Confidence: 95, Priority: -1},
	}
	out = InjectAI(strong, "git checkout y")
	if out[0].Text != "git checkout x" {
		t.Fatalf("AI must not displace a stronger top result: %+v", out)
	}
	if got := InjectAI(nil, ""); got != nil {
		t.Fatal("empty AI text is a no-op")
	}
}

func TestAIDisabledByDefault(t *testing.T) {
	e := newTestEngine(t, config.Default())
	if _, ok := e.AISuggest(context.Background(), "git ch"); ok {
		t.Fatal("AI disabled by default: no suggestion")
	}
	if got := e.EmptyPrompt(context.Background()); got != nil {
		t.Fatal("empty-prompt prediction off by default")
	}
}

func TestSetCWDRejectsRelative(t *testing.T) {
	e := newTestEngine(t, config.Default())
	before := e.CWD()
	e.SetCWD("relative/path")
	if e.CWD() != before {
		t.Fatal("relative CWD updates must be rejected (RUN-007)")
	}
	e.SetCWD("/tmp")
	if e.CWD() != "/tmp" {
		t.Fatal("absolute CWD update rejected")
	}
}
