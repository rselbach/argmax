package ai

import (
	"context"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/config"
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

func TestCompletedSuggestionCacheUsesRequestIdentity(t *testing.T) {
	e := &Engine{}
	cfg := config.AI{Provider: "first"}
	provider := &config.Provider{
		Endpoint:         "https://first.example/v1",
		Model:            "model-a",
		ExtraRequestBody: map[string]any{"seed": int64(1)},
	}
	e.Configure(cfg, provider)
	snapshot := Snapshot{
		CWD:      "/first",
		Sections: []Section{{Label: "git status", Content: "M one.go"}},
	}
	e.cachedAt = time.Now()
	e.cachedText = "git status"
	e.cachedKey = requestCacheKey(e.providerKey, snapshot)

	if got, ok := e.Cached("git sta", snapshot); !ok || got != "git status" {
		t.Fatalf("matching cache lookup = %q, %v", got, ok)
	}
	tests := map[string]Snapshot{
		"working directory": {CWD: "/second", Sections: snapshot.Sections},
		"context content":   {CWD: snapshot.CWD, Sections: []Section{{Label: "git status", Content: "M two.go"}}},
	}
	for name, changed := range tests {
		t.Run(name, func(t *testing.T) {
			if got, ok := e.Cached("git sta", changed); ok {
				t.Errorf("cache reused %q for changed %s", got, name)
			}
		})
	}

	provider.Model = "model-b"
	e.Configure(cfg, provider)
	if got, ok := e.Cached("git sta", snapshot); ok {
		t.Errorf("cache reused %q after model change", got)
	}
	provider.Model = "model-a"
	cfg.Provider = "second"
	e.Configure(cfg, provider)
	if got, ok := e.Cached("git sta", snapshot); ok {
		t.Errorf("cache reused %q after provider change", got)
	}
}

func TestConfigureCancelsPendingRequest(t *testing.T) {
	var snapshots atomic.Int32
	e := &Engine{}
	e.Configure(config.AI{DebounceMS: 50}, &config.Provider{
		Endpoint: "http://127.0.0.1:1", Model: "test", TimeoutMS: 100,
	})
	e.Request("git sta", func() Snapshot {
		snapshots.Add(1)
		return Snapshot{}
	}, func(string, int) {
		t.Error("canceled request must not deliver")
	})
	e.Configure(config.AI{}, nil)
	time.Sleep(100 * time.Millisecond)
	if got := snapshots.Load(); got != 0 {
		t.Errorf("snapshot gathered %d times after pending request was canceled", got)
	}
}

func TestConfigureInvalidatesRequestState(t *testing.T) {
	e := &Engine{}
	cfg := config.AI{Provider: "test"}
	provider := &config.Provider{Endpoint: "https://example.test", Model: "model"}
	snapshot := Snapshot{CWD: "/work"}
	e.Configure(cfg, provider)
	e.cachedAt = time.Now()
	e.cachedText = "git status"
	e.cachedKey = requestCacheKey(e.providerKey, snapshot)
	e.emptyLastHash = requestCacheKey(e.providerKey, snapshot)
	e.emptyCachedCmd = "git status"
	e.cooldownEnd = time.Now().Add(time.Hour)

	e.Configure(config.AI{}, nil)
	e.Configure(cfg, provider)
	if got, ok := e.Cached("git sta", snapshot); ok {
		t.Errorf("cache survived disable and re-enable: %q", got)
	}
	if e.emptyLastHash != "" || e.emptyCachedCmd != "" {
		t.Error("empty-prompt cache survived reconfiguration")
	}
	if !e.cooldownEnd.IsZero() {
		t.Error("provider cooldown survived reconfiguration")
	}
}

func TestConfigureCancelsInflightRequests(t *testing.T) {
	for _, request := range []string{"completion", "empty prompt"} {
		t.Run(request, func(t *testing.T) {
			started := make(chan struct{})
			canceled := make(chan struct{})
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(http.StatusOK)
				w.(http.Flusher).Flush()
				close(started)
				<-r.Context().Done()
				close(canceled)
			}))
			defer func() {
				server.CloseClientConnections()
				server.Close()
			}()

			e := &Engine{}
			e.Configure(config.AI{DebounceMS: 1, MinIntervalMS: 1}, &config.Provider{
				Endpoint: server.URL, Model: "test", TimeoutMS: 5000,
			})
			delivered := make(chan struct{}, 1)
			if request == "completion" {
				e.Request("git sta", func() Snapshot { return Snapshot{} }, func(string, int) {
					delivered <- struct{}{}
				})
			} else {
				e.RequestEmpty(0, func() Snapshot { return Snapshot{} }, func(string) {
					delivered <- struct{}{}
				})
			}
			awaitSignal(t, started, "provider request did not start")
			e.Configure(config.AI{}, nil)
			awaitSignal(t, canceled, "provider request was not canceled")
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			if err := e.Wait(ctx); err != nil {
				t.Fatalf("canceled AI worker did not exit: %v", err)
			}
			select {
			case <-delivered:
				t.Error("request from old configuration delivered a result")
			case <-time.After(20 * time.Millisecond):
			}
		})
	}
}

func awaitSignal(t *testing.T, ch <-chan struct{}, failure string) {
	t.Helper()
	select {
	case <-ch:
	case <-time.After(2 * time.Second):
		t.Fatal(failure)
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
