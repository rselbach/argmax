package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/rselbach/argmax/internal/catalog"
)

// figNode is the JSON shape figexport produces for a spec or subcommand.
// Polymorphic fields stay raw and are decoded leniently.
type figNode struct {
	Name        json.RawMessage `json:"name"`
	Description string          `json:"description"`
	Subcommands []figNode       `json:"subcommands"`
	Options     []figOption     `json:"options"`
	Args        json.RawMessage `json:"args"`
}

type figOption struct {
	Name              json.RawMessage `json:"name"`
	Description       string          `json:"description"`
	Args              json.RawMessage `json:"args"`
	RequiresSeparator json.RawMessage `json:"requiresSeparator"`
}

type figArg struct {
	Template    json.RawMessage `json:"template"`
	Suggestions json.RawMessage `json:"suggestions"`
	IsOptional  bool            `json:"isOptional"`
}

// aliasSources maps catalog entries absent from the corpus to another
// command whose spec they share.
var aliasSources = map[string]string{
	"cc": "gcc", // cc is conventionally the same driver interface as gcc
}

// convertFig converts one exported Fig spec; ok=false when the corpus has
// no spec for the command.
func convertFig(figDir, name, icon string) (*genSpec, bool, error) {
	source := name
	if alias, ok := aliasSources[name]; ok {
		source = alias
	}
	data, err := os.ReadFile(filepath.Join(figDir, source+".json"))
	if os.IsNotExist(err) {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, err
	}
	var root figNode
	if err := json.Unmarshal(data, &root); err != nil {
		return nil, false, fmt.Errorf("parse fig dump: %w", err)
	}
	c := &converter{budget: maxNodes}
	spec := c.node(root, 0)
	if spec == nil {
		return nil, false, fmt.Errorf("fig spec has no usable name")
	}
	// The command is looked up under its PRD name regardless of the
	// spec's canonical spelling.
	if !strings.EqualFold(spec.Name, name) {
		spec.Aliases = append(spec.Aliases, spec.Name)
		spec.Name = name
	}
	spec.Icon = icon
	return &genSpec{SpecData: *spec, truncated: c.truncated}, true, nil
}

type converter struct {
	budget    int
	truncated bool
}

// node converts one fig node, applying depth and node-count caps.
func (c *converter) node(n figNode, depth int) *catalog.SpecData {
	names := decodeNames(n.Name)
	if len(names) == 0 || !validName(names[0]) {
		return nil
	}
	canonical, aliases := canonicalName(names)
	s := &catalog.SpecData{
		Name:        canonical,
		Aliases:     aliases,
		Description: cleanDescription(n.Description),
	}
	s.Generator, s.Values = convertArgs(n.Args)
	for _, o := range n.Options {
		if opt, ok := convertOption(o); ok {
			s.Options = append(s.Options, opt)
		}
	}
	if depth >= maxDepth {
		if len(n.Subcommands) > 0 {
			c.truncated = true
		}
		return s
	}
	seen := map[string]bool{}
	for _, sub := range n.Subcommands {
		if c.budget <= 0 {
			c.truncated = true
			break
		}
		converted := c.node(sub, depth+1)
		if converted == nil {
			continue
		}
		key := strings.ToLower(converted.Name)
		if seen[key] {
			continue
		}
		seen[key] = true
		c.budget--
		s.Subcommands = append(s.Subcommands, converted)
	}
	return s
}

func convertOption(o figOption) (catalog.OptionData, bool) {
	names := decodeNames(o.Name)
	var valid []string
	for _, n := range names {
		if validName(n) {
			valid = append(valid, n)
		}
	}
	if len(valid) == 0 {
		return catalog.OptionData{}, false
	}
	opt := catalog.OptionData{
		Names:       valid,
		Description: cleanDescription(o.Description),
	}
	// Separator options attach their value with "=", consuming no extra
	// token; only plain options with a required argument set TakesArg.
	separator := len(o.RequiresSeparator) > 0 && string(o.RequiresSeparator) != "false"
	if !separator {
		if args := decodeArgs(o.Args); len(args) > 0 && !args[0].IsOptional {
			opt.TakesArg = true
			opt.Generator = templateGenerator(args[0].Template)
		}
	}
	return opt, true
}

// convertArgs maps fig positional args onto a generator reference and
// static values. Only serializable templates and suggestions convert;
// script-based fig generators are dropped (the deep Go specs cover the
// important live sources).
func convertArgs(raw json.RawMessage) (string, []catalog.ValueData) {
	args := decodeArgs(raw)
	if len(args) == 0 {
		return "", nil
	}
	generator := ""
	var values []catalog.ValueData
	for _, a := range args {
		if g := templateGenerator(a.Template); g != "" && generator == "" {
			generator = g
		}
		for _, v := range decodeSuggestions(a.Suggestions) {
			if len(values) >= maxValues {
				break
			}
			values = append(values, v)
		}
	}
	return generator, values
}

func templateGenerator(raw json.RawMessage) string {
	var single string
	if json.Unmarshal(raw, &single) == nil {
		return templateName(single)
	}
	var list []string
	if json.Unmarshal(raw, &list) == nil {
		hasFiles, hasFolders := false, false
		for _, t := range list {
			switch templateName(t) {
			case "files":
				hasFiles = true
			case "directories":
				hasFolders = true
			}
		}
		switch {
		case hasFiles:
			return "files"
		case hasFolders:
			return "directories"
		}
	}
	return ""
}

func templateName(t string) string {
	switch t {
	case "filepaths":
		return "files"
	case "folders":
		return "directories"
	}
	return ""
}

// decodeArgs accepts an object, an array, or nothing.
func decodeArgs(raw json.RawMessage) []figArg {
	if len(raw) == 0 {
		return nil
	}
	var one figArg
	if err := json.Unmarshal(raw, &one); err == nil {
		return []figArg{one}
	}
	var many []figArg
	if err := json.Unmarshal(raw, &many); err == nil {
		return many
	}
	return nil
}

// decodeNames accepts a string or an array of strings.
func decodeNames(raw json.RawMessage) []string {
	if len(raw) == 0 {
		return nil
	}
	var one string
	if err := json.Unmarshal(raw, &one); err == nil {
		return []string{one}
	}
	var many []string
	if err := json.Unmarshal(raw, &many); err == nil {
		return many
	}
	return nil
}

// decodeSuggestions accepts strings or {name, description} objects.
func decodeSuggestions(raw json.RawMessage) []catalog.ValueData {
	if len(raw) == 0 {
		return nil
	}
	var items []json.RawMessage
	if err := json.Unmarshal(raw, &items); err != nil {
		return nil
	}
	var out []catalog.ValueData
	for _, item := range items {
		var name string
		if err := json.Unmarshal(item, &name); err == nil {
			if validName(name) {
				out = append(out, catalog.ValueData{Name: name})
			}
			continue
		}
		var obj struct {
			Name        json.RawMessage `json:"name"`
			Description string          `json:"description"`
		}
		if err := json.Unmarshal(item, &obj); err != nil {
			continue
		}
		names := decodeNames(obj.Name)
		if len(names) == 0 || !validName(names[0]) {
			continue
		}
		out = append(out, catalog.ValueData{
			Name:        names[0],
			Description: cleanDescription(obj.Description),
		})
	}
	return out
}

// canonicalName picks the longest spelling as the name; the rest become
// aliases (fig orders name arrays inconsistently).
func canonicalName(names []string) (string, []string) {
	canonical := names[0]
	for _, n := range names[1:] {
		if len(n) > len(canonical) {
			canonical = n
		}
	}
	var aliases []string
	for _, n := range names {
		if n != canonical && validName(n) {
			aliases = append(aliases, n)
		}
	}
	return canonical, aliases
}

func validName(name string) bool {
	if name == "" || len(name) > 64 {
		return false
	}
	for _, r := range name {
		if r < 0x21 || r > 0x7e {
			return false
		}
	}
	return true
}

// cleanDescription strips control characters, collapses whitespace, and
// caps length.
func cleanDescription(s string) string {
	s = strings.Join(strings.Fields(s), " ")
	var b strings.Builder
	for _, r := range s {
		if r >= 0x20 && r != 0x7f {
			b.WriteRune(r)
		}
	}
	out := b.String()
	if len(out) > maxDescription {
		cut := out[:maxDescription]
		if i := strings.LastIndex(cut, " "); i > maxDescription/2 {
			cut = cut[:i]
		}
		out = cut + "…"
	}
	return out
}
