package session

import (
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"golang.org/x/sys/unix"

	"github.com/rselbach/argmax/internal/logging"
)

// foregroundPollInterval paces the no-hook prompt detection fallback.
const foregroundPollInterval = 100 * time.Millisecond

// watchForeground implements the command-boundary fallback for shells
// whose integration hooks are not installed: while a command is active
// and no hook event has ever been received, it polls the PTY's foreground
// process group. When the shell itself owns the foreground again for two
// consecutive polls, the prompt is back.
func (s *Session) watchForeground() {
	ticker := time.NewTicker(foregroundPollInterval)
	defer ticker.Stop()
	consecutive := 0
	warned := false
	for {
		select {
		case <-s.done:
			return
		case <-ticker.C:
		}
		s.mu.Lock()
		active := s.commandActive
		hooks := s.hooksSeen
		s.mu.Unlock()
		if !active || hooks {
			consecutive = 0
			continue
		}
		fg, err := ptyForeground(s.ptmx)
		if err != nil || fg != s.child.Process.Pid {
			consecutive = 0
			continue
		}
		if consecutive++; consecutive < 2 {
			continue
		}
		consecutive = 0
		if !warned {
			warned = true
			logging.L().Warn("no shell integration events received; using foreground process-group fallback (run 'argmax setup' for full functionality)")
		}
		if cwd := processCWD(s.child.Process.Pid); cwd != "" {
			s.onCWD(cwd)
		}
		s.mu.Lock()
		s.lastSubmitted = "" // exit status is unknown without hooks
		s.mu.Unlock()
		s.onPromptReady()
	}
}

// ptyForeground returns the foreground process group of the terminal.
func ptyForeground(f *os.File) (int, error) {
	return unix.IoctlGetInt(int(f.Fd()), unix.TIOCGPGRP)
}

// processCWD resolves a process's working directory where the platform
// allows; the empty string means unknown.
func processCWD(pid int) string {
	if link, err := os.Readlink("/proc/" + strconv.Itoa(pid) + "/cwd"); err == nil {
		return link
	}
	// macOS has no /proc; lsof is bounded and runs only on the polled
	// prompt-return path.
	out, err := exec.Command("lsof", "-a", "-p", strconv.Itoa(pid), "-d", "cwd", "-Fn").Output()
	if err != nil {
		return ""
	}
	for _, ln := range strings.Split(string(out), "\n") {
		if rest, ok := strings.CutPrefix(ln, "n"); ok && filepath.IsAbs(rest) {
			return rest
		}
	}
	return ""
}
