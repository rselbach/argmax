package shell

import (
	"path/filepath"
	"reflect"
	"testing"
)

func TestHistoryPathHonorsHistfile(t *testing.T) {
	getenv := func(key string) string {
		if key == "HISTFILE" {
			return "/custom/history"
		}
		return ""
	}
	for _, s := range []Shell{Bash, Zsh, Fish} {
		if got := s.HistoryPath(getenv); got != "/custom/history" {
			t.Fatalf("%s.HistoryPath with HISTFILE = %q, want /custom/history", s, got)
		}
	}
}

func TestHistoryPathDefaults(t *testing.T) {
	env := func(pairs map[string]string) func(string) string {
		return func(key string) string { return pairs[key] }
	}
	cases := []struct {
		name  string
		shell Shell
		env   map[string]string
		want  string
	}{
		{"bash", Bash, map[string]string{"HOME": "/h"}, "/h/.bash_history"},
		{"zsh home", Zsh, map[string]string{"HOME": "/h"}, "/h/.zsh_history"},
		{"zsh zdotdir", Zsh, map[string]string{"HOME": "/h", "ZDOTDIR": "/zd"}, "/zd/.zsh_history"},
		{"fish default", Fish, map[string]string{"HOME": "/h"}, "/h/.local/share/fish/fish_history"},
		{"fish xdg", Fish, map[string]string{"HOME": "/h", "XDG_DATA_HOME": "/xd"}, "/xd/fish/fish_history"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.shell.HistoryPath(env(tc.env)); got != filepath.FromSlash(tc.want) {
				t.Fatalf("got %q, want %q", got, tc.want)
			}
		})
	}
}

func TestHistoryPathNilGetenv(t *testing.T) {
	t.Setenv("HISTFILE", "/from/env")
	if got := Bash.HistoryPath(nil); got != "/from/env" {
		t.Fatalf("nil getenv should use the process environment, got %q", got)
	}
}

func TestAliasFiles(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ZDOTDIR", "")
	t.Setenv("XDG_CONFIG_HOME", "")

	wantBash := []string{
		filepath.Join(home, ".bashrc"),
		filepath.Join(home, ".bash_profile"),
		filepath.Join(home, ".bash_aliases"),
	}
	if got := Bash.AliasFiles(); !reflect.DeepEqual(got, wantBash) {
		t.Fatalf("bash alias files = %v, want %v", got, wantBash)
	}

	wantZsh := []string{
		filepath.Join(home, ".zshrc"),
		filepath.Join(home, ".zprofile"),
		filepath.Join(home, ".zshenv"),
	}
	if got := Zsh.AliasFiles(); !reflect.DeepEqual(got, wantZsh) {
		t.Fatalf("zsh alias files = %v, want %v", got, wantZsh)
	}

	wantFish := []string{filepath.Join(home, ".config", "fish", "config.fish")}
	if got := Fish.AliasFiles(); !reflect.DeepEqual(got, wantFish) {
		t.Fatalf("fish alias files = %v, want %v", got, wantFish)
	}
}

func TestAliasFilesZdotdirAndXDG(t *testing.T) {
	home := t.TempDir()
	zd := t.TempDir()
	xdg := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ZDOTDIR", zd)
	t.Setenv("XDG_CONFIG_HOME", xdg)

	wantZsh := []string{
		filepath.Join(zd, ".zshrc"),
		filepath.Join(zd, ".zprofile"),
		filepath.Join(zd, ".zshenv"),
	}
	if got := Zsh.AliasFiles(); !reflect.DeepEqual(got, wantZsh) {
		t.Fatalf("zsh alias files with ZDOTDIR = %v, want %v", got, wantZsh)
	}

	wantFish := []string{filepath.Join(xdg, "fish", "config.fish")}
	if got := Fish.AliasFiles(); !reflect.DeepEqual(got, wantFish) {
		t.Fatalf("fish alias files with XDG_CONFIG_HOME = %v, want %v", got, wantFish)
	}
}

func TestRCFile(t *testing.T) {
	home := t.TempDir()
	zd := t.TempDir()
	xdg := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ZDOTDIR", zd)
	t.Setenv("XDG_CONFIG_HOME", xdg)

	if got, want := Bash.RCFile(), filepath.Join(home, ".bashrc"); got != want {
		t.Fatalf("bash rc = %q, want %q", got, want)
	}
	if got, want := Zsh.RCFile(), filepath.Join(zd, ".zshrc"); got != want {
		t.Fatalf("zsh rc = %q, want %q", got, want)
	}
	if got, want := Fish.RCFile(), filepath.Join(xdg, "fish", "config.fish"); got != want {
		t.Fatalf("fish rc = %q, want %q", got, want)
	}
	if got := Shell("ksh").RCFile(); got != "" {
		t.Fatalf("unsupported shell rc = %q, want empty", got)
	}
}
