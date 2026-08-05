package session

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/BurntSushi/toml"

	"github.com/rselbach/argmax/internal/complete"
)

// pathScan caches the PATH executable scan, performed once per session.
var pathScan struct {
	once  sync.Once
	names []string
}

// pathExecutables lists executable regular files on PATH.
func pathExecutables() []string {
	pathScan.once.Do(func() {
		seen := map[string]bool{}
		for _, dir := range filepath.SplitList(os.Getenv("PATH")) {
			entries, err := os.ReadDir(dir)
			if err != nil {
				continue
			}
			for _, e := range entries {
				name := e.Name()
				if seen[name] {
					continue
				}
				info, err := e.Info()
				if err != nil {
					continue
				}
				m := info.Mode()
				if m.IsRegular() && m.Perm()&0o111 != 0 {
					seen[name] = true
					pathScan.names = append(pathScan.names, name)
				}
				if m&os.ModeSymlink != 0 {
					target, err := os.Stat(filepath.Join(dir, name))
					if err == nil && target.Mode().IsRegular() && target.Mode().Perm()&0o111 != 0 {
						seen[name] = true
						pathScan.names = append(pathScan.names, name)
					}
				}
			}
		}
	})
	return pathScan.names
}

// topLevel merges shell aliases, bundled specs, and PATH executables for
// the first token. Shell aliases rank above equivalent generic sources;
// unknown executables are labeled system commands.
func (s *Session) topLevel(prefix string) []complete.Candidate {
	var cands []complete.Candidate
	for _, a := range s.aliases.Aliases() {
		if !foldPrefix(a.Name, prefix) {
			continue
		}
		cands = append(cands, complete.Candidate{
			Text:        a.Name,
			Title:       a.Name,
			Description: a.Expansion,
			Source:      complete.SourceAlias,
			Priority:    75,
			Icon:        "alias",
		})
	}
	for _, spec := range s.registry.All() {
		if !foldPrefix(spec.Name, prefix) {
			continue
		}
		pr := spec.Priority
		if pr == 0 {
			pr = 60
		}
		cands = append(cands, complete.Candidate{
			Text:        spec.Name,
			Title:       spec.Name,
			Description: spec.Description,
			Source:      complete.SourceSpec,
			Priority:    pr,
			Icon:        spec.Icon,
		})
	}
	for _, name := range pathExecutables() {
		if !foldPrefix(name, prefix) || s.registry.Lookup(name) != nil {
			continue
		}
		cands = append(cands, complete.Candidate{
			Text:        s.opts.Shell.QuoteArg(name),
			Title:       name,
			Description: "system command",
			Source:      complete.SourceSystem,
			Priority:    40,
		})
	}
	return cands
}

// toolScopePriority maps tool alias scopes to ranking priorities.
func toolScopePriority(scope string) int {
	switch scope {
	case "command", "worktree", "local":
		return 85
	case "global":
		return 70
	default: // system
		return 65
	}
}

// gitAliasCache caches discovered Git aliases briefly.
type toolAliasCache struct {
	mu      sync.Mutex
	fetched time.Time
	cwd     string
	git     []toolAlias
}

type toolAlias struct {
	name      string
	expansion string
	scope     string
}

var gitAliases toolAliasCache

// toolAliasCandidates surfaces Git and Cargo tool aliases while typing
// their second token, and expands a typed Git alias for deeper lookup.
func (s *Session) toolAliasCandidates(line, cwd string, tokens []complete.Token) []complete.Candidate {
	if len(tokens) < 2 {
		return nil
	}
	root := tokens[0].Text
	switch root {
	case "git":
		return s.gitAliasCandidates(line, cwd, tokens)
	case "cargo":
		return cargoAliasCandidates(line, cwd, tokens)
	}
	return nil
}

func (s *Session) gitAliasCandidates(line, cwd string, tokens []complete.Token) []complete.Candidate {
	aliases := discoverGitAliases(cwd)
	partial := tokens[len(tokens)-1]
	base := line[:partial.Start]
	var cands []complete.Candidate
	if len(tokens) == 2 {
		for _, a := range aliases {
			if !foldPrefix(a.name, partial.Text) {
				continue
			}
			cands = append(cands, complete.Candidate{
				Text:        base + a.name,
				Title:       a.name,
				Description: a.expansion + " (git alias)",
				Source:      complete.SourceAlias,
				Priority:    toolScopePriority(a.scope),
				Icon:        "git",
			})
		}
		return cands
	}
	// Deeper completion through a recognized alias: expand internally and
	// return completions preserving the alias spelling. Shell-style
	// aliases beginning with "!" are never traversed.
	sub := tokens[1].Text
	for _, a := range aliases {
		if a.name != sub || strings.HasPrefix(a.expansion, "!") {
			continue
		}
		expandedLine := "git " + a.expansion + line[tokens[1].Start+len(sub):]
		ctx := s.completionContext(cwd)
		expanded := s.engine.Complete(ctx, expandedLine)
		aliasPrefix := "git " + sub
		for i := range expanded {
			if strings.HasPrefix(expanded[i].Text, "git "+a.expansion) {
				expanded[i].Text = aliasPrefix + strings.TrimPrefix(expanded[i].Text, "git "+a.expansion)
			}
		}
		return expanded
	}
	return nil
}

func (s *Session) completionContext(cwd string) complete.Context {
	cfg := s.opts.Watcher.Current()
	return complete.Context{
		CWD:                    cwd,
		Shell:                  s.opts.Shell,
		HiddenFiles:            cfg.UI.HiddenFiles,
		GitFilterActiveBranch:  cfg.Git.FilterActiveBranch,
		GitDeduplicateBranches: cfg.Git.DeduplicateBranches,
	}
}

// discoverGitAliases reads aliases from effective Git configuration with
// their scopes, cached briefly per directory.
func discoverGitAliases(cwd string) []toolAlias {
	gitAliases.mu.Lock()
	defer gitAliases.mu.Unlock()
	if gitAliases.cwd == cwd && time.Since(gitAliases.fetched) < 30*time.Second {
		return gitAliases.git
	}
	out := probe(time.Second, "git", "-C", cwd, "config", "--show-scope", "--get-regexp", `^alias\.`)
	var aliases []toolAlias
	for _, ln := range strings.Split(out, "\n") {
		fields := strings.SplitN(ln, "\t", 2)
		if len(fields) != 2 {
			continue
		}
		scope := fields[0]
		name, expansion, ok := strings.Cut(fields[1], " ")
		if !ok {
			continue
		}
		aliases = append(aliases, toolAlias{
			name:      strings.TrimPrefix(name, "alias."),
			expansion: expansion,
			scope:     scope,
		})
	}
	gitAliases.cwd = cwd
	gitAliases.fetched = time.Now()
	gitAliases.git = aliases
	return aliases
}

// cargoAliasCandidates reads Cargo aliases from local ancestor and global
// Cargo configuration.
func cargoAliasCandidates(line, cwd string, tokens []complete.Token) []complete.Candidate {
	if len(tokens) != 2 {
		return nil
	}
	partial := tokens[len(tokens)-1]
	base := line[:partial.Start]
	var cands []complete.Candidate
	for _, entry := range cargoAliasFiles(cwd) {
		for name, expansion := range readCargoAliases(entry.path) {
			if !foldPrefix(name, partial.Text) {
				continue
			}
			cands = append(cands, complete.Candidate{
				Text:        base + name,
				Title:       name,
				Description: expansion + " (cargo alias)",
				Source:      complete.SourceAlias,
				Priority:    toolScopePriority(entry.scope),
				Icon:        "rust",
			})
		}
	}
	return cands
}

type cargoConfigFile struct {
	path  string
	scope string
}

func cargoAliasFiles(cwd string) []cargoConfigFile {
	var files []cargoConfigFile
	for dir := cwd; ; dir = filepath.Dir(dir) {
		for _, name := range []string{"config.toml", "config"} {
			files = append(files, cargoConfigFile{filepath.Join(dir, ".cargo", name), "local"})
		}
		if dir == filepath.Dir(dir) {
			break
		}
	}
	if home, err := os.UserHomeDir(); err == nil {
		files = append(files,
			cargoConfigFile{filepath.Join(home, ".cargo", "config.toml"), "global"},
			cargoConfigFile{filepath.Join(home, ".cargo", "config"), "global"},
		)
	}
	return files
}

func readCargoAliases(path string) map[string]string {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var parsed struct {
		Alias map[string]any `toml:"alias"`
	}
	if err := toml.Unmarshal(data, &parsed); err != nil {
		return nil
	}
	out := make(map[string]string, len(parsed.Alias))
	for name, v := range parsed.Alias {
		switch val := v.(type) {
		case string:
			out[name] = val
		case []any:
			var parts []string
			for _, p := range val {
				if s, ok := p.(string); ok {
					parts = append(parts, s)
				}
			}
			out[name] = strings.Join(parts, " ")
		}
	}
	return out
}

func foldPrefix(s, prefix string) bool {
	return len(s) >= len(prefix) && strings.EqualFold(s[:len(prefix)], prefix)
}
