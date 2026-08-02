// Package config loads, validates, and hot-reloads the argmax TOML
// configuration, applying compiled defaults, file values, environment
// overrides, and CLI flags in that order of precedence.
package config

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/BurntSushi/toml"

	"github.com/rselbach/argmax/internal/paths"
)

// Config is the fully resolved argmax configuration.
type Config struct {
	Core        Core        `toml:"core"`
	UI          UI          `toml:"ui"`
	Keybindings Keybindings `toml:"keybindings"`
	Git         Git         `toml:"git"`
	Updater     Updater     `toml:"updater"`
	AI          AI          `toml:"ai"`
}

// Core holds session-level settings.
type Core struct {
	Version     int    `toml:"version"`
	Shell       string `toml:"shell"`
	ShellLogin  bool   `toml:"shell-login"`
	Mode        string `toml:"mode"`
	Debug       bool   `toml:"debug"`
	ExpandAlias bool   `toml:"expand-alias"`
	AutoExecute bool   `toml:"auto-execute"`
}

// UI holds menu and ghost-text settings.
type UI struct {
	Style          string `toml:"style"`
	GhostText      bool   `toml:"ghost-text"`
	HiddenFiles    bool   `toml:"hidden-files"`
	MaxSuggestions int    `toml:"max-suggestions"`
	MaxHeight      int    `toml:"max-height"`
	MaxWidth       int    `toml:"max-width"`
	NerdFonts      bool   `toml:"nerd-fonts"`
}

// Keybindings holds the configurable key assignments.
type Keybindings struct {
	ToggleMode   string `toml:"toggle-mode"`
	ToggleMenu   string `toml:"toggle-menu"`
	Select       string `toml:"select"`
	NavigateUp   string `toml:"navigate-up"`
	NavigateDown string `toml:"navigate-down"`
}

// Git holds Git-specific completion policy.
type Git struct {
	FilterActiveBranch  bool `toml:"filter-active-branch"`
	DeduplicateBranches bool `toml:"deduplicate-branches"`
}

// Updater holds release-check policy.
type Updater struct {
	CheckOnStartup bool   `toml:"check-on-startup"`
	Channel        string `toml:"channel"`
	CheckInterval  string `toml:"check-interval"`
}

// AI holds the optional completion-provider settings.
type AI struct {
	Enabled        bool                `toml:"enabled"`
	Provider       string              `toml:"provider"`
	DebounceMS     int                 `toml:"debounce_ms"`
	MinIntervalMS  int                 `toml:"min_interval_ms"`
	SuggestOnEmpty SuggestOnEmpty      `toml:"suggest_on_empty"`
	Providers      map[string]Provider `toml:"providers"`
}

// SuggestOnEmpty controls empty-prompt prediction.
type SuggestOnEmpty struct {
	Enabled       bool `toml:"enabled"`
	DebounceMS    int  `toml:"debounce_ms"`
	MinIntervalMS int  `toml:"min_interval_ms"`
}

// Provider describes one named OpenAI-compatible endpoint.
type Provider struct {
	InheritedFrom    string         `toml:"inherited_from"`
	Endpoint         string         `toml:"endpoint"`
	APIKey           string         `toml:"api_key"`
	APIKeyEnv        string         `toml:"api_key_env"`
	Model            string         `toml:"model"`
	TimeoutMS        int            `toml:"timeout_ms"`
	ExtraRequestBody map[string]any `toml:"extra_request_body"`
}

// Default returns the compiled default configuration.
func Default() *Config {
	return &Config{
		Core: Core{
			Version:     1,
			Mode:        "last",
			ExpandAlias: true,
		},
		UI: UI{
			Style:          "modern",
			GhostText:      true,
			MaxSuggestions: 100,
			MaxHeight:      15,
			NerdFonts:      true,
		},
		Keybindings: Keybindings{
			ToggleMode:   "ctrl+r",
			ToggleMenu:   "shift+tab",
			Select:       "tab",
			NavigateUp:   "up",
			NavigateDown: "down",
		},
		Git: Git{
			FilterActiveBranch:  true,
			DeduplicateBranches: true,
		},
		Updater: Updater{
			CheckOnStartup: true,
			Channel:        "stable",
			CheckInterval:  "24h",
		},
		AI: AI{
			DebounceMS:    500,
			MinIntervalMS: 1000,
			SuggestOnEmpty: SuggestOnEmpty{
				DebounceMS:    800,
				MinIntervalMS: 5000,
			},
			Providers: map[string]Provider{},
		},
	}
}

// Load resolves the configuration from defaults, the TOML file at path,
// and environment overrides. A missing file is not an error. Decoding
// into the default-populated struct means absent keys keep their compiled
// defaults.
func Load(path string) (*Config, error) {
	cfg := Default()
	data, err := os.ReadFile(path)
	switch {
	case os.IsNotExist(err):
	case err != nil:
		return nil, fmt.Errorf("read config: %w", err)
	default:
		if err := toml.Unmarshal(data, cfg); err != nil {
			return nil, fmt.Errorf("parse config: %w", err)
		}
		if err := migrate(cfg, path); err != nil {
			return nil, err
		}
	}
	applyEnv(cfg)
	if err := resolveProviderInheritance(cfg); err != nil {
		return nil, err
	}
	if err := Validate(cfg); err != nil {
		return nil, err
	}
	return cfg, nil
}

func applyEnv(cfg *Config) {
	setBool := func(key string, dst *bool) {
		if v, ok := os.LookupEnv(key); ok {
			if b, err := strconv.ParseBool(v); err == nil {
				*dst = b
			}
		}
	}
	setInt := func(key string, dst *int) {
		if v, ok := os.LookupEnv(key); ok {
			if n, err := strconv.Atoi(v); err == nil {
				*dst = n
			}
		}
	}
	setStr := func(key string, dst *string) {
		if v, ok := os.LookupEnv(key); ok {
			*dst = v
		}
	}
	setBool("ARGMAX_CORE_DEBUG", &cfg.Core.Debug)
	setStr("ARGMAX_CORE_SHELL", &cfg.Core.Shell)
	setStr("ARGMAX_CORE_MODE", &cfg.Core.Mode)
	setBool("ARGMAX_UI_GHOST_TEXT", &cfg.UI.GhostText)
	setInt("ARGMAX_UI_MAX_SUGGESTIONS", &cfg.UI.MaxSuggestions)
	setInt("ARGMAX_UI_MAX_HEIGHT", &cfg.UI.MaxHeight)
	setStr("ARGMAX_UPDATER_CHANNEL", &cfg.Updater.Channel)
	setStr("ARGMAX_UPDATER_INTERVAL", &cfg.Updater.CheckInterval)
	setBool("ARGMAX_UPDATER_CHECK_ON_STARTUP", &cfg.Updater.CheckOnStartup)
	setBool("ARGMAX_AI_ENABLED", &cfg.AI.Enabled)
	setStr("ARGMAX_AI_PROVIDER", &cfg.AI.Provider)
}

// Path returns the configuration file location.
func Path() string { return paths.Config() }

// CheckInterval parses the updater interval, defaulting to 24h.
func (c *Config) CheckInterval() time.Duration {
	d, err := time.ParseDuration(c.Updater.CheckInterval)
	if err != nil || d <= 0 {
		return 24 * time.Hour
	}
	return d
}

// ActiveProvider resolves the enabled AI provider, or nil when AI is off.
func (c *Config) ActiveProvider() *Provider {
	if !c.AI.Enabled {
		return nil
	}
	p, ok := c.AI.Providers[c.AI.Provider]
	if !ok {
		return nil
	}
	return &p
}

// ResolveAPIKey returns the provider credential, preferring api_key_env.
func (p *Provider) ResolveAPIKey() string {
	if p.APIKeyEnv != "" {
		if v := os.Getenv(p.APIKeyEnv); v != "" {
			return v
		}
	}
	return p.APIKey
}

// Timeout returns the provider request timeout with the documented fallback.
func (p *Provider) Timeout() time.Duration {
	if p.TimeoutMS <= 0 {
		return 2 * time.Second
	}
	return time.Duration(p.TimeoutMS) * time.Millisecond
}

// Redacted returns resolved TOML with direct API keys masked, for
// `argmax config show`.
func (c *Config) Redacted() (string, error) {
	var b strings.Builder
	enc := toml.NewEncoder(&b)
	type redactedProvider struct {
		InheritedFrom    string         `toml:"inherited_from,omitempty"`
		Endpoint         string         `toml:"endpoint,omitempty"`
		APIKey           string         `toml:"api_key,omitempty"`
		APIKeyEnv        string         `toml:"api_key_env,omitempty"`
		Model            string         `toml:"model,omitempty"`
		TimeoutMS        int            `toml:"timeout_ms,omitempty"`
		ExtraRequestBody map[string]any `toml:"extra_request_body,omitempty"`
	}
	out := struct {
		Core        Core        `toml:"core"`
		UI          UI          `toml:"ui"`
		Keybindings Keybindings `toml:"keybindings"`
		Git         Git         `toml:"git"`
		Updater     Updater     `toml:"updater"`
		AI          struct {
			AI
			Providers map[string]redactedProvider `toml:"providers,omitempty"`
		} `toml:"ai"`
	}{Core: c.Core, UI: c.UI, Keybindings: c.Keybindings, Git: c.Git, Updater: c.Updater}
	out.AI.AI = c.AI
	// The redacted map below replaces the embedded provider table.
	out.AI.AI.Providers = nil
	out.AI.Providers = make(map[string]redactedProvider, len(c.AI.Providers))
	for name, p := range c.AI.Providers {
		rp := redactedProvider(p)
		if rp.APIKey != "" {
			rp.APIKey = "<redacted>"
		}
		out.AI.Providers[name] = rp
	}
	if err := enc.Encode(out); err != nil {
		return "", fmt.Errorf("encode config: %w", err)
	}
	return b.String(), nil
}
