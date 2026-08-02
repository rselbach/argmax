package config

import (
	"fmt"
	"strings"
)

// KeyKind classifies a parsed key.
type KeyKind int

const (
	KeyRune    KeyKind = iota // a single printable character
	KeyCtrl                   // ctrl+<letter> or ctrl+space
	KeySpecial                // named keys: tab, shift+tab, arrows, enter
)

// Special key names.
const (
	KeyTab      = "tab"
	KeyShiftTab = "shift+tab"
	KeyUp       = "up"
	KeyDown     = "down"
	KeyLeft     = "left"
	KeyRight    = "right"
	KeyEnter    = "enter"
)

// Key is a parsed keybinding (PRD 9.3 key name grammar).
type Key struct {
	Kind    KeyKind
	Rune    rune   // KeyRune: the character
	Ctrl    byte   // KeyCtrl: lowercase letter, or 0 for space
	Special string // KeySpecial: one of the Key* constants
}

func (k Key) String() string {
	switch k.Kind {
	case KeyRune:
		return string(k.Rune)
	case KeyCtrl:
		if k.Ctrl == 0 {
			return "ctrl+space"
		}
		return "ctrl+" + string(k.Ctrl)
	default:
		return k.Special
	}
}

// ParseKey parses a configurable key name. Parsing is case-insensitive and
// accepts "-" as an alias for "+".
func ParseKey(name string) (Key, error) {
	n := strings.ToLower(strings.TrimSpace(name))
	n = strings.ReplaceAll(n, "-", "+")

	switch n {
	case "tab":
		return Key{Kind: KeySpecial, Special: KeyTab}, nil
	case "shift+tab":
		return Key{Kind: KeySpecial, Special: KeyShiftTab}, nil
	case "up":
		return Key{Kind: KeySpecial, Special: KeyUp}, nil
	case "down":
		return Key{Kind: KeySpecial, Special: KeyDown}, nil
	case "left":
		return Key{Kind: KeySpecial, Special: KeyLeft}, nil
	case "right":
		return Key{Kind: KeySpecial, Special: KeyRight}, nil
	case "enter", "return", "cr":
		return Key{Kind: KeySpecial, Special: KeyEnter}, nil
	}

	if rest, ok := strings.CutPrefix(n, "ctrl+"); ok {
		if rest == "space" {
			return Key{Kind: KeyCtrl, Ctrl: 0}, nil
		}
		if len(rest) == 1 && rest[0] >= 'a' && rest[0] <= 'z' {
			if rest[0] == 'm' {
				return Key{}, fmt.Errorf("ctrl+m is reserved for command submission")
			}
			return Key{Kind: KeyCtrl, Ctrl: rest[0]}, nil
		}
		return Key{}, fmt.Errorf("invalid ctrl binding %q", name)
	}

	if r := []rune(n); len(r) == 1 {
		return Key{Kind: KeyRune, Rune: r[0]}, nil
	}
	return Key{}, fmt.Errorf("unknown key name %q", name)
}
