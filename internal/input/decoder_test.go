package input

import (
	"testing"

	"github.com/rselbach/argmax/internal/keymap"
)

func feedOne(t *testing.T, data []byte) Event {
	t.Helper()
	d := &Decoder{}
	events := d.Feed(data)
	if len(events) != 1 {
		t.Fatalf("Feed(%q) produced %d events, want 1", data, len(events))
	}
	return events[0]
}

func TestDecoderKeys(t *testing.T) {
	tests := map[string]struct {
		data []byte
		want keymap.Key
	}{
		"rune":        {data: []byte("a"), want: keymap.Key{Kind: keymap.KindRune, Rune: 'a'}},
		"utf8 rune":   {data: []byte("é"), want: keymap.Key{Kind: keymap.KindRune, Rune: 'é'}},
		"enter":       {data: []byte{'\r'}, want: keymap.Key{Kind: keymap.KindEnter}},
		"tab":         {data: []byte{'\t'}, want: keymap.Key{Kind: keymap.KindTab}},
		"backspace":   {data: []byte{0x7f}, want: keymap.Key{Kind: keymap.KindBackspace}},
		"ctrl-r":      {data: []byte{0x12}, want: keymap.Key{Kind: keymap.KindCtrl, Rune: 'r'}},
		"ctrl-space":  {data: []byte{0x00}, want: keymap.Key{Kind: keymap.KindCtrlSpace}},
		"up":          {data: []byte("\x1b[A"), want: keymap.Key{Kind: keymap.KindUp}},
		"shift tab":   {data: []byte("\x1b[Z"), want: keymap.Key{Kind: keymap.KindShiftTab}},
		"delete":      {data: []byte("\x1b[3~"), want: keymap.Key{Kind: keymap.KindDelete}},
		"kitty ctrl":  {data: []byte("\x1b[114;5u"), want: keymap.Key{Kind: keymap.KindCtrl, Rune: 'r'}},
		"kitty enter": {data: []byte("\x1b[13u"), want: keymap.Key{Kind: keymap.KindEnter}},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			ev := feedOne(t, tc.data)
			if ev.Kind != EventKey {
				t.Fatalf("kind = %v, want key", ev.Kind)
			}
			if ev.Key != tc.want {
				t.Errorf("key = %+v, want %+v", ev.Key, tc.want)
			}
		})
	}
}

func TestDecoderCPR(t *testing.T) {
	ev := feedOne(t, []byte("\x1b[12;40R"))
	if ev.Kind != EventCPR || ev.Row != 12 || ev.Col != 40 {
		t.Errorf("CPR = %+v, want row 12 col 40", ev)
	}
}

func TestDecoderBracketedPaste(t *testing.T) {
	d := &Decoder{}
	events := d.Feed([]byte("\x1b[200~hi\x1b[201~"))
	if len(events) != 4 {
		t.Fatalf("got %d events, want paste-start, h, i, paste-end", len(events))
	}
	if events[0].Kind != EventPasteStart || events[3].Kind != EventPasteEnd {
		t.Errorf("paste brackets not detected: %+v", events)
	}
}

func TestDecoderUnknownSequencePreserved(t *testing.T) {
	raw := []byte("\x1b[?1049h")
	ev := feedOne(t, raw)
	if ev.Kind != EventRaw {
		t.Fatalf("kind = %v, want raw", ev.Kind)
	}
	if string(ev.Raw) != string(raw) {
		t.Errorf("unknown sequence altered: %q != %q", ev.Raw, raw)
	}
}

func TestDecoderSplitSequence(t *testing.T) {
	d := &Decoder{}
	if events := d.Feed([]byte("\x1b[")); len(events) != 0 {
		t.Fatalf("incomplete sequence must wait, got %+v", events)
	}
	events := d.Feed([]byte("A"))
	if len(events) != 1 || events[0].Key.Kind != keymap.KindUp {
		t.Errorf("resumed sequence = %+v, want up", events)
	}
}

func TestDecoderLoneEscape(t *testing.T) {
	d := &Decoder{}
	if events := d.Feed([]byte{0x1b}); len(events) != 0 {
		t.Fatalf("lone ESC is pending, got %+v", events)
	}
	events := d.FlushPending()
	if len(events) != 1 || events[0].Key.Kind != keymap.KindEscape {
		t.Errorf("flushed = %+v, want escape", events)
	}
}

func TestBufferEditing(t *testing.T) {
	var b Buffer
	b.InsertString("héllo wörld")
	if b.String() != "héllo wörld" || !b.AtEnd() {
		t.Fatalf("buffer = %q atEnd=%v", b.String(), b.AtEnd())
	}
	b.Backspace()
	if b.String() != "héllo wörl" {
		t.Errorf("after backspace: %q", b.String())
	}
	b.DeleteWordBack()
	if b.String() != "héllo " {
		t.Errorf("after delete word: %q", b.String())
	}
	b.MoveHome()
	if b.AtEnd() {
		t.Error("cursor should be at start")
	}
	b.MoveRight()
	b.Insert('X')
	if b.String() != "hXéllo " {
		t.Errorf("mid-line insert: %q", b.String())
	}
	b.Clear()
	if !b.Empty() {
		t.Error("clear failed")
	}
}
