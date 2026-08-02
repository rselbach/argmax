package sources

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func aliasMap(aliases []Alias) map[string]string {
	m := make(map[string]string, len(aliases))
	for _, a := range aliases {
		m[a.Name] = a.Expansion
	}
	return m
}

func TestShellAliasesBash(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	content := `# comment
alias gs='git status'
alias ll="ls -la"
alias g=git
alias broken
alias novalue=
notalias x=1
`
	if err := os.WriteFile(filepath.Join(home, ".bashrc"), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	got := aliasMap(s.ShellAliases("bash"))
	want := map[string]string{"gs": "git status", "ll": "ls -la", "g": "git"}
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for k, v := range want {
		if got[k] != v {
			t.Fatalf("alias %q = %q, want %q", k, got[k], v)
		}
	}
}

func TestShellAliasesZshGlobal(t *testing.T) {
	zdot := t.TempDir()
	t.Setenv("ZDOTDIR", zdot)
	t.Setenv("HOME", t.TempDir())
	content := "alias g='git'\nalias -g G='| grep'\n"
	if err := os.WriteFile(filepath.Join(zdot, ".zshrc"), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	got := aliasMap(s.ShellAliases("zsh"))
	if got["g"] != "git" || got["G"] != "| grep" {
		t.Fatalf("got %v, want g=git and global G=| grep", got)
	}
}

func TestShellAliasesFish(t *testing.T) {
	home := t.TempDir()
	xdg := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("XDG_CONFIG_HOME", xdg)
	dir := filepath.Join(xdg, "fish")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	content := "alias g 'git'\nalias ll=\"ls -l\"\nalias gcm='git commit -m'\n"
	if err := os.WriteFile(filepath.Join(dir, "config.fish"), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	got := aliasMap(s.ShellAliases("fish"))
	want := map[string]string{"g": "git", "ll": "ls -l", "gcm": "git commit -m"}
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for k, v := range want {
		if got[k] != v {
			t.Fatalf("alias %q = %q, want %q", k, got[k], v)
		}
	}
}

func TestShellAliasesFishDefaultXDG(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("XDG_CONFIG_HOME", "")
	dir := filepath.Join(home, ".config", "fish")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "config.fish"), []byte("alias g 'git'\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	got := aliasMap(s.ShellAliases("fish"))
	if got["g"] != "git" {
		t.Fatalf("got %v", got)
	}
}

func TestShellAliasesMtimeReread(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	rc := filepath.Join(home, ".bashrc")
	if err := os.WriteFile(rc, []byte("alias one='1'\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	if got := aliasMap(s.ShellAliases("bash")); got["one"] != "1" || len(got) != 1 {
		t.Fatalf("first read: got %v", got)
	}
	// Rewrite with a new alias and a bumped mtime so the cache re-reads.
	if err := os.WriteFile(rc, []byte("alias one='1'\nalias two='2'\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	later := time.Now().Add(2 * time.Hour)
	if err := os.Chtimes(rc, later, later); err != nil {
		t.Fatal(err)
	}
	got := aliasMap(s.ShellAliases("bash"))
	if got["one"] != "1" || got["two"] != "2" {
		t.Fatalf("after rewrite: got %v, want one=1 and two=2", got)
	}
}

func TestShellAliasesMissingFiles(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("ZDOTDIR", "")
	t.Setenv("XDG_CONFIG_HOME", "")
	s := newTestSources()
	for _, sh := range []string{"bash", "zsh", "fish", "unknown"} {
		if got := s.ShellAliases(sh); got != nil {
			t.Fatalf("%s: got %v, want nil", sh, got)
		}
	}
}
