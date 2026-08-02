package session

import (
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"syscall"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/logs"
	"github.com/rselbach/argmax/internal/shell"

	"golang.org/x/term"
)

// stderrRing retains at most the last 64 KiB of child stderr (DIAG-002).
type stderrRing struct {
	mu  sync.Mutex
	buf []byte
}

func (r *stderrRing) Write(p []byte) (int, error) {
	r.mu.Lock()
	r.buf = append(r.buf, p...)
	if len(r.buf) > 64<<10 {
		r.buf = r.buf[len(r.buf)-64<<10:]
	}
	r.mu.Unlock()
	return len(p), nil
}

func (r *stderrRing) Bytes() []byte {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]byte(nil), r.buf...)
}

// runWatchdog supervises the interactive session child (DIAG-001): on an
// unexpected failure it restores the terminal, writes a crash report, and
// starts a rescue shell with autostart disabled (DIAG-004).
func runWatchdog(opts Options, version string) int {
	exe, err := os.Executable()
	if err != nil {
		fmt.Fprintf(os.Stderr, "argmax: %v\n", err)
		return 1
	}

	// Save the terminal state before the child puts it in raw mode so the
	// watchdog can restore it after a crash (RUN-003).
	var ttyFd = int(os.Stdin.Fd())
	var oldState *term.State
	if term.IsTerminal(ttyFd) {
		if st, err := term.GetState(ttyFd); err == nil {
			oldState = st
		}
	}

	argv := []string{"__session"}
	if opts.ShellFlag != "" {
		argv = append(argv, "--shell", opts.ShellFlag)
	}
	if opts.Login {
		argv = append(argv, "--shell-login")
	}
	if opts.Debug {
		argv = append(argv, "--debug")
	}

	ring := &stderrRing{}
	child := exec.Command(exe, argv...)
	child.Stdin = os.Stdin
	child.Stdout = os.Stdout
	child.Stderr = io.MultiWriter(os.Stderr, ring)
	child.Env = os.Environ()

	runErr := child.Run()
	code := exitCodeOf(child, runErr)

	// The session removes its marker file only during an orderly shutdown
	// (including the inner shell exiting with any status). A missing marker
	// means a clean exit; a leftover marker means panic, kill, or fatal exit.
	paths := config.ResolvePaths()
	marker := filepath.Join(paths.CacheDir, fmt.Sprintf("session-%d.json", child.Process.Pid))
	_, markerErr := os.Stat(marker)
	clean := os.IsNotExist(markerErr)
	if clean {
		return code
	}
	_ = os.Remove(marker)

	// Unexpected exit: restore the terminal before diagnostics (8.5).
	if oldState != nil {
		_ = term.Restore(ttyFd, oldState)
	}

	// Exit code 3 means the session already wrote a full crash report with a
	// goroutine dump (panic path); otherwise write one from the stderr tail.
	if code != 3 {
		_ = paths.EnsureDirs()
		crashErr := errors.New(failureReason(child, runErr, code))
		path, werr := logs.WriteCrashReport(paths.CrashesDir, version, crashErr, ring.Bytes())
		if werr == nil {
			fmt.Fprintf(os.Stderr, "\r\nargmax: %s\r\ncrash report written to %s\r\n", failureReason(child, runErr, code), path)
		} else {
			fmt.Fprintf(os.Stderr, "\r\nargmax: %s\r\n", failureReason(child, runErr, code))
		}
	}

	return startRescueShell(code)
}

// startRescueShell launches a plain fallback shell with autostart suppressed
// (DIAG-004, RUN-010).
func startRescueShell(code int) int {
	sh, err := shell.Detect("", "")
	if err != nil {
		sh = shell.Bash
	}
	fmt.Fprintf(os.Stderr, "argmax: starting rescue shell (exit to close)\r\n")
	env := []string{}
	for _, kv := range os.Environ() {
		if len(kv) >= 7 && kv[:7] == "ARGMAX_" {
			continue
		}
		env = append(env, kv)
	}
	env = append(env, "ARGMAX_RESCUE=1")
	rescue := exec.Command(sh.Executable(), sh.Args(false)...)
	rescue.Env = env
	rescue.Stdin = os.Stdin
	rescue.Stdout = os.Stdout
	rescue.Stderr = os.Stderr
	_ = rescue.Run()
	return code
}

// exitCodeOf extracts an exit code from a finished child.
func exitCodeOf(cmd *exec.Cmd, runErr error) int {
	if runErr == nil {
		return 0
	}
	if ee, ok := runErr.(*exec.ExitError); ok {
		return ee.ExitCode()
	}
	return 1
}

// failureReason describes how the session child ended for the crash report.
func failureReason(cmd *exec.Cmd, runErr error, code int) string {
	if cmd.ProcessState != nil {
		if ws, ok := cmd.ProcessState.Sys().(syscall.WaitStatus); ok && ws.Signaled() {
			return fmt.Sprintf("session process killed by %s", ws.Signal())
		}
	}
	if runErr != nil {
		return fmt.Sprintf("session process failed: %v", runErr)
	}
	return fmt.Sprintf("session process exited with code %d", code)
}
