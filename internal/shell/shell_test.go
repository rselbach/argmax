package shell

import (
	"strings"
	"testing"
)

// stubParent replaces the OS parent-process lookup for the duration of a
// test so Detect never depends on the real parent process.
func stubParent(t *testing.T, lookup func(pid int) (procInfo, bool)) {
	t.Helper()
	old := parentLookup
	parentLookup = lookup
	t.Cleanup(func() { parentLookup = old })
}

// noParent makes parent-process inspection find nothing.
func noParent(pid int) (procInfo, bool) { return procInfo{}, false }

func TestDetectFlagWinsOverConfig(t *testing.T) {
	t.Setenv("ARGMAX_SHELL", "fish")
	t.Setenv("SHELL", "/bin/bash")
	stubParent(t, noParent)

	s, err := Detect("zsh", "bash")
	if err != nil {
		t.Fatalf("Detect: %v", err)
	}
	if s != Zsh {
		t.Fatalf("flag should win: got %q, want %q", s, Zsh)
	}
}

func TestDetectConfigWhenNoFlag(t *testing.T) {
	t.Setenv("ARGMAX_SHELL", "")
	t.Setenv("SHELL", "")
	stubParent(t, noParent)

	s, err := Detect("", "fish")
	if err != nil {
		t.Fatalf("Detect: %v", err)
	}
	if s != Fish {
		t.Fatalf("got %q, want %q", s, Fish)
	}
}

func TestDetectUnsupportedExplicitValues(t *testing.T) {
	t.Setenv("SHELL", "")
	stubParent(t, noParent)

	for _, tc := range []struct {
		name    string
		flag    string
		cfg     string
		marker  string
		wantErr string
	}{
		{"flag", "pwsh", "", "", "--shell flag"},
		{"config", "", "tcsh", "", "configuration"},
		{"marker", "", "", "ksh", "ARGMAX_SHELL marker"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("ARGMAX_SHELL", tc.marker)
			_, err := Detect(tc.flag, tc.cfg)
			if err == nil {
				t.Fatal("expected error for unsupported explicit shell")
			}
			if !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("error %q should mention source %q", err, tc.wantErr)
			}
		})
	}
}

func TestDetectMarkerBeatsParentAndShellEnv(t *testing.T) {
	t.Setenv("ARGMAX_SHELL", "fish")
	t.Setenv("SHELL", "/bin/bash")
	stubParent(t, func(pid int) (procInfo, bool) {
		return procInfo{name: "zsh", ppid: 1}, true
	})

	s, err := Detect("", "")
	if err != nil {
		t.Fatalf("Detect: %v", err)
	}
	if s != Fish {
		t.Fatalf("marker should win: got %q, want %q", s, Fish)
	}
}

func TestDetectParentBeatsShellEnv(t *testing.T) {
	t.Setenv("ARGMAX_SHELL", "")
	t.Setenv("SHELL", "/bin/bash")
	stubParent(t, func(pid int) (procInfo, bool) {
		return procInfo{name: "/bin/zsh", ppid: 1}, true
	})

	s, err := Detect("", "")
	if err != nil {
		t.Fatalf("Detect: %v", err)
	}
	if s != Zsh {
		t.Fatalf("parent should win over SHELL: got %q, want %q", s, Zsh)
	}
}

func TestDetectShellEnvFallback(t *testing.T) {
	t.Setenv("ARGMAX_SHELL", "")
	stubParent(t, noParent)

	t.Setenv("SHELL", "/bin/zsh")
	if s, err := Detect("", ""); err != nil || s != Zsh {
		t.Fatalf("SHELL=/bin/zsh: got %q, %v; want zsh, nil", s, err)
	}

	t.Setenv("SHELL", "/weird/xonsh")
	if s, err := Detect("", ""); err != nil || s != Bash {
		t.Fatalf("weird SHELL should degrade to bash: got %q, %v", s, err)
	}

	t.Setenv("SHELL", "")
	if s, err := Detect("", ""); err != nil || s != Bash {
		t.Fatalf("empty SHELL should fall back to bash: got %q, %v", s, err)
	}
}

func TestDetectNormalizesValues(t *testing.T) {
	t.Setenv("ARGMAX_SHELL", "")
	t.Setenv("SHELL", "")
	stubParent(t, noParent)

	for value, want := range map[string]Shell{
		"bash":            Bash,
		"-bash":           Bash,
		"/usr/bin/zsh":    Zsh,
		" /opt/fish ":     Fish,
		"ZSH":             Zsh,
		"/usr/local/fish": Fish,
	} {
		s, err := Detect(value, "")
		if err != nil || s != want {
			t.Fatalf("Detect(%q): got %q, %v; want %q, nil", value, s, err, want)
		}
	}
}

func TestDetectFromParentWalksAncestors(t *testing.T) {
	chain := map[int]procInfo{
		100: {name: "tmux", ppid: 50},
		50:  {name: "/opt/homebrew/bin/fish", ppid: 1},
	}
	s, ok := detectFromParent(100, func(pid int) (procInfo, bool) {
		info, ok := chain[pid]
		return info, ok
	})
	if !ok || s != Fish {
		t.Fatalf("got %q, %v; want fish, true", s, ok)
	}
}

func TestDetectFromParentNoShell(t *testing.T) {
	chain := map[int]procInfo{
		100: {name: "tmux", ppid: 50},
		50:  {name: "launchd", ppid: 1},
	}
	if s, ok := detectFromParent(100, func(pid int) (procInfo, bool) {
		info, ok := chain[pid]
		return info, ok
	}); ok {
		t.Fatalf("expected no shell, got %q", s)
	}

	if s, ok := detectFromParent(100, noParent); ok {
		t.Fatalf("lookup failure should yield no shell, got %q", s)
	}
}

func TestDetectFromParentTerminatesOnCycle(t *testing.T) {
	// A corrupt lookup that loops forever must still terminate via the
	// depth bound.
	if s, ok := detectFromParent(100, func(pid int) (procInfo, bool) {
		return procInfo{name: "weird", ppid: pid}, true
	}); ok {
		t.Fatalf("cycle should yield no shell, got %q", s)
	}
}

func TestShellValid(t *testing.T) {
	for _, s := range []Shell{Bash, Zsh, Fish} {
		if !s.Valid() {
			t.Fatalf("%q should be valid", s)
		}
	}
	for _, s := range []Shell{"", "ksh", "BASH", "shell"} {
		if s.Valid() {
			t.Fatalf("%q should be invalid", s)
		}
	}
}

func TestExecutable(t *testing.T) {
	// bash exists on every supported platform; the result must name it.
	if path := Bash.Executable(); !strings.HasSuffix(path, "bash") {
		t.Fatalf("Bash.Executable() = %q, want a path ending in bash", path)
	}
	// Unknown shells get the /bin/<name> fallback.
	if path := Shell("zsh5").Executable(); path != "/bin/zsh5" {
		t.Fatalf("fallback = %q, want /bin/zsh5", path)
	}
}

func TestArgs(t *testing.T) {
	cases := []struct {
		shell Shell
		login bool
		want  []string
	}{
		{Bash, false, []string{"-i"}},
		{Bash, true, []string{"--login", "-i"}},
		{Zsh, false, []string{"-i"}},
		{Zsh, true, []string{"-l", "-i"}},
		{Fish, false, []string{}},
		{Fish, true, []string{"--login"}},
		{Shell("ksh"), false, nil},
	}
	for _, tc := range cases {
		got := tc.shell.Args(tc.login)
		if len(got) != len(tc.want) {
			t.Fatalf("%s.Args(%v) = %v, want %v", tc.shell, tc.login, got, tc.want)
		}
		for i := range got {
			if got[i] != tc.want[i] {
				t.Fatalf("%s.Args(%v) = %v, want %v", tc.shell, tc.login, got, tc.want)
			}
		}
	}
}
