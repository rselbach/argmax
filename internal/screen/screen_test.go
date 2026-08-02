package screen

import (
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"
)

func testSize(t *testing.T, columns, rows uint16) TerminalSize {
	t.Helper()
	size, err := NewTerminalSize(columns, rows)
	if err != nil {
		t.Fatal(err)
	}
	return size
}

func newTestObserver(t *testing.T, columns, rows uint16) *ScreenObserver {
	t.Helper()
	observer, err := New(testSize(t, columns, rows))
	if err != nil {
		t.Fatal(err)
	}
	return observer
}

func rowTexts(t *testing.T, observer *ScreenObserver) []string {
	t.Helper()
	rows := observer.Snapshot().Size().Rows()
	texts := make([]string, rows)
	for row := uint16(0); row < rows; row++ {
		text, ok := observer.RowText(row)
		if !ok {
			t.Fatalf("row %d absent", row)
		}
		texts[row] = text
	}
	return texts
}

func assertSameScreen(t *testing.T, got, want *ScreenObserver, context string) {
	t.Helper()
	if got.Snapshot() != want.Snapshot() {
		t.Fatalf("%s snapshot\n got: %#v\nwant: %#v", context, got.Snapshot(), want.Snapshot())
	}
	gotRows, wantRows := rowTexts(t, got), rowTexts(t, want)
	if !slices.Equal(gotRows, wantRows) {
		t.Fatalf("%s rows\n got: %q\nwant: %q", context, gotRows, wantRows)
	}
}

func assertPartitionInvariant(t *testing.T, data []byte, columns, rows uint16) *ScreenObserver {
	t.Helper()
	whole := newTestObserver(t, columns, rows)
	whole.Observe(data)
	for split := 0; split <= len(data); split++ {
		partitioned := newTestObserver(t, columns, rows)
		partitioned.Observe(data[:split])
		partitioned.Observe(data[split:])
		assertSameScreen(t, partitioned, whole, fmt.Sprintf("split %d", split))
	}
	bytewise := newTestObserver(t, columns, rows)
	for _, value := range data {
		bytewise.Observe([]byte{value})
	}
	assertSameScreen(t, bytewise, whole, "bytewise")
	return whole
}

func TestTerminalSizeAndSynchronizationValidation(t *testing.T) {
	t.Parallel()
	tests := map[string]struct {
		columns uint16
		rows    uint16
	}{
		"zero columns":   {0, 24},
		"zero rows":      {80, 0},
		"too wide":       {MaxTerminalDimension + 1, 1},
		"too many cells": {MaxTerminalDimension, MaxTerminalDimension},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := NewTerminalSize(tc.columns, tc.rows)
			var screenErr ScreenError
			if !errors.As(err, &screenErr) || screenErr.Kind() != InvalidSize {
				t.Fatalf("error = %#v", err)
			}
		})
	}

	observer := newTestObserver(t, 10, 3)
	before := observer.Snapshot()
	err := observer.Synchronize(NewCursorPosition(3, 0))
	var screenErr ScreenError
	if !errors.As(err, &screenErr) || screenErr.Kind() != InvalidCursor {
		t.Fatalf("error = %#v", err)
	}
	if observer.Snapshot() != before {
		t.Fatal("invalid synchronization changed state")
	}
}

func TestUnknownCursorSuppressesUntilSynchronized(t *testing.T) {
	t.Parallel()
	observer := newTestObserver(t, 80, 24)
	observer.Desynchronize()
	if observer.Snapshot().OverlaySafe() {
		t.Fatal("desynchronized observer is overlay-safe")
	}
	cursor := NewCursorPosition(6, 32)
	if err := observer.Synchronize(cursor); err != nil {
		t.Fatal(err)
	}
	if !observer.Snapshot().OverlaySafe() || observer.Snapshot().Cursor() != cursor {
		t.Fatalf("snapshot = %#v", observer.Snapshot())
	}
}

func TestControlsWideCombiningAndChunks(t *testing.T) {
	t.Parallel()
	observer := assertPartitionInvariant(t, []byte("ab\t界e\u0301\rZ\nnext\b!"), 12, 4)
	if got, want := observer.Snapshot().Cursor(), NewCursorPosition(1, 5); got != want {
		t.Fatalf("cursor = %#v, want %#v", got, want)
	}
	rows := rowTexts(t, observer)
	if rows[0] != "Zb      界e\u0301" || rows[1] != " nex!" {
		t.Fatalf("rows = %q", rows)
	}
}

func TestEscapeUTF8AndSavedCursorPartitionInvariant(t *testing.T) {
	t.Parallel()
	data := []byte("\x1b[2J\x1b[2;3H界e\u0301\x1b7\x1b]0;Greendale\x1b\\\x1b[4;10HX\x1b8!")
	observer := assertPartitionInvariant(t, data, 16, 5)
	if text, _ := observer.RowText(1); text != "  界e\u0301!" {
		t.Fatalf("row 1 = %q", text)
	}
	if text, _ := observer.RowText(3); text != "         X" {
		t.Fatalf("row 3 = %q", text)
	}
	if !observer.Snapshot().OverlaySafe() {
		t.Fatalf("snapshot = %#v", observer.Snapshot())
	}
}

func TestIncompleteUTF8SuppressesUntilComplete(t *testing.T) {
	t.Parallel()
	data := []byte("界")
	observer := newTestObserver(t, 10, 3)
	for _, value := range data[:2] {
		observer.Observe([]byte{value})
		if observer.Snapshot().OverlaySafe() {
			t.Fatal("incomplete UTF-8 is overlay-safe")
		}
	}
	observer.Observe(data[2:])
	if !observer.Snapshot().OverlaySafe() || observer.Snapshot().Cursor() != NewCursorPosition(0, 2) {
		t.Fatalf("snapshot = %#v", observer.Snapshot())
	}
}

func TestInterruptedAndInvalidUTF8(t *testing.T) {
	t.Parallel()
	tests := map[string]struct {
		data        []byte
		wantRows    []string
		wantCursor  CursorPosition
		overlaySafe bool
	}{
		"bell":      {[]byte{'\xc3', '\a', 'X'}, []string{"�X"}, NewCursorPosition(0, 2), true},
		"linefeed":  {[]byte{'\xc3', '\n', 'X'}, []string{"�", " X"}, NewCursorPosition(1, 2), true},
		"CSI":       {[]byte("\xc3\x1b[2CX"), []string{"�  X"}, NewCursorPosition(0, 4), true},
		"C1 NEL":    {[]byte{'\xe0', '\x85', 'X'}, []string{"�X"}, NewCursorPosition(0, 2), false},
		"OSC":       {[]byte("\xc3\x1b]0;Greendale\x07X"), []string{"�X"}, NewCursorPosition(0, 2), true},
		"reset":     {[]byte("\xf0\x9f\x92\x1bcX"), []string{"X"}, NewCursorPosition(0, 1), true},
		"malformed": {[]byte("\xf0(\x8c(\x1bcA"), []string{"A"}, NewCursorPosition(0, 1), true},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			observer := assertPartitionInvariant(t, tc.data, 12, 4)
			rows := rowTexts(t, observer)
			for index, want := range tc.wantRows {
				if rows[index] != want {
					t.Fatalf("row %d = %q, want %q", index, rows[index], want)
				}
			}
			if observer.Snapshot().Cursor() != tc.wantCursor || observer.Snapshot().OverlaySafe() != tc.overlaySafe {
				t.Fatalf("snapshot = %#v", observer.Snapshot())
			}
		})
	}
}

func TestRawC1SequencesDesynchronizePlacement(t *testing.T) {
	t.Parallel()
	tests := map[string][]byte{
		"CSI": {0x9b, '2', 'C'},
		"OSC": {0x9d, '0', ';', 'G', 'r', 'e', 'e', 'n', 'd', 'a', 'l', 'e', 0x9c},
	}
	for name, data := range tests {
		t.Run(name, func(t *testing.T) {
			observer := assertPartitionInvariant(t, data, 20, 5)
			if observer.Snapshot().OverlaySafe() {
				t.Fatalf("raw C1 snapshot = %#v", observer.Snapshot())
			}
			observer.Observe([]byte("\x1bc"))
			if !observer.Snapshot().OverlaySafe() {
				t.Fatalf("reset snapshot = %#v", observer.Snapshot())
			}
		})
	}
}

func TestUTF8EncodedC1ControlsArePartitionInvariant(t *testing.T) {
	t.Parallel()
	tests := map[string][]byte{
		"NEL": {0xc2, 0x85},
		"OSC": {0xc2, 0x9d},
	}
	for name, data := range tests {
		t.Run(name, func(t *testing.T) {
			observer := assertPartitionInvariant(t, data, 20, 5)
			if observer.Snapshot().OverlaySafe() || observer.Snapshot().Cursor() != NewCursorPosition(0, 0) {
				t.Fatalf("snapshot = %#v", observer.Snapshot())
			}
		})
	}
}

func TestRandomValidPartitions(t *testing.T) {
	t.Parallel()
	data := []byte("éa\u0301 👨‍❤️‍💋‍👨\x1b[2;3H1️⃣ 🇨🇦")
	whole := newTestObserver(t, 30, 4)
	whole.Observe(data)
	seed := uint64(0x475245454e44414c)
	for partition := 0; partition < 512; partition++ {
		chunked := newTestObserver(t, 30, 4)
		for start := 0; start < len(data); {
			seed = seed*6364136223846793005 + 1
			end := min(start+int((seed>>32)%7+1), len(data))
			chunked.Observe(data[start:end])
			start = end
		}
		assertSameScreen(t, chunked, whole, fmt.Sprintf("partition %d", partition))
	}
}

func TestObservationMetadataAndInvalidResizeAreAtomic(t *testing.T) {
	t.Parallel()
	observer := newTestObserver(t, 10, 3)
	empty := observer.Observe(nil)
	if empty.ConsumedBytes() != 0 || empty.ClearOverlay() || !empty.OverlaySafe() {
		t.Fatalf("empty observation = %#v", empty)
	}
	output := observer.Observe([]byte("Greendale"))
	if output.ConsumedBytes() != 9 || !output.ClearOverlay() || !output.OverlaySafe() {
		t.Fatalf("output observation = %#v", output)
	}
	before := observer.Snapshot()
	_, err := observer.Resize(TerminalSize{})
	var screenErr ScreenError
	if !errors.As(err, &screenErr) || screenErr.Kind() != InvalidSize || observer.Snapshot() != before {
		t.Fatalf("resize error = %#v, snapshot = %#v", err, observer.Snapshot())
	}
}

func TestEmojiGraphemesAndPendingBoundaries(t *testing.T) {
	t.Parallel()
	tests := map[string]struct {
		text    string
		width   uint16
		pending bool
	}{
		"kiss":         {"👨‍❤️‍💋‍👨", 2, false},
		"technologist": {"👩‍💻", 2, false},
		"modifier":     {"👋🏽", 2, true},
		"heart":        {"❤️", 2, true},
		"keycap":       {"1️⃣", 2, false},
		"flag":         {"🇨🇦", 2, false},
		"combining":    {"a\u0301", 1, false},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			observer := assertPartitionInvariant(t, []byte(tc.text), 20, 3)
			text, _ := observer.RowText(0)
			width, _ := observer.RowWidth(0)
			if text != tc.text || width != int(tc.width) || observer.Snapshot().Cursor().Column() != tc.width {
				t.Fatalf("text %q width %d snapshot %#v", text, width, observer.Snapshot())
			}
			if observer.Snapshot().OverlaySafe() == tc.pending {
				t.Fatalf("pending %t snapshot %#v", tc.pending, observer.Snapshot())
			}
			if tc.pending {
				observer.Observe([]byte{'\a'})
				if !observer.Snapshot().OverlaySafe() {
					t.Fatal("proven grapheme boundary remained unsafe")
				}
			}
		})
	}
}

func TestIncompleteGraphemeRecovery(t *testing.T) {
	t.Parallel()
	joiner := newTestObserver(t, 20, 3)
	joiner.Observe([]byte("👨‍"))
	if joiner.Snapshot().OverlaySafe() {
		t.Fatal("joiner tail is overlay-safe")
	}
	joiner.Observe([]byte("💻"))
	if text, _ := joiner.RowText(0); text != "👨‍💻" || !joiner.Snapshot().OverlaySafe() {
		t.Fatalf("joiner text %q snapshot %#v", text, joiner.Snapshot())
	}

	provisional := newTestObserver(t, 20, 3)
	provisional.Observe([]byte("👨‍❤"))
	if provisional.Snapshot().OverlaySafe() {
		t.Fatal("provisional width is overlay-safe")
	}
	provisional.Observe([]byte("️‍💋‍👨"))
	if text, _ := provisional.RowText(0); text != "👨‍❤️‍💋‍👨" || !provisional.Snapshot().OverlaySafe() {
		t.Fatalf("provisional text %q snapshot %#v", text, provisional.Snapshot())
	}

	resized := newTestObserver(t, 20, 3)
	resized.Observe([]byte("👨‍"))
	observation, err := resized.Resize(testSize(t, 24, 4))
	if err != nil {
		t.Fatal(err)
	}
	if !observation.ClearOverlay() || resized.Snapshot().OverlaySafe() {
		t.Fatalf("resize observation %#v snapshot %#v", observation, resized.Snapshot())
	}
	resized.Observe([]byte("💻"))
	if !resized.Snapshot().OverlaySafe() {
		t.Fatalf("completed resize snapshot %#v", resized.Snapshot())
	}
}

func TestEditingModesAndDelayedWrapAreUnsafe(t *testing.T) {
	t.Parallel()
	origin := newTestObserver(t, 20, 6)
	origin.Observe([]byte("\x1b[3;6r\x1b[?6h"))
	if !origin.Snapshot().OriginMode() || origin.Snapshot().OverlaySafe() {
		t.Fatalf("origin snapshot %#v", origin.Snapshot())
	}
	origin.Observe([]byte("\x1b[?6l\x1b[r"))
	if !origin.Snapshot().OverlaySafe() {
		t.Fatalf("restored origin snapshot %#v", origin.Snapshot())
	}

	insert := newTestObserver(t, 20, 6)
	insert.Observe([]byte("\x1b[4h"))
	if !insert.Snapshot().InsertMode() || insert.Snapshot().OverlaySafe() {
		t.Fatalf("insert snapshot %#v", insert.Snapshot())
	}
	insert.Observe([]byte("\x1b[4l"))
	if !insert.Snapshot().OverlaySafe() {
		t.Fatalf("restored insert snapshot %#v", insert.Snapshot())
	}

	pending := newTestObserver(t, 5, 3)
	pending.Observe([]byte("12345"))
	if !pending.Snapshot().WrapPending() || pending.Snapshot().OverlaySafe() {
		t.Fatalf("pending snapshot %#v", pending.Snapshot())
	}
}

func TestSameCursorSynchronizationPreservesDelayedWrap(t *testing.T) {
	t.Parallel()
	observer := newTestObserver(t, 8, 3)
	observer.Observe([]byte("12345678"))
	cursor := observer.Snapshot().Cursor()
	if err := observer.Synchronize(cursor); err != nil {
		t.Fatal(err)
	}
	if !observer.Snapshot().WrapPending() || observer.Snapshot().OverlaySafe() {
		t.Fatalf("snapshot = %#v", observer.Snapshot())
	}
	observer.Observe([]byte("X"))
	rows := rowTexts(t, observer)
	if rows[0] != "12345678" || rows[1] != "X" || observer.Snapshot().Cursor() != NewCursorPosition(1, 1) {
		t.Fatalf("rows %q snapshot %#v", rows, observer.Snapshot())
	}
}

func TestPromptPlacementAndProvenClearSpace(t *testing.T) {
	t.Parallel()
	observer := newTestObserver(t, 30, 5)
	observer.Observe([]byte("\x1b[31mgreendale>\x1b[0m \x1b]0;coursework\x07git\x1b7\x1b[1;25HRP\x1b8"))
	snapshot := observer.Snapshot()
	text, _ := observer.RowText(0)
	if !snapshot.OverlaySafe() || snapshot.Cursor() != NewCursorPosition(0, 14) ||
		snapshot.BlankCellsToRight() != 10 || !snapshot.RowsBelowClear() ||
		text != "greendale> git          RP" {
		t.Fatalf("text %q snapshot %#v", text, snapshot)
	}

	occupied := newTestObserver(t, 30, 5)
	occupied.Observe([]byte("> git\x1b7\x1b[1;20HRP\x1b[3;1Hold output\x1b8"))
	snapshot = occupied.Snapshot()
	if snapshot.BlankCellsToRight() != 14 || snapshot.RowsBelowClear() {
		t.Fatalf("occupied snapshot %#v", snapshot)
	}
}

func TestScrollResizeAlternateAndSynchronizedOutput(t *testing.T) {
	t.Parallel()
	scrolled := newTestObserver(t, 8, 4)
	scrolled.Observe([]byte("one\r\ntwo\r\nthree\r\nfour"))
	scrolled.Observe([]byte("\x1b[2;4r\x1b[4;1H\n"))
	rows := rowTexts(t, scrolled)
	if rows[0] != "one" || rows[1] != "three" || rows[2] != "four" {
		t.Fatalf("rows = %q", rows)
	}
	observation, err := scrolled.Resize(testSize(t, 5, 3))
	if err != nil || !observation.ClearOverlay() || scrolled.Snapshot().Size() != testSize(t, 5, 3) {
		t.Fatalf("resize = %#v, %v", observation, err)
	}
	if width, _ := scrolled.RowWidth(2); width > 5 {
		t.Fatalf("row width = %d", width)
	}

	alternate := newTestObserver(t, 20, 5)
	alternate.Observe([]byte("prompt> "))
	alternate.Observe([]byte("\x1b[?1049hTUI\x1b[?25l"))
	if alternate.Snapshot().Buffer() != Alternate || alternate.Snapshot().OverlaySafe() {
		t.Fatalf("alternate snapshot %#v", alternate.Snapshot())
	}
	alternate.Observe([]byte("\x1b[?25h\x1b[?1049l"))
	text, _ := alternate.RowText(0)
	if alternate.Snapshot().Buffer() != Primary || !alternate.Snapshot().OverlaySafe() || text != "prompt>" {
		t.Fatalf("primary text %q snapshot %#v", text, alternate.Snapshot())
	}
	alternate.Observe([]byte("\x1b[?2026hbatched"))
	if alternate.Snapshot().OverlaySafe() {
		t.Fatal("synchronized output is overlay-safe")
	}
	alternate.Observe([]byte("\x1b[?2026l"))
	if !alternate.Snapshot().OverlaySafe() {
		t.Fatalf("closed synchronized output snapshot %#v", alternate.Snapshot())
	}
}

func TestUnsupportedAndOversizedSequencesFailSafeUntilReset(t *testing.T) {
	t.Parallel()
	observer := newTestObserver(t, 20, 5)
	unsafeSequences := map[string][]byte{
		"unknown CSI":    []byte("\x1b[999z"),
		"window command": []byte("\x1b[8;80;120t"),
		"private string": []byte("\x1b_private\x1b\\"),
	}
	for name, sequence := range unsafeSequences {
		t.Run(name, func(t *testing.T) {
			observer.Observe(sequence)
			if observer.Snapshot().OverlaySafe() {
				t.Fatal("unsupported sequence is overlay-safe")
			}
			observer.Observe([]byte("\x1bc"))
			if !observer.Snapshot().OverlaySafe() {
				t.Fatalf("reset snapshot %#v", observer.Snapshot())
			}
		})
	}

	huge := append([]byte("\x1b]0;"), []byte(strings.Repeat("x", MaxControlStringBytes+24))...)
	observer.Observe(huge)
	if observer.Snapshot().OverlaySafe() {
		t.Fatal("oversized OSC is overlay-safe")
	}
	observer.Observe([]byte("\x07\x1bc"))
	if !observer.Snapshot().OverlaySafe() {
		t.Fatalf("OSC recovery snapshot %#v", observer.Snapshot())
	}

	combining := "a" + strings.Repeat("\u0301", MaxCellTextBytes)
	observer.Observe([]byte(combining))
	if observer.Snapshot().OverlaySafe() || len(observer.machine.surface().cell(0, 0).text) > MaxCellTextBytes {
		t.Fatalf("combining snapshot %#v", observer.Snapshot())
	}
}

func TestSynchronizationClearsDepartedChildState(t *testing.T) {
	t.Parallel()
	observer := newTestObserver(t, 80, 24)
	observer.Observe([]byte("\x1b[?2026h"))
	if observer.Snapshot().OverlaySafe() {
		t.Fatal("open synchronized output is overlay-safe")
	}
	if err := observer.Synchronize(NewCursorPosition(0, 0)); err != nil {
		t.Fatal(err)
	}
	if !observer.Snapshot().OverlaySafe() {
		t.Fatalf("snapshot = %#v", observer.Snapshot())
	}
}

func TestDebugNeverIncludesScreenContents(t *testing.T) {
	t.Parallel()
	observer := newTestObserver(t, 20, 5)
	observer.Observe([]byte("hunter2"))
	debug := fmt.Sprintf("%s %#v", observer, observer)
	if strings.Contains(debug, "hunter2") || !strings.Contains(debug, "nonempty") {
		t.Fatalf("debug = %s", debug)
	}
}
