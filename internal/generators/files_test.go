package generators

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/rselbach/argmax/internal/complete"
)

func setupFS(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	for _, f := range []string{"main.go", "main_test.go", "README.md", ".hidden"} {
		if err := os.WriteFile(filepath.Join(dir, f), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.MkdirAll(filepath.Join(dir, "src"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "src", "app.go"), []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(dir, "My Docs"), 0o755); err != nil {
		t.Fatal(err)
	}
	return dir
}

func inserts(cands []complete.Candidate) []string {
	out := make([]string, 0, len(cands))
	for _, c := range cands {
		out = append(out, c.Insert)
	}
	return out
}

func hasInsert(cands []complete.Candidate, insert string) bool {
	for _, c := range cands {
		if c.Insert == insert {
			return true
		}
	}
	return false
}

func TestFilesGenerator(t *testing.T) {
	dir := setupFS(t)
	ctx := complete.Context{CWD: dir}
	gen := Files()

	got := gen(ctx, nil, "")
	if !hasInsert(got, "main.go") || !hasInsert(got, "src/") {
		t.Errorf("basic listing missing entries: %v", inserts(got))
	}
	if hasInsert(got, ".hidden") {
		t.Error("dot files hidden by default")
	}

	ctx.HiddenFiles = true
	if got := gen(ctx, nil, ""); !hasInsert(got, ".hidden") {
		t.Error("hidden-files=true must include dot entries")
	}
	ctx.HiddenFiles = false

	// Typing a dot prefix reveals dot entries regardless.
	if got := gen(ctx, nil, "."); !hasInsert(got, ".hidden") {
		t.Error("explicit dot prefix must reveal dot entries")
	}

	// Nested path keeps the typed directory prefix.
	if got := gen(ctx, nil, "src/ap"); !hasInsert(got, "src/app.go") {
		t.Errorf("nested completion missing, got %v", inserts(got))
	}

	// Backslash separators are accepted.
	if got := gen(ctx, nil, `src\ap`); !hasInsert(got, "src/app.go") {
		t.Errorf("backslash path completion missing, got %v", inserts(got))
	}
}

func TestDirectoriesOnly(t *testing.T) {
	dir := setupFS(t)
	got := Directories()(complete.Context{CWD: dir}, nil, "")
	for _, c := range got {
		if !c.IsDirectory {
			t.Errorf("directory-only generator returned file %q", c.Title)
		}
	}
	if !hasInsert(got, "src/") {
		t.Errorf("missing src/, got %v", inserts(got))
	}
}

func TestExtensionFilter(t *testing.T) {
	dir := setupFS(t)
	got := FilesWithExt("go")(complete.Context{CWD: dir}, nil, "")
	if hasInsert(got, "README.md") {
		t.Error("extension filter must exclude README.md")
	}
	if !hasInsert(got, "main.go") {
		t.Errorf("main.go missing, got %v", inserts(got))
	}
	// One level below a visible directory is inspected.
	if !hasInsert(got, "src/app.go") {
		t.Errorf("one-level peek missing src/app.go, got %v", inserts(got))
	}
	// Directories still appear for traversal.
	if !hasInsert(got, "src/") {
		t.Error("directories must remain visible under extension filters")
	}
}

func TestSymlinkedDirectoryTreatedAsDirectory(t *testing.T) {
	dir := setupFS(t)
	if err := os.Symlink(filepath.Join(dir, "src"), filepath.Join(dir, "link")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	got := Directories()(complete.Context{CWD: dir}, nil, "li")
	if !hasInsert(got, "link/") {
		t.Errorf("symlink to directory must complete as directory, got %v", inserts(got))
	}
}

func TestSpaceContainingDirectory(t *testing.T) {
	dir := setupFS(t)
	got := Directories()(complete.Context{CWD: dir}, nil, "My")
	if !hasInsert(got, "My Docs/") {
		t.Errorf("multiword directory missing, got %v", inserts(got))
	}
}
