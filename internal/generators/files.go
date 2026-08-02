package generators

import (
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/rselbach/argmax/internal/complete"
)

// Files returns an unrestricted file and directory generator.
func Files() complete.Generator {
	return files(false, nil)
}

// Directories returns a directory-only generator.
func Directories() complete.Generator {
	return files(true, nil)
}

// FilesWithExt returns a generator filtered to the given extensions
// (without dots, matched case-insensitively). Directories always appear so
// traversal can continue, and one level below visible matching directories
// is inspected for accepted files.
func FilesWithExt(exts ...string) complete.Generator {
	set := make(map[string]bool, len(exts))
	for _, e := range exts {
		set[strings.ToLower(e)] = true
	}
	return files(false, set)
}

func files(dirsOnly bool, exts map[string]bool) complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		dirPart, namePart := splitPathPrefix(prefix)
		searchDir := resolveDir(ctx.CWD, dirPart)
		entries, err := os.ReadDir(searchDir)
		if err != nil {
			return nil
		}
		var out []complete.Candidate
		for _, entry := range entries {
			name := entry.Name()
			if !ctx.HiddenFiles && strings.HasPrefix(name, ".") && !strings.HasPrefix(namePart, ".") {
				continue
			}
			if !hasFoldPrefix(name, namePart) {
				continue
			}
			isDir := isDirectory(entry, searchDir)
			switch {
			case isDir:
				out = append(out, dirCandidate(dirPart, name))
				if exts != nil {
					out = append(out, peekBelow(searchDir, dirPart, name, exts, ctx.HiddenFiles)...)
				}
			case dirsOnly:
			case exts != nil && !acceptedExt(name, exts):
			default:
				out = append(out, fileCandidate(dirPart, name))
			}
		}
		sort.Slice(out, func(i, j int) bool {
			return strings.ToLower(out[i].Title) < strings.ToLower(out[j].Title)
		})
		return out
	}
}

// splitPathPrefix separates the typed directory portion from the partial
// entry name, normalizing backslash separators.
func splitPathPrefix(prefix string) (dir, name string) {
	p := strings.ReplaceAll(prefix, `\`, `/`)
	i := strings.LastIndex(p, "/")
	if i < 0 {
		return "", p
	}
	return p[:i+1], p[i+1:]
}

// resolveDir resolves the search directory relative to the child shell's
// CWD, honoring absolute and tilde-prefixed paths.
func resolveDir(cwd, dirPart string) string {
	switch {
	case dirPart == "":
		return cwd
	case strings.HasPrefix(dirPart, "~"):
		home, err := os.UserHomeDir()
		if err != nil {
			return cwd
		}
		return filepath.Join(home, strings.TrimPrefix(strings.TrimPrefix(dirPart, "~"), "/"))
	case filepath.IsAbs(dirPart):
		return filepath.Clean(dirPart)
	default:
		return filepath.Join(cwd, dirPart)
	}
}

// isDirectory treats symlinks to directories as directories.
func isDirectory(entry os.DirEntry, dir string) bool {
	if entry.IsDir() {
		return true
	}
	if entry.Type()&os.ModeSymlink == 0 {
		return false
	}
	info, err := os.Stat(filepath.Join(dir, entry.Name()))
	return err == nil && info.IsDir()
}

func acceptedExt(name string, exts map[string]bool) bool {
	ext := strings.ToLower(strings.TrimPrefix(filepath.Ext(name), "."))
	return ext != "" && exts[ext]
}

func dirCandidate(dirPart, name string) complete.Candidate {
	return complete.Candidate{
		Title:       name + "/",
		Insert:      dirPart + name + "/",
		Description: "directory",
		Icon:        "folder",
		Source:      complete.SourceFile,
		Priority:    50,
		IsDirectory: true,
	}
}

func fileCandidate(dirPart, name string) complete.Candidate {
	desc := "file"
	if ext := strings.TrimPrefix(filepath.Ext(name), "."); ext != "" {
		desc = ext + " file"
	}
	return complete.Candidate{
		Title:       name,
		Insert:      dirPart + name,
		Description: desc,
		Icon:        "file",
		Source:      complete.SourceFile,
		Priority:    50,
	}
}

// peekBelow inspects one directory level below a visible directory for
// files with accepted extensions.
func peekBelow(searchDir, dirPart, name string, exts map[string]bool, hidden bool) []complete.Candidate {
	entries, err := os.ReadDir(filepath.Join(searchDir, name))
	if err != nil {
		return nil
	}
	var out []complete.Candidate
	for _, entry := range entries {
		en := entry.Name()
		if entry.IsDir() || (!hidden && strings.HasPrefix(en, ".")) || !acceptedExt(en, exts) {
			continue
		}
		out = append(out, fileCandidate(dirPart+name+"/", en))
	}
	return out
}
