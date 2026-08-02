package screen

import (
	"fmt"

	"github.com/charmbracelet/x/ansi"
)

type noCopy struct{}

// Lock marks enclosing values as unsafe to copy for go vet's copylocks check.
func (*noCopy) Lock() {}

// Unlock completes the marker interface used by go vet.
func (*noCopy) Unlock() {}

// ScreenObserver is a bounded streaming observer for child terminal output.
// A ScreenObserver must not be copied after construction.
type ScreenObserver struct {
	noCopy           noCopy
	parser           *ansi.Parser
	machine          terminalMachine
	guard            sequenceGuard
	carry            utf8Carry
	suppressDispatch bool
}

// New constructs a synchronized blank primary terminal screen.
func New(size TerminalSize) (*ScreenObserver, error) {
	if err := size.validate(); err != nil {
		return nil, err
	}
	observer := &ScreenObserver{machine: newTerminalMachine(size)}
	observer.resetParser()
	return observer, nil
}

// Desynchronize suppresses absolute placement until synchronization or reset.
func (observer *ScreenObserver) Desynchronize() {
	observer.machine.markUnsafe()
}

// Observe consumes output while retaining only a bounded incomplete UTF-8 prefix.
func (observer *ScreenObserver) Observe(data []byte) ScreenObservation {
	for _, value := range data {
		if observer.guard.state == guardGround {
			observer.carry.pushGround(value, observer.feedNormalized)
			continue
		}
		observer.feedNormalized([]byte{value})
	}
	return ScreenObservation{
		consumedBytes: len(data),
		clearOverlay:  len(data) != 0,
		overlaySafe:   observer.Snapshot().OverlaySafe(),
	}
}

func (observer *ScreenObserver) feedNormalized(data []byte) {
	for _, value := range data {
		oldState := observer.guard.state
		discarding := oldState == guardDiscardOSC ||
			oldState == guardDiscardOSCEscape ||
			oldState == guardDiscardString ||
			oldState == guardDiscardStringEscape

		if oldState == guardEscape && value == 0x1b {
			observer.parser.Reset()
		}
		if (oldState == guardGround && (value == 0x1b || value == 0x9b)) ||
			oldState == guardOSCEscape ||
			(oldState == guardEscape || oldState == guardCSI) && value == 0x1b {
			observer.suppressDispatch = false
		}

		event := observer.guard.step(value)
		switch event {
		case guardUnsupported:
			observer.machine.markUnsafe()
			if oldState == guardEscape || oldState == guardCSI ||
				observer.guard.state == guardEscape || observer.guard.state == guardCSI {
				observer.suppressDispatch = true
			}
		case guardOverflow:
			observer.parser.Reset()
			observer.machine.markUnsafe()
			observer.suppressDispatch = false
			continue
		case guardResumeAfterDiscard:
			observer.suppressDispatch = false
			continue
		}
		if discarding {
			continue
		}
		observer.parser.Advance(value)
		if observer.guard.state == guardGround {
			observer.suppressDispatch = false
		}
	}
}

// Resize applies a validated resize while preserving the intersecting cell grid.
func (observer *ScreenObserver) Resize(size TerminalSize) (ScreenObservation, error) {
	if err := size.validate(); err != nil {
		return ScreenObservation{}, err
	}
	observer.machine.resize(size)
	return ScreenObservation{
		clearOverlay: true,
		overlaySafe:  observer.Snapshot().OverlaySafe(),
	}, nil
}

// Synchronize establishes a trusted primary-shell cursor boundary.
func (observer *ScreenObserver) Synchronize(cursor CursorPosition) error {
	size := observer.machine.primary.size
	if cursor.row >= size.rows || cursor.column >= size.columns {
		return ScreenError{kind: InvalidCursor, cursor: cursor}
	}
	observer.resetParser()
	observer.guard = sequenceGuard{}
	observer.carry.clear()
	observer.suppressDispatch = false
	observer.machine.active = Primary
	observer.machine.synchronizedOutput = false
	observer.machine.primary.graphemePending = graphemeNone
	cursorChanged := observer.machine.primary.cursor != cursor
	observer.machine.primary.cursor = cursor
	if cursorChanged {
		observer.machine.primary.cancelWrap()
	}
	observer.machine.synchronized = true
	return nil
}

// Snapshot returns current placement and terminal-mode state.
func (observer *ScreenObserver) Snapshot() ScreenSnapshot {
	surface := observer.machine.surface()
	return ScreenSnapshot{
		size:              surface.size,
		cursor:            surface.cursor,
		savedCursor:       surface.saved.position,
		scrollRegion:      surface.scrollRegion,
		blankCellsToRight: surface.blankCellsToRight(),
		rowsBelowClear:    surface.rowsBelowClear(),
		buffer:            observer.machine.active,
		wrapping:          observer.machine.wrapping,
		wrapPending:       surface.wrapPending,
		cursorVisible:     observer.machine.cursorVisible,
		originMode:        observer.machine.originMode,
		insertMode:        observer.machine.insertMode,
		synchronized: observer.machine.synchronized &&
			!observer.machine.synchronizedOutput && observer.guard.complete() &&
			observer.carry.empty() && surface.graphemePending == graphemeNone,
	}
}

// RowText returns visible text for a row, omitting trailing blank cells.
func (observer *ScreenObserver) RowText(row uint16) (string, bool) {
	if row >= observer.machine.surface().size.rows {
		return "", false
	}
	return observer.machine.surface().rowText(row), true
}

// RowWidth returns the display width of visible text in a row.
func (observer *ScreenObserver) RowWidth(row uint16) (int, bool) {
	text, ok := observer.RowText(row)
	if !ok {
		return 0, false
	}
	return ansi.StringWidth(text), true
}

// String returns structural observer state without terminal content.
func (observer *ScreenObserver) String() string {
	if observer == nil {
		return "screen.ScreenObserver(<nil>)"
	}
	return fmt.Sprintf(
		"screen.ScreenObserver{snapshot:%+v, guard:%d, utf8_carry:%d, primary_nonempty:%d, alternate_nonempty:%d}",
		observer.Snapshot(), observer.guard.state, observer.carry.length,
		observer.machine.primary.nonemptyCells(), observer.machine.alternate.nonemptyCells(),
	)
}

// GoString returns structural observer state without terminal content.
func (observer *ScreenObserver) GoString() string { return observer.String() }
