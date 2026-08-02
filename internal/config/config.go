// Package config implements argmax configuration loading, validation,
// environment overrides, and platform paths (PRD 9.13, 9.15).
package config

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"time"
)

// CurrentVersion is the supported configuration schema version.
const CurrentVersion = 1

// Paths resolves the platform/XDG filesystem layout.
type Paths struct {
	ConfigFile string
	DataDir    string
	StateFile  string
	DBFile     string
	CacheDir   string
	LogFile    string
	CrashesDir string
}

// ResolvePaths computes the filesystem layout (PRD 9.15).
func ResolvePaths() Paths {
	home, _ := os.UserHomeDir()

	configBase := os.Getenv("XDG_CONFIG_HOME")
	if configBase == "" {
		if runtime.GOOS == "darwin" {
			configBase = filepath.Join(home, "Library", "Application Support")
		} else {
			configBase = filepath.Join(home, ".config")
		}
	}

	dataBase := os.Getenv("XDG_DATA_HOME")
	if dataBase == "" {
		dataBase = filepath.Join(home, ".local", "share")
	}

	cacheBase := os.Getenv("XDG_CACHE_HOME")
	if cacheBase == "" {
		if runtime.GOOS == "darwin" {
			cacheBase = filepath.Join(home, "Library", "Caches")
		} else {
			cacheBase = filepath.Join(home, ".cache")
		}
	}

	dataDir := filepath.Join(dataBase, "argmax")
	cacheDir := filepath.Join(cacheBase, "argmax")
	return Paths{
		ConfigFile: filepath.Join(configBase, "argmax", "config.toml"),
		DataDir:    dataDir,
		StateFile:  filepath.Join(dataDir, "state.toml"),
		DBFile:     filepath.Join(dataDir, "history.db"),
		CacheDir:   cacheDir,
		LogFile:    filepath.Join(cacheDir, "argmax.log"),
		CrashesDir: filepath.Join(cacheDir, "crashes"),
	}
}

// EnsureDirs creates the data and cache directories with private permissions.
func (p Paths) EnsureDirs() error {
	for _, dir := range []string{p.DataDir, p.CacheDir, p.CrashesDir, filepath.Dir(p.ConfigFile)} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return fmt.Errorf("create %s: %w", dir, err)
		}
		// Tighten permissions in case the directory already existed.
		_ = os.Chmod(dir, 0o700)
	}
	return nil
}

// Duration is a time.Duration that unmarshals from a Go duration string.
type Duration time.Duration

// D returns the value as a time.Duration.
func (d Duration) D() time.Duration { return time.Duration(d) }

// UnmarshalText implements encoding.TextUnmarshaler.
func (d *Duration) UnmarshalText(text []byte) error {
	v, err := time.ParseDuration(string(text))
	if err != nil {
		return fmt.Errorf("invalid duration %q", string(text))
	}
	*d = Duration(v)
	return nil
}

// MarshalText implements encoding.TextMarshaler.
func (d Duration) MarshalText() ([]byte, error) {
	return []byte(time.Duration(d).String()), nil
}

// Core holds the [core] section.
type Core struct {
	Version     int    `toml:"version"`
	Shell       string `toml:"shell"`
	ShellLogin  bool   `toml:"shell-login"`
	Mode        string `toml:"mode"`
	Debug       bool   `toml:"debug"`
	ExpandAlias bool   `toml:"expand-alias"`
}

// UI holds the [ui] section.
type UI struct {
	Style          string `toml:"style"`
	GhostText      bool   `toml:"ghost-text"`
	HiddenFiles    bool   `toml:"hidden-files"`
	MaxSuggestions int    `toml:"max-suggestions"`
	MaxHeight      int    `toml:"max-height"`
	MaxWidth       int    `toml:"max-width"`
	NerdFonts      bool   `toml:"nerd-fonts"`
}

// Keybindings holds the [keybindings] section.
type Keybindings struct {
	ToggleMode   string `toml:"toggle-mode"`
	ToggleMenu   string `toml:"toggle-menu"`
	Select       string `toml:"select"`
	NavigateUp   string `toml:"navigate-up"`
	NavigateDown string `toml:"navigate-down"`
}

// Git holds the [git] section.
type Git struct {
	FilterActiveBranch  bool `toml:"filter-active-branch"`
	DeduplicateBranches bool `toml:"deduplicate-branches"`
}

// Updater holds the [updater] section.
type Updater struct {
	CheckOnStartup bool     `toml:"check-on-startup"`
	Channel        string   `toml:"channel"`
	CheckInterval  Duration `toml:"check-interval"`
}

// AIProvider describes one named OpenAI-compatible provider.
type AIProvider struct {
	InheritedFrom    string         `toml:"inherited_from,omitempty"`
	Endpoint         string         `toml:"endpoint,omitempty"`
	APIKey           string         `toml:"api_key,omitempty"`
	APIKeyEnv        string         `toml:"api_key_env,omitempty"`
	Model            string         `toml:"model,omitempty"`
	TimeoutMs        int            `toml:"timeout_ms,omitempty"`
	ExtraRequestBody map[string]any `toml:"extra_request_body,omitempty"`
}

// Key returns the resolved API key from the direct value or environment.
func (p AIProvider) Key() string {
	if p.APIKey != "" {
		return p.APIKey
	}
	if p.APIKeyEnv != "" {
		return os.Getenv(p.APIKeyEnv)
	}
	return ""
}

// SuggestOnEmpty holds the [ai.suggest_on_empty] section.
type SuggestOnEmpty struct {
	Enabled       bool `toml:"enabled"`
	DebounceMs    int  `toml:"debounce_ms"`
	MinIntervalMs int  `toml:"min_interval_ms"`
}

// AI holds the [ai] section.
type AI struct {
	Enabled        bool                  `toml:"enabled"`
	Provider       string                `toml:"provider"`
	DebounceMs     int                   `toml:"debounce_ms"`
	MinIntervalMs  int                   `toml:"min_interval_ms"`
	SuggestOnEmpty SuggestOnEmpty        `toml:"suggest_on_empty"`
	Providers      map[string]AIProvider `toml:"providers,omitempty"`
}

// Config is the fully resolved argmax configuration.
type Config struct {
	Core        Core        `toml:"core"`
	UI          UI          `toml:"ui"`
	Keybindings Keybindings `toml:"keybindings"`
	Git         Git         `toml:"git"`
	Updater     Updater     `toml:"updater"`
	AI          AI          `toml:"ai"`
}

// Default returns the compiled default configuration (PRD 9.13).
func Default() *Config {
	return &Config{
		Core: Core{
			Version:     CurrentVersion,
			Shell:       "",
			ShellLogin:  false,
			Mode:        "last",
			Debug:       false,
			ExpandAlias: true,
		},
		UI: UI{
			Style:          "modern",
			GhostText:      true,
			HiddenFiles:    false,
			MaxSuggestions: 100,
			MaxHeight:      15,
			MaxWidth:       0,
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
			CheckInterval:  Duration(24 * time.Hour),
		},
		AI: AI{
			Enabled:       false,
			Provider:      "",
			DebounceMs:    500,
			MinIntervalMs: 1000,
			SuggestOnEmpty: SuggestOnEmpty{
				Enabled:       false,
				DebounceMs:    800,
				MinIntervalMs: 5000,
			},
			Providers: map[string]AIProvider{},
		},
	}
}
