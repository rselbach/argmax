// Package screen provides bounded virtual-terminal observation for overlay placement.
package screen

import "fmt"

const (
	// MaxTerminalDimension is the largest accepted terminal dimension.
	MaxTerminalDimension uint16 = 4096
	// MaxScreenCells is the largest retained primary or alternate cell grid.
	MaxScreenCells = 262144
	// MaxCellTextBytes is the largest retained base glyph plus combining marks.
	MaxCellTextBytes = 64
	// MaxControlStringBytes is the largest accepted control-string payload.
	MaxControlStringBytes = 4096
	// MaxUTF8CarryBytes is the largest incomplete UTF-8 prefix retained between observations.
	MaxUTF8CarryBytes = 3
)

// ScreenErrorKind identifies an invalid screen operation.
type ScreenErrorKind uint8

const (
	// InvalidSize identifies zero, excessive, or overlarge terminal dimensions.
	InvalidSize ScreenErrorKind = iota + 1
	// InvalidCursor identifies a synchronization cursor outside the screen.
	InvalidCursor
)

// ScreenError reports invalid screen input without retaining terminal content.
type ScreenError struct {
	kind    ScreenErrorKind
	columns uint16
	rows    uint16
	cursor  CursorPosition
}

// Kind returns the rejection category.
func (err ScreenError) Kind() ScreenErrorKind { return err.kind }

// RejectedSize returns rejected dimensions for an InvalidSize error.
func (err ScreenError) RejectedSize() (columns, rows uint16, ok bool) {
	if err.kind != InvalidSize {
		return 0, 0, false
	}
	return err.columns, err.rows, true
}

// RejectedCursor returns the rejected cursor for an InvalidCursor error.
func (err ScreenError) RejectedCursor() (CursorPosition, bool) {
	return err.cursor, err.kind == InvalidCursor
}

// Error describes the invalid operation.
func (err ScreenError) Error() string {
	switch err.kind {
	case InvalidSize:
		return fmt.Sprintf("terminal size %dx%d is unsupported", err.columns, err.rows)
	case InvalidCursor:
		return fmt.Sprintf(
			"terminal cursor %d,%d is outside the current screen",
			err.cursor.row,
			err.cursor.column,
		)
	default:
		return "screen operation is invalid"
	}
}

// String returns an error representation that never contains terminal content.
func (err ScreenError) String() string { return err.Error() }

// GoString returns a structural error representation without terminal content.
func (err ScreenError) GoString() string {
	return fmt.Sprintf("screen.ScreenError{kind:%d}", err.kind)
}

// TerminalSize is a validated terminal size in display cells.
type TerminalSize struct {
	columns uint16
	rows    uint16
}

// NewTerminalSize validates nonzero bounded terminal dimensions.
func NewTerminalSize(columns, rows uint16) (TerminalSize, error) {
	size := TerminalSize{columns: columns, rows: rows}
	if err := size.validate(); err != nil {
		return TerminalSize{}, err
	}
	return size, nil
}

func (size TerminalSize) validate() error {
	cells := uint64(size.columns) * uint64(size.rows)
	if size.columns == 0 || size.rows == 0 ||
		size.columns > MaxTerminalDimension || size.rows > MaxTerminalDimension ||
		cells > MaxScreenCells {
		return ScreenError{
			kind: InvalidSize, columns: size.columns, rows: size.rows,
		}
	}
	return nil
}

// Columns returns the terminal width.
func (size TerminalSize) Columns() uint16 { return size.columns }

// Rows returns the terminal height.
func (size TerminalSize) Rows() uint16 { return size.rows }

func (size TerminalSize) cellCount() int { return int(size.columns) * int(size.rows) }

// CursorPosition is a zero-based terminal cursor position.
type CursorPosition struct {
	row    uint16
	column uint16
}

// NewCursorPosition constructs a cursor without assuming terminal dimensions.
func NewCursorPosition(row, column uint16) CursorPosition {
	return CursorPosition{row: row, column: column}
}

// Row returns the zero-based row.
func (cursor CursorPosition) Row() uint16 { return cursor.row }

// Column returns the zero-based display-cell column.
func (cursor CursorPosition) Column() uint16 { return cursor.column }

// ScreenBuffer identifies the active terminal screen.
type ScreenBuffer uint8

const (
	// Primary is the normal shell screen with scrollback.
	Primary ScreenBuffer = iota + 1
	// Alternate is the alternate screen used by full-screen applications.
	Alternate
)

// String returns the buffer name.
func (buffer ScreenBuffer) String() string {
	switch buffer {
	case Primary:
		return "primary"
	case Alternate:
		return "alternate"
	default:
		return "unknown"
	}
}

// ScrollRegion is an inclusive zero-based scrolling region.
type ScrollRegion struct {
	top    uint16
	bottom uint16
}

func fullScrollRegion(size TerminalSize) ScrollRegion {
	return ScrollRegion{bottom: size.rows - 1}
}

// Top returns the first row in the region.
func (region ScrollRegion) Top() uint16 { return region.top }

// Bottom returns the last row in the region.
func (region ScrollRegion) Bottom() uint16 { return region.bottom }

// IsFull reports whether the region covers the terminal.
func (region ScrollRegion) IsFull(size TerminalSize) bool {
	return size.rows != 0 && region.top == 0 && region.bottom == size.rows-1
}

// ScreenSnapshot is immutable placement and terminal safety state.
type ScreenSnapshot struct {
	size              TerminalSize
	cursor            CursorPosition
	savedCursor       CursorPosition
	scrollRegion      ScrollRegion
	blankCellsToRight uint16
	rowsBelowClear    bool
	buffer            ScreenBuffer
	wrapping          bool
	wrapPending       bool
	cursorVisible     bool
	originMode        bool
	insertMode        bool
	synchronized      bool
}

// Size returns the current terminal dimensions.
func (snapshot ScreenSnapshot) Size() TerminalSize { return snapshot.size }

// Cursor returns the current terminal cursor.
func (snapshot ScreenSnapshot) Cursor() CursorPosition { return snapshot.cursor }

// SavedCursor returns the most recently saved terminal cursor.
func (snapshot ScreenSnapshot) SavedCursor() CursorPosition { return snapshot.savedCursor }

// ScrollRegion returns the current scrolling region.
func (snapshot ScreenSnapshot) ScrollRegion() ScrollRegion { return snapshot.scrollRegion }

// BlankCellsToRight returns untouched cells before the next occupied cell.
func (snapshot ScreenSnapshot) BlankCellsToRight() uint16 {
	return snapshot.blankCellsToRight
}

// RowsBelowClear reports whether every row below the cursor is untouched.
func (snapshot ScreenSnapshot) RowsBelowClear() bool { return snapshot.rowsBelowClear }

// Buffer returns the active primary or alternate buffer.
func (snapshot ScreenSnapshot) Buffer() ScreenBuffer { return snapshot.buffer }

// Wrapping reports whether automatic line wrapping is enabled.
func (snapshot ScreenSnapshot) Wrapping() bool { return snapshot.wrapping }

// WrapPending reports whether the next printable rune triggers delayed wrapping.
func (snapshot ScreenSnapshot) WrapPending() bool { return snapshot.wrapPending }

// CursorVisible reports whether the terminal cursor is visible.
func (snapshot ScreenSnapshot) CursorVisible() bool { return snapshot.cursorVisible }

// OriginMode reports whether cursor rows are relative to the scrolling region.
func (snapshot ScreenSnapshot) OriginMode() bool { return snapshot.originMode }

// InsertMode reports whether printable runes insert instead of replacing cells.
func (snapshot ScreenSnapshot) InsertMode() bool { return snapshot.insertMode }

// Synchronized reports whether all output since reset or synchronization is understood.
func (snapshot ScreenSnapshot) Synchronized() bool { return snapshot.synchronized }

// OverlaySafe reports whether an inline shell overlay may be placed at this snapshot.
func (snapshot ScreenSnapshot) OverlaySafe() bool {
	return snapshot.synchronized && snapshot.cursorVisible &&
		snapshot.buffer == Primary && snapshot.scrollRegion.IsFull(snapshot.size) &&
		!snapshot.originMode && !snapshot.insertMode && !snapshot.wrapPending
}

// ScreenObservation summarizes one output or resize observation.
type ScreenObservation struct {
	consumedBytes int
	clearOverlay  bool
	overlaySafe   bool
}

// ConsumedBytes returns the number of raw bytes consumed.
func (observation ScreenObservation) ConsumedBytes() int { return observation.consumedBytes }

// ClearOverlay reports whether a previously owned overlay must be cleared.
func (observation ScreenObservation) ClearOverlay() bool { return observation.clearOverlay }

// OverlaySafe reports placement safety after the observation.
func (observation ScreenObservation) OverlaySafe() bool { return observation.overlaySafe }
