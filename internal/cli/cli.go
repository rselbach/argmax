// Package cli implements the argmax command-line surface and subcommand
// dispatch (PRD 9.14): session launch, init/setup/config/reload/version/
// update/crash-log/uninstall.
package cli

import (
	"fmt"
	"os"
	"strings"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/logs"
	"github.com/rselbach/argmax/internal/session"
)

// Exit codes: 0 success, 1 command failure, 2 usage error.

const usageText = `usage: argmax [command] [flags]

Start an interactive session:
  argmax [--shell <bash|zsh|fish>] [--shell-login] [--debug]

Commands:
  init <bash|zsh|fish>   print the shell integration script
  setup [shell]          install the binary, shell hook, and default config
  config init            create the commented default config when absent
  config show            print the resolved (redacted) configuration
  reload                 validate config and signal the active session
  version                print the version
  update                 check the configured channel and self-update
  crash-log [--clear]    show the newest crash report, or remove all
  uninstall              remove hooks, state, config, and the local binary
`

// Main dispatches the CLI. version is the build-injected version ("dev" when
// unset). Returns the process exit code.
func Main(args []string, version string) int {
	if version == "" {
		version = "dev"
	}
	if len(args) == 0 {
		return runSession(nil, version, false)
	}
	switch args[0] {
	case "init":
		return cmdInit(args[1:])
	case "setup":
		return cmdSetup(args[1:])
	case "config":
		return cmdConfig(args[1:])
	case "reload":
		return cmdReload(args[1:])
	case "version":
		if len(args) > 1 {
			return usageError("usage: argmax version")
		}
		fmt.Printf("argmax %s\n", version)
		return 0
	case "update":
		return cmdUpdate(args[1:], version)
	case "crash-log":
		return cmdCrashLog(args[1:])
	case "uninstall":
		return cmdUninstall(args[1:])
	case "__session":
		// Hidden entry point of the watchdog's interactive child.
		return runSession(args[1:], version, true)
	case "-h", "--help", "help":
		fmt.Print(usageText)
		return 0
	default:
		if strings.HasPrefix(args[0], "-") {
			return runSession(args, version, false)
		}
		fmt.Fprintf(os.Stderr, "argmax: unknown command %q\n\n%s", args[0], usageText)
		return 2
	}
}

// usageError reports a malformed invocation.
func usageError(msg string) int {
	fmt.Fprintln(os.Stderr, msg)
	return 2
}

// commandError reports a failed command (PRD 9.14: concise stderr, non-zero).
func commandError(cmd string, err error) int {
	fmt.Fprintf(os.Stderr, "argmax %s: %v\n", cmd, err)
	return 1
}

// runSession starts the wrapped shell session, either as the watchdog parent
// or as the hidden __session child (PRD 9.14, 9.19).
func runSession(args []string, version string, child bool) int {
	opts, err := parseSessionFlags(args)
	if err != nil {
		fmt.Fprintf(os.Stderr, "argmax: %v\n", err)
		return 2
	}
	// Best-effort diagnostics; the session initializes its own logging too.
	paths := config.ResolvePaths()
	_ = logs.Init(paths.LogFile, opts.Debug)
	defer logs.Close()
	if child {
		return session.Run(opts, version)
	}
	return session.RunWatchdog(opts, version)
}

// parseSessionFlags parses the launch flags --shell/-s, --shell-login, and
// --debug/-d, accepting both "--flag value" and "--flag=value" forms.
func parseSessionFlags(args []string) (session.Options, error) {
	var opts session.Options
	for i := 0; i < len(args); i++ {
		arg := args[i]
		name, value, hasValue := arg, "", false
		if j := strings.IndexByte(arg, '='); j >= 0 {
			name, value, hasValue = arg[:j], arg[j+1:], true
		}
		switch name {
		case "--shell", "-s":
			if !hasValue {
				i++
				if i >= len(args) {
					return opts, fmt.Errorf("%s requires a shell name", name)
				}
				value = args[i]
			}
			opts.ShellFlag = value
		case "--shell-login", "--debug", "-d":
			if hasValue {
				return opts, fmt.Errorf("%s does not take a value", name)
			}
			if name == "--shell-login" {
				opts.Login = true
			} else {
				opts.Debug = true
			}
		default:
			return opts, fmt.Errorf("unknown flag %q", arg)
		}
	}
	return opts, nil
}
