package sources

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/core"
)

func newTestSources() *Sources {
	return New(config.Default())
}

// collect extracts the Text fields of suggestions for assertions.
func collect(sugs []core.Suggestion) []string {
	out := make([]string, 0, len(sugs))
	for _, s := range sugs {
		out = append(out, s.Text)
	}
	return out
}

func writeFile(t *testing.T, path string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
}

// makeFileTree builds a fixture tree:
//
//	root/src/main.go, root/src/util.go, root/src/lib/helper.go
//	root/README.md, root/AFile.GO, root/.hidden, root/.hdir/secret
//	root/sub/file.txt, root/linkdir -> sub
func makeFileTree(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	writeFile(t, filepath.Join(root, "src", "main.go"))
	writeFile(t, filepath.Join(root, "src", "util.go"))
	writeFile(t, filepath.Join(root, "src", "lib", "helper.go"))
	writeFile(t, filepath.Join(root, "README.md"))
	writeFile(t, filepath.Join(root, "AFile.GO"))
	writeFile(t, filepath.Join(root, ".hidden"))
	writeFile(t, filepath.Join(root, ".hdir", "secret"))
	writeFile(t, filepath.Join(root, "sub", "file.txt"))
	if err := os.Symlink(filepath.Join(root, "sub"), filepath.Join(root, "linkdir")); err != nil {
		t.Fatal(err)
	}
	return root
}

func TestCompleteFilesNestedPartial(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "src/ma", CWD: root, Mode: FileAny})
	if len(res) != 1 || res[0].Text != "src/main.go" {
		t.Fatalf("got %v, want [src/main.go]", collect(res))
	}
	if res[0].Description != "go file" {
		t.Fatalf("description = %q, want %q", res[0].Description, "go file")
	}
}

func TestCompleteFilesTypedDirPrefixAndSort(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "src/", CWD: root, Mode: FileAny})
	want := []string{"src/lib/", "src/main.go", "src/util.go"}
	got := collect(res)
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("got %v, want %v (sorted case-insensitively)", got, want)
		}
	}
	for _, sug := range res {
		if sug.Text == "src/lib/" && sug.Description != "directory" {
			t.Fatalf("dir description = %q", sug.Description)
		}
	}
}

func TestCompleteFilesAbsolute(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	p := filepath.Join(root, "src", "ma")
	res := s.CompleteFiles(FileRequest{Partial: p, CWD: t.TempDir(), Mode: FileAny})
	if len(res) != 1 || res[0].Text != filepath.Join(root, "src", "main.go") {
		t.Fatalf("got %v", collect(res))
	}
}

func TestCompleteFilesTilde(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := os.MkdirAll(filepath.Join(home, "Desktop"), 0o755); err != nil {
		t.Fatal(err)
	}
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "~/De", CWD: t.TempDir(), Mode: FileAny})
	if len(res) != 1 || res[0].Text != "~/Desktop/" {
		t.Fatalf("got %v, want [~/Desktop/] keeping the typed ~ prefix", collect(res))
	}
}

func TestCompleteFilesBackslashSeparator(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: `src\mai`, CWD: root, Mode: FileAny})
	if len(res) != 1 || res[0].Text != `src\main.go` {
		t.Fatalf(`got %v, want [src\main.go]`, collect(res))
	}
}

func TestCompleteFilesHiddenFiltering(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "", CWD: root, Mode: FileAny})
	for _, sug := range res {
		if sug.Text == ".hidden" || sug.Text == ".hdir/" {
			t.Fatalf("hidden entry %q returned with ShowHidden=false", sug.Text)
		}
	}
	res = s.CompleteFiles(FileRequest{Partial: "", CWD: root, Mode: FileAny, ShowHidden: true})
	found := false
	for _, sug := range res {
		if sug.Text == ".hidden" {
			found = true
		}
	}
	if !found {
		t.Fatal(".hidden missing with ShowHidden=true")
	}
}

func TestCompleteFilesDirMode(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "", CWD: root, Mode: FileDir})
	got := collect(res)
	want := []string{"linkdir/", "src/", "sub/"}
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("got %v, want %v", got, want)
		}
	}
}

func TestCompleteFilesSymlinkToDir(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "link", CWD: root, Mode: FileAny})
	if len(res) != 1 || res[0].Text != "linkdir/" || res[0].Description != "directory" {
		t.Fatalf("symlink-to-dir not treated as directory: %v", collect(res))
	}
}

func TestCompleteFilesExtCaseInsensitive(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "", CWD: root, Mode: FileExt, Exts: []string{"go"}})
	got := collect(res)
	// AFile.GO matches case-insensitively; dirs stay for traversal.
	want := []string{"AFile.GO", "linkdir/", "src/", "sub/"}
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("got %v, want %v", got, want)
		}
	}
}

func TestCompleteFilesExtOneLevelPeek(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	// No .txt at top level: peek one level into visible dirs (FILE-007).
	res := s.CompleteFiles(FileRequest{Partial: "", CWD: root, Mode: FileExt, Exts: []string{"txt"}})
	got := collect(res)
	found := false
	for _, g := range got {
		if g == "sub/file.txt" {
			found = true
		}
	}
	if !found {
		t.Fatalf("one-level peek did not find sub/file.txt: %v", got)
	}
	// Traversal dirs are still offered.
	for _, want := range []string{"src/", "sub/"} {
		ok := false
		for _, g := range got {
			if g == want {
				ok = true
			}
		}
		if !ok {
			t.Fatalf("traversal dir %q missing: %v", want, got)
		}
	}
}

func TestCompleteFilesExtNoPeekWhenTopMatches(t *testing.T) {
	root := makeFileTree(t)
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "", CWD: root, Mode: FileExt, Exts: []string{"go"}})
	for _, sug := range res {
		if sug.Text == "src/main.go" {
			t.Fatalf("peek should not run when top level matched: %v", collect(res))
		}
	}
}

func TestCompleteFilesUnreadableDir(t *testing.T) {
	s := newTestSources()
	res := s.CompleteFiles(FileRequest{Partial: "nope/x", CWD: t.TempDir(), Mode: FileAny})
	if res != nil {
		t.Fatalf("got %v, want nil", collect(res))
	}
}
