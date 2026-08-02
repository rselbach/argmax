package config

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"strconv"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

// Load reads the TOML configuration over the compiled defaults, then applies
// environment overrides. A missing file yields defaults with env overrides.
func Load(path string) (*Config, error) {
	cfg := Default()
	data, err := os.ReadFile(path)
	if err != nil {
		if !errors.Is(err, fs.ErrNotExist) {
			return nil, fmt.Errorf("read config: %w", err)
		}
	} else {
		if err := toml.Unmarshal(data, cfg); err != nil {
			return nil, fmt.Errorf("parse %s: %w", path, err)
		}
	}
	cfg.ApplyEnv()
	cfg.Normalize()
	return cfg, nil
}

// LoadValid loads and validates the configuration.
func LoadValid(path string) (*Config, error) {
	cfg, err := Load(path)
	if err != nil {
		return nil, err
	}
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	return cfg, nil
}

// ApplyEnv applies supported environment variable overrides (PRD 9.13).
func (c *Config) ApplyEnv() {
	setBool := func(env string, dst *bool) {
		if v, ok := os.LookupEnv(env); ok {
			if b, err := strconv.ParseBool(strings.TrimSpace(v)); err == nil {
				*dst = b
			}
		}
	}
	setInt := func(env string, dst *int) {
		if v, ok := os.LookupEnv(env); ok {
			if n, err := strconv.Atoi(strings.TrimSpace(v)); err == nil {
				*dst = n
			}
		}
	}
	setStr := func(env string, dst *string) {
		if v, ok := os.LookupEnv(env); ok {
			*dst = v
		}
	}

	setBool("ARGMAX_CORE_DEBUG", &c.Core.Debug)
	setStr("ARGMAX_CORE_SHELL", &c.Core.Shell)
	setStr("ARGMAX_CORE_MODE", &c.Core.Mode)
	setBool("ARGMAX_UI_GHOST_TEXT", &c.UI.GhostText)
	setInt("ARGMAX_UI_MAX_SUGGESTIONS", &c.UI.MaxSuggestions)
	setInt("ARGMAX_UI_MAX_HEIGHT", &c.UI.MaxHeight)
	setStr("ARGMAX_UPDATER_CHANNEL", &c.Updater.Channel)
	if v, ok := os.LookupEnv("ARGMAX_UPDATER_INTERVAL"); ok {
		var d Duration
		if err := d.UnmarshalText([]byte(v)); err == nil {
			c.Updater.CheckInterval = d
		}
	}
	setBool("ARGMAX_UPDATER_CHECK_ON_STARTUP", &c.Updater.CheckOnStartup)
	setBool("ARGMAX_AI_ENABLED", &c.AI.Enabled)
	setStr("ARGMAX_AI_PROVIDER", &c.AI.Provider)
}

// Resolved renders the fully resolved configuration as valid TOML.
// Direct API keys are redacted (PRD CFG-003).
func (c *Config) Resolved() (string, error) {
	clone := *c
	if c.AI.Providers != nil {
		providers := make(map[string]AIProvider, len(c.AI.Providers))
		for name, p := range c.AI.Providers {
			if p.APIKey != "" {
				p.APIKey = "<redacted>"
			}
			providers[name] = p
		}
		clone.AI.Providers = providers
	}
	out, err := toml.Marshal(clone)
	if err != nil {
		return "", err
	}
	return string(out), nil
}
