package catalog

import (
	"testing"

	"github.com/rselbach/argmax/internal/complete"
)

// TestRegistryValidates is the CI gate for duplicate names, malformed
// specs, and unreachable nodes in the bundled catalog.
func TestRegistryValidates(t *testing.T) {
	if err := Registry().Validate(); err != nil {
		t.Fatalf("bundled catalog invalid: %v", err)
	}
}

func TestToSpecKeepsOnlyUnambiguousOptionNames(t *testing.T) {
	data := &SpecData{Name: "tool", Options: []OptionData{
		{Names: []string{"-l", "--selector"}},
		{Names: []string{"-l", "--labels"}},
	}}
	spec, err := data.ToSpec()
	if err != nil {
		t.Fatal(err)
	}
	registry := complete.NewRegistry(spec)
	if err := registry.Validate(); err != nil {
		t.Fatalf("normalized imported spec is invalid: %v", err)
	}
	if got := spec.Options[1].Names; len(got) != 1 || got[0] != "--labels" {
		t.Errorf("second option names = %v, want only --labels", got)
	}
}

func TestToSpecNormalizesSubcommandNamesAndAliases(t *testing.T) {
	data := &SpecData{Name: "tool", Subcommands: []*SpecData{
		{Name: "install", Aliases: []string{"add"}},
		{Name: "ADD"},
		{Name: "apiKey"},
		{Name: "apikey"},
	}}
	spec, err := data.ToSpec()
	if err != nil {
		t.Fatal(err)
	}
	registry := complete.NewRegistry(spec)
	if err := registry.Validate(); err != nil {
		t.Fatalf("normalized imported spec is invalid: %v", err)
	}
	if len(spec.Subcommands) != 3 {
		t.Errorf("subcommands = %d, want case-insensitive duplicates merged", len(spec.Subcommands))
	}
	if len(spec.Subcommands[0].Aliases) != 0 {
		t.Errorf("alias colliding with a canonical sibling was retained: %v", spec.Subcommands[0].Aliases)
	}
}

func TestNormalizeAliasesPrefersCanonicalNames(t *testing.T) {
	cc := &complete.Spec{Name: "cc", Aliases: []string{"gcc"}}
	gcc := &complete.Spec{Name: "gcc"}
	specs := []*complete.Spec{cc, gcc}
	normalizeAliases(specs)
	if len(cc.Aliases) != 0 {
		t.Errorf("alias shadowing canonical gcc was retained: %v", cc.Aliases)
	}
	if err := complete.NewRegistry(specs...).Validate(); err != nil {
		t.Fatalf("normalized registry is invalid: %v", err)
	}
}

func TestCoreEntriesPresent(t *testing.T) {
	r := Registry()
	for _, name := range []string{"git", "go", "docker", "kubectl", "cargo", "npm", "make", "just", "ssh", "kill", "chmod", "cd"} {
		if r.Lookup(name) == nil {
			t.Errorf("catalog missing %q", name)
		}
	}
}

func TestGitCheYieldsCheckout(t *testing.T) {
	e := &complete.Engine{Registry: Registry()}
	got := e.Complete(complete.Context{CWD: t.TempDir()}, "git che")
	for _, c := range got {
		if c.Text == "git checkout" {
			return
		}
	}
	t.Errorf("`git che` must yield `git checkout`; got %d candidates", len(got))
}
