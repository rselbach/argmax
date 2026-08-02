package session

import "testing"

func TestVTPrintable(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("hello"))
	row, col := vt.pos()
	if row != 0 || col != 5 {
		t.Fatalf("pos = %d,%d", row, col)
	}
}

func TestVTNewlineCRLF(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("ab\r\ncd\r\nef"))
	row, col := vt.pos()
	if row != 2 || col != 2 {
		t.Fatalf("pos = %d,%d, want 2,2", row, col)
	}
}

func TestVTScrollAtBottom(t *testing.T) {
	vt := newVTTracker(80, 3)
	vt.feed([]byte("a\nb\nc\nd\ne\nf"))
	row, _ := vt.pos()
	if row != 2 {
		t.Fatalf("row = %d, want 2 (clamped at bottom)", row)
	}
}

func TestVTDeferredWrap(t *testing.T) {
	vt := newVTTracker(5, 24)
	vt.feed([]byte("abcde")) // fills the row; cursor parks at col 4 with wrap pending
	if _, col := vt.pos(); col != 4 {
		t.Fatalf("col = %d, want 4 (deferred wrap)", col)
	}
	vt.feed([]byte("f")) // next printable forces the wrap
	row, col := vt.pos()
	if row != 1 || col != 1 {
		t.Fatalf("pos = %d,%d, want 1,1", row, col)
	}
}

func TestVTWideChars(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("こんにちは")) // 5 wide chars = 10 cells
	if _, col := vt.pos(); col != 10 {
		t.Fatalf("col = %d, want 10", col)
	}
}

func TestVTCSI(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("\x1b[10;20H")) // cup to row 10, col 20 (1-based)
	row, col := vt.pos()
	if row != 9 || col != 19 {
		t.Fatalf("after CUP pos = %d,%d, want 9,19", row, col)
	}
	vt.feed([]byte("\x1b[2A\x1b[3C"))
	row, col = vt.pos()
	if row != 7 || col != 22 {
		t.Fatalf("after CUU/CUF pos = %d,%d, want 7,22", row, col)
	}
	vt.feed([]byte("\x1b[G"))
	if _, col := vt.pos(); col != 0 {
		t.Fatalf("after CHA col = %d, want 0", col)
	}
}

func TestVTIgnoresColorsAndOSC(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("\x1b[1;31mred\x1b[0m\x1b]0;title\a"))
	row, col := vt.pos()
	if row != 0 || col != 3 {
		t.Fatalf("pos = %d,%d, want 0,3", row, col)
	}
	// OSC terminated by ST (ESC \)
	vt.feed([]byte("\x1b]8;;http://x\x1b\\link"))
	_, col = vt.pos()
	if col != 7 {
		t.Fatalf("col after OSC/ST = %d, want 7", col)
	}
}

func TestVTDECSaveRestore(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("abcdef"))
	vt.feed([]byte("\x1b7\x1b[5;5H\x1b8"))
	row, col := vt.pos()
	if row != 0 || col != 6 {
		t.Fatalf("after save/restore pos = %d,%d, want 0,6", row, col)
	}
}

func TestVTAltScreen(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("some text"))
	vt.feed([]byte("\x1b[?1049h")) // enter alt screen
	row, col := vt.pos()
	if row != 0 || col != 0 {
		t.Fatalf("alt screen pos = %d,%d, want 0,0", row, col)
	}
}

func TestVTResize(t *testing.T) {
	vt := newVTTracker(80, 24)
	vt.feed([]byte("\x1b[20;70H"))
	vt.resize(40, 10)
	row, col := vt.pos()
	if row != 9 || col != 39 {
		t.Fatalf("after resize pos = %d,%d, want clamped 9,39", row, col)
	}
}

func TestVTScrolledUp(t *testing.T) {
	vt := newVTTracker(80, 10)
	vt.feed([]byte("\x1b[10;1H")) // row 9 (bottom)
	vt.scrolledUp(6)
	row, _ := vt.pos()
	if row != 3 { // 9 - (9+6-9) = 3
		t.Fatalf("row = %d, want 3", row)
	}
	// No scroll when there is room below: cursor returns unchanged.
	vt2 := newVTTracker(80, 24)
	vt2.feed([]byte("\x1b[5;1H"))
	vt2.scrolledUp(3)
	if row, _ := vt2.pos(); row != 4 {
		t.Fatalf("no-scroll row = %d, want 4", row)
	}
}
