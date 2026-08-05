package main

import (
	"path/filepath"
	"testing"
)

func TestValidateOutputDir(t *testing.T) {
	root := t.TempDir()
	expected := filepath.Join(root, "internal", "catalog", "data")
	got, err := validateOutputDir(expected, root)
	if err != nil {
		t.Fatal(err)
	}
	if got != expected {
		t.Errorf("validated output = %q, want %q", got, expected)
	}
}

func TestValidateOutputDirRejectsDestructivePaths(t *testing.T) {
	root := t.TempDir()
	for name, path := range map[string]string{
		"dot":             ".",
		"filesystem root": filepath.VolumeName(root) + string(filepath.Separator),
		"repository root": root,
		"outside":         filepath.Join(filepath.Dir(root), "elsewhere"),
		"similar suffix":  filepath.Join(root, "internal", "catalog", "data-backup"),
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := validateOutputDir(path, root); err == nil {
				t.Errorf("validateOutputDir(%q) succeeded", path)
			}
		})
	}
}
