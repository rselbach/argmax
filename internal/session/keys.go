package session

import (
	"bytes"
	"unicode/utf8"

	"github.com/rselbach/argmax/internal/config"
)

// keyKind classifies a decoded input event.
type keyKind int

const (
	keyRune       keyKind = iota
	keyCtrl               // raw control byte or Kitty ctrl+<letter>
	keySpecial            // arrows, tab, shift+tab, enter
	keyBackspace          // 0x7f or 0x08
	keyEscape             // lone ESC
	keyPasteStart         // ESC[200~
	keyPasteEnd           // ESC[201~
	keyDSR                // cursor position report ESC[<r>;<c>R
	keyUnknown            // unrecognized escape sequence, forwarded raw (IN-013)
)

type keyEvent struct {
	kind keyKind
	r    rune
	ctrl byte // lowercase letter for keyCtrl; 0 for ctrl+space
	spec string
	rows int
	cols int
	raw  []byte // keyUnknown: exact bytes to forward
}

// keyParser incrementally decodes a byte stream into key events. It buffers
// incomplete escape sequences; Flush turns a pending lone ESC into keyEscape
// and forwards any other partial sequence as keyUnknown.
type keyParser struct {
	pending []byte
}

// feed decodes b (appended to any pending bytes) into events.
func (p *keyParser) feed(b []byte) []keyEvent {
	data := append(p.pending, b...)
	p.pending = nil
	var out []keyEvent
	for len(data) > 0 {
		ev, n, incomplete := decodeOne(data)
		if incomplete {
			p.pending = append(p.pending[:0], data...)
			break
		}
		out = append(out, ev)
		data = data[n:]
	}
	return out
}

// flush resolves buffered bytes at an input idle point.
func (p *keyParser) flush() []keyEvent {
	if len(p.pending) == 0 {
		return nil
	}
	data := p.pending
	p.pending = nil
	if len(data) == 1 && data[0] == 0x1b {
		return []keyEvent{{kind: keyEscape}}
	}
	// Partial sequence that never completed: forward intact.
	return []keyEvent{{kind: keyUnknown, raw: data}}
}

// decodeOne decodes the first event in data. incomplete=true means more bytes
// are needed.
func decodeOne(data []byte) (ev keyEvent, n int, incomplete bool) {
	b := data[0]
	switch {
	case b == 0x1b:
		return decodeEscape(data)
	case b == 0x7f || b == 0x08:
		return keyEvent{kind: keyBackspace}, 1, false
	case b == '\r' || b == '\n':
		return keyEvent{kind: keySpecial, spec: config.KeyEnter}, 1, false
	case b == '\t':
		return keyEvent{kind: keySpecial, spec: config.KeyTab}, 1, false
	case b < 0x20:
		// Raw control byte (IN-014).
		if b == 0 {
			return keyEvent{kind: keyCtrl, ctrl: 0}, 1, false
		}
		return keyEvent{kind: keyCtrl, ctrl: 'a' + b - 1}, 1, false
	case b < utf8.RuneSelf:
		return keyEvent{kind: keyRune, r: rune(b)}, 1, false
	default:
		r, size := utf8.DecodeRune(data)
		if r == utf8.RuneError && size == 1 {
			if !utf8.FullRune(data) {
				return ev, 0, true
			}
			// Invalid byte: forward as unknown.
			return keyEvent{kind: keyUnknown, raw: data[:1]}, 1, false
		}
		return keyEvent{kind: keyRune, r: r}, size, false
	}
}

// decodeEscape handles sequences starting with ESC.
func decodeEscape(data []byte) (ev keyEvent, n int, incomplete bool) {
	if len(data) == 1 {
		return ev, 0, true // maybe lone ESC
	}
	if data[1] != '[' && data[1] != 'O' {
		// ESC + char (meta). Forward both bytes intact.
		n := 2
		if data[1] >= utf8.RuneSelf {
			_, size := utf8.DecodeRune(data[1:])
			n = 1 + size
		}
		if len(data) < n {
			return ev, 0, true
		}
		return keyEvent{kind: keyUnknown, raw: data[:n]}, n, false
	}
	// CSI (ESC[) or SS3 (ESCO).
	if data[1] == 'O' {
		if len(data) < 3 {
			return ev, 0, true
		}
		switch data[2] {
		case 'A':
			return keyEvent{kind: keySpecial, spec: config.KeyUp}, 3, false
		case 'B':
			return keyEvent{kind: keySpecial, spec: config.KeyDown}, 3, false
		case 'C':
			return keyEvent{kind: keySpecial, spec: config.KeyRight}, 3, false
		case 'D':
			return keyEvent{kind: keySpecial, spec: config.KeyLeft}, 3, false
		}
		return keyEvent{kind: keyUnknown, raw: data[:3]}, 3, false
	}
	// CSI: parameters/intermediates then a final byte 0x40-0x7e.
	i := 2
	for i < len(data) && (data[i] < 0x40 || data[i] > 0x7e) {
		i++
	}
	if i >= len(data) {
		return ev, 0, true
	}
	raw := data[:i+1]
	params := string(data[2:i])
	// Modified arrows (e.g. ESC[1;5A) are not our keys: forward intact
	// (IN-013). Only plain arrows map.
	switch data[i] {
	case 'A', 'B', 'C', 'D':
		if params != "" {
			return keyEvent{kind: keyUnknown, raw: raw}, i + 1, false
		}
		dir := map[byte]string{'A': config.KeyUp, 'B': config.KeyDown, 'C': config.KeyRight, 'D': config.KeyLeft}[data[i]]
		return keyEvent{kind: keySpecial, spec: dir}, i + 1, false
	case 'Z':
		if params != "" {
			return keyEvent{kind: keyUnknown, raw: raw}, i + 1, false
		}
		return keyEvent{kind: keySpecial, spec: config.KeyShiftTab}, i + 1, false
	case '~':
		switch params {
		case "200":
			return keyEvent{kind: keyPasteStart}, i + 1, false
		case "201":
			return keyEvent{kind: keyPasteEnd}, i + 1, false
		}
		return keyEvent{kind: keyUnknown, raw: raw}, i + 1, false
	case 'R':
		var r, c int
		if n, err := scanInts(params, &r, &c); err == nil && n == 2 {
			return keyEvent{kind: keyDSR, rows: r, cols: c}, i + 1, false
		}
		return keyEvent{kind: keyUnknown, raw: raw}, i + 1, false
	case 'u':
		// Kitty keyboard protocol: CSI code;modifiers u (IN-014).
		if ev, ok := decodeKitty(params); ok {
			return ev, i + 1, false
		}
		return keyEvent{kind: keyUnknown, raw: raw}, i + 1, false
	}
	return keyEvent{kind: keyUnknown, raw: raw}, i + 1, false
}

// decodeKitty maps Kitty CSI-u events to ctrl keys. Modifier encoding is
// 1+bits where 4 = ctrl.
func decodeKitty(params string) (keyEvent, bool) {
	var code, mod int
	if n, err := scanInts(params, &code, &mod); err != nil || n < 2 {
		return keyEvent{}, false
	}
	if (mod-1)&4 == 0 {
		return keyEvent{}, false
	}
	if code >= 'a' && code <= 'z' {
		return keyEvent{kind: keyCtrl, ctrl: byte(code)}, true
	}
	if code == ' ' {
		return keyEvent{kind: keyCtrl, ctrl: 0}, true
	}
	return keyEvent{}, false
}

// scanInts parses up to two ';'-separated (or ':'-separated) integers.
func scanInts(s string, a, b *int) (int, error) {
	fields := bytes.FieldsFunc([]byte(s), func(r rune) bool { return r == ';' || r == ':' })
	if len(fields) == 0 {
		return 0, errParse
	}
	vals := make([]int, 0, 2)
	for _, f := range fields {
		v := 0
		for _, c := range f {
			if c < '0' || c > '9' {
				return 0, errParse
			}
			v = v*10 + int(c-'0')
		}
		vals = append(vals, v)
		if len(vals) == 2 {
			break
		}
	}
	switch len(vals) {
	case 1:
		*a = vals[0]
		return 1, nil
	default:
		*a, *b = vals[0], vals[1]
		return 2, nil
	}
}

var errParse = errParseT{}

type errParseT struct{}

func (errParseT) Error() string { return "parse error" }
