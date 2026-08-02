package screen

import (
	"strings"
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"
	"github.com/rivo/uniseg"
)

type cell struct {
	text         string
	width        uint8
	continuation bool
}

func (value *cell) blank() {
	value.text = ""
	value.width = 0
	value.continuation = false
}

type savedCursor struct {
	position    CursorPosition
	wrapPending bool
}

type graphemePending uint8

const (
	graphemeNone graphemePending = iota
	graphemeBoundary
	graphemeWidth
)

func scalarTailPending(r rune) bool {
	return r == '\u200d' ||
		(r >= '\ufe00' && r <= '\ufe0f') ||
		(r >= '\U000e0100' && r <= '\U000e01ef') ||
		(r >= '\U0001f3fb' && r <= '\U0001f3ff')
}

func graphemeTailPending(text string) graphemePending {
	last, _ := utf8.DecodeLastRuneInString(text)
	if scalarTailPending(last) {
		return graphemeBoundary
	}
	if last < '\U0001f1e6' || last > '\U0001f1ff' {
		return graphemeNone
	}

	count := 0
	for len(text) != 0 {
		r, size := utf8.DecodeLastRuneInString(text)
		if r < '\U0001f1e6' || r > '\U0001f1ff' {
			break
		}
		count++
		text = text[:len(text)-size]
	}
	if count%2 == 1 {
		return graphemeBoundary
	}
	return graphemeNone
}

type surface struct {
	size            TerminalSize
	cells           []cell
	cursor          CursorPosition
	saved           savedCursor
	scrollRegion    ScrollRegion
	wrapPending     bool
	graphemePending graphemePending
}

func newSurface(size TerminalSize) surface {
	return surface{
		size:         size,
		cells:        make([]cell, size.cellCount()),
		scrollRegion: fullScrollRegion(size),
	}
}

func (screen *surface) nonemptyCells() int {
	count := 0
	for index := range screen.cells {
		if screen.cells[index].text != "" || screen.cells[index].continuation {
			count++
		}
	}
	return count
}

func (screen *surface) index(row, column uint16) int {
	return int(row)*int(screen.size.columns) + int(column)
}

func (screen *surface) cell(row, column uint16) *cell {
	return &screen.cells[screen.index(row, column)]
}

func (screen *surface) clearCellAndWidePair(row, column uint16) {
	value := *screen.cell(row, column)
	switch {
	case value.continuation && column > 0:
		screen.cell(row, column-1).blank()
	case value.width == 2 && column+1 < screen.size.columns:
		screen.cell(row, column+1).blank()
	}
	screen.cell(row, column).blank()
}

func (screen *surface) clearRange(row, start, end uint16) {
	if row >= screen.size.rows {
		return
	}
	if end > screen.size.columns {
		end = screen.size.columns
	}
	if start > end {
		start = end
	}
	for column := start; column < end; column++ {
		screen.clearCellAndWidePair(row, column)
	}
}

func (screen *surface) clearRows(start, end uint16) {
	if end > screen.size.rows {
		end = screen.size.rows
	}
	if start > end {
		start = end
	}
	for row := start; row < end; row++ {
		screen.clearRange(row, 0, screen.size.columns)
	}
}

func (screen *surface) reset() {
	for index := range screen.cells {
		screen.cells[index].blank()
	}
	screen.cursor = CursorPosition{}
	screen.saved = savedCursor{}
	screen.scrollRegion = fullScrollRegion(screen.size)
	screen.wrapPending = false
	screen.graphemePending = graphemeNone
}

func (screen *surface) resize(size TerminalSize) {
	oldSize := screen.size
	oldCells := screen.cells
	cells := make([]cell, size.cellCount())
	rows := min(oldSize.rows, size.rows)
	columns := min(oldSize.columns, size.columns)
	for row := uint16(0); row < rows; row++ {
		oldStart := int(row) * int(oldSize.columns)
		newStart := int(row) * int(size.columns)
		copy(cells[newStart:newStart+int(columns)], oldCells[oldStart:oldStart+int(columns)])
	}
	screen.size = size
	screen.cells = cells
	screen.cursor.row = min(screen.cursor.row, size.rows-1)
	screen.cursor.column = min(screen.cursor.column, size.columns-1)
	screen.saved.position.row = min(screen.saved.position.row, size.rows-1)
	screen.saved.position.column = min(screen.saved.position.column, size.columns-1)
	screen.scrollRegion = fullScrollRegion(size)
	screen.wrapPending = false
	for row := uint16(0); row < size.rows; row++ {
		screen.repairRow(row)
	}
}

func (screen *surface) repairRow(row uint16) {
	for column := uint16(0); column < screen.size.columns; column++ {
		value := screen.cell(row, column)
		if value.continuation && (column == 0 || screen.cell(row, column-1).width != 2) {
			value.blank()
		}
		value = screen.cell(row, column)
		if value.width == 2 &&
			(column+1 >= screen.size.columns || !screen.cell(row, column+1).continuation) {
			value.blank()
		}
	}
}

func (screen *surface) saveCursor() {
	screen.saved = savedCursor{
		position: screen.cursor, wrapPending: screen.wrapPending,
	}
}

func (screen *surface) restoreCursor() {
	screen.cursor = screen.saved.position
	screen.wrapPending = screen.saved.wrapPending
}

func (screen *surface) cancelWrap() { screen.wrapPending = false }

func (screen *surface) finishGrapheme() bool {
	unresolved := screen.graphemePending == graphemeWidth
	screen.graphemePending = graphemeNone
	return unresolved
}

func (screen *surface) carriageReturn() {
	screen.cursor.column = 0
	screen.cancelWrap()
}

func (screen *surface) backspace() {
	if screen.cursor.column != 0 {
		screen.cursor.column--
	}
	screen.cancelWrap()
}

func (screen *surface) linefeed() {
	screen.cancelWrap()
	switch {
	case screen.cursor.row == screen.scrollRegion.bottom:
		screen.scrollUp(1)
	case screen.cursor.row+1 < screen.size.rows:
		screen.cursor.row++
	}
}

func (screen *surface) reverseIndex() {
	screen.cancelWrap()
	if screen.cursor.row == screen.scrollRegion.top {
		screen.scrollDown(1)
		return
	}
	if screen.cursor.row != 0 {
		screen.cursor.row--
	}
}

func (screen *surface) scrollUp(count int) {
	top := int(screen.scrollRegion.top)
	bottom := int(screen.scrollRegion.bottom)
	columns := int(screen.size.columns)
	rows := bottom - top + 1
	count = min(max(count, 0), rows)
	if count == 0 {
		return
	}
	start := top * columns
	end := (bottom + 1) * columns
	shift := count * columns
	copy(screen.cells[start:end-shift], screen.cells[start+shift:end])
	for index := end - shift; index < end; index++ {
		screen.cells[index].blank()
	}
}

func (screen *surface) scrollDown(count int) {
	top := int(screen.scrollRegion.top)
	bottom := int(screen.scrollRegion.bottom)
	columns := int(screen.size.columns)
	rows := bottom - top + 1
	count = min(max(count, 0), rows)
	if count == 0 {
		return
	}
	start := top * columns
	end := (bottom + 1) * columns
	shift := count * columns
	copy(screen.cells[start+shift:end], screen.cells[start:end-shift])
	for index := start; index < start+shift; index++ {
		screen.cells[index].blank()
	}
}

func (screen *surface) insertLines(count int) {
	if screen.cursor.row < screen.scrollRegion.top || screen.cursor.row > screen.scrollRegion.bottom {
		return
	}
	oldTop := screen.scrollRegion.top
	screen.scrollRegion.top = screen.cursor.row
	screen.scrollDown(count)
	screen.scrollRegion.top = oldTop
}

func (screen *surface) deleteLines(count int) {
	if screen.cursor.row < screen.scrollRegion.top || screen.cursor.row > screen.scrollRegion.bottom {
		return
	}
	oldTop := screen.scrollRegion.top
	screen.scrollRegion.top = screen.cursor.row
	screen.scrollUp(count)
	screen.scrollRegion.top = oldTop
}

func (screen *surface) moveRelative(rows, columns int) {
	row := min(max(int(screen.cursor.row)+rows, 0), int(screen.size.rows)-1)
	column := min(max(int(screen.cursor.column)+columns, 0), int(screen.size.columns)-1)
	screen.cursor = CursorPosition{row: uint16(row), column: uint16(column)}
	screen.cancelWrap()
}

func (screen *surface) gotoPosition(row, column uint16, originMode bool) {
	if originMode {
		row = min(screen.scrollRegion.top+min(row, screen.scrollRegion.bottom-screen.scrollRegion.top), screen.scrollRegion.bottom)
	} else {
		row = min(row, screen.size.rows-1)
	}
	screen.cursor = CursorPosition{row: row, column: min(column, screen.size.columns-1)}
	screen.cancelWrap()
}

func (screen *surface) setScrollRegion(top, bottom uint16) bool {
	if top >= bottom || bottom >= screen.size.rows {
		return false
	}
	screen.scrollRegion = ScrollRegion{top: top, bottom: bottom}
	screen.cursor = CursorPosition{}
	screen.cancelWrap()
	return true
}

func (screen *surface) insertChars(count int) {
	row := screen.cursor.row
	start := int(screen.cursor.column)
	columns := int(screen.size.columns)
	count = min(max(count, 0), columns-start)
	if count == 0 {
		return
	}
	rowStart := int(row) * columns
	copy(
		screen.cells[rowStart+start+count:rowStart+columns],
		screen.cells[rowStart+start:rowStart+columns-count],
	)
	for index := rowStart + start; index < rowStart+start+count; index++ {
		screen.cells[index].blank()
	}
	screen.repairRow(row)
}

func (screen *surface) deleteChars(count int) {
	row := screen.cursor.row
	start := int(screen.cursor.column)
	columns := int(screen.size.columns)
	count = min(max(count, 0), columns-start)
	if count == 0 {
		return
	}
	rowStart := int(row) * columns
	copy(
		screen.cells[rowStart+start:rowStart+columns-count],
		screen.cells[rowStart+start+count:rowStart+columns],
	)
	for index := rowStart + columns - count; index < rowStart+columns; index++ {
		screen.cells[index].blank()
	}
	screen.repairRow(row)
}

func (screen *surface) previousBasePosition() (CursorPosition, bool) {
	column := screen.cursor.column
	if !screen.wrapPending {
		if column == 0 {
			return CursorPosition{}, false
		}
		column--
	}
	if screen.cell(screen.cursor.row, column).continuation && column > 0 {
		column--
	}
	if screen.cell(screen.cursor.row, column).text == "" {
		return CursorPosition{}, false
	}
	return CursorPosition{row: screen.cursor.row, column: column}, true
}

func (screen *surface) extendPreviousGrapheme(r rune, wrapping bool) (extended, handled bool) {
	position, ok := screen.previousBasePosition()
	if !ok {
		return false, false
	}
	old := *screen.cell(position.row, position.column)
	text := old.text + string(r)
	if uniseg.GraphemeClusterCount(text) != 1 {
		return false, false
	}
	if len(text) > MaxCellTextBytes {
		return false, true
	}

	width := ansi.StringWidth(text)
	if provisionalZWJWidth(text) {
		width = 3
	}
	if width < 1 || width > 2 || int(position.column)+width > int(screen.size.columns) {
		screen.cell(position.row, position.column).text = text
		screen.graphemePending = graphemeWidth
		return true, true
	}

	switch {
	case old.width == 1 && width == 2:
		screen.clearCellAndWidePair(position.row, position.column+1)
		screen.cell(position.row, position.column+1).continuation = true
	case old.width == 2 && width == 1:
		screen.cell(position.row, position.column+1).blank()
	case (old.width == 1 || old.width == 2) && (width == 1 || width == 2):
	default:
		return false, true
	}

	value := screen.cell(position.row, position.column)
	value.text = text
	value.width = uint8(width)
	screen.graphemePending = graphemeTailPending(text)
	next := int(position.column) + width
	if next >= int(screen.size.columns) {
		screen.cursor.column = screen.size.columns - 1
		screen.wrapPending = wrapping
	} else {
		screen.cursor.column = uint16(next)
		screen.wrapPending = false
	}
	return true, true
}

func provisionalZWJWidth(text string) bool {
	if !strings.ContainsRune(text, '\u200d') {
		return false
	}
	last, _ := utf8.DecodeLastRuneInString(text)
	return last != '\u200d' && ansi.StringWidth(string(last)) == 1
}

func (screen *surface) printRune(r rune, wrapping, insertMode bool) bool {
	if extended, handled := screen.extendPreviousGrapheme(r, wrapping); handled {
		return extended
	}
	previousUnresolved := screen.finishGrapheme()
	width := ansi.StringWidth(string(r))
	if width == 0 {
		return !previousUnresolved
	}
	width = min(width, 2)
	if screen.wrapPending && wrapping {
		screen.cursor.column = 0
		screen.linefeed()
	}
	screen.wrapPending = false

	remaining := int(screen.size.columns - screen.cursor.column)
	if width > remaining && wrapping {
		screen.cursor.column = 0
		screen.linefeed()
	}
	remaining = int(screen.size.columns - screen.cursor.column)
	if width > remaining {
		return false
	}
	if insertMode {
		screen.insertChars(width)
	}

	row := screen.cursor.row
	column := screen.cursor.column
	pending := graphemeNone
	if scalarTailPending(r) || (r >= '\U0001f1e6' && r <= '\U0001f1ff') {
		pending = graphemeBoundary
	}
	for offset := 0; offset < width; offset++ {
		screen.clearCellAndWidePair(row, column+uint16(offset))
	}
	value := screen.cell(row, column)
	value.text = string(r)
	value.width = uint8(width)
	screen.graphemePending = pending
	if width == 2 {
		screen.cell(row, column+1).continuation = true
	}

	next := int(column) + width
	if next >= int(screen.size.columns) {
		screen.cursor.column = screen.size.columns - 1
		screen.wrapPending = wrapping
	} else {
		screen.cursor.column = uint16(next)
	}
	return !previousUnresolved
}

func (screen *surface) rowText(row uint16) string {
	var text strings.Builder
	for column := uint16(0); column < screen.size.columns; column++ {
		value := screen.cell(row, column)
		switch {
		case value.continuation:
			continue
		case value.text == "":
			text.WriteByte(' ')
		default:
			text.WriteString(value.text)
		}
	}
	return strings.TrimRight(text.String(), " ")
}

func (screen *surface) blankCellsToRight() uint16 {
	count := uint16(0)
	for column := screen.cursor.column; column < screen.size.columns; column++ {
		value := screen.cell(screen.cursor.row, column)
		if value.text != "" || value.continuation {
			break
		}
		count++
	}
	return count
}

func (screen *surface) rowsBelowClear() bool {
	for row := screen.cursor.row + 1; row < screen.size.rows; row++ {
		for column := uint16(0); column < screen.size.columns; column++ {
			value := screen.cell(row, column)
			if value.text != "" || value.continuation {
				return false
			}
		}
	}
	return true
}
