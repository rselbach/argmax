package sources

import (
	"os"
	"path/filepath"
	"testing"
)

func TestScanPATHAndExecutables(t *testing.T) {
	bin := t.TempDir()
	writeFile(t, filepath.Join(bin, "GitX"))
	writeFile(t, filepath.Join(bin, "git-y"))
	writeFile(t, filepath.Join(bin, "notexec"))
	writeFile(t, filepath.Join(bin, "subdir", "nested"))
	for _, f := range []string{"GitX", "git-y"} {
		if err := os.Chmod(filepath.Join(bin, f), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("PATH", bin)

	s := newTestSources()
	s.ScanPATH()
	got := collect(s.Executables(""))
	if len(got) != 2 {
		t.Fatalf("got %v, want [GitX git-y]", got)
	}
	for _, sug := range s.Executables("") {
		if sug.Description != "system command" || sug.Icon != "system" ||
			sug.Confidence != 50 || sug.Source != "system" {
			t.Fatalf("metadata = %+v", sug)
		}
	}
	// Prefix match is case-insensitive.
	got = collect(s.Executables("git"))
	if len(got) != 2 {
		t.Fatalf("prefix git: got %v", got)
	}
	got = collect(s.Executables("git-"))
	if len(got) != 1 || got[0] != "git-y" {
		t.Fatalf("prefix git-: got %v", got)
	}
	got = collect(s.Executables("zzz"))
	if len(got) != 0 {
		t.Fatalf("prefix zzz: got %v", got)
	}
}

func TestExecutablesLazyScan(t *testing.T) {
	bin := t.TempDir()
	writeFile(t, filepath.Join(bin, "lazytool"))
	if err := os.Chmod(filepath.Join(bin, "lazytool"), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin)
	s := newTestSources()
	// No explicit ScanPATH: first Executables call scans.
	got := collect(s.Executables("lazy"))
	if len(got) != 1 || got[0] != "lazytool" {
		t.Fatalf("got %v", got)
	}
}
