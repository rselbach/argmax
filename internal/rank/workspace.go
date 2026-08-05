package rank

import (
	"maps"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	project "github.com/rselbach/argmax/internal/workspace"
)

// Workspace holds the detected signatures of the current directory.
type Workspace struct {
	Signatures map[string]bool
	// GitBranch is the active branch when inside a Git repository.
	GitBranch string
}

// Has reports whether the signature was detected.
func (w Workspace) Has(sig string) bool { return w.Signatures[sig] }

// Detector caches one workspace signature set, invalidated by resolved
// roots, marker signatures, and Git HEAD modification times.
type Detector struct {
	mu       sync.Mutex
	dir      string
	root     string
	markers  string
	headMod  time.Time
	cached   Workspace
	hasCache bool
}

// Detect returns the workspace signatures for dir, using the cache when
// nothing changed.
func (d *Detector) Detect(dir string) Workspace {
	d.mu.Lock()
	defer d.mu.Unlock()
	info := project.Resolve(dir)
	markers := signatureKey(info.Signatures)
	headMod := time.Time{}
	if info.GitDir != "" {
		headMod = modTime(filepath.Join(info.GitDir, "HEAD"))
	}
	if d.hasCache && d.dir == dir && d.root == info.Root && d.markers == markers && headMod.Equal(d.headMod) {
		return d.cached
	}
	ws := Workspace{Signatures: maps.Clone(info.Signatures), GitBranch: currentBranch(info.GitDir)}
	d.dir, d.root, d.markers, d.headMod, d.cached, d.hasCache = dir, info.Root, markers, headMod, ws, true
	return ws
}

func modTime(path string) time.Time {
	info, err := os.Stat(path)
	if err != nil {
		return time.Time{}
	}
	return info.ModTime()
}

func signatureKey(signatures map[string]bool) string {
	keys := make([]string, 0, len(signatures))
	for signature := range signatures {
		keys = append(keys, signature)
	}
	sort.Strings(keys)
	return strings.Join(keys, "\x00")
}

// currentBranch reads .git/HEAD directly to avoid a subprocess on the
// scoring path.
func currentBranch(gitDir string) string {
	if gitDir == "" {
		return ""
	}
	data, err := os.ReadFile(filepath.Join(gitDir, "HEAD"))
	if err != nil {
		return ""
	}
	ref := strings.TrimSpace(string(data))
	if name, ok := strings.CutPrefix(ref, "ref: refs/heads/"); ok {
		return name
	}
	return ""
}
