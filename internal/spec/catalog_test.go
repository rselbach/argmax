package spec

import (
	"strings"
	"testing"
)

// TestCategoryCounts pins the per-category sizes of PRD section 18 and the
// 567-entry / 566-distinct-name totals.
func TestCategoryCounts(t *testing.T) {
	categories := []struct {
		name  string
		specs []*Spec
		want  int
	}{
		{"cloud", catalogCloud(), cloudCount},
		{"node", catalogNode(), nodeCount},
		{"python", catalogPython(), pythonCount},
		{"rust", catalogRust(), rustCount},
		{"go", catalogGo(), goDevCount},
		{"jvm", catalogJVM(), jvmCount},
		{"c", catalogC(), ccCount},
		{"git", catalogGit(), gitToolsCount},
		{"pkg", catalogPkg(), pkgCount},
		{"fs", catalogFS(), fsCount},
		{"editors", catalogEditors(), editorsCount},
		{"text", catalogText(), textCount},
		{"task", catalogTask(), taskCount},
		{"sysadmin", catalogSysadmin(), sysadminCount},
	}
	// The constants themselves encode the PRD 18 table.
	wantPRD := map[string]int{
		"cloud": 118, "node": 82, "python": 19, "rust": 11, "go": 3,
		"jvm": 14, "c": 16, "git": 8, "pkg": 12, "fs": 30,
		"editors": 27, "text": 28, "task": 24, "sysadmin": 175,
	}
	total := 0
	for _, c := range categories {
		if c.want != wantPRD[c.name] {
			t.Errorf("%s count constant = %d, PRD says %d", c.name, c.want, wantPRD[c.name])
		}
		if len(c.specs) != c.want {
			t.Errorf("%s has %d entries, count constant says %d", c.name, len(c.specs), c.want)
		}
		total += len(c.specs)
	}
	if total != 567 {
		t.Errorf("catalog total = %d entries, want 567 (PRD 18)", total)
	}
	if got := len(Default().All()); got != 566 {
		t.Errorf("registry has %d distinct names, want 566 (PRD 18)", got)
	}
}

// TestFindMerged verifies the cross-listed find spec (PRD 18): the runtime
// exposes exactly one find spec combining the generator and options of the
// filesystem and text-processing placements.
func TestFindMerged(t *testing.T) {
	r := Default()
	find := r.Lookup("find")
	if find == nil {
		t.Fatal("find not registered")
	}
	// It is the only cross-listed command.
	if len(r.merged) != 1 || r.merged[0] != "find" {
		t.Errorf("merged = %v, want exactly [find]", r.merged)
	}
	if find.Generator != "dirs" {
		t.Errorf("find generator = %q, want dirs (filesystem placement)", find.Generator)
	}
	want := []string{
		// filesystem placement
		"-type", "-maxdepth", "-mindepth", "-mtime", "-size",
		// text processing placement
		"-name", "-iname", "-exec", "-delete",
	}
	for _, name := range want {
		if find.findOption(name) == nil {
			t.Errorf("merged find is missing option %q", name)
		}
	}
}

func TestDefaultRegistryValidates(t *testing.T) {
	if err := Default().Validate(); err != nil {
		t.Errorf("Validate(Default()) = %v, want nil (SPEC-011)", err)
	}
}

// TestCatalogHygiene checks that every top-level spec has a non-empty
// description and a canonical icon key.
func TestCatalogHygiene(t *testing.T) {
	for _, s := range Default().All() {
		if strings.TrimSpace(s.Description) == "" {
			t.Errorf("%s: empty description", s.Name)
		}
		if !canonicalIcons[s.Icon] {
			t.Errorf("%s: icon %q is not a canonical key", s.Name, s.Icon)
		}
	}
}

// TestValidateDetectsProblems exercises the malformed-registry checks of
// SPEC-011 on purpose-built registries.
func TestValidateDetectsProblems(t *testing.T) {
	t.Run("duplicate names", func(t *testing.T) {
		a := cmd("foo", "one", "misc")
		b := cmd("foo", "two", "misc")
		r := &Registry{roots: []*Spec{a, b}}
		if err := r.Validate(); err == nil {
			t.Error("duplicate names not detected")
		}
	})
	t.Run("alias collides with command name", func(t *testing.T) {
		r := buildRegistry([]*Spec{
			{Name: "foo", Aliases: []string{"bar"}},
			{Name: "bar"},
		})
		if err := r.Validate(); err == nil {
			t.Error("alias/name collision not detected")
		}
	})
	t.Run("alias collides with alias", func(t *testing.T) {
		r := buildRegistry([]*Spec{
			{Name: "foo", Aliases: []string{"x"}},
			{Name: "bar", Aliases: []string{"x"}},
		})
		if err := r.Validate(); err == nil {
			t.Error("alias/alias collision not detected")
		}
	})
	t.Run("empty name", func(t *testing.T) {
		r := buildRegistry([]*Spec{{Name: ""}})
		if err := r.Validate(); err == nil {
			t.Error("empty name not detected")
		}
	})
	t.Run("self reference", func(t *testing.T) {
		s := cmd("foo", "x", "misc")
		s.Subcommands = append(s.Subcommands, s)
		r := &Registry{roots: []*Spec{s}}
		if err := r.Validate(); err == nil {
			t.Error("self-reference not detected")
		}
	})
	t.Run("cycle", func(t *testing.T) {
		a := cmd("a", "x", "misc")
		b := cmd("b", "x", "misc")
		a.Subcommands = []*Spec{b}
		b.Subcommands = []*Spec{a}
		r := &Registry{roots: []*Spec{a}}
		if err := r.Validate(); err == nil {
			t.Error("cycle not detected")
		}
	})
	t.Run("duplicate sibling subcommands", func(t *testing.T) {
		r := &Registry{roots: []*Spec{
			cmd("foo", "x", "misc", cmd("sub", "one", "misc"), cmd("sub", "two", "misc")),
		}}
		if err := r.Validate(); err == nil {
			t.Error("duplicate sibling subcommands not detected")
		}
	})
	t.Run("unknown icon", func(t *testing.T) {
		r := &Registry{roots: []*Spec{cmd("foo", "x", "not-an-icon")}}
		if err := r.Validate(); err == nil {
			t.Error("non-canonical icon not detected")
		}
	})
	t.Run("clean registry", func(t *testing.T) {
		r := buildRegistry([]*Spec{
			{Name: "foo", Aliases: []string{"f"}, Icon: "misc",
				Subcommands: []*Spec{{Name: "sub", Aliases: []string{"s"}, Icon: "misc"}}},
		})
		if err := r.Validate(); err != nil {
			t.Errorf("Validate() = %v, want nil", err)
		}
	})
}
