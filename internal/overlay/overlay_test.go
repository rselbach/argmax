package overlay

import (
	"bytes"
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/rselbach/argmax/internal/completion"
	"github.com/rselbach/argmax/internal/screen"
	"github.com/rselbach/argmax/internal/selection"
)

func testQuery(t *testing.T, line string) completion.CompletionQuery {
	t.Helper()
	query, err := completion.NewQuery(line, len(line), []byte("/tmp/Greendale"), 7)
	if err != nil {
		t.Fatal(err)
	}
	return query
}

func testSuggestion(t *testing.T, display, description, icon string) completion.Suggestion {
	t.Helper()
	edit, err := completion.NewTextEdit(0, 3, display)
	if err != nil {
		t.Fatal(err)
	}
	candidate, err := completion.NewSuggestion(
		edit, display, description, icon, completion.SourceSpec,
		completion.InsertionExact, display,
	)
	if err != nil {
		t.Fatal(err)
	}
	return candidate
}

func testSelection(candidates []completion.Suggestion, selected int) *selection.SelectionState {
	state := selection.New()
	state.BeginQuery(7, candidates)
	for range selected {
		state.Down()
	}
	return state
}

func testOptions(style Style) Options {
	return Options{
		Style: style, NerdFonts: false, GhostText: true,
		MaxHeight: 4, MinUsableHeight: 3,
	}
}

func testScreen(t *testing.T, columns, rows uint16, output []byte) *screen.ScreenObserver {
	t.Helper()
	size, err := screen.NewTerminalSize(columns, rows)
	if err != nil {
		t.Fatal(err)
	}
	observer, err := screen.New(size)
	if err != nil {
		t.Fatal(err)
	}
	observer.Observe(output)
	return observer
}

func screenRows(t *testing.T, observer *screen.ScreenObserver) []string {
	t.Helper()
	rows := make([]string, observer.Snapshot().Size().Rows())
	for row := range rows {
		text, ok := observer.RowText(uint16(row))
		if !ok {
			t.Fatalf("row %d absent", row)
		}
		rows[row] = text
	}
	return rows
}

func applyFrame(t *testing.T, renderer *Renderer, observer *screen.ScreenObserver, frame Frame) {
	t.Helper()
	transaction := frame.Transaction()
	observer.Observe(transaction.Bytes())
	if err := renderer.Acknowledge(transaction); err != nil {
		t.Fatal(err)
	}
}

func requireErrorKind(t *testing.T, err error, kind ErrorKind) *Error {
	t.Helper()
	var overlayErr *Error
	if !errors.As(err, &overlayErr) || overlayErr.Kind() != kind {
		t.Fatalf("error = %#v, want kind %d", err, kind)
	}
	return overlayErr
}

func TestBottomRightDrawSuppressesWithoutScrollingOrModeChanges(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 24, 5, []byte("\x1b[5;24H"))
	before := observer.Snapshot().Cursor()
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
		testSuggestion(t, "git stash", "save changes", "git"),
	}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	transaction := frame.Transaction()
	if len(transaction.Bytes()) > MaxRenderBytes || !transaction.Empty() {
		t.Fatalf("transaction = %s", transaction)
	}
	observer.Observe(transaction.Bytes())
	if observer.Snapshot().Cursor() != before || !observer.Snapshot().Wrapping() {
		t.Fatalf("snapshot = %#v", observer.Snapshot())
	}
	if _, ok := renderer.OwnedRegion(); ok {
		t.Fatal("renderer owns a region")
	}
}

func TestMultilineWideCombiningPromptKeepsCellAnchor(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 32, 8, []byte("Greendale 界\u0301\r\n> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "状态", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	observer.Observe(frame.Transaction().Bytes())
	if got, want := observer.Snapshot().Cursor(), screen.NewCursorPosition(1, 5); got != want {
		t.Fatalf("cursor = %#v, want %#v", got, want)
	}
	row, _ := observer.RowText(2)
	if !strings.Contains(row, "> [spec] git status") {
		t.Fatalf("row = %q", row)
	}
}

func TestRightPromptAndOccupiedLowerRowsAreNeverOverwritten(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 6, []byte("> git\x1b7\x1b[1;20HRP\x1b[3;1Hold output\x1b8"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git checkout a-very-long-branch", "", "git"),
	}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, renderer, observer, frame)
	owned, ok := renderer.OwnedRegion()
	if !ok {
		t.Fatal("renderer owns no region")
	}
	for _, span := range owned.Spans() {
		if span.Kind != SpanGhost {
			t.Fatalf("owned span = %#v", span)
		}
	}
	ghost, ok := frame.Ghost()
	if !ok || !ghost.Clipped() {
		t.Fatalf("ghost = %s, %t", ghost, ok)
	}
	row0, _ := observer.RowText(0)
	row2, _ := observer.RowText(2)
	if !strings.HasSuffix(row0, "RP") || row2 != "old output" {
		t.Fatalf("rows = %q", screenRows(t, observer))
	}
}

func TestRenderedTransactionIsChunkPartitionInvariant(t *testing.T) {
	t.Parallel()
	base := []byte("Greendale 界\u0301\r\n> git")
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	source := testScreen(t, 32, 6, base)
	renderer := NewRenderer()
	frame, err := renderer.Render(source.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	data := frame.Transaction().Bytes()
	whole := testScreen(t, 32, 6, base)
	whole.Observe(data)
	wantSnapshot, wantRows := whole.Snapshot(), screenRows(t, whole)
	for split := 0; split <= len(data); split++ {
		chunked := testScreen(t, 32, 6, base)
		chunked.Observe(data[:split])
		chunked.Observe(data[split:])
		if chunked.Snapshot() != wantSnapshot || !slices.Equal(screenRows(t, chunked), wantRows) {
			t.Fatalf("split %d differs", split)
		}
	}
}

func TestClearErasesOwnedCellsAndAllowsLaterMenu(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 6, []byte("\x1b[3;1H> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, renderer, observer, frame)
	clearFrame, err := renderer.Clear(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, renderer, observer, clearFrame)
	if !observer.Snapshot().RowsBelowClear() {
		t.Fatal("rows below cursor remain occupied")
	}
	redraw, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	if redraw.Transaction().Kind() != TransactionDraw {
		t.Fatalf("redraw = %s", redraw)
	}
	applyFrame(t, renderer, observer, redraw)
	owned, ok := renderer.OwnedRegion()
	if !ok {
		t.Fatal("renderer owns no region")
	}
	if !slices.ContainsFunc(owned.Spans(), func(span OwnedSpan) bool { return span.Kind == SpanMenu }) {
		t.Fatalf("owned = %s", owned)
	}
}

func TestResizeAndShorterGhostClearEveryOldCell(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 40, 8, []byte("> git"))
	query := testQuery(t, "git")
	renderer := NewRenderer()
	long := testSelection([]completion.Suggestion{testSuggestion(t, "git checkout feature", "", "git")}, 0)
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, long), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	longGhost, ok := frame.Ghost()
	if !ok {
		t.Fatal("long ghost absent")
	}
	applyFrame(t, renderer, observer, frame)
	short := testSelection([]completion.Suggestion{testSuggestion(t, "git st", "", "git")}, 0)
	frame, err = renderer.Render(observer.Snapshot(), NewRequest(query, short), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	shortGhost, ok := frame.Ghost()
	if !ok || shortGhost.Cells() >= longGhost.Cells() {
		t.Fatalf("ghost = %s", shortGhost)
	}
	applyFrame(t, renderer, observer, frame)
	row, _ := observer.RowText(0)
	if strings.Contains(row, "checkout") {
		t.Fatalf("row = %q", row)
	}
	resized, err := screen.NewTerminalSize(18, 5)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := observer.Resize(resized); err != nil {
		t.Fatal(err)
	}
	clearFrame, err := renderer.OnResize(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, renderer, observer, clearFrame)
	if _, ok := renderer.OwnedRegion(); ok || len(clearFrame.Transaction().Bytes()) > MaxRenderBytes {
		t.Fatalf("renderer = %s, clear = %s", renderer, clearFrame)
	}
	observer.Observe([]byte("\x1b[2;1HUSER-CONTENT"))
	beforeFailure, _ := observer.RowText(1)
	failure, err := renderer.OnFailure(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	if !failure.Transaction().Empty() {
		t.Fatalf("failure = %s", failure)
	}
	applyFrame(t, renderer, observer, failure)
	after, _ := observer.RowText(1)
	if after != beforeFailure {
		t.Fatalf("row after failure = %q, want %q", after, beforeFailure)
	}
}

func TestAlternateScreenAndTinyTerminalSuppressMenu(t *testing.T) {
	t.Parallel()
	alternate := testScreen(t, 24, 5, []byte("\x1b[?1049h"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(alternate.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	if frame.Transaction().Kind() != TransactionSuppressed || !frame.Transaction().Empty() {
		t.Fatalf("alternate frame = %s", frame)
	}
	alternate.Observe([]byte("\x1b[?1049l"))
	tiny := testScreen(t, 8, 2, []byte("> git"))
	frame, err = renderer.Render(tiny.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, renderer, tiny, frame)
	owned, ok := renderer.OwnedRegion()
	if !ok {
		t.Fatal("tiny frame owns no region")
	}
	for _, span := range owned.Spans() {
		if span.Kind != SpanGhost {
			t.Fatalf("tiny span = %#v", span)
		}
	}
	if _, ok := frame.Ghost(); !ok {
		t.Fatal("tiny ghost absent")
	}
}

func TestStaleQueryGenerationNeverDrawsCandidates(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 6, []byte("> git"))
	query, err := completion.NewQuery("git", 3, []byte("/tmp/Greendale"), 8)
	if err != nil {
		t.Fatal(err)
	}
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	if frame.Transaction().Kind() == TransactionDraw || !frame.Transaction().Empty() {
		t.Fatalf("frame = %s", frame)
	}
	if _, ok := renderer.OwnedRegion(); ok {
		t.Fatal("stale draw owns region")
	}
}

func TestCandidateControlsAndHostileIconsNeverReachANSIOutput(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 76, 10, []byte("> git"))
	query := testQuery(t, "git")
	edit, err := completion.NewTextEdit(0, 3, "git status")
	if err != nil {
		t.Fatal(err)
	}
	candidate, err := completion.NewSuggestion(
		edit, "git\x1b]52;c;payload\astatus", "line\n\x1b[2Jdescription", "\x1b[31m",
		completion.SourceSpec, completion.InsertionExact, "hostile",
	)
	if err != nil {
		t.Fatal(err)
	}
	state := testSelection([]completion.Suggestion{candidate}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	output := frame.Transaction().Bytes()
	if bytes.Contains(output, []byte("\x1b]52")) || bytes.Contains(output, []byte("\x1b[2J")) ||
		!bytes.Contains(output, []byte("[spec]")) {
		t.Fatalf("unsafe output = %q", output)
	}
}

func TestUnsafeTerminalModesSuppressAllOverlayOutput(t *testing.T) {
	t.Parallel()
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	for _, output := range [][]byte{[]byte("\x1b[3;6r\x1b[?6h"), []byte("\x1b[4h")} {
		observer := testScreen(t, 30, 6, output)
		renderer := NewRenderer()
		frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
		if err != nil {
			t.Fatal(err)
		}
		if frame.Transaction().Kind() != TransactionSuppressed || !frame.Transaction().Empty() {
			t.Fatalf("frame = %s", frame)
		}
		if _, ok := renderer.OwnedRegion(); ok {
			t.Fatal("unsafe frame owns region")
		}
	}
}

func TestIncompleteUTF8AndGraphemeTailsNeverEmitOverlayBytes(t *testing.T) {
	t.Parallel()
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	outputs := [][]byte{
		{'>', ' ', 0xf0, 0x9f},
		[]byte("> 👨‍"), []byte("> ❤️"), []byte("> 👋🏽"), []byte("> 🇨"),
	}
	for _, output := range outputs {
		observer := testScreen(t, 30, 6, output)
		if observer.Snapshot().OverlaySafe() {
			t.Fatalf("output %q is overlay-safe", output)
		}
		renderer := NewRenderer()
		frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
		if err != nil {
			t.Fatal(err)
		}
		if frame.Transaction().Kind() != TransactionSuppressed || !frame.Transaction().Empty() {
			t.Fatalf("frame = %s", frame)
		}
	}
	for _, output := range [][]byte{[]byte("> 👨‍💻"), []byte("> 🇨🇦")} {
		observer := testScreen(t, 30, 6, output)
		if !observer.Snapshot().OverlaySafe() {
			t.Fatalf("output %q is not overlay-safe", output)
		}
		renderer := NewRenderer()
		frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
		if err != nil {
			t.Fatal(err)
		}
		if frame.Transaction().Kind() != TransactionDraw || frame.Transaction().Empty() {
			t.Fatalf("frame = %s", frame)
		}
	}
}

func TestCompleteDrawPreservesChildSavedCursor(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 8, []byte("\x1b[6;12H\x1b7\x1b[1;1H> git"))
	saved := observer.Snapshot().SavedCursor()
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	output := frame.Transaction().Bytes()
	if bytes.Contains(output, []byte("\x1b7")) || bytes.Contains(output, []byte("\x1b8")) {
		t.Fatalf("transaction changes saved cursor: %q", output)
	}
	applyFrame(t, renderer, observer, frame)
	if observer.Snapshot().SavedCursor() != saved {
		t.Fatalf("saved cursor = %#v, want %#v", observer.Snapshot().SavedCursor(), saved)
	}
	observer.Observe([]byte("\x1b8"))
	if observer.Snapshot().Cursor() != saved {
		t.Fatalf("restored cursor = %#v, want %#v", observer.Snapshot().Cursor(), saved)
	}
}

func TestDrawReplayIsIdempotentAndDoesNotScroll(t *testing.T) {
	t.Parallel()
	base := []byte("\x1b[2;1H> git")
	observer := testScreen(t, 30, 8, base)
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	output := frame.Transaction().Bytes()
	if bytes.Contains(output, []byte("\r\n")) {
		t.Fatalf("transaction has newline: %q", output)
	}
	observer.Observe(output)
	once, onceRows := observer.Snapshot(), screenRows(t, observer)
	observer.Observe(output)
	if observer.Snapshot() != once || !slices.Equal(screenRows(t, observer), onceRows) {
		t.Fatal("replayed draw changed screen")
	}
}

func TestReplayRecoversEveryPartialDrawPrefix(t *testing.T) {
	t.Parallel()
	base := []byte("\x1b[2;1H> git")
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	source := testScreen(t, 30, 8, base)
	renderer := NewRenderer()
	frame, err := renderer.Render(source.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	output := frame.Transaction().Bytes()
	whole := testScreen(t, 30, 8, base)
	whole.Observe(output)
	want, wantRows := whole.Snapshot(), screenRows(t, whole)
	for split := 0; split <= len(output); split++ {
		replayed := testScreen(t, 30, 8, base)
		replayed.Observe(output[:split])
		replayed.Observe(output)
		if replayed.Snapshot() != want || !slices.Equal(screenRows(t, replayed), wantRows) {
			t.Fatalf("split %d differs\n got: %#v %q\nwant: %#v %q", split, replayed.Snapshot(), screenRows(t, replayed), want, wantRows)
		}
	}
}

func TestFailureCleanupRecoversEveryPartialDrawPrefix(t *testing.T) {
	t.Parallel()
	base := []byte("\x1b[2;1H> git")
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	source := testScreen(t, 30, 8, base)
	before, beforeRows := source.Snapshot(), screenRows(t, source)
	renderer := NewRenderer()
	frame, err := renderer.Render(source.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	output := frame.Transaction().Bytes()
	for split := 0; split <= len(output); split++ {
		interrupted := testScreen(t, 30, 8, base)
		renderer := NewRenderer()
		if _, err := renderer.Render(interrupted.Snapshot(), NewRequest(query, state), testOptions(StyleModern)); err != nil {
			t.Fatal(err)
		}
		interrupted.Observe(output[:split])
		cleanup, err := renderer.OnFailure(interrupted.Snapshot())
		if err != nil {
			t.Fatal(err)
		}
		if cleanup.Transaction().Kind() != TransactionClear {
			t.Fatalf("cleanup = %s", cleanup)
		}
		interrupted.Observe(cleanup.Transaction().Bytes())
		if interrupted.Snapshot() != before || !slices.Equal(screenRows(t, interrupted), beforeRows) {
			t.Fatalf("split %d differs", split)
		}
	}
}

func TestAcknowledgedShellClearNeverErasesLaterChildCells(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 8, []byte("\x1b[2;1H> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	renderer := NewRenderer()
	draw, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, renderer, observer, draw)
	clearFrame, err := renderer.BeforeShellOutput(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, renderer, observer, clearFrame)
	observer.Observe([]byte("\x1b[3;1HUSER-CONTENT"))
	before, beforeRows := observer.Snapshot(), screenRows(t, observer)
	failure, err := renderer.OnFailure(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	if failure.Transaction().Kind() != TransactionClear || !failure.Transaction().Empty() {
		t.Fatalf("failure = %s", failure)
	}
	applyFrame(t, renderer, observer, failure)
	row, _ := observer.RowText(2)
	if observer.Snapshot() != before || !slices.Equal(screenRows(t, observer), beforeRows) || row != "USER-CONTENT" {
		t.Fatalf("rows = %q", screenRows(t, observer))
	}
}

func TestFailureCleanupRecoversEveryPartialShellClearPrefix(t *testing.T) {
	t.Parallel()
	base := []byte("\x1b[2;1H> git")
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	beforeObserver := testScreen(t, 30, 8, base)
	before, beforeRows := beforeObserver.Snapshot(), screenRows(t, beforeObserver)
	source := testScreen(t, 30, 8, base)
	sourceRenderer := NewRenderer()
	draw, err := sourceRenderer.Render(source.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, sourceRenderer, source, draw)
	clearFrame, err := sourceRenderer.BeforeShellOutput(source.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	clearBytes := clearFrame.Transaction().Bytes()
	for split := 0; split <= len(clearBytes); split++ {
		interrupted := testScreen(t, 30, 8, base)
		renderer := NewRenderer()
		draw, err := renderer.Render(interrupted.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
		if err != nil {
			t.Fatal(err)
		}
		applyFrame(t, renderer, interrupted, draw)
		clearFrame, err := renderer.BeforeShellOutput(interrupted.Snapshot())
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(clearFrame.Transaction().Bytes(), clearBytes) {
			t.Fatalf("split %d clear differs", split)
		}
		interrupted.Observe(clearFrame.Transaction().Bytes()[:split])
		cleanup, err := renderer.OnFailure(interrupted.Snapshot())
		if err != nil {
			t.Fatal(err)
		}
		if cleanup.Transaction().Kind() != TransactionClear {
			t.Fatalf("cleanup = %s", cleanup)
		}
		applyFrame(t, renderer, interrupted, cleanup)
		if interrupted.Snapshot() != before || !slices.Equal(screenRows(t, interrupted), beforeRows) {
			t.Fatalf("split %d differs", split)
		}
	}
}

func TestFailureCleanupRetriesEveryPartialCleanupPrefix(t *testing.T) {
	t.Parallel()
	base := []byte("\x1b[2;1H> git")
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git status", "working tree", "git"),
		testSuggestion(t, "git switch", "change branch", "git"),
	}, 0)
	beforeObserver := testScreen(t, 30, 8, base)
	before, beforeRows := beforeObserver.Snapshot(), screenRows(t, beforeObserver)
	source := testScreen(t, 30, 8, base)
	sourceRenderer := NewRenderer()
	draw, err := sourceRenderer.Render(source.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	drawBytes := draw.Transaction().Bytes()
	drawPrefix := len(drawBytes) / 2
	source.Observe(drawBytes[:drawPrefix])
	cleanup, err := sourceRenderer.OnFailure(source.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	cleanupBytes := cleanup.Transaction().Bytes()
	for split := 0; split <= len(cleanupBytes); split++ {
		interrupted := testScreen(t, 30, 8, base)
		renderer := NewRenderer()
		draw, err := renderer.Render(interrupted.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
		if err != nil {
			t.Fatal(err)
		}
		interrupted.Observe(draw.Transaction().Bytes()[:drawPrefix])
		cleanup, err := renderer.OnFailure(interrupted.Snapshot())
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(cleanup.Transaction().Bytes(), cleanupBytes) {
			t.Fatalf("split %d cleanup differs", split)
		}
		interrupted.Observe(cleanup.Transaction().Bytes()[:split])
		retry, err := renderer.OnFailure(interrupted.Snapshot())
		if err != nil {
			t.Fatal(err)
		}
		if retry.Transaction().Kind() != TransactionClear {
			t.Fatalf("retry = %s", retry)
		}
		applyFrame(t, renderer, interrupted, retry)
		if interrupted.Snapshot() != before || !slices.Equal(screenRows(t, interrupted), beforeRows) {
			t.Fatalf("split %d differs", split)
		}
	}
}

func TestFailureCleanupRecoversEveryPartialResizeClearPrefix(t *testing.T) {
	t.Parallel()
	base := []byte("\x1b[2;1H> git")
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{
		testSuggestion(t, "git checkout feature", "", "git"),
	}, 0)
	resized, err := screen.NewTerminalSize(18, 5)
	if err != nil {
		t.Fatal(err)
	}
	beforeObserver := testScreen(t, 40, 8, base)
	if _, err := beforeObserver.Resize(resized); err != nil {
		t.Fatal(err)
	}
	before, beforeRows := beforeObserver.Snapshot(), screenRows(t, beforeObserver)
	source := testScreen(t, 40, 8, base)
	sourceRenderer := NewRenderer()
	draw, err := sourceRenderer.Render(source.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, sourceRenderer, source, draw)
	if _, err := source.Resize(resized); err != nil {
		t.Fatal(err)
	}
	clearFrame, err := sourceRenderer.OnResize(source.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	clearBytes := clearFrame.Transaction().Bytes()
	for split := 0; split <= len(clearBytes); split++ {
		interrupted := testScreen(t, 40, 8, base)
		renderer := NewRenderer()
		draw, err := renderer.Render(interrupted.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
		if err != nil {
			t.Fatal(err)
		}
		applyFrame(t, renderer, interrupted, draw)
		if _, err := interrupted.Resize(resized); err != nil {
			t.Fatal(err)
		}
		clearFrame, err := renderer.OnResize(interrupted.Snapshot())
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(clearFrame.Transaction().Bytes(), clearBytes) {
			t.Fatalf("split %d clear differs", split)
		}
		interrupted.Observe(clearFrame.Transaction().Bytes()[:split])
		cleanup, err := renderer.OnFailure(interrupted.Snapshot())
		if err != nil {
			t.Fatal(err)
		}
		if cleanup.Transaction().Kind() != TransactionClear {
			t.Fatalf("cleanup = %s", cleanup)
		}
		applyFrame(t, renderer, interrupted, cleanup)
		if interrupted.Snapshot() != before || !slices.Equal(screenRows(t, interrupted), beforeRows) {
			t.Fatalf("split %d differs", split)
		}
	}
}

func TestPendingTransactionsRequireMatchingAcknowledgments(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 8, []byte("> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	renderer := NewRenderer()
	draw, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	_, err = renderer.Clear(observer.Snapshot())
	pendingErr := requireErrorKind(t, err, TransactionPending)
	if pendingErr.Sequence() != draw.Transaction().Sequence() {
		t.Fatalf("pending sequence = %d", pendingErr.Sequence())
	}
	cleanup, err := renderer.OnFailure(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	err = renderer.Acknowledge(draw.Transaction())
	unexpected := requireErrorKind(t, err, UnexpectedTransaction)
	if unexpected.Expected() != cleanup.Transaction().Sequence() ||
		unexpected.Actual() != draw.Transaction().Sequence() {
		t.Fatalf("unexpected = %#v", unexpected)
	}
	if err := renderer.Acknowledge(cleanup.Transaction()); err != nil {
		t.Fatal(err)
	}
}

func TestForeignSameSequenceDrawsCannotAcknowledge(t *testing.T) {
	t.Parallel()
	observerA := testScreen(t, 30, 8, []byte("> git"))
	observerB := testScreen(t, 30, 8, []byte("> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	rendererA, rendererB := NewRenderer(), NewRenderer()
	drawA, err := rendererA.Render(observerA.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	drawB, err := rendererB.Render(observerB.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	if drawA.Transaction().Sequence() != 1 || drawB.Transaction().Sequence() != 1 {
		t.Fatal("first sequences differ")
	}
	observerA.Observe(drawA.Transaction().Bytes())
	observerB.Observe(drawB.Transaction().Bytes())
	err = rendererA.Acknowledge(drawB.Transaction())
	_ = requireErrorKind(t, err, ForeignTransaction)
	if _, ok := rendererA.OwnedRegion(); ok {
		t.Fatal("foreign acknowledgment established ownership")
	}
	_, err = rendererA.Clear(observerA.Snapshot())
	pending := requireErrorKind(t, err, TransactionPending)
	if pending.Sequence() != 1 {
		t.Fatalf("pending = %#v", pending)
	}
	if err := rendererA.Acknowledge(drawA.Transaction()); err != nil {
		t.Fatal(err)
	}
	if _, ok := rendererA.OwnedRegion(); !ok {
		t.Fatal("valid acknowledgment did not establish ownership")
	}
}

func TestForeignSameSequenceClearsCannotAcknowledge(t *testing.T) {
	t.Parallel()
	observerA := testScreen(t, 30, 8, []byte("> git"))
	observerB := testScreen(t, 30, 8, []byte("> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	rendererA, rendererB := NewRenderer(), NewRenderer()
	drawA, err := rendererA.Render(observerA.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, rendererA, observerA, drawA)
	drawB, err := rendererB.Render(observerB.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	applyFrame(t, rendererB, observerB, drawB)
	clearA, err := rendererA.Clear(observerA.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	clearB, err := rendererB.Clear(observerB.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	if clearA.Transaction().Sequence() != 2 || clearB.Transaction().Sequence() != 2 {
		t.Fatal("clear sequences differ")
	}
	observerA.Observe(clearA.Transaction().Bytes())
	observerB.Observe(clearB.Transaction().Bytes())
	err = rendererA.Acknowledge(clearB.Transaction())
	_ = requireErrorKind(t, err, ForeignTransaction)
	if _, ok := rendererA.OwnedRegion(); !ok {
		t.Fatal("foreign clear dropped ownership")
	}
	if err := rendererA.Acknowledge(clearA.Transaction()); err != nil {
		t.Fatal(err)
	}
	if _, ok := rendererA.OwnedRegion(); ok {
		t.Fatal("valid clear retained ownership")
	}
}

func TestForeignSameSequenceEmptyTransactionsCannotAcknowledge(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 8, []byte("> git"))
	rendererA, rendererB := NewRenderer(), NewRenderer()
	emptyA, err := rendererA.Clear(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	emptyB, err := rendererB.Clear(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	if emptyA.Transaction().Sequence() != 1 || emptyB.Transaction().Sequence() != 1 ||
		!emptyA.Transaction().Empty() || !emptyB.Transaction().Empty() {
		t.Fatal("empty transactions differ")
	}
	err = rendererA.Acknowledge(emptyB.Transaction())
	_ = requireErrorKind(t, err, ForeignTransaction)
	if err := rendererA.Acknowledge(emptyA.Transaction()); err != nil {
		t.Fatal(err)
	}
}

func TestRendererIDAllocationFailsClosedAtExhaustion(t *testing.T) {
	t.Parallel()
	var counter atomic.Uint64
	counter.Store(math.MaxUint64 - 1)
	identifier, err := allocateRendererID(&counter)
	if err != nil || identifier != math.MaxUint64-1 {
		t.Fatalf("identifier = %d, error = %v", identifier, err)
	}
	_, err = allocateRendererID(&counter)
	_ = requireErrorKind(t, err, RendererIDExhausted)
}

func TestFailureCleanupUsesPreDrawCursorWithoutChangingWrap(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 8, []byte("\x1b[?7l\x1b[2;1H> git"))
	before := observer.Snapshot()
	if before.Wrapping() {
		t.Fatal("wrapping remains enabled")
	}
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	output := frame.Transaction().Bytes()
	first := bytes.Index(output, []byte("\x1b["))
	secondRelative := bytes.Index(output[first+2:], []byte("\x1b["))
	if first < 0 || secondRelative < 0 {
		t.Fatalf("transaction = %q", output)
	}
	second := first + 2 + secondRelative
	observer.Observe(output[:second])
	if observer.Snapshot().Cursor() == before.Cursor() {
		t.Fatal("partial draw did not move cursor")
	}
	cleanup, err := renderer.OnFailure(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	observer.Observe(cleanup.Transaction().Bytes())
	if observer.Snapshot().Cursor() != before.Cursor() || observer.Snapshot().Wrapping() != before.Wrapping() {
		t.Fatalf("snapshot = %#v, want cursor/wrap %#v", observer.Snapshot(), before)
	}
}

func TestLeadingCombiningGhostIsRejectedWithoutTouchingInput(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 6, []byte("> git"))
	before, _ := observer.RowText(0)
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git\u0301 status", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := frame.Ghost(); ok {
		t.Fatal("leading combining ghost was accepted")
	}
	applyFrame(t, renderer, observer, frame)
	clearFrame, err := renderer.Clear(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	observer.Observe(clearFrame.Transaction().Bytes())
	after, _ := observer.RowText(0)
	if after != before {
		t.Fatalf("row = %q, want %q", after, before)
	}
}

func TestLeadingEmojiModifierGhostCannotExtendInputCell(t *testing.T) {
	t.Parallel()
	query, err := completion.NewQuery("👋", len("👋"), []byte("/tmp/Greendale"), 7)
	if err != nil {
		t.Fatal(err)
	}
	edit, err := completion.NewTextEdit(0, len("👋"), "👋🏽 wave")
	if err != nil {
		t.Fatal(err)
	}
	candidate, err := completion.NewSuggestion(
		edit, "👋🏽 wave", "", "command", completion.SourceSpec,
		completion.InsertionExact, "emoji modifier",
	)
	if err != nil {
		t.Fatal(err)
	}
	state := testSelection([]completion.Suggestion{candidate}, 0)
	observer := testScreen(t, 30, 6, []byte("> 👋"))
	before, _ := observer.RowText(0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := frame.Ghost(); ok {
		t.Fatal("leading emoji modifier ghost was accepted")
	}
	observer.Observe(frame.Transaction().Bytes())
	after, _ := observer.RowText(0)
	if after != before {
		t.Fatalf("row = %q, want %q", after, before)
	}
}

func TestBidiControlsAreReplacedBeforeRendering(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 76, 10, []byte("> git"))
	query := testQuery(t, "git")
	edit, err := completion.NewTextEdit(0, 3, "git status")
	if err != nil {
		t.Fatal(err)
	}
	candidate, err := completion.NewSuggestion(
		edit, "git\u200f status", "Greendale\u061c coursework", "command",
		completion.SourceSpec, completion.InsertionExact, "bidi",
	)
	if err != nil {
		t.Fatal(err)
	}
	state := testSelection([]completion.Suggestion{candidate}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	output := string(frame.Transaction().Bytes())
	if strings.ContainsRune(output, '\u200f') || strings.ContainsRune(output, '\u061c') ||
		!strings.ContainsRune(output, '�') {
		t.Fatalf("output = %q", output)
	}
}

func TestDelayedWrapSuppressesOverlayAndRemainsAuthoritative(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 8, 3, []byte("12345678"))
	cursor := observer.Snapshot().Cursor()
	if err := observer.Synchronize(cursor); err != nil {
		t.Fatal(err)
	}
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git status", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	if !frame.Transaction().Empty() || !observer.Snapshot().WrapPending() {
		t.Fatalf("frame = %s, snapshot = %#v", frame, observer.Snapshot())
	}
	observer.Observe([]byte("X"))
	row0, _ := observer.RowText(0)
	row1, _ := observer.RowText(1)
	if row0 != "12345678" || row1 != "X" {
		t.Fatalf("rows = %q", screenRows(t, observer))
	}
}

func TestGhostRequiresEndCursorSafeExactPrefixAndNeverWraps(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 12, 6, []byte("> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git checkout", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	ghost, ok := frame.Ghost()
	if !ok || !ghost.Clipped() || ghost.AcceptedSuffix() != " chec" || ghost.Cells() > 6 {
		t.Fatalf("ghost = %s, %t", ghost, ok)
	}
	middleQuery, err := completion.NewQuery("git", 1, []byte("/tmp/Greendale"), 7)
	if err != nil {
		t.Fatal(err)
	}
	renderer = NewRenderer()
	frame, err = renderer.Render(observer.Snapshot(), NewRequest(middleQuery, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := frame.Ghost(); ok {
		t.Fatal("middle-cursor ghost was accepted")
	}
}

func TestMultilineSuffixRendersNoGhost(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 40, 6, []byte("> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git one\ntwo", "", "git")}, 0)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleClassic))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := frame.Ghost(); ok || bytes.Contains(frame.Transaction().Bytes(), []byte{'\n'}) {
		t.Fatalf("frame = %s", frame)
	}
}

func TestStableWindowShowsPositionTotalAndPlainFallback(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 30, 6, []byte("> git"))
	query := testQuery(t, "git")
	candidates := make([]completion.Suggestion, 8)
	for index := range candidates {
		candidates[index] = testSuggestion(t, fmt.Sprintf("git item-%d", index), "description", "unknown")
	}
	state := testSelection(candidates, 6)
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	output := string(frame.Transaction().Bytes())
	if !strings.Contains(output, "7/8") || !strings.Contains(output, "? [spec]") ||
		strings.Contains(output, "󰆍") {
		t.Fatalf("output = %q", output)
	}
}

func TestDirectOptionsAreClampedToOwnedAndOutputLimits(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 80, 80, []byte("\x1b[10;1H> git"))
	query := testQuery(t, "git")
	candidates := make([]completion.Suggestion, 80)
	for index := range candidates {
		candidates[index] = testSuggestion(t, fmt.Sprintf("git course-%d", index), "Greendale", "git")
	}
	state := testSelection(candidates, 0)
	options := Options{
		Style: StyleModern, NerdFonts: false, GhostText: true,
		MaxHeight: math.MaxUint16, MinUsableHeight: 0,
	}
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), NewRequest(query, state), options)
	if err != nil {
		t.Fatal(err)
	}
	if len(frame.Transaction().Bytes()) > MaxRenderBytes {
		t.Fatalf("transaction = %s", frame.Transaction())
	}
	applyFrame(t, renderer, observer, frame)
	owned, ok := renderer.OwnedRegion()
	if !ok || len(owned.Spans()) > MaxOwnedSpans {
		t.Fatalf("owned = %s, %t", owned, ok)
	}
}

func TestOutputFailureHideAndDebugAreBoundedAndPrivate(t *testing.T) {
	t.Parallel()
	observer := testScreen(t, 80, 20, []byte("> git"))
	query := testQuery(t, "git")
	state := testSelection([]completion.Suggestion{testSuggestion(t, "git hunter2", "secret", "git")}, 0)
	request := NewRequest(query, state).WithFooterHint("secret-hint")
	renderer := NewRenderer()
	frame, err := renderer.Render(observer.Snapshot(), request, testOptions(StyleModern))
	if err != nil {
		t.Fatal(err)
	}
	if len(frame.Transaction().Bytes()) > MaxRenderBytes {
		t.Fatalf("frame = %s", frame)
	}
	debug := fmt.Sprintf("%v %#v %v %#v %v %#v", renderer, renderer, frame, frame, request, request)
	if strings.Contains(debug, "hunter2") || strings.Contains(debug, "secret") {
		t.Fatalf("debug leaked content: %s", debug)
	}
	applyFrame(t, renderer, observer, frame)
	clearFrame, err := renderer.BeforeShellOutput(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	clearBytes := clearFrame.Transaction().Bytes()
	if len(clearBytes) > MaxRenderBytes {
		t.Fatalf("clear = %s", clearFrame)
	}
	observer.Observe(clearBytes[:1])
	recovery, err := renderer.OnFailure(observer.Snapshot())
	if err != nil {
		t.Fatal(err)
	}
	if len(recovery.Transaction().Bytes()) > MaxRenderBytes {
		t.Fatalf("recovery = %s", recovery)
	}
}

func TestDisplaySanitizingReplacesEveryInvisibleCharacter(t *testing.T) {
	t.Parallel()
	hidden := []rune{
		'\u00ad', '\u200b', '\u200d', '\u200e', '\u202e', '\u2060',
		'\u2066', '\ufeff', '\U000e0041', '\U0001d173',
	}
	for _, character := range hidden {
		sanitized, truncated := sanitizeBounded(fmt.Sprintf("git%cstatus", character))
		if truncated || sanitized != "git�status" {
			t.Errorf("kept U+%04X: %q, truncated=%t", character, sanitized, truncated)
		}
	}
	if sanitized, _ := sanitizeBounded("git commit café"); sanitized != "git commit café" {
		t.Fatalf("sanitized = %q", sanitized)
	}
}
