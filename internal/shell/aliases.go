package shell

import (
	"os"
	"strings"
	"sync"
	"time"
)

// Alias is one discovered shell alias.
type Alias struct {
	Name      string
	Expansion string
}

// AliasCache re-reads alias files when their modification time changes so
// edits appear without restarting.
type AliasCache struct {
	kind Kind

	mu      sync.Mutex
	aliases []Alias
	stamps  map[string]time.Time
}

// NewAliasCache returns a cache for the given shell.
func NewAliasCache(k Kind) *AliasCache {
	return &AliasCache{kind: k, stamps: map[string]time.Time{}}
}

// Aliases returns the current alias set, refreshing stale files.
func (c *AliasCache) Aliases() []Alias {
	c.mu.Lock()
	defer c.mu.Unlock()
	files := c.kind.AliasFiles()
	if !c.stale(files) {
		return c.aliases
	}
	seen := map[string]bool{}
	var out []Alias
	for _, f := range files {
		info, err := os.Stat(f)
		if err != nil {
			c.stamps[f] = time.Time{}
			continue
		}
		c.stamps[f] = info.ModTime()
		data, err := os.ReadFile(f)
		if err != nil {
			continue
		}
		for _, a := range ParseAliases(string(data), c.kind) {
			if seen[a.Name] {
				continue
			}
			seen[a.Name] = true
			out = append(out, a)
		}
	}
	c.aliases = out
	return out
}

func (c *AliasCache) stale(files []string) bool {
	if c.aliases == nil {
		return true
	}
	for _, f := range files {
		info, err := os.Stat(f)
		var mod time.Time
		if err == nil {
			mod = info.ModTime()
		}
		if !mod.Equal(c.stamps[f]) {
			return true
		}
	}
	return false
}

// ParseAliases extracts alias definitions from shell configuration text.
// It intentionally handles only plain top-level alias lines.
func ParseAliases(content string, k Kind) []Alias {
	var out []Alias
	for _, line := range strings.Split(content, "\n") {
		line = strings.TrimSpace(line)
		rest, ok := strings.CutPrefix(line, "alias ")
		if !ok {
			continue
		}
		rest = strings.TrimSpace(strings.TrimPrefix(rest, "-g "))
		var a Alias
		if k == Fish && !strings.Contains(rest, "=") {
			// fish: alias name 'expansion'
			name, expansion, found := strings.Cut(rest, " ")
			if !found {
				continue
			}
			a = Alias{Name: name, Expansion: unquote(strings.TrimSpace(expansion))}
		} else {
			name, expansion, found := strings.Cut(rest, "=")
			if !found || name == "" || strings.ContainsAny(name, " \t'\"") {
				continue
			}
			a = Alias{Name: name, Expansion: unquote(expansion)}
		}
		if a.Name == "" || a.Expansion == "" {
			continue
		}
		out = append(out, a)
	}
	return out
}

func unquote(s string) string {
	s = strings.TrimSpace(s)
	if len(s) >= 2 {
		if (s[0] == '\'' && s[len(s)-1] == '\'') || (s[0] == '"' && s[len(s)-1] == '"') {
			return s[1 : len(s)-1]
		}
	}
	return s
}
