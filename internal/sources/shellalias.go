package sources

import (
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Alias is a shell alias discovered from shell startup files.
type Alias struct {
	Name      string
	Expansion string
}

// aliasFileCache caches one parsed alias file by modification time.
type aliasFileCache struct {
	mtime   time.Time
	aliases []Alias
}

// ShellAliases returns aliases defined by the given shell's startup files
// ("bash", "zsh", or "fish"). Results are cached per file and re-read when
// file modification times change (SRC-003). Missing files are fine.
func (s *Sources) ShellAliases(sh string) []Alias {
	files, parse := aliasFiles(sh)
	if len(files) == 0 {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []Alias
	for _, f := range files {
		fi, err := os.Stat(f)
		if err != nil {
			continue
		}
		c, ok := s.aliasCache[f]
		if !ok || !c.mtime.Equal(fi.ModTime()) {
			c = &aliasFileCache{mtime: fi.ModTime(), aliases: parse(f)}
			s.aliasCache[f] = c
		}
		out = append(out, c.aliases...)
	}
	return out
}

// aliasFiles returns the startup file candidates and the matching parser
// for the shell (SH-007).
func aliasFiles(sh string) ([]string, func(string) []Alias) {
	home, _ := os.UserHomeDir()
	switch sh {
	case "bash":
		return []string{
			filepath.Join(home, ".bashrc"),
			filepath.Join(home, ".bash_profile"),
			filepath.Join(home, ".bash_aliases"),
		}, parseAliasFile
	case "zsh":
		zdot := os.Getenv("ZDOTDIR")
		if zdot == "" {
			zdot = home
		}
		return []string{
			filepath.Join(zdot, ".zshrc"),
			filepath.Join(zdot, ".zprofile"),
			filepath.Join(zdot, ".zshenv"),
		}, parseAliasFile
	case "fish":
		base := os.Getenv("XDG_CONFIG_HOME")
		if base == "" {
			base = filepath.Join(home, ".config")
		}
		return []string{filepath.Join(base, "fish", "config.fish")}, parseFishAliasFile
	}
	return nil, nil
}

// parseAliasFile parses bash/zsh `alias name=value` lines. Malformed lines
// are skipped.
func parseAliasFile(path string) []Alias {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []Alias
	for _, line := range strings.Split(string(data), "\n") {
		if a, ok := parseAliasLine(line); ok {
			out = append(out, a)
		}
	}
	return out
}

// parseAliasLine handles `alias name='x'`, `alias name="x"`,
// `alias name=x`, and zsh global aliases `alias -g name=...`.
func parseAliasLine(line string) (Alias, bool) {
	line = strings.TrimSpace(line)
	if !strings.HasPrefix(line, "alias ") {
		return Alias{}, false
	}
	rest := strings.TrimSpace(line[len("alias "):])
	if strings.HasPrefix(rest, "-g ") {
		rest = strings.TrimSpace(rest[len("-g "):])
	}
	eq := strings.IndexByte(rest, '=')
	if eq <= 0 {
		return Alias{}, false
	}
	name := rest[:eq]
	val := unquote(strings.TrimSpace(rest[eq+1:]))
	if !validAliasName(name) || val == "" {
		return Alias{}, false
	}
	return Alias{Name: name, Expansion: val}, true
}

// parseFishAliasFile parses fish `alias name 'x'`, `alias name="x"`, and
// `alias name=x` forms.
func parseFishAliasFile(path string) []Alias {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []Alias
	for _, line := range strings.Split(string(data), "\n") {
		if a, ok := parseFishAliasLine(line); ok {
			out = append(out, a)
		}
	}
	return out
}

func parseFishAliasLine(line string) (Alias, bool) {
	line = strings.TrimSpace(line)
	if !strings.HasPrefix(line, "alias ") {
		return Alias{}, false
	}
	rest := strings.TrimSpace(line[len("alias "):])
	if rest == "" || strings.HasPrefix(rest, "-") {
		return Alias{}, false
	}
	sp := strings.IndexAny(rest, " \t")
	eq := strings.IndexByte(rest, '=')
	var name, val string
	switch {
	case eq > 0 && (sp == -1 || eq < sp):
		name, val = rest[:eq], rest[eq+1:]
	case sp > 0:
		name, val = rest[:sp], rest[sp+1:]
	default:
		return Alias{}, false
	}
	val = unquote(strings.TrimSpace(val))
	if !validAliasName(name) || val == "" {
		return Alias{}, false
	}
	return Alias{Name: name, Expansion: val}, true
}

func validAliasName(n string) bool {
	return n != "" && !strings.ContainsAny(n, " \t'\"=")
}

// unquote strips one pair of surrounding quotes; unquoted values end at the
// first whitespace.
func unquote(v string) string {
	if v == "" {
		return ""
	}
	if v[0] == '\'' || v[0] == '"' {
		if idx := strings.IndexByte(v[1:], v[0]); idx >= 0 {
			return v[1 : 1+idx]
		}
		return v
	}
	if i := strings.IndexAny(v, " \t"); i >= 0 {
		return v[:i]
	}
	return v
}
