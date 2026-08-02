package ai

import (
	"strings"
	"testing"
)

func TestValidateOutput(t *testing.T) {
	tests := []struct {
		name   string
		buffer string
		raw    string
		want   string
		ok     bool
	}{
		{"plain", "git", "git status", "git status", true},
		{"exact prefix", "git stat", "git status", "git status", true},
		{"case preserved", "Git", "Git status", "Git status", true},
		{"whitespace prefix preserved", "git  st", "git  status", "git  status", true},
		{"surrounding whitespace", "git", "  git status  ", "git status", true},

		{"fence with language", "git", "```bash\ngit status\n```", "git status", true},
		{"fence without language", "git", "```\ngit status\n```", "git status", true},
		{"single-line fence", "git", "```git status```", "git status", true},

		{"outer double quotes", "git", `"git status"`, "git status", true},
		{"outer single quotes", "git", `'git status'`, "git status", true},
		{"outer backticks", "git", "`git status`", "git status", true},

		{"case changed", "git", "Git status", "", false},
		{"prefix changed", "git stat", "git stash", "", false},
		{"different command", "git", "docker ps", "", false},
		{"empty", "git", "", "", false},
		{"blank", "git", "   \n ", "", false},
		{"empty fence", "git", "```\n```", "", false},
		{"unchanged buffer", "git status", "git status", "", false},
		{"multiline", "git", "git status\ngit log", "", false},
		{"multiline fenced", "git", "```bash\ngit status\ngit log\n```", "", false},
		{"carriage return", "git", "git status\r\ngit log", "", false},
		{"escape sequence", "git", "git status\x1b[31m", "", false},
		{"bell", "git", "git status\a", "", false},
		{"delete", "git", "git status\x7f", "", false},
		{"null byte", "git", "git status\x00", "", false},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := ValidateOutput(tc.buffer, tc.raw)
			if ok != tc.ok || got != tc.want {
				t.Errorf("ValidateOutput(%q, %q) = %q, %v; want %q, %v",
					tc.buffer, tc.raw, got, ok, tc.want, tc.ok)
			}
		})
	}
}

func TestBuildMessages(t *testing.T) {
	req := Request{
		Buffer:      "git ch",
		CWD:         "/repo",
		PrevCommand: "git status",
		PrevExit:    1,
		Recent:      []string{"one", "two", "three", "four"},
	}
	c := Context{
		Ecosystems:    []string{"node", "make"},
		Scripts:       []string{"dev: vite"},
		DirEntries:    []string{"src/", "go.mod"},
		GitBranch:     "main",
		GitPrevBranch: "feature",
		GitBranches:   []string{"main", "feature"},
		GitStatus:     " M file.go",
		StagedDiff:    "diff --git a/file.go b/file.go",
		RecentCommits: []string{"initial commit"},
		MergeState:    "merging",
		Specialized:   "pods output",
		Help:          "help output",
	}

	msgs := BuildMessages(req, c)
	if len(msgs) != 2 {
		t.Fatalf("BuildMessages returned %d messages; want 2", len(msgs))
	}
	if msgs[0].Role != "system" {
		t.Errorf("msgs[0].Role = %q; want system", msgs[0].Role)
	}
	if !strings.Contains(msgs[0].Content, "UNTRUSTED") {
		t.Error("system message does not mark context as untrusted (AI-008)")
	}
	if msgs[1].Role != "user" {
		t.Errorf("msgs[1].Role = %q; want user", msgs[1].Role)
	}
	u := msgs[1].Content
	for _, want := range []string{
		"<<<UNTRUSTED CONTEXT>>>",
		"<<<END UNTRUSTED>>>",
		"INPUT: git ch",
		"cwd: /repo",
		"previous command: git status",
		"previous exit status: 1",
		"ecosystems: node, make",
		"- dev: vite",
		"- src/",
		"git branch: main",
		"git previous branch: feature",
		" M file.go",
		"diff --git a/file.go b/file.go",
		"- initial commit",
		"merging",
		"pods output",
		"help output",
	} {
		if !strings.Contains(u, want) {
			t.Errorf("user message missing %q", want)
		}
	}
	// At most 3 recent commands (AI-007).
	if strings.Contains(u, "four") {
		t.Error("user message contains a 4th recent command; want at most 3")
	}
	// The exact buffer appears after INPUT (AI-009).
	if !strings.HasSuffix(u, "INPUT: git ch") {
		t.Error("user message does not end with the exact INPUT buffer")
	}
}

func TestBuildMessagesOmitsEmptyFields(t *testing.T) {
	msgs := BuildMessages(Request{Buffer: "x", CWD: "/y"}, Context{})
	u := msgs[1].Content
	for _, unwanted := range []string{"previous command", "ecosystems", "git branch", "git state", "relevant resources", "command help"} {
		if strings.Contains(u, unwanted) {
			t.Errorf("user message contains %q for an empty field", unwanted)
		}
	}
}
