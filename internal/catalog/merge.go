package catalog

import (
	"strings"

	"github.com/rselbach/argmax/internal/complete"
)

// enrichSpec deep-merges a corpus spec into a hand-tuned one. The
// hand-tuned node wins wherever it says something — generators, curated
// descriptions, priorities — and the corpus fills what it omits: missing
// flags with their descriptions, missing subcommands, argument sources.
// This is what lets `go build -` list the full flag set while `git
// checkout` keeps its branch generator.
func enrichSpec(dst, src *complete.Spec) {
	if dst.Description == "" {
		dst.Description = src.Description
	}
	if dst.Icon == "" {
		dst.Icon = src.Icon
	}
	if dst.Generator == nil {
		dst.Generator = src.Generator
	}
	if dst.MaxArgs == 0 {
		dst.MaxArgs = src.MaxArgs
	}

	knownAlias := map[string]bool{}
	for _, a := range dst.Aliases {
		knownAlias[strings.ToLower(a)] = true
	}
	for _, a := range src.Aliases {
		if !knownAlias[strings.ToLower(a)] && !strings.EqualFold(a, dst.Name) {
			knownAlias[strings.ToLower(a)] = true
			dst.Aliases = append(dst.Aliases, a)
		}
	}

	haveOption := map[string]bool{}
	for _, o := range dst.Options {
		for _, n := range o.Names {
			haveOption[n] = true
		}
	}
	for _, o := range src.Options {
		conflict := false
		for _, n := range o.Names {
			if haveOption[n] {
				conflict = true
				break
			}
		}
		if conflict {
			continue
		}
		for _, n := range o.Names {
			haveOption[n] = true
		}
		dst.Options = append(dst.Options, o)
	}

	haveSub := map[string]*complete.Spec{}
	for _, sub := range dst.Subcommands {
		haveSub[strings.ToLower(sub.Name)] = sub
		for _, a := range sub.Aliases {
			haveSub[strings.ToLower(a)] = sub
		}
	}
	for _, sub := range src.Subcommands {
		if existing, ok := haveSub[strings.ToLower(sub.Name)]; ok {
			enrichSpec(existing, sub)
			continue
		}
		haveSub[strings.ToLower(sub.Name)] = sub
		for _, a := range sub.Aliases {
			if _, taken := haveSub[strings.ToLower(a)]; taken {
				sub.Aliases = removeAlias(sub.Aliases, a)
				continue
			}
			haveSub[strings.ToLower(a)] = sub
		}
		dst.Subcommands = append(dst.Subcommands, sub)
	}
	normalizeAliases(dst.Subcommands)
}

// normalizeAliases gives canonical sibling names precedence, then keeps
// the first claim on each imported alias.
func normalizeAliases(specs []*complete.Spec) {
	claimed := make(map[string]bool, len(specs)*2)
	for _, spec := range specs {
		claimed[strings.ToLower(spec.Name)] = true
	}
	for _, spec := range specs {
		aliases := spec.Aliases[:0]
		for _, alias := range spec.Aliases {
			key := strings.ToLower(alias)
			if alias == "" || claimed[key] {
				continue
			}
			claimed[key] = true
			aliases = append(aliases, alias)
		}
		spec.Aliases = aliases
	}
}

func removeAlias(aliases []string, alias string) []string {
	out := aliases[:0]
	for _, a := range aliases {
		if a != alias {
			out = append(out, a)
		}
	}
	return out
}
