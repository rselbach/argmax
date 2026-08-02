package config

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"time"

	"github.com/pelletier/go-toml/v2"
)

// State is the persistent session state (PRD 9.15).
type State struct {
	LastMode string `toml:"last-mode"`
	Updater  struct {
		LastCheckTime time.Time `toml:"last-check-time"`
		SeenVersion   string    `toml:"seen-version"`
	} `toml:"updater"`
}

// LoadState reads the state file. A missing file yields a zero state.
func LoadState(path string) (*State, error) {
	st := &State{LastMode: "spec"}
	st.Updater.LastCheckTime = time.Unix(0, 0).UTC()
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return st, nil
		}
		return nil, fmt.Errorf("read state: %w", err)
	}
	if err := toml.Unmarshal(data, st); err != nil {
		return nil, fmt.Errorf("parse state %s: %w", path, err)
	}
	return st, nil
}

// SaveState atomically writes the state file with private permissions.
func SaveState(path string, st *State) error {
	data, err := toml.Marshal(st)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".state-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer func() { _ = os.Remove(tmpName) }()
	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Chmod(0o600); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}
