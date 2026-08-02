package sources

import (
	"bytes"
	"context"
	"os/exec"
	"strings"
	"time"
)

// maxProbeOutput caps captured probe output (PRD 11.3).
const maxProbeOutput = 256 * 1024

// limitBuffer is an io.Writer that retains at most maxProbeOutput bytes.
// Writes never block or error once the cap is reached so a verbose probe
// cannot deadlock on a full pipe.
type limitBuffer struct {
	buf bytes.Buffer
}

func (w *limitBuffer) Write(p []byte) (int, error) {
	remaining := maxProbeOutput - w.buf.Len()
	if remaining > 0 {
		if len(p) > remaining {
			w.buf.Write(p[:remaining])
		} else {
			w.buf.Write(p)
		}
	}
	return len(p), nil
}

// probe runs name with args under a timeout, optionally in dir, and returns
// capped stdout. Any failure (missing binary, non-zero exit, timeout) is
// returned as an error; callers degrade to nil suggestions.
func probe(ctx context.Context, timeout time.Duration, dir, name string, args ...string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, name, args...)
	if dir != "" {
		cmd.Dir = dir
	}
	out := &limitBuffer{}
	cmd.Stdout = out
	if err := cmd.Run(); err != nil {
		return nil, err
	}
	return out.buf.Bytes(), nil
}

// probeLines runs probe and splits the output into non-empty trimmed lines.
func probeLines(ctx context.Context, timeout time.Duration, dir, name string, args ...string) ([]string, error) {
	out, err := probe(ctx, timeout, dir, name, args...)
	if err != nil {
		return nil, err
	}
	return splitLines(out), nil
}

func splitLines(out []byte) []string {
	var lines []string
	for _, line := range strings.Split(string(out), "\n") {
		line = strings.TrimRight(line, "\r")
		if strings.TrimSpace(line) == "" {
			continue
		}
		lines = append(lines, line)
	}
	return lines
}
