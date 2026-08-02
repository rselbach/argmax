package complete

import "strings"

// Context carries the environment a generator may need. Generators inherit
// the child shell's working directory, never the wrapper's.
type Context struct {
	// CWD is the child shell's reported working directory.
	CWD string
	// HiddenFiles includes dot-prefixed entries in path completion.
	HiddenFiles bool
	// GitFilterActiveBranch omits the active branch for checkout/switch.
	GitFilterActiveBranch bool
	// GitDeduplicateBranches merges local and remote-tracking names.
	GitDeduplicateBranches bool
}

// Generator produces live positional values for a spec node. args holds the
// completed positional arguments already typed at this node and prefix the
// partial token being completed. Generators must return no candidates on
// failure and never block input forwarding.
type Generator func(ctx Context, args []string, prefix string) []Candidate

// Spec describes one command or subcommand.
type Spec struct {
	Name        string
	Aliases     []string
	Description string
	Icon        string
	// Priority is an author-defined static priority from 0-100; 0 = unset.
	Priority int
	// MaxArgs limits positional arguments; non-positive means unlimited.
	MaxArgs     int
	Options     []Option
	Subcommands []*Spec
	Generator   Generator
}

// Option is one flag of a spec node.
type Option struct {
	// Names lists the option spellings, such as ["-m", "--message"].
	Names       []string
	Description string
	// TakesArg marks options whose next token is the option's argument.
	TakesArg bool
	// Generator produces values for the option's argument while the user
	// is completing it.
	Generator Generator
}

// Matches reports a case-insensitive name or alias prefix match.
func (s *Spec) Matches(prefix string) bool {
	if hasFoldPrefix(s.Name, prefix) {
		return true
	}
	for _, a := range s.Aliases {
		if hasFoldPrefix(a, prefix) {
			return true
		}
	}
	return false
}

// child returns the subcommand exactly matching token (case-insensitive),
// by name or alias.
func (s *Spec) child(token string) *Spec {
	for _, sub := range s.Subcommands {
		if strings.EqualFold(sub.Name, token) {
			return sub
		}
		for _, a := range sub.Aliases {
			if strings.EqualFold(a, token) {
				return sub
			}
		}
	}
	return nil
}

// option returns the option whose name exactly matches token.
func (s *Spec) option(token string) *Option {
	name, _, _ := strings.Cut(token, "=")
	for i := range s.Options {
		for _, n := range s.Options[i].Names {
			if n == name {
				return &s.Options[i]
			}
		}
	}
	return nil
}

func hasFoldPrefix(s, prefix string) bool {
	return len(s) >= len(prefix) && strings.EqualFold(s[:len(prefix)], prefix)
}
