// Package infer discovers completions for third-party CLIs that implement
// the Cobra __complete protocol when no bundled spec exists.
package infer

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/rselbach/argmax/internal/complete"
)

const (
	timeout = 300 * time.Millisecond
	// directiveError is Cobra's ShellCompDirectiveError bit.
	directiveError = 1
	maxOutput      = 256 << 10
	maxCacheSize   = 128
)

type cacheKey struct {
	binary  string
	modTime int64
	cwd     string
	args    string
	partial string
}

// Inferrer probes Cobra CLIs and caches results by resolved binary
// identity and modification time, so a replaced binary invalidates old
// results.
type Inferrer struct {
	mu    sync.Mutex
	cache map[cacheKey][]complete.Candidate
}

// New returns an empty Inferrer.
func New() *Inferrer {
	return &Inferrer{cache: map[cacheKey][]complete.Candidate{}}
}

// Complete attempts the __complete protocol for executable with the given
// completed arguments and partial token. It returns nil on any failure,
// timeout, or malformed output. Executable names containing path
// separators are refused.
func (in *Inferrer) Complete(cwd, executable string, args []string, partial string) []complete.Candidate {
	if strings.ContainsAny(executable, `/\`) {
		return nil
	}
	path, err := exec.LookPath(executable)
	if err != nil {
		return nil
	}
	cwd = normalizeCWD(cwd)
	key := cacheKey{binary: path, cwd: cwd, args: strings.Join(args, "\x00"), partial: partial}
	if info, err := statPath(path); err == nil {
		key.modTime = info
	}
	in.mu.Lock()
	if cached, ok := in.cache[key]; ok {
		in.mu.Unlock()
		return slices.Clone(cached)
	}
	in.mu.Unlock()

	cands := in.probe(cwd, path, args, partial)
	in.mu.Lock()
	if len(in.cache) >= maxCacheSize {
		in.cache = map[cacheKey][]complete.Candidate{}
	}
	in.cache[key] = cands
	in.mu.Unlock()
	return slices.Clone(cands)
}

func normalizeCWD(cwd string) string {
	if abs, err := filepath.Abs(cwd); err == nil {
		return filepath.Clean(abs)
	}
	return filepath.Clean(cwd)
}

func (in *Inferrer) probe(cwd, path string, args []string, partial string) []complete.Candidate {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	full := append([]string{"__complete"}, args...)
	full = append(full, partial)
	cmd := exec.CommandContext(ctx, path, full...)
	cmd.Dir = cwd
	var out bytes.Buffer
	cmd.Stdout = &boundedBuffer{buf: &out}
	if err := cmd.Run(); err != nil {
		return nil
	}
	return parse(out.String(), partial)
}

func parse(output, partial string) []complete.Candidate {
	rawLines := strings.Split(strings.TrimRight(output, "\n"), "\n")
	if len(rawLines) == 0 {
		return nil
	}
	last := strings.TrimSpace(rawLines[len(rawLines)-1])
	if !strings.HasPrefix(last, ":") {
		return nil
	}
	directive, err := strconv.Atoi(strings.TrimPrefix(last, ":"))
	if err != nil || directive&directiveError != 0 {
		return nil
	}
	var cands []complete.Candidate
	for _, ln := range rawLines[:len(rawLines)-1] {
		if ln == "" || strings.HasPrefix(ln, "Completion ended") {
			continue
		}
		value, desc, _ := strings.Cut(ln, "\t")
		if value == "" || !hasFoldPrefix(value, partial) {
			continue
		}
		cands = append(cands, complete.Candidate{
			Title:       value,
			Description: desc,
			Source:      complete.SourceInferred,
			Priority:    40,
		})
	}
	return cands
}

type boundedBuffer struct{ buf *bytes.Buffer }

func (b *boundedBuffer) Write(p []byte) (int, error) {
	if b.buf.Len() >= maxOutput {
		return 0, fmt.Errorf("inferred completion output exceeds %d bytes", maxOutput)
	}
	return b.buf.Write(p)
}

func statPath(path string) (int64, error) {
	info, err := os.Stat(path)
	if err != nil {
		return 0, err
	}
	return info.ModTime().UnixNano(), nil
}

func hasFoldPrefix(s, prefix string) bool {
	return len(s) >= len(prefix) && strings.EqualFold(s[:len(prefix)], prefix)
}
