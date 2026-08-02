package shell

import (
	"strings"
	"testing"
)

func TestParseAliases(t *testing.T) {
	tests := map[string]struct {
		content string
		kind    Kind
		want    map[string]string
	}{
		"bash single quotes": {
			content: "alias gs='git status'\nalias ll=\"ls -la\"\nexport FOO=bar\n",
			kind:    Bash,
			want:    map[string]string{"gs": "git status", "ll": "ls -la"},
		},
		"zsh global flag": {
			content: "alias -g gco='git checkout'\n",
			kind:    Zsh,
			want:    map[string]string{"gco": "git checkout"},
		},
		"fish space form": {
			content: "alias gs 'git status'\nalias also=works\n",
			kind:    Fish,
			want:    map[string]string{"gs": "git status", "also": "works"},
		},
		"malformed lines skipped": {
			content: "alias\nalias =broken\nalias novalue=\n# alias commented='x'\n",
			kind:    Bash,
			want:    map[string]string{},
		},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got := ParseAliases(tc.content, tc.kind)
			if len(got) != len(tc.want) {
				t.Fatalf("parsed %d aliases, want %d: %+v", len(got), len(tc.want), got)
			}
			for _, a := range got {
				if tc.want[a.Name] != a.Expansion {
					t.Errorf("alias %q = %q, want %q", a.Name, a.Expansion, tc.want[a.Name])
				}
			}
		})
	}
}

func TestInitScriptsGuardAutostart(t *testing.T) {
	for _, k := range []Kind{Bash, Zsh, Fish} {
		script := InitScript(k)
		if script == "" {
			t.Fatalf("no init script for %s", k)
		}
		if !strings.Contains(script, "ARGMAX_ACTIVE") {
			t.Errorf("%s script must guard against nesting", k)
		}
		if !strings.Contains(script, "ARGMAX_RESCUE") {
			t.Errorf("%s script must respect rescue mode", k)
		}
		if !strings.Contains(script, "ARGMAX_TTY") {
			t.Errorf("%s script must clear tmux-inherited markers", k)
		}
		if !strings.Contains(script, "ARGMAX_EVENTS_FD") {
			t.Errorf("%s script must report over the event descriptor", k)
		}
	}
}

func TestBlockMarkers(t *testing.T) {
	for _, k := range []Kind{Bash, Zsh, Fish} {
		block := Block(k)
		if !strings.Contains(block, BeginMarker) || !strings.Contains(block, EndMarker) {
			t.Errorf("%s autostart block missing markers", k)
		}
	}
}

func TestDetectExplicitFlag(t *testing.T) {
	if _, err := Detect("powershell", ""); err == nil {
		t.Error("unsupported explicit shell must fail before starting a session")
	}
	got, err := Detect("fish", "zsh")
	if err != nil || got != Fish {
		t.Errorf("explicit flag must win: got %v, %v", got, err)
	}
	got, err = Detect("", "zsh")
	if err != nil || got != Zsh {
		t.Errorf("configured shell must be used: got %v, %v", got, err)
	}
}
