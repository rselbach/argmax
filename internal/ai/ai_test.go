package ai

import (
	"path/filepath"
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

func TestSnapshotHashIncludesSectionContent(t *testing.T) {
	first := Snapshot{
		CWD:      "/proj",
		Sections: []Section{{Label: "git status", Content: "M one.go"}},
	}
	second := first
	second.Sections = []Section{{Label: "git status", Content: "M two.go"}}
	if first.Hash() == second.Hash() {
		t.Error("equal-length section content must produce distinct snapshot hashes")
	}
	if first.Hash() != first.Hash() {
		t.Error("identical snapshots must produce stable hashes")
	}
}

func TestHelpAllowlistRejectsPaths(t *testing.T) {
	g := &Gatherer{Probe: func(_ string, _ time.Duration, name string, _ ...string) string {
		t.Fatalf("probe must not run for %q", name)
		return ""
	}}
	if out := g.helpFor(t.TempDir(), "./script --x"); out != "" {
		t.Errorf("path-like executable must not gather help, got %q", out)
	}
	if out := g.helpFor(t.TempDir(), "nmap -sV"); out != "" {
		t.Errorf("non-allowlisted tool must not gather help, got %q", out)
	}
}

func TestGathererProbeCacheIsolatedByWorkingDirectory(t *testing.T) {
	first := t.TempDir()
	second := t.TempDir()
	calls := map[string]int{}
	g := &Gatherer{Probe: func(cwd string, _ time.Duration, _ string, _ ...string) string {
		calls[cwd]++
		return cwd
	}}

	firstSnap := g.Gather(first, "kubectl logs ", "", 0, nil)
	g.Gather(first+string(filepath.Separator)+".", "kubectl logs ", "", 0, nil)
	secondSnap := g.Gather(second, "kubectl logs ", "", 0, nil)
	if calls[first] != 1 || calls[second] != 1 {
		t.Fatalf("probe calls by directory = %#v, want one per normalized CWD", calls)
	}
	if got := firstSnap.Sections[0].Content; got != first {
		t.Errorf("first context = %q, want %q", got, first)
	}
	if got := secondSnap.Sections[0].Content; got != second {
		t.Errorf("second context = %q, want %q", got, second)
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
