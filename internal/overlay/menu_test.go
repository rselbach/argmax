package overlay

import (
	"bytes"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/core"
)

func render(t *testing.T, items []core.Suggestion, opts RenderOpts) (string, Frame) {
	t.Helper()
	var buf bytes.Buffer
	f := RenderMenu(&buf, items, opts)
	return buf.String(), f
}

// menuLines splits rendered output into its printable rows (leading column
// padding included), asserting the cursor save/restore symmetry (UI-001).
func menuLines(t *testing.T, out string) []string {
	t.Helper()
	if !strings.HasPrefix(out, "\x1b7") {
		t.Fatalf("output does not start with ESC 7 (cursor save): %q", out)
	}
	if !strings.HasSuffix(out, "\x1b8") {
		t.Fatalf("output does not end with ESC 8 (cursor restore): %q", out)
	}
	parts := strings.Split(out, "\r\n")
	lines := make([]string, 0, len(parts)-1)
	for _, p := range parts[1:] {
		if !strings.HasPrefix(p, "\x1b[2K") {
			t.Fatalf("row missing erase-line prefix: %q", p)
		}
		lines = append(lines, StripANSI(p))
	}
	return lines
}

func assertLineWidths(t *testing.T, lines []string, width int) {
	t.Helper()
	for i, l := range lines {
		if w := StringWidth(l); w != width {
			t.Errorf("line %d width = %d, want %d: %q", i, w, width, l)
		}
	}
}

func basicItems() []core.Suggestion {
	return []core.Suggestion{
		{Text: "git status", Description: "show working tree", Icon: "git", Source: core.SourceSpec},
		{Text: "git stash", Description: "stash changes", Icon: "git", Source: core.SourceSpec},
		{Text: "gcloud", Description: "Google Cloud CLI", Icon: "cloud", Source: core.SourceSystem},
	}
}

func basicOpts() RenderOpts {
	return RenderOpts{
		TermWidth:  80,
		TermHeight: 24,
		StartCol:   0,
		LinesBelow: 20,
		Style:      "modern",
		NerdFonts:  true,
		Query:      "gi",
		Footer:     []FooterHint{{Key: "tab", Action: "insert"}, {Key: "ctrl+r", Action: "history"}},
		Selected:   1,
	}
}

func TestRenderMenuModernBasics(t *testing.T) {
	pal := DefaultPalette()
	out, f := render(t, basicItems(), basicOpts())

	if f.Lines != 6 { // top border + 3 items + footer + bottom border
		t.Errorf("Frame.Lines = %d, want 6", f.Lines)
	}
	for _, ch := range []string{"╭", "╮", "╰", "╯", "│", "─"} {
		if !strings.Contains(out, ch) {
			t.Errorf("output missing border char %q", ch)
		}
	}
	if !strings.Contains(out, "▸") {
		t.Error("output missing selection marker ▸")
	}
	if !strings.Contains(out, pal.SelectedBg) {
		t.Error("output missing selected background SGR")
	}
	if !strings.Contains(out, pal.Accent+"gi") {
		t.Error("output missing accent-highlighted query prefix")
	}
	lines := menuLines(t, out)
	if len(lines) != 6 {
		t.Fatalf("got %d lines, want 6", len(lines))
	}
	assertLineWidths(t, lines, preferredWidth)
	plain := strings.Join(lines, "\n")
	if !strings.Contains(plain, "[spec]") || !strings.Contains(plain, "[system]") {
		t.Errorf("output missing source pills: %q", plain)
	}
}

func TestRenderMenuWindowedCounter(t *testing.T) {
	items := make([]core.Suggestion, 10)
	for i := range items {
		items[i] = core.Suggestion{Text: "cmd" + string(rune('0'+i)), Source: core.SourceSystem}
	}
	opts := basicOpts()
	opts.MaxHeight = 7
	opts.Selected = 1

	out, f := render(t, items, opts)
	if f.Lines != 7 {
		t.Errorf("Frame.Lines = %d, want 7 (MaxHeight cap)", f.Lines)
	}
	if !strings.Contains(out, "2/10") {
		t.Error("windowed output missing position counter 2/10")
	}
	if !strings.Contains(out, "cmd1") {
		t.Error("selected item scrolled out of view")
	}
	if strings.Contains(out, "cmd5") {
		t.Error("item outside the window was rendered")
	}

	opts.Selected = 8
	out, _ = render(t, items, opts)
	if !strings.Contains(out, "9/10") {
		t.Error("windowed output missing position counter 9/10")
	}
	if !strings.Contains(out, "cmd8") {
		t.Error("selected item 8 not kept in view")
	}
	if strings.Contains(out, "cmd0") {
		t.Error("item 0 rendered although the window scrolled past it")
	}
}

func TestRenderMenuClassic(t *testing.T) {
	items := make([]core.Suggestion, 10)
	for i := range items {
		items[i] = core.Suggestion{Text: "cmd" + string(rune('0'+i)), Description: "desc", Icon: "git", Source: core.SourceSystem}
	}
	opts := basicOpts()
	opts.Style = "classic"
	opts.MaxHeight = 5
	opts.Selected = 0

	out, f := render(t, items, opts)
	if f.Lines != 5 { // top + 2 items + counter + bottom
		t.Errorf("Frame.Lines = %d, want 5", f.Lines)
	}
	for _, ch := range []string{"┌", "┐", "└", "┘"} {
		if !strings.Contains(out, ch) {
			t.Errorf("classic output missing border char %q", ch)
		}
	}
	if strings.Contains(out, "▸") || !strings.Contains(out, ">") {
		t.Error("classic selection marker should be '>', not '▸'")
	}
	if strings.Contains(out, IconFor("git", true)) {
		t.Error("classic output must not contain icons (UI-009)")
	}
	if strings.Contains(out, "insert") || strings.Contains(out, "ctrl+r") {
		t.Error("classic output must not contain footer hints (UI-009)")
	}

	// The counter is centered inside the panel (UI-009).
	lines := menuLines(t, out)
	plain := strings.Join(lines, "\n")
	if strings.Contains(plain, "[system]") {
		t.Error("classic output must not contain source pills")
	}
	var counterLine string
	for _, l := range lines {
		if strings.Contains(l, "1/10") {
			counterLine = l
		}
	}
	if counterLine == "" {
		t.Fatal("classic windowed output missing centered counter")
	}
	inner := preferredWidth - 2
	wantIdx := len("│") + (inner-len("1/10"))/2 // left border (3 bytes) + centered fill
	if idx := strings.Index(counterLine, "1/10"); idx != wantIdx {
		t.Errorf("counter starts at byte %d, want %d (centered): %q", idx, wantIdx, counterLine)
	}
}

func TestRenderMenuPlacementShiftLeft(t *testing.T) {
	items := basicItems()
	opts := basicOpts()
	opts.Footer = nil
	opts.MaxWidth = 40

	// Fits at StartCol: 30 + 40 <= 80.
	opts.StartCol = 30
	out, _ := render(t, items, opts)
	lines := menuLines(t, out)
	assertLineWidths(t, lines, 70)
	if !strings.HasPrefix(lines[0], strings.Repeat(" ", 30)) {
		t.Errorf("menu not placed at StartCol 30: %q", lines[0])
	}

	// Does not fit: shift left so the panel stays inside the terminal
	// (UI-004).
	opts.TermWidth = 50
	out, _ = render(t, items, opts)
	lines = menuLines(t, out)
	assertLineWidths(t, lines, 50)
	if !strings.HasPrefix(lines[0], strings.Repeat(" ", 10)) {
		t.Errorf("menu not shifted left to column 10: %q", lines[0])
	}
}

func TestRenderMenuWidthCaps(t *testing.T) {
	items := basicItems()
	opts := basicOpts()
	opts.Footer = nil

	// MaxWidth caps the panel.
	opts.MaxWidth = 30
	out, _ := render(t, items, opts)
	assertLineWidths(t, menuLines(t, out), 30)

	// A narrow terminal shrinks the panel to TermWidth (UI-005).
	opts.MaxWidth = 0
	opts.TermWidth = 30
	out, _ = render(t, items, opts)
	assertLineWidths(t, menuLines(t, out), 30)
}

func TestRenderMenuNarrowDegrades(t *testing.T) {
	items := []core.Suggestion{
		{Text: "kubectl get pods --all-namespaces", Description: "list everything", Icon: "kubernetes", Source: core.SourceSystem},
	}
	opts := basicOpts()
	opts.Footer = nil
	opts.Selected = 0
	opts.Query = ""

	// Description is truncated away first (UI-005).
	opts.TermWidth = 24
	out, _ := render(t, items, opts)
	if strings.Contains(out, "list everything") {
		t.Error("description should be dropped at width 24")
	}
	if !strings.Contains(out, "…") {
		t.Error("long title should be truncated with an ellipsis")
	}
	assertLineWidths(t, menuLines(t, out), 24)

	// Very narrow: pill dropped too, nothing written beyond TermWidth.
	opts.TermWidth = 14
	out, _ = render(t, items, opts)
	lines := menuLines(t, out)
	if strings.Contains(strings.Join(lines, "\n"), "[system]") {
		t.Error("pill should be dropped at width 14")
	}
	assertLineWidths(t, lines, 14)
}

func TestRenderMenuNerdFontsFalse(t *testing.T) {
	items := []core.Suggestion{
		{Text: "gs", Description: "git status alias", Icon: "git", Source: core.SourceAlias},
	}
	opts := basicOpts()
	opts.NerdFonts = false

	out, _ := render(t, items, opts)
	if strings.Contains(out, IconFor("git", true)) {
		t.Error("NerdFonts=false output contains a glyph (UI-010)")
	}
	if !strings.Contains(out, "▸") {
		t.Error("NerdFonts=false must keep the selection marker (UI-016)")
	}
	lines := menuLines(t, out)
	if !strings.Contains(strings.Join(lines, "\n"), "[alias]") {
		t.Error("NerdFonts=false must keep source labels (UI-016)")
	}
	assertLineWidths(t, lines, preferredWidth)
}

func TestRenderMenuEmptyItems(t *testing.T) {
	out, f := render(t, nil, basicOpts())
	if out != "" {
		t.Errorf("empty items produced output: %q", out)
	}
	if f != (Frame{Lines: 0}) {
		t.Errorf("empty items frame = %+v, want Frame{0}", f)
	}
}

func TestRenderMenuLinesBelowCap(t *testing.T) {
	opts := basicOpts()
	opts.Footer = nil
	opts.LinesBelow = 3 // caps total rows at 3
	opts.Selected = 0

	out, f := render(t, basicItems(), opts)
	if f.Lines != 3 {
		t.Errorf("Frame.Lines = %d, want 3 (LinesBelow cap)", f.Lines)
	}
	if !strings.Contains(out, "1/3") {
		t.Error("capped frame should window and show the counter")
	}
}

func TestClearMenu(t *testing.T) {
	var buf bytes.Buffer
	ClearMenu(&buf, Frame{Lines: 3})
	want := "\x1b7" + strings.Repeat("\r\n\x1b[2K", 3) + "\x1b8"
	if buf.String() != want {
		t.Errorf("ClearMenu = %q, want %q", buf.String(), want)
	}

	buf.Reset()
	ClearMenu(&buf, Frame{Lines: 0})
	if buf.String() != "" {
		t.Errorf("ClearMenu(Frame{0}) = %q, want empty", buf.String())
	}
}

// Regression: at the bottom of the terminal the menu must never draw more
// rows than exist below the cursor, or the final row-write scrolls the
// screen mid-frame and detaches the prompt (UI-002).
func TestRenderMenuNeverExceedsLinesBelow(t *testing.T) {
	items := make([]core.Suggestion, 100)
	for i := range items {
		items[i] = core.Suggestion{Text: "item", Source: core.SourceSystem}
	}
	for _, below := range []int{0, 1, 2, 5, 13} {
		opts := basicOpts()
		opts.LinesBelow = below
		opts.MaxHeight = 15
		opts.Selected = 0
		_, f := render(t, items, opts)
		if f.Lines > below {
			t.Fatalf("LinesBelow=%d: frame drew %d rows (would scroll)", below, f.Lines)
		}
	}
	// Zero free rows means no output at all.
	opts := basicOpts()
	opts.LinesBelow = 0
	out, f := render(t, items, opts)
	if f.Lines != 0 || out != "" {
		t.Fatalf("LinesBelow=0 must render nothing, got %d rows", f.Lines)
	}
}
