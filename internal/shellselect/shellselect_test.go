//go:build linux || darwin

package shellselect

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func installShell(t *testing.T, directory string, kind Kind, mode os.FileMode) string {
	t.Helper()
	path := filepath.Join(directory, kind.String())
	if err := os.WriteFile(path, []byte("#!/bin/sh\nexit 0\n"), mode); err != nil {
		t.Fatalf("WriteFile(): %v", err)
	}
	return path
}

func kindPointer(kind Kind) *Kind { return &kind }

func TestSelectPrecedenceAndFailClosedRequests(t *testing.T) {
	cli := t.TempDir()
	environment := t.TempDir()
	installShell(t, cli, Bash, 0o700)
	installShell(t, environment, Fish, 0o700)
	request := Request{
		CommandLine:        kindPointer(Bash),
		EnvironmentRequest: kindPointer(Fish),
		ActiveShell:        "zsh",
		ShellEnvironment:   "fish",
		SearchPath:         cli + string(os.PathListSeparator) + environment,
		SearchPathSet:      true,
	}
	selected, err := Select(request)
	if err != nil {
		t.Fatalf("Select(): %v", err)
	}
	if selected.Kind() != Bash || selected.Source() != SourceCommandLine ||
		selected.Executable() != filepath.Join(cli, "bash") || !filepath.IsAbs(selected.Executable()) {
		t.Errorf("Select() = %#v", selected)
	}

	_, err = Select(Request{
		CommandLine: kindPointer(Zsh), SearchPath: environment, SearchPathSet: true,
	})
	var selectionErr SelectionError
	if !errors.As(err, &selectionErr) || selectionErr.Kind != ErrorRequestedUnavailable ||
		selectionErr.Shell != Zsh || selectionErr.Source != SourceCommandLine {
		t.Fatalf("unavailable explicit error = %#v", err)
	}
}

func TestSelectStaleHintsFallThroughDeterministically(t *testing.T) {
	directory := t.TempDir()
	fish := installShell(t, directory, Fish, 0o700)
	selected, err := Select(Request{
		ActiveShell:      "tcsh",
		ShellEnvironment: "/missing/zsh",
		SearchPath:       directory,
		SearchPathSet:    true,
	})
	if err != nil {
		t.Fatalf("Select(): %v", err)
	}
	if selected.Kind() != Fish || selected.Source() != SourceFallback ||
		selected.Executable() != fish {
		t.Errorf("Select() = %#v", selected)
	}

	installShell(t, directory, Zsh, 0o700)
	installShell(t, directory, Bash, 0o700)
	selected, err = Select(Request{SearchPath: directory, SearchPathSet: true})
	if err != nil || selected.Kind() != Bash {
		t.Errorf("fallback = (%#v, %v), want Bash", selected, err)
	}
}

func TestSelectDiscoveryPathsAndExecutableValidation(t *testing.T) {
	directory := t.TempDir()
	zsh := installShell(t, directory, Zsh, 0o700)
	selected, err := Select(Request{
		ActiveShell: zsh, SearchPath: t.TempDir(), SearchPathSet: true,
	})
	if err != nil || selected.Executable() != zsh || selected.Source() != SourceActiveShell {
		t.Fatalf("absolute hint = (%#v, %v)", selected, err)
	}

	if err := os.Chmod(zsh, 0o600); err != nil {
		t.Fatal(err)
	}
	fish := installShell(t, directory, Fish, 0o700)
	selected, err = Select(Request{
		ActiveShell: zsh, ShellEnvironment: "fish", SearchPath: directory, SearchPathSet: true,
	})
	if err != nil || selected.Executable() != fish || selected.Source() != SourceShellEnvironment {
		t.Errorf("non-executable stale hint = (%#v, %v)", selected, err)
	}
}

func TestEnvironmentOverrideValidationAndPathBounds(t *testing.T) {
	values := map[string]string{
		EnvironmentOverride:    "powershell",
		EnvironmentActiveShell: "/secret/active",
		"SHELL":                "/secret/shell",
		"PATH":                 "/secret/path",
	}
	_, err := FromEnvironment(nil, func(name string) (string, bool) {
		value, ok := values[name]
		return value, ok
	})
	if err == nil || strings.Contains(fmt.Sprintf("%v %#v", err, err), "/secret") {
		t.Fatalf("invalid override error = %#v", err)
	}

	for _, path := range []string{
		strings.Repeat("x", MaxSearchPathBytes+1),
		strings.Repeat(string(os.PathListSeparator), MaxSearchPathEntries),
	} {
		_, err := Select(Request{SearchPath: path, SearchPathSet: true})
		var selectionErr SelectionError
		if !errors.As(err, &selectionErr) || selectionErr.Kind != ErrorSearchPathTooLarge {
			t.Errorf("oversized PATH error = %#v", err)
		}
	}
}

func TestFormattingRedactsPathsAndEnvironmentValues(t *testing.T) {
	secret := "/private/Greendale/hunter2"
	request := Request{
		ActiveShell: secret, ShellEnvironment: secret, SearchPath: secret, SearchPathSet: true,
	}
	selected := Selected{kind: Bash, executable: secret, source: SourceFallback}
	formatted := fmt.Sprintf("%v %#v %v %#v", request, request, selected, selected)
	if strings.Contains(formatted, secret) {
		t.Errorf("formatting exposed path: %s", formatted)
	}
}
