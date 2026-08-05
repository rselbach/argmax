package workspace

import (
	"os"
	"path/filepath"
	"testing"
)

func TestResolveFindsAncestorGitWorkspace(t *testing.T) {
	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, ".git"), 0o700); err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(root, "go.mod"), "module example.com/project\n")
	nested := filepath.Join(root, "cmd", "tool")
	if err := os.MkdirAll(nested, 0o700); err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(root, "cmd", "package.json"), "{}\n")

	info := Resolve(nested)
	if info.Root != root || info.GitDir != filepath.Join(root, ".git") {
		t.Errorf("resolved workspace = %+v, want root %q", info, root)
	}
	if !info.Has("git") || !info.Has("go") || !info.Has("node") {
		t.Errorf("signatures = %v, want git, go, and node", info.Signatures)
	}
}

func TestResolveSupportsGitDirFiles(t *testing.T) {
	parent := t.TempDir()
	metadata := filepath.Join(parent, "metadata", "worktree")
	if err := os.MkdirAll(metadata, 0o700); err != nil {
		t.Fatal(err)
	}
	root := filepath.Join(parent, "checkout")
	nested := filepath.Join(root, "nested")
	if err := os.MkdirAll(nested, 0o700); err != nil {
		t.Fatal(err)
	}
	relative, err := filepath.Rel(root, metadata)
	if err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(root, ".git"), "gitdir: "+relative+"\n")

	info := Resolve(nested)
	if info.Root != root || info.GitDir != metadata || !info.Has("git") {
		t.Errorf("gitdir file resolved as %+v", info)
	}
}

func TestResolveUsesNearestMarkerOutsideGit(t *testing.T) {
	parent := t.TempDir()
	writeFile(t, filepath.Join(parent, "go.mod"), "module parent\n")
	root := filepath.Join(parent, "nested")
	if err := os.Mkdir(root, 0o700); err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(root, "Cargo.toml"), "[package]\nname='nested'\n")
	info := Resolve(filepath.Join(root, "."))
	if info.Root != root || !info.Has("rust") || info.Has("go") {
		t.Errorf("nearest workspace resolved as %+v", info)
	}
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}
