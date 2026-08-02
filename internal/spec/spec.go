// Package spec implements the bundled command-specification registry, a
// shell-like command-line tokenizer, and completion-tree resolution for
// argmax (PRD 9.5, SPEC-001 through SPEC-011).
//
// The registry is a static catalog compiled into the binary (PRD 18): each
// Spec describes one executable with aliases, options, recursive
// subcommands, an optional dynamic generator ID, an argument limit, and a
// static priority. Generator IDs are fixed strings shared with the sources
// package, which dispatches on the ID and the resolved root command (so
// e.g. podman reuses the "docker-containers-running" ID).
package spec

import "strings"

// Option is a flag/option of a command.
type Option struct {
	Names        []string // e.g. ["-h", "--help"]
	Description  string
	TakesArg     bool
	ArgGenerator string // optional generator ID for the option's argument value
}

// Spec is a command specification node (recursive).
type Spec struct {
	Name        string
	Aliases     []string
	Description string
	Icon        string // category key, see canonicalIcons
	Options     []Option
	Subcommands []*Spec
	Generator   string // dynamic-value generator ID for positional args; "" = none
	MaxArgs     int    // positive = positional limit; non-positive = unlimited
	Priority    int    // 0 = default
}

// canonicalIcons is the fixed set of icon/category keys (PRD UI-016).
// Unknown keys fall back to "misc" at display time.
var canonicalIcons = map[string]bool{
	"git": true, "github": true, "docker": true, "kubernetes": true,
	"cloud": true, "database": true, "node": true, "python": true,
	"rust": true, "go": true, "java": true, "c": true, "build": true,
	"package": true, "fs": true, "archive": true, "editor": true,
	"viewer": true, "text": true, "json": true, "task": true,
	"sysadmin": true, "network": true, "process": true, "shell": true,
	"search": true, "vcs": true, "misc": true,
}

// --- catalog construction helpers -----------------------------------------
//
// The helpers below keep the catalog data compact and readable. They exist
// only to build Spec literals; they add no behavior.

// cmd builds a plain spec node with subcommands.
func cmd(name, desc, icon string, subs ...*Spec) *Spec {
	return &Spec{Name: name, Description: desc, Icon: icon, Subcommands: subs}
}

// genCmd builds a leaf spec node whose positional arguments are produced by
// a dynamic generator (e.g. "files", "dirs", "ext:go").
func genCmd(name, desc, icon, generator string) *Spec {
	return &Spec{Name: name, Description: desc, Icon: icon, Generator: generator}
}

// optD builds a flag option with a description.
func optD(desc string, names ...string) Option {
	return Option{Names: names, Description: desc}
}

// optVal builds an option that takes a plain (non-generated) argument.
func optVal(desc string, names ...string) Option {
	return Option{Names: names, Description: desc, TakesArg: true}
}

// optGen builds an option whose argument is produced by a generator.
func optGen(gen, desc string, names ...string) Option {
	return Option{Names: names, Description: desc, TakesArg: true, ArgGenerator: gen}
}

// findSub returns the direct subcommand whose name or alias equals tok
// (case-insensitive), or nil.
func (s *Spec) findSub(tok string) *Spec {
	for _, sub := range s.Subcommands {
		if equalFoldAny(tok, sub.Name, sub.Aliases) {
			return sub
		}
	}
	return nil
}

// findOption returns the option of this node matching tok exactly or in
// --name=value form, or nil. Option names are case-sensitive.
func (s *Spec) findOption(tok string) *Option {
	for i := range s.Options {
		o := &s.Options[i]
		for _, n := range o.Names {
			if tok == n || strings.HasPrefix(tok, n+"=") {
				return o
			}
		}
	}
	return nil
}

// visibleOption searches node's options, then walks up the ancestor chain
// (global/root options stay valid at deeper nodes).
func visibleOption(chain []*Spec, tok string) *Option {
	for i := len(chain) - 1; i >= 0; i-- {
		if o := chain[i].findOption(tok); o != nil {
			return o
		}
	}
	return nil
}

func equalFoldAny(tok string, name string, aliases []string) bool {
	if strings.EqualFold(tok, name) {
		return true
	}
	for _, a := range aliases {
		if strings.EqualFold(tok, a) {
			return true
		}
	}
	return false
}
