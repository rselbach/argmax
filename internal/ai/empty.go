package ai

import (
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Prediction is one empty-prompt suggestion.
type Prediction struct {
	Command    string
	Reason     string
	Confidence int
}

// PredictEmpty evaluates the local deterministic rules for an empty
// prompt. Only rules with confidence at least 70 are returned; an AI
// fallback runs separately when nothing qualifies.
func PredictEmpty(cwd, prevCommand string, prevExit int, probe Prober) (Prediction, bool) {
	gitDir := filepath.Join(cwd, ".git")
	if _, err := os.Stat(filepath.Join(gitDir, "MERGE_HEAD")); err == nil {
		return Prediction{Command: "git commit", Reason: "finish the in-progress merge", Confidence: 90}, true
	}
	if _, err := os.Stat(filepath.Join(gitDir, "rebase-merge")); err == nil {
		return Prediction{Command: "git rebase --continue", Reason: "continue the rebase", Confidence: 90}, true
	}
	if _, err := os.Stat(filepath.Join(gitDir, "rebase-apply")); err == nil {
		return Prediction{Command: "git rebase --continue", Reason: "continue the rebase", Confidence: 90}, true
	}
	if prevExit != 0 && prevCommand != "" {
		return Prediction{Command: prevCommand, Reason: "retry the failed command", Confidence: 75}, true
	}
	if strings.TrimSpace(prevCommand) == "git status" {
		return Prediction{Command: "git diff", Reason: "inspect the reported changes", Confidence: 74}, true
	}
	if _, err := os.Stat(gitDir); err == nil && probe != nil {
		status := probe(cwd, 800*time.Millisecond, "git", "status", "--porcelain")
		if strings.TrimSpace(status) != "" {
			return Prediction{Command: "git status", Reason: "the repository has uncommitted changes", Confidence: 72}, true
		}
	}
	if _, err := os.Stat(filepath.Join(cwd, "package.json")); err == nil {
		return Prediction{Command: "npm run dev", Reason: "start the Node dev server", Confidence: 70}, true
	}
	return Prediction{}, false
}
