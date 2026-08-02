//go:build linux || darwin

// Command argmax-go is the runnable Go transparent-shell milestone.
package main

import (
	"errors"
	"fmt"
	"os"
	osSignal "os/signal"
	"strings"

	"github.com/rselbach/argmax/internal/pty"
	"github.com/rselbach/argmax/internal/shellintegration"
	"github.com/rselbach/argmax/internal/shellselect"
	"github.com/rselbach/argmax/internal/transparuntime"
	"golang.org/x/sys/unix"
)

const usage = `Usage:
  argmax-go [--shell bash|zsh|fish]
  argmax-go init bash|zsh|fish

Run one supported interactive shell through a byte-transparent pseudoterminal,
or print sourceable shell integration code.

Options:
  --shell SHELL  select bash, zsh, or fish
  -h, --help     show this help
`

func main() {
	status, code := execute(os.Args[1:])
	if status.Kind == pty.ExitSignaled {
		osSignal.Reset(status.Signal)
		if err := unix.Kill(unix.Getpid(), status.Signal); err != nil {
			fmt.Fprintln(os.Stderr, "argmax-go: reproduce shell signal:", err)
		}
	}
	os.Exit(code)
}

func execute(arguments []string) (pty.ExitStatus, int) {
	parsed, err := parseArguments(arguments)
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax-go:", err)
		fmt.Fprint(os.Stderr, usage)
		return pty.ExitStatus{}, 2
	}
	if parsed.help {
		fmt.Print(usage)
		return pty.ExitStatus{}, 0
	}
	if parsed.initShell != nil {
		script, err := shellintegration.Script(*parsed.initShell)
		if err != nil {
			fmt.Fprintln(os.Stderr, "argmax-go:", err)
			return pty.ExitStatus{}, 2
		}
		if _, err := fmt.Fprint(os.Stdout, script); err != nil {
			fmt.Fprintln(os.Stderr, "argmax-go: write shell integration:", err)
			return pty.ExitStatus{}, 1
		}
		return pty.ExitStatus{}, 0
	}
	request, err := shellselect.FromProcess(parsed.interactiveShell)
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax-go:", err)
		return pty.ExitStatus{}, 2
	}
	shell, err := shellselect.Select(request)
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax-go:", err)
		return pty.ExitStatus{}, 1
	}
	cwd, err := os.Getwd()
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax-go: determine working directory")
		return pty.ExitStatus{}, 1
	}
	marker, err := transparuntime.GenerateMarker()
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax-go:", err)
		return pty.ExitStatus{}, 1
	}
	status, err := transparuntime.Run(transparuntime.Config{
		Shell: shell, Cwd: cwd, Marker: marker, Input: os.Stdin, Output: os.Stdout,
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax-go:", err)
		return status, 1
	}
	code, ok := status.WrapperCode()
	if !ok {
		fmt.Fprintln(os.Stderr, "argmax-go: shell exit status is unavailable")
		return status, 1
	}
	return status, code
}

type parsedArguments struct {
	interactiveShell *shellselect.Kind
	initShell        *shellselect.Kind
	help             bool
}

func parseArguments(arguments []string) (parsedArguments, error) {
	if len(arguments) > 0 && arguments[0] == "init" {
		if len(arguments) != 2 {
			return parsedArguments{}, errors.New("init requires exactly one shell")
		}
		kind, err := shellselect.ParseKind(arguments[1])
		if err != nil {
			return parsedArguments{}, err
		}
		return parsedArguments{initShell: &kind}, nil
	}

	var parsed parsedArguments
	for index := 0; index < len(arguments); index++ {
		argument := arguments[index]
		switch {
		case argument == "-h" || argument == "--help":
			parsed.help = true
			return parsed, nil
		case argument == "--shell":
			index++
			if index == len(arguments) {
				return parsedArguments{}, errors.New("--shell requires a value")
			}
			kind, err := shellselect.ParseKind(arguments[index])
			if err != nil {
				return parsedArguments{}, err
			}
			if parsed.interactiveShell != nil {
				return parsedArguments{}, errors.New("--shell may be specified only once")
			}
			parsed.interactiveShell = &kind
		case strings.HasPrefix(argument, "--shell="):
			kind, err := shellselect.ParseKind(strings.TrimPrefix(argument, "--shell="))
			if err != nil {
				return parsedArguments{}, err
			}
			if parsed.interactiveShell != nil {
				return parsedArguments{}, errors.New("--shell may be specified only once")
			}
			parsed.interactiveShell = &kind
		case strings.HasPrefix(argument, "-"):
			return parsedArguments{}, fmt.Errorf("unknown option %q", argument)
		default:
			return parsedArguments{}, fmt.Errorf("interactive mode does not accept argument %q", argument)
		}
	}
	return parsed, nil
}
