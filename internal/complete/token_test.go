package complete

import (
	"reflect"
	"testing"
)

func TestTokenize(t *testing.T) {
	tests := map[string]struct {
		line string
		want []Token
	}{
		"empty": {
			line: "",
			want: []Token{{Text: "", Start: 0}},
		},
		"single word": {
			line: "git",
			want: []Token{{Text: "git", Start: 0}},
		},
		"trailing space yields empty token": {
			line: "git ",
			want: []Token{{Text: "git", Start: 0}, {Text: "", Start: 4}},
		},
		"two words": {
			line: "git che",
			want: []Token{{Text: "git", Start: 0}, {Text: "che", Start: 4}},
		},
		"double quotes": {
			line: `cp "My Documents" dest`,
			want: []Token{
				{Text: "cp", Start: 0},
				{Text: "My Documents", Start: 3},
				{Text: "dest", Start: 18},
			},
		},
		"single quotes": {
			line: `echo 'a b'`,
			want: []Token{{Text: "echo", Start: 0}, {Text: "a b", Start: 5}},
		},
		"escaped space": {
			line: `cd My\ Docs`,
			want: []Token{{Text: "cd", Start: 0}, {Text: "My Docs", Start: 3}},
		},
		"unterminated quote": {
			line: `git commit -m "wip`,
			want: []Token{
				{Text: "git", Start: 0},
				{Text: "commit", Start: 4},
				{Text: "-m", Start: 11},
				{Text: "wip", Start: 14},
			},
		},
		"tabs separate": {
			line: "a\tb",
			want: []Token{{Text: "a", Start: 0}, {Text: "b", Start: 2}},
		},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got := Tokenize(tc.line)
			if !reflect.DeepEqual(got, tc.want) {
				t.Errorf("Tokenize(%q) = %#v, want %#v", tc.line, got, tc.want)
			}
		})
	}
}
