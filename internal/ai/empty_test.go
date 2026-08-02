package ai

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/rselbach/argmax/internal/core"
)

func TestEmptyPromptMergeRule(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, ".git", "MERGE_HEAD"), "abc123\n")

	got := EmptyPromptSuggestions(context.Background(), dir, "", 0)
	if len(got) != 1 || got[0].Text != "git commit" {
		t.Fatalf("suggestions = %v; want [git commit]", got)
	}
	s := got[0]
	if s.Confidence != 90 {
		t.Errorf("Confidence = %d; want 90", s.Confidence)
	}
	if s.Description != "finish merge" {
		t.Errorf("Description = %q; want finish merge", s.Description)
	}
	if s.Source != core.SourceDynamic {
		t.Errorf("Source = %q; want %q", s.Source, core.SourceDynamic)
	}
	if s.Icon != "ai" {
		t.Errorf("Icon = %q; want ai", s.Icon)
	}
}

func TestEmptyPromptRebaseRule(t *testing.T) {
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, ".git", "rebase-merge"), 0o755); err != nil {
		t.Fatal(err)
	}

	got := EmptyPromptSuggestions(context.Background(), dir, "", 0)
	if len(got) != 1 || got[0].Text != "git rebase --continue" {
		t.Fatalf("suggestions = %v; want [git rebase --continue]", got)
	}
	if got[0].Confidence != 90 {
		t.Errorf("Confidence = %d; want 90", got[0].Confidence)
	}
}

func TestEmptyPromptRetryFailedRule(t *testing.T) {
	dir := t.TempDir() // no .git, no package.json: only the retry rule applies
	got := EmptyPromptSuggestions(context.Background(), dir, "make test", 1)
	if len(got) != 1 || got[0].Text != "make test" {
		t.Fatalf("suggestions = %v; want [make test]", got)
	}
	if got[0].Confidence != 75 {
		t.Errorf("Confidence = %d; want 75", got[0].Confidence)
	}
	if got[0].Description != "retry failed command" {
		t.Errorf("Description = %q; want retry failed command", got[0].Description)
	}

	// Successful previous command: no retry.
	if got := EmptyPromptSuggestions(context.Background(), dir, "make test", 0); len(got) != 0 {
		t.Fatalf("suggestions = %v; want none for exit 0", got)
	}
}

func TestEmptyPromptGitDiffAfterStatusRule(t *testing.T) {
	dir := t.TempDir()
	got := EmptyPromptSuggestions(context.Background(), dir, "git status", 0)
	if len(got) != 1 || got[0].Text != "git diff" {
		t.Fatalf("suggestions = %v; want [git diff]", got)
	}
	if got[0].Confidence != 70 {
		t.Errorf("Confidence = %d; want 70", got[0].Confidence)
	}
}

func TestEmptyPromptNodeDevRule(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "package.json"), `{"name":"x","scripts":{"dev":"vite"}}`)

	got := EmptyPromptSuggestions(context.Background(), dir, "", 0)
	if len(got) != 1 || got[0].Text != "npm run dev" {
		t.Fatalf("suggestions = %v; want [npm run dev]", got)
	}
	if got[0].Confidence != 70 {
		t.Errorf("Confidence = %d; want 70", got[0].Confidence)
	}

	// No dev script: the rule does not fire.
	writeFile(t, filepath.Join(dir, "package.json"), `{"name":"x","scripts":{"build":"tsc"}}`)
	if got := EmptyPromptSuggestions(context.Background(), dir, "", 0); len(got) != 0 {
		t.Fatalf("suggestions = %v; want none without a dev script", got)
	}
}

func TestEmptyPromptDirtyRepoRule(t *testing.T) {
	dir := initGitRepo(t) // skips when git is unavailable

	// Clean repository: no dirty suggestion.
	if got := EmptyPromptSuggestions(context.Background(), dir, "", 0); len(got) != 0 {
		t.Fatalf("suggestions = %v; want none for a clean repo", got)
	}

	// Modify a tracked file: dirty.
	writeFile(t, filepath.Join(dir, "file.txt"), "changed\n")
	got := EmptyPromptSuggestions(context.Background(), dir, "", 0)
	if len(got) != 1 || got[0].Text != "git status" {
		t.Fatalf("suggestions = %v; want [git status]", got)
	}
	if got[0].Confidence != 75 {
		t.Errorf("Confidence = %d; want 75", got[0].Confidence)
	}
	if got[0].Description != "dirty repository" {
		t.Errorf("Description = %q; want dirty repository", got[0].Description)
	}
}

func TestEmptyPromptRuleOrderAndConfidence(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, ".git", "MERGE_HEAD"), "abc123\n")

	// Merge rule (90) precedes retry rule (75) in evaluation order.
	got := EmptyPromptSuggestions(context.Background(), dir, "make test", 1)
	if len(got) != 2 {
		t.Fatalf("suggestions = %v; want 2", got)
	}
	if got[0].Text != "git commit" || got[1].Text != "make test" {
		t.Fatalf("order = %q, %q; want git commit, make test", got[0].Text, got[1].Text)
	}
	for _, s := range got {
		if s.Confidence < 70 {
			t.Errorf("Confidence = %d; EMPTY-003 requires ≥70", s.Confidence)
		}
		if s.Source != core.SourceDynamic || s.Icon != "ai" {
			t.Errorf("Source/Icon = %q/%q; want dynamic/ai", s.Source, s.Icon)
		}
	}
}
