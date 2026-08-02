package shellintegration

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/shellselect"
)

func TestScriptSnapshotsAndContracts(t *testing.T) {
	tests := map[shellselect.Kind]struct {
		hash               string
		interactiveGuard   string
		wrapper            string
		probeBuffer        string
		ownership          string
		cleanup            string
		additionalContract []string
	}{
		shellselect.Bash: {
			hash:             "17abdd00a8c29e085dc7e4da43fc360114af793a98d8ce68bce5e806cd86f4ed",
			interactiveGuard: "$- == *i*", wrapper: "exec argmax --shell bash",
			probeBuffer: "probe-buffer:$argmax_unit:", ownership: "argmax-owned-bash-v1",
			cleanup:            "builtin unset __ARGMAX_BASH_HOOKS",
			additionalContract: []string{"builtin fc -ln -0", "builtin shopt -q lithist", "command-start:$argmax_command"},
		},
		shellselect.Zsh: {
			hash:             "8cf62000097ed4beb2afff6a7b99a94a4556ee3494e637b224871689fc8e789a",
			interactiveGuard: "-o interactive", wrapper: "exec argmax --shell zsh",
			probeBuffer: "probe-buffer:$argmax_unit:", ownership: "argmax-owned-zsh-v1",
			cleanup:            "unset __ARGMAX_ZSH_HOOKS",
			additionalContract: []string{"\\setopt no_aliases", "\\builtin eval '", "[[ -o multibyte ]]"},
		},
		shellselect.Fish: {
			hash:             "b6a197dea8e03cfd7a9b9853d070bb2739d8ca502e4a71a966f5ab75f514bdb7",
			interactiveGuard: "status is-interactive", wrapper: "exec argmax --shell fish",
			probeBuffer: "probe-buffer:f:", ownership: "argmax-owned-fish-v1",
			cleanup:            "set -e __ARGMAX_FISH_INSTALLED",
			additionalContract: []string{"fish_posterror", "string collect -N", "set -l argmax_probe \\x1e"},
		},
	}

	common := []string{
		SessionMarkerEnvironment, SessionOwnerPIDEnvironment, "ARGMAX_CONTROL_FD",
		"argmax-control-v1", "replace", "resync", "probe-resync:",
		"__argmax_control_drain", "command-start", "command-stop:", "cwd:",
		"prompt-ready", "capability:", "16384", "16417", "32817", "2147483647",
	}
	for shell, tc := range tests {
		t.Run(shell.String(), func(t *testing.T) {
			script, err := Script(shell)
			if err != nil {
				t.Fatal(err)
			}
			sum := sha256.Sum256([]byte(script))
			if got := hex.EncodeToString(sum[:]); got != tc.hash {
				t.Errorf("snapshot hash = %s, want %s", got, tc.hash)
			}
			if !strings.HasSuffix(script, "\n") {
				t.Error("script is not LF-terminated")
			}
			for _, fragment := range append(common, tc.interactiveGuard, tc.wrapper, tc.probeBuffer, tc.ownership, tc.cleanup) {
				if !strings.Contains(script, fragment) {
					t.Errorf("script lacks contract %q", fragment)
				}
			}
			for _, fragment := range tc.additionalContract {
				if !strings.Contains(script, fragment) {
					t.Errorf("script lacks shell contract %q", fragment)
				}
			}
			for _, forbidden := range []string{"set --", "shift", "$@", "command ps -o comm="} {
				if strings.Contains(script, forbidden) {
					t.Errorf("script contains argument-mutating or probing construct %q", forbidden)
				}
			}
		})
	}

	if MaxSyncEventCharacters != 16_384 || MaxSyncEventFrameCharacters != 16_417 || MaxSyncEventWireBytes != 65_669 {
		t.Fatal("synchronous event limits changed")
	}
	if string(SyncProbeSequence) != "\x1b[argmax-sync~" || string(FishSyncProbeSequence) != "\x1e" {
		t.Fatal("probe sequences changed")
	}
	if _, err := Script(0); !errors.Is(err, ErrUnsupportedShell) {
		t.Fatalf("Script(0) error = %v", err)
	}
}

func TestInstalledShellsAcceptScriptSyntax(t *testing.T) {
	for _, tc := range []struct {
		shell shellselect.Kind
		args  []string
	}{
		{shellselect.Bash, []string{"-n"}},
		{shellselect.Zsh, []string{"-n"}},
		{shellselect.Fish, []string{"--no-execute"}},
	} {
		t.Run(tc.shell.String(), func(t *testing.T) {
			program := requireProgram(t, tc.shell.String())
			script := mustScript(t, tc.shell)
			output, err := runCommand(t, 10*time.Second, []byte(script), program, tc.args...)
			if err != nil {
				t.Fatalf("syntax check: %v\n%s", err, output)
			}
		})
	}
}

func TestBashScriptPassesShellCheck(t *testing.T) {
	program := requireProgram(t, "shellcheck")
	output, err := runCommand(t, 20*time.Second, []byte(mustScript(t, shellselect.Bash)), program, "--shell", "bash", "-")
	if err != nil {
		t.Fatalf("ShellCheck: %v\n%s", err, output)
	}
}

func TestNoninteractiveSourceDoesNothingWithoutPrivateSession(t *testing.T) {
	for _, shell := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(shell.String(), func(t *testing.T) {
			program := requireProgram(t, shell.String())
			directory := t.TempDir()
			path := filepath.Join(directory, "init")
			if err := os.WriteFile(path, []byte(mustScript(t, shell)), 0o600); err != nil {
				t.Fatal(err)
			}
			var args []string
			switch shell {
			case shellselect.Fish:
				args = []string{"--no-config", "-c", "source $ARGMAX_TEST_INIT; functions -q __argmax_emit; and exit 9; exit 0"}
			default:
				args = []string{"-dfc", ". \"$ARGMAX_TEST_INIT\"; type __argmax_emit >/dev/null 2>&1 && exit 9; exit 0"}
				if shell == shellselect.Bash {
					args = []string{"--noprofile", "--norc", "-c", ". \"$ARGMAX_TEST_INIT\"; type __argmax_emit >/dev/null 2>&1 && exit 9; exit 0"}
				}
			}
			output, err := runCommandWithEnv(t, 10*time.Second, nil, []string{"ARGMAX_TEST_INIT=" + path}, program, args...)
			if err != nil {
				t.Fatalf("noninteractive source: %v\n%s", err, output)
			}
			if len(output) != 0 {
				t.Errorf("noninteractive source output = %q", output)
			}
		})
	}
}

func TestLiveSourceTwiceLifecycleAndProbe(t *testing.T) {
	for _, shell := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(shell.String(), func(t *testing.T) {
			events := runLifecycleHarness(t, shell)
			if countFrame(events, "capability:sync-probe:0") < 2 {
				t.Fatalf("source-twice capability events = %q", events)
			}
			assertFrame(t, events, "command-start:echo hi")
			assertFrame(t, events, "command-stop:0")
			if !hasFramePrefix(events, "cwd:") || !hasFrame(events, "prompt-ready") {
				t.Fatalf("missing cwd or prompt event: %q", events)
			}
			if !hasProbeBuffer(events, "echo hi") {
				t.Fatalf("missing correlated exact buffer snapshot: %q", events)
			}
		})
	}
}

func TestInheritedSessionEnvironmentIsCleaned(t *testing.T) {
	for _, shell := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(shell.String(), func(t *testing.T) {
			runInheritedSessionHarness(t, shell)
		})
	}
}

func runLifecycleHarness(t *testing.T, shell shellselect.Kind) [][]byte {
	t.Helper()
	requireLiveShell(t, shell)
	requireProgram(t, "expect")
	directory := t.TempDir()
	initPath := filepath.Join(directory, "init")
	eventsPath := filepath.Join(directory, "events")
	if err := os.WriteFile(initPath, []byte(mustScript(t, shell)), 0o600); err != nil {
		t.Fatal(err)
	}
	expectScript := `
set timeout 12
log_user 0
spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec $ARGMAX_TEST_SHELL $ARGMAX_TEST_ARGS}
if {$env(ARGMAX_TEST_SHELL) eq "fish"} {
  send -- "set -g ARGMAX_TEST_PROMPT_COUNT 0; function fish_prompt; set -g ARGMAX_TEST_PROMPT_COUNT (math \$ARGMAX_TEST_PROMPT_COUNT + 1); printf 'ARGMAX%s\\x3e ' \$ARGMAX_TEST_PROMPT_COUNT; end; source \"$env(ARGMAX_TEST_INIT)\"\r"
  expect "ARGMAX1> "
} else {
  send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"\r"
  expect "ARGMAX> "
}
send -- "source \"$env(ARGMAX_TEST_INIT)\"\r"
if {$env(ARGMAX_TEST_SHELL) eq "fish"} { expect "ARGMAX2> " } else { expect "ARGMAX> " }
send -- "echo hi"
send -- "$env(ARGMAX_TEST_PROBE)"
after 300
send -- "\r"
if {$env(ARGMAX_TEST_SHELL) eq "fish"} { expect "ARGMAX3> " } else { expect "ARGMAX> " }
send -- "exit\r"
expect eof
`
	environment := []string{
		"ARGMAX_PRIVATE_SESSION=1", "ARGMAX_EVENT_FD=3",
		"ARGMAX_TEST_EVENTS=" + eventsPath, "ARGMAX_TEST_INIT=" + initPath,
		"ARGMAX_TEST_SHELL=" + shell.String(), "ARGMAX_TEST_ARGS=" + shellArguments(shell),
		"ARGMAX_TEST_PROBE=" + probeSequence(shell), "LC_ALL=en_US.UTF-8",
	}
	output, err := runCommandWithEnv(t, 20*time.Second, nil, environment, "expect", "-c", expectScript)
	if err != nil {
		t.Fatalf("lifecycle harness: %v\n%s", err, output)
	}
	return readEvents(t, eventsPath)
}

func runInheritedSessionHarness(t *testing.T, shell shellselect.Kind) {
	t.Helper()
	requireLiveShell(t, shell)
	requireProgram(t, "expect")
	directory := t.TempDir()
	initPath := filepath.Join(directory, "init")
	eventsPath := filepath.Join(directory, "events")
	sentinelPath := filepath.Join(directory, "cleared")
	if err := os.WriteFile(initPath, []byte(mustScript(t, shell)), 0o600); err != nil {
		t.Fatal(err)
	}
	expectScript := `
set timeout 12
log_user 0
spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec $ARGMAX_TEST_SHELL $ARGMAX_TEST_ARGS}
if {$env(ARGMAX_TEST_SHELL) eq "fish"} {
  send -- "function fish_prompt; printf 'ARGMAX\\x3e '; end; source \"$env(ARGMAX_TEST_INIT)\"; not set -q ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_CONTROL_FD ARGMAX_ACTIVE_SHELL ARGMAX_SESSION_OWNER_PID; and not functions -q __argmax_emit; and printf cleared > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
} elseif {$env(ARGMAX_TEST_SHELL) eq "zsh"} {
  send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"; test -z \"\${ARGMAX_PRIVATE_SESSION+x}\" && test -z \"\${ARGMAX_EVENT_FD+x}\" && test -z \"\${ARGMAX_CONTROL_FD+x}\" && test -z \"\${ARGMAX_ACTIVE_SHELL+x}\" && test -z \"\${ARGMAX_SESSION_OWNER_PID+x}\" && ! functions __argmax_emit >/dev/null 2>&1 && printf cleared > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
} else {
  send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"; test -z \"\${ARGMAX_PRIVATE_SESSION+x}\" && test -z \"\${ARGMAX_EVENT_FD+x}\" && test -z \"\${ARGMAX_CONTROL_FD+x}\" && test -z \"\${ARGMAX_ACTIVE_SHELL+x}\" && test -z \"\${ARGMAX_SESSION_OWNER_PID+x}\" && ! declare -F __argmax_emit >/dev/null && printf cleared > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
}
expect "ARGMAX> "
send -- "exit\r"
expect eof
`
	environment := []string{
		"ARGMAX_PRIVATE_SESSION=1", "ARGMAX_EVENT_FD=3", "ARGMAX_CONTROL_FD=4",
		"ARGMAX_ACTIVE_SHELL=" + shell.String(), "ARGMAX_SESSION_OWNER_PID=1",
		"ARGMAX_TEST_EVENTS=" + eventsPath, "ARGMAX_TEST_INIT=" + initPath,
		"ARGMAX_TEST_SENTINEL=" + sentinelPath, "ARGMAX_TEST_SHELL=" + shell.String(),
		"ARGMAX_TEST_ARGS=" + shellArguments(shell),
	}
	output, err := runCommandWithEnv(t, 20*time.Second, nil, environment, "expect", "-c", expectScript)
	if err != nil {
		t.Fatalf("inherited-session harness: %v\n%s", err, output)
	}
	if got, err := os.ReadFile(sentinelPath); err != nil || string(got) != "cleared" {
		t.Fatalf("cleanup sentinel = %q, %v", got, err)
	}
	if got, err := os.ReadFile(eventsPath); err != nil || len(got) != 0 {
		t.Fatalf("inherited event channel = %q, %v", got, err)
	}
}

func requireLiveShell(t *testing.T, shell shellselect.Kind) {
	t.Helper()
	program := requireProgram(t, shell.String())
	if shell != shellselect.Bash {
		return
	}
	output, err := runCommand(t, 5*time.Second, nil, program, "-c", "(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))")
	if err != nil {
		t.Skipf("Bash 4.4 or newer unavailable: %v (%s)", err, output)
	}
}

func shellArguments(shell shellselect.Kind) string {
	switch shell {
	case shellselect.Bash:
		return "--noprofile --norc -i"
	case shellselect.Zsh:
		return "-df"
	case shellselect.Fish:
		return "--no-config --interactive"
	default:
		return ""
	}
}

func probeSequence(shell shellselect.Kind) string {
	if shell == shellselect.Fish {
		return string(FishSyncProbeSequence)
	}
	return string(SyncProbeSequence)
}

func mustScript(t *testing.T, shell shellselect.Kind) string {
	t.Helper()
	script, err := Script(shell)
	if err != nil {
		t.Fatal(err)
	}
	return script
}

func requireProgram(t *testing.T, program string) string {
	t.Helper()
	path, err := exec.LookPath(program)
	if err != nil {
		t.Skipf("%s unavailable", program)
	}
	return path
}

func runCommand(t *testing.T, timeout time.Duration, input []byte, program string, arguments ...string) ([]byte, error) {
	t.Helper()
	return runCommandWithEnv(t, timeout, input, nil, program, arguments...)
}

func runCommandWithEnv(t *testing.T, timeout time.Duration, input []byte, environment []string, program string, arguments ...string) ([]byte, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	command := exec.CommandContext(ctx, program, arguments...)
	command.Env = append(os.Environ(), environment...)
	command.Stdin = bytes.NewReader(input)
	output, err := command.CombinedOutput()
	if ctx.Err() != nil {
		return output, fmt.Errorf("hard deadline exceeded: %w", ctx.Err())
	}
	return output, err
}

func readEvents(t *testing.T, path string) [][]byte {
	t.Helper()
	wire, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var events [][]byte
	for _, frame := range bytes.Split(wire, []byte{0}) {
		if len(frame) > 0 {
			events = append(events, frame)
		}
	}
	return events
}

func hasFrame(events [][]byte, want string) bool {
	return countFrame(events, want) > 0
}

func countFrame(events [][]byte, want string) int {
	count := 0
	for _, event := range events {
		if string(event) == want {
			count++
		}
	}
	return count
}

func hasFramePrefix(events [][]byte, prefix string) bool {
	for _, event := range events {
		if bytes.HasPrefix(event, []byte(prefix)) {
			return true
		}
	}
	return false
}

func hasProbeBuffer(events [][]byte, buffer string) bool {
	for _, event := range events {
		if bytes.HasPrefix(event, []byte("probe-buffer:")) && strings.HasSuffix(strings.TrimSuffix(string(event), "\n"), ":"+buffer) {
			return true
		}
	}
	return false
}

func assertFrame(t *testing.T, events [][]byte, want string) {
	t.Helper()
	if !hasFrame(events, want) {
		t.Errorf("missing frame %q in %q", want, events)
	}
}
