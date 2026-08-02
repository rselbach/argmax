package rank

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// Signature keys used in Workspace.Signatures (RANK-009).
const (
	SigGit        = "git"
	SigNode       = "node"
	SigGo         = "go"
	SigRust       = "rust"
	SigPython     = "python"
	SigDocker     = "docker"
	SigMake       = "make"
	SigJust       = "just"
	SigKubernetes = "kubernetes"
	SigTaskfile   = "taskfile"
	SigMaven      = "maven"
	SigGradle     = "gradle"
	SigCMake      = "cmake"
)

// maxWalkParents bounds how many parent directories Detect inspects when no
// .git turns up, keeping detection cheap deep inside untracked trees.
const maxWalkParents = 5

// Workspace describes detected project signatures (RANK-009).
type Workspace struct {
	Dir        string
	Signatures map[string]bool // keys: git,node,go,rust,python,docker,make,just,kubernetes,taskfile,maven,gradle,cmake
	InGit      bool
	GitBranch  string // current branch when InGit ("" if undetectable)
}

// Has reports whether the workspace carries the given signature.
func (w *Workspace) Has(sig string) bool {
	return w != nil && w.Signatures[sig]
}

// Detector caches workspace detection per directory. A cached entry is
// invalidated by the directory's mtime and, for Git worktrees, the mtime of
// the .git/HEAD file that was consulted (RANK-008). All detection is plain
// os.Stat/os.ReadFile; no subprocess is ever spawned. Detector is safe for
// concurrent use.
type Detector struct {
	mu    sync.Mutex
	cache map[string]cacheEntry
}

type cacheEntry struct {
	ws        *Workspace
	dirMtime  time.Time
	headPath  string // HEAD file consulted; "" when not in a Git worktree
	headMtime time.Time
}

// NewDetector returns an empty Detector.
func NewDetector() *Detector {
	return &Detector{cache: make(map[string]cacheEntry)}
}

// Detect returns the workspace signatures for cwd, reusing the cached result
// while neither the directory mtime nor the consulted Git HEAD mtime changed.
func (d *Detector) Detect(cwd string) *Workspace {
	d.mu.Lock()
	defer d.mu.Unlock()
	if e, ok := d.cache[cwd]; ok {
		if mtimeOf(cwd).Equal(e.dirMtime) && mtimeOf(e.headPath).Equal(e.headMtime) {
			return e.ws
		}
	}
	ws, headPath := detect(cwd)
	e := cacheEntry{ws: ws, dirMtime: mtimeOf(cwd), headPath: headPath}
	if headPath != "" {
		e.headMtime = mtimeOf(headPath)
	}
	d.cache[cwd] = e
	return ws
}

// detect scans dir and, accumulating signatures, its parents: the walk stops
// at the first directory containing a .git entry, at the filesystem root, or
// after maxWalkParents levels without one.
func detect(dir string) (ws *Workspace, headPath string) {
	ws = &Workspace{Dir: dir, Signatures: make(map[string]bool)}
	for level := 0; ; level++ {
		checkSignatures(dir, ws.Signatures)
		gitPath := filepath.Join(dir, ".git")
		if _, err := os.Stat(gitPath); err == nil {
			ws.InGit = true
			ws.Signatures[SigGit] = true
			headPath, ws.GitBranch = readGitHEAD(gitPath, dir)
			break
		}
		parent := filepath.Dir(dir)
		if level >= maxWalkParents || parent == dir {
			break
		}
		dir = parent
	}
	return ws, headPath
}

// checkSignatures records every recognized ecosystem marker found in dir
// (RANK-009). All checks are plain os.Stat calls.
func checkSignatures(dir string, sigs map[string]bool) {
	has := func(name string) bool {
		_, err := os.Stat(filepath.Join(dir, name))
		return err == nil
	}
	hasDir := func(name string) bool {
		fi, err := os.Stat(filepath.Join(dir, name))
		return err == nil && fi.IsDir()
	}
	if !sigs[SigNode] && has("package.json") {
		sigs[SigNode] = true
	}
	if !sigs[SigGo] && has("go.mod") {
		sigs[SigGo] = true
	}
	if !sigs[SigRust] && has("Cargo.toml") {
		sigs[SigRust] = true
	}
	if !sigs[SigPython] && (has("pyproject.toml") || has("requirements.txt")) {
		sigs[SigPython] = true
	}
	if !sigs[SigDocker] && (has("Dockerfile") || has("docker-compose.yml") ||
		has("docker-compose.yaml") || has("compose.yml") || has("compose.yaml")) {
		sigs[SigDocker] = true
	}
	if !sigs[SigMake] && has("Makefile") {
		sigs[SigMake] = true
	}
	if !sigs[SigJust] && (has("justfile") || has("Justfile")) {
		sigs[SigJust] = true
	}
	if !sigs[SigKubernetes] && (has("Chart.yaml") || hasDir("k8s") ||
		hasDir("kubernetes") || hasDir("helm")) {
		sigs[SigKubernetes] = true
	}
	if !sigs[SigTaskfile] && (has("Taskfile.yml") || has("Taskfile.yaml")) {
		sigs[SigTaskfile] = true
	}
	if !sigs[SigMaven] && has("pom.xml") {
		sigs[SigMaven] = true
	}
	if !sigs[SigGradle] && (has("build.gradle") || has("build.gradle.kts") ||
		has("settings.gradle")) {
		sigs[SigGradle] = true
	}
	if !sigs[SigCMake] && has("CMakeLists.txt") {
		sigs[SigCMake] = true
	}
}

// readGitHEAD resolves the current branch by reading .git/HEAD directly —
// "ref: refs/heads/<branch>" — never via a subprocess. gitPath may be the
// .git directory or, for worktrees and submodules, a .git file containing a
// "gitdir: <path>" pointer. It returns the HEAD path consulted (for cache
// invalidation) and the branch name ("" for detached HEAD or unreadable
// state).
func readGitHEAD(gitPath, dir string) (headPath, branch string) {
	fi, err := os.Stat(gitPath)
	if err != nil {
		return "", ""
	}
	if !fi.IsDir() {
		data, err := os.ReadFile(gitPath)
		if err != nil {
			return "", ""
		}
		target := strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(string(data)), "gitdir:"))
		if target == "" {
			return "", ""
		}
		if !filepath.IsAbs(target) {
			target = filepath.Join(dir, target)
		}
		gitPath = target
	}
	headPath = filepath.Join(gitPath, "HEAD")
	data, err := os.ReadFile(headPath)
	if err != nil {
		return headPath, ""
	}
	const refPrefix = "ref: refs/heads/"
	line := strings.TrimSpace(string(data))
	if strings.HasPrefix(line, refPrefix) {
		return headPath, line[len(refPrefix):]
	}
	return headPath, "" // detached HEAD
}

func mtimeOf(path string) time.Time {
	if path == "" {
		return time.Time{}
	}
	fi, err := os.Stat(path)
	if err != nil {
		return time.Time{}
	}
	return fi.ModTime()
}
