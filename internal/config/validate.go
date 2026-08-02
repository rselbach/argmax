package config

import (
	"fmt"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/keymap"
)

// Validate checks enums, numeric bounds, durations, key names, and the
// active AI provider reference. Errors name the exact invalid key and the
// accepted range.
func Validate(cfg *Config) error {
	if cfg.Core.Version < 1 {
		return keyError("core.version", "positive supported schema version")
	}
	switch cfg.Core.Shell {
	case "", "bash", "zsh", "fish":
	default:
		return keyError("core.shell", `empty, "bash", "zsh", or "fish"`)
	}
	switch cfg.Core.Mode {
	case "last", "spec", "history":
	default:
		return keyError("core.mode", `"last", "spec", or "history"`)
	}
	switch strings.ToLower(cfg.UI.Style) {
	case "modern", "classic":
	case "minimal", "minimalist":
		cfg.UI.Style = "classic"
	default:
		return keyError("ui.style", `"modern" or "classic"`)
	}
	if cfg.UI.MaxSuggestions < 1 || cfg.UI.MaxSuggestions > 500 {
		return keyError("ui.max-suggestions", "1-500")
	}
	if cfg.UI.MaxHeight < 3 || cfg.UI.MaxHeight > 50 {
		return keyError("ui.max-height", "3-50")
	}
	if cfg.UI.MaxWidth < 0 {
		return keyError("ui.max-width", "0 or a positive value")
	}
	switch cfg.Updater.Channel {
	case "stable", "nightly":
	default:
		return keyError("updater.channel", `"stable" or "nightly"`)
	}
	if d, err := time.ParseDuration(cfg.Updater.CheckInterval); err != nil || d <= 0 {
		return keyError("updater.check-interval", `positive Go duration such as "30m", "6h", or "24h"`)
	}
	for key, v := range map[string]int{
		"ai.debounce_ms":                      cfg.AI.DebounceMS,
		"ai.min_interval_ms":                  cfg.AI.MinIntervalMS,
		"ai.suggest_on_empty.debounce_ms":     cfg.AI.SuggestOnEmpty.DebounceMS,
		"ai.suggest_on_empty.min_interval_ms": cfg.AI.SuggestOnEmpty.MinIntervalMS,
	} {
		if v < 0 {
			return keyError(key, "non-negative integer")
		}
	}
	for key, raw := range map[string]string{
		"keybindings.toggle-mode":   cfg.Keybindings.ToggleMode,
		"keybindings.toggle-menu":   cfg.Keybindings.ToggleMenu,
		"keybindings.select":        cfg.Keybindings.Select,
		"keybindings.navigate-up":   cfg.Keybindings.NavigateUp,
		"keybindings.navigate-down": cfg.Keybindings.NavigateDown,
	} {
		k, err := keymap.Parse(raw)
		if err != nil {
			return keyError(key, err.Error())
		}
		if k.IsEnter() {
			return keyError(key, "must not shadow Enter/command submission")
		}
	}
	if cfg.AI.Enabled {
		if cfg.AI.Provider == "" {
			return keyError("ai.provider", "must name a configured provider while AI is enabled")
		}
		if _, ok := cfg.AI.Providers[cfg.AI.Provider]; !ok {
			return keyError("ai.provider", fmt.Sprintf("provider %q is not configured under [ai.providers.*]", cfg.AI.Provider))
		}
	}
	for name, p := range cfg.AI.Providers {
		if p.TimeoutMS < 0 {
			return keyError("ai.providers."+name+".timeout_ms", "non-negative integer")
		}
	}
	return nil
}

func keyError(key, accepted string) error {
	return fmt.Errorf("invalid configuration key %s: accepted: %s", key, accepted)
}
