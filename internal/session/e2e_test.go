package session

// End-to-end test: run a real session inside a PTY with a bash child and
// drive keystrokes from the master side. The test acts as the terminal:
// it answers DSR cursor queries and scans output for menu content.

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/shell"

	"github.com/creack/pty"
)

// TestMain runs the session when the helper env var is set.
func TestMain(m *testing.M) {
	if os.Getenv("ARGMAX_E2E_HELPER") == "1" {
		os.Exit(runSession(Options{ShellFlag: "bash"}, "e2e"))
	}
	os.Exit(m.Run())
}

func TestSessionEndToEnd(t *testing.T) {
	if _, err := exec.LookPath("bash"); err != nil {
		t.Skip("bash not available")
	}

	home := t.TempDir()
	xdg := func(parts ...string) string {
		return filepath.Join(append([]string{home}, parts...)...)
	}
	// The hook block must be installed for the child shell to emit events.
	rc := filepath.Join(home, ".bashrc")
	if err := os.WriteFile(rc, []byte(shell.Bash.InitScript()), 0o600); err != nil {
		t.Fatal(err)
	}

	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(exe, "-test.run", "^TestMain$")
	cmd.Env = []string{
		"ARGMAX_E2E_HELPER=1",
		"HOME=" + home,
		"XDG_CONFIG_HOME=" + xdg(".config"),
		"XDG_DATA_HOME=" + xdg(".local", "share"),
		"XDG_CACHE_HOME=" + xdg(".cache"),
		"TERM=xterm-256color",
		"PATH=" + os.Getenv("PATH"),
	}

	ptmx, err := pty.StartWithSize(cmd, &pty.Winsize{Rows: 24, Cols: 80})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = ptmx.Close() }()

	out := &lockedBuffer{}
	done := make(chan struct{})
	go func() {
		defer close(done)
		buf := make([]byte, 8192)
		for {
			n, err := ptmx.Read(buf)
			if n > 0 {
				_, _ = out.Write(buf[:n])
				// Answer DSR cursor queries like a real terminal.
				if bytes.Contains(buf[:n], []byte("\x1b[6n")) {
					_, _ = ptmx.Write([]byte("\x1b[10;5R"))
				}
			}
			if err != nil {
				return
			}
		}
	}()

	// Wait for the wrapped bash prompt to appear.
	if !out.waitFor(t, "bash", 8*time.Second) && !out.waitFor(t, "$", 3*time.Second) {
		t.Fatalf("prompt never appeared; output so far:\n%s", out.String())
	}

	// Type a partial command; the menu should render "git checkout"
	// (the ANSI highlight splits the word, so match the suffix).
	_, _ = ptmx.Write([]byte("git che"))
	if !out.waitFor(t, "ckout", 8*time.Second) {
		t.Fatalf("menu did not show git checkout; output:\n%s", out.String())
	}

	// Tab accepts the highlighted suggestion (IN-008): line becomes
	// "git checkout " and the shell echoes it.
	_, _ = ptmx.Write([]byte("\t"))
	time.Sleep(500 * time.Millisecond)

	// Toggle the menu off for the rest of the session (IN-011), then run a
	// command to prove the shell still works end to end.
	_, _ = ptmx.Write([]byte("\x1b[Z"))
	_, _ = ptmx.Write([]byte{0x15}) // Ctrl+U clears the line
	_, _ = ptmx.Write([]byte("echo argmax-e2e-ok\r"))
	if !out.waitFor(t, "argmax-e2e-ok", 8*time.Second) {
		t.Fatalf("command output missing; output:\n%s", out.String())
	}

	// Exit the shell; the session process must exit too.
	_, _ = ptmx.Write([]byte("exit\r"))
	waitErr := make(chan error, 1)
	go func() { waitErr <- cmd.Wait() }()
	select {
	case err := <-waitErr:
		if err != nil {
			t.Fatalf("session exit: %v", err)
		}
	case <-time.After(8 * time.Second):
		_ = cmd.Process.Kill()
		t.Fatal("session did not exit after shell exit")
	}
	<-done
}

type lockedBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *lockedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

// waitFor polls until needle appears in the output or the timeout expires.
func (b *lockedBuffer) waitFor(t *testing.T, needle string, timeout time.Duration) bool {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if b.contains(needle) {
			return true
		}
		time.Sleep(50 * time.Millisecond)
	}
	return b.contains(needle)
}

func (b *lockedBuffer) contains(needle string) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return bytes.Contains(b.buf.Bytes(), []byte(needle))
}
