// Package overlay renders the argmax completion menu, source pills, icons,
// and ghost text as ANSI byte sequences, and provides terminal cell-width
// utilities. It only generates bytes and computes layout: the session owns
// the actual terminal writes, cursor save/restore policy, and scroll
// pre-allocation near the bottom of the screen.
package overlay

import (
	"fmt"
	"strconv"
	"strings"
)

// Palette holds the built-in semantic colors as ANSI SGR truecolor
// sequences: foreground roles use "\x1b[38;2;r;g;bm" and background roles
// use "\x1b[48;2;r;g;bm".
type Palette struct {
	Border      string // borders and scroll information
	Accent      string // accent and matched query prefix
	Muted       string // muted icons and secondary text
	Primary     string // primary text
	Selected    string // selected row text
	Description string // description text
	SelectedBg  string // selected row background
	Ghost       string // ghost text
}

// defaultPalette is precomputed from the PRD 9.4 semantic defaults.
var defaultPalette = Palette{
	Border:      fg("#a277ff"),
	Accent:      fg("#61ffca"),
	Muted:       fg("#6d6a7f"),
	Primary:     fg("#edecee"),
	Selected:    fg("#ffffff"),
	Description: fg("#9692a8"),
	SelectedBg:  bg("#3d375e"),
	Ghost:       fg("#4b4a4c"),
}

// DefaultPalette returns the built-in semantic palette with all SGR
// sequences precomputed.
func DefaultPalette() Palette { return defaultPalette }

func fg(hex string) string {
	r, g, b := parseHex(hex)
	return fmt.Sprintf("\x1b[38;2;%d;%d;%dm", r, g, b)
}

func bg(hex string) string {
	r, g, b := parseHex(hex)
	return fmt.Sprintf("\x1b[48;2;%d;%d;%dm", r, g, b)
}

func parseHex(hex string) (r, g, b int) {
	v, _ := strconv.ParseUint(strings.TrimPrefix(hex, "#"), 16, 32)
	return int(v >> 16 & 0xff), int(v >> 8 & 0xff), int(v & 0xff)
}
