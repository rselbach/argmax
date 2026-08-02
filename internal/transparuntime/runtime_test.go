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

	"github.com/rselbach/argmax/internal/completion"
	"github.com/rselbach/argmax/internal/pty"
	"github.com/rselbach/argmax/internal/shellcontrol"
	"github.com/rselbach/argmax/internal/shellevents"
	"github.com/rselbach/argmax/internal/shellintegration"
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

func isolateShellConfiguration(t *testing.T, directory string, kind shellselect.Kind) {
	t.Helper()
	script, err := shellintegration.Script(kind)
	if err != nil {
		t.Fatalf("shellintegration.Script(%s): %v", kind, err)
	}
	scriptName := ".argmax-integration"
	if err := os.WriteFile(filepath.Join(directory, scriptName), []byte(script), 0o600); err != nil {
		t.Fatalf("write isolated shell integration: %v", err)
	}
	for _, startup := range []struct {
		path string
	}{
		{path: filepath.Join(directory, ".bashrc")},
		{path: filepath.Join(directory, ".zshrc")},
		{
			path: filepath.Join(directory, "fish", "config.fish"),
		},
	} {
		if err := os.MkdirAll(filepath.Dir(startup.path), 0o700); err != nil {
			t.Fatalf("create isolated shell startup directory: %v", err)
		}
		contents := []byte(nil)
		switch {
		case kind == shellselect.Bash && filepath.Base(startup.path) == ".bashrc":
			contents = []byte("source \"$HOME/" + scriptName + "\"\n")
		case kind == shellselect.Zsh && filepath.Base(startup.path) == ".zshrc":
			contents = []byte("source \"$ZDOTDIR/" + scriptName + "\"\n")
		case kind == shellselect.Fish && filepath.Base(startup.path) == "config.fish":
			contents = []byte("source \"$HOME/" + scriptName + "\"\n")
		}
		if err := os.WriteFile(startup.path, contents, 0o600); err != nil {
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
			isolateShellConfiguration(t, cwd, kind)
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
	isolateShellConfiguration(t, cwd, shellselect.Bash)
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
		isolateShellConfiguration(t, cwd, shellselect.Bash)
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
		isolateShellConfiguration(t, cwd, shellselect.Bash)
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
	isolateShellConfiguration(t, cwd, shellselect.Bash)
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

func newTestController(t *testing.T, kind shellselect.Kind) *sessionController {
	t.Helper()
	controller, err := newSessionControllerForShell(kind, []byte(t.TempDir()))
	if err != nil {
		t.Fatalf("newSessionControllerForShell(%s): %v", kind, err)
	}
	t.Cleanup(func() {
		if err := controller.reducer.Close(); err != nil {
			t.Errorf("close reducer: %v", err)
		}
	})
	return controller
}

func applyIntegrationWire(t *testing.T, controller *sessionController, wire []byte) {
	t.Helper()
	var applyErr error
	controller.decoder.Push(wire, func(frame shellevents.DecodedFrame) {
		if applyErr == nil {
			applyErr = controller.applyFrame(frame)
		}
	})
	if applyErr != nil {
		t.Fatalf("apply integration wire: %v", applyErr)
	}
}

func pendingDestinationBytes(pending *pendingWrites, destination writeDestination) []byte {
	var result []byte
	for _, write := range pending.writes {
		if write.destination == destination {
			result = append(result, write.bytes[write.written:]...)
		}
	}
	return result
}

func TestControllerUsesExactInitialConfiguration(t *testing.T) {
	cwd := []byte("/tmp/Greendale-\xff")
	for _, tc := range []struct {
		kind shellselect.Kind
		want string
	}{
		{kind: shellselect.Bash, want: shellintegration.SyncProbeSequence},
		{kind: shellselect.Zsh, want: shellintegration.SyncProbeSequence},
		{kind: shellselect.Fish, want: shellintegration.FishSyncProbeSequence},
	} {
		t.Run(tc.kind.String(), func(t *testing.T) {
			controller, err := newSessionControllerForShell(tc.kind, cwd)
			if err != nil {
				t.Fatal(err)
			}
			defer func() {
				if err := controller.reducer.Close(); err != nil {
					t.Errorf("close reducer: %v", err)
				}
			}()
			if !bytes.Equal(controller.reducer.CWD(), cwd) {
				t.Error("reducer did not preserve exact cwd bytes")
			}
			shell := controller.reducer.Shell()
			if controller.decoder.Epoch() != shellevents.InitialStreamEpoch() ||
				shell.Epoch() != shellevents.InitialStreamEpoch() {
				t.Error("controller did not use the initial stream epoch")
			}
			if string(controller.syncProbeSequence) != tc.want {
				t.Errorf("sync probe = %q, want %q", controller.syncProbeSequence, tc.want)
			}
		})
	}
}

func TestPendingProbeResyncControlPrecedesWakeupAndCancelsSafely(t *testing.T) {
	controller := newTestController(t, shellselect.Bash)
	request, err := shellcontrol.NewProbeResyncRequestID(7)
	if err != nil {
		t.Fatal(err)
	}
	if err := controller.enqueueProbeResync(request); err != nil {
		t.Fatal(err)
	}
	if len(controller.pending.writes) != 2 {
		t.Fatalf("pending writes = %d, want 2", len(controller.pending.writes))
	}
	control := controller.pending.writes[0]
	wakeup := controller.pending.writes[1]
	if control.destination != writeControl || wakeup.destination != writePTY {
		t.Fatalf("destinations = (%d, %d), want control then PTY", control.destination, wakeup.destination)
	}
	if !bytes.Equal(wakeup.bytes, []byte(shellintegration.SyncProbeSequence)) {
		t.Errorf("wakeup = %q", wakeup.bytes)
	}
	if err := controller.pending.advanceFront(3); err != nil {
		t.Fatal(err)
	}
	controller.pending.cancelProbeResync(request)
	if len(controller.pending.writes) != 1 {
		t.Fatalf("writes after partial cancellation = %d, want 1", len(controller.pending.writes))
	}
	remaining := controller.pending.writes[0]
	if remaining.destination != writeControl || remaining.written != 3 || remaining.hasGroup {
		t.Errorf("preserved write = %+v, want detached partial control", remaining)
	}
	if got := pendingDestinationBytes(&controller.pending, writePTY); len(got) != 0 {
		t.Errorf("cancelled wakeup remains: %q", got)
	}
}

func TestControllerInputBatchBoundariesRemainByteTransparent(t *testing.T) {
	controller := newTestController(t, shellselect.Bash)
	chunks := [][]byte{
		{0x12},
		{0x1b},
		[]byte("[Z\xc3"),
		append([]byte{0xa9}, bytes.Repeat([]byte("x"), 600)...),
	}
	var want []byte
	for _, chunk := range chunks {
		want = append(want, chunk...)
		if err := controller.routeInput(chunk); err != nil {
			t.Fatalf("route input: %v", err)
		}
	}
	if err := controller.finishInput(); err != nil {
		t.Fatalf("finish input: %v", err)
	}
	if got := pendingDestinationBytes(&controller.pending, writePTY); !bytes.Equal(got, want) {
		t.Errorf("forwarded input = %q, want %q", got, want)
	}
	if got := pendingDestinationBytes(&controller.pending, writeControl); len(got) != 0 {
		t.Errorf("unexpected control bytes in transparent fallback: %q", got)
	}
}

func TestControllerMalformedIntegrationRecoversAuthoritativeSnapshot(t *testing.T) {
	controller := newTestController(t, shellselect.Bash)
	applyIntegrationWire(t, controller, []byte("not-an-event\x00"))
	applyIntegrationWire(t, controller, []byte("capability:sync-probe:7\x00prompt-ready\x00"))
	if err := controller.routeInput([]byte("é")); err != nil {
		t.Fatal(err)
	}
	if got := pendingDestinationBytes(&controller.pending, writePTY); !bytes.HasSuffix(got, []byte(shellintegration.SyncProbeSequence)) {
		t.Fatalf("probe wakeup missing after recovery: %q", got)
	}
	controller.pending = pendingWrites{}
	applyIntegrationWire(t, controller, []byte("probe-buffer:b:8:2:é\x00"))

	shell := controller.reducer.Shell()
	if shell.Capability() != shellevents.BufferSyncProbe {
		t.Errorf("capability = %d, want probe", shell.Capability())
	}
	snapshot, ok := shell.Buffer()
	if !ok || !bytes.Equal(snapshot.Bytes(), []byte("é")) || snapshot.Cursor() != 2 {
		t.Errorf("authoritative snapshot = (%v, %t)", snapshot, ok)
	}
}

func TestControllerNoProviderQueryUnicodeMultilineReplacementAndAcknowledgment(t *testing.T) {
	controller := newTestController(t, shellselect.Bash)
	applyIntegrationWire(t, controller, []byte("capability:sync-probe:0\x00prompt-ready\x00"))
	if err := controller.routeInput([]byte("tail")); err != nil {
		t.Fatal(err)
	}
	controller.pending = pendingWrites{}
	applyIntegrationWire(t, controller, []byte("probe-buffer:b:1:4:tail\x00"))
	query, ok := controller.reducer.ActiveQuery()
	if !ok {
		t.Fatal("authoritative nonempty snapshot did not start a no-provider query")
	}

	inserted := "Troy 🏫\nGreendale: "
	want := inserted + "tail"
	edit, err := completion.NewTextEdit(0, 0, inserted)
	if err != nil {
		t.Fatal(err)
	}
	if err := controller.applyEffects(
		controller.reducer.ApplyAliasExpansion(query.Generation(), edit),
	); err != nil {
		t.Fatalf("apply replacement effects: %v", err)
	}
	if len(controller.pending.writes) != 2 ||
		controller.pending.writes[0].destination != writeControl ||
		controller.pending.writes[1].destination != writePTY {
		t.Fatalf("replacement writes = %+v, want control then wakeup", controller.pending.writes)
	}
	if got := controller.pending.writes[1].bytes; !bytes.Equal(got, []byte(shellintegration.SyncProbeSequence)) {
		t.Errorf("replacement PTY bytes = %q, want only sync wakeup", got)
	}

	var frames []shellcontrol.DecodedControlFrame
	decoder := shellcontrol.NewDecoder()
	decoder.Push(controller.pending.writes[0].bytes, func(frame shellcontrol.DecodedControlFrame) {
		frames = append(frames, frame)
	})
	if len(frames) != 1 {
		t.Fatalf("decoded replacement frames = %d, want 1", len(frames))
	}
	control, ok := frames[0].Replacement()
	if !ok || control.Buffer() != want || control.Cursor() != len(want) ||
		control.RequestID().Value() != 2 {
		t.Errorf("replacement control = (%v, %t)", control, ok)
	}

	controller.pending = pendingWrites{}
	ack := []byte(fmt.Sprintf("probe-buffer:b:2:%d:%s\x00", len(want), want))
	applyIntegrationWire(t, controller, ack)
	shell := controller.reducer.Shell()
	snapshot, ok := shell.Buffer()
	if !ok || string(snapshot.Bytes()) != want || snapshot.Cursor() != len(want) {
		t.Errorf("acknowledged snapshot = (%v, %t)", snapshot, ok)
	}
	if controller.reducer.ReplacementPending() {
		t.Error("replacement remained pending after authoritative acknowledgment")
	}
}

func TestPendingWritesEnforceBoundWithoutMutation(t *testing.T) {
	var pending pendingWrites
	oversized := make([]byte, maxPendingWriteBytes+1)
	if err := pending.push(writePTY, oversized); err == nil {
		t.Fatal("oversized pending write succeeded")
	}
	if !pending.empty() || pending.bytes != 0 {
		t.Errorf("rejected queue = (%d writes, %d bytes)", len(pending.writes), pending.bytes)
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
