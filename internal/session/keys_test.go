package session

import (
	"testing"
)

func TestKeyParserRunes(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte("git che"))
	if len(evs) != 7 {
		t.Fatalf("expected 7 rune events, got %d", len(evs))
	}
	for i, want := range "git che" {
		if evs[i].kind != keyRune || evs[i].r != want {
			t.Fatalf("event %d = %+v, want rune %q", i, evs[i], want)
		}
	}
	if len(p.pending) != 0 {
		t.Fatal("no pending bytes expected")
	}
}

func TestKeyParserUTF8(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte("héllo→"))
	if len(evs) != 6 {
		t.Fatalf("expected 6 runes, got %d", len(evs))
	}
	if evs[5].r != '→' {
		t.Fatalf("last rune = %q", evs[5].r)
	}
}

func TestKeyParserSplitUTF8(t *testing.T) {
	p := &keyParser{}
	full := []byte("→")
	evs := p.feed(full[:1])
	if len(evs) != 0 {
		t.Fatal("incomplete rune must be buffered")
	}
	evs = p.feed(full[1:])
	if len(evs) != 1 || evs[0].r != '→' {
		t.Fatalf("split rune = %+v", evs)
	}
}

func TestKeyParserArrowsAndShiftTab(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte("\x1b[A\x1b[B\x1b[C\x1b[D\x1b[Z"))
	want := []string{"up", "down", "right", "left", "shift+tab"}
	if len(evs) != 5 {
		t.Fatalf("expected 5 events, got %d", len(evs))
	}
	for i, w := range want {
		if evs[i].kind != keySpecial || evs[i].spec != w {
			t.Fatalf("event %d = %+v, want %s", i, evs[i], w)
		}
	}
}

func TestKeyParserSS3Arrows(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte("\x1bOA\x1bOD"))
	if len(evs) != 2 || evs[0].spec != "up" || evs[1].spec != "left" {
		t.Fatalf("SS3 arrows = %+v", evs)
	}
}

func TestKeyParserCtrlBytes(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte{0x01, 0x05, 0x17, 0x15, 0x03, 0x0c, 0x12, 0x00})
	want := []byte{'a', 'e', 'w', 'u', 'c', 'l', 'r', 0}
	if len(evs) != 8 {
		t.Fatalf("expected 8 ctrl events, got %d", len(evs))
	}
	for i, w := range want {
		if evs[i].kind != keyCtrl || evs[i].ctrl != w {
			t.Fatalf("event %d = %+v, want ctrl+%c", i, evs[i], w)
		}
	}
}

func TestKeyParserEnterAndTab(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte{'\r', '\t'})
	if evs[0].spec != "enter" || evs[1].spec != "tab" {
		t.Fatalf("enter/tab = %+v", evs)
	}
}

func TestKeyParserLoneEscape(t *testing.T) {
	p := &keyParser{}
	if evs := p.feed([]byte{0x1b}); len(evs) != 0 {
		t.Fatal("lone ESC must be held pending")
	}
	evs := p.flush()
	if len(evs) != 1 || evs[0].kind != keyEscape {
		t.Fatalf("flush = %+v", evs)
	}
}

func TestKeyParserUnknownSequenceForwarded(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte("\x1b[1;5A")) // ctrl+up: unknown to us
	if len(evs) != 1 || evs[0].kind != keyUnknown {
		t.Fatalf("expected keyUnknown, got %+v", evs)
	}
	if string(evs[0].raw) != "\x1b[1;5A" {
		t.Fatalf("raw bytes must be preserved, got %q", evs[0].raw)
	}
}

func TestKeyParserPasteMarkers(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte("\x1b[200~hello\x1b[201~"))
	if len(evs) != 7 {
		t.Fatalf("expected 7 events, got %d", len(evs))
	}
	if evs[0].kind != keyPasteStart || evs[6].kind != keyPasteEnd {
		t.Fatalf("paste markers = %+v", evs)
	}
}

func TestKeyParserDSR(t *testing.T) {
	p := &keyParser{}
	evs := p.feed([]byte("\x1b[24;80R"))
	if len(evs) != 1 || evs[0].kind != keyDSR {
		t.Fatalf("DSR = %+v", evs)
	}
	if evs[0].rows != 24 || evs[0].cols != 80 {
		t.Fatalf("DSR rows/cols = %d/%d", evs[0].rows, evs[0].cols)
	}
}

func TestKeyParserKittyCtrl(t *testing.T) {
	p := &keyParser{}
	// Kitty: CSI 114;5u = ctrl+r
	evs := p.feed([]byte("\x1b[114;5u"))
	if len(evs) != 1 || evs[0].kind != keyCtrl || evs[0].ctrl != 'r' {
		t.Fatalf("kitty ctrl+r = %+v", evs)
	}
	// Without ctrl modifier it is not a ctrl binding.
	evs = p.feed([]byte("\x1b[114;1u"))
	if len(evs) != 1 || evs[0].kind != keyUnknown {
		t.Fatalf("kitty plain r should be unknown to us: %+v", evs)
	}
}

func TestKeyParserSplitSequence(t *testing.T) {
	p := &keyParser{}
	if evs := p.feed([]byte("\x1b[")); len(evs) != 0 {
		t.Fatal("partial CSI must be buffered")
	}
	evs := p.feed([]byte("A"))
	if len(evs) != 1 || evs[0].spec != "up" {
		t.Fatalf("reassembled arrow = %+v", evs)
	}
}

func TestDeleteWord(t *testing.T) {
	buf, cur := deleteWord([]rune("git commit -m"), 13)
	if string(buf) != "git commit " || cur != 11 {
		t.Fatalf("deleteWord = %q, %d", buf, cur)
	}
	// Readline behavior: kills trailing whitespace AND the preceding word.
	buf, cur = deleteWord([]rune("git   "), 6)
	if string(buf) != "" || cur != 0 {
		t.Fatalf("deleteWord trailing spaces = %q, %d", buf, cur)
	}
	buf, cur = deleteWord([]rune("word"), 4)
	if string(buf) != "" || cur != 0 {
		t.Fatalf("deleteWord all = %q, %d", buf, cur)
	}
	// Mid-line: removes only before the cursor.
	buf, cur = deleteWord([]rune("git checkout main"), 12)
	if string(buf) != "git  main" || cur != 4 {
		t.Fatalf("deleteWord mid-line = %q, %d", buf, cur)
	}
}

func TestRawBytesRoundTrip(t *testing.T) {
	cases := []keyEvent{
		{kind: keyRune, r: 'x'},
		{kind: keyCtrl, ctrl: 'r'},
		{kind: keyCtrl, ctrl: 0},
		{kind: keyBackspace},
		{kind: keySpecial, spec: "enter"},
		{kind: keySpecial, spec: "up"},
		{kind: keyEscape},
		{kind: keyPasteStart},
	}
	for _, ev := range cases {
		if len(ev.rawBytes()) == 0 {
			t.Fatalf("rawBytes(%+v) empty", ev)
		}
	}
}

func TestSameKey(t *testing.T) {
	a := keyEvent{kind: keyCtrl, ctrl: 'r'}
	b := keyEvent{kind: keyCtrl, ctrl: 'r'}
	if !sameKey(a, b) {
		t.Fatal("identical ctrl keys must match")
	}
	c := keyEvent{kind: keySpecial, spec: "up"}
	if sameKey(a, c) {
		t.Fatal("different kinds must not match")
	}
}
