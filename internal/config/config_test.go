package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func writeConfig(t *testing.T, content string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "config.toml")
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestLoadDefaults(t *testing.T) {
	cfg, err := Load(filepath.Join(t.TempDir(), "missing.toml"))
	if err != nil {
		t.Fatalf("missing file must not fail: %v", err)
	}
	if cfg.UI.MaxSuggestions != 100 || cfg.UI.Style != "modern" || cfg.AI.Enabled {
		t.Errorf("defaults not applied: %+v", cfg.UI)
	}
	if cfg.Keybindings.ToggleMode != "ctrl+r" {
		t.Errorf("default toggle-mode = %q", cfg.Keybindings.ToggleMode)
	}
}

func TestLoadFileOverridesDefaults(t *testing.T) {
	path := writeConfig(t, "[ui]\nstyle = \"classic\"\nghost-text = false\nhidden-files = false\nmax-suggestions = 42\nmax-height = 10\nmax-width = 0\nnerd-fonts = false\n")
	cfg, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.UI.Style != "classic" || cfg.UI.MaxSuggestions != 42 {
		t.Errorf("file values not applied: %+v", cfg.UI)
	}
	if cfg.Core.Mode != "last" {
		t.Errorf("untouched sections keep defaults, got mode %q", cfg.Core.Mode)
	}
}

func TestLoadRejectsUnknownKeys(t *testing.T) {
	path := writeConfig(t, "[ui]\nmax-sugestions = 42\nunknown = true\n")
	_, err := Load(path)
	if err == nil {
		t.Fatal("unknown keys must be rejected")
	}
	for _, key := range []string{"ui.max-sugestions", "ui.unknown"} {
		if !strings.Contains(err.Error(), key) {
			t.Errorf("error %q does not report %q", err, key)
		}
	}
}

func TestWatcherRetriesFailedReloadAtSameModTime(t *testing.T) {
	path := writeConfig(t, "[core]\nmode = \"last\"\n")
	initial, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	w := NewWatcher(path, Default())
	failedModTime := initial.ModTime().Add(2 * time.Second)

	if err := os.WriteFile(path, []byte("[core\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(path, failedModTime, failedModTime); err != nil {
		t.Fatal(err)
	}
	w.Refresh()

	if err := os.WriteFile(path, []byte("[core]\nmode = \"history\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(path, failedModTime, failedModTime); err != nil {
		t.Fatal(err)
	}
	w.Refresh()
	if got := w.Current().Core.Mode; got != "history" {
		t.Errorf("mode = %q, want corrected config to be retried", got)
	}
}

func TestEnvOverridesFile(t *testing.T) {
	path := writeConfig(t, "[core]\nversion = 1\nmode = \"spec\"\n")
	t.Setenv("ARGMAX_CORE_MODE", "history")
	t.Setenv("ARGMAX_UI_MAX_SUGGESTIONS", "7")
	cfg, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Core.Mode != "history" {
		t.Errorf("env override lost, mode = %q", cfg.Core.Mode)
	}
	if cfg.UI.MaxSuggestions != 7 {
		t.Errorf("env override lost, max-suggestions = %d", cfg.UI.MaxSuggestions)
	}
}

func TestValidationNamesKey(t *testing.T) {
	tests := map[string]struct {
		content string
		wantKey string
	}{
		"bad shell":      {content: "[core]\nversion = 1\nmode = \"last\"\nshell = \"powershell\"\n", wantKey: "core.shell"},
		"bad mode":       {content: "[core]\nversion = 1\nmode = \"turbo\"\n", wantKey: "core.mode"},
		"bad style":      {content: "[ui]\nstyle = \"neon\"\nmax-suggestions = 10\nmax-height = 10\nnerd-fonts = true\n", wantKey: "ui.style"},
		"bad max":        {content: "[ui]\nstyle = \"modern\"\nmax-suggestions = 900\nmax-height = 10\n", wantKey: "ui.max-suggestions"},
		"bad channel":    {content: "[updater]\nchannel = \"beta\"\ncheck-interval = \"24h\"\ncheck-on-startup = true\n", wantKey: "updater.channel"},
		"bad interval":   {content: "[updater]\nchannel = \"stable\"\ncheck-interval = \"soon\"\ncheck-on-startup = true\n", wantKey: "updater.check-interval"},
		"enter binding":  {content: "[keybindings]\ntoggle-mode = \"ctrl+m\"\ntoggle-menu = \"shift+tab\"\nselect = \"tab\"\nnavigate-up = \"up\"\nnavigate-down = \"down\"\n", wantKey: "keybindings.toggle-mode"},
		"ai no provider": {content: "[ai]\nenabled = true\n", wantKey: "ai.provider"},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := Load(writeConfig(t, tc.content))
			if err == nil {
				t.Fatal("want validation error")
			}
			if !strings.Contains(err.Error(), tc.wantKey) {
				t.Errorf("error %q does not name key %q", err, tc.wantKey)
			}
		})
	}
}

func TestActiveProviderRequiresValidEndpointAndModel(t *testing.T) {
	tests := map[string]struct {
		provider string
		wantKey  string
	}{
		"missing endpoint": {
			provider: "model = \"test\"\n",
			wantKey:  "ai.providers.test.endpoint",
		},
		"unsupported endpoint scheme": {
			provider: "endpoint = \"file:///tmp/socket\"\nmodel = \"test\"\n",
			wantKey:  "ai.providers.test.endpoint",
		},
		"endpoint credentials": {
			provider: "endpoint = \"https://secret@example.com/v1\"\nmodel = \"test\"\n",
			wantKey:  "ai.providers.test.endpoint",
		},
		"missing model": {
			provider: "endpoint = \"https://example.com/v1\"\n",
			wantKey:  "ai.providers.test.model",
		},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			path := writeConfig(t, "[ai]\nenabled = true\nprovider = \"test\"\n\n[ai.providers.test]\n"+test.provider)
			_, err := Load(path)
			if err == nil || !strings.Contains(err.Error(), test.wantKey) {
				t.Errorf("Load() error = %v, want key %q", err, test.wantKey)
			}
		})
	}
}

func TestClassicAliases(t *testing.T) {
	path := writeConfig(t, "[ui]\nstyle = \"minimal\"\nmax-suggestions = 10\nmax-height = 10\n")
	cfg, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.UI.Style != "classic" {
		t.Errorf("minimal should resolve to classic, got %q", cfg.UI.Style)
	}
}

func TestDefaultTemplateIsValid(t *testing.T) {
	path := writeConfig(t, DefaultTemplate)
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("commented default template must load: %v", err)
	}
	want := Default()
	if cfg.UI != want.UI || cfg.Core != want.Core || cfg.AI.Enabled {
		t.Errorf("template values diverge from compiled defaults")
	}
}

func TestProvidersAndRedaction(t *testing.T) {
	path := writeConfig(t, `
[ai]
enabled = true
provider = "groq"

[ai.providers.groq]
endpoint = "https://api.groq.com/openai/v1"
api_key = "supersecret"
model = "llama-3.3-70b-versatile"
timeout_ms = 3000

[ai.providers.groq.extra_request_body]
temperature = 0.2
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	p := cfg.ActiveProvider()
	if p == nil || p.Model != "llama-3.3-70b-versatile" {
		t.Fatalf("active provider = %+v", p)
	}
	out, err := cfg.Redacted()
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out, "supersecret") {
		t.Error("config show must redact direct API keys")
	}
	if !strings.Contains(out, "<redacted>") {
		t.Error("redaction marker missing")
	}
}

func TestProviderKeyResolution(t *testing.T) {
	t.Setenv("TEST_ARGMAX_KEY", "from-env")
	p := Provider{APIKey: "direct", APIKeyEnv: "TEST_ARGMAX_KEY"}
	if got := p.ResolveAPIKey(); got != "from-env" {
		t.Errorf("api_key_env should win, got %q", got)
	}
	p.APIKeyEnv = "TEST_ARGMAX_MISSING"
	if got := p.ResolveAPIKey(); got != "direct" {
		t.Errorf("fallback to direct key, got %q", got)
	}
}

func TestProviderInheritance(t *testing.T) {
	path := writeConfig(t, `
[ai]
enabled = true
provider = "groq"

[ai.providers.base]
endpoint = "https://base.example/v1"
model = "base-model"
api_key_env = "BASE_KEY"
timeout_ms = 4000

[ai.providers.base.extra_request_body]
temperature = 0.5
max_tokens = 80

[ai.providers.groq]
inherited_from = "base"
model = "llama-3.3-70b-versatile"

[ai.providers.groq.extra_request_body]
temperature = 0.2
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	p := cfg.ActiveProvider()
	if p == nil {
		t.Fatal("no active provider")
	}
	if p.Endpoint != "https://base.example/v1" {
		t.Errorf("endpoint not inherited: %q", p.Endpoint)
	}
	if p.Model != "llama-3.3-70b-versatile" {
		t.Errorf("own model must win: %q", p.Model)
	}
	if p.APIKeyEnv != "BASE_KEY" || p.TimeoutMS != 4000 {
		t.Errorf("credentials/timeout not inherited: %+v", p)
	}
	if p.ExtraRequestBody["temperature"] != 0.2 {
		t.Errorf("own extra body must override base: %v", p.ExtraRequestBody["temperature"])
	}
	if p.ExtraRequestBody["max_tokens"] != int64(80) && p.ExtraRequestBody["max_tokens"] != float64(80) {
		t.Errorf("base extra body must merge: %v", p.ExtraRequestBody["max_tokens"])
	}
}

func TestProviderInheritanceOpenAIBase(t *testing.T) {
	path := writeConfig(t, `
[ai]
enabled = true
provider = "mine"

[ai.providers.mine]
inherited_from = "openai"
model = "gpt-4o-mini"
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	p := cfg.ActiveProvider()
	if p.Endpoint != "https://api.openai.com/v1" || p.APIKeyEnv != "OPENAI_API_KEY" {
		t.Errorf("openai protocol defaults not applied: %+v", p)
	}
}

func TestProviderInheritanceErrors(t *testing.T) {
	tests := map[string]string{
		"unknown base": `
[ai.providers.a]
inherited_from = "nope"
`,
		"cycle": `
[ai.providers.a]
inherited_from = "b"

[ai.providers.b]
inherited_from = "a"
`,
	}
	for name, content := range tests {
		t.Run(name, func(t *testing.T) {
			if _, err := Load(writeConfig(t, content)); err == nil {
				t.Error("want inheritance validation error")
			}
		})
	}
}
