package ai

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/core"
)

// EmptyPromptSuggestions implements the deterministic local rules (EMPTY-002):
// in-progress merge → "git commit"; in-progress rebase →
// "git rebase --continue"; last command failed → retry it; after
// "git status" → "git diff"; dirty repo → "git status"; Node workspace →
// "npm run dev". Only rules with confidence ≥70 are returned (EMPTY-003), in
// rule-evaluation order. All checks are local and bounded; failures skip the
// rule.
func EmptyPromptSuggestions(ctx context.Context, cwd, prevCommand string, prevExit int) []core.Suggestion {
	if ctx.Err() != nil {
		return nil
	}

	var out []core.Suggestion
	add := func(text, description string, confidence int) {
		out = append(out, core.Suggestion{
			Text:        text,
			Description: description,
			Icon:        "ai",
			Source:      core.SourceDynamic,
			Confidence:  confidence,
			Priority:    -1,
		})
	}

	gitDir := resolveGitDir(cwd)
	if gitDir != "" {
		if isFile(filepath.Join(gitDir, "MERGE_HEAD")) {
			add("git commit", "finish merge", 90)
		}
		if isDir(filepath.Join(gitDir, "rebase-merge")) || isDir(filepath.Join(gitDir, "rebase-apply")) {
			add("git rebase --continue", "continue rebase", 90)
		}
	}

	prev := strings.TrimSpace(prevCommand)
	if prevExit != 0 && prev != "" {
		add(prev, "retry failed command", 75)
	}
	if prev == "git status" {
		add("git diff", "git diff after git status", 70)
	}
	if gitDir != "" && repoDirty(ctx, cwd) {
		add("git status", "dirty repository", 75)
	}
	if hasNodeDevScript(cwd) {
		add("npm run dev", "node workspace", 70)
	}
	return out
}

// repoDirty reports whether the repository at cwd has staged, unstaged, or
// untracked changes. The probe is bounded; any failure means "not dirty".
func repoDirty(ctx context.Context, cwd string) bool {
	ctx, cancel := context.WithTimeout(ctx, 800*time.Millisecond)
	defer cancel()
	out, err := runProbe(ctx, cwd, 4096, "git", "status", "--porcelain")
	return err == nil && out != ""
}

// hasNodeDevScript reports whether cwd is a Node workspace with a dev script.
func hasNodeDevScript(cwd string) bool {
	data, err := readBounded(filepath.Join(cwd, "package.json"), maxPackageJSONBytes)
	if err != nil {
		return false
	}
	var pkg struct {
		Scripts map[string]string `json:"scripts"`
	}
	if err := json.Unmarshal(data, &pkg); err != nil {
		return false
	}
	return pkg.Scripts["dev"] != ""
}
