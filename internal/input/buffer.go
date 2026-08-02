// Package input models the prompt buffer and decodes terminal input into
// key events, including CSI sequences, bracketed paste, cursor-position
// reports, and the Kitty keyboard protocol.
package input

import "strings"

// Buffer is a Unicode-safe model of the current prompt line and cursor.
type Buffer struct {
	runes  []rune
	cursor int // rune index
	// version increments on every content or cursor change so renderers
	// can detect stale state.
	version uint64
}

// Version returns the mutation counter.
func (b *Buffer) Version() uint64 { return b.version }

// String returns the buffer content.
func (b *Buffer) String() string { return string(b.runes) }

// Len returns the rune count.
func (b *Buffer) Len() int { return len(b.runes) }

// Cursor returns the rune index of the cursor.
func (b *Buffer) Cursor() int { return b.cursor }

// AtEnd reports whether the cursor sits at end-of-line.
func (b *Buffer) AtEnd() bool { return b.cursor == len(b.runes) }

// Set replaces the content and places the cursor at the end.
func (b *Buffer) Set(s string) {
	b.version++
	b.runes = []rune(s)
	b.cursor = len(b.runes)
}

// SetWithCursor replaces the content and cursor (clamped).
func (b *Buffer) SetWithCursor(s string, cursor int) {
	b.version++
	b.runes = []rune(s)
	b.cursor = clampInt(cursor, 0, len(b.runes))
}

// Insert adds a rune at the cursor.
func (b *Buffer) Insert(r rune) {
	b.version++
	b.runes = append(b.runes[:b.cursor], append([]rune{r}, b.runes[b.cursor:]...)...)
	b.cursor++
}

// InsertString adds a string at the cursor.
func (b *Buffer) InsertString(s string) {
	for _, r := range s {
		b.Insert(r)
	}
}

// Backspace removes the rune before the cursor.
func (b *Buffer) Backspace() {
	b.version++
	if b.cursor == 0 {
		return
	}
	b.runes = append(b.runes[:b.cursor-1], b.runes[b.cursor:]...)
	b.cursor--
}

// DeleteWordBack removes the word before the cursor.
func (b *Buffer) DeleteWordBack() {
	b.version++
	i := b.cursor
	for i > 0 && b.runes[i-1] == ' ' {
		i--
	}
	for i > 0 && b.runes[i-1] != ' ' {
		i--
	}
	b.runes = append(b.runes[:i], b.runes[b.cursor:]...)
	b.cursor = i
}

// Clear empties the buffer.
func (b *Buffer) Clear() {
	b.version++
	b.runes = b.runes[:0]
	b.cursor = 0
}

// MoveLeft moves the cursor one rune left.
func (b *Buffer) MoveLeft() {
	if b.cursor > 0 {
		b.cursor--
		b.version++
	}
}

// MoveRight moves the cursor one rune right.
func (b *Buffer) MoveRight() {
	if b.cursor < len(b.runes) {
		b.cursor++
		b.version++
	}
}

// MoveHome places the cursor at the beginning of the line.
func (b *Buffer) MoveHome() { b.cursor = 0; b.version++ }

// MoveEnd places the cursor at the end of the line.
func (b *Buffer) MoveEnd() { b.cursor = len(b.runes); b.version++ }

// Empty reports an empty buffer.
func (b *Buffer) Empty() bool { return len(b.runes) == 0 }

// IsBlank reports a buffer containing only whitespace.
func (b *Buffer) IsBlank() bool { return strings.TrimSpace(string(b.runes)) == "" }

func clampInt(v, lo, hi int) int {
	switch {
	case v < lo:
		return lo
	case v > hi:
		return hi
	}
	return v
}
