//go:build linux || darwin

package main

import (
	"testing"

	"github.com/rselbach/argmax/internal/shellselect"
)

func TestParseArguments(t *testing.T) {
	tests := map[string]struct {
		arguments []string
		wantKind  shellselect.Kind
		wantHelp  bool
		wantError bool
	}{
		"default":           {},
		"separate shell":    {arguments: []string{"--shell", "zsh"}, wantKind: shellselect.Zsh},
		"joined shell":      {arguments: []string{"--shell=fish"}, wantKind: shellselect.Fish},
		"short help":        {arguments: []string{"-h"}, wantHelp: true},
		"long help":         {arguments: []string{"--help"}, wantHelp: true},
		"missing value":     {arguments: []string{"--shell"}, wantError: true},
		"unsupported shell": {arguments: []string{"--shell", "tcsh"}, wantError: true},
		"duplicate":         {arguments: []string{"--shell=bash", "--shell=zsh"}, wantError: true},
		"unknown option":    {arguments: []string{"--version"}, wantError: true},
		"noninteractive":    {arguments: []string{"echo", "hello"}, wantError: true},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			kind, help, err := parseArguments(tc.arguments)
			if (err != nil) != tc.wantError {
				t.Fatalf("parseArguments() error = %v", err)
			}
			if help != tc.wantHelp {
				t.Errorf("help = %t, want %t", help, tc.wantHelp)
			}
			if tc.wantKind == 0 {
				if kind != nil {
					t.Errorf("kind = %v, want nil", kind)
				}
			} else if kind == nil || *kind != tc.wantKind {
				t.Errorf("kind = %v, want %v", kind, tc.wantKind)
			}
		})
	}
}
