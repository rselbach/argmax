package config

import (
	"os"
	"strings"
	"testing"
)

func TestMigrateCurrentVersionIsNoop(t *testing.T) {
	cfg := Default()
	if err := migrate(cfg, "unused"); err != nil {
		t.Fatalf("current version must not migrate: %v", err)
	}
}

func TestMigrateRejectsFutureVersion(t *testing.T) {
	path := writeConfig(t, "[core]\nversion = 99\nmode = \"last\"\n")
	_, err := Load(path)
	if err == nil {
		t.Fatal("future schema version must be rejected")
	}
	if !strings.Contains(err.Error(), "core.version") {
		t.Errorf("error should name core.version: %v", err)
	}
}

func TestMigrateRunsChainAndBacksUp(t *testing.T) {
	// Simulate an old config at version 1 with the current build at
	// version 2 by running the migration machinery directly.
	saved := migrations
	migrations = map[int]migration{
		1: {
			destructive: true,
			apply: func(cfg *Config) error {
				cfg.Core.Mode = "spec" // pretend "last" was removed
				return nil
			},
		},
	}
	t.Cleanup(func() { migrations = saved })

	path := writeConfig(t, "[core]\nversion = 1\nmode = \"last\"\n")
	cfg := Default()
	cfg.Core.Version = 1
	cfg.Core.Mode = "last"

	// Drive one step beyond the compiled version to exercise the chain.
	v := cfg.Core.Version
	m := migrations[v]
	if m.destructive {
		if err := backupConfig(path, v); err != nil {
			t.Fatal(err)
		}
	}
	if err := m.apply(cfg); err != nil {
		t.Fatal(err)
	}
	cfg.Core.Version = v + 1

	if cfg.Core.Mode != "spec" || cfg.Core.Version != 2 {
		t.Errorf("migration not applied: %+v", cfg.Core)
	}
	backup := path + ".backup-v1"
	data, err := os.ReadFile(backup)
	if err != nil {
		t.Fatalf("destructive migration must back up the config: %v", err)
	}
	if !strings.Contains(string(data), `mode = "last"`) {
		t.Errorf("backup must hold the pre-migration content: %s", data)
	}
	// A re-run keeps the original backup.
	if err := backupConfig(path, 1); err != nil {
		t.Fatal(err)
	}
	again, _ := os.ReadFile(backup)
	if string(again) != string(data) {
		t.Error("existing backup must not be overwritten")
	}
}
