//go:build linux || darwin

package pty

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

const testMarker = "greendale-private-session"

func startShell(t *testing.T, script string) *Session {
	t.Helper()
	session, err := Start(Config{
		Executable: "/bin/sh",
		Args:       []string{"-c", script},
		Cwd:        t.TempDir(),
		Size:       Size{Rows: 24, Cols: 80},
		Marker:     testMarker,
	})
	if err != nil {
		t.Fatalf("Start(): %v", err)
	}
	t.Cleanup(func() {
		if err := session.Shutdown(); err != nil {
			t.Errorf("Shutdown() during cleanup: %v", err)
		}
	})
	return session
}

func readUntil(t *testing.T, descriptor *Descriptor, marker []byte) []byte {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	var output []byte
	buffer := make([]byte, 4096)
	for time.Now().Before(deadline) {
		n, err := descriptor.Read(buffer)
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
		t.Fatalf("Read() before marker %q: %v; output %q", marker, err, output)
	}
	t.Fatalf("timed out reading marker %q; output %q", marker, output)
	return nil
}

func waitForExit(t *testing.T, session *Session) ExitStatus {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		status, err := session.TryWait()
		if err != nil {
			t.Fatalf("TryWait(): %v", err)
		}
		if status != nil {
			return *status
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatal("child did not exit")
	return ExitStatus{}
}

func descriptorFlags(t *testing.T, descriptor *Descriptor) (int, int) {
	t.Helper()
	fd := descriptor.FD()
	fdFlags, err := unix.FcntlInt(uintptr(fd), unix.F_GETFD, 0)
	if err != nil {
		t.Fatalf("F_GETFD: %v", err)
	}
	statusFlags, err := unix.FcntlInt(uintptr(fd), unix.F_GETFL, 0)
	if err != nil {
		t.Fatalf("F_GETFL: %v", err)
	}
	return fdFlags, statusFlags
}

func TestStartCreatesControllingTerminalAndExactDescriptorThree(t *testing.T) {
	t.Setenv(markerEnvironment, "parent-marker")
	t.Setenv(eventFDEnvironment, "91")
	t.Setenv(controlEnvironment, "92")

	cwd := t.TempDir()
	size := Size{Rows: 37, Cols: 109, PixelWidth: 1600, PixelHeight: 900}
	script := `
if test -t 0 && test -t 1 && test -t 2; then printf 'tty\n' >&3; fi
printf '%s|%s|%s\n' "$ARGMAX_PRIVATE_SESSION" "$ARGMAX_EVENT_FD" "$ARGMAX_CONTROL_FD" >&3
pwd >&3
if (printf x >&4) 2>/dev/null; then printf 'fd4-open\n' >&3; else printf 'fd4-closed\n' >&3; fi
sleep 10
`
	session, err := Start(Config{
		Executable: "/bin/sh",
		Args:       []string{"-c", script},
		Cwd:        cwd,
		Size:       size,
		Marker:     testMarker,
	})
	if err != nil {
		t.Fatalf("Start(): %v", err)
	}
	t.Cleanup(func() {
		if err := session.Shutdown(); err != nil {
			t.Errorf("Shutdown(): %v", err)
		}
	})

	output := string(readUntil(t, session.Integration(), []byte("fd4-closed\n")))
	for _, want := range []string{
		"tty\n", testMarker + "|3|3\n", cwd + "\n", "fd4-closed\n",
	} {
		if !strings.Contains(output, want) {
			t.Errorf("integration output %q does not contain %q", output, want)
		}
	}
	if strings.Contains(output, "fd4-open") {
		t.Errorf("unrelated descriptor inherited: %q", output)
	}
	gotSize, err := session.Size()
	if err != nil {
		t.Fatalf("Size(): %v", err)
	}
	if gotSize != size {
		t.Errorf("Size() = %+v, want %+v", gotSize, size)
	}
	state := session.ForegroundState()
	if !state.Available || !state.ShellOwnsTerminal || state.ProcessGroup != session.PID() {
		t.Errorf("ForegroundState() = %+v, want shell PID %d", state, session.PID())
	}
	for name, descriptor := range map[string]*Descriptor{
		"master": session.Master(), "integration": session.Integration(),
	} {
		fdFlags, statusFlags := descriptorFlags(t, descriptor)
		if fdFlags&unix.FD_CLOEXEC == 0 {
			t.Errorf("%s lacks FD_CLOEXEC", name)
		}
		if statusFlags&unix.O_NONBLOCK == 0 {
			t.Errorf("%s lacks O_NONBLOCK", name)
		}
	}
	if os.Getenv(markerEnvironment) != "parent-marker" ||
		os.Getenv(eventFDEnvironment) != "91" ||
		os.Getenv(controlEnvironment) != "92" {
		t.Error("Start changed parent integration environment")
	}
}

func TestShellEnvironmentIsExplicitAndOwnerHintRemoved(t *testing.T) {
	t.Setenv("SHELL", "/secret/parent-shell")
	t.Setenv(activeShellEnvironment, "fish")
	t.Setenv(sessionOwnerEnvironment, "999")
	session, err := Start(Config{
		Executable: "/bin/sh",
		Args:       []string{"-c", `printf '%s|%s|%s\n' "$SHELL" "$ARGMAX_ACTIVE_SHELL" "${ARGMAX_SESSION_OWNER_PID-unset}" >&3`},
		Cwd:        t.TempDir(), Size: Size{Rows: 24, Cols: 80}, Marker: testMarker,
		ShellKind: "bash",
	})
	if err != nil {
		t.Fatalf("Start(): %v", err)
	}
	t.Cleanup(func() {
		if err := session.Shutdown(); err != nil {
			t.Errorf("Shutdown(): %v", err)
		}
	})
	output := readUntil(t, session.Integration(), []byte("unset\n"))
	if !bytes.Equal(output, []byte("/bin/sh|bash|unset\n")) {
		t.Errorf("shell environment = %q", output)
	}
}

func TestIntegrationDescriptorThreeIsFullDuplex(t *testing.T) {
	session := startShell(t, `printf 'ready\000' >&3; value=$(dd bs=1 count=12 <&3 2>/dev/null) || exit 91; printf 'reply:%s\000' "$value" >&3`)
	if got := readUntil(t, session.Integration(), []byte("ready\x00")); !bytes.Equal(got, []byte("ready\x00")) {
		t.Fatalf("readiness = %q", got)
	}
	message := []byte("Troy Barnes\n")
	if n, err := session.Integration().Write(message); err != nil || n != len(message) {
		t.Fatalf("integration Write() = (%d, %v)", n, err)
	}
	if got := readUntil(t, session.Integration(), []byte("reply:Troy Barnes\x00")); !bytes.Equal(got, []byte("reply:Troy Barnes\x00")) {
		t.Errorf("reply = %q", got)
	}
	if status := waitForExit(t, session); !status.Success() {
		t.Errorf("status = %+v", status)
	}
}

func TestTransportPreservesBinaryBytesAndExactExit(t *testing.T) {
	session := startShell(t, `stty raw -echo; printf 'ready\000' >&3; dd bs=5 count=1 2>/dev/null; exit 23`)
	if got := readUntil(t, session.Integration(), []byte("ready\x00")); !bytes.Equal(got, []byte("ready\x00")) {
		t.Fatalf("readiness = %q", got)
	}
	want := []byte{0, 1, 2, 128, 255}
	if n, err := session.Master().Write(want); err != nil || n != len(want) {
		t.Fatalf("Write() = (%d, %v), want (%d, nil)", n, err, len(want))
	}
	if got := readUntil(t, session.Master(), want); !bytes.Equal(got, want) {
		t.Errorf("PTY bytes = %v, want %v", got, want)
	}
	status, err := session.Wait()
	if err != nil || status != (ExitStatus{Kind: ExitExited, Code: 23}) {
		t.Fatalf("Wait() = (%+v, %v), want exit 23", status, err)
	}
	second, err := session.Wait()
	if err != nil || second != status {
		t.Errorf("second Wait() = (%+v, %v), want %+v", second, err, status)
	}
}

func TestTryWaitAndExternallyReapedOutcome(t *testing.T) {
	session := startShell(t, "sleep 0.1; exit 7")
	if status, err := session.TryWait(); err != nil || status != nil {
		t.Fatalf("initial TryWait() = (%+v, %v), want pending", status, err)
	}
	var native unix.WaitStatus
	if _, err := unix.Wait4(session.PID(), &native, 0, nil); err != nil {
		t.Fatalf("external Wait4(): %v", err)
	}
	if !native.Exited() || native.ExitStatus() != 7 {
		t.Fatalf("external status = %#v, want exit 7", native)
	}
	status, err := session.TryWait()
	if err != nil || status == nil || status.Kind != ExitExternallyReaped {
		t.Fatalf("TryWait() = (%+v, %v), want externally reaped", status, err)
	}
	if delivery, err := session.ForwardSignal(SignalTerminate); err != nil || delivery != SignalNoForegroundProcessGroup {
		t.Errorf("ForwardSignal() = (%v, %v) after reap", delivery, err)
	}
}

func TestNativeSignalExitIsStructured(t *testing.T) {
	session := startShell(t, "kill -TERM $$")
	status, err := session.Wait()
	if err != nil {
		t.Fatalf("Wait(): %v", err)
	}
	if status.Kind != ExitSignaled || status.Signal != syscall.SIGTERM {
		t.Fatalf("Wait() = %+v, want SIGTERM", status)
	}
	if code, ok := status.WrapperCode(); !ok || code != 128+int(syscall.SIGTERM) {
		t.Errorf("WrapperCode() = (%d, %t)", code, ok)
	}
}

func TestSupportedSignalsMapExactly(t *testing.T) {
	tests := map[Signal]syscall.Signal{
		SignalInterrupt: syscall.SIGINT,
		SignalQuit:      syscall.SIGQUIT,
		SignalTerminate: syscall.SIGTERM,
		SignalHangup:    syscall.SIGHUP,
		SignalSuspend:   syscall.SIGTSTP,
		SignalContinue:  syscall.SIGCONT,
	}
	for signal, want := range tests {
		if got, err := signal.native(); err != nil || got != want {
			t.Errorf("Signal(%d).native() = (%v, %v), want %v", signal, got, err, want)
		}
	}
}

func TestForegroundSignalDeliveryAndWrapperRefusal(t *testing.T) {
	session := startShell(t, `trap 'exit 42' TERM; stty raw -echo; printf R >&3; while :; do read value; done`)
	readUntil(t, session.Integration(), []byte("R"))
	if delivery, err := session.ForwardSignal(SignalTerminate); err != nil || delivery != SignalDelivered {
		t.Fatalf("ForwardSignal() = (%v, %v)", delivery, err)
	}
	status, err := session.Wait()
	if err != nil || status.Kind != ExitExited || status.Code != 42 {
		t.Fatalf("Wait() = (%+v, %v), want exit 42", status, err)
	}
	if delivery, err := forwardProcessGroup(unix.Getpgrp(), syscall.SIGTERM); err != nil || delivery != SignalRefusedWrapperProcessGroup {
		t.Errorf("wrapper-group forwarding = (%v, %v)", delivery, err)
	}
	if _, err := session.ForwardSignal(Signal(255)); !errors.Is(err, ErrUnsupportedSignal) {
		t.Errorf("unsupported ForwardSignal() error = %v", err)
	}
}

func TestCloseInputQueuesCurrentEOFWithNonblockingProgress(t *testing.T) {
	session := startShell(t, `stty raw -echo; printf R >&3; while :; do sleep 1; done`)
	readUntil(t, session.Integration(), []byte("R"))
	input := bytes.Repeat([]byte{'x'}, MaxIOBytes)
	saturated := false
	for range 128 {
		_, err := session.Master().Write(input)
		if errors.Is(err, unix.EAGAIN) {
			saturated = true
			break
		}
		if err != nil {
			t.Fatalf("fill PTY: %v", err)
		}
	}
	if !saturated {
		t.Fatal("PTY input did not saturate")
	}
	started := time.Now()
	progress, err := session.CloseInput()
	if err != nil {
		t.Fatalf("CloseInput(): %v", err)
	}
	if time.Since(started) >= 100*time.Millisecond {
		t.Errorf("CloseInput() blocked for %v", time.Since(started))
	}
	if progress.Total != 2 || progress.Written < 0 || progress.Written > progress.Total {
		t.Errorf("CloseInput() = %+v, want bounded shell-owned EOF progress", progress)
	}
	if _, err := session.Master().Write([]byte("after EOF")); !errors.Is(err, ErrInputClosed) {
		t.Errorf("Write() after CloseInput() = %v, want ErrInputClosed", err)
	}
	second, err := session.CloseInput()
	if err != nil || second.Written < progress.Written || second.Total != progress.Total {
		t.Errorf("second CloseInput() = (%+v, %v), first %+v", second, err, progress)
	}
}

func TestCloseInputCompletesIdempotently(t *testing.T) {
	session := startShell(t, `stty -echo; printf R >&3; line=; IFS= read -r line`)
	readUntil(t, session.Integration(), []byte("R"))
	deadline := time.Now().Add(2 * time.Second)
	var progress CloseInputResult
	for time.Now().Before(deadline) {
		var err error
		progress, err = session.CloseInput()
		if err != nil {
			t.Fatalf("CloseInput(): %v", err)
		}
		if progress.Closed {
			break
		}
		time.Sleep(time.Millisecond)
	}
	if !progress.Closed || progress.Total != 2 {
		t.Fatalf("CloseInput() = %+v, want completed newline and VEOF", progress)
	}
	if again, err := session.CloseInput(); err != nil || again != progress {
		t.Errorf("idempotent CloseInput() = (%+v, %v), want %+v", again, err, progress)
	}
	if _, err := session.Master().Write([]byte("later")); !errors.Is(err, ErrInputClosed) {
		t.Errorf("Write() after EOF = %v, want ErrInputClosed", err)
	}
	if status := waitForExit(t, session); !status.Success() {
		t.Errorf("status after EOF = %+v", status)
	}
}

func TestCloseInputDoesNotPrefixNewlineForForegroundJob(t *testing.T) {
	session := startShell(t, `stty -echo; printf R >&3; set -m; cat`)
	readUntil(t, session.Integration(), []byte("R"))
	deadline := time.Now().Add(2 * time.Second)
	state := session.ForegroundState()
	for state.Available && state.ShellOwnsTerminal && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
		state = session.ForegroundState()
	}
	if !state.Available || state.ShellOwnsTerminal {
		t.Fatalf("foreground job did not acquire PTY: %+v", state)
	}
	progress, err := session.CloseInput()
	if err != nil {
		t.Fatalf("CloseInput(): %v", err)
	}
	if progress.Total != 1 {
		t.Errorf("CloseInput() = %+v, want VEOF without newline", progress)
	}
	if status := waitForExit(t, session); !status.Success() {
		t.Errorf("status after foreground EOF = %+v", status)
	}
}

func TestReadNormalizesOnlyLinuxMasterEIOToEOF(t *testing.T) {
	got := normalizeMasterReadError(unix.EIO)
	if runtime.GOOS == "linux" {
		if !errors.Is(got, io.EOF) {
			t.Fatalf("Linux EIO normalization = %v, want EOF", got)
		}
	} else if !errors.Is(got, unix.EIO) {
		t.Fatalf("Darwin EIO normalization = %v, want EIO", got)
	}

	masterFD, slaveFD, err := openPTY()
	if err != nil {
		t.Fatalf("openPTY(): %v", err)
	}
	descriptor := newDescriptor(masterFD, true)
	t.Cleanup(func() { _ = descriptor.Close() })
	if err := unix.SetNonblock(masterFD, true); err != nil {
		t.Fatalf("SetNonblock(): %v", err)
	}
	if err := unix.Close(slaveFD); err != nil {
		t.Fatalf("close slave: %v", err)
	}
	buffer := make([]byte, 1)
	if n, err := descriptor.Read(buffer); n != 0 || !errors.Is(err, io.EOF) {
		t.Fatalf("Read() after slave close = (%d, %v), want EOF", n, err)
	}
}

func TestShutdownEscalatesAndReapsHUPIgnoringShell(t *testing.T) {
	session := startShell(t, `trap '' HUP; printf R >&3; while :; do :; done`)
	readUntil(t, session.Integration(), []byte("R"))
	started := time.Now()
	if err := session.Shutdown(); err != nil {
		t.Fatalf("Shutdown(): %v", err)
	}
	if elapsed := time.Since(started); elapsed < 90*time.Millisecond || elapsed >= time.Second {
		t.Errorf("Shutdown() escalation duration = %v", elapsed)
	}
	status, err := session.Wait()
	if err != nil || status.Kind != ExitSignaled || status.Signal != syscall.SIGKILL {
		t.Errorf("Wait() after escalation = (%+v, %v), want SIGKILL", status, err)
	}
}

func TestShutdownKillsHUPIgnoringDescendantsAndHeldResources(t *testing.T) {
	session := startShell(t, `trap '' HUP; sh -c 'trap "" HUP; printf R >&3; while :; do :; done' & wait`)
	readUntil(t, session.Integration(), []byte("R"))
	pid := session.PID()
	master := session.Master()
	integration := session.Integration()
	started := time.Now()
	if err := session.Shutdown(); err != nil {
		t.Fatalf("Shutdown(): %v", err)
	}
	if time.Since(started) >= time.Second {
		t.Errorf("Shutdown() took %v", time.Since(started))
	}
	if err := session.Shutdown(); err != nil {
		t.Errorf("second Shutdown(): %v", err)
	}
	if err := unix.Kill(-pid, 0); !errors.Is(err, unix.ESRCH) {
		t.Errorf("child group remains after Shutdown(): %v", err)
	}
	if _, err := master.Read(make([]byte, 1)); !errors.Is(err, io.EOF) {
		t.Errorf("held master Read() = %v, want EOF", err)
	}
	if _, err := integration.Read(make([]byte, 1)); !errors.Is(err, io.EOF) {
		t.Errorf("held integration Read() = %v, want EOF", err)
	}
	if master.FD() != -1 || integration.FD() != -1 {
		t.Error("held descriptors remain open after Shutdown()")
	}
}

func TestDetachedDaemonRemainsOutsideContainment(t *testing.T) {
	if _, err := os.Stat("/usr/bin/perl"); err != nil {
		t.Skip("Perl is unavailable")
	}
	script := `/usr/bin/perl -MPOSIX -e 'POSIX::setsid() >= 0 or exit 2; $SIG{HUP}="IGNORE"; open(my $event, ">&=3") or exit 3; select($event); $|=1; print "$$\0"; sleep 10' & sleep 0.2; exit 0`
	session := startShell(t, script)
	frame := readUntil(t, session.Integration(), []byte{0})
	pidText := strings.TrimSuffix(string(frame), "\x00")
	var daemonPID int
	if _, err := fmt.Sscanf(pidText, "%d", &daemonPID); err != nil {
		t.Fatalf("parse daemon PID %q: %v", pidText, err)
	}
	status, err := session.Wait()
	if err != nil || !status.Success() {
		t.Fatalf("Wait() = (%+v, %v)", status, err)
	}
	if err := session.Shutdown(); err != nil {
		t.Fatalf("Shutdown(): %v", err)
	}
	if err := unix.Kill(daemonPID, 0); err != nil {
		t.Fatalf("detached daemon was contained: %v", err)
	}
	if err := unix.Kill(daemonPID, syscall.SIGTERM); err != nil {
		t.Fatalf("kill detached daemon: %v", err)
	}
	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		if errors.Is(unix.Kill(daemonPID, 0), unix.ESRCH) {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	_ = unix.Kill(daemonPID, syscall.SIGKILL)
}

func TestInvalidConfigFailsBeforeDescriptorAllocation(t *testing.T) {
	directory := t.TempDir()
	nonExecutable := filepath.Join(directory, "not-executable")
	if err := os.WriteFile(nonExecutable, []byte("#!/bin/sh\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	valid := Config{
		Executable: "/bin/sh", Args: []string{"-c", "exit 0"}, Cwd: directory,
		Size: Size{Rows: 24, Cols: 80}, Marker: testMarker,
	}
	tooManyArgs := valid
	tooManyArgs.Args = make([]string, MaxArgs+1)
	tooLargeArgs := valid
	tooLargeArgs.Args = []string{strings.Repeat("x", MaxArgumentBytes+1)}
	tooLongMarker := valid
	tooLongMarker.Marker = strings.Repeat("x", maxMarkerBytes+1)
	tests := map[string]Config{
		"zero rows":            withSize(valid, Size{Cols: 80}),
		"zero columns":         withSize(valid, Size{Rows: 24}),
		"missing executable":   withExecutable(valid, "/definitely/missing/argmax-shell"),
		"directory executable": withExecutable(valid, directory),
		"non-executable file":  withExecutable(valid, nonExecutable),
		"missing directory":    withDirectory(valid, filepath.Join(directory, "missing")),
		"file directory":       withDirectory(valid, nonExecutable),
		"too many args":        tooManyArgs,
		"too many arg bytes":   tooLargeArgs,
		"oversized marker":     tooLongMarker,
		"invalid marker":       withMarker(valid, "Dean Pelton"),
		"invalid shell kind":   withShellKind(valid, "tcsh"),
	}
	before := countOpenDescriptors(t)
	for name, config := range tests {
		t.Run(name, func(t *testing.T) {
			if _, err := Start(config); err == nil {
				t.Fatal("Start() succeeded")
			}
		})
	}
	if after := countOpenDescriptors(t); after != before {
		t.Errorf("open descriptors after validation = %d, want %d", after, before)
	}
}

func TestBoundedIOResizeAndFormatting(t *testing.T) {
	secret := "private-token"
	config := Config{
		Executable: "/secret/executable", Args: []string{"--password", secret},
		Cwd: "/secret/directory", Size: Size{Rows: 24, Cols: 80}, Marker: secret,
	}
	for _, formatted := range []string{fmt.Sprintf("%v", config), fmt.Sprintf("%+v", config), fmt.Sprintf("%#v", config)} {
		if strings.Contains(formatted, secret) || strings.Contains(formatted, "/secret") {
			t.Errorf("Config formatting exposed input: %s", formatted)
		}
	}
	session := startShell(t, "sleep 10")
	oversized := make([]byte, MaxIOBytes+1)
	if _, err := session.Master().Read(oversized); !errors.Is(err, ErrIOTooLarge) {
		t.Errorf("oversized Read() = %v", err)
	}
	if _, err := session.Master().Write(oversized); !errors.Is(err, ErrIOTooLarge) {
		t.Errorf("oversized Write() = %v", err)
	}
	want := Size{Rows: 61, Cols: 143, PixelWidth: 2560, PixelHeight: 1440}
	if err := session.Resize(want); err != nil {
		t.Fatalf("Resize(): %v", err)
	}
	if got, err := session.Size(); err != nil || got != want {
		t.Errorf("Size() = (%+v, %v), want %+v", got, err, want)
	}
	if err := session.Resize(Size{Rows: 0, Cols: 1}); err == nil {
		t.Error("zero-row Resize() succeeded")
	}
}

func countOpenDescriptors(t *testing.T) int {
	t.Helper()
	count := 0
	for fd := range 4096 {
		if _, err := unix.FcntlInt(uintptr(fd), unix.F_GETFD, 0); err == nil {
			count++
		} else if !errors.Is(err, unix.EBADF) {
			t.Fatalf("F_GETFD(%d): %v", fd, err)
		}
	}
	return count
}

func withSize(config Config, size Size) Config {
	config.Size = size
	return config
}

func withExecutable(config Config, executable string) Config {
	config.Executable = executable
	return config
}

func withDirectory(config Config, directory string) Config {
	config.Cwd = directory
	return config
}

func withMarker(config Config, marker string) Config {
	config.Marker = marker
	return config
}

func withShellKind(config Config, kind string) Config {
	config.ShellKind = kind
	return config
}
