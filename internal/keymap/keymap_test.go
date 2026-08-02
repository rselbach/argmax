package keymap

import "testing"

func TestParse(t *testing.T) {
	tests := map[string]struct {
		in      string
		want    Key
		wantErr bool
	}{
		"ctrl letter":      {in: "ctrl+r", want: Key{Kind: KindCtrl, Rune: 'r'}},
		"uppercase":        {in: "CTRL+R", want: Key{Kind: KindCtrl, Rune: 'r'}},
		"dash alias":       {in: "ctrl-r", want: Key{Kind: KindCtrl, Rune: 'r'}},
		"tab":              {in: "tab", want: Key{Kind: KindTab}},
		"shift tab":        {in: "shift+tab", want: Key{Kind: KindShiftTab}},
		"arrows":           {in: "up", want: Key{Kind: KindUp}},
		"enter":            {in: "enter", want: Key{Kind: KindEnter}},
		"return alias":     {in: "return", want: Key{Kind: KindEnter}},
		"cr alias":         {in: "cr", want: Key{Kind: KindEnter}},
		"ctrl space":       {in: "ctrl+space", want: Key{Kind: KindCtrlSpace}},
		"single character": {in: "j", want: Key{Kind: KindRune, Rune: 'j'}},
		"invalid ctrl":     {in: "ctrl+enter", wantErr: true},
		"garbage":          {in: "megakey", wantErr: true},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got, err := Parse(tc.in)
			if tc.wantErr {
				if err == nil {
					t.Errorf("Parse(%q) = %+v, want error", tc.in, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("Parse(%q): %v", tc.in, err)
			}
			if got != tc.want {
				t.Errorf("Parse(%q) = %+v, want %+v", tc.in, got, tc.want)
			}
		})
	}
}

func TestIsEnter(t *testing.T) {
	if !(Key{Kind: KindEnter}).IsEnter() {
		t.Error("enter must report IsEnter")
	}
	if !(Key{Kind: KindCtrl, Rune: 'm'}).IsEnter() {
		t.Error("ctrl+m must report IsEnter: it shares the Enter byte")
	}
	if (Key{Kind: KindCtrl, Rune: 'r'}).IsEnter() {
		t.Error("ctrl+r must not report IsEnter")
	}
}
