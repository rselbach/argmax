package shellintegration

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/shellselect"
)

func TestControlsReplaceUnicodeMultilineBufferWithoutExecution(t *testing.T) {
	for _, shell := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(shell.String(), func(t *testing.T) {
			directory := t.TempDir()
			sentinel := filepath.Join(directory, "must-not-execute")
			replacement := "printf 'Troy'\n世界; touch " + sentinel
			cursor := len("printf 'Troy'\n世界")
			events := runControlHarness(t, shell, replacementControl(1, cursor, replacement), "safe")
			wantCursor := len([]rune(replacement[:cursor]))
			if !hasExactProbeBuffer(events, 1, wantCursor, replacement) {
				t.Fatalf("exact replacement snapshot missing: %q", events)
			}
			if _, err := os.Stat(sentinel); !os.IsNotExist(err) {
				t.Fatalf("replacement content executed: %v", err)
			}
		})
	}
}

func TestProbeResynchronizationDoesNotConsumeSnapshotNonce(t *testing.T) {
	control := []byte("argmax-control-v1:resync:1\x00")
	for _, shell := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(shell.String(), func(t *testing.T) {
			events := runResyncHarness(t, shell, control)
			assertFrame(t, events, "probe-resync:1:0")
			if !hasExactProbeBuffer(events, 1, 4, "safe") {
				t.Fatalf("resync consumed the next snapshot nonce: %q", events)
			}
		})
	}
}

func TestMalformedOversizedPartialAndMismatchedControlsAreInert(t *testing.T) {
	controls := replacementControl(2, 8, "attacker")
	controls = append(controls, []byte("argmax-control-v1:replace:1:0:1:GG\x00")...)
	controls = append(controls, bytes.Repeat([]byte{'x'}, 32_818)...)
	controls = append(controls, 0)
	partial := replacementControl(1, 8, "attacker")
	controls = append(controls, partial[:len(partial)-1]...)

	for _, shell := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(shell.String(), func(t *testing.T) {
			events := runControlHarness(t, shell, controls, "safe")
			if !hasExactProbeBuffer(events, 1, 4, "safe") {
				t.Fatalf("invalid controls changed the line: %q", events)
			}
			for _, event := range events {
				if bytes.Contains(event, []byte("attacker")) {
					t.Fatalf("invalid control became observable: %q", events)
				}
			}
		})
	}
}

func runControlHarness(t *testing.T, shell shellselect.Kind, controls []byte, initialLine string) [][]byte {
	t.Helper()
	requireLiveShell(t, shell)
	requireProgram(t, "expect")
	directory := t.TempDir()
	initPath := filepath.Join(directory, "init")
	eventsPath := filepath.Join(directory, "events")
	controlsPath := filepath.Join(directory, "controls")
	if err := os.WriteFile(initPath, []byte(mustScript(t, shell)), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(controlsPath, controls, 0o600); err != nil {
		t.Fatal(err)
	}
	expectScript := `
set timeout 15
log_user 0
spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec 4<"$ARGMAX_TEST_CONTROLS"; exec $ARGMAX_TEST_SHELL $ARGMAX_TEST_ARGS}
if {$env(ARGMAX_TEST_SHELL) eq "fish"} {
  send -- "function fish_prompt; printf 'ARGMAX\\x3e '; end; source \"$env(ARGMAX_TEST_INIT)\"\r"
} else {
  send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"\r"
}
expect "ARGMAX> "
send -- "$env(ARGMAX_TEST_INITIAL)"
send -- "$env(ARGMAX_TEST_PROBE)"
after 1000
send -- "\003"
after 300
send -- "exit\r"
expect eof
`
	environment := []string{
		"ARGMAX_PRIVATE_SESSION=1", "ARGMAX_EVENT_FD=3", "ARGMAX_CONTROL_FD=4",
		"ARGMAX_TEST_EVENTS=" + eventsPath, "ARGMAX_TEST_CONTROLS=" + controlsPath,
		"ARGMAX_TEST_INIT=" + initPath, "ARGMAX_TEST_INITIAL=" + initialLine,
		"ARGMAX_TEST_PROBE=" + probeSequence(shell), "ARGMAX_TEST_SHELL=" + shell.String(),
		"ARGMAX_TEST_ARGS=" + shellArguments(shell), "LC_ALL=en_US.UTF-8",
	}
	output, err := runCommandWithEnv(t, 25*time.Second, nil, environment, "expect", "-c", expectScript)
	if err != nil {
		t.Fatalf("control harness: %v\n%s", err, output)
	}
	return readEvents(t, eventsPath)
}

func runResyncHarness(t *testing.T, shell shellselect.Kind, control []byte) [][]byte {
	t.Helper()
	requireLiveShell(t, shell)
	requireProgram(t, "expect")
	directory := t.TempDir()
	initPath := filepath.Join(directory, "init")
	eventsPath := filepath.Join(directory, "events")
	controlsPath := filepath.Join(directory, "controls")
	if err := os.WriteFile(initPath, []byte(mustScript(t, shell)), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(controlsPath, control, 0o600); err != nil {
		t.Fatal(err)
	}
	expectScript := `
set timeout 15
log_user 0
spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec 4<"$ARGMAX_TEST_CONTROLS"; exec $ARGMAX_TEST_SHELL $ARGMAX_TEST_ARGS}
if {$env(ARGMAX_TEST_SHELL) eq "fish"} {
  send -- "function fish_prompt; printf 'ARGMAX\\x3e '; end; source \"$env(ARGMAX_TEST_INIT)\"\r"
} else {
  send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"\r"
}
expect "ARGMAX> "
send -- "safe"
send -- "$env(ARGMAX_TEST_PROBE)"
after 500
send -- "$env(ARGMAX_TEST_PROBE)"
after 500
send -- "\003"
after 300
send -- "exit\r"
expect eof
`
	environment := []string{
		"ARGMAX_PRIVATE_SESSION=1", "ARGMAX_EVENT_FD=3", "ARGMAX_CONTROL_FD=4",
		"ARGMAX_TEST_EVENTS=" + eventsPath, "ARGMAX_TEST_CONTROLS=" + controlsPath,
		"ARGMAX_TEST_INIT=" + initPath, "ARGMAX_TEST_PROBE=" + probeSequence(shell),
		"ARGMAX_TEST_SHELL=" + shell.String(), "ARGMAX_TEST_ARGS=" + shellArguments(shell),
		"LC_ALL=en_US.UTF-8",
	}
	output, err := runCommandWithEnv(t, 25*time.Second, nil, environment, "expect", "-c", expectScript)
	if err != nil {
		t.Fatalf("resync harness: %v\n%s", err, output)
	}
	return readEvents(t, eventsPath)
}

func replacementControl(request uint64, cursor int, buffer string) []byte {
	wire := []byte(fmt.Sprintf("argmax-control-v1:replace:%d:%d:%d:", request, cursor, len(buffer)))
	wire = append(wire, fmt.Sprintf("%x", []byte(buffer))...)
	return append(wire, 0)
}

func hasExactProbeBuffer(events [][]byte, nonce, cursor int, buffer string) bool {
	wantTail := fmt.Sprintf(":%d:%d:%s", nonce, cursor, buffer)
	for _, event := range events {
		frame := strings.TrimSuffix(string(event), "\n")
		if strings.HasPrefix(frame, "probe-buffer:") && strings.HasSuffix(frame, wantTail) {
			return true
		}
	}
	return false
}
