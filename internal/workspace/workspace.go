// Package workspace resolves project markers and Git metadata roots from a
// working directory and its ancestors.
package workspace

import (
	"os"
	"path/filepath"
	"strings"
)

// Info describes the workspace containing a working directory.
type Info struct {
	Root       string
	GitDir     string
	Signatures map[string]bool
}

// Has reports whether a workspace signature is present.
func (i Info) Has(signature string) bool { return i.Signatures[signature] }

// Resolve finds the containing Git repository when present, accumulating
// project markers between dir and that repository root. Outside Git, the
// nearest ancestor containing project markers is the workspace root.
func Resolve(dir string) Info {
	dir = absoluteClean(dir)
	gitRoot, gitDir := findGit(dir)
	info := Info{Root: dir, GitDir: gitDir, Signatures: map[string]bool{}}
	if gitRoot != "" {
		info.Root = gitRoot
		info.Signatures["git"] = true
		for current := dir; ; current = filepath.Dir(current) {
			addSignatures(info.Signatures, current)
			if current == gitRoot {
				break
			}
		}
		return info
	}
	for current := dir; ; current = filepath.Dir(current) {
		found := addSignatures(info.Signatures, current)
		if found {
			info.Root = current
			return info
		}
		parent := filepath.Dir(current)
		if parent == current {
			return info
		}
	}
}

func findGit(dir string) (root, gitDir string) {
	for current := dir; ; current = filepath.Dir(current) {
		path := filepath.Join(current, ".git")
		if info, err := os.Stat(path); err == nil {
			if info.IsDir() {
				return current, path
			}
			if resolved := gitDirFile(path); resolved != "" {
				return current, resolved
			}
		}
		parent := filepath.Dir(current)
		if parent == current {
			return "", ""
		}
	}
}

func gitDirFile(path string) string {
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	line := strings.TrimSpace(strings.SplitN(string(data), "\n", 2)[0])
	value, ok := strings.CutPrefix(line, "gitdir:")
	if !ok {
		return ""
	}
	value = strings.TrimSpace(value)
	if value == "" {
		return ""
	}
	if !filepath.IsAbs(value) {
		value = filepath.Join(filepath.Dir(path), value)
	}
	value = filepath.Clean(value)
	if info, err := os.Stat(value); err != nil || !info.IsDir() {
		return ""
	}
	return value
}

func absoluteClean(dir string) string {
	if abs, err := filepath.Abs(dir); err == nil {
		return filepath.Clean(abs)
	}
	return filepath.Clean(dir)
}

func addSignatures(signatures map[string]bool, dir string) bool {
	found := false
	for file, signature := range signatureFiles {
		if _, err := os.Stat(filepath.Join(dir, file)); err == nil {
			signatures[signature] = true
			found = true
		}
	}
	if info, err := os.Stat(filepath.Join(dir, "k8s")); err == nil && info.IsDir() {
		signatures["kubernetes"] = true
		found = true
	}
	return found
}

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
