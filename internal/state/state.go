// Package state persists small cross-session product state such as the
// last selected suggestion mode and updater bookkeeping. Writes are atomic.
package state

import (
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/BurntSushi/toml"
	"golang.org/x/sys/unix"

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

var updateMu sync.Mutex

// Load reads the state file; a missing or malformed file yields defaults.
func Load() *State {
	return load()
}

func load() *State {
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

// Update atomically loads, modifies, and saves the latest state while
// holding both a process-local and inter-process lock.
func Update(fn func(*State)) error {
	return withUpdateLock(func() error {
		st := load()
		fn(st)
		return save(st)
	})
}

func withUpdateLock(fn func() error) error {
	updateMu.Lock()
	defer updateMu.Unlock()
	dir := paths.DataDir()
	if err := paths.EnsureDir(dir); err != nil {
		return fmt.Errorf("create state dir: %w", err)
	}
	lock, err := os.OpenFile(filepath.Join(dir, "state.lock"), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return fmt.Errorf("open state lock: %w", err)
	}
	defer func() { _ = lock.Close() }()
	if err := unix.Flock(int(lock.Fd()), unix.LOCK_EX); err != nil {
		return fmt.Errorf("lock state: %w", err)
	}
	defer func() { _ = unix.Flock(int(lock.Fd()), unix.LOCK_UN) }()
	return fn()
}

func save(st *State) error {
	dir := paths.DataDir()
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
