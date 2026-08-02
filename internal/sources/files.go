package sources

import (
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/rselbach/argmax/internal/core"
)

// maxFileResults bounds file completion results for safety.
const maxFileResults = 500

// maxPeekDirs bounds how many directories the FILE-007 one-level peek enters.
const maxPeekDirs = 50

// FileMode selects which entries file completion returns.
type FileMode int

const (
	// FileAny completes files and directories.
	FileAny FileMode = iota
	// FileDir completes directories only.
	FileDir
	// FileExt completes files matching Exts, plus directories for traversal.
	FileExt
)

// FileRequest describes one file completion query.
type FileRequest struct {
	Partial    string   // the typed path token, e.g. "src/mai", "~/De", "/usr/bi", `dir\sub`
	CWD        string   // child shell CWD; relative paths resolve here
	Mode       FileMode // which entries to return
	Exts       []string // for FileExt, case-insensitive, without dots, e.g. ["go", "mod"]
	ShowHidden bool     // include dot-prefixed entries
}

// CompleteFiles completes the typed path token (PRD 9.7). The returned Text
// preserves the typed directory prefix; directories carry a trailing "/".
func (s *Sources) CompleteFiles(req FileRequest) []core.Suggestion {
	typedDir, base := splitPartial(req.Partial)
	fsDir := resolveDir(typedDir, req.CWD)
	entries, err := os.ReadDir(fsDir)
	if err != nil {
		return nil
	}

	lowerBase := strings.ToLower(base)
	exts := lowerExts(req.Exts)
	var res []core.Suggestion
	fileMatched := false
	var subdirs []string

	for _, e := range entries {
		if len(res) >= maxFileResults {
			break
		}
		name := e.Name()
		if !req.ShowHidden && strings.HasPrefix(name, ".") {
			continue
		}
		if lowerBase != "" && !strings.HasPrefix(strings.ToLower(name), lowerBase) {
			continue
		}
		isDir := entryIsDir(fsDir, e)
		switch req.Mode {
		case FileDir:
			if !isDir {
				continue
			}
		case FileExt:
			if isDir {
				subdirs = append(subdirs, name)
			} else {
				if !extMatch(name, exts) {
					continue
				}
				fileMatched = true
			}
		}
		res = append(res, fileSuggestion(typedDir+name, name, isDir))
	}

	// FILE-007: when no file matched at the top level, peek one directory
	// level below matching visible directories for accepted extensions.
	if req.Mode == FileExt && !fileMatched {
		res = peekSubdirs(req, fsDir, typedDir, subdirs, exts, res)
	}

	sortSuggestions(res)
	return res
}

// peekSubdirs looks one level below the named directories for files with
// accepted extensions, returned as typedDir/dir/file.ext.
func peekSubdirs(req FileRequest, fsDir, typedDir string, subdirs, exts []string, res []core.Suggestion) []core.Suggestion {
	for i, d := range subdirs {
		if i >= maxPeekDirs || len(res) >= maxFileResults {
			break
		}
		sub, err := os.ReadDir(filepath.Join(fsDir, d))
		if err != nil {
			continue
		}
		for _, se := range sub {
			if len(res) >= maxFileResults {
				break
			}
			name := se.Name()
			if !req.ShowHidden && strings.HasPrefix(name, ".") {
				continue
			}
			if entryIsDir(filepath.Join(fsDir, d), se) || !extMatch(name, exts) {
				continue
			}
			res = append(res, fileSuggestion(typedDir+d+"/"+name, name, false))
		}
	}
	return res
}

// splitPartial splits a typed path token into its directory part (kept
// verbatim, including the trailing separator) and the final name part. Both
// '/' and '\' count as separators.
func splitPartial(p string) (dir, base string) {
	if i := strings.LastIndexAny(p, `/\`); i >= 0 {
		return p[:i+1], p[i+1:]
	}
	return "", p
}

// resolveDir maps the typed directory part to a filesystem directory:
// backslash separators become slashes, "~" expands to the home directory,
// and relative paths resolve against cwd (FILE-001/002).
func resolveDir(typedDir, cwd string) string {
	d := strings.ReplaceAll(typedDir, `\`, "/")
	if d == "~" || strings.HasPrefix(d, "~/") {
		if home, err := os.UserHomeDir(); err == nil {
			d = filepath.Join(home, strings.TrimPrefix(strings.TrimPrefix(d, "~"), "/"))
		}
	}
	if d == "" {
		d = "."
	}
	if !filepath.IsAbs(d) {
		d = filepath.Join(cwd, d)
	}
	return d
}

// entryIsDir reports whether the entry is a directory, treating symlinks to
// directories as directories (FILE-004).
func entryIsDir(dir string, e os.DirEntry) bool {
	if e.IsDir() {
		return true
	}
	if e.Type()&os.ModeSymlink == 0 {
		return false
	}
	fi, err := os.Stat(filepath.Join(dir, e.Name()))
	return err == nil && fi.IsDir()
}

// fileSuggestion builds a file or directory suggestion. Text is the
// completed token (typed directory prefix + entry name).
func fileSuggestion(text, name string, isDir bool) core.Suggestion {
	if isDir {
		return core.Suggestion{
			Text:        text + "/",
			Description: "directory",
			Icon:        "directory",
			Source:      core.SourceFile,
			Confidence:  60,
			Priority:    -1,
		}
	}
	desc := "file"
	if ext := extensionOf(name); ext != "" {
		desc = ext + " file"
	}
	return core.Suggestion{
		Text:        text,
		Description: desc,
		Icon:        "file",
		Source:      core.SourceFile,
		Confidence:  60,
		Priority:    -1,
	}
}

// extensionOf returns the lowercase file extension without the dot.
func extensionOf(name string) string {
	ext := filepath.Ext(name)
	if len(ext) < 2 {
		return ""
	}
	return strings.ToLower(ext[1:])
}

func lowerExts(exts []string) []string {
	out := make([]string, 0, len(exts))
	for _, e := range exts {
		out = append(out, strings.ToLower(strings.TrimPrefix(e, ".")))
	}
	return out
}

func extMatch(name string, exts []string) bool {
	if len(exts) == 0 {
		return true
	}
	ext := extensionOf(name)
	for _, e := range exts {
		if ext == e {
			return true
		}
	}
	return false
}

func sortSuggestions(res []core.Suggestion) {
	sort.SliceStable(res, func(i, j int) bool {
		a, b := strings.ToLower(res[i].Text), strings.ToLower(res[j].Text)
		if a == b {
			return res[i].Text < res[j].Text
		}
		return a < b
	})
}
