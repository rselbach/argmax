package generators

import (
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/rselbach/argmax/internal/complete"
)

// npmPriorityScripts are surfaced above other package scripts.
var npmPriorityScripts = map[string]bool{
	"dev": true, "start": true, "build": true, "test": true,
	"lint": true, "preview": true, "typecheck": true, "format": true,
}

// PackageScripts reads package.json scripts for npm, pnpm, Yarn, and Bun.
// Common placeholders appear only when no manifest exists.
func PackageScripts() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		data, err := os.ReadFile(filepath.Join(ctx.CWD, "package.json"))
		if err != nil {
			return placeholders(prefix)
		}
		var manifest struct {
			Scripts map[string]string `json:"scripts"`
		}
		if err := json.Unmarshal(data, &manifest); err != nil {
			return nil
		}
		names := make([]string, 0, len(manifest.Scripts))
		for name := range manifest.Scripts {
			names = append(names, name)
		}
		sort.Strings(names)
		var out []complete.Candidate
		for _, name := range names {
			if !hasFoldPrefix(name, prefix) {
				continue
			}
			priority := 40
			if npmPriorityScripts[name] {
				priority = 70
			}
			desc := manifest.Scripts[name]
			if len(desc) > 60 {
				desc = desc[:60] + "…"
			}
			out = append(out, complete.Candidate{
				Title: name, Description: desc, Icon: "node", Priority: priority,
			})
		}
		return out
	}
}

func placeholders(prefix string) []complete.Candidate {
	var out []complete.Candidate
	for _, name := range []string{"dev", "start", "build", "test"} {
		if hasFoldPrefix(name, prefix) {
			out = append(out, complete.Candidate{
				Title: name, Description: "common script", Icon: "node", Priority: 20,
			})
		}
	}
	return out
}

// JustRecipes parses justfile recipes, using the immediately preceding
// comment as the description.
func JustRecipes() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		var data []byte
		for _, name := range []string{"justfile", "Justfile", ".justfile"} {
			if d, err := os.ReadFile(filepath.Join(ctx.CWD, name)); err == nil {
				data = d
				break
			}
		}
		if data == nil {
			return nil
		}
		var (
			out         []complete.Candidate
			seen        = map[string]bool{}
			lastComment string
		)
		for _, ln := range strings.Split(string(data), "\n") {
			trimmed := strings.TrimSpace(ln)
			if rest, ok := strings.CutPrefix(trimmed, "#"); ok {
				lastComment = strings.TrimSpace(rest)
				continue
			}
			name := recipeName(ln)
			if name == "" || seen[name] {
				lastComment = ""
				continue
			}
			if !hasFoldPrefix(name, prefix) {
				lastComment = ""
				continue
			}
			seen[name] = true
			desc := lastComment
			if desc == "" {
				desc = "just recipe"
			}
			out = append(out, complete.Candidate{
				Title: name, Description: desc, Icon: "task", Priority: 60,
			})
			lastComment = ""
		}
		return out
	}
}

// recipeName extracts a top-level recipe name from a justfile line.
func recipeName(line string) string {
	if line == "" || line[0] == ' ' || line[0] == '\t' || line[0] == '@' {
		return ""
	}
	head, _, ok := strings.Cut(line, ":")
	if !ok || strings.Contains(head, "=") {
		return ""
	}
	fields := strings.Fields(head)
	if len(fields) == 0 {
		return ""
	}
	name := fields[0]
	if strings.HasPrefix(name, "set") && len(fields) > 1 {
		return ""
	}
	for _, r := range name {
		if !isRecipeRune(r) {
			return ""
		}
	}
	return name
}

func isRecipeRune(r rune) bool {
	switch {
	case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9':
		return true
	case r == '-' || r == '_':
		return true
	}
	return false
}

// MakeTargets parses visible Makefile targets, excluding pseudo-target
// metadata such as .PHONY.
func MakeTargets() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		var data []byte
		for _, name := range []string{"Makefile", "makefile", "GNUmakefile"} {
			if d, err := os.ReadFile(filepath.Join(ctx.CWD, name)); err == nil {
				data = d
				break
			}
		}
		if data == nil {
			return nil
		}
		var out []complete.Candidate
		seen := map[string]bool{}
		for _, ln := range strings.Split(string(data), "\n") {
			if ln == "" || ln[0] == '\t' || ln[0] == '#' {
				continue
			}
			head, _, ok := strings.Cut(ln, ":")
			if !ok || strings.ContainsAny(head, "=$") {
				continue
			}
			for _, target := range strings.Fields(head) {
				if strings.HasPrefix(target, ".") || strings.ContainsAny(target, "%$") {
					continue
				}
				if seen[target] || !hasFoldPrefix(target, prefix) {
					continue
				}
				seen[target] = true
				out = append(out, complete.Candidate{
					Title: target, Description: "make target", Icon: "task", Priority: 60,
				})
			}
		}
		return out
	}
}
