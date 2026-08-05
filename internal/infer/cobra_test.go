package infer

import (
	"os"
	"path/filepath"
	"sync"
	"testing"
)

func TestCompleteIsolatesCacheByWorkingDirectory(t *testing.T) {
	in, executable := testInferrer(t)
	firstDir := t.TempDir()
	secondDir := t.TempDir()

	first := in.Complete(firstDir, executable, nil, "")
	second := in.Complete(secondDir, executable, nil, "")
	if len(first) != 1 || first[0].Title != firstDir {
		t.Fatalf("first completion = %#v, want working directory %q", first, firstDir)
	}
	if len(second) != 1 || second[0].Title != secondDir {
		t.Fatalf("second completion = %#v, want working directory %q", second, secondDir)
	}

	// Equivalent directory spellings must share one normalized cache key.
	in.Complete(filepath.Join(firstDir, "."), executable, nil, "")
	if got := len(in.cache); got != 2 {
		t.Errorf("cache contains %d entries, want 2 normalized directories", got)
	}
}

func TestCompleteReturnsIndependentCandidates(t *testing.T) {
	in, executable := testInferrer(t)
	cwd := t.TempDir()

	first := in.Complete(cwd, executable, nil, "")
	if len(first) != 1 {
		t.Fatalf("first completion = %#v, want one candidate", first)
	}
	first[0].Title = "caller mutation"

	second := in.Complete(cwd, executable, nil, "")
	if len(second) != 1 || second[0].Title != cwd {
		t.Fatalf("cached completion was mutated through caller slice: %#v", second)
	}

	start := make(chan struct{})
	var wg sync.WaitGroup
	for range 16 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			got := in.Complete(cwd, executable, nil, "")
			got[0].Title = "concurrent caller mutation"
		}()
	}
	close(start)
	wg.Wait()
	if got := in.Complete(cwd, executable, nil, ""); len(got) != 1 || got[0].Title != cwd {
		t.Fatalf("concurrent callers shared the cached slice: %#v", got)
	}
}

func testInferrer(t *testing.T) (*Inferrer, string) {
	t.Helper()
	binDir := t.TempDir()
	executable := "argmax-test-cobra"
	path := filepath.Join(binDir, executable)
	script := "#!/bin/sh\nprintf '%s\\tcwd\\n:0\\n' \"$PWD\"\n"
	if err := os.WriteFile(path, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", binDir)
	return New(), executable
}
