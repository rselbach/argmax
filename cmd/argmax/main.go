package main

import (
	"os"

	"github.com/rselbach/argmax/internal/cli"
)

// version is injected at build time (-X main.version=...). Development
// builds print "dev" (PRD UPD-001).
var version = "dev"

func main() {
	os.Exit(cli.Main(os.Args[1:], version))
}
