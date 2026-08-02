package sources

import (
	"context"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

// Tool alias scope ranks (SRC-008).
const (
	ScopeLocal  = 85 // command/worktree/local
	ScopeGlobal = 70
	ScopeSystem = 65
)

// ToolAlias is an alias defined by a tool such as Git or Cargo.
type ToolAlias struct {
	Root      string // "git" or "cargo"
	Name      string // alias name as typed after root
	Expansion string // expansion text
	Scope     int    // 85 command/worktree/local, 70 global, 65 system
	// Shell marks Git aliases whose expansion begins with '!' (shell
	// command aliases; the engine must not traverse them).
	Shell bool
}

// GitAliases returns aliases from the effective git configuration in cwd,
// ranked by scope. It probes `git config` and falls back to parsing INI
// files when git is absent or fails.
func (s *Sources) GitAliases(cwd string) []ToolAlias {
	out, err := probeLines(context.Background(), 2*time.Second, cwd,
		"git", "config", "--show-scope", "--get-regexp", `^alias\.`)
	if err != nil {
		return s.gitAliasesINI(cwd)
	}
	var res []ToolAlias
	for _, line := range out {
		// git prints "<scope>\t<key> <value>".
		i := strings.IndexAny(line, " \t")
		if i <= 0 {
			continue
		}
		scope, rest := line[:i], strings.TrimSpace(line[i+1:])
		key, expansion, _ := strings.Cut(rest, " ")
		name, ok := strings.CutPrefix(key, "alias.")
		if !ok || name == "" || expansion == "" {
			continue
		}
		rank, ok := gitScopeRank(scope)
		if !ok {
			continue
		}
		res = append(res, ToolAlias{
			Root:      "git",
			Name:      name,
			Expansion: expansion,
			Scope:     rank,
			Shell:     strings.HasPrefix(expansion, "!"),
		})
	}
	return res
}

func gitScopeRank(scope string) (int, bool) {
	switch scope {
	case "local", "worktree", "command":
		return ScopeLocal, true
	case "global":
		return ScopeGlobal, true
	case "system":
		return ScopeSystem, true
	}
	return 0, false
}

// gitAliasesINI parses git config files directly (fallback path).
func (s *Sources) gitAliasesINI(cwd string) []ToolAlias {
	var out []ToolAlias
	if gd := findGitDir(cwd); gd != "" {
		out = append(out, parseGitConfigAliases(filepath.Join(gd, "config"), ScopeLocal)...)
		out = append(out, parseGitConfigAliases(filepath.Join(gd, "config.worktree"), ScopeLocal)...)
	}
	home, _ := os.UserHomeDir()
	out = append(out, parseGitConfigAliases(filepath.Join(home, ".gitconfig"), ScopeGlobal)...)
	xdg := os.Getenv("XDG_CONFIG_HOME")
	if xdg == "" {
		xdg = filepath.Join(home, ".config")
	}
	out = append(out, parseGitConfigAliases(filepath.Join(xdg, "git", "config"), ScopeGlobal)...)
	out = append(out, parseGitConfigAliases("/etc/gitconfig", ScopeSystem)...)
	return out
}

// findGitDir locates the .git directory governing cwd, resolving gitfiles.
func findGitDir(cwd string) string {
	if cwd == "" {
		return ""
	}
	dir := filepath.Clean(cwd)
	for {
		cand := filepath.Join(dir, ".git")
		if fi, err := os.Stat(cand); err == nil {
			if fi.IsDir() {
				return cand
			}
			if data, err := os.ReadFile(cand); err == nil {
				if p, ok := strings.CutPrefix(strings.TrimSpace(string(data)), "gitdir:"); ok {
					p = strings.TrimSpace(p)
					if !filepath.IsAbs(p) {
						p = filepath.Join(dir, p)
					}
					return p
				}
			}
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return ""
		}
		dir = parent
	}
}

// parseGitConfigAliases extracts the [alias] section from a git INI file.
func parseGitConfigAliases(path string, scope int) []ToolAlias {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []ToolAlias
	inAlias := false
	for _, raw := range strings.Split(string(data), "\n") {
		line := strings.TrimSpace(raw)
		if line == "" || strings.HasPrefix(line, "#") || strings.HasPrefix(line, ";") {
			continue
		}
		if strings.HasPrefix(line, "[") {
			section := strings.TrimSpace(strings.Trim(line, "[]"))
			section, _, _ = strings.Cut(section, " ")
			inAlias = strings.EqualFold(section, "alias")
			continue
		}
		if !inAlias {
			continue
		}
		eq := strings.IndexByte(line, '=')
		if eq <= 0 {
			continue
		}
		name := strings.TrimSpace(line[:eq])
		val := strings.TrimSpace(line[eq+1:])
		if len(val) >= 2 && val[0] == '"' && val[len(val)-1] == '"' {
			val = val[1 : len(val)-1]
		}
		if name == "" || val == "" {
			continue
		}
		out = append(out, ToolAlias{
			Root:      "git",
			Name:      name,
			Expansion: val,
			Scope:     scope,
			Shell:     strings.HasPrefix(val, "!"),
		})
	}
	return out
}

// CargoAliases returns aliases from ancestor .cargo/config.toml files
// (scope 85) and the global Cargo configuration (scope 70).
func (s *Sources) CargoAliases(cwd string) []ToolAlias {
	var out []ToolAlias
	if cwd != "" {
		dir := filepath.Clean(cwd)
		for {
			out = append(out, parseCargoAliases(filepath.Join(dir, ".cargo", "config.toml"), ScopeLocal)...)
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			dir = parent
		}
	}
	global := os.Getenv("CARGO_HOME")
	if global == "" {
		home, _ := os.UserHomeDir()
		global = filepath.Join(home, ".cargo")
	}
	out = append(out, parseCargoAliases(filepath.Join(global, "config.toml"), ScopeGlobal)...)
	return out
}

var cargoArrayValueRe = regexp.MustCompile(`"([^"]*)"|'([^']*)'`)

// parseCargoAliases hand-parses the [alias] table of a Cargo config file:
// `name = "expansion"` or the array form `name = ["x", "y"]`.
func parseCargoAliases(path string, scope int) []ToolAlias {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []ToolAlias
	inAlias := false
	for _, raw := range strings.Split(string(data), "\n") {
		line := strings.TrimSpace(raw)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		if strings.HasPrefix(line, "[") {
			inAlias = line == "[alias]"
			continue
		}
		if !inAlias {
			continue
		}
		eq := strings.IndexByte(line, '=')
		if eq <= 0 {
			continue
		}
		name := strings.Trim(strings.TrimSpace(line[:eq]), `"'`)
		v := strings.TrimSpace(line[eq+1:])
		var expansion string
		if strings.HasPrefix(v, "[") {
			var parts []string
			for _, m := range cargoArrayValueRe.FindAllStringSubmatch(v, -1) {
				if m[1] != "" {
					parts = append(parts, m[1])
				} else {
					parts = append(parts, m[2])
				}
			}
			expansion = strings.Join(parts, " ")
		} else {
			expansion = unquote(v)
		}
		if name == "" || expansion == "" {
			continue
		}
		out = append(out, ToolAlias{Root: "cargo", Name: name, Expansion: expansion, Scope: scope})
	}
	return out
}
