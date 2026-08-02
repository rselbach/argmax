package ai

import (
	"strings"
	"testing"
	"time"
)

func TestSanitize(t *testing.T) {
	tests := map[string]struct {
		raw     string
		buffer  string
		want    string
		wantErr bool
	}{
		"plain completion": {
			raw: "git checkout main", buffer: "git che", want: "git checkout main",
		},
		"code fence stripped": {
			raw: "```bash\ngit checkout main\n```", buffer: "git che", want: "git checkout main",
		},
		"outer quotes stripped": {
			raw: `"git checkout main"`, buffer: "git che", want: "git checkout main",
		},
		"case-insensitive prefix restored": {
			raw: "Git checkout main", buffer: "git che", want: "git checkout main",
		},
		"unchanged buffer rejected": {
			raw: "git che", buffer: "git che", wantErr: true,
		},
		"empty rejected": {
			raw: "   ", buffer: "git che", wantErr: true,
		},
		"multiline rejected": {
			raw: "git checkout main\ngit pull", buffer: "git che", wantErr: true,
		},
		"non-prefix rejected": {
			raw: "docker ps", buffer: "git che", wantErr: true,
		},
		"control data rejected": {
			raw: "git che\x1b[2Jckout", buffer: "git che", wantErr: true,
		},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got, err := Sanitize(tc.raw, tc.buffer)
			if tc.wantErr {
				if err == nil {
					t.Errorf("Sanitize(%q) = %q, want error", tc.raw, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("Sanitize(%q) failed: %v", tc.raw, err)
			}
			if got != tc.want {
				t.Errorf("Sanitize(%q) = %q, want %q", tc.raw, got, tc.want)
			}
		})
	}
}

func TestSanitizePrefixCasing(t *testing.T) {
	got, err := Sanitize("Git Checkout Main", "git c")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(got, "git c") {
		t.Errorf("user's exact typed prefix must be preserved, got %q", got)
	}
}

func TestBuildPromptMarksUntrusted(t *testing.T) {
	snap := Snapshot{
		CWD:      "/proj",
		Sections: []Section{{Label: "git status", Content: "M main.go"}},
	}
	system, user := BuildPrompt("git ", snap)
	if !strings.Contains(system, "untrusted") {
		t.Error("system prompt must declare context untrusted")
	}
	if !strings.Contains(user, "--- untrusted git status ---") {
		t.Error("context sections must be delimited as untrusted")
	}
	if !strings.Contains(user, "git ") {
		t.Error("user prompt must include the buffer")
	}
}

func TestHelpAllowlistRejectsPaths(t *testing.T) {
	g := &Gatherer{Probe: func(_ time.Duration, name string, _ ...string) string {
		t.Fatalf("probe must not run for %q", name)
		return ""
	}}
	if out := g.helpFor("./script --x"); out != "" {
		t.Errorf("path-like executable must not gather help, got %q", out)
	}
	if out := g.helpFor("nmap -sV"); out != "" {
		t.Errorf("non-allowlisted tool must not gather help, got %q", out)
	}
}

func TestPredictEmptyRetryFailed(t *testing.T) {
	pred, ok := PredictEmpty(t.TempDir(), "make test", 2, nil)
	if !ok || pred.Command != "make test" {
		t.Errorf("failed command should be retried, got %+v ok=%v", pred, ok)
	}
}

func TestPredictEmptyGitStatusThenDiff(t *testing.T) {
	pred, ok := PredictEmpty(t.TempDir(), "git status", 0, nil)
	if !ok || pred.Command != "git diff" {
		t.Errorf("git status should suggest git diff, got %+v ok=%v", pred, ok)
	}
}
