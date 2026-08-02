//go:build linux || darwin

package main

import (
	"os"
	"testing"

	"github.com/rselbach/argmax/internal/shellintegration"
	"github.com/rselbach/argmax/internal/shellselect"
)

func TestParseArguments(t *testing.T) {
	tests := map[string]struct {
		arguments []string
		wantKind  shellselect.Kind
		wantInit  shellselect.Kind
		wantHelp  bool
		wantError bool
	}{
		"default":             {},
		"separate shell":      {arguments: []string{"--shell", "zsh"}, wantKind: shellselect.Zsh},
		"joined shell":        {arguments: []string{"--shell=fish"}, wantKind: shellselect.Fish},
		"init bash":           {arguments: []string{"init", "bash"}, wantInit: shellselect.Bash},
		"init zsh":            {arguments: []string{"init", "zsh"}, wantInit: shellselect.Zsh},
		"init fish":           {arguments: []string{"init", "fish"}, wantInit: shellselect.Fish},
		"short help":          {arguments: []string{"-h"}, wantHelp: true},
		"long help":           {arguments: []string{"--help"}, wantHelp: true},
		"missing value":       {arguments: []string{"--shell"}, wantError: true},
		"unsupported shell":   {arguments: []string{"--shell", "tcsh"}, wantError: true},
		"duplicate":           {arguments: []string{"--shell=bash", "--shell=zsh"}, wantError: true},
		"unknown option":      {arguments: []string{"--version"}, wantError: true},
		"noninteractive":      {arguments: []string{"echo", "hello"}, wantError: true},
		"init missing shell":  {arguments: []string{"init"}, wantError: true},
		"init extra argument": {arguments: []string{"init", "bash", "extra"}, wantError: true},
		"init invalid shell":  {arguments: []string{"init", "tcsh"}, wantError: true},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			parsed, err := parseArguments(tc.arguments)
			if (err != nil) != tc.wantError {
				t.Fatalf("parseArguments() error = %v", err)
			}
			if parsed.help != tc.wantHelp {
				t.Errorf("help = %t, want %t", parsed.help, tc.wantHelp)
			}
			assertKind(t, "interactive shell", parsed.interactiveShell, tc.wantKind)
			assertKind(t, "init shell", parsed.initShell, tc.wantInit)
		})
	}
}

func TestExecuteInitWritesOnlyScriptToStandardOutput(t *testing.T) {
	for _, shell := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(shell.String(), func(t *testing.T) {
			stdout, err := os.CreateTemp(t.TempDir(), "stdout")
			if err != nil {
				t.Fatal(err)
			}
			stderr, err := os.CreateTemp(t.TempDir(), "stderr")
			if err != nil {
				t.Fatal(err)
			}
			originalStdout, originalStderr := os.Stdout, os.Stderr
			os.Stdout, os.Stderr = stdout, stderr
			_, code := execute([]string{"init", shell.String()})
			os.Stdout, os.Stderr = originalStdout, originalStderr
			if err := stdout.Close(); err != nil {
				t.Fatal(err)
			}
			if err := stderr.Close(); err != nil {
				t.Fatal(err)
			}
			if code != 0 {
				t.Errorf("exit code = %d", code)
			}
			got, err := os.ReadFile(stdout.Name())
			if err != nil {
				t.Fatal(err)
			}
			want, err := shellintegration.Script(shell)
			if err != nil {
				t.Fatal(err)
			}
			if string(got) != want {
				t.Error("standard output did not equal the sourceable script")
			}
			errors, err := os.ReadFile(stderr.Name())
			if err != nil {
				t.Fatal(err)
			}
			if len(errors) != 0 {
				t.Errorf("standard error = %q", errors)
			}
		})
	}
}

func assertKind(t *testing.T, name string, got *shellselect.Kind, want shellselect.Kind) {
	t.Helper()
	if want == 0 {
		if got != nil {
			t.Errorf("%s = %v, want nil", name, *got)
		}
		return
	}
	if got == nil || *got != want {
		t.Errorf("%s = %v, want %v", name, got, want)
	}
}
