package rank

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// Workspace holds the detected signatures of the current directory.
type Workspace struct {
	Signatures map[string]bool
	// GitBranch is the active branch when inside a Git repository.
	GitBranch string
}

// Has reports whether the signature was detected.
func (w Workspace) Has(sig string) bool { return w.Signatures[sig] }

// Detector caches one workspace signature set, invalidated by directory
// and Git HEAD modification times.
type Detector struct {
	mu       sync.Mutex
	dir      string
	dirMod   time.Time
	headMod  time.Time
	cached   Workspace
	hasCache bool
}

// Detect returns the workspace signatures for dir, using the cache when
// nothing changed.
func (d *Detector) Detect(dir string) Workspace {
	d.mu.Lock()
	defer d.mu.Unlock()
	dirMod := modTime(dir)
	headMod := modTime(filepath.Join(dir, ".git", "HEAD"))
	if d.hasCache && d.dir == dir && dirMod.Equal(d.dirMod) && headMod.Equal(d.headMod) {
		return d.cached
	}
	ws := detect(dir)
	d.dir, d.dirMod, d.headMod, d.cached, d.hasCache = dir, dirMod, headMod, ws, true
	return ws
}

func modTime(path string) time.Time {
	info, err := os.Stat(path)
	if err != nil {
		return time.Time{}
	}
	return info.ModTime()
}

// signatureFiles maps marker files to signature names.
var signatureFiles = map[string]string{
	"package.json":        "node",
	"bun.lockb":           "node",
	"go.mod":              "go",
	"Cargo.toml":          "rust",
	"pyproject.toml":      "python",
	"requirements.txt":    "python",
	"Dockerfile":          "docker",
	"docker-compose.yml":  "docker",
	"docker-compose.yaml": "docker",
	"compose.yml":         "docker",
	"compose.yaml":        "docker",
	"Makefile":            "make",
	"makefile":            "make",
	"justfile":            "just",
	"Justfile":            "just",
	"Taskfile.yml":        "task",
	"Taskfile.yaml":       "task",
	"pom.xml":             "maven",
	"build.gradle":        "gradle",
	"build.gradle.kts":    "gradle",
	"CMakeLists.txt":      "cmake",
	"Chart.yaml":          "kubernetes",
}

func detect(dir string) Workspace {
	ws := Workspace{Signatures: map[string]bool{}}
	for file, sig := range signatureFiles {
		if _, err := os.Stat(filepath.Join(dir, file)); err == nil {
			ws.Signatures[sig] = true
		}
	}
	if info, err := os.Stat(filepath.Join(dir, ".git")); err == nil && info.IsDir() {
		ws.Signatures["git"] = true
		ws.GitBranch = currentBranch(dir)
	}
	if _, err := os.Stat(filepath.Join(dir, "k8s")); err == nil {
		ws.Signatures["kubernetes"] = true
	}
	return ws
}

// currentBranch reads .git/HEAD directly to avoid a subprocess on the
// scoring path.
func currentBranch(dir string) string {
	data, err := os.ReadFile(filepath.Join(dir, ".git", "HEAD"))
	if err != nil {
		return ""
	}
	ref := strings.TrimSpace(string(data))
	if name, ok := strings.CutPrefix(ref, "ref: refs/heads/"); ok {
		return name
	}
	return ""
}
