package catalog

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/complete"
)

// TestCatalogContract is the CI gate for the PRD section 18 catalog
// contract: 567 category entries, 566 distinct executable names, 14
// categories, every entry resolvable in the registry.
func TestCatalogContract(t *testing.T) {
	m, err := LoadManifest()
	if err != nil {
		t.Fatalf("manifest: %v", err)
	}
	if got := len(m.Categories); got != 14 {
		t.Errorf("categories = %d, want 14", got)
	}
	reg := Registry()
	total := 0
	distinct := map[string]bool{}
	for _, cat := range m.Categories {
		if len(cat.Entries) == 0 {
			t.Errorf("category %s has no entries", cat.Slug)
		}
		for _, name := range cat.Entries {
			total++
			distinct[name] = true
			if reg.Lookup(name) == nil {
				t.Errorf("catalog entry %q (%s) does not resolve in the registry", name, cat.Slug)
			}
		}
	}
	if total != 567 {
		t.Errorf("total category entries = %d, want 567", total)
	}
	if len(distinct) != 566 {
		t.Errorf("distinct executable names = %d, want 566", len(distinct))
	}
}

// TestPerCategoryCounts pins the PRD section 18 count table.
func TestPerCategoryCounts(t *testing.T) {
	want := map[string]int{
		"cloud": 118, "javascript": 82, "python": 19, "rust": 11, "go": 3,
		"jvm": 14, "cpp": 16, "git": 8, "syspkg": 12, "filesystem": 30,
		"editors": 27, "text": 28, "taskrunners": 24, "sysadmin": 175,
	}
	m, err := LoadManifest()
	if err != nil {
		t.Fatalf("manifest: %v", err)
	}
	for _, cat := range m.Categories {
		if got := len(cat.Entries); got != want[cat.Slug] {
			t.Errorf("category %s has %d entries, want %d", cat.Slug, got, want[cat.Slug])
		}
	}
}

// TestFindMerged verifies the runtime exposes one merged find spec with
// the generator and options from both PRD placements.
func TestFindMerged(t *testing.T) {
	spec := Registry().Lookup("find")
	if spec == nil {
		t.Fatal("find spec missing")
	}
	if spec.Generator == nil {
		t.Error("merged find spec must keep a generator")
	}
	var hasName, hasRegex bool
	for _, o := range spec.Options {
		for _, n := range o.Names {
			switch n {
			case "-name":
				hasName = true
			case "-regex":
				hasRegex = true
			}
		}
	}
	if !hasName {
		t.Error("merged find spec missing filesystem placement option -name")
	}
	if !hasRegex {
		t.Error("merged find spec missing text-processing placement option -regex")
	}
}

// TestDataSpecsUsable spot-checks converted specs end to end.
func TestDataSpecsUsable(t *testing.T) {
	e := &complete.Engine{Registry: Registry()}
	ctx := complete.Context{CWD: t.TempDir()}
	tests := map[string]struct {
		line string
		want string
	}{
		"terraform subcommand": {line: "terraform pl", want: "terraform plan"},
		"helm subcommand":      {line: "helm ins", want: "helm install"},
		"gh nested":            {line: "gh pr cre", want: "gh pr create"},
		"eslint option":        {line: "eslint --f", want: "eslint --fix"},
		"aws service":          {line: "aws s3", want: "aws s3"},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got := e.Complete(ctx, tc.line)
			for _, c := range got {
				if strings.HasPrefix(c.Text, tc.want) {
					return
				}
			}
			t.Errorf("Complete(%q): no candidate with prefix %q among %d results", tc.line, tc.want, len(got))
		})
	}
}

// TestRegistryValidatesWithData runs the registry validator over the full
// merged catalog.
func TestRegistryValidatesWithData(t *testing.T) {
	if err := Registry().Validate(); err != nil {
		t.Fatalf("merged catalog invalid: %v", err)
	}
}

// BenchmarkRegistry tracks catalog construction cost, which sits on the
// session startup path.
func BenchmarkRegistry(b *testing.B) {
	for b.Loop() {
		if Registry() == nil {
			b.Fatal("nil registry")
		}
	}
}

// TestGeneratedDocsMatch fails when docs/commands.md drifts from the
// registry; regenerate with `go run ./tools/docgen`.
func TestGeneratedDocsMatch(t *testing.T) {
	want, err := GenerateDocs()
	if err != nil {
		t.Fatalf("generate docs: %v", err)
	}
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("cannot locate source file")
	}
	path := filepath.Join(filepath.Dir(thisFile), "..", "..", "docs", "commands.md")
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s (run `go run ./tools/docgen`): %v", path, err)
	}
	if string(got) != want {
		t.Errorf("docs/commands.md drifted from the registry; regenerate with `go run ./tools/docgen`")
	}
}

// TestGoSpecsEnrichedFromCorpus verifies hand-tuned specs gain the corpus
// flags they omit while keeping their curated entries and generators.
func TestGoSpecsEnrichedFromCorpus(t *testing.T) {
	reg := Registry()
	goSpec := reg.Lookup("go")
	if goSpec == nil {
		t.Fatal("go spec missing")
	}
	var build *complete.Spec
	for _, sub := range goSpec.Subcommands {
		if sub.Name == "build" {
			build = sub
		}
	}
	if build == nil {
		t.Fatal("go build subcommand missing")
	}
	names := map[string]bool{}
	for _, o := range build.Options {
		for _, n := range o.Names {
			names[n] = true
		}
	}
	for _, want := range []string{"-o", "-race", "-v", "-ldflags", "-tags"} {
		if !names[want] {
			t.Errorf("go build missing option %s after enrichment (have %d options)", want, len(build.Options))
		}
	}
	if build.Generator == nil {
		t.Error("hand-tuned go build generator must survive the merge")
	}

	git := reg.Lookup("git")
	var checkout *complete.Spec
	for _, sub := range git.Subcommands {
		if sub.Name == "checkout" {
			checkout = sub
		}
	}
	if checkout == nil || checkout.Generator == nil {
		t.Fatal("git checkout branch generator must survive the merge")
	}
	if len(checkout.Options) <= 2 {
		t.Errorf("git checkout should gain corpus flags, has %d", len(checkout.Options))
	}
}
