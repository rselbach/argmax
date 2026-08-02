package config

import (
	"fmt"
	"os"
)

// currentConfigVersion is the schema version this build writes and
// understands.
const currentConfigVersion = 1

// migration upgrades a configuration from one schema version to the next.
// A destructive migration (one that drops or rewrites user data) triggers
// a file backup before it runs; migrations that also touch persistent
// state must back that up inside apply.
type migration struct {
	destructive bool
	apply       func(*Config) error
}

// migrations maps a source schema version to the migration that upgrades
// it one step. The chain is empty at schema version 1; future schema
// changes register here, keyed by core.version.
var migrations = map[int]migration{}

// migrate upgrades cfg from its declared schema version to the current
// one. Configurations from newer builds are rejected rather than
// misinterpreted.
func migrate(cfg *Config, path string) error {
	v := cfg.Core.Version
	switch {
	case v == currentConfigVersion:
		return nil
	case v < 1:
		return keyError("core.version", "positive supported schema version")
	case v > currentConfigVersion:
		return keyError("core.version", fmt.Sprintf(
			"schema version %d is newer than this build supports (%d); update argmax", v, currentConfigVersion))
	}
	for ; v < currentConfigVersion; v++ {
		m, ok := migrations[v]
		if !ok {
			return keyError("core.version", fmt.Sprintf("no migration path from schema version %d", v))
		}
		if m.destructive {
			if err := backupConfig(path, v); err != nil {
				return fmt.Errorf("back up configuration before migration from version %d: %w", v, err)
			}
		}
		if err := m.apply(cfg); err != nil {
			return fmt.Errorf("migrate configuration from version %d: %w", v, err)
		}
		cfg.Core.Version = v + 1
	}
	return nil
}

// backupConfig copies the configuration file aside before a destructive
// migration; an existing backup for the same version is preserved.
func backupConfig(path string, fromVersion int) error {
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	dst := fmt.Sprintf("%s.backup-v%d", path, fromVersion)
	if _, err := os.Stat(dst); err == nil {
		return nil
	}
	return os.WriteFile(dst, data, 0o600)
}
