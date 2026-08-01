//go:build linux || darwin

package transparuntime

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/pty"
	"github.com/rselbach/argmax/internal/shellselect"
	"golang.org/x/sys/unix"
)

const integrationDeadline = 8 * time.Second

type runResult struct {
	status pty.ExitStatus
	err    error
}

type outerSession struct {
	master  *os.File
	slave   *os.File
	result  <-chan runResult
	pid     int
	stopped bool
}

func isolateShellConfiguration(t *testing.T, directory string) {
	t.Helper()
	for _, name := range []string{".bashrc", ".zshrc"} {
		if err := os.WriteFile(filepath.Join(directory, name), nil, 0o600); err != nil {
			t.Fatalf("create isolated shell startup: %v", err)
		}
	}
	t.Setenv("HOME", directory)
	t.Setenv("ZDOTDIR", directory)
	t.Setenv("XDG_CONFIG_HOME", directory)
	t.Setenv("BASH_ENV", filepath.Join(directory, "missing-bash-env"))
	t.Setenv("ENV", filepath.Join(directory, "missing-env"))
}

func installedShell(t *testing.T, kind shellselect.Kind) (shellselect.Selected, bool) {
	t.Helper()
	executable, err := exec.LookPath(kind.String())
	if err != nil {
		return shellselect.Selected{}, false
	}
	executable, err = filepath.Abs(executable)
	if err != nil {
		t.Fatalf("Abs(shell): %v", err)
	}
	selected, err := shellselect.Select(shellselect.Request{
		CommandLine:   &kind,
		SearchPath:    filepath.Dir(executable),
		SearchPathSet: true,
	})
	if err != nil {
		t.Fatalf("Select(%s): %v", kind, err)
	}
	return selected, true
}

func startOuterSession(t *testing.T, shell shellselect.Selected, cwd string) *outerSession {
	t.Helper()
	master, slave, err := openOuterPTY()
	if err != nil {
		t.Fatalf("openOuterPTY(): %v", err)
	}
	if err := unix.IoctlSetWinsize(int(master.Fd()), unix.TIOCSWINSZ, &unix.Winsize{
		Row: 31, Col: 97, Xpixel: 970, Ypixel: 620,
	}); err != nil {
		t.Fatalf("set outer size: %v", err)
	}
	if _, err := outerTermios(slave); err != nil {
		t.Fatalf("outerTermios(): %v", err)
	}
	if err := unix.SetNonblock(int(master.Fd()), true); err != nil {
		t.Fatalf("SetNonblock(master): %v", err)
	}
	marker, err := GenerateMarker()
	if err != nil {
		t.Fatalf("GenerateMarker(): %v", err)
	}
	result := make(chan runResult, 1)
	started := make(chan int, 1)
	go func() {
		status, runErr := run(Config{
			Shell: shell, Cwd: cwd, Marker: marker, Input: slave, Output: slave,
		}, func(session *pty.Session) { started <- session.PID() })
		result <- runResult{status: status, err: runErr}
	}()
	session := &outerSession{master: master, slave: slave, result: result}
	select {
	case session.pid = <-started:
	case <-time.After(2 * time.Second):
		t.Fatal("transparent runtime did not start shell")
	}
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		current, termiosErr := outerTermios(slave)
		if termiosErr != nil {
			t.Fatalf("inspect outer raw mode: %v", termiosErr)
		}
		if current.Lflag&(unix.ECHO|unix.ICANON) == 0 {
			break
		}
		time.Sleep(time.Millisecond)
	}
	current, err := outerTermios(slave)
	if err != nil || current.Lflag&(unix.ECHO|unix.ICANON) != 0 {
		t.Fatalf("transparent runtime did not enter raw mode: %v", err)
	}
	t.Cleanup(func() {
		_ = master.Close()
		if !session.stopped {
			select {
			case <-result:
			case <-time.After(2 * time.Second):
				_ = unix.Kill(unix.Getpid(), syscall.SIGHUP)
				select {
				case <-result:
				case <-time.After(2 * time.Second):
					t.Errorf("transparent runtime did not stop during cleanup")
				}
			}
		}
		_ = slave.Close()
	})
	return session
}

func (session *outerSession) write(t *testing.T, data []byte) {
	t.Helper()
	deadline := time.Now().Add(integrationDeadline)
	for len(data) != 0 && time.Now().Before(deadline) {
		n, err := unix.Write(int(session.master.Fd()), data)
		if n > 0 {
			data = data[n:]
		}
		if err == nil || errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
			time.Sleep(time.Millisecond)
			continue
		}
		t.Fatalf("write outer PTY: %v", err)
	}
	if len(data) != 0 {
		t.Fatal("timed out writing outer PTY")
	}
}

func (session *outerSession) readUntil(t *testing.T, marker []byte) []byte {
	t.Helper()
	deadline := time.Now().Add(integrationDeadline)
	output := make([]byte, 0, 4096)
	buffer := make([]byte, 4096)
	for time.Now().Before(deadline) {
		n, err := unix.Read(int(session.master.Fd()), buffer)
		if n > 0 {
			output = append(output, buffer[:n]...)
			if bytes.Contains(output, marker) {
				return output
			}
		}
		if err == nil || errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
			time.Sleep(time.Millisecond)
			continue
		}
		t.Fatalf("read outer PTY waiting for %q: %v; output %q", marker, err, output)
	}
	t.Fatalf("timed out reading %q; output %q", marker, output)
	return nil
}

func (session *outerSession) wait(t *testing.T) runResult {
	t.Helper()
	select {
	case result := <-session.result:
		session.stopped = true
		return result
	case <-time.After(integrationDeadline):
		t.Fatal("transparent runtime did not exit")
		return runResult{}
	}
}

func TestRealOuterPTYTransparentBytesShellsCwdAndNonzeroExit(t *testing.T) {
	for _, kind := range []shellselect.Kind{shellselect.Bash, shellselect.Zsh, shellselect.Fish} {
		t.Run(kind.String(), func(t *testing.T) {
			shell, ok := installedShell(t, kind)
			if !ok {
				t.Skipf("%s is not installed", kind)
			}
			cwd := t.TempDir()
			isolateShellConfiguration(t, cwd)
			session := startOuterSession(t, shell, cwd)
			session.write(t, []byte("printf '%s%s<%s>\\n' ARGMAX_ CWD: \"$PWD\"; stty raw -echo; printf '%s%s' ARGMAX_ BYTES:; dd bs=5 count=1 2>/dev/null; exit 23\n"))
			output := session.readUntil(t, []byte("ARGMAX_BYTES:"))
			wantCwd, err := filepath.EvalSymlinks(cwd)
			if err != nil {
				t.Fatalf("EvalSymlinks(cwd): %v", err)
			}
			if !bytes.Contains(output, []byte("ARGMAX_CWD:<"+wantCwd+">")) {
				t.Errorf("cwd output missing from %q", output)
			}
			want := []byte{0, 1, 2, 128, 255}
			session.write(t, want)
			output = session.readUntil(t, want)
			if !bytes.Contains(output, want) {
				t.Errorf("binary output = %v, want sequence %v", output, want)
			}
			result := session.wait(t)
			if result.err != nil || result.status.Kind != pty.ExitExited || result.status.Code != 23 {
				t.Errorf("Run() = (%+v, %v), want exit 23", result.status, result.err)
			}
		})
	}
}

func TestRealOuterPTYResizeAndForegroundCtrlC(t *testing.T) {
	shell, ok := installedShell(t, shellselect.Bash)
	if !ok {
		t.Skip("bash is not installed")
	}
	cwd := t.TempDir()
	isolateShellConfiguration(t, cwd)
	session := startOuterSession(t, shell, cwd)
	session.write(t, []byte("printf '%s%s' INITIAL :; stty size; sleep 30\n"))
	output := session.readUntil(t, []byte("INITIAL:31 97"))
	if !bytes.Contains(output, []byte("INITIAL:31 97")) {
		t.Fatalf("initial size output = %q", output)
	}
	if err := unix.IoctlSetWinsize(int(session.master.Fd()), unix.TIOCSWINSZ, &unix.Winsize{
		Row: 45, Col: 123, Xpixel: 1230, Ypixel: 900,
	}); err != nil {
		t.Fatalf("resize outer PTY: %v", err)
	}
	if err := unix.Kill(unix.Getpid(), syscall.SIGWINCH); err != nil {
		t.Fatalf("send SIGWINCH: %v", err)
	}
	time.Sleep(50 * time.Millisecond)
	session.write(t, []byte{3})
	session.write(t, []byte("printf '%s%s' RESIZED :; stty size; printf '%s%s\\n' AFTER_ INT; exit 0\n"))
	output = session.readUntil(t, []byte("AFTER_INT"))
	if !bytes.Contains(output, []byte("RESIZED:45 123")) {
		t.Errorf("resized output = %q", output)
	}
	result := session.wait(t)
	if result.err != nil || !result.status.Success() {
		t.Errorf("Run() = (%+v, %v)", result.status, result.err)
	}
}

func TestRealOuterPTYNativeSignalExitAndIntegrationDrain(t *testing.T) {
	shell, ok := installedShell(t, shellselect.Bash)
	if !ok {
		t.Skip("bash is not installed")
	}
	t.Run("integration drain", func(t *testing.T) {
		cwd := t.TempDir()
		isolateShellConfiguration(t, cwd)
		session := startOuterSession(t, shell, cwd)
		session.write(t, []byte("dd if=/dev/zero bs=65536 count=32 >&3 2>/dev/null; printf '%s%s\\n' DRAIN ED; exit 0\n"))
		session.readUntil(t, []byte("DRAINED"))
		result := session.wait(t)
		if result.err != nil || !result.status.Success() {
			t.Errorf("Run() = (%+v, %v)", result.status, result.err)
		}
	})
	t.Run("signal status", func(t *testing.T) {
		cwd := t.TempDir()
		isolateShellConfiguration(t, cwd)
		session := startOuterSession(t, shell, cwd)
		session.write(t, []byte("exec /bin/sh -c 'kill -TERM $$'\n"))
		result := session.wait(t)
		if result.err != nil || result.status.Kind != pty.ExitSignaled ||
			result.status.Signal != syscall.SIGTERM {
			t.Errorf("Run() = (%+v, %v), want SIGTERM", result.status, result.err)
		}
	})
}

func TestWrapperTerminationForwardsAndLeavesNoChild(t *testing.T) {
	shell, ok := installedShell(t, shellselect.Bash)
	if !ok {
		t.Skip("bash is not installed")
	}
	cwd := t.TempDir()
	isolateShellConfiguration(t, cwd)
	session := startOuterSession(t, shell, cwd)
	session.write(t, []byte("trap 'exit 42' TERM; printf '%s%s\\n' TERM_ READY\n"))
	session.readUntil(t, []byte("TERM_READY"))
	if err := unix.Kill(unix.Getpid(), syscall.SIGTERM); err != nil {
		t.Fatalf("signal wrapper: %v", err)
	}
	result := session.wait(t)
	if result.err != nil || result.status.Kind != pty.ExitExited {
		t.Fatalf("Run() = (%+v, %v), want shell exit", result.status, result.err)
	}
	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		if errors.Is(unix.Kill(-session.pid, 0), unix.ESRCH) {
			return
		}
		time.Sleep(time.Millisecond)
	}
	t.Errorf("child process group %d remains after wrapper termination", session.pid)
}

func TestTerminalRestoredAfterNormalErrorAndPanic(t *testing.T) {
	shell, ok := installedShell(t, shellselect.Bash)
	if !ok {
		t.Skip("bash is not installed")
	}
	for _, tc := range []struct {
		name       string
		cwd        string
		panicProbe bool
		wantPanic  bool
	}{
		{name: "normal error", cwd: filepath.Join(t.TempDir(), "missing")},
		{name: "panic", cwd: t.TempDir(), panicProbe: true, wantPanic: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			master, slave, err := openOuterPTY()
			if err != nil {
				t.Fatal(err)
			}
			defer func() {
				if closeErr := master.Close(); closeErr != nil {
					t.Errorf("close outer master: %v", closeErr)
				}
			}()
			defer func() {
				if closeErr := slave.Close(); closeErr != nil {
					t.Errorf("close outer slave: %v", closeErr)
				}
			}()
			if err := unix.IoctlSetWinsize(int(master.Fd()), unix.TIOCSWINSZ, &unix.Winsize{Row: 24, Col: 80}); err != nil {
				t.Fatal(err)
			}
			original, err := outerTermios(slave)
			if err != nil {
				t.Fatal(err)
			}
			var afterStart func(*pty.Session)
			if tc.panicProbe {
				afterStart = func(*pty.Session) { panic("runtime panic restoration probe") }
			}
			status, runErr := run(Config{
				Shell: shell, Cwd: tc.cwd, Marker: "greendale-restoration-probe",
				Input: slave, Output: slave,
			}, afterStart)
			if runErr == nil || tc.wantPanic && !errors.Is(runErr, ErrRuntimePanic) {
				t.Errorf("Run() = (%+v, %v)", status, runErr)
			}
			after, err := outerTermios(slave)
			if err != nil {
				t.Fatal(err)
			}
			const localModes = unix.ECHO | unix.ICANON | unix.ISIG | unix.IEXTEN
			if after.Lflag&localModes != original.Lflag&localModes {
				t.Errorf("terminal local modes not restored: got %#x, want %#x", after.Lflag, original.Lflag)
			}
		})
	}
}

func TestGenerateMarkerIsBoundedAndShellSafe(t *testing.T) {
	first, err := GenerateMarker()
	if err != nil {
		t.Fatal(err)
	}
	second, err := GenerateMarker()
	if err != nil {
		t.Fatal(err)
	}
	if first == second || len(first) != 43 {
		t.Fatalf("markers are not distinct fixed-size values: %q %q", first, second)
	}
	for _, marker := range []string{first, second} {
		for _, character := range marker {
			if !bytes.ContainsRune([]byte("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_"), character) {
				t.Fatalf("marker contains unsafe character: %q", marker)
			}
		}
	}
	if formatted := fmt.Sprintf("%v %#v", Config{Marker: first}, Config{Marker: first}); bytes.Contains([]byte(formatted), []byte(first)) {
		t.Errorf("Config formatting exposed marker: %s", formatted)
	}
}
