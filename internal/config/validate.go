package config

import (
	"fmt"
	"strings"
)

// Validate checks the resolved configuration, naming the exact invalid key
// and the accepted range (PRD CFG-004).
func (c *Config) Validate() error {
	if c.Core.Version < 1 || c.Core.Version > CurrentVersion {
		return fmt.Errorf("core.version: must be a supported schema version between 1 and %d, got %d", CurrentVersion, c.Core.Version)
	}
	switch c.Core.Shell {
	case "", "bash", "zsh", "fish":
	default:
		return fmt.Errorf("core.shell: must be empty, bash, zsh, or fish, got %q", c.Core.Shell)
	}
	switch c.Core.Mode {
	case "last", "spec", "history":
	default:
		return fmt.Errorf("core.mode: must be last, spec, or history, got %q", c.Core.Mode)
	}
	switch c.UI.Style {
	case "modern", "classic":
	default:
		return fmt.Errorf("ui.style: must be modern or classic, got %q", c.UI.Style)
	}
	if c.UI.MaxSuggestions < 1 || c.UI.MaxSuggestions > 500 {
		return fmt.Errorf("ui.max-suggestions: must be between 1 and 500, got %d", c.UI.MaxSuggestions)
	}
	if c.UI.MaxHeight < 3 || c.UI.MaxHeight > 50 {
		return fmt.Errorf("ui.max-height: must be between 3 and 50, got %d", c.UI.MaxHeight)
	}
	if c.UI.MaxWidth < 0 {
		return fmt.Errorf("ui.max-width: must be 0 or a positive value, got %d", c.UI.MaxWidth)
	}

	keys := map[string]string{
		"keybindings.toggle-mode":   c.Keybindings.ToggleMode,
		"keybindings.toggle-menu":   c.Keybindings.ToggleMenu,
		"keybindings.select":        c.Keybindings.Select,
		"keybindings.navigate-up":   c.Keybindings.NavigateUp,
		"keybindings.navigate-down": c.Keybindings.NavigateDown,
	}
	for name, value := range keys {
		if _, err := ParseKey(value); err != nil {
			return fmt.Errorf("%s: %v", name, err)
		}
	}

	switch c.Updater.Channel {
	case "stable", "nightly":
	default:
		return fmt.Errorf("updater.channel: must be stable or nightly, got %q", c.Updater.Channel)
	}
	if c.Updater.CheckInterval.D() <= 0 {
		return fmt.Errorf("updater.check-interval: must be a positive Go-style duration such as 30m, 6h, or 24h, got %q", c.Updater.CheckInterval.D().String())
	}

	for key, val := range map[string]int{
		"ai.debounce_ms":                      c.AI.DebounceMs,
		"ai.min_interval_ms":                  c.AI.MinIntervalMs,
		"ai.suggest_on_empty.debounce_ms":     c.AI.SuggestOnEmpty.DebounceMs,
		"ai.suggest_on_empty.min_interval_ms": c.AI.SuggestOnEmpty.MinIntervalMs,
	} {
		if val < 0 {
			return fmt.Errorf("%s: must be a non-negative integer, got %d", key, val)
		}
	}
	for name, p := range c.AI.Providers {
		if p.TimeoutMs < 0 {
			return fmt.Errorf("ai.providers.%s.timeout_ms: must be a non-negative integer, got %d", name, p.TimeoutMs)
		}
		if p.Endpoint != "" && !strings.HasPrefix(p.Endpoint, "http://") && !strings.HasPrefix(p.Endpoint, "https://") {
			return fmt.Errorf("ai.providers.%s.endpoint: must start with http:// or https://, got %q", name, p.Endpoint)
		}
	}
	if c.AI.Enabled {
		if c.AI.Provider == "" {
			return fmt.Errorf("ai.provider: must name a configured provider while ai.enabled is true")
		}
		if _, ok := c.AI.Providers[c.AI.Provider]; !ok {
			return fmt.Errorf("ai.provider: provider %q is not defined in ai.providers", c.AI.Provider)
		}
	}
	return nil
}

// Normalize applies accepted aliases (e.g. minimal/minimalist for ui.style).
func (c *Config) Normalize() {
	switch strings.ToLower(c.UI.Style) {
	case "minimal", "minimalist":
		c.UI.Style = "classic"
	}
}
