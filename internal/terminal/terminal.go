//go:build linux || darwin

// Package terminal validates and owns parent-terminal raw and visual state.
package terminal

import (
	"errors"
	"fmt"
	"io"
	"os"
	"strconv"
	"time"

	"github.com/rselbach/argmax/internal/pty"
	"golang.org/x/sys/unix"
)

const (
	// MaxSerializedWrite is the largest frame accepted by SerializedOutput.
	MaxSerializedWrite = 64 * 1024
	// MaxOverlayCleanup is the largest retained ANSI cleanup program.
	MaxOverlayCleanup = MaxSerializedWrite - 32

	restoreOutputTimeout = 2 * time.Second
)

var (
	showCursor  = []byte("\x1b[?25h")
	hideCursor  = []byte("\x1b[?25l")
	enableWrap  = []byte("\x1b[?7h")
	disableWrap = []byte("\x1b[?7l")
	resetStyle  = []byte("\x1b[0m")
)

// Descriptor exposes a terminal file descriptor. The owner must keep it open
// until Guard.Close has completed.
type Descriptor interface {
	Fd() uintptr
}

// DimensionSource identifies the capability used to acquire dimensions.
type DimensionSource uint8

const (
	DimensionKernel DimensionSource = iota + 1
	DimensionEnvironment
	DimensionFallback
)

// DimensionErrorKind identifies an invalid or unavailable dimension input.
type DimensionErrorKind uint8

const (
	DimensionKernelQuery DimensionErrorKind = iota + 1
	DimensionKernelResize
	DimensionMissing
	DimensionInvalid
	DimensionZeroRows
	DimensionZeroColumns
)

// DimensionError reports dimension failure without retaining environment data.
type DimensionError struct {
	Kind     DimensionErrorKind
	Variable string
	IO       IOKind
}

func (err DimensionError) Error() string {
	switch err.Kind {
	case DimensionKernelQuery:
		return "could not query terminal dimensions: " + err.IO.String()
	case DimensionKernelResize:
		return "could not resize terminal: " + err.IO.String()
	case DimensionMissing:
		return err.Variable + " is not set"
	case DimensionInvalid:
		return err.Variable + " must be an integer from 1 through 65535"
	case DimensionZeroRows:
		return "terminal rows must be greater than zero"
	case DimensionZeroColumns:
		return "terminal columns must be greater than zero"
	default:
		return "terminal dimensions are invalid"
	}
}

// Dimensions is a validated terminal size and its acquisition source.
type Dimensions struct {
	size   pty.Size
	source DimensionSource
}

// FromTerminal captures exact cell and pixel dimensions from descriptor.
func FromTerminal(descriptor Descriptor) (Dimensions, error) {
	value, err := unix.IoctlGetWinsize(int(descriptor.Fd()), unix.TIOCGWINSZ)
	if err != nil {
		return Dimensions{}, DimensionError{Kind: DimensionKernelQuery, IO: classifyIO(err)}
	}
	return FromKernel(pty.Size{
		Rows:        value.Row,
		Cols:        value.Col,
		PixelWidth:  value.Xpixel,
		PixelHeight: value.Ypixel,
	})
}

// FromKernel validates dimensions supplied by a kernel query or adapter.
func FromKernel(size pty.Size) (Dimensions, error) {
	return newDimensions(size, DimensionKernel)
}

// FromEnvironment reads bounded COLUMNS and LINES values. Both must be present.
func FromEnvironment(lookup func(string) (string, bool)) (Dimensions, error) {
	columns, err := parseDimension(lookup, "COLUMNS")
	if err != nil {
		return Dimensions{}, err
	}
	rows, err := parseDimension(lookup, "LINES")
	if err != nil {
		return Dimensions{}, err
	}
	return newDimensions(pty.Size{Rows: rows, Cols: columns}, DimensionEnvironment)
}

// Fallback returns the conventional non-authoritative 80-by-24 dimensions.
func Fallback() Dimensions {
	return Dimensions{
		size:   pty.Size{Rows: 24, Cols: 80},
		source: DimensionFallback,
	}
}

// Size returns the exact validated cell and pixel dimensions.
func (dimensions Dimensions) Size() pty.Size { return dimensions.size }

// Source returns the capability used to acquire the dimensions.
func (dimensions Dimensions) Source() DimensionSource { return dimensions.source }

// ApplyTo applies exact cell and pixel dimensions to descriptor.
func (dimensions Dimensions) ApplyTo(descriptor Descriptor) error {
	if err := dimensions.validate(); err != nil {
		return err
	}
	err := unix.IoctlSetWinsize(int(descriptor.Fd()), unix.TIOCSWINSZ, &unix.Winsize{
		Row: dimensions.size.Rows, Col: dimensions.size.Cols,
		Xpixel: dimensions.size.PixelWidth, Ypixel: dimensions.size.PixelHeight,
	})
	if err != nil {
		return DimensionError{Kind: DimensionKernelResize, IO: classifyIO(err)}
	}
	return nil
}

func newDimensions(size pty.Size, source DimensionSource) (Dimensions, error) {
	dimensions := Dimensions{size: size, source: source}
	if err := dimensions.validate(); err != nil {
		return Dimensions{}, err
	}
	return dimensions, nil
}

func (dimensions Dimensions) validate() error {
	if dimensions.size.Rows == 0 {
		return DimensionError{Kind: DimensionZeroRows}
	}
	if dimensions.size.Cols == 0 {
		return DimensionError{Kind: DimensionZeroColumns}
	}
	return nil
}

func parseDimension(lookup func(string) (string, bool), variable string) (uint16, error) {
	value, ok := lookup(variable)
	if !ok {
		return 0, DimensionError{Kind: DimensionMissing, Variable: variable}
	}
	if len(value) > 5 {
		return 0, DimensionError{Kind: DimensionInvalid, Variable: variable}
	}
	parsed, err := strconv.ParseUint(value, 10, 16)
	if err != nil || parsed == 0 {
		return 0, DimensionError{Kind: DimensionInvalid, Variable: variable}
	}
	return uint16(parsed), nil
}

// Cursor is a validated one-based terminal cursor position.
type Cursor struct {
	row    uint16
	column uint16
}

// NewCursor validates a cursor against dimensions.
func NewCursor(row, column uint16, dimensions Dimensions) (Cursor, error) {
	size := dimensions.Size()
	if row == 0 || row > size.Rows || column == 0 || column > size.Cols {
		return Cursor{}, CleanupError{Kind: CleanupCursorOutOfBounds}
	}
	return Cursor{row: row, column: column}, nil
}

// Row returns the one-based row.
func (cursor Cursor) Row() uint16 { return cursor.row }

// Column returns the one-based column.
func (cursor Cursor) Column() uint16 { return cursor.column }

// CleanupErrorKind identifies rejected overlay cleanup state.
type CleanupErrorKind uint8

const (
	CleanupTooLarge CleanupErrorKind = iota + 1
	CleanupCursorOutOfBounds
	CleanupDimensionMismatch
	CleanupUnsafeSequence
)

// CleanupError reports cleanup rejection without retaining submitted bytes.
type CleanupError struct {
	Kind   CleanupErrorKind
	Actual int
	Limit  int
}

func (err CleanupError) Error() string {
	switch err.Kind {
	case CleanupTooLarge:
		return fmt.Sprintf("cleanup has %d bytes; the limit is %d", err.Actual, err.Limit)
	case CleanupCursorOutOfBounds:
		return "cleanup cursor is outside the terminal dimensions"
	case CleanupDimensionMismatch:
		return "cleanup dimensions do not match the terminal"
	case CleanupUnsafeSequence:
		return "cleanup must contain bounded cursor-and-erasure pairs and an exact final cursor"
	default:
		return "cleanup is invalid"
	}
}

// OverlayCleanup is a bounded sequence of exact, dimension-checked erasures.
type OverlayCleanup struct {
	bytes       []byte
	rows        uint16
	cols        uint16
	finalCursor Cursor
}

// NewOverlayCleanup validates CUP,ECH pairs followed by the exact final CUP.
func NewOverlayCleanup(data []byte, dimensions Dimensions, finalCursor Cursor) (OverlayCleanup, error) {
	if len(data) > MaxOverlayCleanup {
		return OverlayCleanup{}, CleanupError{
			Kind: CleanupTooLarge, Actual: len(data), Limit: MaxOverlayCleanup,
		}
	}
	size := dimensions.Size()
	if finalCursor.row == 0 || finalCursor.row > size.Rows ||
		finalCursor.column == 0 || finalCursor.column > size.Cols {
		return OverlayCleanup{}, CleanupError{Kind: CleanupCursorOutOfBounds}
	}
	if !isInertCleanup(data, size, finalCursor) {
		return OverlayCleanup{}, CleanupError{Kind: CleanupUnsafeSequence}
	}
	owned := make([]byte, len(data))
	copy(owned, data)
	return OverlayCleanup{
		bytes: owned, rows: size.Rows, cols: size.Cols, finalCursor: finalCursor,
	}, nil
}

// String redacts validated ANSI content.
func (cleanup OverlayCleanup) String() string {
	return fmt.Sprintf(
		"terminal.OverlayCleanup{bytes:<validated ANSI>, length:%d, rows:%d, cols:%d, final:(%d,%d)}",
		len(cleanup.bytes), cleanup.rows, cleanup.cols,
		cleanup.finalCursor.row, cleanup.finalCursor.column,
	)
}

// GoString redacts validated ANSI content.
func (cleanup OverlayCleanup) GoString() string { return cleanup.String() }

type eraseRange struct {
	row, start, end uint16
}

func isInertCleanup(data []byte, size pty.Size, finalCursor Cursor) bool {
	index := 0
	erasurePairs := 0
	owned := make([]eraseRange, 0, len(data)/12)
	for {
		cursor, afterCursor, ok := parseCursor(data, index)
		if !ok || cursor.row > size.Rows || cursor.column > size.Cols {
			return false
		}
		if afterCursor == len(data) {
			return erasurePairs != 0 && cursor == finalCursor
		}
		count, afterErase, ok := parseErase(data, afterCursor)
		if !ok || count > size.Cols-cursor.column+1 {
			return false
		}
		end := uint32(cursor.column) + uint32(count) - 1
		if end > uint32(^uint16(0)) {
			return false
		}
		candidate := eraseRange{row: cursor.row, start: cursor.column, end: uint16(end)}
		for _, previous := range owned {
			if previous.row == candidate.row &&
				candidate.start <= previous.end && previous.start <= candidate.end {
				return false
			}
		}
		owned = append(owned, candidate)
		erasurePairs++
		index = afterErase
	}
}

func parseCursor(data []byte, index int) (Cursor, int, bool) {
	if index < 0 || len(data)-index < 2 || data[index] != '\x1b' || data[index+1] != '[' {
		return Cursor{}, index, false
	}
	row, afterRow, ok := parseDecimal(data, index+2, ';')
	if !ok {
		return Cursor{}, index, false
	}
	column, afterColumn, ok := parseDecimal(data, afterRow, 'H')
	if !ok {
		return Cursor{}, index, false
	}
	return Cursor{row: row, column: column}, afterColumn, true
}

func parseErase(data []byte, index int) (uint16, int, bool) {
	if index < 0 || len(data)-index < 2 || data[index] != '\x1b' || data[index+1] != '[' {
		return 0, index, false
	}
	return parseDecimal(data, index+2, 'X')
}

func parseDecimal(data []byte, index int, terminator byte) (uint16, int, bool) {
	start := index
	var value uint32
	for index < len(data) && data[index] >= '0' && data[index] <= '9' {
		value = value*10 + uint32(data[index]-'0')
		if value > uint32(^uint16(0)) {
			return 0, index, false
		}
		index++
	}
	if index == start || value == 0 || index >= len(data) || data[index] != terminator {
		return 0, index, false
	}
	return uint16(value), index + 1, true
}

// IOKind is a stable, content-free I/O failure category.
type IOKind uint8

const (
	IOOther IOKind = iota + 1
	IOWouldBlock
	IOBrokenPipe
	IOInterrupted
	IOInvalid
	IONotTerminal
	IOBadDescriptor
)

// String returns a content-free I/O category.
func (kind IOKind) String() string {
	switch kind {
	case IOWouldBlock:
		return "operation would block"
	case IOBrokenPipe:
		return "broken pipe"
	case IOInterrupted:
		return "interrupted"
	case IOInvalid:
		return "invalid argument"
	case IONotTerminal:
		return "not a terminal"
	case IOBadDescriptor:
		return "bad descriptor"
	default:
		return "other I/O error"
	}
}

func classifyIO(err error) IOKind {
	switch {
	case errors.Is(err, unix.EAGAIN):
		return IOWouldBlock
	case errors.Is(err, unix.EPIPE):
		return IOBrokenPipe
	case errors.Is(err, unix.EINTR):
		return IOInterrupted
	case errors.Is(err, unix.EINVAL):
		return IOInvalid
	case errors.Is(err, unix.ENOTTY), errors.Is(err, unix.EOPNOTSUPP),
		errors.Is(err, unix.ENODEV):
		return IONotTerminal
	case errors.Is(err, unix.EBADF):
		return IOBadDescriptor
	default:
		return IOOther
	}
}

// OutputErrorKind identifies a bounded output failure.
type OutputErrorKind uint8

const (
	OutputTooLarge OutputErrorKind = iota + 1
	OutputIO
)

// OutputError reports output failure without retaining output or writer errors.
type OutputError struct {
	Kind   OutputErrorKind
	Actual int
	Limit  int
	IO     IOKind
}

func (err OutputError) Error() string {
	if err.Kind == OutputTooLarge {
		return fmt.Sprintf("output frame has %d bytes; the limit is %d", err.Actual, err.Limit)
	}
	return "terminal output failed: " + err.IO.String()
}

type outputState struct {
	writer io.Writer
	lock   chan struct{}
}

// SerializedOutput provides one bounded frame boundary shared by producers.
type SerializedOutput struct {
	state *outputState
}

// NewSerializedOutput constructs an output boundary. A capacity-one channel is
// used as the lock so timed acquisition needs neither polling nor a goroutine.
func NewSerializedOutput(writer io.Writer) SerializedOutput {
	lock := make(chan struct{}, 1)
	lock <- struct{}{}
	return SerializedOutput{state: &outputState{writer: writer, lock: lock}}
}

// WriteShell writes one complete shell-output frame without interleaving.
func (output SerializedOutput) WriteShell(data []byte) error { return output.writeFrame(data) }

// WriteOverlay writes one complete overlay frame without interleaving.
func (output SerializedOutput) WriteOverlay(data []byte) error { return output.writeFrame(data) }

// WriteOverlayWithin gives up if the boundary remains held for timeout.
func (output SerializedOutput) WriteOverlayWithin(data []byte, timeout time.Duration) error {
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	return output.writeWithin(data, timer, timeout)
}

// String returns an output representation without writer or output content.
func (output SerializedOutput) String() string {
	return fmt.Sprintf("terminal.SerializedOutput{writer:<redacted>, maximum_frame:%d}", MaxSerializedWrite)
}

// GoString returns an output representation without writer or output content.
func (output SerializedOutput) GoString() string { return output.String() }

func (output SerializedOutput) writeFrame(data []byte) error {
	if err := validateFrame(data); err != nil {
		return err
	}
	<-output.state.lock
	defer func() { output.state.lock <- struct{}{} }()
	return writeAndFlush(output.state.writer, data)
}

func (output SerializedOutput) writeWithin(data []byte, timer *time.Timer, timeout time.Duration) error {
	if err := validateFrame(data); err != nil {
		return err
	}
	stopAndDrain(timer)
	timer.Reset(timeout)
	select {
	case <-output.state.lock:
		stopAndDrain(timer)
		defer func() { output.state.lock <- struct{}{} }()
		return writeAndFlush(output.state.writer, data)
	case <-timer.C:
		return OutputError{Kind: OutputIO, IO: IOWouldBlock}
	}
}

func stopAndDrain(timer *time.Timer) {
	if timer.Stop() {
		return
	}
	select {
	case <-timer.C:
	default:
	}
}

func validateFrame(data []byte) error {
	if len(data) > MaxSerializedWrite {
		return OutputError{Kind: OutputTooLarge, Actual: len(data), Limit: MaxSerializedWrite}
	}
	return nil
}

func writeAndFlush(writer io.Writer, data []byte) error {
	for len(data) > 0 {
		written, err := writer.Write(data)
		if written < 0 || written > len(data) {
			return OutputError{Kind: OutputIO, IO: IOOther}
		}
		data = data[written:]
		if err != nil {
			return OutputError{Kind: OutputIO, IO: classifyIO(err)}
		}
		if written == 0 {
			return OutputError{Kind: OutputIO, IO: IOOther}
		}
	}
	if flusher, ok := writer.(interface{ Flush() error }); ok {
		if err := flusher.Flush(); err != nil {
			return OutputError{Kind: OutputIO, IO: classifyIO(err)}
		}
	}
	return nil
}

// DimensionUpdateErrorKind identifies a rejected dimension update.
type DimensionUpdateErrorKind uint8

const (
	DimensionOverlayCleanupPending DimensionUpdateErrorKind = iota + 1
	DimensionClearFailed
	DimensionUpdateInvalid
)

// DimensionUpdateError reports why guard dimensions stayed unchanged.
type DimensionUpdateError struct {
	Kind      DimensionUpdateErrorKind
	Output    OutputError
	Dimension DimensionError
}

func (err DimensionUpdateError) Error() string {
	switch err.Kind {
	case DimensionOverlayCleanupPending:
		return "old terminal dimensions still own overlay cells"
	case DimensionClearFailed:
		return "could not clear the old terminal grid: " + err.Output.Error()
	case DimensionUpdateInvalid:
		return "could not update terminal dimensions: " + err.Dimension.Error()
	default:
		return "could not update terminal dimensions"
	}
}

// TerminalErrorKind identifies parent-terminal initialization failure.
type TerminalErrorKind uint8

const (
	TerminalInputNotTTY TerminalErrorKind = iota + 1
	TerminalOutputNotTTY
	TerminalInspect
	TerminalCaptureAttributes
	TerminalEnableRawMode
	TerminalEnableRawModeAndRestoreFailed
)

// TerminalError reports setup failure without descriptor or path content.
type TerminalError struct {
	Kind TerminalErrorKind
	IO   IOKind
}

func (err TerminalError) Error() string {
	switch err.Kind {
	case TerminalInputNotTTY:
		return "argmax requires an interactive terminal on standard input"
	case TerminalOutputNotTTY:
		return "argmax requires an interactive terminal on standard output"
	case TerminalInspect:
		return "could not inspect the parent terminal: " + err.IO.String()
	case TerminalCaptureAttributes:
		return "could not capture the parent terminal settings"
	case TerminalEnableRawMode:
		return "could not put the parent terminal into raw mode"
	case TerminalEnableRawModeAndRestoreFailed:
		return "could not enable raw mode or restore the parent terminal settings"
	default:
		return "could not initialize the parent terminal"
	}
}

// RestoreOperation identifies one restoration operation.
type RestoreOperation uint8

const (
	RestoreVisualState RestoreOperation = iota + 1
	RestoreTermios
)

// RestoreFailure is one content-free best-effort restoration failure.
type RestoreFailure struct {
	Operation RestoreOperation
	IO        IOKind
	HasIO     bool
}

// RestoreErrors is a fixed-capacity aggregate of independent failures.
type RestoreErrors struct {
	failures [2]RestoreFailure
	length   uint8
}

func (err RestoreErrors) Error() string {
	switch err.length {
	case 1:
		return "1 terminal restoration operation(s) failed"
	case 2:
		return "2 terminal restoration operation(s) failed"
	default:
		return "terminal restoration failed"
	}
}

// Failures returns all attempted restoration operations that failed.
func (err RestoreErrors) Failures() []RestoreFailure {
	return err.failures[:err.length:err.length]
}

func (err *RestoreErrors) push(failure RestoreFailure) {
	if int(err.length) >= len(err.failures) {
		return
	}
	err.failures[err.length] = failure
	err.length++
}

// Guard explicitly owns parent raw mode and renderer visual state.
type Guard struct {
	input         Descriptor
	output        SerializedOutput
	dimensions    Dimensions
	original      unix.Termios
	rawActive     bool
	cursorHidden  bool
	wrapDisabled  bool
	cleanup       OverlayCleanup
	hasCleanup    bool
	restoreBuffer []byte
	restoreTimer  *time.Timer
}

// Enter validates separate input/output TTY descriptors and enables input raw mode.
func Enter(input, outputDescriptor Descriptor, outputWriter io.Writer, dimensions Dimensions) (*Guard, error) {
	if err := dimensions.validate(); err != nil {
		return nil, err
	}
	if err := validateTTY(input, TerminalInputNotTTY); err != nil {
		return nil, err
	}
	if err := validateTTY(outputDescriptor, TerminalOutputNotTTY); err != nil {
		return nil, err
	}
	original, err := getTermios(int(input.Fd()))
	if err != nil {
		return nil, TerminalError{Kind: TerminalCaptureAttributes}
	}

	timer := time.NewTimer(time.Hour)
	stopAndDrain(timer)
	guard := &Guard{
		input: input, output: NewSerializedOutput(outputWriter), dimensions: dimensions,
		original: *original, restoreBuffer: make([]byte, 0, MaxSerializedWrite),
		restoreTimer: timer,
	}
	raw := *original
	makeRaw(&raw)
	if err := setTermios(int(input.Fd()), &raw); err != nil {
		guard.restoreTimer.Stop()
		if restoreErr := setTermios(int(input.Fd()), original); restoreErr != nil {
			return nil, TerminalError{Kind: TerminalEnableRawModeAndRestoreFailed}
		}
		return nil, TerminalError{Kind: TerminalEnableRawMode}
	}
	guard.rawActive = true
	return guard, nil
}

// EnterStdio enters raw mode on standard input with serialized standard output.
func EnterStdio(dimensions Dimensions) (*Guard, error) {
	return Enter(os.Stdin, os.Stdout, os.Stdout, dimensions)
}

// Output returns the one shared output serialization boundary.
func (guard *Guard) Output() SerializedOutput { return guard.output }

// Dimensions returns the currently retained parent-terminal dimensions.
func (guard *Guard) Dimensions() Dimensions { return guard.dimensions }

// UpdateDimensions stores dimensions only when no old-grid cleanup is retained.
func (guard *Guard) UpdateDimensions(dimensions Dimensions) error {
	if err := dimensions.validate(); err != nil {
		return DimensionUpdateError{
			Kind: DimensionUpdateInvalid, Dimension: err.(DimensionError),
		}
	}
	if guard.hasCleanup {
		return DimensionUpdateError{Kind: DimensionOverlayCleanupPending}
	}
	guard.dimensions = dimensions
	return nil
}

// ClearOverlayAndUpdateDimensions clears retained old-grid cells before update.
func (guard *Guard) ClearOverlayAndUpdateDimensions(dimensions Dimensions) error {
	if err := dimensions.validate(); err != nil {
		return DimensionUpdateError{
			Kind: DimensionUpdateInvalid, Dimension: err.(DimensionError),
		}
	}
	if guard.hasCleanup {
		if err := guard.output.WriteOverlay(guard.cleanup.bytes); err != nil {
			return DimensionUpdateError{Kind: DimensionClearFailed, Output: asOutputError(err)}
		}
		guard.cleanup = OverlayCleanup{}
		guard.hasCleanup = false
	}
	guard.dimensions = dimensions
	return nil
}

// SetOverlayCleanup retains validated cleanup for the current cell grid.
func (guard *Guard) SetOverlayCleanup(cleanup OverlayCleanup) error {
	size := guard.dimensions.Size()
	if cleanup.rows != size.Rows || cleanup.cols != size.Cols {
		return CleanupError{Kind: CleanupDimensionMismatch}
	}
	guard.cleanup = cleanup
	guard.hasCleanup = true
	return nil
}

// OverlayCleared releases retained overlay ownership.
func (guard *Guard) OverlayCleared() {
	guard.cleanup = OverlayCleanup{}
	guard.hasCleanup = false
}

// HideCursor hides and retains ownership of cursor visibility.
func (guard *Guard) HideCursor() error {
	if guard.cursorHidden {
		return nil
	}
	guard.cursorHidden = true
	return guard.output.WriteOverlay(hideCursor)
}

// ShowCursor restores cursor visibility owned by the guard.
func (guard *Guard) ShowCursor() error {
	if !guard.cursorHidden {
		return nil
	}
	if err := guard.output.WriteOverlay(showCursor); err != nil {
		return err
	}
	guard.cursorHidden = false
	return nil
}

// DisableWrap disables wrapping and retains ownership of that mode.
func (guard *Guard) DisableWrap() error {
	if guard.wrapDisabled {
		return nil
	}
	guard.wrapDisabled = true
	return guard.output.WriteOverlay(disableWrap)
}

// EnableWrap restores wrapping owned by the guard.
func (guard *Guard) EnableWrap() error {
	if !guard.wrapDisabled {
		return nil
	}
	if err := guard.output.WriteOverlay(enableWrap); err != nil {
		return err
	}
	guard.wrapDisabled = false
	return nil
}

// EnterRaw reapplies raw mode after a successful Restore, such as when the
// wrapper resumes after suspension. The originally captured settings remain
// the restoration target.
func (guard *Guard) EnterRaw() error {
	if guard.rawActive {
		return nil
	}
	raw := guard.original
	makeRaw(&raw)
	if err := setTermios(int(guard.input.Fd()), &raw); err != nil {
		if restoreErr := setTermios(int(guard.input.Fd()), &guard.original); restoreErr != nil {
			return TerminalError{Kind: TerminalEnableRawModeAndRestoreFailed}
		}
		return TerminalError{Kind: TerminalEnableRawMode}
	}
	guard.rawActive = true
	return nil
}

// Restore restores termios before bounded visual output. Successful portions
// are not repeated; failed portions remain eligible for retry.
func (guard *Guard) Restore() error {
	var failures RestoreErrors
	if guard.rawActive {
		if err := setTermios(int(guard.input.Fd()), &guard.original); err != nil {
			failures.push(RestoreFailure{Operation: RestoreTermios})
		} else {
			guard.rawActive = false
		}
	}

	if guard.hasCleanup || guard.cursorHidden || guard.wrapDisabled {
		guard.restoreBuffer = guard.restoreBuffer[:0]
		if guard.hasCleanup {
			guard.restoreBuffer = append(guard.restoreBuffer, guard.cleanup.bytes...)
		}
		guard.restoreBuffer = append(guard.restoreBuffer, resetStyle...)
		if guard.wrapDisabled {
			guard.restoreBuffer = append(guard.restoreBuffer, enableWrap...)
		}
		if guard.cursorHidden {
			guard.restoreBuffer = append(guard.restoreBuffer, showCursor...)
		}
		err := guard.output.writeWithin(
			guard.restoreBuffer, guard.restoreTimer, restoreOutputTimeout,
		)
		if err != nil {
			outputErr := asOutputError(err)
			failure := RestoreFailure{Operation: RestoreVisualState}
			if outputErr.Kind == OutputIO {
				failure.IO = outputErr.IO
				failure.HasIO = true
			}
			failures.push(failure)
		} else {
			guard.cleanup = OverlayCleanup{}
			guard.hasCleanup = false
			guard.cursorHidden = false
			guard.wrapDisabled = false
		}
	}
	if failures.length != 0 {
		return failures
	}
	return nil
}

// Close is an idempotent explicit restoration alias suitable for defer.
func (guard *Guard) Close() error { return guard.Restore() }

// String redacts descriptors, writer, cleanup bytes, and termios content.
func (guard *Guard) String() string {
	return fmt.Sprintf(
		"terminal.Guard{dimensions:%+v, raw_active:%t, cursor_hidden:%t, wrap_disabled:%t, has_cleanup:%t}",
		guard.dimensions.Size(), guard.rawActive, guard.cursorHidden,
		guard.wrapDisabled, guard.hasCleanup,
	)
}

// GoString redacts descriptors, writer, cleanup bytes, and termios content.
func (guard *Guard) GoString() string { return guard.String() }

func validateTTY(descriptor Descriptor, side TerminalErrorKind) error {
	_, err := getTermios(int(descriptor.Fd()))
	if err == nil {
		return nil
	}
	if errors.Is(err, unix.ENOTTY) || errors.Is(err, unix.EOPNOTSUPP) ||
		errors.Is(err, unix.ENODEV) {
		return TerminalError{Kind: side}
	}
	return TerminalError{Kind: TerminalInspect, IO: classifyIO(err)}
}

func asOutputError(err error) OutputError {
	var outputErr OutputError
	if errors.As(err, &outputErr) {
		return outputErr
	}
	return OutputError{Kind: OutputIO, IO: IOOther}
}

func makeRaw(terminal *unix.Termios) {
	terminal.Iflag &^= unix.BRKINT | unix.ICRNL | unix.INPCK | unix.ISTRIP | unix.IXON
	terminal.Oflag &^= unix.OPOST
	terminal.Cflag &^= unix.CSIZE | unix.PARENB
	terminal.Cflag |= unix.CS8
	terminal.Lflag &^= unix.ECHO | unix.ICANON | unix.IEXTEN | unix.ISIG
	terminal.Cc[unix.VMIN] = 1
	terminal.Cc[unix.VTIME] = 0
}
