package ai

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/config"
)

// recordedRequest captures what the fake provider received.
type recordedRequest struct {
	path string
	auth string
	body []byte
}

// fakeProvider is an httptest-backed OpenAI-compatible endpoint.
type fakeProvider struct {
	mu     sync.Mutex
	hits   int
	last   recordedRequest
	status int
	body   string
	delay  time.Duration
}

func newFakeProvider(t *testing.T) (*httptest.Server, *fakeProvider) {
	t.Helper()
	f := &fakeProvider{
		status: http.StatusOK,
		body:   `{"choices":[{"message":{"content":"zzz status"}}]}`,
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		data, _ := io.ReadAll(r.Body)
		f.mu.Lock()
		f.hits++
		f.last = recordedRequest{path: r.URL.Path, auth: r.Header.Get("Authorization"), body: data}
		status, body, delay := f.status, f.body, f.delay
		f.mu.Unlock()
		if delay > 0 {
			time.Sleep(delay)
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = io.WriteString(w, body)
	}))
	t.Cleanup(srv.Close)
	return srv, f
}

func (f *fakeProvider) hitCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.hits
}

func (f *fakeProvider) lastRequest() recordedRequest {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.last
}

func (f *fakeProvider) respond(status int, body string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.status, f.body = status, body
}

func (f *fakeProvider) setDelay(d time.Duration) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.delay = d
}

// testConfig builds an enabled AI config pointing at endpoint, with a 1ms
// minimum interval so back-to-back test calls are not throttled.
func testConfig(endpoint string, mutate func(*config.AIProvider)) *config.AI {
	p := config.AIProvider{Endpoint: endpoint, Model: "test-model", TimeoutMs: 2000}
	if mutate != nil {
		mutate(&p)
	}
	return &config.AI{
		Enabled:       true,
		Provider:      "test",
		MinIntervalMs: 1,
		Providers:     map[string]config.AIProvider{"test": p},
	}
}

func suggestRequest(buffer string) Request {
	return Request{Buffer: buffer, CWD: "/tmp"}
}

func TestSuggestHappyPath(t *testing.T) {
	srv, f := newFakeProvider(t)
	e := New(testConfig(srv.URL+"/v1", nil))

	text, ok := e.Suggest(context.Background(), suggestRequest("zzz st"))
	if !ok || text != "zzz status" {
		t.Fatalf("Suggest = %q, %v; want %q, true", text, ok, "zzz status")
	}
	if got := f.hitCount(); got != 1 {
		t.Fatalf("hits = %d; want 1", got)
	}
	last := f.lastRequest()
	if last.path != "/v1/chat/completions" {
		t.Errorf("path = %q; want /v1/chat/completions", last.path)
	}
	var body map[string]any
	if err := json.Unmarshal(last.body, &body); err != nil {
		t.Fatalf("request body not JSON: %v", err)
	}
	if body["model"] != "test-model" {
		t.Errorf("model = %v; want test-model", body["model"])
	}
	if body["stream"] != false {
		t.Errorf("stream = %v; want false", body["stream"])
	}
	msgs, ok := body["messages"].([]any)
	if !ok || len(msgs) != 2 {
		t.Errorf("messages = %v; want 2 entries", body["messages"])
	}
}

func TestEndpointSuffixAppendedExactlyOnce(t *testing.T) {
	for _, tc := range []struct {
		name     string
		endpoint func(base string) string
		wantPath string
	}{
		{"bare", func(base string) string { return base }, "/chat/completions"},
		{"path", func(base string) string { return base + "/v1" }, "/v1/chat/completions"},
		{"trailing slash", func(base string) string { return base + "/v1/" }, "/v1/chat/completions"},
		{"already suffixed", func(base string) string { return base + "/v1/chat/completions" }, "/v1/chat/completions"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			srv, f := newFakeProvider(t)
			e := New(testConfig(tc.endpoint(srv.URL), nil))
			if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
				t.Fatal("Suggest failed")
			}
			if got := f.lastRequest().path; got != tc.wantPath {
				t.Errorf("path = %q; want %q", got, tc.wantPath)
			}
		})
	}
}

func TestBearerAuthOnlyWithKey(t *testing.T) {
	t.Run("direct key", func(t *testing.T) {
		srv, f := newFakeProvider(t)
		e := New(testConfig(srv.URL, func(p *config.AIProvider) { p.APIKey = "secret" }))
		if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
			t.Fatal("Suggest failed")
		}
		if got := f.lastRequest().auth; got != "Bearer secret" {
			t.Errorf("Authorization = %q; want %q", got, "Bearer secret")
		}
	})
	t.Run("env key", func(t *testing.T) {
		t.Setenv("ARGMAX_TEST_API_KEY", "envsecret")
		srv, f := newFakeProvider(t)
		e := New(testConfig(srv.URL, func(p *config.AIProvider) { p.APIKeyEnv = "ARGMAX_TEST_API_KEY" }))
		if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
			t.Fatal("Suggest failed")
		}
		if got := f.lastRequest().auth; got != "Bearer envsecret" {
			t.Errorf("Authorization = %q; want %q", got, "Bearer envsecret")
		}
	})
	t.Run("no key", func(t *testing.T) {
		srv, f := newFakeProvider(t)
		e := New(testConfig(srv.URL, nil))
		if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
			t.Fatal("Suggest failed")
		}
		if got := f.lastRequest().auth; got != "" {
			t.Errorf("Authorization = %q; want empty", got)
		}
	})
}

func TestExtraRequestBodyOverrides(t *testing.T) {
	srv, f := newFakeProvider(t)
	e := New(testConfig(srv.URL, func(p *config.AIProvider) {
		p.ExtraRequestBody = map[string]any{
			"temperature": 0.7,
			"model":       "override-model",
			"max_tokens":  64,
		}
	}))
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
		t.Fatal("Suggest failed")
	}
	var body map[string]any
	if err := json.Unmarshal(f.lastRequest().body, &body); err != nil {
		t.Fatalf("request body not JSON: %v", err)
	}
	if body["model"] != "override-model" {
		t.Errorf("model = %v; want override-model (extra body must override default)", body["model"])
	}
	if body["temperature"] != 0.7 {
		t.Errorf("temperature = %v; want 0.7", body["temperature"])
	}
	if body["max_tokens"] != float64(64) {
		t.Errorf("max_tokens = %v; want 64", body["max_tokens"])
	}
	if body["stream"] != false {
		t.Errorf("stream = %v; want false", body["stream"])
	}
}

func TestDisabledEnginePerformsNoIO(t *testing.T) {
	srv, f := newFakeProvider(t)

	cfg := testConfig(srv.URL, nil)
	cfg.Enabled = false
	e := New(cfg)
	if e.Enabled() {
		t.Fatal("Enabled = true for disabled config")
	}
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); ok {
		t.Fatal("Suggest succeeded on disabled engine")
	}
	if got := f.hitCount(); got != 0 {
		t.Fatalf("hits = %d; want 0 (AI-001)", got)
	}

	// Enabled flag but unresolvable provider: also disabled.
	e2 := New(&config.AI{Enabled: true, Provider: "missing", Providers: map[string]config.AIProvider{}})
	if e2.Enabled() {
		t.Fatal("Enabled = true for missing provider")
	}
	if _, ok := e2.Suggest(context.Background(), suggestRequest("zzz st")); ok {
		t.Fatal("Suggest succeeded with missing provider")
	}

	// Nil config: disabled.
	if e3 := New(nil); e3.Enabled() {
		t.Fatal("Enabled = true for nil config")
	}
}

func TestMinIntervalThrottles(t *testing.T) {
	srv, f := newFakeProvider(t)
	cfg := testConfig(srv.URL, nil)
	cfg.MinIntervalMs = 60000
	e := New(cfg)

	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
		t.Fatal("first Suggest failed")
	}
	// Cache-incompatible buffer: only the minimum interval can block this.
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz xx")); ok {
		t.Fatal("second Suggest succeeded inside minimum interval")
	}
	if got := f.hitCount(); got != 1 {
		t.Fatalf("hits = %d; want 1 (AI-005)", got)
	}
}

func TestRateLimitCooldown(t *testing.T) {
	srv, f := newFakeProvider(t)
	f.respond(http.StatusTooManyRequests, `{"error":"slow down"}`)
	e := New(testConfig(srv.URL, nil))
	e.cooldown = 80 * time.Millisecond // test hook (production: 20s, AI-013)

	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); ok {
		t.Fatal("Suggest succeeded on HTTP 429")
	}
	if got := f.hitCount(); got != 1 {
		t.Fatalf("hits = %d; want 1", got)
	}
	// Within cooldown: no request is made.
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); ok {
		t.Fatal("Suggest succeeded during cooldown")
	}
	if got := f.hitCount(); got != 1 {
		t.Fatalf("hits = %d during cooldown; want 1 (AI-013)", got)
	}
	// After cooldown: requests resume.
	time.Sleep(120 * time.Millisecond)
	f.respond(http.StatusOK, `{"choices":[{"message":{"content":"zzz status"}}]}`)
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
		t.Fatal("Suggest failed after cooldown expired")
	}
	if got := f.hitCount(); got != 2 {
		t.Fatalf("hits = %d after cooldown; want 2", got)
	}
}

func TestSuggestionCache(t *testing.T) {
	srv, f := newFakeProvider(t)
	e := New(testConfig(srv.URL, nil))

	if text, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok || text != "zzz status" {
		t.Fatalf("first Suggest = %q, %v", text, ok)
	}
	// Extended, prefix-compatible buffer within the TTL: served from cache.
	if text, ok := e.Suggest(context.Background(), suggestRequest("zzz sta")); !ok || text != "zzz status" {
		t.Fatalf("cached Suggest = %q, %v", text, ok)
	}
	if got := f.hitCount(); got != 1 {
		t.Fatalf("hits = %d; want 1 (AI-012 cache)", got)
	}
	// Prefix-incompatible buffer: fresh request.
	time.Sleep(2 * time.Millisecond) // clear the 1ms minimum interval
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz xx")); ok {
		t.Fatal("Suggest succeeded for prefix-incompatible buffer")
	}
	if got := f.hitCount(); got != 2 {
		t.Fatalf("hits = %d; want 2", got)
	}
}

func TestProviderTimeout(t *testing.T) {
	srv, f := newFakeProvider(t)
	f.setDelay(500 * time.Millisecond)
	e := New(testConfig(srv.URL, func(p *config.AIProvider) { p.TimeoutMs = 50 }))

	start := time.Now()
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); ok {
		t.Fatal("Suggest succeeded despite provider timeout")
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Fatalf("Suggest blocked %v; want bounded by provider timeout (PERF-005)", elapsed)
	}
	if got := f.hitCount(); got != 1 {
		t.Fatalf("hits = %d; want 1", got)
	}
}

func TestUpdateConfigLiveReload(t *testing.T) {
	srv, f := newFakeProvider(t)

	disabled := testConfig(srv.URL, nil)
	disabled.Enabled = false
	e := New(disabled)
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); ok {
		t.Fatal("Suggest succeeded while disabled")
	}
	e.UpdateConfig(testConfig(srv.URL, nil))
	if !e.Enabled() {
		t.Fatal("Enabled = false after UpdateConfig")
	}
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz st")); !ok {
		t.Fatal("Suggest failed after enabling via UpdateConfig")
	}
	e.UpdateConfig(disabled)
	if _, ok := e.Suggest(context.Background(), suggestRequest("zzz xy")); ok {
		t.Fatal("Suggest succeeded after disabling via UpdateConfig")
	}
	if got := f.hitCount(); got != 1 {
		t.Fatalf("hits = %d; want 1", got)
	}
}
