package ai

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"unicode/utf8"
)

func requireGit(t *testing.T) {
	t.Helper()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}
}

func runGit(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}

// initGitRepo creates a fixture repository on branch "main" with one commit.
func initGitRepo(t *testing.T) string {
	t.Helper()
	requireGit(t)
	dir := t.TempDir()
	runGit(t, dir, "init", "-q")
	// Deterministic initial branch name on any git version.
	runGit(t, dir, "symbolic-ref", "HEAD", "refs/heads/main")
	if err := os.WriteFile(filepath.Join(dir, "file.txt"), []byte("hello\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	runGit(t, dir, "add", "file.txt")
	runGit(t, dir, "-c", "user.email=test@example.com", "-c", "user.name=Test", "commit", "-qm", "initial commit")
	return dir
}

func TestSelectProvider(t *testing.T) {
	tests := []struct {
		buffer string
		want   string
	}{
		{"", providerUniversal},
		{"git log", providerUniversal},
		{"ls -la", providerUniversal},

		{"docker exec ", providerDocker},
		{"docker logs -f ", providerDocker},
		{"docker stop ", providerDocker},
		{"docker restart ", providerDocker},
		{"docker rm ", providerDocker},
		{"docker ps", providerUniversal},

		{"docker compose exec ", providerCompose},
		{"docker compose logs ", providerCompose},
		{"docker-compose exec ", providerCompose},
		{"docker-compose logs ", providerCompose},
		{"docker compose ps", providerUniversal},

		{"kubectl exec ", providerPods},
		{"kubectl logs ", providerPods},
		{"kubectl describe pod ", providerPods},
		{"kubectl delete pod ", providerPods},
		{"kubectl get pods", providerUniversal},

		{"git checkout ", providerGitBranches},
		{"git switch ", providerGitBranches},
		{"git merge ", providerGitBranches},
		{"git rebase ", providerGitBranches},
		{"git branch -d ", providerGitBranches},
		{"git branch -D ", providerGitBranches},
		{"git branch", providerUniversal},

		{"kill ", providerProcesses},
		{"kill -9 ", providerProcesses},

		{"systemctl restart ", providerSystemd},
		{"systemctl stop ", providerSystemd},
		{"systemctl status ", providerSystemd},
		{"systemctl start ", providerUniversal},
	}
	for _, tc := range tests {
		if got := selectProvider(strings.Fields(tc.buffer)); got != tc.want {
			t.Errorf("selectProvider(%q) = %q; want %q", tc.buffer, got, tc.want)
		}
	}
}

func TestGatherContextGitRepo(t *testing.T) {
	dir := initGitRepo(t)
	resetContextCache()

	// Ecosystem + scripts.
	if err := os.WriteFile(filepath.Join(dir, "package.json"),
		[]byte(`{"name":"x","scripts":{"dev":"vite","build":"tsc"}}`), 0o644); err != nil {
		t.Fatal(err)
	}
	// Second branch for the branch list.
	runGit(t, dir, "branch", "feature")
	// 5KB staged diff, must be capped to 1500 chars.
	if err := os.WriteFile(filepath.Join(dir, "big.txt"), []byte(strings.Repeat("line\n", 1000)), 0o644); err != nil {
		t.Fatal(err)
	}
	runGit(t, dir, "add", "big.txt")
	// Unstaged modification for status.
	if err := os.WriteFile(filepath.Join(dir, "file.txt"), []byte("changed\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	c := GatherContext(context.Background(), Request{Buffer: "", CWD: dir})

	if c.GitBranch != "main" {
		t.Errorf("GitBranch = %q; want main", c.GitBranch)
	}
	if c.MergeState != "" {
		t.Errorf("MergeState = %q; want empty", c.MergeState)
	}
	if len(c.GitBranches) == 0 || len(c.GitBranches) > 10 {
		t.Errorf("GitBranches = %v; want 1-10 entries", c.GitBranches)
	}
	if !contains(c.GitBranches, "main") || !contains(c.GitBranches, "feature") {
		t.Errorf("GitBranches = %v; want main and feature", c.GitBranches)
	}
	if c.GitStatus == "" || len(c.GitStatus) > 1000 {
		t.Errorf("GitStatus length = %d; want 1-1000", len(c.GitStatus))
	}
	if c.StagedDiff == "" {
		t.Error("StagedDiff is empty; want staged 5KB diff")
	}
	if len(c.StagedDiff) > 1500 {
		t.Errorf("StagedDiff length = %d; want ≤1500", len(c.StagedDiff))
	}
	if !utf8.ValidString(c.StagedDiff) {
		t.Error("StagedDiff is not valid UTF-8 after capping")
	}
	if len(c.RecentCommits) != 1 || c.RecentCommits[0] != "initial commit" {
		t.Errorf("RecentCommits = %v; want [initial commit]", c.RecentCommits)
	}
	if !contains(c.Ecosystems, "node") {
		t.Errorf("Ecosystems = %v; want node", c.Ecosystems)
	}
	if !contains(c.Scripts, "dev: vite") || !contains(c.Scripts, "build: tsc") {
		t.Errorf("Scripts = %v; want package.json scripts", c.Scripts)
	}
	if len(c.DirEntries) > 30 {
		t.Errorf("DirEntries = %d entries; want ≤30", len(c.DirEntries))
	}
	if !contains(c.DirEntries, "file.txt") || !contains(c.DirEntries, "package.json") {
		t.Errorf("DirEntries = %v; want visible files", c.DirEntries)
	}
	// Only visible names may be disclosed (PRD 11.2).
	if contains(c.DirEntries, ".git") || contains(c.DirEntries, ".git/") {
		t.Errorf("DirEntries = %v; must not contain .git", c.DirEntries)
	}
	if c.Specialized != "" || c.Help != "" {
		t.Errorf("Specialized/Help = %q/%q; want empty for bare buffer", c.Specialized, c.Help)
	}
}

func TestGatherContextMergeState(t *testing.T) {
	dir := initGitRepo(t)

	writeFile(t, filepath.Join(dir, ".git", "MERGE_HEAD"), "abc123\n")
	resetContextCache()
	if c := GatherContext(context.Background(), Request{Buffer: "a", CWD: dir}); c.MergeState != "merging" {
		t.Errorf("MergeState = %q; want merging", c.MergeState)
	}

	if err := os.Remove(filepath.Join(dir, ".git", "MERGE_HEAD")); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(dir, ".git", "rebase-merge"), 0o755); err != nil {
		t.Fatal(err)
	}
	resetContextCache()
	if c := GatherContext(context.Background(), Request{Buffer: "a", CWD: dir}); c.MergeState != "rebasing" {
		t.Errorf("MergeState = %q; want rebasing", c.MergeState)
	}
}

func TestGatherContextSpecializedGitBranches(t *testing.T) {
	dir := initGitRepo(t)
	runGit(t, dir, "branch", "feature")
	resetContextCache()

	c := GatherContext(context.Background(), Request{Buffer: "git checkout ", CWD: dir})
	if !strings.Contains(c.Specialized, "main") || !strings.Contains(c.Specialized, "feature") {
		t.Errorf("Specialized = %q; want local branches", c.Specialized)
	}
	if len(c.Specialized) > 1000 {
		t.Errorf("Specialized length = %d; want ≤1000", len(c.Specialized))
	}
}

func TestGatherContextCache(t *testing.T) {
	dir := t.TempDir() // not a repository: all git probes fail fast
	writeFile(t, filepath.Join(dir, "before.txt"), "x")
	resetContextCache()

	req := Request{Buffer: "", CWD: dir}
	first := GatherContext(context.Background(), req)
	if !contains(first.DirEntries, "before.txt") {
		t.Fatalf("first gather DirEntries = %v", first.DirEntries)
	}

	// A change within the 4s window is invisible: the cache is served.
	writeFile(t, filepath.Join(dir, "after.txt"), "x")
	second := GatherContext(context.Background(), req)
	if contains(second.DirEntries, "after.txt") {
		t.Fatal("second gather saw a post-cache file; want cached result (AI-012)")
	}

	// After invalidation the fresh state is gathered.
	resetContextCache()
	third := GatherContext(context.Background(), req)
	if !contains(third.DirEntries, "after.txt") {
		t.Fatal("third gather did not see the new file after cache reset")
	}
}

func TestGatherHelpWithStubTool(t *testing.T) {
	bin := t.TempDir()
	stub := "#!/bin/sh\n" +
		"if [ \"$1\" = \"--help\" ]; then\n" +
		"  echo 'FAKE HELP " + strings.Repeat("x", 700) + "'\n" +
		"  exit 0\n" +
		"fi\n" +
		"exit 1\n"
	writeFile(t, filepath.Join(bin, "git"), stub)
	if err := os.Chmod(filepath.Join(bin, "git"), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin+string(os.PathListSeparator)+os.Getenv("PATH"))
	resetContextCache()

	// Bare root in a non-repo directory: universal provider gathers help;
	// the stub makes every git probe fail so git fields stay empty.
	c := GatherContext(context.Background(), Request{Buffer: "git ", CWD: t.TempDir()})
	if !strings.HasPrefix(c.Help, "FAKE HELP") {
		t.Errorf("Help = %q; want stub help output", c.Help)
	}
	if len(c.Help) > 600 {
		t.Errorf("Help length = %d; want ≤600", len(c.Help))
	}
	if c.GitBranch != "" {
		t.Errorf("GitBranch = %q; want empty outside a repository", c.GitBranch)
	}
}

func TestGatherHelpSkippedForLongBuffers(t *testing.T) {
	resetContextCache()
	// Three tokens: too specific for cheap help gathering.
	c := GatherContext(context.Background(), Request{Buffer: "npm run build", CWD: t.TempDir()})
	if c.Help != "" {
		t.Errorf("Help = %q; want empty for >2 tokens", c.Help)
	}
}

func TestHelpAllowed(t *testing.T) {
	for _, tc := range []struct {
		root string
		want bool
	}{
		{"git", true},
		{"nix", true},
		{"python3", true},
		{"unknowncmd", false},
		{"./x", false},
		{"foo/bar", false},
		{`foo\bar`, false},
		{"", false},
	} {
		if got := helpAllowed(tc.root); got != tc.want {
			t.Errorf("helpAllowed(%q) = %v; want %v", tc.root, got, tc.want)
		}
	}
}

func TestCapString(t *testing.T) {
	if got := capString(strings.Repeat("a", 5000), 1500); len(got) != 1500 {
		t.Errorf("capString length = %d; want 1500", len(got))
	}
	if got := capString("short", 100); got != "short" {
		t.Errorf("capString = %q; want short", got)
	}
	// A cut in the middle of a multi-byte rune stays valid UTF-8.
	got := capString(strings.Repeat("é", 1000), 999)
	if len(got) > 999 || !utf8.ValidString(got) {
		t.Errorf("capString = %d bytes valid=%v; want ≤999 bytes of valid UTF-8", len(got), utf8.ValidString(got))
	}
}

func TestGatherScriptsSubdirectories(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "package.json"), `{"scripts":{"dev":"vite"}}`)
	writeFile(t, filepath.Join(dir, "Makefile"), ".PHONY: build\nbuild: deps\n\tgo build\nX := 1\n")
	writeFile(t, filepath.Join(dir, "justfile"), "test:\n    go test ./...\n")
	writeFile(t, filepath.Join(dir, "web", "package.json"), `{"scripts":{"start":"next"}}`)
	writeFile(t, filepath.Join(dir, "node_modules", "pkg", "package.json"), `{"scripts":{"hidden":"x"}}`)
	writeFile(t, filepath.Join(dir, ".hidden", "package.json"), `{"scripts":{"secret":"x"}}`)

	scripts := gatherScripts(dir)
	if !contains(scripts, "dev: vite") {
		t.Errorf("scripts = %v; want root package.json script", scripts)
	}
	if !contains(scripts, "build") || !contains(scripts, "test") {
		t.Errorf("scripts = %v; want make and just targets", scripts)
	}
	if contains(scripts, "X") || contains(scripts, ".PHONY") {
		t.Errorf("scripts = %v; must not contain assignments or .PHONY", scripts)
	}
	if !contains(scripts, "web/start: next") {
		t.Errorf("scripts = %v; want subdirectory script with prefix", scripts)
	}
	for _, s := range scripts {
		if strings.Contains(s, "hidden") || strings.Contains(s, "secret") {
			t.Errorf("scripts = %v; must exclude node_modules and hidden dirs", scripts)
		}
	}
}

func contains(list []string, want string) bool {
	for _, s := range list {
		if s == want {
			return true
		}
	}
	return false
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}
