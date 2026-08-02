package spec

import "testing"

func TestQuoteIfNeeded(t *testing.T) {
	cases := []struct {
		in, want string
	}{
		{"plain", "plain"},
		{"path/to/file.txt", "path/to/file.txt"},
		{"a-b_c@d+e=f:g,h.i/j", "a-b_c@d+e=f:g,h.i/j"},
		{"~user/file", "~user/file"},
		{"", "''"},
		{"a b", "'a b'"},
		{"a  b", "'a  b'"},
		{"a'b", `'a'\''b'`},
		{"a;b|c&d", "'a;b|c&d'"},
		{"$(x)", "'$(x)'"},
		{"a*b", "'a*b'"},
		{`a"b`, `'a"b'`},
		{"a b(c)", "'a b(c)'"},
		{"already 'x' y", `'already '\''x'\'' y'`}, // not fully quoted: re-quoted as a whole
		{"'a b'", "'a b'"},                         // already single-quoted: unchanged
		{`"a b"`, `"a b"`},                         // already double-quoted: unchanged
		{"'a b", `''\''a b'`},                      // unbalanced quote: quoted, quotes escaped
	}
	for _, c := range cases {
		if got := QuoteIfNeeded(c.in); got != c.want {
			t.Errorf("QuoteIfNeeded(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}
