//go:build linux || darwin

// Package shellselect resolves a supported interactive shell without exposing
// environment or executable paths through errors and formatted values.
package shellselect

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/unix"
)

const (
	// MaxSearchPathBytes is the largest PATH snapshot inspected by Select.
	MaxSearchPathBytes = 64 * 1024
	// MaxSearchPathEntries is the largest number of PATH entries inspected.
	MaxSearchPathEntries = 256

	// EnvironmentOverride is the documented explicit shell override.
	EnvironmentOverride = "argmax_CORE_SHELL"
	// EnvironmentActiveShell is the shell-native discovery hint.
	EnvironmentActiveShell = "ARGMAX_ACTIVE_SHELL"
)

// Kind identifies a supported interactive shell.
type Kind uint8

const (
	// Bash identifies Bash.
	Bash Kind = iota + 1
	// Zsh identifies Zsh.
	Zsh
	// Fish identifies Fish.
	Fish
)

// ParseKind parses the stable shell spelling.
func ParseKind(value string) (Kind, error) {
	switch value {
	case "bash":
		return Bash, nil
	case "zsh":
		return Zsh, nil
	case "fish":
		return Fish, nil
	default:
		return 0, errors.New("shell must be bash, zsh, or fish")
	}
}

func (kind Kind) String() string {
	switch kind {
	case Bash:
		return "bash"
	case Zsh:
		return "zsh"
	case Fish:
		return "fish"
	default:
		return "unsupported"
	}
}

// Source identifies the precedence layer that selected a shell.
type Source uint8

const (
	// SourceCommandLine identifies --shell.
	SourceCommandLine Source = iota + 1
	// SourceEnvironmentOverride identifies argmax_CORE_SHELL.
	SourceEnvironmentOverride
	// SourceActiveShell identifies ARGMAX_ACTIVE_SHELL.
	SourceActiveShell
	// SourceShellEnvironment identifies SHELL.
	SourceShellEnvironment
	// SourceFallback identifies deterministic supported-shell discovery.
	SourceFallback
)

func (source Source) String() string {
	switch source {
	case SourceCommandLine:
		return "command line"
	case SourceEnvironmentOverride:
		return EnvironmentOverride
	case SourceActiveShell:
		return EnvironmentActiveShell
	case SourceShellEnvironment:
		return "SHELL"
	case SourceFallback:
		return "fallback discovery"
	default:
		return "unknown source"
	}
}

// Request contains explicit selections and bounded discovery snapshots.
// Formatting reports only presence, never environment contents.
type Request struct {
	CommandLine        *Kind
	EnvironmentRequest *Kind
	ActiveShell        string
	ShellEnvironment   string
	SearchPath         string
	SearchPathSet      bool
}

// FromEnvironment captures selection inputs. An invalid explicit environment
// override fails closed before discovery begins.
func FromEnvironment(commandLine *Kind, lookup func(string) (string, bool)) (Request, error) {
	request := Request{CommandLine: commandLine}
	if value, ok := lookup(EnvironmentOverride); ok {
		kind, err := ParseKind(value)
		if err != nil {
			return Request{}, fmt.Errorf("invalid %s: %w", EnvironmentOverride, err)
		}
		request.EnvironmentRequest = &kind
	}
	request.ActiveShell, _ = lookup(EnvironmentActiveShell)
	request.ShellEnvironment, _ = lookup("SHELL")
	request.SearchPath, request.SearchPathSet = lookup("PATH")
	return request, nil
}

// FromProcess captures selection inputs from the current process.
func FromProcess(commandLine *Kind) (Request, error) {
	return FromEnvironment(commandLine, os.LookupEnv)
}

// String redacts all discovery values.
func (request Request) String() string {
	return fmt.Sprintf(
		"shellselect.Request{command_line:%v, environment_override:%t, active_shell:%t, shell:%t, path:%t}",
		request.CommandLine, request.EnvironmentRequest != nil, request.ActiveShell != "",
		request.ShellEnvironment != "", request.SearchPathSet,
	)
}

// GoString redacts all discovery values.
func (request Request) GoString() string { return request.String() }

// Selected is one validated supported executable.
type Selected struct {
	kind       Kind
	executable string
	source     Source
}

// Kind returns the selected shell kind.
func (selected Selected) Kind() Kind { return selected.kind }

// Executable returns the absolute validated executable path.
func (selected Selected) Executable() string { return selected.executable }

// Source returns the winning precedence layer.
func (selected Selected) Source() Source { return selected.source }

// String redacts the executable path.
func (selected Selected) String() string {
	return fmt.Sprintf(
		"shellselect.Selected{kind:%s, executable:<validated path>, source:%s}",
		selected.kind, selected.source,
	)
}

// GoString redacts the executable path.
func (selected Selected) GoString() string { return selected.String() }

// ErrorKind identifies a content-free selection failure.
type ErrorKind uint8

const (
	// ErrorRequestedUnavailable means an explicit selection was unavailable.
	ErrorRequestedUnavailable ErrorKind = iota + 1
	// ErrorSearchPathTooLarge means PATH exceeded a safety bound.
	ErrorSearchPathTooLarge
	// ErrorNoSupportedShell means discovery found no executable supported shell.
	ErrorNoSupportedShell
)

// SelectionError reports a shell selection failure without retaining paths.
type SelectionError struct {
	Kind   ErrorKind
	Shell  Kind
	Source Source
}

func (err SelectionError) Error() string {
	switch err.Kind {
	case ErrorRequestedUnavailable:
		return fmt.Sprintf("the %s shell selected by %s is not executable", err.Shell, err.Source)
	case ErrorSearchPathTooLarge:
		return "the shell search path exceeds the safety limit"
	case ErrorNoSupportedShell:
		return "no executable Bash, Zsh, or Fish shell was found"
	default:
		return "shell selection failed"
	}
}

// Select applies explicit fail-closed precedence followed by stale-tolerant
// discovery. PATH order is retained; fallback shell order is Bash, Zsh, Fish.
func Select(request Request) (Selected, error) {
	if err := validateSearchPath(request); err != nil {
		return Selected{}, err
	}
	for _, explicit := range []struct {
		kind   *Kind
		source Source
	}{
		{kind: request.CommandLine, source: SourceCommandLine},
		{kind: request.EnvironmentRequest, source: SourceEnvironmentOverride},
	} {
		if explicit.kind == nil {
			continue
		}
		if selected, ok := resolveKind(*explicit.kind, explicit.source, request); ok {
			return selected, nil
		}
		return Selected{}, SelectionError{
			Kind: ErrorRequestedUnavailable, Shell: *explicit.kind, Source: explicit.source,
		}
	}

	for _, hint := range []struct {
		value  string
		source Source
	}{
		{value: request.ActiveShell, source: SourceActiveShell},
		{value: request.ShellEnvironment, source: SourceShellEnvironment},
	} {
		if selected, ok := resolveHint(hint.value, hint.source, request); ok {
			return selected, nil
		}
	}
	for _, kind := range []Kind{Bash, Zsh, Fish} {
		if selected, ok := resolveKind(kind, SourceFallback, request); ok {
			return selected, nil
		}
	}
	return Selected{}, SelectionError{Kind: ErrorNoSupportedShell}
}

func validateSearchPath(request Request) error {
	if !request.SearchPathSet {
		return nil
	}
	if len(request.SearchPath) > MaxSearchPathBytes {
		return SelectionError{Kind: ErrorSearchPathTooLarge}
	}
	entries := 1
	if request.SearchPath != "" {
		entries = strings.Count(request.SearchPath, string(os.PathListSeparator)) + 1
	}
	if entries > MaxSearchPathEntries {
		return SelectionError{Kind: ErrorSearchPathTooLarge}
	}
	return nil
}

func resolveHint(value string, source Source, request Request) (Selected, bool) {
	if value == "" {
		return Selected{}, false
	}
	base := filepath.Base(value)
	base = strings.TrimPrefix(base, "-")
	kind, err := ParseKind(base)
	if err != nil {
		return Selected{}, false
	}
	if filepath.IsAbs(value) || filepath.Dir(value) != "." {
		candidate, err := filepath.Abs(value)
		if err != nil || !executableFile(candidate) {
			return Selected{}, false
		}
		return Selected{kind: kind, executable: candidate, source: source}, true
	}
	return resolveKind(kind, source, request)
}

func resolveKind(kind Kind, source Source, request Request) (Selected, bool) {
	if _, err := ParseKind(kind.String()); err != nil {
		return Selected{}, false
	}
	if request.SearchPathSet {
		for _, directory := range searchPathEntries(request.SearchPath) {
			candidate, err := filepath.Abs(filepath.Join(directory, kind.String()))
			if err != nil {
				continue
			}
			if executableFile(candidate) {
				return Selected{kind: kind, executable: candidate, source: source}, true
			}
		}
		return Selected{}, false
	}
	for _, directory := range []string{"/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"} {
		candidate := filepath.Join(directory, kind.String())
		if executableFile(candidate) {
			return Selected{kind: kind, executable: candidate, source: source}, true
		}
	}
	return Selected{}, false
}

func searchPathEntries(searchPath string) []string {
	// filepath.SplitList intentionally returns an empty slice for an empty
	// value, but an empty Unix PATH entry denotes the current directory.
	if searchPath == "" {
		return []string{""}
	}
	return filepath.SplitList(searchPath)
}

func executableFile(path string) bool {
	info, err := os.Stat(path)
	return err == nil && info.Mode().IsRegular() && unix.Access(path, unix.X_OK) == nil
}
