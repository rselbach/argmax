// Package watchdog runs the interactive session in a monitored child
// process. On an unexpected crash it restores the terminal, writes a
// private crash report, and starts a rescue shell so the terminal remains
// usable.
package watchdog

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"sync/atomic"
	"syscall"
	"time"

	"golang.org/x/term"

	"github.com/rselbach/argmax/internal/paths"
)

const maxStderr = 64 << 10 // retained child stderr for crash diagnosis

const forceKillDelay = 3 * time.Second

// SessionArg is the internal argument marking the monitored session child.
const SessionArg = "__session"

// Run spawns the session child and supervises it, returning the exit code
// to propagate.
func Run(version string) int {
	self, err := os.Executable()
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax: cannot resolve executable:", err)
		return 1
	}
	args := append([]string{SessionArg}, os.Args[1:]...)
	cmd := exec.Command(self, args...)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	// Full goroutine dumps make crash reports actionable.
	cmd.Env = append(os.Environ(), "GOTRACEBACK=all")
	ring := &ringBuffer{limit: maxStderr}
	cmd.Stderr = ring

	// Save terminal state before the child manipulates it.
	var saved *term.State
	ttyFD := int(os.Stdin.Fd())
	if term.IsTerminal(ttyFD) {
		saved, _ = term.GetState(ttyFD)
	}
	restore := func() {
		if saved != nil {
			_ = term.Restore(ttyFD, saved)
		}
	}

	// Buffer signals before starting the child, but do not start forwarding
	// until cmd.Process is guaranteed to exist.
	signals := make(chan os.Signal, 4)
	signal.Notify(signals, syscall.SIGTERM, syscall.SIGINT, syscall.SIGHUP)
	defer signal.Stop(signals)
	if err := cmd.Start(); err != nil {
		restore()
		fmt.Fprintln(os.Stderr, "argmax: cannot start monitored session:", err)
		return 1
	}

	// Forward every termination signal to the child so the terminal is
	// restored before this process exits; signal death is then a normal
	// shutdown, not a crash.
	var forwarded atomic.Bool
	var childExited atomic.Bool
	childDone := make(chan struct{})
	forwardDone := make(chan struct{})
	go func() {
		forwardSignals(cmd.Process, signals, childDone, &forwarded, &childExited, forceKillDelay)
		close(forwardDone)
	}()

	err = cmd.Wait()
	childExited.Store(true)
	close(childDone)
	<-forwardDone
	if err == nil {
		restore()
		return 0
	}
	exitCode := 1
	if cmd.ProcessState != nil {
		exitCode = cmd.ProcessState.ExitCode()
	}
	if forwarded.Load() || !crashed(cmd, ring) {
		restore()
		return exitCode
	}

	restore()
	reportPath, reportErr := writeReport(version, ring.Bytes())
	fmt.Fprintln(os.Stderr, "\r\nargmax: the session ended unexpectedly.")
	if reportErr == nil {
		fmt.Fprintf(os.Stderr, "argmax: crash report written to %s\r\n", reportPath)
	} else {
		fmt.Fprintf(os.Stderr, "argmax: failed to write crash report: %v\r\n", reportErr)
	}
	fmt.Fprintln(os.Stderr, "argmax: starting a rescue shell (argmax disabled).")
	runRescueShell()
	return exitCode
}

func forwardSignals(
	process *os.Process,
	signals <-chan os.Signal,
	childDone <-chan struct{},
	forwarded, childExited *atomic.Bool,
	forceAfter time.Duration,
) {
	var (
		forceTimer *time.Timer
		force      <-chan time.Time
	)
	defer func() {
		if forceTimer != nil {
			forceTimer.Stop()
		}
	}()
	for {
		select {
		case sig, ok := <-signals:
			if !ok {
				return
			}
			forwarded.Store(true)
			if !childExited.Load() {
				_ = process.Signal(sig)
			}
			if forceTimer == nil {
				forceTimer = time.NewTimer(forceAfter)
				force = forceTimer.C
			}
		case <-force:
			if !childExited.Load() {
				_ = process.Kill()
			}
			forceTimer = nil
			force = nil
		case <-childDone:
			return
		}
	}
}

// crashed distinguishes an internal panic or fatal failure from a normal
// non-zero shell exit, which the session reports as its own exit code.
func crashed(cmd *exec.Cmd, ring *ringBuffer) bool {
	if cmd.ProcessState == nil {
		return true
	}
	if !cmd.ProcessState.Exited() {
		return true // killed by a signal
	}
	// A Go panic always leaves a trace on stderr.
	return len(ring.Bytes()) > 0 && containsPanic(ring.Bytes())
}

func containsPanic(b []byte) bool {
	for _, marker := range []string{"panic:", "fatal error:", "runtime error:"} {
		if bytes.Contains(b, []byte(marker)) {
			return true
		}
	}
	return false
}

// writeReport stores a private timestamped crash report.
func writeReport(version string, stderr []byte) (string, error) {
	dir := paths.CrashDir()
	if err := paths.EnsureDir(dir); err != nil {
		return "", err
	}
	name := filepath.Join(dir, "crash-"+time.Now().Format("20060102-150405")+".txt")
	var content []byte
	content = fmt.Appendf(content,
		"argmax crash report\ntime: %s\nversion: %s\nos/arch: %s/%s\n\n--- captured stderr (up to 64 KiB) ---\n",
		time.Now().Format(time.RFC3339), version, runtime.GOOS, runtime.GOARCH)
	content = append(content, stderr...)
	if err := os.WriteFile(name, content, 0o600); err != nil {
		return "", err
	}
	abs, err := filepath.Abs(name)
	if err != nil {
		return name, nil
	}
	return abs, nil
}

// runRescueShell starts a plain shell with autostart suppressed.
func runRescueShell() {
	shellPath := os.Getenv("SHELL")
	if shellPath == "" {
		shellPath = "/bin/sh"
	}
	cmd := exec.Command(shellPath)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Env = append(os.Environ(), "ARGMAX_RESCUE=1")
	_ = cmd.Run()
}

// ringBuffer keeps the last limit bytes written.
type ringBuffer struct {
	limit int
	data  []byte
}

func (r *ringBuffer) Write(p []byte) (int, error) {
	r.data = append(r.data, p...)
	if len(r.data) > r.limit {
		r.data = r.data[len(r.data)-r.limit:]
	}
	return len(p), nil
}

// Bytes returns the retained tail of the stream.
func (r *ringBuffer) Bytes() []byte { return r.data }
