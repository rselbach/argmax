package sources

import (
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/rselbach/argmax/internal/core"
)

// ScanPATH scans PATH for executable regular files and caches their names.
// Repeat calls re-scan (SRC-002).
func (s *Sources) ScanPATH() {
	seen := make(map[string]bool)
	var names []string
	for _, dir := range filepath.SplitList(os.Getenv("PATH")) {
		if dir == "" {
			continue
		}
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.IsDir() || seen[e.Name()] {
				continue
			}
			// os.Stat follows symlinks so symlinked executables count.
			fi, err := os.Stat(filepath.Join(dir, e.Name()))
			if err != nil || !fi.Mode().IsRegular() || fi.Mode().Perm()&0o111 == 0 {
				continue
			}
			seen[e.Name()] = true
			names = append(names, e.Name())
		}
	}
	sort.Strings(names)
	s.mu.Lock()
	s.pathNames = names
	s.scanned = true
	s.mu.Unlock()
}

// Executables returns PATH executables prefix-matching partial
// (case-insensitive). Unknown executables are labeled as system commands
// (SRC-001).
func (s *Sources) Executables(partial string) []core.Suggestion {
	s.mu.Lock()
	if !s.scanned {
		s.mu.Unlock()
		s.ScanPATH()
		s.mu.Lock()
	}
	names := s.pathNames
	s.mu.Unlock()

	lower := strings.ToLower(partial)
	var res []core.Suggestion
	for _, n := range names {
		if lower != "" && !strings.HasPrefix(strings.ToLower(n), lower) {
			continue
		}
		res = append(res, core.Suggestion{
			Text:        n,
			Description: "system command",
			Icon:        "system",
			Source:      core.SourceSystem,
			Confidence:  50,
			Priority:    -1,
		})
	}
	return res
}
