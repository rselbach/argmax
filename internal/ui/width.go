// Package ui renders the inline suggestion menu and ghost text with
// non-destructive ANSI sequences, terminal-cell width measurement, and the
// modern and classic styles.
package ui

import (
	"strings"

	"github.com/mattn/go-runewidth"
)

// VisibleWidth measures s in terminal cells, ignoring CSI/OSC/control
// sequences and counting wide Unicode characters as two cells.
func VisibleWidth(s string) int {
	width := 0
	runes := []rune(s)
	for i := 0; i < len(runes); i++ {
		r := runes[i]
		if r == 0x1b && i+1 < len(runes) {
			i = skipEscape(runes, i)
			continue
		}
		if r < 0x20 || r == 0x7f {
			continue
		}
		width += runewidth.RuneWidth(r)
	}
	return width
}

// skipEscape returns the index of the final rune of an escape sequence
// starting at runes[i] (which is ESC).
func skipEscape(runes []rune, i int) int {
	next := runes[i+1]
	switch next {
	case '[': // CSI: parameters then a final byte in @-~
		for j := i + 2; j < len(runes); j++ {
			if runes[j] >= 0x40 && runes[j] <= 0x7e {
				return j
			}
		}
		return len(runes) - 1
	case ']': // OSC: terminated by BEL or ST
		for j := i + 2; j < len(runes); j++ {
			if runes[j] == 0x07 {
				return j
			}
			if runes[j] == 0x1b && j+1 < len(runes) && runes[j+1] == '\\' {
				return j + 1
			}
		}
		return len(runes) - 1
	default:
		return i + 1
	}
}

// TruncateToWidth cuts s to at most cells terminal columns, appending an
// ellipsis when content was removed.
func TruncateToWidth(s string, cells int) string {
	if cells <= 0 {
		return ""
	}
	if VisibleWidth(s) <= cells {
		return s
	}
	var b strings.Builder
	width := 0
	for _, r := range s {
		rw := runewidth.RuneWidth(r)
		if width+rw > cells-1 {
			break
		}
		b.WriteRune(r)
		width += rw
	}
	return b.String() + "…"
}

// PadToWidth right-pads s with spaces to exactly cells columns,
// truncating first when too wide.
func PadToWidth(s string, cells int) string {
	s = TruncateToWidth(s, cells)
	if pad := cells - VisibleWidth(s); pad > 0 {
		return s + strings.Repeat(" ", pad)
	}
	return s
}
