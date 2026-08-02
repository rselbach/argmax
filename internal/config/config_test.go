package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestDefaultValidates(t *testing.T) {
	if err := Default().Validate(); err != nil {
		t.Fatalf("default config must validate: %v", err)
	}
}

func TestLoadMissingFileYieldsDefaults(t *testing.T) {
	cfg, err := LoadValid(filepath.Join(t.TempDir(), "nope.toml"))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.UI.MaxSuggestions != 100 || cfg.Core.Mode != "last" {
		t.Fatalf("unexpected defaults: %+v", cfg.UI)
	}
}

func TestLoadPrecedenceEnvOverFile(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")
	if err := os.WriteFile(path, []byte("[ui]\nmax-suggestions = 42\nghost-text = true\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("ARGMAX_UI_MAX_SUGGESTIONS", "7")
	t.Setenv("ARGMAX_UI_GHOST_TEXT", "false")
	cfg, err := LoadValid(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.UI.MaxSuggestions != 7 {
		t.Fatalf("env must override file: got %d", cfg.UI.MaxSuggestions)
	}
	if cfg.UI.GhostText {
		t.Fatal("env bool override failed")
	}
}

func TestValidateNamesExactKey(t *testing.T) {
	cfg := Default()
	cfg.Core.Mode = "bogus"
	err := cfg.Validate()
	if err == nil || !strings.Contains(err.Error(), "core.mode") {
		t.Fatalf("expected core.mode error, got %v", err)
	}

	cfg = Default()
	cfg.UI.MaxHeight = 2
	if err := cfg.Validate(); err == nil || !strings.Contains(err.Error(), "ui.max-height") {
		t.Fatalf("expected ui.max-height error, got %v", err)
	}

	cfg = Default()
	cfg.AI.Enabled = true
	if err := cfg.Validate(); err == nil || !strings.Contains(err.Error(), "ai.provider") {
		t.Fatalf("expected ai.provider error, got %v", err)
	}
}

func TestValidateCtrlMReserved(t *testing.T) {
	cfg := Default()
	cfg.Keybindings.Select = "ctrl+m"
	if err := cfg.Validate(); err == nil || !strings.Contains(err.Error(), "reserved") {
		t.Fatalf("expected reserved ctrl+m error, got %v", err)
	}
}

func TestParseKeyForms(t *testing.T) {
	cases := map[string]Key{
		"tab":        {Kind: KeySpecial, Special: KeyTab},
		"Shift-Tab":  {Kind: KeySpecial, Special: KeyShiftTab},
		"CTRL+R":     {Kind: KeyCtrl, Ctrl: 'r'},
		"ctrl+space": {Kind: KeyCtrl, Ctrl: 0},
		"x":          {Kind: KeyRune, Rune: 'x'},
		"ENTER":      {Kind: KeySpecial, Special: KeyEnter},
		"cr":         {Kind: KeySpecial, Special: KeyEnter},
	}
	for in, want := range cases {
		got, err := ParseKey(in)
		if err != nil {
			t.Fatalf("ParseKey(%q): %v", in, err)
		}
		if got != want {
			t.Fatalf("ParseKey(%q) = %+v, want %+v", in, got, want)
		}
	}
	for _, bad := range []string{"", "ctrl+m", "ctrl+ab", "banana", "ctrl+"} {
		if _, err := ParseKey(bad); err == nil {
			t.Fatalf("ParseKey(%q) should fail", bad)
		}
	}
}

func TestResolvedRedactsKeys(t *testing.T) {
	cfg := Default()
	cfg.AI.Enabled = true
	cfg.AI.Provider = "groq"
	cfg.AI.Providers["groq"] = AIProvider{Endpoint: "https://x", APIKey: "sekret", Model: "m"}
	out, err := cfg.Resolved()
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out, "sekret") {
		t.Fatal("api key leaked into resolved config")
	}
	if !strings.Contains(out, "<redacted>") {
		t.Fatal("expected redaction marker")
	}
}

func TestMinimalStyleAlias(t *testing.T) {
	cfg := Default()
	cfg.UI.Style = "minimal"
	cfg.Normalize()
	if cfg.UI.Style != "classic" {
		t.Fatalf("minimal alias must resolve to classic, got %q", cfg.UI.Style)
	}
}

func TestStateRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.toml")
	st, err := LoadState(path)
	if err != nil {
		t.Fatal(err)
	}
	if st.LastMode != "spec" {
		t.Fatalf("default last-mode = %q", st.LastMode)
	}
	st.LastMode = "history"
	st.Updater.SeenVersion = "1.2.3"
	st.Updater.LastCheckTime = time.Now().UTC().Truncate(time.Second)
	if err := SaveState(path, st); err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("state file mode = %o, want 600", info.Mode().Perm())
	}
	got, err := LoadState(path)
	if err != nil {
		t.Fatal(err)
	}
	if got.LastMode != "history" || got.Updater.SeenVersion != "1.2.3" {
		t.Fatalf("state mismatch: %+v", got)
	}
	if !got.Updater.LastCheckTime.Equal(st.Updater.LastCheckTime) {
		t.Fatalf("time mismatch: %v vs %v", got.Updater.LastCheckTime, st.Updater.LastCheckTime)
	}
}

func TestCheckIntervalValidation(t *testing.T) {
	cfg := Default()
	if err := cfg.Validate(); err != nil {
		t.Fatal(err)
	}
	if cfg.Updater.CheckInterval.D() != 24*time.Hour {
		t.Fatalf("default interval = %v", cfg.Updater.CheckInterval.D())
	}
}
