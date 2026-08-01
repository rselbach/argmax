//go:build linux || darwin

package terminal

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"reflect"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/pty"
	"golang.org/x/sys/unix"
)

type sharedWriter struct {
	state *sharedWriterState
}

type sharedWriterState struct {
	mu      sync.Mutex
	bytes   []byte
	flushes int
	chunk   int
}

func newSharedWriter(chunk int) sharedWriter {
	return sharedWriter{state: &sharedWriterState{chunk: chunk}}
}

func (writer sharedWriter) Write(data []byte) (int, error) {
	writer.state.mu.Lock()
	defer writer.state.mu.Unlock()
	if writer.state.chunk > 0 && len(data) > writer.state.chunk {
		data = data[:writer.state.chunk]
	}
	writer.state.bytes = append(writer.state.bytes, data...)
	runtime.Gosched()
	return len(data), nil
}

func (writer sharedWriter) Flush() error {
	writer.state.mu.Lock()
	defer writer.state.mu.Unlock()
	writer.state.flushes++
	return nil
}

func (writer sharedWriter) snapshot() ([]byte, int) {
	writer.state.mu.Lock()
	defer writer.state.mu.Unlock()
	return append([]byte(nil), writer.state.bytes...), writer.state.flushes
}

type failingWriter struct {
	err error
}

func (writer failingWriter) Write([]byte) (int, error) { return 0, writer.err }
func (writer failingWriter) Flush() error              { return writer.err }

type prefixFailWriter struct {
	state *prefixFailState
}

type prefixFailState struct {
	mu                     sync.Mutex
	bytes                  []byte
	remainingBeforeFailure int
	failedOnce             bool
}

func newPrefixFailWriter(prefix int) prefixFailWriter {
	return prefixFailWriter{state: &prefixFailState{remainingBeforeFailure: prefix}}
}

func (writer prefixFailWriter) Write(data []byte) (int, error) {
	writer.state.mu.Lock()
	defer writer.state.mu.Unlock()
	if writer.state.failedOnce {
		writer.state.bytes = append(writer.state.bytes, data...)
		return len(data), nil
	}
	if writer.state.remainingBeforeFailure == 0 {
		writer.state.failedOnce = true
		return 0, unix.EPIPE
	}
	accepted := min(len(data), writer.state.remainingBeforeFailure)
	writer.state.bytes = append(writer.state.bytes, data[:accepted]...)
	writer.state.remainingBeforeFailure -= accepted
	return accepted, nil
}

func (writer prefixFailWriter) Flush() error {
	writer.state.mu.Lock()
	defer writer.state.mu.Unlock()
	if writer.state.failedOnce {
		return nil
	}
	writer.state.failedOnce = true
	return unix.EPIPE
}

func (writer prefixFailWriter) bytes() []byte {
	writer.state.mu.Lock()
	defer writer.state.mu.Unlock()
	return append([]byte(nil), writer.state.bytes...)
}

type blockingWriter struct {
	block   *atomic.Bool
	started chan<- struct{}
	release <-chan struct{}
}

func (writer blockingWriter) Write(data []byte) (int, error) {
	if writer.block.Load() {
		writer.started <- struct{}{}
		<-writer.release
	}
	return len(data), nil
}

func (blockingWriter) Flush() error { return nil }

func testDimensions(t *testing.T) Dimensions {
	t.Helper()
	dimensions, err := FromKernel(pty.Size{Rows: 24, Cols: 80})
	if err != nil {
		t.Fatalf("FromKernel(): %v", err)
	}
	return dimensions
}

func testCursor(t *testing.T, dimensions Dimensions, row, column uint16) Cursor {
	t.Helper()
	cursor, err := NewCursor(row, column, dimensions)
	if err != nil {
		t.Fatalf("NewCursor(): %v", err)
	}
	return cursor
}

func testCleanup(t *testing.T, dimensions Dimensions, data []byte, row, column uint16) OverlayCleanup {
	t.Helper()
	cleanup, err := NewOverlayCleanup(
		data, dimensions, testCursor(t, dimensions, row, column),
	)
	if err != nil {
		t.Fatalf("NewOverlayCleanup(): %v", err)
	}
	return cleanup
}

func testPTY(t *testing.T) (*os.File, *os.File) {
	t.Helper()
	master, slave, err := openTestPTY()
	if err != nil {
		t.Fatalf("openTestPTY(): %v", err)
	}
	t.Cleanup(func() {
		if err := errors.Join(master.Close(), slave.Close()); err != nil {
			t.Errorf("close test PTY: %v", err)
		}
	})
	return master, slave
}

func duplicateFile(t *testing.T, file *os.File) *os.File {
	t.Helper()
	fd, err := unix.Dup(int(file.Fd()))
	if err != nil {
		t.Fatalf("Dup(): %v", err)
	}
	duplicate := os.NewFile(uintptr(fd), "test-pty-duplicate")
	if duplicate == nil {
		_ = unix.Close(fd)
		t.Fatal("os.NewFile() returned nil")
	}
	t.Cleanup(func() {
		if err := duplicate.Close(); err != nil {
			t.Errorf("close duplicate: %v", err)
		}
	})
	return duplicate
}

func termiosEqual(actual, want unix.Termios) bool {
	// PENDIN is kernel work state on Darwin rather than a persistent user mode.
	actual.Lflag &^= unix.PENDIN
	want.Lflag &^= unix.PENDIN
	return actual == want
}

func requireTermios(t *testing.T, descriptor Descriptor, want unix.Termios) {
	t.Helper()
	actual, err := getTermios(int(descriptor.Fd()))
	if err != nil {
		t.Fatalf("getTermios(): %v", err)
	}
	if !termiosEqual(*actual, want) {
		t.Errorf("terminal attributes were not restored")
	}
}

func TestDimensionsPreserveCellsPixelsAndSources(t *testing.T) {
	master, slave := testPTY(t)
	initial, err := FromKernel(pty.Size{
		Rows: 37, Cols: 113, PixelWidth: 904, PixelHeight: 592,
	})
	if err != nil {
		t.Fatalf("FromKernel(initial): %v", err)
	}
	if err := initial.ApplyTo(slave); err != nil {
		t.Fatalf("ApplyTo(slave): %v", err)
	}
	captured, err := FromTerminal(master)
	if err != nil {
		t.Fatalf("FromTerminal(master): %v", err)
	}
	if captured != initial || captured.Source() != DimensionKernel {
		t.Errorf("captured = %+v, want %+v", captured, initial)
	}

	resized, err := FromKernel(pty.Size{
		Rows: 51, Cols: 141, PixelWidth: 1128, PixelHeight: 816,
	})
	if err != nil {
		t.Fatalf("FromKernel(resized): %v", err)
	}
	if err := resized.ApplyTo(master); err != nil {
		t.Fatalf("ApplyTo(master): %v", err)
	}
	captured, err = FromTerminal(slave)
	if err != nil {
		t.Fatalf("FromTerminal(slave): %v", err)
	}
	if captured != resized {
		t.Errorf("captured after resize = %+v, want %+v", captured, resized)
	}

	environment, err := FromEnvironment(func(name string) (string, bool) {
		values := map[string]string{"COLUMNS": "132", "LINES": "43"}
		value, ok := values[name]
		return value, ok
	})
	if err != nil {
		t.Fatalf("FromEnvironment(): %v", err)
	}
	if environment.Size() != (pty.Size{Rows: 43, Cols: 132}) ||
		environment.Source() != DimensionEnvironment {
		t.Errorf("environment dimensions = %+v", environment)
	}
	fallback := Fallback()
	if fallback.Size() != (pty.Size{Rows: 24, Cols: 80}) ||
		fallback.Source() != DimensionFallback {
		t.Errorf("Fallback() = %+v", fallback)
	}
}

func TestDimensionsRejectInvalidAndRedactValues(t *testing.T) {
	for _, tc := range []struct {
		name string
		size pty.Size
		kind DimensionErrorKind
	}{
		{name: "rows", size: pty.Size{Cols: 1}, kind: DimensionZeroRows},
		{name: "columns", size: pty.Size{Rows: 1}, kind: DimensionZeroColumns},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := FromKernel(tc.size)
			var dimensionErr DimensionError
			if !errors.As(err, &dimensionErr) || dimensionErr.Kind != tc.kind {
				t.Fatalf("FromKernel() error = %#v, want kind %d", err, tc.kind)
			}
		})
	}

	secret := "hunter2"
	_, err := FromEnvironment(func(name string) (string, bool) {
		if name == "COLUMNS" {
			return secret, true
		}
		return "24", true
	})
	if err == nil || strings.Contains(fmt.Sprintf("%v %#v", err, err), secret) {
		t.Fatalf("malformed environment error leaked value: %v", err)
	}
	_, err = FromEnvironment(func(string) (string, bool) { return "", false })
	var dimensionErr DimensionError
	if !errors.As(err, &dimensionErr) || dimensionErr.Kind != DimensionMissing ||
		dimensionErr.Variable != "COLUMNS" {
		t.Errorf("missing dimension error = %#v", err)
	}

	_, slave := testPTY(t)
	if err := (Dimensions{}).ApplyTo(slave); !errors.As(err, &dimensionErr) ||
		dimensionErr.Kind != DimensionZeroRows {
		t.Errorf("zero Dimensions.ApplyTo() error = %#v", err)
	}
}

func TestCleanupGrammarBoundsOverlapAndFinalCursor(t *testing.T) {
	dimensions := testDimensions(t)
	final := testCursor(t, dimensions, 1, 1)
	if _, err := NewOverlayCleanup(
		[]byte("\x1b[3;4H\x1b[12X\x1b[24;1H"), dimensions,
		testCursor(t, dimensions, 24, 1),
	); err != nil {
		t.Fatalf("valid cleanup rejected: %v", err)
	}
	if _, err := NewOverlayCleanup(
		[]byte("\x1b[3;4H\x1b[1X\x1b[3;5H\x1b[2X\x1b[1;1H"), dimensions, final,
	); err != nil {
		t.Fatalf("adjacent ranges rejected: %v", err)
	}

	unsafeSequences := [][]byte{
		{}, []byte("\x1b[2K"), []byte("\x1b[2J"), []byte("\x1b[A"),
		[]byte("\x1b[3C"), []byte("\x1b[s"), []byte("\x1b[u"),
		[]byte("\x1b[2S"), []byte("\x1b[1;20r"), []byte("\x1b[?25h"),
		[]byte("\x1b[1;2;3H"), []byte("\x1b[12X"), []byte("\x1b[0X"),
		[]byte("\x1b[65536X"), []byte("\x1b[1;1H"),
		[]byte("\x1b[1;1H\x1b[2;2H"),
		[]byte("\x1b[1;79H\x1b[3X\x1b[1;1H"),
		[]byte("\x1b[65536;1H\x1b[1X\x1b[1;1H"),
		[]byte("\x1b[25;1H\x1b[1X\x1b[1;1H"),
		[]byte("\x1b[3;4H\x1b[1X\x1b[3;4H\x1b[1X\x1b[1;1H"),
		[]byte("\x1b[3;4H\x1b[3X\x1b[3;6H\x1b[2X\x1b[1;1H"),
		[]byte("\x1b]0;hunter2\x07"), []byte("rm -rf Greendale"),
		[]byte("\x1b[3;4H\x1b[12X\x1b[24;1H"),
	}
	for _, sequence := range unsafeSequences {
		_, err := NewOverlayCleanup(sequence, dimensions, final)
		var cleanupErr CleanupError
		if !errors.As(err, &cleanupErr) || cleanupErr.Kind != CleanupUnsafeSequence {
			t.Errorf("NewOverlayCleanup(%q) error = %#v", sequence, err)
		}
	}

	tooLarge := bytes.Repeat([]byte{'x'}, MaxOverlayCleanup+1)
	_, err := NewOverlayCleanup(tooLarge, dimensions, final)
	var cleanupErr CleanupError
	if !errors.As(err, &cleanupErr) || cleanupErr.Kind != CleanupTooLarge ||
		cleanupErr.Actual != len(tooLarge) {
		t.Errorf("oversized cleanup error = %#v", err)
	}
	if _, err := NewCursor(0, 1, dimensions); err == nil {
		t.Error("NewCursor accepted row zero")
	}
}

func TestCleanupAcceptsLargestEraseAndRedactsBytes(t *testing.T) {
	dimensions, err := FromKernel(pty.Size{Rows: 1, Cols: ^uint16(0)})
	if err != nil {
		t.Fatalf("FromKernel(): %v", err)
	}
	cleanup := testCleanup(
		t, dimensions, []byte("\x1b[1;1H\x1b[65535X\x1b[1;1H"), 1, 1,
	)
	formatted := fmt.Sprintf("%v %#v", cleanup, cleanup)
	if strings.Contains(formatted, "65535X") {
		t.Errorf("cleanup formatting leaked ANSI: %s", formatted)
	}
}

func TestOutputSerializesPartialWritesAndBoundsFrames(t *testing.T) {
	writer := newSharedWriter(1)
	output := NewSerializedOutput(writer)
	start := make(chan struct{})
	results := make(chan error, 2)
	for _, frame := range [][]byte{[]byte("AAAA"), []byte("BBBB")} {
		frame := frame
		go func() {
			<-start
			results <- output.WriteShell(frame)
		}()
	}
	close(start)
	for range 2 {
		if err := <-results; err != nil {
			t.Fatalf("WriteShell(): %v", err)
		}
	}
	got, flushes := writer.snapshot()
	if string(got) != "AAAABBBB" && string(got) != "BBBBAAAA" {
		t.Errorf("frames interleaved: %q", got)
	}
	if flushes != 2 {
		t.Errorf("flushes = %d, want 2", flushes)
	}

	err := output.WriteOverlay(make([]byte, MaxSerializedWrite+1))
	var outputErr OutputError
	if !errors.As(err, &outputErr) || outputErr.Kind != OutputTooLarge ||
		outputErr.Actual != MaxSerializedWrite+1 {
		t.Errorf("oversized frame error = %#v", err)
	}
}

func TestOutputReportsPartialAndFlushFailuresWithoutContent(t *testing.T) {
	for _, tc := range []struct {
		name   string
		writer io.Writer
	}{
		{name: "write", writer: failingWriter{err: errors.New("hunter2 write")}},
		{name: "flush", writer: &flushFailureWriter{}},
		{name: "zero", writer: zeroWriter{}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := NewSerializedOutput(tc.writer).WriteShell([]byte("Greendale secret"))
			var outputErr OutputError
			if !errors.As(err, &outputErr) || outputErr.Kind != OutputIO {
				t.Fatalf("WriteShell() error = %#v", err)
			}
			formatted := fmt.Sprintf("%v %#v", err, err)
			if strings.Contains(formatted, "hunter2") || strings.Contains(formatted, "Greendale secret") {
				t.Errorf("output error leaked content: %s", formatted)
			}
		})
	}
}

type flushFailureWriter struct{}

func (*flushFailureWriter) Write(data []byte) (int, error) { return len(data), nil }
func (*flushFailureWriter) Flush() error                   { return errors.New("hunter2 flush") }

type zeroWriter struct{}

func (zeroWriter) Write([]byte) (int, error) { return 0, nil }

func TestTimedOutputAcquisitionUsesBoundedChannelPath(t *testing.T) {
	output := NewSerializedOutput(io.Discard)
	<-output.state.lock
	started := time.Now()
	err := output.WriteOverlayWithin([]byte("restore"), 40*time.Millisecond)
	elapsed := time.Since(started)
	output.state.lock <- struct{}{}
	var outputErr OutputError
	if !errors.As(err, &outputErr) || outputErr.IO != IOWouldBlock {
		t.Fatalf("WriteOverlayWithin() error = %#v", err)
	}
	if elapsed < 30*time.Millisecond || elapsed > time.Second {
		t.Errorf("bounded acquisition took %v", elapsed)
	}
	if err := output.WriteOverlayWithin([]byte("restore"), time.Second); err != nil {
		t.Errorf("WriteOverlayWithin() after release: %v", err)
	}
}

func TestEnterRejectsSeparateNonTTYDescriptorsWithoutPaths(t *testing.T) {
	nullInput, err := os.Open("/dev/null")
	if err != nil {
		t.Fatalf("open input: %v", err)
	}
	t.Cleanup(func() {
		if err := nullInput.Close(); err != nil {
			t.Errorf("close null input: %v", err)
		}
	})
	nullOutput, err := os.Open("/dev/null")
	if err != nil {
		t.Fatalf("open output: %v", err)
	}
	t.Cleanup(func() {
		if err := nullOutput.Close(); err != nil {
			t.Errorf("close null output: %v", err)
		}
	})

	_, err = Enter(nullInput, nullOutput, io.Discard, testDimensions(t))
	var terminalErr TerminalError
	if !errors.As(err, &terminalErr) || terminalErr.Kind != TerminalInputNotTTY ||
		strings.Contains(err.Error(), "/dev") {
		t.Fatalf("non-TTY input error = %#v", err)
	}

	_, slave := testPTY(t)
	_, err = Enter(slave, nullOutput, io.Discard, testDimensions(t))
	if !errors.As(err, &terminalErr) || terminalErr.Kind != TerminalOutputNotTTY ||
		strings.Contains(err.Error(), "/dev") {
		t.Fatalf("non-TTY output error = %#v", err)
	}
}

func TestGuardExplicitCloseIsIdempotent(t *testing.T) {
	_, slave := testPTY(t)
	observer := duplicateFile(t, slave)
	outputDescriptor := duplicateFile(t, slave)
	original, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(original): %v", err)
	}
	guard, err := Enter(slave, outputDescriptor, io.Discard, testDimensions(t))
	if err != nil {
		t.Fatalf("Enter(): %v", err)
	}
	raw, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(raw): %v", err)
	}
	if termiosEqual(*raw, *original) {
		t.Error("Enter did not enable raw mode")
	}
	if err := guard.Close(); err != nil {
		t.Fatalf("first Close(): %v", err)
	}
	if err := guard.Close(); err != nil {
		t.Fatalf("second Close(): %v", err)
	}
	requireTermios(t, observer, *original)
}

func TestGuardCanReenterRawAfterExplicitRestore(t *testing.T) {
	_, slave := testPTY(t)
	observer := duplicateFile(t, slave)
	outputDescriptor := duplicateFile(t, slave)
	original, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(original): %v", err)
	}
	guard, err := Enter(slave, outputDescriptor, io.Discard, testDimensions(t))
	if err != nil {
		t.Fatalf("Enter(): %v", err)
	}
	if err := guard.Restore(); err != nil {
		t.Fatalf("Restore(): %v", err)
	}
	requireTermios(t, observer, *original)
	if err := guard.EnterRaw(); err != nil {
		t.Fatalf("EnterRaw(): %v", err)
	}
	raw, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(raw): %v", err)
	}
	if termiosEqual(*raw, *original) {
		t.Error("EnterRaw did not reapply raw mode")
	}
	if err := guard.Close(); err != nil {
		t.Fatalf("Close(): %v", err)
	}
	requireTermios(t, observer, *original)
}

func TestDeferredCloseRestoresThroughPanicRecover(t *testing.T) {
	_, slave := testPTY(t)
	observer := duplicateFile(t, slave)
	outputDescriptor := duplicateFile(t, slave)
	original, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(original): %v", err)
	}
	var closeErr error
	func() {
		defer func() {
			if recovered := recover(); recovered == nil {
				t.Error("panic was not recovered")
			}
		}()
		guard, enterErr := Enter(slave, outputDescriptor, io.Discard, testDimensions(t))
		if enterErr != nil {
			t.Fatalf("Enter(): %v", enterErr)
		}
		defer func() { closeErr = guard.Close() }()
		panic("Troy triggered the crash boundary")
	}()
	if closeErr != nil {
		t.Fatalf("deferred Close(): %v", closeErr)
	}
	requireTermios(t, observer, *original)
}

func TestGuardRestoresOwnedVisualStateOnce(t *testing.T) {
	_, slave := testPTY(t)
	outputDescriptor := duplicateFile(t, slave)
	writer := newSharedWriter(0)
	guard, err := Enter(slave, outputDescriptor, writer, testDimensions(t))
	if err != nil {
		t.Fatalf("Enter(): %v", err)
	}
	if err := guard.HideCursor(); err != nil {
		t.Fatalf("HideCursor(): %v", err)
	}
	if err := guard.DisableWrap(); err != nil {
		t.Fatalf("DisableWrap(): %v", err)
	}
	cleanup := testCleanup(
		t, testDimensions(t), []byte("\x1b[24;1H\x1b[12X\x1b[1;1H"), 1, 1,
	)
	if err := guard.SetOverlayCleanup(cleanup); err != nil {
		t.Fatalf("SetOverlayCleanup(): %v", err)
	}
	if err := guard.Restore(); err != nil {
		t.Fatalf("Restore(): %v", err)
	}
	afterFirst, _ := writer.snapshot()
	if err := guard.Restore(); err != nil {
		t.Fatalf("second Restore(): %v", err)
	}
	afterSecond, _ := writer.snapshot()
	if !bytes.Equal(afterSecond, afterFirst) {
		t.Errorf("second restore emitted bytes: first %q, second %q", afterFirst, afterSecond)
	}
	if !bytes.HasSuffix(afterFirst, []byte("\x1b[0m\x1b[?7h\x1b[?25h")) ||
		!bytes.Contains(afterFirst, []byte("\x1b[12X")) {
		t.Errorf("visual restore = %q", afterFirst)
	}
}

func TestVisualRestoreRetriesEveryAcceptedPrefix(t *testing.T) {
	const cleanupANSI = "\x1b[24;1H\x1b[12X\x1b[1;1H"
	fullRestore := append([]byte(cleanupANSI), resetStyle...)
	for prefix := 0; prefix <= len(fullRestore); prefix++ {
		t.Run(fmt.Sprintf("prefix-%d", prefix), func(t *testing.T) {
			_, slave := testPTY(t)
			outputDescriptor := duplicateFile(t, slave)
			writer := newPrefixFailWriter(prefix)
			guard, err := Enter(slave, outputDescriptor, writer, testDimensions(t))
			if err != nil {
				t.Fatalf("Enter(): %v", err)
			}
			cleanup := testCleanup(t, testDimensions(t), []byte(cleanupANSI), 1, 1)
			if err := guard.SetOverlayCleanup(cleanup); err != nil {
				t.Fatalf("SetOverlayCleanup(): %v", err)
			}
			if err := guard.Restore(); err == nil {
				t.Fatal("first Restore() succeeded")
			}
			if err := guard.Restore(); err != nil {
				t.Fatalf("retry Restore(): %v", err)
			}
			afterRetry := writer.bytes()
			if err := guard.Restore(); err != nil {
				t.Fatalf("idempotent Restore(): %v", err)
			}
			if got := writer.bytes(); !bytes.Equal(got, afterRetry) {
				t.Errorf("third restore emitted bytes: %q", got)
			}
			want := append([]byte(nil), fullRestore[:prefix]...)
			want = append(want, fullRestore...)
			if !bytes.Equal(afterRetry, want) {
				t.Errorf("retry output = %q, want %q", afterRetry, want)
			}
		})
	}
}

func TestRestoreRestoresTermiosBeforeContendedVisualOutput(t *testing.T) {
	_, slave := testPTY(t)
	observer := duplicateFile(t, slave)
	outputDescriptor := duplicateFile(t, slave)
	original, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(original): %v", err)
	}
	block := &atomic.Bool{}
	started := make(chan struct{}, 1)
	release := make(chan struct{})
	writer := blockingWriter{block: block, started: started, release: release}
	guard, err := Enter(slave, outputDescriptor, writer, testDimensions(t))
	if err != nil {
		t.Fatalf("Enter(): %v", err)
	}
	if err := guard.HideCursor(); err != nil {
		t.Fatalf("HideCursor(): %v", err)
	}
	output := guard.Output()
	block.Store(true)
	writerDone := make(chan error, 1)
	go func() { writerDone <- output.WriteShell([]byte("Greendale")) }()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("writer did not acquire output boundary")
	}
	closeDone := make(chan error, 1)
	go func() { closeDone <- guard.Close() }()

	deadline := time.Now().Add(500 * time.Millisecond)
	for {
		actual, getErr := getTermios(int(observer.Fd()))
		if getErr != nil {
			t.Fatalf("getTermios(wait): %v", getErr)
		}
		if termiosEqual(*actual, *original) {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("termios remained raw while visual output was contended")
		}
		time.Sleep(5 * time.Millisecond)
	}
	select {
	case err := <-closeDone:
		t.Fatalf("Close returned before output boundary release: %v", err)
	default:
	}
	block.Store(false)
	close(release)
	if err := <-writerDone; err != nil {
		t.Errorf("blocked WriteShell(): %v", err)
	}
	if err := <-closeDone; err != nil {
		t.Errorf("Close(): %v", err)
	}
}

func TestRestoreTimesOutWithoutLosingVisualOwnership(t *testing.T) {
	_, slave := testPTY(t)
	observer := duplicateFile(t, slave)
	outputDescriptor := duplicateFile(t, slave)
	original, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(original): %v", err)
	}
	guard, err := Enter(slave, outputDescriptor, io.Discard, testDimensions(t))
	if err != nil {
		t.Fatalf("Enter(): %v", err)
	}
	if err := guard.HideCursor(); err != nil {
		t.Fatalf("HideCursor(): %v", err)
	}
	<-guard.output.state.lock
	started := time.Now()
	err = guard.Restore()
	elapsed := time.Since(started)
	guard.output.state.lock <- struct{}{}
	var restoreErr RestoreErrors
	if !errors.As(err, &restoreErr) || !reflect.DeepEqual(
		restoreErr.Failures(),
		[]RestoreFailure{{Operation: RestoreVisualState, IO: IOWouldBlock, HasIO: true}},
	) {
		t.Fatalf("Restore() error = %#v", err)
	}
	if elapsed < restoreOutputTimeout || elapsed > restoreOutputTimeout+time.Second {
		t.Errorf("Restore() timeout = %v", elapsed)
	}
	requireTermios(t, observer, *original)
	if !guard.cursorHidden {
		t.Error("timed-out restore discarded cursor ownership")
	}
	if err := guard.Restore(); err != nil {
		t.Fatalf("retry Restore(): %v", err)
	}
}

func TestVisualFailureLeavesTermiosRestoredAndStateRetryable(t *testing.T) {
	_, slave := testPTY(t)
	observer := duplicateFile(t, slave)
	outputDescriptor := duplicateFile(t, slave)
	original, err := getTermios(int(observer.Fd()))
	if err != nil {
		t.Fatalf("getTermios(original): %v", err)
	}
	guard, err := Enter(
		slave, outputDescriptor, failingWriter{err: unix.EPIPE}, testDimensions(t),
	)
	if err != nil {
		t.Fatalf("Enter(): %v", err)
	}
	bufferPointer := &guard.restoreBuffer[:1][0]
	if err := guard.HideCursor(); err == nil {
		t.Fatal("HideCursor() succeeded")
	}
	err = guard.Restore()
	var restoreErr RestoreErrors
	if !errors.As(err, &restoreErr) || !reflect.DeepEqual(
		restoreErr.Failures(),
		[]RestoreFailure{{Operation: RestoreVisualState, IO: IOBrokenPipe, HasIO: true}},
	) {
		t.Fatalf("Restore() error = %#v", err)
	}
	requireTermios(t, observer, *original)
	if &guard.restoreBuffer[:1][0] != bufferPointer || cap(guard.restoreBuffer) != MaxSerializedWrite {
		t.Error("Restore replaced the buffer allocated before raw mode")
	}
	if !guard.cursorHidden {
		t.Error("failed visual restoration discarded cursor ownership")
	}
}

func TestDimensionUpdatesPreserveOldGridUntilClearSucceeds(t *testing.T) {
	oldDimensions := testDimensions(t)
	newDimensions, err := FromKernel(pty.Size{
		Rows: 30, Cols: 100, PixelWidth: 800, PixelHeight: 600,
	})
	if err != nil {
		t.Fatalf("FromKernel(new): %v", err)
	}
	oldCleanup := testCleanup(
		t, oldDimensions, []byte("\x1b[24;1H\x1b[4X\x1b[1;1H"), 1, 1,
	)

	t.Run("success", func(t *testing.T) {
		_, slave := testPTY(t)
		outputDescriptor := duplicateFile(t, slave)
		writer := newSharedWriter(0)
		guard, enterErr := Enter(slave, outputDescriptor, writer, oldDimensions)
		if enterErr != nil {
			t.Fatalf("Enter(): %v", enterErr)
		}
		t.Cleanup(func() {
			if err := guard.Close(); err != nil {
				t.Errorf("Close() during cleanup: %v", err)
			}
		})
		if err := guard.SetOverlayCleanup(oldCleanup); err != nil {
			t.Fatalf("SetOverlayCleanup(): %v", err)
		}
		err := guard.ClearOverlayAndUpdateDimensions(Dimensions{})
		var updateErr DimensionUpdateError
		if !errors.As(err, &updateErr) || updateErr.Kind != DimensionUpdateInvalid ||
			guard.Dimensions() != oldDimensions || !guard.hasCleanup {
			t.Fatalf("zero dimension update = %#v, dimensions %+v", err, guard.Dimensions())
		}
		err = guard.UpdateDimensions(newDimensions)
		if !errors.As(err, &updateErr) || updateErr.Kind != DimensionOverlayCleanupPending ||
			guard.Dimensions() != oldDimensions {
			t.Fatalf("UpdateDimensions() = %#v, dimensions %+v", err, guard.Dimensions())
		}
		if err := guard.ClearOverlayAndUpdateDimensions(newDimensions); err != nil {
			t.Fatalf("ClearOverlayAndUpdateDimensions(): %v", err)
		}
		got, _ := writer.snapshot()
		if !bytes.Equal(got, oldCleanup.bytes) || guard.Dimensions() != newDimensions {
			t.Errorf("clear output/dimensions = %q / %+v", got, guard.Dimensions())
		}
		if err := guard.SetOverlayCleanup(oldCleanup); err == nil {
			t.Error("accepted stale-grid cleanup")
		}
	})

	t.Run("failure", func(t *testing.T) {
		_, slave := testPTY(t)
		outputDescriptor := duplicateFile(t, slave)
		guard, enterErr := Enter(
			slave, outputDescriptor, failingWriter{err: unix.EPIPE}, oldDimensions,
		)
		if enterErr != nil {
			t.Fatalf("Enter(): %v", enterErr)
		}
		if err := guard.SetOverlayCleanup(oldCleanup); err != nil {
			t.Fatalf("SetOverlayCleanup(): %v", err)
		}
		for range 2 {
			err := guard.ClearOverlayAndUpdateDimensions(newDimensions)
			var updateErr DimensionUpdateError
			if !errors.As(err, &updateErr) || updateErr.Kind != DimensionClearFailed ||
				guard.Dimensions() != oldDimensions || !guard.hasCleanup {
				t.Errorf("failed update = %#v, dimensions %+v", err, guard.Dimensions())
			}
		}
		guard.OverlayCleared()
		if err := guard.Close(); err != nil {
			t.Errorf("Close(): %v", err)
		}
	})
}

func TestGuardAndOutputFormattingRedactOwnedContent(t *testing.T) {
	_, slave := testPTY(t)
	outputDescriptor := duplicateFile(t, slave)
	writer := newSharedWriter(0)
	guard, err := Enter(slave, outputDescriptor, writer, testDimensions(t))
	if err != nil {
		t.Fatalf("Enter(): %v", err)
	}
	t.Cleanup(func() {
		if err := guard.Close(); err != nil {
			t.Errorf("Close() during cleanup: %v", err)
		}
	})
	cleanup := testCleanup(
		t, testDimensions(t), []byte("\x1b[1;1H\x1b[2X\x1b[1;1H"), 1, 1,
	)
	if err := guard.SetOverlayCleanup(cleanup); err != nil {
		t.Fatalf("SetOverlayCleanup(): %v", err)
	}
	formatted := fmt.Sprintf("%v %#v %v %#v", guard, guard, guard.Output(), guard.Output())
	for _, forbidden := range []string{"1;1H", "sharedWriter", "test-pty", "Cc:"} {
		if strings.Contains(formatted, forbidden) {
			t.Errorf("formatting leaked %q: %s", forbidden, formatted)
		}
	}
}
