package shell

import (
	"bytes"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
)

func TestInitScriptContent(t *testing.T) {
	cases := []struct {
		shell    Shell
		contains []string
	}{
		{Bash, []string{
			"PROMPT_COMMAND", "__argmax_prompt", "__ARGMAX_HOOKED",
			`$- == *i*`, "ARGMAX_SHELL=bash",
		}},
		{Zsh, []string{
			"add-zsh-hook", "precmd", "preexec", "zle -N", "zle .",
			"zle-line-init", "self-insert", `$- == *i*`, "ARGMAX_SHELL=zsh",
		}},
		{Fish, []string{
			"fish_prompt", "fish_preexec", "fish_postexec",
			"status is-interactive", "set -gx ARGMAX_SHELL fish",
		}},
	}
	for _, tc := range cases {
		t.Run(string(tc.shell), func(t *testing.T) {
			script := tc.shell.InitScript()
			common := []string{
				BlockBegin, BlockEnd, // documented in the header comment
				"ARGMAX_SESSION", "ARGMAX_RESCUE", "ARGMAX_HOOK_FD",
				"exec argmax", "kill -0", // staleness + autostart
			}
			for _, want := range append(common, tc.contains...) {
				if !strings.Contains(script, want) {
					t.Errorf("%s script should contain %q", tc.shell, want)
				}
			}
			// No line may BE a marker, or the managed-block scanner would
			// match the header comment instead of an installed block.
			for _, line := range strings.Split(script, "\n") {
				if line == BlockBegin || line == BlockEnd {
					t.Errorf("script line %q must not equal a marker exactly", line)
				}
			}
		})
	}
}

func TestInitScriptUnsupported(t *testing.T) {
	if got := Shell("ksh").InitScript(); got != "" {
		t.Fatalf("unsupported shell script = %q, want empty", got)
	}
}

// syntaxCheck parses the init script with the shell's own no-execute mode.
func syntaxCheck(t *testing.T, shell string, args []string, script string) {
	t.Helper()
	path, err := exec.LookPath(shell)
	if err != nil {
		t.Skipf("%s not on PATH", shell)
	}
	file := filepath.Join(t.TempDir(), "init."+shell)
	if err := os.WriteFile(file, []byte(script), 0o600); err != nil {
		t.Fatal(err)
	}
	out, err := exec.Command(path, append(args, file)...).CombinedOutput()
	if err != nil {
		t.Fatalf("%s syntax check failed: %v\n%s", shell, err, out)
	}
}

func TestInitScriptSyntax(t *testing.T) {
	for _, tc := range []struct {
		shell string
		args  []string
	}{
		{"bash", []string{"-n"}},
		{"zsh", []string{"-n"}},
		{"fish", []string{"--no-execute"}},
	} {
		t.Run(tc.shell, func(t *testing.T) {
			syntaxCheck(t, tc.shell, tc.args, Shell(tc.shell).InitScript())
		})
	}
}

// runWithHookPipe executes `<shell> -c <code>` with fd 3 connected to a
// pipe, exactly how the session hands the hook fd to the wrapped shell, and
// decodes whatever the hooks wrote. code may reference the init script path
// via the shell's first positional parameter.
func runWithHookPipe(t *testing.T, shell, code, scriptPath, dir string) (stdout, stderr string, events []Event, rest []byte) {
	t.Helper()
	path, err := exec.LookPath(shell)
	if err != nil {
		t.Skipf("%s not on PATH", shell)
	}
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	args := []string{"-c", code, shell, scriptPath}
	if shell == "fish" {
		args = []string{"-c", code, scriptPath} // fish: $argv[1]
	}
	cmd := exec.Command(path, args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), HookFDEnv+"=3")
	cmd.ExtraFiles = []*os.File{w} // becomes fd 3 in the child
	var outBuf, errBuf bytes.Buffer
	cmd.Stdout = &outBuf
	cmd.Stderr = &errBuf
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	_ = w.Close() // the child holds its own copy now
	data, _ := io.ReadAll(r)
	_ = r.Close()
	if err := cmd.Wait(); err != nil {
		t.Fatalf("%s exited with %v; stderr=%q", shell, err, errBuf.String())
	}
	events, rest = ParseEvents(data)
	return outBuf.String(), errBuf.String(), events, rest
}

// writeScript writes content to a temp file and returns its path.
func writeScript(t *testing.T, name, content string) string {
	t.Helper()
	file := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(file, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	return file
}

// workDir returns a temp directory with symlinks resolved: shells report
// the physical path in $PWD (macOS /var -> /private/var).
func workDir(t *testing.T) string {
	t.Helper()
	dir, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestBashHookRuntime(t *testing.T) {
	dir := workDir(t)
	script := writeScript(t, "init.bash", Bash.InitScript())

	// Source twice to prove the PROMPT_COMMAND composition is idempotent.
	code := `ARGMAX_SESSION=$$
source "$1"
source "$1"
false
__argmax_prompt
cd /
__argmax_prompt
printf 'PC=%s\n' "${PROMPT_COMMAND-}"`
	stdout, stderr, events, rest := runWithHookPipe(t, "bash", code, script, dir)

	if stderr != "" {
		t.Errorf("hooks must be silent, stderr = %q", stderr)
	}
	if len(rest) != 0 {
		t.Errorf("unconsumed record bytes: %q", rest)
	}
	want := []Event{
		{Type: "prompt", Payload: "1", Cursor: -1},
		{Type: "cwd", Payload: dir, Cursor: -1},
		{Type: "prompt", Payload: "0", Cursor: -1},
		{Type: "cwd", Payload: "/", Cursor: -1},
	}
	if !reflect.DeepEqual(events, want) {
		t.Errorf("events = %+v, want %+v", events, want)
	}
	if strings.TrimSpace(stdout) != "PC=__argmax_prompt" {
		t.Errorf("PROMPT_COMMAND after double source = %q, want exactly __argmax_prompt", stdout)
	}
}

func TestBashHookPromptCommandArray(t *testing.T) {
	script := writeScript(t, "init.bash", Bash.InitScript())
	code := `ARGMAX_SESSION=$$
PROMPT_COMMAND=("true" "echo user")
source "$1"
source "$1"
declare -p PROMPT_COMMAND`
	stdout, stderr, _, _ := runWithHookPipe(t, "bash", code, script, t.TempDir())

	if stderr != "" {
		t.Errorf("hooks must be silent, stderr = %q", stderr)
	}
	if !strings.Contains(stdout, "declare -a") {
		t.Errorf("array form should stay an array: %q", stdout)
	}
	if n := strings.Count(stdout, "__argmax_prompt"); n != 1 {
		t.Errorf("array should contain __argmax_prompt exactly once (%d): %q", n, stdout)
	}
	if !strings.Contains(stdout, "echo user") {
		t.Errorf("existing entries must be preserved: %q", stdout)
	}
}

func TestBashHookPromptCommandString(t *testing.T) {
	script := writeScript(t, "init.bash", Bash.InitScript())
	code := `ARGMAX_SESSION=$$
PROMPT_COMMAND='echo hi;'
source "$1"
printf 'PC=%s\n' "$PROMPT_COMMAND"`
	stdout, stderr, _, _ := runWithHookPipe(t, "bash", code, script, t.TempDir())

	if stderr != "" {
		t.Errorf("hooks must be silent, stderr = %q", stderr)
	}
	if strings.TrimSpace(stdout) != "PC=echo hi;__argmax_prompt" {
		t.Errorf("string composition = %q, want PC=echo hi;__argmax_prompt", stdout)
	}
}

func TestBashHookNoFdNoNoise(t *testing.T) {
	// Inside a session but without ARGMAX_HOOK_FD: no hooks, no errors.
	script := writeScript(t, "init.bash", Bash.InitScript())
	path, err := exec.LookPath("bash")
	if err != nil {
		t.Skip("bash not on PATH")
	}
	code := `ARGMAX_SESSION=$$
unset ARGMAX_HOOK_FD
source "$1"
declare -F __argmax_prompt >/dev/null && echo "UNEXPECTED: hooks installed"
echo done`
	out, err := exec.Command(path, "-c", code, "bash", script).CombinedOutput()
	if err != nil {
		t.Fatalf("bash failed: %v\n%s", err, out)
	}
	if strings.TrimSpace(string(out)) != "done" {
		t.Fatalf("unexpected output: %q", out)
	}
}

func TestZshHookRuntime(t *testing.T) {
	dir := workDir(t)
	script := writeScript(t, "init.zsh", Zsh.InitScript())

	// Stub the zle builtin (unavailable in non-interactive zsh) so the
	// widget-wrapping path runs; zle-line-init reports "not taken".
	code := `zle() {
    if [ "$1" = "-l" ] && [ "$2" = "zle-line-init" ]; then
        return 1
    fi
    return 0
}
ARGMAX_SESSION=$$
source "$1"
false
__argmax_precmd
cd /
__argmax_precmd
__argmax_preexec 'echo hi'
BUFFER='echo test'
CURSOR=4
eval "$functions[__argmax_wrap_self-insert]"
print -r -- "WRAP=${functions[__argmax_wrap_self-insert]:-MISSING}"
print -r -- "LI=${functions[__argmax_line_init]:-MISSING}"
print -r -- "HOOKS=${precmd_functions[*]} ${preexec_functions[*]}"`
	stdout, stderr, events, rest := runWithHookPipe(t, "zsh", code, script, dir)

	if stderr != "" {
		t.Errorf("hooks must be silent, stderr = %q", stderr)
	}
	if len(rest) != 0 {
		t.Errorf("unconsumed record bytes: %q", rest)
	}
	want := []Event{
		{Type: "prompt", Payload: "1", Cursor: -1},
		{Type: "cwd", Payload: dir, Cursor: -1},
		{Type: "prompt", Payload: "0", Cursor: -1},
		{Type: "cwd", Payload: "/", Cursor: -1},
		{Type: "preexec", Payload: "echo hi", Cursor: -1},
		// Evaluating the generated self-insert wrapper must emit a proper
		// buffer record: cursor, a REAL tab, then the buffer text.
		{Type: "buffer", Payload: "4\techo test", Cursor: 4, Text: "echo test"},
	}
	if !reflect.DeepEqual(events, want) {
		t.Errorf("events = %+v, want %+v", events, want)
	}

	// The generated widget wrapper calls the dot-prefixed original and then
	// reports the buffer.
	if !strings.Contains(stdout, "zle .self-insert") ||
		!strings.Contains(stdout, "__argmax_emit buffer") {
		t.Errorf("self-insert wrapper body wrong:\n%s", stdout)
	}
	if strings.Contains(stdout, "WRAP=MISSING") {
		t.Errorf("self-insert was not wrapped:\n%s", stdout)
	}
	if strings.Contains(stdout, "LI=MISSING") {
		t.Errorf("zle-line-init was not installed:\n%s", stdout)
	}
	if !strings.Contains(stdout, "__argmax_precmd") ||
		!strings.Contains(stdout, "__argmax_preexec") {
		t.Errorf("hooks not registered in precmd/preexec functions:\n%s", stdout)
	}
}

func TestFishHookRuntime(t *testing.T) {
	dir := workDir(t)
	script := writeScript(t, "init.fish", Fish.InitScript())
	code := `set -gx ARGMAX_SESSION $fish_pid
source $argv[1]
__argmax_prompt
__argmax_preexec "echo hi"
false
__argmax_postexec`
	stdout, stderr, events, rest := runWithHookPipe(t, "fish", code, script, dir)

	if stderr != "" {
		t.Errorf("hooks must be silent, stderr = %q", stderr)
	}
	if len(rest) != 0 {
		t.Errorf("unconsumed record bytes: %q", rest)
	}
	want := []Event{
		{Type: "prompt", Payload: "0", Cursor: -1},
		{Type: "cwd", Payload: dir, Cursor: -1},
		{Type: "preexec", Payload: "echo hi", Cursor: -1},
		{Type: "postexec", Payload: "1", Cursor: -1},
	}
	if !reflect.DeepEqual(events, want) {
		t.Errorf("events = %+v, want %+v", events, want)
	}
	if stdout != "" {
		t.Errorf("unexpected stdout: %q", stdout)
	}
}

// TestAutostartBranch drives branch 2 with a fake argmax on PATH and checks
// the SH-002/RUN-010 gate: an interactive, non-nested, non-rescue shell
// execs argmax with the right ARGMAX_SHELL marker; non-interactive, rescue,
// and nested (live ARGMAX_SESSION) shells do not; a stale ARGMAX_SESSION
// (dead PID, e.g. inherited through tmux) is dropped and autostarts again.
func TestAutostartBranch(t *testing.T) {
	bin := t.TempDir()
	fake := filepath.Join(bin, "argmax")
	fakeScript := "#!/bin/sh\nprintf 'EXECED ARGMAX_SHELL=%s\\n' \"$ARGMAX_SHELL\"\n"
	if err := os.WriteFile(fake, []byte(fakeScript), 0o755); err != nil {
		t.Fatal(err)
	}

	cases := []struct {
		name     string
		extraEnv []string
		wantExec bool
	}{
		{"interactive", nil, true},
		{"rescue", []string{"ARGMAX_RESCUE=1"}, false},
		{"stale session marker", []string{"ARGMAX_SESSION=999999"}, true},
		{"nested live session", nil, false}, // ARGMAX_SESSION set to a live PID below
	}

	for _, shell := range []string{"bash", "zsh"} {
		path, err := exec.LookPath(shell)
		if err != nil {
			t.Skipf("%s not on PATH", shell)
		}
		rc := writeScript(t, "init."+shell, Shell(shell).InitScript())
		for _, tc := range cases {
			t.Run(shell+"/"+tc.name, func(t *testing.T) {
				env := []string{
					"PATH=" + bin + string(os.PathListSeparator) + os.Getenv("PATH"),
					"HOME=" + t.TempDir(), // no user rc files
					"ARGMAX_RC=" + rc,
				}
				env = append(env, tc.extraEnv...)
				if tc.name == "nested live session" {
					// The test process is alive, so the marker counts as fresh.
					env = append(env, "ARGMAX_SESSION="+strconv.Itoa(os.Getpid()))
				}
				// A genuinely interactive shell that sources the integration.
				cmd := exec.Command(path, "-i", "-c", `. "$ARGMAX_RC"`, shell)
				cmd.Env = env
				out, err := cmd.CombinedOutput()
				if err != nil {
					t.Fatalf("%s failed: %v\n%s", shell, err, out)
				}
				execed := strings.Contains(string(out), "EXECED ARGMAX_SHELL="+shell)
				if execed != tc.wantExec {
					t.Errorf("exec = %v, want %v; output:\n%s", execed, tc.wantExec, out)
				}
			})
		}
	}

	// Non-interactive shells must never autostart, even with argmax on PATH.
	for _, shell := range []string{"bash", "zsh"} {
		path, err := exec.LookPath(shell)
		if err != nil {
			t.Skipf("%s not on PATH", shell)
		}
		rc := writeScript(t, "init."+shell, Shell(shell).InitScript())
		t.Run(shell+"/non-interactive", func(t *testing.T) {
			cmd := exec.Command(path, "-c", `. "$1"`, shell, rc)
			cmd.Env = []string{
				"PATH=" + bin + string(os.PathListSeparator) + os.Getenv("PATH"),
				"HOME=" + t.TempDir(),
			}
			out, err := cmd.CombinedOutput()
			if err != nil {
				t.Fatalf("%s failed: %v\n%s", shell, err, out)
			}
			if len(out) != 0 {
				t.Errorf("non-interactive shell must stay silent, got %q", out)
			}
		})
	}
}
