package input

import (
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/rselbach/argmax/internal/keymap"
)

// EventKind classifies a decoded input event.
type EventKind int

// Event kinds.
const (
	EventKey EventKind = iota
	// EventCPR is a cursor-position report responding to a DSR query; it
	// is consumed by the renderer and never forwarded.
	EventCPR
	// EventPasteStart and EventPasteEnd bracket pasted content, which is
	// forwarded byte-exact without alias expansion.
	EventPasteStart
	EventPasteEnd
	// EventRaw carries an unrecognized sequence forwarded intact.
	EventRaw
)

// Event is one decoded input unit.
type Event struct {
	Kind EventKind
	Key  keymap.Key
	// Raw holds the original bytes for passthrough forwarding.
	Raw []byte
	// Row and Col carry the CPR coordinates.
	Row, Col int
}

// Decoder incrementally parses terminal input bytes into events. Unknown
// escape sequences are preserved and forwarded intact.
type Decoder struct {
	buf []byte
}

// Feed appends raw bytes and returns all complete events. An incomplete
// trailing escape sequence is retained for the next call.
func (d *Decoder) Feed(p []byte) []Event {
	d.buf = append(d.buf, p...)
	var events []Event
	for len(d.buf) > 0 {
		ev, n, ok := d.next()
		if !ok {
			break
		}
		d.buf = d.buf[n:]
		events = append(events, ev)
	}
	return events
}

// Pending reports whether an incomplete sequence is buffered.
func (d *Decoder) Pending() bool { return len(d.buf) > 0 }

// FlushPending interprets a lone buffered ESC as the Escape key; called
// when no continuation bytes arrived in the same read.
func (d *Decoder) FlushPending() []Event {
	if len(d.buf) == 1 && d.buf[0] == 0x1b {
		d.buf = nil
		return []Event{{Kind: EventKey, Key: keymap.Key{Kind: keymap.KindEscape}, Raw: []byte{0x1b}}}
	}
	return nil
}

func (d *Decoder) next() (Event, int, bool) {
	b := d.buf
	switch {
	case b[0] == 0x1b:
		return d.escape()
	case b[0] == '\r' || b[0] == '\n':
		return Event{Kind: EventKey, Key: keymap.Key{Kind: keymap.KindEnter}, Raw: b[:1]}, 1, true
	case b[0] == '\t':
		return Event{Kind: EventKey, Key: keymap.Key{Kind: keymap.KindTab}, Raw: b[:1]}, 1, true
	case b[0] == 0x7f || b[0] == 0x08:
		return Event{Kind: EventKey, Key: keymap.Key{Kind: keymap.KindBackspace}, Raw: b[:1]}, 1, true
	case b[0] == 0x00:
		return Event{Kind: EventKey, Key: keymap.Key{Kind: keymap.KindCtrlSpace}, Raw: b[:1]}, 1, true
	case b[0] < 0x20:
		letter := rune('a' + b[0] - 1)
		return Event{Kind: EventKey, Key: keymap.Key{Kind: keymap.KindCtrl, Rune: letter}, Raw: b[:1]}, 1, true
	default:
		if !utf8.FullRune(b) {
			return Event{}, 0, false
		}
		r, size := utf8.DecodeRune(b)
		return Event{Kind: EventKey, Key: keymap.Key{Kind: keymap.KindRune, Rune: r}, Raw: b[:size]}, size, true
	}
}

// escape parses sequences beginning with ESC.
func (d *Decoder) escape() (Event, int, bool) {
	b := d.buf
	if len(b) == 1 {
		return Event{}, 0, false // possibly incomplete; FlushPending decides
	}
	if b[1] != '[' && b[1] != 'O' {
		// ESC-prefixed rune (Alt+key): forward intact.
		if !utf8.FullRune(b[1:]) {
			return Event{}, 0, false
		}
		_, size := utf8.DecodeRune(b[1:])
		return Event{Kind: EventRaw, Raw: b[:1+size]}, 1 + size, true
	}
	// CSI or SS3: find the final byte.
	for i := 2; i < len(b); i++ {
		c := b[i]
		if c >= 0x40 && c <= 0x7e {
			seq := b[:i+1]
			return d.classify(seq), len(seq), true
		}
		if i > 64 {
			return Event{Kind: EventRaw, Raw: b[:i+1]}, i + 1, true
		}
	}
	return Event{}, 0, false
}

func (d *Decoder) classify(seq []byte) Event {
	s := string(seq)
	body := s[2 : len(s)-1]
	final := s[len(s)-1]
	key := func(k keymap.Kind, r rune) Event {
		return Event{Kind: EventKey, Key: keymap.Key{Kind: k, Rune: r}, Raw: seq}
	}
	switch final {
	case 'A':
		return key(keymap.KindUp, 0)
	case 'B':
		return key(keymap.KindDown, 0)
	case 'C':
		return key(keymap.KindRight, 0)
	case 'D':
		return key(keymap.KindLeft, 0)
	case 'H':
		return key(keymap.KindHome, 0)
	case 'F':
		return key(keymap.KindEnd, 0)
	case 'Z':
		return key(keymap.KindShiftTab, 0)
	case 'R': // cursor position report
		row, col, ok := strings.Cut(body, ";")
		if ok {
			r, err1 := strconv.Atoi(row)
			c, err2 := strconv.Atoi(col)
			if err1 == nil && err2 == nil {
				return Event{Kind: EventCPR, Row: r, Col: c, Raw: seq}
			}
		}
	case '~':
		switch body {
		case "1", "7":
			return key(keymap.KindHome, 0)
		case "3":
			return key(keymap.KindDelete, 0)
		case "4", "8":
			return key(keymap.KindEnd, 0)
		case "200":
			return Event{Kind: EventPasteStart, Raw: seq}
		case "201":
			return Event{Kind: EventPasteEnd, Raw: seq}
		}
	case 'u': // Kitty keyboard protocol: code;mods u
		if ev, ok := kittyKey(body, seq); ok {
			return ev
		}
	}
	return Event{Kind: EventRaw, Raw: seq}
}

// kittyKey decodes Kitty protocol Ctrl+letter and common control keys.
func kittyKey(body string, seq []byte) (Event, bool) {
	codeStr, modStr, _ := strings.Cut(body, ";")
	code, err := strconv.Atoi(codeStr)
	if err != nil {
		return Event{}, false
	}
	mods := 1
	if modStr != "" {
		head, _, _ := strings.Cut(modStr, ":")
		if m, err := strconv.Atoi(head); err == nil {
			mods = m
		}
	}
	ctrl := (mods-1)&4 != 0
	shift := (mods-1)&1 != 0
	mk := func(k keymap.Kind, r rune) (Event, bool) {
		return Event{Kind: EventKey, Key: keymap.Key{Kind: k, Rune: r}, Raw: seq}, true
	}
	switch {
	case code == 13:
		return mk(keymap.KindEnter, 0)
	case code == 27:
		return mk(keymap.KindEscape, 0)
	case code == 9 && shift:
		return mk(keymap.KindShiftTab, 0)
	case code == 9:
		return mk(keymap.KindTab, 0)
	case code == 32 && ctrl:
		return mk(keymap.KindCtrlSpace, 0)
	case ctrl && code >= 'a' && code <= 'z':
		return mk(keymap.KindCtrl, rune(code))
	case ctrl && code >= 'A' && code <= 'Z':
		return mk(keymap.KindCtrl, rune(code-'A'+'a'))
	case !ctrl && code >= 0x20 && code < 0x7f:
		return mk(keymap.KindRune, rune(code))
	}
	return Event{}, false
}
