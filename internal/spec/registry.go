package spec

import (
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"

	"github.com/rselbach/argmax/internal/core"
)

// Registry is the bundled command-specification registry (SPEC-001). It
// indexes top-level specs by name and alias, case-insensitively.
type Registry struct {
	roots   []*Spec
	byName  map[string]*Spec
	byAlias map[string]*Spec
	merged  []string // names merged during build (PRD 18 sanctions "find")
	issues  []error  // build-time problems reported by Validate
}

// Default returns the bundled catalog (PRD 18), built once.
var Default = sync.OnceValue(func() *Registry {
	return buildRegistry(catalog())
})

// buildRegistry indexes specs by lowercase name, merging duplicates (PRD 18:
// find is intentionally cross-listed; the runtime exposes one merged spec).
func buildRegistry(list []*Spec) *Registry {
	r := &Registry{byName: map[string]*Spec{}, byAlias: map[string]*Spec{}}
	for _, s := range list {
		if s == nil || s.Name == "" {
			r.issues = append(r.issues, errors.New("spec: entry with empty name"))
			continue
		}
		key := strings.ToLower(s.Name)
		if existing, dup := r.byName[key]; dup {
			mergeSpec(existing, s)
			r.merged = append(r.merged, s.Name)
			continue
		}
		r.byName[key] = s
		r.roots = append(r.roots, s)
	}
	for _, s := range r.roots {
		for _, a := range s.Aliases {
			key := strings.ToLower(a)
			if _, clash := r.byName[key]; clash {
				r.issues = append(r.issues, fmt.Errorf("spec: alias %q of %q collides with a command name", a, s.Name))
				continue
			}
			if prev, clash := r.byAlias[key]; clash {
				r.issues = append(r.issues, fmt.Errorf("spec: alias %q of %q already used by %q", a, s.Name, prev.Name))
				continue
			}
			r.byAlias[key] = s
		}
	}
	return r
}

// mergeSpec folds src into dst, combining aliases, options, and subcommands
// and preferring dst's scalar fields unless dst left them unset.
func mergeSpec(dst, src *Spec) {
	dst.Aliases = append(dst.Aliases, src.Aliases...)
	for _, o := range src.Options {
		if dst.findOption(o.Names[0]) == nil {
			dst.Options = append(dst.Options, o)
		}
	}
	for _, sub := range src.Subcommands {
		if existing := dst.findSub(sub.Name); existing != nil {
			mergeSpec(existing, sub)
		} else {
			dst.Subcommands = append(dst.Subcommands, sub)
		}
	}
	if dst.Description == "" {
		dst.Description = src.Description
	}
	if dst.Icon == "" {
		dst.Icon = src.Icon
	}
	if dst.Generator == "" {
		dst.Generator = src.Generator
	}
	if dst.MaxArgs == 0 {
		dst.MaxArgs = src.MaxArgs
	}
	if dst.Priority == 0 {
		dst.Priority = src.Priority
	}
}

// Lookup returns the spec for name or one of its aliases, case-insensitively.
func (r *Registry) Lookup(name string) *Spec {
	key := strings.ToLower(name)
	if s, ok := r.byName[key]; ok {
		return s
	}
	return r.byAlias[key]
}

// All returns every top-level spec, sorted by name.
func (r *Registry) All() []*Spec {
	out := make([]*Spec, len(r.roots))
	copy(out, r.roots)
	sort.Slice(out, func(i, j int) bool {
		return strings.ToLower(out[i].Name) < strings.ToLower(out[j].Name)
	})
	return out
}

// Names returns the sorted distinct command names.
func (r *Registry) Names() []string {
	out := make([]string, len(r.roots))
	for i, s := range r.roots {
		out[i] = s.Name
	}
	sort.Slice(out, func(i, j int) bool { return strings.ToLower(out[i]) < strings.ToLower(out[j]) })
	return out
}

// TopLevel returns spec candidates whose name or alias has partial as a
// case-insensitive prefix (SPEC-003). Confidence 90, Source core.SourceSpec,
// base Priority from spec (default 60 per PRD 10.1), Icon from spec.
func (r *Registry) TopLevel(partial string) []core.Suggestion {
	var out []core.Suggestion
	for _, s := range r.All() {
		if partial != "" && !prefixFoldAny(partial, s.Name, s.Aliases) {
			continue
		}
		out = append(out, core.Suggestion{
			Text:        s.Name,
			Description: s.Description,
			Icon:        s.Icon,
			Source:      core.SourceSpec,
			Confidence:  90,
			Priority:    basePriority(s.Priority, 60),
		})
	}
	return out
}

// StaticCandidates returns subcommand and option candidates for the active
// position of res (SPEC-006/007):
//   - subcommands prefix-matching Partial (skipped when res.MaxedOut or
//     res.Dead): Confidence 90, Source spec, default author priority 30
//     (PRD 10.1)
//   - options: any option already present in the line (any of its Names) is
//     excluded; Priority 10 while typing a normal argument, 80 when
//     res.Dash (PRD 10.1); filtered by Partial prefix when res.Dash;
//     Confidence 80, Source spec
//
// Text is res.LinePrefix + candidate value, preserving the user's typed
// text (SPEC-003).
func (r *Registry) StaticCandidates(res Result) []core.Suggestion {
	if res.Root == nil || res.Node == nil {
		return nil
	}
	node := res.Node
	icon := node.Icon
	if icon == "" {
		icon = res.Root.Icon
	}

	var out []core.Suggestion
	if !res.MaxedOut && !res.Dead {
		for _, sub := range node.Subcommands {
			if !prefixFoldAny(res.Partial, sub.Name, sub.Aliases) {
				continue
			}
			subIcon := sub.Icon
			if subIcon == "" {
				subIcon = icon
			}
			out = append(out, core.Suggestion{
				Text:        res.LinePrefix + sub.Name,
				Description: sub.Description,
				Icon:        subIcon,
				Source:      core.SourceSpec,
				Confidence:  90,
				Priority:    basePriority(sub.Priority, 30),
			})
		}
	}

	used := r.usedOptions(res)
	for i := range node.Options {
		o := &node.Options[i]
		if used[o] {
			continue
		}
		var name string
		if res.Dash {
			name = optionPrefixMatch(o, res.Partial)
			if name == "" {
				continue
			}
		} else {
			name = displayName(o)
		}
		priority := 10
		if res.Dash {
			priority = 80
		}
		out = append(out, core.Suggestion{
			Text:        res.LinePrefix + name,
			Description: o.Description,
			Icon:        icon,
			Source:      core.SourceSpec,
			Confidence:  80,
			Priority:    priority,
		})
	}
	return out
}

// usedOptions reports which options of the active chain are already present
// in the line (SPEC-006), considering completed tokens only (the Partial
// token is still being typed and does not count as present).
func (r *Registry) usedOptions(res Result) map[*Option]bool {
	used := map[*Option]bool{}
	tokens, _ := tokenize(res.Line)
	for _, tok := range tokens[:len(tokens)-1] {
		if !strings.HasPrefix(tok, "-") {
			continue
		}
		if o := visibleOption(res.chain, tok); o != nil {
			used[o] = true
		}
	}
	return used
}

// GeneratorRequest extracts the dynamic-generator invocation for res, if
// any (SPEC-008): the active node's Generator or, when the Partial is the
// argument of a known option that TakesArg with an ArgGenerator (including
// the res.Dash case where the partial exactly names such an option), that
// generator. args are the completed positional arguments at the active
// node; the engine pairs them with res.Partial and the resolved root
// command when invoking the sources package. Merging generator values with
// file completion (scp/rsync, git checkout) is the engine's job.
func (r *Registry) GeneratorRequest(res Result) (id string, args []string, ok bool) {
	if res.Root == nil || res.Node == nil || res.Dead {
		return "", nil, false
	}
	// The partial is the argument of a known option: use that option's
	// generator, or nothing (e.g. the new-branch name of `git checkout -b`
	// suppresses existing-branch suggestions, PRD 9.8).
	if res.optArg != nil {
		if res.optArg.ArgGenerator == "" {
			return "", nil, false
		}
		return res.optArg.ArgGenerator, res.Args, true
	}
	if res.Dash {
		// The partial exactly names an option taking a generated argument
		// (e.g. `ssh -i` with the cursor still on the option token).
		for i := len(res.chain) - 1; i >= 0; i-- {
			for j := range res.chain[i].Options {
				o := &res.chain[i].Options[j]
				if !o.TakesArg || o.ArgGenerator == "" {
					continue
				}
				for _, n := range o.Names {
					if res.Partial == n {
						return o.ArgGenerator, res.Args, true
					}
				}
			}
		}
		return "", nil, false
	}
	if res.MaxedOut || res.Node.Generator == "" {
		return "", nil, false
	}
	return res.Node.Generator, res.Args, true
}

// Skeleton extracts the normalized command skeleton for workflow learning
// (SPEC-010): "root sub sub" using the registered tree (canonical names,
// aliases resolved), omitting flags and positional values. Unknown commands
// fall back to the bare executable name.
func (r *Registry) Skeleton(line string) string {
	tokens, _ := tokenize(line)
	if n := len(tokens); n > 0 && tokens[n-1] == "" {
		tokens = tokens[:n-1] // trailing "ready for next argument" token
	}
	if len(tokens) == 0 || tokens[0] == "" {
		return ""
	}
	root := r.Lookup(tokens[0])
	if root == nil {
		return tokens[0]
	}
	parts := []string{root.Name}
	node := root
	chain := []*Spec{root}
	skipArg := false
	for _, tok := range tokens[1:] {
		if skipArg {
			skipArg = false
			continue
		}
		if strings.HasPrefix(tok, "-") {
			if o := visibleOption(chain, tok); o != nil && o.TakesArg && !strings.Contains(tok, "=") {
				skipArg = true
			}
			continue
		}
		if isNameValue(tok) {
			continue
		}
		if sub := node.findSub(tok); sub != nil {
			node = sub
			chain = append(chain, sub)
			parts = append(parts, sub.Name)
		}
	}
	return strings.Join(parts, " ")
}

// Validate checks the registry (SPEC-011): duplicate names, alias
// collisions, malformed specs (empty name, self-reference, cycles), and
// unreachable nodes. PRD 18 intentionally cross-lists find; that merge is
// not an error.
func (r *Registry) Validate() error {
	var errs []error
	errs = append(errs, r.issues...)

	seen := map[string]string{}
	for _, s := range r.roots {
		key := strings.ToLower(s.Name)
		if prev, dup := seen[key]; dup {
			errs = append(errs, fmt.Errorf("spec: duplicate command %q (also %q)", s.Name, prev))
			continue
		}
		seen[key] = s.Name
		if s.Icon != "" && !canonicalIcons[s.Icon] {
			errs = append(errs, fmt.Errorf("spec: %q has non-canonical icon %q", s.Name, s.Icon))
		}
		errs = append(errs, validateNode(s)...)
	}
	return errors.Join(errs...)
}

// validateNode walks a spec tree checking for empty names, self-reference,
// cycles, duplicate sibling names/aliases, and non-canonical icons.
func validateNode(root *Spec) []error {
	var errs []error
	onPath := map[*Spec]bool{root: true}
	var walk func(n *Spec, path string)
	walk = func(n *Spec, path string) {
		if n.Name == "" {
			errs = append(errs, fmt.Errorf("spec: %s has a subcommand with empty name", path))
		}
		if n.Icon != "" && !canonicalIcons[n.Icon] {
			errs = append(errs, fmt.Errorf("spec: %s/%s has non-canonical icon %q", path, n.Name, n.Icon))
		}
		siblingNames := map[string]bool{}
		siblingAliases := map[string]string{}
		for _, sub := range n.Subcommands {
			key := strings.ToLower(sub.Name)
			if siblingNames[key] {
				errs = append(errs, fmt.Errorf("spec: %s/%s has duplicate subcommand %q", path, n.Name, sub.Name))
			}
			siblingNames[key] = true
			for _, a := range sub.Aliases {
				ak := strings.ToLower(a)
				if siblingNames[ak] {
					errs = append(errs, fmt.Errorf("spec: alias %q of %s/%s/%s collides with a sibling name", a, path, n.Name, sub.Name))
				}
				if prev, clash := siblingAliases[ak]; clash {
					errs = append(errs, fmt.Errorf("spec: alias %q of %s/%s/%s already used by sibling %s", a, path, n.Name, sub.Name, prev))
				}
				siblingAliases[ak] = sub.Name
			}
		}
		for _, sub := range n.Subcommands {
			switch {
			case sub == n:
				errs = append(errs, fmt.Errorf("spec: %s/%s is its own subcommand", path, n.Name))
			case onPath[sub]:
				errs = append(errs, fmt.Errorf("spec: cycle through %s/%s/%s", path, n.Name, sub.Name))
			default:
				onPath[sub] = true
				walk(sub, path+"/"+n.Name)
				delete(onPath, sub)
			}
		}
	}
	walk(root, root.Name)
	return errs
}

// basePriority applies the PRD 10.1 default when the spec carries no
// explicit priority.
func basePriority(p, def int) int {
	if p <= 0 {
		return def
	}
	if p > 100 {
		return 100
	}
	return p
}

// prefixFoldAny reports whether name or any alias has partial as a
// case-insensitive prefix.
func prefixFoldAny(partial, name string, aliases []string) bool {
	if hasPrefixFold(name, partial) {
		return true
	}
	for _, a := range aliases {
		if hasPrefixFold(a, partial) {
			return true
		}
	}
	return false
}

func hasPrefixFold(s, prefix string) bool {
	return len(s) >= len(prefix) && strings.EqualFold(s[:len(prefix)], prefix)
}

// optionPrefixMatch returns the first option name that has partial as a
// case-insensitive prefix (SPEC-003), or "".
func optionPrefixMatch(o *Option, partial string) string {
	for _, n := range o.Names {
		if hasPrefixFold(n, partial) {
			return n
		}
	}
	return ""
}

// displayName picks the name shown when suggesting an option without a
// dash-partial: the first long ("--") name, else the first name.
func displayName(o *Option) string {
	for _, n := range o.Names {
		if strings.HasPrefix(n, "--") {
			return n
		}
	}
	return o.Names[0]
}
