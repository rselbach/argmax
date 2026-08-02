package rank

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func writeFixture(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write fixture: %v", err)
	}
}

func TestDetectorSignatures(t *testing.T) {
	dir := t.TempDir()
	for _, f := range []string{
		"package.json", "go.mod", "Cargo.toml", "pyproject.toml",
		"Dockerfile", "Makefile", "justfile", "Taskfile.yml",
		"pom.xml", "build.gradle", "CMakeLists.txt", "Chart.yaml",
	} {
		writeFixture(t, filepath.Join(dir, f), "")
	}
	ws := NewDetector().Detect(dir)
	for _, sig := range []string{
		SigNode, SigGo, SigRust, SigPython, SigDocker, SigMake,
		SigJust, SigKubernetes, SigTaskfile, SigMaven, SigGradle, SigCMake,
	} {
		if !ws.Has(sig) {
			t.Errorf("signature %q not detected", sig)
		}
	}
	if ws.InGit {
		t.Error("InGit true without .git")
	}
}

func TestDetectorSignatureVariants(t *testing.T) {
	dir := t.TempDir()
	writeFixture(t, filepath.Join(dir, "requirements.txt"), "")
	writeFixture(t, filepath.Join(dir, "compose.yaml"), "")
	writeFixture(t, filepath.Join(dir, "Justfile"), "")
	writeFixture(t, filepath.Join(dir, "settings.gradle"), "")
	writeFixture(t, filepath.Join(dir, "Taskfile.yaml"), "")
	if err := os.MkdirAll(filepath.Join(dir, "k8s"), 0o755); err != nil {
		t.Fatal(err)
	}
	ws := NewDetector().Detect(dir)
	for _, sig := range []string{SigPython, SigDocker, SigJust, SigGradle, SigTaskfile, SigKubernetes} {
		if !ws.Has(sig) {
			t.Errorf("variant signature %q not detected", sig)
		}
	}
}

func TestDetectorGitBranch(t *testing.T) {
	dir := t.TempDir()
	writeFixture(t, filepath.Join(dir, ".git", "HEAD"), "ref: refs/heads/feature/cool-thing\n")
	ws := NewDetector().Detect(dir)
	if !ws.InGit || !ws.Has(SigGit) {
		t.Fatal("git workspace not detected")
	}
	if ws.GitBranch != "feature/cool-thing" {
		t.Fatalf("branch: got %q, want %q", ws.GitBranch, "feature/cool-thing")
	}
}

func TestDetectorGitDetachedHEAD(t *testing.T) {
	dir := t.TempDir()
	writeFixture(t, filepath.Join(dir, ".git", "HEAD"), "9fceb02d0ae598e95dc970b74767f19372d61af8\n")
	ws := NewDetector().Detect(dir)
	if !ws.InGit {
		t.Fatal("git workspace not detected")
	}
	if ws.GitBranch != "" {
		t.Fatalf("detached HEAD: got branch %q, want \"\"", ws.GitBranch)
	}
}

func TestDetectorGitFileWorktree(t *testing.T) {
	dir := t.TempDir()
	realGitDir := filepath.Join(dir, "real-gitdir")
	writeFixture(t, filepath.Join(realGitDir, "HEAD"), "ref: refs/heads/worktree-branch\n")
	writeFixture(t, filepath.Join(dir, ".git"), "gitdir: "+realGitDir+"\n")
	ws := NewDetector().Detect(dir)
	if !ws.InGit {
		t.Fatal("worktree not detected")
	}
	if ws.GitBranch != "worktree-branch" {
		t.Fatalf("branch: got %q, want %q", ws.GitBranch, "worktree-branch")
	}
}

func TestDetectorWalksParents(t *testing.T) {
	root := t.TempDir()
	writeFixture(t, filepath.Join(root, "go.mod"), "module example.com/x\n")
	writeFixture(t, filepath.Join(root, ".git", "HEAD"), "ref: refs/heads/main\n")
	nested := filepath.Join(root, "a", "b", "c")
	if err := os.MkdirAll(nested, 0o755); err != nil {
		t.Fatal(err)
	}
	ws := NewDetector().Detect(nested)
	if !ws.Has(SigGo) || !ws.InGit || ws.GitBranch != "main" {
		t.Fatalf("parent walk: got %+v", ws)
	}
	if ws.Dir != nested {
		t.Fatalf("Dir: got %q, want %q", ws.Dir, nested)
	}
}

func TestDetectorWalkBound(t *testing.T) {
	root := t.TempDir()
	writeFixture(t, filepath.Join(root, "package.json"), "{}")
	// Six levels down: the walk stops after five parents without a .git, so
	// the root's package.json is out of reach.
	deep := filepath.Join(root, "d1", "d2", "d3", "d4", "d5", "d6")
	if err := os.MkdirAll(deep, 0o755); err != nil {
		t.Fatal(err)
	}
	if ws := NewDetector().Detect(deep); ws.Has(SigNode) {
		t.Fatal("signature detected beyond the walk bound")
	}
	// Five levels down is still within reach.
	near := filepath.Join(root, "d1", "d2", "d3", "d4", "d5")
	if ws := NewDetector().Detect(near); !ws.Has(SigNode) {
		t.Fatal("signature within the walk bound not detected")
	}
}

func TestDetectorCacheReuse(t *testing.T) {
	dir := t.TempDir()
	writeFixture(t, filepath.Join(dir, "go.mod"), "module example.com/x\n")
	d := NewDetector()
	w1 := d.Detect(dir)
	w2 := d.Detect(dir)
	if w1 != w2 {
		t.Fatal("unchanged directory: expected the cached *Workspace")
	}
}

func TestDetectorCacheInvalidatedByDirMtime(t *testing.T) {
	dir := t.TempDir()
	d := NewDetector()
	w1 := d.Detect(dir)
	if ws := d.Detect(dir); ws != w1 {
		t.Fatal("premature invalidation")
	}
	future := time.Now().Add(2 * time.Hour)
	if err := os.Chtimes(dir, future, future); err != nil {
		t.Fatalf("Chtimes: %v", err)
	}
	w2 := d.Detect(dir)
	if w2 == w1 {
		t.Fatal("directory mtime change did not invalidate the cache")
	}
	// Newly appeared signatures are picked up after invalidation.
	writeFixture(t, filepath.Join(dir, "package.json"), "{}")
	if w3 := d.Detect(dir); !w3.Has(SigNode) {
		t.Fatal("new package.json not detected after invalidation")
	}
}

func TestDetectorCacheInvalidatedByHEADChange(t *testing.T) {
	dir := t.TempDir()
	head := filepath.Join(dir, ".git", "HEAD")
	writeFixture(t, head, "ref: refs/heads/main\n")
	d := NewDetector()
	w1 := d.Detect(dir)
	if w1.GitBranch != "main" {
		t.Fatalf("branch: got %q, want main", w1.GitBranch)
	}
	// Switching branches rewrites HEAD; the mtime change must invalidate.
	time.Sleep(10 * time.Millisecond) // keep mtimes distinct on coarse filesystems
	writeFixture(t, head, "ref: refs/heads/topic\n")
	w2 := d.Detect(dir)
	if w2 == w1 {
		t.Fatal("HEAD change did not invalidate the cache")
	}
	if w2.GitBranch != "topic" {
		t.Fatalf("branch: got %q, want topic", w2.GitBranch)
	}
}
