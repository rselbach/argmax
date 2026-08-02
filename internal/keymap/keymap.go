// Package keymap parses configurable key names and models decoded key
// presses shared by configuration validation and terminal input handling.
package keymap

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

// Kind identifies a class of decoded key.
type Kind int

// Key kinds.
const (
	KindRune Kind = iota
	KindCtrl
	KindTab
	KindShiftTab
	KindUp
	KindDown
	KindLeft
	KindRight
	KindEnter
	KindEscape
	KindBackspace
	KindCtrlSpace
	KindHome
	KindEnd
	KindDelete
	KindUnknown
)

// Key is one decoded key press.
type Key struct {
	Kind Kind
	// Rune is the printable character for KindRune, or the lowercase
	// letter for KindCtrl.
	Rune rune
}

// Parse resolves a configured key name such as "ctrl+r", "shift+tab", or a
// single character. Parsing is case-insensitive and accepts "-" for "+".
func Parse(name string) (Key, error) {
	n := strings.ToLower(strings.TrimSpace(name))
	n = strings.ReplaceAll(n, "-", "+")
	switch n {
	case "tab":
		return Key{Kind: KindTab}, nil
	case "shift+tab":
		return Key{Kind: KindShiftTab}, nil
	case "up":
		return Key{Kind: KindUp}, nil
	case "down":
		return Key{Kind: KindDown}, nil
	case "left":
		return Key{Kind: KindLeft}, nil
	case "right":
		return Key{Kind: KindRight}, nil
	case "enter", "return", "cr":
		return Key{Kind: KindEnter}, nil
	case "ctrl+space":
		return Key{Kind: KindCtrlSpace}, nil
	}
	if rest, ok := strings.CutPrefix(n, "ctrl+"); ok {
		if utf8.RuneCountInString(rest) == 1 {
			r := rune(rest[0])
			if r >= 'a' && r <= 'z' {
				return Key{Kind: KindCtrl, Rune: r}, nil
			}
		}
		return Key{}, fmt.Errorf("unsupported key %q: ctrl bindings accept a single letter or space", name)
	}
	if utf8.RuneCountInString(n) == 1 {
		r, _ := utf8.DecodeRuneInString(n)
		return Key{Kind: KindRune, Rune: r}, nil
	}
	return Key{}, fmt.Errorf("unsupported key name %q", name)
}

// IsEnter reports whether the key would shadow command submission.
func (k Key) IsEnter() bool {
	return k.Kind == KindEnter || (k.Kind == KindCtrl && k.Rune == 'm')
}
