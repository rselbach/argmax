package overlay

import (
	"io"
	"strconv"

	"github.com/rselbach/argmax/internal/core"
)

// GhostSuffix returns the untyped suffix of sel.Text given the typed buffer
// (UI-012): only when the buffer is non-empty and a case-insensitive prefix
// of sel.Text. ok is false otherwise, including when nothing remains untyped.
func GhostSuffix(buffer string, sel core.Suggestion) (suffix string, ok bool) {
	if buffer == "" {
		return "", false
	}
	n := prefixFoldLen(sel.Text, buffer)
	if n == 0 || n >= len(sel.Text) {
		return "", false
	}
	return sel.Text[n:], true
}

// RenderGhost writes the ghost suffix at the cursor in Palette.Ghost,
// truncated to availWidth cells with an ellipsis when needed (UI-013), then
// returns the cursor to its original position. Empty suffix or non-positive
// width produces no output.
func RenderGhost(w io.Writer, suffix string, availWidth int) {
	if suffix == "" || availWidth <= 0 {
		return
	}
	s := suffix
	if StringWidth(s) > availWidth {
		s = Truncate(s, availWidth)
	}
	n := StringWidth(s)
	if n <= 0 {
		return
	}
	_, _ = io.WriteString(w, DefaultPalette().Ghost+s+ansiReset+cursorBack(n))
}

// ClearGhost erases any ghost cells from the cursor to the end of the line
// (UI-013).
func ClearGhost(w io.Writer) {
	_, _ = io.WriteString(w, "\x1b[0K")
}

// cursorBack moves the cursor left by n cells.
func cursorBack(n int) string {
	return "\x1b[" + strconv.Itoa(n) + "D"
}
