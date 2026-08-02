package sources

import (
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func toolAliasMap(aliases []ToolAlias) map[string]ToolAlias {
	m := make(map[string]ToolAlias, len(aliases))
	for _, a := range aliases {
		m[a.Name] = a
	}
	return m
}

func TestGitAliases(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("XDG_CONFIG_HOME", "")
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")

	repo := t.TempDir()
	run := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", args...)
		cmd.Dir = repo
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	run("init", "-q", "-b", "main")
	run("config", "alias.st", "status")
	run("config", "alias.sh", "!echo hi")
	run("config", "--global", "alias.co", "checkout")

	s := newTestSources()
	got := toolAliasMap(s.GitAliases(repo))

	st, ok := got["st"]
	if !ok || st.Expansion != "status" || st.Scope != ScopeLocal {
		t.Fatalf("st = %+v, want expansion status scope %d", st, ScopeLocal)
	}
	co, ok := got["co"]
	if !ok || co.Expansion != "checkout" || co.Scope != ScopeGlobal {
		t.Fatalf("co = %+v, want expansion checkout scope %d", co, ScopeGlobal)
	}
	sh, ok := got["sh"]
	if !ok || !sh.Shell || sh.Expansion != "!echo hi" {
		t.Fatalf("sh = %+v, want Shell=true expansion !echo hi", sh)
	}
	for name, a := range got {
		if a.Root != "git" {
			t.Fatalf("alias %q root = %q, want git", name, a.Root)
		}
	}
}

func TestGitAliasesINIFallback(t *testing.T) {
	// Exercise the INI parser directly with fixture files.
	dir := t.TempDir()
	local := filepath.Join(dir, "config")
	content := `[core]
	repositoryformatversion = 0
[alias]
	st = status
	co = checkout
	sh = !echo hi
`
	if err := os.WriteFile(local, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	got := toolAliasMap(parseGitConfigAliases(local, ScopeLocal))
	if got["st"].Expansion != "status" || got["st"].Scope != ScopeLocal {
		t.Fatalf("st = %+v", got["st"])
	}
	if got["co"].Expansion != "checkout" {
		t.Fatalf("co = %+v", got["co"])
	}
	if !got["sh"].Shell {
		t.Fatalf("sh = %+v, want Shell=true", got["sh"])
	}
}

func TestCargoAliases(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cargoHome := t.TempDir()
	t.Setenv("CARGO_HOME", cargoHome)
	global := `[alias]
g = "generate"
`
	if err := os.WriteFile(filepath.Join(cargoHome, "config.toml"), []byte(global), 0o644); err != nil {
		t.Fatal(err)
	}

	proj := t.TempDir()
	localDir := filepath.Join(proj, ".cargo")
	if err := os.MkdirAll(localDir, 0o755); err != nil {
		t.Fatal(err)
	}
	local := `[build]
jobs = 4

[alias]
b = "build"
rr = ["run", "--release"]
bad

[other]
x = "y"
`
	if err := os.WriteFile(filepath.Join(localDir, "config.toml"), []byte(local), 0o644); err != nil {
		t.Fatal(err)
	}

	s := newTestSources()
	got := toolAliasMap(s.CargoAliases(proj))

	if got["b"].Expansion != "build" || got["b"].Scope != ScopeLocal {
		t.Fatalf("b = %+v, want expansion build scope %d", got["b"], ScopeLocal)
	}
	if got["rr"].Expansion != "run --release" || got["rr"].Scope != ScopeLocal {
		t.Fatalf("rr = %+v, want expansion 'run --release' scope %d", got["rr"], ScopeLocal)
	}
	if got["g"].Expansion != "generate" || got["g"].Scope != ScopeGlobal {
		t.Fatalf("g = %+v, want expansion generate scope %d", got["g"], ScopeGlobal)
	}
	if _, ok := got["x"]; ok {
		t.Fatalf("alias from [other] section leaked: %+v", got["x"])
	}
	for name, a := range got {
		if a.Root != "cargo" {
			t.Fatalf("alias %q root = %q, want cargo", name, a.Root)
		}
	}
}
