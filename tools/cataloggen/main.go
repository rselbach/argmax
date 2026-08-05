// Command cataloggen generates the embedded catalog data files from the
// PRD section 18 command list and JSON dumps of the MIT-licensed Fig
// autocomplete corpus (produced by tools/figexport).
//
// For every PRD entry it emits internal/catalog/data/<category>/<name>.json
// in the SpecData schema, skipping names implemented as Go specs, and
// writes a minimal stub for commands absent from the corpus. It also
// emits data/manifest.json recording the category contract, and reports
// conversions, stubs, and truncations. Nothing is dropped silently.
package main

import (
	"compress/gzip"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"github.com/rselbach/argmax/internal/catalog"
	project "github.com/rselbach/argmax/internal/workspace"
)

const (
	maxDepth       = 4
	maxNodes       = 4000
	maxValues      = 40
	maxDescription = 140
)

func main() {
	prd := flag.String("prd", "argmax-prd.md", "path to the PRD")
	figDir := flag.String("fig", "", "directory of figexport JSON dumps")
	outDir := flag.String("out", "internal/catalog/data", "output data directory")
	flag.Parse()
	if *figDir == "" {
		fmt.Fprintln(os.Stderr, "cataloggen: -fig is required")
		os.Exit(2)
	}
	if err := run(*prd, *figDir, *outDir); err != nil {
		fmt.Fprintln(os.Stderr, "cataloggen:", err)
		os.Exit(1)
	}
}

func run(prdPath, figDir, outDir string) error {
	workspace := project.Resolve(".")
	safeOutDir, err := validateOutputDir(outDir, workspace.Root)
	if err != nil {
		return err
	}
	outDir = safeOutDir

	categories, err := parsePRD(prdPath)
	if err != nil {
		return err
	}
	goNames := map[string]bool{}
	for _, n := range catalog.GoSpecNames() {
		goNames[n] = true
	}

	if err := os.RemoveAll(outDir); err != nil {
		return err
	}
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		return err
	}
	stats := struct {
		converted, enriching, stubbed, goOnly int
		truncated                             []string
		stubs                                 []string
	}{}
	bundle := map[string]catalog.SpecData{} // "<slug>/<name>" -> spec
	seen := map[string]string{}             // name -> category slug of first placement
	for _, cat := range categories {
		for _, name := range cat.Entries {
			key := cat.Slug + "/" + name
			if first, dup := seen[name]; dup {
				// Cross-listed entry (such as find): emit a placement
				// complement only if we have one; the loader merges by name.
				if complement := crossListComplement(name, cat.Slug, first); complement != nil {
					bundle[key] = complement.SpecData
				}
				continue
			}
			seen[name] = cat.Slug
			spec, ok, err := convertFig(figDir, name, cat.Icon)
			if err != nil {
				return fmt.Errorf("%s: %w", name, err)
			}
			switch {
			case ok && goNames[name]:
				// The loader deep-merges this under the hand-tuned spec,
				// filling in flags and subcommands the Go code omits.
				stats.enriching++
			case ok:
				stats.converted++
			case goNames[name]:
				// Hand-authored coverage; nothing to convert or stub.
				stats.goOnly++
				continue
			default:
				spec = stub(name, cat.Icon)
				stats.stubbed++
				stats.stubs = append(stats.stubs, name)
			}
			if spec.truncated {
				stats.truncated = append(stats.truncated, name)
			}
			bundle[key] = spec.SpecData
		}
	}
	if err := writeBundle(outDir, bundle); err != nil {
		return err
	}
	if err := writeManifest(outDir, categories); err != nil {
		return err
	}
	total := 0
	for _, c := range categories {
		total += len(c.Entries)
	}
	fmt.Printf("cataloggen: %d PRD entries across %d categories\n", total, len(categories))
	fmt.Printf("  converted from Fig corpus: %d\n", stats.converted)
	fmt.Printf("  enriching Go specs:        %d\n", stats.enriching)
	fmt.Printf("  Go specs without corpus:   %d\n", stats.goOnly)
	fmt.Printf("  minimal stubs:             %d\n", stats.stubbed)
	if len(stats.truncated) > 0 {
		fmt.Printf("  truncated by caps:         %s\n", strings.Join(stats.truncated, ", "))
	}
	if len(stats.stubs) > 0 {
		sort.Strings(stats.stubs)
		fmt.Printf("  stubbed commands:          %s\n", strings.Join(stats.stubs, ", "))
	}
	return nil
}

func validateOutputDir(outDir, repositoryRoot string) (string, error) {
	if strings.TrimSpace(outDir) == "" {
		return "", fmt.Errorf("output directory is empty")
	}
	absOut, err := filepath.Abs(outDir)
	if err != nil {
		return "", fmt.Errorf("resolve output directory: %w", err)
	}
	absRoot, err := filepath.Abs(repositoryRoot)
	if err != nil {
		return "", fmt.Errorf("resolve repository root: %w", err)
	}
	expected := filepath.Join(absRoot, "internal", "catalog", "data")
	if filepath.Clean(absOut) != expected {
		return "", fmt.Errorf("refusing to replace unsafe output directory %q; expected %q", outDir, expected)
	}
	return expected, nil
}

// category mirrors one PRD section 18 subsection.
type category struct {
	Title   string
	Slug    string
	Icon    string
	Entries []string
}

var categoryMeta = []struct{ slug, icon string }{
	{"cloud", "package"}, {"javascript", "node"}, {"python", "python"},
	{"rust", "rust"}, {"go", "go"}, {"jvm", "package"}, {"cpp", "package"},
	{"git", "git"}, {"syspkg", "package"}, {"filesystem", "folder"},
	{"editors", "file"}, {"text", "file"}, {"taskrunners", "task"},
	{"sysadmin", "process"},
}

var (
	sectionRe = regexp.MustCompile(`(?m)^### 18\.(\d+) (.+)$`)
	nameRe    = regexp.MustCompile("`([^`]+)`")
)

// parsePRD extracts the category lists from PRD section 18.
func parsePRD(path string) ([]category, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	content := string(data)
	locs := sectionRe.FindAllStringSubmatchIndex(content, -1)
	if len(locs) != len(categoryMeta) {
		return nil, fmt.Errorf("expected %d section 18 subsections, found %d", len(categoryMeta), len(locs))
	}
	var categories []category
	for i, loc := range locs {
		end := len(content)
		if i+1 < len(locs) {
			end = locs[i+1][0]
		}
		if next := strings.Index(content[loc[1]:], "\n## "); next >= 0 && loc[1]+next < end {
			end = loc[1] + next
		}
		body := content[loc[1]:end]
		var names []string
		for _, m := range nameRe.FindAllStringSubmatch(body, -1) {
			names = append(names, m[1])
		}
		if len(names) == 0 {
			return nil, fmt.Errorf("section %s lists no entries", content[loc[2]:loc[3]])
		}
		categories = append(categories, category{
			Title:   strings.TrimSpace(content[loc[4]:loc[5]]),
			Slug:    categoryMeta[i].slug,
			Icon:    categoryMeta[i].icon,
			Entries: names,
		})
	}
	return categories, nil
}

func writeManifest(outDir string, categories []category) error {
	m := catalog.Manifest{}
	for _, c := range categories {
		m.Categories = append(m.Categories, catalog.ManifestCategory{
			Title:   c.Title,
			Slug:    c.Slug,
			Icon:    c.Icon,
			Entries: c.Entries,
		})
	}
	data, err := json.MarshalIndent(m, "", " ")
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(outDir, "manifest.json"), append(data, '\n'), 0o644)
}

// genSpec wraps SpecData with generation bookkeeping.
type genSpec struct {
	catalog.SpecData
	truncated bool
}

// writeBundle stores every generated spec in one gzip-compressed JSON
// document so the embedded catalog stays small.
func writeBundle(outDir string, bundle map[string]catalog.SpecData) error {
	data, err := json.Marshal(bundle)
	if err != nil {
		return err
	}
	f, err := os.Create(filepath.Join(outDir, "bundle.json.gz"))
	if err != nil {
		return err
	}
	gz, err := gzip.NewWriterLevel(f, gzip.BestCompression)
	if err != nil {
		_ = f.Close()
		return err
	}
	if _, err := gz.Write(data); err != nil {
		_ = gz.Close()
		_ = f.Close()
		return err
	}
	if err := gz.Close(); err != nil {
		_ = f.Close()
		return err
	}
	return f.Close()
}

// stub is the minimal spec for a command absent from the corpus: name
// plus generic file completion, satisfying required top-level coverage.
func stub(name, icon string) *genSpec {
	return &genSpec{SpecData: catalog.SpecData{
		Name:      name,
		Icon:      icon,
		Generator: "files",
	}}
}

// crossListComplement returns the second placement for entries the PRD
// cross-lists in two categories; the loader merges both by name.
func crossListComplement(name, slug, firstSlug string) *genSpec {
	if name != "find" {
		return nil
	}
	// find is listed under filesystem and text processing; the corpus
	// spec carries only traversal flags, so this placement contributes
	// the common expression primaries of both flavors.
	return &genSpec{SpecData: catalog.SpecData{
		Name:      "find",
		Icon:      "file",
		Generator: "directories",
		Options: []OptionAlias{
			{Names: []string{"-name"}, Description: "match the base name against a glob pattern", TakesArg: true},
			{Names: []string{"-iname"}, Description: "case-insensitive -name", TakesArg: true},
			{Names: []string{"-path"}, Description: "match the whole path against a glob pattern", TakesArg: true},
			{Names: []string{"-type"}, Description: "match by file type: f, d, l, s, p, c, b", TakesArg: true},
			{Names: []string{"-mtime"}, Description: "match by modification time in days", TakesArg: true},
			{Names: []string{"-size"}, Description: "match by file size, e.g. +1M", TakesArg: true},
			{Names: []string{"-maxdepth"}, Description: "descend at most this many directory levels", TakesArg: true},
			{Names: []string{"-mindepth"}, Description: "skip levels above this depth", TakesArg: true},
			{Names: []string{"-newer"}, Description: "match files newer than the reference file", TakesArg: true},
			{Names: []string{"-regex"}, Description: "match the whole path against a regular expression", TakesArg: true},
			{Names: []string{"-iregex"}, Description: "case-insensitive -regex", TakesArg: true},
			{Names: []string{"-print0"}, Description: "print results NUL-separated for xargs -0"},
			{Names: []string{"-exec"}, Description: "run a command on each result", TakesArg: true},
			{Names: []string{"-delete"}, Description: "delete matched files"},
			{Names: []string{"-ls"}, Description: "list results in ls -dils format"},
		},
	}}
}

// OptionAlias keeps the literal above readable.
type OptionAlias = catalog.OptionData
