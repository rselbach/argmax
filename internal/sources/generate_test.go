package sources

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/core"
)

func genByText(sugs []core.Suggestion) map[string]core.Suggestion {
	m := make(map[string]core.Suggestion, len(sugs))
	for _, s := range sugs {
		m[s.Text] = s
	}
	return m
}

func TestKnownGenerator(t *testing.T) {
	for _, id := range []string{
		"files", "dirs", "ext:go,mod",
		"git-branches", "git-remotes", "git-tags", "git-commits", "git-stashes",
		"git-pushpull", "git-checkout", "git-reset", "git-show",
		"docker-containers-all", "docker-containers-running", "docker-images", "docker-inspect",
		"ssh-hosts", "node-scripts", "just-recipes", "make-targets", "zoxide-dirs",
		"packages-installed", "pip-packages", "processes", "process-names", "env-vars", "chmod-modes",
	} {
		if !KnownGenerator(id) {
			t.Errorf("KnownGenerator(%q) = false", id)
		}
	}
	for _, id := range []string{"", "ext:", "nope", "git", "FILES"} {
		if KnownGenerator(id) {
			t.Errorf("KnownGenerator(%q) = true", id)
		}
	}
}

func TestGenerateUnknownID(t *testing.T) {
	s := newTestSources()
	if got := s.Generate(context.Background(), GenRequest{ID: "nope"}); got != nil {
		t.Fatalf("got %v, want nil", collect(got))
	}
}

func TestGenerateFilesDispatch(t *testing.T) {
	root := t.TempDir()
	writeFile(t, filepath.Join(root, "a.go"))
	writeFile(t, filepath.Join(root, "b.txt"))
	s := newTestSources()

	res := s.Generate(context.Background(), GenRequest{ID: "files", CWD: root})
	if len(res) != 2 {
		t.Fatalf("files: got %v", collect(res))
	}
	res = s.Generate(context.Background(), GenRequest{ID: "dirs", CWD: root})
	if len(res) != 0 {
		t.Fatalf("dirs: got %v", collect(res))
	}
	res = s.Generate(context.Background(), GenRequest{ID: "ext:go", CWD: root})
	if len(res) != 1 || res[0].Text != "a.go" {
		t.Fatalf("ext:go: got %v", collect(res))
	}
}

func TestMakeTargets(t *testing.T) {
	root := t.TempDir()
	makefile := `.PHONY: all clean
VAR := value
all: build
build:
	go build ./...
clean: ## cleanup
	rm -rf out
%.o: %.c
	cc -c $<
`
	if err := os.WriteFile(filepath.Join(root, "Makefile"), []byte(makefile), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "make-targets", CWD: root})
	got := genByText(res)
	for _, want := range []string{"all", "build", "clean"} {
		if sug, ok := got[want]; !ok || sug.Description != "make target" {
			t.Fatalf("target %q missing or wrong desc: %v", want, collect(res))
		}
	}
	for _, bad := range []string{".PHONY", "VAR", "%.o"} {
		if _, ok := got[bad]; ok {
			t.Fatalf("pseudo/pattern target %q should be excluded: %v", bad, collect(res))
		}
	}
}

func TestJustRecipes(t *testing.T) {
	root := t.TempDir()
	justfile := `set shell := ["sh", "-c"]
alias b := build

# Build the project
build:
    go build ./...

# Run tests
test filter="":
    go test {{filter}}

deploy host:
    echo deploy
`
	if err := os.WriteFile(filepath.Join(root, "justfile"), []byte(justfile), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "just-recipes", CWD: root})
	got := genByText(res)
	if len(got) != 3 {
		t.Fatalf("got %v, want 3 recipes", collect(res))
	}
	if got["build"].Description != "Build the project" {
		t.Fatalf("build desc = %q", got["build"].Description)
	}
	if got["test"].Description != "Run tests" {
		t.Fatalf("test desc = %q", got["test"].Description)
	}
	if got["deploy"].Description != "recipe" {
		t.Fatalf("deploy desc = %q, want fallback %q", got["deploy"].Description, "recipe")
	}
}

func TestNodeScripts(t *testing.T) {
	root := t.TempDir()
	pkg := `{"scripts":{"dev":"vite","test":"vitest","custom":"echo hi"}}`
	if err := os.WriteFile(filepath.Join(root, "package.json"), []byte(pkg), 0o644); err != nil {
		t.Fatal(err)
	}
	sub := filepath.Join(root, "sub")
	if err := os.MkdirAll(sub, 0o755); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "node-scripts", CWD: sub})
	got := genByText(res)
	if len(got) != 3 {
		t.Fatalf("got %v, want 3 scripts", collect(res))
	}
	if got["dev"].Confidence != 85 || got["dev"].Description != "vite" {
		t.Fatalf("dev = %+v", got["dev"])
	}
	if got["test"].Confidence != 85 {
		t.Fatalf("test = %+v", got["test"])
	}
	if got["custom"].Confidence != 70 || got["custom"].Description != "echo hi" {
		t.Fatalf("custom = %+v", got["custom"])
	}
}

func TestNodeScriptsPlaceholders(t *testing.T) {
	root := t.TempDir()
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "node-scripts", CWD: root})
	got := genByText(res)
	for _, want := range []string{"dev", "start", "build", "test", "lint"} {
		if sug, ok := got[want]; !ok || sug.Description != "common script" {
			t.Fatalf("placeholder %q missing: %v", want, collect(res))
		}
	}
	if len(got) != 5 {
		t.Fatalf("got %v, want 5 placeholders", collect(res))
	}
}

func TestSSHHosts(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	sshDir := filepath.Join(home, ".ssh")
	if err := os.MkdirAll(sshDir, 0o755); err != nil {
		t.Fatal(err)
	}
	config := `Host prod
  HostName 10.0.0.1

Host *.example.com
  User admin

Host !bastion *
  User root

Host web?
  User x

Host prod
  HostName 10.0.0.2
`
	if err := os.WriteFile(filepath.Join(sshDir, "config"), []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "ssh-hosts", CWD: home})
	count := 0
	for _, sug := range res {
		if strings.ContainsAny(sug.Text, "*?!") {
			t.Fatalf("pattern host %q should be excluded", sug.Text)
		}
		if sug.Text == "prod" {
			count++
			if sug.Description != "ssh host" {
				t.Fatalf("prod desc = %q", sug.Description)
			}
		}
	}
	if count != 1 {
		t.Fatalf("prod appeared %d times, want exactly 1 (dedupe): %v", count, collect(res))
	}
}

func TestEnvVarsRedaction(t *testing.T) {
	t.Setenv("MY_API_TOKEN", "supersecret")
	t.Setenv("ARGMAX_TEST_PLAIN", "hello")
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "env-vars"})
	got := genByText(res)
	if got["MY_API_TOKEN"].Description != "••••••" {
		t.Fatalf("MY_API_TOKEN desc = %q, want redacted", got["MY_API_TOKEN"].Description)
	}
	if got["ARGMAX_TEST_PLAIN"].Description != "hello" {
		t.Fatalf("ARGMAX_TEST_PLAIN desc = %q", got["ARGMAX_TEST_PLAIN"].Description)
	}
}

func TestEnvVarsPrefixFilter(t *testing.T) {
	t.Setenv("ARGMAX_FILTER_ME", "x")
	t.Setenv("OTHER_THING", "y")
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "env-vars", Partial: "ARGMAX_FILTER"})
	got := collect(res)
	if len(got) != 1 || got[0] != "ARGMAX_FILTER_ME" {
		t.Fatalf("got %v", got)
	}
}

func TestChmodModes(t *testing.T) {
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "chmod-modes"})
	if len(res) != len(chmodModes) {
		t.Fatalf("got %d modes, want %d", len(res), len(chmodModes))
	}
	got := genByText(res)
	for _, want := range []string{"+x", "755", "go-w", "+t"} {
		if _, ok := got[want]; !ok {
			t.Fatalf("mode %q missing: %v", want, collect(res))
		}
	}
}

func TestChmodModesFilesPreferExecutables(t *testing.T) {
	root := t.TempDir()
	writeFile(t, filepath.Join(root, "script.sh"))
	writeFile(t, filepath.Join(root, "readme.txt"))
	if err := os.Chmod(filepath.Join(root, "script.sh"), 0o755); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "chmod-modes", Args: []string{"+x"}, CWD: root})
	got := collect(res)
	if len(got) != 2 || got[0] != "script.sh" {
		t.Fatalf("executables should sort first: %v", got)
	}
}

func TestProcessesSmoke(t *testing.T) {
	if _, err := exec.LookPath("ps"); err != nil {
		t.Skip("ps not available")
	}
	s := newTestSources()
	ctx, cancel := context.WithTimeout(context.Background(), 5e9)
	defer cancel()
	res := s.Generate(ctx, GenRequest{ID: "processes"})
	if len(res) == 0 {
		t.Fatal("no processes returned")
	}
	for _, sug := range res {
		for _, r := range sug.Text {
			if r < '0' || r > '9' {
				t.Fatalf("non-numeric PID text %q", sug.Text)
			}
		}
		if sug.Description == "" {
			t.Fatalf("empty process description for %q", sug.Text)
		}
	}
	names := s.Generate(ctx, GenRequest{ID: "process-names"})
	if len(names) == 0 {
		t.Fatal("no process names returned")
	}
}

func TestPackagesInstalledSmoke(t *testing.T) {
	s := newTestSources()
	ctx := context.Background()
	// Unknown root yields nil.
	if got := s.Generate(ctx, GenRequest{ID: "packages-installed", RootCmd: "nope"}); got != nil {
		t.Fatalf("unknown root: got %v", collect(got))
	}
	roots := map[string]string{
		"pacman": "pacman", "apt": "dpkg-query", "dnf": "rpm", "brew": "brew",
	}
	for root, tool := range roots {
		if _, err := exec.LookPath(tool); err != nil {
			continue
		}
		res := s.Generate(ctx, GenRequest{ID: "packages-installed", RootCmd: root})
		if len(res) == 0 {
			t.Errorf("%s: no packages returned", root)
		}
	}
}

func TestPipPackagesSmoke(t *testing.T) {
	if _, err := exec.LookPath("pip"); err != nil {
		if _, err := exec.LookPath("pip3"); err != nil {
			t.Skip("pip/pip3 not available")
		}
	}
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "pip-packages"})
	if len(res) == 0 {
		t.Fatal("no pip packages returned")
	}
}

func TestDockerGeneratorsSmoke(t *testing.T) {
	tool := ""
	for _, c := range []string{"docker", "podman"} {
		if _, err := exec.LookPath(c); err == nil {
			tool = c
			break
		}
	}
	if tool == "" {
		t.Skip("docker/podman not available")
	}
	s := newTestSources()
	ctx, cancel := context.WithTimeout(context.Background(), 5e9)
	defer cancel()
	for _, id := range []string{"docker-containers-all", "docker-containers-running", "docker-images", "docker-inspect"} {
		// Smoke: must not panic; daemon may be absent (nil is fine).
		_ = s.Generate(ctx, GenRequest{ID: id, RootCmd: tool})
	}
}

func TestZoxideDirsSmoke(t *testing.T) {
	if _, err := exec.LookPath("zoxide"); err != nil {
		t.Skip("zoxide not available")
	}
	s := newTestSources()
	res := s.Generate(context.Background(), GenRequest{ID: "zoxide-dirs", CWD: t.TempDir()})
	// Smoke: bounded result from the zoxide side plus file dirs.
	if len(res) > 30 {
		t.Fatalf("unbounded zoxide results: %d", len(res))
	}
}

func TestGitGeneratorsSmoke(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}
	home := t.TempDir()
	t.Setenv("HOME", home)
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
	run("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "first")
	run("branch", "feature")
	run("tag", "v1")
	run("remote", "add", "origin", "https://example.invalid/x.git")

	s := newTestSources()
	ctx := context.Background()
	got := genByText(s.Generate(ctx, GenRequest{ID: "git-branches", CWD: repo}))
	// Default config filters the active branch (main).
	if _, ok := got["main"]; ok {
		t.Fatalf("active branch should be filtered: %v", collect(s.Generate(ctx, GenRequest{ID: "git-branches", CWD: repo})))
	}
	if _, ok := got["feature"]; !ok {
		t.Fatalf("feature branch missing: %v", got)
	}

	cfg := newTestSources()
	c := cfg.config()
	c.Git.FilterActiveBranch = false
	got = genByText(cfg.Generate(ctx, GenRequest{ID: "git-branches", CWD: repo}))
	if _, ok := got["main"]; !ok {
		t.Fatalf("main missing with FilterActiveBranch=false: %v", got)
	}

	// Branch creation flags suppress existing-branch suggestions.
	if res := s.Generate(ctx, GenRequest{ID: "git-branches", CWD: repo, Args: []string{"-b"}}); res != nil {
		t.Fatalf("-b should suppress branches: %v", collect(res))
	}

	if res := s.Generate(ctx, GenRequest{ID: "git-remotes", CWD: repo}); len(res) != 1 || res[0].Text != "origin" {
		t.Fatalf("remotes: %v", collect(res))
	}
	if res := s.Generate(ctx, GenRequest{ID: "git-tags", CWD: repo}); len(res) != 1 || res[0].Text != "v1" {
		t.Fatalf("tags: %v", collect(res))
	}
	commits := s.Generate(ctx, GenRequest{ID: "git-commits", CWD: repo})
	if len(commits) != 1 || !strings.Contains(commits[0].Description, "first") {
		t.Fatalf("commits: %v", collect(commits))
	}

	// push/pull flow: remotes, then branches, then nothing.
	if res := s.Generate(ctx, GenRequest{ID: "git-pushpull", CWD: repo}); len(res) != 1 || res[0].Text != "origin" {
		t.Fatalf("pushpull step 1: %v", collect(res))
	}
	if res := s.Generate(ctx, GenRequest{ID: "git-pushpull", CWD: repo, Args: []string{"origin"}}); len(res) == 0 {
		t.Fatal("pushpull step 2: no branches")
	}
	if res := s.Generate(ctx, GenRequest{ID: "git-pushpull", CWD: repo, Args: []string{"origin", "main"}}); res != nil {
		t.Fatalf("pushpull step 3: %v", collect(res))
	}

	// checkout mixes branches and files, branches first.
	writeFile(t, filepath.Join(repo, "somefile.txt"))
	res := s.Generate(ctx, GenRequest{ID: "git-checkout", CWD: repo})
	if len(res) < 2 || res[0].Description == "directory" {
		t.Fatalf("checkout: %v", collect(res))
	}
	// show mixes tags and commits.
	res = s.Generate(ctx, GenRequest{ID: "git-show", CWD: repo})
	if len(res) != 2 || res[0].Text != "v1" {
		t.Fatalf("show: %v", collect(res))
	}
}
