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
