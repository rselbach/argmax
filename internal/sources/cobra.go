package sources

import (
	"context"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/core"
)

const (
	// cobraTimeout is the hard deadline for Cobra inference (COBRA-003).
	cobraTimeout = 300 * time.Millisecond
	// cobraCacheSize bounds the inference result cache (COBRA-005).
	cobraCacheSize = 128
)

// CobraComplete attempts the Cobra `__complete` protocol for executables
// without a bundled spec (PRD 9.9). Failures and timeouts return nil.
func (s *Sources) CobraComplete(ctx context.Context, name string, args []string, partial string) []core.Suggestion {
	// COBRA-004: refuse path separators so completion cannot execute
	// arbitrary paths.
	if name == "" || strings.ContainsAny(name, `/\`) {
		return nil
	}
	path, err := exec.LookPath(name)
	if err != nil {
		return nil
	}
	fi, err := os.Stat(path)
	if err != nil {
		return nil
	}
	// Cache key: resolved binary identity (path + mtime + size), args,
	// partial. A replaced binary invalidates naturally (COBRA-005).
	key := strings.Join([]string{
		path,
		strconv.FormatInt(fi.ModTime().UnixNano(), 10),
		strconv.FormatInt(fi.Size(), 10),
		strings.Join(args, "\x00"),
		partial,
	}, "|")

	s.mu.Lock()
	if cached, ok := s.cobraCache[key]; ok {
		s.mu.Unlock()
		return cached
	}
	s.mu.Unlock()

	if ctx == nil {
		ctx = context.Background()
	}
	argv := make([]string, 0, len(args)+2)
	argv = append(argv, "__complete")
	argv = append(argv, args...)
	argv = append(argv, partial)
	// cmd.Dir stays unset: inherit the current directory.
	out, err := probe(ctx, cobraTimeout, "", path, argv...)
	if err != nil {
		return nil
	}
	res := parseCobraOutput(out, partial)
	if res == nil {
		res = []core.Suggestion{}
	}

	s.mu.Lock()
	for len(s.cobraCache) >= cobraCacheSize && len(s.cobraOrder) > 0 {
		delete(s.cobraCache, s.cobraOrder[0])
		s.cobraOrder = s.cobraOrder[1:]
	}
	s.cobraCache[key] = res
	s.cobraOrder = append(s.cobraOrder, key)
	s.mu.Unlock()
	return res
}

// parseCobraOutput parses `value<TAB>description` lines up to the directive
// line. Malformed lines are ignored; the error directive (:1) yields nil
// (COBRA-002). Values are filtered case-insensitively by partial and Cobra
// internal values starting with '_' are skipped (COBRA-006).
func parseCobraOutput(out []byte, partial string) []core.Suggestion {
	var res []core.Suggestion
	lower := strings.ToLower(partial)
	for _, line := range strings.Split(string(out), "\n") {
		line = strings.TrimRight(line, "\r")
		if strings.HasPrefix(line, ":") {
			if strings.TrimSpace(line[1:]) == "1" {
				return nil
			}
			break
		}
		if line == "" {
			continue
		}
		value, desc, _ := strings.Cut(line, "\t")
		if value == "" || strings.HasPrefix(value, "_") {
			continue
		}
		if lower != "" && !strings.HasPrefix(strings.ToLower(value), lower) {
			continue
		}
		res = append(res, core.Suggestion{
			Text:        value,
			Description: desc,
			Icon:        "misc",
			Source:      core.SourceInferred,
			Confidence:  65,
			Priority:    -1,
		})
	}
	return res
}
