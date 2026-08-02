// Package ai implements the optional, disabled-by-default OpenAI-compatible
// completion engine (PRD 9.12): provider client, bounded context gathering,
// prompt construction, output validation, caching/cooldown, and deterministic
// empty-prompt rules.
//
// All provider errors fail silently (ok=false) and nothing is logged here, so
// API keys and request/response payloads can never leak into diagnostic logs
// (DIAG-008). Generated text is only ever suggested, never executed (AI-015).
package ai

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/rselbach/argmax/internal/config"
)

// Engine defaults and bounds (PRD 9.12, PERF-005).
const (
	defaultMinIntervalMs = 1000 // AI-005 one-second minimum call interval
	defaultTimeoutMs     = 2000 // PERF-005 provider timeout fallback
	defaultCooldown      = 20 * time.Second
	suggestionCacheTTL   = 30 * time.Second // AI-012
)

// Request is one typed-completion request.
type Request struct {
	Buffer      string // current command buffer (must be preserved as prefix)
	CWD         string
	PrevCommand string
	PrevExit    int
	Recent      []string // up to 3 recent commands, newest first
}

// Message is one chat-completion message.
type Message struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// cachedSuggestion is the single 30-second suggestion slot (AI-012, PERF-007).
type cachedSuggestion struct {
	buffer string    // request buffer the suggestion was produced for
	text   string    // validated completion (starts with buffer)
	at     time.Time // when the suggestion was stored
}

// Engine is the AI completion engine. Its state is mutex-guarded and safe for
// concurrent use, although Suggest is normally called from a single goroutine.
type Engine struct {
	mu            sync.Mutex
	cfg           *config.AI
	lastCall      time.Time
	cooldownUntil time.Time
	cached        cachedSuggestion

	httpClient *http.Client

	// Test hooks. Production code must not touch these after New.
	now      func() time.Time // clock (injectable)
	cooldown time.Duration    // AI-013 rate-limit cooldown, default 20s
	cacheTTL time.Duration    // suggestion cache TTL, default 30s
}

// New creates an Engine from the [ai] configuration section. cfg may be nil,
// which yields a disabled engine.
func New(cfg *config.AI) *Engine {
	return &Engine{
		cfg:        cfg,
		httpClient: &http.Client{},
		now:        time.Now,
		cooldown:   defaultCooldown,
		cacheTTL:   suggestionCacheTTL,
	}
}

// UpdateConfig swaps the configuration for live reload. Runtime state
// (cooldown, cached suggestion) is kept; it is time-based and stays valid.
func (e *Engine) UpdateConfig(cfg *config.AI) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.cfg = cfg
}

// Enabled reports whether AI completion is active: the feature flag is on and
// the selected provider resolves to a usable endpoint. When false, Suggest
// performs no I/O at all (AI-001).
func (e *Engine) Enabled() bool {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.enabledLocked()
}

func (e *Engine) enabledLocked() bool {
	if e.cfg == nil || !e.cfg.Enabled {
		return false
	}
	p, ok := e.cfg.Providers[e.cfg.Provider]
	return ok && p.Endpoint != ""
}

// Suggest returns one completed command line beginning exactly with
// req.Buffer, or ok=false. Honors: provider cooldown (20s after HTTP 429,
// AI-013), minimum call interval (AI-005; engine enforces MinIntervalMs with
// runtime fallback 1000ms), a 30s prefix-compatible suggestion cache
// (AI-012), and response validation (AI-009/010). All provider errors fail
// silently (ok=false). The whole call is bounded by the provider timeout
// (configured TimeoutMs, fallback 2s), which also caps context gathering.
func (e *Engine) Suggest(ctx context.Context, req Request) (text string, ok bool) {
	e.mu.Lock()
	if !e.enabledLocked() {
		e.mu.Unlock()
		return "", false
	}
	now := e.now()
	if now.Before(e.cooldownUntil) {
		e.mu.Unlock()
		return "", false
	}
	if c := e.cached; c.text != "" && now.Sub(c.at) < e.cacheTTL &&
		strings.HasPrefix(c.text, req.Buffer) && strings.HasPrefix(req.Buffer, c.buffer) {
		e.mu.Unlock()
		return c.text, true
	}
	minInterval := time.Duration(e.cfg.MinIntervalMs) * time.Millisecond
	if minInterval <= 0 {
		minInterval = defaultMinIntervalMs * time.Millisecond
	}
	if now.Sub(e.lastCall) < minInterval {
		e.mu.Unlock()
		return "", false
	}
	e.lastCall = now
	provider := e.cfg.Providers[e.cfg.Provider]
	e.mu.Unlock()

	if ctx.Err() != nil {
		return "", false
	}

	// Bound the entire request, context gathering included, by the provider
	// timeout so a stale request can never outlive it (PERF-005).
	timeout := time.Duration(provider.TimeoutMs) * time.Millisecond
	if timeout <= 0 {
		timeout = defaultTimeoutMs * time.Millisecond
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	gathered := GatherContext(ctx, req)
	raw, err := e.chat(ctx, provider, BuildMessages(req, gathered))
	if err != nil {
		if errors.Is(err, errRateLimited) {
			e.mu.Lock()
			e.cooldownUntil = e.now().Add(e.cooldown)
			e.mu.Unlock()
		}
		return "", false
	}
	text, ok = ValidateOutput(req.Buffer, raw)
	if !ok {
		return "", false
	}
	e.mu.Lock()
	e.cached = cachedSuggestion{buffer: req.Buffer, text: text, at: e.now()}
	e.mu.Unlock()
	return text, true
}
