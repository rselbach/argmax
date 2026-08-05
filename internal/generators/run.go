// Package generators implements file completion and the dynamic command
// providers (Git, Docker, SSH, scripts, tasks, packages, processes,
// environment). Probes run with argument arrays, hard deadlines, and
// bounded output, and return no candidates on failure.
package generators

import (
	"bytes"
	"context"
	"os/exec"
	"strings"
	"time"
)

const (
	// defaultTimeout bounds command-specific dynamic generators.
	defaultTimeout = 5 * time.Second
	// packageTimeout bounds installed-system-package enumeration.
	packageTimeout = 8 * time.Second
	// maxProbeOutput bounds captured probe output.
	maxProbeOutput = 1 << 20
)

// run executes a probe with a deadline and bounded output. It returns the
// trimmed stdout, or "" on any failure.
func run(dir string, timeout time.Duration, name string, args ...string) string {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir = dir
	var out bytes.Buffer
	cmd.Stdout = &limitWriter{w: &out, n: maxProbeOutput}
	if err := cmd.Run(); err != nil {
		return ""
	}
	return strings.TrimRight(out.String(), "\n")
}

type limitWriter struct {
	w *bytes.Buffer
	n int
}

func (l *limitWriter) Write(p []byte) (int, error) {
	if l.n <= 0 {
		return len(p), nil // discard beyond the cap, keep the probe happy
	}
	if len(p) > l.n {
		l.w.Write(p[:l.n])
		l.n = 0
		return len(p), nil
	}
	l.n -= len(p)
	return l.w.Write(p)
}

// lines splits probe output into non-empty lines.
func lines(out string) []string {
	if out == "" {
		return nil
	}
	var res []string
	for _, ln := range strings.Split(out, "\n") {
		if ln = strings.TrimRight(ln, "\r"); ln != "" {
			res = append(res, ln)
		}
	}
	return res
}

// hasFoldPrefix reports a case-insensitive prefix match.
func hasFoldPrefix(s, prefix string) bool {
	return len(s) >= len(prefix) && strings.EqualFold(s[:len(prefix)], prefix)
}
