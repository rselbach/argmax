// Package history lazily reads shell history files (Bash, Zsh extended,
// Fish), merges in-session commands, and provides tiered fuzzy search.
package history

import (
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Entry is one history command with its recency timestamp.
type Entry struct {
	Command string
	Time    time.Time
}

// Provider serves history for one shell session. The backing file is read
// lazily on first request and re-read when its modification time advances.
// A missing history file is an empty history, never a failure.
type Provider struct {
	path   string
	format Format

	mu      sync.Mutex
	loaded  bool
	modTime time.Time
	entries []Entry // newest first, de-duplicated
	session []Entry // newest first, commands from this session
}

// Format selects the history file dialect.
type Format int

// History formats.
const (
	FormatBash Format = iota
	FormatZsh
	FormatFish
)

// NewProvider returns a lazy provider for the given file and dialect.
func NewProvider(path string, format Format) *Provider {
	return &Provider{path: path, format: format}
}

// AddSession records a command submitted during the current session,
// visible immediately even before the shell flushes its history file.
// Consecutive duplicates collapse into one row.
func (p *Provider) AddSession(command string) {
	command = strings.TrimSpace(command)
	if command == "" {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if len(p.session) > 0 && p.session[0].Command == command {
		p.session[0].Time = time.Now()
		return
	}
	p.session = append([]Entry{{Command: command, Time: time.Now()}}, p.session...)
}

// Entries returns merged session and persistent history, newest first,
// de-duplicated by exact command text keeping the newest occurrence.
func (p *Provider) Entries() []Entry {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.refresh()
	merged := make([]Entry, 0, len(p.session)+len(p.entries))
	seen := make(map[string]bool, len(p.session)+len(p.entries))
	for _, e := range p.session {
		if !seen[e.Command] {
			seen[e.Command] = true
			merged = append(merged, e)
		}
	}
	for _, e := range p.entries {
		if !seen[e.Command] {
			seen[e.Command] = true
			merged = append(merged, e)
		}
	}
	return merged
}

// refresh loads or reloads the backing file when stale. Callers hold p.mu.
func (p *Provider) refresh() {
	info, err := os.Stat(p.path)
	if err != nil {
		if !p.loaded {
			p.loaded = true
			p.entries = nil
		}
		return
	}
	if p.loaded && !info.ModTime().After(p.modTime) {
		return
	}
	data, err := os.ReadFile(p.path)
	if err != nil {
		p.loaded = true
		return
	}
	p.loaded = true
	p.modTime = info.ModTime()
	p.entries = dedupeNewestFirst(Parse(string(data), p.format))
}

// dedupeNewestFirst reverses parse order (files are oldest-first) and
// keeps the newest occurrence of each command.
func dedupeNewestFirst(parsed []Entry) []Entry {
	out := make([]Entry, 0, len(parsed))
	seen := make(map[string]bool, len(parsed))
	for i := len(parsed) - 1; i >= 0; i-- {
		e := parsed[i]
		if e.Command == "" || seen[e.Command] {
			continue
		}
		seen[e.Command] = true
		out = append(out, e)
	}
	return out
}

// Parse reads history file content in the given dialect, oldest first.
func Parse(content string, format Format) []Entry {
	switch format {
	case FormatZsh:
		return parseZsh(content)
	case FormatFish:
		return parseFish(content)
	default:
		return parseBash(content)
	}
}

// parseBash handles plain lines and `#<epoch>` timestamp lines.
func parseBash(content string) []Entry {
	var (
		out  []Entry
		when time.Time
	)
	for _, ln := range strings.Split(content, "\n") {
		if ts, ok := strings.CutPrefix(ln, "#"); ok {
			if epoch, err := strconv.ParseInt(strings.TrimSpace(ts), 10, 64); err == nil {
				when = time.Unix(epoch, 0)
				continue
			}
		}
		cmd := strings.TrimSpace(ln)
		if cmd == "" {
			continue
		}
		out = append(out, Entry{Command: cmd, Time: when})
		when = time.Time{}
	}
	return out
}

// parseZsh handles extended-history metadata `: <start>:<elapsed>;cmd` and
// backslash line continuations.
func parseZsh(content string) []Entry {
	var out []Entry
	lines := strings.Split(content, "\n")
	for i := 0; i < len(lines); i++ {
		ln := lines[i]
		for strings.HasSuffix(ln, "\\") && i+1 < len(lines) {
			i++
			ln = strings.TrimSuffix(ln, "\\") + "\n" + lines[i]
		}
		var when time.Time
		if rest, ok := strings.CutPrefix(ln, ": "); ok {
			meta, cmd, found := strings.Cut(rest, ";")
			if found {
				if start, _, ok := strings.Cut(meta, ":"); ok {
					if epoch, err := strconv.ParseInt(start, 10, 64); err == nil {
						when = time.Unix(epoch, 0)
					}
				}
				ln = cmd
			}
		}
		cmd := strings.TrimSpace(ln)
		if cmd == "" {
			continue
		}
		out = append(out, Entry{Command: cmd, Time: when})
	}
	return out
}

// parseFish handles the YAML-like `- cmd:` records with `when:` metadata.
func parseFish(content string) []Entry {
	var out []Entry
	for _, ln := range strings.Split(content, "\n") {
		if rest, ok := strings.CutPrefix(ln, "- cmd: "); ok {
			cmd := strings.TrimSpace(strings.ReplaceAll(rest, `\n`, "\n"))
			if cmd != "" {
				out = append(out, Entry{Command: cmd})
			}
			continue
		}
		trimmed := strings.TrimSpace(ln)
		if ts, ok := strings.CutPrefix(trimmed, "when: "); ok && len(out) > 0 {
			if epoch, err := strconv.ParseInt(strings.TrimSpace(ts), 10, 64); err == nil {
				out[len(out)-1].Time = time.Unix(epoch, 0)
			}
		}
	}
	return out
}
