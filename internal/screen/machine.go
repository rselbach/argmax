package screen

import "github.com/charmbracelet/x/ansi"

type terminalMachine struct {
	primary            surface
	alternate          surface
	active             ScreenBuffer
	wrapping           bool
	cursorVisible      bool
	originMode         bool
	insertMode         bool
	newlineMode        bool
	synchronizedOutput bool
	synchronized       bool
	tabStops           []bool
	lastPrinted        rune
	hasLastPrinted     bool
}

func newTerminalMachine(size TerminalSize) terminalMachine {
	return terminalMachine{
		primary:       newSurface(size),
		alternate:     newSurface(size),
		active:        Primary,
		wrapping:      true,
		cursorVisible: true,
		synchronized:  true,
		tabStops:      defaultTabStops(size.columns),
	}
}

func (machine *terminalMachine) surface() *surface {
	if machine.active == Alternate {
		return &machine.alternate
	}
	return &machine.primary
}

func (machine *terminalMachine) markUnsafe() { machine.synchronized = false }

func (machine *terminalMachine) finishGrapheme() {
	if machine.surface().finishGrapheme() {
		machine.markUnsafe()
	}
}

func (machine *terminalMachine) reset() {
	size := machine.primary.size
	machine.primary.reset()
	machine.alternate.reset()
	machine.active = Primary
	machine.wrapping = true
	machine.cursorVisible = true
	machine.originMode = false
	machine.insertMode = false
	machine.newlineMode = false
	machine.synchronizedOutput = false
	machine.synchronized = true
	machine.tabStops = defaultTabStops(size.columns)
	machine.lastPrinted = 0
	machine.hasLastPrinted = false
}

func (machine *terminalMachine) resize(size TerminalSize) {
	unresolved := machine.primary.graphemePending == graphemeWidth ||
		machine.alternate.graphemePending == graphemeWidth
	machine.primary.resize(size)
	machine.alternate.resize(size)
	oldStops := machine.tabStops
	machine.tabStops = defaultTabStops(size.columns)
	copy(machine.tabStops, oldStops)
	if unresolved {
		machine.markUnsafe()
	}
}

func (machine *terminalMachine) switchAlternate(clearScreen bool) {
	machine.active = Alternate
	if clearScreen {
		machine.alternate.reset()
	}
	machine.originMode = false
}

func (machine *terminalMachine) switchPrimary() {
	machine.active = Primary
	machine.originMode = false
}

func (machine *terminalMachine) saveCursor() { machine.surface().saveCursor() }

func (machine *terminalMachine) restoreCursor() { machine.surface().restoreCursor() }

func (machine *terminalMachine) tab(count int, backwards bool) {
	columns := machine.surface().size.columns
	column := machine.surface().cursor.column
	for range count {
		found := false
		if backwards {
			for candidate := int(column) - 1; candidate >= 0; candidate-- {
				if machine.tabStops[candidate] {
					column = uint16(candidate)
					found = true
					break
				}
			}
			if !found {
				column = 0
			}
			continue
		}
		for candidate := int(column) + 1; candidate < int(columns); candidate++ {
			if machine.tabStops[candidate] {
				column = uint16(candidate)
				found = true
				break
			}
		}
		if !found {
			column = columns - 1
		}
	}
	machine.surface().cursor.column = column
	machine.surface().cancelWrap()
}

func (machine *terminalMachine) setPrivateMode(mode uint16, enabled bool) {
	switch mode {
	case 1, 5, 12, 45, 1000, 1002, 1003, 1004, 1005, 1006, 1015, 2004:
	case 6:
		machine.originMode = enabled
		top := uint16(0)
		if enabled {
			top = machine.surface().scrollRegion.top
		}
		machine.surface().gotoPosition(top, 0, false)
	case 7:
		machine.wrapping = enabled
		machine.surface().cancelWrap()
	case 25:
		machine.cursorVisible = enabled
	case 47, 1047:
		if enabled {
			machine.switchAlternate(mode == 1047)
		} else {
			machine.switchPrimary()
		}
	case 1048:
		if enabled {
			machine.saveCursor()
		} else {
			machine.restoreCursor()
		}
	case 1049:
		if enabled {
			machine.primary.saveCursor()
			machine.switchAlternate(true)
		} else {
			machine.switchPrimary()
			machine.primary.restoreCursor()
		}
	case 2026:
		machine.synchronizedOutput = enabled
	default:
		machine.markUnsafe()
	}
}

func (machine *terminalMachine) setANSIMode(mode uint16, enabled bool) {
	switch mode {
	case 4:
		machine.insertMode = enabled
	case 20:
		machine.newlineMode = enabled
	default:
		machine.markUnsafe()
	}
}

func logicalParam(params ansi.Params, index, defaultValue int) int {
	logicalIndex := 0
	first := true
	for _, packed := range params {
		if first && logicalIndex == index {
			return packed.Param(defaultValue)
		}
		if !packed.HasMore() {
			logicalIndex++
			first = true
		} else {
			first = false
		}
	}
	return defaultValue
}

func forEachLogicalParam(params ansi.Params, fn func(int)) {
	first := true
	for _, packed := range params {
		if first {
			fn(packed.Param(0))
		}
		first = !packed.HasMore()
	}
}

func logicalParamsEmpty(params ansi.Params) bool { return len(params) == 0 }

func (machine *terminalMachine) clearScreen(mode uint16) {
	cursor := machine.surface().cursor
	size := machine.surface().size
	switch mode {
	case 0:
		machine.surface().clearRange(cursor.row, cursor.column, size.columns)
		machine.surface().clearRows(cursor.row+1, size.rows)
	case 1:
		machine.surface().clearRows(0, cursor.row)
		machine.surface().clearRange(cursor.row, 0, cursor.column+1)
	case 2:
		machine.surface().clearRows(0, size.rows)
	case 3:
	default:
		machine.markUnsafe()
	}
}

func (machine *terminalMachine) clearLine(mode uint16) {
	cursor := machine.surface().cursor
	columns := machine.surface().size.columns
	switch mode {
	case 0:
		machine.surface().clearRange(cursor.row, cursor.column, columns)
	case 1:
		machine.surface().clearRange(cursor.row, 0, cursor.column+1)
	case 2:
		machine.surface().clearRange(cursor.row, 0, columns)
	default:
		machine.markUnsafe()
	}
}

func (machine *terminalMachine) printRune(r rune) {
	if r >= '\u0080' && r <= '\u009f' {
		machine.execute(byte(r))
		return
	}
	if !machine.surface().printRune(r, machine.wrapping, machine.insertMode) {
		machine.markUnsafe()
	}
	machine.lastPrinted = r
	machine.hasLastPrinted = true
}

func (machine *terminalMachine) execute(value byte) {
	machine.finishGrapheme()
	switch value {
	case 0x00, 0x07, 0x0e, 0x0f, 0x18, 0x1a, 0x7f:
	case 0x08:
		machine.surface().backspace()
	case 0x09:
		machine.tab(1, false)
	case 0x0a, 0x0b, 0x0c:
		machine.surface().linefeed()
		if machine.newlineMode {
			machine.surface().carriageReturn()
		}
	case 0x0d:
		machine.surface().carriageReturn()
	default:
		machine.markUnsafe()
	}
}

func boundedParam(params ansi.Params, index int, defaultValue uint16) uint16 {
	value := logicalParam(params, index, int(defaultValue))
	if value <= 0 {
		return defaultValue
	}
	if value > int(^uint16(0)) {
		return ^uint16(0)
	}
	return uint16(value)
}

func (machine *terminalMachine) handleCSI(cmd ansi.Cmd, params ansi.Params, suppress bool) {
	machine.finishGrapheme()
	if suppress {
		machine.markUnsafe()
		return
	}

	final := cmd.Final()
	prefix := cmd.Prefix()
	intermediate := cmd.Intermediate()
	count := int(boundedParam(params, 0, 1))
	if prefix == 0 && intermediate == 0 {
		switch final {
		case 'A':
			machine.surface().moveRelative(-count, 0)
		case 'B', 'e':
			machine.surface().moveRelative(count, 0)
		case 'C', 'a':
			machine.surface().moveRelative(0, count)
		case 'D':
			machine.surface().moveRelative(0, -count)
		case 'E':
			machine.surface().moveRelative(count, 0)
			machine.surface().carriageReturn()
		case 'F':
			machine.surface().moveRelative(-count, 0)
			machine.surface().carriageReturn()
		case 'G', '`':
			column := boundedParam(params, 0, 1) - 1
			machine.surface().gotoPosition(machine.surface().cursor.row, column, false)
		case 'd':
			row := boundedParam(params, 0, 1) - 1
			machine.surface().gotoPosition(row, machine.surface().cursor.column, machine.originMode)
		case 'H', 'f':
			row := boundedParam(params, 0, 1) - 1
			column := boundedParam(params, 1, 1) - 1
			machine.surface().gotoPosition(row, column, machine.originMode)
		case 'I':
			machine.tab(count, false)
		case 'Z':
			machine.tab(count, true)
		case 'J':
			machine.clearScreen(boundedParam(params, 0, 0))
		case 'K':
			machine.clearLine(boundedParam(params, 0, 0))
		case '@':
			machine.surface().insertChars(count)
		case 'P':
			machine.surface().deleteChars(count)
		case 'X':
			cursor := machine.surface().cursor
			end := uint32(cursor.column) + uint32(count)
			if end > uint32(^uint16(0)) {
				end = uint32(^uint16(0))
			}
			machine.surface().clearRange(cursor.row, cursor.column, uint16(end))
		case 'L':
			machine.surface().insertLines(count)
		case 'M':
			machine.surface().deleteLines(count)
		case 'S':
			machine.surface().scrollUp(count)
		case 'T':
			machine.surface().scrollDown(count)
		case 'b':
			if machine.hasLastPrinted {
				for range count {
					machine.printRune(machine.lastPrinted)
				}
			}
		case 'g':
			switch boundedParam(params, 0, 0) {
			case 0:
				machine.tabStops[machine.surface().cursor.column] = false
			case 3:
				clear(machine.tabStops)
			default:
				machine.markUnsafe()
			}
		case 'h':
			forEachLogicalParam(params, func(mode int) {
				if mode < 0 || mode > int(^uint16(0)) {
					machine.markUnsafe()
					return
				}
				machine.setANSIMode(uint16(mode), true)
			})
		case 'l':
			forEachLogicalParam(params, func(mode int) {
				if mode < 0 || mode > int(^uint16(0)) {
					machine.markUnsafe()
					return
				}
				machine.setANSIMode(uint16(mode), false)
			})
		case 'm', 'c', 'n':
		case 'r':
			rows := machine.surface().size.rows
			top := boundedParam(params, 0, 1) - 1
			bottom := boundedParam(params, 1, rows)
			if bottom == 0 || !machine.surface().setScrollRegion(top, bottom-1) {
				machine.markUnsafe()
			} else if machine.originMode {
				machine.surface().gotoPosition(0, 0, true)
			}
		case 's':
			if logicalParamsEmpty(params) {
				machine.saveCursor()
			} else {
				machine.markUnsafe()
			}
		case 'u':
			if logicalParamsEmpty(params) {
				machine.restoreCursor()
			} else {
				machine.markUnsafe()
			}
		default:
			machine.markUnsafe()
		}
		return
	}

	if prefix == '?' && intermediate == 0 && (final == 'h' || final == 'l') {
		enabled := final == 'h'
		forEachLogicalParam(params, func(mode int) {
			if mode < 0 || mode > int(^uint16(0)) {
				machine.markUnsafe()
				return
			}
			machine.setPrivateMode(uint16(mode), enabled)
		})
		return
	}
	if prefix == 0 && intermediate == ' ' && final == 'q' {
		return
	}
	if prefix == 0 && intermediate == '!' && final == 'p' {
		machine.originMode = false
		machine.insertMode = false
		machine.newlineMode = false
		machine.wrapping = true
		machine.surface().cancelWrap()
		return
	}
	machine.markUnsafe()
}

func (machine *terminalMachine) handleESC(cmd ansi.Cmd, suppress bool) {
	machine.finishGrapheme()
	if suppress {
		machine.markUnsafe()
		return
	}
	final := cmd.Final()
	intermediate := cmd.Intermediate()
	if cmd.Prefix() != 0 {
		machine.markUnsafe()
		return
	}
	if intermediate == 0 {
		switch final {
		case '7':
			machine.saveCursor()
		case '8':
			machine.restoreCursor()
		case 'D':
			machine.surface().linefeed()
		case 'E':
			machine.surface().linefeed()
			machine.surface().carriageReturn()
		case 'H':
			machine.tabStops[machine.surface().cursor.column] = true
		case 'M':
			machine.surface().reverseIndex()
		case 'c':
			machine.reset()
		case '=', '>', '\\':
		default:
			machine.markUnsafe()
		}
		return
	}
	if (intermediate == '(' || intermediate == ')' || intermediate == '*' || intermediate == '+') &&
		(final == 'B' || final == '0') {
		return
	}
	if intermediate == '%' && (final == '@' || final == 'G') {
		return
	}
	machine.markUnsafe()
}

func defaultTabStops(columns uint16) []bool {
	stops := make([]bool, columns)
	for column := uint16(1); column < columns; column++ {
		stops[column] = column%8 == 0
	}
	return stops
}
