package ui

import (
	"fmt"
	"io"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/rselbach/argmax/internal/complete"
)

const preferredWidth = 76

// Options are the live-reloadable rendering settings.
type Options struct {
	Classic    bool
	NerdFonts  bool
	GhostText  bool
	MaxHeight  int
	MaxWidth   int
	Palette    Palette
	FooterHint string
}

// Renderer serializes shell output and overlay drawing onto one writer so
// menu content never garbles shell output and never lands in scrollback as
// committed output. It draws relative to the shell cursor position
// obtained from cursor-position reports.
type Renderer struct {
	mu  sync.Mutex
	out io.Writer

	width, height int
	row, col      int // last reported shell cursor position, 1-based
	havePos       bool

	drawnLines int
	ghostWidth int
	ghostRow   int
	ghostCol   int

	outSeq atomic.Uint64

	opts Options
}

// NewRenderer returns a renderer writing to out.
func NewRenderer(out io.Writer, opts Options) *Renderer {
	return &Renderer{out: out, opts: opts, width: 80, height: 24}
}

// SetOptions applies reloaded UI settings.
func (r *Renderer) SetOptions(opts Options) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.opts = opts
}

// Write forwards shell output unchanged, serialized with overlay drawing.
// The sequence increment happens under the same lock as the write so its
// ordering relative to RequestCursor matches the byte order the terminal
// sees.
func (r *Renderer) Write(p []byte) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.outSeq.Add(1)
	return r.out.Write(p)
}

// OutputSeq counts shell-output writes. A cursor report is trustworthy
// only when no shell output raced the position query; callers compare the
// sequence at query time against the sequence at response time.
func (r *Renderer) OutputSeq() uint64 { return r.outSeq.Load() }

// SetSize records the terminal dimensions.
func (r *Renderer) SetSize(width, height int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if width > 0 && height > 0 {
		r.width, r.height = width, height
	}
}

// SetCursor records the shell cursor position from a CPR response.
func (r *Renderer) SetCursor(row, col int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.row, r.col = row, col
	r.havePos = true
}

// RequestCursor emits a Device Status Report query and returns the shell
// output sequence at the moment the query was written; the input pump
// intercepts the response and calls SetCursor. When OutputSeq differs
// from the returned value at response time, shell output raced the query
// and the reported position may be stale.
func (r *Renderer) RequestCursor() uint64 {
	r.mu.Lock()
	defer r.mu.Unlock()
	_, _ = fmt.Fprint(r.out, "\x1b[6n")
	return r.outSeq.Load()
}

// Notice draws one dim informational line below the prompt, such as an
// update notification. It participates in normal overlay clearing.
func (r *Renderer) Notice(text string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.havePos || r.row+1 > r.height {
		return
	}
	text = TruncateToWidth(text, r.width-1)
	_, _ = fmt.Fprintf(r.out, "\x1b7\x1b[%d;1H\x1b[2K%s%s%s\x1b8",
		r.row+1, fg(r.opts.Palette.Muted), text, reset)
	if r.drawnLines < 1 {
		r.drawnLines = 1
	}
}

// Clear removes the menu and ghost text from the screen.
func (r *Renderer) Clear() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.clearLocked()
}

// AcceptGhost forgets the drawn ghost cells without erasing them: the
// shell has echoed the accepted suffix into those exact cells, so they now
// hold real typed text.
func (r *Renderer) AcceptGhost() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.ghostWidth = 0
}

// ShrinkGhost advances the remembered ghost region by cells: a character
// echoed at end-of-line lands on the ghost's first cell, which then holds
// real typed text that a later Clear must not blank.
func (r *Renderer) ShrinkGhost(cells int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.ghostWidth == 0 {
		return
	}
	if cells >= r.ghostWidth {
		r.ghostWidth = 0
		return
	}
	r.ghostCol += cells
	r.ghostWidth -= cells
}

func (r *Renderer) clearLocked() {
	if r.drawnLines == 0 && r.ghostWidth == 0 {
		return
	}
	var b strings.Builder
	b.WriteString("\x1b7") // save cursor
	for i := 1; i <= r.drawnLines; i++ {
		fmt.Fprintf(&b, "\x1b[%d;1H\x1b[2K", r.row+i)
	}
	if r.ghostWidth > 0 {
		fmt.Fprintf(&b, "\x1b[%d;%dH%s", r.ghostRow, r.ghostCol, strings.Repeat(" ", r.ghostWidth))
	}
	b.WriteString("\x1b8") // restore cursor
	_, _ = fmt.Fprint(r.out, b.String())
	r.drawnLines = 0
	r.ghostWidth = 0
}

// Render draws the menu below the prompt and the ghost suffix at the
// cursor, pre-allocating vertical space near the bottom of the terminal
// so the prompt is not scrolled away.
func (r *Renderer) Render(items []complete.Candidate, selected, scroll int, query, ghost string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.havePos {
		return
	}
	if !r.opts.GhostText {
		ghost = ""
	}
	lines := r.menuLines(items, selected, scroll, query)
	var b strings.Builder

	// Pre-allocate vertical space by scrolling before saving the cursor.
	if needed := r.row + len(lines) - (r.height - 0); needed > 0 && len(lines) > 0 {
		if needed > r.row-1 {
			needed = r.row - 1
		}
		fmt.Fprintf(&b, "\x1b[%d;1H", r.height)
		b.WriteString(strings.Repeat("\n", needed))
		r.row -= needed
		if r.ghostRow > 0 {
			r.ghostRow -= needed
		}
		fmt.Fprintf(&b, "\x1b[%d;%dH", r.row, r.col)
	}

	b.WriteString("\x1b7")
	// Stale ghost cells on the current line are covered by the erase-to-
	// end-of-line below; cells left on another row (line wrap) need an
	// explicit wipe.
	if r.ghostWidth > 0 && r.ghostRow != r.row {
		fmt.Fprintf(&b, "\x1b[%d;%dH\x1b[0K", r.ghostRow, r.ghostCol)
	}
	for i, line := range lines {
		fmt.Fprintf(&b, "\x1b[%d;1H\x1b[2K%s", r.row+1+i, line)
	}
	for i := len(lines); i < r.drawnLines; i++ {
		fmt.Fprintf(&b, "\x1b[%d;1H\x1b[2K", r.row+1+i)
	}
	// Ghost text: erase to end of line, then draw the dim suffix.
	fmt.Fprintf(&b, "\x1b[%d;%dH\x1b[0K", r.row, r.col)
	ghost = TruncateToWidth(ghost, r.width-r.col)
	if ghost != "" {
		fmt.Fprintf(&b, "%s%s%s", fg(r.opts.Palette.Ghost), ghost, reset)
	}
	b.WriteString("\x1b8")
	_, _ = fmt.Fprint(r.out, b.String())

	r.drawnLines = len(lines)
	r.ghostWidth = VisibleWidth(ghost)
	r.ghostRow, r.ghostCol = r.row, r.col
}

// menuLines formats the visible candidate window in the active style.
func (r *Renderer) menuLines(items []complete.Candidate, selected, scroll int, query string) []string {
	if len(items) == 0 {
		return nil
	}
	maxH := r.opts.MaxHeight
	if maxH <= 0 {
		maxH = 15
	}
	// Reserve rows for the prompt and one footer line.
	if avail := r.height - 2; maxH > avail {
		maxH = avail
	}
	if maxH < 1 {
		maxH = 1
	}
	window := len(items)
	if window > maxH {
		window = maxH
	}
	width := r.opts.MaxWidth
	if width <= 0 {
		width = preferredWidth
	}
	if width > r.width-1 {
		width = r.width - 1
	}
	if width < 10 {
		width = r.width
	}
	// Place the menu after the prompt and typed query when space permits;
	// shift left rather than overflow.
	indent := r.col - 1 - VisibleWidth(query)
	if indent < 0 {
		indent = 0
	}
	if indent+width > r.width {
		indent = r.width - width
		if indent < 0 {
			indent = 0
		}
	}
	pad := strings.Repeat(" ", indent)

	if scroll > len(items)-window {
		scroll = len(items) - window
	}
	if scroll < 0 {
		scroll = 0
	}
	p := r.opts.Palette
	var lines []string
	for i := scroll; i < scroll+window; i++ {
		lines = append(lines, pad+r.itemLine(items[i], i == selected, query, width))
	}
	counter := ""
	if len(items) > window {
		cur := selected + 1
		if selected < 0 {
			cur = scroll + 1
		}
		counter = fmt.Sprintf("%d/%d", cur, len(items))
	}
	switch {
	case r.opts.Classic:
		if counter != "" {
			gap := (width - VisibleWidth(counter)) / 2
			if gap < 0 {
				gap = 0
			}
			lines = append(lines, pad+strings.Repeat(" ", gap)+fg(p.Border)+counter+reset)
		}
	default:
		footer := r.opts.FooterHint
		if counter != "" {
			footer = counter + "  " + footer
		}
		if footer != "" {
			lines = append(lines, pad+fg(p.Muted)+TruncateToWidth(footer, width)+reset)
		}
	}
	return lines
}

// itemLine renders one candidate row.
func (r *Renderer) itemLine(c complete.Candidate, selected bool, query string, width int) string {
	p := r.opts.Palette
	var b strings.Builder
	textColor := fg(p.Primary)
	if selected {
		b.WriteString(bg(p.SelectedBG))
		textColor = fg(p.Selected)
		b.WriteString(fg(p.Accent) + "❯ " + textColor)
	} else {
		b.WriteString("  ")
	}
	used := 2
	if !r.opts.Classic {
		if g := icon(c, r.opts.NerdFonts); g != "" {
			b.WriteString(fg(p.Muted) + g + " " + textColor)
			used += 2
		}
	}
	title := highlightPrefix(c.Title, lastWord(query), p, selected)
	titleWidth := VisibleWidth(c.Title)
	maxTitle := width - used - 1
	if titleWidth > maxTitle {
		title = TruncateToWidth(c.Title, maxTitle)
		titleWidth = VisibleWidth(title)
	}
	b.WriteString(textColor + title)
	used += titleWidth

	pill := ""
	if !r.opts.Classic {
		pill = sourceLabel(c.Source)
	}
	rest := width - used
	var tail strings.Builder
	if pill != "" && rest > len(pill)+4 {
		tail.WriteString(" " + fg(p.Border) + "[" + pill + "]" + reset)
		rest -= len(pill) + 3
		if selected {
			tail.WriteString(bg(p.SelectedBG))
		}
	}
	if c.Description != "" && rest > 6 {
		desc := TruncateToWidth(c.Description, rest-2)
		tail.WriteString(" " + fg(p.Description) + desc)
	}
	b.WriteString(tail.String())
	if selected {
		// Extend the selection background across the row.
		if fill := width - used - VisibleWidth(tail.String()); fill > 0 {
			b.WriteString(strings.Repeat(" ", fill))
		}
	}
	b.WriteString(reset)
	return b.String()
}

// highlightPrefix colors the case-insensitive typed prefix of title.
func highlightPrefix(title, typed string, p Palette, selected bool) string {
	if typed == "" || len(typed) > len(title) || !strings.EqualFold(title[:len(typed)], typed) {
		return title
	}
	restore := fg(p.Primary)
	if selected {
		restore = fg(p.Selected)
	}
	return fg(p.Accent) + title[:len(typed)] + restore + title[len(typed):]
}

func lastWord(query string) string {
	fields := strings.Fields(query)
	if len(fields) == 0 || strings.HasSuffix(query, " ") {
		return ""
	}
	return fields[len(fields)-1]
}
