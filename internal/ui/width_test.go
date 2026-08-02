package ui

import "testing"

func TestVisibleWidth(t *testing.T) {
	tests := map[string]struct {
		in   string
		want int
	}{
		"plain ascii":       {in: "hello", want: 5},
		"csi color ignored": {in: "\x1b[38;2;1;2;3mhi\x1b[0m", want: 2},
		"osc title ignored": {in: "\x1b]0;title\x07prompt", want: 6},
		"osc st terminated": {in: "\x1b]0;title\x1b\\ok", want: 2},
		"cjk wide":          {in: "日本", want: 4},
		"emoji wide":        {in: "🚀x", want: 3},
		"control skipped":   {in: "a\x01b", want: 2},
		"empty":             {in: "", want: 0},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := VisibleWidth(tc.in); got != tc.want {
				t.Errorf("VisibleWidth(%q) = %d, want %d", tc.in, got, tc.want)
			}
		})
	}
}

func TestTruncateToWidth(t *testing.T) {
	tests := map[string]struct {
		in    string
		cells int
		want  string
	}{
		"fits":      {in: "abc", cells: 5, want: "abc"},
		"truncated": {in: "abcdefgh", cells: 5, want: "abcd…"},
		"cjk":       {in: "日本語テスト", cells: 5, want: "日本…"},
		"zero":      {in: "abc", cells: 0, want: ""},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := TruncateToWidth(tc.in, tc.cells); got != tc.want {
				t.Errorf("TruncateToWidth(%q, %d) = %q, want %q", tc.in, tc.cells, got, tc.want)
			}
		})
	}
}

func TestPadToWidth(t *testing.T) {
	if got := PadToWidth("ab", 5); got != "ab   " {
		t.Errorf("PadToWidth = %q", got)
	}
	if got := VisibleWidth(PadToWidth("日本語テスト", 5)); got > 5 {
		t.Errorf("padded width %d exceeds 5", got)
	}
}
