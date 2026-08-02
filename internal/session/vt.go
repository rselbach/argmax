package session

import (
	"sync"
	"unicode/utf8"

	"github.com/rselbach/argmax/internal/overlay"
)

// vtTracker is a minimal virtual-terminal cursor model fed with the child
// shell's output stream. It tracks the real cursor position so the overlay
// can be placed without ever querying the terminal (which would fight with
// prompt frameworks that issue their own queries). Only cursor-affecting
// sequences are modeled; erase/color/attribute sequences are parsed and
// ignored.
type vtTracker struct {
	mu           sync.Mutex
	row, col     int
	w, h         int
	svRow, svCol int
	pendingWrap  bool

	st      vtState
	params  []byte
	partial []byte // partial UTF-8 sequence
}

type vtState int

const (
	vtGround vtState = iota
	vtEsc
	vtEscSkip // skip one byte (charset/DEC commands)
	vtCSI
	vtOSC
	vtOSCEsc
)

func newVTTracker(w, h int) *vtTracker {
	return &vtTracker{w: w, h: h}
}

// pos returns the current cursor row and column (0-based).
func (t *vtTracker) pos() (int, int) {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.row, t.col
}

// scrolledUp accounts for k literal newlines written directly to the
// terminal at the current cursor row (the overlay's space pre-allocation):
// the cursor moves down k (scrolling S times at the bottom) and is then
// moved back up k, landing S rows above its start.
func (t *vtTracker) scrolledUp(k int) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.h <= 0 {
		return
	}
	s := t.row + k - (t.h - 1)
	if s > 0 {
		t.row -= s
		if t.row < 0 {
			t.row = 0
		}
	}
}

// resize updates the terminal dimensions.
func (t *vtTracker) resize(w, h int) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.w, t.h = w, h
	t.clamp()
}

func (t *vtTracker) clamp() {
	if t.row < 0 {
		t.row = 0
	}
	if t.h > 0 && t.row > t.h-1 {
		t.row = t.h - 1
	}
	if t.col < 0 {
		t.col = 0
	}
	if t.w > 0 && t.col > t.w-1 {
		t.col = t.w - 1
	}
}

// feed processes output bytes.
func (t *vtTracker) feed(b []byte) {
	t.mu.Lock()
	defer t.mu.Unlock()
	for _, c := range b {
		t.step(c)
	}
}

func (t *vtTracker) step(c byte) {
	switch t.st {
	case vtGround:
		t.ground(c)
	case vtEsc:
		t.esc(c)
	case vtEscSkip:
		t.st = vtGround
	case vtCSI:
		t.csi(c)
	case vtOSC:
		switch c {
		case '\a':
			t.st = vtGround
		case 0x1b:
			t.st = vtOSCEsc
		}
	case vtOSCEsc:
		// ST (ESC\) ends the OSC; anything else aborts back to ground.
		t.st = vtGround
	}
}

func (t *vtTracker) ground(c byte) {
	switch {
	case c == 0x1b:
		t.st = vtEsc
	case c == '\r':
		t.col = 0
		t.pendingWrap = false
	case c == '\n' || c == '\v' || c == '\f':
		t.pendingWrap = false
		t.rowDown()
	case c == '\b':
		if t.col > 0 {
			t.col--
		}
		t.pendingWrap = false
	case c == '\t':
		t.col = min(t.w-1, (t.col/8+1)*8)
		t.pendingWrap = false
	case c < 0x20 || c == 0x7f:
		// other control characters: no cursor effect
	case c < utf8.RuneSelf:
		t.putch(1)
	default:
		// UTF-8 accumulation: continuation bytes land here too.
		if len(t.partial) >= 4 {
			t.partial = t.partial[:0] // resync on garbage
		}
		t.partial = append(t.partial, c)
		if !utf8.FullRune(t.partial) {
			return
		}
		r, _ := utf8.DecodeRune(t.partial)
		t.putch(overlay.StringWidth(string(r)))
		t.partial = t.partial[:0]
	}
}

// putch advances the cursor by a printable cell width, honoring deferred
// wrap at the right margin.
func (t *vtTracker) putch(width int) {
	if width <= 0 {
		return
	}
	if t.pendingWrap {
		t.col = 0
		t.rowDown()
		t.pendingWrap = false
	}
	if t.w <= 0 {
		return
	}
	if t.col+width > t.w {
		t.col = 0
		t.rowDown()
	}
	t.col += width
	if t.col >= t.w {
		t.col = t.w - 1
		t.pendingWrap = true
	}
}

func (t *vtTracker) rowDown() {
	t.row++
	if t.h > 0 && t.row > t.h-1 {
		t.row = t.h - 1 // bottom margin scroll: cursor stays, content moves
	}
}

func (t *vtTracker) esc(c byte) {
	t.st = vtGround
	switch c {
	case '[':
		t.params = t.params[:0]
		t.st = vtCSI
	case ']':
		t.st = vtOSC
	case '7':
		t.svRow, t.svCol = t.row, t.col
	case '8':
		t.row, t.col = t.svRow, t.svCol
		t.clamp()
	case 'M': // reverse index
		t.row--
		if t.row < 0 {
			t.row = 0
		}
	case 'D': // index
		t.rowDown()
	case 'E':
		t.rowDown()
		t.col = 0
	case 'c': // full reset
		t.row, t.col = 0, 0
		t.pendingWrap = false
	case '(', ')', '#', '%', '*', '+':
		t.st = vtEscSkip
	}
}

func (t *vtTracker) csi(c byte) {
	if c < 0x40 || c > 0x7e {
		t.params = append(t.params, c)
		return
	}
	t.st = vtGround
	n, m := t.twoParams()
	switch c {
	case 'A':
		t.row -= n
	case 'B':
		t.row += n
	case 'C':
		t.col += n
	case 'D':
		t.col -= n
	case 'E':
		t.row += n
		t.col = 0
	case 'F':
		t.row -= n
		t.col = 0
	case 'G', '`':
		t.col = n - 1
	case 'H', 'f':
		t.row, t.col = n-1, m-1
	case 'd':
		t.row = n - 1
	case 's':
		t.svRow, t.svCol = t.row, t.col
	case 'u':
		t.row, t.col = t.svRow, t.svCol
	case 'h', 'l':
		if hasParam(t.params, 1049) || hasParam(t.params, 1047) || hasParam(t.params, 47) {
			t.row, t.col = 0, 0
			t.pendingWrap = false
		}
	}
	t.clamp()
}

// twoParams parses the first two numeric parameters (default 1 each).
func (t *vtTracker) twoParams() (int, int) {
	vals := [2]int{1, 1}
	idx := 0
	cur := 0
	has := false
	for _, b := range t.params {
		if b >= '0' && b <= '9' {
			cur = cur*10 + int(b-'0')
			has = true
			continue
		}
		if has {
			if idx < 2 {
				vals[idx] = cur
			}
			idx++
		}
		cur, has = 0, false
		if idx >= 2 {
			break
		}
	}
	if has && idx < 2 {
		vals[idx] = cur
	}
	return vals[0], vals[1]
}

func hasParam(params []byte, want int) bool {
	n, m := 0, 0
	for _, b := range params {
		if b >= '0' && b <= '9' {
			n = n*10 + int(b-'0')
			m = 1
			continue
		}
		if m == 1 && n == want {
			return true
		}
		n, m = 0, 0
	}
	return m == 1 && n == want
}
