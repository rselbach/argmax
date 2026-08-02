package catalog

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"io"
	"sort"
	"strings"

	_ "embed"

	"github.com/rselbach/argmax/internal/complete"
	"github.com/rselbach/argmax/internal/logging"
)

// bundleGz holds every generated catalog spec as one gzip-compressed JSON
// document keyed "<category-slug>/<name>", produced by tools/cataloggen
// from the PRD section 18 list and the MIT-licensed Fig autocomplete
// corpus (see NOTICE).
//
//go:embed data/bundle.json.gz
var bundleGz []byte

//go:embed data/manifest.json
var manifestJSON []byte

// LoadManifest returns the embedded category contract.
func LoadManifest() (*Manifest, error) {
	var m Manifest
	if err := json.Unmarshal(manifestJSON, &m); err != nil {
		return nil, fmt.Errorf("parse catalog manifest: %w", err)
	}
	return &m, nil
}

// Registry builds the bundled specification registry: hand-tuned Go specs
// take precedence, generated data specs fill the remaining catalog. A
// corrupt data bundle degrades to the Go specs alone.
func Registry() *complete.Registry {
	goSpecs := specs()
	exclude := make(map[string]bool)
	for _, s := range goSpecs {
		exclude[strings.ToLower(s.Name)] = true
		for _, a := range s.Aliases {
			exclude[strings.ToLower(a)] = true
		}
	}
	data, err := dataSpecs(exclude)
	if err != nil {
		logging.L().Error("embedded catalog data unavailable; using built-in specs only", "error", err)
		return complete.NewRegistry(goSpecs...)
	}
	return complete.NewRegistry(append(goSpecs, data...)...)
}

// dataSpecs decodes the embedded bundle, merges cross-listed placements
// by command name, and resolves generator references.
func dataSpecs(exclude map[string]bool) ([]*complete.Spec, error) {
	gz, err := gzip.NewReader(bytes.NewReader(bundleGz))
	if err != nil {
		return nil, fmt.Errorf("open catalog bundle: %w", err)
	}
	raw, err := io.ReadAll(gz)
	if err != nil {
		return nil, fmt.Errorf("decompress catalog bundle: %w", err)
	}
	if err := gz.Close(); err != nil {
		return nil, fmt.Errorf("decompress catalog bundle: %w", err)
	}
	var bundle map[string]*SpecData
	if err := json.Unmarshal(raw, &bundle); err != nil {
		return nil, fmt.Errorf("parse catalog bundle: %w", err)
	}

	keys := make([]string, 0, len(bundle))
	for k := range bundle {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	merged := map[string]*SpecData{}
	var order []string
	for _, key := range keys {
		d := bundle[key]
		if d == nil || d.Name == "" {
			continue
		}
		name := strings.ToLower(d.Name)
		if exclude[name] {
			continue
		}
		if existing, ok := merged[name]; ok {
			mergeSpecData(existing, d)
			continue
		}
		merged[name] = d
		order = append(order, name)
	}

	out := make([]*complete.Spec, 0, len(order))
	for _, name := range order {
		spec, err := merged[name].ToSpec()
		if err != nil {
			return nil, err
		}
		out = append(out, spec)
	}
	return out, nil
}

// mergeSpecData folds a cross-listed placement into the first one: the
// runtime must expose one spec containing the generator and options from
// both placements.
func mergeSpecData(dst, src *SpecData) {
	if dst.Description == "" {
		dst.Description = src.Description
	}
	if dst.Icon == "" {
		dst.Icon = src.Icon
	}
	if dst.Generator == "" {
		dst.Generator = src.Generator
	}
	if src.Priority > dst.Priority {
		dst.Priority = src.Priority
	}
	if src.MaxArgs > dst.MaxArgs {
		dst.MaxArgs = src.MaxArgs
	}
	dst.Values = append(dst.Values, src.Values...)
	haveOption := map[string]bool{}
	for _, o := range dst.Options {
		haveOption[strings.Join(o.Names, "\x00")] = true
	}
	for _, o := range src.Options {
		if !haveOption[strings.Join(o.Names, "\x00")] {
			dst.Options = append(dst.Options, o)
		}
	}
	haveSub := map[string]bool{}
	for _, s := range dst.Subcommands {
		haveSub[strings.ToLower(s.Name)] = true
	}
	for _, s := range src.Subcommands {
		if !haveSub[strings.ToLower(s.Name)] {
			dst.Subcommands = append(dst.Subcommands, s)
		}
	}
}
