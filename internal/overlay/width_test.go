package overlay

import "testing"

func TestStripANSI(t *testing.T) {
	cases := []struct {
		name, in, want string
	}{
		{"plain", "plain", "plain"},
		{"csi color", "\x1b[31mred\x1b[0m", "red"},
		{"csi truecolor", "\x1b[1;38;2;1;2;3mx", "x"},
		{"csi cursor back", "a\x1b[Db", "ab"},
		{"csi erase line", "\x1b[2K\rcontent", "\rcontent"},
		{"osc bel", "\x1b]0;window title\x07after", "after"},
		{"osc st", "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\", "link"},
		{"dec save restore", "\x1b7saved\x1b8", "saved"},
		{"charset", "\x1b(0x\x1b(B", "x"},
		{"unterminated osc", "pre\x1b]0;unterminated", "pre"},
		{"lone esc at end", "tail\x1b", "tail"},
		{"tabs kept", "a\tb", "a\tb"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := StripANSI(c.in); got != c.want {
				t.Errorf("StripANSI(%q) = %q, want %q", c.in, got, c.want)
			}
		})
	}
}

func TestStringWidth(t *testing.T) {
	cases := []struct {
		name string
		in   string
		want int
	}{
		{"empty", "", 0},
		{"ascii", "hello", 5},
		{"cjk", "こんにちは", 10},
		{"emoji rocket", "🚀", 2},
		{"emoji zwj family", "👨‍👩‍👧", 2},
		{"emoji vs16 gear", "⚙️", 2},
		{"flag pair", "🇺🇸", 2},
		{"combining mark", "e\u0301", 1},
		{"tab at start", "\t", 8},
		{"tab after one cell", "a\tb", 9},
		{"tab after two cells", "ab\tc", 9},
		{"tab on multiple of 8", "abcdefgh\ti", 17},
		{"control chars zero", "a\x01\x7fb", 2},
		{"ansi around wide", "\x1b[1mこんにちは\x1b[0m", 10},
		{"mixed ansi wide", "\x1b[38;2;1;2;3mab🚀\x1b[0m cd", 7},
		{"hangul syllable", "한", 2},
		{"fullwidth", "ＡＢ", 4},
		{"pua nerd glyph", "\ue702", 1},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := StringWidth(c.in); got != c.want {
				t.Errorf("StringWidth(%q) = %d, want %d", c.in, got, c.want)
			}
		})
	}
}

func TestTruncate(t *testing.T) {
	cases := []struct {
		name  string
		in    string
		width int
		want  string
	}{
		{"fits exactly", "hello world", 11, "hello world"},
		{"fits with room", "short", 10, "short"},
		{"ascii cut", "hello world", 8, "hello w…"},
		{"cjk cut on boundary", "こんにちは", 5, "こん…"},
		{"cjk cut avoids split", "こんにちは", 4, "こ…"},
		{"cjk width two", "こんにちは", 2, "…"},
		{"width one", "ab", 1, "…"},
		{"width zero", "abc", 0, ""},
		{"negative width", "abc", -3, ""},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got := Truncate(c.in, c.width)
			if got != c.want {
				t.Errorf("Truncate(%q, %d) = %q, want %q", c.in, c.width, got, c.want)
			}
			if w := StringWidth(got); w > c.width && c.width > 0 {
				t.Errorf("Truncate(%q, %d) = %q has width %d > %d", c.in, c.width, got, w, c.width)
			}
		})
	}
}

func TestTruncateANSI(t *testing.T) {
	styled := "\x1b[38;2;97;255;202mgit\x1b[0m plain"
	got := truncateANSI(styled, 5)
	if StripANSI(got) != "git p" {
		t.Errorf("truncateANSI plain text = %q, want %q", StripANSI(got), "git p")
	}
	if !hasESC(got) {
		t.Errorf("truncateANSI dropped escape sequences: %q", got)
	}
	if w := StringWidth(got); w > 5 {
		t.Errorf("truncateANSI width = %d, want <= 5", w)
	}
}

func hasESC(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] == 0x1b {
			return true
		}
	}
	return false
}

func TestPadRight(t *testing.T) {
	if got := padRight("ab", 5); got != "ab   " {
		t.Errorf("padRight = %q", got)
	}
	if got := padRight("こんにちは", 10); got != "こんにちは" {
		t.Errorf("padRight wide exact = %q", got)
	}
	if got := padRight("toolong", 3); got != "toolong" {
		t.Errorf("padRight no shrink = %q", got)
	}
}

func TestPrefixFoldLen(t *testing.T) {
	cases := []struct {
		s, prefix string
		want      int
	}{
		{"git status", "gi", 2},
		{"git status", "GIT", 3},
		{"git status", "", 0},
		{"git", "git ", 0},
		{"git", "git", 3},
		{"github", "git ", 0},
		{"über", "ÜB", 3}, // ü folds with Ü (2 bytes) plus b (1 byte)
	}
	for _, c := range cases {
		if got := prefixFoldLen(c.s, c.prefix); got != c.want {
			t.Errorf("prefixFoldLen(%q, %q) = %d, want %d", c.s, c.prefix, got, c.want)
		}
	}
}
