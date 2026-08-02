package complete

import (
	"fmt"
	"strings"
)

// Registry holds the compiled top-level command specifications.
type Registry struct {
	byName map[string]*Spec
	list   []*Spec
}

// NewRegistry indexes specs by lowercase name and alias.
func NewRegistry(specs ...*Spec) *Registry {
	r := &Registry{byName: make(map[string]*Spec, len(specs)*2), list: specs}
	for _, s := range specs {
		r.byName[strings.ToLower(s.Name)] = s
		for _, a := range s.Aliases {
			if _, exists := r.byName[strings.ToLower(a)]; !exists {
				r.byName[strings.ToLower(a)] = s
			}
		}
	}
	return r
}

// Lookup returns the spec registered under name or alias.
func (r *Registry) Lookup(name string) *Spec {
	return r.byName[strings.ToLower(name)]
}

// All returns every registered top-level spec.
func (r *Registry) All() []*Spec { return r.list }

// Validate checks the registry for duplicate names, malformed specs, and
// unreachable nodes. It is exercised in CI.
func (r *Registry) Validate() error {
	seen := map[string]bool{}
	for _, s := range r.list {
		if s.Name == "" {
			return fmt.Errorf("spec with empty name")
		}
		key := strings.ToLower(s.Name)
		if seen[key] {
			return fmt.Errorf("duplicate spec name %q", s.Name)
		}
		seen[key] = true
		if err := validateNode(s, s.Name); err != nil {
			return err
		}
	}
	return nil
}

func validateNode(s *Spec, path string) error {
	if s.Priority < 0 || s.Priority > 100 {
		return fmt.Errorf("spec %s: priority %d outside 0-100", path, s.Priority)
	}
	subs := map[string]bool{}
	for _, sub := range s.Subcommands {
		if sub.Name == "" {
			return fmt.Errorf("spec %s: subcommand with empty name", path)
		}
		key := strings.ToLower(sub.Name)
		if subs[key] {
			return fmt.Errorf("spec %s: duplicate subcommand %q", path, sub.Name)
		}
		subs[key] = true
		if err := validateNode(sub, path+" "+sub.Name); err != nil {
			return err
		}
	}
	for _, opt := range s.Options {
		if len(opt.Names) == 0 {
			return fmt.Errorf("spec %s: option with no names", path)
		}
	}
	return nil
}

// walkResult is the resolved state after traversing completed tokens.
type walkResult struct {
	node    *Spec
	posArgs []string
	// dead marks a typed token that cannot match a required subcommand;
	// deeper completion is suppressed.
	dead bool
	// pendingOption is set when the buffer ends while an option's
	// required argument is still missing: the partial token being
	// completed belongs to that option, not to the node.
	pendingOption *Option
	// path holds matched spec names for skeleton extraction.
	path []string
}

// walk traverses completed tokens from root, ignoring flags, name=value
// tokens, and known flag arguments.
func walk(root *Spec, tokens []Token) walkResult {
	res := walkResult{node: root, path: []string{root.Name}}
	for i := 0; i < len(tokens); i++ {
		tok := tokens[i].Text
		if strings.HasPrefix(tok, "-") {
			if opt := res.node.option(tok); opt != nil && opt.TakesArg && !strings.Contains(tok, "=") {
				if i == len(tokens)-1 {
					res.pendingOption = opt
					return res
				}
				i++ // skip the flag's argument
			}
			continue
		}
		if strings.Contains(tok, "=") {
			continue
		}
		if len(res.posArgs) == 0 {
			if sub := res.node.child(tok); sub != nil {
				res.node = sub
				res.path = append(res.path, sub.Name)
				continue
			}
		}
		if len(res.node.Subcommands) > 0 && res.node.Generator == nil {
			res.dead = true
			return res
		}
		res.posArgs = append(res.posArgs, tok)
	}
	return res
}

// Engine turns a typed buffer into spec-derived candidates.
type Engine struct {
	Registry *Registry
}

// Complete returns candidates for the buffer. The final token is treated
// as the partial word being completed. Top-level command completion is
// handled by the merge layer; Complete handles buffers whose first token
// resolves to a registered spec.
func (e *Engine) Complete(ctx Context, line string) []Candidate {
	tokens := Tokenize(line)
	if len(tokens) < 2 {
		return nil
	}
	partial := tokens[len(tokens)-1]
	completed := tokens[:len(tokens)-1]
	root := e.Registry.Lookup(completed[0].Text)
	if root == nil {
		return nil
	}
	res := walk(root, completed[1:])
	if res.dead {
		return nil
	}
	return e.nodeCandidates(ctx, line, res, partial)
}

func (e *Engine) nodeCandidates(ctx Context, line string, res walkResult, partial Token) []Candidate {
	var (
		out          []Candidate
		node         = res.node
		prefix       = partial.Text
		base         = line[:partial.Start]
		typingOption = strings.HasPrefix(prefix, "-")
		argsCapped   = node.MaxArgs > 0 && len(res.posArgs) >= node.MaxArgs
	)
	if res.pendingOption != nil {
		// The partial token is the option's argument: only the option's
		// own generator applies, never the node's positional values (a
		// branch-creation flag must not suggest existing branches).
		if res.pendingOption.Generator == nil {
			return nil
		}
		return materialize(res.pendingOption.Generator(ctx, nil, prefix), base, node)
	}
	if len(res.posArgs) == 0 && !typingOption {
		for _, sub := range node.Subcommands {
			if !sub.Matches(prefix) {
				continue
			}
			pr := sub.Priority
			if pr == 0 {
				pr = 30
			}
			out = append(out, Candidate{
				Text:        base + sub.Name,
				Title:       sub.Name,
				Description: sub.Description,
				Icon:        firstNonEmpty(sub.Icon, node.Icon),
				Source:      SourceSpec,
				Priority:    pr,
			})
		}
	}
	for i := range node.Options {
		opt := &node.Options[i]
		// Only options completed earlier in the line count as used; the
		// partial token being typed is the one we are completing.
		if optionUsed(base, opt) {
			continue
		}
		name := matchingOptionName(opt, prefix)
		if name == "" {
			continue
		}
		pr := 10
		if typingOption {
			pr = 80
		}
		out = append(out, Candidate{
			Text:        base + name,
			Title:       name,
			Description: opt.Description,
			Icon:        node.Icon,
			Source:      SourceSpec,
			Priority:    pr,
		})
	}
	if node.Generator != nil && !argsCapped && !typingOption {
		out = append(out, materialize(node.Generator(ctx, res.posArgs, prefix), base, node)...)
	}
	return out
}

// materialize turns raw generator candidates into full command lines.
func materialize(cands []Candidate, base string, node *Spec) []Candidate {
	out := make([]Candidate, 0, len(cands))
	for _, c := range cands {
		insert := c.Insert
		if insert == "" {
			insert = c.Title
		}
		c.Text = base + QuoteArg(insert)
		if c.Source == "" {
			c.Source = SourceSpec
		}
		if c.Priority == 0 {
			c.Priority = 30
		}
		if c.Icon == "" {
			c.Icon = node.Icon
		}
		out = append(out, c)
	}
	return out
}

// Skeleton extracts the normalized command skeleton for workflow learning,
// omitting flags and positional values. Unknown commands fall back to the
// executable name.
func (r *Registry) Skeleton(line string) string {
	tokens := Tokenize(line)
	if len(tokens) == 0 || tokens[0].Text == "" {
		return ""
	}
	root := r.Lookup(tokens[0].Text)
	if root == nil {
		return tokens[0].Text
	}
	res := walk(root, tokens[1:])
	return strings.Join(res.path, " ")
}

// ParentSkeleton returns the skeleton with its deepest element removed, or
// "" when already at the root.
func ParentSkeleton(skeleton string) string {
	i := strings.LastIndex(skeleton, " ")
	if i < 0 {
		return ""
	}
	return skeleton[:i]
}

func optionUsed(line string, opt *Option) bool {
	for _, tok := range Tokenize(line) {
		name, _, _ := strings.Cut(tok.Text, "=")
		for _, n := range opt.Names {
			if name == n {
				return true
			}
		}
	}
	return false
}

func matchingOptionName(opt *Option, prefix string) string {
	if prefix == "" || prefix == "-" || prefix == "--" {
		return opt.Names[len(opt.Names)-1]
	}
	for _, n := range opt.Names {
		if hasFoldPrefix(n, prefix) {
			return n
		}
	}
	return ""
}

func firstNonEmpty(vals ...string) string {
	for _, v := range vals {
		if v != "" {
			return v
		}
	}
	return ""
}
