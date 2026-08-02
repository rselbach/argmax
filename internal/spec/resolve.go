package spec

import "strings"

// Result describes the completion position for a command line.
type Result struct {
	Line       string   // original line
	LinePrefix string   // line up to the start of the Partial token (ends with the separating space, or the whole line minus partial). Candidate text = LinePrefix + value.
	Root       *Spec    // matched root spec, nil when the command is unknown
	Node       *Spec    // active completion node after traversal (nil when Root is nil)
	NodePath   []string // command/subcommand names from root to Node
	Args       []string // completed positional args at the active node
	Partial    string   // last token being completed; "" when ready for the next argument
	Dash       bool     // Partial begins with '-'
	MaxedOut   bool     // positive MaxArgs reached (SPEC-005): suggest nothing more positional
	Dead       bool     // a typed token cannot match a required subcommand (SPEC-009)

	chain  []*Spec // nodes from root to Node, for option lookup up the tree
	optArg *Option // non-nil when Partial is the argument of a known TakesArg option
}

// Resolve tokenizes line and traverses the registry (SPEC-003/004/009):
//   - case-insensitive prefix matching for command/subcommand/alias names
//   - while identifying the active node, skip tokens that are flags (start
//     with '-'), `name=value` tokens, and the argument of a known option
//     that TakesArg
//   - a token that exactly (case-insensitively) matches a subcommand name
//     or alias descends; a token that is only a prefix of a subcommand while
//     it is the LAST token is the Partial (subcommand completion position);
//     a non-matching token when the node REQUIRES a subcommand (has
//     subcommands and no positional args accepted yet) sets Dead
//   - MaxedOut when the active node has MaxArgs > 0 and len(Args) >= MaxArgs
//
// Resolve only resolves the static tree. Merging file/directory completion
// with generator results (e.g. scp/rsync completing both hosts and files)
// is the completion engine's job, not this package's.
func (r *Registry) Resolve(line string) Result {
	res := Result{Line: line}
	tokens, starts := tokenize(line)
	last := len(tokens) - 1
	res.Partial = tokens[last]
	res.LinePrefix = line[:starts[last]]
	res.Dash = strings.HasPrefix(res.Partial, "-")

	root := r.Lookup(tokens[0])
	if root == nil {
		return res // Root/Node nil, Dead false: unknown command
	}
	res.Root = root
	node := root
	res.Node = node
	res.NodePath = []string{root.Name}
	res.chain = []*Spec{root}

	skipArg := false
	for i := 1; i < last; i++ {
		tok := tokens[i]
		if skipArg { // consumed as a known option's argument (SPEC-004)
			skipArg = false
			continue
		}
		if strings.HasPrefix(tok, "-") {
			if o := visibleOption(res.chain, tok); o != nil && o.TakesArg && !strings.Contains(tok, "=") {
				if i+1 == last {
					res.optArg = o // Partial is this option's argument
				} else {
					skipArg = true
				}
			}
			continue
		}
		if isNameValue(tok) {
			continue
		}
		if sub := node.findSub(tok); sub != nil {
			node = sub
			res.Node = sub
			res.NodePath = append(res.NodePath, sub.Name)
			res.chain = append(res.chain, sub)
			res.Args = nil
			continue
		}
		// Non-matching token. When the node requires a subcommand the line
		// can never become valid: suppress deeper completions (SPEC-009).
		if len(node.Subcommands) > 0 && len(res.Args) == 0 {
			res.Dead = true
			return res
		}
		res.Args = append(res.Args, tok)
	}
	res.MaxedOut = node.MaxArgs > 0 && len(res.Args) >= node.MaxArgs
	return res
}

// isNameValue reports whether tok is a `name=value` token (SPEC-004).
func isNameValue(tok string) bool {
	return strings.IndexByte(tok, '=') > 0
}
