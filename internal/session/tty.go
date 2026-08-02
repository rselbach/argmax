package session

import (
	"fmt"
	"os"

	"github.com/creack/pty"
	"golang.org/x/sys/unix"
	"golang.org/x/term"
)

// tty wraps the controlling terminal used for input and geometry (RUN-011).
type tty struct {
	file *os.File
	fd   int
	old  *term.State
}

// openTTY returns stdin when it is a terminal, otherwise /dev/tty.
func openTTY() (*tty, error) {
	if term.IsTerminal(int(os.Stdin.Fd())) {
		return &tty{file: os.Stdin, fd: int(os.Stdin.Fd())}, nil
	}
	f, err := os.OpenFile("/dev/tty", os.O_RDWR, 0)
	if err != nil {
		return nil, fmt.Errorf("no controlling terminal: %w", err)
	}
	return &tty{file: f, fd: int(f.Fd())}, nil
}

// makeRaw puts the terminal in raw mode, saving the previous state (RUN-002).
func (t *tty) makeRaw() error {
	st, err := term.MakeRaw(t.fd)
	if err != nil {
		return err
	}
	t.old = st
	return nil
}

// restore returns the terminal to its saved state (RUN-003).
func (t *tty) restore() {
	if t.old != nil {
		_ = term.Restore(t.fd, t.old)
	}
}

// size reports the terminal dimensions (cols, rows).
func (t *tty) size() (int, int) {
	w, h, err := term.GetSize(t.fd)
	if err != nil || w <= 0 || h <= 0 {
		return 80, 24
	}
	return w, h
}

// winsize returns the size as a pty.Winsize for the child.
func (t *tty) winsize() *pty.Winsize {
	w, h := t.size()
	return &pty.Winsize{Rows: uint16(h), Cols: uint16(w)}
}

// foregroundPgrp returns the foreground process group of the given pty
// master, for Bash command-boundary fallback (RUN-006).
func foregroundPgrp(ptmxFd int) (int, error) {
	return unix.IoctlGetInt(ptmxFd, unix.TIOCGPGRP)
}

// pidAlive reports whether pid exists (used for stale session markers).
func pidAlive(pid int) bool {
	return unix.Kill(pid, 0) == nil
}
