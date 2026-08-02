package overlay

import (
	"io"
	"strconv"
	"strings"

	"github.com/rselbach/argmax/internal/core"
)

const ansiReset = "\x1b[0m"

// preferredWidth is the responsive default panel width (UI-005).
const preferredWidth = 76

// RenderOpts describes geometry and style for one frame.
type RenderOpts struct {
	TermWidth  int // terminal width in cells
	TermHeight int // terminal height in rows
	// StartCol is the preferred left edge column (0-based) — where the
	// prompt query ends.
	StartCol int
	// LinesBelow is the number of free rows below the cursor before the
	// terminal bottom.
	LinesBelow int
	Style      string // "modern" | "classic"
	NerdFonts  bool
	// MaxHeight is the configured cap in rows, including borders and footer;
	// <= 0 means unlimited.
	MaxHeight int
	// MaxWidth is the configured panel width cap; 0 uses the responsive
	// preferred width of 76.
	MaxWidth int
	// Query is the typed text highlighted in titles (case-insensitive
	// prefix).
	Query  string
	Footer []FooterHint // key hints shown in the modern style footer
	// Selected is the selected index into Items (-1 = none).
	Selected int
}

// FooterHint is a key hint shown in the modern style footer,
// e.g. {"tab", "insert"} or {"ctrl+r", "history"}.
type FooterHint struct {
	Key, Action string
}

// Frame describes what RenderMenu drew, so ClearMenu can erase it.
type Frame struct {
	// Lines is the number of terminal rows occupied below the cursor row.
	Lines int
}

// menuStyle holds the box-drawing characters and behavior switches of a
// built-in style (UI-015).
type menuStyle struct {
	tl, tr, bl, br, side, fill string
	classic                    bool
}

// renderer carries per-frame rendering state.
type renderer struct {
	pal    Palette
	st     menuStyle
	nerd   bool
	query  string
	innerW int
}

// RenderMenu writes the menu starting one row below the cursor and returns
// the cursor to its original position (DEC save/restore: ESC 7 / ESC 8).
// Each row write is "\r\n" + erase-line + content, so menu rows never touch
// scrollback content (UI-001). Empty items produce no output and Frame{0}.
func RenderMenu(w io.Writer, items []core.Suggestion, opts RenderOpts) Frame {
	if len(items) == 0 || opts.TermWidth < 2 {
		return Frame{}
	}
	st := menuStyle{tl: "╭", tr: "╮", bl: "╰", br: "╯", side: "│", fill: "─"}
	if opts.Style == "classic" {
		st = menuStyle{tl: "┌", tr: "┐", bl: "└", br: "┘", side: "│", fill: "─", classic: true}
	}

	// Width: prefer StartCol placement; shift left and shrink rather than
	// overflow (UI-004). The hard ceiling is TermWidth.
	panelW := opts.MaxWidth
	if panelW <= 0 {
		panelW = preferredWidth
	}
	panelW = min(panelW, opts.TermWidth)
	col := max(opts.StartCol, 0)
	if col+panelW > opts.TermWidth {
		col = opts.TermWidth - panelW
	}
	innerW := panelW - 2

	// Height: min(MaxHeight, LinesBelow, needed). The menu is drawn one row
	// below the cursor, so it may occupy at most LinesBelow rows; drawing
	// one more would scroll the terminal mid-frame and corrupt the prompt
	// (UI-002). The session pre-allocates scroll space before calling.
	maxRows := opts.MaxHeight
	if maxRows <= 0 {
		maxRows = int(^uint(0) >> 1)
	}
	maxRows = min(maxRows, opts.LinesBelow)
	if maxRows <= 0 {
		return Frame{}
	}

	// Footer: modern shows key hints; both styles need a footer row for the
	// counter when the item window scrolls (UI-007/009).
	footerRows := 0
	if !st.classic && len(opts.Footer) > 0 {
		footerRows = 1
	}
	vis := len(items)
	windowed := false
	if 2+footerRows+len(items) > maxRows {
		windowed = true
		footerRows = 1
		vis = max(maxRows-2-footerRows, 0)
	}

	sel := min(opts.Selected, len(items)-1)
	start := 0
	if windowed && sel > 0 {
		// Keep the selection in view, centered when possible.
		start = clamp(sel-vis/2, 0, len(items)-vis)
	}
	counter := ""
	if windowed {
		counter = strconv.Itoa(max(sel+1, 1)) + "/" + strconv.Itoa(len(items))
	}

	r := &renderer{
		pal:    DefaultPalette(),
		st:     st,
		nerd:   opts.NerdFonts,
		query:  opts.Query,
		innerW: innerW,
	}
	rows := make([]string, 0, 2+vis+footerRows)
	rows = append(rows, r.border(st.tl, st.tr))
	for i := start; i < start+vis; i++ {
		rows = append(rows, r.itemRow(items[i], i == sel))
	}
	if footerRows == 1 {
		rows = append(rows, r.footerRow(opts.Footer, counter))
	}
	rows = append(rows, r.border(st.bl, st.br))
	if len(rows) > maxRows {
		rows = rows[:maxRows]
	}

	var b strings.Builder
	b.WriteString("\x1b7") // DEC save cursor
	pad := strings.Repeat(" ", col)
	for _, row := range rows {
		b.WriteString("\r\n\x1b[2K")
		b.WriteString(pad)
		b.WriteString(row)
	}
	b.WriteString("\x1b8") // DEC restore cursor
	_, _ = io.WriteString(w, b.String())
	return Frame{Lines: len(rows)}
}

// ClearMenu erases a previously rendered frame non-destructively: save the
// cursor, erase each occupied row below, restore the cursor. It never emits
// ESC[0J, which would clobber content below the menu.
func ClearMenu(w io.Writer, f Frame) {
	if f.Lines <= 0 {
		return
	}
	var b strings.Builder
	b.WriteString("\x1b7")
	for i := 0; i < f.Lines; i++ {
		b.WriteString("\r\n\x1b[2K")
	}
	b.WriteString("\x1b8")
	_, _ = io.WriteString(w, b.String())
}

// border builds a horizontal border row: corner + fill + corner.
func (r *renderer) border(l, rt string) string {
	return r.pal.Border + l + strings.Repeat(r.st.fill, r.innerW) + rt + ansiReset
}

// wrap pads/truncates a styled row core to the inner width and frames it
// with the side borders. The trailing reset is appended after truncation so
// colors can never bleed into the border or the next row.
func (r *renderer) wrap(core string) string {
	core = padRight(truncateANSI(core, r.innerW), r.innerW)
	return r.pal.Border + r.st.side + core + ansiReset + r.pal.Border + r.st.side + ansiReset
}

// itemRow builds one candidate row:
//
//	[selection marker] [icon] title [source pill] ... description
//
// The description is truncated first when space runs out, then the title
// (UI-005). The selected row uses both a marker and color (UI-006).
func (r *renderer) itemRow(it core.Suggestion, selected bool) string {
	title := oneLine(it.Text)
	desc := oneLine(it.Description)

	var b strings.Builder
	if selected && !r.st.classic {
		b.WriteString(r.pal.SelectedBg)
	}
	marker := " "
	if selected {
		marker = "▸"
		if r.st.classic {
			marker = ">"
		}
	}
	b.WriteByte(' ')
	if selected {
		b.WriteString(r.pal.Accent)
	}
	b.WriteString(marker)
	b.WriteByte(' ')
	used := 3

	if !r.st.classic && r.nerd {
		if g := IconFor(it.Icon, true); g != "" {
			b.WriteString(r.pal.Muted)
			b.WriteString(g)
			b.WriteByte(' ')
			used += StringWidth(g) + 1
		}
	}

	pill := ""
	if !r.st.classic {
		pill = SourceLabel(it.Source)
	}
	pillW := 0
	if pill != "" {
		pillW = StringWidth(pill) + 3 // leading space + surrounding brackets
	}

	avail := r.innerW - used
	if pillW > 0 && avail-pillW < 6 {
		// Too tight for a useful title: drop the pill first.
		pill, pillW = "", 0
	}
	avail -= pillW

	tw := StringWidth(title)
	dw := 0
	if desc != "" {
		dw = StringWidth(desc) + 1 // leading space
	}
	if tw+dw > avail && desc != "" {
		if dAvail := avail - tw - 1; dAvail < 4 {
			desc, dw = "", 0
		} else {
			desc = Truncate(desc, dAvail)
			dw = StringWidth(desc) + 1
		}
	}
	if tw+dw > avail {
		title = Truncate(title, avail-dw)
	}

	titleColor := r.pal.Primary
	if selected {
		titleColor = r.pal.Selected
	}
	if n := prefixFoldLen(title, r.query); n > 0 {
		b.WriteString(r.pal.Accent)
		b.WriteString(title[:n])
		b.WriteString(titleColor)
		b.WriteString(title[n:])
	} else {
		b.WriteString(titleColor)
		b.WriteString(title)
	}

	if pill != "" {
		b.WriteByte(' ')
		b.WriteString(r.pal.Border)
		b.WriteByte('[')
		b.WriteString(r.pal.Muted)
		b.WriteString(pill)
		b.WriteString(r.pal.Border)
		b.WriteByte(']')
	}
	if desc != "" {
		b.WriteByte(' ')
		b.WriteString(r.pal.Description)
		b.WriteString(desc)
	}
	return r.wrap(b.String())
}

// footerRow builds the footer. Modern style shows key hints left-aligned
// and the scroll counter right-aligned (UI-007/008); classic style drops the
// hints and centers the counter (UI-009).
func (r *renderer) footerRow(hints []FooterHint, counter string) string {
	if r.st.classic {
		return r.wrap(center(r.pal.Border+counter, r.innerW))
	}

	var hb strings.Builder
	for i, h := range hints {
		if i > 0 {
			hb.WriteString("  ")
		}
		hb.WriteString(r.pal.Accent)
		hb.WriteString(oneLine(h.Key))
		hb.WriteByte(' ')
		hb.WriteString(r.pal.Muted)
		hb.WriteString(oneLine(h.Action))
	}
	left := " " + hb.String()
	right := ""
	if counter != "" {
		right = r.pal.Border + counter + " "
	}
	lw, rw := StringWidth(left), StringWidth(right)
	var core string
	switch {
	case lw+rw <= r.innerW:
		core = left + strings.Repeat(" ", r.innerW-lw-rw) + right
	case rw+1 <= r.innerW:
		// Hints do not fit: keep just the counter right-aligned.
		core = strings.Repeat(" ", r.innerW-rw) + right
	default:
		core = left + " " + right // wrap() truncates
	}
	return r.wrap(core)
}

// center pads a styled string with spaces so it is centered in width cells.
func center(s string, width int) string {
	w := StringWidth(s)
	if w >= width {
		return s
	}
	left := (width - w) / 2
	return strings.Repeat(" ", left) + s + strings.Repeat(" ", width-w-left)
}

var lineSanitizer = strings.NewReplacer("\n", " ", "\r", " ", "\v", " ", "\f", " ")

// oneLine flattens embedded newlines so a candidate can never break the
// frame's row structure.
func oneLine(s string) string { return lineSanitizer.Replace(s) }

func clamp(v, lo, hi int) int {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}
