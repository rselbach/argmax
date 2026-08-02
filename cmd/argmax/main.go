// Command argmax is a terminal-resident completion and prediction tool
// that wraps an interactive shell in a PTY. See argmax-prd.md for the
// product contract.
package main

import (
	"os"

	"github.com/rselbach/argmax/internal/cli"
)

// version is injected by the release build:
//
//	go build -ldflags "-X main.version=1.2.3"
var version = "dev"

func main() {
	if version != "" {
		cli.Version = version
	}
	os.Exit(cli.Main(os.Args[1:]))
}
