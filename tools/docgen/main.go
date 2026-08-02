// Command docgen writes the generated catalog documentation to
// docs/commands.md. CI compares the committed file against the registry
// so documentation cannot drift.
package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"

	"github.com/rselbach/argmax/internal/catalog"
)

func main() {
	out := flag.String("out", "docs/commands.md", "output file")
	flag.Parse()
	docs, err := catalog.GenerateDocs()
	if err != nil {
		fmt.Fprintln(os.Stderr, "docgen:", err)
		os.Exit(1)
	}
	if err := os.MkdirAll(filepath.Dir(*out), 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "docgen:", err)
		os.Exit(1)
	}
	if err := os.WriteFile(*out, []byte(docs), 0o644); err != nil {
		fmt.Fprintln(os.Stderr, "docgen:", err)
		os.Exit(1)
	}
	fmt.Printf("docgen: wrote %s\n", *out)
}
