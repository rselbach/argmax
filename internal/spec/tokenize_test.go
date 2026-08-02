package spec

import (
	"reflect"
	"testing"
)

func TestTokenize(t *testing.T) {
	cases := []struct {
		line string
		want []string
	}{
		// SPEC-002 examples
		{"git che", []string{"git", "che"}},
		{"git checkout ", []string{"git", "checkout", ""}},
		{`echo "a b" c`, []string{"echo", "a b", "c"}},
		{`echo a\ b`, []string{"echo", "a b"}},
		{"", []string{""}},
		{"   ", []string{""}},
		{"\t", []string{""}},

		// Single token, no trailing whitespace: no trailing empty token.
		{"git", []string{"git"}},

		// Quotes.
		{`echo 'a b' c`, []string{"echo", "a b", "c"}},
		{`echo "it's"`, []string{"echo", "it's"}},
		{`echo 'a'"b"`, []string{"echo", "ab"}},
		{`echo a"b c"d`, []string{"echo", "ab cd"}},
		{`echo ""`, []string{"echo", ""}},
		{`echo '' x`, []string{"echo", "", "x"}},
		{`echo "unterminated`, []string{"echo", "unterminated"}},
		{`echo 'unterminated`, []string{"echo", "unterminated"}},

		// Escapes.
		{`echo a\"b`, []string{"echo", `a"b`}},
		{`echo "a\"b"`, []string{"echo", `a"b`}},
		{`echo a\\b`, []string{"echo", `a\b`}},
		{`echo "a\\b"`, []string{"echo", `a\b`}},
		{`git commit -m "fix \"the\" bug" `, []string{"git", "commit", "-m", `fix "the" bug`, ""}},

		// Multiple separators collapse; escaped trailing space continues the
		// token instead of producing a trailing empty token.
		{"git  checkout", []string{"git", "checkout"}},
		{"git checkout  ", []string{"git", "checkout", ""}},
		{`echo a\ `, []string{"echo", "a "}},
	}
	for _, c := range cases {
		got := Tokenize(c.line)
		if !reflect.DeepEqual(got, c.want) {
			t.Errorf("Tokenize(%q) = %#v, want %#v", c.line, got, c.want)
		}
	}
}

func TestTokenizeStarts(t *testing.T) {
	// The internal starts offsets must let Resolve reconstruct the raw line
	// prefix of the final token even with quotes and escapes.
	cases := []struct {
		line       string
		wantPrefix string
	}{
		{"git che", "git "},
		{"git checkout ", "git checkout "},
		{"git", ""},
		{"", ""},
		{`echo a\ b`, "echo "},
		{`echo "a b"`, "echo "},
	}
	for _, c := range cases {
		tokens, starts := tokenize(c.line)
		last := len(tokens) - 1
		if got := c.line[:starts[last]]; got != c.wantPrefix {
			t.Errorf("tokenize(%q) last prefix = %q, want %q", c.line, got, c.wantPrefix)
		}
	}
}
