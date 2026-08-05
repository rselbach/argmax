package shell

import (
	"os/exec"
	"strings"
	"testing"
)

func TestQuoteArgSyntax(t *testing.T) {
	tests := []struct {
		name string
		kind Kind
		in   string
		want string
	}{
		{name: "safe path", kind: Bash, in: "src/main.go", want: "src/main.go"},
		{name: "empty", kind: Bash, in: "", want: `''`},
		{name: "bash spaces", kind: Bash, in: "My Documents", want: `'My Documents'`},
		{name: "zsh single quote", kind: Zsh, in: "it's", want: `'it'\''s'`},
		{name: "fish single quote", kind: Fish, in: "it's", want: `'it\'s'`},
		{name: "bash backslash", kind: Bash, in: `a\b`, want: `'a\b'`},
		{name: "fish backslash", kind: Fish, in: `a\b`, want: `'a\\b'`},
		{name: "quoted input is data", kind: Bash, in: `"quoted"`, want: `'"quoted"'`},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.kind.QuoteArg(tc.in); got != tc.want {
				t.Errorf("%s.QuoteArg(%q) = %q, want %q", tc.kind, tc.in, got, tc.want)
			}
		})
	}
}

func TestBashQuoteArgRoundTrip(t *testing.T) {
	bash, err := exec.LookPath("bash")
	if err != nil {
		t.Skip("bash not available")
	}
	values := []string{
		"", "plain", "spaces and\ttabs", "line one\nline two",
		"'", `"`, `\`, "$", "`", "$()", ";", "&", "|", ">", "<",
		"*", "?", "[", "]", "(", ")", "!", "#", "~",
		`"already quoted"`, `'already quoted'`,
	}
	for _, value := range values {
		t.Run(value, func(t *testing.T) {
			script := `set -- ` + Bash.QuoteArg(value) + `; test "$#" -eq 1 && printf %s "$1"`
			out, err := exec.Command(bash, "-c", script).Output()
			if err != nil {
				t.Fatalf("bash rejected quoted argument %q: %v", Bash.QuoteArg(value), err)
			}
			if string(out) != value {
				t.Errorf("round trip = %q, want %q (source %q)", out, value, Bash.QuoteArg(value))
			}
		})
	}
}

func TestQuoteArgProtectsShellMetacharacters(t *testing.T) {
	const metacharacters = " \t\n'\"\\$`;&|><*?[]()!#~"
	for _, kind := range []Kind{Bash, Zsh, Fish} {
		quoted := kind.QuoteArg(metacharacters)
		if !strings.HasPrefix(quoted, "'") || !strings.HasSuffix(quoted, "'") {
			t.Errorf("%s metacharacters not quoted: %q", kind, quoted)
		}
	}
}

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
