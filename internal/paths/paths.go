// Package paths resolves the XDG-aware filesystem locations used by argmax
// for configuration, persistent state, the learning database, caches, logs,
// and crash reports.
package paths

import (
	"os"
	"path/filepath"
)

// Config returns the path of the user configuration file.
func Config() string {
	return filepath.Join(configDir(), "config.toml")
}

// State returns the path of the persistent state file.
func State() string {
	return filepath.Join(DataDir(), "state.toml")
}

// Database returns the path of the learning database.
func Database() string {
	return filepath.Join(DataDir(), "history.db")
}

// Log returns the path of the main diagnostic log.
func Log() string {
	return filepath.Join(CacheDir(), "argmax.log")
}

// CrashDir returns the directory holding crash reports.
func CrashDir() string {
	return filepath.Join(CacheDir(), "crashes")
}

// DataDir returns the persistent state directory.
func DataDir() string {
	if dir := os.Getenv("XDG_DATA_HOME"); dir != "" {
		return filepath.Join(dir, "argmax")
	}
	return filepath.Join(home(), ".local", "share", "argmax")
}

// CacheDir returns the disposable cache directory.
func CacheDir() string {
	if dir := os.Getenv("XDG_CACHE_HOME"); dir != "" {
		return filepath.Join(dir, "argmax")
	}
	dir, err := os.UserCacheDir()
	if err != nil {
		return filepath.Join(home(), ".cache", "argmax")
	}
	return filepath.Join(dir, "argmax")
}

func configDir() string {
	if dir := os.Getenv("XDG_CONFIG_HOME"); dir != "" {
		return filepath.Join(dir, "argmax")
	}
	dir, err := os.UserConfigDir()
	if err != nil {
		return filepath.Join(home(), ".config", "argmax")
	}
	return filepath.Join(dir, "argmax")
}

// EnsureDir creates dir with mode 0700 when absent.
func EnsureDir(dir string) error {
	return os.MkdirAll(dir, 0o700)
}

func home() string {
	h, err := os.UserHomeDir()
	if err != nil {
		return "."
	}
	return h
}
