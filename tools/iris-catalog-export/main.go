// Command iris-catalog-export serializes IRIS's registered completion catalog.
package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"sort"
	"strings"

	_ "github.com/versenilvis/iris/commands"
	"github.com/versenilvis/iris/spec"
)

type exportCatalog struct {
	Inventory []inventoryEntry `json:"inventory"`
	Commands  []commandSpec    `json:"commands"`
}

type inventoryEntry struct {
	Category    string `json:"category"`
	Name        string `json:"name"`
	Description string `json:"description"`
	Source      string `json:"source"`
	Merged      bool   `json:"merged,omitempty"`
}

type commandSpec struct {
	Name        string        `json:"name"`
	Aliases     []string      `json:"aliases,omitempty"`
	Description string        `json:"description"`
	Icon        string        `json:"icon,omitempty"`
	Subcommands []commandSpec `json:"subcommands,omitempty"`
	Options     []optionSpec  `json:"options,omitempty"`
	Generator   string        `json:"generator,omitempty"`
	MaxArgs     int           `json:"max_args,omitempty"`
	Priority    int           `json:"priority,omitempty"`
}

type optionSpec struct {
	Name        string `json:"name"`
	Description string `json:"description"`
	Priority    int    `json:"priority,omitempty"`
}

func main() {
	var irisRoot string
	var output string
	var wantInventory int
	flag.StringVar(&irisRoot, "iris-root", ".", "path to the IRIS repository")
	flag.StringVar(&output, "output", "", "destination JSON file; stdout when empty")
	flag.IntVar(&wantInventory, "expect-inventory", 0,
		"fail unless the inventory has exactly this many entries; 0 disables the check")
	flag.Parse()

	catalog, err := buildCatalog(irisRoot, wantInventory)
	if err != nil {
		fmt.Fprintf(os.Stderr, "iris-catalog-export: %v\n", err)
		os.Exit(1)
	}
	contents, err := json.MarshalIndent(catalog, "", "  ")
	if err != nil {
		fmt.Fprintf(os.Stderr, "iris-catalog-export: encode catalog: %v\n", err)
		os.Exit(1)
	}
	contents = append(contents, '\n')
	if output == "" {
		if _, err := os.Stdout.Write(contents); err != nil {
			fmt.Fprintf(os.Stderr, "iris-catalog-export: write stdout: %v\n", err)
			os.Exit(1)
		}
		return
	}
	if err := writeAtomic(output, contents); err != nil {
		fmt.Fprintf(os.Stderr, "iris-catalog-export: %v\n", err)
		os.Exit(1)
	}
}

func buildCatalog(irisRoot string, wantInventory int) (exportCatalog, error) {
	inventory, err := readInventory(filepath.Join(irisRoot, "commands", "README.md"))
	if err != nil {
		return exportCatalog{}, err
	}
	// The expected count is supplied by the caller. Hard-coding it made any
	// legitimate change to the upstream catalog fail the export until the
	// number was edited here.
	if wantInventory > 0 && len(inventory) != wantInventory {
		return exportCatalog{}, fmt.Errorf("inventory has %d entries; want %d", len(inventory), wantInventory)
	}

	commands := make([]commandSpec, 0, len(spec.Registry))
	registryRoots := make(map[string]*spec.Spec, len(spec.Registry))
	for _, source := range spec.Registry {
		commands = append(commands, convertRoot(source))
		registryRoots[source.Name] = source
	}
	sort.Slice(commands, func(i, j int) bool { return commands[i].Name < commands[j].Name })

	counts := make(map[string]int, len(inventory))
	for _, entry := range inventory {
		counts[entry.Name]++
	}
	for index := range inventory {
		root := registryRoots[inventory[index].Name]
		if root == nil {
			return exportCatalog{}, fmt.Errorf(
				"inventory root %q is absent from the runtime registry",
				inventory[index].Name,
			)
		}
		if counts[inventory[index].Name] > 1 {
			inventory[index].Merged = inventory[index].Description != root.Description
		}
	}
	canonicalCounts := make(map[string]int, len(counts))
	for _, entry := range inventory {
		if !entry.Merged {
			canonicalCounts[entry.Name]++
		}
	}
	for name := range counts {
		if canonicalCounts[name] != 1 {
			return exportCatalog{}, fmt.Errorf(
				"inventory root %q has %d canonical records; expected 1",
				name,
				canonicalCounts[name],
			)
		}
	}
	return exportCatalog{Inventory: inventory, Commands: commands}, nil
}

func readInventory(path string) ([]inventoryEntry, error) {
	contents, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read inventory: %w", err)
	}
	var category string
	var inventory []inventoryEntry
	for _, line := range strings.Split(string(contents), "\n") {
		if strings.HasPrefix(line, "## ") {
			start := strings.LastIndex(line, "(`")
			end := strings.LastIndex(line, "/`)")
			if start < 0 || end <= start {
				// Carrying the previous category forward would silently
				// misattribute every row under a malformed heading.
				return nil, fmt.Errorf("malformed inventory heading %q", line)
			}
			category = line[start+2 : end]
			continue
		}
		if !strings.HasPrefix(line, "| **`") {
			continue
		}
		row := strings.TrimSuffix(strings.TrimPrefix(line, "| "), " |")
		parts := strings.Split(row, " | ")
		if len(parts) != 3 || category == "" {
			return nil, fmt.Errorf("malformed inventory row %q", line)
		}
		name := strings.TrimSuffix(strings.TrimPrefix(parts[0], "**`"), "`**")
		description := strings.ReplaceAll(parts[1], "\\|", "|")
		sourceStart := strings.Index(parts[2], "(./")
		sourceEnd := strings.LastIndex(parts[2], ")")
		if name == "" || sourceStart < 0 || sourceEnd <= sourceStart+3 {
			return nil, fmt.Errorf("malformed inventory row %q", line)
		}
		inventory = append(inventory, inventoryEntry{
			Category:    category,
			Name:        name,
			Description: description,
			Source:      parts[2][sourceStart+3 : sourceEnd],
		})
	}
	return inventory, nil
}

func convertRoot(source *spec.Spec) commandSpec {
	children := make([]commandSpec, 0, len(source.Subcommands))
	for _, child := range source.Subcommands {
		children = append(children, convertSubcommand(child))
	}
	return commandSpec{
		Name:        source.Name,
		Aliases:     source.Aliases,
		Description: source.Description,
		Icon:        source.Icon,
		Subcommands: children,
		Options:     convertOptions(source.Options),
		Generator:   generatorName(source.Generator),
		MaxArgs:     source.MaxArgs,
	}
}

func convertSubcommand(source spec.Subcommand) commandSpec {
	children := make([]commandSpec, 0, len(source.Subcommands))
	for _, child := range source.Subcommands {
		children = append(children, convertSubcommand(child))
	}
	return commandSpec{
		Name:        source.Name,
		Aliases:     source.Aliases,
		Description: source.Description,
		Icon:        source.Icon,
		Subcommands: children,
		Options:     convertOptions(source.Options),
		Generator:   generatorName(source.Generator),
		MaxArgs:     source.MaxArgs,
		Priority:    source.Priority,
	}
}

func convertOptions(source []spec.Option) []optionSpec {
	options := make([]optionSpec, 0, len(source))
	for _, option := range source {
		options = append(options, optionSpec{
			Name:        option.Name,
			Description: option.Description,
			Priority:    option.Priority,
		})
	}
	return options
}

func generatorName(generator spec.GeneratorFunc) string {
	if generator == nil {
		return ""
	}
	function := runtime.FuncForPC(reflect.ValueOf(generator).Pointer())
	if function == nil {
		return "unknown"
	}
	return strings.TrimPrefix(function.Name(), "github.com/versenilvis/iris/")
}

func writeAtomic(destination string, contents []byte) error {
	if err := os.MkdirAll(filepath.Dir(destination), 0o755); err != nil {
		return fmt.Errorf("create output directory: %w", err)
	}
	temporary, err := os.CreateTemp(filepath.Dir(destination), ".iris-catalog-*.tmp")
	if err != nil {
		return fmt.Errorf("create temporary output: %w", err)
	}
	temporaryName := temporary.Name()
	defer os.Remove(temporaryName)
	if err := temporary.Chmod(0o644); err != nil {
		temporary.Close()
		return fmt.Errorf("set output permissions: %w", err)
	}
	if _, err := temporary.Write(contents); err != nil {
		temporary.Close()
		return fmt.Errorf("write output: %w", err)
	}
	if err := temporary.Sync(); err != nil {
		temporary.Close()
		return fmt.Errorf("sync output: %w", err)
	}
	if err := temporary.Close(); err != nil {
		return fmt.Errorf("close output: %w", err)
	}
	if err := os.Rename(temporaryName, destination); err != nil {
		if errors.Is(err, os.ErrPermission) {
			return fmt.Errorf("replace output: permission denied")
		}
		return fmt.Errorf("replace output: %w", err)
	}
	return nil
}
