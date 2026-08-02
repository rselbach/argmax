package screen

import (
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"
)

type guardState uint8

const (
	guardGround guardState = iota
	guardEscape
	guardCSI
	guardOSC
	guardOSCEscape
	guardString
	guardStringEscape
	guardDiscardOSC
	guardDiscardOSCEscape
	guardDiscardString
	guardDiscardStringEscape
)

type guardEvent uint8

const (
	guardNone guardEvent = iota
	guardUnsupported
	guardOverflow
	guardResumeAfterDiscard
)

type sequenceGuard struct {
	state              guardState
	utf8Remaining      uint8
	controlBytes       int
	escapeIntermediate bool
	csiIntermediate    bool
	csiPrefix          bool
	csiParamSeen       bool
	csiParams          int
	csiValue           uint32
}

func (guard sequenceGuard) complete() bool {
	return guard.state == guardGround && guard.utf8Remaining == 0
}

func (guard *sequenceGuard) beginEscape() {
	guard.state = guardEscape
	guard.escapeIntermediate = false
}

func (guard *sequenceGuard) beginCSI() {
	guard.state = guardCSI
	guard.controlBytes = 0
	guard.csiIntermediate = false
	guard.csiPrefix = false
	guard.csiParamSeen = false
	guard.csiParams = 1
	guard.csiValue = 0
}

func (guard *sequenceGuard) beginOSC() {
	guard.state = guardOSC
	guard.controlBytes = 0
}

func (guard *sequenceGuard) beginString() {
	guard.state = guardString
	guard.controlBytes = 0
}

func utf8ContinuationCount(value byte) uint8 {
	switch {
	case value >= 0xc2 && value <= 0xdf:
		return 1
	case value >= 0xe0 && value <= 0xef:
		return 2
	case value >= 0xf0 && value <= 0xf4:
		return 3
	default:
		return 0
	}
}

func (guard *sequenceGuard) step(value byte) guardEvent {
	if guard.state == guardGround && guard.utf8Remaining != 0 {
		if value >= 0x80 && value <= 0xbf {
			guard.utf8Remaining--
			return guardNone
		}
		guard.utf8Remaining = 0
		return guardUnsupported
	}

	switch guard.state {
	case guardGround:
		if remaining := utf8ContinuationCount(value); remaining != 0 {
			guard.utf8Remaining = remaining
			return guardNone
		}
		switch value {
		case 0x1b:
			guard.beginEscape()
		case 0x90:
			guard.beginString()
			return guardUnsupported
		case 0x98, 0x9e, 0x9f:
			guard.beginString()
			return guardUnsupported
		case 0x9b:
			guard.beginCSI()
			return guardUnsupported
		case 0x9d:
			guard.beginOSC()
			return guardUnsupported
		case 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
			0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f,
			0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x99,
			0x9a, 0x9c:
			return guardUnsupported
		}
		return guardNone

	case guardEscape:
		switch {
		case value == '[':
			guard.beginCSI()
		case value == ']':
			guard.beginOSC()
		case value == 'P' || value == 'X' || value == '^' || value == '_':
			guard.beginString()
			return guardUnsupported
		case value == 0x18 || value == 0x1a:
			guard.state = guardGround
		case value == 0x1b:
			guard.beginEscape()
		case value >= 0x20 && value <= 0x2f:
			if guard.escapeIntermediate {
				return guardUnsupported
			}
			guard.escapeIntermediate = true
		case value >= 0x30 && value <= 0x7e:
			guard.state = guardGround
		case value == 0x7f || value <= 0x1f:
		default:
			guard.state = guardGround
			return guardUnsupported
		}
		return guardNone

	case guardCSI:
		guard.controlBytes++
		if guard.controlBytes > MaxControlStringBytes {
			return guardUnsupported
		}
		switch {
		case value == 0x18 || value == 0x1a:
			guard.state = guardGround
		case value == 0x1b:
			guard.beginEscape()
		case value <= 0x1f || value == 0x7f:
		case value >= 0x20 && value <= 0x2f:
			if guard.csiIntermediate {
				return guardUnsupported
			}
			guard.csiIntermediate = true
		case value >= '0' && value <= '9':
			if guard.csiIntermediate {
				guard.state = guardGround
				return guardUnsupported
			}
			guard.csiParamSeen = true
			guard.csiValue = min(guard.csiValue*10+uint32(value-'0'), uint32(^uint16(0))+1)
			if guard.csiValue > uint32(^uint16(0)) {
				return guardUnsupported
			}
		case value == ';' || value == ':':
			if guard.csiIntermediate {
				guard.state = guardGround
				return guardUnsupported
			}
			guard.csiParamSeen = true
			guard.csiParams++
			guard.csiValue = 0
			if guard.csiParams > 32 {
				return guardUnsupported
			}
		case value >= 0x3c && value <= 0x3f:
			if guard.csiPrefix || guard.csiIntermediate || guard.csiParamSeen {
				return guardUnsupported
			}
			guard.csiPrefix = true
		case value >= 0x40 && value <= 0x7e:
			guard.state = guardGround
		default:
			return guardUnsupported
		}
		return guardNone

	case guardOSC:
		switch value {
		case 0x07, 0x18, 0x1a, 0x9c:
			guard.state = guardGround
		case 0x1b:
			guard.state = guardOSCEscape
		default:
			guard.controlBytes++
			if guard.controlBytes > MaxControlStringBytes {
				guard.state = guardDiscardOSC
				return guardOverflow
			}
		}
		return guardNone

	case guardOSCEscape:
		if value == '\\' {
			guard.state = guardGround
			return guardNone
		}
		guard.beginEscape()
		return guard.step(value)

	case guardString:
		switch value {
		case 0x18, 0x1a, 0x9c:
			guard.state = guardGround
		case 0x1b:
			guard.state = guardStringEscape
		default:
			guard.controlBytes++
			if guard.controlBytes > MaxControlStringBytes {
				guard.state = guardDiscardString
				return guardOverflow
			}
		}
		return guardNone

	case guardStringEscape:
		if value == '\\' {
			guard.state = guardGround
		} else {
			guard.state = guardString
		}
		return guardNone

	case guardDiscardOSC:
		switch value {
		case 0x07, 0x18, 0x1a, 0x9c:
			guard.state = guardGround
			return guardResumeAfterDiscard
		case 0x1b:
			guard.state = guardDiscardOSCEscape
		}
		return guardNone

	case guardDiscardOSCEscape:
		if value == '\\' {
			guard.state = guardGround
			return guardResumeAfterDiscard
		}
		guard.state = guardDiscardOSC
		return guardNone

	case guardDiscardString:
		switch value {
		case 0x18, 0x1a, 0x9c:
			guard.state = guardGround
			return guardResumeAfterDiscard
		case 0x1b:
			guard.state = guardDiscardStringEscape
		}
		return guardNone

	case guardDiscardStringEscape:
		if value == '\\' {
			guard.state = guardGround
			return guardResumeAfterDiscard
		}
		guard.state = guardDiscardString
		return guardNone
	default:
		guard.state = guardGround
		return guardUnsupported
	}
}

type utf8Carry struct {
	bytes    [utf8.UTFMax]byte
	length   uint8
	expected uint8
}

func (carry utf8Carry) empty() bool { return carry.length == 0 }

func (carry *utf8Carry) clear() {
	carry.length = 0
	carry.expected = 0
}

func (carry *utf8Carry) start(value, expected byte) {
	carry.bytes[0] = value
	carry.length = 1
	carry.expected = expected
}

func (carry *utf8Carry) pushGround(value byte, emit func([]byte)) {
	if carry.empty() {
		switch {
		case value <= 0x9f:
			emit([]byte{value})
		case value >= 0xc2 && value <= 0xdf:
			carry.start(value, 2)
		case value >= 0xe0 && value <= 0xef:
			carry.start(value, 3)
		case value >= 0xf0 && value <= 0xf4:
			carry.start(value, 4)
		default:
			emit([]byte(string(utf8.RuneError)))
		}
		return
	}

	carry.bytes[carry.length] = value
	carry.length++
	sequence := carry.bytes[:carry.length]
	if utf8.Valid(sequence) {
		emit(sequence)
		carry.clear()
		return
	}
	if !utf8.FullRune(sequence) && carry.length < carry.expected {
		return
	}

	invalidLength := invalidUTF8PrefixLength(sequence)
	remainder := append([]byte(nil), sequence[invalidLength:]...)
	emit([]byte(string(utf8.RuneError)))
	carry.clear()
	for _, next := range remainder {
		carry.pushGround(next, emit)
	}
}

func invalidUTF8PrefixLength(sequence []byte) int {
	if len(sequence) < 2 {
		return 1
	}
	for index := 1; index < len(sequence); index++ {
		lower, upper := byte(0x80), byte(0xbf)
		if index == 1 {
			switch sequence[0] {
			case 0xe0:
				lower = 0xa0
			case 0xed:
				upper = 0x9f
			case 0xf0:
				lower = 0x90
			case 0xf4:
				upper = 0x8f
			}
		}
		if sequence[index] < lower || sequence[index] > upper {
			return index
		}
	}
	return len(sequence)
}

func (observer *ScreenObserver) resetParser() {
	parser := ansi.NewParser()
	parser.SetDataSize(MaxControlStringBytes)
	parser.SetHandler(ansi.Handler{
		Print:   observer.machine.printRune,
		Execute: observer.machine.execute,
		HandleCsi: func(cmd ansi.Cmd, params ansi.Params) {
			observer.machine.handleCSI(cmd, params, observer.suppressDispatch)
		},
		HandleEsc: func(cmd ansi.Cmd) {
			observer.machine.handleESC(cmd, observer.suppressDispatch)
		},
		HandleDcs: func(ansi.Cmd, ansi.Params, []byte) {
			observer.machine.finishGrapheme()
			observer.machine.markUnsafe()
		},
		HandleOsc: func(int, []byte) {
			observer.machine.finishGrapheme()
		},
		HandlePm: func([]byte) {
			observer.machine.finishGrapheme()
			observer.machine.markUnsafe()
		},
		HandleApc: func([]byte) {
			observer.machine.finishGrapheme()
			observer.machine.markUnsafe()
		},
		HandleSos: func([]byte) {
			observer.machine.finishGrapheme()
			observer.machine.markUnsafe()
		},
	})
	observer.parser = parser
}
