// Command figexport evaluates built Fig autocomplete specs (self-contained
// ES modules from the MIT-licensed @withfig/autocomplete npm package) in an
// embedded JavaScript interpreter and dumps each spec as JSON. Functions
// are replaced by {"__function": true} markers; serializable generator
// fields such as script arrays and templates survive as data.
//
// This tool runs at catalog-generation time only; nothing here ships in
// the argmax binary.
package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/dop251/goja"
)

var (
	exportRe  = regexp.MustCompile(`export\{[^}]*\};?`)
	defaultRe = regexp.MustCompile(`([A-Za-z0-9_$]+) as default`)
)

const serialize = `JSON.stringify(globalThis.__spec, function(k, v) {
	if (typeof v === "function") { return {"__function": true}; }
	return v;
})`

func main() {
	in := flag.String("in", "", "directory containing built Fig spec .js files")
	out := flag.String("out", "", "output directory for .json dumps")
	only := flag.String("only", "", "comma-separated spec names to export (default: all)")
	flag.Parse()
	if *in == "" || *out == "" {
		fmt.Fprintln(os.Stderr, "figexport: -in and -out are required")
		os.Exit(2)
	}
	if err := os.MkdirAll(*out, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "figexport:", err)
		os.Exit(1)
	}
	wanted := map[string]bool{}
	for _, name := range strings.Split(*only, ",") {
		if name = strings.TrimSpace(name); name != "" {
			wanted[name] = true
		}
	}

	entries, err := os.ReadDir(*in)
	if err != nil {
		fmt.Fprintln(os.Stderr, "figexport:", err)
		os.Exit(1)
	}
	exported, failed := 0, 0
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".js") {
			continue
		}
		name := strings.TrimSuffix(e.Name(), ".js")
		if len(wanted) > 0 && !wanted[name] {
			continue
		}
		if err := export(filepath.Join(*in, e.Name()), filepath.Join(*out, name+".json")); err != nil {
			fmt.Fprintf(os.Stderr, "figexport: %s: %v\n", name, err)
			failed++
			continue
		}
		exported++
	}
	fmt.Printf("figexport: exported %d specs, %d failed\n", exported, failed)
	if exported == 0 {
		os.Exit(1)
	}
}

func export(srcPath, dstPath string) error {
	src, err := os.ReadFile(srcPath)
	if err != nil {
		return err
	}
	code := exportRe.ReplaceAllStringFunc(string(src), func(stmt string) string {
		m := defaultRe.FindStringSubmatch(stmt)
		if m == nil {
			return ";"
		}
		return ";globalThis.__spec = " + m[1] + ";"
	})
	if code == string(src) {
		return fmt.Errorf("no default export found")
	}
	vm := goja.New()
	// Minimal environment shims for specs that peek at their host.
	if err := setupShims(vm); err != nil {
		return err
	}
	if _, err := vm.RunString(code); err != nil {
		return fmt.Errorf("evaluate: %w", err)
	}
	res, err := vm.RunString(serialize)
	if err != nil {
		return fmt.Errorf("serialize: %w", err)
	}
	str, ok := res.Export().(string)
	if !ok || str == "" || str == "undefined" {
		return fmt.Errorf("spec did not serialize to JSON")
	}
	return os.WriteFile(dstPath, []byte(str), 0o644)
}

func setupShims(vm *goja.Runtime) error {
	_, err := vm.RunString(`
		globalThis.process = { env: {}, platform: "darwin" };
		globalThis.window = globalThis;
	`)
	return err
}
