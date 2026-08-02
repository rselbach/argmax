// Package state persists small cross-session product state such as the
// last selected suggestion mode and updater bookkeeping. Writes are atomic.
package state

import (
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/BurntSushi/toml"

	"github.com/rselbach/argmax/internal/paths"
)

// State is the persisted product state.
type State struct {
	LastMode string  `toml:"last-mode"`
	Updater  Updater `toml:"updater"`
}

// Updater tracks release-check bookkeeping across sessions.
type Updater struct {
	LastCheckTime time.Time `toml:"last-check-time"`
	SeenVersion   string    `toml:"seen-version"`
	// NotifiedVersion records the last version announced in a session so
	// each newly discovered version notifies exactly once.
	NotifiedVersion string `toml:"notified-version,omitempty"`
}

// Load reads the state file; a missing or malformed file yields defaults.
func Load() *State {
	st := &State{LastMode: "spec"}
	data, err := os.ReadFile(paths.State())
	if err != nil {
		return st
	}
	if err := toml.Unmarshal(data, st); err != nil {
		return &State{LastMode: "spec"}
	}
	if st.LastMode != "spec" && st.LastMode != "history" {
		st.LastMode = "spec"
	}
	return st
}

// Save writes the state atomically with mode 0600.
func Save(st *State) error {
	dir := paths.DataDir()
	if err := paths.EnsureDir(dir); err != nil {
		return fmt.Errorf("create state dir: %w", err)
	}
	tmp, err := os.CreateTemp(dir, "state-*.toml")
	if err != nil {
		return fmt.Errorf("create temp state: %w", err)
	}
	defer func() { _ = os.Remove(tmp.Name()) }()
	if err := tmp.Chmod(0o600); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("chmod state: %w", err)
	}
	if err := toml.NewEncoder(tmp).Encode(st); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("encode state: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("close state: %w", err)
	}
	if err := os.Rename(tmp.Name(), filepath.Join(dir, "state.toml")); err != nil {
		return fmt.Errorf("replace state: %w", err)
	}
	return nil
}
